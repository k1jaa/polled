// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, Severity};
use crate::error::Result;
use crate::win::SystemEnv;

/// Read-only inspection of PCI xHCI USB 3.x Host Controller Message Signaled Interrupts (MSI).
pub struct UsbXhciMsiCheck;

impl HealthCheck for UsbXhciMsiCheck {
    fn id(&self) -> &str {
        "usb_xhci_msi"
    }

    fn name(&self) -> &str {
        "USB xHCI Controller MSI Interrupts"
    }

    fn can_fix(&self) -> bool {
        false
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let controllers = env.list_usb_controllers()?;
        if controllers.is_empty() {
            return Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: "No PCI USB xHCI controllers found".to_string(),
                recommended_value: "MSI enabled (MSISupported = 1)".to_string(),
                reason:
                    "Could not enumerate PCI USB xHCI controllers from the Windows device tree."
                        .to_string(),
                severity: Severity::Info,
                can_fix: false,
                is_optional_tweak: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,
                requires_reboot: false,
            });
        }

        let total = controllers.len();
        let msi_on = controllers
            .iter()
            .filter(|c| c.msi_supported == Some(true))
            .count();
        let legacy: Vec<&str> = controllers
            .iter()
            .filter(|c| c.msi_supported == Some(false))
            .map(|c| c.name.as_str())
            .collect();

        let (severity, current_value, reason) = if !legacy.is_empty() {
            (
                Severity::Warn,
                format!(
                    "{msi_on}/{total} xHCI controllers using MSI · legacy IRQ on: {}",
                    legacy.join(", ")
                ),
                "A USB 3.x xHCI controller has MSI disabled (using legacy line-based IRQ sharing), which can increase USB interrupt latency on high-polling mice.",
            )
        } else if msi_on == total {
            (
                Severity::Ok,
                format!("MSI enabled on all {total} USB xHCI controller{}", if total == 1 { "" } else { "s" }),
                "All USB 3.x xHCI host controllers use low-latency Message Signaled Interrupts (MSI) instead of shared legacy IRQs.",
            )
        } else {
            (
                Severity::Info,
                format!("{msi_on}/{total} xHCI controllers explicitly set MSISupported=1"),
                "Windows defaults modern xHCI USB controllers to MSI when supported by firmware/driver.",
            )
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "MSI enabled on all xHCI controllers".to_string(),
            reason: reason.to_string(),
            severity,
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
    use crate::win::UsbControllerInfo;

    #[test]
    fn reports_ok_when_all_xhci_use_msi_and_warns_on_legacy() {
        let env = MockEnv::new();
        assert_eq!(UsbXhciMsiCheck.scan(&env).unwrap().severity, Severity::Ok);

        env.set_usb_controllers(vec![UsbControllerInfo {
            name: "AMD USB 3.10 eXtensible Host Controller".to_string(),
            msi_supported: Some(false),
        }]);
        assert_eq!(UsbXhciMsiCheck.scan(&env).unwrap().severity, Severity::Warn);
    }
}
