// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Arbiter Studio Hall-Effect keyboard backend (`0x1A81` / `0x371C`, `0xFF01` / `0xFF02`).
//!
//! Protocol codec implemented from `OpenMouse-Project/keyboard-protocol` (`src/arbiter/`,
//! AGPL-3.0-or-later). Covers Polar 65, Polar 65+, Polar 65 PRO, Polar 75 PRO, and Nimbus 65.

use std::collections::HashSet;
use std::ffi::CString;

use hidapi::{DeviceInfo as HidDeviceInfo, HidApi, HidDevice};

use crate::devices::keyboards::{
    device_dedup_key, exchange_report0, kb_err, kb_unsupported, kb_verify_err, KeyboardBrand,
    KeyboardCapabilities, KeyboardDevice, KeyboardProfileSlot, KeyboardStatus,
};
use crate::devices::SupportTier;
use crate::error::Result;

pub const ARBITER_VID: u16 = 0x1A81;
pub const ARBITER_VID_ALT: u16 = 0x371C;
const ARBITER_USAGE: u16 = 0x0001;
const KEYS_PER_PACKET: usize = 29;

const CMD_SET_TRAVEL: u8 = 0;
const CMD_GET_TRAVEL: u8 = 1;
const CMD_GET_FIRMWARE: u8 = 2;
const CMD_SET_BOARD_PROFILE: u8 = 38;
const CMD_SET_USER_MODE: u8 = 52;
const CMD_GET_USER_MODE: u8 = 53;

const PKT_SHORT: u8 = 0x00;
const PKT_LONG: u8 = 0x40;

#[derive(Debug, Clone, Copy)]
pub struct ArbiterProductSpec {
    pub name: &'static str,
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub travel_keys: usize,
    pub profiles: u8,
    pub super_tap: bool,
}

pub const ARBITER_PRODUCTS: &[ArbiterProductSpec] = &[
    ArbiterProductSpec {
        name: "Arbiter Polar 65",
        vid: ARBITER_VID,
        pid: 0x2072,
        usage_page: 0xFF01,
        travel_keys: 68,
        profiles: 1,
        super_tap: false,
    },
    ArbiterProductSpec {
        name: "Arbiter Polar 65",
        vid: ARBITER_VID,
        pid: 0x2072,
        usage_page: 0xFF02,
        travel_keys: 68,
        profiles: 1,
        super_tap: true,
    },
    ArbiterProductSpec {
        name: "Arbiter Polar 65",
        vid: ARBITER_VID,
        pid: 0x208E,
        usage_page: 0xFF02,
        travel_keys: 68,
        profiles: 1,
        super_tap: true,
    },
    ArbiterProductSpec {
        name: "Arbiter Nimbus 65",
        vid: ARBITER_VID,
        pid: 0x2077,
        usage_page: 0xFF01,
        travel_keys: 68,
        profiles: 1,
        super_tap: false,
    },
    ArbiterProductSpec {
        name: "Arbiter Polar 65",
        vid: ARBITER_VID_ALT,
        pid: 0xA005,
        usage_page: 0xFF02,
        travel_keys: 68,
        profiles: 1,
        super_tap: true,
    },
    ArbiterProductSpec {
        name: "Arbiter Polar 65",
        vid: ARBITER_VID_ALT,
        pid: 0xA006,
        usage_page: 0xFF01,
        travel_keys: 69,
        profiles: 1,
        super_tap: true,
    },
    ArbiterProductSpec {
        name: "Arbiter Polar 65 PRO",
        vid: ARBITER_VID_ALT,
        pid: 0xA003,
        usage_page: 0xFF02,
        travel_keys: 68,
        profiles: 3,
        super_tap: true,
    },
    ArbiterProductSpec {
        name: "Arbiter Polar 65+",
        vid: ARBITER_VID_ALT,
        pid: 0xA009,
        usage_page: 0xFF02,
        travel_keys: 68,
        profiles: 3,
        super_tap: true,
    },
    ArbiterProductSpec {
        name: "Arbiter Polar 75 PRO",
        vid: ARBITER_VID_ALT,
        pid: 0xA001,
        usage_page: 0xFF02,
        travel_keys: 82,
        profiles: 3,
        super_tap: true,
    },
];

pub fn lookup_arbiter_product(vid: u16, pid: u16, usage_page: u16) -> Option<ArbiterProductSpec> {
    ARBITER_PRODUCTS
        .iter()
        .copied()
        .find(|p| p.vid == vid && p.pid == pid && p.usage_page == usage_page)
}

/// Arbiter checksum: `(85 - sum(bytes 0..63)) & 0xFF`.
pub fn arbiter_checksum(frame: &[u8; 64]) -> u8 {
    let sum: u32 = frame[..63].iter().map(|&b| b as u32).sum();
    (85u32.wrapping_sub(sum) & 0xFF) as u8
}

pub fn encode_arbiter_request(command: u8, packet_type: u8, data: &[u8]) -> [u8; 64] {
    let mut frame = [0u8; 64];
    frame[1] = command | packet_type;
    let n = data.len().min(59);
    frame[3] = n as u8;
    frame[4..4 + n].copy_from_slice(&data[..n]);
    frame[63] = arbiter_checksum(&frame);
    frame
}

pub fn decode_arbiter_reply(frame: &[u8]) -> Option<(u8, u8, Vec<u8>)> {
    if frame.len() < 64 {
        return None;
    }
    let arr: [u8; 64] = frame[..64].try_into().ok()?;
    if arr[63] != arbiter_checksum(&arr) {
        return None;
    }
    let size = if arr[3] == 0xFF { 0 } else { arr[3] as usize };
    if size > 59 {
        return None;
    }
    Some((arr[1] & 0x3F, arr[2], arr[4..4 + size].to_vec()))
}

pub struct ArbiterKeyboardDevice {
    id: String,
    spec: ArbiterProductSpec,
    path: CString,
}

impl ArbiterKeyboardDevice {
    fn open(&self) -> Result<HidDevice> {
        let api = HidApi::new().map_err(|e| kb_err(self.spec.name, e.to_string()))?;
        api.open_path(self.path.as_c_str())
            .map_err(|e| kb_err(self.spec.name, format!("open failed: {e}")))
    }

    fn exchange(dev: &HidDevice, command: u8, pkt_type: u8, data: &[u8]) -> Option<Vec<u8>> {
        let frame = encode_arbiter_request(command, pkt_type, data);
        let raw = exchange_report0(dev, &frame, 750, |r| {
            decode_arbiter_reply(r).is_some_and(|(cmd, _, _)| cmd == command)
        })?;
        let (_, status, payload) = decode_arbiter_reply(&raw)?;
        (status == 0).then_some(payload)
    }

    fn read_travel_table(&self, dev: &HidDevice) -> Option<Vec<(u8, u8)>> {
        let total = self.spec.travel_keys.div_ceil(KEYS_PER_PACKET);
        let mut out = Vec::with_capacity(self.spec.travel_keys);
        for idx in 0..total {
            let hdr = (idx as u8) | ((total as u8) << 4);
            let payload = Self::exchange(dev, CMD_GET_TRAVEL, PKT_LONG, &[hdr])?;
            if payload.is_empty() || payload.len() % 2 != 1 {
                return None;
            }
            if (payload[0] & 0x0F) as usize != idx {
                return None;
            }
            for pair in payload[1..].as_chunks::<2>().0 {
                out.push((pair[0], pair[1]));
            }
        }
        out.truncate(self.spec.travel_keys);
        Some(out)
    }
}

impl KeyboardDevice for ArbiterKeyboardDevice {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        self.spec.name
    }

    fn brand(&self) -> KeyboardBrand {
        KeyboardBrand::Arbiter
    }

    fn support_tier(&self) -> SupportTier {
        SupportTier::ImplementedFromDocs
    }

    fn read_status(&self) -> KeyboardStatus {
        let mut firmware_version = None;
        let mut actuation_mm = None;
        let mut rapid_trigger_mm = None;
        let mut socd_enabled = None;
        let mut extra = Vec::new();

        if let Ok(dev) = self.open() {
            if let Some(p) = Self::exchange(&dev, CMD_GET_FIRMWARE, PKT_SHORT, &[]) {
                if p.len() >= 8 {
                    firmware_version = Some(format!("V{:02x}{:02x}", p[7], p[0]));
                }
            }
            if let Some(travel) = self.read_travel_table(&dev) {
                if let Some(&(make, brk)) = travel.first() {
                    if (1..=38).contains(&make) {
                        actuation_mm = Some(make as f32 / 10.0);
                    }
                    if (1..=24).contains(&brk) {
                        rapid_trigger_mm = Some(brk as f32 / 10.0);
                    }
                }
                extra.push(("Analog Keys".to_string(), travel.len().to_string()));
            }
            if self.spec.super_tap {
                if let Some(um) = Self::exchange(&dev, CMD_GET_USER_MODE, PKT_SHORT, &[]) {
                    if um.len() == 6 {
                        socd_enabled = Some(um[2] > 0);
                    }
                }
            }
        }

        let profiles = if self.spec.profiles > 1 {
            (0..self.spec.profiles)
                .map(|idx| KeyboardProfileSlot {
                    index: idx,
                    name: format!("Profile {}", idx + 1),
                    is_active: false,
                })
                .collect()
        } else {
            Vec::new()
        };

        let capabilities = KeyboardCapabilities {
            supports_profile_switch: self.spec.profiles > 1,
            profile_switch_volatile: false,
            supports_base_actuation_write: true,
            supports_socd_toggle: self.spec.super_tap,
            socd_label: self.spec.super_tap.then(|| "SuperTap (SOCD)".to_string()),
            ..Default::default()
        };

        KeyboardStatus {
            id: self.id.clone(),
            brand: KeyboardBrand::Arbiter,
            name: self.spec.name.to_string(),
            vid_pid: format!("{:04x}:{:04x}", self.spec.vid, self.spec.pid),
            support_tier: SupportTier::ImplementedFromDocs,
            protocol_label: format!("Arbiter 0x{:04X}", self.spec.usage_page),
            chip_type: "Arbiter HE MCU".to_string(),
            switch_tech: "Fuji / Hall Effect".to_string(),
            firmware_version,
            layout: None,
            active_profile: None,
            profiles,
            actuation_mm,
            actuation_range_mm: Some((0.1, 3.8)),
            rapid_trigger_enabled: rapid_trigger_mm.map(|_| true),
            rapid_trigger_mm,
            polling_rate_hz: None,
            battery: None,
            system_mode: None,
            socd_enabled,
            tachyon_enabled: None,
            debounce_mode: None,
            debounce_ms: None,
            extra_telemetry: extra,
            status_note: Some(
                "Implemented from OpenMouse Arbiter Studio protocol docs; writes are confirm-gated and verified by read-back."
                    .to_string(),
            ),
            capabilities,
            wooting: None,
        }
    }

    fn switch_profile(&self, slot: u8) -> Result<bool> {
        if slot >= self.spec.profiles {
            return Err(kb_err(
                self.spec.name,
                format!("Profile {slot} out of range (0..{})", self.spec.profiles),
            ));
        }
        let dev = self.open()?;
        Self::exchange(&dev, CMD_SET_BOARD_PROFILE, PKT_SHORT, &[slot]).ok_or_else(|| {
            kb_err(
                self.spec.name,
                "Keyboard did not acknowledge profile switch",
            )
        })?;
        Ok(true)
    }

    fn set_base_actuation(
        &self,
        actuation_mm: f32,
        _rt_enabled: Option<bool>,
        rt_mm: Option<f32>,
    ) -> Result<()> {
        let make = (actuation_mm * 10.0).round().clamp(1.0, 38.0) as u8;
        let dev = self.open()?;
        let mut table = self
            .read_travel_table(&dev)
            .ok_or_else(|| kb_err(self.spec.name, "Could not read existing travel table"))?;
        let brk_override = rt_mm.map(|v| (v * 10.0).round().clamp(1.0, 24.0) as u8);
        for entry in &mut table {
            entry.0 = make;
            if let Some(b) = brk_override {
                entry.1 = b;
            }
        }
        let total = table.len().div_ceil(KEYS_PER_PACKET);
        for (idx, chunk) in table.chunks(KEYS_PER_PACKET).enumerate() {
            let mut data = Vec::with_capacity(1 + chunk.len() * 2);
            data.push((idx as u8) | ((total as u8) << 4));
            for &(m, b) in chunk {
                data.push(m);
                data.push(b);
            }
            Self::exchange(&dev, CMD_SET_TRAVEL, PKT_LONG, &data).ok_or_else(|| {
                kb_err(
                    self.spec.name,
                    format!("Keyboard did not acknowledge travel packet {idx}"),
                )
            })?;
        }
        let confirmed = self.read_travel_table(&dev).ok_or_else(|| {
            kb_err(
                self.spec.name,
                "Could not read back travel table after write",
            )
        })?;
        if confirmed != table {
            return Err(kb_verify_err(
                self.spec.name,
                format!("{:.1} mm across {} keys", make as f32 / 10.0, table.len()),
                "Travel read-back mismatch",
            ));
        }
        Ok(())
    }

    fn set_socd(&self, enabled: bool) -> Result<bool> {
        if !self.spec.super_tap {
            return Err(kb_unsupported(self.spec.name, "SuperTap"));
        }
        let dev = self.open()?;
        let mut um = Self::exchange(&dev, CMD_GET_USER_MODE, PKT_SHORT, &[])
            .ok_or_else(|| kb_err(self.spec.name, "Could not read current SuperTap state"))?;
        if um.len() != 6 {
            return Err(kb_err(self.spec.name, "Unexpected SuperTap payload length"));
        }
        um[2] = u8::from(enabled);
        Self::exchange(&dev, CMD_SET_USER_MODE, PKT_SHORT, &um).ok_or_else(|| {
            kb_err(
                self.spec.name,
                "Keyboard did not acknowledge SuperTap update",
            )
        })?;
        let after = Self::exchange(&dev, CMD_GET_USER_MODE, PKT_SHORT, &[]);
        if after.as_deref() != Some(um.as_slice()) {
            return Err(kb_verify_err(
                self.spec.name,
                if enabled { "ON" } else { "OFF" },
                format!("{after:?}"),
            ));
        }
        Ok(enabled)
    }
}

pub(crate) fn detect_arbiter_keyboards(
    api: &HidApi,
    infos: &[&HidDeviceInfo],
    claimed: &mut HashSet<(u16, u16, String)>,
) -> Vec<ArbiterKeyboardDevice> {
    let mut found = Vec::new();
    for info in infos {
        if info.usage() != ARBITER_USAGE {
            continue;
        }
        let Some(spec) =
            lookup_arbiter_product(info.vendor_id(), info.product_id(), info.usage_page())
        else {
            continue;
        };
        let key = device_dedup_key(info);
        if claimed.contains(&key) {
            continue;
        }
        if api.open_path(info.path()).is_err() {
            continue;
        }
        claimed.insert(key);
        found.push(ArbiterKeyboardDevice {
            id: format!("arbiter:{:04x}:{:04x}", spec.vid, spec.pid),
            spec,
            path: info.path().to_owned(),
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbiter_checksum_and_roundtrip_match_spec() {
        let frame = encode_arbiter_request(CMD_GET_FIRMWARE, PKT_SHORT, &[]);
        assert_eq!(frame[1], CMD_GET_FIRMWARE);
        assert_eq!(frame[63], (85u8).wrapping_sub(CMD_GET_FIRMWARE));
        let (cmd, status, data) = decode_arbiter_reply(&frame).unwrap();
        assert_eq!(cmd, CMD_GET_FIRMWARE);
        assert_eq!(status, 0);
        assert!(data.is_empty());
    }
}
