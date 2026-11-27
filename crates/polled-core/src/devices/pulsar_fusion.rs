// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pulsar Fusion 2 / Sonix 64-byte (`Report ID 0x00`, `usage_page 0xFFFF`, Interface 3)
//! HID Feature Report protocol driver.
//!
//! Covers modern Pulsar 8K/1K dongles and Sonix-chipset wired mice that speak the 64-byte
//! feature-report protocol (distinct from the 17-byte CompX `Report ID 0x08` protocol in
//! [`crate::devices::compx`] used by Pulsar X2 CrazyLight `3710:5406` and older `0x3554` models):
//! - **Pulsar 8K Wireless Dongle (`3710:5403`)** — shared Fusion 2 8K receiver (X3 / X3 LHD /
//!   Xlite v4 / X2 v3 / X2H v3 / X2A v3; queries paired mouse PID via `01/84/04` to refine model
//!   name and LOD encoding)
//! - **Pulsar 1K Wireless Dongle (`3710:5402`)** — shared Fusion 2 1K receiver
//! - **Pulsar Feinmann F01 8K (`3710:5404`)** — 8K wireless dongle
//! - **Pulsar Fusion 2 Wired** — X3 (`3710:3409`), X3 LHD (`3710:3410`), Xlite v4 (`3710:3401`),
//!   X2A v3 (`3710:3404`)
//! - **Pulsar Sonix Wired** — X2A (`3710:1404`), X2H (`3710:1403`), X2 (`3710:1402`),
//!   Xlite Wired (`3710:1401`)
//!
//! # Protocol Attribution & Licenses (AGPL-3.0 compatible)
//! Every USB VID/PID, 64-byte frame layout, LE uint16 checksum, RF round-trip polling state
//! machine, paired-mouse query quirk, register map, and encoding table comes from verified
//! open-source implementations:
//! - `packerlschupfer/pulsar-mouse-linux` (<https://github.com/packerlschupfer/pulsar-mouse-linux>)
//!   - `src/pulsar_mouse/drivers/fusion2_8k_dongle.py`, `xlite_v4_wireless.py`, `feinmann8k.py`,
//!     `x3.py`, `x3_lhd.py`, `xlite_v4.py`, `x2a.py`, `x2h.py`, `x2_wired.py`, `xlite_wired.py`
//!   - **MIT License**, Copyright (c) 2026 Packerlschupfer
//! - `OpenMouse-Project/mouse-protocol` (<https://github.com/OpenMouse-Project/mouse-protocol>)
//!   - `src/pulsar/index.ts`, `src/drivers/pulsar/pulsar-xs1-hid.ts`
//!   - **AGPL-3.0-or-later**, Copyright (C) OpenMouse Project Contributors
//!
//! No HID bytes are guessed. Destructive commands (such as `02/05/01` factory reset) are never
//! encoded or issued.

use std::collections::HashSet;
use std::thread;
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    BatteryStatus, DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice,
    MouseSettings, SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};
use crate::hid::parse_hid_report_descriptor;

pub const PULSAR_VID: u16 = 0x3710;
pub const PULSAR_FUSION_INTERFACE: i32 = 3;
pub const PULSAR_FUSION_USAGE_PAGE: u16 = 0xFFFF;
pub const PULSAR_FUSION_USAGE: u16 = 0x0001;
pub const PULSAR_FUSION_REPORT_ID: u8 = 0x00;
pub const PULSAR_FUSION_PACKET_LEN: usize = 64;

pub const PULSAR_8K_DONGLE_PID: u16 = 0x5403;
pub const PULSAR_1K_DONGLE_PID: u16 = 0x5402;
pub const PULSAR_FEINMANN_8K_PID: u16 = 0x5404;

// --- Direction / status bytes (pkt[0]) ---
const DIR_CMD: u8 = 0x00;
const DIR_DONE: u8 = 0x01;
const DIR_PENDING: u8 = 0x02;
const DIR_NO_LINK: u8 = 0x05;

// --- Command categories & registers ---
const CAT_SYSTEM: u8 = 0x01;
const REG_CONNECTED_MOUSE: u8 = 0x04;
const REG_POLLING_RATE: u8 = 0x09;

const CAT_PROFILE: u8 = 0x02;
const REG_ACTIVE_PROFILE: u8 = 0x06;

const CAT_BUTTON: u8 = 0x04;
const REG_DEBOUNCE: u8 = 0x03;

const CAT_DPI: u8 = 0x05;
const REG_ACTIVE_DPI_STAGE: u8 = 0x01;
const REG_DPI_STAGES: u8 = 0x04;

const CAT_SENSOR: u8 = 0x07;
const REG_LOD: u8 = 0x02;
const REG_RIPPLE_CONTROL: u8 = 0x03;
const REG_ANGLE_SNAPPING: u8 = 0x04;
const REG_MOTION_SYNC: u8 = 0x05;

const CAT_POWER: u8 = 0x08;
const REG_BATTERY_PCT: u8 = 0x01;
const REG_BATTERY_CHARGING: u8 = 0x03;
const REG_SLEEP_TIMEOUT: u8 = 0x05;

const LOD_MODE_TENTHS: u8 = 0x02;

// --- Timing parameters verified in packerlschupfer/pulsar-mouse-linux ---
const WIRELESS_REPLY_TIMEOUT: Duration = Duration::from_millis(1_000);
const WIRELESS_REPLY_POLL: Duration = Duration::from_millis(10);
const WIRELESS_STALE_POLLS: usize = 5;
const WIRELESS_STALE_RESENDS: usize = 2;
const WIRELESS_STALE_RESEND_ROOM: Duration = Duration::from_millis(350);
const WIRELESS_WRITE_GAP: Duration = Duration::from_millis(30);
const FEINMANN_WRITE_SETTLE: Duration = Duration::from_millis(150);

const WIRED_ACK_POLLS: usize = 8;
const WIRED_ACK_DELAY: Duration = Duration::from_millis(20);
const WIRED_READ_RESENDS: usize = 3;
const WIRED_READ_RESEND_DELAY: Duration = Duration::from_millis(80);

const RATES_1K: &[u16] = &[125, 250, 500, 1000];
const RATES_8K: &[u16] = &[125, 250, 500, 1000, 2000, 4000, 8000];

const LOD_TENTHS_OPTIONS: &[LiftOffDistance] = &[
    LiftOffDistance::Mm07,
    LiftOffDistance::Mm10,
    LiftOffDistance::Mm20,
];
const LOD_LEGACY_OPTIONS: &[LiftOffDistance] = &[LiftOffDistance::Mm10, LiftOffDistance::Mm20];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PulsarLodEncoding {
    /// Mode byte `0x02` + tenths of a mm (`0.7 -> 7`, `1.0 -> 10`, `2.0 -> 20`).
    /// Used by XS-1 Fusion 2 mice (X3, X3 LHD, Xlite v4, X2A v3, Feinmann F01).
    Tenths,
    /// Legacy two-value index (`1.0 mm -> [0, 0]`, `2.0 mm -> [1, 1]`).
    /// Used by older Sonix wired PAW3395 models (X2A, X2H, X2, Xlite Wired).
    LegacyIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PulsarFusionProductSpec {
    pub pid: u16,
    pub model: &'static str,
    pub wireless: bool,
    pub shared_dongle: bool,
    pub is_8k_dongle: bool,
    pub is_feinmann: bool,
    pub supports_power: bool,
    pub default_lod_encoding: PulsarLodEncoding,
    pub dpi_min: u16,
    pub dpi_max: u16,
    pub dpi_step: u16,
    pub max_dpi_stages: u8,
    pub debounce_max_ms: u8,
    pub polling_rates: &'static [u16],
}

/// Verified Pulsar Fusion 2 / Sonix 64-byte USB PID catalog (`VID 0x3710`).
///
/// Note: `0x5406` / `0x3414` (X2 CrazyLight) and `0x5504` (Feinmann Noctua) speak the 17-byte
/// CompX `Report ID 0x08` protocol and are deliberately excluded here to avoid overlap.
pub const PULSAR_FUSION_PRODUCTS: &[PulsarFusionProductSpec] = &[
    PulsarFusionProductSpec {
        pid: PULSAR_8K_DONGLE_PID,
        model: "Pulsar 8K Wireless Dongle",
        wireless: true,
        shared_dongle: true,
        is_8k_dongle: true,
        is_feinmann: false,
        supports_power: true,
        default_lod_encoding: PulsarLodEncoding::Tenths,
        dpi_min: 100,
        dpi_max: 32_000,
        dpi_step: 10,
        max_dpi_stages: 6,
        debounce_max_ms: 15,
        polling_rates: RATES_8K,
    },
    PulsarFusionProductSpec {
        pid: PULSAR_1K_DONGLE_PID,
        model: "Pulsar 1K Wireless Dongle",
        wireless: true,
        shared_dongle: true,
        is_8k_dongle: false,
        is_feinmann: false,
        supports_power: true,
        default_lod_encoding: PulsarLodEncoding::Tenths,
        dpi_min: 100,
        dpi_max: 32_000,
        dpi_step: 10,
        max_dpi_stages: 6,
        debounce_max_ms: 15,
        polling_rates: RATES_1K,
    },
    PulsarFusionProductSpec {
        pid: PULSAR_FEINMANN_8K_PID,
        model: "Pulsar Feinmann F01 8K",
        wireless: true,
        shared_dongle: false,
        is_8k_dongle: true,
        is_feinmann: true,
        supports_power: true,
        default_lod_encoding: PulsarLodEncoding::Tenths,
        dpi_min: 50,
        dpi_max: 32_000,
        dpi_step: 10,
        max_dpi_stages: 6,
        debounce_max_ms: 15,
        polling_rates: RATES_8K,
    },
    PulsarFusionProductSpec {
        pid: 0x3409,
        model: "Pulsar X3 Wired",
        wireless: false,
        shared_dongle: false,
        is_8k_dongle: false,
        is_feinmann: false,
        supports_power: true,
        default_lod_encoding: PulsarLodEncoding::Tenths,
        dpi_min: 100,
        dpi_max: 32_000,
        dpi_step: 10,
        max_dpi_stages: 6,
        debounce_max_ms: 15,
        polling_rates: RATES_1K,
    },
    PulsarFusionProductSpec {
        pid: 0x3410,
        model: "Pulsar X3 LHD Wired",
        wireless: false,
        shared_dongle: false,
        is_8k_dongle: false,
        is_feinmann: false,
        supports_power: true,
        default_lod_encoding: PulsarLodEncoding::Tenths,
        dpi_min: 100,
        dpi_max: 32_000,
        dpi_step: 10,
        max_dpi_stages: 6,
        debounce_max_ms: 15,
        polling_rates: RATES_1K,
    },
    PulsarFusionProductSpec {
        pid: 0x3401,
        model: "Pulsar Xlite v4 Wired",
        wireless: false,
        shared_dongle: false,
        is_8k_dongle: false,
        is_feinmann: false,
        supports_power: true,
        default_lod_encoding: PulsarLodEncoding::Tenths,
        dpi_min: 100,
        dpi_max: 32_000,
        dpi_step: 10,
        max_dpi_stages: 6,
        debounce_max_ms: 15,
        polling_rates: RATES_1K,
    },
    PulsarFusionProductSpec {
        pid: 0x3404,
        model: "Pulsar X2A v3 Wired",
        wireless: false,
        shared_dongle: false,
        is_8k_dongle: false,
        is_feinmann: false,
        supports_power: true,
        default_lod_encoding: PulsarLodEncoding::Tenths,
        dpi_min: 100,
        dpi_max: 32_000,
        dpi_step: 10,
        max_dpi_stages: 6,
        debounce_max_ms: 15,
        polling_rates: RATES_1K,
    },
    PulsarFusionProductSpec {
        pid: 0x1404,
        model: "Pulsar X2A Wired",
        wireless: false,
        shared_dongle: false,
        is_8k_dongle: false,
        is_feinmann: false,
        supports_power: false,
        default_lod_encoding: PulsarLodEncoding::LegacyIndex,
        dpi_min: 100,
        dpi_max: 26_000,
        dpi_step: 100,
        max_dpi_stages: 6,
        debounce_max_ms: 15,
        polling_rates: RATES_1K,
    },
    PulsarFusionProductSpec {
        pid: 0x1403,
        model: "Pulsar X2H Wired",
        wireless: false,
        shared_dongle: false,
        is_8k_dongle: false,
        is_feinmann: false,
        supports_power: false,
        default_lod_encoding: PulsarLodEncoding::LegacyIndex,
        dpi_min: 100,
        dpi_max: 26_000,
        dpi_step: 100,
        max_dpi_stages: 6,
        debounce_max_ms: 15,
        polling_rates: RATES_1K,
    },
    PulsarFusionProductSpec {
        pid: 0x1402,
        model: "Pulsar X2 Wired",
        wireless: false,
        shared_dongle: false,
        is_8k_dongle: false,
        is_feinmann: false,
        supports_power: false,
        default_lod_encoding: PulsarLodEncoding::LegacyIndex,
        dpi_min: 100,
        dpi_max: 26_000,
        dpi_step: 100,
        max_dpi_stages: 6,
        debounce_max_ms: 15,
        polling_rates: RATES_1K,
    },
    PulsarFusionProductSpec {
        pid: 0x1401,
        model: "Pulsar Xlite Wired",
        wireless: false,
        shared_dongle: false,
        is_8k_dongle: false,
        is_feinmann: false,
        supports_power: false,
        default_lod_encoding: PulsarLodEncoding::LegacyIndex,
        dpi_min: 100,
        dpi_max: 26_000,
        dpi_step: 50,
        max_dpi_stages: 6,
        debounce_max_ms: 15,
        polling_rates: RATES_1K,
    },
];

pub fn lookup_pulsar_fusion_product(vid: u16, pid: u16) -> Option<PulsarFusionProductSpec> {
    if vid != PULSAR_VID {
        return None;
    }
    PULSAR_FUSION_PRODUCTS
        .iter()
        .copied()
        .find(|p| p.pid == pid)
}

/// Maps a paired Fusion 2 mouse's cabled PID (reported by `01/84/04` on `3710:5403` / `5402`)
/// to its human-readable model name and LOD encoding.
pub fn paired_fusion_mouse_info(vid: u16, pid: u16) -> Option<(&'static str, PulsarLodEncoding)> {
    if vid != PULSAR_VID {
        return None;
    }
    match pid {
        0x3401 => Some(("Pulsar Xlite v4", PulsarLodEncoding::Tenths)),
        0x3402 => Some(("Pulsar X2 v3", PulsarLodEncoding::Tenths)),
        0x3403 => Some(("Pulsar X2H v3", PulsarLodEncoding::Tenths)),
        0x3404 => Some(("Pulsar X2A v3", PulsarLodEncoding::Tenths)),
        0x3409 => Some(("Pulsar X3", PulsarLodEncoding::Tenths)),
        0x3410 => Some(("Pulsar X3 LHD", PulsarLodEncoding::Tenths)),
        0x1401 => Some(("Pulsar Xlite", PulsarLodEncoding::LegacyIndex)),
        0x1402 => Some(("Pulsar X2", PulsarLodEncoding::LegacyIndex)),
        0x1403 => Some(("Pulsar X2H", PulsarLodEncoding::LegacyIndex)),
        0x1404 => Some(("Pulsar X2A", PulsarLodEncoding::LegacyIndex)),
        _ => None,
    }
}

/// Computes the Pulsar Fusion 2 / Sonix 16-bit little-endian checksum: `sum(pkt[0..62]) & 0xFFFF`.
pub fn checksum(pkt: &[u8]) -> u16 {
    let mut sum = 0u32;
    for &b in pkt.iter().take(62) {
        sum = sum.wrapping_add(u32::from(b));
    }
    (sum & 0xFFFF) as u16
}

/// Builds a 64-byte Pulsar Fusion 2 command frame (`pkt[0] = 0x00`) with trailing LE uint16 checksum.
pub fn build_packet(
    cat: u8,
    reg: u8,
    sub: u8,
    profile: u8,
    payload: &[u8],
) -> [u8; PULSAR_FUSION_PACKET_LEN] {
    let mut pkt = [0u8; PULSAR_FUSION_PACKET_LEN];
    pkt[0] = DIR_CMD;
    pkt[1] = cat;
    pkt[2] = reg;
    pkt[3] = sub;
    pkt[6] = profile;
    let copy_len = payload.len().min(62 - 7);
    pkt[7..7 + copy_len].copy_from_slice(&payload[..copy_len]);
    let cs = checksum(&pkt).to_le_bytes();
    pkt[62] = cs[0];
    pkt[63] = cs[1];
    pkt
}

pub fn build_read_packet(
    cat: u8,
    reg: u8,
    sub: u8,
    profile: u8,
    payload: &[u8],
) -> [u8; PULSAR_FUSION_PACKET_LEN] {
    build_packet(cat, reg | 0x80, sub, profile, payload)
}

pub fn encode_polling_rate(hz: u16) -> Option<u8> {
    match hz {
        125 => Some(0x01),
        250 => Some(0x02),
        500 => Some(0x04),
        1000 => Some(0x08),
        2000 => Some(0x10),
        4000 => Some(0x20),
        8000 => Some(0x40),
        _ => None,
    }
}

pub fn decode_polling_rate(code: u8) -> Option<u16> {
    match code {
        0x01 => Some(125),
        0x02 => Some(250),
        0x04 => Some(500),
        0x08 => Some(1000),
        0x10 => Some(2000),
        0x20 => Some(4000),
        0x40 => Some(8000),
        _ => None,
    }
}

/// Decodes `07/82/03` LOD reply bytes (`rsp[7]` mode/index, `rsp[8]` tenths-mm) and reports
/// whether the mouse is actively using tenths-mm mode (`rsp[7] == 0x02`).
pub fn decode_lod(
    byte7: u8,
    byte8: u8,
    expected: PulsarLodEncoding,
) -> (Option<LiftOffDistance>, PulsarLodEncoding) {
    if byte7 == LOD_MODE_TENTHS {
        let lod = if byte8 == 0 {
            None
        } else if byte8 < 10 {
            Some(LiftOffDistance::Mm07)
        } else if byte8 < 20 {
            Some(LiftOffDistance::Mm10)
        } else {
            Some(LiftOffDistance::Mm20)
        };
        return (lod, PulsarLodEncoding::Tenths);
    }
    match expected {
        PulsarLodEncoding::Tenths => {
            // Non-mode-2 legacy pair on a tenths-mode mouse behaves as ~1.0 mm per x2a.py.
            (Some(LiftOffDistance::Mm10), PulsarLodEncoding::Tenths)
        }
        PulsarLodEncoding::LegacyIndex => {
            let lod = match byte7 {
                0 => Some(LiftOffDistance::Mm10),
                1 => Some(LiftOffDistance::Mm20),
                _ => None,
            };
            (lod, PulsarLodEncoding::LegacyIndex)
        }
    }
}

pub fn encode_lod(lod: LiftOffDistance, encoding: PulsarLodEncoding) -> [u8; 2] {
    match encoding {
        PulsarLodEncoding::Tenths => [LOD_MODE_TENTHS, lod.tenths_mm()],
        PulsarLodEncoding::LegacyIndex => match lod {
            LiftOffDistance::Mm07 | LiftOffDistance::Mm10 => [0x00, 0x00],
            LiftOffDistance::Mm20 => [0x01, 0x01],
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulsarDpiTable {
    pub active_stage: u8,
    pub stages: Vec<(u16, u16)>,
}

/// Decodes a `05/84/15` DPI stage table reply.
pub fn decode_dpi_stages(
    rsp: &[u8; PULSAR_FUSION_PACKET_LEN],
    max_stages: u8,
) -> Option<PulsarDpiTable> {
    let raw_count = rsp[8];
    if raw_count == 0 {
        return None;
    }
    let count = usize::from(raw_count.min(max_stages.max(1)));
    let mut stages = Vec::with_capacity(count);
    for i in 0..count {
        let off = 9 + i * 5;
        if off + 4 >= rsp.len() {
            break;
        }
        let x = u16::from_le_bytes([rsp[off + 1], rsp[off + 2]]);
        let y = u16::from_le_bytes([rsp[off + 3], rsp[off + 4]]);
        stages.push((x, y));
    }
    if stages.is_empty() {
        return None;
    }
    let active_stage = rsp[7].clamp(1, stages.len() as u8);
    Some(PulsarDpiTable {
        active_stage,
        stages,
    })
}

/// Encodes payload for `05/04/21` (`[active, count, (stage_idx, x_lo, x_hi, y_lo, y_hi)...]`).
pub fn encode_dpi_stages_payload(table: &PulsarDpiTable) -> Vec<u8> {
    let count = table.stages.len().min(6);
    let active = table.active_stage.clamp(1, count.max(1) as u8);
    let mut payload = Vec::with_capacity(2 + count * 5);
    payload.push(active);
    payload.push(count as u8);
    for (idx, &(x, y)) in table.stages.iter().take(count).enumerate() {
        let xb = x.to_le_bytes();
        let yb = y.to_le_bytes();
        payload.push((idx + 1) as u8);
        payload.push(xb[0]);
        payload.push(xb[1]);
        payload.push(yb[0]);
        payload.push(yb[1]);
    }
    payload
}

fn reply_echoes(
    rsp: &[u8; PULSAR_FUSION_PACKET_LEN],
    req: &[u8; PULSAR_FUSION_PACKET_LEN],
) -> bool {
    rsp[1] == req[1] && rsp[2] == req[2] && (req[6] == 0x00 || rsp[6] == req[6])
}

/// Hardware driver for Pulsar Fusion 2 / Sonix 64-byte mice and 1K/8K dongles.
pub struct PulsarFusionDevice {
    hid: HidDevice,
    spec: PulsarFusionProductSpec,
    display_name: String,
    vid_pid: String,
    lod_encoding: PulsarLodEncoding,
    last_sent: Option<[u8; PULSAR_FUSION_PACKET_LEN]>,
    last_write_answered: Option<Instant>,
}

impl PulsarFusionDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.display_name.clone(),
            message: message.into(),
        }
    }

    fn send_feature(&self, pkt: &[u8; PULSAR_FUSION_PACKET_LEN]) -> Result<()> {
        let mut buf = [0u8; PULSAR_FUSION_PACKET_LEN + 1];
        buf[0] = PULSAR_FUSION_REPORT_ID;
        buf[1..].copy_from_slice(pkt);
        self.hid
            .send_feature_report(&buf)
            .map_err(|e| self.dev_err(format!("Pulsar Fusion2 SET_FEATURE failed: {e}")))
    }

    fn get_feature(&self) -> Result<[u8; PULSAR_FUSION_PACKET_LEN]> {
        let mut buf = [0u8; PULSAR_FUSION_PACKET_LEN + 1];
        buf[0] = PULSAR_FUSION_REPORT_ID;
        let n = self
            .hid
            .get_feature_report(&mut buf)
            .map_err(|e| self.dev_err(format!("Pulsar Fusion2 GET_FEATURE failed: {e}")))?;
        let mut out = [0u8; PULSAR_FUSION_PACKET_LEN];
        if n > PULSAR_FUSION_PACKET_LEN {
            out.copy_from_slice(&buf[1..=PULSAR_FUSION_PACKET_LEN]);
        } else if n >= PULSAR_FUSION_PACKET_LEN {
            out.copy_from_slice(&buf[..PULSAR_FUSION_PACKET_LEN]);
        } else {
            return Err(self.dev_err(format!(
                "Pulsar Fusion2 GET_FEATURE returned short {n}-byte frame"
            )));
        }
        Ok(out)
    }

    fn transact(
        &mut self,
        cmd: [u8; PULSAR_FUSION_PACKET_LEN],
    ) -> Result<[u8; PULSAR_FUSION_PACKET_LEN]> {
        if self.spec.wireless {
            self.transact_wireless(cmd)
        } else {
            self.transact_wired(cmd)
        }
    }

    /// Wireless dongle RF round-trip transaction matching `xlite_v4_wireless.py` / `feinmann8k.py`:
    /// waits for `0x01` echo, fails fast on `0x05` (mouse asleep / no link), recovers from stale
    /// reply buffer latches while the mouse is moving, and enforces the inter-write RF gap.
    fn transact_wireless(
        &mut self,
        cmd: [u8; PULSAR_FUSION_PACKET_LEN],
    ) -> Result<[u8; PULSAR_FUSION_PACKET_LEN]> {
        if let Some(last_w) = self.last_write_answered.take() {
            let gap = if self.spec.is_feinmann {
                FEINMANN_WRITE_SETTLE
            } else {
                WIRELESS_WRITE_GAP
            };
            let elapsed = last_w.elapsed();
            if elapsed < gap {
                thread::sleep(gap - elapsed);
            }
        }

        let prev = self.last_sent.replace(cmd);
        let is_write = (cmd[2] & 0x80) == 0;
        self.send_feature(&cmd)?;

        let deadline = Instant::now() + WIRELESS_REPLY_TIMEOUT;
        let mut sends = 1usize;
        let mut stale = 0usize;
        let mut shown = false;

        loop {
            let rsp = self.get_feature()?;
            if reply_echoes(&rsp, &cmd) {
                stale = 0;
                if rsp[0] == DIR_DONE {
                    if is_write {
                        self.last_write_answered = Some(Instant::now());
                    }
                    return Ok(rsp);
                }
                if rsp[0] == DIR_NO_LINK {
                    return Err(self.dev_err(format!(
                        "No link to wireless mouse for cat=0x{:02x} reg=0x{:02x} (asleep — move mouse to wake)",
                        cmd[1], cmd[2]
                    )));
                }
                shown = true;
            } else if rsp[0] == DIR_DONE
                && (shown || prev.as_ref().is_some_and(|p| reply_echoes(&rsp, p)))
            {
                stale += 1;
                if stale >= WIRELESS_STALE_POLLS
                    && sends <= WIRELESS_STALE_RESENDS
                    && Instant::now() + WIRELESS_STALE_RESEND_ROOM < deadline
                {
                    self.send_feature(&cmd)?;
                    sends += 1;
                    stale = 0;
                }
            } else {
                stale = 0;
            }

            if Instant::now() >= deadline {
                return Err(self.dev_err(format!(
                    "Timed out waiting for Pulsar Fusion2 reply (cat=0x{:02x} reg=0x{:02x}; move mouse to wake)",
                    cmd[1], cmd[2]
                )));
            }
            thread::sleep(WIRELESS_REPLY_POLL);
        }
    }

    fn transact_wired(
        &mut self,
        cmd: [u8; PULSAR_FUSION_PACKET_LEN],
    ) -> Result<[u8; PULSAR_FUSION_PACKET_LEN]> {
        let is_write = (cmd[2] & 0x80) == 0;
        self.send_feature(&cmd)?;

        let poll_once = |dev: &Self| -> Result<[u8; PULSAR_FUSION_PACKET_LEN]> {
            let mut rsp = dev.get_feature()?;
            for _ in 0..WIRED_ACK_POLLS.saturating_sub(1) {
                if rsp[0] == DIR_DONE || (is_write && rsp[0] == DIR_PENDING) {
                    return Ok(rsp);
                }
                thread::sleep(WIRED_ACK_DELAY);
                rsp = dev.get_feature()?;
            }
            Ok(rsp)
        };

        let mut rsp = poll_once(self)?;
        if !is_write {
            let mut resends = 0usize;
            while rsp[0] == DIR_CMD && resends < WIRED_READ_RESENDS {
                resends += 1;
                thread::sleep(WIRED_READ_RESEND_DELAY);
                self.send_feature(&cmd)?;
                rsp = poll_once(self)?;
            }
        }

        if rsp[0] == DIR_DONE || (is_write && rsp[0] == DIR_PENDING) {
            Ok(rsp)
        } else {
            Err(self.dev_err(format!(
                "Unexpected Pulsar Fusion2 response status 0x{:02x} for cat=0x{:02x} reg=0x{:02x}",
                rsp[0], cmd[1], cmd[2]
            )))
        }
    }

    fn read_cmd(
        &mut self,
        cat: u8,
        reg: u8,
        sub: u8,
        profile: u8,
        payload: &[u8],
    ) -> Result<[u8; PULSAR_FUSION_PACKET_LEN]> {
        self.transact(build_read_packet(cat, reg, sub, profile, payload))
    }

    fn write_cmd(&mut self, cat: u8, reg: u8, sub: u8, profile: u8, payload: &[u8]) -> Result<()> {
        let _ = self.transact(build_packet(cat, reg, sub, profile, payload))?;
        Ok(())
    }

    /// Queries the shared 1K / 8K dongle (`01/84/04`) for the paired mouse's cabled VID/PID.
    /// Handles the verified 8K dongle (`3710:5403`) quirk where the reply carries the paired
    /// Pulsar VID/PID under the *previous* request's header (`fusion2_8k_dongle.py`).
    fn try_identify_paired_mouse(&mut self) {
        if !self.spec.shared_dongle {
            return;
        }
        let cmd = build_read_packet(CAT_SYSTEM, REG_CONNECTED_MOUSE, 0x04, 0x00, &[]);
        if self.send_feature(&cmd).is_err() {
            return;
        }
        let deadline = Instant::now() + WIRELESS_REPLY_TIMEOUT;
        while Instant::now() < deadline {
            let Ok(rsp) = self.get_feature() else {
                break;
            };
            if rsp[0] == DIR_NO_LINK && reply_echoes(&rsp, &cmd) {
                break;
            }
            let vid = u16::from_le_bytes([rsp[6], rsp[7]]);
            let pid = u16::from_le_bytes([rsp[8], rsp[9]]);
            let matched = if self.spec.is_8k_dongle {
                rsp[0] == DIR_DONE && vid == PULSAR_VID && pid != 0
            } else {
                rsp[0] == DIR_DONE && reply_echoes(&rsp, &cmd) && vid == PULSAR_VID && pid != 0
            };
            if matched {
                self.last_sent = None;
                if let Some((model_name, lod_enc)) = paired_fusion_mouse_info(vid, pid) {
                    let suffix = if self.spec.is_8k_dongle {
                        "8K Dongle"
                    } else {
                        "1K Dongle"
                    };
                    self.display_name = format!("{model_name} ({suffix})");
                    self.lod_encoding = lod_enc;
                }
                return;
            }
            thread::sleep(WIRELESS_REPLY_POLL);
        }
        self.last_sent = None;
    }

    fn read_active_profile(&mut self) -> u8 {
        self.read_cmd(CAT_PROFILE, REG_ACTIVE_PROFILE, 0x01, 0x00, &[])
            .ok()
            .map(|rsp| {
                let p = if rsp[7] != 0 { rsp[7] } else { rsp[6] };
                p.clamp(1, 6)
            })
            .unwrap_or(1)
    }
}

impl MouseDevice for PulsarFusionDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: format!("pulsar_fusion_{:04x}", self.spec.pid),
            name: self.display_name.clone(),
            vendor: "Pulsar".to_string(),
            connection: if self.spec.wireless {
                if self.spec.is_8k_dongle {
                    "Wireless 8K Dongle (Fusion 2)".to_string()
                } else {
                    "Wireless 1K Dongle (Fusion 2)".to_string()
                }
            } else {
                "Wired USB (Fusion 2)".to_string()
            },
            vid_pid: Some(self.vid_pid.clone()),
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let lod_options = match self.lod_encoding {
            PulsarLodEncoding::Tenths => LOD_TENTHS_OPTIONS.to_vec(),
            PulsarLodEncoding::LegacyIndex => LOD_LEGACY_OPTIONS.to_vec(),
        };
        DeviceCapabilities {
            dpi_min: self.spec.dpi_min,
            dpi_max: self.spec.dpi_max,
            dpi_step: self.spec.dpi_step,
            supports_xy_dpi: true,
            polling_rates_hz: self.spec.polling_rates.to_vec(),
            lod_options,
            supports_debounce: true,
            debounce_min_ms: 0,
            debounce_max_ms: self.spec.debounce_max_ms,
            supports_motion_sync: true,
            supports_angle_snapping: true,
            supports_ripple_control: true,
            supports_sleep_timeout: self.spec.supports_power,
            sleep_timeout_min_secs: if self.spec.supports_power { 30 } else { 0 },
            sleep_timeout_max_secs: if self.spec.supports_power { 900 } else { 0 },
            dpi_stage_count: self.spec.max_dpi_stages,
            supports_dpi_lock: true,
            supports_glass_mode: false,
            supports_volatile_writes: false,
            flash_write_warning: Some(format!(
                "{} writes settings to onboard profile flash (Fusion 2 64B HID). Polled diffs every register, enforces burst rate-limiting, and verifies writes via read-back.",
                self.display_name
            )),
        }
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        let profile = self.read_active_profile();

        // DPI stages (`05/84/15`) — fail early if mouse is asleep so we don't wait on 8 timeouts.
        let dpi_rsp = self.read_cmd(CAT_DPI, REG_DPI_STAGES, 0x15, profile, &[])?;
        let (dpi, dpi_y, dpi_stage) =
            if let Some(table) = decode_dpi_stages(&dpi_rsp, self.spec.max_dpi_stages) {
                let idx = usize::from(table.active_stage.saturating_sub(1));
                let (x, y) = table.stages.get(idx).copied().unwrap_or(table.stages[0]);
                (
                    Some(x),
                    if y != x { Some(y) } else { None },
                    Some(table.active_stage),
                )
            } else {
                (None, None, None)
            };

        let polling_rate_hz = self
            .read_cmd(CAT_SYSTEM, REG_POLLING_RATE, 0x02, 0x00, &[])
            .ok()
            .and_then(|r| decode_polling_rate(r[7]));

        let lod = self
            .read_cmd(CAT_SENSOR, REG_LOD, 0x03, profile, &[])
            .ok()
            .and_then(|r| {
                let (decoded, detected_enc) = decode_lod(r[7], r[8], self.lod_encoding);
                if detected_enc == PulsarLodEncoding::Tenths {
                    self.lod_encoding = PulsarLodEncoding::Tenths;
                }
                decoded
            });

        let debounce_ms = self
            .read_cmd(CAT_BUTTON, REG_DEBOUNCE, 0x03, 0x00, &[])
            .ok()
            .map(|r| r[7])
            .filter(|&ms| ms <= self.spec.debounce_max_ms);

        let motion_sync = self
            .read_cmd(CAT_SENSOR, REG_MOTION_SYNC, 0x02, 0x00, &[])
            .ok()
            .map(|r| r[7] != 0);

        let angle_snapping = self
            .read_cmd(CAT_SENSOR, REG_ANGLE_SNAPPING, 0x02, 0x00, &[])
            .ok()
            .map(|r| r[7] != 0);

        let ripple_control = self
            .read_cmd(CAT_SENSOR, REG_RIPPLE_CONTROL, 0x02, 0x00, &[])
            .ok()
            .map(|r| r[7] != 0);

        let sleep_timeout_secs = if self.spec.supports_power {
            let sleep_payload: &[u8] = if self.spec.is_feinmann { &[0x01] } else { &[] };
            self.read_cmd(CAT_POWER, REG_SLEEP_TIMEOUT, 0x03, 0x00, sleep_payload)
                .ok()
                .map(|r| u16::from_le_bytes([r[7], r[8]]))
                .filter(|&s| (30..=900).contains(&s))
        } else {
            None
        };

        Ok(MouseSettings {
            dpi,
            dpi_y,
            polling_rate_hz,
            lod,
            debounce_ms,
            motion_sync,
            angle_snapping,
            ripple_control,
            sleep_timeout_secs,
            dpi_stage,
            lock_dpi_stages: None,
            glass_mode: None,
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        let caps = self.capabilities();
        caps.validate(&self.display_name, settings)?;

        let profile = self.read_active_profile();

        // 1. DPI stage selection, DPI X/Y value, and optional Lock All DPI Stages.
        if settings.dpi.is_some()
            || settings.dpi_y.is_some()
            || settings.dpi_stage.is_some()
            || settings.lock_dpi_stages == Some(true)
        {
            let dpi_rsp = self.read_cmd(CAT_DPI, REG_DPI_STAGES, 0x15, profile, &[])?;
            let mut table =
                decode_dpi_stages(&dpi_rsp, self.spec.max_dpi_stages).ok_or_else(|| {
                    self.dev_err("Could not decode current Pulsar Fusion2 DPI stage table")
                })?;

            let target_stage = settings
                .dpi_stage
                .map(|s| s.clamp(1, table.stages.len() as u8))
                .unwrap_or(table.active_stage);
            let idx = usize::from(target_stage.saturating_sub(1));

            let stage_only_change = settings.dpi.is_none()
                && settings.dpi_y.is_none()
                && settings.lock_dpi_stages != Some(true);

            if stage_only_change {
                if target_stage != table.active_stage {
                    self.write_cmd(
                        CAT_DPI,
                        REG_ACTIVE_DPI_STAGE,
                        0x02,
                        profile,
                        &[target_stage],
                    )?;
                }
            } else {
                let (cur_x, cur_y) = table.stages[idx];
                let new_x = settings.dpi.unwrap_or(cur_x);
                let new_y = settings.dpi_y.unwrap_or_else(|| {
                    if settings.dpi.is_some() {
                        new_x
                    } else {
                        cur_y
                    }
                });
                let mut mutated = target_stage != table.active_stage;
                if settings.lock_dpi_stages == Some(true) {
                    for slot in &mut table.stages {
                        if *slot != (new_x, new_y) {
                            *slot = (new_x, new_y);
                            mutated = true;
                        }
                    }
                } else if table.stages[idx] != (new_x, new_y) {
                    table.stages[idx] = (new_x, new_y);
                    mutated = true;
                }
                table.active_stage = target_stage;
                if mutated {
                    let payload = encode_dpi_stages_payload(&table);
                    self.write_cmd(CAT_DPI, REG_DPI_STAGES, 0x21, profile, &payload)?;
                }
            }
        }

        // 2. Polling rate (`01/09/02`, profile 0).
        if let Some(hz) = settings.polling_rate_hz {
            let code = encode_polling_rate(hz)
                .ok_or_else(|| self.dev_err(format!("Unsupported polling rate: {hz} Hz")))?;
            self.write_cmd(CAT_SYSTEM, REG_POLLING_RATE, 0x02, 0x00, &[code])?;
        }

        // 3. Lift-off distance (`07/02/03`, active profile).
        if let Some(lod) = settings.lod {
            let payload = encode_lod(lod, self.lod_encoding);
            self.write_cmd(CAT_SENSOR, REG_LOD, 0x03, profile, &payload)?;
        }

        // 4. Button debounce (`04/03/03`, profile 0).
        if let Some(ms) = settings.debounce_ms {
            self.write_cmd(CAT_BUTTON, REG_DEBOUNCE, 0x03, 0x00, &[ms])?;
        }

        // 5. Sensor toggles (`07/05`, `07/04`, `07/03`, profile 0).
        if let Some(enabled) = settings.motion_sync {
            self.write_cmd(
                CAT_SENSOR,
                REG_MOTION_SYNC,
                0x02,
                0x00,
                &[u8::from(enabled)],
            )?;
        }
        if let Some(enabled) = settings.angle_snapping {
            self.write_cmd(
                CAT_SENSOR,
                REG_ANGLE_SNAPPING,
                0x02,
                0x00,
                &[u8::from(enabled)],
            )?;
        }
        if let Some(enabled) = settings.ripple_control {
            self.write_cmd(
                CAT_SENSOR,
                REG_RIPPLE_CONTROL,
                0x02,
                0x00,
                &[u8::from(enabled)],
            )?;
        }

        // 6. Wireless sleep timeout (`08/05/03`, profile 0).
        if let Some(secs) = settings.sleep_timeout_secs {
            if self.spec.supports_power {
                self.write_cmd(
                    CAT_POWER,
                    REG_SLEEP_TIMEOUT,
                    0x03,
                    0x00,
                    &secs.to_le_bytes(),
                )?;
            }
        }

        Ok(())
    }

    fn read_battery(&mut self) -> Option<BatteryStatus> {
        if !self.spec.supports_power {
            return None;
        }
        let pct_rsp = self
            .read_cmd(CAT_POWER, REG_BATTERY_PCT, 0x01, 0x00, &[])
            .ok()?;
        let pct = pct_rsp[6];
        if pct == 0xFF || pct > 100 {
            return None;
        }
        let charging = self
            .read_cmd(CAT_POWER, REG_BATTERY_CHARGING, 0x01, 0x00, &[])
            .ok()
            .is_some_and(|r| r[6] != 0);
        Some(BatteryStatus {
            percent: pct,
            charging,
            voltage_mv: None,
        })
    }
}

pub struct PulsarFusionProtocolDriver;

impl DeviceDriver for PulsarFusionProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "Pulsar Fusion 2 / Sonix 64B"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();
        let mut seen_vid_pid: HashSet<(u16, u16)> = HashSet::new();

        for dev in api.device_list() {
            let vid = dev.vendor_id();
            let pid = dev.product_id();
            let Some(spec) = lookup_pulsar_fusion_product(vid, pid) else {
                continue;
            };
            // Strictly bind to the Fusion 2 command interface (Interface 3, usage page 0xFFFF).
            if dev.interface_number() != PULSAR_FUSION_INTERFACE
                || dev.usage_page() != PULSAR_FUSION_USAGE_PAGE
                || dev.usage() != PULSAR_FUSION_USAGE
            {
                continue;
            }
            if !seen_vid_pid.insert((vid, pid)) {
                continue;
            }

            let Ok(hid) = api.open_path(dev.path()) else {
                continue;
            };

            // Verify descriptor confirms an unnumbered 64-byte feature report (Report ID 0x00)
            // and is NOT a CompX 17-byte `Report ID 0x08` interface.
            let mut desc_buf = [0u8; 4096];
            if let Ok(len) = hid.get_report_descriptor(&mut desc_buf) {
                let parsed = parse_hid_report_descriptor(&desc_buf[..len]);
                let has_64b_feat = parsed
                    .feature_reports
                    .get(&PULSAR_FUSION_REPORT_ID)
                    .copied()
                    == Some(PULSAR_FUSION_PACKET_LEN);
                let has_compx17 = parsed
                    .output_reports
                    .get(&0x08)
                    .is_some_and(|&sz| sz == 16 || sz == 17);
                if !has_64b_feat || has_compx17 {
                    continue;
                }
            }

            let mut device = PulsarFusionDevice {
                hid,
                spec,
                display_name: spec.model.to_string(),
                vid_pid: format!("{vid:04x}:{pid:04x}"),
                lod_encoding: spec.default_lod_encoding,
                last_sent: None,
                last_write_answered: None,
            };
            device.try_identify_paired_mouse();
            found.push(Box::new(device));
        }

        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_valid_64b_fusion2_packet_with_le_checksum() {
        // Polling rate 8000 Hz write: cat=0x01, reg=0x09, sub=0x02, profile=0x00, payload=[0x40]
        let pkt = build_packet(CAT_SYSTEM, REG_POLLING_RATE, 0x02, 0x00, &[0x40]);
        assert_eq!(pkt[0], 0x00);
        assert_eq!(pkt[1], 0x01);
        assert_eq!(pkt[2], 0x09);
        assert_eq!(pkt[3], 0x02);
        assert_eq!(pkt[6], 0x00);
        assert_eq!(pkt[7], 0x40);
        let expected_sum: u16 = 0x01 + 0x09 + 0x02 + 0x40;
        assert_eq!(u16::from_le_bytes([pkt[62], pkt[63]]), expected_sum);

        // Read packet sets bit 7 on register byte.
        let rd = build_read_packet(CAT_POWER, REG_BATTERY_PCT, 0x01, 0x00, &[]);
        assert_eq!(rd[1], 0x08);
        assert_eq!(rd[2], 0x81);
        assert_eq!(rd[3], 0x01);
        assert_eq!(u16::from_le_bytes([rd[62], rd[63]]), 0x08 + 0x81 + 0x01);
    }

    #[test]
    fn roundtrips_8k_polling_lod_modes_and_dpi_stages() {
        for &hz in RATES_8K {
            let code = encode_polling_rate(hz).unwrap();
            assert_eq!(decode_polling_rate(code), Some(hz));
        }
        assert_eq!(encode_polling_rate(3000), None);

        // Tenths-mm LOD encoding (X3 / Xlite v4 / Feinmann / 8K Dongle)
        assert_eq!(
            encode_lod(LiftOffDistance::Mm07, PulsarLodEncoding::Tenths),
            [0x02, 0x07]
        );
        assert_eq!(
            encode_lod(LiftOffDistance::Mm10, PulsarLodEncoding::Tenths),
            [0x02, 0x0A]
        );
        assert_eq!(
            encode_lod(LiftOffDistance::Mm20, PulsarLodEncoding::Tenths),
            [0x02, 0x14]
        );
        assert_eq!(
            decode_lod(0x02, 0x07, PulsarLodEncoding::LegacyIndex),
            (Some(LiftOffDistance::Mm07), PulsarLodEncoding::Tenths)
        );
        assert_eq!(
            decode_lod(0x02, 0x0A, PulsarLodEncoding::Tenths),
            (Some(LiftOffDistance::Mm10), PulsarLodEncoding::Tenths)
        );
        assert_eq!(
            decode_lod(0x02, 0x14, PulsarLodEncoding::Tenths),
            (Some(LiftOffDistance::Mm20), PulsarLodEncoding::Tenths)
        );

        // Legacy index encoding (X2A / X2H / X2 / Xlite wired)
        assert_eq!(
            encode_lod(LiftOffDistance::Mm10, PulsarLodEncoding::LegacyIndex),
            [0x00, 0x00]
        );
        assert_eq!(
            encode_lod(LiftOffDistance::Mm20, PulsarLodEncoding::LegacyIndex),
            [0x01, 0x01]
        );
        assert_eq!(
            decode_lod(0x01, 0x01, PulsarLodEncoding::LegacyIndex),
            (Some(LiftOffDistance::Mm20), PulsarLodEncoding::LegacyIndex)
        );

        // DPI stage table encode/decode roundtrip (including independent X/Y DPI)
        let table = PulsarDpiTable {
            active_stage: 2,
            stages: vec![(400, 400), (1600, 1200), (3200, 3200)],
        };
        let payload = encode_dpi_stages_payload(&table);
        let mut fake_rsp = [0u8; PULSAR_FUSION_PACKET_LEN];
        fake_rsp[0] = DIR_DONE;
        fake_rsp[1] = CAT_DPI;
        fake_rsp[2] = REG_DPI_STAGES | 0x80;
        fake_rsp[3] = 0x21;
        fake_rsp[6] = 1;
        fake_rsp[7..7 + payload.len()].copy_from_slice(&payload);
        let decoded = decode_dpi_stages(&fake_rsp, 6).unwrap();
        assert_eq!(decoded, table);
    }

    #[test]
    fn recognizes_8k_dongle_and_excludes_crazylight_compx17() {
        let dongle = lookup_pulsar_fusion_product(PULSAR_VID, PULSAR_8K_DONGLE_PID).unwrap();
        assert!(dongle.wireless);
        assert!(dongle.is_8k_dongle);
        assert_eq!(dongle.polling_rates, RATES_8K);

        // X2 CrazyLight (3710:5406 / 3414) belongs exclusively to CompX 17B driver.
        assert!(lookup_pulsar_fusion_product(PULSAR_VID, 0x5406).is_none());
        assert!(lookup_pulsar_fusion_product(PULSAR_VID, 0x3414).is_none());

        assert_eq!(
            paired_fusion_mouse_info(PULSAR_VID, 0x3410),
            Some(("Pulsar X3 LHD", PulsarLodEncoding::Tenths))
        );
        assert_eq!(
            paired_fusion_mouse_info(PULSAR_VID, 0x3401),
            Some(("Pulsar Xlite v4", PulsarLodEncoding::Tenths))
        );
    }
}
