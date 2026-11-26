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

/// Global Windows hotkey combination (`RegisterHotKey`, zero keyboard hooks).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyBinding {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    /// Key name: `"A"`..=`"Z"`, `"0"`..=`"9"`, or `"F1"`..=`"F12"`.
    pub key: String,
}

impl HotkeyBinding {
    pub const PRESET_KEYS: &'static [&'static str] = &[
        "A", "P", "L", "I", "K", "M", "1", "2", "3", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
    ];

    pub fn new(ctrl: bool, alt: bool, shift: bool, key: &str) -> Self {
        Self {
            ctrl,
            alt,
            shift,
            key: key.to_ascii_uppercase(),
        }
    }

    pub fn display_label(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        parts.push(self.key.as_str());
        parts.join("+")
    }

    /// Maps `key` to its Win32 virtual-key code (`VK_*`).
    pub fn win32_vk(&self) -> Option<u32> {
        let k = self.key.trim().to_ascii_uppercase();
        if k.len() == 1 {
            let b = k.as_bytes()[0];
            if b.is_ascii_uppercase() || b.is_ascii_digit() {
                return Some(u32::from(b));
            }
        }
        if let Some(num) = k.strip_prefix('F').and_then(|s| s.parse::<u32>().ok()) {
            if (1..=12).contains(&num) {
                return Some(0x70 + (num - 1));
            }
        }
        None
    }

    /// Win32 `MOD_*` bitmask (`MOD_ALT=1`, `MOD_CONTROL=2`, `MOD_SHIFT=4`, `MOD_NOREPEAT=0x4000`).
    pub fn win32_modifiers(&self) -> u32 {
        let mut m = 0x4000u32; // MOD_NOREPEAT
        if self.alt {
            m |= 0x0001;
        }
        if self.ctrl {
            m |= 0x0002;
        }
        if self.shift {
            m |= 0x0004;
        }
        m
    }
}

/// Optional global desktop hotkeys (disabled by default so Polled never intercepts keys unexpectedly).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_hotkey_cycle")]
    pub cycle_profile: Option<HotkeyBinding>,
    #[serde(default = "default_hotkey_lock")]
    pub toggle_lock: Option<HotkeyBinding>,
    #[serde(default = "default_hotkey_lab")]
    pub toggle_input_lab: Option<HotkeyBinding>,
    #[serde(default = "default_hotkey_audio")]
    pub cycle_audio_output: Option<HotkeyBinding>,
    #[serde(default = "default_hotkey_wooting")]
    pub cycle_wooting_profile: Option<HotkeyBinding>,
    #[serde(default = "default_hotkey_mic_mute")]
    pub toggle_mic_mute: Option<HotkeyBinding>,
}

fn default_hotkey_cycle() -> Option<HotkeyBinding> {
    Some(HotkeyBinding::new(true, true, false, "P"))
}

fn default_hotkey_lock() -> Option<HotkeyBinding> {
    Some(HotkeyBinding::new(true, true, false, "L"))
}

fn default_hotkey_lab() -> Option<HotkeyBinding> {
    Some(HotkeyBinding::new(true, true, false, "I"))
}

fn default_hotkey_audio() -> Option<HotkeyBinding> {
    Some(HotkeyBinding::new(true, true, false, "A"))
}

fn default_hotkey_wooting() -> Option<HotkeyBinding> {
    Some(HotkeyBinding::new(true, true, false, "K"))
}

fn default_hotkey_mic_mute() -> Option<HotkeyBinding> {
    Some(HotkeyBinding::new(true, true, false, "M"))
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cycle_profile: default_hotkey_cycle(),
            toggle_lock: default_hotkey_lock(),
            toggle_input_lab: default_hotkey_lab(),
            cycle_audio_output: default_hotkey_audio(),
            cycle_wooting_profile: default_hotkey_wooting(),
            toggle_mic_mute: default_hotkey_mic_mute(),
        }
    }
}

/// Tray icon & Windows toast notification preferences (`settings.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayAlertConfig {
    /// Render live wireless mouse battery % & charge bar directly on the system tray icon.
    #[serde(default = "default_true")]
    pub battery_tray_icon: bool,
    /// Send a Windows toast notification when a wireless mouse drops to or below `low_battery_threshold_pct`.
    #[serde(default = "default_true")]
    pub low_battery_toast: bool,
    /// Send a Windows toast notification when a charging wireless mouse reaches 100%.
    #[serde(default = "default_true")]
    pub full_battery_toast: bool,
    /// Percentage threshold (`5..=50`) at which low-battery alerts trigger.
    #[serde(default = "default_low_battery_threshold")]
    pub low_battery_threshold_pct: u8,
    /// Send an optional Windows toast notification when Polled switches to a game/desktop profile.
    #[serde(default)]
    pub profile_switch_toast: bool,
    /// Show a brief click-through monochrome Win32 on-screen pill (zero game hooks) on profile/DPI switches.
    #[serde(default)]
    pub osd_pill_enabled: bool,
    /// Opt-in battery guard: automatically step down wireless 2K/4K/8K mice to 1000 Hz at or below
    /// `low_battery_threshold_pct`, restoring profile polling rate once charging or above recovery.
    #[serde(default)]
    pub low_battery_polling_guard: bool,
}

fn default_true() -> bool {
    true
}

fn default_low_battery_threshold() -> u8 {
    15
}

impl Default for TrayAlertConfig {
    fn default() -> Self {
        Self {
            battery_tray_icon: true,
            low_battery_toast: true,
            full_battery_toast: true,
            low_battery_threshold_pct: default_low_battery_threshold(),
            profile_switch_toast: false,
            osd_pill_enabled: false,
            low_battery_polling_guard: false,
        }
    }
}

impl TrayAlertConfig {
    /// Returns the low-battery alert threshold clamped to a safe `5..=50` percent range.
    pub fn clamped_threshold(&self) -> u8 {
        self.low_battery_threshold_pct.clamp(5, 50)
    }

    /// Hysteresis recovery percentage (`threshold + 10%`) before re-arming the low-battery toast.
    pub fn reset_threshold(&self) -> u8 {
        self.clamped_threshold().saturating_add(10).min(95)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub theme: ThemePreference,
    #[serde(default)]
    pub auto_switch_on_startup: bool,
    #[serde(default)]
    pub hotkeys: HotkeyConfig,
    #[serde(default)]
    pub alerts: TrayAlertConfig,
    /// Optional pinned playback endpoint IDs for `Cycle Audio Output` (e.g. Headphones <-> Speakers,
    /// skipping unused HDMI monitor audio outputs). Empty = cycle all active playback devices.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_cycle_device_ids: Vec<String>,
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
            hotkeys: HotkeyConfig::default(),
            alerts: TrayAlertConfig::default(),
            audio_cycle_device_ids: Vec::new(),
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
            hotkeys: HotkeyConfig {
                enabled: true,
                cycle_profile: Some(HotkeyBinding::new(true, true, false, "F8")),
                toggle_lock: Some(HotkeyBinding::new(true, true, false, "L")),
                toggle_input_lab: None,
                cycle_audio_output: Some(HotkeyBinding::new(true, true, false, "A")),
                cycle_wooting_profile: Some(HotkeyBinding::new(true, true, false, "K")),
                toggle_mic_mute: Some(HotkeyBinding::new(true, true, false, "M")),
            },
            alerts: TrayAlertConfig {
                battery_tray_icon: true,
                low_battery_toast: true,
                full_battery_toast: false,
                low_battery_threshold_pct: 20,
                profile_switch_toast: true,
                osd_pill_enabled: true,
                low_battery_polling_guard: true,
            },
            audio_cycle_device_ids: vec![
                "{0.0.0.00000000}.{headset-guid}".to_string(),
                "{0.0.0.00000000}.{speakers-guid}".to_string(),
            ],
        };
        custom.save_atomic(&path).unwrap();
        assert_eq!(AppSettings::load_or_default(&path), custom);
        assert_eq!(custom.alerts.clamped_threshold(), 20);
        assert_eq!(custom.alerts.reset_threshold(), 30);

        // Legacy settings.json without `alerts` still deserializes cleanly with defaults.
        let legacy_json = r#"{"schema_version":1,"theme":"dark","auto_switch_on_startup":true}"#;
        fs::write(&path, legacy_json).unwrap();
        let loaded_legacy = AppSettings::load_or_default(&path);
        assert!(loaded_legacy.auto_switch_on_startup);
        assert_eq!(loaded_legacy.alerts, TrayAlertConfig::default());
        assert_eq!(
            custom.hotkeys.cycle_profile.as_ref().unwrap().win32_vk(),
            Some(0x77)
        );
        assert_eq!(
            custom
                .hotkeys
                .cycle_profile
                .as_ref()
                .unwrap()
                .display_label(),
            "Ctrl+Alt+F8"
        );
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
