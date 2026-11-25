// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod debouncer;
pub mod game_discovery;
pub mod matcher;
pub mod watcher;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::checks::power_plan::{GUID_BALANCED, GUID_HIGH_PERFORMANCE, GUID_ULTIMATE_PERFORMANCE};
use crate::checks::snapshot::write_json_atomic;
use crate::devices::{LiftOffDistance, MouseSettings};
use crate::error::{PolledError, Result};
use crate::win::PowerSchemeInfo;

pub use debouncer::FocusDebouncer;
pub use game_discovery::{
    discover_installed_games, discover_running_apps, DiscoveredGame, GameSource,
};
pub use matcher::{match_profile, normalize_exe_name};
pub use watcher::{FocusEvent, ForegroundWatcher};

pub const PROFILES_SCHEMA_VERSION: u32 = 1;
pub const PROFILE_EXPORT_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_DEBOUNCE_MS: u64 = 1500;

/// Optional Windows power plan preset tied to a game or desktop profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerPlanPreset {
    Balanced,
    HighPerformance,
    UltimatePerformance,
}

impl PowerPlanPreset {
    pub const ALL: [PowerPlanPreset; 3] = [
        PowerPlanPreset::Balanced,
        PowerPlanPreset::HighPerformance,
        PowerPlanPreset::UltimatePerformance,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PowerPlanPreset::Balanced => "Balanced",
            PowerPlanPreset::HighPerformance => "High Performance",
            PowerPlanPreset::UltimatePerformance => "Ultimate Performance",
        }
    }

    /// Resolves this preset to an installed Windows power scheme on the current host.
    /// Returns `None` gracefully if the scheme does not exist (e.g., Modern Standby laptops
    /// that only expose Balanced).
    pub fn resolve_scheme(self, schemes: &[PowerSchemeInfo]) -> Option<&PowerSchemeInfo> {
        let (target_guid, name_match) = match self {
            PowerPlanPreset::Balanced => (GUID_BALANCED, "balanced"),
            PowerPlanPreset::HighPerformance => (GUID_HIGH_PERFORMANCE, "high performance"),
            PowerPlanPreset::UltimatePerformance => {
                (GUID_ULTIMATE_PERFORMANCE, "ultimate performance")
            }
        };
        schemes
            .iter()
            .find(|s| s.guid.eq_ignore_ascii_case(target_guid))
            .or_else(|| {
                schemes
                    .iter()
                    .find(|s| s.name.to_ascii_lowercase().contains(name_match))
            })
    }
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_pointer_speed: Option<u32>,
    /// Optional Windows power plan preset switched automatically when this profile activates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_power_plan: Option<PowerPlanPreset>,
    /// Optional default Windows audio playback (output) endpoint ID switched on activation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_output_device_id: Option<String>,
    /// Friendly name of the selected playback device (shown even if currently disconnected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_output_device_name: Option<String>,
    /// Optional playback endpoint volume (`0..=100%`) applied on activation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_output_volume: Option<u8>,
    /// Optional default Windows microphone (capture) endpoint ID switched on activation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_input_device_id: Option<String>,
    /// Friendly name of the selected microphone (shown even if currently disconnected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_input_device_name: Option<String>,
    /// Optional microphone capture volume (`0..=100%`) applied on activation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_input_volume: Option<u8>,
    /// Periodically enforces `audio_input_volume` every 2s so games/Steam/Discord cannot hijack mic gain.
    #[serde(default)]
    pub lock_mic_volume: bool,
    /// Optional target display device name (e.g. `\\.\DISPLAY1`). `None` targets the primary monitor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_device_name: Option<String>,
    /// Optional temporary display width override (e.g. `1280` for 4:3 stretched). Restored automatically on tab-out/exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_width: Option<u32>,
    /// Optional temporary display height override (e.g. `960`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_height: Option<u32>,
    /// Optional temporary display refresh rate override in Hz (e.g. `240`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_refresh_hz: Option<u32>,
    /// Optional Wooting keyboard onboard profile slot (`0..=3`, displayed as Profile 1–4; zero flash wear).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wooting_profile: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
                ..Default::default()
            },
            device_overrides: BTreeMap::new(),
            windows_pointer_speed: Some(10),
            windows_power_plan: None,
            audio_output_device_id: None,
            audio_output_device_name: None,
            audio_output_volume: None,
            audio_input_device_id: None,
            audio_input_device_name: None,
            audio_input_volume: None,
            lock_mic_volume: false,
            display_device_name: None,
            display_width: None,
            display_height: None,
            display_refresh_hz: None,
            wooting_profile: None,
            author: None,
            notes: Some("Fallback profile used whenever no game profile matches.".to_string()),
        }
    }

    /// Returns `Some((optional_device_name, width, height, refresh_hz))` when this profile configures
    /// a valid temporary display resolution & refresh-rate override.
    pub fn display_mode_override(&self) -> Option<(Option<&str>, u32, u32, u32)> {
        let w = self.display_width?;
        let h = self.display_height?;
        let hz = self.display_refresh_hz?;
        if w < 800 || h < 600 || hz < 30 {
            return None;
        }
        Some((self.display_device_name.as_deref(), w, h, hz))
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
        self.audio_output_device_id = self
            .audio_output_device_id
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.audio_output_device_name = self
            .audio_output_device_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.audio_output_volume = self.audio_output_volume.map(|v| v.min(100));
        self.audio_input_device_id = self
            .audio_input_device_id
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.audio_input_device_name = self
            .audio_input_device_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.audio_input_volume = self.audio_input_volume.map(|v| v.min(100));
        self.display_device_name = self
            .display_device_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let valid_display = matches!(
            (self.display_width, self.display_height, self.display_refresh_hz),
            (Some(w), Some(h), Some(hz)) if w >= 800 && h >= 600 && hz >= 30
        );
        if !valid_display {
            self.display_device_name = None;
            self.display_width = None;
            self.display_height = None;
            self.display_refresh_hz = None;
        }
        self.wooting_profile = self.wooting_profile.map(|idx| idx.min(3));
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
                dpi_y: Some(1000),
                polling_rate_hz: Some(4000),
                lod: Some(LiftOffDistance::Mm07),
                debounce_ms: Some(1),
                motion_sync: Some(false),
                angle_snapping: Some(false),
                ripple_control: Some(false),
                sleep_timeout_secs: Some(300),
                dpi_stage: Some(2),
                lock_dpi_stages: Some(true),
                glass_mode: None,
            },
            device_overrides: BTreeMap::from([(
                "lamzu_compx_4k".to_string(),
                MouseSettings {
                    dpi: Some(800),
                    polling_rate_hz: Some(4000),
                    lod: Some(LiftOffDistance::Mm10),
                    debounce_ms: Some(2),
                    ..Default::default()
                },
            )]),
            windows_pointer_speed: Some(10),
            windows_power_plan: Some(PowerPlanPreset::HighPerformance),
            audio_output_device_id: Some("{0.0.0.00000000}.{1111}".to_string()),
            audio_output_device_name: Some("Headphones (USB DAC)".to_string()),
            audio_output_volume: Some(72),
            audio_input_device_id: Some("{0.0.1.00000000}.{2222}".to_string()),
            audio_input_device_name: Some("Shure MV7".to_string()),
            audio_input_volume: Some(85),
            lock_mic_volume: true,
            display_device_name: Some(r"\\.\DISPLAY1".to_string()),
            display_width: Some(1280),
            display_height: Some(960),
            display_refresh_hz: Some(240),
            wooting_profile: Some(1),
            author: Some("k1jaa".to_string()),
            notes: Some("Tournament config".to_string()),
        });

        store.save_atomic(&path).unwrap();
        let loaded = ProfileStore::load_or_default(&path).unwrap();
        assert_eq!(loaded.profiles.len(), 2);
        assert_eq!(loaded.profiles[1].executables, vec!["cs2.exe"]);
        assert_eq!(
            loaded.profiles[1].display_mode_override(),
            Some((Some(r"\\.\DISPLAY1"), 1280, 960, 240))
        );
        assert_eq!(loaded.profiles[1].mouse.dpi_stage, Some(2));
        assert_eq!(loaded.profiles[1].mouse.lock_dpi_stages, Some(true));
        assert_eq!(loaded.profiles[1].wooting_profile, Some(1));
        assert_eq!(
            loaded.profiles[1].windows_power_plan,
            Some(PowerPlanPreset::HighPerformance)
        );
        assert_eq!(loaded.profiles[1].audio_output_volume, Some(72));
        assert_eq!(loaded.profiles[1].audio_input_volume, Some(85));
        assert!(loaded.profiles[1].lock_mic_volume);
        let (lamzu, is_override) = loaded.profiles[1].settings_for_device("lamzu_compx_4k");
        assert!(is_override);
        assert_eq!(lamzu.lod, Some(LiftOffDistance::Mm10));
        assert_eq!(lamzu.debounce_ms, Some(2));
        let (pulsar, is_override_pulsar) =
            loaded.profiles[1].settings_for_device("pulsar_x2_crazylight_wireless");
        assert!(!is_override_pulsar);
        assert_eq!(pulsar.lod, Some(LiftOffDistance::Mm07));
        assert_eq!(pulsar.motion_sync, Some(false));
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
                ..Default::default()
            },
            device_overrides: BTreeMap::new(),
            windows_pointer_speed: None,
            windows_power_plan: None,
            audio_output_device_id: None,
            audio_output_device_name: None,
            audio_output_volume: None,
            audio_input_device_id: None,
            audio_input_device_name: None,
            audio_input_volume: None,
            lock_mic_volume: false,
            display_device_name: None,
            display_width: None,
            display_height: None,
            display_refresh_hz: None,
            wooting_profile: None,
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
