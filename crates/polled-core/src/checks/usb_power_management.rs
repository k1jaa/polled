// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, Severity};
use crate::error::Result;
use crate::win::{SystemEnv, UsbPowerDeviceKind};

/// Report-only check inspecting device-level USB power-saving flags on connected HID mice,
/// wireless receivers, and USB Root Hubs.
pub struct UsbPowerManagementCheck;

impl HealthCheck for UsbPowerManagementCheck {
    fn id(&self) -> &'static str {
        "usb_power_management"
    }

    fn name(&self) -> &'static str {
        "USB Mouse & Root Hub Power Saving"
    }

    fn can_fix(&self) -> bool {
        false
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let devices = env.list_usb_power_devices()?;

        let mice_flagged: Vec<&str> = devices
            .iter()
            .filter(|d| d.kind == UsbPowerDeviceKind::MouseOrDongle && d.power_saving_enabled)
            .map(|d| d.name.as_str())
            .collect();
        let hubs_flagged = devices
            .iter()
            .filter(|d| d.kind == UsbPowerDeviceKind::RootHub && d.power_saving_enabled)
            .count();
        let total_mice = devices
            .iter()
            .filter(|d| d.kind == UsbPowerDeviceKind::MouseOrDongle)
            .count();
        let total_hubs = devices
            .iter()
            .filter(|d| d.kind == UsbPowerDeviceKind::RootHub)
            .count();

        let (severity, current_value, reason) = if !mice_flagged.is_empty() {
            (
                Severity::Warn,
                format!(
                    "Power saving enabled on {} ({} USB hub{} also flagged)",
                    mice_flagged.join(", "),
                    hubs_flagged,
                    if hubs_flagged == 1 { "" } else { "s" }
                ),
                "Device-level USB power saving is enabled on a connected mouse/receiver. In Device Manager -> Properties -> Power Management, uncheck 'Allow the computer to turn off this device to save power' to prevent wake stutters.",
            )
        } else if hubs_flagged > 0 {
            let hub_summary = format!(
                "{hubs_flagged}/{total_hubs} USB Root Hub{} allow power-off",
                if total_hubs == 1 { "" } else { "s" }
            );
            let value = if total_mice > 0 {
                format!(
                    "{total_mice} connected {} clean · {hub_summary}",
                    if total_mice == 1 { "mouse" } else { "mice" }
                )
            } else {
                hub_summary
            };
            (
                Severity::Info,
                value,
                "Your connected mouse has no device-level suspend flag, though Windows still allows powering off one or more USB Root Hubs. If you notice brief wireless disconnects, disable power-off on your mouse's USB Root Hub in Device Manager.",
            )
        } else if total_mice == 0 && total_hubs == 0 {
            (
                Severity::Info,
                "No USB mouse or Root Hub power flags found".to_string(),
                "No device-level USB power management entries were detected in the Windows USB registry tree.",
            )
        } else {
            (
                Severity::Ok,
                format!(
                    "No suspend flags on {total_mice} connected {} and {total_hubs} USB Root Hub{}",
                    if total_mice == 1 { "mouse" } else { "mice" },
                    if total_hubs == 1 { "" } else { "s" }
                ),
                "No explicit device-level USB power-saving flags are active on your connected mice or USB Root Hubs.",
            )
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Power-off disabled on mouse & active USB hub".to_string(),
            reason: reason.to_string(),
            severity,
            can_fix: false,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;
    use crate::win::UsbPowerDevice;

    #[test]
    fn distinguishes_clean_hub_info_and_mouse_warn_states() {
        let env = MockEnv::new();
        env.set_usb_power_devices(vec![
            UsbPowerDevice {
                name: "Pulsar X2 CrazyLight (3710:5406)".to_string(),
                kind: UsbPowerDeviceKind::MouseOrDongle,
                power_saving_enabled: false,
                detail: "Power saving disabled".to_string(),
            },
            UsbPowerDevice {
                name: "USB Root Hub (USB 3.0)".to_string(),
                kind: UsbPowerDeviceKind::RootHub,
                power_saving_enabled: false,
                detail: "Hub power-off disabled".to_string(),
            },
        ]);
        assert_eq!(
            UsbPowerManagementCheck.scan(&env).unwrap().severity,
            Severity::Ok
        );

        env.set_usb_power_devices(vec![
            UsbPowerDevice {
                name: "Pulsar X2 CrazyLight (3710:5406)".to_string(),
                kind: UsbPowerDeviceKind::MouseOrDongle,
                power_saving_enabled: false,
                detail: "No flags".to_string(),
            },
            UsbPowerDevice {
                name: "USB Root Hub (USB 3.0)".to_string(),
                kind: UsbPowerDeviceKind::RootHub,
                power_saving_enabled: true,
                detail: "Windows allowed to power off hub".to_string(),
            },
        ]);
        assert_eq!(
            UsbPowerManagementCheck.scan(&env).unwrap().severity,
            Severity::Info
        );

        env.set_usb_power_devices(vec![UsbPowerDevice {
            name: "LAMZU 4K Receiver (3554:F510)".to_string(),
            kind: UsbPowerDeviceKind::MouseOrDongle,
            power_saving_enabled: true,
            detail: "EnhancedPowerManagementEnabled=1".to_string(),
        }]);
        let r = UsbPowerManagementCheck.scan(&env).unwrap();
        assert_eq!(r.severity, Severity::Warn);
        assert!(r.current_value.contains("LAMZU 4K Receiver"));
    }
}
