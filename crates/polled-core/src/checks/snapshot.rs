// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{PolledError, Result};
use crate::win::{AccessibilityKind, KeyboardRepeatParams, MouseAccelParams, RegHive};

pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Typed representation of a check's original pre-fix Windows value.
///
/// Using a tagged Rust `enum` makes snapshots self-describing and human-inspectable in JSON
/// while guaranteeing type safety on restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SavedValue {
    MouseAccel(MouseAccelParams),
    MouseSpeed(u32),
    AccelCurves {
        /// Base64-encoded original `SmoothMouseXCurve` bytes, or `None` if the value did not exist.
        x_curve_b64: Option<String>,
        /// Base64-encoded original `SmoothMouseYCurve` bytes, or `None` if the value did not exist.
        y_curve_b64: Option<String>,
    },
    PowerSetting {
        scheme_guid: String,
        ac: Option<u32>,
        dc: Option<u32>,
    },
    PowerSchemeGuid(String),
    GameMode {
        auto_game_mode_enabled: Option<u32>,
        allow_auto_game_mode: Option<u32>,
    },
    GameDvr {
        app_capture_enabled: Option<u32>,
        game_dvr_enabled: Option<u32>,
    },
    UserGpuPreferences(Option<String>),
    CursorSanity {
        mouse_trails: Option<String>,
        snap_to_default: Option<String>,
    },
    MmcssGames {
        system_responsiveness: Option<u32>,
        network_throttling_index: Option<u32>,
        gpu_priority: Option<u32>,
        priority: Option<u32>,
        scheduling_category: Option<String>,
        sfio_priority: Option<String>,
    },
    RegistryDword {
        hive: RegHive,
        subkey: String,
        name: String,
        value: Option<u32>,
    },
    RegistryString {
        hive: RegHive,
        subkey: String,
        name: String,
        value: Option<String>,
    },
    AccessibilityFlags {
        kind: AccessibilityKind,
        flags: u32,
    },
    KeyboardRepeat(KeyboardRepeatParams),
    NetworkNagle {
        interface_guid: String,
        tcp_ack_frequency: Option<u32>,
        tcp_no_delay: Option<u32>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub check_id: String,
    pub captured_at: DateTime<Utc>,
    pub original: SavedValue,
}

/// Versioned baseline snapshot stored on disk before Polled applies any fix.
///
/// Uses **first-write-wins** semantics per `check_id`: fixing check A today and check B
/// tomorrow preserves both true original values so "Undo all" restores the exact pre-Polled state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineSnapshot {
    pub schema_version: u32,
    pub created_at: DateTime<Utc>,
    pub windows_version: String,
    pub entries: BTreeMap<String, SnapshotEntry>,
}

impl BaselineSnapshot {
    pub fn new(windows_version: impl Into<String>) -> Self {
        Self {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            created_at: Utc::now(),
            windows_version: windows_version.into(),
            entries: BTreeMap::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(path).map_err(|source| PolledError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let snapshot: Self = serde_json::from_str(&raw)?;
        Ok(Some(snapshot))
    }

    /// Records the original value only if `check_id` does not already have a saved baseline.
    /// Returns `true` if a new entry was inserted.
    pub fn record_if_absent(&mut self, check_id: &str, original: SavedValue) -> bool {
        if self.entries.contains_key(check_id) {
            return false;
        }
        self.entries.insert(
            check_id.to_string(),
            SnapshotEntry {
                check_id: check_id.to_string(),
                captured_at: Utc::now(),
                original,
            },
        );
        true
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Atomically writes and syncs the snapshot JSON via a sibling `.tmp` file.
    pub fn save_atomic(&self, path: &Path) -> Result<()> {
        write_json_atomic(path, self)
    }
}

/// Writes pretty-printed JSON to a sibling temporary file, fsyncs it, and atomically replaces `path`.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| PolledError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let tmp_path = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(value)?;

    {
        let mut file = File::create(&tmp_path).map_err(|source| PolledError::Io {
            path: tmp_path.clone(),
            source,
        })?;
        file.write_all(json.as_bytes())
            .map_err(|source| PolledError::Io {
                path: tmp_path.clone(),
                source,
            })?;
        file.sync_all().map_err(|source| PolledError::Io {
            path: tmp_path.clone(),
            source,
        })?;
    }

    fs::rename(&tmp_path, path).map_err(|source| PolledError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn first_write_wins_and_roundtrips_json() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("baseline_snapshot.json");

        let mut snap = BaselineSnapshot::new("26200.1000");
        assert!(snap.record_if_absent("pointer_speed", SavedValue::MouseSpeed(14)));
        // Second capture for the same check must keep the true pre-Polled baseline (14).
        assert!(!snap.record_if_absent("pointer_speed", SavedValue::MouseSpeed(10)));

        snap.save_atomic(&path).unwrap();

        let loaded = BaselineSnapshot::load(&path).unwrap().unwrap();
        assert_eq!(loaded.schema_version, SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(
            loaded.entries["pointer_speed"].original,
            SavedValue::MouseSpeed(14)
        );
    }
}
