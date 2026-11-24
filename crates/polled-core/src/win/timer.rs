// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Non-elevated Windows 1 ms timer wake-jitter micro-test inspired by InputLab's Home timer check.
//!
//! Temporarily requests a 1 ms multimedia timer period via `timeBeginPeriod(1)`, measures
//! `Sleep(1)` wake intervals using Rust's `Instant` (backed by `QueryPerformanceCounter`),
//! and guarantees `timeEndPeriod(1)` runs on drop so Polled never pins the system timer.

use std::time::{Duration, Instant};

use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};

use crate::checks::Severity;
use crate::diagnostics::TimerJitterBenchmark;
use crate::error::Result;
use crate::win::TimerResolutionSample;

const WARMUP_SAMPLES: usize = 5;
const MEASURED_SAMPLES: usize = 48;

struct TimerPeriodGuard {
    period_ms: u32,
    active: bool,
}

impl TimerPeriodGuard {
    fn acquire(period_ms: u32) -> Self {
        // SAFETY: `timeBeginPeriod` is a documented Win32 multimedia timer call with no pointer args.
        let res = unsafe { timeBeginPeriod(period_ms) };
        Self {
            period_ms,
            active: res == 0,
        }
    }
}

impl Drop for TimerPeriodGuard {
    fn drop(&mut self) {
        if self.active {
            // SAFETY: Matches the successful `timeBeginPeriod(self.period_ms)` call above.
            unsafe {
                let _ = timeEndPeriod(self.period_ms);
            }
        }
    }
}

/// Runs a ~75 ms non-elevated 1 ms timer resolution micro-benchmark and returns wake statistics.
pub fn measure_timer_resolution() -> Result<TimerResolutionSample> {
    let _guard = TimerPeriodGuard::acquire(1);

    for _ in 0..WARMUP_SAMPLES {
        std::thread::sleep(Duration::from_millis(1));
    }

    let mut deltas_ms = Vec::with_capacity(MEASURED_SAMPLES);
    for _ in 0..MEASURED_SAMPLES {
        let start = Instant::now();
        std::thread::sleep(Duration::from_millis(1));
        deltas_ms.push(start.elapsed().as_secs_f64() * 1000.0);
    }

    deltas_ms.sort_by(|a, b| a.total_cmp(b));
    let n = deltas_ms.len();
    let min_ms = deltas_ms[0];
    let max_ms = deltas_ms[n - 1];
    let median_ms = if n % 2 == 0 {
        (deltas_ms[n / 2 - 1] + deltas_ms[n / 2]) * 0.5
    } else {
        deltas_ms[n / 2]
    };
    let p95_idx = ((n as f64) * 0.95).ceil() as usize;
    let p95_ms = deltas_ms[p95_idx.saturating_sub(1).min(n - 1)];

    Ok(TimerResolutionSample {
        requested_ms: 1,
        samples: n,
        min_ms,
        median_ms,
        p95_ms,
        max_ms,
    })
}

/// Runs a detailed on-demand Windows 1 ms timer jitter benchmark for `duration_ms` (e.g. 1,000 ms),
/// returning every wake sample in milliseconds so the GUI can plot scheduling/DPC spikes.
pub fn measure_timer_jitter_benchmark(duration_ms: u64) -> Result<TimerJitterBenchmark> {
    let _guard = TimerPeriodGuard::acquire(1);
    let clamped_ms = duration_ms.clamp(250, 5_000);
    let target = Duration::from_millis(clamped_ms);

    for _ in 0..WARMUP_SAMPLES {
        std::thread::sleep(Duration::from_millis(1));
    }

    let bench_start = Instant::now();
    let mut samples_ms = Vec::with_capacity((clamped_ms as usize).min(1_200));
    while bench_start.elapsed() < target {
        let s = Instant::now();
        std::thread::sleep(Duration::from_millis(1));
        samples_ms.push(s.elapsed().as_secs_f64() * 1000.0);
    }
    let elapsed_ms = bench_start.elapsed().as_secs_f64() * 1000.0;

    let mut sorted = samples_ms.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let n = sorted.len().max(1);
    let min_ms = sorted.first().copied().unwrap_or(0.0);
    let max_ms = sorted.last().copied().unwrap_or(0.0);
    let median_ms = sorted[n / 2];
    let p95_ms = sorted[((n as f64) * 0.95).ceil() as usize - 1];
    let p99_ms = sorted[((n as f64) * 0.99).ceil() as usize - 1];
    let spike_count_over_3ms = samples_ms.iter().filter(|&&v| v > 3.0).count();

    let severity = if median_ms <= 2.2 && p99_ms <= 4.0 && spike_count_over_3ms <= 2 {
        Severity::Ok
    } else if median_ms <= 3.5 && p99_ms <= 8.0 {
        Severity::Warn
    } else {
        Severity::Bad
    };

    Ok(TimerJitterBenchmark {
        requested_ms: 1,
        elapsed_ms,
        samples_ms,
        min_ms,
        median_ms,
        p95_ms,
        p99_ms,
        max_ms,
        spike_count_over_3ms,
        severity,
    })
}
