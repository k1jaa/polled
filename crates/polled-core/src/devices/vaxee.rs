// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! VAXEE Control Center v3.6 (`Report ID 0x0E` on `0xFF05:0x01`) HID feature-report driver.
//!
//! Covers XE Wireless, XE V2, XE-S, XE-S-L, NP-01, NP-01S, NP-01S V2/V3/Ergo, NP-01 Ergo,
//! Outset AX, E1 Wireless, and VAXEE x Ninjutso Sora across wired USB and 1K/4K receivers.
//!
//! # Protocol Attribution & Licenses (AGPL-3.0 compatible)
//! Packet framing, command IDs, product tables, polling codes, and DPI/LOD encodings are
//! ported from:
//! - `OpenMouse-Project/mouse-protocol` (<https://github.com/OpenMouse-Project/mouse-protocol>)
//!   - `src/vaxee/index.ts`, `src/drivers/vaxee/hid.ts`
//!   - **AGPL-3.0-or-later**, Copyright (C) OpenMouse Project Contributors
//!
//! No HID bytes are guessed.

use std::thread;
use std::time::Duration;

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    BatteryStatus, DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice,
    MouseSettings, SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};
use crate::hid::parse_hid_report_descriptor;

pub const VAXEE_VID: u16 = 0x3057;
pub const VAXEE_USAGE_PAGE: u16 = 0xFF05;
pub const VAXEE_USAGE: u16 = 0x0001;
pub const VAXEE_REPORT_ID: u8 = 0x0E;
pub const VAXEE_REPORT_SIZE: usize = 63;

const PAUSE_MS: u64 = 100;

const CMD_DPI_STAGE: u8 = 0x02;
const CMD_DPI_VALUE: u8 = 0x04;
const CMD_POLLING: u8 = 0x07;
const CMD_TRACKING: u8 = 0x08;
const CMD_LOD: u8 = 0x09;
const CMD_MOUSE_PID: u8 = 0x0A;
const CMD_BATTERY: u8 = 0x0B;
const CMD_CHARGING: u8 = 0x10;

pub const VAXEE_PRODUCT_IDS: &[u16] = &[
    0x0005, 0x1001, 0x1002, 0x1003, 0x1004, 0x1005, 0x1006, 0x1007, 0x1008, 0x1009, 0x1010, 0x1011,
    0x1012, 0x1013, 0x1014, 0x1015, 0x2001, 0x2002,
];

const VAXEE_RECEIVER_PIDS: &[u16] = &[0x0005, 0x1001, 0x1002, 0x2001, 0x2002];
const VAXEE_4K_RECEIVER_PIDS: &[u16] = &[0x1002, 0x2001, 0x2002];

pub fn vaxee_mouse_name(mouse_pid: u16) -> Option<&'static str> {
    match mouse_pid {
        0x1003 => Some("VAXEE XE Wireless"),
        0x1004 => Some("VAXEE NP-01S Wireless"),
        0x1005 => Some("VAXEE Outset AX Wireless"),
        0x1006 => Some("VAXEE NP-01 Wireless"),
        0x1007 => Some("VAXEE XE-S Wireless"),
        0x1008 => Some("VAXEE XE-S-L Wireless"),
        0x1009 => Some("VAXEE x Ninjutso Sora"),
        0x1010 => Some("VAXEE E1 Wireless"),
        0x1011 => Some("VAXEE NP-01S V2 Wireless"),
        0x1012 => Some("VAXEE XE V2 Wireless"),
        0x1013 => Some("VAXEE NP-01S Ergo Wireless"),
        0x1014 => Some("VAXEE NP-01S V3 Wireless"),
        0x1015 => Some("VAXEE NP-01 Ergo Wireless"),
        _ => None,
    }
}

/// Builds a 64-byte VAXEE HID feature report (`[0x0E, 0xA5, cmd, write?2:1, len, values...]`).
pub fn build_vaxee_request(
    command: u8,
    length: u8,
    values: &[u8],
    write: bool,
) -> [u8; VAXEE_REPORT_SIZE + 1] {
    let mut pkt = [0u8; VAXEE_REPORT_SIZE + 1];
    pkt[0] = VAXEE_REPORT_ID;
    pkt[1] = 0xA5;
    pkt[2] = command;
    pkt[3] = if write { 2 } else { 1 };
    pkt[4] = length;
    let copy_len = values.len().min(VAXEE_REPORT_SIZE - 4);
    pkt[5..5 + copy_len].copy_from_slice(&values[..copy_len]);
    pkt
}

/// Validates and extracts a VAXEE reply envelope (handles with or without leading report ID).
pub fn decode_vaxee_reply(raw: &[u8], command: u8, min_data_len: usize) -> Option<Vec<u8>> {
    let body = if raw.first() == Some(&VAXEE_REPORT_ID) && raw.get(1) == Some(&0xA5) {
        &raw[1..]
    } else if raw.first() == Some(&0xA5) {
        raw
    } else {
        return None;
    };
    if body.len() < 5 + min_data_len
        || body[0] != 0xA5
        || body[1] != command
        || body[2] != 3
        || body[3] != 1
        || usize::from(body[4]) < min_data_len
    {
        return None;
    }
    Some(body.to_vec())
}

pub fn decode_vaxee_polling_rate(code: u8) -> Option<u16> {
    match code {
        1 => Some(500),
        2 => Some(1000),
        3 => Some(2000),
        4 => Some(4000),
        5 => Some(8000),
        _ => None,
    }
}

pub fn encode_vaxee_polling_rate(hz: u16) -> Option<u8> {
    match hz {
        500 => Some(1),
        1000 => Some(2),
        2000 => Some(3),
        4000 => Some(4),
        8000 => Some(5),
        _ => None,
    }
}

/// Hardware driver for VAXEE Control Center v3.6 mice.
pub struct VaxeeDevice {
    hid: HidDevice,
    pid: u16,
    display_name: String,
    vid_pid: String,
}

impl VaxeeDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.display_name.clone(),
            message: message.into(),
        }
    }

    fn exchange(
        &self,
        command: u8,
        length: u8,
        values: &[u8],
        min_reply: usize,
        write: bool,
    ) -> Result<Vec<u8>> {
        let req = build_vaxee_request(command, length, values, write);
        self.hid.send_feature_report(&req).map_err(|e| {
            self.dev_err(format!("VAXEE feature write 0x{command:02x} failed: {e}"))
        })?;
        thread::sleep(Duration::from_millis(PAUSE_MS));

        let mut buf = [0u8; VAXEE_REPORT_SIZE + 1];
        buf[0] = VAXEE_REPORT_ID;
        let n = self
            .hid
            .get_feature_report(&mut buf)
            .map_err(|e| self.dev_err(format!("VAXEE feature read 0x{command:02x} failed: {e}")))?;
        decode_vaxee_reply(&buf[..n], command, min_reply).ok_or_else(|| {
            self.dev_err(format!(
                "Invalid VAXEE reply for command 0x{command:02x} (ensure mouse is awake)"
            ))
        })
    }

    fn supported_polling_rates(&self) -> Vec<u16> {
        if VAXEE_4K_RECEIVER_PIDS.contains(&self.pid) {
            vec![500, 1000, 2000, 4000]
        } else {
            vec![500, 1000]
        }
    }
}

impl MouseDevice for VaxeeDevice {
    fn info(&self) -> DeviceInfo {
        let wireless = VAXEE_RECEIVER_PIDS.contains(&self.pid);
        DeviceInfo {
            id: format!("vaxee_{:04x}", self.pid),
            name: self.display_name.clone(),
            vendor: "VAXEE".to_string(),
            vid_pid: Some(self.vid_pid.clone()),
            connection: if wireless {
                "Wireless 2.4 GHz Receiver".to_string()
            } else {
                "Wired USB".to_string()
            },
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            dpi_min: 100,
            dpi_max: 26_000,
            dpi_step: 50,
            polling_rates_hz: self.supported_polling_rates(),
            // VAXEE exposes Low (1 -> 1.0 mm) and High (2 -> 2.0 mm).
            lod_options: vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20],
            supports_debounce: false,
            debounce_min_ms: 0,
            debounce_max_ms: 0,
            supports_motion_sync: false,
            supports_angle_snapping: false,
            supports_ripple_control: false,
            supports_volatile_writes: false,
            flash_write_warning: Some(format!(
                "{} persists settings to onboard memory. Polled diffs unchanged values and rate-limits rapid writes.",
                self.display_name
            )),
        }
    }

    fn read_battery(&mut self) -> Option<BatteryStatus> {
        let steps = self.exchange(CMD_BATTERY, 1, &[], 1, false).ok()?[5];
        if steps > 20 {
            return None;
        }
        let charging = self
            .exchange(CMD_CHARGING, 1, &[], 1, false)
            .ok()
            .is_some_and(|r| r[5] != 0);
        Some(BatteryStatus {
            percent: steps * 5,
            charging,
            voltage_mv: None,
        })
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        let stage = self.exchange(CMD_DPI_STAGE, 1, &[], 1, false)?[5];
        if !(1..=4).contains(&stage) {
            return Err(self.dev_err(format!("Invalid VAXEE DPI stage {stage}")));
        }
        let dpi_reply = self.exchange(CMD_DPI_VALUE, 3, &[stage], 3, false)?;
        let dpi = u16::from_le_bytes([dpi_reply[6], dpi_reply[7]]);

        let poll_code = self.exchange(CMD_POLLING, 1, &[], 1, false)?[5];
        let polling_rate_hz = decode_vaxee_polling_rate(poll_code)
            .ok_or_else(|| self.dev_err(format!("Unknown VAXEE polling code {poll_code}")))?;

        let lod_code = self.exchange(CMD_LOD, 1, &[], 1, false)?[5];
        let lod = match lod_code {
            1 => Some(LiftOffDistance::Mm10),
            2 => Some(LiftOffDistance::Mm20),
            _ => None,
        };

        Ok(MouseSettings {
            dpi: Some(dpi),
            polling_rate_hz: Some(polling_rate_hz),
            lod,
            ..Default::default()
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        let current = self.read_settings()?;

        if let Some(target_dpi) = settings.dpi {
            if current.dpi != Some(target_dpi) {
                let stage = self.exchange(CMD_DPI_STAGE, 1, &[], 1, false)?[5];
                if !(1..=4).contains(&stage) {
                    return Err(self.dev_err(format!("Invalid VAXEE DPI stage {stage}")));
                }
                let le = target_dpi.to_le_bytes();
                let _ = self.exchange(CMD_DPI_VALUE, 3, &[stage, le[0], le[1]], 1, true)?;
            }
        }

        if let Some(target_hz) = settings.polling_rate_hz {
            if current.polling_rate_hz != Some(target_hz) {
                if target_hz > 1000 {
                    let newer = self.pid == 0x2002;
                    let tracking =
                        self.exchange(CMD_TRACKING, if newer { 2 } else { 1 }, &[], 1, false)?[5];
                    let is_standard_mode = if newer {
                        tracking == 0
                    } else {
                        tracking == 2 || tracking == 4
                    };
                    if is_standard_mode {
                        return Err(self.dev_err(
                            "VAXEE mouse is in Standard tracking mode (limited to 1000 Hz; enable Competitive mode for 2K/4K)",
                        ));
                    }
                }
                let code = encode_vaxee_polling_rate(target_hz).ok_or_else(|| {
                    PolledError::UnsupportedSetting {
                        device: self.display_name.clone(),
                        detail: format!("Unsupported VAXEE polling rate {target_hz} Hz"),
                    }
                })?;
                let _ = self.exchange(CMD_POLLING, 1, &[code], 1, true)?;
            }
        }

        if let Some(target_lod) = settings.lod {
            if current.lod != Some(target_lod) {
                let code = match target_lod {
                    LiftOffDistance::Mm10 => 1u8,
                    LiftOffDistance::Mm20 => 2u8,
                    LiftOffDistance::Mm07 => {
                        return Err(PolledError::UnsupportedSetting {
                            device: self.display_name.clone(),
                            detail: "VAXEE supports Low (1.0 mm) or High (2.0 mm) LOD only"
                                .to_string(),
                        });
                    }
                };
                let _ = self.exchange(CMD_LOD, 1, &[code], 1, true)?;
            }
        }

        Ok(())
    }
}

/// Protocol-family auto-detector for VAXEE Control Center v3.6 mice.
pub struct VaxeeProtocolDriver;

impl DeviceDriver for VaxeeProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "VAXEE Control Center v3.6 (0x3057)"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();
        for dev in api.device_list() {
            if dev.vendor_id() != VAXEE_VID
                || !VAXEE_PRODUCT_IDS.contains(&dev.product_id())
                || dev.usage_page() != VAXEE_USAGE_PAGE
                || dev.usage() != VAXEE_USAGE
            {
                continue;
            }
            let Ok(hid) = dev.open_device(api) else {
                continue;
            };
            let mut desc_buf = [0u8; 4096];
            let has_feat_0e = hid
                .get_report_descriptor(&mut desc_buf)
                .ok()
                .map(|len| parse_hid_report_descriptor(&desc_buf[..len]))
                .is_some_and(|s| s.feature_reports.contains_key(&VAXEE_REPORT_ID));
            if !has_feat_0e {
                continue;
            }

            let pid = dev.product_id();
            let fallback_name = vaxee_mouse_name(pid)
                .or_else(|| dev.product_string().filter(|s| !s.trim().is_empty()))
                .unwrap_or("VAXEE Mouse")
                .to_string();
            let mut device = VaxeeDevice {
                hid,
                pid,
                display_name: fallback_name,
                vid_pid: format!("{:04x}:{:04x}", VAXEE_VID, pid),
            };
            // Refine paired mouse model behind wireless receiver if awake.
            if let Ok(reply) = device.exchange(CMD_MOUSE_PID, 2, &[], 2, false) {
                let mouse_pid = u16::from_le_bytes([reply[5], reply[6]]);
                if let Some(name) = vaxee_mouse_name(mouse_pid) {
                    device.display_name = name.to_string();
                }
            }
            found.push(Box::new(device));
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_vaxee_requests_and_decodes_replies() {
        let req = build_vaxee_request(CMD_DPI_VALUE, 3, &[2, 0x40, 0x06], true);
        assert_eq!(req[0], VAXEE_REPORT_ID);
        assert_eq!(req[1], 0xA5);
        assert_eq!(req[2], CMD_DPI_VALUE);
        assert_eq!(req[3], 2); // write
        assert_eq!(req[4], 3);
        assert_eq!(&req[5..8], &[2, 0x40, 0x06]);

        let reply = [VAXEE_REPORT_ID, 0xA5, CMD_DPI_VALUE, 3, 1, 3, 2, 0x40, 0x06];
        let decoded = decode_vaxee_reply(&reply, CMD_DPI_VALUE, 3).unwrap();
        assert_eq!(u16::from_le_bytes([decoded[6], decoded[7]]), 1600);

        assert_eq!(
            decode_vaxee_polling_rate(encode_vaxee_polling_rate(4000).unwrap()),
            Some(4000)
        );
        assert_eq!(vaxee_mouse_name(0x1011), Some("VAXEE NP-01S V2 Wireless"));
    }
}
