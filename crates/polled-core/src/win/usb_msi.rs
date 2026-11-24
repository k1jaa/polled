// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Read-only inspection of PCI Message Signaled Interrupt (MSI) mode for USB xHCI host
//! controllers, GPUs (Display adapters), and PCI HD Audio controllers.

use crate::error::Result;
use crate::win::{registry, PciMsiCategory, PciMsiDevice, RegHive, UsbControllerInfo};

const PCI_ENUM_ROOT: &str = r"SYSTEM\CurrentControlSet\Enum\PCI";
const CLASS_GUID_DISPLAY: &str = "{4d36e968-e325-11ce-bfc1-08002be10318}";
const CLASS_GUID_MEDIA: &str = "{4d36e96c-e325-11ce-bfc1-08002be10318}";

const GPU_SERVICES: &[&str] = &[
    "nvlddmkm",
    "amdkmdag",
    "amdwddmg",
    "igfx",
    "igdkmd64",
    "igdkmdn64",
    "igdkmdnd64",
    "xe",
];

const PCI_AUDIO_SERVICES: &[&str] = &[
    "hdaudbus",
    "hdaudio",
    "intcazaudaddservice",
    "nvhda",
    "atihdwt6",
    "amdhdaService",
];

fn clean_desc(raw: &str) -> String {
    raw.rsplit(';')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(raw)
        .to_string()
}

fn pci_vendor_tag(dev_key: &str) -> Option<&'static str> {
    let upper = dev_key.to_ascii_uppercase();
    let ven = upper.strip_prefix("VEN_")?;
    let id = ven.get(..4)?;
    match id {
        "10DE" => Some("NVIDIA"),
        "1002" | "1022" => Some("AMD"),
        "8086" => Some("Intel"),
        "10EC" => Some("Realtek"),
        "1102" => Some("Creative"),
        _ => None,
    }
}

/// Lists PCI USB xHCI host controllers (`Service == "USBXHCI"`) and reads whether Windows
/// Message Signaled Interrupts (`MSISupported = 1`) are enabled. Strictly read-only, no admin.
pub fn list_usb_controllers() -> Result<Vec<UsbControllerInfo>> {
    let mut controllers = Vec::new();
    let pci_devs = registry::list_subkeys(RegHive::Hklm, PCI_ENUM_ROOT).unwrap_or_default();

    for dev in pci_devs {
        let dev_path = format!(r"{PCI_ENUM_ROOT}\{dev}");
        let instances = registry::list_subkeys(RegHive::Hklm, &dev_path).unwrap_or_default();
        for inst in instances {
            let inst_path = format!(r"{dev_path}\{inst}");
            let service = registry::get_string(RegHive::Hklm, &inst_path, "Service")
                .ok()
                .flatten()
                .unwrap_or_default();
            if !service.eq_ignore_ascii_case("USBXHCI") {
                continue;
            }

            let name = registry::get_string(RegHive::Hklm, &inst_path, "FriendlyName")
                .ok()
                .flatten()
                .or_else(|| {
                    registry::get_string(RegHive::Hklm, &inst_path, "DeviceDesc")
                        .ok()
                        .flatten()
                })
                .map(|s| clean_desc(&s))
                .unwrap_or_else(|| format!("USB xHCI Controller ({dev})"));

            let msi_path = format!(
                r"{inst_path}\Device Parameters\Interrupt Management\MessageSignaledInterruptProperties"
            );
            let msi_supported = registry::get_dword(RegHive::Hklm, &msi_path, "MSISupported")
                .ok()
                .flatten()
                .map(|v| v != 0);

            controllers.push(UsbControllerInfo {
                name,
                msi_supported,
            });
        }
    }

    Ok(controllers)
}

/// Lists PCI GPUs (`Display` class / graphics driver services) and PCI High Definition Audio
/// controllers (`HDAudBus`, `HdAudio`, `MEDIA` class) together with their `MSISupported` state.
/// Strictly read-only, no admin required.
pub fn list_pci_msi_devices() -> Result<Vec<PciMsiDevice>> {
    let mut devices = Vec::new();
    let pci_devs = registry::list_subkeys(RegHive::Hklm, PCI_ENUM_ROOT).unwrap_or_default();

    for dev in pci_devs {
        let dev_path = format!(r"{PCI_ENUM_ROOT}\{dev}");
        let instances = registry::list_subkeys(RegHive::Hklm, &dev_path).unwrap_or_default();
        for inst in instances {
            let inst_path = format!(r"{dev_path}\{inst}");
            let service = registry::get_string(RegHive::Hklm, &inst_path, "Service")
                .ok()
                .flatten()
                .unwrap_or_default();
            if service.trim().is_empty() {
                continue;
            }
            let class_guid = registry::get_string(RegHive::Hklm, &inst_path, "ClassGUID")
                .ok()
                .flatten()
                .unwrap_or_default();
            let class_name = registry::get_string(RegHive::Hklm, &inst_path, "Class")
                .ok()
                .flatten()
                .unwrap_or_default();

            let is_gpu = class_guid.eq_ignore_ascii_case(CLASS_GUID_DISPLAY)
                || class_name.eq_ignore_ascii_case("Display")
                || GPU_SERVICES.iter().any(|s| service.eq_ignore_ascii_case(s));
            let is_audio = !is_gpu
                && (class_guid.eq_ignore_ascii_case(CLASS_GUID_MEDIA)
                    || class_name.eq_ignore_ascii_case("MEDIA")
                    || PCI_AUDIO_SERVICES
                        .iter()
                        .any(|s| service.eq_ignore_ascii_case(s)));

            let category = if is_gpu {
                PciMsiCategory::Gpu
            } else if is_audio {
                PciMsiCategory::Audio
            } else {
                continue;
            };

            let mut name = registry::get_string(RegHive::Hklm, &inst_path, "FriendlyName")
                .ok()
                .flatten()
                .or_else(|| {
                    registry::get_string(RegHive::Hklm, &inst_path, "DeviceDesc")
                        .ok()
                        .flatten()
                })
                .map(|s| clean_desc(&s))
                .unwrap_or_else(|| format!("PCI Device ({dev})"));

            if category == PciMsiCategory::Audio
                && name.eq_ignore_ascii_case("High Definition Audio Controller")
            {
                if let Some(tag) = pci_vendor_tag(&dev) {
                    name = format!("{name} ({tag})");
                }
            }

            let msi_path = format!(
                r"{inst_path}\Device Parameters\Interrupt Management\MessageSignaledInterruptProperties"
            );
            let msi_supported = registry::get_dword(RegHive::Hklm, &msi_path, "MSISupported")
                .ok()
                .flatten()
                .map(|v| v != 0);

            devices.push(PciMsiDevice {
                name,
                category,
                msi_supported,
            });
        }
    }

    // Show GPUs first, then PCI Audio controllers, stably sorted by name.
    devices.sort_by(|a, b| (a.category, &a.name).cmp(&(b.category, &b.name)));
    Ok(devices)
}
