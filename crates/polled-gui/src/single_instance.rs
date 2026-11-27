// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Single-instance guard using a per-session Win32 Named Mutex + Named Events, plus
//! `%LOCALAPPDATA%\Polled\daemon.json` metadata so launching a newer `polled-gui.exe`
//! automatically asks an older tray daemon to exit and seamlessly takes over.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Metadata persisted while the primary Polled tray daemon (or standalone instance) owns the
/// single-instance mutex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningDaemonInfo {
    pub pid: u32,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe_path: Option<String>,
}

/// Returns `true` when `running` belongs to a different process running a strictly older Polled
/// version than `current_version`.
pub fn should_take_over_running_daemon(
    running: &RunningDaemonInfo,
    current_pid: u32,
    current_version: &str,
) -> bool {
    running.pid != current_pid && polled_core::is_newer_version(&running.version, current_version)
}

/// Atomically records the running daemon's PID, version, and exe path to `path` (`daemon.json`).
pub fn write_running_daemon_info(path: &Path, info: &RunningDaemonInfo) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(json) = serde_json::to_string_pretty(info) else {
        return;
    };
    let tmp = path.with_extension(format!("json.{}.tmp", info.pid));
    if fs::write(&tmp, json).is_err() {
        return;
    }
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    if fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

/// Reads the currently recorded running daemon info from `path`, if present and valid JSON.
pub fn read_running_daemon_info(path: &Path) -> Option<RunningDaemonInfo> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Removes `path` only if it still points to `owner_pid` (prevents an exiting older daemon from
/// accidentally deleting the metadata freshly written by a newer daemon that took over).
pub fn clear_running_daemon_info_for_pid(path: &Path, owner_pid: u32) {
    if let Some(existing) = read_running_daemon_info(path) {
        if existing.pid == owner_pid {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
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

    use super::{clear_running_daemon_info_for_pid, write_running_daemon_info, RunningDaemonInfo};

    const MUTEX_NAME: &str = r"Local\PolledGuiSingleInstance";
    const SHOW_EVENT_NAME: &str = r"Local\PolledGuiShowEvent";
    const QUIT_EVENT_NAME: &str = r"Local\PolledGuiQuitEvent";

    fn to_wide_null(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    fn signal_named_event(name: &str) -> bool {
        let event_w = to_wide_null(name);
        // SAFETY: `event_w` is null-terminated UTF-16.
        if let Ok(ev) = unsafe { OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(event_w.as_ptr())) } {
            let _ev_guard = OwnedHandle(ev);
            // SAFETY: `ev` is an open event handle with `EVENT_MODIFY_STATE`.
            return unsafe { SetEvent(ev).is_ok() };
        }
        false
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
        quit_event: Option<OwnedHandle>,
        stop_event: Option<OwnedHandle>,
        listener_handle: Option<JoinHandle<()>>,
        daemon_state_path: Option<PathBuf>,
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
                    quit_event: None,
                    stop_event: None,
                    listener_handle: None,
                    daemon_state_path: None,
                });
            };

            // SAFETY: `GetLastError` immediately inspects the `CreateMutexW` status on this thread.
            let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            if already_exists {
                let _guard = OwnedHandle(mutex);
                if signal_existing_to_show {
                    let _ = Self::signal_existing_to_show();
                }
                return InstanceLock::AlreadyRunning;
            }

            let show_w = to_wide_null(SHOW_EVENT_NAME);
            // Auto-reset event (`manual_reset = false`), initially non-signaled.
            // SAFETY: `show_w` is null-terminated UTF-16.
            let show_event = unsafe { CreateEventW(None, false, false, PCWSTR(show_w.as_ptr())) }
                .ok()
                .map(OwnedHandle);

            let quit_w = to_wide_null(QUIT_EVENT_NAME);
            // Auto-reset takeover-quit event (`manual_reset = false`), initially non-signaled.
            // SAFETY: `quit_w` is null-terminated UTF-16.
            let quit_event = unsafe { CreateEventW(None, false, false, PCWSTR(quit_w.as_ptr())) }
                .ok()
                .map(OwnedHandle);

            InstanceLock::Primary(Self {
                _mutex: OwnedHandle(mutex),
                show_event,
                quit_event,
                stop_event: None,
                listener_handle: None,
                daemon_state_path: None,
            })
        }

        /// Wakes an already-running primary instance so its window comes to the front.
        pub fn signal_existing_to_show() -> bool {
            signal_named_event(SHOW_EVENT_NAME)
        }

        /// Signals an already-running older primary instance to quit cleanly so a newer version
        /// can take over the single-instance lock.
        pub fn signal_existing_to_quit() -> bool {
            signal_named_event(QUIT_EVENT_NAME)
        }

        /// Records this primary instance's PID, version, and exe path in `daemon.json`, and
        /// automatically cleans it up on `Drop` if we are still the recorded owner.
        pub fn record_running_daemon(&mut self, path: &Path) {
            let info = RunningDaemonInfo {
                pid: std::process::id(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                exe_path: std::env::current_exe()
                    .ok()
                    .map(|p| p.display().to_string()),
            };
            write_running_daemon_info(path, &info);
            self.daemon_state_path = Some(path.to_path_buf());
        }

        /// Moves the show- and takeover-quit event handles into a zero-CPU kernel-blocking
        /// background thread (`WaitForMultipleObjects(..., INFINITE)`) that invokes `on_show` when
        /// another instance asks to focus the UI, or `on_quit` when a newer version requests a
        /// graceful handoff.
        pub fn start_event_listener(
            &mut self,
            on_show: Arc<dyn Fn() + Send + Sync>,
            on_quit: Arc<dyn Fn() + Send + Sync>,
        ) {
            let Some(show_event) = self.show_event.take() else {
                return;
            };
            let quit_event = self.quit_event.take();
            // Unnamed manual-reset stop event signaled in `Drop` before joining the listener thread.
            let Ok(stop_raw) = (unsafe { CreateEventW(None, true, false, PCWSTR::null()) }) else {
                self.show_event = Some(show_event);
                self.quit_event = quit_event;
                return;
            };
            let stop_ptr = stop_raw.0 as isize;
            self.stop_event = Some(OwnedHandle(stop_raw));

            let handle = std::thread::Builder::new()
                .name("polled-show-listener".to_string())
                .spawn(move || {
                    let stop_h = HANDLE(stop_ptr as *mut core::ffi::c_void);
                    let mut handles = Vec::with_capacity(3);
                    handles.push(show_event.0);
                    let quit_index = if let Some(ref qe) = quit_event {
                        handles.push(qe.0);
                        Some(1u32)
                    } else {
                        None
                    };
                    handles.push(stop_h);

                    loop {
                        // SAFETY: `show_event` and `quit_event` are owned by this thread; `stop_h`
                        // is owned by `SingleInstanceGuard`, which signals and joins this thread
                        // before closing `stop_event` in `Drop`.
                        let wait = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                        if wait == WAIT_OBJECT_0 {
                            on_show();
                        } else if quit_index.is_some_and(|idx| wait.0 == WAIT_OBJECT_0.0 + idx) {
                            on_quit();
                            break;
                        } else {
                            break;
                        }
                    }
                    drop(quit_event);
                    drop(show_event);
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
            if let Some(path) = self.daemon_state_path.take() {
                clear_running_daemon_info_for_pid(&path, std::process::id());
            }
        }
    }
}

#[cfg(windows)]
pub use platform::{InstanceLock, SingleInstanceGuard};

#[cfg(not(windows))]
pub struct SingleInstanceGuard {
    daemon_state_path: Option<std::path::PathBuf>,
}

#[cfg(not(windows))]
pub enum InstanceLock {
    Primary(SingleInstanceGuard),
    AlreadyRunning,
}

#[cfg(not(windows))]
impl SingleInstanceGuard {
    pub fn acquire(_signal_existing_to_show: bool) -> InstanceLock {
        InstanceLock::Primary(Self {
            daemon_state_path: None,
        })
    }

    pub fn signal_existing_to_show() -> bool {
        false
    }

    pub fn signal_existing_to_quit() -> bool {
        false
    }

    pub fn record_running_daemon(&mut self, path: &Path) {
        let info = RunningDaemonInfo {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            exe_path: std::env::current_exe()
                .ok()
                .map(|p| p.display().to_string()),
        };
        write_running_daemon_info(path, &info);
        self.daemon_state_path = Some(path.to_path_buf());
    }

    pub fn start_event_listener(
        &mut self,
        _on_show: std::sync::Arc<dyn Fn() + Send + Sync>,
        _on_quit: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) {
    }
}

#[cfg(not(windows))]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        if let Some(path) = self.daemon_state_path.take() {
            clear_running_daemon_info_for_pid(&path, std::process::id());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_info_roundtrip_and_pid_guarded_cleanup() {
        let dir =
            std::env::temp_dir().join(format!("polled_daemon_state_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("daemon.json");

        assert!(read_running_daemon_info(&path).is_none());

        let old = RunningDaemonInfo {
            pid: 1001,
            version: "0.14.4".to_string(),
            exe_path: Some(r"C:\Polled\old\polled-gui.exe".to_string()),
        };
        write_running_daemon_info(&path, &old);
        assert_eq!(read_running_daemon_info(&path), Some(old.clone()));

        // A newer version from another PID should trigger takeover; same/older should not.
        assert!(should_take_over_running_daemon(&old, 2002, "0.15.0"));
        assert!(!should_take_over_running_daemon(&old, 2002, "0.14.4"));
        assert!(!should_take_over_running_daemon(&old, 2002, "0.14.3"));
        assert!(!should_take_over_running_daemon(&old, 1001, "0.15.0"));

        // Overwrite with newer daemon PID, then simulate late exit of old PID 1001:
        // clearing for PID 1001 must NOT delete PID 2002's metadata.
        let newer = RunningDaemonInfo {
            pid: 2002,
            version: "0.15.0".to_string(),
            exe_path: Some(r"C:\Polled\new\polled-gui.exe".to_string()),
        };
        write_running_daemon_info(&path, &newer);
        clear_running_daemon_info_for_pid(&path, 1001);
        assert_eq!(read_running_daemon_info(&path), Some(newer));

        // Clearing for the real owner PID removes the file cleanly.
        clear_running_daemon_info_for_pid(&path, 2002);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
