//! Host-owned suspend/resume notifications.
//!
//! Each platform source reports OS lifecycle messages directly.  The caller
//! owns the resulting power state; this module only handles subscription,
//! cancellation, and decoding.  When a platform does not expose a source,
//! the caller remains on the stale/observed-power contract instead of
//! inferring suspend from elapsed time.

#[cfg(target_os = "linux")]
use std::time::Duration;
use tokio_util::sync::CancellationToken;

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

pub(crate) async fn next_suspend_lifecycle_event(
    stop: &CancellationToken,
) -> Option<SuspendLifecycleEvent> {
    #[cfg(target_os = "linux")]
    {
        return next_linux_suspend_lifecycle_event(stop).await;
    }

    #[cfg(target_os = "macos")]
    {
        return next_macos_suspend_lifecycle_event(stop).await;
    }

    #[cfg(target_os = "windows")]
    {
        return next_windows_suspend_lifecycle_event(stop).await;
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        stop.cancelled().await;
        None
    }
}

#[cfg(target_os = "linux")]
async fn next_linux_suspend_lifecycle_event(
    stop: &CancellationToken,
) -> Option<SuspendLifecycleEvent> {
    use futures_util::StreamExt;

    const RETRY: Duration = Duration::from_secs(30);
    loop {
        if stop.is_cancelled() {
            return None;
        }
        let connection = tokio::select! {
            _ = stop.cancelled() => return None,
            connection = zbus::Connection::system() => match connection {
                Ok(connection) => connection,
                Err(error) => {
                    tracing::debug!(target: "bex", operation = "background.power.lifecycle.connect", message = %error);
                    tokio::select! {
                        _ = stop.cancelled() => return None,
                        _ = tokio::time::sleep(RETRY) => continue,
                    }
                }
            },
        };
        let proxy = match tokio::select! {
            _ = stop.cancelled() => return None,
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
                    _ = stop.cancelled() => return None,
                    _ = tokio::time::sleep(RETRY) => continue,
                }
            }
        };
        let mut signals = match tokio::select! {
            _ = stop.cancelled() => return None,
            signals = proxy.receive_signal("PrepareForSleep") => signals,
        } {
            Ok(signals) => signals,
            Err(error) => {
                tracing::debug!(target: "bex", operation = "background.power.lifecycle.subscribe", message = %error);
                tokio::select! {
                    _ = stop.cancelled() => return None,
                    _ = tokio::time::sleep(RETRY) => continue,
                }
            }
        };
        loop {
            tokio::select! {
                _ = stop.cancelled() => return None,
                signal = signals.next() => match signal {
                    Some(Ok(signal)) => match signal.body().deserialize::<bool>() {
                        Ok(true) => return Some(SuspendLifecycleEvent::Suspended),
                        Ok(false) => return Some(SuspendLifecycleEvent::Resumed),
                        Err(error) => tracing::debug!(target: "bex", operation = "background.power.lifecycle.decode", message = %error),
                    },
                    Some(Err(error)) => {
                        tracing::debug!(target: "bex", operation = "background.power.lifecycle.stream", message = %error);
                        break;
                    }
                    None => break,
                },
            }
        }
    }
}

#[cfg(target_os = "macos")]
async fn next_macos_suspend_lifecycle_event(
    stop: &CancellationToken,
) -> Option<SuspendLifecycleEvent> {
    // The IOKit callback runs on a short-lived run-loop worker.  It is
    // recreated after each notification so dropping/cancelling the async
    // owner always tears down the notification port and root power object.
    use std::sync::mpsc;

    let (event_tx, event_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let watcher = tokio::task::spawn_blocking(move || {
        run_macos_power_watcher(event_tx, stop_rx);
    });
    let receive = tokio::task::spawn_blocking(move || event_rx.recv().ok());
    let event = tokio::select! {
        _ = stop.cancelled() => {
            let _ = stop_tx.send(());
            None
        }
        result = receive => result.ok().flatten(),
    };
    let _ = stop_tx.send(());
    let _ = watcher.await;
    event.map(|suspended| {
        if suspended {
            SuspendLifecycleEvent::Suspended
        } else {
            SuspendLifecycleEvent::Resumed
        }
    })
}

#[cfg(target_os = "macos")]
fn run_macos_power_watcher(
    event_tx: std::sync::mpsc::Sender<bool>,
    stop_rx: std::sync::mpsc::Receiver<()>,
) {
    use std::{ffi::c_void, ptr, sync::mpsc::TryRecvError};

    type IoObject = u32;
    type IoConnect = u32;
    type NotificationPort = *mut c_void;
    type RunLoop = *mut c_void;
    type RunLoopSource = *mut c_void;
    type StringRef = *const c_void;

    const MESSAGE_CAN_SLEEP: u32 = 0x0000_0200;
    const MESSAGE_WILL_SLEEP: u32 = 0x0000_0100;
    const MESSAGE_HAS_POWERED_ON: u32 = 0x0000_0800;

    #[repr(C)]
    struct CallbackContext {
        event_tx: std::sync::mpsc::Sender<bool>,
        root_port: IoConnect,
    }

    unsafe extern "C" fn callback(
        refcon: *mut c_void,
        _service: IoObject,
        message_type: u32,
        message_argument: *mut c_void,
    ) {
        let context = &mut *(refcon.cast::<CallbackContext>());
        match message_type {
            MESSAGE_CAN_SLEEP | MESSAGE_WILL_SLEEP => {
                if message_type == MESSAGE_WILL_SLEEP {
                    let _ = context.event_tx.send(true);
                }
                let _ =
                    IOAllowPowerChange(context.root_port, message_argument.cast::<isize>().read());
            }
            MESSAGE_HAS_POWERED_ON => {
                let _ = context.event_tx.send(false);
            }
            _ => {}
        }
    }

    unsafe {
        let mut notify_port: NotificationPort = ptr::null_mut();
        let mut notifier: IoObject = 0;
        let context = Box::new(CallbackContext {
            event_tx,
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
            if !notify_port.is_null() {
                IONotificationPortDestroy(notify_port);
            }
            if notifier != 0 {
                let _ = IOObjectRelease(notifier);
            }
            drop(Box::from_raw(context_ptr));
            return;
        }
        (*context_ptr).root_port = root_port;
        let run_loop = CFRunLoopGetCurrent();
        let source = IONotificationPortGetRunLoopSource(notify_port);
        if !source.is_null() {
            CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);
        }
        loop {
            match stop_rx.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            let _ = CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 1);
        }
        if !source.is_null() {
            CFRunLoopRemoveSource(run_loop, source, kCFRunLoopDefaultMode);
        }
        let _ = IOServiceClose(root_port);
        IONotificationPortDestroy(notify_port);
        if notifier != 0 {
            let _ = IOObjectRelease(notifier);
        }
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
        fn IOAllowPowerChange(root_port: IoConnect, notification_id: isize) -> i32;
        fn IOServiceClose(root_port: IoConnect) -> i32;
        fn IOObjectRelease(object: IoObject) -> i32;
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
async fn next_windows_suspend_lifecycle_event(
    stop: &CancellationToken,
) -> Option<SuspendLifecycleEvent> {
    use std::sync::mpsc;

    let (event_tx, event_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let stop_notify = stop_tx.clone();
    let watcher = tokio::task::spawn_blocking(move || {
        run_windows_power_watcher(event_tx, stop_rx, stop_notify);
    });
    let receive = tokio::task::spawn_blocking(move || event_rx.recv().ok());
    let event = tokio::select! {
        _ = stop.cancelled() => {
            let _ = stop_tx.send(());
            let _ = watcher.await;
            None
        }
        result = receive => result.ok().flatten(),
    };
    let _ = stop_tx.send(());
    let _ = watcher.await;
    event
}

#[cfg(target_os = "windows")]
fn run_windows_power_watcher(
    event_tx: std::sync::mpsc::Sender<bool>,
    stop_rx: std::sync::mpsc::Receiver<()>,
    stop_notify: std::sync::mpsc::Sender<()>,
) {
    // The worker owns a message-only HWND and registers that HWND with the
    // power manager.  The message loop is the documented Windows power
    // notification source; no idle-frequency heuristic is involved.
    use std::{mem, ptr};
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
        event_tx: std::sync::mpsc::Sender<bool>,
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
                let context = GetWindowLongPtrW(window, GWLP_USERDATA) as *const WindowContext;
                if !context.is_null() {
                    let _ = (&*context).event_tx.send(event);
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
        let context = Box::new(WindowContext { event_tx });
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
        let stop_thread = std::thread::spawn(move || {
            let _ = stop_rx.recv();
            let _ = PostThreadMessageW(thread_id, WM_QUIT, 0, 0);
        });
        while GetMessageW(&mut message, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        let _ = stop_notify.send(());
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

    #[tokio::test]
    async fn canceled_watcher_does_not_leave_a_platform_subscription() {
        let stop = CancellationToken::new();
        stop.cancel();
        assert!(next_suspend_lifecycle_event(&stop).await.is_none());
    }
}
