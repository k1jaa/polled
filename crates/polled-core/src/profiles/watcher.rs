// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Anti-cheat-safe Windows foreground window watcher using `SetWinEventHook`.
//!
//! Uses `WINEVENT_OUTOFCONTEXT` (no DLL injection, no hooks in game processes, no memory
//! reads, no admin rights, and zero polling loops). Deliberately omits `WINEVENT_SKIPOWNPROCESS`
//! so clicking back onto Polled's own window properly restores the Desktop profile.

use std::time::Instant;

/// Foreground focus change reported by Windows.
#[derive(Debug, Clone)]
pub struct FocusEvent {
    pub exe_name: Option<String>,
    /// Full Win32 path of the foreground `.exe` when resolvable (used for optional per-game GPU preference).
    pub exe_path: Option<String>,
    pub observed_at: Instant,
}

#[cfg(windows)]
mod platform {
    use std::cell::RefCell;
    use std::sync::mpsc::{self, Sender};
    use std::thread::{self, JoinHandle};
    use std::time::Instant;

    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HWND};
    use windows::Win32::System::Threading::{
        GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
        PostThreadMessageW, TranslateMessage, EVENT_SYSTEM_FOREGROUND, MSG, WINEVENT_OUTOFCONTEXT,
        WM_QUIT,
    };

    use crate::error::{PolledError, Result};
    use crate::profiles::matcher::normalize_exe_name;
    use crate::profiles::watcher::FocusEvent;

    // Thread-local sender used strictly on the dedicated Win32 hook message-loop thread.
    thread_local! {
        static HOOK_SENDER: RefCell<Option<Sender<FocusEvent>>> = const { RefCell::new(None) };
    }

    pub struct ForegroundWatcher {
        thread_id: u32,
        join_handle: Option<JoinHandle<()>>,
    }

    impl ForegroundWatcher {
        /// Starts a dedicated Win32 message-loop thread hooked to `EVENT_SYSTEM_FOREGROUND`.
        pub fn start(sender: Sender<FocusEvent>) -> Result<Self> {
            let (ready_tx, ready_rx) = mpsc::channel::<std::result::Result<u32, String>>();

            let handle = thread::Builder::new()
                .name("polled-fg-watcher".to_string())
                .spawn(move || {
                    // SAFETY: `GetCurrentThreadId` has no preconditions.
                    let tid = unsafe { GetCurrentThreadId() };

                    // Emit the currently active foreground window immediately on startup.
                    // SAFETY: `GetForegroundWindow` returns a window handle or null.
                    let initial_hwnd = unsafe { GetForegroundWindow() };
                    let (exe_name, exe_path) = exe_info_from_hwnd(initial_hwnd);
                    let _ = sender.send(FocusEvent {
                        exe_name,
                        exe_path,
                        observed_at: Instant::now(),
                    });

                    HOOK_SENDER.with(|slot| {
                        *slot.borrow_mut() = Some(sender);
                    });

                    // SAFETY: `WINEVENT_OUTOFCONTEXT` delivers callbacks asynchronously on this
                    // thread's message loop with zero DLL injection into other processes.
                    let hook = unsafe {
                        SetWinEventHook(
                            EVENT_SYSTEM_FOREGROUND,
                            EVENT_SYSTEM_FOREGROUND,
                            None,
                            Some(win_event_proc),
                            0,
                            0,
                            WINEVENT_OUTOFCONTEXT,
                        )
                    };

                    if hook.is_invalid() {
                        HOOK_SENDER.with(|slot| *slot.borrow_mut() = None);
                        let _ = ready_tx.send(Err("SetWinEventHook returned null handle".into()));
                        return;
                    }

                    let _ = ready_tx.send(Ok(tid));

                    let mut msg = MSG::default();
                    // SAFETY: Standard Win32 message loop for the thread's own queue.
                    unsafe {
                        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                        let _ = UnhookWinEvent(hook);
                    }

                    HOOK_SENDER.with(|slot| *slot.borrow_mut() = None);
                })
                .map_err(|e| PolledError::Win32 {
                    context: "ForegroundWatcher::start".to_string(),
                    message: e.to_string(),
                })?;

            let thread_id = ready_rx
                .recv()
                .map_err(|_| PolledError::Win32 {
                    context: "ForegroundWatcher::start".to_string(),
                    message: "Watcher thread terminated before installing hook".to_string(),
                })?
                .map_err(|msg| PolledError::Win32 {
                    context: "SetWinEventHook".to_string(),
                    message: msg,
                })?;

            Ok(Self {
                thread_id,
                join_handle: Some(handle),
            })
        }

        pub fn stop(&mut self) {
            if let Some(handle) = self.join_handle.take() {
                // SAFETY: Posting `WM_QUIT` cleanly exits the watcher thread's `GetMessageW` loop.
                unsafe {
                    let _ = PostThreadMessageW(
                        self.thread_id,
                        WM_QUIT,
                        windows::Win32::Foundation::WPARAM(0),
                        windows::Win32::Foundation::LPARAM(0),
                    );
                }
                let _ = handle.join();
            }
        }
    }

    impl Drop for ForegroundWatcher {
        fn drop(&mut self) {
            self.stop();
        }
    }

    unsafe extern "system" fn win_event_proc(
        _hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        _id_object: i32,
        _id_child: i32,
        _event_thread: u32,
        _event_time: u32,
    ) {
        if event != EVENT_SYSTEM_FOREGROUND {
            return;
        }
        // When the foreground window becomes null (e.g., minimizing everything to an empty
        // desktop), emit `exe_name: None` so the matcher falls back to the Desktop profile.
        let (exe_name, exe_path) = if hwnd.is_invalid() {
            (None, None)
        } else {
            exe_info_from_hwnd(hwnd)
        };
        HOOK_SENDER.with(|slot| {
            if let Some(tx) = slot.borrow().as_ref() {
                let _ = tx.send(FocusEvent {
                    exe_name,
                    exe_path,
                    observed_at: Instant::now(),
                });
            }
        });
    }

    /// Resolves `(lowercase_basename, full_win32_path)` of `hwnd`'s owning process using only
    /// `PROCESS_QUERY_LIMITED_INFORMATION` (no admin, no memory reads).
    fn exe_info_from_hwnd(hwnd: HWND) -> (Option<String>, Option<String>) {
        if hwnd.is_invalid() {
            return (None, None);
        }

        let mut pid = 0u32;
        // SAFETY: `hwnd` is non-null and `pid` is valid out storage.
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        if pid == 0 {
            return (None, None);
        }

        // SAFETY: Requests minimal read-only metadata permission only.
        let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
        else {
            return (None, None);
        };

        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: `handle` is open and `buf` has `len` writable UTF-16 code units.
        let ok = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
        }
        .is_ok();

        // SAFETY: Close the opened process handle immediately.
        unsafe {
            let _ = CloseHandle(handle);
        }

        if !ok || len == 0 {
            return (None, None);
        }

        let full_path = String::from_utf16_lossy(&buf[..len as usize]);
        let normalized = normalize_exe_name(&full_path);
        if normalized.is_empty() {
            (None, None)
        } else {
            (Some(normalized), Some(full_path))
        }
    }
}

#[cfg(windows)]
pub use platform::ForegroundWatcher;

#[cfg(not(windows))]
pub struct ForegroundWatcher;

#[cfg(not(windows))]
impl ForegroundWatcher {
    pub fn start(_sender: std::sync::mpsc::Sender<FocusEvent>) -> crate::error::Result<Self> {
        Err(crate::error::PolledError::UnsupportedPlatform)
    }

    pub fn stop(&mut self) {}
}
