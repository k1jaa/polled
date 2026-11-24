// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, Severity};
use crate::error::Result;
use crate::win::{PciMsiCategory, SystemEnv};

/// Read-only inspection of PCI GPU and High Definition Audio controller Message Signaled
/// Interrupts (MSI) vs legacy line-based IRQ sharing.
pub struct PciGpuAudioMsiCheck;

impl HealthCheck for PciGpuAudioMsiCheck {
    fn id(&self) -> &str {
        "pci_gpu_audio_msi"
    }

    fn name(&self) -> &str {
        "GPU & Audio PCI MSI Interrupts"
    }

    fn can_fix(&self) -> bool {
        false
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let devices = env.list_pci_msi_devices()?;
        if devices.is_empty() {
            return Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: "No PCI GPU or HD Audio controllers found".to_string(),
                recommended_value: "MSI enabled (MSISupported = 1)".to_string(),
                reason:
                    "Could not enumerate PCI Display or HD Audio controllers from the Windows device tree."
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

        let total = devices.len();
        let msi_on = devices
            .iter()
            .filter(|d| d.msi_supported == Some(true))
            .count();
        let legacy: Vec<String> = devices
            .iter()
            .filter(|d| d.msi_supported == Some(false))
            .map(|d| format!("{} [{}]", d.name, d.category.label()))
            .collect();
        let gpu_legacy = devices
            .iter()
            .any(|d| d.category == PciMsiCategory::Gpu && d.msi_supported == Some(false));

        let (severity, current_value, reason) = if !legacy.is_empty() {
            let detail = if gpu_legacy {
                "Your GPU (or PCI Audio controller) is using legacy line-based IRQs instead of Message Signaled Interrupts (MSI), which can cause DPC latency spikes and frame-time stutter under 4K/8K mouse polling."
            } else {
                "A PCI High Definition Audio controller has MSI disabled (legacy line-based IRQ sharing). While your GPU uses MSI, legacy audio IRQs can share interrupt lines and add DPC jitter under load."
            };
            (
                Severity::Warn,
                format!(
                    "{msi_on}/{total} using MSI · legacy IRQ on: {}",
                    legacy.join(", ")
                ),
                detail,
            )
        } else if msi_on == total {
            let summary: Vec<String> = devices
                .iter()
                .map(|d| format!("{} [{}]", d.name, d.category.label()))
                .collect();
            (
                Severity::Ok,
                format!("MSI enabled on all {total} devices ({})", summary.join(", ")),
                "Your PCI GPU and HD Audio controllers use low-latency Message Signaled Interrupts (MSI) instead of shared legacy IRQs.",
            )
        } else {
            let unset: Vec<String> = devices
                .iter()
                .filter(|d| d.msi_supported.is_none())
                .map(|d| format!("{} [{}]", d.name, d.category.label()))
                .collect();
            (
                Severity::Info,
                format!(
                    "{msi_on}/{total} explicitly set MSISupported=1 · unset on: {}",
                    unset.join(", ")
                ),
                "Some PCI Display/Audio drivers leave MSISupported unset and rely on driver/OS defaults.",
            )
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "MSI enabled on PCI GPU & Audio controllers".to_string(),
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
    use crate::win::{PciMsiCategory, PciMsiDevice};

    #[test]
    fn reports_ok_when_gpu_and_audio_use_msi_and_warns_on_legacy() {
        let env = MockEnv::new();
        assert_eq!(
            PciGpuAudioMsiCheck.scan(&env).unwrap().severity,
            Severity::Ok
        );

        env.set_pci_msi_devices(vec![
            PciMsiDevice {
                name: "NVIDIA GeForce RTX 3060".to_string(),
                category: PciMsiCategory::Gpu,
                msi_supported: Some(true),
            },
            PciMsiDevice {
                name: "High Definition Audio Controller (NVIDIA)".to_string(),
                category: PciMsiCategory::Audio,
                msi_supported: Some(false),
            },
        ]);
        let report = PciGpuAudioMsiCheck.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Warn);
        assert!(report.current_value.contains("1/2 using MSI"));
        assert!(report.current_value.contains("High Definition Audio"));
    }

    #[test]
    fn reports_info_when_msi_property_unset() {
        let env = MockEnv::new();
        env.set_pci_msi_devices(vec![PciMsiDevice {
            name: "NVIDIA GeForce RTX 3060".to_string(),
            category: PciMsiCategory::Gpu,
            msi_supported: None,
        }]);
        assert_eq!(
            PciGpuAudioMsiCheck.scan(&env).unwrap().severity,
            Severity::Info
        );
    }
}
