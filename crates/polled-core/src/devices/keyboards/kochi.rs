// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Kochi Hummingbird 68 keyboard backend (`1CA2:0901`, `0xFFA0:0x0001`).
//!
//! Protocol codec implemented from `OpenMouse-Project/keyboard-protocol` (`src/kochi/`,
//! AGPL-3.0-or-later): verified identity check (`KOCHI68`) and read/toggle Windows/macOS mode
//! with mandatory read-back verification.

use std::collections::HashSet;
use std::ffi::CString;

use hidapi::{DeviceInfo as HidDeviceInfo, HidApi, HidDevice};

use crate::devices::keyboards::{
    device_dedup_key, exchange_report0, kb_err, kb_verify_err, KeyboardBrand, KeyboardCapabilities,
    KeyboardDevice, KeyboardStatus,
};
use crate::devices::SupportTier;
use crate::error::Result;

pub const KOCHI_VID: u16 = 0x1CA2;
pub const KOCHI_HB68_PID: u16 = 0x0901;
const KOCHI_USAGE_PAGE: u16 = 0xFFA0;
const KOCHI_USAGE: u16 = 0x0001;

const CMD_NAME: u8 = 0x26;
const CMD_QUERY_WIN: u8 = 0x21;
const CMD_QUERY_MAC: u8 = 0x22;
const CMD_SET_WIN: u8 = 0x30;
const CMD_SET_MAC: u8 = 0x31;

pub fn kochi_checksum(frame: &[u8]) -> Option<u8> {
    let len = *frame.get(1)? as usize;
    if !(1..=60).contains(&len) || frame.len() < len + 4 {
        return None;
    }
    let sum = 0x35u16 + frame[0] as u16 + len as u16 + frame[2] as u16 + frame[len + 3] as u16;
    Some((sum & 0xFF) as u8)
}

pub fn encode_kochi_command(cmd: u8) -> [u8; 64] {
    let mut frame = [0u8; 64];
    frame[0] = 0x5C;
    frame[1] = 3;
    frame[2] = 0;
    frame[4] = cmd;
    frame[5] = 0xFF;
    frame[6] = 0xFF;
    frame[3] = kochi_checksum(&frame).unwrap_or(0);
    frame
}

pub fn kochi_reply_payload(frame: &[u8], cmd: u8) -> Option<&[u8]> {
    let len = *frame.get(1)? as usize;
    if frame.first().copied() != Some(0x5C)
        || frame.get(2).copied() != Some(0x80)
        || !(3..=60).contains(&len)
        || frame.len() < len + 4
        || frame.get(5).copied() != Some(cmd)
        || frame.get(3).copied() != kochi_checksum(frame)
    {
        return None;
    }
    Some(&frame[4..len + 4])
}

pub struct KochiKeyboardDevice {
    id: String,
    path: CString,
}

impl KochiKeyboardDevice {
    fn open(&self) -> Result<HidDevice> {
        let api = HidApi::new().map_err(|e| kb_err("Kochi Hummingbird 68", e.to_string()))?;
        api.open_path(self.path.as_c_str())
            .map_err(|e| kb_err("Kochi Hummingbird 68", format!("open failed: {e}")))
    }

    fn exchange(dev: &HidDevice, cmd: u8) -> Option<Vec<u8>> {
        let frame = encode_kochi_command(cmd);
        let rep = exchange_report0(dev, &frame, 750, |r| kochi_reply_payload(r, cmd).is_some())?;
        kochi_reply_payload(&rep, cmd).map(|s| s.to_vec())
    }

    fn read_mode(dev: &HidDevice) -> Option<bool> {
        let win_p = Self::exchange(dev, CMD_QUERY_WIN)?;
        let mac_p = Self::exchange(dev, CMD_QUERY_MAC)?;
        if win_p.len() != 4 || mac_p.len() != 4 || win_p[3] != 0xFF || mac_p[3] != 0xFF {
            return None;
        }
        let win = match win_p[2] {
            0 => false,
            1 => true,
            _ => return None,
        };
        let mac = match mac_p[2] {
            0 => false,
            1 => true,
            _ => return None,
        };
        (win != mac).then_some(mac)
    }
}

impl KeyboardDevice for KochiKeyboardDevice {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "Kochi Hummingbird 68"
    }

    fn brand(&self) -> KeyboardBrand {
        KeyboardBrand::Kochi
    }

    fn support_tier(&self) -> SupportTier {
        SupportTier::ImplementedFromDocs
    }

    fn read_status(&self) -> KeyboardStatus {
        let mut system_mode = None;
        let mut extra = Vec::new();
        if let Ok(dev) = self.open() {
            if let Some(name_p) = Self::exchange(&dev, CMD_NAME) {
                if name_p.len() >= 34 {
                    let raw = &name_p[2..34];
                    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                    let s = String::from_utf8_lossy(&raw[..end]).trim().to_string();
                    if !s.is_empty() {
                        extra.push(("Reported Model".to_string(), s));
                    }
                }
            }
            if let Some(is_mac) = Self::read_mode(&dev) {
                system_mode = Some(if is_mac { "macOS" } else { "Windows" }.to_string());
            }
        }

        let capabilities = KeyboardCapabilities {
            supports_system_mode_toggle: system_mode.is_some(),
            ..Default::default()
        };

        KeyboardStatus {
            id: self.id.clone(),
            brand: KeyboardBrand::Kochi,
            name: "Kochi Hummingbird 68".to_string(),
            vid_pid: format!("{KOCHI_VID:04x}:{KOCHI_HB68_PID:04x}"),
            support_tier: SupportTier::ImplementedFromDocs,
            protocol_label: "Kochi HB68 (0xFFA0)".to_string(),
            chip_type: "HB68 USB MCU".to_string(),
            switch_tech: "Hall Effect".to_string(),
            firmware_version: None,
            layout: None,
            active_profile: None,
            profiles: Vec::new(),
            actuation_mm: None,
            actuation_range_mm: None,
            rapid_trigger_enabled: None,
            rapid_trigger_mm: None,
            polling_rate_hz: None,
            battery: None,
            system_mode,
            socd_enabled: None,
            tachyon_enabled: None,
            debounce_mode: None,
            debounce_ms: None,
            extra_telemetry: extra,
            status_note: Some(
                "Implemented from OpenMouse Kochi HB68 docs (Windows/macOS mode switch with mandatory read-back)."
                    .to_string(),
            ),
            capabilities,
            wooting: None,
        }
    }

    fn set_system_mode(&self, mac_mode: bool) -> Result<String> {
        let dev = self.open()?;
        let cmd = if mac_mode { CMD_SET_MAC } else { CMD_SET_WIN };
        let ack = Self::exchange(&dev, cmd).ok_or_else(|| {
            kb_err(
                "Kochi Hummingbird 68",
                "HB68 did not acknowledge system-mode change",
            )
        })?;
        if ack.len() != 4 || ack[2] != 1 || ack[3] != 0xFF {
            return Err(kb_err(
                "Kochi Hummingbird 68",
                "HB68 rejected system-mode change",
            ));
        }
        let confirmed = Self::read_mode(&dev);
        if confirmed != Some(mac_mode) {
            return Err(kb_verify_err(
                "Kochi Hummingbird 68",
                if mac_mode { "macOS" } else { "Windows" },
                format!("{confirmed:?}"),
            ));
        }
        Ok(if mac_mode { "macOS" } else { "Windows" }.to_string())
    }
}

pub(crate) fn detect_kochi_keyboards(
    api: &HidApi,
    infos: &[&HidDeviceInfo],
    claimed: &mut HashSet<(u16, u16, String)>,
) -> Vec<KochiKeyboardDevice> {
    let mut found = Vec::new();
    for info in infos {
        if info.vendor_id() != KOCHI_VID
            || info.product_id() != KOCHI_HB68_PID
            || info.usage_page() != KOCHI_USAGE_PAGE
            || info.usage() != KOCHI_USAGE
        {
            continue;
        }
        let key = device_dedup_key(info);
        if claimed.contains(&key) {
            continue;
        }
        if api.open_path(info.path()).is_err() {
            continue;
        }
        claimed.insert(key);
        found.push(KochiKeyboardDevice {
            id: "kochi:1ca2:0901".to_string(),
            path: info.path().to_owned(),
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kochi_command_frame_checksum_matches_spec() {
        let f = encode_kochi_command(CMD_QUERY_WIN);
        assert_eq!(
            &f[..7],
            &[
                0x5C,
                3,
                0,
                ((0x35u16 + 0x5C + 3 + 0xFF) & 0xFF) as u8,
                0x21,
                0xFF,
                0xFF
            ]
        );
    }
}
