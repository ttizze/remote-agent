//! Host-owned suspend/resume notifications.
//!
//! Each platform source reports OS lifecycle messages directly.  The caller
//! owns the resulting power state; this module only handles subscription,
//! cancellation, and decoding.  When a platform does not expose a source,
//! the caller remains on the stale/observed-power contract instead of
//! inferring suspend from elapsed time.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const LIFECYCLE_BUFFER: usize = 8;

#[cfg(target_os = "macos")]
const MAC_MESSAGE_CAN_SLEEP: u32 = 0xe000_0270;
#[cfg(target_os = "macos")]
const MAC_MESSAGE_WILL_SLEEP: u32 = 0xe000_0280;
#[cfg(target_os = "macos")]
const MAC_MESSAGE_HAS_POWERED_ON: u32 = 0xe000_0300;

#[cfg(target_os = "macos")]
fn mac_notification_id(message_argument: *mut std::ffi::c_void) -> isize {
    message_argument as isize
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SuspendLifecycleEvent {
    Suspended,
    Resumed,
}

impl SuspendLifecycleEvent {
    pub(crate) fn suspended(self) -> bool {
        matches!(self, Self::Suspended)
    }
}

struct LifecycleMailboxState {
    events: VecDeque<SuspendLifecycleEvent>,
    closed: bool,
}

struct LifecycleMailbox {
    capacity: usize,
    state: Mutex<LifecycleMailboxState>,
    notify: Notify,
}

impl LifecycleMailbox {
    fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity: capacity.max(1),
            state: Mutex::new(LifecycleMailboxState {
                events: VecDeque::new(),
                closed: false,
            }),
            notify: Notify::new(),
        })
    }

    fn publish(&self, event: SuspendLifecycleEvent) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed {
            return false;
        }
        if state.events.back() == Some(&event) {
            return true;
        }
        if state.events.len() >= self.capacity {
            state.events.pop_front();
        }
        state.events.push_back(event);
        drop(state);
        self.notify.notify_one();
        true
    }

    fn take(&self) -> Option<SuspendLifecycleEvent> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .events
            .pop_front()
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        drop(state);
        self.notify.notify_waiters();
    }

    fn is_closed(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closed
    }

    async fn recv(&self, stop: &CancellationToken) -> Option<SuspendLifecycleEvent> {
        loop {
            let notified = self.notify.notified();
            if self.is_closed() || stop.is_cancelled() {
                return None;
            }
            if let Some(event) = self.take() {
                return Some(event);
            }
            tokio::select! {
                _ = stop.cancelled() => return None,
                _ = notified => {}
            }
        }
    }
}

pub(crate) struct SuspendLifecycleSource {
    mailbox: Arc<LifecycleMailbox>,
    stop: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl SuspendLifecycleSource {
    pub(crate) fn start(owner_stop: &CancellationToken) -> Self {
        let mailbox = LifecycleMailbox::new(LIFECYCLE_BUFFER);
        let run_mailbox = mailbox.clone();
        let close_mailbox = mailbox.clone();
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let owner_stop = owner_stop.clone();
        let task = tokio::spawn(async move {
            let run = run_platform_source(run_mailbox, task_stop.clone());
            tokio::pin!(run);
            tokio::select! {
                _ = owner_stop.cancelled() => {
                    task_stop.cancel();
                    let _ = run.await;
                }
                _ = &mut run => {}
            }
            close_mailbox.close();
        });
        Self {
            mailbox,
            stop,
            task: Some(task),
        }
    }

    pub(crate) async fn recv(&mut self) -> Option<SuspendLifecycleEvent> {
        self.mailbox.recv(&self.stop).await
    }

    pub(crate) async fn shutdown(&mut self) {
        self.mailbox.close();
        self.stop.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for SuspendLifecycleSource {
    fn drop(&mut self) {
        self.mailbox.close();
        self.stop.cancel();
    }
}

async fn run_platform_source(mailbox: Arc<LifecycleMailbox>, stop: CancellationToken) {
    #[cfg(target_os = "linux")]
    {
        run_linux_suspend_lifecycle_source(mailbox, stop).await;
        return;
    }

    #[cfg(target_os = "macos")]
    {
        run_macos_suspend_lifecycle_source(mailbox, stop).await;
    }

    #[cfg(target_os = "windows")]
    {
        run_windows_suspend_lifecycle_source(mailbox, stop).await;
        return;
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        stop.cancelled().await;
    }
}

#[cfg(any(target_os = "linux", test))]
fn send_lifecycle_event(
    mailbox: &LifecycleMailbox,
    stop: &CancellationToken,
    event: SuspendLifecycleEvent,
) -> bool {
    !stop.is_cancelled() && mailbox.publish(event)
}

#[cfg(target_os = "linux")]
async fn run_linux_suspend_lifecycle_source(
    mailbox: Arc<LifecycleMailbox>,
    stop: CancellationToken,
) {
    use futures_util::StreamExt;

    const RETRY: Duration = Duration::from_secs(30);
    loop {
        if stop.is_cancelled() {
            return;
        }
        let connection = tokio::select! {
            _ = stop.cancelled() => return,
            connection = zbus::Connection::system() => match connection {
                Ok(connection) => connection,
                Err(error) => {
                    tracing::debug!(target: "bex", operation = "background.power.lifecycle.connect", message = %error);
                    tokio::select! {
                        _ = stop.cancelled() => return,
                        _ = tokio::time::sleep(RETRY) => continue,
                    }
                }
            },
        };
        let proxy = match tokio::select! {
            _ = stop.cancelled() => return,
            proxy = zbus::Proxy::new(
                &connection,
                "org.freedesktop.login1",
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
            ) => proxy,
        } {
            Ok(proxy) => proxy,
            Err(error) => {
                tracing::debug!(target: "bex", operation = "background.power.lifecycle.proxy", message = %error);
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(RETRY) => continue,
                }
            }
        };
        let mut signals = match tokio::select! {
            _ = stop.cancelled() => return,
            signals = proxy.receive_signal("PrepareForSleep") => signals,
        } {
            Ok(signals) => signals,
            Err(error) => {
                tracing::debug!(target: "bex", operation = "background.power.lifecycle.subscribe", message = %error);
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(RETRY) => continue,
                }
            }
        };
        loop {
            tokio::select! {
                _ = stop.cancelled() => return,
                signal = signals.next() => match signal {
                    Some(signal) => match signal.body().deserialize::<bool>() {
                        Ok(suspended) => {
                            let event = if suspended {
                                SuspendLifecycleEvent::Suspended
                            } else {
                                SuspendLifecycleEvent::Resumed
                            };
                            if !send_lifecycle_event(&mailbox, &stop, event) {
                                return;
                            }
                        }
                        Err(error) => tracing::debug!(target: "bex", operation = "background.power.lifecycle.decode", message = %error),
                    },
                    None => break,
                },
            }
        }
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
}

#[cfg(target_os = "macos")]
async fn run_macos_suspend_lifecycle_source(
    mailbox: Arc<LifecycleMailbox>,
    stop: CancellationToken,
) {
    const RETRY: Duration = Duration::from_secs(30);
    loop {
        if stop.is_cancelled() {
            return;
        }
        let watcher_stop = stop.clone();
        let watcher_mailbox = mailbox.clone();
        let watcher = tokio::task::spawn_blocking(move || {
            run_macos_power_watcher(watcher_mailbox, watcher_stop);
        });
        let _ = watcher.await;
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(RETRY) => {}
        }
    }
}

#[cfg(target_os = "macos")]
fn run_macos_power_watcher(mailbox: Arc<LifecycleMailbox>, stop: CancellationToken) {
    use std::{ffi::c_void, ptr};

    type IoObject = u32;
    type IoConnect = u32;
    type NotificationPort = *mut c_void;
    type RunLoop = *mut c_void;
    type RunLoopSource = *mut c_void;
    type StringRef = *const c_void;

    #[repr(C)]
    struct CallbackContext {
        mailbox: Arc<LifecycleMailbox>,
        root_port: IoConnect,
    }

    unsafe extern "C" fn callback(
        refcon: *mut c_void,
        _service: IoObject,
        message_type: u32,
        message_argument: *mut c_void,
    ) {
        let context = unsafe { &mut *(refcon.cast::<CallbackContext>()) };
        match message_type {
            MAC_MESSAGE_CAN_SLEEP | MAC_MESSAGE_WILL_SLEEP => {
                if message_type == MAC_MESSAGE_WILL_SLEEP {
                    let event = SuspendLifecycleEvent::Suspended;
                    let _ = context.mailbox.publish(event);
                }
                let _ = unsafe {
                    IOAllowPowerChange(context.root_port, mac_notification_id(message_argument))
                };
            }
            MAC_MESSAGE_HAS_POWERED_ON => {
                let event = SuspendLifecycleEvent::Resumed;
                let _ = context.mailbox.publish(event);
            }
            _ => {}
        }
    }

    unsafe {
        let mut notify_port: NotificationPort = ptr::null_mut();
        let mut notifier: IoObject = 0;
        let context = Box::new(CallbackContext {
            mailbox,
            root_port: 0,
        });
        let context_ptr = Box::into_raw(context);
        let root_port = IORegisterForSystemPower(
            context_ptr.cast(),
            &mut notify_port,
            callback,
            &mut notifier,
        );
        if root_port == 0 || notify_port.is_null() {
            if notifier != 0 {
                let _ = IODeregisterForSystemPower(&mut notifier);
            }
            if root_port != 0 {
                let _ = IOServiceClose(root_port);
            }
            if !notify_port.is_null() {
                IONotificationPortDestroy(notify_port);
            }
            drop(Box::from_raw(context_ptr));
            return;
        }
        (*context_ptr).root_port = root_port;
        let run_loop = CFRunLoopGetCurrent();
        let source = IONotificationPortGetRunLoopSource(notify_port);
        if source.is_null() {
            if notifier != 0 {
                let _ = IODeregisterForSystemPower(&mut notifier);
            }
            let _ = IOServiceClose(root_port);
            IONotificationPortDestroy(notify_port);
            drop(Box::from_raw(context_ptr));
            return;
        }
        CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);
        while !stop.is_cancelled() {
            let _ = CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 1);
        }
        if !source.is_null() {
            CFRunLoopRemoveSource(run_loop, source, kCFRunLoopDefaultMode);
        }
        if notifier != 0 {
            let _ = IODeregisterForSystemPower(&mut notifier);
        }
        let _ = IOServiceClose(root_port);
        IONotificationPortDestroy(notify_port);
        drop(Box::from_raw(context_ptr));
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IORegisterForSystemPower(
            refcon: *mut c_void,
            notify_port: *mut NotificationPort,
            callback: unsafe extern "C" fn(*mut c_void, IoObject, u32, *mut c_void),
            notifier: *mut IoObject,
        ) -> IoConnect;
        fn IONotificationPortGetRunLoopSource(port: NotificationPort) -> RunLoopSource;
        fn IONotificationPortDestroy(port: NotificationPort);
        fn IODeregisterForSystemPower(notifier: *mut IoObject) -> i32;
        fn IOAllowPowerChange(root_port: IoConnect, notification_id: isize) -> i32;
        fn IOServiceClose(root_port: IoConnect) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopGetCurrent() -> RunLoop;
        fn CFRunLoopAddSource(run_loop: RunLoop, source: RunLoopSource, mode: StringRef);
        fn CFRunLoopRemoveSource(run_loop: RunLoop, source: RunLoopSource, mode: StringRef);
        fn CFRunLoopRunInMode(mode: StringRef, seconds: f64, return_after_source: u8) -> i32;
        static kCFRunLoopDefaultMode: StringRef;
    }
}

#[cfg(target_os = "windows")]
async fn run_windows_suspend_lifecycle_source(
    mailbox: Arc<LifecycleMailbox>,
    stop: CancellationToken,
) {
    const RETRY: Duration = Duration::from_secs(30);
    loop {
        if stop.is_cancelled() {
            return;
        }
        let watcher_stop = stop.clone();
        let watcher_mailbox = mailbox.clone();
        let watcher = tokio::task::spawn_blocking(move || {
            run_windows_power_watcher(watcher_mailbox, watcher_stop);
        });
        let _ = watcher.await;
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(RETRY) => {}
        }
    }
}

#[cfg(target_os = "windows")]
fn run_windows_power_watcher(mailbox: Arc<LifecycleMailbox>, stop: CancellationToken) {
    // The worker owns a message-only HWND and registers that HWND with the
    // power manager.  The message loop is the documented Windows power
    // notification source; no idle-frequency heuristic is involved.
    use std::{mem, ptr, time::Duration};
    use windows_sys::Win32::{
        Foundation::{GetLastError, HINSTANCE, LPARAM, LRESULT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            Power::{RegisterSuspendResumeNotification, UnregisterSuspendResumeNotification},
            Threading::GetCurrentThreadId,
        },
        UI::WindowsAndMessaging::{
            CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE,
            DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetMessageW,
            GetWindowLongPtrW, HWND_MESSAGE, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMECRITICAL,
            PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
            RegisterClassW, SetWindowLongPtrW, TranslateMessage, UnregisterClassW, WM_NCCREATE,
            WM_NCDESTROY, WM_POWERBROADCAST, WM_QUIT, WNDCLASSW,
        },
    };

    struct WindowContext {
        mailbox: Arc<LifecycleMailbox>,
    }
    const CLASS_NAME: [u16; 17] = [
        82, 101, 109, 111, 116, 101, 65, 103, 101, 110, 116, 80, 111, 119, 101, 114, 0,
    ];

    unsafe extern "system" fn window_proc(
        window: windows_sys::Win32::Foundation::HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_NCCREATE {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        if message == WM_POWERBROADCAST {
            let event = match wparam as u32 {
                PBT_APMSUSPEND => Some(true),
                PBT_APMRESUMECRITICAL | PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
                    Some(false)
                }
                _ => None,
            };
            if let Some(event) = event {
                let context = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut WindowContext;
                if !context.is_null() {
                    let event = if event {
                        SuspendLifecycleEvent::Suspended
                    } else {
                        SuspendLifecycleEvent::Resumed
                    };
                    let context = &mut *context;
                    let _ = context.mailbox.publish(event);
                }
            }
            return 1;
        }
        if message == WM_NCDESTROY {
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            return DefWindowProcW(window, message, wparam, lparam);
        }
        DefWindowProcW(window, message, wparam, lparam)
    }

    unsafe {
        let thread_id = GetCurrentThreadId();
        let instance: HINSTANCE = GetModuleHandleW(ptr::null());
        if instance.is_null() {
            tracing::debug!(target: "bex", operation = "background.power.lifecycle.windows.module", error = GetLastError());
            return;
        }
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: CLASS_NAME.as_ptr(),
            ..mem::zeroed()
        };
        let _ = RegisterClassW(&class);
        let context = Box::new(WindowContext { mailbox });
        let window = CreateWindowExW(
            0,
            CLASS_NAME.as_ptr(),
            CLASS_NAME.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            0,
            instance,
            (&*context as *const WindowContext).cast(),
        );
        if window.is_null() {
            UnregisterClassW(CLASS_NAME.as_ptr(), instance);
            return;
        }
        let registration = RegisterSuspendResumeNotification(window, DEVICE_NOTIFY_WINDOW_HANDLE);
        if registration == 0 {
            DestroyWindow(window);
            UnregisterClassW(CLASS_NAME.as_ptr(), instance);
            drop(context);
            return;
        }
        let mut message = MSG::default();
        PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_NOREMOVE);
        let watcher_stop = CancellationToken::new();
        let helper_stop = watcher_stop.clone();
        let parent_stop = stop.clone();
        let stop_thread = std::thread::spawn(move || {
            while !parent_stop.is_cancelled() && !helper_stop.is_cancelled() {
                std::thread::sleep(Duration::from_millis(10));
            }
            helper_stop.cancel();
            let _ = PostThreadMessageW(thread_id, WM_QUIT, 0, 0);
        });
        loop {
            let status = GetMessageW(&mut message, ptr::null_mut(), 0, 0);
            if status <= 0 {
                break;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        watcher_stop.cancel();
        let _ = stop_thread.join();
        UnregisterSuspendResumeNotification(registration);
        DestroyWindow(window);
        UnregisterClassW(CLASS_NAME.as_ptr(), instance);
        drop(context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_event_mapping_keeps_os_meaning() {
        assert!(SuspendLifecycleEvent::Suspended.suspended());
        assert!(!SuspendLifecycleEvent::Resumed.suspended());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_power_constants_and_notification_ids_match_iokit() {
        assert_eq!(MAC_MESSAGE_CAN_SLEEP, 0xe000_0270);
        assert_eq!(MAC_MESSAGE_WILL_SLEEP, 0xe000_0280);
        assert_eq!(MAC_MESSAGE_HAS_POWERED_ON, 0xe000_0300);
        let raw = 0x1234usize as *mut std::ffi::c_void;
        assert_eq!(mac_notification_id(raw), 0x1234isize);
    }

    #[test]
    fn lifecycle_channel_coalesces_duplicates_without_dropping_wake() {
        let mailbox = LifecycleMailbox::new(2);
        let stop = CancellationToken::new();
        assert!(send_lifecycle_event(
            &mailbox,
            &stop,
            SuspendLifecycleEvent::Suspended,
        ));
        assert!(send_lifecycle_event(
            &mailbox,
            &stop,
            SuspendLifecycleEvent::Suspended,
        ));
        assert!(send_lifecycle_event(
            &mailbox,
            &stop,
            SuspendLifecycleEvent::Resumed,
        ));
        assert_eq!(mailbox.take(), Some(SuspendLifecycleEvent::Suspended));
        assert_eq!(mailbox.take(), Some(SuspendLifecycleEvent::Resumed));
    }

    #[test]
    fn saturated_callback_keeps_latest_alternating_state_without_blocking() {
        let mailbox = LifecycleMailbox::new(2);
        let producer_mailbox = mailbox.clone();
        std::thread::spawn(move || {
            assert!(producer_mailbox.publish(SuspendLifecycleEvent::Suspended));
            assert!(producer_mailbox.publish(SuspendLifecycleEvent::Resumed));
            assert!(producer_mailbox.publish(SuspendLifecycleEvent::Suspended));
        })
        .join()
        .expect("saturated callback producer blocked");
        assert_eq!(mailbox.take(), Some(SuspendLifecycleEvent::Resumed));
        assert_eq!(mailbox.take(), Some(SuspendLifecycleEvent::Suspended));
    }

    #[test]
    fn startup_failure_closes_callback_sink_without_waiting_for_a_reader() {
        let mailbox = LifecycleMailbox::new(LIFECYCLE_BUFFER);
        mailbox.close();
        assert!(!mailbox.publish(SuspendLifecycleEvent::Suspended));
    }

    #[tokio::test]
    async fn message_loop_error_joins_owned_helper_without_canceling_owner() {
        let owner_stop = CancellationToken::new();
        let watcher_stop = CancellationToken::new();
        let mailbox = LifecycleMailbox::new(2);
        assert!(mailbox.publish(SuspendLifecycleEvent::Suspended));
        assert!(mailbox.publish(SuspendLifecycleEvent::Resumed));
        let helper_stop = watcher_stop.clone();
        let helper = std::thread::spawn(move || {
            while !helper_stop.is_cancelled() {
                std::thread::yield_now();
            }
        });
        let started = std::time::Instant::now();
        let fake_message_result = -1;
        if fake_message_result <= 0 {
            watcher_stop.cancel();
        }
        mailbox.close();
        let joined = tokio::task::spawn_blocking(move || helper.join().is_ok());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), joined)
                .await
                .expect("message-loop error did not join watcher helper")
                .expect("watcher helper join task failed")
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!owner_stop.is_cancelled());
        let stop = CancellationToken::new();
        assert_eq!(mailbox.recv(&stop).await, None);
        assert!(!mailbox.publish(SuspendLifecycleEvent::Suspended));
    }

    #[tokio::test]
    async fn canceled_source_closes_after_its_worker_stops() {
        let stop = CancellationToken::new();
        let mut source = SuspendLifecycleSource::start(&stop);
        stop.cancel();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), source.shutdown())
                .await
                .is_ok()
        );
        assert!(source.recv().await.is_none());
    }
}
