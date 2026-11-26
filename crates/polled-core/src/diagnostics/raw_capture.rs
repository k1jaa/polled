// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dedicated Win32 Raw Input (`WM_INPUT` + `RIDEV_INPUTSINK`) capture thread using
//! `QueryPerformanceCounter` timestamps so GUI framerate never skews 4K/8K measurements.
//!
//! Architecture (zero self-inflicted measurement jitter):
//! - **Capture thread (`GetMessageW`, `THREAD_PRIORITY_HIGHEST`)**: strictly timestamps each
//!   `WM_INPUT` arrival via `QueryPerformanceCounter`, decodes `RAWINPUT`, and pushes a tiny
//!   `Copy` `RawEvent` into a pre-allocated buffer. Performs zero sorting, zero histogram
//!   computation, zero `WM_TIMER` callbacks, and zero UI wakeups.
//! - **Analyzer thread (~30 Hz)**: performs a ~50 ns `Vec` swap to drain queued events,
//!   updates all statistical analyzers, publishes `DiagnosticsSnapshot`, and wakes the GUI.
//! - Stopping capture unregisters Raw Input, joins the capture thread, drains remaining
//!   events, and joins the analyzer thread cleanly (0% CPU when idle).
//!
//! 100% anti-cheat safe: standard Win32 Raw Input API, zero hooks, zero DLL injection, no admin.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::diagnostics::analyzer::{
    ClickBounceAnalyzer, DiagnosticsSnapshot, DpiCalibrator, KeyboardPollingAnalyzer,
    MouseButtonKind, PollingAnalyzer, PollingTarget, ScrollEncoderAnalyzer, SystemLoadStats,
    KEY_CHATTER_THRESHOLD_MS,
};

const TARGET_AUTO_SENTINEL: u32 = 0;
const INITIAL_QUEUE_CAPACITY: usize = 4_096;
const PUBLISH_INTERVAL_MS: u64 = 33;

#[derive(Debug, Clone, Copy)]
enum RawEvent {
    MouseMotion {
        ts_ns: u64,
        dx: i32,
        dy: i32,
    },
    MouseButton {
        ts_ns: u64,
        button: MouseButtonKind,
        pressed: bool,
    },
    MouseWheel {
        ts_ns: u64,
        delta: i16,
    },
    Key {
        ts_ns: u64,
        slot: u8,
        pressed: bool,
    },
}

/// Live handle controlling an active background Win32 Raw Input capture session.
pub struct RawInputCaptureSession {
    snapshot: Arc<Mutex<DiagnosticsSnapshot>>,
    running: Arc<AtomicBool>,
    analyzer_stop: Arc<AtomicBool>,
    reset_requested: Arc<AtomicBool>,
    dpi_measuring: Arc<AtomicBool>,
    dpi_reset_requested: Arc<AtomicBool>,
    target_hz_atomic: Arc<AtomicU32>,
    key_chatter_threshold_tenths: Arc<AtomicU32>,
    last_left_down_qpc_ns: Arc<AtomicU64>,
    msg_hwnd: Arc<AtomicIsize>,
    wake_signal: Arc<(Mutex<u64>, Condvar)>,
    capture_handle: Option<JoinHandle<()>>,
    analyzer_handle: Option<JoinHandle<()>>,
}

impl RawInputCaptureSession {
    /// Starts capturing raw mouse motion, button, and keyboard timing events on a dedicated
    /// Win32 message thread decoupled from snapshot analysis.
    ///
    /// `on_update` is called at ~30 Hz whenever a fresh snapshot is published (e.g. `ctx.request_repaint()`).
    pub fn start<F>(target: PollingTarget, on_update: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
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
        let analyzer_stop = Arc::new(AtomicBool::new(false));
        let reset_requested = Arc::new(AtomicBool::new(false));
        let dpi_measuring = Arc::new(AtomicBool::new(false));
        let dpi_reset_requested = Arc::new(AtomicBool::new(false));
        let target_hz_atomic = Arc::new(AtomicU32::new(initial_target_raw));
        let key_chatter_threshold_tenths =
            Arc::new(AtomicU32::new((KEY_CHATTER_THRESHOLD_MS * 10.0) as u32));
        let last_left_down_qpc_ns = Arc::new(AtomicU64::new(0));
        let msg_hwnd = Arc::new(AtomicIsize::new(0));
        let event_queue = Arc::new(Mutex::new(Vec::<RawEvent>::with_capacity(
            INITIAL_QUEUE_CAPACITY,
        )));
        let wake_signal = Arc::new((Mutex::new(0u64), Condvar::new()));
        let on_update: Arc<dyn Fn() + Send + Sync> = Arc::new(on_update);

        let snap_cap = Arc::clone(&snapshot);
        let run_cap = Arc::clone(&running);
        let stop_cap = Arc::clone(&analyzer_stop);
        let hwnd_cap = Arc::clone(&msg_hwnd);
        let click_ns_cap = Arc::clone(&last_left_down_qpc_ns);
        let queue_cap = Arc::clone(&event_queue);
        let wake_cap = Arc::clone(&wake_signal);
        let update_cap = Arc::clone(&on_update);

        let capture_handle = std::thread::Builder::new()
            .name("polled-raw-capture".to_string())
            .spawn(move || {
                #[cfg(windows)]
                run_win32_capture_loop(
                    snap_cap,
                    run_cap,
                    stop_cap,
                    hwnd_cap,
                    click_ns_cap,
                    queue_cap,
                    wake_cap,
                    update_cap,
                );

                #[cfg(not(windows))]
                {
                    let _ = (run_cap, hwnd_cap, click_ns_cap, queue_cap);
                    if let Ok(mut guard) = snap_cap.lock() {
                        guard.capturing = false;
                        guard.error =
                            Some("Raw Input capture is only available on Windows.".to_string());
                    }
                    stop_cap.store(true, Ordering::SeqCst);
                    notify_condvar(&wake_cap);
                    (update_cap)();
                }
            })
            .ok();

        let snap_ana = Arc::clone(&snapshot);
        let stop_ana = Arc::clone(&analyzer_stop);
        let reset_ana = Arc::clone(&reset_requested);
        let dpi_meas_ana = Arc::clone(&dpi_measuring);
        let dpi_reset_ana = Arc::clone(&dpi_reset_requested);
        let target_ana = Arc::clone(&target_hz_atomic);
        let chatter_thr_ana = Arc::clone(&key_chatter_threshold_tenths);
        let queue_ana = Arc::clone(&event_queue);
        let wake_ana = Arc::clone(&wake_signal);
        let update_ana = Arc::clone(&on_update);

        let analyzer_handle = std::thread::Builder::new()
            .name("polled-raw-analyzer".to_string())
            .spawn(move || {
                run_analyzer_loop(
                    initial_target_raw,
                    snap_ana,
                    stop_ana,
                    reset_ana,
                    dpi_meas_ana,
                    dpi_reset_ana,
                    target_ana,
                    chatter_thr_ana,
                    queue_ana,
                    wake_ana,
                    update_ana,
                );
            })
            .ok();

        Self {
            snapshot,
            running,
            analyzer_stop,
            reset_requested,
            dpi_measuring,
            dpi_reset_requested,
            target_hz_atomic,
            key_chatter_threshold_tenths,
            last_left_down_qpc_ns,
            msg_hwnd,
            wake_signal,
            capture_handle,
            analyzer_handle,
        }
    }

    pub fn snapshot(&self) -> DiagnosticsSnapshot {
        self.snapshot.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Returns the QPC nanosecond timestamp of the most recent hardware left-button press
    /// observed on the high-priority `WM_INPUT` thread (0 if none yet).
    pub fn last_left_click_qpc_ns(&self) -> u64 {
        self.last_left_down_qpc_ns.load(Ordering::Acquire)
    }

    pub fn set_target(&self, target: PollingTarget) {
        let raw = match target {
            PollingTarget::Auto => TARGET_AUTO_SENTINEL,
            PollingTarget::FixedHz(hz) => hz,
        };
        self.target_hz_atomic.store(raw, Ordering::SeqCst);
        notify_condvar(&self.wake_signal);
    }

    pub fn set_key_chatter_threshold_ms(&self, ms: f64) {
        let tenths = (ms.clamp(5.0, 100.0) * 10.0).round() as u32;
        self.key_chatter_threshold_tenths
            .store(tenths, Ordering::SeqCst);
        notify_condvar(&self.wake_signal);
    }

    pub fn set_dpi_measuring(&self, active: bool) {
        self.dpi_measuring.store(active, Ordering::SeqCst);
        notify_condvar(&self.wake_signal);
    }

    pub fn request_dpi_reset(&self) {
        self.dpi_reset_requested.store(true, Ordering::SeqCst);
        notify_condvar(&self.wake_signal);
    }

    pub fn request_reset(&self) {
        self.reset_requested.store(true, Ordering::SeqCst);
        notify_condvar(&self.wake_signal);
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
        // Join capture thread first so no further events can be queued, then flush analyzer.
        if let Some(handle) = self.capture_handle.take() {
            let _ = handle.join();
        }
        self.analyzer_stop.store(true, Ordering::SeqCst);
        notify_condvar(&self.wake_signal);
        if let Some(handle) = self.analyzer_handle.take() {
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

fn notify_condvar(signal: &(Mutex<u64>, Condvar)) {
    if let Ok(mut seq) = signal.0.lock() {
        *seq = seq.wrapping_add(1);
        signal.1.notify_one();
    }
}

fn decode_target(raw: u32) -> PollingTarget {
    if raw == TARGET_AUTO_SENTINEL {
        PollingTarget::Auto
    } else {
        PollingTarget::FixedHz(raw)
    }
}

/// Returns the current monotonic `QueryPerformanceCounter` timestamp in nanoseconds on the
/// exact same clock domain as `RawInputCaptureSession::last_left_click_qpc_ns()`.
pub fn current_qpc_ns() -> u64 {
    #[cfg(windows)]
    {
        qpc_now_ns(qpc_frequency())
    }
    #[cfg(not(windows))]
    {
        0
    }
}

#[cfg(windows)]
fn qpc_now_ns(freq: u64) -> u64 {
    use windows::Win32::System::Performance::QueryPerformanceCounter;

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

#[cfg(windows)]
fn qpc_frequency() -> u64 {
    use windows::Win32::System::Performance::QueryPerformanceFrequency;

    let mut freq = 0i64;
    // SAFETY: `freq` is valid writable storage.
    unsafe {
        let _ = QueryPerformanceFrequency(&mut freq);
    }
    freq.max(1) as u64
}

#[cfg(windows)]
fn read_system_cpu_ticks() -> Option<(u64, u64, u64)> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetSystemTimes;

    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: Standard Win32 `GetSystemTimes` writes 64-bit tick counters into valid stack structs.
    unsafe {
        GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).ok()?;
    }
    let to_u64 = |ft: FILETIME| (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
    Some((to_u64(idle), to_u64(kernel), to_u64(user)))
}

#[cfg(not(windows))]
fn read_system_cpu_ticks() -> Option<(u64, u64, u64)> {
    None
}

/// Lightweight OS/CPU stall tracker running strictly on the ~30 Hz analyzer thread
/// (zero work on the `WM_INPUT` capture thread) to correlate late mouse packets against
/// Windows CPU saturation, kernel/DPC spikes, or scheduler wake overshoots.
#[derive(Debug, Default)]
struct SystemStallTracker {
    prev_ticks: Option<(u64, u64, u64)>,
    sample_count: u64,
    sum_cpu_pct: f64,
    peak_cpu_pct: f64,
    peak_kernel_pct: f64,
    scheduler_stalls: u64,
    max_wake_overshoot_ms: f64,
    last_window_was_stressed: bool,
    os_correlated_dropouts: u64,
    transport_only_dropouts: u64,
    prev_total_dropouts: u64,
}

impl SystemStallTracker {
    fn reset(&mut self) {
        *self = Self::default();
        self.prev_ticks = read_system_cpu_ticks();
    }

    fn sample_window(&mut self, wake_overshoot_ms: f64) {
        if wake_overshoot_ms > self.max_wake_overshoot_ms {
            self.max_wake_overshoot_ms = wake_overshoot_ms;
        }
        let mut stressed = false;
        if wake_overshoot_ms >= 15.0 {
            self.scheduler_stalls += 1;
            stressed = true;
        }

        if let Some((idle, kernel, user)) = read_system_cpu_ticks() {
            if let Some((p_idle, p_kernel, p_user)) = self.prev_ticks {
                let d_idle = idle.saturating_sub(p_idle);
                let d_kernel = kernel.saturating_sub(p_kernel);
                let d_user = user.saturating_sub(p_user);
                let d_total = d_kernel.saturating_add(d_user);
                if d_total > 0 {
                    // In Win32 `GetSystemTimes`, `kernel` includes `idle`.
                    let d_kernel_busy = d_kernel.saturating_sub(d_idle);
                    let d_busy = d_kernel_busy.saturating_add(d_user);
                    let cpu_pct = ((d_busy as f64) * 100.0 / (d_total as f64)).clamp(0.0, 100.0);
                    let kernel_pct =
                        ((d_kernel_busy as f64) * 100.0 / (d_total as f64)).clamp(0.0, 100.0);
                    self.sample_count += 1;
                    self.sum_cpu_pct += cpu_pct;
                    if cpu_pct > self.peak_cpu_pct {
                        self.peak_cpu_pct = cpu_pct;
                    }
                    if kernel_pct > self.peak_kernel_pct {
                        self.peak_kernel_pct = kernel_pct;
                    }
                    if cpu_pct >= 85.0 || kernel_pct >= 30.0 {
                        stressed = true;
                    }
                }
            }
            self.prev_ticks = Some((idle, kernel, user));
        }

        self.last_window_was_stressed = stressed;
    }

    fn correlate_new_dropouts(&mut self, current_true_dropouts: u64) {
        if current_true_dropouts > self.prev_total_dropouts {
            let delta = current_true_dropouts - self.prev_total_dropouts;
            if self.last_window_was_stressed {
                self.os_correlated_dropouts += delta;
            } else {
                self.transport_only_dropouts += delta;
            }
        }
        self.prev_total_dropouts = current_true_dropouts;
    }

    fn snapshot(&self) -> SystemLoadStats {
        let avg_cpu_pct = if self.sample_count > 0 {
            self.sum_cpu_pct / (self.sample_count as f64)
        } else {
            0.0
        };
        let total_corr = self.os_correlated_dropouts + self.transport_only_dropouts;
        let diagnosis = if self.sample_count == 0 {
            "Start a capture to correlate mouse dropouts against Windows CPU & scheduler stalls."
                .to_string()
        } else if total_corr == 0 && self.scheduler_stalls == 0 {
            format!(
                "Clean OS headroom — avg {avg_cpu_pct:.0}% CPU (peak {:.0}%, kernel {:.0}%), 0 scheduler stalls.",
                self.peak_cpu_pct, self.peak_kernel_pct
            )
        } else if self.os_correlated_dropouts > 0
            && self.os_correlated_dropouts >= self.transport_only_dropouts
        {
            format!(
                "OS/CPU-linked dropouts — {}/{total_corr} coincided with CPU/kernel spikes (peak {:.0}% CPU, {:.0}% kernel, {} stalls).",
                self.os_correlated_dropouts,
                self.peak_cpu_pct,
                self.peak_kernel_pct,
                self.scheduler_stalls
            )
        } else if self.transport_only_dropouts > 0 && self.os_correlated_dropouts == 0 {
            format!(
                "Wireless / USB isolated — 0/{total_corr} dropouts matched OS CPU stalls (avg {avg_cpu_pct:.0}% CPU, {} stalls).",
                self.scheduler_stalls
            )
        } else {
            format!(
                "Mixed causes — {} OS/CPU-linked vs {} wireless/USB-only dropouts (peak {:.0}% CPU, {} stalls).",
                self.os_correlated_dropouts,
                self.transport_only_dropouts,
                self.peak_cpu_pct,
                self.scheduler_stalls
            )
        };

        SystemLoadStats {
            sample_count: self.sample_count,
            avg_cpu_pct,
            peak_cpu_pct: self.peak_cpu_pct,
            peak_kernel_pct: self.peak_kernel_pct,
            scheduler_stalls: self.scheduler_stalls,
            max_wake_overshoot_ms: self.max_wake_overshoot_ms,
            os_correlated_dropouts: self.os_correlated_dropouts,
            transport_only_dropouts: self.transport_only_dropouts,
            diagnosis,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_analyzer_loop(
    initial_target_raw: u32,
    snapshot: Arc<Mutex<DiagnosticsSnapshot>>,
    analyzer_stop: Arc<AtomicBool>,
    reset_requested: Arc<AtomicBool>,
    dpi_measuring: Arc<AtomicBool>,
    dpi_reset_requested: Arc<AtomicBool>,
    target_hz_atomic: Arc<AtomicU32>,
    key_chatter_threshold_tenths: Arc<AtomicU32>,
    event_queue: Arc<Mutex<Vec<RawEvent>>>,
    wake_signal: Arc<(Mutex<u64>, Condvar)>,
    on_update: Arc<dyn Fn() + Send + Sync>,
) {
    #[cfg(windows)]
    let qpc_freq = qpc_frequency();

    let mut polling = PollingAnalyzer::new(decode_target(initial_target_raw));
    let mut dpi_cal = DpiCalibrator::default();
    let mut clicks = ClickBounceAnalyzer::new();
    let mut scroll = ScrollEncoderAnalyzer::new();
    let mut keyboard = KeyboardPollingAnalyzer::new();
    let mut sys_stall = SystemStallTracker::default();
    sys_stall.reset();
    let mut last_target_raw = initial_target_raw;
    let mut local_batch = Vec::<RawEvent>::with_capacity(INITIAL_QUEUE_CAPACITY);
    let mut last_seq = 0u64;
    let mut last_wake_overshoot_ms = 0.0f64;

    loop {
        let stopping = analyzer_stop.load(Ordering::SeqCst);

        // ~50 ns pointer swap under lock so the WM_INPUT capture thread never waits on analysis.
        local_batch.clear();
        if let Ok(mut shared) = event_queue.lock() {
            std::mem::swap(&mut *shared, &mut local_batch);
        }

        if reset_requested.swap(false, Ordering::SeqCst) {
            polling.reset();
            clicks.reset();
            scroll.reset();
            dpi_cal.reset();
            keyboard.reset();
            sys_stall.reset();
            local_batch.clear();
            last_wake_overshoot_ms = 0.0;
        }
        if dpi_reset_requested.swap(false, Ordering::SeqCst) {
            dpi_cal.reset();
        }
        let want_dpi = dpi_measuring.load(Ordering::SeqCst);
        if want_dpi != dpi_cal.is_active() {
            dpi_cal.set_active(want_dpi);
        }

        let target_raw = target_hz_atomic.load(Ordering::SeqCst);
        if target_raw != last_target_raw {
            polling.set_target(decode_target(target_raw));
            last_target_raw = target_raw;
        }
        let chatter_ms = f64::from(key_chatter_threshold_tenths.load(Ordering::SeqCst)) / 10.0;
        if (chatter_ms - keyboard.chatter_threshold_ms()).abs() > 0.01 {
            keyboard.set_chatter_threshold_ms(chatter_ms);
        }

        for ev in &local_batch {
            match *ev {
                RawEvent::MouseMotion { ts_ns, dx, dy } => {
                    polling.record_motion(ts_ns, dx, dy);
                    dpi_cal.record_motion(dx, dy);
                }
                RawEvent::MouseButton {
                    ts_ns,
                    button,
                    pressed,
                } => {
                    clicks.record_button(button, pressed, ts_ns);
                }
                RawEvent::MouseWheel { ts_ns, delta } => {
                    scroll.record_wheel(ts_ns, delta);
                }
                RawEvent::Key {
                    ts_ns,
                    slot,
                    pressed,
                } => {
                    keyboard.record_key(slot, pressed, ts_ns);
                }
            }
        }

        #[cfg(windows)]
        let now_ns = Some(qpc_now_ns(qpc_freq));
        #[cfg(not(windows))]
        let now_ns = None;

        sys_stall.sample_window(last_wake_overshoot_ms);
        let polling_snap = polling.snapshot(now_ns);
        sys_stall.correlate_new_dropouts(polling_snap.true_dropout_count);

        let prev_err = snapshot.lock().ok().and_then(|g| g.error.clone());
        let next_snap = DiagnosticsSnapshot {
            capturing: !stopping && prev_err.is_none(),
            target: polling.target(),
            polling: polling_snap,
            tracking: polling.tracking_snapshot(now_ns),
            dpi_measurement: dpi_cal.snapshot(),
            clicks: clicks.snapshot(),
            scroll: scroll.snapshot(),
            keyboard: keyboard.snapshot(),
            system_load: sys_stall.snapshot(),
            error: prev_err,
        };
        if let Ok(mut guard) = snapshot.lock() {
            *guard = next_snap;
        }
        (on_update)();

        if stopping {
            break;
        }

        last_wake_overshoot_ms = 0.0;
        if let Ok(guard) = wake_signal.0.lock() {
            if *guard == last_seq {
                let wait_start = std::time::Instant::now();
                let (guard_after, timeout_res) = wake_signal
                    .1
                    .wait_timeout(guard, Duration::from_millis(PUBLISH_INTERVAL_MS))
                    .unwrap_or_else(|e| e.into_inner());
                if timeout_res.timed_out() {
                    let actual_ms = wait_start.elapsed().as_secs_f64() * 1000.0;
                    last_wake_overshoot_ms = (actual_ms - (PUBLISH_INTERVAL_MS as f64)).max(0.0);
                }
                last_seq = *guard_after;
            } else {
                last_seq = *guard;
            }
        }
    }
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn run_win32_capture_loop(
    snapshot: Arc<Mutex<DiagnosticsSnapshot>>,
    running: Arc<AtomicBool>,
    analyzer_stop: Arc<AtomicBool>,
    msg_hwnd: Arc<AtomicIsize>,
    last_left_down_qpc_ns: Arc<AtomicU64>,
    event_queue: Arc<Mutex<Vec<RawEvent>>>,
    wake_signal: Arc<(Mutex<u64>, Condvar)>,
    on_update: Arc<dyn Fn() + Send + Sync>,
) {
    use std::cell::RefCell;
    use std::ffi::OsStr;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
    };
    use windows::Win32::UI::Input::{
        GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE,
        RAWINPUTHEADER, RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        PostQuitMessage, RegisterClassW, TranslateMessage, UnregisterClassW, HWND_MESSAGE, MSG,
        RI_KEY_BREAK, RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP, RI_MOUSE_BUTTON_5_DOWN,
        RI_MOUSE_BUTTON_5_UP, RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_LEFT_BUTTON_UP,
        RI_MOUSE_MIDDLE_BUTTON_DOWN, RI_MOUSE_MIDDLE_BUTTON_UP, RI_MOUSE_RIGHT_BUTTON_DOWN,
        RI_MOUSE_RIGHT_BUTTON_UP, RI_MOUSE_WHEEL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE,
        WM_DESTROY, WM_INPUT, WNDCLASSW,
    };

    struct CaptureThreadState {
        qpc_freq: u64,
        last_left_down_qpc_ns: Arc<AtomicU64>,
        queue: Arc<Mutex<Vec<RawEvent>>>,
    }

    thread_local! {
        static STATE: RefCell<Option<CaptureThreadState>> = const { RefCell::new(None) };
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
                    let borrow = cell.borrow();
                    let Some(state) = borrow.as_ref() else {
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
                    if copied == u32::MAX {
                        return;
                    }
                    if raw.header.dwType == RIM_TYPEKEYBOARD.0 {
                        // SAFETY: `dwType == RIM_TYPEKEYBOARD` guarantees `raw.data.keyboard` is active.
                        let kb = unsafe { raw.data.keyboard };
                        let pressed = (u32::from(kb.Flags) & RI_KEY_BREAK) == 0;
                        let slot = (kb.VKey & 0xFF) as u8;
                        if let Ok(mut q) = state.queue.lock() {
                            q.push(RawEvent::Key {
                                ts_ns,
                                slot,
                                pressed,
                            });
                        }
                        return;
                    }
                    if raw.header.dwType != RIM_TYPEMOUSE.0 {
                        return;
                    }

                    // SAFETY: `dwType == RIM_TYPEMOUSE` guarantees `raw.data.mouse` is active.
                    let mouse = unsafe { raw.data.mouse };
                    let dx = mouse.lLastX;
                    let dy = mouse.lLastY;
                    // SAFETY: Standard Windows `RAWMOUSE` button flags union access.
                    let flags = u32::from(unsafe { mouse.Anonymous.Anonymous.usButtonFlags });

                    if dx == 0 && dy == 0 && flags == 0 {
                        return;
                    }

                    if let Ok(mut q) = state.queue.lock() {
                        if dx != 0 || dy != 0 {
                            q.push(RawEvent::MouseMotion { ts_ns, dx, dy });
                        }
                        if flags != 0 {
                            if (flags & RI_MOUSE_LEFT_BUTTON_DOWN) != 0 {
                                state.last_left_down_qpc_ns.store(ts_ns, Ordering::Release);
                                q.push(RawEvent::MouseButton {
                                    ts_ns,
                                    button: MouseButtonKind::Left,
                                    pressed: true,
                                });
                            }
                            if (flags & RI_MOUSE_LEFT_BUTTON_UP) != 0 {
                                q.push(RawEvent::MouseButton {
                                    ts_ns,
                                    button: MouseButtonKind::Left,
                                    pressed: false,
                                });
                            }
                            if (flags & RI_MOUSE_RIGHT_BUTTON_DOWN) != 0 {
                                q.push(RawEvent::MouseButton {
                                    ts_ns,
                                    button: MouseButtonKind::Right,
                                    pressed: true,
                                });
                            }
                            if (flags & RI_MOUSE_RIGHT_BUTTON_UP) != 0 {
                                q.push(RawEvent::MouseButton {
                                    ts_ns,
                                    button: MouseButtonKind::Right,
                                    pressed: false,
                                });
                            }
                            if (flags & RI_MOUSE_MIDDLE_BUTTON_DOWN) != 0 {
                                q.push(RawEvent::MouseButton {
                                    ts_ns,
                                    button: MouseButtonKind::Middle,
                                    pressed: true,
                                });
                            }
                            if (flags & RI_MOUSE_MIDDLE_BUTTON_UP) != 0 {
                                q.push(RawEvent::MouseButton {
                                    ts_ns,
                                    button: MouseButtonKind::Middle,
                                    pressed: false,
                                });
                            }
                            if (flags & RI_MOUSE_BUTTON_4_DOWN) != 0 {
                                q.push(RawEvent::MouseButton {
                                    ts_ns,
                                    button: MouseButtonKind::SideBack,
                                    pressed: true,
                                });
                            }
                            if (flags & RI_MOUSE_BUTTON_4_UP) != 0 {
                                q.push(RawEvent::MouseButton {
                                    ts_ns,
                                    button: MouseButtonKind::SideBack,
                                    pressed: false,
                                });
                            }
                            if (flags & RI_MOUSE_BUTTON_5_DOWN) != 0 {
                                q.push(RawEvent::MouseButton {
                                    ts_ns,
                                    button: MouseButtonKind::SideForward,
                                    pressed: true,
                                });
                            }
                            if (flags & RI_MOUSE_BUTTON_5_UP) != 0 {
                                q.push(RawEvent::MouseButton {
                                    ts_ns,
                                    button: MouseButtonKind::SideForward,
                                    pressed: false,
                                });
                            }
                            if (flags & RI_MOUSE_WHEEL) != 0 {
                                // SAFETY: `RI_MOUSE_WHEEL` guarantees `usButtonData` holds a signed wheel delta.
                                let delta =
                                    unsafe { mouse.Anonymous.Anonymous.usButtonData } as i16;
                                if delta != 0 {
                                    q.push(RawEvent::MouseWheel { ts_ns, delta });
                                }
                            }
                        }
                    };
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

    // Elevate only this ultra-lightweight WM_INPUT timestamping thread while actively recording.
    // SAFETY: Setting priority on our own current thread has no memory preconditions.
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
    }

    let qpc_freq = qpc_frequency();

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
            &analyzer_stop,
            &wake_signal,
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
                &analyzer_stop,
                &wake_signal,
                &on_update,
            );
            return;
        }
    };

    let rids = [
        RAWINPUTDEVICE {
            usUsagePage: 0x01, // Generic Desktop Controls
            usUsage: 0x02,     // Mouse
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        },
        RAWINPUTDEVICE {
            usUsagePage: 0x01, // Generic Desktop Controls
            usUsage: 0x06,     // Keyboard (anonymous QPC timing only)
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        },
    ];
    // SAFETY: `rids` is a valid array matching `size_of::<RAWINPUTDEVICE>()`.
    if unsafe { RegisterRawInputDevices(&rids, size_of::<RAWINPUTDEVICE>() as u32) }.is_err() {
        // SAFETY: Clean up window & class on registration failure.
        unsafe {
            let _ = DestroyWindow(hwnd);
            let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
        }
        publish_error(
            &snapshot,
            "Windows denied RegisterRawInputDevices for mouse/keyboard input.",
            &analyzer_stop,
            &wake_signal,
            &on_update,
        );
        return;
    }

    STATE.with(|cell| {
        *cell.borrow_mut() = Some(CaptureThreadState {
            qpc_freq,
            last_left_down_qpc_ns,
            queue: event_queue,
        });
    });

    msg_hwnd.store(hwnd.0 as isize, Ordering::SeqCst);

    // If `stop()` raced before `msg_hwnd` was published, exit immediately without entering GetMessageW.
    if running.load(Ordering::SeqCst) {
        let mut msg = MSG::default();
        // Kernel-blocking message pump: wakes immediately on every `WM_INPUT` arrival (even 8K Hz)
        // with zero timers, zero sorting, and zero allocations on this thread.
        // SAFETY: Standard Win32 thread message loop.
        while running.load(Ordering::SeqCst) && unsafe { GetMessageW(&mut msg, None, 0, 0).0 > 0 } {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    // Unregister Raw Input and destroy window cleanly.
    let rid_remove = [
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02,
            dwFlags: RIDEV_REMOVE,
            hwndTarget: HWND::default(),
        },
        RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x06,
            dwFlags: RIDEV_REMOVE,
            hwndTarget: HWND::default(),
        },
    ];
    // SAFETY: Standard cleanup matching `RegisterRawInputDevices` above.
    unsafe {
        let _ = RegisterRawInputDevices(&rid_remove, size_of::<RAWINPUTDEVICE>() as u32);
        let _ = DestroyWindow(hwnd);
        let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
    }
    msg_hwnd.store(0, Ordering::SeqCst);
    STATE.with(|cell| {
        cell.borrow_mut().take();
    });
}

#[cfg(windows)]
fn publish_error(
    snapshot: &Arc<Mutex<DiagnosticsSnapshot>>,
    msg: &str,
    analyzer_stop: &Arc<AtomicBool>,
    wake_signal: &Arc<(Mutex<u64>, Condvar)>,
    on_update: &Arc<dyn Fn() + Send + Sync>,
) {
    if let Ok(mut guard) = snapshot.lock() {
        guard.capturing = false;
        guard.error = Some(msg.to_string());
    }
    analyzer_stop.store(true, Ordering::SeqCst);
    notify_condvar(wake_signal);
    (on_update)();
}
