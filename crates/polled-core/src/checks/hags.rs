// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const GRAPHICS_DRIVERS_SUBKEY: &str = r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers";
const HW_SCH_MODE: &str = "HwSchMode";

/// Optional reversible tweak for Hardware-Accelerated GPU Scheduling (`HwSchMode = 2`),
/// with full baseline rollback (`HwSchMode = 1` or absent).
pub struct HagsCheck;

impl HealthCheck for HagsCheck {
    fn id(&self) -> &str {
        "hags"
    }

    fn name(&self) -> &str {
        "Hardware-Accelerated GPU Scheduling (HAGS)"
    }

    fn is_optional_tweak(&self) -> bool {
        true
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Changing HAGS writes HwSchMode under HKLM\\SYSTEM\\CurrentControlSet\\Control\\GraphicsDrivers.")
    }

    fn requires_reboot(&self) -> bool {
        true
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let mode = env.reg_get_dword(RegHive::Hklm, GRAPHICS_DRIVERS_SUBKEY, HW_SCH_MODE)?;
        let (severity, current_value) = match mode {
            Some(2) => (Severity::Ok, "Enabled (HwSchMode = 2)".to_string()),
            Some(1) => (Severity::Info, "Disabled (HwSchMode = 1)".to_string()),
            None => (
                Severity::Info,
                "Not explicitly configured (driver/OS default)".to_string(),
            ),
            Some(other) => (Severity::Info, format!("Other (HwSchMode = {other})")),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Enabled (HwSchMode = 2) — optional tweak (required for Frame Gen)"
                .to_string(),
            reason: "Offloads GPU frame scheduling to the GPU processor, which lowers scheduling latency in supported titles and is required for DLSS/FSR Frame Generation (though a few VR or OBS setups prefer it off). Requires reboot; fully reversible via Undo."
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
        let value = env.reg_get_dword(RegHive::Hklm, GRAPHICS_DRIVERS_SUBKEY, HW_SCH_MODE)?;
        Ok(SavedValue::RegistryDword {
            hive: RegHive::Hklm,
            subkey: GRAPHICS_DRIVERS_SUBKEY.to_string(),
            name: HW_SCH_MODE.to_string(),
            value,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_dword(RegHive::Hklm, GRAPHICS_DRIVERS_SUBKEY, HW_SCH_MODE, 2)
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
    fn enables_hags_as_reversible_tweak_and_restores_original_on_undo() {
        let env = MockEnv::new();
        env.reg_set_dword(RegHive::Hklm, GRAPHICS_DRIVERS_SUBKEY, HW_SCH_MODE, 1)
            .unwrap();

        let check = HagsCheck;
        assert!(check.can_fix());
        assert!(check.is_optional_tweak());
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        let after = check.scan(&env).unwrap();
        assert_eq!(after.severity, Severity::Ok);
        assert!(after.current_value.contains("Enabled"));

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(RegHive::Hklm, GRAPHICS_DRIVERS_SUBKEY, HW_SCH_MODE)
                .unwrap(),
            Some(1)
        );
    }
}
