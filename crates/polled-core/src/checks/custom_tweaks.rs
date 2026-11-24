// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

//! User-defined reversible Windows registry tweaks persisted in `%LOCALAPPDATA%\Polled\custom_tweaks.json`.
//!
//! Every custom tweak captures its exact pre-existing registry value (or `None` if absent)
//! into `baseline_snapshot.json` before modifying Windows so it can be reverted cleanly at any time.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::checks::snapshot::write_json_atomic;
use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

pub const CUSTOM_TWEAKS_SCHEMA_VERSION: u32 = 1;

/// Protected Windows security/boot subkey prefixes that Polled refuses to target via custom tweaks.
const BLOCKED_SUBKEY_PREFIXES: &[&str] = &[
    r"sam",
    r"security",
    r"bcd",
    r"software\microsoft\windows nt\currentversion\winlogon",
    r"software\microsoft\windows defender",
    r"software\policies\microsoft\windows defender",
    r"system\currentcontrolset\control\deviceguard",
    r"system\currentcontrolset\control\lsa",
    r"system\currentcontrolset\control\safeboot",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum CustomValueKind {
    Dword(u32),
    String(String),
}

impl CustomValueKind {
    pub fn display_value(&self) -> String {
        match self {
            CustomValueKind::Dword(v) => format!("{v} (0x{v:X})"),
            CustomValueKind::String(s) => format!("\"{s}\""),
        }
    }
}

/// Definition of a user-defined reversible registry tweak.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomTweakDef {
    /// Safe ASCII identifier prefixed with `custom_` (used in snapshots and elevated CLI `--fix`/`--undo`).
    pub id: String,
    pub name: String,
    pub hive: RegHive,
    pub subkey: String,
    pub value_name: String,
    pub target: CustomValueKind,
    #[serde(default)]
    pub requires_reboot: bool,
    #[serde(default)]
    pub notes: String,
}

impl CustomTweakDef {
    /// Generates a safe `custom_<slug>` ID from a display name.
    pub fn make_id(name: &str) -> String {
        let mut slug = String::from("custom_");
        let mut last_underscore = true;
        for ch in name.chars() {
            if ch.is_ascii_alphanumeric() {
                slug.push(ch.to_ascii_lowercase());
                last_underscore = false;
            } else if !last_underscore {
                slug.push('_');
                last_underscore = true;
            }
            if slug.len() >= 48 {
                break;
            }
        }
        while slug.ends_with('_') {
            slug.pop();
        }
        if slug == "custom" || slug.len() <= 7 {
            slug = "custom_tweak".to_string();
        }
        slug
    }

    /// Validates identifier syntax, non-empty fields, and blocks security/boot-critical paths.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !self.id.starts_with("custom_")
            || self.id.len() <= 7
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(format!(
                "Invalid tweak ID '{}': must start with 'custom_' and use lowercase letters, digits, or '_'.",
                self.id
            ));
        }
        if self.name.trim().is_empty() {
            return Err("Tweak name cannot be empty.".to_string());
        }
        let subkey_trimmed = self.subkey.trim().trim_matches('\\');
        if subkey_trimmed.is_empty() {
            return Err("Registry subkey path cannot be empty.".to_string());
        }
        if self.value_name.trim().is_empty() {
            return Err("Registry value name cannot be empty.".to_string());
        }
        let lower = subkey_trimmed.to_ascii_lowercase();
        for blocked in BLOCKED_SUBKEY_PREFIXES {
            if lower == *blocked || lower.starts_with(&format!(r"{blocked}\")) {
                return Err(format!(
                    "Blocked protected Windows security/boot registry path: '{subkey_trimmed}'."
                ));
            }
        }
        Ok(())
    }

    pub fn hive_label(&self) -> &'static str {
        match self.hive {
            RegHive::Hkcu => "HKCU",
            RegHive::Hklm => "HKLM",
        }
    }

    pub fn full_reg_path(&self) -> String {
        format!(
            r"{}\{}\{}",
            self.hive_label(),
            self.subkey.trim().trim_matches('\\'),
            self.value_name.trim()
        )
    }
}

/// Curated optional low-latency presets users can one-click import into Custom Tweaks.
pub fn curated_custom_tweak_presets() -> Vec<CustomTweakDef> {
    vec![
        CustomTweakDef {
            id: "custom_priority_sep_short_fixed".to_string(),
            name: "Win32PrioritySeparation: Short Fixed (0x2A)".to_string(),
            hive: RegHive::Hklm,
            subkey: r"SYSTEM\CurrentControlSet\Control\PriorityControl".to_string(),
            value_name: "Win32PrioritySeparation".to_string(),
            target: CustomValueKind::Dword(0x2A),
            requires_reboot: false,
            notes: "Short, Fixed CPU scheduler quantum (42 / 0x2A) with high foreground boost—popular alternative to 0x26 for consistent FPS frame pacing."
                .to_string(),
        },
        CustomTweakDef {
            id: "custom_disable_power_throttling".to_string(),
            name: "Disable Windows Power Throttling".to_string(),
            hive: RegHive::Hklm,
            subkey: r"SYSTEM\CurrentControlSet\Control\Power\PowerThrottling".to_string(),
            value_name: "PowerThrottlingOff".to_string(),
            target: CustomValueKind::Dword(1),
            requires_reboot: true,
            notes: "Prevents Windows from placing background/foreground helper threads into EcoQoS efficiency-core throttling on hybrid Intel/AMD CPUs."
                .to_string(),
        },
        CustomTweakDef {
            id: "custom_mouse_queue_64".to_string(),
            name: "MouseDataQueueSize = 64 (Tighter 4K/8K-Safe Buffer)".to_string(),
            hive: RegHive::Hklm,
            subkey: r"SYSTEM\CurrentControlSet\Services\mouclass\Parameters".to_string(),
            value_name: "MouseDataQueueSize".to_string(),
            target: CustomValueKind::Dword(64),
            requires_reboot: true,
            notes: "Reduces the mouclass ring buffer slightly from default 100 while staying safely above 32 so 4K/8K mice don't drop packets during micro-hitches."
                .to_string(),
        },
        CustomTweakDef {
            id: "custom_mouse_hover_10ms".to_string(),
            name: "Fast Desktop Mouse Hover Time (10 ms)".to_string(),
            hive: RegHive::Hkcu,
            subkey: r"Control Panel\Mouse".to_string(),
            value_name: "MouseHoverTime".to_string(),
            target: CustomValueKind::String("10".to_string()),
            requires_reboot: false,
            notes: "Reduces desktop tooltip hover delay from 400 ms to 10 ms (HKCU, no admin required)."
                .to_string(),
        },
    ]
}

/// Persistent collection of user-defined custom tweaks (`custom_tweaks.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomTweakStore {
    pub schema_version: u32,
    pub tweaks: Vec<CustomTweakDef>,
}

impl Default for CustomTweakStore {
    fn default() -> Self {
        Self {
            schema_version: CUSTOM_TWEAKS_SCHEMA_VERSION,
            tweaks: Vec::new(),
        }
    }
}

impl CustomTweakStore {
    pub fn load_or_default(path: &Path) -> Self {
        if !path.exists() {
            return Self::default();
        }
        fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save_atomic(&self, path: &Path) -> Result<()> {
        write_json_atomic(path, self)
    }

    pub fn upsert(&mut self, mut def: CustomTweakDef) -> std::result::Result<String, String> {
        def.subkey = def.subkey.trim().trim_matches('\\').to_string();
        def.value_name = def.value_name.trim().to_string();
        def.name = def.name.trim().to_string();
        def.notes = def.notes.trim().to_string();
        def.validate()?;
        let id = def.id.clone();
        if let Some(slot) = self.tweaks.iter_mut().find(|t| t.id == def.id) {
            *slot = def;
        } else {
            self.tweaks.push(def);
        }
        Ok(id)
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.tweaks.len();
        self.tweaks.retain(|t| t.id != id);
        self.tweaks.len() != before
    }

    pub fn into_checks(self) -> Vec<Box<dyn HealthCheck>> {
        self.tweaks
            .into_iter()
            .filter(|t| t.validate().is_ok())
            .map(|t| Box::new(CustomTweakCheck::new(t)) as Box<dyn HealthCheck>)
            .collect()
    }
}

/// Runtime `HealthCheck` adapter for a user-defined reversible registry tweak.
pub struct CustomTweakCheck {
    def: CustomTweakDef,
    elevation_msg: Option<String>,
}

impl CustomTweakCheck {
    pub fn new(def: CustomTweakDef) -> Self {
        let elevation_msg = match def.hive {
            RegHive::Hklm => Some(format!(
                "Applying custom tweak '{}' writes to {}.",
                def.name,
                def.full_reg_path()
            )),
            RegHive::Hkcu => None,
        };
        Self { def, elevation_msg }
    }
}

impl HealthCheck for CustomTweakCheck {
    fn id(&self) -> &str {
        &self.def.id
    }

    fn name(&self) -> &str {
        &self.def.name
    }

    fn is_optional_tweak(&self) -> bool {
        true
    }

    fn requires_elevation(&self) -> bool {
        self.def.hive == RegHive::Hklm
    }

    fn elevation_reason(&self) -> Option<&str> {
        self.elevation_msg.as_deref()
    }

    fn requires_reboot(&self) -> bool {
        self.def.requires_reboot
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let (matches, current_value) = match &self.def.target {
            CustomValueKind::Dword(want) => {
                let cur =
                    env.reg_get_dword(self.def.hive, &self.def.subkey, &self.def.value_name)?;
                match cur {
                    Some(v) => (v == *want, format!("{v} (0x{v:X})")),
                    None => (false, "Not set (Windows default)".to_string()),
                }
            }
            CustomValueKind::String(want) => {
                let cur =
                    env.reg_get_string(self.def.hive, &self.def.subkey, &self.def.value_name)?;
                match cur {
                    Some(s) => (s == *want, format!("\"{s}\"")),
                    None => (false, "Not set (Windows default)".to_string()),
                }
            }
        };

        let reason = if self.def.notes.is_empty() {
            format!(
                "Custom registry tweak targeting {}.",
                self.def.full_reg_path()
            )
        } else {
            format!("{} ({})", self.def.notes, self.def.full_reg_path())
        };

        Ok(CheckReport {
            id: self.def.id.clone(),
            name: self.def.name.clone(),
            current_value,
            recommended_value: self.def.target.display_value(),
            reason,
            severity: if matches {
                Severity::Ok
            } else {
                Severity::Info
            },
            can_fix: true,
            is_optional_tweak: true,
            requires_elevation: self.requires_elevation(),
            elevation_reason: self.elevation_msg.clone(),
            requires_relogin: false,
            requires_reboot: self.def.requires_reboot,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        match &self.def.target {
            CustomValueKind::Dword(_) => {
                let value =
                    env.reg_get_dword(self.def.hive, &self.def.subkey, &self.def.value_name)?;
                Ok(SavedValue::RegistryDword {
                    hive: self.def.hive,
                    subkey: self.def.subkey.clone(),
                    name: self.def.value_name.clone(),
                    value,
                })
            }
            CustomValueKind::String(_) => {
                let value =
                    env.reg_get_string(self.def.hive, &self.def.subkey, &self.def.value_name)?;
                Ok(SavedValue::RegistryString {
                    hive: self.def.hive,
                    subkey: self.def.subkey.clone(),
                    name: self.def.value_name.clone(),
                    value,
                })
            }
        }
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        match &self.def.target {
            CustomValueKind::Dword(v) => {
                env.reg_set_dword(self.def.hive, &self.def.subkey, &self.def.value_name, *v)
            }
            CustomValueKind::String(s) => {
                env.reg_set_string(self.def.hive, &self.def.subkey, &self.def.value_name, s)
            }
        }
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        match saved {
            SavedValue::RegistryDword {
                hive,
                subkey,
                name,
                value,
            } => match value {
                Some(v) => env.reg_set_dword(*hive, subkey, name, *v),
                None => env.reg_delete_value(*hive, subkey, name),
            },
            SavedValue::RegistryString {
                hive,
                subkey,
                name,
                value,
            } => match value {
                Some(s) => env.reg_set_string(*hive, subkey, name, s),
                None => env.reg_delete_value(*hive, subkey, name),
            },
            _ => Err(PolledError::InvalidSnapshot {
                check_id: self.def.id.clone(),
                detail: "Expected SavedValue::RegistryDword or RegistryString".to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;
    use tempfile::TempDir;

    #[test]
    fn blocks_security_critical_registry_paths() {
        let bad = CustomTweakDef {
            id: "custom_bad_defender".to_string(),
            name: "Bad".to_string(),
            hive: RegHive::Hklm,
            subkey: r"SOFTWARE\Microsoft\Windows Defender\Real-Time Protection".to_string(),
            value_name: "DisableRealtimeMonitoring".to_string(),
            target: CustomValueKind::Dword(1),
            requires_reboot: false,
            notes: String::new(),
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn custom_tweak_applies_and_reverts_cleanly_including_absent_keys() {
        let env = MockEnv::new();
        let def = CustomTweakDef {
            id: "custom_power_throttle".to_string(),
            name: "Disable Power Throttling".to_string(),
            hive: RegHive::Hklm,
            subkey: r"SYSTEM\CurrentControlSet\Control\Power\PowerThrottling".to_string(),
            value_name: "PowerThrottlingOff".to_string(),
            target: CustomValueKind::Dword(1),
            requires_reboot: true,
            notes: "Test".to_string(),
        };
        let check = CustomTweakCheck::new(def.clone());
        assert!(check.is_optional_tweak());
        assert!(check.requires_elevation());
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(RegHive::Hklm, &def.subkey, &def.value_name)
                .unwrap(),
            None
        );
    }

    #[test]
    fn store_roundtrips_json_and_all_presets_validate() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("custom_tweaks.json");
        let mut store = CustomTweakStore::default();
        for preset in curated_custom_tweak_presets() {
            store.upsert(preset).unwrap();
        }
        store.save_atomic(&path).unwrap();
        let loaded = CustomTweakStore::load_or_default(&path);
        assert_eq!(loaded.tweaks.len(), 4);
    }
}
