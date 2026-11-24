// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, Severity};
use crate::error::Result;
use crate::win::SystemEnv;

/// Report-only check inspecting whether connected USB mice / 4K-8K dongles sit behind an
/// external USB hub or share a hub with high-bandwidth webcams, USB audio DACs, or storage.
pub struct UsbTopologyCheck;

impl HealthCheck for UsbTopologyCheck {
    fn id(&self) -> &str {
        "usb_topology"
    }

    fn name(&self) -> &str {
        "USB Mouse Port & Hub Topology"
    }

    fn can_fix(&self) -> bool {
        false
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let mice = env.list_usb_topology()?;
        if mice.is_empty() {
            return Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: "No USB HID mouse topology detected".to_string(),
                recommended_value: "Direct rear motherboard USB port (dedicated hub)".to_string(),
                reason: "Could not map a USB HID mouse to a Windows USB hub entry.".to_string(),
                severity: Severity::Info,
                can_fix: false,
                is_optional_tweak: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,
                requires_reboot: false,
            });
        }

        let mut warnings: Vec<String> = Vec::new();
        let mut clean: Vec<String> = Vec::new();

        for m in &mice {
            let loc = match (m.port_number, m.hub_number) {
                (Some(p), Some(h)) => format!("Port #{p}, Hub #{h}"),
                (_, Some(h)) => format!("Hub #{h}"),
                _ => m.hub_name.clone(),
            };

            if m.behind_external_hub && !m.shared_heavy_devices.is_empty() {
                let peers: Vec<&str> = m
                    .shared_heavy_devices
                    .iter()
                    .map(|d| d.name.as_str())
                    .collect();
                warnings.push(format!(
                    "{} ({loc}) is behind external hub '{}' sharing bandwidth with: {}",
                    m.name,
                    m.hub_name,
                    peers.join(", ")
                ));
            } else if m.behind_external_hub {
                warnings.push(format!(
                    "{} ({loc}) is connected through external hub '{}'",
                    m.name, m.hub_name
                ));
            } else if !m.shared_heavy_devices.is_empty() {
                let peers: Vec<&str> = m
                    .shared_heavy_devices
                    .iter()
                    .map(|d| d.name.as_str())
                    .collect();
                warnings.push(format!(
                    "{} ({loc}) shares {} with high-bandwidth device(s): {}",
                    m.name,
                    m.hub_name,
                    peers.join(", ")
                ));
            } else {
                clean.push(format!("{} ({loc}, {})", m.name, m.hub_name));
            }
        }

        if !warnings.is_empty() {
            Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: warnings.join(" | "),
                recommended_value: "Direct rear motherboard CPU-lane USB port (no shared webcam/DAC/hub)".to_string(),
                reason: "External/monitor USB hubs and shared isochronous devices (webcams, capture cards, USB DACs) introduce microframe contention and polling jitter at 2K/4K/8K Hz. Plug your mouse/dongle directly into a rear motherboard USB port."
                    .to_string(),
                severity: Severity::Warn,
                can_fix: false,
                is_optional_tweak: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,
                requires_reboot: false,
            })
        } else {
            Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: clean.join(" | "),
                recommended_value: "Direct motherboard USB Root Hub (uncontested)".to_string(),
                reason: "Mouse/dongle is attached directly to a USB Root Hub with no active webcam, USB audio, or mass-storage devices contending on the same hub."
                    .to_string(),
                severity: Severity::Ok,
                can_fix: false,
                is_optional_tweak: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,
                requires_reboot: false,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;
    use crate::win::{UsbMouseTopology, UsbSharedDevice, UsbSharedDeviceKind};

    #[test]
    fn warns_when_mouse_behind_external_hub_or_sharing_with_webcam() {
        let env = MockEnv::new();
        env.set_usb_topology(vec![UsbMouseTopology {
            name: "Pulsar 8K Dongle".to_string(),
            vid_pid: "3710:5406".to_string(),
            port_number: Some(3),
            hub_number: Some(5),
            hub_name: "Generic SuperSpeed USB Hub".to_string(),
            behind_external_hub: true,
            shared_heavy_devices: vec![UsbSharedDevice {
                name: "Logitech Brio Webcam".to_string(),
                kind: UsbSharedDeviceKind::CameraOrCapture,
            }],
        }]);

        let report = UsbTopologyCheck.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Warn);
        assert!(report.current_value.contains("Logitech Brio Webcam"));
    }

    #[test]
    fn reports_ok_when_directly_attached_to_uncontested_root_hub() {
        let env = MockEnv::new();
        env.set_usb_topology(vec![UsbMouseTopology {
            name: "LAMZU 4K Receiver".to_string(),
            vid_pid: "3554:f510".to_string(),
            port_number: Some(4),
            hub_number: Some(1),
            hub_name: "USB Root Hub #1".to_string(),
            behind_external_hub: false,
            shared_heavy_devices: Vec::new(),
        }]);

        let report = UsbTopologyCheck.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Ok);
    }
}
