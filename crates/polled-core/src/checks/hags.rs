// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, Severity};
use crate::error::Result;
use crate::win::{RegHive, SystemEnv};

const GRAPHICS_DRIVERS_SUBKEY: &str = r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers";
const HW_SCH_MODE: &str = "HwSchMode";

/// Reports Hardware-Accelerated GPU Scheduling (HAGS) status without modifying it.
pub struct HagsCheck;

impl HealthCheck for HagsCheck {
    fn id(&self) -> &str {
        "hags"
    }

    fn name(&self) -> &str {
        "Hardware-Accelerated GPU Scheduling (HAGS)"
    }

    fn can_fix(&self) -> bool {
        false
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let mode = env.reg_get_dword(RegHive::Hklm, GRAPHICS_DRIVERS_SUBKEY, HW_SCH_MODE)?;
        let current_value = match mode {
            Some(2) => "Enabled (HwSchMode = 2)".to_string(),
            Some(1) => "Disabled (HwSchMode = 1)".to_string(),
            None => "Not configured / unsupported by current driver".to_string(),
            Some(other) => format!("Other (HwSchMode = {other})"),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Report only (depends on GPU & game)".to_string(),
            reason: "HAGS can lower latency in some titles (and is required for Frame Generation) but causes stutter in others; requires a reboot to toggle."
                .to_string(),
            severity: Severity::Info,
            can_fix: false,

            is_optional_tweak: false,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,

            requires_reboot: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn reports_hags_states_as_report_only_info() {
        let env = MockEnv::new();
        let check = HagsCheck;
        assert!(!check.can_fix());

        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);

        env.reg_set_dword(RegHive::Hklm, GRAPHICS_DRIVERS_SUBKEY, HW_SCH_MODE, 2)
            .unwrap();
        let r = check.scan(&env).unwrap();
        assert_eq!(r.severity, Severity::Info);
        assert!(r.current_value.contains("Enabled"));

        env.reg_set_dword(RegHive::Hklm, GRAPHICS_DRIVERS_SUBKEY, HW_SCH_MODE, 1)
            .unwrap();
        assert!(check.scan(&env).unwrap().current_value.contains("Disabled"));
    }
}
