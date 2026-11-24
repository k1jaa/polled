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
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::sync::Arc;

    use eframe::egui;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0,
    };
    use windows::Win32::System::Threading::{
        CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
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
            })
        }

        /// Moves the show-event handle into a lightweight background thread that brings the
        /// window to the foreground whenever a second instance signals it.
        pub fn start_show_listener(
            &mut self,
            main_hwnd: Arc<AtomicIsize>,
            ctx: egui::Context,
            quit_requested: Arc<AtomicBool>,
        ) {
            let Some(event) = self.show_event.take() else {
                return;
            };
            std::thread::Builder::new()
                .name("polled-show-listener".to_string())
                .spawn(move || {
                    loop {
                        if quit_requested.load(Ordering::SeqCst) {
                            break;
                        }
                        // SAFETY: `event.0` is an owned valid event handle alive for the entire loop.
                        let wait = unsafe { WaitForSingleObject(event.0, 250) };
                        if wait == WAIT_OBJECT_0 {
                            crate::tray::show_and_focus(&main_hwnd, &ctx);
                        }
                    }
                    drop(event);
                })
                .ok();
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

    pub fn start_show_listener(
        &mut self,
        _main_hwnd: std::sync::Arc<std::sync::atomic::AtomicIsize>,
        _ctx: eframe::egui::Context,
        _quit_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
    }
}
