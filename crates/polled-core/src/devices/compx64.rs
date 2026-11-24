// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Unified 64-byte CompX Page-Command (`Report ID 0x00`) feature-report protocol driver.
//!
//! Covers newer CompX/Nordic mice that use 64-byte page/command feature reports (distinct from the
//! older 17-byte `Report ID 0x08` protocol in [`crate::devices::compx`]):
//! - **WLMouse** (`0x36A7`): Beast X, Beast X Pro, Beast Max, Beast Mini, Beast Mini Pro,
//!   Beast Miao, Beast G, Sword X, Strider, Huan, Ying, 1K Receiver
//! - **LAMZU Aurora / 8K Gen** (`0x373E` / `0x37B0`): Maya X, Inca 8K, Paro Aurora, Aurora 1K/8K Receivers
//! - **Attack Shark** (`0x373E`): R5 Ultra, R6, R8
//! - **Vancer x Arbiter Studio** (`0x373E`): Akitsu Medium 3950
//! - **RAWM** (`0x373E`): Leviathan V4 GT
//! - **CRDRAKO** (`0x373E`): KO-ONE
//! - **LunaFury** (`0x373E`): LUNA33, TYPE33
//!
//! # Protocol Attribution & Licenses (AGPL-3.0 compatible)
//! Packet framing, status bytes, page/command IDs, polling/LOD tables, and product catalogs are
//! ported from:
//! - `OpenMouse-Project/mouse-protocol` (<https://github.com/OpenMouse-Project/mouse-protocol>)
//!   - `src/compx/codec.ts`, `src/wlmouse/index.ts`, `src/lamzu/index.ts`,
//!     `src/drivers/wlmouse/hid.ts`, `src/drivers/lamzu/hid.ts`, `src/drivers/vendors.ts`
//!   - **AGPL-3.0-or-later**, Copyright (C) OpenMouse Project Contributors
//!
//! No HID bytes are guessed. DFU bootloader PIDs and Beast X 4K (`36a7:a887`, which speaks a
//! separate Report-4 protocol) are deliberately excluded.

use std::collections::HashSet;
use std::thread;
use std::time::Duration;

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice, MouseSettings,
    SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};
use crate::hid::parse_hid_report_descriptor;

pub const WLMOUSE_VID: u16 = 0x36A7;
pub const LAMZU_COMPX_VID: u16 = 0x373E;
pub const LAMZU_INCA_VID: u16 = 0x37B0;

pub const COMPX64_REPORT_ID: u8 = 0x00;
pub const COMPX64_PACKET_LEN: usize = 64;
pub const COMPX64_HEADER_LEN: usize = 6;

const STATUS_REQUEST: u8 = 0x00;
const STATUS_OK: u8 = 0xA1;
const STATUS_UNSUPPORTED: u8 = 0xA2;

const TARGET_PAIRING: u8 = 0x01;
const TARGET_MOUSE: u8 = 0x02;

const PAGE_DEVICE: u8 = 0x00;
const PAGE_PROFILE: u8 = 0x01;

const CMD_READ_FIRMWARE: u8 = 0x81;
const CMD_READ_ACTIVE_PROFILE: u8 = 0x85;
const CMD_READ_DEBOUNCE: u8 = 0x88;
const CMD_READ_PAIRED_PRODUCT: u8 = 0x8B;

const CMD_READ_POLLING: u8 = 0x80;
const CMD_READ_DPI_STAGES: u8 = 0x81;
const CMD_READ_ACTIVE_STAGE: u8 = 0x82;
const CMD_READ_LOD: u8 = 0x88;

const CMD_WRITE_POLLING: u8 = 0x00;
const CMD_WRITE_DPI_STAGES: u8 = 0x01;
const CMD_WRITE_LOD: u8 = 0x08;
const CMD_WRITE_DEBOUNCE: u8 = 0x08;

const DPI_STEP: u16 = 50;
const DEBOUNCE_MAX_MS: u8 = 15;
const RESPONSE_DELAY_MS: u64 = 30;
const WAKE_DELAY_MS: u64 = 200;
const QUICK_ATTEMPTS: usize = 3;
const RESPONSE_ATTEMPTS: usize = 10;

const RATES_1K: &[u16] = &[125, 250, 500, 1000];
const RATES_8K: &[u16] = &[500, 1000, 2000, 4000, 8000];
const RATES_8K_FULL: &[u16] = &[125, 250, 500, 1000, 2000, 4000, 8000];
const RATES_VANCER_WIRED: &[u16] = &[500, 1000];

const POLLING_TABLE: &[(u8, u16)] = &[
    (0x08, 125),
    (0x04, 250),
    (0x02, 500),
    (0x01, 1000),
    (0x10, 1000),
    (0x20, 2000),
    (0x40, 4000),
    (0x80, 8000),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compx64ProductSpec {
    pub vid: u16,
    pub pid: u16,
    pub brand: &'static str,
    pub model: &'static str,
    pub wireless: bool,
    pub max_dpi: u16,
    pub max_dpi_stages: u8,
    pub polling_rates: &'static [u16],
    pub mouse_target: u8,
    pub shared_receiver: bool,
}

const fn wl(
    pid: u16,
    model: &'static str,
    wireless: bool,
    rates: &'static [u16],
) -> Compx64ProductSpec {
    Compx64ProductSpec {
        vid: WLMOUSE_VID,
        pid,
        brand: "WLmouse",
        model,
        wireless,
        max_dpi: 30_000,
        max_dpi_stages: 6,
        polling_rates: rates,
        mouse_target: TARGET_MOUSE,
        shared_receiver: pid == 0xA882,
    }
}

#[allow(clippy::too_many_arguments)]
const fn lamzu_shared(
    pid: u16,
    brand: &'static str,
    model: &'static str,
    wireless: bool,
    max_dpi: u16,
    max_dpi_stages: u8,
    polling_rates: &'static [u16],
    mouse_target: u8,
) -> Compx64ProductSpec {
    Compx64ProductSpec {
        vid: LAMZU_COMPX_VID,
        pid,
        brand,
        model,
        wireless,
        max_dpi,
        max_dpi_stages,
        polling_rates,
        mouse_target,
        shared_receiver: false,
    }
}

const fn lamzu_inca(
    pid: u16,
    model: &'static str,
    wireless: bool,
    polling_rates: &'static [u16],
    shared_receiver: bool,
) -> Compx64ProductSpec {
    Compx64ProductSpec {
        vid: LAMZU_INCA_VID,
        pid,
        brand: "Lamzu",
        model,
        wireless,
        max_dpi: 30_000,
        max_dpi_stages: 6,
        polling_rates,
        mouse_target: TARGET_MOUSE,
        shared_receiver,
    }
}

pub const COMPX64_PRODUCTS: &[Compx64ProductSpec] = &[
    // --- WLMouse (0x36A7) ---
    wl(0xA860, "Beast G", true, RATES_8K_FULL),
    wl(0xA861, "Beast G (Wired)", false, RATES_8K_FULL),
    wl(0xA863, "Huan", true, RATES_8K_FULL),
    wl(0xA864, "Huan (Wired)", false, RATES_8K_FULL),
    wl(0xA866, "Beast Miao", true, RATES_8K_FULL),
    wl(0xA867, "Beast Miao (Wired)", false, RATES_8K_FULL),
    wl(0xA868, "Beast Mini Pro", true, RATES_8K_FULL),
    wl(0xA869, "Beast Mini Pro (Wired)", false, RATES_8K_FULL),
    wl(0xA870, "Beast X Pro", true, RATES_8K_FULL),
    wl(0xA871, "Beast X Pro (Wired)", false, RATES_8K_FULL),
    wl(0xA872, "Strider", true, RATES_8K_FULL),
    wl(0xA873, "Strider (Wired)", false, RATES_8K_FULL),
    wl(0xA874, "Ying", true, RATES_8K_FULL),
    wl(0xA875, "Ying (Wired)", false, RATES_8K_FULL),
    wl(0xA878, "Sword X", true, RATES_8K_FULL),
    wl(0xA879, "Sword X (Wired)", false, RATES_8K_FULL),
    wl(0xA880, "Beast Max", true, RATES_8K_FULL),
    wl(0xA881, "Beast Max (Wired)", false, RATES_8K_FULL),
    wl(0xA882, "1K Receiver", true, RATES_1K),
    wl(0xA883, "Beast X", true, RATES_8K_FULL),
    wl(0xA884, "Beast X (Wired)", false, RATES_8K_FULL),
    wl(0xA885, "Beast Mini", true, RATES_8K_FULL),
    wl(0xA886, "Beast Mini (Wired)", false, RATES_8K_FULL),
    // --- Lamzu / Attack Shark / Vancer / RAWM / CRDRAKO / LunaFury (0x373E) ---
    lamzu_shared(
        0x001C,
        "Lamzu",
        "Maya X (Wired)",
        false,
        30_000,
        6,
        RATES_1K,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x001D,
        "Lamzu",
        "Maya X 1K",
        true,
        30_000,
        6,
        RATES_1K,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x001E,
        "Lamzu",
        "Maya X 8K",
        true,
        30_000,
        6,
        RATES_8K,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x0046,
        "Attack Shark",
        "R5 Ultra (Wired)",
        false,
        42_000,
        6,
        RATES_1K,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x0047,
        "Attack Shark",
        "R5 Ultra",
        true,
        42_000,
        6,
        RATES_8K_FULL,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x0021,
        "Attack Shark",
        "R6 (Wired)",
        false,
        42_000,
        6,
        RATES_1K,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x0022,
        "Attack Shark",
        "R6",
        true,
        42_000,
        6,
        RATES_8K_FULL,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x003A,
        "Attack Shark",
        "R8 (Wired)",
        false,
        42_000,
        6,
        RATES_1K,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x003B,
        "Attack Shark",
        "R8",
        true,
        42_000,
        6,
        RATES_8K_FULL,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x0066,
        "Vancer",
        "Akitsu Medium 3950 (Wired)",
        false,
        30_000,
        5,
        RATES_VANCER_WIRED,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x0067,
        "Vancer",
        "Akitsu Medium 3950",
        true,
        30_000,
        5,
        RATES_8K,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x0098,
        "RAWM",
        "Leviathan V4 GT (Wired)",
        false,
        45_000,
        6,
        RATES_8K_FULL,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x0099,
        "RAWM",
        "Leviathan V4 GT",
        true,
        45_000,
        6,
        RATES_8K_FULL,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x006A,
        "CRDRAKO",
        "KO-ONE (Wired)",
        false,
        30_000,
        6,
        RATES_8K_FULL,
        0x00,
    ),
    lamzu_shared(
        0x006B,
        "CRDRAKO",
        "KO-ONE",
        true,
        30_000,
        6,
        RATES_8K_FULL,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x0032,
        "LunaFury",
        "LUNA33 (Wired)",
        false,
        30_000,
        6,
        RATES_1K,
        0x00,
    ),
    lamzu_shared(
        0x0033,
        "LunaFury",
        "LUNA33",
        true,
        30_000,
        6,
        RATES_8K_FULL,
        TARGET_MOUSE,
    ),
    lamzu_shared(
        0x0054,
        "LunaFury",
        "TYPE33 (Wired)",
        false,
        30_000,
        5,
        RATES_8K_FULL,
        0x00,
    ),
    lamzu_shared(
        0x0084,
        "LunaFury",
        "TYPE33",
        true,
        30_000,
        5,
        RATES_8K_FULL,
        TARGET_MOUSE,
    ),
    // --- Lamzu Aurora / Inca (0x37B0) ---
    lamzu_inca(0x0001, "Aurora 8K Receiver", true, RATES_8K, true),
    lamzu_inca(0x0003, "Aurora 1K Receiver", true, RATES_1K, true),
    lamzu_inca(0x0009, "Inca 8K (Wired)", false, RATES_1K, false),
    lamzu_inca(0x000F, "Inca 8K (1K)", true, RATES_1K, false),
    lamzu_inca(0x0010, "Inca 8K", true, RATES_8K, false),
    lamzu_inca(0x0007, "Paro Aurora (Wired)", false, RATES_1K, false),
    lamzu_inca(0x000D, "Paro Aurora (1K)", true, RATES_1K, false),
    lamzu_inca(0x000E, "Paro Aurora", true, RATES_8K, false),
];

pub fn lookup_compx64_product(vid: u16, pid: u16) -> Option<Compx64ProductSpec> {
    COMPX64_PRODUCTS
        .iter()
        .copied()
        .find(|p| p.vid == vid && p.pid == pid)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compx64DpiStage {
    pub x: u16,
    pub y: u16,
}

/// Encodes a 64-byte CompX page/command request packet.
pub fn encode_compx64_request(
    target: u8,
    page: u8,
    command: u8,
    length: u8,
    args: &[u8],
) -> [u8; COMPX64_PACKET_LEN] {
    let mut pkt = [0u8; COMPX64_PACKET_LEN];
    pkt[0] = STATUS_REQUEST;
    pkt[2] = target;
    pkt[3] = length;
    pkt[4] = page;
    pkt[5] = command;
    let copy_len = args.len().min(COMPX64_PACKET_LEN - COMPX64_HEADER_LEN);
    pkt[COMPX64_HEADER_LEN..COMPX64_HEADER_LEN + copy_len].copy_from_slice(&args[..copy_len]);
    pkt
}

/// Decodes a 64-byte CompX feature reply, handling both 0-byte and 1-byte leading report ID offsets.
pub fn decode_compx64_reply(
    reply: &[u8],
    page: u8,
    command: u8,
) -> Option<std::result::Result<Vec<u8>, &'static str>> {
    for offset in [0usize, 1] {
        if reply.len() < COMPX64_HEADER_LEN + offset {
            continue;
        }
        if reply[4 + offset] != page || reply[5 + offset] != command {
            continue;
        }
        let status = reply[offset];
        if status == STATUS_UNSUPPORTED {
            return Some(Err("unsupported"));
        }
        if status != STATUS_OK {
            continue;
        }
        let start = COMPX64_HEADER_LEN + offset;
        let payload_len = usize::from(reply[3 + offset]).min(reply.len().saturating_sub(start));
        return Some(Ok(reply[start..start + payload_len].to_vec()));
    }
    None
}

pub fn decode_compx64_dpi_stages(payload: &[u8]) -> Vec<Compx64DpiStage> {
    let count = usize::from(payload.get(1).copied().unwrap_or(0));
    let mut stages = Vec::with_capacity(count);
    for idx in 0..count {
        let off = 2 + idx * 4;
        if off + 3 >= payload.len() {
            break;
        }
        let x = u16::from_be_bytes([payload[off], payload[off + 1]]);
        let y = u16::from_be_bytes([payload[off + 2], payload[off + 3]]);
        stages.push(Compx64DpiStage { x, y });
    }
    stages
}

pub fn encode_compx64_polling_rate(hz: u16) -> Option<u8> {
    POLLING_TABLE
        .iter()
        .find_map(|&(code, rate)| (rate == hz).then_some(code))
}

pub fn decode_compx64_polling_rate(code: u8) -> Option<u16> {
    POLLING_TABLE
        .iter()
        .find_map(|&(c, rate)| (c == code).then_some(rate))
}

pub fn encode_compx64_lod(lod: LiftOffDistance) -> u8 {
    match lod {
        LiftOffDistance::Mm07 => 0x87,
        LiftOffDistance::Mm10 => 0x01,
        LiftOffDistance::Mm20 => 0x02,
    }
}

pub fn decode_compx64_lod(raw: u8) -> Option<LiftOffDistance> {
    if raw == 0 {
        return None;
    }
    let tenths_mm = if (raw & 0x80) != 0 {
        raw & 0x7F
    } else {
        raw.saturating_mul(10)
    };
    if tenths_mm < 10 {
        Some(LiftOffDistance::Mm07)
    } else if tenths_mm < 20 {
        Some(LiftOffDistance::Mm10)
    } else {
        Some(LiftOffDistance::Mm20)
    }
}

/// Hardware driver for 64-byte CompX page/command mice (WLMouse, Lamzu Maya X/Inca/Paro, etc.).
pub struct Compx64Device {
    hid: HidDevice,
    spec: Compx64ProductSpec,
    display_name: String,
    vid_pid: String,
}

impl Compx64Device {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.display_name.clone(),
            message: message.into(),
        }
    }

    fn exchange_raw(
        hid: &HidDevice,
        target: u8,
        page: u8,
        command: u8,
        length: u8,
        args: &[u8],
        attempts: usize,
    ) -> std::result::Result<Vec<u8>, String> {
        let req = encode_compx64_request(target, page, command, length, args);
        let mut send_buf = [0u8; COMPX64_PACKET_LEN + 1];
        send_buf[0] = COMPX64_REPORT_ID;
        send_buf[1..].copy_from_slice(&req);

        for attempt in 0..attempts {
            hid.send_feature_report(&send_buf).map_err(|e| {
                format!("CompX64 write page=0x{page:02x} cmd=0x{command:02x} failed: {e}")
            })?;
            thread::sleep(Duration::from_millis(RESPONSE_DELAY_MS));

            let mut recv_buf = [0u8; COMPX64_PACKET_LEN + 1];
            recv_buf[0] = COMPX64_REPORT_ID;
            let n = hid.get_feature_report(&mut recv_buf).map_err(|e| {
                format!("CompX64 read page=0x{page:02x} cmd=0x{command:02x} failed: {e}")
            })?;
            if let Some(res) = decode_compx64_reply(&recv_buf[..n], page, command) {
                return res
                    .map_err(|e| format!("CompX64 page=0x{page:02x} cmd=0x{command:02x}: {e}"));
            }
            thread::sleep(Duration::from_millis(if attempt < QUICK_ATTEMPTS {
                RESPONSE_DELAY_MS
            } else {
                WAKE_DELAY_MS
            }));
        }
        Err(format!(
            "Timed out waiting for CompX64 reply (page=0x{page:02x}, cmd=0x{command:02x}; move mouse to wake)"
        ))
    }

    fn request(
        &self,
        target: u8,
        page: u8,
        command: u8,
        length: u8,
        args: &[u8],
    ) -> Result<Vec<u8>> {
        Self::exchange_raw(
            &self.hid,
            target,
            page,
            command,
            length,
            args,
            RESPONSE_ATTEMPTS,
        )
        .map_err(|msg| self.dev_err(msg))
    }

    fn read_active_profile(&self) -> u8 {
        self.request(
            self.spec.mouse_target,
            PAGE_DEVICE,
            CMD_READ_ACTIVE_PROFILE,
            0x01,
            &[],
        )
        .ok()
        .and_then(|r| r.first().copied())
        .map(|p| p.max(1))
        .unwrap_or(1)
    }

    fn write_profile_cmd(&self, page: u8, command: u8, profile: u8, values: &[u8]) -> Result<()> {
        let mut args = Vec::with_capacity(1 + values.len());
        args.push(profile);
        args.extend_from_slice(values);
        let _ = self.request(
            self.spec.mouse_target,
            page,
            command,
            args.len() as u8,
            &args,
        )?;
        Ok(())
    }

    fn try_resolve_paired_product(&mut self) {
        if !self.spec.shared_receiver {
            return;
        }
        if let Ok(reply) = Self::exchange_raw(
            &self.hid,
            TARGET_PAIRING,
            PAGE_DEVICE,
            CMD_READ_PAIRED_PRODUCT,
            0x06,
            &[0x02],
            2,
        ) {
            if reply.len() >= 6 {
                let paired_pid = u16::from_be_bytes([reply[4], reply[5]]);
                if let Some(paired) = lookup_compx64_product(self.spec.vid, paired_pid) {
                    self.display_name = format!("{} {}", paired.brand, paired.model);
                    self.spec.max_dpi = paired.max_dpi;
                    self.spec.max_dpi_stages = paired.max_dpi_stages;
                }
            }
        }
    }
}

impl MouseDevice for Compx64Device {
    fn info(&self) -> DeviceInfo {
        let id = format!("compx64_{:04x}_{:04x}", self.spec.vid, self.spec.pid);
        DeviceInfo {
            id,
            name: self.display_name.clone(),
            vendor: self.spec.brand.to_string(),
            vid_pid: Some(self.vid_pid.clone()),
            connection: if self.spec.wireless {
                "Wireless 2.4 GHz (CompX 64B)".to_string()
            } else {
                "Wired USB (CompX 64B)".to_string()
            },
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            dpi_min: DPI_STEP,
            dpi_max: self.spec.max_dpi,
            dpi_step: DPI_STEP,
            polling_rates_hz: self.spec.polling_rates.to_vec(),
            lod_options: LiftOffDistance::ALL.to_vec(),
            supports_debounce: true,
            debounce_min_ms: 0,
            debounce_max_ms: DEBOUNCE_MAX_MS,
            supports_volatile_writes: false,
            flash_write_warning: Some(format!(
                "{} writes profile settings to onboard flash. Polled diffs unchanged fields and rate-limits rapid writes.",
                self.display_name
            )),
        }
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        let profile = self.read_active_profile();
        let stages_reply = self.request(
            self.spec.mouse_target,
            PAGE_PROFILE,
            CMD_READ_DPI_STAGES,
            0x0A,
            &[profile, self.spec.max_dpi_stages],
        )?;
        let stages = decode_compx64_dpi_stages(&stages_reply);
        let active_reply = self.request(
            self.spec.mouse_target,
            PAGE_PROFILE,
            CMD_READ_ACTIVE_STAGE,
            0x02,
            &[profile],
        )?;
        let active_idx = usize::from(active_reply.get(1).copied().unwrap_or(1).max(1))
            .saturating_sub(1)
            .min(stages.len().saturating_sub(1));
        let dpi = stages.get(active_idx).map(|s| s.x);

        let poll_reply = self.request(
            self.spec.mouse_target,
            PAGE_PROFILE,
            CMD_READ_POLLING,
            0x02,
            &[profile],
        )?;
        let polling_rate_hz = poll_reply
            .get(1)
            .copied()
            .and_then(decode_compx64_polling_rate);

        let lod_reply = self
            .request(
                self.spec.mouse_target,
                PAGE_PROFILE,
                CMD_READ_LOD,
                0x02,
                &[profile],
            )
            .ok();
        let lod = lod_reply
            .as_deref()
            .and_then(|r| r.get(1).copied())
            .and_then(decode_compx64_lod);

        let db_reply = self
            .request(
                self.spec.mouse_target,
                PAGE_DEVICE,
                CMD_READ_DEBOUNCE,
                0x02,
                &[profile],
            )
            .ok();
        let debounce_ms = db_reply.as_deref().and_then(|r| r.get(1).copied());

        Ok(MouseSettings {
            dpi,
            polling_rate_hz,
            lod,
            debounce_ms,
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        let current = self.read_settings()?;
        let profile = self.read_active_profile();

        if let Some(target_dpi) = settings.dpi {
            if current.dpi != Some(target_dpi) {
                let stages_reply = self.request(
                    self.spec.mouse_target,
                    PAGE_PROFILE,
                    CMD_READ_DPI_STAGES,
                    0x0A,
                    &[profile, self.spec.max_dpi_stages],
                )?;
                let mut stages = decode_compx64_dpi_stages(&stages_reply);
                if stages.is_empty() {
                    return Err(self.dev_err("Mouse did not report any DPI stages"));
                }
                let active_reply = self.request(
                    self.spec.mouse_target,
                    PAGE_PROFILE,
                    CMD_READ_ACTIVE_STAGE,
                    0x02,
                    &[profile],
                )?;
                let active_idx = usize::from(active_reply.get(1).copied().unwrap_or(1).max(1))
                    .saturating_sub(1)
                    .min(stages.len() - 1);
                stages[active_idx] = Compx64DpiStage {
                    x: target_dpi,
                    y: target_dpi,
                };

                let mut payload = Vec::with_capacity(1 + stages.len() * 4);
                payload.push(stages.len() as u8);
                for st in &stages {
                    payload.extend_from_slice(&st.x.to_be_bytes());
                    payload.extend_from_slice(&st.y.to_be_bytes());
                }
                self.write_profile_cmd(PAGE_PROFILE, CMD_WRITE_DPI_STAGES, profile, &payload)?;
            }
        }

        if let Some(target_hz) = settings.polling_rate_hz {
            if current.polling_rate_hz != Some(target_hz) {
                let code = encode_compx64_polling_rate(target_hz).ok_or_else(|| {
                    PolledError::UnsupportedSetting {
                        device: self.display_name.clone(),
                        detail: format!("Unsupported polling rate {target_hz} Hz"),
                    }
                })?;
                self.write_profile_cmd(PAGE_PROFILE, CMD_WRITE_POLLING, profile, &[code])?;
            }
        }

        if let Some(target_lod) = settings.lod {
            if current.lod != Some(target_lod) {
                let code = encode_compx64_lod(target_lod);
                self.write_profile_cmd(PAGE_PROFILE, CMD_WRITE_LOD, profile, &[code])?;
            }
        }

        if let Some(target_db) = settings.debounce_ms {
            if current.debounce_ms != Some(target_db) {
                self.write_profile_cmd(PAGE_DEVICE, CMD_WRITE_DEBOUNCE, profile, &[target_db])?;
            }
        }

        Ok(())
    }
}

/// Protocol-family auto-detector for 64-byte CompX page/command mice.
pub struct Compx64ProtocolDriver;

impl DeviceDriver for Compx64ProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "CompX 64B Page-Command (WLMouse / Lamzu 8K / Attack Shark)"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();
        let mut claimed: HashSet<(u16, u16, String)> = HashSet::new();

        for dev in api.device_list() {
            let Some(spec) = lookup_compx64_product(dev.vendor_id(), dev.product_id()) else {
                continue;
            };
            let dedup_key = (
                spec.vid,
                spec.pid,
                dev.serial_number().unwrap_or_default().to_string(),
            );
            if claimed.contains(&dedup_key) {
                continue;
            }
            let Ok(hid) = dev.open_device(api) else {
                continue;
            };

            let mut desc_buf = [0u8; 4096];
            let has_feat0 = hid
                .get_report_descriptor(&mut desc_buf)
                .ok()
                .map(|len| parse_hid_report_descriptor(&desc_buf[..len]))
                .is_some_and(|s| s.feature_reports.contains_key(&COMPX64_REPORT_ID));
            if !has_feat0 {
                continue;
            }

            // Probe read-only firmware command to confirm this exact collection answers.
            if Compx64Device::exchange_raw(
                &hid,
                spec.mouse_target,
                PAGE_DEVICE,
                CMD_READ_FIRMWARE,
                0x10,
                &[],
                2,
            )
            .is_err()
            {
                continue;
            }

            claimed.insert(dedup_key);
            let mut device = Compx64Device {
                hid,
                spec,
                display_name: format!("{} {}", spec.brand, spec.model),
                vid_pid: format!("{:04x}:{:04x}", spec.vid, spec.pid),
            };
            device.try_resolve_paired_product();
            found.push(Box::new(device));
        }

        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_request_and_decodes_dpi_polling_and_lod() {
        let pkt = encode_compx64_request(
            TARGET_MOUSE,
            PAGE_PROFILE,
            CMD_READ_DPI_STAGES,
            0x0A,
            &[1, 6],
        );
        assert_eq!(pkt[0], STATUS_REQUEST);
        assert_eq!(pkt[2], TARGET_MOUSE);
        assert_eq!(pkt[3], 0x0A);
        assert_eq!(pkt[4], PAGE_PROFILE);
        assert_eq!(pkt[5], CMD_READ_DPI_STAGES);
        assert_eq!(&pkt[6..8], &[1, 6]);

        // Reply with 1-byte report ID prefix (Windows hidapi layout).
        let mut reply = vec![0u8; 65];
        reply[1] = STATUS_OK;
        reply[4] = 10; // payload length
        reply[5] = PAGE_PROFILE;
        reply[6] = CMD_READ_DPI_STAGES;
        reply[7] = 1; // profile
        reply[8] = 2; // 2 stages
        reply[9..13].copy_from_slice(&[0x03, 0x20, 0x03, 0x20]); // 800/800
        reply[13..17].copy_from_slice(&[0x06, 0x40, 0x06, 0x40]); // 1600/1600

        let payload = decode_compx64_reply(&reply, PAGE_PROFILE, CMD_READ_DPI_STAGES)
            .unwrap()
            .unwrap();
        let stages = decode_compx64_dpi_stages(&payload);
        assert_eq!(
            stages,
            vec![
                Compx64DpiStage { x: 800, y: 800 },
                Compx64DpiStage { x: 1600, y: 1600 },
            ]
        );

        assert_eq!(decode_compx64_polling_rate(0x01), Some(1000));
        assert_eq!(decode_compx64_polling_rate(0x10), Some(1000));
        assert_eq!(decode_compx64_polling_rate(0x80), Some(8000));
        assert_eq!(encode_compx64_polling_rate(4000), Some(0x40));

        assert_eq!(decode_compx64_lod(0x87), Some(LiftOffDistance::Mm07));
        assert_eq!(decode_compx64_lod(0x01), Some(LiftOffDistance::Mm10));
        assert_eq!(decode_compx64_lod(0x02), Some(LiftOffDistance::Mm20));
    }

    #[test]
    fn resolves_wlmouse_lamzu_attack_shark_and_excludes_beast_x_4k() {
        assert_eq!(
            lookup_compx64_product(WLMOUSE_VID, 0xA870).unwrap().model,
            "Beast X Pro"
        );
        assert!(lookup_compx64_product(WLMOUSE_VID, 0xA887).is_none());
        assert_eq!(
            lookup_compx64_product(LAMZU_COMPX_VID, 0x001E)
                .unwrap()
                .model,
            "Maya X 8K"
        );
        assert_eq!(
            lookup_compx64_product(LAMZU_INCA_VID, 0x0010)
                .unwrap()
                .model,
            "Inca 8K"
        );
        assert_eq!(
            lookup_compx64_product(LAMZU_COMPX_VID, 0x0047)
                .unwrap()
                .brand,
            "Attack Shark"
        );
    }
}
