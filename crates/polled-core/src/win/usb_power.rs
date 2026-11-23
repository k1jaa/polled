// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

//! Read-only inspection of Windows USB power management flags on active USB Root Hubs
//! and currently connected USB HID mice / wireless dongles.

use std::collections::BTreeSet;

use crate::error::Result;
use crate::win::{registry, RegHive, UsbPowerDevice, UsbPowerDeviceKind};

const USB_ENUM_ROOT: &str = r"SYSTEM\CurrentControlSet\Enum\USB";

/// Strips INF resource prefixes like `@usbxhci.inf,%usbxhci.devicedesc%;USB Root Hub (USB 3.0)`.
fn clean_device_desc(raw: &str) -> String {
    raw.rsplit(';')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(raw)
        .to_string()
}

/// Inspects power-saving registry flags (`EnhancedPowerManagementEnabled`, `SelectiveSuspendEnabled`,
/// `AllowIdleIrpInD3`, `WDF\IdleInWorkingState`) on currently attached USB mice/dongles and
/// active USB Root Hubs. Strictly read-only and requires no administrator privileges.
pub fn list_usb_power_devices() -> Result<Vec<UsbPowerDevice>> {
    let mut results = Vec::new();

    // 1. Collect VID/PID pairs of currently connected HID mice (Usage Page 0x01, Usage 0x02).
    let mut connected_mice: BTreeSet<(u16, u16, String)> = BTreeSet::new();
    if let Ok(api) = hidapi::HidApi::new() {
        let mut seen_vid_pid: BTreeSet<(u16, u16)> = BTreeSet::new();
        for dev in api.device_list() {
            if dev.usage_page() == 0x01 && dev.usage() == 0x02 && dev.vendor_id() != 0 {
                let pair = (dev.vendor_id(), dev.product_id());
                if seen_vid_pid.insert(pair) {
                    let label = dev
                        .product_string()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("USB Mouse {:04X}:{:04X}", pair.0, pair.1));
                    connected_mice.insert((pair.0, pair.1, label));
                }
            }
        }
    }

    for (vid, pid, label) in connected_mice {
        let key_prefix = format!(r"{USB_ENUM_ROOT}\VID_{vid:04X}&PID_{pid:04X}");
        let instances = registry::list_subkeys(RegHive::Hklm, &key_prefix).unwrap_or_default();
        if instances.is_empty() {
            continue;
        }

        let mut any_enabled = false;
        let mut any_explicit_disabled = false;
        let mut flags_found = Vec::new();

        for inst in &instances {
            let dp = format!(r"{key_prefix}\{inst}\Device Parameters");
            if let Ok(Some(v)) =
                registry::get_dword(RegHive::Hklm, &dp, "EnhancedPowerManagementEnabled")
            {
                if v != 0 {
                    any_enabled = true;
                    flags_found.push("EnhancedPowerManagementEnabled=1");
                } else {
                    any_explicit_disabled = true;
                }
            }
            if let Ok(Some(v)) = registry::get_dword(RegHive::Hklm, &dp, "SelectiveSuspendEnabled")
            {
                if v != 0 {
                    any_enabled = true;
                    flags_found.push("SelectiveSuspendEnabled=1");
                } else {
                    any_explicit_disabled = true;
                }
            }
            if let Ok(Some(v)) = registry::get_dword(RegHive::Hklm, &dp, "AllowIdleIrpInD3") {
                if v != 0 {
                    any_enabled = true;
                    flags_found.push("AllowIdleIrpInD3=1");
                } else {
                    any_explicit_disabled = true;
                }
            }
        }

        flags_found.sort_unstable();
        flags_found.dedup();

        let detail = if any_enabled {
            format!("Power saving enabled ({})", flags_found.join(", "))
        } else if any_explicit_disabled {
            "Power saving disabled".to_string()
        } else {
            "No device-level suspend flags set".to_string()
        };

        results.push(UsbPowerDevice {
            name: format!("{label} ({vid:04X}:{pid:04X})"),
            kind: UsbPowerDeviceKind::MouseOrDongle,
            power_saving_enabled: any_enabled,
            detail,
        });
    }

    // 2. Inspect USB Root Hubs (ROOT_HUB30 / ROOT_HUB20) for explicit WDF / Enhanced PM idle flags.
    for hub_class in ["ROOT_HUB30", "ROOT_HUB20"] {
        let hub_root = format!(r"{USB_ENUM_ROOT}\{hub_class}");
        let instances = registry::list_subkeys(RegHive::Hklm, &hub_root).unwrap_or_default();
        for inst in instances {
            let inst_key = format!(r"{hub_root}\{inst}");
            let desc = registry::get_string(RegHive::Hklm, &inst_key, "DeviceDesc")
                .ok()
                .flatten()
                .map(|s| clean_device_desc(&s))
                .unwrap_or_else(|| hub_class.to_string());

            let dp = format!(r"{inst_key}\Device Parameters");
            let wdf = format!(r"{dp}\WDF");

            let epm = registry::get_dword(RegHive::Hklm, &dp, "EnhancedPowerManagementEnabled")
                .ok()
                .flatten();
            let idle = registry::get_dword(RegHive::Hklm, &wdf, "IdleInWorkingState")
                .ok()
                .flatten();

            let enabled = epm == Some(1) || idle == Some(1);
            let detail = match (epm, idle) {
                (Some(0), Some(0)) | (Some(0), None) | (None, Some(0)) => {
                    "Hub power-off disabled".to_string()
                }
                (Some(1), _) | (_, Some(1)) => "Windows allowed to power off hub".to_string(),
                _ => "Windows default".to_string(),
            };

            results.push(UsbPowerDevice {
                name: desc,
                kind: UsbPowerDeviceKind::RootHub,
                power_saving_enabled: enabled,
                detail,
            });
        }
    }

    Ok(results)
}
