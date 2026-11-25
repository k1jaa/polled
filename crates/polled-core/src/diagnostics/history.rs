// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Persistent benchmark run history (`%LOCALAPPDATA%\Polled\bench_history.json`)
//! so users can save named polling/latency snapshots and reload any historical run
//! directly into Bench's Before / After A/B comparison across restarts.

use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::checks::snapshot::write_json_atomic;
use crate::diagnostics::{
    DiagnosticsSnapshot, DpcMonitorSnapshot, PinnedBenchmark, ReactionTestSummary,
    TimerJitterBenchmark,
};
use crate::error::{PolledError, Result};

pub const BENCH_HISTORY_SCHEMA_VERSION: u32 = 1;
pub const MAX_BENCH_HISTORY_ENTRIES: usize = 30;
const MAX_SAVED_INTERVAL_SAMPLES: usize = 500;

/// One persisted Bench run snapshot that can be reloaded as Baseline (A).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedBenchRun {
    pub id: String,
    pub label: String,
    pub saved_at: DateTime<Utc>,
    pub configured_dpi: u32,
    pub snapshot: DiagnosticsSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer: Option<TimerJitterBenchmark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reaction: Option<ReactionTestSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpc_peak_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpc_core0_starved: Option<bool>,
}

impl SavedBenchRun {
    /// Creates a compact persisted run snapshot (trims verbose traces so `bench_history.json` stays small).
    pub fn from_live(
        label: String,
        configured_dpi: u32,
        snapshot: &DiagnosticsSnapshot,
        timer: Option<&TimerJitterBenchmark>,
        reaction: Option<&ReactionTestSummary>,
        dpc: Option<&DpcMonitorSnapshot>,
    ) -> Self {
        let now = Utc::now();
        let mut compact_snap = snapshot.clone();
        compact_snap.capturing = false;
        if compact_snap.polling.recent_samples.len() > MAX_SAVED_INTERVAL_SAMPLES {
            let drain = compact_snap.polling.recent_samples.len() - MAX_SAVED_INTERVAL_SAMPLES;
            compact_snap.polling.recent_samples.drain(0..drain);
        }
        compact_snap.tracking.xy_trace.clear();

        let id = format!(
            "run_{}_{}",
            now.timestamp_millis(),
            compact_snap.polling.total_motion_packets
        );
        let clean_label = if label.trim().is_empty() {
            if compact_snap.polling.total_motion_packets > 0 {
                format!(
                    "{:.0} Hz ({} Hz target) · {} DPI",
                    compact_snap.polling.effective_hz,
                    compact_snap.polling.nominal_hz,
                    configured_dpi
                )
            } else {
                format!("Bench Snapshot ({} DPI)", configured_dpi)
            }
        } else {
            label.trim().to_string()
        };

        let valid_dpc = dpc.filter(|d| d.core_count > 0);
        Self {
            id,
            label: clean_label,
            saved_at: now,
            configured_dpi,
            snapshot: compact_snap,
            timer: timer.cloned(),
            reaction: reaction.filter(|r| r.trial_count > 0).cloned(),
            dpc_peak_pct: valid_dpc.map(|d| d.peak_dpc_irq_pct),
            dpc_core0_starved: valid_dpc.map(|d| d.core0_starved),
        }
    }

    /// Converts this historical run into a `PinnedBenchmark` ready for Bench's Before/After A/B view.
    pub fn to_pinned_baseline(&self) -> PinnedBenchmark {
        PinnedBenchmark {
            label: self.label.clone(),
            configured_dpi: self.configured_dpi,
            snapshot: self.snapshot.clone(),
            timer: self.timer.clone(),
        }
    }
}

/// Versioned collection of saved Bench runs persisted at `%LOCALAPPDATA%\Polled\bench_history.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchHistoryStore {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub runs: Vec<SavedBenchRun>,
}

fn default_schema_version() -> u32 {
    BENCH_HISTORY_SCHEMA_VERSION
}

impl Default for BenchHistoryStore {
    fn default() -> Self {
        Self {
            schema_version: BENCH_HISTORY_SCHEMA_VERSION,
            runs: Vec::new(),
        }
    }
}

impl BenchHistoryStore {
    pub fn load_or_default(path: &Path) -> Self {
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    pub fn save_atomic(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| PolledError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        write_json_atomic(path, self)
    }

    pub fn push_run(&mut self, run: SavedBenchRun) {
        self.runs.retain(|r| r.id != run.id);
        self.runs.insert(0, run);
        self.runs.truncate(MAX_BENCH_HISTORY_ENTRIES);
    }

    pub fn delete_run(&mut self, id: &str) -> bool {
        let before = self.runs.len();
        self.runs.retain(|r| r.id != id);
        self.runs.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn bench_history_roundtrips_caps_and_converts_to_baseline() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bench_history.json");

        assert_eq!(
            BenchHistoryStore::load_or_default(&path),
            BenchHistoryStore::default()
        );

        let mut store = BenchHistoryStore::default();
        let mut snap = DiagnosticsSnapshot::default();
        snap.polling.nominal_hz = 4000;
        snap.polling.effective_hz = 3992.0;
        snap.polling.total_motion_packets = 12_000;

        let run = SavedBenchRun::from_live(
            "Rear USB3 · 4K · After MSI Fix".to_string(),
            1600,
            &snap,
            None,
            None,
            None,
        );
        let run_id = run.id.clone();
        store.push_run(run);
        store.save_atomic(&path).unwrap();

        let loaded = BenchHistoryStore::load_or_default(&path);
        assert_eq!(loaded.runs.len(), 1);
        assert_eq!(loaded.runs[0].label, "Rear USB3 · 4K · After MSI Fix");
        let pinned = loaded.runs[0].to_pinned_baseline();
        assert_eq!(pinned.configured_dpi, 1600);
        assert_eq!(pinned.snapshot.polling.nominal_hz, 4000);

        let mut store2 = loaded;
        assert!(store2.delete_run(&run_id));
        assert!(store2.runs.is_empty());
    }
}
