// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pure-Rust statistical analyzers for high-polling mouse motion consistency (up to 8,000 Hz),
//! motion-saturated wireless dropout vs. slow-movement separation, interval distribution
//! histograms, smoothed IPS/G sensor tracking, measured DPI calibration, and switch bounce testing.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::checks::Severity;

/// Maximum interval (in microseconds) treated as continuous mouse motion rather than a user pause.
/// Any gap longer than 40 ms resets the continuous streak instead of counting as a dropout.
pub const PAUSE_THRESHOLD_US: f64 = 40_000.0;

/// Minimum human release-to-press interval in milliseconds.
/// Unintentional switch contact bounce or too-low debounce typically fires in 1–30 ms.
pub const BOUNCE_THRESHOLD_MS: f64 = 35.0;

/// Minimum same-key release-to-press interval (ms) below which keyboard switch chatter is flagged.
pub const KEY_CHATTER_THRESHOLD_MS: f64 = 25.0;

/// Standard mouse polling rates used when snapping auto-detected nominal cadence.
pub const STANDARD_RATES_HZ: [u32; 7] = [125, 250, 500, 1000, 2000, 4000, 8000];

/// Number of equal-width bins spanning `0.0 .. 2.8x` nominal interval (plus 1 overflow bin).
pub const HISTOGRAM_BASE_BINS: usize = 28;

/// Standard gravity in m/s² used to convert mouse acceleration into G-force.
pub const GRAVITY_MPS2: f64 = 9.80665;

const ROLLING_INTERVAL_CAPACITY: usize = 1_200;
const CHART_POINTS_LIMIT: usize = 400;
const EFFECTIVE_WINDOW_NS: u64 = 500_000_000; // 500 ms rolling rate window
const SPEED_WINDOW_NS: u64 = 4_000_000; // 4 ms sliding window suppresses 4K/8K quantization spikes
const SPEED_MIN_SPAN_NS: u64 = 1_500_000; // 1.5 ms minimum span for clean IPS calculation
const ACCEL_MIN_DT_NS: u64 = 4_000_000; // 4 ms minimum span for clean G calculation
const RECENT_CLICK_LOG_LIMIT: usize = 24;

/// Target polling rate used to evaluate jitter, wireless gaps, and bunched bursts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PollingTarget {
    #[default]
    Auto,
    FixedHz(u32),
}

impl PollingTarget {
    pub fn label(self) -> String {
        match self {
            PollingTarget::Auto => "Auto-detect".to_string(),
            PollingTarget::FixedHz(hz) => format!("{hz} Hz"),
        }
    }
}

/// Mouse button tracked by the switch debounce / bounce analyzer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButtonKind {
    Left,
    Right,
    Middle,
    SideBack,
    SideForward,
}

impl MouseButtonKind {
    pub const ALL: [MouseButtonKind; 5] = [
        MouseButtonKind::Left,
        MouseButtonKind::Right,
        MouseButtonKind::Middle,
        MouseButtonKind::SideBack,
        MouseButtonKind::SideForward,
    ];

    pub fn index(self) -> usize {
        match self {
            MouseButtonKind::Left => 0,
            MouseButtonKind::Right => 1,
            MouseButtonKind::Middle => 2,
            MouseButtonKind::SideBack => 3,
            MouseButtonKind::SideForward => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MouseButtonKind::Left => "Left",
            MouseButtonKind::Right => "Right",
            MouseButtonKind::Middle => "Middle",
            MouseButtonKind::SideBack => "Side 4",
            MouseButtonKind::SideForward => "Side 5",
        }
    }
}

/// Single plotted motion interval with its saturation flag, raw counts, and smoothed speed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IntervalSample {
    pub dt_us: f64,
    pub dx: i32,
    pub dy: i32,
    /// Smoothed local speed in counts/sec at this sample (divide by DPI for IPS).
    pub speed_cps: f64,
    /// `true` when motion velocity was high enough to guarantee >= 1–2 counts per poll slot,
    /// so any long gap represents a genuine wireless/USB dropout rather than slow hand speed.
    pub saturated: bool,
}

/// Single bin in the polling interval distribution histogram.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistogramBin {
    pub start_us: f64,
    pub end_us: f64,
    pub is_overflow: bool,
    pub saturated_count: u32,
    pub unsaturated_count: u32,
}

impl HistogramBin {
    pub fn total_count(&self) -> u32 {
        self.saturated_count + self.unsaturated_count
    }
}

/// Single point on the 2D XY relative motion trace.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct XyTracePoint {
    pub x: f32,
    pub y: f32,
    pub speed_cps: f32,
}

/// Live & peak velocity/acceleration metrics (stored in raw counts/sec so DPI can change live).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackingStats {
    /// Live smoothed speed in counts/sec (`0.0` when mouse is stationary).
    pub current_cps: f64,
    /// Peak smoothed flick speed observed in counts/sec.
    pub peak_cps: f64,
    /// Peak smoothed acceleration observed in counts/sec².
    pub peak_accel_cps2: f64,
    /// Recent relative 2D path points (up to 400 points) for the XY Trace plot.
    pub xy_trace: Vec<XyTracePoint>,
}

impl TrackingStats {
    pub fn current_ips(&self, dpi: f64) -> f64 {
        if dpi > 0.0 {
            self.current_cps / dpi
        } else {
            0.0
        }
    }

    pub fn peak_ips(&self, dpi: f64) -> f64 {
        if dpi > 0.0 {
            self.peak_cps / dpi
        } else {
            0.0
        }
    }

    pub fn peak_mps(&self, dpi: f64) -> f64 {
        self.peak_ips(dpi) * 0.0254
    }

    pub fn peak_accel_g(&self, dpi: f64) -> f64 {
        if dpi > 0.0 {
            (self.peak_accel_cps2 / dpi) * 0.0254 / GRAVITY_MPS2
        } else {
            0.0
        }
    }
}

/// Raw counts accumulated during a physical ruler DPI calibration slide.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DpiMeasurementSnapshot {
    pub active: bool,
    pub net_x: i64,
    pub net_y: i64,
    pub net_counts: f64,
    pub path_counts: f64,
    pub straightness_pct: f64,
    pub packet_count: u64,
}

/// Computed real-world DPI calibration result for a known physical ruler distance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DpiCalibrationResult {
    pub measured_dpi: f64,
    pub deviation_pct: f64,
    /// Multiply in-game sensitivity by this factor to match a true `configured_dpi` sensor.
    pub sensitivity_multiplier: f64,
    pub severity: Severity,
}

impl DpiMeasurementSnapshot {
    /// Computes real measured DPI and deviation against `configured_dpi` for `distance_inches`.
    pub fn evaluate(
        &self,
        configured_dpi: f64,
        distance_inches: f64,
    ) -> Option<DpiCalibrationResult> {
        if self.net_counts < 10.0 || configured_dpi <= 0.0 || distance_inches <= 0.0 {
            return None;
        }
        let measured_dpi = self.net_counts / distance_inches;
        let deviation_pct = ((measured_dpi - configured_dpi) / configured_dpi) * 100.0;
        let sensitivity_multiplier = if measured_dpi > 0.0 {
            configured_dpi / measured_dpi
        } else {
            1.0
        };
        let abs_dev = deviation_pct.abs();
        let severity = if abs_dev <= 2.5 {
            Severity::Ok
        } else if abs_dev <= 5.5 {
            Severity::Info
        } else {
            Severity::Warn
        };
        Some(DpiCalibrationResult {
            measured_dpi,
            deviation_pct,
            sensitivity_multiplier,
            severity,
        })
    }
}

/// Accumulates raw relative counts during an explicit physical-ruler DPI test run.
#[derive(Debug, Default)]
pub struct DpiCalibrator {
    active: bool,
    net_x: i64,
    net_y: i64,
    path_counts: f64,
    packet_count: u64,
}

impl DpiCalibrator {
    pub fn set_active(&mut self, active: bool) {
        if active && !self.active {
            // Starting a fresh measurement run clears previous counts automatically.
            self.net_x = 0;
            self.net_y = 0;
            self.path_counts = 0.0;
            self.packet_count = 0;
        }
        self.active = active;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn reset(&mut self) {
        self.net_x = 0;
        self.net_y = 0;
        self.path_counts = 0.0;
        self.packet_count = 0;
    }

    pub fn record_motion(&mut self, dx: i32, dy: i32) {
        if !self.active || (dx == 0 && dy == 0) {
            return;
        }
        self.net_x += i64::from(dx);
        self.net_y += i64::from(dy);
        self.path_counts += f64::from(dx).hypot(f64::from(dy));
        self.packet_count += 1;
    }

    pub fn snapshot(&self) -> DpiMeasurementSnapshot {
        let net_counts = (self.net_x as f64).hypot(self.net_y as f64);
        let straightness_pct = if self.path_counts > 0.0 {
            (net_counts / self.path_counts * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        DpiMeasurementSnapshot {
            active: self.active,
            net_x: self.net_x,
            net_y: self.net_y,
            net_counts,
            path_counts: self.path_counts,
            straightness_pct,
            packet_count: self.packet_count,
        }
    }
}

/// Summary statistics for raw mouse polling timing consistency and wireless/USB health.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PollingStats {
    pub total_motion_packets: u64,
    pub active_intervals: u64,
    pub saturated_intervals: u64,
    /// Percentage (0.0..=100.0) of rolling active intervals that saturated the polling rate.
    pub saturated_pct: f64,
    pub effective_hz: f64,
    pub peak_hz: f64,
    pub nominal_hz: u32,
    pub nominal_interval_us: f64,
    pub min_us: f64,
    pub median_us: f64,
    pub p95_us: f64,
    pub p99_us: f64,
    pub max_us: f64,
    pub std_dev_us: f64,
    /// Percentage (0.0..=100.0) of all rolling intervals within ±15% of `nominal_interval_us`.
    pub within_15_pct: f64,
    /// Percentage (0.0..=100.0) of motion-saturated intervals within ±15% of `nominal_interval_us`.
    pub saturated_within_15_pct: f64,
    /// True wireless/USB dropouts: gaps > 1.85x nominal while motion-saturated.
    pub true_dropout_count: u64,
    /// Harmless slow-movement empty slots: gaps > 1.85x nominal during low-speed movement.
    pub slow_slot_count: u64,
    /// Total gaps > 1.85x nominal (`true_dropout_count + slow_slot_count`).
    pub gap_count: u64,
    /// Intervals < 0.35x nominal: coalesced or bunched report bursts.
    pub bunch_count: u64,
    pub health_severity: Option<Severity>,
    pub health_summary: String,
    /// Up to the most recent 400 active interval samples for plotting.
    pub recent_samples: Vec<IntervalSample>,
    /// Binned interval distribution (`0 .. 2.8x` nominal in `0.1x` steps + overflow bin).
    pub histogram_bins: Vec<HistogramBin>,
}

/// Per-button double-click and switch bounce metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ButtonBounceStats {
    pub button: MouseButtonKind,
    pub press_count: u64,
    pub last_hold_ms: Option<f64>,
    pub last_release_to_press_ms: Option<f64>,
    pub min_release_to_press_ms: Option<f64>,
    /// Number of unintentional switch-bounce repeats (release -> next press < 35 ms).
    pub bounce_faults: u64,
}

impl ButtonBounceStats {
    fn new(button: MouseButtonKind) -> Self {
        Self {
            button,
            press_count: 0,
            last_hold_ms: None,
            last_release_to_press_ms: None,
            min_release_to_press_ms: None,
            bounce_faults: 0,
        }
    }
}

/// Single recent click entry displayed in the click debounce log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClickEventRecord {
    pub button: MouseButtonKind,
    pub release_to_press_ms: Option<f64>,
    pub hold_ms: Option<f64>,
    pub is_bounce_fault: bool,
}

/// Aggregate snapshot of the click bounce analyzer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClickBounceSnapshot {
    pub buttons: Vec<ButtonBounceStats>,
    pub total_presses: u64,
    pub total_bounce_faults: u64,
    pub recent_events: Vec<ClickEventRecord>,
}

impl Default for ClickBounceSnapshot {
    fn default() -> Self {
        Self {
            buttons: MouseButtonKind::ALL
                .into_iter()
                .map(ButtonBounceStats::new)
                .collect(),
            total_presses: 0,
            total_bounce_faults: 0,
            recent_events: Vec::new(),
        }
    }
}

/// Full result of an on-demand 1-second Windows timer jitter benchmark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimerJitterBenchmark {
    pub requested_ms: u32,
    pub elapsed_ms: f64,
    pub samples_ms: Vec<f64>,
    pub min_ms: f64,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub spike_count_over_3ms: usize,
    pub severity: Severity,
}

/// Privacy-preserving keyboard USB scan-rate & switch chatter statistics.
///
/// Key characters and virtual key codes are NEVER recorded or exported — only anonymous
/// high-resolution QPC timing intervals between non-repeat key transitions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KeyboardPollingStats {
    pub total_events: u64,
    pub key_down_count: u64,
    pub key_up_count: u64,
    pub min_interval_ms: Option<f64>,
    pub median_interval_ms: Option<f64>,
    pub min_hold_ms: Option<f64>,
    pub min_release_to_press_ms: Option<f64>,
    pub estimated_scan_hz: Option<u32>,
    pub chatter_faults: u64,
    pub recent_intervals_ms: Vec<f64>,
    pub health_summary: String,
}

/// Pinned baseline Input Lab capture used for side-by-side Before (A) vs After (B) comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinnedBenchmark {
    pub label: String,
    pub configured_dpi: u32,
    pub snapshot: DiagnosticsSnapshot,
    pub timer: Option<TimerJitterBenchmark>,
}

/// Immutable snapshot published periodically (~30 Hz) from the capture thread to the GUI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsSnapshot {
    pub capturing: bool,
    pub target: PollingTarget,
    pub polling: PollingStats,
    pub tracking: TrackingStats,
    pub dpi_measurement: DpiMeasurementSnapshot,
    pub clicks: ClickBounceSnapshot,
    #[serde(default)]
    pub keyboard: KeyboardPollingStats,
    pub error: Option<String>,
}

/// Snaps a measured median interval (in microseconds) to the nearest standard polling rate.
pub fn snap_nominal_hz(median_us: f64) -> u32 {
    if median_us <= 0.0 || !median_us.is_finite() {
        return 1000;
    }
    let measured_hz = 1_000_000.0 / median_us;
    STANDARD_RATES_HZ
        .into_iter()
        .min_by(|&a, &b| {
            let da = (f64::from(a) - measured_hz).abs() / f64::from(a);
            let db = (f64::from(b) - measured_hz).abs() / f64::from(b);
            da.total_cmp(&db)
        })
        .unwrap_or(1000)
}

#[derive(Debug, Clone, Copy)]
struct RawIntervalRecord {
    dt_us: f64,
    dx: i32,
    dy: i32,
    axis_counts: f64,
    inst_rate_cps: f64,
    prev_rate_cps: f64,
    smoothed_cps: f64,
}

impl RawIntervalRecord {
    /// Returns `true` when motion velocity was high enough that the mouse sensor was guaranteed
    /// to have movement data ready on every poll slot at `nominal_hz`.
    fn is_saturated(&self, nominal_hz: u32, nominal_us: f64) -> bool {
        let sat_rate_cps = f64::from(nominal_hz.max(125)) * 1.4;
        if self.inst_rate_cps >= sat_rate_cps {
            return true;
        }
        // Catch mid-flick radio dropouts where the receiver dropped one or more frames while
        // the mouse was already moving fast and delivered >= 2 accumulated counts on recovery.
        if self.dt_us > nominal_us * 1.85
            && self.axis_counts >= 2.0
            && self.prev_rate_cps >= sat_rate_cps * 0.8
        {
            return true;
        }
        // Bunched packets (< 0.35x nominal) are always transport/OS coalescing anomalies, never slow hand.
        self.dt_us < nominal_us * 0.35 && self.prev_rate_cps >= sat_rate_cps * 0.6
    }
}

#[derive(Debug, Clone, Copy)]
struct SpeedWindowPacket {
    ts_ns: u64,
    dx: i32,
    dy: i32,
}

/// High-precision mouse polling interval, wireless health, and IPS/G/XY tracking analyzer.
#[derive(Debug, Default)]
pub struct PollingAnalyzer {
    target: PollingTarget,
    last_motion_ns: Option<u64>,
    last_rate_cps: f64,
    total_motion_packets: u64,
    active_intervals: u64,
    rolling: VecDeque<RawIntervalRecord>,
    recent_timestamps_ns: VecDeque<u64>,
    speed_window: VecDeque<SpeedWindowPacket>,
    current_cps: f64,
    peak_cps: f64,
    last_accel_sample: Option<(u64, f64)>,
    peak_accel_cps2: f64,
    xy_cursor: (f32, f32),
    xy_trace: VecDeque<XyTracePoint>,
    peak_hz: f64,
    cumulative_true_dropouts: u64,
    cumulative_slow_slots: u64,
    cumulative_bunches: u64,
}

impl PollingAnalyzer {
    pub fn new(target: PollingTarget) -> Self {
        Self {
            target,
            ..Default::default()
        }
    }

    pub fn set_target(&mut self, target: PollingTarget) {
        self.target = target;
        self.recompute_anomaly_counts_from_rolling();
    }

    pub fn target(&self) -> PollingTarget {
        self.target
    }

    pub fn reset(&mut self) {
        let target = self.target;
        *self = Self::new(target);
    }

    /// Records a single raw mouse motion packet timestamped in monotonic nanoseconds (`QPC`).
    pub fn record_motion(&mut self, timestamp_ns: u64, dx: i32, dy: i32) {
        if dx == 0 && dy == 0 {
            return;
        }
        self.total_motion_packets += 1;

        let mut is_pause = false;
        if let Some(prev_ns) = self.last_motion_ns {
            if timestamp_ns >= prev_ns {
                let delta_us = (timestamp_ns - prev_ns) as f64 / 1_000.0;
                if delta_us > PAUSE_THRESHOLD_US {
                    is_pause = true;
                }
            }
        }

        if is_pause {
            // User paused mouse movement; reset rolling speed/rate streaks cleanly.
            self.recent_timestamps_ns.clear();
            self.speed_window.clear();
            self.last_rate_cps = 0.0;
            self.current_cps = 0.0;
            self.last_accel_sample = None;
        }

        // Update 4 ms sliding speed window (filters 4K/8K sub-millisecond quantization spikes).
        self.speed_window.push_back(SpeedWindowPacket {
            ts_ns: timestamp_ns,
            dx,
            dy,
        });
        while let Some(front) = self.speed_window.front() {
            if timestamp_ns.saturating_sub(front.ts_ns) > SPEED_WINDOW_NS {
                self.speed_window.pop_front();
            } else {
                break;
            }
        }

        let mut smoothed_cps = self.current_cps;
        if self.speed_window.len() >= 2 {
            if let (Some(first), Some(last)) = (self.speed_window.front(), self.speed_window.back())
            {
                let span_ns = last.ts_ns.saturating_sub(first.ts_ns);
                if span_ns >= SPEED_MIN_SPAN_NS {
                    // Sum displacement after the opening anchor packet to match exact `span_ns`.
                    let (sum_x, sum_y) = self
                        .speed_window
                        .iter()
                        .skip(1)
                        .fold((0i64, 0i64), |(sx, sy), p| {
                            (sx + i64::from(p.dx), sy + i64::from(p.dy))
                        });
                    let dist = (sum_x as f64).hypot(sum_y as f64);
                    let cps = dist / (span_ns as f64 / 1_000_000_000.0);
                    if cps.is_finite() {
                        smoothed_cps = cps;
                        self.current_cps = cps;
                        self.peak_cps = self.peak_cps.max(cps);

                        if let Some((prev_a_ts, prev_a_cps)) = self.last_accel_sample {
                            let dt_ns = timestamp_ns.saturating_sub(prev_a_ts);
                            if dt_ns >= ACCEL_MIN_DT_NS {
                                let accel =
                                    (cps - prev_a_cps).abs() / (dt_ns as f64 / 1_000_000_000.0);
                                if accel.is_finite() {
                                    self.peak_accel_cps2 = self.peak_accel_cps2.max(accel);
                                }
                                self.last_accel_sample = Some((timestamp_ns, cps));
                            }
                        } else {
                            self.last_accel_sample = Some((timestamp_ns, cps));
                        }
                    }
                }
            }
        }

        // Update 2D XY motion trace.
        self.xy_cursor.0 += dx as f32;
        self.xy_cursor.1 += dy as f32;
        if self.xy_trace.len() >= CHART_POINTS_LIMIT {
            self.xy_trace.pop_front();
        }
        self.xy_trace.push_back(XyTracePoint {
            x: self.xy_cursor.0,
            y: self.xy_cursor.1,
            speed_cps: smoothed_cps as f32,
        });

        if let Some(prev_ns) = self.last_motion_ns {
            if !is_pause && timestamp_ns >= prev_ns {
                let delta_us = (timestamp_ns - prev_ns) as f64 / 1_000.0;
                if delta_us > 0.0 && delta_us <= PAUSE_THRESHOLD_US {
                    self.active_intervals += 1;
                    let axis_counts = dx.abs().max(dy.abs()) as f64;
                    let inst_rate_cps = axis_counts / (delta_us / 1_000_000.0);
                    let rec = RawIntervalRecord {
                        dt_us: delta_us,
                        dx,
                        dy,
                        axis_counts,
                        inst_rate_cps,
                        prev_rate_cps: self.last_rate_cps,
                        smoothed_cps,
                    };
                    self.last_rate_cps = inst_rate_cps;

                    if self.rolling.len() >= ROLLING_INTERVAL_CAPACITY {
                        self.rolling.pop_front();
                    }
                    self.rolling.push_back(rec);

                    if let Some((nominal_hz, nominal_us)) = self.current_nominal() {
                        if delta_us > nominal_us * 1.85 {
                            if rec.is_saturated(nominal_hz, nominal_us) {
                                self.cumulative_true_dropouts += 1;
                            } else {
                                self.cumulative_slow_slots += 1;
                            }
                        } else if delta_us < nominal_us * 0.35 {
                            self.cumulative_bunches += 1;
                        }
                    }
                }
            }
        }

        self.last_motion_ns = Some(timestamp_ns);
        self.recent_timestamps_ns.push_back(timestamp_ns);
        while let Some(&front) = self.recent_timestamps_ns.front() {
            if timestamp_ns.saturating_sub(front) > EFFECTIVE_WINDOW_NS {
                self.recent_timestamps_ns.pop_front();
            } else {
                break;
            }
        }

        if self.recent_timestamps_ns.len() >= 12 {
            if let (Some(&first), Some(&last)) = (
                self.recent_timestamps_ns.front(),
                self.recent_timestamps_ns.back(),
            ) {
                let span_ns = last.saturating_sub(first);
                if span_ns >= 80_000_000 {
                    let hz = ((self.recent_timestamps_ns.len() - 1) as f64)
                        / (span_ns as f64 / 1_000_000_000.0);
                    if hz.is_finite() && hz <= 20_000.0 {
                        self.peak_hz = self.peak_hz.max(hz);
                    }
                }
            }
        }
    }

    fn current_nominal(&self) -> Option<(u32, f64)> {
        match self.target {
            PollingTarget::FixedHz(hz) if hz > 0 => Some((hz, 1_000_000.0 / f64::from(hz))),
            _ => {
                if self.rolling.len() < 20 {
                    return None;
                }
                let mut sorted: Vec<f64> = self.rolling.iter().map(|r| r.dt_us).collect();
                sorted.sort_by(|a, b| a.total_cmp(b));
                let p25 = sorted[sorted.len() / 4];
                let hz = snap_nominal_hz(p25);
                Some((hz, 1_000_000.0 / f64::from(hz)))
            }
        }
    }

    fn recompute_anomaly_counts_from_rolling(&mut self) {
        let Some((nominal_hz, nominal_us)) = self.current_nominal() else {
            self.cumulative_true_dropouts = 0;
            self.cumulative_slow_slots = 0;
            self.cumulative_bunches = 0;
            return;
        };
        let mut dropouts = 0u64;
        let mut slow_slots = 0u64;
        let mut bunches = 0u64;
        for rec in &self.rolling {
            if rec.dt_us > nominal_us * 1.85 {
                if rec.is_saturated(nominal_hz, nominal_us) {
                    dropouts += 1;
                } else {
                    slow_slots += 1;
                }
            } else if rec.dt_us < nominal_us * 0.35 {
                bunches += 1;
            }
        }
        self.cumulative_true_dropouts = dropouts;
        self.cumulative_slow_slots = slow_slots;
        self.cumulative_bunches = bunches;
    }

    /// Returns a snapshot of smoothed velocity, peak acceleration, and the recent 2D XY trace.
    pub fn tracking_snapshot(&self, now_ns: Option<u64>) -> TrackingStats {
        let is_moving = match (now_ns, self.last_motion_ns) {
            (Some(now), Some(last)) => now.saturating_sub(last) <= 200_000_000,
            (None, Some(_)) => true,
            _ => false,
        };
        TrackingStats {
            current_cps: if is_moving { self.current_cps } else { 0.0 },
            peak_cps: self.peak_cps,
            peak_accel_cps2: self.peak_accel_cps2,
            xy_trace: self.xy_trace.iter().copied().collect(),
        }
    }

    /// Computes a complete snapshot of timing consistency, saturation, histogram, and wireless health.
    pub fn snapshot(&self, now_ns: Option<u64>) -> PollingStats {
        let mut effective_hz = 0.0;
        let is_actively_moving = match (now_ns, self.last_motion_ns) {
            (Some(now), Some(last)) => now.saturating_sub(last) <= 350_000_000,
            (None, Some(_)) => true,
            _ => false,
        };
        if is_actively_moving && self.recent_timestamps_ns.len() >= 4 {
            if let (Some(&first), Some(&last)) = (
                self.recent_timestamps_ns.front(),
                self.recent_timestamps_ns.back(),
            ) {
                let span_ns = last.saturating_sub(first);
                if span_ns >= 25_000_000 {
                    effective_hz = ((self.recent_timestamps_ns.len() - 1) as f64)
                        / (span_ns as f64 / 1_000_000_000.0);
                }
            }
        }

        if self.rolling.is_empty() {
            let nominal_hz = match self.target {
                PollingTarget::FixedHz(hz) => hz,
                PollingTarget::Auto => 0,
            };
            return PollingStats {
                total_motion_packets: self.total_motion_packets,
                nominal_hz,
                nominal_interval_us: if nominal_hz > 0 {
                    1_000_000.0 / f64::from(nominal_hz)
                } else {
                    0.0
                },
                health_summary: "Move your mouse continuously to measure polling consistency."
                    .to_string(),
                ..Default::default()
            };
        }

        let mut sorted: Vec<f64> = self.rolling.iter().map(|r| r.dt_us).collect();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let n = sorted.len();

        let min_us = sorted[0];
        let max_us = sorted[n - 1];
        let median_us = percentile_sorted(&sorted, 0.50);
        let p95_us = percentile_sorted(&sorted, 0.95);
        let p99_us = percentile_sorted(&sorted, 0.99);

        let mean = sorted.iter().sum::<f64>() / (n as f64);
        let variance = sorted
            .iter()
            .map(|&v| {
                let d = v - mean;
                d * d
            })
            .sum::<f64>()
            / (n as f64);
        let std_dev_us = variance.sqrt();

        let nominal_hz = match self.target {
            PollingTarget::FixedHz(hz) if hz > 0 => hz,
            _ => {
                let p25 = percentile_sorted(&sorted, 0.25);
                snap_nominal_hz(p25)
            }
        };
        let nominal_interval_us = 1_000_000.0 / f64::from(nominal_hz);

        let lo_ok = nominal_interval_us * 0.85;
        let hi_ok = nominal_interval_us * 1.15;

        let mut within_all = 0usize;
        let mut sat_total = 0usize;
        let mut sat_within = 0usize;
        let mut rolling_true_dropouts = 0usize;
        let mut rolling_slow_slots = 0usize;
        let mut rolling_bunches = 0usize;

        let bin_width = (nominal_interval_us * 0.10).max(5.0);
        let mut histogram_bins: Vec<HistogramBin> = (0..=HISTOGRAM_BASE_BINS)
            .map(|i| {
                let is_overflow = i == HISTOGRAM_BASE_BINS;
                HistogramBin {
                    start_us: (i as f64) * bin_width,
                    end_us: if is_overflow {
                        f64::INFINITY
                    } else {
                        ((i + 1) as f64) * bin_width
                    },
                    is_overflow,
                    saturated_count: 0,
                    unsaturated_count: 0,
                }
            })
            .collect();

        for rec in &self.rolling {
            let d = rec.dt_us;
            let sat = rec.is_saturated(nominal_hz, nominal_interval_us);
            let in_band = d >= lo_ok && d <= hi_ok;

            if in_band {
                within_all += 1;
            }
            if sat {
                sat_total += 1;
                if in_band {
                    sat_within += 1;
                }
            }

            if d > nominal_interval_us * 1.85 {
                if sat {
                    rolling_true_dropouts += 1;
                } else {
                    rolling_slow_slots += 1;
                }
            } else if d < nominal_interval_us * 0.35 {
                rolling_bunches += 1;
            }

            let bin_idx = ((d / bin_width).floor() as usize).min(HISTOGRAM_BASE_BINS);
            if sat {
                histogram_bins[bin_idx].saturated_count += 1;
            } else {
                histogram_bins[bin_idx].unsaturated_count += 1;
            }
        }

        let within_15_pct = (within_all as f64) * 100.0 / (n as f64);
        let saturated_pct = (sat_total as f64) * 100.0 / (n as f64);
        let saturated_within_15_pct = if sat_total > 0 {
            (sat_within as f64) * 100.0 / (sat_total as f64)
        } else {
            0.0
        };

        let (health_severity, health_summary) = if n < 40 {
            (
                Some(Severity::Info),
                format!("Collecting samples ({n}/40)... Move your mouse in brisk, steady circles."),
            )
        } else if sat_total < 25 {
            let needed_cps = (f64::from(nominal_hz) * 1.4).round() as u32;
            (
                Some(Severity::Info),
                format!(
                    "Hand speed too low to saturate {nominal_hz} Hz ({sat_total}/{n} saturated; {rolling_slow_slots} slow empty slots). Swipe slightly faster (~{needed_cps} counts/s) to stress-test wireless stability."
                ),
            )
        } else {
            let sat_anomaly_pct =
                ((rolling_true_dropouts + rolling_bunches) as f64) * 100.0 / (sat_total as f64);
            if saturated_within_15_pct >= 88.0 && sat_anomaly_pct < 1.5 {
                (
                    Some(Severity::Ok),
                    format!(
                        "Clean {nominal_hz} Hz link — {saturated_within_15_pct:.1}% of {sat_total} saturated polls within ±15% of {:.2} ms ({rolling_true_dropouts} true dropouts, {rolling_bunches} bunched; {rolling_slow_slots} slow slots ignored).",
                        nominal_interval_us / 1000.0,
                    ),
                )
            } else if saturated_within_15_pct >= 70.0 && sat_anomaly_pct < 5.0 {
                (
                    Some(Severity::Warn),
                    format!(
                        "Mild jitter at {nominal_hz} Hz — {saturated_within_15_pct:.1}% saturated consistency ({rolling_true_dropouts} true dropouts, {rolling_bunches} bunched). Try moving your wireless dongle closer or using a direct motherboard USB port."
                    ),
                )
            } else {
                (
                    Some(Severity::Bad),
                    format!(
                        "Unstable {nominal_hz} Hz delivery — {saturated_within_15_pct:.1}% saturated consistency ({rolling_true_dropouts} true dropouts, {rolling_bunches} bunched). Check 2.4 GHz interference, dongle distance, USB hub contention, or background DPC load."
                    ),
                )
            }
        };

        let chart_start = self.rolling.len().saturating_sub(CHART_POINTS_LIMIT);
        let recent_samples = self
            .rolling
            .iter()
            .skip(chart_start)
            .map(|r| IntervalSample {
                dt_us: r.dt_us,
                dx: r.dx,
                dy: r.dy,
                speed_cps: r.smoothed_cps,
                saturated: r.is_saturated(nominal_hz, nominal_interval_us),
            })
            .collect();

        let true_dropout_count = self
            .cumulative_true_dropouts
            .max(rolling_true_dropouts as u64);
        let slow_slot_count = self.cumulative_slow_slots.max(rolling_slow_slots as u64);

        PollingStats {
            total_motion_packets: self.total_motion_packets,
            active_intervals: self.active_intervals,
            saturated_intervals: sat_total as u64,
            saturated_pct,
            effective_hz,
            peak_hz: self.peak_hz.max(effective_hz),
            nominal_hz,
            nominal_interval_us,
            min_us,
            median_us,
            p95_us,
            p99_us,
            max_us,
            std_dev_us,
            within_15_pct,
            saturated_within_15_pct,
            true_dropout_count,
            slow_slot_count,
            gap_count: true_dropout_count + slow_slot_count,
            bunch_count: self.cumulative_bunches.max(rolling_bunches as u64),
            health_severity,
            health_summary,
            recent_samples,
            histogram_bins,
        }
    }
}

/// Tracks raw mouse button press/release intervals to catch double-click switch bounce.
#[derive(Debug)]
pub struct ClickBounceAnalyzer {
    stats: [ButtonBounceStats; 5],
    last_down_ns: [Option<u64>; 5],
    last_up_ns: [Option<u64>; 5],
    total_presses: u64,
    total_bounce_faults: u64,
    recent_events: VecDeque<ClickEventRecord>,
}

impl Default for ClickBounceAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl ClickBounceAnalyzer {
    pub fn new() -> Self {
        Self {
            stats: [
                ButtonBounceStats::new(MouseButtonKind::Left),
                ButtonBounceStats::new(MouseButtonKind::Right),
                ButtonBounceStats::new(MouseButtonKind::Middle),
                ButtonBounceStats::new(MouseButtonKind::SideBack),
                ButtonBounceStats::new(MouseButtonKind::SideForward),
            ],
            last_down_ns: [None; 5],
            last_up_ns: [None; 5],
            total_presses: 0,
            total_bounce_faults: 0,
            recent_events: VecDeque::with_capacity(RECENT_CLICK_LOG_LIMIT),
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn record_button(&mut self, button: MouseButtonKind, pressed: bool, timestamp_ns: u64) {
        let idx = button.index();
        if pressed {
            self.total_presses += 1;
            self.stats[idx].press_count += 1;
            self.last_down_ns[idx] = Some(timestamp_ns);

            let release_to_press_ms = self.last_up_ns[idx].and_then(|up_ns| {
                (timestamp_ns >= up_ns).then(|| (timestamp_ns - up_ns) as f64 / 1_000_000.0)
            });

            let mut is_bounce = false;
            if let Some(gap_ms) = release_to_press_ms {
                self.stats[idx].last_release_to_press_ms = Some(gap_ms);
                self.stats[idx].min_release_to_press_ms = Some(
                    self.stats[idx]
                        .min_release_to_press_ms
                        .map_or(gap_ms, |prev| prev.min(gap_ms)),
                );
                if gap_ms < BOUNCE_THRESHOLD_MS {
                    is_bounce = true;
                    self.stats[idx].bounce_faults += 1;
                    self.total_bounce_faults += 1;
                }
            }

            if self.recent_events.len() >= RECENT_CLICK_LOG_LIMIT {
                self.recent_events.pop_back();
            }
            self.recent_events.push_front(ClickEventRecord {
                button,
                release_to_press_ms,
                hold_ms: None,
                is_bounce_fault: is_bounce,
            });
        } else {
            self.last_up_ns[idx] = Some(timestamp_ns);
            if let Some(down_ns) = self.last_down_ns[idx] {
                if timestamp_ns >= down_ns {
                    let hold_ms = (timestamp_ns - down_ns) as f64 / 1_000_000.0;
                    self.stats[idx].last_hold_ms = Some(hold_ms);
                    if let Some(ev) = self
                        .recent_events
                        .iter_mut()
                        .find(|ev| ev.button == button && ev.hold_ms.is_none())
                    {
                        ev.hold_ms = Some(hold_ms);
                    }
                }
            }
        }
    }

    pub fn snapshot(&self) -> ClickBounceSnapshot {
        ClickBounceSnapshot {
            buttons: self.stats.to_vec(),
            total_presses: self.total_presses,
            total_bounce_faults: self.total_bounce_faults,
            recent_events: self.recent_events.iter().cloned().collect(),
        }
    }
}

/// Privacy-preserving keyboard USB polling / scan-rate and switch chatter analyzer.
///
/// Tracks only anonymous timing intervals and ignores OS auto-repeat events; never stores
/// or surfaces typed characters or key names.
#[derive(Debug)]
pub struct KeyboardPollingAnalyzer {
    held: [bool; 256],
    last_down_ns: [Option<u64>; 256],
    last_up_ns: [Option<u64>; 256],
    last_event_ns: Option<u64>,
    total_events: u64,
    key_down_count: u64,
    key_up_count: u64,
    min_interval_ms: Option<f64>,
    min_hold_ms: Option<f64>,
    min_release_to_press_ms: Option<f64>,
    chatter_faults: u64,
    recent_intervals_ms: VecDeque<f64>,
}

impl Default for KeyboardPollingAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyboardPollingAnalyzer {
    pub fn new() -> Self {
        Self {
            held: [false; 256],
            last_down_ns: [None; 256],
            last_up_ns: [None; 256],
            last_event_ns: None,
            total_events: 0,
            key_down_count: 0,
            key_up_count: 0,
            min_interval_ms: None,
            min_hold_ms: None,
            min_release_to_press_ms: None,
            chatter_faults: 0,
            recent_intervals_ms: VecDeque::with_capacity(64),
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn record_key(&mut self, slot: u8, pressed: bool, timestamp_ns: u64) {
        let idx = usize::from(slot);
        if pressed {
            // Ignore OS auto-repeat key-down events while key is already held.
            if self.held[idx] {
                return;
            }
            self.held[idx] = true;
            self.key_down_count += 1;
            if let Some(up_ns) = self.last_up_ns[idx] {
                if timestamp_ns >= up_ns {
                    let gap_ms = (timestamp_ns - up_ns) as f64 / 1_000_000.0;
                    if gap_ms >= 0.05 {
                        self.min_release_to_press_ms = Some(
                            self.min_release_to_press_ms
                                .map_or(gap_ms, |m| m.min(gap_ms)),
                        );
                        if gap_ms < KEY_CHATTER_THRESHOLD_MS {
                            self.chatter_faults += 1;
                        }
                    }
                }
            }
            self.last_down_ns[idx] = Some(timestamp_ns);
        } else {
            self.held[idx] = false;
            self.key_up_count += 1;
            if let Some(down_ns) = self.last_down_ns[idx] {
                if timestamp_ns >= down_ns {
                    let hold_ms = (timestamp_ns - down_ns) as f64 / 1_000_000.0;
                    if hold_ms >= 0.05 {
                        self.min_hold_ms =
                            Some(self.min_hold_ms.map_or(hold_ms, |m| m.min(hold_ms)));
                    }
                }
            }
            self.last_up_ns[idx] = Some(timestamp_ns);
        }

        self.total_events += 1;
        if let Some(prev_ns) = self.last_event_ns {
            if timestamp_ns > prev_ns {
                let dt_ms = (timestamp_ns - prev_ns) as f64 / 1_000_000.0;
                // Keep rapid transitions (0.05 ms ..= 500 ms) so pauses don't skew scan stats.
                if (0.05..=500.0).contains(&dt_ms) {
                    self.min_interval_ms =
                        Some(self.min_interval_ms.map_or(dt_ms, |m| m.min(dt_ms)));
                    if self.recent_intervals_ms.len() >= 64 {
                        self.recent_intervals_ms.pop_front();
                    }
                    self.recent_intervals_ms.push_back(dt_ms);
                }
            }
        }
        self.last_event_ns = Some(timestamp_ns);
    }

    pub fn snapshot(&self) -> KeyboardPollingStats {
        let mut sorted: Vec<f64> = self.recent_intervals_ms.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let median_interval_ms = if sorted.is_empty() {
            None
        } else {
            Some(percentile_sorted(&sorted, 0.50))
        };

        let estimated_scan_hz = self
            .min_interval_ms
            .filter(|&ms| ms <= 20.0)
            .map(|ms| snap_nominal_hz(ms * 1000.0));

        let health_summary = if self.total_events == 0 {
            "Tap or roll adjacent keys while capturing to measure keyboard scan rate & chatter (zero keystrokes stored)."
                .to_string()
        } else if self.chatter_faults > 0 {
            format!(
                "Detected {} possible key chatter fault(s) (< {:.0} ms release->press).",
                self.chatter_faults, KEY_CHATTER_THRESHOLD_MS
            )
        } else if let (Some(hz), Some(min_ms)) = (estimated_scan_hz, self.min_interval_ms) {
            format!(
                "Finest transition {:.2} ms (~{} Hz scan ceiling) with 0 chatter faults.",
                min_ms, hz
            )
        } else {
            "Roll adjacent keys rapidly (e.g. A+D) to resolve keyboard scan-rate quantum."
                .to_string()
        };

        KeyboardPollingStats {
            total_events: self.total_events,
            key_down_count: self.key_down_count,
            key_up_count: self.key_up_count,
            min_interval_ms: self.min_interval_ms,
            median_interval_ms,
            min_hold_ms: self.min_hold_ms,
            min_release_to_press_ms: self.min_release_to_press_ms,
            estimated_scan_hz,
            chatter_faults: self.chatter_faults,
            recent_intervals_ms: self.recent_intervals_ms.iter().copied().collect(),
            health_summary,
        }
    }
}

/// Formats a clean plain-text diagnostic report suitable for clipboard export or issue tickets.
pub fn format_diagnostic_report(
    snap: &DiagnosticsSnapshot,
    configured_dpi: u32,
    dpi_ruler_inches: Option<f64>,
    timer: Option<&TimerJitterBenchmark>,
) -> String {
    let p = &snap.polling;
    let dpi_f = f64::from(configured_dpi.max(1));
    let mut lines = vec![
        "Polled Input Lab Diagnostic Report".to_string(),
        format!(
            "Polling Rate:   {} Hz target | effective {:.0} Hz | peak {:.0} Hz",
            p.nominal_hz, p.effective_hz, p.peak_hz
        ),
        format!(
            "Intervals:      min {:.3} ms | median {:.3} ms | p95 {:.3} ms | p99 {:.3} ms | max {:.3} ms | sigma {:.3} ms",
            p.min_us / 1000.0,
            p.median_us / 1000.0,
            p.p95_us / 1000.0,
            p.p99_us / 1000.0,
            p.max_us / 1000.0,
            p.std_dev_us / 1000.0
        ),
        format!(
            "Saturation:     {:.1}% saturated ({} polls) | saturated +/-15%: {:.1}% | all +/-15%: {:.1}%",
            p.saturated_pct, p.saturated_intervals, p.saturated_within_15_pct, p.within_15_pct
        ),
        format!(
            "Anomalies:      true dropouts: {} | slow empty slots: {} | bunched: {} | packets: {}",
            p.true_dropout_count, p.slow_slot_count, p.bunch_count, p.total_motion_packets
        ),
        format!(
            "Tracking:       peak {:.1} IPS ({:.2} m/s) | peak accel {:.1} G (at {} DPI)",
            snap.tracking.peak_ips(dpi_f),
            snap.tracking.peak_mps(dpi_f),
            snap.tracking.peak_accel_g(dpi_f),
            configured_dpi
        ),
        format!("Assessment:     {}", p.health_summary),
    ];

    if let Some(dist_in) = dpi_ruler_inches {
        if let Some(cal) = snap.dpi_measurement.evaluate(dpi_f, dist_in) {
            lines.push(format!(
                "DPI Calibrator: configured {} DPI -> measured {:.0} DPI ({:+.1}%) | net {:.0} counts over {:.2} in ({:.1} cm) | straightness {:.1}% | sens match {:.3}x",
                configured_dpi,
                cal.measured_dpi,
                cal.deviation_pct,
                snap.dpi_measurement.net_counts,
                dist_in,
                dist_in * 2.54,
                snap.dpi_measurement.straightness_pct,
                cal.sensitivity_multiplier
            ));
        }
    }

    lines.push(format!(
        "Click Debounce: {} total presses | {} bounce faults (< {:.0} ms)",
        snap.clicks.total_presses, snap.clicks.total_bounce_faults, BOUNCE_THRESHOLD_MS
    ));

    for btn in &snap.clicks.buttons {
        if btn.press_count > 0 {
            let min_gap = btn
                .min_release_to_press_ms
                .map(|v| format!("{v:.1} ms"))
                .unwrap_or_else(|| "n/a".to_string());
            lines.push(format!(
                "  - {:<6}: {} presses | fastest release->press: {} | bounce faults: {}",
                btn.button.label(),
                btn.press_count,
                min_gap,
                btn.bounce_faults
            ));
        }
    }

    if snap.keyboard.total_events > 0 {
        let min_str = snap
            .keyboard
            .min_interval_ms
            .map(|v| format!("{v:.2} ms"))
            .unwrap_or_else(|| "n/a".to_string());
        let hz_str = snap
            .keyboard
            .estimated_scan_hz
            .map(|hz| format!("~{hz} Hz"))
            .unwrap_or_else(|| "n/a".to_string());
        lines.push(format!(
            "Keyboard Scan:  {} events | min transition {} ({}) | chatter faults: {}",
            snap.keyboard.total_events, min_str, hz_str, snap.keyboard.chatter_faults
        ));
    }

    if let Some(t) = timer {
        lines.push(format!(
            "Timer 1s Test:  median {:.2} ms | p95 {:.2} ms | p99 {:.2} ms | max {:.2} ms | spikes >3ms: {} ({} samples)",
            t.median_ms,
            t.p95_ms,
            t.p99_ms,
            t.max_ms,
            t.spike_count_over_3ms,
            t.samples_ms.len()
        ));
    }

    lines.join("\n")
}

fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64) * p).ceil() as usize;
    sorted[idx.saturating_sub(1).min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_4000hz_stream_scores_ok_with_zero_dropouts_and_ignores_pauses() {
        let mut analyzer = PollingAnalyzer::new(PollingTarget::Auto);
        let mut t_ns = 1_000_000_000u64;
        // 150 saturated packets spaced cleanly at 250 us (4000 Hz, 3 counts/poll = 12,000 cps)
        for i in 0..150u64 {
            let jitter = if i % 2 == 0 { 4_000 } else { 0 };
            analyzer.record_motion(t_ns + jitter, 3, -1);
            t_ns += 250_000;
        }
        // User pauses for 120 ms (must NOT count as a dropout!)
        t_ns += 120_000_000;
        for _ in 0..50 {
            analyzer.record_motion(t_ns, 3, 0);
            t_ns += 250_000;
        }

        let stats = analyzer.snapshot(Some(t_ns));
        assert_eq!(stats.nominal_hz, 4000);
        assert_eq!(stats.true_dropout_count, 0);
        assert_eq!(stats.slow_slot_count, 0);
        assert_eq!(stats.bunch_count, 0);
        assert!(stats.saturated_pct > 95.0);
        assert!(stats.saturated_within_15_pct > 95.0);
        assert_eq!(stats.health_severity, Some(Severity::Ok));
        let peak_bin =
            stats.histogram_bins[9].saturated_count + stats.histogram_bins[10].saturated_count;
        assert!(peak_bin >= 190, "peak_bin={peak_bin}");
    }

    #[test]
    fn slow_hand_movement_at_4000hz_counts_as_slow_slots_not_wireless_dropouts() {
        let mut analyzer = PollingAnalyzer::new(PollingTarget::FixedHz(4000));
        let mut t_ns = 1_000_000_000u64;
        for _ in 0..60 {
            analyzer.record_motion(t_ns, 3, 0);
            t_ns += 250_000;
        }
        for _ in 0..20 {
            t_ns += 750_000;
            analyzer.record_motion(t_ns, 1, 0);
        }

        let stats = analyzer.snapshot(Some(t_ns));
        assert_eq!(stats.nominal_hz, 4000);
        assert_eq!(stats.true_dropout_count, 0);
        assert!(stats.slow_slot_count >= 19);
        assert_eq!(stats.health_severity, Some(Severity::Ok));
    }

    #[test]
    fn detects_saturated_wireless_dropouts_bunches_and_2x_histogram_harmonic() {
        let mut analyzer = PollingAnalyzer::new(PollingTarget::FixedHz(4000));
        let mut t_ns = 1_000_000_000u64;
        for i in 0..100u64 {
            if i % 10 == 5 {
                t_ns += 500_000;
                analyzer.record_motion(t_ns, 6, 1);
            } else if i % 10 == 6 {
                t_ns += 40_000;
                analyzer.record_motion(t_ns, 2, 0);
            } else {
                t_ns += 250_000;
                analyzer.record_motion(t_ns, 3, 1);
            }
        }

        let stats = analyzer.snapshot(Some(t_ns));
        assert_eq!(stats.nominal_hz, 4000);
        assert!(stats.true_dropout_count >= 9);
        assert!(stats.bunch_count >= 9);
        assert_eq!(stats.health_severity, Some(Severity::Bad));
        assert!(stats.histogram_bins[20].saturated_count >= 9);
    }

    #[test]
    fn computes_smoothed_ips_and_g_acceleration_accurately() {
        let mut analyzer = PollingAnalyzer::new(PollingTarget::FixedHz(1000));
        let mut t_ns = 1_000_000_000u64;
        // Phase 1: 10 ms steady sweep at 16 counts/ms = 16,000 cps (= 10.0 IPS at 1600 DPI)
        for _ in 0..15 {
            analyzer.record_motion(t_ns, 16, 0);
            t_ns += 1_000_000;
        }
        // Phase 2: accelerate to 48 counts/ms = 48,000 cps (= 30.0 IPS at 1600 DPI)
        for _ in 0..15 {
            analyzer.record_motion(t_ns, 48, 0);
            t_ns += 1_000_000;
        }

        let tr = analyzer.tracking_snapshot(Some(t_ns));
        let peak_ips = tr.peak_ips(1600.0);
        assert!((peak_ips - 30.0).abs() < 1.0, "peak_ips={peak_ips}");
        assert!(
            tr.peak_accel_g(1600.0) > 5.0,
            "accel_g={}",
            tr.peak_accel_g(1600.0)
        );
        assert_eq!(tr.xy_trace.len(), 30);
    }

    #[test]
    fn dpi_calibrator_measures_true_cpi_deviation_and_sens_multiplier() {
        let mut cal = DpiCalibrator::default();
        // Inactive ignores motion
        cal.record_motion(500, 0);
        assert_eq!(cal.snapshot().packet_count, 0);

        cal.set_active(true);
        // Simulate sliding 10 cm (3.93700787 in) on a mouse configured at 1600 DPI that truly reads 1552 DPI (-3.0%)
        // Expected counts = 1552 * (10 / 2.54) = 6110.24 counts
        for _ in 0..611 {
            cal.record_motion(10, 0);
        }
        cal.set_active(false);
        // After stopping, extra motion must be ignored
        cal.record_motion(999, 0);

        let snap = cal.snapshot();
        assert!(!snap.active);
        assert_eq!(snap.net_x, 6110);
        assert!((snap.straightness_pct - 100.0).abs() < 0.01);

        let eval = snap.evaluate(1600.0, 10.0 / 2.54).unwrap();
        assert!(
            (eval.measured_dpi - 1552.0).abs() < 1.0,
            "dpi={}",
            eval.measured_dpi
        );
        assert!(
            (eval.deviation_pct - (-3.0)).abs() < 0.1,
            "dev={}",
            eval.deviation_pct
        );
        assert!((eval.sensitivity_multiplier - (1600.0 / eval.measured_dpi)).abs() < 0.001);
        assert_eq!(eval.severity, Severity::Info);
    }

    #[test]
    fn detects_switch_double_click_bounce_without_flagging_normal_clicks() {
        let mut clicks = ClickBounceAnalyzer::new();
        let mut t_ns = 1_000_000_000u64;

        clicks.record_button(MouseButtonKind::Left, true, t_ns);
        t_ns += 60_000_000;
        clicks.record_button(MouseButtonKind::Left, false, t_ns);
        t_ns += 120_000_000;

        clicks.record_button(MouseButtonKind::Left, true, t_ns);
        t_ns += 55_000_000;
        clicks.record_button(MouseButtonKind::Left, false, t_ns);

        t_ns += 12_000_000;
        clicks.record_button(MouseButtonKind::Left, true, t_ns);
        t_ns += 8_000_000;
        clicks.record_button(MouseButtonKind::Left, false, t_ns);

        let snap = clicks.snapshot();
        assert_eq!(snap.total_presses, 3);
        assert_eq!(snap.total_bounce_faults, 1);
        let left = &snap.buttons[MouseButtonKind::Left.index()];
        assert_eq!(left.bounce_faults, 1);
        assert!((left.min_release_to_press_ms.unwrap() - 12.0).abs() < 0.01);
    }

    #[test]
    fn keyboard_analyzer_ignores_repeats_detects_scan_rate_and_chatter() {
        let mut kb = KeyboardPollingAnalyzer::new();
        let mut t_ns = 1_000_000_000u64;

        // Key A down, repeated OS auto-repeat (must be ignored), then Key D down 0.25 ms later (~4000 Hz)
        kb.record_key(65, true, t_ns);
        t_ns += 50_000;
        kb.record_key(65, true, t_ns); // auto-repeat -> ignored
        t_ns += 200_000; // 0.25 ms since first key-down
        kb.record_key(68, true, t_ns);
        t_ns += 15_000_000;
        kb.record_key(65, false, t_ns);
        t_ns += 10_000_000; // 10 ms release->press chatter fault (< 25 ms)
        kb.record_key(65, true, t_ns);

        let snap = kb.snapshot();
        assert_eq!(snap.key_down_count, 3);
        assert_eq!(snap.key_up_count, 1);
        assert_eq!(snap.chatter_faults, 1);
        assert_eq!(snap.estimated_scan_hz, Some(4000));
    }
}
