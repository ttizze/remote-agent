use std::ffi::c_void;
use std::io::Error;
use std::num::NonZeroU32;
use std::os::windows::process::ExitStatusExt;
use std::process::ExitStatus;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use polling::os::iocp::{CompletionPacket, PollerIocpExt};
use polling::{Event, Poller};

use windows_sys::Win32::Foundation::{BOOLEAN, FALSE, HANDLE};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessId, INFINITE, RegisterWaitForSingleObject, UnregisterWait,
    WT_EXECUTEINWAITTHREAD, WT_EXECUTEONLYONCE,
};

use crate::tty::ChildEvent;

struct Interest {
    poller: Arc<Poller>,
    event: Event,
}

#[derive(Default)]
struct Registration {
    interest: Option<Interest>,
    exited: bool,
}

struct ChildExitSender {
    sender: mpsc::Sender<ChildEvent>,
    registration: Arc<Mutex<Registration>>,
    child_handle: AtomicPtr<c_void>,
}

/// WinAPI callback to run when child process exits.
extern "system" fn child_exit_callback(ctx: *mut c_void, timed_out: BOOLEAN) {
    if timed_out != 0 {
        return;
    }

    let event_tx: Box<_> = unsafe { Box::from_raw(ctx as *mut ChildExitSender) };

    let mut exit_code = 0_u32;
    let child_handle = event_tx.child_handle.load(Ordering::Relaxed) as HANDLE;
    let status = unsafe { GetExitCodeProcess(child_handle, &mut exit_code) };
    let exit_status = if status == FALSE { None } else { Some(ExitStatus::from_raw(exit_code)) };
    event_tx.sender.send(ChildEvent::Exited(exit_status)).ok();

    let mut registration = event_tx.registration.lock().unwrap();
    registration.exited = true;
    if let Some(interest) = registration.interest.as_ref() {
        interest.poller.post(CompletionPacket::new(interest.event)).ok();
    }
}

pub struct ChildExitWatcher {
    wait_handle: AtomicPtr<c_void>,
    event_rx: mpsc::Receiver<ChildEvent>,
    registration: Arc<Mutex<Registration>>,
    child_handle: AtomicPtr<c_void>,
    pid: Option<NonZeroU32>,
}

impl ChildExitWatcher {
    pub fn new(child_handle: HANDLE) -> Result<ChildExitWatcher, Error> {
        let (event_tx, event_rx) = mpsc::channel();

        let mut wait_handle: HANDLE = ptr::null_mut();
        let registration = Arc::new(Mutex::new(Registration::default()));
        let sender_ref = Box::new(ChildExitSender {
            sender: event_tx,
            registration: registration.clone(),
            child_handle: AtomicPtr::from(child_handle),
        });

        let success = unsafe {
            RegisterWaitForSingleObject(
                &mut wait_handle,
                child_handle,
                Some(child_exit_callback),
                Box::into_raw(sender_ref).cast(),
                INFINITE,
                WT_EXECUTEINWAITTHREAD | WT_EXECUTEONLYONCE,
            )
        };

        if success == 0 {
            Err(Error::last_os_error())
        } else {
            let pid = unsafe { NonZeroU32::new(GetProcessId(child_handle)) };
            Ok(ChildExitWatcher {
                event_rx,
                registration,
                pid,
                child_handle: AtomicPtr::from(child_handle),
                wait_handle: AtomicPtr::from(wait_handle),
            })
        }
    }

    pub fn next_event(&self) -> Option<ChildEvent> {
        let mut registration = self.registration.lock().unwrap();
        if !registration.exited {
            return None;
        }
        // The callback queues the exit before latching readiness. Consume it
        // once, so re-registration during the final output drain cannot report
        // another exit merely because the callback's sender was dropped.
        let event = self.event_rx.try_recv().ok();
        registration.exited = false;
        event
    }

    pub fn register(&self, poller: &Arc<Poller>, event: Event) {
        let mut registration = self.registration.lock().unwrap();
        registration.interest = Some(Interest { poller: poller.clone(), event });
        // A short-lived process can exit before its first registration. Latch
        // readiness under the same lock as the callback to avoid a lost wakeup.
        if registration.exited {
            poller.post(CompletionPacket::new(event)).ok();
        }
    }

    pub fn deregister(&self) {
        self.registration.lock().unwrap().interest = None;
    }

    /// Retrieve the process handle of the underlying child process.
    ///
    /// This function does **not** pass ownership of the raw handle to you,
    /// and the handle is only guaranteed to be valid while the hosted application
    /// has not yet been destroyed.
    ///
    /// If you terminate the process using this handle, the terminal will get a
    /// timeout error, and the child watcher will emit an `Exited` event.
    pub fn raw_handle(&self) -> HANDLE {
        self.child_handle.load(Ordering::Relaxed) as HANDLE
    }

    /// Retrieve the Process ID associated to the underlying child process.
    pub fn pid(&self) -> Option<NonZeroU32> {
        self.pid
    }
}

impl Drop for ChildExitWatcher {
    fn drop(&mut self) {
        unsafe {
            UnregisterWait(self.wait_handle.load(Ordering::Relaxed) as HANDLE);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::windows::io::AsRawHandle;
    use std::process::Command;
    use std::sync::Arc;
    use std::time::Duration;

    use super::super::PTY_CHILD_EVENT_TOKEN;
    use super::*;

    #[test]
    fn registration_replays_an_exit_that_was_already_queued() {
        let mut child = Command::new("cmd.exe").args(["/C", "exit", "7"]).spawn().unwrap();
        let watcher = ChildExitWatcher::new(child.as_raw_handle() as HANDLE).unwrap();
        child.wait().unwrap();

        // Wait for the callback to queue its event without consuming that event.
        let timeout = Duration::from_secs(5);
        let deadline = std::time::Instant::now() + timeout;
        while !watcher.registration.lock().unwrap().exited {
            assert!(std::time::Instant::now() < deadline, "exit callback did not run");
            std::thread::yield_now();
        }
        let poller = Arc::new(Poller::new().unwrap());
        watcher.register(&poller, Event::readable(PTY_CHILD_EVENT_TOKEN));
        let mut events = polling::Events::new();
        poller.wait(&mut events, Some(timeout)).unwrap();
        assert!(events.iter().any(|event| event.key == PTY_CHILD_EVENT_TOKEN));
        assert_eq!(
            watcher.next_event(),
            Some(ChildEvent::Exited(Some(ExitStatus::from_raw(7)))),
        );
        assert_eq!(watcher.next_event(), None);
        watcher.register(&poller, Event::readable(PTY_CHILD_EVENT_TOKEN));
        events.clear();
        poller.wait(&mut events, Some(Duration::from_millis(20))).unwrap();
        assert!(events.is_empty(), "consumed exit was reported again");
    }

    #[test]
    pub fn event_is_emitted_when_child_exits() {
        const WAIT_TIMEOUT: Duration = Duration::from_millis(200);

        let poller = Arc::new(Poller::new().unwrap());

        let mut child = Command::new("cmd.exe").spawn().unwrap();
        let child_exit_watcher = ChildExitWatcher::new(child.as_raw_handle() as HANDLE).unwrap();
        child_exit_watcher.register(&poller, Event::readable(PTY_CHILD_EVENT_TOKEN));

        child.kill().unwrap();

        // Poll for the event or fail with timeout if nothing has been sent.
        let mut events = polling::Events::new();
        poller.wait(&mut events, Some(WAIT_TIMEOUT)).unwrap();
        assert_eq!(events.iter().next().unwrap().key, PTY_CHILD_EVENT_TOKEN);
        // Verify that at least one `ChildEvent::Exited` was received.
        let expected_status = ExitStatus::from_raw(1);
        assert_eq!(
            child_exit_watcher.next_event(),
            Some(ChildEvent::Exited(Some(expected_status)))
        );
    }
}
