//! TTY related functionality.

use std::ffi::CStr;
use std::fs::File;
use std::io::{Error, ErrorKind, Read, Result};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
#[cfg(target_os = "macos")]
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::sync::Arc;
use std::{env, ptr};

use libc::{F_GETFL, F_SETFL, O_NONBLOCK, TIOCSCTTY, c_int, fcntl};
use log::error;
use polling::{Event, PollMode, Poller};
use rustix_openpty::openpty;
use rustix_openpty::rustix::termios::Winsize;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use rustix_openpty::rustix::termios::{self, InputModes, OptionalActions};
use signal_hook::low_level::{pipe as signal_pipe, unregister as unregister_signal};
use signal_hook::{SigId, consts as sigconsts};

use crate::event::{OnResize, WindowSize};
use crate::tty::{ChildEvent, EventedPty, EventedReadWrite, Options};

// Interest in PTY read/writes.
pub(crate) const PTY_READ_WRITE_TOKEN: usize = 0;

// Interest in new child events.
pub(crate) const PTY_CHILD_EVENT_TOKEN: usize = 1;

/// Really only needed on BSD, but should be fine elsewhere.
fn set_controlling_terminal(fd: c_int) -> Result<()> {
    let res = unsafe {
        // TIOSCTTY changes based on platform and the `ioctl` call is different
        // based on architecture (32/64). So a generic cast is used to make sure
        // there are no issues. To allow such a generic cast the clippy warning
        // is disabled.
        #[allow(clippy::cast_lossless)]
        libc::ioctl(fd, TIOCSCTTY as _, 0)
    };

    if res == 0 { Ok(()) } else { Err(Error::last_os_error()) }
}

#[derive(Debug)]
struct Passwd<'a> {
    name: &'a str,
    dir: &'a str,
    shell: &'a str,
}

/// Return a Passwd struct with pointers into the provided buf.
fn get_pw_entry(buf: &mut [i8; 1024]) -> Result<Passwd<'_>> {
    let uid = unsafe { libc::getuid() };
    // SAFETY: getpwuid_r initializes the entry and buffer on a successful lookup.
    unsafe {
        get_pw_entry_with(buf, uid, |entry, buf, result| {
            libc::getpwuid_r(uid, entry, buf.as_mut_ptr().cast(), buf.len(), result)
        })
    }
}

/// # Safety
///
/// On success with a non-null result, `lookup` must initialize the passwd entry
/// and its NUL-terminated string fields, backed by `buf` or longer-lived memory.
unsafe fn get_pw_entry_with(
    buf: &mut [i8; 1024],
    uid: libc::uid_t,
    lookup: impl FnOnce(*mut libc::passwd, &mut [i8; 1024], &mut *mut libc::passwd) -> c_int,
) -> Result<Passwd<'_>> {
    let mut entry: MaybeUninit<libc::passwd> = MaybeUninit::uninit();
    let mut res: *mut libc::passwd = ptr::null_mut();

    let status = lookup(entry.as_mut_ptr(), buf, &mut res);
    if status != 0 {
        // POSIX returns the positive error number directly, including ERANGE
        // when this bounded buffer is too small. No passwd fields are valid yet.
        return Err(Error::from_raw_os_error(status));
    }

    if res.is_null() {
        return Err(Error::other("pw not found"));
    }
    let entry = unsafe { entry.assume_init() };

    // Sanity check.
    assert_eq!(entry.pw_uid, uid);

    // Build a borrowed Passwd struct.
    Ok(Passwd {
        name: unsafe { CStr::from_ptr(entry.pw_name).to_str().unwrap() },
        dir: unsafe { CStr::from_ptr(entry.pw_dir).to_str().unwrap() },
        shell: unsafe { CStr::from_ptr(entry.pw_shell).to_str().unwrap() },
    })
}

pub struct Pty {
    child: Option<Child>,
    child_id: u32,
    file: File,
    signals: UnixStream,
    sig_id: SigId,
    child_exit_reported: bool,
}

impl Pty {
    pub fn child(&self) -> Option<&Child> {
        self.child.as_ref()
    }

    /// Transfer child cleanup and reaping to the caller.
    ///
    /// The PTY continues to report exit events without reaping. The caller must
    /// keep the child waitable until the complete session has been cleaned up,
    /// including when PTY setup or its event loop fails.
    pub fn take_child(&mut self) -> Option<Child> {
        self.child.take()
    }

    pub fn file(&self) -> &File {
        &self.file
    }
}

/// User information that is required for a new shell session.
struct ShellUser {
    user: String,
    home: String,
    shell: String,
}

impl ShellUser {
    /// look for shell, username, longname, and home dir in the respective environment variables
    /// before falling back on looking into `passwd`.
    fn from_env() -> Result<Self> {
        let mut buf = [0; 1024];
        let pw = get_pw_entry(&mut buf);

        let user = match env::var("USER") {
            Ok(user) => user,
            Err(_) => match pw {
                Ok(ref pw) => pw.name.to_owned(),
                Err(err) => return Err(err),
            },
        };

        let home = match env::var("HOME") {
            Ok(home) => home,
            Err(_) => match pw {
                Ok(ref pw) => pw.dir.to_owned(),
                Err(err) => return Err(err),
            },
        };

        let shell = match env::var("SHELL") {
            Ok(shell) => shell,
            Err(_) => match pw {
                Ok(ref pw) => pw.shell.to_owned(),
                Err(err) => return Err(err),
            },
        };

        Ok(Self { user, home, shell })
    }
}

#[cfg(not(target_os = "macos"))]
fn default_shell_command(shell: &str, _user: &str, _home: &str) -> Command {
    Command::new(shell)
}

#[cfg(target_os = "macos")]
fn default_shell_command(shell: &str, user: &str, home: &str) -> Command {
    let shell_name = shell.rsplit('/').next().unwrap();

    // On macOS, use the `login` command so the shell will appear as a tty session.
    let mut login_command = Command::new("/usr/bin/login");

    // Exec the shell with argv[0] prepended by '-' so it becomes a login shell.
    // `login` normally does this itself, but `-l` disables this.
    let exec = format!("exec -a -{} {}", shell_name, shell);

    // Since we use -l, `login` will not change directory to the user's home. However,
    // `login` only checks the current working directory for a .hushlogin file, causing
    // it to miss any in the user's home directory. We can fix this by doing the check
    // ourselves and passing `-q`
    let has_home_hushlogin = Path::new(home).join(".hushlogin").exists();

    // -f: Bypasses authentication for the already-logged-in user.
    // -l: Skips changing directory to $HOME and prepending '-' to argv[0].
    // -p: Preserves the environment.
    // -q: Act as if `.hushlogin` exists.
    //
    // XXX: we use zsh here over sh due to `exec -a`.
    let flags = if has_home_hushlogin { "-qflp" } else { "-flp" };
    login_command.args([flags, user, "/bin/zsh", "-fc", &exec]);
    login_command
}

/// Create a new TTY and return a handle to interact with it.
pub fn new(config: &Options, window_size: WindowSize, window_id: u64) -> Result<Pty> {
    let pty = openpty(None, Some(&window_size.to_winsize()))?;
    let (master, slave) = (pty.controller, pty.user);
    from_fd(config, window_id, master, slave)
}

/// Create a new TTY from a PTY's file descriptors.
pub fn from_fd(config: &Options, window_id: u64, master: OwnedFd, slave: OwnedFd) -> Result<Pty> {
    let master_fd = master.as_raw_fd();
    let slave_fd = slave.as_raw_fd();

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Ok(mut termios) = termios::tcgetattr(&master) {
        // Set character encoding to UTF-8.
        termios.input_modes.set(InputModes::IUTF8, true);
        let _ = termios::tcsetattr(&master, OptionalActions::Now, &termios);
    }

    let user = ShellUser::from_env()?;

    let mut builder = if let Some(shell) = config.shell.as_ref() {
        let mut cmd = Command::new(&shell.program);
        cmd.args(shell.args.as_slice());
        cmd
    } else {
        default_shell_command(&user.shell, &user.user, &user.home)
    };

    // Setup child stdin/stdout/stderr as slave fd of PTY.
    builder.stdin(slave.try_clone()?);
    builder.stderr(slave.try_clone()?);
    builder.stdout(slave);

    // Setup shell environment.
    let window_id = window_id.to_string();
    builder.env("ALACRITTY_WINDOW_ID", &window_id);
    builder.env("USER", user.user);
    builder.env("HOME", user.home);
    // Set Window ID for clients relying on X11 hacks.
    builder.env("WINDOWID", window_id);
    for (key, value) in &config.env {
        builder.env(key, value);
    }

    // Prevent child processes from inheriting linux-specific startup notification env.
    builder.env_remove("XDG_ACTIVATION_TOKEN");
    builder.env_remove("DESKTOP_STARTUP_ID");

    if let Some(working_directory) = config.working_directory.as_ref() {
        builder.current_dir(working_directory);
    }

    unsafe {
        builder.pre_exec(move || {
            // Create a new process group.
            let err = libc::setsid();
            if err == -1 {
                return Err(Error::last_os_error());
            }

            set_controlling_terminal(slave_fd)?;

            // No longer need slave/master fds.
            libc::close(slave_fd);
            libc::close(master_fd);

            libc::signal(libc::SIGCHLD, libc::SIG_DFL);
            libc::signal(libc::SIGHUP, libc::SIG_DFL);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGQUIT, libc::SIG_DFL);
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::signal(libc::SIGALRM, libc::SIG_DFL);

            Ok(())
        });
    }

    // Prepare signal handling before spawning child.
    let (signals, sig_id) = {
        let (sender, recv) = UnixStream::pair()?;

        recv.set_nonblocking(true)?;
        // Register the recv end of the pipe for SIGCHLD.
        let sig_id = signal_pipe::register(sigconsts::SIGCHLD, sender)?;
        (recv, sig_id)
    };

    match builder.spawn() {
        Ok(child) => {
            unsafe {
                // Maybe this should be done outside of this function so nonblocking
                // isn't forced upon consumers. Although maybe it should be?
                set_nonblocking(master_fd);
            }

            Ok(Pty {
                child_id: child.id(),
                child: Some(child),
                file: File::from(master),
                signals,
                sig_id,
                child_exit_reported: false,
            })
        },
        Err(err) => {
            unregister_signal(sig_id);
            Err(Error::new(
                err.kind(),
                format!(
                    "Failed to spawn command '{}': {}",
                    builder.get_program().to_string_lossy(),
                    err
                ),
            ))
        },
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Clear signal-hook handler.
        unregister_signal(self.sig_id);

        if let Some(child) = self.child.as_mut() {
            // Callers owning session-wide cleanup take the child first, so
            // dropping the PTY cannot reap its leader before cleanup completes.
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGHUP);
            }
            let _ = child.wait();
        }
    }
}

impl EventedReadWrite for Pty {
    type Reader = File;
    type Writer = File;

    #[inline]
    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        mut interest: Event,
        poll_opts: PollMode,
    ) -> Result<()> {
        interest.key = PTY_READ_WRITE_TOKEN;
        unsafe {
            poll.add_with_mode(&self.file, interest, poll_opts)?;
        }

        unsafe {
            poll.add_with_mode(
                &self.signals,
                Event::readable(PTY_CHILD_EVENT_TOKEN),
                PollMode::Level,
            )
        }
    }

    #[inline]
    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        mut interest: Event,
        poll_opts: PollMode,
    ) -> Result<()> {
        interest.key = PTY_READ_WRITE_TOKEN;
        poll.modify_with_mode(&self.file, interest, poll_opts)?;

        poll.modify_with_mode(
            &self.signals,
            Event::readable(PTY_CHILD_EVENT_TOKEN),
            PollMode::Level,
        )
    }

    #[inline]
    fn deregister(&mut self, poll: &Arc<Poller>) -> Result<()> {
        poll.delete(&self.file)?;
        poll.delete(&self.signals)
    }

    #[inline]
    fn reader(&mut self) -> &mut File {
        &mut self.file
    }

    #[inline]
    fn writer(&mut self) -> &mut File {
        &mut self.file
    }
}

impl EventedPty for Pty {
    #[inline]
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        // Drain all pending notifications, including signals from unrelated
        // children. Leaving data unread would spin the level-triggered poller.
        let mut signaled = false;
        let mut buf = [0u8; 128];
        loop {
            match self.signals.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => signaled = true,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) => {
                    error!("Error reading from signal pipe: {err}");
                    break;
                },
            }
        }
        if !signaled || self.child_exit_reported {
            return None;
        }

        match child_exit_status(self.child_id) {
            Err(err) => {
                error!("Error checking child process termination: {err}");
                None
            },
            Ok(None) => None,
            Ok(Some(exit_status)) => {
                self.child_exit_reported = true;
                Some(ChildEvent::Exited(Some(exit_status)))
            },
        }
    }
}

/// Observe termination without reaping the session leader.
fn child_exit_status(pid: u32) -> Result<Option<ExitStatus>> {
    loop {
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let result = unsafe {
            libc::waitid(libc::P_PID, pid, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT)
        };
        if result == 0 {
            if unsafe { info.si_pid() } == 0 {
                return Ok(None);
            }
            return exit_status_from_info(info.si_code, unsafe { info.si_status() }).map(Some);
        }
        let err = Error::last_os_error();
        if err.kind() != ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

fn exit_status_from_info(code: c_int, status: c_int) -> Result<ExitStatus> {
    let raw = match code {
        libc::CLD_EXITED => status << 8,
        libc::CLD_KILLED => status,
        libc::CLD_DUMPED => status | 0x80,
        _ => {
            return Err(Error::new(ErrorKind::InvalidData, "unexpected child wait status"));
        },
    };
    Ok(ExitStatus::from_raw(raw))
}

impl OnResize for Pty {
    /// Resize the PTY.
    ///
    /// Tells the kernel that the window size changed with the new pixel
    /// dimensions and line/column counts.
    fn on_resize(&mut self, window_size: WindowSize) -> Result<()> {
        let win = window_size.to_winsize();

        let res = unsafe { libc::ioctl(self.file.as_raw_fd(), libc::TIOCSWINSZ, &win as *const _) };

        if res < 0 { Err(Error::last_os_error()) } else { Ok(()) }
    }
}

/// Types that can produce a `Winsize`.
pub trait ToWinsize {
    /// Get a `Winsize`.
    fn to_winsize(self) -> Winsize;
}

impl ToWinsize for WindowSize {
    fn to_winsize(self) -> Winsize {
        let ws_row = self.num_lines as libc::c_ushort;
        let ws_col = self.num_cols as libc::c_ushort;

        let ws_xpixel = ws_col * self.cell_width as libc::c_ushort;
        let ws_ypixel = ws_row * self.cell_height as libc::c_ushort;
        Winsize { ws_row, ws_col, ws_xpixel, ws_ypixel }
    }
}

unsafe fn set_nonblocking(fd: c_int) {
    let res = unsafe { fcntl(fd, F_SETFL, fcntl(fd, F_GETFL, 0) | O_NONBLOCK) };
    assert_eq!(res, 0);
}

#[test]
fn test_get_pw_entry() {
    let mut buf: [i8; 1024] = [0; 1024];
    let _pw = get_pw_entry(&mut buf).unwrap();
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::tty::Shell;

    // SIGCHLD notifications are process-wide, so keep the real-child tests from
    // sending notifications while another test asserts the pipe has been drained.
    static PTY_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn passwd_lookup_failure_does_not_read_uninitialized_entry() {
        for error_code in [libc::EIO, libc::ERANGE] {
            let mut buf = [0; 1024];
            // A failed lookup need not initialize passwd, even if the result
            // pointer is left non-null. Its positive errno must take precedence.
            let error = unsafe {
                get_pw_entry_with(&mut buf, 0, |entry, _, result| {
                    *result = entry;
                    error_code
                })
            }
            .unwrap_err();
            assert_eq!(error.raw_os_error(), Some(error_code));
        }

        let mut buf = [0; 1024];
        // A successful lookup with a null result means no matching account;
        // POSIX does not initialize passwd in this case either.
        let error = unsafe { get_pw_entry_with(&mut buf, 0, |_, _, _| 0) }.unwrap_err();
        assert_eq!(error.to_string(), "pw not found");
    }

    fn window_size() -> WindowSize {
        WindowSize { num_lines: 24, num_cols: 80, cell_width: 8, cell_height: 16 }
    }

    fn options(script: &str) -> Options {
        Options {
            shell: Some(Shell::new("/bin/sh".into(), vec!["-c".into(), script.into()])),
            ..Options::default()
        }
    }

    fn await_exit(pty: &mut Pty) -> ExitStatus {
        let poller = Poller::new().unwrap();
        let mut events = polling::Events::new();
        unsafe {
            poller
                .add_with_mode(
                    &pty.signals,
                    Event::readable(PTY_CHILD_EVENT_TOKEN),
                    PollMode::Level,
                )
                .unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            events.clear();
            poller
                .wait(&mut events, Some(deadline.saturating_duration_since(Instant::now())))
                .unwrap();
            assert!(!events.is_empty(), "PTY child did not notify its exit");
            if let Some(ChildEvent::Exited(Some(status))) = pty.next_child_event() {
                poller.delete(&pty.signals).unwrap();
                return status;
            }
            assert!(Instant::now() < deadline, "PTY child did not report its exit");
        }
    }

    #[test]
    fn child_exit_preserves_native_status_and_waitable_session_until_drop() {
        let _guard = PTY_TEST_LOCK.lock().unwrap();
        for (script, code, signal) in
            [("exit 37", Some(37), None), ("kill -TERM $$", None, Some(libc::SIGTERM))]
        {
            let mut pty = new(&options(script), window_size(), 0).unwrap();
            let pid = pty.child().unwrap().id();
            let status = await_exit(&mut pty);
            assert_eq!(status.code(), code);
            assert_eq!(status.signal(), signal);
            // A second non-reaping observation must still see the original child.
            assert_eq!(child_exit_status(pid).unwrap(), Some(status));
            assert_eq!(unsafe { libc::getsid(pid as libc::pid_t) }, pid as libc::pid_t);

            // Further SIGCHLDs must be drained without reporting the zombie again.
            for _ in 0..256 {
                assert_eq!(unsafe { libc::raise(libc::SIGCHLD) }, 0);
            }
            assert_eq!(pty.next_child_event(), None);
            assert_eq!(pty.next_child_event(), None);
            assert_eq!(pty.signals.read(&mut [0]).unwrap_err().kind(), ErrorKind::WouldBlock);
            drop(pty);
            assert_eq!(child_exit_status(pid).unwrap_err().raw_os_error(), Some(libc::ECHILD));
        }
    }

    #[test]
    fn live_child_has_no_exit_event_and_resize_reports_kernel_result() {
        let _guard = PTY_TEST_LOCK.lock().unwrap();
        let mut pty = new(&options("read value"), window_size(), 0).unwrap();
        assert_eq!(child_exit_status(pty.child().unwrap().id()).unwrap(), None);
        assert_eq!(pty.next_child_event(), None);

        let resized = WindowSize { num_lines: 31, num_cols: 97, cell_width: 9, cell_height: 18 };
        pty.on_resize(resized).unwrap();
        let mut actual = unsafe { std::mem::zeroed::<libc::winsize>() };
        assert_eq!(unsafe { libc::ioctl(pty.file.as_raw_fd(), libc::TIOCGWINSZ, &mut actual) }, 0);
        assert_eq!((actual.ws_row, actual.ws_col), (31, 97));
        assert_eq!((actual.ws_xpixel, actual.ws_ypixel), (873, 558));

        // Retain the real master so replacing the test descriptor does not end
        // the child session before exercising the ioctl error path.
        let _master = std::mem::replace(&mut pty.file, File::open("/dev/null").unwrap());
        assert_eq!(pty.on_resize(resized).unwrap_err().raw_os_error(), Some(libc::ENOTTY));
    }

    #[test]
    fn transferred_child_remains_waitable_after_pty_drop() {
        let _guard = PTY_TEST_LOCK.lock().unwrap();
        let mut pty = new(&options("exit 41"), window_size(), 0).unwrap();
        let mut child = pty.take_child().unwrap();
        let pid = child.id();
        assert!(pty.child().is_none());
        assert!(pty.take_child().is_none());

        let status = await_exit(&mut pty);
        assert_eq!(status.code(), Some(41));
        drop(pty);
        assert_eq!(child_exit_status(pid).unwrap(), Some(status));
        assert_eq!(unsafe { libc::getsid(pid as libc::pid_t) }, pid as libc::pid_t);
        assert_eq!(child.wait().unwrap(), status);
        assert_eq!(child_exit_status(pid).unwrap_err().raw_os_error(), Some(libc::ECHILD));

        // An early event-loop/setup failure drops the PTY before receiving an
        // exit notification. The caller must retain its session leader then too.
        let mut pty = new(&options("read value"), window_size(), 0).unwrap();
        let mut child = pty.take_child().unwrap();
        let pid = child.id();
        drop(pty);
        assert_eq!(unsafe { libc::getsid(pid as libc::pid_t) }, pid as libc::pid_t);
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGHUP));
        assert_eq!(child_exit_status(pid).unwrap_err().raw_os_error(), Some(libc::ECHILD));
    }

    #[test]
    fn configured_working_directory_is_used_or_spawn_fails() {
        let _guard = PTY_TEST_LOCK.lock().unwrap();
        let mut config = options("test \"$PWD\" = /");
        config.working_directory = Some("/".into());
        let mut pty = new(&config, window_size(), 0).unwrap();
        assert!(await_exit(&mut pty).success());
        drop(pty);

        config.working_directory = Some("/dev/null/invalid-directory".into());
        let error = new(&config, window_size(), 0).err().expect("invalid cwd must fail spawn");
        assert_eq!(error.kind(), ErrorKind::NotADirectory);

        config.working_directory =
            Some(std::ffi::OsString::from_vec(b"/tmp/invalid\0cwd".to_vec()).into());
        let error = new(&config, window_size(), 0).err().expect("NUL cwd must fail spawn");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn waitid_status_conversion_preserves_exit_codes_signals_and_core_flag() {
        for code in 0..=255 {
            let status = exit_status_from_info(libc::CLD_EXITED, code).unwrap();
            assert_eq!(status.code(), Some(code));
            assert_eq!(status.signal(), None);
            assert!(!status.core_dumped());
        }
        for signal in [libc::SIGHUP, libc::SIGTERM, libc::SIGKILL, libc::SIGABRT] {
            for (reason, core_dumped) in [(libc::CLD_KILLED, false), (libc::CLD_DUMPED, true)] {
                let status = exit_status_from_info(reason, signal).unwrap();
                assert_eq!(status.code(), None);
                assert_eq!(status.signal(), Some(signal));
                assert_eq!(status.core_dumped(), core_dumped);
            }
        }
        assert_eq!(
            exit_status_from_info(libc::CLD_STOPPED, libc::SIGSTOP).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }
}
