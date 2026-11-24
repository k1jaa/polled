// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

//! Pure-Rust statistical analyzers for high-polling mouse motion consistency (up to 8,000 Hz),
//! wireless gap/bunching detection, and mouse switch double-click bounce testing.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::checks::Severity;

/// Maximum interval (in microseconds) treated as continuous mouse motion rather than a user pause.
/// Any gap longer than 40 ms resets the continuous streak instead of counting as a dropout.
pub const PAUSE_THRESHOLD_US: f64 = 40_000.0;

/// Minimum human release-to-press interval in milliseconds.
/// Unintentional switch contact bounce or too-low debounce typically fires in 1–30 ms.
pub const BOUNCE_THRESHOLD_MS: f64 = 35.0;

/// Standard mouse polling rates used when snapping auto-detected nominal cadence.
pub const STANDARD_RATES_HZ: [u32; 7] = [125, 250, 500, 1000, 2000, 4000, 8000];

const ROLLING_INTERVAL_CAPACITY: usize = 1_200;
const CHART_POINTS_LIMIT: usize = 400;
const EFFECTIVE_WINDOW_NS: u64 = 500_000_000; // 500 ms rolling rate window
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

/// Summary statistics for raw mouse polling timing consistency.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PollingStats {
    pub total_motion_packets: u64,
    pub active_intervals: u64,
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
    /// Percentage (0.0..=100.0) of active intervals within ±15% of `nominal_interval_us`.
    pub within_15_pct: f64,
    /// Intervals > 1.85x nominal (and <= 40 ms): wireless dropouts, empty 4K/8K slots, or USB stalls.
    pub gap_count: u64,
    /// Intervals < 0.35x nominal: coalesced or bunched report bursts.
    pub bunch_count: u64,
    pub health_severity: Option<Severity>,
    pub health_summary: String,
    /// Up to the most recent 400 active interval samples in microseconds for plotting.
    pub recent_intervals_us: Vec<f64>,
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

/// Immutable snapshot published periodically (~30 Hz) from the capture thread to the GUI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsSnapshot {
    pub capturing: bool,
    pub target: PollingTarget,
    pub polling: PollingStats,
    pub clicks: ClickBounceSnapshot,
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

/// High-precision mouse polling interval & wireless health analyzer.
#[derive(Debug, Default)]
pub struct PollingAnalyzer {
    target: PollingTarget,
    last_motion_ns: Option<u64>,
    total_motion_packets: u64,
    active_intervals: u64,
    rolling_intervals_us: VecDeque<f64>,
    recent_timestamps_ns: VecDeque<u64>,
    peak_hz: f64,
    cumulative_gaps: u64,
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

        if let Some(prev_ns) = self.last_motion_ns {
            if timestamp_ns >= prev_ns {
                let delta_us = (timestamp_ns - prev_ns) as f64 / 1_000.0;
                if delta_us <= PAUSE_THRESHOLD_US {
                    self.active_intervals += 1;
                    if self.rolling_intervals_us.len() >= ROLLING_INTERVAL_CAPACITY {
                        self.rolling_intervals_us.pop_front();
                    }
                    self.rolling_intervals_us.push_back(delta_us);

                    if let Some(nominal_us) = self.current_nominal_interval_us() {
                        if delta_us > nominal_us * 1.85 {
                            self.cumulative_gaps += 1;
                        } else if delta_us < nominal_us * 0.35 {
                            self.cumulative_bunches += 1;
                        }
                    }
                } else {
                    // User paused mouse movement; reset rolling rate window cleanly.
                    self.recent_timestamps_ns.clear();
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

    fn current_nominal_interval_us(&self) -> Option<f64> {
        match self.target {
            PollingTarget::FixedHz(hz) if hz > 0 => Some(1_000_000.0 / f64::from(hz)),
            _ => {
                if self.rolling_intervals_us.len() < 20 {
                    return None;
                }
                let mut sorted: Vec<f64> = self.rolling_intervals_us.iter().copied().collect();
                sorted.sort_by(|a, b| a.total_cmp(b));
                let med = sorted[sorted.len() / 2];
                let hz = snap_nominal_hz(med);
                Some(1_000_000.0 / f64::from(hz))
            }
        }
    }

    fn recompute_anomaly_counts_from_rolling(&mut self) {
        let Some(nominal_us) = self.current_nominal_interval_us() else {
            self.cumulative_gaps = 0;
            self.cumulative_bunches = 0;
            return;
        };
        let mut gaps = 0u64;
        let mut bunches = 0u64;
        for &d in &self.rolling_intervals_us {
            if d > nominal_us * 1.85 {
                gaps += 1;
            } else if d < nominal_us * 0.35 {
                bunches += 1;
            }
        }
        self.cumulative_gaps = gaps;
        self.cumulative_bunches = bunches;
    }

    /// Computes a complete snapshot of timing consistency and wireless/USB health.
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

        if self.rolling_intervals_us.is_empty() {
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

        let mut sorted: Vec<f64> = self.rolling_intervals_us.iter().copied().collect();
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
            _ => snap_nominal_hz(median_us),
        };
        let nominal_interval_us = 1_000_000.0 / f64::from(nominal_hz);

        let lo_ok = nominal_interval_us * 0.85;
        let hi_ok = nominal_interval_us * 1.15;
        let within_count = self
            .rolling_intervals_us
            .iter()
            .filter(|&&d| d >= lo_ok && d <= hi_ok)
            .count();
        let within_15_pct = (within_count as f64) * 100.0 / (n as f64);

        let rolling_gaps = self
            .rolling_intervals_us
            .iter()
            .filter(|&&d| d > nominal_interval_us * 1.85)
            .count();
        let rolling_bunches = self
            .rolling_intervals_us
            .iter()
            .filter(|&&d| d < nominal_interval_us * 0.35)
            .count();
        let anomaly_pct = ((rolling_gaps + rolling_bunches) as f64) * 100.0 / (n as f64);

        let (health_severity, health_summary) = if n < 40 {
            (
                Some(Severity::Info),
                format!("Collecting samples ({n}/40)... Keep moving your mouse in steady circles."),
            )
        } else if within_15_pct >= 88.0 && anomaly_pct < 1.5 {
            (
                Some(Severity::Ok),
                format!(
                    "Clean {nominal_hz} Hz stream — {within_15_pct:.1}% within ±15% of {:.2} ms ({} gaps, {} bunched in last {n} reports).",
                    nominal_interval_us / 1000.0,
                    rolling_gaps,
                    rolling_bunches
                ),
            )
        } else if within_15_pct >= 70.0 && anomaly_pct < 5.0 {
            (
                Some(Severity::Warn),
                format!(
                    "Mild timing jitter at {nominal_hz} Hz — {within_15_pct:.1}% within ±15% ({rolling_gaps} gaps, {rolling_bunches} bunched). Try moving your wireless dongle closer or switching to a motherboard USB port."
                ),
            )
        } else {
            (
                Some(Severity::Bad),
                format!(
                    "Unstable {nominal_hz} Hz delivery — {within_15_pct:.1}% within ±15% ({rolling_gaps} gaps, {rolling_bunches} bunched). Check 2.4 GHz interference, dongle distance, USB hub contention, or background CPU/DPC load."
                ),
            )
        };

        let chart_start = self
            .rolling_intervals_us
            .len()
            .saturating_sub(CHART_POINTS_LIMIT);
        let recent_intervals_us = self
            .rolling_intervals_us
            .iter()
            .skip(chart_start)
            .copied()
            .collect();

        PollingStats {
            total_motion_packets: self.total_motion_packets,
            active_intervals: self.active_intervals,
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
            gap_count: self.cumulative_gaps.max(rolling_gaps as u64),
            bunch_count: self.cumulative_bunches.max(rolling_bunches as u64),
            health_severity,
            health_summary,
            recent_intervals_us,
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

/// Formats a clean plain-text diagnostic report suitable for clipboard export or issue tickets.
pub fn format_diagnostic_report(
    snap: &DiagnosticsSnapshot,
    timer: Option<&TimerJitterBenchmark>,
) -> String {
    let p = &snap.polling;
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
            "Consistency:    {:.1}% within +/-15% | gaps: {} | bunched: {} | packets: {}",
            p.within_15_pct, p.gap_count, p.bunch_count, p.total_motion_packets
        ),
        format!("Assessment:     {}", p.health_summary),
        format!(
            "Click Debounce: {} total presses | {} bounce faults (< {:.0} ms)",
            snap.clicks.total_presses, snap.clicks.total_bounce_faults, BOUNCE_THRESHOLD_MS
        ),
    ];

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
    fn clean_4000hz_stream_scores_ok_with_zero_gaps_and_ignores_pauses() {
        let mut analyzer = PollingAnalyzer::new(PollingTarget::Auto);
        let mut t_ns = 1_000_000_000u64;
        // 150 packets spaced cleanly at 250 us (4000 Hz)
        for i in 0..150u64 {
            // Slight realistic +/- 6 us jitter
            let jitter = if i % 2 == 0 { 4_000 } else { 0 };
            analyzer.record_motion(t_ns + jitter, 2, -1);
            t_ns += 250_000;
        }
        // User pauses for 120 ms (must NOT count as a wireless gap!)
        t_ns += 120_000_000;
        for _ in 0..50 {
            analyzer.record_motion(t_ns, 1, 0);
            t_ns += 250_000;
        }

        let stats = analyzer.snapshot(Some(t_ns));
        assert_eq!(stats.nominal_hz, 4000);
        assert_eq!(stats.gap_count, 0);
        assert_eq!(stats.bunch_count, 0);
        assert!(stats.within_15_pct > 95.0);
        assert_eq!(stats.health_severity, Some(Severity::Ok));
    }

    #[test]
    fn detects_wireless_gaps_and_bunched_bursts() {
        let mut analyzer = PollingAnalyzer::new(PollingTarget::FixedHz(4000));
        let mut t_ns = 1_000_000_000u64;
        for i in 0..100u64 {
            analyzer.record_motion(t_ns, 1, 1);
            if i % 10 == 5 {
                // 1.5 ms wireless dropout (> 1.85x 250 us)
                t_ns += 1_500_000;
            } else if i % 10 == 6 {
                // Bunched catch-up burst 40 us later (< 0.35x 250 us)
                t_ns += 40_000;
            } else {
                t_ns += 250_000;
            }
        }

        let stats = analyzer.snapshot(Some(t_ns));
        assert_eq!(stats.nominal_hz, 4000);
        assert!(stats.gap_count >= 9, "gaps={}", stats.gap_count);
        assert!(stats.bunch_count >= 9, "bunches={}", stats.bunch_count);
        assert_eq!(stats.health_severity, Some(Severity::Bad));
    }

    #[test]
    fn detects_switch_double_click_bounce_without_flagging_normal_clicks() {
        let mut clicks = ClickBounceAnalyzer::new();
        let mut t_ns = 1_000_000_000u64;

        // Normal click #1: hold 60 ms, wait 120 ms
        clicks.record_button(MouseButtonKind::Left, true, t_ns);
        t_ns += 60_000_000;
        clicks.record_button(MouseButtonKind::Left, false, t_ns);
        t_ns += 120_000_000;

        // Normal click #2: hold 55 ms
        clicks.record_button(MouseButtonKind::Left, true, t_ns);
        t_ns += 55_000_000;
        clicks.record_button(MouseButtonKind::Left, false, t_ns);

        // Faulty bounce 12 ms after release (< 35 ms threshold!)
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
}
