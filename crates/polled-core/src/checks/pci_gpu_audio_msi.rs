// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{PciMsiCategory, RegHive, SystemEnv};

const MSI_VALUE_NAME: &str = "MSISupported";

/// Reversible opt-in audit and fixer for PCI GPU and High Definition Audio controller Message
/// Signaled Interrupts (MSI) vs legacy line-based IRQ sharing.
///
/// Only modifies devices that already expose a `MessageSignaledInterruptProperties` key with
/// `MSISupported = 0` — never blindly creates MSI keys on unknown hardware. Every modified
/// device path and prior value is recorded in `baseline_snapshot.json` for full rollback.
pub struct PciGpuAudioMsiCheck;

impl HealthCheck for PciGpuAudioMsiCheck {
    fn id(&self) -> &str {
        "pci_gpu_audio_msi"
    }

    fn name(&self) -> &str {
        "GPU & Audio PCI MSI Interrupts"
    }

    fn can_fix(&self) -> bool {
        true
    }

    fn is_optional_tweak(&self) -> bool {
        true
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Enabling MSI mode writes MSISupported=1 under HKLM\\SYSTEM\\CurrentControlSet\\Enum\\PCI (reboot required; fully reversible via Undo).")
    }

    fn requires_reboot(&self) -> bool {
        true
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
                is_optional_tweak: true,
                requires_elevation: true,
                elevation_reason: self.elevation_reason().map(str::to_string),
                requires_relogin: false,
                requires_reboot: true,
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
        let fixable_legacy = devices
            .iter()
            .any(|d| d.msi_supported == Some(false) && !d.msi_reg_path.is_empty());
        let gpu_legacy = devices
            .iter()
            .any(|d| d.category == PciMsiCategory::Gpu && d.msi_supported == Some(false));

        let (severity, current_value, reason, can_fix_now) = if !legacy.is_empty() {
            let detail = if gpu_legacy {
                "Your GPU (or PCI Audio controller) is using legacy line-based IRQs instead of Message Signaled Interrupts (MSI), which can cause DPC latency spikes and frame-time stutter under 4K/8K mouse polling. Apply sets MSISupported=1 (reboot required; reversible via Undo)."
            } else {
                "A PCI High Definition Audio controller has MSI disabled (legacy line-based IRQ sharing). While your GPU uses MSI, legacy audio IRQs can share interrupt lines and add DPC jitter under load. Apply sets MSISupported=1 (reboot required; reversible via Undo)."
            };
            (
                Severity::Warn,
                format!(
                    "{msi_on}/{total} using MSI · legacy IRQ on: {}",
                    legacy.join(", ")
                ),
                detail,
                fixable_legacy,
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
                false,
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
                "Some PCI Display/Audio drivers leave MSISupported unset and rely on driver/OS defaults (left untouched for safety).",
                false,
            )
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "MSI enabled on PCI GPU & Audio controllers".to_string(),
            reason: reason.to_string(),
            severity,
            can_fix: can_fix_now,
            is_optional_tweak: true,
            requires_elevation: true,
            elevation_reason: self.elevation_reason().map(str::to_string),
            requires_relogin: false,
            requires_reboot: true,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let devices = env.list_pci_msi_devices()?;
        let mut entries = Vec::new();
        for dev in devices {
            if dev.msi_supported == Some(false) && !dev.msi_reg_path.is_empty() {
                let exact = env
                    .reg_get_dword(RegHive::Hklm, &dev.msi_reg_path, MSI_VALUE_NAME)?
                    .or(Some(0));
                entries.push((dev.msi_reg_path, exact));
            }
        }
        Ok(SavedValue::PciMsiMode { entries })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        let devices = env.list_pci_msi_devices()?;
        let targets: Vec<_> = devices
            .into_iter()
            .filter(|d| d.msi_supported == Some(false) && !d.msi_reg_path.is_empty())
            .collect();
        for dev in targets {
            env.reg_set_dword(RegHive::Hklm, &dev.msi_reg_path, MSI_VALUE_NAME, 1)?;
        }
        Ok(())
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        let SavedValue::PciMsiMode { entries } = saved else {
            return Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected PciMsiMode saved snapshot value".to_string(),
            });
        };
        for (path, original) in entries {
            match original {
                Some(val) => env.reg_set_dword(RegHive::Hklm, path, MSI_VALUE_NAME, *val)?,
                None => env.reg_delete_value(RegHive::Hklm, path, MSI_VALUE_NAME)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;
    use crate::win::{PciMsiCategory, PciMsiDevice};

    #[test]
    fn fixes_legacy_audio_msi_and_restores_original_zero_on_undo() {
        let env = MockEnv::new();
        assert_eq!(
            PciGpuAudioMsiCheck.scan(&env).unwrap().severity,
            Severity::Ok
        );

        let audio_path = r"SYSTEM\CurrentControlSet\Enum\PCI\VEN_10DE&DEV_AUDIO\0000\Device Parameters\Interrupt Management\MessageSignaledInterruptProperties".to_string();
        env.reg_set_dword(RegHive::Hklm, &audio_path, MSI_VALUE_NAME, 0)
            .unwrap();
        env.set_pci_msi_devices(vec![
            PciMsiDevice {
                name: "NVIDIA GeForce RTX 3060".to_string(),
                category: PciMsiCategory::Gpu,
                msi_supported: Some(true),
                msi_reg_path: r"SYSTEM\CurrentControlSet\Enum\PCI\VEN_10DE&DEV_GPU\0000\Device Parameters\Interrupt Management\MessageSignaledInterruptProperties".to_string(),
            },
            PciMsiDevice {
                name: "High Definition Audio Controller (NVIDIA)".to_string(),
                category: PciMsiCategory::Audio,
                msi_supported: Some(false),
                msi_reg_path: audio_path.clone(),
            },
        ]);

        let before = PciGpuAudioMsiCheck.scan(&env).unwrap();
        assert_eq!(before.severity, Severity::Warn);
        assert!(before.can_fix);
        assert!(before.is_optional_tweak);
        assert!(before.requires_elevation);
        assert!(before.requires_reboot);

        let saved = PciGpuAudioMsiCheck.capture_state(&env).unwrap();
        PciGpuAudioMsiCheck.apply_fix(&env).unwrap();

        let after = PciGpuAudioMsiCheck.scan(&env).unwrap();
        assert_eq!(after.severity, Severity::Ok);
        assert_eq!(
            env.reg_get_dword(RegHive::Hklm, &audio_path, MSI_VALUE_NAME)
                .unwrap(),
            Some(1)
        );

        PciGpuAudioMsiCheck.undo(&env, &saved).unwrap();
        let restored = PciGpuAudioMsiCheck.scan(&env).unwrap();
        assert_eq!(restored.severity, Severity::Warn);
        assert_eq!(
            env.reg_get_dword(RegHive::Hklm, &audio_path, MSI_VALUE_NAME)
                .unwrap(),
            Some(0)
        );
    }

    #[test]
    fn reports_info_and_non_fixable_when_msi_property_unset() {
        let env = MockEnv::new();
        env.set_pci_msi_devices(vec![PciMsiDevice {
            name: "NVIDIA GeForce RTX 3060".to_string(),
            category: PciMsiCategory::Gpu,
            msi_supported: None,
            msi_reg_path: String::new(),
        }]);
        let report = PciGpuAudioMsiCheck.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Info);
        assert!(!report.can_fix);
    }
}
