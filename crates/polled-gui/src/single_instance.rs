// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Single-instance guard using a per-session Win32 Named Mutex + Named Event.
//!
//! If Polled is already running in the tray and the user launches `polled-gui.exe` again,
//! the second process signals the existing instance's show-event and exits immediately.

#[cfg(windows)]
mod platform {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::sync::Arc;
    use std::thread::JoinHandle;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0,
    };
    use windows::Win32::System::Threading::{
        CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForMultipleObjects,
        EVENT_MODIFY_STATE, INFINITE,
    };

    const MUTEX_NAME: &str = r"Local\PolledGuiSingleInstance";
    const SHOW_EVENT_NAME: &str = r"Local\PolledGuiShowEvent";

    fn to_wide_null(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    /// RAII wrapper around a Win32 kernel `HANDLE` that always calls `CloseHandle` on drop.
    pub struct OwnedHandle(HANDLE);

    // SAFETY: Win32 kernel synchronization handles (mutex/event) are process-wide and safe to move
    // to another thread for waiting/closing.
    unsafe impl Send for OwnedHandle {}

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: `self.0` is an owned Win32 handle not yet closed.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    pub struct SingleInstanceGuard {
        _mutex: OwnedHandle,
        show_event: Option<OwnedHandle>,
        stop_event: Option<OwnedHandle>,
        listener_handle: Option<JoinHandle<()>>,
    }

    pub enum InstanceLock {
        Primary(SingleInstanceGuard),
        AlreadyRunning,
    }

    impl SingleInstanceGuard {
        /// Acquires the single-instance lock. When another instance is already running and
        /// `signal_existing_to_show` is true, wakes that instance so its window comes to the front.
        pub fn acquire(signal_existing_to_show: bool) -> InstanceLock {
            let mutex_w = to_wide_null(MUTEX_NAME);
            // SAFETY: `mutex_w` is null-terminated UTF-16.
            let Ok(mutex) = (unsafe { CreateMutexW(None, false, PCWSTR(mutex_w.as_ptr())) }) else {
                // If creating a mutex somehow fails, allow startup rather than blocking the user.
                return InstanceLock::Primary(Self {
                    _mutex: OwnedHandle(HANDLE::default()),
                    show_event: None,
                    stop_event: None,
                    listener_handle: None,
                });
            };

            // SAFETY: `GetLastError` immediately inspects the `CreateMutexW` status on this thread.
            let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            if already_exists {
                let _guard = OwnedHandle(mutex);
                if signal_existing_to_show {
                    let event_w = to_wide_null(SHOW_EVENT_NAME);
                    // SAFETY: `event_w` is null-terminated UTF-16.
                    if let Ok(ev) =
                        unsafe { OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(event_w.as_ptr())) }
                    {
                        let _ev_guard = OwnedHandle(ev);
                        // SAFETY: `ev` is an open event handle with `EVENT_MODIFY_STATE`.
                        unsafe {
                            let _ = SetEvent(ev);
                        }
                    }
                }
                return InstanceLock::AlreadyRunning;
            }

            let event_w = to_wide_null(SHOW_EVENT_NAME);
            // Auto-reset event (`manual_reset = false`), initially non-signaled.
            // SAFETY: `event_w` is null-terminated UTF-16.
            let show_event = unsafe { CreateEventW(None, false, false, PCWSTR(event_w.as_ptr())) }
                .ok()
                .map(OwnedHandle);

            InstanceLock::Primary(Self {
                _mutex: OwnedHandle(mutex),
                show_event,
                stop_event: None,
                listener_handle: None,
            })
        }

        /// Moves the show-event handle into a zero-CPU kernel-blocking background thread
        /// (`WaitForMultipleObjects(..., INFINITE)`) that invokes `on_show` whenever a second
        /// instance signals the named event.
        pub fn start_show_listener(&mut self, on_show: Arc<dyn Fn() + Send + Sync>) {
            let Some(event) = self.show_event.take() else {
                return;
            };
            // Unnamed manual-reset stop event signaled in `Drop` before joining the listener thread.
            let Ok(stop_raw) = (unsafe { CreateEventW(None, true, false, PCWSTR::null()) }) else {
                self.show_event = Some(event);
                return;
            };
            let stop_ptr = stop_raw.0 as isize;
            self.stop_event = Some(OwnedHandle(stop_raw));

            let handle = std::thread::Builder::new()
                .name("polled-show-listener".to_string())
                .spawn(move || {
                    let handles = [event.0, HANDLE(stop_ptr as *mut core::ffi::c_void)];
                    loop {
                        // SAFETY: Both `event.0` (owned by this thread) and `stop_handle` (owned by
                        // `SingleInstanceGuard`, which signals and joins this thread before closing
                        // `stop_event` in `Drop`) remain valid for the full wait.
                        let wait = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                        if wait == WAIT_OBJECT_0 {
                            on_show();
                        } else {
                            break;
                        }
                    }
                    drop(event);
                })
                .ok();
            self.listener_handle = handle;
        }
    }

    impl Drop for SingleInstanceGuard {
        fn drop(&mut self) {
            if let Some(stop) = &self.stop_event {
                // SAFETY: `stop.0` is an owned valid Win32 event handle.
                unsafe {
                    let _ = SetEvent(stop.0);
                }
            }
            if let Some(handle) = self.listener_handle.take() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(windows)]
pub use platform::{InstanceLock, SingleInstanceGuard};

#[cfg(not(windows))]
pub struct SingleInstanceGuard;

#[cfg(not(windows))]
pub enum InstanceLock {
    Primary(SingleInstanceGuard),
    AlreadyRunning,
}

#[cfg(not(windows))]
impl SingleInstanceGuard {
    pub fn acquire(_signal_existing_to_show: bool) -> InstanceLock {
        InstanceLock::Primary(Self)
    }

    pub fn start_show_listener(&mut self, _on_show: std::sync::Arc<dyn Fn() + Send + Sync>) {}
}
