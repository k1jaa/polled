// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! SteelSeries Apex Pro keyboard backend (`0x1038`, `0xFFC0:0x0001`).
//!
//! Protocol codec implemented from `OpenMouse-Project/keyboard-protocol` (`src/steelseries/`,
//! AGPL-3.0-or-later):
//! - Wired Apex Pro TKL (2023) `0x1628` & Apex Pro TKL Gen 3 `0x1642` (on-board profile blob
//!   read + STM32 CRC-32 validation + live hall actuation + Rapid Trigger + Rapid Tap toggle)
//! - Apex Pro TKL Wireless (2023) `0x1630` / `0x1632` (identity & firmware telemetry)

use std::collections::HashSet;
use std::ffi::CString;
use std::thread;
use std::time::Duration;

use hidapi::{DeviceInfo as HidDeviceInfo, HidApi, HidDevice};

use crate::devices::keyboards::{
    device_dedup_key, exchange_report0, feature_exchange_report0, kb_err, kb_unsupported,
    KeyboardBrand, KeyboardCapabilities, KeyboardDevice, KeyboardStatus,
};
use crate::devices::SupportTier;
use crate::error::Result;

pub const STEELSERIES_VID: u16 = 0x1038;
pub const STEELSERIES_USAGE_PAGE: u16 = 0xFFC0;
pub const STEELSERIES_USAGE: u16 = 0x0001;

const FEATURE_LEN: usize = 644;
const PROFILE_LEN: usize = 12_500;
const CHUNK_LEN: usize = 500;
const CHECKSUM_OFFSET: usize = 12_320;
const FIRST_PROFILE_FS: u8 = 0x80;

const OFFSET_DEVICE_SCHEMA: usize = 1080;
const OFFSET_ACTUATION: usize = 1084;
const OFFSET_RESET: usize = 1154;
const OFFSET_RELEASE_MODE: usize = 1446;
const OFFSET_RAPID_TAP_ENABLED: usize = 2700;

/// 68 analog key HID usages in SteelSeries GG's table order (wired Apex Pro 2023 / Gen 3).
pub const APEX_PRO_ANALOG_SLOTS: [u8; 68] = [
    0x35, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x2D, 0x2E, 0x89, 0x2B, 0x14,
    0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13, 0x2F, 0x30, 0x31, 0x39, 0x04, 0x16, 0x07,
    0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F, 0x33, 0x34, 0x32, 0x28, 0xE1, 0x64, 0x1D, 0x1B, 0x06, 0x19,
    0x05, 0x11, 0x10, 0x36, 0x37, 0x38, 0x87, 0xE5, 0xE0, 0xE3, 0xE2, 0x8B, 0x2C, 0x8A, 0x88, 0xE6,
    0xE7, 0xF0, 0xE4, 0x2A,
];

/// GG's `(actuation, reset)` raw thresholds for 0.1–4.0 mm (index = tenths - 1).
pub const ACTUATION_POINTS: [(u8, u8); 40] = [
    (5, 5),
    (5, 4),
    (6, 5),
    (8, 6),
    (10, 8),
    (12, 10),
    (14, 12),
    (16, 14),
    (18, 16),
    (21, 18),
    (24, 21),
    (27, 24),
    (30, 27),
    (33, 29),
    (36, 33),
    (39, 36),
    (43, 40),
    (47, 42),
    (52, 47),
    (57, 52),
    (62, 57),
    (68, 62),
    (74, 68),
    (81, 74),
    (88, 81),
    (95, 87),
    (104, 95),
    (114, 104),
    (124, 114),
    (134, 123),
    (147, 134),
    (161, 147),
    (175, 161),
    (189, 174),
    (197, 189),
    (206, 197),
    (215, 206),
    (224, 215),
    (224, 217),
    (224, 219),
];

#[derive(Debug, Clone, Copy)]
pub struct SteelSeriesProductSpec {
    pub pid: u16,
    pub name: &'static str,
    pub wired_profile_support: bool,
}

pub const STEELSERIES_PRODUCTS: &[SteelSeriesProductSpec] = &[
    SteelSeriesProductSpec {
        pid: 0x1628,
        name: "SteelSeries Apex Pro TKL (2023)",
        wired_profile_support: true,
    },
    SteelSeriesProductSpec {
        pid: 0x1642,
        name: "SteelSeries Apex Pro TKL Gen 3",
        wired_profile_support: true,
    },
    SteelSeriesProductSpec {
        pid: 0x1630,
        name: "SteelSeries Apex Pro TKL Wireless (2023)",
        wired_profile_support: false,
    },
    SteelSeriesProductSpec {
        pid: 0x1632,
        name: "SteelSeries Apex Pro TKL Wireless (USB)",
        wired_profile_support: false,
    },
];

pub fn lookup_steelseries_kb(pid: u16) -> Option<SteelSeriesProductSpec> {
    STEELSERIES_PRODUCTS.iter().copied().find(|p| p.pid == pid)
}

/// STM32 hardware CRC-32 (poly `0x04C11DB7`, init `0xFFFFFFFF`, LE 32-bit words MSB-first).
pub fn steelseries_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for chunk in data.as_chunks::<4>().0 {
        for &byte in chunk.iter().rev() {
            crc ^= (byte as u32) << 24;
            for _ in 0..8 {
                crc = if (crc & 0x8000_0000) != 0 {
                    (crc << 1) ^ 0x04C1_1DB7
                } else {
                    crc << 1
                };
            }
        }
    }
    crc
}

pub fn profile_checksum_valid(blob: &[u8]) -> bool {
    if blob.len() < PROFILE_LEN {
        return false;
    }
    let stored = u32::from_le_bytes([
        blob[CHECKSUM_OFFSET],
        blob[CHECKSUM_OFFSET + 1],
        blob[CHECKSUM_OFFSET + 2],
        blob[CHECKSUM_OFFSET + 3],
    ]);
    stored == steelseries_crc32(&blob[..CHECKSUM_OFFSET])
}

pub fn actuation_raw_to_mm(act: u8, rst: u8) -> Option<f32> {
    ACTUATION_POINTS
        .iter()
        .position(|&(h, l)| h == act && l == rst)
        .map(|idx| (idx + 1) as f32 / 10.0)
}

pub fn actuation_mm_to_raw(mm: f32) -> (u8, u8) {
    let tenths = (mm * 10.0).round().clamp(1.0, 40.0) as usize;
    ACTUATION_POINTS[tenths - 1]
}

pub fn encode_profile_read_feature(fs_id: u8, size: u16, offset: u32) -> Vec<u8> {
    let mut buf = vec![0u8; FEATURE_LEN];
    buf[0] = 0x83;
    buf[1] = 0x01;
    buf[2] = fs_id;
    buf[3..5].copy_from_slice(&size.to_le_bytes());
    buf[5..9].copy_from_slice(&offset.to_le_bytes());
    buf
}

pub fn encode_hall_thresholds_all(act: u8, rst: u8) -> Vec<u8> {
    let mut usages = APEX_PRO_ANALOG_SLOTS;
    usages.sort_unstable();
    let mut buf = vec![0u8; FEATURE_LEN];
    buf[0] = 0x38;
    buf[1] = 0x61;
    buf[2] = usages.len() as u8;
    buf[3] = 0x00; // primary hall layer
    for (idx, &usage) in usages.iter().enumerate() {
        let off = 4 + idx * 3;
        buf[off] = usage;
        buf[off + 1] = act;
        buf[off + 2] = rst;
    }
    buf
}

pub fn encode_release_mode_all(mode: u8) -> Vec<u8> {
    let mut usages = APEX_PRO_ANALOG_SLOTS;
    usages.sort_unstable();
    let mut buf = vec![0u8; FEATURE_LEN];
    buf[0] = 0x38;
    buf[1] = 0x62;
    buf[2] = usages.len() as u8;
    for (idx, &usage) in usages.iter().enumerate() {
        let off = 3 + idx * 2;
        buf[off] = usage;
        buf[off + 1] = mode;
    }
    buf
}

pub struct SteelSeriesKeyboardDevice {
    id: String,
    spec: SteelSeriesProductSpec,
    path: CString,
}

impl SteelSeriesKeyboardDevice {
    fn open(&self) -> Result<HidDevice> {
        let api = HidApi::new().map_err(|e| kb_err(self.spec.name, e.to_string()))?;
        api.open_path(self.path.as_c_str())
            .map_err(|e| kb_err(self.spec.name, format!("open failed: {e}")))
    }

    fn read_firmware(dev: &HidDevice) -> Option<String> {
        let mut req = [0u8; 64];
        req[0] = 0x90;
        req[1] = 0x00;
        let rep = exchange_report0(dev, &req, 600, |r| {
            r.len() >= 3 && r[0] == 0x90 && r[1] == 0x00
        })?;
        let end = rep[2..]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(rep.len() - 2);
        let text = String::from_utf8_lossy(&rep[2..2 + end]).trim().to_string();
        (!text.is_empty()).then_some(text)
    }

    fn read_profile_blob(dev: &HidDevice, slot: u8) -> Option<Vec<u8>> {
        let fs_id = FIRST_PROFILE_FS + slot;
        let mut blob = Vec::with_capacity(PROFILE_LEN);
        let chunks = PROFILE_LEN / CHUNK_LEN;
        for idx in 0..chunks {
            let offset = (idx * CHUNK_LEN) as u32;
            let req = encode_profile_read_feature(fs_id, CHUNK_LEN as u16, offset);
            let rep = feature_exchange_report0(dev, &req, FEATURE_LEN)?;
            if rep.len() < 3 + CHUNK_LEN {
                return None;
            }
            blob.extend_from_slice(&rep[3..3 + CHUNK_LEN]);
            if idx + 1 < chunks {
                thread::sleep(Duration::from_millis(13));
            }
        }
        Some(blob)
    }
}

impl KeyboardDevice for SteelSeriesKeyboardDevice {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        self.spec.name
    }

    fn brand(&self) -> KeyboardBrand {
        KeyboardBrand::SteelSeries
    }

    fn support_tier(&self) -> SupportTier {
        SupportTier::ImplementedFromDocs
    }

    fn read_status(&self) -> KeyboardStatus {
        let mut firmware_version = None;
        let mut actuation_mm = None;
        let mut rapid_trigger_enabled = None;
        let mut socd_enabled = None;
        let mut extra = Vec::new();

        if let Ok(dev) = self.open() {
            firmware_version = Self::read_firmware(&dev);
            if self.spec.wired_profile_support {
                if let Some(blob) = Self::read_profile_blob(&dev, 0) {
                    let crc_ok = profile_checksum_valid(&blob);
                    let schema = blob[OFFSET_DEVICE_SCHEMA];
                    extra.push(("Profile Schema".to_string(), format!("v{schema}")));
                    extra.push((
                        "Profile CRC-32".to_string(),
                        if crc_ok { "Valid" } else { "Mismatch" }.to_string(),
                    ));
                    // Read W key (slot index 16 = HID 0x1A) as representative primary key.
                    let w_idx = 16usize;
                    actuation_mm = actuation_raw_to_mm(
                        blob[OFFSET_ACTUATION + w_idx],
                        blob[OFFSET_RESET + w_idx],
                    );
                    rapid_trigger_enabled = Some(blob[OFFSET_RELEASE_MODE + w_idx] == 0x02);
                    if schema == 8 {
                        let rt_flag = blob[OFFSET_RAPID_TAP_ENABLED];
                        if rt_flag == 0 || rt_flag == 1 {
                            socd_enabled = Some(rt_flag == 1);
                        }
                    }
                }
            }
        }

        let capabilities = KeyboardCapabilities {
            supports_base_actuation_write: self.spec.wired_profile_support,
            supports_rapid_trigger_toggle: self.spec.wired_profile_support,
            supports_socd_toggle: self.spec.wired_profile_support,
            socd_label: Some("Rapid Tap (SOCD)".to_string()),
            ..Default::default()
        };

        KeyboardStatus {
            id: self.id.clone(),
            brand: KeyboardBrand::SteelSeries,
            name: self.spec.name.to_string(),
            vid_pid: format!("{:04x}:{:04x}", STEELSERIES_VID, self.spec.pid),
            support_tier: SupportTier::ImplementedFromDocs,
            protocol_label: "SteelSeries 0xFFC0".to_string(),
            chip_type: "STM32 / OmniPoint".to_string(),
            switch_tech: "OmniPoint Hall Effect".to_string(),
            firmware_version,
            layout: None,
            active_profile: None,
            profiles: Vec::new(),
            actuation_mm,
            actuation_range_mm: Some((0.1, 4.0)),
            rapid_trigger_enabled,
            rapid_trigger_mm: None,
            polling_rate_hz: None,
            battery: None,
            system_mode: None,
            socd_enabled,
            tachyon_enabled: None,
            debounce_mode: None,
            debounce_ms: None,
            extra_telemetry: extra,
            status_note: Some(
                "Implemented from OpenMouse SteelSeries GG protocol docs (live RAM hall updates; no flash wear). Confirm-gated."
                    .to_string(),
            ),
            capabilities,
            wooting: None,
        }
    }

    fn set_base_actuation(
        &self,
        actuation_mm: f32,
        rt_enabled: Option<bool>,
        _rt_mm: Option<f32>,
    ) -> Result<()> {
        if !self.spec.wired_profile_support {
            return Err(kb_unsupported(
                self.spec.name,
                "wired Apex Pro hall threshold write",
            ));
        }
        let dev = self.open()?;
        let (act, rst) = actuation_mm_to_raw(actuation_mm);
        let frame = encode_hall_thresholds_all(act, rst);
        feature_exchange_report0(&dev, &frame, FEATURE_LEN).ok_or_else(|| {
            kb_err(
                self.spec.name,
                "Keyboard did not answer hall threshold feature report",
            )
        })?;
        if let Some(rt_on) = rt_enabled {
            let mode = if rt_on { 0x02 } else { 0x00 };
            let rframe = encode_release_mode_all(mode);
            feature_exchange_report0(&dev, &rframe, FEATURE_LEN).ok_or_else(|| {
                kb_err(
                    self.spec.name,
                    "Keyboard did not answer Rapid Trigger mode feature report",
                )
            })?;
        }
        Ok(())
    }

    fn set_rapid_trigger_enabled(&self, enabled: bool) -> Result<bool> {
        if !self.spec.wired_profile_support {
            return Err(kb_unsupported(self.spec.name, "Rapid Trigger toggle"));
        }
        let dev = self.open()?;
        let mode = if enabled { 0x02 } else { 0x00 };
        let rframe = encode_release_mode_all(mode);
        feature_exchange_report0(&dev, &rframe, FEATURE_LEN).ok_or_else(|| {
            kb_err(
                self.spec.name,
                "Keyboard did not answer Rapid Trigger mode feature report",
            )
        })?;
        Ok(enabled)
    }

    fn set_socd(&self, enabled: bool) -> Result<bool> {
        if !self.spec.wired_profile_support {
            return Err(kb_unsupported(self.spec.name, "Rapid Tap toggle"));
        }
        let dev = self.open()?;
        let mut out = [0u8; 65];
        out[0] = 0x00;
        out[1] = 0x38;
        out[2] = 0x66;
        out[3] = u8::from(enabled);
        dev.write(&out)
            .map_err(|e| kb_err(self.spec.name, e.to_string()))?;
        Ok(enabled)
    }
}

pub(crate) fn detect_steelseries_keyboards(
    api: &HidApi,
    infos: &[&HidDeviceInfo],
    claimed: &mut HashSet<(u16, u16, String)>,
) -> Vec<SteelSeriesKeyboardDevice> {
    let mut found = Vec::new();
    for info in infos {
        if info.vendor_id() != STEELSERIES_VID {
            continue;
        }
        if info.usage_page() != STEELSERIES_USAGE_PAGE || info.usage() != STEELSERIES_USAGE {
            continue;
        }
        let Some(spec) = lookup_steelseries_kb(info.product_id()) else {
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
        found.push(SteelSeriesKeyboardDevice {
            id: format!("steelseries:1038:{:04x}", spec.pid),
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
    fn steelseries_crc32_and_actuation_table_match_spec() {
        let mut blob = vec![0u8; PROFILE_LEN];
        let crc = steelseries_crc32(&blob[..CHECKSUM_OFFSET]);
        blob[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        assert!(profile_checksum_valid(&blob));

        let (act, rst) = actuation_mm_to_raw(1.0);
        assert_eq!((act, rst), (21, 18));
        assert_eq!(actuation_raw_to_mm(21, 18), Some(1.0));
    }
}
