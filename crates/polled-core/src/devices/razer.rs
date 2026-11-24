// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Razer 90-byte HID feature-report protocol-family driver (Viper V3 Pro / SE / HyperSpeed,
//! Viper V2 Pro, Viper Ultimate, Viper 8KHz, DeathAdder V4 Pro / V3 Pro / V3 / V2 / Essential,
//! Basilisk V3 Pro / V3 / X, Cobra Pro, Naga V2 Pro / Pro / X, Orochi V2, Mouse Dock Pro,
//! HyperPolling Wireless Dongle, and Chroma-era wired/wireless models).
//!
//! # Protocol Attribution & Licenses (AGPL-3.0 compatible)
//! Packet framing, XOR checksum, command classes/IDs, per-PID transaction IDs, storage selectors,
//! extended polling-rate commit sequencing, and product capability tables are ported from:
//! - `OpenMouse-Project/mouse-protocol` (<https://github.com/OpenMouse-Project/mouse-protocol>)
//!   - `src/razer/codec.ts`, `src/razer/devices.ts`, `src/drivers/razer/hid.ts`
//!   - **AGPL-3.0-or-later**, Copyright (C) OpenMouse Project Contributors
//! - `openrazer/openrazer` (<https://github.com/openrazer/openrazer>)
//!   - **GPL-2.0-or-later**
//!
//! No HID bytes are guessed. Button remapping, firmware flashing, and asymmetric lift-off
//! write probes are never issued; only DPI, polling rate, and symmetric tracking distance
//! (when explicitly supported by the PID entry) are managed.

use std::collections::HashSet;
use std::thread;
use std::time::Duration;

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    BatteryStatus, DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice,
    MouseSettings, SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};

pub const RAZER_VID: u16 = 0x1532;
pub const RAZER_REPORT_ID: u8 = 0x00;
pub const RAZER_PACKET_LEN: usize = 90;

pub const RAZER_TX_FF: u8 = 0xFF;
pub const RAZER_TX_3F: u8 = 0x3F;
pub const RAZER_TX_1F: u8 = 0x1F;

pub const RAZER_STORAGE_NOSTORE: u8 = 0x00;
pub const RAZER_STORAGE_VARSTORE: u8 = 0x01;

const ARGS_OFFSET: usize = 8;
const CHECKSUM_INDEX: usize = 88;
const CHECKSUM_FIRST: usize = 2;
const CHECKSUM_LAST: usize = 88;

const STATUS_BUSY: u8 = 0x01;
const STATUS_OK: u8 = 0x02;
const STATUS_UNSUPPORTED: u8 = 0x05;

const SENSOR_SELECTOR: u8 = 0x04;
const SENSOR_SETTING_TRACKING_DISTANCE: u8 = 0x01;

const DPI_MIN: u16 = 100;
const DPI_STEP: u16 = 50;

const RESPONSE_DELAY_MS: u64 = 60;
const PROBE_DELAY_MS: u64 = 25;
const RESPONSE_ATTEMPTS: usize = 6;
const POLLING_LINK_SETTLE_MS: u64 = 150;

const RATES_1K: &[u16] = &[125, 500, 1000];
const RATES_8K: &[u16] = &[125, 500, 1000, 2000, 4000, 8000];

const DPI_PRE_CHROMA: u16 = 8_200;
const DPI_CHROMA: u16 = 16_000;
const DPI_FOCUS: u16 = 20_000;
const DPI_FOCUS_PRO: u16 = 30_000;
const DPI_FOCUS_PRO_35K: u16 = 35_000;
const DPI_DEATHADDER_V4_PRO: u16 = 45_000;

/// Wire-level Razer 90-byte command descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RazerCommand {
    pub command_class: u8,
    pub command_id: u8,
    pub data_size: u8,
    pub args: Vec<u8>,
}

impl RazerCommand {
    pub fn is_getter(&self) -> bool {
        (self.command_id & 0x80) != 0
    }
}

/// Decoded Razer command response error classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RazerDecodeError {
    BadLength(usize),
    BadChecksum,
    Busy,
    Unsupported,
    StaleReply,
    Status(u8),
}

/// Static capability profile for one Razer USB product ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RazerProductSpec {
    pub pid: u16,
    pub model: &'static str,
    pub wireless: bool,
    pub max_dpi: u16,
    pub polling_rates: &'static [u16],
    pub high_rate_polling: bool,
    pub extended_polling_commit_tx: Option<u8>,
    pub dpi_storage_byte: u8,
    pub lift_off: bool,
    pub vendor_control_interface: bool,
    pub probe_polling_rates: bool,
    pub v4_dpi_protocol: bool,
    pub connection_label: Option<&'static str>,
}

impl RazerProductSpec {
    pub fn transaction_id(self) -> u8 {
        transaction_id_for(self.pid)
    }
}

/// Audited per-PID `0x3F` transaction IDs (from OpenMouse / OpenRazer).
const TRANSACTION_3F: &[u16] = &[
    0x0050, 0x0059, 0x005A, 0x005C, 0x0060, 0x0064, 0x0065, 0x006E, 0x006F, 0x0070, 0x0071, 0x0072,
    0x0073, 0x007A, 0x007B, 0x007C, 0x007D, 0x0084, 0x008C, 0x0098,
];

/// Audited per-PID `0x1F` transaction IDs (from OpenMouse / OpenRazer).
const TRANSACTION_1F: &[u16] = &[
    0x0062, 0x006C, 0x0077, 0x0080, 0x0085, 0x0086, 0x0088, 0x008D, 0x008F, 0x0090, 0x0094, 0x0096,
    0x0099, 0x009A, 0x009C, 0x009E, 0x009F, 0x00A1, 0x00A4, 0x00A5, 0x00A6, 0x00A7, 0x00A8, 0x00AA,
    0x00AB, 0x00AF, 0x00B0, 0x00B2, 0x00B3, 0x00B4, 0x00B6, 0x00B7, 0x00B8, 0x00B9, 0x00BE, 0x00BF,
    0x00C0, 0x00C1, 0x00C2, 0x00C3, 0x00C4, 0x00C5, 0x00C7, 0x00C8, 0x00CB, 0x00CC, 0x00CD, 0x00D0,
    0x00D1, 0x00D3, 0x00D4, 0x00D6, 0x00D7, 0x00DE, 0x00DF, 0x00E5, 0x00E6, 0x00EF, 0x00F0,
];

/// Returns the Razer protocol transaction ID for a catalogued product ID (`0x3F`, `0x1F`, or `0xFF`).
pub fn transaction_id_for(pid: u16) -> u8 {
    if TRANSACTION_3F.contains(&pid) {
        RAZER_TX_3F
    } else if TRANSACTION_1F.contains(&pid) {
        RAZER_TX_1F
    } else {
        RAZER_TX_FF
    }
}

const fn std_wired(pid: u16, model: &'static str, max_dpi: u16) -> RazerProductSpec {
    RazerProductSpec {
        pid,
        model,
        wireless: false,
        max_dpi,
        polling_rates: RATES_1K,
        high_rate_polling: false,
        extended_polling_commit_tx: None,
        dpi_storage_byte: RAZER_STORAGE_VARSTORE,
        lift_off: false,
        vendor_control_interface: true,
        probe_polling_rates: false,
        v4_dpi_protocol: false,
        connection_label: None,
    }
}

const fn std_wireless(pid: u16, model: &'static str, max_dpi: u16) -> RazerProductSpec {
    RazerProductSpec {
        wireless: true,
        ..std_wired(pid, model, max_dpi)
    }
}

const fn modern_wired(pid: u16, model: &'static str, max_dpi: u16) -> RazerProductSpec {
    RazerProductSpec {
        pid,
        model,
        wireless: false,
        max_dpi,
        polling_rates: RATES_1K,
        high_rate_polling: false,
        extended_polling_commit_tx: None,
        dpi_storage_byte: RAZER_STORAGE_VARSTORE,
        lift_off: false,
        vendor_control_interface: false,
        probe_polling_rates: false,
        v4_dpi_protocol: false,
        connection_label: None,
    }
}

const fn modern_receiver(
    pid: u16,
    model: &'static str,
    max_dpi: u16,
    high_rate_polling: bool,
) -> RazerProductSpec {
    RazerProductSpec {
        wireless: true,
        high_rate_polling,
        ..modern_wired(pid, model, max_dpi)
    }
}

const fn hyperpolling_receiver(pid: u16, model: &'static str, max_dpi: u16) -> RazerProductSpec {
    RazerProductSpec {
        polling_rates: RATES_8K,
        ..modern_receiver(pid, model, max_dpi, true)
    }
}

/// Complete catalog of Razer mice speaking the 90-byte HID feature-report protocol.
pub const RAZER_PRODUCTS: &[RazerProductSpec] = &[
    // --- Flagship Viper V4 Pro (V4 multi-stage DPI protocol) ---
    RazerProductSpec {
        polling_rates: RATES_8K,
        high_rate_polling: true,
        v4_dpi_protocol: true,
        ..modern_wired(0x00E5, "Viper V4 Pro (Wired)", 50_000)
    },
    RazerProductSpec {
        v4_dpi_protocol: true,
        ..hyperpolling_receiver(0x00E6, "Viper V4 Pro", 50_000)
    },
    // --- Flagship Viper V2 Pro / V3 Pro ---
    RazerProductSpec {
        lift_off: true,
        ..modern_wired(0x00A5, "Viper V2 Pro (Wired)", DPI_FOCUS_PRO)
    },
    RazerProductSpec {
        lift_off: true,
        ..modern_receiver(0x00A6, "Viper V2 Pro", DPI_FOCUS_PRO, true)
    },
    RazerProductSpec {
        lift_off: true,
        ..modern_wired(0x00C0, "Viper V3 Pro (Wired)", DPI_FOCUS_PRO_35K)
    },
    RazerProductSpec {
        lift_off: true,
        extended_polling_commit_tx: Some(RAZER_TX_1F),
        ..hyperpolling_receiver(0x00C1, "Viper V3 Pro", DPI_FOCUS_PRO_35K)
    },
    RazerProductSpec {
        lift_off: true,
        ..modern_receiver(0x00B8, "Viper V3 HyperSpeed", DPI_FOCUS_PRO, false)
    },
    RazerProductSpec {
        vendor_control_interface: true,
        ..modern_wired(0x00DE, "Viper V3 Pro SE (Wired)", DPI_FOCUS_PRO_35K)
    },
    RazerProductSpec {
        vendor_control_interface: true,
        ..modern_receiver(0x00DF, "Viper V3 Pro SE", DPI_FOCUS_PRO_35K, false)
    },
    // --- DeathAdder V4 Pro / V3 Pro / V3 / V2 / Essential ---
    RazerProductSpec {
        polling_rates: RATES_8K,
        high_rate_polling: true,
        ..modern_wired(0x00BE, "DeathAdder V4 Pro (Wired)", DPI_DEATHADDER_V4_PRO)
    },
    hyperpolling_receiver(0x00BF, "DeathAdder V4 Pro", DPI_DEATHADDER_V4_PRO),
    RazerProductSpec {
        polling_rates: RATES_8K,
        high_rate_polling: true,
        ..modern_wired(
            0x00EF,
            "DeathAdder V4 Pro Carbon Fiber (Wired)",
            DPI_DEATHADDER_V4_PRO,
        )
    },
    hyperpolling_receiver(
        0x00F0,
        "DeathAdder V4 Pro Carbon Fiber",
        DPI_DEATHADDER_V4_PRO,
    ),
    RazerProductSpec {
        polling_rates: RATES_8K,
        high_rate_polling: true,
        extended_polling_commit_tx: Some(RAZER_TX_1F),
        ..std_wired(0x00B2, "DeathAdder V3", DPI_FOCUS_PRO)
    },
    modern_wired(0x00B6, "DeathAdder V3 Pro (Wired)", DPI_FOCUS_PRO),
    modern_receiver(0x00B7, "DeathAdder V3 Pro", DPI_FOCUS_PRO, false),
    modern_wired(0x00C2, "DeathAdder V3 Pro (Wired)", DPI_FOCUS_PRO),
    modern_receiver(0x00C3, "DeathAdder V3 Pro", DPI_FOCUS_PRO, true),
    modern_wired(0x00C4, "DeathAdder V3 HyperSpeed (Wired)", DPI_FOCUS_PRO),
    modern_receiver(0x00C5, "DeathAdder V3 HyperSpeed", DPI_FOCUS_PRO, false),
    RazerProductSpec {
        dpi_storage_byte: RAZER_STORAGE_NOSTORE,
        ..std_wired(0x0084, "DeathAdder V2", DPI_FOCUS)
    },
    modern_wired(0x007C, "DeathAdder V2 Pro (Wired)", DPI_FOCUS),
    modern_receiver(0x007D, "DeathAdder V2 Pro", DPI_FOCUS, true),
    std_wired(0x008C, "DeathAdder V2 Mini", 8_500),
    std_wired(0x00A1, "DeathAdder V2 Lite", 8_500),
    modern_receiver(0x009C, "DeathAdder V2 X HyperSpeed", 14_000, false),
    std_wired(0x006E, "DeathAdder Essential", 6_400),
    std_wired(0x0071, "DeathAdder Essential White Edition", 6_400),
    std_wired(0x0098, "DeathAdder Essential (2021)", 6_400),
    // --- HyperPolling Dongle & Mouse Dock Pro ---
    RazerProductSpec {
        vendor_control_interface: true,
        extended_polling_commit_tx: Some(RAZER_TX_FF),
        connection_label: Some("HyperPolling Wireless Dongle"),
        ..hyperpolling_receiver(0x00B3, "HyperPolling Wireless Dongle", DPI_FOCUS_PRO)
    },
    RazerProductSpec {
        probe_polling_rates: true,
        connection_label: Some("Mouse Dock Pro"),
        ..modern_receiver(0x00A4, "Mouse Dock Pro", DPI_FOCUS_PRO, true)
    },
    // --- Viper Ultimate / 8KHz / Mini SE ---
    RazerProductSpec {
        polling_rates: RATES_8K,
        high_rate_polling: true,
        ..std_wired(0x0091, "Viper 8KHz", DPI_FOCUS)
    },
    modern_wired(0x007A, "Viper Ultimate (Wired)", DPI_FOCUS),
    modern_receiver(0x007B, "Viper Ultimate", DPI_FOCUS, true),
    modern_wired(
        0x009E,
        "Viper Mini Signature Edition (Wired)",
        DPI_FOCUS_PRO,
    ),
    hyperpolling_receiver(0x009F, "Viper Mini Signature Edition", DPI_FOCUS_PRO),
    // --- Basilisk / Cobra / Naga / Orochi / Pro Click ---
    std_wired(0x0099, "Basilisk V3", 26_000),
    std_wired(0x00CB, "Basilisk V3 35K", DPI_FOCUS_PRO_35K),
    modern_wired(0x00AA, "Basilisk V3 Pro (Wired)", DPI_FOCUS_PRO),
    modern_receiver(0x00AB, "Basilisk V3 Pro", DPI_FOCUS_PRO, true),
    modern_wired(0x00CC, "Basilisk V3 Pro 35K (Wired)", DPI_FOCUS_PRO_35K),
    hyperpolling_receiver(0x00CD, "Basilisk V3 Pro 35K", DPI_FOCUS_PRO_35K),
    modern_wired(
        0x00D6,
        "Basilisk V3 Pro 35K Phantom Green (Wired)",
        DPI_FOCUS_PRO_35K,
    ),
    hyperpolling_receiver(
        0x00D7,
        "Basilisk V3 Pro 35K Phantom Green",
        DPI_FOCUS_PRO_35K,
    ),
    modern_receiver(0x00B9, "Basilisk V3 X HyperSpeed", 18_000, false),
    modern_wired(0x0086, "Basilisk Ultimate (Wired)", DPI_FOCUS),
    modern_receiver(0x0088, "Basilisk Ultimate", DPI_FOCUS, false),
    std_wired(0x0085, "Basilisk V2", DPI_FOCUS),
    modern_receiver(0x0083, "Basilisk X HyperSpeed", DPI_CHROMA, false),
    std_wired(0x0064, "Basilisk", DPI_CHROMA),
    std_wired(0x0065, "Basilisk Essential", 6_400),
    modern_wired(0x00D3, "Basilisk Mobile (Wired)", 18_000),
    modern_receiver(0x00D4, "Basilisk Mobile", 18_000, false),
    modern_wired(0x00AF, "Cobra Pro (Wired)", DPI_FOCUS_PRO),
    modern_receiver(0x00B0, "Cobra Pro", DPI_FOCUS_PRO, true),
    modern_wired(0x00A7, "Naga V2 Pro (Wired)", DPI_FOCUS_PRO),
    modern_receiver(0x00A8, "Naga V2 Pro", DPI_FOCUS_PRO, true),
    modern_receiver(0x00B4, "Naga V2 HyperSpeed", DPI_FOCUS_PRO, false),
    modern_wired(0x008F, "Naga Pro (Wired)", DPI_FOCUS),
    modern_receiver(0x0090, "Naga Pro", DPI_FOCUS, false),
    std_wired(0x0096, "Naga X", 18_000),
    std_wired(0x008D, "Naga Left-Handed Edition 2020", DPI_FOCUS),
    std_wired(0x0067, "Naga Trinity", DPI_CHROMA),
    std_wireless(0x0094, "Orochi V2", 18_000),
    std_wireless(0x0062, "Atheris", 7_200),
    modern_receiver(0x0077, "Pro Click", DPI_CHROMA, false),
    modern_wired(0x0080, "Pro Click (Wired)", DPI_CHROMA),
    modern_receiver(0x009A, "Pro Click Mini", 12_000, false),
    modern_wired(0x00D0, "Pro Click V2 (Wired)", DPI_FOCUS),
    modern_receiver(0x00D1, "Pro Click V2", DPI_FOCUS, false),
    modern_wired(0x00C7, "Pro Click V2 Vertical (Wired)", DPI_FOCUS),
    modern_receiver(0x00C8, "Pro Click V2 Vertical", DPI_FOCUS, false),
    // --- Chroma & Pre-Chroma Legacy ---
    std_wired(0x0015, "Naga", DPI_PRE_CHROMA),
    std_wired(0x001F, "Naga Epic", DPI_PRE_CHROMA),
    std_wired(0x0020, "Abyssus 1800", 1_800),
    std_wired(0x0024, "Mamba 2012 (Wired)", DPI_PRE_CHROMA),
    std_wireless(0x0025, "Mamba 2012", DPI_PRE_CHROMA),
    std_wired(0x002E, "Naga 2012", DPI_PRE_CHROMA),
    std_wired(0x002F, "Imperator 2012", DPI_PRE_CHROMA),
    std_wired(0x0032, "Ouroboros 2012", DPI_PRE_CHROMA),
    std_wired(0x0034, "Taipan", DPI_PRE_CHROMA),
    std_wired(0x0036, "Naga Hex (Red)", DPI_PRE_CHROMA),
    std_wired(0x0037, "DeathAdder 2013", 6_400),
    std_wired(0x0038, "DeathAdder 1800", 1_800),
    std_wireless(0x0039, "Orochi 2013", DPI_PRE_CHROMA),
    std_wired(0x003E, "Naga Epic Chroma (Wired)", DPI_CHROMA),
    std_wireless(0x003F, "Naga Epic Chroma", DPI_CHROMA),
    std_wired(0x0040, "Naga 2014", DPI_PRE_CHROMA),
    std_wired(0x0041, "Naga Hex", DPI_CHROMA),
    std_wired(0x0042, "Abyssus 2014", 1_800),
    std_wired(0x0043, "DeathAdder Chroma", DPI_CHROMA),
    std_wired(0x0044, "Mamba (Wired)", DPI_CHROMA),
    std_wireless(0x0045, "Mamba", DPI_CHROMA),
    std_wired(0x0046, "Mamba Tournament Edition", DPI_CHROMA),
    std_wired(0x0048, "Orochi (Wired)", DPI_PRE_CHROMA),
    std_wired(0x004C, "Diamondback Chroma", DPI_CHROMA),
    std_wired(0x004F, "DeathAdder 2000", 2_000),
    std_wired(0x0050, "Naga Hex V2", DPI_CHROMA),
    std_wired(0x0053, "Naga Chroma", DPI_CHROMA),
    std_wired(0x0054, "DeathAdder 3500", 3_500),
    std_wired(0x0059, "Lancehead (Wired)", DPI_CHROMA),
    std_wireless(0x005A, "Lancehead", DPI_CHROMA),
    std_wired(0x005B, "Abyssus V2", 5_000),
    std_wired(0x005C, "DeathAdder Elite", DPI_CHROMA),
    std_wired(0x005E, "Abyssus 2000", 2_000),
    std_wired(0x0060, "Lancehead Tournament Edition", DPI_CHROMA),
    std_wired(0x006A, "Abyssus Elite (D.Va Edition)", 7_200),
    std_wired(0x006B, "Abyssus Essential", 7_200),
    std_wired(0x006C, "Mamba Elite", DPI_CHROMA),
    modern_receiver(0x006F, "Lancehead Wireless", DPI_CHROMA, false),
    modern_wired(0x0070, "Lancehead Wireless (Wired)", DPI_CHROMA),
    modern_receiver(0x0072, "Mamba Wireless", DPI_CHROMA, false),
    modern_wired(0x0073, "Mamba Wireless (Wired)", DPI_CHROMA),
];

pub fn lookup_razer_product(pid: u16) -> Option<RazerProductSpec> {
    RAZER_PRODUCTS.iter().copied().find(|p| p.pid == pid)
}

/// Computes the Razer 90-byte XOR checksum across bytes `2..88`.
pub fn razer_checksum(packet: &[u8]) -> u8 {
    let mut sum = 0u8;
    for &b in &packet[CHECKSUM_FIRST..CHECKSUM_LAST.min(packet.len())] {
        sum ^= b;
    }
    sum
}

/// Encodes a Razer command into a 90-byte request packet.
pub fn encode_razer_request(cmd: &RazerCommand, transaction_id: u8) -> [u8; RAZER_PACKET_LEN] {
    let mut pkt = [0u8; RAZER_PACKET_LEN];
    pkt[1] = transaction_id;
    pkt[5] = cmd.data_size;
    pkt[6] = cmd.command_class;
    pkt[7] = cmd.command_id;
    let copy_len = cmd.args.len().min(RAZER_PACKET_LEN - ARGS_OFFSET - 2);
    pkt[ARGS_OFFSET..ARGS_OFFSET + copy_len].copy_from_slice(&cmd.args[..copy_len]);
    pkt[CHECKSUM_INDEX] = razer_checksum(&pkt);
    pkt
}

/// Validates and extracts the response argument slice from a 90-byte Razer reply packet.
pub fn decode_razer_response(
    packet: &[u8],
    cmd: &RazerCommand,
) -> std::result::Result<Vec<u8>, RazerDecodeError> {
    if packet.len() != RAZER_PACKET_LEN {
        return Err(RazerDecodeError::BadLength(packet.len()));
    }
    if packet[CHECKSUM_INDEX] != razer_checksum(packet) {
        return Err(RazerDecodeError::BadChecksum);
    }
    let status = packet[0];
    match status {
        STATUS_BUSY => return Err(RazerDecodeError::Busy),
        STATUS_UNSUPPORTED => return Err(RazerDecodeError::Unsupported),
        STATUS_OK => {}
        other => return Err(RazerDecodeError::Status(other)),
    }
    if packet[6] != cmd.command_class || packet[7] != cmd.command_id {
        return Err(RazerDecodeError::StaleReply);
    }
    let len = usize::from(packet[5]).min(RAZER_PACKET_LEN - ARGS_OFFSET);
    Ok(packet[ARGS_OFFSET..ARGS_OFFSET + len].to_vec())
}

pub fn cmd_read_firmware() -> RazerCommand {
    RazerCommand {
        command_class: 0x00,
        command_id: 0x81,
        data_size: 0x02,
        args: Vec::new(),
    }
}

pub fn cmd_read_dpi(storage_byte: u8) -> RazerCommand {
    RazerCommand {
        command_class: 0x04,
        command_id: 0x85,
        data_size: 0x07,
        args: vec![storage_byte],
    }
}

pub fn cmd_set_dpi(x: u16, y: u16, storage_byte: u8) -> RazerCommand {
    let xb = x.to_be_bytes();
    let yb = y.to_be_bytes();
    RazerCommand {
        command_class: 0x04,
        command_id: 0x05,
        data_size: 0x07,
        args: vec![storage_byte, xb[0], xb[1], yb[0], yb[1], 0x00, 0x00],
    }
}

pub fn cmd_read_legacy_polling() -> RazerCommand {
    RazerCommand {
        command_class: 0x00,
        command_id: 0x85,
        data_size: 0x01,
        args: Vec::new(),
    }
}

pub fn cmd_set_legacy_polling(hz: u16) -> Option<RazerCommand> {
    if hz == 0 || !1000u16.is_multiple_of(hz) {
        return None;
    }
    let div = u8::try_from(1000 / hz).ok()?;
    if div == 0 {
        return None;
    }
    Some(RazerCommand {
        command_class: 0x00,
        command_id: 0x05,
        data_size: 0x01,
        args: vec![div],
    })
}

pub fn cmd_read_extended_polling() -> RazerCommand {
    RazerCommand {
        command_class: 0x00,
        command_id: 0xC0,
        data_size: 0x02,
        args: vec![0x00],
    }
}

pub fn cmd_set_extended_polling(selector: u8, hz: u16) -> Option<RazerCommand> {
    if hz == 0 || !8000u16.is_multiple_of(hz) {
        return None;
    }
    let div = u8::try_from(8000 / hz).ok()?;
    if div == 0 {
        return None;
    }
    Some(RazerCommand {
        command_class: 0x00,
        command_id: 0x40,
        data_size: 0x02,
        args: vec![selector, div],
    })
}

pub fn cmd_read_lift_off() -> RazerCommand {
    RazerCommand {
        command_class: 0x0B,
        command_id: 0x85,
        data_size: 0x05,
        args: Vec::new(),
    }
}

pub fn cmd_set_tracking_distance(lod: LiftOffDistance) -> RazerCommand {
    let level = match lod {
        LiftOffDistance::Mm07 => 0x00,
        LiftOffDistance::Mm10 => 0x01,
        LiftOffDistance::Mm20 => 0x02,
    };
    RazerCommand {
        command_class: 0x0B,
        command_id: 0x0B,
        data_size: 0x04,
        args: vec![
            0x00,
            SENSOR_SELECTOR,
            SENSOR_SETTING_TRACKING_DISTANCE,
            level,
        ],
    }
}

pub fn cmd_read_battery() -> RazerCommand {
    RazerCommand {
        command_class: 0x07,
        command_id: 0x80,
        data_size: 0x02,
        args: vec![0x00, 0x00],
    }
}

pub fn cmd_read_charging() -> RazerCommand {
    RazerCommand {
        command_class: 0x07,
        command_id: 0x84,
        data_size: 0x02,
        args: vec![0x00, 0x00],
    }
}

pub fn cmd_read_v4_dpi_stages() -> RazerCommand {
    RazerCommand {
        command_class: 0x04,
        command_id: 0x86,
        data_size: 80,
        args: Vec::new(),
    }
}

pub fn cmd_read_v4_polling() -> RazerCommand {
    RazerCommand {
        command_class: 0x00,
        command_id: 0xC0,
        data_size: 0x02,
        args: vec![0x01],
    }
}

pub fn cmd_read_sleep_timeout() -> RazerCommand {
    RazerCommand {
        command_class: 0x07,
        command_id: 0x83,
        data_size: 0x02,
        args: Vec::new(),
    }
}

pub fn cmd_set_sleep_timeout(seconds: u16) -> RazerCommand {
    RazerCommand {
        command_class: 0x07,
        command_id: 0x03,
        data_size: 0x02,
        args: seconds.to_be_bytes().to_vec(),
    }
}

pub fn decode_razer_sleep_timeout(args: &[u8]) -> Option<u16> {
    if args.len() < 2 {
        return None;
    }
    let secs = u16::from_be_bytes([args[0], args[1]]);
    (secs > 0).then_some(secs)
}

pub fn decode_razer_battery(
    level_args: &[u8],
    charging_args: Option<&[u8]>,
) -> Option<BatteryStatus> {
    let raw = *level_args.get(1)?;
    let percent = ((u16::from(raw) * 100 + 127) / 255).min(100) as u8;
    let charging = charging_args.and_then(|a| a.get(1)).copied() == Some(1);
    Some(BatteryStatus {
        percent,
        charging,
        voltage_mv: None,
    })
}

/// Decodes Razer Viper V4 Pro `0x04/0x86` multi-stage DPI payload -> `(active_stage_0based, stages)`.
pub fn decode_razer_v4_dpi_stages(args: &[u8]) -> Option<(usize, Vec<(u16, u16)>)> {
    let count = usize::from((*args.get(2)?).min(5));
    if count == 0 {
        return None;
    }
    let mut stages = Vec::with_capacity(count);
    for idx in 0..count {
        let off = 3 + idx * 7;
        if off + 5 > args.len() {
            return None;
        }
        let x = u16::from_be_bytes([args[off + 1], args[off + 2]]);
        let y = u16::from_be_bytes([args[off + 3], args[off + 4]]);
        stages.push((x, y));
    }
    let active = usize::from(args.get(1).copied().unwrap_or(1).saturating_sub(1))
        .min(stages.len().saturating_sub(1));
    Some((active, stages))
}

pub fn cmd_set_v4_dpi_stages(active_stage: usize, stages: &[(u16, u16)]) -> RazerCommand {
    let mut args = vec![0u8; 3 + stages.len() * 7];
    args[0] = 1;
    args[1] = (active_stage + 1) as u8;
    args[2] = stages.len() as u8;
    for (idx, &(x, y)) in stages.iter().enumerate() {
        let off = 3 + idx * 7;
        args[off] = idx as u8;
        let xbe = x.to_be_bytes();
        let ybe = y.to_be_bytes();
        args[off + 1] = xbe[0];
        args[off + 2] = xbe[1];
        args[off + 3] = ybe[0];
        args[off + 4] = ybe[1];
    }
    let len = args.len() as u8;
    RazerCommand {
        command_class: 0x04,
        command_id: 0x06,
        data_size: len,
        args,
    }
}

pub fn decode_dpi(args: &[u8]) -> Option<(u16, u16)> {
    if args.len() < 5 {
        return None;
    }
    let x = u16::from_be_bytes([args[1], args[2]]);
    let y = u16::from_be_bytes([args[3], args[4]]);
    Some((x, y))
}

pub fn decode_legacy_polling_rate(args: &[u8]) -> Option<u16> {
    let div = *args.first()?;
    if div == 0 {
        return None;
    }
    Some((1000.0 / f64::from(div)).round() as u16)
}

pub fn decode_extended_polling_rate(args: &[u8]) -> Option<u16> {
    let div = *args.get(1)?;
    if div == 0 {
        return None;
    }
    Some((8000.0 / f64::from(div)).round() as u16)
}

pub fn decode_tracking_distance(args: &[u8]) -> Option<LiftOffDistance> {
    match *args.get(2)? {
        0 => Some(LiftOffDistance::Mm07),
        1 => Some(LiftOffDistance::Mm10),
        2 => Some(LiftOffDistance::Mm20),
        _ => None,
    }
}

fn slugify_razer_id(model: &str, pid: u16) -> String {
    let mut slug = String::from("razer_");
    let mut prev_us = true;
    for ch in model.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_us = false;
        } else if !prev_us {
            slug.push('_');
            prev_us = true;
        }
    }
    let trimmed = slug.trim_end_matches('_');
    format!("{trimmed}_{pid:04x}")
}

/// Native Windows `hidapi` driver for Razer 90-byte feature-report mice.
pub struct RazerDevice {
    hid: HidDevice,
    spec: RazerProductSpec,
    id_slug: String,
    display_name: String,
    vid_pid: String,
    discovered_high_rate: Option<bool>,
    discovered_rates: Option<Vec<u16>>,
}

impl RazerDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.display_name.clone(),
            message: message.into(),
        }
    }

    fn exchange_raw(
        hid: &HidDevice,
        cmd: &RazerCommand,
        tx_id: u8,
        delay_ms: u64,
        max_attempts: usize,
    ) -> std::result::Result<Vec<u8>, String> {
        let req_pkt = encode_razer_request(cmd, tx_id);
        let mut send_buf = [0u8; RAZER_PACKET_LEN + 1];
        send_buf[0] = RAZER_REPORT_ID;
        send_buf[1..].copy_from_slice(&req_pkt);

        for _ in 0..max_attempts {
            hid.send_feature_report(&send_buf).map_err(|e| {
                format!(
                    "Razer feature write 0x{:02x}/0x{:02x} failed (close Razer Synapse): {e}",
                    cmd.command_class, cmd.command_id
                )
            })?;

            for _ in 0..max_attempts {
                thread::sleep(Duration::from_millis(delay_ms));
                let mut recv_buf = [0u8; RAZER_PACKET_LEN + 1];
                recv_buf[0] = RAZER_REPORT_ID;
                let n = hid.get_feature_report(&mut recv_buf).map_err(|e| {
                    format!(
                        "Razer feature read 0x{:02x}/0x{:02x} failed: {e}",
                        cmd.command_class, cmd.command_id
                    )
                })?;
                let payload = if n > RAZER_PACKET_LEN {
                    &recv_buf[1..=RAZER_PACKET_LEN]
                } else if n == RAZER_PACKET_LEN {
                    &recv_buf[..RAZER_PACKET_LEN]
                } else {
                    continue;
                };

                match decode_razer_response(payload, cmd) {
                    Ok(args) => return Ok(args),
                    Err(RazerDecodeError::Busy) => continue,
                    Err(RazerDecodeError::BadChecksum | RazerDecodeError::BadLength(_)) => break,
                    Err(RazerDecodeError::StaleReply) if cmd.is_getter() => break,
                    Err(RazerDecodeError::StaleReply) => {
                        return Err(format!(
                            "Stale reply on Razer write 0x{:02x}/0x{:02x}",
                            cmd.command_class, cmd.command_id
                        ));
                    }
                    Err(RazerDecodeError::Unsupported) => {
                        return Err(format!(
                            "Command 0x{:02x}/0x{:02x} unsupported",
                            cmd.command_class, cmd.command_id
                        ));
                    }
                    Err(RazerDecodeError::Status(s)) => {
                        return Err(format!(
                            "Razer command 0x{:02x}/0x{:02x} returned status 0x{s:02x}",
                            cmd.command_class, cmd.command_id
                        ));
                    }
                }
            }
        }

        Err(format!(
            "Timed out waiting for Razer reply to 0x{:02x}/0x{:02x} (mouse may be asleep or held by Synapse)",
            cmd.command_class, cmd.command_id
        ))
    }

    fn request(&self, cmd: &RazerCommand, tx_override: Option<u8>) -> Result<Vec<u8>> {
        let tx = tx_override.unwrap_or_else(|| self.spec.transaction_id());
        Self::exchange_raw(&self.hid, cmd, tx, RESPONSE_DELAY_MS, RESPONSE_ATTEMPTS)
            .map_err(|msg| self.dev_err(msg))
    }

    fn uses_high_rate_polling(&self) -> bool {
        self.discovered_high_rate
            .unwrap_or(self.spec.high_rate_polling)
    }

    fn read_polling_rate(&mut self) -> Result<u16> {
        let prefer_ext = self.uses_high_rate_polling();
        let order = if prefer_ext {
            [true, false]
        } else {
            [false, true]
        };

        for is_ext in order {
            let reply = if is_ext {
                self.request(&cmd_read_extended_polling(), None)
                    .ok()
                    .and_then(|a| decode_extended_polling_rate(&a))
            } else {
                self.request(&cmd_read_legacy_polling(), None)
                    .ok()
                    .and_then(|a| decode_legacy_polling_rate(&a))
            };
            if let Some(hz) = reply {
                if self.spec.probe_polling_rates {
                    self.discovered_high_rate = Some(is_ext);
                    self.discovered_rates = Some(if is_ext { RATES_8K } else { RATES_1K }.to_vec());
                }
                return Ok(hz);
            }
        }
        Err(self.dev_err("Mouse did not report a polling rate"))
    }
}

impl MouseDevice for RazerDevice {
    fn info(&self) -> DeviceInfo {
        let connection = self
            .spec
            .connection_label
            .map(str::to_string)
            .unwrap_or_else(|| {
                if self.spec.wireless {
                    "HyperSpeed Wireless".to_string()
                } else {
                    "Wired USB".to_string()
                }
            });
        DeviceInfo {
            id: self.id_slug.clone(),
            name: self.display_name.clone(),
            vendor: "Razer".to_string(),
            vid_pid: Some(self.vid_pid.clone()),
            connection,
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let polling_rates_hz = self
            .discovered_rates
            .clone()
            .unwrap_or_else(|| self.spec.polling_rates.to_vec());
        let lod_options = if self.spec.lift_off {
            LiftOffDistance::ALL.to_vec()
        } else {
            Vec::new()
        };
        let volatile = self.spec.dpi_storage_byte == RAZER_STORAGE_NOSTORE;
        DeviceCapabilities {
            dpi_min: DPI_MIN,
            dpi_max: self.spec.max_dpi,
            dpi_step: DPI_STEP,
            supports_xy_dpi: true,
            polling_rates_hz,
            lod_options,
            supports_debounce: false,
            debounce_min_ms: 0,
            debounce_max_ms: 0,
            supports_motion_sync: false,
            supports_angle_snapping: false,
            supports_ripple_control: false,
            supports_sleep_timeout: self.spec.wireless,
            sleep_timeout_min_secs: if self.spec.wireless { 60 } else { 0 },
            sleep_timeout_max_secs: if self.spec.wireless { 900 } else { 0 },
            supports_volatile_writes: volatile,
            flash_write_warning: (!volatile).then(|| {
                format!(
                    "{} writes to Razer's persistent VARSTORE (0x01). Polled diffs unchanged values and rate-limits rapid bursts.",
                    self.display_name
                )
            }),
        }
    }

    fn read_battery(&mut self) -> Option<BatteryStatus> {
        if !self.spec.wireless {
            return None;
        }
        let level = self.request(&cmd_read_battery(), None).ok()?;
        let charging = self.request(&cmd_read_charging(), None).ok();
        decode_razer_battery(&level, charging.as_deref())
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        let sleep_timeout_secs = if self.spec.wireless {
            self.request(&cmd_read_sleep_timeout(), None)
                .ok()
                .and_then(|a| decode_razer_sleep_timeout(&a))
        } else {
            None
        };

        if self.spec.v4_dpi_protocol {
            let dpi_reply = self.request(&cmd_read_v4_dpi_stages(), None)?;
            let (active, stages) = decode_razer_v4_dpi_stages(&dpi_reply)
                .ok_or_else(|| self.dev_err("Could not decode Razer V4 DPI stages"))?;
            let poll_reply = self.request(&cmd_read_v4_polling(), None)?;
            let polling_rate_hz = decode_extended_polling_rate(&poll_reply)
                .ok_or_else(|| self.dev_err("Could not decode Razer V4 polling rate"))?;
            let (dpi_x, dpi_y) = stages[active];
            return Ok(MouseSettings {
                dpi: Some(dpi_x),
                dpi_y: (dpi_y != dpi_x).then_some(dpi_y),
                polling_rate_hz: Some(polling_rate_hz),
                sleep_timeout_secs,
                ..Default::default()
            });
        }

        let dpi_reply = self.request(&cmd_read_dpi(self.spec.dpi_storage_byte), None)?;
        let (dpi_x, dpi_y) = decode_dpi(&dpi_reply)
            .ok_or_else(|| self.dev_err("Could not decode Razer DPI reply"))?;
        let polling_rate_hz = self.read_polling_rate()?;
        let lod = if self.spec.lift_off {
            self.request(&cmd_read_lift_off(), None)
                .ok()
                .and_then(|a| decode_tracking_distance(&a))
        } else {
            None
        };

        Ok(MouseSettings {
            dpi: Some(dpi_x),
            dpi_y: (dpi_y != dpi_x).then_some(dpi_y),
            polling_rate_hz: Some(polling_rate_hz),
            lod,
            sleep_timeout_secs,
            ..Default::default()
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        let current = self.read_settings()?;

        if self.spec.v4_dpi_protocol {
            if settings.dpi.is_some() || settings.dpi_y.is_some() {
                let cur_x = current.dpi.unwrap_or(800);
                let cur_y = current.dpi_y.unwrap_or(cur_x);
                let target_x = settings.dpi.unwrap_or(cur_x);
                let target_y = settings.dpi_y.or(settings.dpi).unwrap_or(cur_y);
                if target_x != cur_x || target_y != cur_y {
                    let dpi_reply = self.request(&cmd_read_v4_dpi_stages(), None)?;
                    let (active, mut stages) = decode_razer_v4_dpi_stages(&dpi_reply)
                        .ok_or_else(|| self.dev_err("Could not decode Razer V4 DPI stages"))?;
                    stages[active] = (target_x, target_y);
                    self.request(&cmd_set_v4_dpi_stages(active, &stages), None)?;
                }
            }
            if let Some(target_hz) = settings.polling_rate_hz {
                if current.polling_rate_hz != Some(target_hz) {
                    let cmd = cmd_set_extended_polling(0x01, target_hz).ok_or_else(|| {
                        PolledError::UnsupportedSetting {
                            device: self.display_name.clone(),
                            detail: format!("Unsupported Razer V4 polling rate {target_hz} Hz"),
                        }
                    })?;
                    self.request(&cmd, None)?;
                    if self.spec.wireless {
                        thread::sleep(Duration::from_millis(POLLING_LINK_SETTLE_MS));
                    }
                }
            }
            if let Some(target_sleep) = settings.sleep_timeout_secs {
                if self.spec.wireless && current.sleep_timeout_secs != Some(target_sleep) {
                    self.request(&cmd_set_sleep_timeout(target_sleep), None)?;
                }
            }
            return Ok(());
        }

        if settings.dpi.is_some() || settings.dpi_y.is_some() {
            let cur_x = current.dpi.unwrap_or(800);
            let cur_y = current.dpi_y.unwrap_or(cur_x);
            let target_x = settings.dpi.unwrap_or(cur_x);
            let target_y = settings.dpi_y.or(settings.dpi).unwrap_or(cur_y);
            if target_x != cur_x || target_y != cur_y {
                let cmd = cmd_set_dpi(target_x, target_y, self.spec.dpi_storage_byte);
                self.request(&cmd, None)?;
            }
        }

        if let Some(target_hz) = settings.polling_rate_hz {
            if current.polling_rate_hz != Some(target_hz) {
                let high_rate = self.uses_high_rate_polling();
                let cmd = if high_rate {
                    cmd_set_extended_polling(0x00, target_hz)
                } else {
                    cmd_set_legacy_polling(target_hz)
                }
                .ok_or_else(|| PolledError::UnsupportedSetting {
                    device: self.display_name.clone(),
                    detail: format!("Unsupported Razer polling rate {target_hz} Hz"),
                })?;
                self.request(&cmd, None)?;

                if high_rate {
                    if let Some(commit_tx) = self.spec.extended_polling_commit_tx {
                        if let Some(commit_cmd) = cmd_set_extended_polling(0x01, target_hz) {
                            self.request(&commit_cmd, Some(commit_tx))?;
                        }
                    }
                }
                if self.spec.wireless {
                    thread::sleep(Duration::from_millis(POLLING_LINK_SETTLE_MS));
                }
            }
        }

        if let Some(target_lod) = settings.lod {
            if self.spec.lift_off && current.lod != Some(target_lod) {
                self.request(&cmd_set_tracking_distance(target_lod), None)?;
            }
        }

        if let Some(target_sleep) = settings.sleep_timeout_secs {
            if self.spec.wireless && current.sleep_timeout_secs != Some(target_sleep) {
                self.request(&cmd_set_sleep_timeout(target_sleep), None)?;
            }
        }

        Ok(())
    }
}

/// Protocol-family auto-detector for Razer 90-byte feature-report mice.
pub struct RazerProtocolDriver;

impl DeviceDriver for RazerProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "Razer 90B Feature Report (0x1532)"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();
        let mut claimed_keys: HashSet<(u16, String)> = HashSet::new();

        // Collect candidates and prioritize Generic Desktop Mouse (0x01:0x02) over vendor collections.
        let mut candidates: Vec<_> = api
            .device_list()
            .filter(|d| d.vendor_id() == RAZER_VID)
            .filter_map(|d| {
                let spec = lookup_razer_product(d.product_id())?;
                let is_mouse_col = d.usage_page() == 0x01 && d.usage() == 0x02;
                let is_vendor_col = d.usage_page() >= 0xFF00;
                if !is_mouse_col && !(spec.vendor_control_interface && is_vendor_col) {
                    return None;
                }
                let priority = if is_mouse_col { 0u8 } else { 1u8 };
                Some((priority, spec, d))
            })
            .collect();
        candidates.sort_by_key(|(prio, spec, _)| (*prio, spec.pid));

        let fw_cmd = cmd_read_firmware();
        for (_, spec, dev_info) in candidates {
            let dedup_key = (
                spec.pid,
                dev_info.serial_number().unwrap_or_default().to_string(),
            );
            if claimed_keys.contains(&dedup_key) {
                continue;
            }
            let Ok(hid) = dev_info.open_device(api) else {
                continue;
            };
            // Verify this exact HID interface answers Razer's read-only firmware query.
            if RazerDevice::exchange_raw(&hid, &fw_cmd, spec.transaction_id(), PROBE_DELAY_MS, 3)
                .is_err()
            {
                continue;
            }

            claimed_keys.insert(dedup_key);
            let display_name = format!("Razer {}", spec.model);
            let vid_pid = format!("{:04x}:{:04x}", RAZER_VID, spec.pid);
            let id_slug = slugify_razer_id(spec.model, spec.pid);
            found.push(Box::new(RazerDevice {
                hid,
                spec,
                id_slug,
                display_name,
                vid_pid,
                discovered_high_rate: None,
                discovered_rates: None,
            }));
        }

        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_razer_90b_request_with_exact_xor_checksum() {
        let cmd = cmd_set_dpi(1600, 1600, RAZER_STORAGE_VARSTORE);
        let pkt = encode_razer_request(&cmd, RAZER_TX_1F);
        assert_eq!(pkt.len(), RAZER_PACKET_LEN);
        assert_eq!(pkt[1], 0x1F);
        assert_eq!(pkt[5], 0x07);
        assert_eq!(pkt[6], 0x04);
        assert_eq!(pkt[7], 0x05);
        assert_eq!(&pkt[8..13], &[0x01, 0x06, 0x40, 0x06, 0x40]);
        assert_eq!(pkt[88], razer_checksum(&pkt));

        // Simulate device OK reply for read DPI.
        let read_cmd = cmd_read_dpi(RAZER_STORAGE_VARSTORE);
        let mut reply = encode_razer_request(&read_cmd, RAZER_TX_1F);
        reply[0] = STATUS_OK;
        reply[9] = 0x06;
        reply[10] = 0x40;
        reply[11] = 0x06;
        reply[12] = 0x40;
        reply[88] = razer_checksum(&reply);

        let args = decode_razer_response(&reply, &read_cmd).unwrap();
        assert_eq!(decode_dpi(&args), Some((1600, 1600)));
    }

    #[test]
    fn encodes_and_decodes_legacy_and_extended_polling_rates() {
        for hz in [125u16, 500, 1000] {
            let cmd = cmd_set_legacy_polling(hz).unwrap();
            assert_eq!(decode_legacy_polling_rate(&cmd.args), Some(hz));
        }
        for hz in [125u16, 500, 1000, 2000, 4000, 8000] {
            let cmd = cmd_set_extended_polling(0x00, hz).unwrap();
            assert_eq!(decode_extended_polling_rate(&cmd.args), Some(hz));
        }
        assert!(cmd_set_legacy_polling(4000).is_none());
    }

    #[test]
    fn resolves_audited_per_pid_transaction_ids_and_specs() {
        let v3 = lookup_razer_product(0x00C1).unwrap();
        assert_eq!(v3.model, "Viper V3 Pro");
        assert_eq!(v3.transaction_id(), RAZER_TX_1F);
        assert_eq!(v3.max_dpi, 35_000);
        assert_eq!(v3.extended_polling_commit_tx, Some(RAZER_TX_1F));
        assert!(v3.lift_off);

        let da_v4 = lookup_razer_product(0x00BF).unwrap();
        assert_eq!(da_v4.max_dpi, 45_000);
        assert_eq!(da_v4.transaction_id(), RAZER_TX_1F);

        let da_v2 = lookup_razer_product(0x0084).unwrap();
        assert_eq!(da_v2.transaction_id(), RAZER_TX_3F);
        assert_eq!(da_v2.dpi_storage_byte, RAZER_STORAGE_NOSTORE);

        let viper_8k = lookup_razer_product(0x0091).unwrap();
        assert_eq!(viper_8k.transaction_id(), RAZER_TX_FF);
        assert!(viper_8k.high_rate_polling);
    }

    #[test]
    fn detects_bad_checksum_busy_and_stale_replies() {
        let cmd = cmd_read_firmware();
        let mut reply = encode_razer_request(&cmd, RAZER_TX_1F);
        reply[0] = STATUS_BUSY;
        reply[88] = razer_checksum(&reply);
        assert_eq!(
            decode_razer_response(&reply, &cmd),
            Err(RazerDecodeError::Busy)
        );

        reply[0] = STATUS_OK;
        reply[88] ^= 0xFF;
        assert_eq!(
            decode_razer_response(&reply, &cmd),
            Err(RazerDecodeError::BadChecksum)
        );

        reply[7] = 0x85; // wrong command id
        reply[88] = razer_checksum(&reply);
        assert_eq!(
            decode_razer_response(&reply, &cmd),
            Err(RazerDecodeError::StaleReply)
        );
    }

    #[test]
    fn decodes_razer_battery_and_viper_v4_pro_stages() {
        let v4 = lookup_razer_product(0x00E6).unwrap();
        assert_eq!(v4.model, "Viper V4 Pro");
        assert_eq!(v4.max_dpi, 50_000);
        assert!(v4.v4_dpi_protocol);
        assert_eq!(v4.transaction_id(), RAZER_TX_1F);

        let cmd = cmd_set_v4_dpi_stages(1, &[(800, 800), (1600, 1600), (3200, 3200)]);
        let (active, stages) = decode_razer_v4_dpi_stages(&cmd.args).unwrap();
        assert_eq!(active, 1);
        assert_eq!(stages, vec![(800, 800), (1600, 1600), (3200, 3200)]);

        let batt = decode_razer_battery(&[0x00, 204], Some(&[0x00, 1])).unwrap();
        assert_eq!(batt.percent, 80);
        assert!(batt.charging);
    }
}
