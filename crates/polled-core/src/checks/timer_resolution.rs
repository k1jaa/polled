// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, Severity};
use crate::error::Result;
use crate::win::SystemEnv;

/// Non-elevated micro-benchmark checking how accurately Windows honors a 1 ms timer request.
///
/// Inspired by InputLab's Home timer test: temporarily requests `timeBeginPeriod(1)`, measures
/// `Sleep(1)` wake intervals, and immediately releases the timer period.
pub struct TimerResolutionCheck;

impl HealthCheck for TimerResolutionCheck {
    fn id(&self) -> &str {
        "timer_resolution"
    }

    fn name(&self) -> &str {
        "Windows Timer Resolution & Wake Jitter"
    }

    fn can_fix(&self) -> bool {
        false
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let sample = env.measure_timer_resolution()?;

        // On healthy Windows 10/11 systems with `timeBeginPeriod(1)`, `Sleep(1)` typically wakes
        // in 1.0–2.0 ms due to 0.5–1.0 ms kernel clock tick quantization.
        let severity = if sample.median_ms <= 2.2 && sample.p95_ms <= 3.5 {
            Severity::Ok
        } else if sample.median_ms <= 4.0 && sample.p95_ms <= 8.0 {
            Severity::Warn
        } else {
            Severity::Bad
        };

        let current_value = format!(
            "Median {:.2} ms · p95 {:.2} ms · max {:.2} ms ({}x {} ms sleeps)",
            sample.median_ms, sample.p95_ms, sample.max_ms, sample.samples, sample.requested_ms
        );

        let reason = match severity {
            Severity::Ok => {
                "Windows honored a 1 ms timer request cleanly with low wake jitter, keeping game loops and high-polling input scheduling responsive."
            }
            Severity::Warn => {
                "Elevated wake jitter was measured during a 1 ms timer test. Heavy background load, power throttling, or noisy drivers (DPC latency) can cause occasional input micro-stutters."
            }
            Severity::Bad => {
                "Severe timer wake delay detected (Windows did not wake near 1 ms). Check power-saving modes, Windows timer throttling, or runaway background processes."
            }
            Severity::Info => "",
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Median <= 2.2 ms, p95 <= 3.5 ms (1 ms request)".to_string(),
            reason: reason.to_string(),
            severity,
            can_fix: false,

            is_optional_tweak: false,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,

            requires_reboot: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;
    use crate::win::TimerResolutionSample;

    #[test]
    fn classifies_healthy_jittery_and_throttled_timers() {
        let env = MockEnv::new();
        assert_eq!(
            TimerResolutionCheck.scan(&env).unwrap().severity,
            Severity::Ok
        );

        env.set_timer_sample(TimerResolutionSample {
            requested_ms: 1,
            samples: 48,
            min_ms: 1.4,
            median_ms: 2.9,
            p95_ms: 5.2,
            max_ms: 6.4,
        });
        assert_eq!(
            TimerResolutionCheck.scan(&env).unwrap().severity,
            Severity::Warn
        );

        env.set_timer_sample(TimerResolutionSample {
            requested_ms: 1,
            samples: 48,
            min_ms: 14.8,
            median_ms: 15.6,
            p95_ms: 16.1,
            max_ms: 16.5,
        });
        assert_eq!(
            TimerResolutionCheck.scan(&env).unwrap().severity,
            Severity::Bad
        );
    }
}
