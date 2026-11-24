// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const DIRECTX_PREFS_SUBKEY: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
const GLOBAL_SETTINGS_VAL: &str = "DirectXUserGlobalSettings";
const SWAP_EFFECT_KEY: &str = "SwapEffectUpgradeEnable";

/// Reads `SwapEffectUpgradeEnable` from Windows' semicolon-delimited `DirectXUserGlobalSettings`.
fn parse_swap_effect(raw: Option<&str>) -> Option<u32> {
    let s = raw?;
    for token in s.split(';') {
        if let Some((k, v)) = token.split_once('=') {
            if k.trim().eq_ignore_ascii_case(SWAP_EFFECT_KEY) {
                return v.trim().parse::<u32>().ok();
            }
        }
    }
    None
}

/// Updates or inserts `SwapEffectUpgradeEnable=1;` while preserving all other global DX preferences.
fn set_swap_effect_enabled(raw: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut replaced = false;
    if let Some(s) = raw {
        for token in s.split(';') {
            let trimmed = token.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some((k, _)) = trimmed.split_once('=') {
                if k.trim().eq_ignore_ascii_case(SWAP_EFFECT_KEY) {
                    parts.push(format!("{SWAP_EFFECT_KEY}=1"));
                    replaced = true;
                    continue;
                }
            }
            parts.push(trimmed.to_string());
        }
    }
    if !replaced {
        parts.push(format!("{SWAP_EFFECT_KEY}=1"));
    }
    format!("{};", parts.join(";"))
}

/// Checks Windows 11 "Optimizations for windowed games" (`SwapEffectUpgradeEnable=1`),
/// which upgrades legacy DX10/DX11 borderless games to Independent Flip and cuts ~1 frame of input lag.
pub struct WindowedOptimizationsCheck;

impl HealthCheck for WindowedOptimizationsCheck {
    fn id(&self) -> &str {
        "windowed_optimizations"
    }

    fn name(&self) -> &str {
        "Windowed Game Optimizations (Flip-Model)"
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let raw = env.reg_get_string(RegHive::Hkcu, DIRECTX_PREFS_SUBKEY, GLOBAL_SETTINGS_VAL)?;
        let flag = parse_swap_effect(raw.as_deref());

        let (severity, current_value) = match flag {
            Some(1) => (
                Severity::Ok,
                "Enabled (SwapEffectUpgradeEnable = 1)".to_string(),
            ),
            Some(0) => (
                Severity::Warn,
                "Disabled (SwapEffectUpgradeEnable = 0)".to_string(),
            ),
            None => (
                Severity::Warn,
                "Not explicitly enabled (default / off on older builds)".to_string(),
            ),
            Some(other) => (
                Severity::Info,
                format!("Custom (SwapEffectUpgradeEnable = {other})"),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Enabled (SwapEffectUpgradeEnable = 1)".to_string(),
            reason: "Upgrades borderless/windowed DX10 & DX11 games from DWM copy-model to DXGI Independent Flip, cutting 1+ frame (~4–16 ms) of mouse-to-photon latency."
                .to_string(),
            severity,
            can_fix: true,
            is_optional_tweak: false,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,
            requires_reboot: false,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let raw = env.reg_get_string(RegHive::Hkcu, DIRECTX_PREFS_SUBKEY, GLOBAL_SETTINGS_VAL)?;
        Ok(SavedValue::UserGpuPreferences(raw))
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        let raw = env.reg_get_string(RegHive::Hkcu, DIRECTX_PREFS_SUBKEY, GLOBAL_SETTINGS_VAL)?;
        let updated = set_swap_effect_enabled(raw.as_deref());
        env.reg_set_string(
            RegHive::Hkcu,
            DIRECTX_PREFS_SUBKEY,
            GLOBAL_SETTINGS_VAL,
            &updated,
        )
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        let SavedValue::UserGpuPreferences(orig) = saved else {
            return Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::UserGpuPreferences".to_string(),
            });
        };
        match orig {
            Some(val) => env.reg_set_string(
                RegHive::Hkcu,
                DIRECTX_PREFS_SUBKEY,
                GLOBAL_SETTINGS_VAL,
                val,
            ),
            None => env.reg_delete_value(RegHive::Hkcu, DIRECTX_PREFS_SUBKEY, GLOBAL_SETTINGS_VAL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn enables_swap_effect_preserving_other_dx_flags_and_undoes() {
        let env = MockEnv::new();
        env.reg_set_string(
            RegHive::Hkcu,
            DIRECTX_PREFS_SUBKEY,
            GLOBAL_SETTINGS_VAL,
            "VRROptimizeEnable=1;SwapEffectUpgradeEnable=0;",
        )
        .unwrap();

        let check = WindowedOptimizationsCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Warn);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        let updated = env
            .reg_get_string(RegHive::Hkcu, DIRECTX_PREFS_SUBKEY, GLOBAL_SETTINGS_VAL)
            .unwrap()
            .unwrap();
        assert!(updated.contains("VRROptimizeEnable=1"));
        assert!(updated.contains("SwapEffectUpgradeEnable=1"));

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_string(RegHive::Hkcu, DIRECTX_PREFS_SUBKEY, GLOBAL_SETTINGS_VAL)
                .unwrap()
                .as_deref(),
            Some("VRROptimizeEnable=1;SwapEffectUpgradeEnable=0;")
        );
    }
}
