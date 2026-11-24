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
    build_diagnostic_advice, format_advice_summary, format_diagnostic_report, snap_nominal_hz,
    ButtonBounceStats, ClickBounceAnalyzer, ClickBounceSnapshot, ClickEventRecord,
    DiagnosticAdvice, DiagnosticsSnapshot, DpiCalibrationResult, DpiCalibrator,
    DpiMeasurementSnapshot, HistogramBin, IntervalSample, KeyboardPollingAnalyzer,
    KeyboardPollingStats, MouseButtonKind, PinnedBenchmark, PollingAnalyzer, PollingStats,
    PollingTarget, TimerJitterBenchmark, TrackingStats, XyTracePoint, BOUNCE_THRESHOLD_MS,
    GRAVITY_MPS2, HISTOGRAM_BASE_BINS, KEY_CHATTER_THRESHOLD_MS, PAUSE_THRESHOLD_US,
    STANDARD_RATES_HZ,
};
pub use export::{format_intervals_csv, render_ab_comparison_png, render_report_png};
pub use raw_capture::RawInputCaptureSession;

use crate::error::Result;
use crate::win::{AudioLatencyBenchmark, NetworkJitterBenchmark};

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

/// Runs an on-demand dual-hop ICMP ping jitter & packet-loss probe (local gateway + target IPv4).
pub fn run_network_jitter_benchmark(
    target_ip: &str,
    count: usize,
    interval_ms: u64,
) -> Result<NetworkJitterBenchmark> {
    #[cfg(windows)]
    {
        crate::win::network::measure_network_jitter_benchmark(target_ip, count, interval_ms)
    }
    #[cfg(not(windows))]
    {
        let _ = (target_ip, count, interval_ms);
        Ok(NetworkJitterBenchmark {
            adapter_name: "Unavailable (non-Windows)".to_string(),
            adapter_kind: crate::win::NetworkAdapterKind::Other,
            elapsed_ms: 0.0,
            gateway: None,
            internet: crate::win::PingHopStats {
                label: "Internet Target".to_string(),
                target_ip: target_ip.to_string(),
                sent: 0,
                received: 0,
                lost: 0,
                loss_pct: 0.0,
                samples_ms: Vec::new(),
                min_ms: None,
                median_ms: None,
                p95_ms: None,
                max_ms: None,
                jitter_ms: None,
                severity: crate::checks::Severity::Info,
            },
            diagnosis: "ICMP probe is only available on Windows.".to_string(),
            severity: crate::checks::Severity::Info,
        })
    }
}

/// Formats a compact plain-text summary block for a dual-hop ICMP network jitter benchmark.
pub fn format_network_benchmark_summary(bench: &NetworkJitterBenchmark) -> String {
    let gw_line = match &bench.gateway {
        Some(g) => format!(
            "\n- Local Gateway ({}): median {:.2} ms | p95 {:.2} ms | max {:.2} ms | jitter {:.2} ms | loss {:.0}%",
            g.target_ip,
            g.median_ms.unwrap_or(0.0),
            g.p95_ms.unwrap_or(0.0),
            g.max_ms.unwrap_or(0.0),
            g.jitter_ms.unwrap_or(0.0),
            g.loss_pct
        ),
        None => String::new(),
    };
    let net = &bench.internet;
    format!(
        "Dual-Hop Network Ping Benchmark ({}): {}{gw_line}\n\
         - Internet ({}): median {:.1} ms | p95 {:.1} ms | max {:.1} ms | jitter {:.1} ms | loss {:.0}% [{}]\n\
         - Diagnosis: {}",
        bench.adapter_kind.label(),
        bench.adapter_name,
        net.target_ip,
        net.median_ms.unwrap_or(0.0),
        net.p95_ms.unwrap_or(0.0),
        net.max_ms.unwrap_or(0.0),
        net.jitter_ms.unwrap_or(0.0),
        net.loss_pct,
        bench.severity.as_str().to_uppercase(),
        bench.diagnosis,
    )
}
