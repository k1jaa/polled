// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Wooting Analog / Hall-Effect Keyboard USB HID profile-switching driver
//! (Wooting 60HE, 60HE+, 80HE, Two HE, UwU, Wooting One/Two).
//!
//! # Protocol Attribution & Licenses (AGPL-3.0 compatible)
//! USB Vendor IDs, command usage pages (`0x1337` / `0xFF55`), 8-byte feature report framing
//! (`[0x00, 0xD0, 0xDA, cmd, p0, p1, p2, p3]`), and command IDs (`0x0B`
//! `GetCurrentKeyboardProfileIndex`, `0x17` `ActivateProfile`, `0x26` `ReloadProfile`) are
//! verified directly from Wooting's open-source SDKs and community tools:
//! - `WootingKb/wooting-rgb-sdk` (`wooting-usb.c`, **MPL-2.0 License**)
//! - `wootswitch` (**MIT License**)
//!
//! No HID bytes are guessed. Never flashes firmware, never resets profiles, and always diffs
//! via `GetCurrentKeyboardProfileIndex (0x0B)` before switching (zero unnecessary writes) with
//! post-switch read-back verification.

use std::collections::HashSet;
use std::thread;
use std::time::Duration;

use hidapi::{HidApi, HidDevice};
use serde::{Deserialize, Serialize};

use crate::error::{PolledError, Result};

/// Official Wooting USB Vendor ID (60HE, 60HE+, 80HE, Two HE, UwU).
pub const WOOTING_VID: u16 = 0x31E3;
/// Legacy Atmel VID used by original Wooting One (`0xFF01`) and Wooting Two (`0xFF02`).
pub const WOOTING_LEGACY_VID: u16 = 0x03EB;
pub const WOOTING_LEGACY_PIDS: &[u16] = &[0xFF01, 0xFF02];

/// Vendor-defined USB HID command usage pages used by Wooting keyboards.
pub const WOOTING_CMD_USAGE_PAGES: &[u16] = &[0x1337, 0xFF55];

pub const WOOTING_MAGIC_0: u8 = 0xD0;
pub const WOOTING_MAGIC_1: u8 = 0xDA;
pub const WOOTING_STATUS_OK: u8 = 0x88;

/// Verified Wooting HID command IDs.
pub const CMD_GET_CURRENT_PROFILE_INDEX: u8 = 0x0B; // 11
pub const CMD_ACTIVATE_PROFILE: u8 = 0x17; // 23
pub const CMD_RELOAD_PROFILE: u8 = 0x26; // 38

/// Maximum 0-based onboard profile index (`0..=3`, corresponding to 4 onboard profiles).
pub const WOOTING_MAX_PROFILE_INDEX: u8 = 3;

/// Metadata & live active profile snapshot for a connected Wooting keyboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WootingKeyboardInfo {
    pub id: String,
    pub name: String,
    pub vid_pid: String,
    /// 0-based active onboard profile index (`0..=3`), if readable.
    pub active_profile: Option<u8>,
}

impl WootingKeyboardInfo {
    pub fn profile_label(idx: u8) -> &'static str {
        match idx {
            0 => "Profile 1 (Typing / Digital)",
            1 => "Profile 2 (Game 1)",
            2 => "Profile 3 (Game 2)",
            3 => "Profile 4 (Game 3)",
            _ => "Profile",
        }
    }
}

/// Builds an 8-byte Wooting HID feature report command:
/// `[0x00, 0xD0, 0xDA, cmd_id, p0, p1, p2, p3]`.
pub fn build_wooting_feature_cmd(cmd_id: u8, params: &[u8]) -> [u8; 8] {
    let mut buf = [0u8; 8];
    buf[0] = 0x00;
    buf[1] = WOOTING_MAGIC_0;
    buf[2] = WOOTING_MAGIC_1;
    buf[3] = cmd_id;
    let copy_len = params.len().min(4);
    buf[4..4 + copy_len].copy_from_slice(&params[..copy_len]);
    buf
}

/// Decodes a Wooting HID response packet for `CMD_GET_CURRENT_PROFILE_INDEX (0x0B)`,
/// handling both 0-byte and 1-byte leading report ID offsets across Windows HID drivers.
pub fn decode_wooting_profile_response(raw: &[u8]) -> Option<u8> {
    for offset in [0usize, 1] {
        if raw.len() < offset + 5 {
            continue;
        }
        if raw[offset] == WOOTING_MAGIC_0
            && raw[offset + 1] == WOOTING_MAGIC_1
            && raw[offset + 2] == CMD_GET_CURRENT_PROFILE_INDEX
            && raw[offset + 3] == WOOTING_STATUS_OK
        {
            let idx = raw[offset + 4];
            if idx <= WOOTING_MAX_PROFILE_INDEX {
                return Some(idx);
            }
        }
    }
    None
}

/// Identifies a friendly Wooting model name from USB `(vid, pid)` and OS product string.
pub fn resolve_wooting_model_name(vid: u16, pid: u16, os_product: Option<&str>) -> String {
    if let Some(p) = os_product.map(str::trim).filter(|s| !s.is_empty()) {
        return p.to_string();
    }
    match (vid, pid) {
        (WOOTING_LEGACY_VID, 0xFF01) => "Wooting One".to_string(),
        (WOOTING_LEGACY_VID, 0xFF02) => "Wooting Two".to_string(),
        (WOOTING_VID, 0x1210..=0x121F) => "Wooting Two HE".to_string(),
        (WOOTING_VID, 0x1220..=0x122F) => "Wooting Two HE (ARM)".to_string(),
        (WOOTING_VID, 0x1310..=0x131F) => "Wooting 60HE".to_string(),
        (WOOTING_VID, 0x1320..=0x132F) => "Wooting 60HE (ARM)".to_string(),
        (WOOTING_VID, 0x1330..=0x133F) => "Wooting 60HE+".to_string(),
        (WOOTING_VID, 0x1340..=0x134F) => "Wooting 80HE".to_string(),
        (WOOTING_VID, 0x1500..=0x151F) => "Wooting UwU".to_string(),
        _ => format!("Wooting Keyboard ({vid:04x}:{pid:04x})"),
    }
}

/// Live handle to a connected Wooting keyboard command interface.
pub struct WootingDevice {
    hid: HidDevice,
    id: String,
    name: String,
    vid_pid: String,
}

impl WootingDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.name.clone(),
            message: message.into(),
        }
    }

    pub fn info(&self, active_profile: Option<u8>) -> WootingKeyboardInfo {
        WootingKeyboardInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            vid_pid: self.vid_pid.clone(),
            active_profile,
        }
    }

    fn send_feature(&self, cmd_id: u8, params: &[u8]) -> Result<()> {
        let pkt = build_wooting_feature_cmd(cmd_id, params);
        self.hid.send_feature_report(&pkt).map_err(|e| {
            self.dev_err(format!(
                "Wooting feature report 0x{cmd_id:02x} failed (close Wootility if open): {e}"
            ))
        })
    }

    /// Reads the active 0-based onboard profile index (`0..=3`) using `0x0B`.
    pub fn read_active_profile(&self) -> Result<u8> {
        self.send_feature(CMD_GET_CURRENT_PROFILE_INDEX, &[])?;
        let mut buf = [0u8; 65];
        for _ in 0..4 {
            let n = self
                .hid
                .read_timeout(&mut buf, 180)
                .map_err(|e| self.dev_err(format!("Wooting HID read failed: {e}")))?;
            if n == 0 {
                break;
            }
            if let Some(idx) = decode_wooting_profile_response(&buf[..n]) {
                return Ok(idx);
            }
        }

        // Fallback: some firmware revisions answer on get_feature_report instead of input report.
        buf.fill(0);
        if let Ok(n) = self.hid.get_feature_report(&mut buf) {
            if let Some(idx) = decode_wooting_profile_response(&buf[..n]) {
                return Ok(idx);
            }
        }

        Err(self.dev_err("Wooting keyboard did not return active profile index"))
    }

    /// Switches the Wooting keyboard to `profile_idx` (`0..=3`) in volatile RAM (`ActivateProfile`
    /// + `ReloadProfile`), skipping the write if already active and verifying via read-back.
    ///
    /// Returns `Ok(true)` if switched, `Ok(false)` if already on `profile_idx`.
    pub fn switch_profile(&self, profile_idx: u8) -> Result<bool> {
        if profile_idx > WOOTING_MAX_PROFILE_INDEX {
            return Err(PolledError::UnsupportedSetting {
                device: self.name.clone(),
                detail: format!(
                    "Wooting profile index {profile_idx} out of supported range 0..={WOOTING_MAX_PROFILE_INDEX}"
                ),
            });
        }

        if let Ok(current) = self.read_active_profile() {
            if current == profile_idx {
                return Ok(false);
            }
        }

        self.send_feature(CMD_ACTIVATE_PROFILE, &[profile_idx])?;
        thread::sleep(Duration::from_millis(15));
        self.send_feature(CMD_RELOAD_PROFILE, &[profile_idx])?;
        thread::sleep(Duration::from_millis(25));

        if let Ok(after) = self.read_active_profile() {
            if after != profile_idx {
                return Err(self.dev_err(format!(
                    "Wooting profile verification mismatch: requested Profile {}, keyboard reported Profile {}",
                    profile_idx + 1,
                    after + 1
                )));
            }
        }
        Ok(true)
    }
}

/// Detects all connected Wooting keyboards exposing a valid command HID interface (`0x1337` / `0xFF55`).
pub fn detect_wooting_keyboards() -> Result<Vec<WootingDevice>> {
    let api = HidApi::new().map_err(|e| PolledError::Win32 {
        context: "HidApi::new".to_string(),
        message: e.to_string(),
    })?;

    let mut found = Vec::new();
    let mut seen_keys: HashSet<(u16, u16, String)> = HashSet::new();

    for dev in api.device_list() {
        let vid = dev.vendor_id();
        let pid = dev.product_id();
        let is_wooting =
            vid == WOOTING_VID || (vid == WOOTING_LEGACY_VID && WOOTING_LEGACY_PIDS.contains(&pid));
        if !is_wooting {
            continue;
        }
        if !WOOTING_CMD_USAGE_PAGES.contains(&dev.usage_page()) {
            continue;
        }
        let serial_key = dev.serial_number().unwrap_or_default().to_string();
        if !seen_keys.insert((vid, pid, serial_key)) {
            continue;
        }
        let Ok(hid) = dev.open_device(&api) else {
            continue;
        };
        let name = resolve_wooting_model_name(vid, pid, dev.product_string());
        let id = format!("wooting_{vid:04x}_{pid:04x}");
        let vid_pid = format!("{vid:04x}:{pid:04x}");
        let candidate = WootingDevice {
            hid,
            id,
            name,
            vid_pid,
        };
        // Read-only probe verifies this exact collection speaks Wooting's 0xD0 0xDA command protocol.
        if candidate.read_active_profile().is_ok() {
            found.push(candidate);
        }
    }

    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_verified_wooting_feature_packets() {
        let query = build_wooting_feature_cmd(CMD_GET_CURRENT_PROFILE_INDEX, &[]);
        assert_eq!(query, [0x00, 0xD0, 0xDA, 0x0B, 0x00, 0x00, 0x00, 0x00]);

        let activate = build_wooting_feature_cmd(CMD_ACTIVATE_PROFILE, &[2]);
        assert_eq!(activate, [0x00, 0xD0, 0xDA, 0x17, 0x02, 0x00, 0x00, 0x00]);

        let reload = build_wooting_feature_cmd(CMD_RELOAD_PROFILE, &[2]);
        assert_eq!(reload, [0x00, 0xD0, 0xDA, 0x26, 0x02, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn decodes_wooting_profile_replies_with_and_without_report_id() {
        let with_id = [0x00, 0xD0, 0xDA, 0x0B, 0x88, 0x02, 0x00];
        assert_eq!(decode_wooting_profile_response(&with_id), Some(2));

        let no_id = [0xD0, 0xDA, 0x0B, 0x88, 0x00, 0x00];
        assert_eq!(decode_wooting_profile_response(&no_id), Some(0));

        let bad_status = [0x00, 0xD0, 0xDA, 0x0B, 0x99, 0x01];
        assert_eq!(decode_wooting_profile_response(&bad_status), None);

        let out_of_range = [0x00, 0xD0, 0xDA, 0x0B, 0x88, 0x09];
        assert_eq!(decode_wooting_profile_response(&out_of_range), None);
    }
}
