// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod debouncer;
pub mod matcher;
pub mod watcher;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::checks::snapshot::write_json_atomic;
use crate::devices::{LiftOffDistance, MouseSettings};
use crate::error::{PolledError, Result};

pub use debouncer::FocusDebouncer;
pub use matcher::{match_profile, normalize_exe_name};
pub use watcher::{FocusEvent, ForegroundWatcher};

pub const PROFILES_SCHEMA_VERSION: u32 = 1;
pub const PROFILE_EXPORT_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_DEBOUNCE_MS: u64 = 1500;

/// Single peripheral profile targeting either specific game executables or the default desktop.
///
/// Kept flat, clean, and versioned so shareable "settings cards" can be built directly on top.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub is_desktop_default: bool,
    /// Lowercase `.exe` basenames (e.g. `["cs2.exe", "valorant-win64-shipping.exe"]`).
    #[serde(default)]
    pub executables: Vec<String>,
    pub mouse: MouseSettings,
    /// Optional per-mouse overrides keyed by `DeviceInfo.id` (e.g., `lamzu_compx_4k`,
    /// `pulsar_x2_crazylight_wireless`). When present for a connected mouse, these settings
    /// are used instead of the shared `mouse` defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub device_overrides: BTreeMap<String, MouseSettings>,
    /// Optional Windows pointer speed override (`1..=20`, where `10` = 6/11 1.0x).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub windows_pointer_speed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl GameProfile {
    pub fn desktop_default() -> Self {
        Self {
            id: "desktop".to_string(),
            name: "Desktop (Default)".to_string(),
            is_desktop_default: true,
            executables: Vec::new(),
            mouse: MouseSettings {
                dpi: Some(800),
                polling_rate_hz: Some(1000),
                lod: Some(LiftOffDistance::Mm10),
                debounce_ms: Some(4),
            },
            device_overrides: BTreeMap::new(),
            windows_pointer_speed: Some(10),
            author: None,
            notes: Some("Fallback profile used whenever no game profile matches.".to_string()),
        }
    }

    /// Resolves the mouse settings to apply for a specific `device_id`, returning
    /// `(settings, used_device_override)`.
    pub fn settings_for_device(&self, device_id: &str) -> (&MouseSettings, bool) {
        if let Some(overridden) = self.device_overrides.get(device_id) {
            (overridden, true)
        } else {
            (&self.mouse, false)
        }
    }

    pub fn sanitized(mut self) -> Self {
        self.id = self.id.trim().to_string();
        self.name = self.name.trim().to_string();
        self.executables = self
            .executables
            .into_iter()
            .filter_map(|e| {
                let norm = normalize_exe_name(&e);
                if norm.is_empty() {
                    None
                } else {
                    Some(norm)
                }
            })
            .collect();
        self.device_overrides = self
            .device_overrides
            .into_iter()
            .filter_map(|(k, v)| {
                let key = k.trim().to_string();
                if key.is_empty() {
                    None
                } else {
                    Some((key, v))
                }
            })
            .collect();
        self.windows_pointer_speed = self.windows_pointer_speed.map(|s| s.clamp(1, 20));
        self
    }
}

/// Versioned collection of all profiles stored at `%LOCALAPPDATA%\Polled\profiles.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileStore {
    pub schema_version: u32,
    /// Milliseconds a game must hold foreground focus before switching (prevents alt-tab spam).
    pub debounce_ms: u64,
    pub profiles: Vec<GameProfile>,
}

impl Default for ProfileStore {
    fn default() -> Self {
        Self {
            schema_version: PROFILES_SCHEMA_VERSION,
            debounce_ms: DEFAULT_DEBOUNCE_MS,
            profiles: vec![GameProfile::desktop_default()],
        }
    }
}

impl ProfileStore {
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path).map_err(|source| PolledError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut store: Self = serde_json::from_str(&raw)?;
        if store.schema_version != PROFILES_SCHEMA_VERSION {
            return Err(PolledError::InvalidProfile(format!(
                "Unsupported profiles schema_version {} (expected {PROFILES_SCHEMA_VERSION})",
                store.schema_version
            )));
        }
        store.debounce_ms = store.debounce_ms.clamp(500, 10_000);
        if !store.profiles.iter().any(|p| p.is_desktop_default) {
            store.profiles.insert(0, GameProfile::desktop_default());
        }
        Ok(store)
    }

    pub fn save_atomic(&self, path: &Path) -> Result<()> {
        write_json_atomic(path, self)
    }

    pub fn upsert_profile(&mut self, profile: GameProfile) {
        let profile = profile.sanitized();
        if let Some(existing) = self.profiles.iter_mut().find(|p| p.id == profile.id) {
            *existing = profile;
        } else {
            self.profiles.push(profile);
        }
    }

    pub fn remove_profile(&mut self, id: &str) -> bool {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.is_desktop_default || p.id != id);
        self.profiles.len() != before
    }
}

/// Versioned portable JSON format for exporting/importing a single profile (settings card ready).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileCardExport {
    pub schema_version: u32,
    pub format: String,
    pub exported_at: DateTime<Utc>,
    pub profile: GameProfile,
}

pub fn export_profile_json(profile: &GameProfile) -> Result<String> {
    let card = ProfileCardExport {
        schema_version: PROFILE_EXPORT_SCHEMA_VERSION,
        format: "polled.profile.v1".to_string(),
        exported_at: Utc::now(),
        profile: profile.clone().sanitized(),
    };
    Ok(serde_json::to_string_pretty(&card)?)
}

pub fn import_profile_json(json: &str) -> Result<GameProfile> {
    let card: ProfileCardExport = serde_json::from_str(json)?;
    if card.schema_version != PROFILE_EXPORT_SCHEMA_VERSION {
        return Err(PolledError::InvalidProfile(format!(
            "Unsupported exported profile schema_version {} (expected {PROFILE_EXPORT_SCHEMA_VERSION})",
            card.schema_version
        )));
    }
    let profile = card.profile.sanitized();
    if profile.id.is_empty() || profile.name.is_empty() {
        return Err(PolledError::InvalidProfile(
            "Imported profile must have non-empty id and name".to_string(),
        ));
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn store_roundtrips_json_and_preserves_desktop_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("profiles.json");

        let mut store = ProfileStore::default();
        store.upsert_profile(GameProfile {
            id: "cs2".to_string(),
            name: "Counter-Strike 2".to_string(),
            is_desktop_default: false,
            executables: vec![r"C:\Steam\cs2.exe".to_string()],
            mouse: MouseSettings {
                dpi: Some(800),
                polling_rate_hz: Some(4000),
                lod: Some(LiftOffDistance::Mm07),
                debounce_ms: Some(1),
            },
            device_overrides: BTreeMap::from([(
                "lamzu_compx_4k".to_string(),
                MouseSettings {
                    dpi: Some(800),
                    polling_rate_hz: Some(4000),
                    lod: Some(LiftOffDistance::Mm10),
                    debounce_ms: Some(2),
                },
            )]),
            windows_pointer_speed: Some(10),
            author: Some("k1jaa".to_string()),
            notes: Some("Tournament config".to_string()),
        });

        store.save_atomic(&path).unwrap();
        let loaded = ProfileStore::load_or_default(&path).unwrap();
        assert_eq!(loaded.profiles.len(), 2);
        assert_eq!(loaded.profiles[1].executables, vec!["cs2.exe"]);
        let (lamzu, is_override) = loaded.profiles[1].settings_for_device("lamzu_compx_4k");
        assert!(is_override);
        assert_eq!(lamzu.lod, Some(LiftOffDistance::Mm10));
        assert_eq!(lamzu.debounce_ms, Some(2));
        let (pulsar, is_override_pulsar) =
            loaded.profiles[1].settings_for_device("pulsar_x2_crazylight_wireless");
        assert!(!is_override_pulsar);
        assert_eq!(pulsar.lod, Some(LiftOffDistance::Mm07));
    }

    #[test]
    fn export_and_import_profile_card_roundtrips() {
        let profile = GameProfile {
            id: "valorant".to_string(),
            name: "VALORANT".to_string(),
            is_desktop_default: false,
            executables: vec!["VALORANT-Win64-Shipping.exe".to_string()],
            mouse: MouseSettings {
                dpi: Some(1600),
                polling_rate_hz: Some(4000),
                lod: Some(LiftOffDistance::Mm07),
                debounce_ms: Some(0),
            },
            device_overrides: BTreeMap::new(),
            windows_pointer_speed: None,
            author: Some("k1jaa".to_string()),
            notes: Some("Low LOD + 4K".to_string()),
        };

        let json = export_profile_json(&profile).unwrap();
        let imported = import_profile_json(&json).unwrap();
        assert_eq!(imported.id, "valorant");
        assert_eq!(imported.executables, vec!["valorant-win64-shipping.exe"]);
        assert_eq!(imported.mouse.polling_rate_hz, Some(4000));
    }
}
