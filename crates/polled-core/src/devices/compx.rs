// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Unified Compx CX52850 / Nordic 17-byte (`Report ID 0x08` on `usage_page 0xFF02`) protocol driver.
//!
//! Automatically detects mice across multiple brands that share the Compx / Nordic 17-byte HID
//! configuration protocol (including Pulsar X2 CrazyLight / Nordic series, LAMZU 4K/1K/Wired,
//! and generic `0x3554` Compx mice).
//!
//! # Protocol Attribution & Licenses (all AGPL-3.0 compatible)
//! Every USB VID/PID, HID report ID, frame checksum, register address, and DPI encoding formula
//! in this module comes from verified open-source implementations:
//! - `packerlschupfer/pulsar-mouse-linux` (<https://github.com/packerlschupfer/pulsar-mouse-linux>)
//!   - `docs/protocol-x2-crazylight.md`, `drivers/x2_crazylight.py`, `drivers/x2_crazylight_wired.py`, `drivers/nordic.py`
//!   - **MIT License**, Copyright (c) 2026 Packerlschupfer
//! - `LeadSun/lamzu-cfg` (<https://github.com/LeadSun/lamzu-cfg>)
//!   - `src/atlantis.rs`, `src/atlantis/hid.rs`, `src/device.rs`
//!   - **MIT / Apache-2.0 License**
//! - `anany1702/lamzu-ctl` (<https://github.com/anany1702/lamzu-ctl>)
//!   - `lamzu_ctl.py`
//!   - **MIT License**, Copyright (c) 2026 Misra
//!
//! No HID bytes are guessed. Destructive commands (such as `0x09` factory reset) are never issued.
//!
//! # Flash-Wear Protection
//! `CMD_MEM_SET (0x07)` writes to the active profile's onboard EEPROM/flash storage.
//! `supports_volatile_writes` is therefore `false`: [`crate::devices::SafeDeviceController`]
//! diffs settings and enforces a write cooldown, and [`CompxDevice::apply_settings`] writes
//! only individual registers whose value actually changed.

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    BatteryStatus, DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice,
    MouseSettings, SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};
use crate::hid::parse_hid_report_descriptor;

// --- Shared Compx / Nordic Transport Signature ---
pub const COMPX_VID: u16 = 0x3554;
pub const PULSAR_SONIX_VID: u16 = 0x3710;
pub const GWOLVES_VID: u16 = 0x33E4;
pub const HITSCAN_VID: u16 = 0x3770;

pub const X2_CRAZYLIGHT_WIRELESS_PID: u16 = 0x5406;
pub const X2_CRAZYLIGHT_WIRED_PID: u16 = 0x3414;

pub const LAMZU_4K_PID: u16 = 0xF510;
pub const LAMZU_1K_PID: u16 = 0xF50D;
pub const LAMZU_WIRED_PID: u16 = 0xF50F;

pub const PULSAR_NORDIC_WIRED_PID: u16 = 0xF507;
pub const PULSAR_NORDIC_WIRELESS_PID: u16 = 0xF508;
pub const PULSAR_NORDIC_ALT_PID: u16 = 0xF509;

pub const VGN_F2_WIRELESS_PID: u16 = 0xFB56;
pub const VGN_F2_WIRED_PID: u16 = 0xFB57;

pub const SCYROX_V8_WIRELESS_PID: u16 = 0xF5F7;
/// Scyrox V8S uses a separate PAW3955 DPI flash layout at address 6912 (`0x1B00`);
/// excluded from legacy 0x000C CompX fallback so legacy defaults are never misread.
const SCYROX_V8S_3955_PIDS: &[u16] = &[0xF515, 0xF517];

const ATK_VXE_COMPX_PIDS: &[u16] = &[0xF58A, 0xF58C, 0xF58E, 0xF58F];

/// G-Wolves XVI:0 mice speaking the 17-byte `Report ID 0x08` 0x55-checksum protocol
/// (HTX Ultra, HTS Plus / Pro, Lycan, Fenrir Pro/Asym, HTX Mini, WARG).
const GWOLVES_XVI0_MODELS: &[(u16, &str, bool)] = &[
    (0x3854, "G-Wolves HTX Ultra Wireless", false),
    (0x5618, "G-Wolves HTX Ultra Wired", true),
    (0x5219, "G-Wolves HTS Plus Pro Wired", true),
    (0x5418, "G-Wolves HTS Plus Wired", true),
    (0x4718, "G-Wolves Lycan Wired", true),
    (0x4719, "G-Wolves Lycan Wired", true),
    (0x5619, "G-Wolves HTX Ultra Wired", true),
    (0x5419, "G-Wolves HTS Plus Wired", true),
    (0x3619, "G-Wolves Fenrir Pro Wired", true),
    (0x2719, "G-Wolves HTX Mini Wired", true),
    (0x4219, "G-Wolves WARG Wired", true),
    (0x2619, "G-Wolves HTX Mini Asym Wired", true),
    (0x3519, "G-Wolves Fenrir Asym Wired", true),
];

/// Vendor-defined Top-Level Collection shared by Compx / Nordic 17-byte config devices.
pub const COMPX_USAGE_PAGE: u16 = 0xFF02;
pub const COMPX_USAGE: u16 = 0x0002;
pub const REPORT_ID: u8 = 0x08;
pub const REPORT_LEN: usize = 17;

// Identity pairs answered by `CMD_HANDSHAKE (0x01)` on the shared Pulsar CrazyLight dongle:
// - (0x57, 0x01): Pulsar X2 CrazyLight Mini (confirmed via live `polled --detect`)
// - (0x57, 0x03): Pulsar X2 CrazyLight (documented in packerlschupfer/pulsar-mouse-linux)
// - (0x57, 0x68): Pulsar X3 LHD CrazyLight Medium (non-X2 model, skipped here)
pub const X2_CRAZYLIGHT_CID: u8 = 0x57;
pub const X2_CRAZYLIGHT_MINI_MID: u8 = 0x01;
pub const X2_CRAZYLIGHT_MID: u8 = 0x03;
const X3_LHD_CRAZYLIGHT_MID: u8 = 0x68;

// --- Command IDs ---
const CMD_HANDSHAKE: u8 = 0x01;
const CMD_BATTERY: u8 = 0x04;
const CMD_MEM_SET: u8 = 0x07;
const CMD_MEM_GET: u8 = 0x08;
const CMD_EVENT: u8 = 0x0A;

// --- Shared Active-Profile Register Addresses ---
const ADDR_POLLING_RATE: u16 = 0x0000;
const ADDR_ACTIVE_DPI_STAGE: u16 = 0x0004;
const ADDR_LOD_MM: u16 = 0x000A;
const ADDR_DPI_BASE: u16 = 0x000C;
const DPI_STAGE_STRIDE: u16 = 4;
const ADDR_DEBOUNCE: u16 = 0x00A9;
const ADDR_MOTION_SYNC: u16 = 0x00AB;
const ADDR_SLEEP_TIME: u16 = 0x00AD;
const ADDR_ANGLE_SNAPPING: u16 = 0x00AF;
const ADDR_RIPPLE_CONTROL: u16 = 0x00B1;

const READ_TIMEOUT_MS: i32 = 1000;
const MAX_SKIPPED_REPORTS: usize = 8;

/// DPI encoding scheme used by a Compx / Nordic firmware variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpiEncoding {
    /// Multi-mode 10/50/100 DPI encoding up to 32000 DPI (Pulsar X2 CrazyLight).
    MultiMode10Step,
    /// Standard Compx single-mode 50 DPI step up to 26000 DPI (LAMZU, Pulsar Nordic, generic Compx).
    SingleMode50Step,
}

/// Model-specific capability parameters layered on top of the shared Compx 17-byte protocol.
#[derive(Debug, Clone)]
pub struct CompxVariantSpec {
    pub id_slug: String,
    pub display_name: String,
    pub vendor: String,
    pub connection_label: String,
    pub wired_1k_cap: bool,
    pub dpi_encoding: DpiEncoding,
    pub dpi_min: u16,
    pub dpi_max: u16,
    pub dpi_step: u16,
    pub max_dpi_stages: u8,
    pub polling_rates_hz: Vec<u16>,
    pub lod_options: Vec<LiftOffDistance>,
    pub debounce_max_ms: u8,
    pub support_tier: SupportTier,
}

/// Computes the Compx / Nordic complement checksum: `(0x55 - sum(bytes)) & 0xFF`.
///
/// Note: when `bytes[0]` is `REPORT_ID (0x08)`, this is algebraically identical to
/// `(0x4D - sum(bytes[1..16])) & 0xFF` (`0x55 - 0x08 == 0x4D`).
pub fn checksum(bytes: &[u8]) -> u8 {
    let sum = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    0x55u8.wrapping_sub(sum)
}

/// Builds a 17-byte Report ID `0x08` command frame with its trailing byte-16 checksum.
pub fn build_packet(cmd: u8, addr: u16, len: u8, payload: &[u8]) -> [u8; REPORT_LEN] {
    let mut pkt = [0u8; REPORT_LEN];
    pkt[0] = REPORT_ID;
    pkt[1] = cmd;
    pkt[2] = 0x00;
    pkt[3] = (addr >> 8) as u8;
    pkt[4] = (addr & 0xFF) as u8;
    pkt[5] = len;
    let copy_len = payload.len().min(10);
    pkt[6..6 + copy_len].copy_from_slice(&payload[..copy_len]);
    pkt[16] = checksum(&pkt[..16]);
    pkt
}

pub fn verify_packet_checksum(pkt: &[u8]) -> bool {
    pkt.len() >= REPORT_LEN && pkt[0] == REPORT_ID && checksum(&pkt[..16]) == pkt[16]
}

/// Encodes polling rate Hz into register `0x0000`.
pub fn encode_polling_rate(hz: u16) -> Option<u8> {
    match hz {
        1000 => Some(0x01),
        500 => Some(0x02),
        250 => Some(0x04),
        125 => Some(0x08),
        2000 => Some(0x10),
        4000 => Some(0x20),
        8000 => Some(0x40),
        _ => None,
    }
}

/// Decodes register `0x0000` into polling rate Hz.
pub fn decode_polling_rate(code: u8) -> Option<u16> {
    match code {
        0x01 => Some(1000),
        0x02 => Some(500),
        0x04 => Some(250),
        0x08 => Some(125),
        0x10 => Some(2000),
        0x20 => Some(4000),
        0x40 => Some(8000),
        _ => None,
    }
}

/// Encodes lift-off distance into register `0x000A`.
pub fn encode_lod(lod: LiftOffDistance) -> u8 {
    match lod {
        LiftOffDistance::Mm10 => 0x01,
        LiftOffDistance::Mm20 => 0x02,
        LiftOffDistance::Mm07 => 0x03,
    }
}

/// Decodes register `0x000A` into lift-off distance.
pub fn decode_lod(code: u8) -> Option<LiftOffDistance> {
    match code {
        0x01 => Some(LiftOffDistance::Mm10),
        0x02 => Some(LiftOffDistance::Mm20),
        0x03 => Some(LiftOffDistance::Mm07),
        _ => None,
    }
}

fn rot2_nibble(n: u8) -> u8 {
    ((n << 2) | (n >> 2)) & 0x0F
}

fn encode_dpi_axis(dpi: u16, encoding: DpiEncoding) -> Option<(u8, u8)> {
    match encoding {
        DpiEncoding::MultiMode10Step => {
            if !(10..=32000).contains(&dpi) {
                return None;
            }
            let pick = |val: u32| -> (u8, u32, u32) {
                if val <= 10240 {
                    (0, 10, 1)
                } else if val <= 25600 {
                    (2, 50, 201)
                } else {
                    (3, 100, 201)
                }
            };
            let (mut mode, mut step, mut base) = pick(u32::from(dpi));
            let snapped = ((u32::from(dpi) + step / 2) / step) * step;
            if snapped != u32::from(dpi) {
                (mode, step, base) = pick(snapped);
            }
            let units = snapped / step;
            if units < base {
                return None;
            }
            let index = units - base;
            if index > 1023 {
                return None;
            }
            let page = (index / 256) as u8;
            let low = (index % 256) as u8;
            let nib = rot2_nibble((mode << 2) | page);
            Some((low, nib))
        }
        DpiEncoding::SingleMode50Step => {
            if !(50..=30000).contains(&dpi) {
                return None;
            }
            let units = u32::from(dpi) / 50;
            if units == 0 {
                return None;
            }
            let index = units - 1;
            if index > 1023 {
                return None;
            }
            let page = (index / 256) as u8;
            let low = (index % 256) as u8;
            let nib = rot2_nibble(page);
            Some((low, nib))
        }
    }
}

fn decode_dpi_axis(low: u8, raw_nib: u8, encoding: DpiEncoding) -> Option<u16> {
    let high = rot2_nibble(raw_nib & 0x0F);
    let mode = high >> 2;
    let page = u32::from(high & 0x03);
    let (step, base) = match encoding {
        DpiEncoding::MultiMode10Step => match mode {
            0 => (10u32, 1u32),
            2 => (50u32, 201u32),
            3 => (100u32, 201u32),
            _ => return None,
        },
        DpiEncoding::SingleMode50Step => (50u32, 1u32),
    };
    let dpi = (u32::from(low) + base + 256 * page) * step;
    u16::try_from(dpi).ok()
}

/// Encodes independent X/Y DPI into `[x_lo, y_lo, (y_nib << 4) | x_nib]`.
pub fn encode_dpi_xy_raw(dpi_x: u16, dpi_y: u16, encoding: DpiEncoding) -> Option<[u8; 3]> {
    let (x_lo, x_nib) = encode_dpi_axis(dpi_x, encoding)?;
    let (y_lo, y_nib) = encode_dpi_axis(dpi_y, encoding)?;
    Some([x_lo, y_lo, (y_nib << 4) | x_nib])
}

/// Decodes `[x_lo, y_lo, high]` into `(dpi_x, dpi_y)`.
pub fn decode_dpi_xy_raw(raw: [u8; 3], encoding: DpiEncoding) -> Option<(u16, u16)> {
    let x = decode_dpi_axis(raw[0], raw[2] & 0x0F, encoding)?;
    let y = decode_dpi_axis(raw[1], (raw[2] >> 4) & 0x0F, encoding)?;
    Some((x, y))
}

/// Encodes uniform DPI into the 3 data bytes `[x_lo, y_lo, high]` according to `encoding`.
pub fn encode_dpi_raw(dpi: u16, encoding: DpiEncoding) -> Option<[u8; 3]> {
    encode_dpi_xy_raw(dpi, dpi, encoding)
}

/// Decodes the 3 data bytes `[x_lo, y_lo, high]` according to `encoding` (X axis).
pub fn decode_dpi_raw(raw: [u8; 3], encoding: DpiEncoding) -> Option<u16> {
    decode_dpi_xy_raw(raw, encoding).map(|(x, _)| x)
}

/// Resolves the Compx/Nordic capability spec from USB `(vid, pid)` and OS product/manufacturer strings.
pub fn resolve_variant_spec(
    vid: u16,
    pid: u16,
    manufacturer: Option<&str>,
    product: Option<&str>,
) -> Option<CompxVariantSpec> {
    match (vid, pid) {
        (PULSAR_SONIX_VID, X2_CRAZYLIGHT_WIRELESS_PID) => Some(CompxVariantSpec {
            id_slug: "pulsar_x2_crazylight_wireless".to_string(),
            display_name: "Pulsar X2 CrazyLight Wireless".to_string(),
            vendor: "Pulsar".to_string(),
            connection_label: "Wireless 2.4 GHz (8K Dongle)".to_string(),
            wired_1k_cap: false,
            dpi_encoding: DpiEncoding::MultiMode10Step,
            dpi_min: 10,
            dpi_max: 32000,
            dpi_step: 10,
            max_dpi_stages: 6,
            polling_rates_hz: vec![125, 250, 500, 1000, 2000, 4000, 8000],
            lod_options: vec![
                LiftOffDistance::Mm07,
                LiftOffDistance::Mm10,
                LiftOffDistance::Mm20,
            ],
            debounce_max_ms: 20,
            support_tier: SupportTier::ImplementedFromDocs,
        }),
        (PULSAR_SONIX_VID, X2_CRAZYLIGHT_WIRED_PID) => Some(CompxVariantSpec {
            id_slug: "pulsar_x2_crazylight_wired".to_string(),
            display_name: "Pulsar X2 CrazyLight Wired".to_string(),
            vendor: "Pulsar".to_string(),
            connection_label: "Wired USB-C (1K max)".to_string(),
            wired_1k_cap: true,
            dpi_encoding: DpiEncoding::MultiMode10Step,
            dpi_min: 10,
            dpi_max: 32000,
            dpi_step: 10,
            max_dpi_stages: 6,
            polling_rates_hz: vec![125, 250, 500, 1000],
            lod_options: vec![
                LiftOffDistance::Mm07,
                LiftOffDistance::Mm10,
                LiftOffDistance::Mm20,
            ],
            debounce_max_ms: 20,
            support_tier: SupportTier::ImplementedFromDocs,
        }),
        (COMPX_VID, LAMZU_4K_PID) => Some(CompxVariantSpec {
            id_slug: "lamzu_compx_4k".to_string(),
            display_name: product
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("LAMZU 4K Wireless")
                .to_string(),
            vendor: "LAMZU / Compx".to_string(),
            connection_label: "Wireless 2.4 GHz (4K Dongle)".to_string(),
            wired_1k_cap: false,
            dpi_encoding: DpiEncoding::SingleMode50Step,
            dpi_min: 50,
            dpi_max: 26000,
            dpi_step: 50,
            max_dpi_stages: 8,
            polling_rates_hz: vec![125, 250, 500, 1000, 2000, 4000],
            lod_options: vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20],
            debounce_max_ms: 15,
            // Confirmed detect, read, write, read-back, hot-plug & focus auto-switch on hardware.
            support_tier: SupportTier::TestedOnHardware,
        }),
        (COMPX_VID, LAMZU_1K_PID | LAMZU_WIRED_PID) => Some(CompxVariantSpec {
            id_slug: format!("lamzu_compx_{pid:04x}"),
            display_name: product
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(if pid == LAMZU_WIRED_PID {
                    "LAMZU Wired"
                } else {
                    "LAMZU 1K Wireless"
                })
                .to_string(),
            vendor: "LAMZU / Compx".to_string(),
            connection_label: if pid == LAMZU_WIRED_PID {
                "Wired USB (1K)".to_string()
            } else {
                "Wireless 2.4 GHz (1K Dongle)".to_string()
            },
            wired_1k_cap: pid == LAMZU_WIRED_PID,
            dpi_encoding: DpiEncoding::SingleMode50Step,
            dpi_min: 50,
            dpi_max: 26000,
            dpi_step: 50,
            max_dpi_stages: 8,
            polling_rates_hz: vec![125, 250, 500, 1000],
            lod_options: vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20],
            debounce_max_ms: 15,
            support_tier: SupportTier::ImplementedFromDocs,
        }),
        (
            COMPX_VID,
            PULSAR_NORDIC_WIRED_PID | PULSAR_NORDIC_WIRELESS_PID | PULSAR_NORDIC_ALT_PID,
        ) => Some(CompxVariantSpec {
            id_slug: format!("pulsar_nordic_{pid:04x}"),
            display_name: product
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("Pulsar Nordic Mouse")
                .to_string(),
            vendor: "Pulsar / Compx".to_string(),
            connection_label: if pid == PULSAR_NORDIC_WIRED_PID {
                "Wired USB".to_string()
            } else {
                "Wireless 2.4 GHz".to_string()
            },
            wired_1k_cap: pid == PULSAR_NORDIC_WIRED_PID,
            dpi_encoding: DpiEncoding::SingleMode50Step,
            dpi_min: 50,
            dpi_max: 26000,
            dpi_step: 50,
            max_dpi_stages: 4,
            polling_rates_hz: vec![125, 250, 500, 1000],
            lod_options: vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20],
            debounce_max_ms: 30,
            support_tier: SupportTier::ImplementedFromDocs,
        }),
        // VGN Dragonfly F2 Master+ (3554:fb56 wireless / 3554:fb57 wired)
        (COMPX_VID, VGN_F2_WIRELESS_PID | VGN_F2_WIRED_PID) => Some(CompxVariantSpec {
            id_slug: format!("vgn_f2_{pid:04x}"),
            display_name: product
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("VGN Dragonfly F2 Master+")
                .to_string(),
            vendor: "VGN".to_string(),
            connection_label: if pid == VGN_F2_WIRED_PID {
                "Wired USB (8K)".to_string()
            } else {
                "Wireless 2.4 GHz (8K Receiver)".to_string()
            },
            wired_1k_cap: false,
            dpi_encoding: DpiEncoding::SingleMode50Step,
            dpi_min: 50,
            dpi_max: 26000,
            dpi_step: 50,
            max_dpi_stages: 8,
            polling_rates_hz: vec![125, 250, 500, 1000, 2000, 4000, 8000],
            lod_options: LiftOffDistance::ALL.to_vec(),
            debounce_max_ms: 30,
            support_tier: SupportTier::ImplementedFromDocs,
        }),
        // Scyrox V8 (3554:f5f7, PAW3950 legacy DPI layout, up to 30,000 DPI & 8K Hz)
        (COMPX_VID, SCYROX_V8_WIRELESS_PID) => Some(CompxVariantSpec {
            id_slug: "scyrox_v8_8k".to_string(),
            display_name: product
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("Scyrox V8 8K")
                .to_string(),
            vendor: "Scyrox".to_string(),
            connection_label: "Wireless 2.4 GHz (8K Dongle)".to_string(),
            wired_1k_cap: false,
            dpi_encoding: DpiEncoding::SingleMode50Step,
            dpi_min: 50,
            dpi_max: 30000,
            dpi_step: 50,
            max_dpi_stages: 5,
            polling_rates_hz: vec![125, 250, 500, 1000, 2000, 4000, 8000],
            lod_options: LiftOffDistance::ALL.to_vec(),
            debounce_max_ms: 15,
            support_tier: SupportTier::ImplementedFromDocs,
        }),
        // Scyrox V8S (PAW3955 flash 6912 layout) is deliberately skipped by legacy 0x000C fallback.
        (COMPX_VID, p) if SCYROX_V8S_3955_PIDS.contains(&p) => None,
        // ATK / VXE R1 CompX transports (3554:f58a/f58c/f58e/f58f)
        (COMPX_VID, p) if ATK_VXE_COMPX_PIDS.contains(&p) => {
            let wired = matches!(p, 0xF58C | 0xF58F);
            Some(CompxVariantSpec {
                id_slug: format!("vxe_r1_compx_{p:04x}"),
                display_name: product
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("VXE R1 Series")
                    .to_string(),
                vendor: "ATK / VXE".to_string(),
                connection_label: if wired {
                    "Wired USB".to_string()
                } else {
                    "Wireless 2.4 GHz Receiver".to_string()
                },
                wired_1k_cap: wired,
                dpi_encoding: DpiEncoding::SingleMode50Step,
                dpi_min: 50,
                dpi_max: 26000,
                dpi_step: 50,
                max_dpi_stages: 8,
                polling_rates_hz: if wired {
                    vec![125, 250, 500, 1000]
                } else {
                    vec![125, 250, 500, 1000, 2000, 4000]
                },
                lod_options: vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20],
                debounce_max_ms: 20,
                support_tier: SupportTier::ImplementedFromDocs,
            })
        }
        // G-Wolves XVI:0 mice (0x33E4: HTX Ultra, HTS Plus / Pro, Lycan, Fenrir, HTX Mini, WARG)
        (GWOLVES_VID, p) => {
            let &(_, model_name, wired) =
                GWOLVES_XVI0_MODELS.iter().find(|&&(id, _, _)| id == p)?;
            Some(CompxVariantSpec {
                id_slug: format!("gwolves_{p:04x}"),
                display_name: model_name.to_string(),
                vendor: "G-Wolves".to_string(),
                connection_label: if wired {
                    "Wired USB (XVI:0)".to_string()
                } else {
                    "Wireless 2.4 GHz Receiver (8K)".to_string()
                },
                wired_1k_cap: false,
                dpi_encoding: DpiEncoding::SingleMode50Step,
                dpi_min: 50,
                dpi_max: 26000,
                dpi_step: 50,
                max_dpi_stages: 8,
                polling_rates_hz: vec![125, 250, 500, 1000, 2000, 4000, 8000],
                lod_options: LiftOffDistance::ALL.to_vec(),
                debounce_max_ms: 20,
                support_tier: SupportTier::ImplementedFromDocs,
            })
        }
        // Hitscan Hyperlight (0x3770:0100 wired / 0200 1K / 0300 8K)
        (HITSCAN_VID, p @ (0x0100 | 0x0200 | 0x0300)) => {
            let (name, conn, rates) = match p {
                0x0100 => (
                    "Hitscan Hyperlight Wired",
                    "Wired USB",
                    vec![125, 250, 500, 1000],
                ),
                0x0200 => (
                    "Hitscan Hyperlight (1K)",
                    "Wireless 2.4 GHz (1K Receiver)",
                    vec![125, 250, 500, 1000],
                ),
                _ => (
                    "Hitscan Hyperlight (8K)",
                    "Wireless 2.4 GHz (8K Receiver)",
                    vec![125, 250, 500, 1000, 2000, 4000, 8000],
                ),
            };
            Some(CompxVariantSpec {
                id_slug: format!("hitscan_{p:04x}"),
                display_name: name.to_string(),
                vendor: "Hitscan".to_string(),
                connection_label: conn.to_string(),
                wired_1k_cap: p == 0x0100,
                dpi_encoding: DpiEncoding::SingleMode50Step,
                dpi_min: 50,
                dpi_max: 26000,
                dpi_step: 50,
                max_dpi_stages: 4,
                polling_rates_hz: rates,
                lod_options: vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20],
                debounce_max_ms: 20,
                support_tier: SupportTier::ImplementedFromDocs,
            })
        }
        // Generic auto-detection for any other Compx (0x3554) mouse exposing 0xFF02 Report 0x08 17B.
        (COMPX_VID, _) => {
            let name = product
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("Compx CX52850 Mouse")
                .to_string();
            let lower = name.to_ascii_lowercase();
            let polling_rates_hz = if lower.contains("8k") {
                vec![125, 250, 500, 1000, 2000, 4000, 8000]
            } else if lower.contains("4k") {
                vec![125, 250, 500, 1000, 2000, 4000]
            } else {
                vec![125, 250, 500, 1000]
            };
            Some(CompxVariantSpec {
                id_slug: format!("compx_{vid:04x}_{pid:04x}"),
                display_name: name,
                vendor: manufacturer
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("Compx")
                    .to_string(),
                connection_label: "Compx 17B HID (Auto-detected)".to_string(),
                wired_1k_cap: false,
                dpi_encoding: DpiEncoding::SingleMode50Step,
                dpi_min: 50,
                dpi_max: 26000,
                dpi_step: 50,
                max_dpi_stages: 6,
                polling_rates_hz,
                lod_options: vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20],
                debounce_max_ms: 15,
                support_tier: SupportTier::ImplementedFromDocs,
            })
        }
        _ => None,
    }
}

fn resolve_crazylight_model_name(cid: u8, mid: u8, wired: bool) -> Option<String> {
    let suffix = if wired { "Wired" } else { "Wireless" };
    match (cid, mid) {
        (X2_CRAZYLIGHT_CID, X2_CRAZYLIGHT_MINI_MID) => {
            Some(format!("Pulsar X2 CrazyLight Mini {suffix}"))
        }
        (X2_CRAZYLIGHT_CID, X2_CRAZYLIGHT_MID) => Some(format!("Pulsar X2 CrazyLight {suffix}")),
        (X2_CRAZYLIGHT_CID, X3_LHD_CRAZYLIGHT_MID) => None,
        _ => Some(format!(
            "Pulsar CrazyLight {suffix} (cid=0x{cid:02x}, mid=0x{mid:02x})"
        )),
    }
}

/// Hardware backend for any mouse speaking the Compx CX52850 / Nordic 17-byte `0x08` protocol.
pub struct CompxDevice {
    hid: HidDevice,
    vid_pid: String,
    spec: CompxVariantSpec,
}

impl CompxDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.spec.display_name.clone(),
            message: message.into(),
        }
    }

    fn send_command(
        &mut self,
        cmd: u8,
        addr: u16,
        len: u8,
        payload: &[u8],
    ) -> Result<[u8; REPORT_LEN]> {
        let pkt = build_packet(cmd, addr, len, payload);
        self.hid
            .write(&pkt)
            .map_err(|e| self.dev_err(format!("HID write failed for cmd 0x{cmd:02x}: {e}")))?;

        let mut buf = [0u8; 64];
        for _ in 0..MAX_SKIPPED_REPORTS {
            let n = self
                .hid
                .read_timeout(&mut buf, READ_TIMEOUT_MS)
                .map_err(|e| self.dev_err(format!("HID read failed for cmd 0x{cmd:02x}: {e}")))?;
            if n == 0 {
                return Err(self.dev_err(format!(
                    "Timed out waiting for response to cmd 0x{cmd:02x} (move mouse to wake it)"
                )));
            }
            if n < REPORT_LEN || !verify_packet_checksum(&buf[..REPORT_LEN]) {
                continue;
            }
            if buf[1] == CMD_EVENT || buf[1] != cmd {
                continue;
            }
            if cmd == CMD_MEM_GET && buf[3..6] != pkt[3..6] {
                continue;
            }
            let mut reply = [0u8; REPORT_LEN];
            reply.copy_from_slice(&buf[..REPORT_LEN]);
            return Ok(reply);
        }

        Err(self.dev_err(format!(
            "No matching reply for cmd 0x{cmd:02x} after {MAX_SKIPPED_REPORTS} packets"
        )))
    }

    fn mem_read(&mut self, addr: u16, len: u8) -> Result<Vec<u8>> {
        let n = len.min(10);
        let reply = self.send_command(CMD_MEM_GET, addr, n, &[])?;
        Ok(reply[6..6 + usize::from(n)].to_vec())
    }

    fn mem_write(&mut self, addr: u16, data: &[u8]) -> Result<()> {
        if data.is_empty() || data.len() > 10 {
            return Err(self.dev_err(format!(
                "Invalid register write length {} at 0x{addr:04x}",
                data.len()
            )));
        }
        let reply = self.send_command(CMD_MEM_SET, addr, data.len() as u8, data)?;
        if reply[2] != 0x00 {
            return Err(self.dev_err(format!(
                "Device rejected write at 0x{addr:04x} (status 0x{:02x})",
                reply[2]
            )));
        }
        Ok(())
    }

    fn write_byte_with_checksum(&mut self, addr: u16, value: u8) -> Result<()> {
        self.mem_write(addr, &[value, checksum(&[value])])
    }

    pub fn query_model_ids(&mut self) -> Result<(u8, u8)> {
        let nonce = [0x50u8, 0x4F, 0x4C, 0x44];
        let reply = self.send_command(CMD_HANDSHAKE, 0, 0x08, &nonce)?;
        if reply[2] != 0x00 || reply[10] == 0 || reply[11] == 0 {
            return Err(self.dev_err(format!(
                "Handshake did not identify model (status=0x{:02x}, cid=0x{:02x}, mid=0x{:02x})",
                reply[2], reply[10], reply[11]
            )));
        }
        Ok((reply[10], reply[11]))
    }

    fn read_stored_polling_rate(&mut self) -> Result<u16> {
        let bytes = self.mem_read(ADDR_POLLING_RATE, 2)?;
        decode_polling_rate(bytes[0]).ok_or_else(|| {
            self.dev_err(format!("Unrecognized polling rate code 0x{:02x}", bytes[0]))
        })
    }

    fn read_active_dpi_stage(&mut self) -> Result<u8> {
        let bytes = self.mem_read(ADDR_ACTIVE_DPI_STAGE, 2)?;
        Ok(bytes[0].min(self.spec.max_dpi_stages.saturating_sub(1)))
    }

    fn is_wireless(&self) -> bool {
        !self.spec.wired_1k_cap
            && !self
                .spec
                .connection_label
                .to_ascii_lowercase()
                .contains("wired")
    }

    fn read_bool_scalar(&mut self, addr: u16) -> Option<bool> {
        let bytes = self.mem_read(addr, 2).ok()?;
        if bytes.len() < 2 || checksum(&[bytes[0]]) != bytes[1] {
            return None;
        }
        Some(bytes[0] != 0)
    }

    fn read_sleep_timeout_secs(&mut self) -> Option<u16> {
        if !self.is_wireless() {
            return None;
        }
        let bytes = self.mem_read(ADDR_SLEEP_TIME, 2).ok()?;
        if bytes.len() < 2 || checksum(&[bytes[0]]) != bytes[1] || bytes[0] == 0 {
            return None;
        }
        Some(u16::from(bytes[0]) * 10)
    }
}

/// Decodes a 17-byte Compx `CMD_BATTERY (0x04)` reply (`[0x08, 0x04, status, ..., pct, chg, v_hi, v_lo]`).
pub fn decode_compx_battery(reply: &[u8]) -> Option<BatteryStatus> {
    if reply.len() < REPORT_LEN || !verify_packet_checksum(reply) {
        return None;
    }
    if reply[1] != CMD_BATTERY || reply[2] != 0x00 {
        return None;
    }
    let percent = reply[6];
    if percent > 100 {
        return None;
    }
    let charging = reply[7] == 1;
    let raw_mv = u16::from_be_bytes([reply[8], reply[9]]);
    Some(BatteryStatus {
        percent,
        charging,
        voltage_mv: (raw_mv > 0).then_some(raw_mv),
    })
}

impl MouseDevice for CompxDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: self.spec.id_slug.clone(),
            name: self.spec.display_name.clone(),
            vendor: self.spec.vendor.clone(),
            vid_pid: Some(self.vid_pid.clone()),
            connection: self.spec.connection_label.clone(),
            support_tier: self.spec.support_tier,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let wireless = self.is_wireless();
        DeviceCapabilities {
            dpi_min: self.spec.dpi_min,
            dpi_max: self.spec.dpi_max,
            dpi_step: self.spec.dpi_step,
            supports_xy_dpi: true,
            polling_rates_hz: self.spec.polling_rates_hz.clone(),
            lod_options: self.spec.lod_options.clone(),
            supports_debounce: true,
            debounce_min_ms: 0,
            debounce_max_ms: self.spec.debounce_max_ms,
            supports_motion_sync: true,
            supports_angle_snapping: true,
            supports_ripple_control: true,
            supports_sleep_timeout: wireless,
            sleep_timeout_min_secs: if wireless { 10 } else { 0 },
            sleep_timeout_max_secs: if wireless { 1800 } else { 0 },
            supports_volatile_writes: false,
            flash_write_warning: Some(format!(
                "{} uses Compx/Nordic onboard profile flash (no volatile RAM-only write command). Polled diffs every register and rate-limits writes to prevent flash wear.",
                self.spec.display_name
            )),
        }
    }

    fn read_battery(&mut self) -> Option<BatteryStatus> {
        let reply = self.send_command(CMD_BATTERY, 0, 0, &[]).ok()?;
        decode_compx_battery(&reply)
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        let stored_hz = self.read_stored_polling_rate()?;
        let effective_hz = if self.spec.wired_1k_cap {
            stored_hz.min(1000)
        } else {
            stored_hz
        };

        let active_stage = self.read_active_dpi_stage()?;
        let stage_addr = ADDR_DPI_BASE + u16::from(active_stage) * DPI_STAGE_STRIDE;
        let dpi_bytes = self.mem_read(stage_addr, 4)?;
        let (dpi_x, dpi_y) = decode_dpi_xy_raw(
            [dpi_bytes[0], dpi_bytes[1], dpi_bytes[2]],
            self.spec.dpi_encoding,
        )
        .ok_or_else(|| {
            self.dev_err(format!(
                "Failed to decode DPI stage bytes {:02x?}",
                &dpi_bytes[..3]
            ))
        })?;

        let lod_bytes = self.mem_read(ADDR_LOD_MM, 2)?;
        let lod = decode_lod(lod_bytes[0])
            .ok_or_else(|| self.dev_err(format!("Unrecognized LOD code 0x{:02x}", lod_bytes[0])))?;

        let debounce_bytes = self.mem_read(ADDR_DEBOUNCE, 2)?;
        let debounce_ms = debounce_bytes[0];

        let motion_sync = self.read_bool_scalar(ADDR_MOTION_SYNC);
        let angle_snapping = self.read_bool_scalar(ADDR_ANGLE_SNAPPING);
        let ripple_control = self.read_bool_scalar(ADDR_RIPPLE_CONTROL);
        let sleep_timeout_secs = self.read_sleep_timeout_secs();

        Ok(MouseSettings {
            dpi: Some(dpi_x),
            dpi_y: (dpi_y != dpi_x).then_some(dpi_y),
            polling_rate_hz: Some(effective_hz),
            lod: Some(lod),
            debounce_ms: Some(debounce_ms),
            motion_sync,
            angle_snapping,
            ripple_control,
            sleep_timeout_secs,
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        let current = self.read_settings()?;

        if let Some(target_hz) = settings.polling_rate_hz {
            if current.polling_rate_hz != Some(target_hz) {
                let skip_wired_cap = self.spec.wired_1k_cap
                    && target_hz == 1000
                    && self.read_stored_polling_rate()? > 1000;

                if !skip_wired_cap {
                    let code = encode_polling_rate(target_hz).ok_or_else(|| {
                        PolledError::UnsupportedSetting {
                            device: self.spec.display_name.clone(),
                            detail: format!("Unsupported polling rate {target_hz} Hz"),
                        }
                    })?;
                    self.write_byte_with_checksum(ADDR_POLLING_RATE, code)?;
                }
            }
        }

        if settings.dpi.is_some() || settings.dpi_y.is_some() {
            let cur_x = current.dpi.unwrap_or(800);
            let cur_y = current.dpi_y.unwrap_or(cur_x);
            let target_x = settings.dpi.unwrap_or(cur_x);
            let target_y = settings.dpi_y.or(settings.dpi).unwrap_or(cur_y);
            if target_x != cur_x || target_y != cur_y {
                let raw = encode_dpi_xy_raw(target_x, target_y, self.spec.dpi_encoding)
                    .ok_or_else(|| PolledError::UnsupportedSetting {
                        device: self.spec.display_name.clone(),
                        detail: format!(
                            "DPI {target_x}x{target_y} cannot be encoded for {}",
                            self.spec.display_name
                        ),
                    })?;
                let active_stage = self.read_active_dpi_stage()?;
                let stage_addr = ADDR_DPI_BASE + u16::from(active_stage) * DPI_STAGE_STRIDE;
                self.mem_write(stage_addr, &[raw[0], raw[1], raw[2], checksum(&raw)])?;
            }
        }

        if let Some(target_lod) = settings.lod {
            if current.lod != Some(target_lod) {
                self.write_byte_with_checksum(ADDR_LOD_MM, encode_lod(target_lod))?;
            }
        }

        if let Some(target_db) = settings.debounce_ms {
            if current.debounce_ms != Some(target_db) {
                self.write_byte_with_checksum(ADDR_DEBOUNCE, target_db)?;
            }
        }

        if let Some(target_ms) = settings.motion_sync {
            if current.motion_sync != Some(target_ms) {
                self.write_byte_with_checksum(ADDR_MOTION_SYNC, u8::from(target_ms))?;
            }
        }

        if let Some(target_ang) = settings.angle_snapping {
            if current.angle_snapping != Some(target_ang) {
                self.write_byte_with_checksum(ADDR_ANGLE_SNAPPING, u8::from(target_ang))?;
            }
        }

        if let Some(target_rip) = settings.ripple_control {
            if current.ripple_control != Some(target_rip) {
                self.write_byte_with_checksum(ADDR_RIPPLE_CONTROL, u8::from(target_rip))?;
            }
        }

        if let Some(target_sleep) = settings.sleep_timeout_secs {
            if self.is_wireless() && current.sleep_timeout_secs != Some(target_sleep) {
                let units = ((target_sleep.saturating_add(5)) / 10).clamp(1, 180) as u8;
                self.write_byte_with_checksum(ADDR_SLEEP_TIME, units)?;
            }
        }

        Ok(())
    }
}

/// Protocol-family auto-detector for all Compx CX52850 / Nordic 17-byte `0xFF02` Report-`0x08` mice.
pub struct CompxProtocolDriver;

impl DeviceDriver for CompxProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "Compx / Nordic 17B (0xFF02)"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();

        for dev in api.device_list() {
            // Match the exact Compx/Nordic config collection signature first (zero false positives).
            if dev.usage_page() != COMPX_USAGE_PAGE || dev.usage() != COMPX_USAGE {
                continue;
            }

            let Some(spec) = resolve_variant_spec(
                dev.vendor_id(),
                dev.product_id(),
                dev.manufacturer_string(),
                dev.product_string(),
            ) else {
                continue;
            };

            let Ok(hid) = dev.open_device(api) else {
                continue;
            };

            // Verify this exact collection owns 17-byte Input + Output Report ID 0x08.
            let mut desc_buf = [0u8; 4096];
            let has_17b_report_8 = hid
                .get_report_descriptor(&mut desc_buf)
                .ok()
                .map(|len| parse_hid_report_descriptor(&desc_buf[..len]))
                .is_some_and(|summary| {
                    summary.output_reports.get(&REPORT_ID) == Some(&REPORT_LEN)
                        && summary.input_reports.get(&REPORT_ID) == Some(&REPORT_LEN)
                });
            if !has_17b_report_8 {
                continue;
            }

            let vid_pid = format!("{:04x}:{:04x}", dev.vendor_id(), dev.product_id());
            let mut device = CompxDevice { hid, vid_pid, spec };

            // Refine model name for Pulsar CrazyLight shared dongle/cable via CMD_HANDSHAKE.
            if dev.vendor_id() == PULSAR_SONIX_VID
                && matches!(
                    dev.product_id(),
                    X2_CRAZYLIGHT_WIRELESS_PID | X2_CRAZYLIGHT_WIRED_PID
                )
            {
                if let Ok((cid, mid)) = device.query_model_ids() {
                    match resolve_crazylight_model_name(cid, mid, device.spec.wired_1k_cap) {
                        Some(name) => {
                            device.spec.display_name = name;
                            // Confirmed read + write + read-back + live focus auto-switch on real hardware.
                            if !device.spec.wired_1k_cap
                                && cid == X2_CRAZYLIGHT_CID
                                && mid == X2_CRAZYLIGHT_MINI_MID
                            {
                                device.spec.support_tier = SupportTier::TestedOnHardware;
                            }
                        }
                        None => continue,
                    }
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

    /// Verifies ALL 14 hardware-captured (bytes -> DPI) pairs for Pulsar X2 CrazyLight.
    #[test]
    fn encodes_and_decodes_all_14_crazylight_dpi_values() {
        let cases: &[([u8; 3], u16)] = &[
            ([0x27, 0x27, 0x00], 400),
            ([0x4F, 0x4F, 0x00], 800),
            ([0x9F, 0x9F, 0x00], 1600),
            ([0xEF, 0xEF, 0x00], 2400),
            ([0x3F, 0x3F, 0x44], 3200),
            ([0x7F, 0x7F, 0x88], 6400),
            ([0x1F, 0x1F, 0xCC], 8000),
            ([0xE7, 0xE7, 0xCC], 10000),
            ([0x37, 0x37, 0x22], 12800),
            ([0x77, 0x77, 0x22], 16000),
            ([0xC7, 0xC7, 0x22], 20000),
            ([0x17, 0x17, 0x66], 24000),
            ([0x37, 0x37, 0x66], 25600),
            ([0x77, 0x77, 0x33], 32000),
        ];

        for &(expected_raw, dpi) in cases {
            assert_eq!(
                decode_dpi_raw(expected_raw, DpiEncoding::MultiMode10Step),
                Some(dpi)
            );
            assert_eq!(
                encode_dpi_raw(dpi, DpiEncoding::MultiMode10Step),
                Some(expected_raw)
            );
        }
    }

    /// Verifies standard Compx / LAMZU 50-DPI step encoding (matching LeadSun/lamzu-cfg).
    #[test]
    fn encodes_and_decodes_compx_lamzu_50_step_dpi() {
        for (dpi, expected_raw) in [
            (400u16, [0x07, 0x07, 0x00]),
            (800, [0x0F, 0x0F, 0x00]),
            (1600, [0x1F, 0x1F, 0x00]),
            (3200, [0x3F, 0x3F, 0x00]),
            (16000, [0x3F, 0x3F, 0x44]), // page 1: (0x3F + 1 + 256) * 50 = 320 * 50 = 16000
        ] {
            assert_eq!(
                encode_dpi_raw(dpi, DpiEncoding::SingleMode50Step),
                Some(expected_raw),
                "encode {dpi}"
            );
            assert_eq!(
                decode_dpi_raw(expected_raw, DpiEncoding::SingleMode50Step),
                Some(dpi),
                "decode {dpi}"
            );
        }
    }

    #[test]
    fn resolves_pulsar_lamzu_and_generic_compx_variants() {
        let pulsar =
            resolve_variant_spec(0x3710, 0x5406, Some("Pulsar"), Some("Pulsar 8K Dongle")).unwrap();
        assert_eq!(pulsar.dpi_encoding, DpiEncoding::MultiMode10Step);
        assert!(pulsar.polling_rates_hz.contains(&8000));

        let lamzu =
            resolve_variant_spec(0x3554, 0xF510, Some("Compx"), Some("LAMZU 4K Receiver")).unwrap();
        assert_eq!(lamzu.dpi_encoding, DpiEncoding::SingleMode50Step);
        assert_eq!(lamzu.display_name, "LAMZU 4K Receiver");
        assert!(lamzu.polling_rates_hz.contains(&4000));
        assert!(!lamzu.polling_rates_hz.contains(&8000));

        let generic =
            resolve_variant_spec(0x3554, 0xF999, Some("Compx"), Some("Example 4K Mouse")).unwrap();
        assert_eq!(generic.dpi_encoding, DpiEncoding::SingleMode50Step);
        assert!(generic.polling_rates_hz.contains(&4000));

        assert!(resolve_variant_spec(0x373E, 0x000A, Some("Vancer"), Some("Akitsu")).is_none());
    }

    #[test]
    fn verifies_packet_framing_and_checksum() {
        let pkt = build_packet(
            CMD_MEM_SET,
            ADDR_POLLING_RATE,
            2,
            &[0x20, checksum(&[0x20])],
        );
        assert_eq!(pkt[0], REPORT_ID);
        assert_eq!(pkt[6], 0x20);
        assert_eq!(pkt[7], 0x35);
        assert!(verify_packet_checksum(&pkt));

        // Check algebraic equivalence: 0x55 - sum(pkt[0..16]) == 0x4D - sum(pkt[1..16]).
        let sum_payload = pkt[1..16].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        assert_eq!(pkt[16], 0x4Du8.wrapping_sub(sum_payload));
    }

    #[test]
    fn resolves_crazylight_model_identities() {
        assert_eq!(
            resolve_crazylight_model_name(0x57, 0x01, false).as_deref(),
            Some("Pulsar X2 CrazyLight Mini Wireless")
        );
        assert_eq!(
            resolve_crazylight_model_name(0x57, 0x03, true).as_deref(),
            Some("Pulsar X2 CrazyLight Wired")
        );
        assert_eq!(resolve_crazylight_model_name(0x57, 0x68, false), None);
    }

    #[test]
    fn resolves_wave2_vgn_scyrox_gwolves_hitscan_and_decodes_battery() {
        let vgn = resolve_variant_spec(COMPX_VID, VGN_F2_WIRELESS_PID, None, None).unwrap();
        assert_eq!(vgn.vendor, "VGN");
        assert!(vgn.polling_rates_hz.contains(&8000));

        let scyrox = resolve_variant_spec(COMPX_VID, SCYROX_V8_WIRELESS_PID, None, None).unwrap();
        assert_eq!(scyrox.dpi_max, 30_000);
        assert!(resolve_variant_spec(COMPX_VID, 0xF515, None, None).is_none());

        let gw = resolve_variant_spec(GWOLVES_VID, 0x3854, None, None).unwrap();
        assert_eq!(gw.vendor, "G-Wolves");

        let hs = resolve_variant_spec(HITSCAN_VID, 0x0300, None, None).unwrap();
        assert_eq!(hs.vendor, "Hitscan");
        assert!(hs.polling_rates_hz.contains(&8000));

        let batt_pkt = build_packet(CMD_BATTERY, 0, 4, &[78, 1, 0x0F, 0xFA]);
        let batt = decode_compx_battery(&batt_pkt).unwrap();
        assert_eq!(batt.percent, 78);
        assert!(batt.charging);
        assert_eq!(batt.voltage_mv, Some(0x0FFA));
    }
}
