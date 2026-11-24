// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const DWM_SUBKEY: &str = r"SOFTWARE\Microsoft\Windows\Dwm";
const OVERLAY_TEST_MODE_VAL: &str = "OverlayTestMode";

/// Optional troubleshooting tweak toggling Windows DWM Multi-Plane Overlay (`OverlayTestMode = 5`).
///
/// Only needed on rigs experiencing mixed-refresh-rate dual-monitor stutters, black-screen flickers,
/// or Chrome/Discord hardware-video glitches; clean undo deletes `OverlayTestMode`.
pub struct MpoToggleCheck;

impl HealthCheck for MpoToggleCheck {
    fn id(&self) -> &str {
        "mpo_toggle"
    }

    fn name(&self) -> &str {
        "Disable Multi-Plane Overlay (MPO)"
    }

    fn is_optional_tweak(&self) -> bool {
        true
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Setting OverlayTestMode writes to HKLM\\SOFTWARE\\Microsoft\\Windows\\Dwm.")
    }

    fn requires_reboot(&self) -> bool {
        true
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let val = env.reg_get_dword(RegHive::Hklm, DWM_SUBKEY, OVERLAY_TEST_MODE_VAL)?;
        let (severity, current_value) = match val {
            Some(5) => (
                Severity::Ok,
                "MPO Disabled (OverlayTestMode = 5 — troubleshooting override active)".to_string(),
            ),
            None => (
                Severity::Info,
                "Windows default (Multi-Plane Overlay enabled)".to_string(),
            ),
            Some(other) => (
                Severity::Info,
                format!("Custom (OverlayTestMode = {other})"),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Troubleshooting only: OverlayTestMode = 5 if hitting dual-monitor flicker/stutter"
                .to_string(),
            reason: "Leave at Windows default if your display is smooth—MPO helps borderless presentation on healthy drivers. Apply this official Microsoft/NVIDIA troubleshooting override only if you experience black-screen flickers, Chrome/Discord video stutters, or mixed-Hz multi-monitor hitching. Requires reboot; Undo restores Windows default."
                .to_string(),
            severity,
            can_fix: true,
            is_optional_tweak: true,
            requires_elevation: true,
            elevation_reason: self.elevation_reason().map(str::to_string),
            requires_relogin: false,
            requires_reboot: true,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let value = env.reg_get_dword(RegHive::Hklm, DWM_SUBKEY, OVERLAY_TEST_MODE_VAL)?;
        Ok(SavedValue::RegistryDword {
            hive: RegHive::Hklm,
            subkey: DWM_SUBKEY.to_string(),
            name: OVERLAY_TEST_MODE_VAL.to_string(),
            value,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_dword(RegHive::Hklm, DWM_SUBKEY, OVERLAY_TEST_MODE_VAL, 5)
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
    fn disables_mpo_and_deletes_override_on_undo() {
        let env = MockEnv::new();
        let check = MpoToggleCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(RegHive::Hklm, DWM_SUBKEY, OVERLAY_TEST_MODE_VAL)
                .unwrap(),
            None
        );
    }
}
