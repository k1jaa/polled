// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dedicated Win32 Raw Input (`WM_INPUT` + `RIDEV_INPUTSINK`) capture thread using
//! `QueryPerformanceCounter` timestamps so GUI framerate never skews 4K/8K measurements.
//!
//! Uses a kernel-blocking `GetMessageW` loop with a 33 ms `SetTimer` publish tick so:
//! - Every `WM_INPUT` packet wakes the thread immediately on arrival (zero `sleep()` bunching)
//! - Idle capture uses ~0% CPU
//! - Stopping capture unregisters Raw Input and joins the thread cleanly (0% CPU when idle)
//!
//! 100% anti-cheat safe: standard Win32 Raw Input API, zero hooks, zero DLL injection, no admin.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::diagnostics::analyzer::{
    ClickBounceAnalyzer, DiagnosticsSnapshot, DpiCalibrator, MouseButtonKind, PollingAnalyzer,
    PollingTarget,
};

const TARGET_AUTO_SENTINEL: u32 = 0;

#[cfg(windows)]
const WM_POLLED_SYNC: u32 = 0x8001; // WM_APP + 1
#[cfg(windows)]
const TIMER_PUBLISH_ID: usize = 1;
#[cfg(windows)]
const TIMER_PUBLISH_MS: u32 = 33;

/// Live handle controlling an active background Win32 Raw Input capture session.
pub struct RawInputCaptureSession {
    snapshot: Arc<Mutex<DiagnosticsSnapshot>>,
    running: Arc<AtomicBool>,
    reset_requested: Arc<AtomicBool>,
    dpi_measuring: Arc<AtomicBool>,
    dpi_reset_requested: Arc<AtomicBool>,
    target_hz_atomic: Arc<AtomicU32>,
    msg_hwnd: Arc<AtomicIsize>,
    join_handle: Option<JoinHandle<()>>,
}

impl RawInputCaptureSession {
    /// Starts capturing raw mouse motion and button events on a dedicated Win32 message thread.
    ///
    /// `on_update` is called at ~30 Hz whenever a fresh snapshot is published (e.g. `ctx.request_repaint()`).
    pub fn start<F>(target: PollingTarget, on_update: F) -> Self
    where
        F: Fn() + Send + 'static,
    {
        let initial_target_raw = match target {
            PollingTarget::Auto => TARGET_AUTO_SENTINEL,
            PollingTarget::FixedHz(hz) => hz,
        };
        let snapshot = Arc::new(Mutex::new(DiagnosticsSnapshot {
            capturing: true,
            target,
            ..Default::default()
        }));
        let running = Arc::new(AtomicBool::new(true));
        let reset_requested = Arc::new(AtomicBool::new(false));
        let dpi_measuring = Arc::new(AtomicBool::new(false));
        let dpi_reset_requested = Arc::new(AtomicBool::new(false));
        let target_hz_atomic = Arc::new(AtomicU32::new(initial_target_raw));
        let msg_hwnd = Arc::new(AtomicIsize::new(0));

        let snap_bg = Arc::clone(&snapshot);
        let run_bg = Arc::clone(&running);
        let reset_bg = Arc::clone(&reset_requested);
        let dpi_meas_bg = Arc::clone(&dpi_measuring);
        let dpi_reset_bg = Arc::clone(&dpi_reset_requested);
        let target_bg = Arc::clone(&target_hz_atomic);
        let hwnd_bg = Arc::clone(&msg_hwnd);

        let handle = std::thread::spawn(move || {
            #[cfg(windows)]
            run_win32_capture_loop(
                snap_bg,
                run_bg,
                reset_bg,
                dpi_meas_bg,
                dpi_reset_bg,
                target_bg,
                hwnd_bg,
                on_update,
            );

            #[cfg(not(windows))]
            {
                let _ = (
                    run_bg,
                    reset_bg,
                    dpi_meas_bg,
                    dpi_reset_bg,
                    target_bg,
                    hwnd_bg,
                );
                if let Ok(mut guard) = snap_bg.lock() {
                    guard.capturing = false;
                    guard.error =
                        Some("Raw Input capture is only available on Windows.".to_string());
                }
                on_update();
            }
        });

        Self {
            snapshot,
            running,
            reset_requested,
            dpi_measuring,
            dpi_reset_requested,
            target_hz_atomic,
            msg_hwnd,
            join_handle: Some(handle),
        }
    }

    pub fn snapshot(&self) -> DiagnosticsSnapshot {
        self.snapshot.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn set_target(&self, target: PollingTarget) {
        let raw = match target {
            PollingTarget::Auto => TARGET_AUTO_SENTINEL,
            PollingTarget::FixedHz(hz) => hz,
        };
        self.target_hz_atomic.store(raw, Ordering::SeqCst);
        self.post_sync_wakeup();
    }

    pub fn set_dpi_measuring(&self, active: bool) {
        self.dpi_measuring.store(active, Ordering::SeqCst);
        self.post_sync_wakeup();
    }

    pub fn request_dpi_reset(&self) {
        self.dpi_reset_requested.store(true, Ordering::SeqCst);
        self.post_sync_wakeup();
    }

    pub fn request_reset(&self) {
        self.reset_requested.store(true, Ordering::SeqCst);
        self.post_sync_wakeup();
    }

    fn post_sync_wakeup(&self) {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

            let raw = self.msg_hwnd.load(Ordering::SeqCst);
            if raw != 0 {
                let hwnd = HWND(raw as *mut core::ffi::c_void);
                // SAFETY: Posting `WM_POLLED_SYNC` to our own message-only window wakes `GetMessageW`.
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_POLLED_SYNC, WPARAM(0), LPARAM(0));
                }
            }
        }
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

            let raw = self.msg_hwnd.load(Ordering::SeqCst);
            if raw != 0 {
                let hwnd = HWND(raw as *mut core::ffi::c_void);
                // SAFETY: Posting `WM_CLOSE` to our own message-only window wakes `GetMessageW`.
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
        if let Ok(mut guard) = self.snapshot.lock() {
            guard.capturing = false;
        }
    }
}

impl Drop for RawInputCaptureSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn run_win32_capture_loop<F>(
    snapshot: Arc<Mutex<DiagnosticsSnapshot>>,
    running: Arc<AtomicBool>,
    reset_requested: Arc<AtomicBool>,
    dpi_measuring: Arc<AtomicBool>,
    dpi_reset_requested: Arc<AtomicBool>,
    target_hz_atomic: Arc<AtomicU32>,
    msg_hwnd: Arc<AtomicIsize>,
    on_update: F,
) where
    F: Fn() + Send + 'static,
{
    use std::cell::RefCell;
    use std::ffi::OsStr;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
    };
    use windows::Win32::UI::Input::{
        GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE,
        RAWINPUTHEADER, RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT, RIM_TYPEMOUSE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer,
        PostQuitMessage, RegisterClassW, SetTimer, TranslateMessage, UnregisterClassW,
        HWND_MESSAGE, MSG, RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP, RI_MOUSE_BUTTON_5_DOWN,
        RI_MOUSE_BUTTON_5_UP, RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_LEFT_BUTTON_UP,
        RI_MOUSE_MIDDLE_BUTTON_DOWN, RI_MOUSE_MIDDLE_BUTTON_UP, RI_MOUSE_RIGHT_BUTTON_DOWN,
        RI_MOUSE_RIGHT_BUTTON_UP, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_DESTROY, WM_INPUT,
        WM_TIMER, WNDCLASSW,
    };

    struct CaptureThreadState {
        qpc_freq: u64,
        polling: PollingAnalyzer,
        dpi_cal: DpiCalibrator,
        clicks: ClickBounceAnalyzer,
        last_target_raw: u32,
        snapshot: Arc<Mutex<DiagnosticsSnapshot>>,
        reset_requested: Arc<AtomicBool>,
        dpi_measuring: Arc<AtomicBool>,
        dpi_reset_requested: Arc<AtomicBool>,
        target_hz_atomic: Arc<AtomicU32>,
        on_update: Box<dyn Fn() + Send>,
    }

    impl CaptureThreadState {
        fn sync_dpi_flags(&mut self) {
            if self.dpi_reset_requested.swap(false, Ordering::SeqCst) {
                self.dpi_cal.reset();
            }
            let want_active = self.dpi_measuring.load(Ordering::SeqCst);
            if want_active != self.dpi_cal.is_active() {
                self.dpi_cal.set_active(want_active);
            }
        }

        fn sync_and_publish(&mut self) {
            if self.reset_requested.swap(false, Ordering::SeqCst) {
                self.polling.reset();
                self.clicks.reset();
                self.dpi_cal.reset();
            }
            self.sync_dpi_flags();
            let target_raw = self.target_hz_atomic.load(Ordering::SeqCst);
            if target_raw != self.last_target_raw {
                self.polling.set_target(decode_target(target_raw));
                self.last_target_raw = target_raw;
            }
            let now_ns = qpc_now_ns(self.qpc_freq);
            let next_snap = DiagnosticsSnapshot {
                capturing: true,
                target: self.polling.target(),
                polling: self.polling.snapshot(Some(now_ns)),
                tracking: self.polling.tracking_snapshot(Some(now_ns)),
                dpi_measurement: self.dpi_cal.snapshot(),
                clicks: self.clicks.snapshot(),
                error: None,
            };
            if let Ok(mut guard) = self.snapshot.lock() {
                *guard = next_snap;
            }
            (self.on_update)();
        }
    }

    thread_local! {
        static STATE: RefCell<Option<CaptureThreadState>> = const { RefCell::new(None) };
    }

    fn qpc_now_ns(freq: u64) -> u64 {
        let mut count = 0i64;
        // SAFETY: `count` is valid writable storage for `QueryPerformanceCounter`.
        unsafe {
            let _ = QueryPerformanceCounter(&mut count);
        }
        if count <= 0 || freq == 0 {
            return 0;
        }
        ((count as u128) * 1_000_000_000u128 / (freq as u128)) as u64
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_INPUT => {
                STATE.with(|cell| {
                    let mut borrow = cell.borrow_mut();
                    let Some(state) = borrow.as_mut() else {
                        return;
                    };
                    // Timestamp immediately on message arrival before decoding the payload.
                    let ts_ns = qpc_now_ns(state.qpc_freq);

                    let mut raw = RAWINPUT::default();
                    let mut size = size_of::<RAWINPUT>() as u32;
                    // SAFETY: `lparam` is the `HRAWINPUT` handle for `WM_INPUT`; `raw` is sized properly.
                    let copied = unsafe {
                        GetRawInputData(
                            HRAWINPUT(lparam.0 as *mut core::ffi::c_void),
                            RID_INPUT,
                            Some((&mut raw as *mut RAWINPUT).cast()),
                            &mut size,
                            size_of::<RAWINPUTHEADER>() as u32,
                        )
                    };
                    if copied == u32::MAX || raw.header.dwType != RIM_TYPEMOUSE.0 {
                        return;
                    }

                    // SAFETY: `dwType == RIM_TYPEMOUSE` guarantees `raw.data.mouse` is active.
                    let mouse = unsafe { raw.data.mouse };
                    state
                        .polling
                        .record_motion(ts_ns, mouse.lLastX, mouse.lLastY);
                    state.sync_dpi_flags();
                    state.dpi_cal.record_motion(mouse.lLastX, mouse.lLastY);

                    // SAFETY: Standard Windows `RAWMOUSE` button flags union access.
                    let flags = u32::from(unsafe { mouse.Anonymous.Anonymous.usButtonFlags });
                    if flags != 0 {
                        if (flags & RI_MOUSE_LEFT_BUTTON_DOWN) != 0 {
                            state
                                .clicks
                                .record_button(MouseButtonKind::Left, true, ts_ns);
                        }
                        if (flags & RI_MOUSE_LEFT_BUTTON_UP) != 0 {
                            state
                                .clicks
                                .record_button(MouseButtonKind::Left, false, ts_ns);
                        }
                        if (flags & RI_MOUSE_RIGHT_BUTTON_DOWN) != 0 {
                            state
                                .clicks
                                .record_button(MouseButtonKind::Right, true, ts_ns);
                        }
                        if (flags & RI_MOUSE_RIGHT_BUTTON_UP) != 0 {
                            state
                                .clicks
                                .record_button(MouseButtonKind::Right, false, ts_ns);
                        }
                        if (flags & RI_MOUSE_MIDDLE_BUTTON_DOWN) != 0 {
                            state
                                .clicks
                                .record_button(MouseButtonKind::Middle, true, ts_ns);
                        }
                        if (flags & RI_MOUSE_MIDDLE_BUTTON_UP) != 0 {
                            state
                                .clicks
                                .record_button(MouseButtonKind::Middle, false, ts_ns);
                        }
                        if (flags & RI_MOUSE_BUTTON_4_DOWN) != 0 {
                            state
                                .clicks
                                .record_button(MouseButtonKind::SideBack, true, ts_ns);
                        }
                        if (flags & RI_MOUSE_BUTTON_4_UP) != 0 {
                            state
                                .clicks
                                .record_button(MouseButtonKind::SideBack, false, ts_ns);
                        }
                        if (flags & RI_MOUSE_BUTTON_5_DOWN) != 0 {
                            state
                                .clicks
                                .record_button(MouseButtonKind::SideForward, true, ts_ns);
                        }
                        if (flags & RI_MOUSE_BUTTON_5_UP) != 0 {
                            state
                                .clicks
                                .record_button(MouseButtonKind::SideForward, false, ts_ns);
                        }
                    }
                });
                LRESULT(0)
            }
            WM_TIMER | WM_POLLED_SYNC => {
                STATE.with(|cell| {
                    if let Some(state) = cell.borrow_mut().as_mut() {
                        state.sync_and_publish();
                    }
                });
                LRESULT(0)
            }
            WM_CLOSE => {
                // SAFETY: Closing our own message-only window triggers `WM_DESTROY`.
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                // SAFETY: Signals this thread's `GetMessageW` loop to exit cleanly.
                unsafe {
                    PostQuitMessage(0);
                }
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    // Elevate only this tiny message-draining thread while actively recording for clean 8K timestamps.
    // SAFETY: Setting priority on our own current thread has no memory preconditions.
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
    }

    let mut freq = 0i64;
    // SAFETY: `freq` is valid writable storage.
    unsafe {
        let _ = QueryPerformanceFrequency(&mut freq);
    }
    let qpc_freq = freq.max(1) as u64;

    let initial_target_raw = target_hz_atomic.load(Ordering::SeqCst);
    let initial_target = decode_target(initial_target_raw);

    // Unique per-session class name so repeated Start/Stop cycles never collide.
    let class_str = format!(
        "PolledRawInputSink_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let class_w: Vec<u16> = OsStr::new(&class_str)
        .encode_wide()
        .chain(Some(0))
        .collect();

    // SAFETY: Querying our own executable's HMODULE.
    let hmodule = unsafe { GetModuleHandleW(PCWSTR::null()) }.unwrap_or_default();
    let hinstance = HINSTANCE(hmodule.0);

    let wc = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinstance,
        lpszClassName: PCWSTR(class_w.as_ptr()),
        ..Default::default()
    };

    // SAFETY: `wc` points to a null-terminated class name and valid `wnd_proc`.
    if unsafe { RegisterClassW(&wc) } == 0 {
        publish_error(
            &snapshot,
            "Failed to register Win32 Raw Input window class.",
            &on_update,
        );
        return;
    }

    // SAFETY: Creating a hidden message-only (`HWND_MESSAGE`) window owned by this thread.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_w.as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance),
            None,
        )
    };

    let hwnd = match hwnd {
        Ok(h) if !h.is_invalid() => h,
        _ => {
            // SAFETY: Matches `RegisterClassW` above.
            unsafe {
                let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
            }
            publish_error(
                &snapshot,
                "Failed to create Win32 Raw Input message window.",
                &on_update,
            );
            return;
        }
    };

    let rid = RAWINPUTDEVICE {
        usUsagePage: 0x01, // Generic Desktop Controls
        usUsage: 0x02,     // Mouse
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };
    // SAFETY: `rid` is a valid single-element array matching `size_of::<RAWINPUTDEVICE>()`.
    if unsafe { RegisterRawInputDevices(&[rid], size_of::<RAWINPUTDEVICE>() as u32) }.is_err() {
        // SAFETY: Clean up window & class on registration failure.
        unsafe {
            let _ = DestroyWindow(hwnd);
            let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
        }
        publish_error(
            &snapshot,
            "Windows denied RegisterRawInputDevices for mouse input.",
            &on_update,
        );
        return;
    }

    STATE.with(|cell| {
        *cell.borrow_mut() = Some(CaptureThreadState {
            qpc_freq,
            polling: PollingAnalyzer::new(initial_target),
            dpi_cal: DpiCalibrator::default(),
            clicks: ClickBounceAnalyzer::new(),
            last_target_raw: initial_target_raw,
            snapshot: Arc::clone(&snapshot),
            reset_requested,
            dpi_measuring,
            dpi_reset_requested,
            target_hz_atomic,
            on_update: Box::new(on_update),
        });
    });

    msg_hwnd.store(hwnd.0 as isize, Ordering::SeqCst);

    // If `stop()` raced before `msg_hwnd` was published, exit immediately without entering GetMessageW.
    if running.load(Ordering::SeqCst) {
        // SAFETY: `hwnd` is our own valid window on this thread.
        unsafe {
            let _ = SetTimer(Some(hwnd), TIMER_PUBLISH_ID, TIMER_PUBLISH_MS, None);
        }

        let mut msg = MSG::default();
        // Kernel-blocking message pump: wakes immediately on every `WM_INPUT` arrival (even 8K Hz)
        // and publishes snapshots every 33 ms on `WM_TIMER` without busy-waiting or sleep bunching.
        // SAFETY: Standard Win32 thread message loop.
        while running.load(Ordering::SeqCst) && unsafe { GetMessageW(&mut msg, None, 0, 0).0 > 0 } {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // SAFETY: Clean up timer on our own window before tearing it down.
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_PUBLISH_ID);
        }
    }

    // Unregister Raw Input and destroy window cleanly.
    let rid_remove = RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x02,
        dwFlags: RIDEV_REMOVE,
        hwndTarget: HWND::default(),
    };
    // SAFETY: Standard cleanup matching `RegisterRawInputDevices` above.
    unsafe {
        let _ = RegisterRawInputDevices(&[rid_remove], size_of::<RAWINPUTDEVICE>() as u32);
        let _ = DestroyWindow(hwnd);
        let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
    }
    msg_hwnd.store(0, Ordering::SeqCst);

    if let Ok(mut guard) = snapshot.lock() {
        guard.capturing = false;
    }
    STATE.with(|cell| {
        if let Some(state) = cell.borrow_mut().take() {
            (state.on_update)();
        }
    });
}

#[cfg(windows)]
fn decode_target(raw: u32) -> PollingTarget {
    if raw == TARGET_AUTO_SENTINEL {
        PollingTarget::Auto
    } else {
        PollingTarget::FixedHz(raw)
    }
}

#[cfg(windows)]
fn publish_error<F: Fn()>(snapshot: &Arc<Mutex<DiagnosticsSnapshot>>, msg: &str, on_update: &F) {
    if let Ok(mut guard) = snapshot.lock() {
        guard.capturing = false;
        guard.error = Some(msg.to_string());
    }
    on_update();
}
