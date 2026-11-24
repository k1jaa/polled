// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Persisted user UI preferences (`settings.json`) and non-elevated Windows login autostart
//! (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`).

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const AUTOSTART_REG_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const AUTOSTART_VALUE_NAME: &str = "Polled";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    Dark,
    Light,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub theme: ThemePreference,
    #[serde(default)]
    pub auto_switch_on_startup: bool,
}

fn default_schema_version() -> u32 {
    SETTINGS_SCHEMA_VERSION
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            theme: ThemePreference::Dark,
            auto_switch_on_startup: false,
        }
    }
}

impl AppSettings {
    /// Loads `settings.json`, falling back to defaults if the file does not exist or cannot be parsed.
    pub fn load_or_default(path: &Path) -> Self {
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    /// Atomically writes `settings.json` via a sibling temp file and rename.
    pub fn save_atomic(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| PolledError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let json = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|source| PolledError::Io {
            path: tmp.clone(),
            source,
        })?;
        if path.exists() {
            let _ = fs::remove_file(path);
        }
        fs::rename(&tmp, path).map_err(|source| PolledError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }
}

/// Formats the quoted `HKCU\...\Run` command line so paths with spaces launch cleanly minimized to tray.
pub fn format_autostart_command(exe_path: &Path) -> String {
    format!("\"{}\" --minimized", exe_path.display())
}

/// Checks whether Polled is registered in the current user's Windows startup `Run` key (no admin needed).
pub fn is_autostart_enabled(env: &dyn SystemEnv) -> Result<bool> {
    let val = env.reg_get_string(RegHive::Hkcu, AUTOSTART_REG_SUBKEY, AUTOSTART_VALUE_NAME)?;
    Ok(val.is_some_and(|s| !s.trim().is_empty()))
}

/// Registers or removes Polled from the current user's Windows startup `Run` key (`--minimized`).
pub fn set_autostart_enabled(env: &dyn SystemEnv, enabled: bool, exe_path: &Path) -> Result<()> {
    if enabled {
        let cmd = format_autostart_command(exe_path);
        env.reg_set_string(
            RegHive::Hkcu,
            AUTOSTART_REG_SUBKEY,
            AUTOSTART_VALUE_NAME,
            &cmd,
        )
    } else {
        env.reg_delete_value(RegHive::Hkcu, AUTOSTART_REG_SUBKEY, AUTOSTART_VALUE_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn settings_roundtrip_atomic_json() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");

        // Missing file loads defaults cleanly.
        assert_eq!(AppSettings::load_or_default(&path), AppSettings::default());

        let custom = AppSettings {
            schema_version: SETTINGS_SCHEMA_VERSION,
            theme: ThemePreference::Light,
            auto_switch_on_startup: true,
        };
        custom.save_atomic(&path).unwrap();
        assert_eq!(AppSettings::load_or_default(&path), custom);
    }

    #[test]
    fn autostart_writes_quoted_minimized_command_and_deletes_on_disable() {
        let env = MockEnv::new();
        let exe = PathBuf::from(r"C:\Program Files\Polled\polled-gui.exe");

        assert!(!is_autostart_enabled(&env).unwrap());

        set_autostart_enabled(&env, true, &exe).unwrap();
        assert!(is_autostart_enabled(&env).unwrap());
        let stored = env
            .reg_get_string(RegHive::Hkcu, AUTOSTART_REG_SUBKEY, AUTOSTART_VALUE_NAME)
            .unwrap()
            .unwrap();
        assert_eq!(
            stored,
            r#""C:\Program Files\Polled\polled-gui.exe" --minimized"#
        );

        set_autostart_enabled(&env, false, &exe).unwrap();
        assert!(!is_autostart_enabled(&env).unwrap());
    }
}
