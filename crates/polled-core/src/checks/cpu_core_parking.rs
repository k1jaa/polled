// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::cpu_throttle::GUID_PROCESSOR_SUBGROUP;
use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{PowerValueIndex, SystemEnv};

/// Documented Microsoft setting GUID for Processor Performance Core Parking Min Cores (`CPMINCORES`).
pub const GUID_CPMINCORES: &str = "0cc5b647-c1df-4637-891a-dec35c318583";

/// Optional reversible tweak setting `CPMINCORES = 100%` on AC power (leaving DC/battery untouched)
/// to avoid 0.5–2 ms core-unpark wake stutters in competitive shooters.
pub struct CpuCoreParkingCheck;

impl HealthCheck for CpuCoreParkingCheck {
    fn id(&self) -> &str {
        "cpu_core_parking"
    }

    fn name(&self) -> &str {
        "CPU Core Parking Off (AC)"
    }

    fn is_optional_tweak(&self) -> bool {
        true
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Updating Processor Core Parking writes to the active Windows power policy under HKLM\\SYSTEM\\CurrentControlSet\\Control\\Power.")
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let scheme = env.get_active_power_scheme()?;
        let val =
            env.get_power_setting_index(&scheme.guid, GUID_PROCESSOR_SUBGROUP, GUID_CPMINCORES)?;

        let (severity, current_value) = match val.ac {
            Some(100) => (
                Severity::Ok,
                format!("AC Core Parking Off (100% unparked, plan: {})", scheme.name),
            ),
            Some(v) => (
                Severity::Info,
                format!(
                    "AC Min Unparked Cores: {v}% — parking active (plan: {})",
                    scheme.name
                ),
            ),
            None => (
                Severity::Info,
                format!(
                    "Windows default / core parking active (plan: {})",
                    scheme.name
                ),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "AC 100% unparked — optional low-latency tweak (DC untouched)".to_string(),
            reason: "Windows parks idle CPU cores to save power, which can add 0.5–2 ms unpark wake stutters on sudden input bursts or gunfights. Sets CPMINCORES=100% on AC power only (laptop battery policy is never modified) and restores your exact prior AC/DC values on Undo."
                .to_string(),
            severity,
            can_fix: true,
            is_optional_tweak: true,
            requires_elevation: true,
            elevation_reason: self.elevation_reason().map(str::to_string),
            requires_relogin: false,
            requires_reboot: false,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let scheme = env.get_active_power_scheme()?;
        let val =
            env.get_power_setting_index(&scheme.guid, GUID_PROCESSOR_SUBGROUP, GUID_CPMINCORES)?;
        Ok(SavedValue::PowerSetting {
            scheme_guid: scheme.guid,
            ac: val.ac,
            dc: val.dc,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        let scheme = env.get_active_power_scheme()?;
        env.set_power_setting_index(
            &scheme.guid,
            GUID_PROCESSOR_SUBGROUP,
            GUID_CPMINCORES,
            PowerValueIndex {
                ac: Some(100),
                dc: None,
            },
        )
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        let SavedValue::PowerSetting {
            scheme_guid,
            ac,
            dc,
        } = saved
        else {
            return Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::PowerSetting".to_string(),
            });
        };
        env.set_power_setting_index(
            scheme_guid,
            GUID_PROCESSOR_SUBGROUP,
            GUID_CPMINCORES,
            PowerValueIndex { ac: *ac, dc: *dc },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn unparks_ac_cores_leaving_dc_untouched_and_restores_on_undo() {
        let env = MockEnv::new();
        let active = env.get_active_power_scheme().unwrap();
        env.set_power_setting_index(
            &active.guid,
            GUID_PROCESSOR_SUBGROUP,
            GUID_CPMINCORES,
            PowerValueIndex {
                ac: Some(10),
                dc: Some(5),
            },
        )
        .unwrap();

        let check = CpuCoreParkingCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        let after = env
            .get_power_setting_index(&active.guid, GUID_PROCESSOR_SUBGROUP, GUID_CPMINCORES)
            .unwrap();
        assert_eq!(after.ac, Some(100));
        assert_eq!(after.dc, Some(5));

        check.undo(&env, &saved).unwrap();
        let restored = env
            .get_power_setting_index(&active.guid, GUID_PROCESSOR_SUBGROUP, GUID_CPMINCORES)
            .unwrap();
        assert_eq!(restored.ac, Some(10));
        assert_eq!(restored.dc, Some(5));
    }
}
