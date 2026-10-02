//! A PTY session contains multiple job-control process groups. Keep its leader
//! waitable until cleanup completes so its session ID cannot be reused.
use std::{
    io,
    time::{Duration, Instant},
};

pub(super) fn terminate(session: libc::pid_t) -> io::Result<()> {
    let started = Instant::now();
    loop {
        let mut live = false;
        for pid in process_ids()? {
            if unsafe { libc::getsid(pid) } != session || !is_live(pid)? {
                continue;
            }
            live = true;
            // Include foreground and background jobs, even after the shell
            // exits. Rescan to cover children forked during the previous scan.
            if unsafe { libc::getsid(pid) } == session
                && unsafe { libc::kill(pid, libc::SIGKILL) } != 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
        }
        if !live {
            return Ok(());
        }
        if started.elapsed() >= Duration::from_secs(10) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "PTY session cleanup incomplete",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn process_ids() -> io::Result<Vec<libc::pid_t>> {
    std::fs::read_dir("/proc")?
        .filter_map(|entry| match entry {
            Ok(entry) => entry.file_name().to_str()?.parse().ok().map(Ok),
            Err(error) => Some(Err(error)),
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn is_live(pid: libc::pid_t) -> io::Result<bool> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    // comm is parenthesized and can itself contain spaces and parentheses.
    let state = stat
        .rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().next());
    match state {
        Some("Z" | "X" | "x") => Ok(false),
        Some(_) => Ok(true),
        None => Err(io::Error::other("invalid process status")),
    }
}

#[cfg(target_os = "macos")]
fn process_ids() -> io::Result<Vec<libc::pid_t>> {
    let mut capacity = 1024;
    loop {
        let mut pids = vec![0; capacity];
        let count = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr().cast(),
                std::mem::size_of_val(pids.as_slice()) as i32,
            )
        };
        if count <= 0 {
            return Err(io::Error::last_os_error());
        }
        if (count as usize) < capacity {
            pids.truncate(count as usize);
            pids.retain(|pid| *pid > 0);
            return Ok(pids);
        }
        capacity *= 2;
    }
}

#[cfg(target_os = "macos")]
fn is_live(pid: libc::pid_t) -> io::Result<bool> {
    let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
    let size = std::mem::size_of_val(&info) as i32;
    if unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    } == size
    {
        return Ok(info.pbi_status != libc::SZOMB);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_ids() -> io::Result<Vec<libc::pid_t>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "PTY session cleanup is unsupported on this OS",
    ))
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn is_live(_: libc::pid_t) -> io::Result<bool> {
    Ok(true)
}

/// Keep the native child unreaped until all of its job-control groups are gone.
pub(super) struct Session {
    child: std::process::Child,
    finished: bool,
}
impl Session {
    pub(super) fn new(child: std::process::Child) -> Self {
        Self {
            child,
            finished: false,
        }
    }
    pub(super) fn finish(&mut self) -> io::Result<()> {
        if !self.finished {
            terminate(self.child.id() as libc::pid_t)?;
            self.child.wait()?;
            self.finished = true;
        }
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
