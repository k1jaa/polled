// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

//! Live input performance diagnostics (Input Lab):
//! - Sub-microsecond Win32 Raw Input polling consistency & wireless gap/bunching analysis (up to 8K)
//! - Mouse switch debounce & double-click bounce tester
//! - On-demand 1-second OS timer jitter benchmark

pub mod analyzer;
pub mod raw_capture;

pub use analyzer::{
    format_diagnostic_report, snap_nominal_hz, ButtonBounceStats, ClickBounceAnalyzer,
    ClickBounceSnapshot, ClickEventRecord, DiagnosticsSnapshot, HistogramBin, IntervalSample,
    MouseButtonKind, PollingAnalyzer, PollingStats, PollingTarget, TimerJitterBenchmark,
    BOUNCE_THRESHOLD_MS, HISTOGRAM_BASE_BINS, PAUSE_THRESHOLD_US, STANDARD_RATES_HZ,
};
pub use raw_capture::RawInputCaptureSession;

use crate::error::Result;

/// Runs an on-demand 1-second Windows 1 ms timer jitter benchmark (non-elevated, RAII cleanup).
pub fn run_timer_jitter_benchmark(duration_ms: u64) -> Result<TimerJitterBenchmark> {
    #[cfg(windows)]
    {
        crate::win::timer::measure_timer_jitter_benchmark(duration_ms)
    }
    #[cfg(not(windows))]
    {
        let _ = duration_ms;
        Ok(TimerJitterBenchmark {
            requested_ms: 1,
            elapsed_ms: 0.0,
            samples_ms: Vec::new(),
            min_ms: 0.0,
            median_ms: 0.0,
            p95_ms: 0.0,
            p99_ms: 0.0,
            max_ms: 0.0,
            spike_count_over_3ms: 0,
            severity: crate::checks::Severity::Info,
        })
    }
}
