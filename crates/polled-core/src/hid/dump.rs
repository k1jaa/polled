// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use chrono::{DateTime, Utc};
use hidapi::HidApi;
use serde::{Deserialize, Serialize};

use crate::error::{PolledError, Result};

/// Metadata for a single enumerated USB/Bluetooth HID interface, safe to share in GitHub issues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidDeviceDump {
    pub vendor_id: u16,
    pub product_id: u16,
    pub vid_pid: String,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial_number: Option<String>,
    pub release_number: u16,
    pub interface_number: i32,
    pub usage_page: u16,
    pub usage: u16,
    pub usage_page_label: String,
    pub bus_type: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidDumpReport {
    pub schema_version: u32,
    pub captured_at: DateTime<Utc>,
    pub serials_redacted: bool,
    pub device_count: usize,
    pub devices: Vec<HidDeviceDump>,
}

/// Enumerates all HID interfaces visible to the OS via `hidapi` (pure read-only discovery).
///
/// Serial numbers are redacted by default so users can safely paste `--dump` output into
/// GitHub issues when requesting support for a new mouse.
pub fn dump_hid_devices(include_serials: bool, filter_vid: Option<u16>) -> Result<HidDumpReport> {
    let api = HidApi::new().map_err(|e| PolledError::Win32 {
        context: "HidApi::new".to_string(),
        message: e.to_string(),
    })?;

    let mut devices: Vec<HidDeviceDump> = api
        .device_list()
        .filter(|dev| filter_vid.is_none_or(|vid| dev.vendor_id() == vid))
        .map(|dev| {
            let vid = dev.vendor_id();
            let pid = dev.product_id();
            let usage_page = dev.usage_page();
            let serial_number = dev.serial_number().map(|s| {
                if include_serials || s.is_empty() {
                    s.to_string()
                } else {
                    "<redacted>".to_string()
                }
            });

            HidDeviceDump {
                vendor_id: vid,
                product_id: pid,
                vid_pid: format!("{vid:04x}:{pid:04x}"),
                manufacturer: dev.manufacturer_string().map(str::to_string),
                product: dev.product_string().map(str::to_string),
                serial_number,
                release_number: dev.release_number(),
                interface_number: dev.interface_number(),
                usage_page,
                usage: dev.usage(),
                usage_page_label: describe_usage_page(usage_page).to_string(),
                bus_type: format!("{:?}", dev.bus_type()),
                path: dev.path().to_string_lossy().into_owned(),
            }
        })
        .collect();

    devices.sort_by(|a, b| {
        (
            a.vendor_id,
            a.product_id,
            a.interface_number,
            a.usage_page,
            a.usage,
        )
            .cmp(&(
                b.vendor_id,
                b.product_id,
                b.interface_number,
                b.usage_page,
                b.usage,
            ))
    });

    Ok(HidDumpReport {
        schema_version: 1,
        captured_at: Utc::now(),
        serials_redacted: !include_serials,
        device_count: devices.len(),
        devices,
    })
}

pub fn describe_usage_page(page: u16) -> &'static str {
    match page {
        0x0001 => "Generic Desktop (Mouse/Keyboard)",
        0x0007 => "Keyboard/Keypad",
        0x0008 => "LED",
        0x0009 => "Button",
        0x000C => "Consumer Control",
        0x000D => "Digitizer",
        0xFF00..=0xFFFF => "Vendor-Defined (Config/Telemetry)",
        _ => "Other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_common_hid_usage_pages() {
        assert_eq!(
            describe_usage_page(0x0001),
            "Generic Desktop (Mouse/Keyboard)"
        );
        assert_eq!(
            describe_usage_page(0xFF00),
            "Vendor-Defined (Config/Telemetry)"
        );
        assert_eq!(
            describe_usage_page(0xFFA0),
            "Vendor-Defined (Config/Telemetry)"
        );
    }
}
