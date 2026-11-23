// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use hidapi::HidApi;
use serde::{Deserialize, Serialize};

use crate::error::{PolledError, Result};

/// Summary of Input/Output/Feature reports declared by a single HID collection's descriptor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HidReportSummary {
    /// Map of Report ID -> total byte length (including Report ID byte if non-zero).
    pub input_reports: BTreeMap<u8, usize>,
    pub output_reports: BTreeMap<u8, usize>,
    pub feature_reports: BTreeMap<u8, usize>,
}

impl HidReportSummary {
    pub fn has_report_id(&self, report_id: u8) -> bool {
        self.input_reports.contains_key(&report_id)
            || self.output_reports.contains_key(&report_id)
            || self.feature_reports.contains_key(&report_id)
    }

    pub fn compact_label(&self) -> String {
        let fmt_map = |prefix: &str, m: &BTreeMap<u8, usize>| -> Option<String> {
            if m.is_empty() {
                return None;
            }
            let items: Vec<String> = m
                .iter()
                .map(|(id, len)| format!("0x{id:02x}({len}B)"))
                .collect();
            Some(format!("{prefix}:[{}]", items.join(",")))
        };

        let parts: Vec<String> = [
            fmt_map("in", &self.input_reports),
            fmt_map("out", &self.output_reports),
            fmt_map("feat", &self.feature_reports),
        ]
        .into_iter()
        .flatten()
        .collect();

        if parts.is_empty() {
            "reports:unavailable".to_string()
        } else {
            parts.join(" ")
        }
    }
}

/// Parses standard USB HID 1.11 short items to extract Input/Output/Feature report IDs and byte lengths.
pub fn parse_hid_report_descriptor(desc: &[u8]) -> HidReportSummary {
    let mut input_bits: BTreeMap<u8, usize> = BTreeMap::new();
    let mut output_bits: BTreeMap<u8, usize> = BTreeMap::new();
    let mut feature_bits: BTreeMap<u8, usize> = BTreeMap::new();

    let mut report_id: u8 = 0;
    let mut report_size: usize = 0;
    let mut report_count: usize = 0;

    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        i += 1;

        // Long item (USB HID 1.11 §6.2.2.3)
        if prefix == 0xFE {
            if i >= desc.len() {
                break;
            }
            let data_len = usize::from(desc[i]);
            i = i.saturating_add(2 + data_len);
            continue;
        }

        let data_len = match prefix & 0x03 {
            0 => 0usize,
            1 => 1,
            2 => 2,
            _ => 4,
        };
        if i + data_len > desc.len() {
            break;
        }

        let mut val: u32 = 0;
        for byte_idx in 0..data_len {
            val |= u32::from(desc[i + byte_idx]) << (8 * byte_idx);
        }
        i += data_len;

        match prefix & 0xFC {
            0x74 => report_size = val as usize,  // Global: Report Size
            0x84 => report_id = val as u8,       // Global: Report ID
            0x94 => report_count = val as usize, // Global: Report Count
            0x80 => {
                // Main: Input
                *input_bits.entry(report_id).or_default() += report_size * report_count;
            }
            0x90 => {
                // Main: Output
                *output_bits.entry(report_id).or_default() += report_size * report_count;
            }
            0xB0 => {
                // Main: Feature
                *feature_bits.entry(report_id).or_default() += report_size * report_count;
            }
            _ => {}
        }
    }

    let to_bytes = |bits_map: BTreeMap<u8, usize>| -> BTreeMap<u8, usize> {
        bits_map
            .into_iter()
            .filter(|&(_, bits)| bits > 0)
            .map(|(id, bits)| {
                let payload_bytes = bits.div_ceil(8);
                let total = if id == 0 {
                    payload_bytes
                } else {
                    payload_bytes + 1
                };
                (id, total)
            })
            .collect()
    };

    HidReportSummary {
        input_reports: to_bytes(input_bits),
        output_reports: to_bytes(output_bits),
        feature_reports: to_bytes(feature_bits),
    }
}

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
    pub reports: Option<HidReportSummary>,
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

            // Read-only descriptor inspection to list exact Report IDs and byte lengths.
            let reports = dev.open_device(&api).ok().and_then(|opened| {
                let mut buf = [0u8; 4096];
                let len = opened.get_report_descriptor(&mut buf).ok()?;
                Some(parse_hid_report_descriptor(&buf[..len]))
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
                reports,
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

    #[test]
    fn parses_report_ids_and_lengths_from_hid_descriptor() {
        // Report ID 0x08: 16 bytes Input + 16 bytes Output -> 17B total including Report ID.
        let desc = [
            0x06, 0x02, 0xFF, // Usage Page (0xFF02)
            0x09, 0x02, // Usage (0x02)
            0xA1, 0x01, // Collection (Application)
            0x85, 0x08, // Report ID (8)
            0x75, 0x08, // Report Size (8)
            0x95, 0x10, // Report Count (16)
            0x81, 0x02, // Input (Data,Var,Abs)
            0x91, 0x02, // Output (Data,Var,Abs)
            0xC0, // End Collection
        ];
        let summary = parse_hid_report_descriptor(&desc);
        assert_eq!(summary.input_reports.get(&0x08), Some(&17));
        assert_eq!(summary.output_reports.get(&0x08), Some(&17));
        assert!(summary.feature_reports.is_empty());
        assert!(summary.has_report_id(0x08));
    }
}
