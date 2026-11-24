// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Live input performance diagnostics (Input Lab):
//! - Sub-microsecond Win32 Raw Input polling consistency & wireless gap/bunching analysis (up to 8K)
//! - Mouse switch debounce & double-click bounce tester
//! - On-demand 1-second OS timer jitter benchmark

pub mod analyzer;
pub mod export;
pub mod raw_capture;

pub use analyzer::{
    format_diagnostic_report, snap_nominal_hz, ButtonBounceStats, ClickBounceAnalyzer,
    ClickBounceSnapshot, ClickEventRecord, DiagnosticsSnapshot, DpiCalibrationResult,
    DpiCalibrator, DpiMeasurementSnapshot, HistogramBin, IntervalSample, KeyboardPollingAnalyzer,
    KeyboardPollingStats, MouseButtonKind, PinnedBenchmark, PollingAnalyzer, PollingStats,
    PollingTarget, TimerJitterBenchmark, TrackingStats, XyTracePoint, BOUNCE_THRESHOLD_MS,
    GRAVITY_MPS2, HISTOGRAM_BASE_BINS, KEY_CHATTER_THRESHOLD_MS, PAUSE_THRESHOLD_US,
    STANDARD_RATES_HZ,
};
pub use export::{format_intervals_csv, render_ab_comparison_png, render_report_png};
pub use raw_capture::RawInputCaptureSession;

use crate::error::Result;
use crate::win::AudioLatencyBenchmark;

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

/// Runs an on-demand 1-second strictly silent shared-mode WASAPI audio wake-jitter benchmark.
pub fn run_audio_latency_benchmark(duration_ms: u64) -> Result<AudioLatencyBenchmark> {
    #[cfg(windows)]
    {
        crate::win::audio::measure_audio_latency_benchmark(duration_ms)
    }
    #[cfg(not(windows))]
    {
        let _ = duration_ms;
        Ok(AudioLatencyBenchmark {
            device_name: "Unavailable (non-Windows)".to_string(),
            sample_rate_hz: 48_000,
            channels: 2,
            default_period_ms: 10.0,
            min_period_ms: 10.0,
            active_buffer_ms: 10.0,
            elapsed_ms: 0.0,
            samples_ms: Vec::new(),
            min_ms: 0.0,
            median_ms: 0.0,
            p95_ms: 0.0,
            p99_ms: 0.0,
            max_ms: 0.0,
            glitch_count: 0,
            severity: crate::checks::Severity::Info,
        })
    }
}

/// Formats a compact plain-text summary block for a WASAPI audio latency benchmark.
pub fn format_audio_benchmark_summary(bench: &AudioLatencyBenchmark) -> String {
    format!(
        "WASAPI Audio Engine Benchmark (1s silent): {}\n\
         - Format: {} Hz · {}ch | Engine Period: {:.2} ms (min {:.2} ms) | Active Buffer: {:.2} ms\n\
         - Wake Intervals: median {:.2} ms | p95 {:.2} ms | p99 {:.2} ms | max {:.2} ms | glitches: {} [{}]",
        bench.device_name,
        bench.sample_rate_hz,
        bench.channels,
        bench.default_period_ms,
        bench.min_period_ms,
        bench.active_buffer_ms,
        bench.median_ms,
        bench.p95_ms,
        bench.p99_ms,
        bench.max_ms,
        bench.glitch_count,
        bench.severity.as_str().to_uppercase(),
    )
}
