// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const POWER_THROTTLING_SUBKEY: &str = r"SYSTEM\CurrentControlSet\Control\Power\PowerThrottling";
const POWER_THROTTLING_OFF_VAL: &str = "PowerThrottlingOff";

/// Optional reversible tweak setting `PowerThrottlingOff = 1` to prevent Windows EcoQoS from
/// down-clocking background audio, voice, capture, or peripheral helpers onto slow cores.
pub struct PowerThrottlingCheck;

impl HealthCheck for PowerThrottlingCheck {
    fn id(&self) -> &str {
        "power_throttling"
    }

    fn name(&self) -> &str {
        "Disable Windows Power Throttling"
    }

    fn is_optional_tweak(&self) -> bool {
        true
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Setting PowerThrottlingOff writes to HKLM\\SYSTEM\\CurrentControlSet\\Control\\Power\\PowerThrottling.")
    }

    fn requires_reboot(&self) -> bool {
        true
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let val = env.reg_get_dword(
            RegHive::Hklm,
            POWER_THROTTLING_SUBKEY,
            POWER_THROTTLING_OFF_VAL,
        )?;
        let (severity, current_value) = match val {
            Some(1) => (
                Severity::Ok,
                "Disabled (PowerThrottlingOff = 1)".to_string(),
            ),
            Some(0) => (
                Severity::Info,
                "Enabled (PowerThrottlingOff = 0 — Windows EcoQoS active)".to_string(),
            ),
            None => (
                Severity::Info,
                "Enabled (Windows default — EcoQoS throttling active)".to_string(),
            ),
            Some(other) => (
                Severity::Info,
                format!("Custom (PowerThrottlingOff = {other})"),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "PowerThrottlingOff = 1 — optional desktop low-latency tweak"
                .to_string(),
            reason: "Stops Windows EcoQoS from aggressively down-clocking or pinning background voice, audio, OBS, and mouse helper threads onto slow efficiency cores. Recommended for desktop PCs (skip on battery-powered laptops). Requires reboot; fully reversible."
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
        let value = env.reg_get_dword(
            RegHive::Hklm,
            POWER_THROTTLING_SUBKEY,
            POWER_THROTTLING_OFF_VAL,
        )?;
        Ok(SavedValue::RegistryDword {
            hive: RegHive::Hklm,
            subkey: POWER_THROTTLING_SUBKEY.to_string(),
            name: POWER_THROTTLING_OFF_VAL.to_string(),
            value,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_dword(
            RegHive::Hklm,
            POWER_THROTTLING_SUBKEY,
            POWER_THROTTLING_OFF_VAL,
            1,
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
    fn disables_power_throttling_and_restores_absent_on_undo() {
        let env = MockEnv::new();
        let check = PowerThrottlingCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(
                RegHive::Hklm,
                POWER_THROTTLING_SUBKEY,
                POWER_THROTTLING_OFF_VAL
            )
            .unwrap(),
            None
        );
    }
}
