// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Read-only inspection of PCI xHCI USB 3.x Host Controller Message Signaled Interrupt (MSI) mode.

use crate::error::Result;
use crate::win::{registry, RegHive, UsbControllerInfo};

const PCI_ENUM_ROOT: &str = r"SYSTEM\CurrentControlSet\Enum\PCI";

fn clean_desc(raw: &str) -> String {
    raw.rsplit(';')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(raw)
        .to_string()
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
