// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{PowerValueIndex, SystemEnv};

/// Documented Microsoft Windows power subgroup GUID for Processor Power Management.
pub const GUID_PROCESSOR_SUBGROUP: &str = "54533251-82be-4824-96c1-47b60b740d00";
/// Documented Microsoft setting GUID for Minimum Processor State (`PROCTHROTTLEMIN`).
pub const GUID_PROCTHROTTLEMIN: &str = "893dee8e-2bef-41e0-89c6-b55d0929964c";
/// Documented Microsoft setting GUID for Maximum Processor State (`PROCTHROTTLEMAX`).
pub const GUID_PROCTHROTTLEMAX: &str = "bc5038f7-23e0-4960-96da-33abaf5935ec";

/// Ensures Maximum Processor State on AC is 100% (no accidental CPU cap) and reports Min State.
pub struct CpuProcessorStateCheck;

impl HealthCheck for CpuProcessorStateCheck {
    fn id(&self) -> &str {
        "cpu_processor_state"
    }

    fn name(&self) -> &str {
        "CPU Maximum Processor State (AC)"
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Updating Maximum Processor State writes to the active Windows power policy under HKLM\\SYSTEM\\CurrentControlSet\\Control\\Power.")
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let scheme = env.get_active_power_scheme()?;
        let max_val = env.get_power_setting_index(
            &scheme.guid,
            GUID_PROCESSOR_SUBGROUP,
            GUID_PROCTHROTTLEMAX,
        )?;
        let min_val = env.get_power_setting_index(
            &scheme.guid,
            GUID_PROCESSOR_SUBGROUP,
            GUID_PROCTHROTTLEMIN,
        )?;

        let min_str = min_val
            .ac
            .map(|v| format!(" · Min {v}%"))
            .unwrap_or_default();

        let (severity, current_value) = match max_val.ac {
            Some(100) => (
                Severity::Ok,
                format!("AC Max 100%{min_str} (plan: {})", scheme.name),
            ),
            Some(v) if v < 95 => (
                Severity::Bad,
                format!(
                    "AC Max capped at {v}%{min_str} — Turbo Boost disabled (plan: {})",
                    scheme.name
                ),
            ),
            Some(v) => (
                Severity::Warn,
                format!(
                    "AC Max capped at {v}%{min_str} — Turbo Boost disabled below 100% (plan: {})",
                    scheme.name
                ),
            ),
            None => (
                Severity::Info,
                format!("Not exposed in active plan ({})", scheme.name),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "AC Max 100%".to_string(),
            reason: "Any Maximum Processor State below 100% disables CPU Turbo Boost on Windows, hurting frame times and 4K/8K USB interrupt handling."
                .to_string(),
            severity,
            can_fix: true,
            is_optional_tweak: false,
            requires_elevation: self.requires_elevation(),
            elevation_reason: self.elevation_reason().map(str::to_string),
            requires_relogin: false,
            requires_reboot: false,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let scheme = env.get_active_power_scheme()?;
        let val = env.get_power_setting_index(
            &scheme.guid,
            GUID_PROCESSOR_SUBGROUP,
            GUID_PROCTHROTTLEMAX,
        )?;
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
            GUID_PROCTHROTTLEMAX,
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
            GUID_PROCTHROTTLEMAX,
            PowerValueIndex { ac: *ac, dc: *dc },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn flags_capped_cpu_max_fixes_to_100_and_undoes() {
        let env = MockEnv::new();
        let active = env.get_active_power_scheme().unwrap();
        env.set_power_setting_index(
            &active.guid,
            GUID_PROCESSOR_SUBGROUP,
            GUID_PROCTHROTTLEMAX,
            PowerValueIndex {
                ac: Some(99),
                dc: Some(80),
            },
        )
        .unwrap();

        let check = CpuProcessorStateCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Warn);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        let restored = env
            .get_power_setting_index(&active.guid, GUID_PROCESSOR_SUBGROUP, GUID_PROCTHROTTLEMAX)
            .unwrap();
        assert_eq!(restored.ac, Some(99));
        assert_eq!(restored.dc, Some(80));
    }
}
