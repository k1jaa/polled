// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Versioned full-setup backup & restore (`polled.setup.v1`) bundling `settings.json`,
//! `profiles.json`, and `custom_tweaks.json` into a single portable JSON file.
//!
//! Hardware baseline snapshots (`baseline_snapshot.json`) and host-specific OS state
//! (`state.json`) are deliberately excluded so backups remain safe to move across PCs or
//! fresh Windows installs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::checks::custom_tweaks::CUSTOM_TWEAKS_SCHEMA_VERSION;
use crate::checks::CustomTweakStore;
use crate::error::{PolledError, Result};
use crate::paths::AppPaths;
use crate::profiles::{GameProfile, ProfileStore, PROFILES_SCHEMA_VERSION};
use crate::settings::{AppSettings, SETTINGS_SCHEMA_VERSION};

pub const SETUP_BACKUP_SCHEMA_VERSION: u32 = 1;
pub const SETUP_BACKUP_FORMAT: &str = "polled.setup.v1";

/// Portable full-setup JSON bundle containing user preferences, all game profiles, and custom tweaks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupBackupBundle {
    pub schema_version: u32,
    pub format: String,
    pub exported_at: DateTime<Utc>,
    pub polled_version: String,
    pub settings: AppSettings,
    pub profiles: ProfileStore,
    pub custom_tweaks: CustomTweakStore,
}

impl SetupBackupBundle {
    /// Sanitizes and validates all contained settings, profiles, and custom tweaks.
    pub fn validate_and_sanitize(mut self) -> Result<Self> {
        if self.schema_version != SETUP_BACKUP_SCHEMA_VERSION {
            return Err(PolledError::InvalidProfile(format!(
                "Unsupported setup backup schema_version {} (expected {SETUP_BACKUP_SCHEMA_VERSION})",
                self.schema_version
            )));
        }
        if !self.format.trim().is_empty() && self.format != SETUP_BACKUP_FORMAT {
            return Err(PolledError::InvalidProfile(format!(
                "Unsupported backup format '{}' (expected '{SETUP_BACKUP_FORMAT}')",
                self.format
            )));
        }
        self.format = SETUP_BACKUP_FORMAT.to_string();

        self.settings.schema_version = SETTINGS_SCHEMA_VERSION;
        self.settings.alerts.low_battery_threshold_pct = self.settings.alerts.clamped_threshold();

        if self.profiles.schema_version != PROFILES_SCHEMA_VERSION {
            return Err(PolledError::InvalidProfile(format!(
                "Unsupported profiles schema_version {} in backup",
                self.profiles.schema_version
            )));
        }
        self.profiles.debounce_ms = self.profiles.debounce_ms.clamp(500, 10_000);
        self.profiles.profiles = self
            .profiles
            .profiles
            .into_iter()
            .map(GameProfile::sanitized)
            .filter(|p| !p.id.is_empty() && !p.name.is_empty())
            .collect();
        if !self.profiles.profiles.iter().any(|p| p.is_desktop_default) {
            self.profiles
                .profiles
                .insert(0, GameProfile::desktop_default());
        }

        if self.custom_tweaks.schema_version != CUSTOM_TWEAKS_SCHEMA_VERSION {
            return Err(PolledError::InvalidProfile(format!(
                "Unsupported custom_tweaks schema_version {} in backup",
                self.custom_tweaks.schema_version
            )));
        }
        for tweak in &self.custom_tweaks.tweaks {
            tweak.validate().map_err(|msg| {
                PolledError::InvalidProfile(format!("Invalid custom tweak: {msg}"))
            })?;
        }

        Ok(self)
    }

    pub fn summary_counts(&self) -> String {
        let profiles = self.profiles.profiles.len();
        let tweaks = self.custom_tweaks.tweaks.len();
        format!(
            "{profiles} profile{}, {tweaks} custom tweak{}, settings & hotkeys",
            if profiles == 1 { "" } else { "s" },
            if tweaks == 1 { "" } else { "s" }
        )
    }
}

/// Captures `settings.json`, `profiles.json`, and `custom_tweaks.json` into a versioned bundle.
pub fn export_setup_bundle(paths: &AppPaths) -> Result<SetupBackupBundle> {
    let settings = AppSettings::load_or_default(&paths.settings_path());
    let profiles = ProfileStore::load_or_default(&paths.profiles_path())?;
    let custom_tweaks = CustomTweakStore::load_or_default(&paths.custom_tweaks_path());

    Ok(SetupBackupBundle {
        schema_version: SETUP_BACKUP_SCHEMA_VERSION,
        format: SETUP_BACKUP_FORMAT.to_string(),
        exported_at: Utc::now(),
        polled_version: env!("CARGO_PKG_VERSION").to_string(),
        settings,
        profiles,
        custom_tweaks,
    })
}

/// Serializes the full setup bundle as pretty-printed JSON.
pub fn export_setup_bundle_json(paths: &AppPaths) -> Result<String> {
    let bundle = export_setup_bundle(paths)?;
    Ok(serde_json::to_string_pretty(&bundle)?)
}

/// Parses, validates, and sanitizes a `polled.setup.v1` JSON string without writing to disk.
pub fn parse_setup_bundle_json(json: &str) -> Result<SetupBackupBundle> {
    let bundle: SetupBackupBundle = serde_json::from_str(json)?;
    bundle.validate_and_sanitize()
}

/// Atomically restores `settings.json`, `profiles.json`, and `custom_tweaks.json` from a validated bundle.
pub fn restore_setup_bundle(
    paths: &AppPaths,
    bundle: &SetupBackupBundle,
) -> Result<SetupBackupBundle> {
    let clean = bundle.clone().validate_and_sanitize()?;
    paths.ensure_dir()?;
    clean.settings.save_atomic(&paths.settings_path())?;
    clean.profiles.save_atomic(&paths.profiles_path())?;
    clean
        .custom_tweaks
        .save_atomic(&paths.custom_tweaks_path())?;
    Ok(clean)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::{CustomTweakDef, CustomValueKind};
    use crate::settings::ThemePreference;
    use crate::win::RegHive;
    use tempfile::TempDir;

    #[test]
    fn roundtrips_full_setup_bundle_across_directories() {
        let src_dir = TempDir::new().unwrap();
        let src = AppPaths::from_root(src_dir.path());
        src.ensure_dir().unwrap();

        let mut settings = AppSettings {
            theme: ThemePreference::Light,
            ..Default::default()
        };
        settings.alerts.low_battery_threshold_pct = 20;
        settings.hotkeys.enabled = true;
        settings.save_atomic(&src.settings_path()).unwrap();

        let mut profiles = ProfileStore {
            debounce_ms: 2000,
            ..Default::default()
        };
        let mut cs2 = GameProfile::desktop_default();
        cs2.id = "cs2".to_string();
        cs2.name = "CS2".to_string();
        cs2.is_desktop_default = false;
        cs2.executables = vec!["cs2.exe".to_string()];
        profiles.upsert_profile(cs2);
        profiles.save_atomic(&src.profiles_path()).unwrap();

        let mut tweaks = CustomTweakStore::default();
        tweaks
            .upsert(CustomTweakDef {
                id: "custom_hover_10".to_string(),
                name: "Fast Hover".to_string(),
                hive: RegHive::Hkcu,
                subkey: r"Control Panel\Mouse".to_string(),
                value_name: "MouseHoverTime".to_string(),
                target: CustomValueKind::String("10".to_string()),
                requires_reboot: false,
                notes: "Snappy hover".to_string(),
            })
            .unwrap();
        tweaks.save_atomic(&src.custom_tweaks_path()).unwrap();

        let json = export_setup_bundle_json(&src).unwrap();
        let parsed = parse_setup_bundle_json(&json).unwrap();

        let dst_dir = TempDir::new().unwrap();
        let dst = AppPaths::from_root(dst_dir.path());
        let restored = restore_setup_bundle(&dst, &parsed).unwrap();

        assert_eq!(restored.settings.theme, ThemePreference::Light);
        assert_eq!(restored.settings.alerts.low_battery_threshold_pct, 20);
        assert!(restored.settings.hotkeys.enabled);
        assert_eq!(restored.profiles.profiles.len(), 2);
        assert_eq!(restored.profiles.debounce_ms, 2000);
        assert_eq!(restored.custom_tweaks.tweaks.len(), 1);

        let reloaded_profiles = ProfileStore::load_or_default(&dst.profiles_path()).unwrap();
        assert_eq!(reloaded_profiles.profiles.len(), 2);
        let reloaded_tweaks = CustomTweakStore::load_or_default(&dst.custom_tweaks_path());
        assert_eq!(reloaded_tweaks.tweaks.len(), 1);
    }

    #[test]
    fn rejects_protected_security_custom_tweak_during_import() {
        let bad = SetupBackupBundle {
            schema_version: SETUP_BACKUP_SCHEMA_VERSION,
            format: SETUP_BACKUP_FORMAT.to_string(),
            exported_at: Utc::now(),
            polled_version: "0.11.0".to_string(),
            settings: AppSettings::default(),
            profiles: ProfileStore::default(),
            custom_tweaks: CustomTweakStore {
                schema_version: CUSTOM_TWEAKS_SCHEMA_VERSION,
                tweaks: vec![CustomTweakDef {
                    id: "custom_bad".to_string(),
                    name: "Blocked".to_string(),
                    hive: RegHive::Hklm,
                    subkey: r"SYSTEM\CurrentControlSet\Control\Lsa".to_string(),
                    value_name: "Test".to_string(),
                    target: CustomValueKind::Dword(0),
                    requires_reboot: false,
                    notes: String::new(),
                }],
            },
        };
        let json = serde_json::to_string(&bad).unwrap();
        assert!(parse_setup_bundle_json(&json).is_err());
    }
}
