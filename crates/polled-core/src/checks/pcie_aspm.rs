// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{PowerValueIndex, SystemEnv};

/// Documented Microsoft Windows power subgroup GUID for PCI Express (`GUID_PCIEXPRESS_SETTINGS_SUBGROUP`).
pub const GUID_PCIEXPRESS_SUBGROUP: &str = "501a4d13-42af-4429-9fd1-a8218c268e20";
/// Documented Microsoft Windows power setting GUID for Link State Power Management (`GUID_PCIEXPRESS_ASPM_POLICY`).
pub const GUID_PCIEXPRESS_ASPM_POLICY: &str = "ee12f906-d277-404b-b6da-e5fa1a576df5";

const ASPM_OFF: u32 = 0;
const ASPM_MODERATE: u32 = 1;
const ASPM_MAXIMUM: u32 = 2;

fn aspm_label(val: u32) -> &'static str {
    match val {
        ASPM_OFF => "Off",
        ASPM_MODERATE => "Moderate power savings",
        ASPM_MAXIMUM => "Maximum power savings",
        _ => "Custom",
    }
}

/// Checks PCIe Link State Power Management (ASPM) in the active Windows power plan.
pub struct PcieAspmCheck;

impl HealthCheck for PcieAspmCheck {
    fn id(&self) -> &str {
        "pcie_aspm"
    }

    fn name(&self) -> &str {
        "PCIe Link State Power Management (ASPM)"
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Updating PCIe Link State Power Management writes to the active Windows power policy under HKLM\\SYSTEM\\CurrentControlSet\\Control\\Power.")
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let scheme = env.get_active_power_scheme()?;
        let val = env.get_power_setting_index(
            &scheme.guid,
            GUID_PCIEXPRESS_SUBGROUP,
            GUID_PCIEXPRESS_ASPM_POLICY,
        )?;

        let (severity, current_value) = match (val.ac, val.dc) {
            (Some(ASPM_OFF), Some(dc)) if dc != ASPM_OFF => (
                Severity::Ok,
                format!(
                    "Off on AC, {} on battery/DC (plan: {})",
                    aspm_label(dc),
                    scheme.name
                ),
            ),
            (Some(ASPM_OFF), _) => (Severity::Ok, format!("Off on AC (plan: {})", scheme.name)),
            (Some(ac), _) if ac == ASPM_MODERATE || ac == ASPM_MAXIMUM => (
                Severity::Warn,
                format!("{} on AC (plan: {})", aspm_label(ac), scheme.name),
            ),
            (None, Some(ASPM_OFF)) => (
                Severity::Ok,
                format!("Off on DC; AC not set (plan: {})", scheme.name),
            ),
            (None, Some(dc)) if dc == ASPM_MODERATE || dc == ASPM_MAXIMUM => (
                Severity::Warn,
                format!(
                    "{} on DC; AC not set (plan: {})",
                    aspm_label(dc),
                    scheme.name
                ),
            ),
            (None, None) => (
                Severity::Info,
                format!("Not exposed in active plan ({})", scheme.name),
            ),
            (ac, dc) => (
                Severity::Info,
                format!("Unknown value AC={ac:?}, DC={dc:?} (plan: {})", scheme.name),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Off on AC".to_string(),
            reason: "PCIe ASPM can place PCIe xHCI USB host controllers into low-power link states, causing brief polling-rate dips or micro-stutters on 4K/8K mice."
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
            GUID_PCIEXPRESS_SUBGROUP,
            GUID_PCIEXPRESS_ASPM_POLICY,
        )?;
        Ok(SavedValue::PowerSetting {
            scheme_guid: scheme.guid,
            ac: val.ac,
            dc: val.dc,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        let scheme = env.get_active_power_scheme()?;
        // Disable ASPM on AC while leaving battery (DC) unchanged to preserve laptop battery life.
        env.set_power_setting_index(
            &scheme.guid,
            GUID_PCIEXPRESS_SUBGROUP,
            GUID_PCIEXPRESS_ASPM_POLICY,
            PowerValueIndex {
                ac: Some(ASPM_OFF),
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
            GUID_PCIEXPRESS_SUBGROUP,
            GUID_PCIEXPRESS_ASPM_POLICY,
            PowerValueIndex { ac: *ac, dc: *dc },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn detects_pcie_aspm_power_savings_fixes_ac_and_undoes() {
        let env = MockEnv::new();
        let active = env.get_active_power_scheme().unwrap();
        env.set_power_setting_index(
            &active.guid,
            GUID_PCIEXPRESS_SUBGROUP,
            GUID_PCIEXPRESS_ASPM_POLICY,
            PowerValueIndex {
                ac: Some(ASPM_MAXIMUM),
                dc: Some(ASPM_MAXIMUM),
            },
        )
        .unwrap();

        let check = PcieAspmCheck;
        let report = check.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Warn);
        assert!(report.current_value.contains("Maximum"));

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();

        let fixed = check.scan(&env).unwrap();
        assert_eq!(fixed.severity, Severity::Ok);
        let idx = env
            .get_power_setting_index(
                &active.guid,
                GUID_PCIEXPRESS_SUBGROUP,
                GUID_PCIEXPRESS_ASPM_POLICY,
            )
            .unwrap();
        assert_eq!(idx.ac, Some(ASPM_OFF));
        assert_eq!(idx.dc, Some(ASPM_MAXIMUM));

        check.undo(&env, &saved).unwrap();
        let restored = env
            .get_power_setting_index(
                &active.guid,
                GUID_PCIEXPRESS_SUBGROUP,
                GUID_PCIEXPRESS_ASPM_POLICY,
            )
            .unwrap();
        assert_eq!(restored.ac, Some(ASPM_MAXIMUM));
    }
}
