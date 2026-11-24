// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, Severity};
use crate::error::Result;
use crate::win::{RegHive, SystemEnv};

const DEVICE_GUARD_SUBKEY: &str = r"SYSTEM\CurrentControlSet\Control\DeviceGuard";
const ENABLE_VBS: &str = "EnableVirtualizationBasedSecurity";

const HVCI_SUBKEY: &str =
    r"SYSTEM\CurrentControlSet\Control\DeviceGuard\Scenarios\HypervisorEnforcedCodeIntegrity";
const HVCI_ENABLED: &str = "Enabled";

/// Report-only security transparency check for Windows Virtualization-Based Security (VBS)
/// and Memory Integrity (HVCI). Polled intentionally never auto-disables OS security features.
pub struct VbsMemoryIntegrityCheck;

impl HealthCheck for VbsMemoryIntegrityCheck {
    fn id(&self) -> &str {
        "vbs_memory_integrity"
    }

    fn name(&self) -> &str {
        "VBS / Memory Integrity (HVCI)"
    }

    fn can_fix(&self) -> bool {
        false
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let vbs = env.reg_get_dword(RegHive::Hklm, DEVICE_GUARD_SUBKEY, ENABLE_VBS)?;
        let hvci = env.reg_get_dword(RegHive::Hklm, HVCI_SUBKEY, HVCI_ENABLED)?;

        let vbs_str = match vbs {
            Some(1) => "Enabled",
            Some(0) => "Disabled",
            None => "Not configured",
            Some(_) => "Custom",
        };
        let hvci_str = match hvci {
            Some(1) => "Enabled",
            Some(0) => "Disabled",
            None => "Not configured",
            Some(_) => "Custom",
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value: format!("VBS: {vbs_str} · Memory Integrity (HVCI): {hvci_str}"),
            recommended_value: "Report only (security vs. CPU overhead trade-off)".to_string(),
            reason: "Core Isolation / Memory Integrity protects the Windows kernel and is required by some anti-cheats, though it can add minor CPU/interrupt overhead on older systems. Polled reports its state without weakening security."
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
    fn reports_vbs_and_hvci_as_info_report_only() {
        let env = MockEnv::new();
        let check = VbsMemoryIntegrityCheck;
        assert!(!check.can_fix());

        let r0 = check.scan(&env).unwrap();
        assert_eq!(r0.severity, Severity::Info);

        env.reg_set_dword(RegHive::Hklm, DEVICE_GUARD_SUBKEY, ENABLE_VBS, 1)
            .unwrap();
        env.reg_set_dword(RegHive::Hklm, HVCI_SUBKEY, HVCI_ENABLED, 1)
            .unwrap();
        let r1 = check.scan(&env).unwrap();
        assert_eq!(r1.severity, Severity::Info);
        assert!(r1.current_value.contains("VBS: Enabled"));
        assert!(r1
            .current_value
            .contains("Memory Integrity (HVCI): Enabled"));
    }
}
