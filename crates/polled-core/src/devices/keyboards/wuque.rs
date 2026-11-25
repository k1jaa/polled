// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Wuque Studio / SparkLink Hall-Effect keyboard backend (`0x1CA6` / `0x36B5`, `0xFFB0:0x01` or `0xFF80:0x01`).
//!
//! Protocol codec implemented from `OpenMouse-Project/keyboard-protocol` (`src/wuque/`, AGPL-3.0-or-later).
//!
//! Supported models:
//! - Wuque Studio BABAO60 HE (`1ca6:1b10`, test `1ca6:1b0a`)
//! - Wuque Studio ZOOM TIGA HE (`1ca6:1b09`)
//! - Wuque Studio BABAO80 HE (`36b5:75d4`, test `36b5:75d2`)
//!
//! Safety: Factory reset (`[2, 1]`), firmware upgrade (`group 8`), and `highPollingRateReset`
//! (`[2, 15, 1]`) are NEVER encoded or sent.

use std::collections::HashSet;
use std::ffi::CString;

use hidapi::{DeviceInfo as HidDeviceInfo, HidApi, HidDevice};

use crate::devices::keyboards::{
    device_dedup_key, exchange_report0, kb_err, kb_verify_err, KeyboardBrand, KeyboardCapabilities,
    KeyboardDevice, KeyboardProfileSlot, KeyboardStatus,
};
use crate::devices::SupportTier;
use crate::error::Result;

pub const WUQUE_VID: u16 = 0x1CA6;
pub const WUQUE_VID_ALT: u16 = 0x36B5;
pub const WUQUE_USAGE_PAGE: u16 = 0xFFB0;
pub const WUQUE_USAGE_PAGE_ALT: u16 = 0xFF80;
pub const WUQUE_USAGE: u16 = 0x0001;

const WUQUE_UNSUPPORTED: u8 = 0xFF;

const GROUP_DEVICE: u8 = 1;
const GROUP_GLOBAL: u8 = 2;

const DEV_PROTOCOL: u8 = 1;
const DEV_INFO: u8 = 2;
const DEV_FEATURE: u8 = 3;

const GLOBAL_CONFIG_SWITCH: u8 = 3;
const GLOBAL_SYSTEM_SETTING: u8 = 4;
const GLOBAL_REPORT_RATE: u8 = 5;
const GLOBAL_RT_PRECISION: u8 = 12;
const GLOBAL_SLEEP_TIME: u8 = 13;
const GLOBAL_SHAKE_OPT: u8 = 16;

const ACCESS_READ_LIST: u8 = 0;
const ACCESS_READ: u8 = 1;
const ACCESS_WRITE: u8 = 2;

const SYSTEM_WIN: u8 = 0;
const SYSTEM_MAC: u8 = 1;

#[derive(Debug, Clone, Copy)]
pub struct WuqueProductSpec {
    pub vid: u16,
    pub pid: u16,
    pub name: &'static str,
}

pub const WUQUE_PRODUCTS: &[WuqueProductSpec] = &[
    WuqueProductSpec {
        vid: WUQUE_VID,
        pid: 0x1B10,
        name: "Wuque BABAO60 HE",
    },
    WuqueProductSpec {
        vid: WUQUE_VID,
        pid: 0x1B0A,
        name: "Wuque BABAO60 HE (Test)",
    },
    WuqueProductSpec {
        vid: WUQUE_VID,
        pid: 0x1B09,
        name: "Wuque ZOOM TIGA HE",
    },
    WuqueProductSpec {
        vid: WUQUE_VID_ALT,
        pid: 0x75D4,
        name: "Wuque BABAO80 HE",
    },
    WuqueProductSpec {
        vid: WUQUE_VID_ALT,
        pid: 0x75D2,
        name: "Wuque BABAO80 HE (Test)",
    },
];

pub fn lookup_wuque_product(vid: u16, pid: u16) -> Option<WuqueProductSpec> {
    WUQUE_PRODUCTS
        .iter()
        .copied()
        .find(|p| p.vid == vid && p.pid == pid)
}

pub fn encode_wuque_frame(group: u8, sub: u8, params: &[u8]) -> [u8; 64] {
    let mut frame = [0u8; 64];
    frame[0] = group;
    frame[1] = sub;
    let n = params.len().min(62);
    frame[2..2 + n].copy_from_slice(&params[..n]);
    frame
}

pub fn wuque_reply_payload(reply: &[u8], group: u8, sub: u8) -> Option<&[u8]> {
    if reply.len() < 2 || reply[0] != group || reply[1] == WUQUE_UNSUPPORTED || reply[1] != sub {
        return None;
    }
    Some(&reply[2..])
}

pub fn wuque_rate_code_to_hz(code: u8) -> Option<u16> {
    match code {
        0 => Some(8000),
        1 => Some(4000),
        2 => Some(2000),
        3 => Some(1000),
        4 => Some(500),
        5 => Some(250),
        6 => Some(125),
        _ => None,
    }
}

pub fn wuque_hz_to_rate_code(hz: u16) -> Option<u8> {
    match hz {
        8000 => Some(0),
        4000 => Some(1),
        2000 => Some(2),
        1000 => Some(3),
        500 => Some(4),
        250 => Some(5),
        125 => Some(6),
        _ => None,
    }
}

pub fn decode_wuque_protocol_version(reply: &[u8]) -> Option<String> {
    let p = wuque_reply_payload(reply, GROUP_DEVICE, DEV_PROTOCOL)?;
    if p.len() < 4 {
        return None;
    }
    Some(format!("{}.{}.{}.{}", p[0], p[1], p[2], p[3]))
}

pub fn decode_wuque_device_info(reply: &[u8]) -> Option<(String, String)> {
    let p = wuque_reply_payload(reply, GROUP_DEVICE, DEV_INFO)?;
    if p.len() < 14 {
        return None;
    }
    let app = format!("{}.{}.{}.{}", p[6], p[7], p[8], p[9]);
    let pcb = format!("{}-{}-{}-{}", p[10], p[11], p[12], p[13]);
    Some((app, pcb))
}

pub fn decode_wuque_report_rate_list(reply: &[u8]) -> Vec<u16> {
    let Some(p) = wuque_reply_payload(reply, GROUP_GLOBAL, GLOBAL_REPORT_RATE) else {
        return Vec::new();
    };
    if p.len() < 2 {
        return Vec::new();
    }
    let total = p[1] as usize;
    if p.len() < 2 + total {
        return Vec::new();
    }
    let mut rates: Vec<u16> = p[2..2 + total]
        .iter()
        .filter_map(|&code| wuque_rate_code_to_hz(code))
        .collect();
    rates.sort_unstable();
    rates.dedup();
    rates
}

pub struct WuqueKeyboardDevice {
    id: String,
    name: String,
    vid: u16,
    pid: u16,
    path: CString,
}

impl WuqueKeyboardDevice {
    fn open(&self) -> Result<HidDevice> {
        let api = HidApi::new().map_err(|e| kb_err(&self.name, e.to_string()))?;
        api.open_path(self.path.as_c_str())
            .map_err(|e| kb_err(&self.name, format!("open failed: {e}")))
    }

    fn query(dev: &HidDevice, group: u8, sub: u8, params: &[u8]) -> Option<Vec<u8>> {
        let frame = encode_wuque_frame(group, sub, params);
        for _ in 0..2 {
            if let Some(reply) = exchange_report0(dev, &frame, 450, |r| {
                r.len() >= 2 && r[0] == group && (r[1] == sub || r[1] == WUQUE_UNSUPPORTED)
            }) {
                if reply.get(1) == Some(&WUQUE_UNSUPPORTED) {
                    return None;
                }
                return Some(reply);
            }
        }
        None
    }
}

impl KeyboardDevice for WuqueKeyboardDevice {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn brand(&self) -> KeyboardBrand {
        KeyboardBrand::Wuque
    }

    fn support_tier(&self) -> SupportTier {
        SupportTier::ImplementedFromDocs
    }

    fn read_status(&self) -> KeyboardStatus {
        let mut firmware_version = None;
        let mut active_profile = None;
        let mut polling_rate_hz = None;
        let mut supported_rates = Vec::new();
        let mut system_mode = None;
        let mut switch_tech = "Hall Effect (SparkLink)".to_string();
        let mut extra = Vec::new();

        if let Ok(dev) = self.open() {
            if let Some(rep) = Self::query(&dev, GROUP_DEVICE, DEV_PROTOCOL, &[]) {
                firmware_version = decode_wuque_protocol_version(&rep);
            }
            if let Some(rep) = Self::query(&dev, GROUP_DEVICE, DEV_INFO, &[]) {
                if let Some((app, pcb)) = decode_wuque_device_info(&rep) {
                    extra.push(("App Version".to_string(), app));
                    extra.push(("PCB Revision".to_string(), pcb));
                }
            }
            if let Some(rep) = Self::query(&dev, GROUP_DEVICE, DEV_FEATURE, &[]) {
                if let Some(p) = wuque_reply_payload(&rep, GROUP_DEVICE, DEV_FEATURE) {
                    if !p.is_empty() {
                        let axis = p[0];
                        let tech = if axis & 16 != 0 {
                            "3D Magnetic"
                        } else if axis & 2 != 0 {
                            "Hall Effect (Magnetic)"
                        } else if axis & 8 != 0 {
                            "Inductive"
                        } else if axis & 4 != 0 {
                            "Optical"
                        } else {
                            "Mechanical"
                        };
                        switch_tech = tech.to_string();
                    }
                }
            }
            if let Some(rep) = Self::query(&dev, GROUP_GLOBAL, GLOBAL_CONFIG_SWITCH, &[ACCESS_READ])
            {
                if let Some(p) = wuque_reply_payload(&rep, GROUP_GLOBAL, GLOBAL_CONFIG_SWITCH) {
                    if p.len() >= 2 && p[1] <= 3 {
                        active_profile = Some(p[1]);
                    }
                }
            }
            if let Some(rep) =
                Self::query(&dev, GROUP_GLOBAL, GLOBAL_REPORT_RATE, &[ACCESS_READ_LIST])
            {
                supported_rates = decode_wuque_report_rate_list(&rep);
            }
            if let Some(rep) = Self::query(&dev, GROUP_GLOBAL, GLOBAL_REPORT_RATE, &[ACCESS_READ]) {
                if let Some(p) = wuque_reply_payload(&rep, GROUP_GLOBAL, GLOBAL_REPORT_RATE) {
                    if p.len() >= 2 {
                        polling_rate_hz = wuque_rate_code_to_hz(p[1]);
                    }
                }
            }
            if let Some(rep) =
                Self::query(&dev, GROUP_GLOBAL, GLOBAL_SYSTEM_SETTING, &[ACCESS_READ])
            {
                if let Some(p) = wuque_reply_payload(&rep, GROUP_GLOBAL, GLOBAL_SYSTEM_SETTING) {
                    if p.len() >= 2 {
                        system_mode = Some(if p[1] == SYSTEM_MAC {
                            "macOS".to_string()
                        } else {
                            "Windows".to_string()
                        });
                    }
                }
            }
            if let Some(rep) = Self::query(&dev, GROUP_GLOBAL, GLOBAL_RT_PRECISION, &[0]) {
                if let Some(p) = wuque_reply_payload(&rep, GROUP_GLOBAL, GLOBAL_RT_PRECISION) {
                    if p.len() >= 2 && p[1] > 0 {
                        extra.push((
                            "RT Min Precision".to_string(),
                            format!("{:.3} mm", p[1] as f32 / 1000.0),
                        ));
                    }
                }
            }
            if let Some(rep) = Self::query(&dev, GROUP_GLOBAL, GLOBAL_SLEEP_TIME, &[1]) {
                if let Some(p) = wuque_reply_payload(&rep, GROUP_GLOBAL, GLOBAL_SLEEP_TIME) {
                    if p.len() >= 3 {
                        let secs = u16::from_le_bytes([p[1], p[2]]);
                        extra.push(("Lighting Sleep".to_string(), format!("{secs} s")));
                    }
                }
            }
            if let Some(rep) = Self::query(&dev, GROUP_GLOBAL, GLOBAL_SHAKE_OPT, &[1]) {
                if let Some(p) = wuque_reply_payload(&rep, GROUP_GLOBAL, GLOBAL_SHAKE_OPT) {
                    if p.len() >= 2 {
                        extra.push((
                            "Shake Optimization".to_string(),
                            if p[1] == 1 { "ON" } else { "OFF" }.to_string(),
                        ));
                    }
                }
            }
        }

        let profiles = (0u8..4)
            .map(|idx| KeyboardProfileSlot {
                index: idx,
                name: format!("Config {}", idx + 1),
                is_active: active_profile == Some(idx),
            })
            .collect();

        let capabilities = KeyboardCapabilities {
            supports_profile_switch: true,
            profile_switch_volatile: false,
            supports_polling_rate_write: !supported_rates.is_empty(),
            supported_polling_rates_hz: supported_rates,
            supports_system_mode_toggle: system_mode.is_some(),
            ..Default::default()
        };

        KeyboardStatus {
            id: self.id.clone(),
            brand: KeyboardBrand::Wuque,
            name: self.name.clone(),
            vid_pid: format!("{:04x}:{:04x}", self.vid, self.pid),
            support_tier: SupportTier::ImplementedFromDocs,
            protocol_label: "Wuque / SparkLink 64B".to_string(),
            chip_type: "SparkLink MCU".to_string(),
            switch_tech,
            firmware_version,
            layout: None,
            active_profile,
            profiles,
            actuation_mm: None,
            actuation_range_mm: Some((0.1, 4.0)),
            rapid_trigger_enabled: None,
            rapid_trigger_mm: None,
            polling_rate_hz,
            battery: None,
            system_mode,
            socd_enabled: None,
            tachyon_enabled: None,
            debounce_mode: None,
            debounce_ms: None,
            extra_telemetry: extra,
            status_note: Some(
                "Implemented from OpenMouse Wuque/SparkLink protocol docs; writes are confirm-gated and verified by read-back."
                    .to_string(),
            ),
            capabilities,
            wooting: None,
        }
    }

    fn switch_profile(&self, slot: u8) -> Result<bool> {
        if slot > 3 {
            return Err(kb_err(
                &self.name,
                format!("Wuque config slot must be 0..=3 (got {slot})"),
            ));
        }
        let dev = self.open()?;
        let reply = Self::query(
            &dev,
            GROUP_GLOBAL,
            GLOBAL_CONFIG_SWITCH,
            &[ACCESS_WRITE, slot],
        )
        .ok_or_else(|| kb_err(&self.name, "Keyboard did not acknowledge config switch"))?;
        let ack_slot = wuque_reply_payload(&reply, GROUP_GLOBAL, GLOBAL_CONFIG_SWITCH)
            .and_then(|p| p.get(1).copied());
        if ack_slot != Some(slot) {
            return Err(kb_verify_err(
                &self.name,
                format!("Config {}", slot + 1),
                format!("{ack_slot:?}"),
            ));
        }
        let readback = Self::query(&dev, GROUP_GLOBAL, GLOBAL_CONFIG_SWITCH, &[ACCESS_READ])
            .and_then(|r| {
                wuque_reply_payload(&r, GROUP_GLOBAL, GLOBAL_CONFIG_SWITCH)
                    .and_then(|p| p.get(1).copied())
            });
        if readback != Some(slot) {
            return Err(kb_verify_err(
                &self.name,
                format!("Config {}", slot + 1),
                format!("{readback:?}"),
            ));
        }
        Ok(true)
    }

    fn set_polling_rate(&self, hz: u16) -> Result<u16> {
        let code = wuque_hz_to_rate_code(hz).ok_or_else(|| {
            kb_err(
                &self.name,
                format!("Unsupported Wuque polling rate {hz} Hz"),
            )
        })?;
        let dev = self.open()?;
        Self::query(
            &dev,
            GROUP_GLOBAL,
            GLOBAL_REPORT_RATE,
            &[ACCESS_WRITE, code],
        )
        .ok_or_else(|| {
            kb_err(
                &self.name,
                "Keyboard did not acknowledge polling rate change",
            )
        })?;
        let actual = Self::query(&dev, GROUP_GLOBAL, GLOBAL_REPORT_RATE, &[ACCESS_READ])
            .and_then(|r| {
                wuque_reply_payload(&r, GROUP_GLOBAL, GLOBAL_REPORT_RATE)
                    .and_then(|p| p.get(1).copied())
            })
            .and_then(wuque_rate_code_to_hz);
        if actual != Some(hz) {
            return Err(kb_verify_err(
                &self.name,
                format!("{hz} Hz"),
                format!("{actual:?}"),
            ));
        }
        Ok(hz)
    }

    fn set_system_mode(&self, mac_mode: bool) -> Result<String> {
        let val = if mac_mode { SYSTEM_MAC } else { SYSTEM_WIN };
        let dev = self.open()?;
        Self::query(
            &dev,
            GROUP_GLOBAL,
            GLOBAL_SYSTEM_SETTING,
            &[ACCESS_WRITE, val],
        )
        .ok_or_else(|| {
            kb_err(
                &self.name,
                "Keyboard did not acknowledge system mode change",
            )
        })?;
        let actual = Self::query(&dev, GROUP_GLOBAL, GLOBAL_SYSTEM_SETTING, &[ACCESS_READ])
            .and_then(|r| {
                wuque_reply_payload(&r, GROUP_GLOBAL, GLOBAL_SYSTEM_SETTING)
                    .and_then(|p| p.get(1).copied())
            });
        if actual != Some(val) {
            return Err(kb_verify_err(
                &self.name,
                if mac_mode { "macOS" } else { "Windows" },
                format!("{actual:?}"),
            ));
        }
        Ok(if mac_mode { "macOS" } else { "Windows" }.to_string())
    }
}

pub(crate) fn detect_wuque_keyboards(
    api: &HidApi,
    infos: &[&HidDeviceInfo],
    claimed: &mut HashSet<(u16, u16, String)>,
) -> Vec<WuqueKeyboardDevice> {
    let mut found = Vec::new();
    for info in infos {
        let vid = info.vendor_id();
        let pid = info.product_id();
        let Some(spec) = lookup_wuque_product(vid, pid) else {
            continue;
        };
        let up = info.usage_page();
        if (up != WUQUE_USAGE_PAGE && up != WUQUE_USAGE_PAGE_ALT) || info.usage() != WUQUE_USAGE {
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
        found.push(WuqueKeyboardDevice {
            id: format!("wuque:{vid:04x}:{pid:04x}"),
            name: spec.name.to_string(),
            vid,
            pid,
            path: info.path().to_owned(),
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wuque_frame_and_decoders_match_verified_hardware_samples() {
        let f = encode_wuque_frame(GROUP_GLOBAL, GLOBAL_CONFIG_SWITCH, &[ACCESS_WRITE, 2]);
        assert_eq!(&f[..4], &[2, 3, 2, 2]);
        assert_eq!(f[63], 0);

        // Hardware sample: 01 01 01 02 01 00 -> 1.2.1.0
        let ver_rep = [0x01, 0x01, 0x01, 0x02, 0x01, 0x00];
        assert_eq!(
            decode_wuque_protocol_version(&ver_rep).as_deref(),
            Some("1.2.1.0")
        );

        // Hardware sample: 02 05 00 06 00 01 02 03 04 05 -> 250..8000 Hz
        let rate_rep = [0x02, 0x05, 0x00, 0x06, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05];
        assert_eq!(
            decode_wuque_report_rate_list(&rate_rep),
            vec![250, 500, 1000, 2000, 4000, 8000]
        );

        // Unsupported sub-command echo (0xFF) yields None
        assert!(wuque_reply_payload(&[0x10, 0xFF, 0x00], 0x10, 0x01).is_none());
    }
}
