// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const PERSONALIZE_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const ENABLE_TRANSPARENCY_VAL: &str = "EnableTransparency";

/// Optional zero-admin user tweak disabling Windows Mica/Acrylic backdrop transparency
/// (`EnableTransparency = 0`) to reduce Desktop Window Manager (DWM) compositing overhead.
pub struct TransparencyEffectsCheck;

impl HealthCheck for TransparencyEffectsCheck {
    fn id(&self) -> &str {
        "transparency_effects"
    }

    fn name(&self) -> &str {
        "Disable DWM Transparency Effects"
    }

    fn is_optional_tweak(&self) -> bool {
        true
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let val = env.reg_get_dword(RegHive::Hkcu, PERSONALIZE_SUBKEY, ENABLE_TRANSPARENCY_VAL)?;
        let (severity, current_value) = match val {
            Some(0) => (
                Severity::Ok,
                "Disabled (EnableTransparency = 0 — flat DWM compositing)".to_string(),
            ),
            Some(1) => (
                Severity::Info,
                "Enabled (Mica / Acrylic backdrop blur active)".to_string(),
            ),
            None => (Severity::Info, "Enabled (Windows default)".to_string()),
            Some(other) => (
                Severity::Info,
                format!("Custom (EnableTransparency = {other})"),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Disabled (0) — lighter DWM GPU overhead (optional user tweak)"
                .to_string(),
            reason: "Turns off Windows 10/11 Acrylic & Mica multi-pass backdrop blur in Start, Taskbar, and overlay windows so the Desktop Window Manager uses slightly less GPU time behind borderless games. Zero admin; takes effect immediately and is fully reversible."
                .to_string(),
            severity,
            can_fix: true,
            is_optional_tweak: true,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,
            requires_reboot: false,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let value =
            env.reg_get_dword(RegHive::Hkcu, PERSONALIZE_SUBKEY, ENABLE_TRANSPARENCY_VAL)?;
        Ok(SavedValue::RegistryDword {
            hive: RegHive::Hkcu,
            subkey: PERSONALIZE_SUBKEY.to_string(),
            name: ENABLE_TRANSPARENCY_VAL.to_string(),
            value,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_dword(
            RegHive::Hkcu,
            PERSONALIZE_SUBKEY,
            ENABLE_TRANSPARENCY_VAL,
            0,
        )
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        let SavedValue::RegistryDword {
            hive,
            subkey,
            name,
            value,
        } = saved
        else {
            return Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::RegistryDword".to_string(),
            });
        };
        match value {
            Some(v) => env.reg_set_dword(*hive, subkey, name, *v),
            None => env.reg_delete_value(*hive, subkey, name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn disables_transparency_and_restores_original_on_undo() {
        let env = MockEnv::new();
        env.reg_set_dword(
            RegHive::Hkcu,
            PERSONALIZE_SUBKEY,
            ENABLE_TRANSPARENCY_VAL,
            1,
        )
        .unwrap();

        let check = TransparencyEffectsCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(RegHive::Hkcu, PERSONALIZE_SUBKEY, ENABLE_TRANSPARENCY_VAL)
                .unwrap(),
            Some(1)
        );
    }
}
