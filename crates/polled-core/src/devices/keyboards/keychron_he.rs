// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Keychron Hall-Effect / TMR keyboard backend (`0x3434`, `0xFF60:0x61`, `KC_HE` command `0xA9`).
//!
//! Protocol codec implemented from `OpenMouse-Project/keyboard-protocol` (`src/keychron/`,
//! AGPL-3.0-or-later). Supports HE command versions 1 through 5 (including 16-bit v5 travel
//! steps), profile switching, global actuation & Rapid Trigger writes (`setTravel` + `saveProfile`),
//! and profile name / metadata read-back.
//!
//! Safety: Never encodes calibration wipe, `clearProfileBuffer` (`0x1E`), or bootloader jumps.

use std::collections::HashSet;
use std::ffi::CString;

use hidapi::{DeviceInfo as HidDeviceInfo, HidApi, HidDevice};

use crate::devices::keyboards::{
    device_dedup_key, exchange_report0, kb_err, kb_verify_err, KeyboardBrand, KeyboardCapabilities,
    KeyboardDevice, KeyboardProfileSlot, KeyboardStatus,
};
use crate::devices::SupportTier;
use crate::error::Result;

pub const KEYCHRON_VID: u16 = 0x3434;
pub const KEYCHRON_USAGE_PAGE: u16 = 0xFF60;
pub const KEYCHRON_USAGE: u16 = 0x0061;
const KC_HE_CMD: u8 = 0xA9;

const SUB_GET_VERSION: u8 = 0x01;
const SUB_GET_PROFILES_INFO: u8 = 0x10;
const SUB_SELECT_PROFILE: u8 = 0x11;
const SUB_GET_PROFILE_BUFFER: u8 = 0x12;
const SUB_SET_TRAVEL: u8 = 0x14;
const SUB_SAVE_PROFILE: u8 = 0x1F;

const WORK_MODE_STATIC: u8 = 1;
const WORK_MODE_QUICK_TRIGGER: u8 = 2;
const KEY_MODE_ALL_KEYS: u8 = 1;

#[derive(Debug, Clone, Copy)]
pub struct KeychronHeProductSpec {
    pub pid: u16,
    pub name: &'static str,
    pub min_mm: f32,
    pub max_mm: f32,
}

const fn kc(pid: u16, name: &'static str, min_mm: f32, max_mm: f32) -> KeychronHeProductSpec {
    KeychronHeProductSpec {
        pid,
        name,
        min_mm,
        max_mm,
    }
}

/// Curated Keychron / Lemokey Hall-Effect & TMR product catalog (`VID 0x3434`).
/// Non-HE Keychron boards fall through to the Universal QMK/VIA backend automatically.
pub const KEYCHRON_HE_PRODUCTS: &[KeychronHeProductSpec] = &[
    kc(0x0402, "Keychron P1 HE", 0.2, 3.8),
    kc(0x0403, "Keychron P1 HE", 0.2, 3.8),
    kc(0x040C, "Keychron J3 HE ANSI", 0.2, 3.3),
    kc(0x0416, "Keychron J2 HE 8K ANSI", 0.1, 3.35),
    kc(0x0417, "Keychron J8 HE 8K ANSI", 0.1, 3.35),
    kc(0x0418, "Keychron J8 HE 8K ISO", 0.1, 3.35),
    kc(0x0419, "Keychron J8 HE 8K JIS", 0.1, 3.35),
    kc(0x041B, "Keychron J14 HE ANSI", 0.2, 3.3),
    kc(0x041C, "Keychron J12 HE ANSI", 0.2, 3.3),
    kc(0x041E, "Keychron J5 HE ANSI", 0.1, 3.4),
    kc(0x041F, "Keychron J4 HE ANSI", 0.2, 3.3),
    kc(0x0429, "Keychron J15 HE ANSI", 0.2, 3.3),
    kc(0x042F, "Keychron V1 HE ANSI", 0.2, 3.35),
    kc(0x043B, "Keychron C3 Pro HE ANSI", 0.1, 3.8),
    kc(0x0441, "Keychron V6 HE ANSI", 0.1, 3.4),
    kc(0x0445, "Keychron J5 TMR ANSI", 0.1, 3.4),
    kc(0x0446, "Keychron J5 HE 8K ISO", 0.1, 3.35),
    kc(0x0449, "Keychron J2 TMR", 0.2, 3.3),
    kc(0x044A, "Keychron C100 HE 8K ANSI", 0.1, 3.35),
    kc(0x044B, "Keychron J5 TMR ANSI", 0.1, 3.1),
    kc(0x0450, "Keychron T2 HE", 0.2, 3.8),
    kc(0x0454, "Keychron J2 TMR ANSI", 0.2, 3.3),
    kc(0x045A, "Keychron eX75 HE 8K ANSI", 0.1, 3.35),
    kc(0x045B, "Keychron eX67 HE 8K ANSI", 0.1, 3.35),
    kc(0x045C, "Keychron eX99 HE 8K ANSI", 0.1, 3.35),
    kc(0x045D, "Keychron eX99 HE 8K ANSI", 0.1, 3.5),
    kc(0x045E, "Keychron J3 TMR ANSI", 0.1, 3.5),
    kc(0x045F, "Keychron J3 TMR Silent", 0.1, 3.5),
    kc(0x0460, "Keychron C75 TMR 8K JIS", 0.1, 3.35),
    kc(0x0464, "Keychron F4 HE 8K", 0.2, 3.3),
    kc(0x0471, "Keychron Carbon 60 HE 8K", 0.1, 3.5),
    kc(0x0472, "Keychron Carbon 68 HE 8K", 0.1, 3.45),
    kc(0x0476, "Keychron F6 HE 8K", 0.2, 3.3),
    kc(0x047B, "Keychron C65 HE 8K ANSI", 0.1, 3.5),
    kc(0x047C, "Keychron C6 HE 8K ANSI", 0.1, 3.35),
    kc(0x047D, "Keychron Q17 HE", 0.2, 3.8),
    kc(0x047E, "Keychron C75 HE ANSI", 0.1, 3.5),
    kc(0x0480, "Keychron EX75 HE Lite", 0.1, 3.35),
    kc(0x0481, "Keychron Carbon0 HE 8K", 0.2, 3.5),
    kc(0x0484, "Keychron C75 HE 1K ANSI", 0.1, 3.8),
    kc(0x0485, "Keychron EX75 HE Lite", 0.1, 3.5),
    kc(0x0486, "Keychron Z2-60 HE 8K", 0.1, 3.55),
    kc(0x048A, "Keychron J8 HE 8K ANSI", 0.1, 3.35),
    kc(0x048E, "Keychron K18 HE ANSI", 0.2, 3.35),
    kc(0x048F, "Keychron Q17 HE ANSI", 0.2, 3.3),
    kc(0x0490, "Keychron Carbon 75 HE 8K", 0.1, 3.5),
    kc(0x0494, "Keychron J5 HE 8K", 0.1, 3.5),
    kc(0x0497, "Keychron K20 HE 8K", 0.1, 3.5),
    kc(0x0498, "Keychron C100 HE 8K ISO", 0.1, 3.35),
    kc(0x049B, "Keychron Carbon 87 HE 8K", 0.1, 3.5),
    kc(0x049C, "Keychron Carbon 108 HE 8K", 0.1, 3.5),
    kc(0x04A1, "Keychron J3 HE Silent", 0.2, 3.3),
    kc(0x04A2, "Keychron V1 HE 8K", 0.1, 3.5),
    kc(0x04A5, "Keychron J3 HE ANSI", 0.2, 3.3),
    kc(0x04A6, "Keychron J3 HE Silent", 0.2, 3.3),
    kc(0x0511, "Lemokey L1 HE ISO", 0.2, 3.8),
    kc(0x0B00, "Keychron Q0 HE", 0.2, 3.8),
    kc(0x0B01, "Keychron Q0 HE V2", 0.2, 3.3),
    kc(0x0B02, "Keychron Q0 HE V2", 0.2, 3.3),
    kc(0x0B10, "Keychron Q1 HE ANSI", 0.2, 3.8),
    kc(0x0B11, "Keychron Q1 HE ISO", 0.2, 3.8),
    kc(0x0B12, "Keychron Q1 HE JIS", 0.2, 3.8),
    kc(0x0B13, "Keychron Q1 HE DAYZ", 0.2, 3.8),
    kc(0x0B14, "Keychron Q1 HE ANSI", 0.2, 3.3),
    kc(0x0B15, "Keychron Q1 HE ANSI", 0.2, 3.3),
    kc(0x0B16, "Keychron Q1 HE ISO", 0.2, 3.3),
    kc(0x0B20, "Keychron Q2 HE ANSI", 0.2, 3.8),
    kc(0x0B30, "Keychron Q3 HE ANSI", 0.2, 3.8),
    kc(0x0B31, "Keychron Q3 HE ISO", 0.2, 3.8),
    kc(0x0B32, "Keychron Q3 HE JIS", 0.2, 3.8),
    kc(0x0B33, "Keychron Q3 HE ANSI", 0.2, 3.3),
    kc(0x0B40, "Keychron Q4 HE ANSI", 0.2, 3.8),
    kc(0x0B50, "Keychron Q5 HE ANSI", 0.2, 3.8),
    kc(0x0B51, "Keychron Q5 HE ISO", 0.2, 3.8),
    kc(0x0B52, "Keychron Q5 HE JIS", 0.2, 3.8),
    kc(0x0B53, "Keychron Q5 HE V2 ANSI", 0.2, 3.3),
    kc(0x0B60, "Keychron Q6 HE ANSI", 0.2, 3.8),
    kc(0x0B61, "Keychron Q6 HE ISO", 0.2, 3.8),
    kc(0x0B62, "Keychron Q6 HE JIS", 0.2, 3.8),
    kc(0x0B63, "Keychron Q6 HE V2 ANSI", 0.2, 3.3),
    kc(0x0B64, "Keychron Q6 HE V2 ANSI", 0.2, 3.3),
    kc(0x0B65, "Keychron Q6 HE ISO", 0.2, 3.3),
    kc(0x0B66, "Keychron Q6 HE V3 ANSI", 0.2, 3.3),
    kc(0x0BA0, "Keychron Q10 HE ANSI", 0.2, 3.8),
    kc(0x0BB0, "Keychron Q11 HE ANSI", 0.2, 3.3),
    kc(0x0BC0, "Keychron Q12 HE ANSI", 0.2, 3.8),
    kc(0x0BC1, "Keychron Q12 HE ISO", 0.2, 3.8),
    kc(0x0E20, "Keychron K2 HE ANSI", 0.2, 3.8),
    kc(0x0E21, "Keychron K2 HE ISO", 0.2, 3.8),
    kc(0x0E22, "Keychron K2 HE JIS", 0.2, 3.8),
    kc(0x0E23, "Keychron K2 HE BR", 0.2, 3.8),
    kc(0x0E24, "Keychron K2 HE ANSI", 0.2, 3.3),
    kc(0x0E25, "Keychron K2 HE ANSI", 0.2, 3.3),
    kc(0x0E26, "Keychron K2 HE V2 ISO", 0.2, 3.4),
    kc(0x0E27, "Keychron K2 HE ANSI", 0.2, 3.3),
    kc(0x0E2A, "Keychron K2 HE V3 ISO", 0.2, 3.8),
    kc(0x0E30, "Keychron K3 HE ANSI", 0.2, 2.8),
    kc(0x0E31, "Keychron K3 HE ISO", 0.2, 2.8),
    kc(0x0E32, "Keychron K3 HE JIS", 0.2, 2.8),
    kc(0x0E35, "Keychron K3 HE JIS", 0.2, 2.8),
    kc(0x0E40, "Keychron K4 HE ANSI", 0.2, 3.8),
    kc(0x0E41, "Keychron K4 HE ISO", 0.2, 3.8),
    kc(0x0E42, "Keychron K4 HE JIS", 0.2, 3.8),
    kc(0x0E43, "Keychron K4 HE ANSI", 0.2, 3.3),
    kc(0x0E44, "Keychron K4 HE ABNT2", 0.2, 3.3),
    kc(0x0E45, "Keychron K4 HE ISO V2", 0.2, 3.3),
    kc(0x0E60, "Keychron K6 HE ANSI", 0.2, 3.8),
    kc(0x0E61, "Keychron K6 HE ISO", 0.2, 3.8),
    kc(0x0E80, "Keychron K8 HE", 0.2, 3.8),
    kc(0x0E81, "Keychron K8 HE ISO", 0.2, 3.8),
    kc(0x0E82, "Keychron K8 HE JIS", 0.2, 3.8),
    kc(0x0E83, "Keychron K8 HE ANSI", 0.2, 3.3),
    kc(0x0E84, "Keychron K8 HE ANSI", 0.2, 3.3),
    kc(0x0E85, "Keychron K8 HE ISO", 0.2, 3.3),
    kc(0x0E86, "Keychron K8 HE ISO", 0.2, 3.3),
    kc(0x0EA0, "Keychron K10 HE ANSI", 0.2, 3.8),
    kc(0x0EA1, "Keychron K10 HE ISO", 0.2, 3.8),
    kc(0x0EA2, "Keychron K10 HE JIS", 0.2, 3.8),
    kc(0x0EA3, "Keychron K10 HE V2 ANSI", 0.2, 3.3),
    kc(0x0EA4, "Keychron K10 HE ISO", 0.2, 3.3),
    kc(0x0EA5, "Keychron K10 HE ABNT2", 0.2, 3.3),
    kc(0x0EA6, "Keychron K10 HE V2 ANSI", 0.2, 3.2),
    kc(0x0EA7, "Keychron K10 HE ANSI", 0.2, 3.3),
    kc(0x0EA8, "Keychron K10 HE ANSI", 0.2, 3.3),
    kc(0x0EA9, "Keychron K10 HE ISO", 0.2, 3.3),
    kc(0x0EC0, "Keychron K12 HE ANSI", 0.2, 3.8),
    kc(0x1001, "Keychron Q18 HE 8K ANSI", 0.1, 3.5),
    kc(0x1004, "Keychron Q20 HE 8K ANSI", 0.1, 3.35),
    kc(0x1005, "Keychron Q19 HE 8K", 0.1, 3.5),
    kc(0x1010, "Keychron Q1 HE 8K ANSI", 0.1, 3.35),
    kc(0x1011, "Keychron Q1 HE 8K ISO", 0.1, 3.35),
    kc(0x1012, "Keychron Q1 HE 8K JIS", 0.1, 3.35),
    kc(0x1013, "Keychron Q1 HE 8K ABNT2", 0.1, 3.35),
    kc(0x1014, "Keychron Q1 HE Marble 8K", 0.1, 3.55),
    kc(0x1015, "Keychron Q1 SE 8K ANSI", 0.1, 3.5),
    kc(0x1016, "Keychron Q1 HE 8K ANSI", 0.1, 3.45),
    kc(0x1020, "Keychron Q2 HE 8K ANSI", 0.1, 3.35),
    kc(0x1023, "Keychron Q2 HE 8K V2", 0.1, 3.5),
    kc(0x1030, "Keychron Q3 HE 8K ANSI", 0.1, 3.35),
    kc(0x1031, "Keychron Q3 HE 8K ISO", 0.1, 3.35),
    kc(0x1032, "Keychron Q3 HE 8K JIS", 0.1, 3.35),
    kc(0x1033, "Keychron Q3 HE 8K ANSI", 0.1, 3.5),
    kc(0x1050, "Keychron Q5 HE 8K ANSI", 0.1, 3.35),
    kc(0x1053, "Keychron Q5 HE 8K ANSI", 0.1, 3.5),
    kc(0x1060, "Keychron Q6 HE 8K ANSI", 0.1, 3.35),
    kc(0x1061, "Keychron Q6 HE 8K ISO", 0.1, 3.35),
    kc(0x1062, "Keychron Q6 HE 8K JIS", 0.1, 3.35),
    kc(0x1063, "Keychron Q6 HE 8K", 0.1, 3.5),
    kc(0x1064, "Keychron Q6 HE 8K ANSI", 0.1, 3.35),
    kc(0x10F3, "Keychron Q16 HE 8K ANSI", 0.1, 3.35),
    kc(0x10F4, "Keychron Q16 HE 8K ISO", 0.1, 3.35),
    kc(0x1234, "Keychron K2 HE ISO", 0.2, 3.8),
    kc(0x1500, "Keychron C0 HE 8K", 0.1, 3.35),
    kc(0x1532, "Keychron C3 HE 8K JIS", 0.1, 3.35),
    kc(0x1533, "Keychron C3 HE ANSI", 0.1, 3.5),
    kc(0x1535, "Keychron C3 HE 8K JIS", 0.1, 3.5),
    kc(0x1536, "Keychron C3 HE ANSI", 0.2, 3.5),
    kc(0x1540, "Keychron C4 HE 8K ANSI", 0.1, 3.35),
    kc(0x1541, "Keychron C4 HE 8K ANSI", 0.1, 3.5),
    kc(0x1710, "Keychron V1 HE 8K SE", 0.1, 3.5),
    kc(0x1711, "Keychron V1 HE 8K", 0.1, 3.5),
    kc(0x1728, "Keychron J2 HE ANSI", 0.2, 3.3),
    kc(0x1729, "Antec WH1 ANSI", 0.2, 3.3),
    kc(0x172C, "Keychron K004 ISO", 0.2, 3.5),
    kc(0x172D, "Secret Gear x Keychron KX100", 0.1, 3.35),
    kc(0x1740, "Keychron V4 HE 8K", 0.1, 3.5),
    kc(0x17C0, "Keychron V12 HE 8K", 0.1, 3.5),
    kc(0x17F0, "Keychron V15 HE 8K", 0.1, 3.5),
    kc(0x1860, "Keychron V6 Ultra HE ANSI", 0.1, 3.5),
];

pub fn lookup_keychron_he(pid: u16) -> Option<KeychronHeProductSpec> {
    KEYCHRON_HE_PRODUCTS.iter().copied().find(|p| p.pid == pid)
}

pub fn encode_keychron_he_frame(sub: u8, args: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[0] = KC_HE_CMD;
    out[1] = sub;
    let n = args.len().min(30);
    out[2..2 + n].copy_from_slice(&args[..n]);
    out
}

pub fn encode_keychron_set_travel_all_keys(
    version: u8,
    slot: u8,
    rapid_trigger: bool,
    press_steps: u16,
    rt_steps: u16,
) -> [u8; 32] {
    let work_mode = if rapid_trigger {
        WORK_MODE_QUICK_TRIGGER
    } else {
        WORK_MODE_STATIC
    };
    let mut out = encode_keychron_he_frame(SUB_SET_TRAVEL, &[slot, work_mode]);
    if version >= 5 {
        out[4..6].copy_from_slice(&press_steps.to_le_bytes());
        out[6..8].copy_from_slice(&rt_steps.to_le_bytes());
        out[8..10].copy_from_slice(&rt_steps.to_le_bytes());
        out[10] = KEY_MODE_ALL_KEYS;
    } else {
        out[4] = press_steps.min(255) as u8;
        out[5] = rt_steps.min(255) as u8;
        out[6] = rt_steps.min(255) as u8;
        out[7] = KEY_MODE_ALL_KEYS;
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeychronProfilesMeta {
    pub current: u8,
    pub total: u8,
    pub size: u16,
    pub dks_count: Option<u8>,
    pub snap_tap_count: Option<u8>,
    pub unit_mm: f32,
}

pub fn decode_keychron_profiles_info(report: &[u8], version: u8) -> Option<KeychronProfilesMeta> {
    if report.len() < 6 || report[0] != KC_HE_CMD || report[1] != SUB_GET_PROFILES_INFO {
        return None;
    }
    let unit_mm = if version >= 5 {
        match report.get(8).copied().unwrap_or(1) {
            2 => 0.01,
            3 => 0.001,
            _ => 0.1,
        }
    } else {
        0.1
    };
    Some(KeychronProfilesMeta {
        current: report[2],
        total: report[3],
        size: u16::from_le_bytes([report[4], report[5]]),
        dks_count: (version >= 4).then(|| report.get(6).copied().unwrap_or(0)),
        snap_tap_count: (version >= 4).then(|| report.get(7).copied().unwrap_or(0)),
        unit_mm,
    })
}

pub fn decode_keychron_global_slot(
    version: u8,
    bytes: &[u8],
    unit_mm: f32,
) -> Option<(u8, f32, f32)> {
    if version >= 5 {
        if bytes.len() < 7 {
            return None;
        }
        let mut word: u64 = 0;
        for i in (0..6).rev() {
            word = (word << 8) | (bytes[i] as u64);
        }
        let mode = (word & 0x3) as u8;
        let distance = ((word >> 2) & 0x3FFF) as f32 * unit_mm;
        let rt_press = ((word >> 16) & 0x3FFF) as f32 * unit_mm;
        Some((mode, distance, rt_press))
    } else {
        if bytes.len() < 4 {
            return None;
        }
        let word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let mode = (word & 0x3) as u8;
        let distance = ((word >> 2) & 0x3F) as f32 * unit_mm;
        let rt_press = ((word >> 8) & 0x3F) as f32 * unit_mm;
        Some((mode, distance, rt_press))
    }
}

pub fn decode_keychron_profile_name(report: &[u8]) -> Option<String> {
    if report.len() < 8 || report[0] != KC_HE_CMD || report[1] != SUB_GET_PROFILE_BUFFER {
        return None;
    }
    let mut units = Vec::new();
    for pair in report[6..report.len().min(32)].as_chunks::<2>().0 {
        let code = u16::from_be_bytes(*pair);
        if code != 0 {
            units.push(code);
        }
    }
    let name = String::from_utf16_lossy(&units).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

pub struct KeychronHeKeyboardDevice {
    id: String,
    spec: KeychronHeProductSpec,
    path: CString,
}

impl KeychronHeKeyboardDevice {
    fn open(&self) -> Result<HidDevice> {
        let api = HidApi::new().map_err(|e| kb_err(self.spec.name, e.to_string()))?;
        api.open_path(self.path.as_c_str())
            .map_err(|e| kb_err(self.spec.name, format!("open failed: {e}")))
    }

    fn call_he(dev: &HidDevice, sub: u8, args: &[u8]) -> Option<Vec<u8>> {
        let frame = encode_keychron_he_frame(sub, args);
        exchange_report0(dev, &frame, 600, |r| {
            r.len() >= 2 && r[0] == KC_HE_CMD && r[1] == sub
        })
    }

    fn read_version_and_meta(dev: &HidDevice) -> Option<(u8, u8, KeychronProfilesMeta)> {
        let ver_rep = Self::call_he(dev, SUB_GET_VERSION, &[])?;
        if ver_rep.len() < 5 {
            return None;
        }
        let version = ver_rep[2];
        let feat_bits = ver_rep[4];
        if !(1..=5).contains(&version) {
            return None;
        }
        let info_rep = Self::call_he(dev, SUB_GET_PROFILES_INFO, &[])?;
        let meta = decode_keychron_profiles_info(&info_rep, version)?;
        Some((version, feat_bits, meta))
    }
}

impl KeyboardDevice for KeychronHeKeyboardDevice {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        self.spec.name
    }

    fn brand(&self) -> KeyboardBrand {
        KeyboardBrand::Keychron
    }

    fn support_tier(&self) -> SupportTier {
        SupportTier::ImplementedFromDocs
    }

    fn read_status(&self) -> KeyboardStatus {
        let mut protocol_label = "Keychron KC_HE".to_string();
        let mut active_profile = None;
        let mut profiles = Vec::new();
        let mut actuation_mm = None;
        let mut rapid_trigger_enabled = None;
        let mut rapid_trigger_mm = None;
        let mut extra = Vec::new();

        if let Ok(dev) = self.open() {
            if let Some((version, feat_bits, meta)) = Self::read_version_and_meta(&dev) {
                protocol_label = format!("Keychron KC_HE v{version}");
                active_profile = Some(meta.current);
                extra.push(("Travel Step".to_string(), format!("{} mm", meta.unit_mm)));
                if let Some(dks) = meta.dks_count {
                    extra.push(("DKS Slots".to_string(), dks.to_string()));
                }
                if let Some(st) = meta.snap_tap_count {
                    extra.push(("Snap Tap Slots".to_string(), st.to_string()));
                }
                if feat_bits & 0x08 != 0 {
                    extra.push(("Turbo Feature".to_string(), "Supported".to_string()));
                }

                // Read onboard profile names (capped safely at 8 slots).
                let count = meta.total.min(8);
                let name_offset = meta.size.saturating_sub(if version == 1 { 33 } else { 32 });
                let name_len: u8 = if version >= 5 { 26 } else { 30 };
                for slot in 0..count {
                    let off = name_offset.to_le_bytes();
                    let name = Self::call_he(
                        &dev,
                        SUB_GET_PROFILE_BUFFER,
                        &[slot, off[0], off[1], name_len],
                    )
                    .and_then(|rep| decode_keychron_profile_name(&rep))
                    .unwrap_or_else(|| format!("Profile {}", slot + 1));
                    profiles.push(KeyboardProfileSlot {
                        index: slot,
                        name,
                        is_active: slot == meta.current,
                    });
                }

                // Read first 7 bytes (global key slot) of active profile buffer.
                let slot_bytes: u8 = if version >= 5 { 7 } else { 4 };
                if let Some(rep) = Self::call_he(
                    &dev,
                    SUB_GET_PROFILE_BUFFER,
                    &[meta.current, 0, 0, slot_bytes],
                ) {
                    if rep.len() >= 6 + slot_bytes as usize {
                        if let Some((mode, dist, rt)) = decode_keychron_global_slot(
                            version,
                            &rep[6..6 + slot_bytes as usize],
                            meta.unit_mm,
                        ) {
                            if dist > 0.0 && dist <= 4.5 {
                                actuation_mm = Some(dist);
                            }
                            rapid_trigger_enabled = Some(mode == WORK_MODE_QUICK_TRIGGER);
                            if rt > 0.0 && rt <= 4.0 {
                                rapid_trigger_mm = Some(rt);
                            }
                        }
                    }
                }
            }
        }

        let has_profiles = profiles.len() > 1;
        let capabilities = KeyboardCapabilities {
            supports_profile_switch: has_profiles,
            profile_switch_volatile: false,
            supports_base_actuation_write: active_profile.is_some(),
            supports_rapid_trigger_toggle: active_profile.is_some(),
            ..Default::default()
        };

        KeyboardStatus {
            id: self.id.clone(),
            brand: KeyboardBrand::Keychron,
            name: self.spec.name.to_string(),
            vid_pid: format!("{:04x}:{:04x}", KEYCHRON_VID, self.spec.pid),
            support_tier: SupportTier::ImplementedFromDocs,
            protocol_label,
            chip_type: "QMK / Keychron HE".to_string(),
            switch_tech: "Hall Effect / TMR".to_string(),
            firmware_version: None,
            layout: None,
            active_profile,
            profiles,
            actuation_mm,
            actuation_range_mm: Some((self.spec.min_mm, self.spec.max_mm)),
            rapid_trigger_enabled,
            rapid_trigger_mm,
            polling_rate_hz: None,
            battery: None,
            system_mode: None,
            socd_enabled: None,
            tachyon_enabled: None,
            debounce_mode: None,
            debounce_ms: None,
            extra_telemetry: extra,
            status_note: Some(
                "Implemented from OpenMouse Keychron Launcher KC_HE protocol docs; writes are confirm-gated and verified by read-back."
                    .to_string(),
            ),
            capabilities,
            wooting: None,
        }
    }

    fn switch_profile(&self, slot: u8) -> Result<bool> {
        let dev = self.open()?;
        let (_, _, meta) = Self::read_version_and_meta(&dev)
            .ok_or_else(|| kb_err(self.spec.name, "Could not read Keychron HE profile layout"))?;
        if slot >= meta.total {
            return Err(kb_err(
                self.spec.name,
                format!("Profile slot {slot} out of range (0..{})", meta.total),
            ));
        }
        Self::call_he(&dev, SUB_SELECT_PROFILE, &[slot])
            .ok_or_else(|| kb_err(self.spec.name, "Keyboard did not answer profile select"))?;
        let (_, _, after) = Self::read_version_and_meta(&dev).ok_or_else(|| {
            kb_err(
                self.spec.name,
                "Could not verify active profile after switch",
            )
        })?;
        if after.current != slot {
            return Err(kb_verify_err(
                self.spec.name,
                format!("Profile {}", slot + 1),
                format!("Profile {}", after.current + 1),
            ));
        }
        Ok(true)
    }

    fn set_base_actuation(
        &self,
        actuation_mm: f32,
        rt_enabled: Option<bool>,
        rt_mm: Option<f32>,
    ) -> Result<()> {
        let dev = self.open()?;
        let (version, _, meta) = Self::read_version_and_meta(&dev)
            .ok_or_else(|| kb_err(self.spec.name, "Could not read Keychron HE metadata"))?;
        let clamped_act = actuation_mm.clamp(self.spec.min_mm, self.spec.max_mm);
        let press_steps = (clamped_act / meta.unit_mm).round().max(1.0) as u16;
        let rt_on = rt_enabled.unwrap_or(false);
        let rt_val = rt_mm.unwrap_or(0.2).clamp(0.1, 2.5);
        let rt_steps = (rt_val / meta.unit_mm).round().max(1.0) as u16;

        let frame = encode_keychron_set_travel_all_keys(
            version,
            meta.current,
            rt_on,
            press_steps,
            rt_steps,
        );
        let ack = exchange_report0(&dev, &frame, 800, |r| {
            r.len() >= 3 && r[0] == KC_HE_CMD && r[1] == SUB_SET_TRAVEL
        })
        .ok_or_else(|| kb_err(self.spec.name, "Keyboard did not acknowledge travel update"))?;
        if ack[2] != 0 {
            return Err(kb_err(
                self.spec.name,
                format!("Keyboard rejected travel setting (code {})", ack[2]),
            ));
        }
        let save_ack = Self::call_he(&dev, SUB_SAVE_PROFILE, &[meta.current])
            .ok_or_else(|| kb_err(self.spec.name, "Keyboard did not acknowledge profile save"))?;
        if save_ack.get(2).copied() != Some(0) {
            return Err(kb_err(
                self.spec.name,
                "Keyboard did not confirm profile save",
            ));
        }
        Ok(())
    }
}

pub(crate) fn detect_keychron_he_keyboards(
    api: &HidApi,
    infos: &[&HidDeviceInfo],
    claimed: &mut HashSet<(u16, u16, String)>,
) -> Vec<KeychronHeKeyboardDevice> {
    let mut found = Vec::new();
    for info in infos {
        if info.vendor_id() != KEYCHRON_VID {
            continue;
        }
        if info.usage_page() != KEYCHRON_USAGE_PAGE || info.usage() != KEYCHRON_USAGE {
            continue;
        }
        let Some(spec) = lookup_keychron_he(info.product_id()) else {
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
        found.push(KeychronHeKeyboardDevice {
            id: format!("keychron-he:3434:{:04x}", spec.pid),
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
    fn keychron_he_v4_and_v5_travel_frames_match_spec() {
        let f4 = encode_keychron_set_travel_all_keys(4, 1, true, 12, 2);
        assert_eq!(&f4[..8], &[0xA9, 0x14, 1, 2, 12, 2, 2, 1]);

        let f5 = encode_keychron_set_travel_all_keys(5, 0, false, 150, 20);
        assert_eq!(f5[0], 0xA9);
        assert_eq!(f5[1], 0x14);
        assert_eq!(f5[3], WORK_MODE_STATIC);
        assert_eq!(u16::from_le_bytes([f5[4], f5[5]]), 150);
        assert_eq!(f5[10], KEY_MODE_ALL_KEYS);

        // Decode UTF-16BE profile name "FPS"
        let mut rep = [0u8; 32];
        rep[0] = 0xA9;
        rep[1] = 0x12;
        rep[6..12].copy_from_slice(&[0x00, b'F', 0x00, b'P', 0x00, b'S']);
        assert_eq!(decode_keychron_profile_name(&rep).as_deref(), Some("FPS"));
    }
}
