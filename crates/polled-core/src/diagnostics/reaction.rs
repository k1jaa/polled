// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Statistics & rating helpers for Bench's Raw-Input Click Reaction & Switch Consistency Test.

use serde::{Deserialize, Serialize};

use crate::checks::Severity;

/// Summary statistics over a set of visual-flash reaction trials timed on the Raw Input QPC clock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReactionTestSummary {
    pub trial_count: usize,
    pub false_starts: u32,
    pub best_ms: f64,
    pub median_ms: f64,
    pub mean_ms: f64,
    pub p95_ms: f64,
    pub stddev_ms: f64,
    pub rating: String,
    pub severity: Severity,
}

impl Default for ReactionTestSummary {
    fn default() -> Self {
        Self {
            trial_count: 0,
            false_starts: 0,
            best_ms: 0.0,
            median_ms: 0.0,
            mean_ms: 0.0,
            p95_ms: 0.0,
            stddev_ms: 0.0,
            rating: String::new(),
            severity: Severity::Info,
        }
    }
}

/// Computes median, best, mean, P95, and standard deviation across valid reaction times (ms).
pub fn compute_reaction_summary(trials_ms: &[f64], false_starts: u32) -> ReactionTestSummary {
    let mut valid: Vec<f64> = trials_ms
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v >= 60.0 && *v <= 2_500.0)
        .collect();
    if valid.is_empty() {
        return ReactionTestSummary {
            false_starts,
            rating: "Ready".to_string(),
            severity: Severity::Info,
            ..Default::default()
        };
    }

    valid.sort_by(f64::total_cmp);
    let n = valid.len();
    let best_ms = valid[0];
    let median_ms = if n % 2 == 1 {
        valid[n / 2]
    } else {
        (valid[n / 2 - 1] + valid[n / 2]) * 0.5
    };
    let p95_idx = ((n as f64) * 0.95).ceil() as usize;
    let p95_ms = valid[p95_idx.saturating_sub(1).min(n - 1)];
    let mean_ms = valid.iter().sum::<f64>() / (n as f64);
    let variance = valid
        .iter()
        .map(|v| {
            let d = v - mean_ms;
            d * d
        })
        .sum::<f64>()
        / (n as f64);
    let stddev_ms = variance.sqrt();

    let (severity, rating) = if median_ms <= 175.0 && stddev_ms <= 18.0 {
        (Severity::Ok, "Elite & consistent")
    } else if median_ms <= 220.0 && stddev_ms <= 28.0 {
        (Severity::Ok, "Competitive")
    } else if stddev_ms > 35.0 {
        (
            Severity::Warn,
            "High jitter (check debounce / display refresh)",
        )
    } else {
        (Severity::Info, "Measured")
    };

    ReactionTestSummary {
        trial_count: n,
        false_starts,
        best_ms,
        median_ms,
        mean_ms,
        p95_ms,
        stddev_ms,
        rating: rating.to_string(),
        severity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_reaction_median_best_p95_and_stddev() {
        let trials = [165.0, 172.0, 158.0, 180.0, 170.0, 168.0, 175.0, 162.0];
        let s = compute_reaction_summary(&trials, 1);
        assert_eq!(s.trial_count, 8);
        assert_eq!(s.false_starts, 1);
        assert!((s.best_ms - 158.0).abs() < 0.01);
        assert!((s.median_ms - 169.0).abs() < 0.01);
        assert!((s.p95_ms - 180.0).abs() < 0.01);
        assert!(s.stddev_ms < 10.0);
        assert_eq!(s.severity, Severity::Ok);
    }
}
