// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! ASUS ROG / TUF 64-byte HID mouse & ROG Omni Receiver (`0x0B05`) protocol backend.
//!
//! Covers ASUS ROG and TUF gaming mice speaking the unified `12 xx` (read) / `51 31` (write) /
//! `50 03` (commit) protocol over:
//! - Direct USB wired and dedicated 2.4 GHz wireless dongles (`usage_page 0xFF01`, `Report ID 0x00`,
//!   or `Report ID 0x03` on the Harpe II Ace 8K receiver `0x1AD0`).
//! - The **ROG Omni Receiver** (`0x0B05:0x1ACE`), querying the `0xFF02` (`Report ID 0x01`)
//!   receiver control interface (`A0 00`) to identify the paired mouse, routing commands over the
//!   paired mouse collection (`0xFF01`, typically `Report ID 0x03`), and probing the optional
//!   2K/4K/8K **ROG Polling Rate Booster** (`7D 20 02`).
//!
//! # Protocol Attribution & Licenses (AGPL-3.0 compatible)
//! Packet framing, command/register IDs, Omni pair-list/booster handshake, and per-model PID
//! capability tables are ported from:
//! - `OpenMouse-Project/mouse-protocol` (<https://github.com/OpenMouse-Project/mouse-protocol>)
//!   - `src/asus/index.ts`, `src/asus/devices.ts`, `src/asus/omni.ts`, `src/drivers/asus/hid.ts`
//!   - **AGPL-3.0-or-later**, Copyright (C) OpenMouse Project Contributors
//! - `seerge/g-helper` (<https://github.com/seerge/g-helper>)
//!   - `app/Peripherals/Mouse/AsusMouse.cs`, `app/Peripherals/PeripheralsProvider.cs`,
//!     `app/Peripherals/Mouse/Models/*.cs`
//!   - **GPL-3.0-or-later**, Copyright (C) Serge
//! - `libratbag` (<https://github.com/libratbag/libratbag>)
//!   - `src/asus.c`, `src/driver-asus.c` (**MIT License**, Copyright (C) 2021 Kyoken)
//!
//! # Safety Invariants
//! - Only verified mouse PIDs are matched; ASUS keyboards (Azoth, Scope II, Falchion), headsets,
//!   mousepads (Balteus), SSD enclosures (Arion), and docks are strictly ignored.
//! - Destructive/administrative receiver commands (`0xA2` pair/unpair, `0xB0` reboot/reset,
//!   firmware flashing) are **never** encoded.
//! - Marked [`SupportTier::ImplementedFromDocs`] with `supports_volatile_writes: false` so
//!   [`crate::devices::SafeDeviceController`] prompts on first write, diffs registers, rate-limits
//!   onboard flash writes, and verifies every write by read-back.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    BatteryStatus, DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice,
    MouseSettings, SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};
use crate::hid::parse_hid_report_descriptor;

pub const ASUS_VID: u16 = 0x0B05;
pub const ASUS_OMNI_PID: u16 = 0x1ACE;
pub const ASUS_HARPE_II_WIRELESS_PID: u16 = 0x1AD0;

pub const ASUS_MOUSE_USAGE_PAGE: u16 = 0xFF01;
pub const ASUS_OMNI_USAGE_PAGE: u16 = 0xFF02;
pub const ASUS_USAGE: u16 = 0x0001;

pub const ASUS_DIRECT_REPORT_ID: u8 = 0x00;
pub const ASUS_OMNI_CTRL_REPORT_ID: u8 = 0x01;
pub const ASUS_OMNI_MOUSE_REPORT_ID: u8 = 0x03;

pub const ASUS_DIRECT_PAYLOAD_LEN: usize = 64;
pub const ASUS_OMNI_PAYLOAD_LEN: usize = 63;

const QUERY_TIMEOUT_MS: u64 = 700;

const POLLING_RATES_1K: &[u16] = &[125, 250, 500, 1000];
const POLLING_RATES_250_1K: &[u16] = &[250, 500, 1000];
const POLLING_RATES_8K: &[u16] = &[125, 250, 500, 1000, 2000, 4000, 8000];
const POLLING_RATES_250_8K: &[u16] = &[250, 500, 1000, 2000, 4000, 8000];

/// Wire index -> polling rate Hz (`ASUS_POLLING_RATES` / G-Helper `PollingRate` enum).
pub const ASUS_WIRE_POLLING_RATES: [u16; 7] = [125, 250, 500, 1000, 2000, 4000, 8000];

/// Valid ASUS debounce steps in ms (`0x02..=0x07` -> `raw * 4 + 4`).
pub const ASUS_DEBOUNCE_STEPS_MS: [u8; 6] = [12, 16, 20, 24, 28, 32];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsusLodKind {
    None,
    /// Standard `12 06` (`data[7]`) / `51 35 FF 00 FF <0|1>`.
    Standard,
    /// Older Keris P513 `12 06` (`data[4]`) / `51 35 00 00 <0|1>`.
    LegacySlot0,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsusMouseSpec {
    pub pid: u16,
    pub name: &'static str,
    pub wireless: bool,
    pub dpi_stages: u8,
    pub min_dpi: u16,
    pub max_dpi: u16,
    pub dpi_step: u16,
    /// Reads DPI from `12 04 02` (4 bytes X,Y per stage) instead of `12 04 00`.
    pub dpi_xy: bool,
    /// `51 31 <stage>` must preserve the stage's RGB indicator color read from `12 04 03`.
    pub dpi_colors: bool,
    pub default_polling_rates: &'static [u16],
    pub supports_booster: bool,
    pub debounce: bool,
    pub lod: AsusLodKind,
    pub angle_snapping: bool,
    pub motion_sync: bool,
    pub battery: bool,
    /// Legacy Chakram / Keris Wireless report battery in 25% steps (`0..=4`).
    pub battery_quarter_steps: bool,
    pub auto_power_off: bool,
    pub report_id: u8,
}

#[allow(clippy::too_many_arguments)]
const fn aimpoint_spec(
    pid: u16,
    name: &'static str,
    wireless: bool,
    min_dpi: u16,
    max_dpi: u16,
    debounce: bool,
    supports_booster: bool,
) -> AsusMouseSpec {
    AsusMouseSpec {
        pid,
        name,
        wireless,
        dpi_stages: 4,
        min_dpi,
        max_dpi,
        dpi_step: 50,
        dpi_xy: true,
        dpi_colors: true,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster,
        debounce,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    }
}

/// Verified direct-USB and dedicated-dongle ASUS ROG / TUF mouse catalog (`VID 0x0B05`).
pub const ASUS_DIRECT_MODELS: &[AsusMouseSpec] = &[
    // ROG Harpe II Ace (native 8K + Motion Sync)
    AsusMouseSpec {
        pid: 0x1C69,
        name: "ROG Harpe II Ace (Wired)",
        wireless: false,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 42_000,
        dpi_step: 50,
        dpi_xy: true,
        dpi_colors: true,
        default_polling_rates: POLLING_RATES_8K,
        supports_booster: false,
        debounce: false,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: true,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: ASUS_HARPE_II_WIRELESS_PID,
        name: "ROG Harpe II Ace (Wireless)",
        wireless: true,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 42_000,
        dpi_step: 50,
        dpi_xy: true,
        dpi_colors: true,
        default_polling_rates: POLLING_RATES_8K,
        supports_booster: false,
        debounce: false,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: true,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_OMNI_MOUSE_REPORT_ID,
    },
    // ROG Harpe Ace Aim Lab / Extreme / Mini / Mini Core
    aimpoint_spec(
        0x1A92,
        "ROG Harpe Ace Aim Lab Edition (Wired)",
        false,
        50,
        36_000,
        true,
        true,
    ),
    aimpoint_spec(
        0x1A94,
        "ROG Harpe Ace Aim Lab Edition (Wireless)",
        true,
        50,
        36_000,
        true,
        true,
    ),
    aimpoint_spec(
        0x1B67,
        "ROG Harpe Ace Extreme (Wired)",
        false,
        50,
        42_000,
        true,
        true,
    ),
    aimpoint_spec(
        0x1B63,
        "ROG Harpe Ace Mini (Wired)",
        false,
        100,
        42_000,
        false,
        true,
    ),
    AsusMouseSpec {
        pid: 0x1C8F,
        name: "ROG Harpe Mini Core",
        wireless: false,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 12_000,
        dpi_step: 50,
        dpi_xy: false,
        dpi_colors: true,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: false,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: false,
        battery: false,
        battery_quarter_steps: false,
        auto_power_off: false,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    // ROG Keris II Ace / Origin / Wireless AimPoint
    aimpoint_spec(
        0x1B16,
        "ROG Keris II Ace (Wired)",
        false,
        100,
        42_000,
        false,
        true,
    ),
    aimpoint_spec(
        0x1C0C,
        "ROG Keris II Origin (Wired)",
        false,
        100,
        42_000,
        false,
        true,
    ),
    aimpoint_spec(
        0x1D4C,
        "ROG Keris II Origin KJP (Wired)",
        false,
        100,
        42_000,
        false,
        true,
    ),
    aimpoint_spec(
        0x1A66,
        "ROG Keris Wireless AimPoint (Wired)",
        false,
        100,
        36_000,
        true,
        true,
    ),
    aimpoint_spec(
        0x1A68,
        "ROG Keris Wireless AimPoint (Wireless)",
        true,
        100,
        36_000,
        true,
        true,
    ),
    // ROG Gladius III Wireless AimPoint / EVA-02
    aimpoint_spec(
        0x1A70,
        "ROG Gladius III AimPoint (Wired)",
        false,
        100,
        36_000,
        true,
        true,
    ),
    aimpoint_spec(
        0x1A72,
        "ROG Gladius III AimPoint (Wireless)",
        true,
        100,
        36_000,
        true,
        true,
    ),
    aimpoint_spec(
        0x1B0A,
        "ROG Gladius III AimPoint EVA-02 (Wired)",
        false,
        100,
        36_000,
        true,
        true,
    ),
    aimpoint_spec(
        0x1B0C,
        "ROG Gladius III AimPoint EVA-02 (Wireless)",
        true,
        100,
        36_000,
        true,
        true,
    ),
    // ROG Gladius III / III Wireless
    AsusMouseSpec {
        pid: 0x197B,
        name: "ROG Gladius III",
        wireless: false,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 26_000,
        dpi_step: 50,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: false,
        battery: false,
        battery_quarter_steps: false,
        auto_power_off: false,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x197D,
        name: "ROG Gladius III (Wired)",
        wireless: false,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 26_000,
        dpi_step: 50,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x197F,
        name: "ROG Gladius III (Wireless)",
        wireless: true,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 26_000,
        dpi_step: 50,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    // ROG Keris / Keris Wireless / EVA
    AsusMouseSpec {
        pid: 0x195C,
        name: "ROG Keris",
        wireless: false,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 16_000,
        dpi_step: 100,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::LegacySlot0,
        angle_snapping: true,
        motion_sync: false,
        battery: false,
        battery_quarter_steps: false,
        auto_power_off: false,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x195E,
        name: "ROG Keris Wireless (Wired)",
        wireless: false,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 16_000,
        dpi_step: 100,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::LegacySlot0,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: true,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x1960,
        name: "ROG Keris Wireless",
        wireless: true,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 16_000,
        dpi_step: 100,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::LegacySlot0,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: true,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x1A57,
        name: "ROG Keris EVA Edition (Wired)",
        wireless: false,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 16_000,
        dpi_step: 100,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::LegacySlot0,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: true,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x1A59,
        name: "ROG Keris EVA Edition (Wireless)",
        wireless: true,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 16_000,
        dpi_step: 100,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::LegacySlot0,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: true,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    // ROG Chakram X (Wired 8K / Wireless 1K)
    AsusMouseSpec {
        pid: 0x1A18,
        name: "ROG Chakram X (Wired)",
        wireless: false,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 36_000,
        dpi_step: 50,
        dpi_xy: true,
        dpi_colors: true,
        default_polling_rates: POLLING_RATES_250_8K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x1A1A,
        name: "ROG Chakram X (Wireless)",
        wireless: true,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 36_000,
        dpi_step: 50,
        dpi_xy: true,
        dpi_colors: true,
        default_polling_rates: POLLING_RATES_250_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    // ROG Spatha X
    AsusMouseSpec {
        pid: 0x1977,
        name: "ROG Spatha X (Wired)",
        wireless: false,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 19_000,
        dpi_step: 50,
        dpi_xy: false,
        dpi_colors: true,
        default_polling_rates: POLLING_RATES_250_1K,
        supports_booster: false,
        debounce: false,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x1979,
        name: "ROG Spatha X (Wireless)",
        wireless: true,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 19_000,
        dpi_step: 50,
        dpi_xy: false,
        dpi_colors: true,
        default_polling_rates: POLLING_RATES_250_1K,
        supports_booster: false,
        debounce: false,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    // ROG Strix Impact III (Wired)
    AsusMouseSpec {
        pid: 0x1A88,
        name: "ROG Strix Impact III",
        wireless: false,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 12_000,
        dpi_step: 50,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: false,
        lod: AsusLodKind::None,
        angle_snapping: true,
        motion_sync: false,
        battery: false,
        battery_quarter_steps: false,
        auto_power_off: false,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    // ROG Gladius II family (2 DPI stages)
    AsusMouseSpec {
        pid: 0x1845,
        name: "ROG Gladius II",
        wireless: false,
        dpi_stages: 2,
        min_dpi: 100,
        max_dpi: 12_000,
        dpi_step: 100,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: false,
        battery: false,
        battery_quarter_steps: false,
        auto_power_off: false,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x1877,
        name: "ROG Gladius II Origin",
        wireless: false,
        dpi_stages: 2,
        min_dpi: 100,
        max_dpi: 12_000,
        dpi_step: 100,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: false,
        battery: false,
        battery_quarter_steps: false,
        auto_power_off: false,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x18CD,
        name: "ROG Gladius II Origin PNK LTD",
        wireless: false,
        dpi_stages: 2,
        min_dpi: 100,
        max_dpi: 12_000,
        dpi_step: 100,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: false,
        battery: false,
        battery_quarter_steps: false,
        auto_power_off: false,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x18B1,
        name: "ROG Gladius II Origin COD",
        wireless: false,
        dpi_stages: 2,
        min_dpi: 100,
        max_dpi: 12_000,
        dpi_step: 100,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: false,
        battery: false,
        battery_quarter_steps: false,
        auto_power_off: false,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    // TUF Gaming mice
    AsusMouseSpec {
        pid: 0x1898,
        name: "TUF Gaming M5",
        wireless: false,
        dpi_stages: 2,
        min_dpi: 100,
        max_dpi: 6_200,
        dpi_step: 100,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::None,
        angle_snapping: true,
        motion_sync: false,
        battery: false,
        battery_quarter_steps: false,
        auto_power_off: false,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x1910,
        name: "TUF Gaming M3",
        wireless: false,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 7_000,
        dpi_step: 100,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::None,
        angle_snapping: true,
        motion_sync: false,
        battery: false,
        battery_quarter_steps: false,
        auto_power_off: false,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x1A03,
        name: "TUF Gaming M4 Air",
        wireless: false,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 16_000,
        dpi_step: 100,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: false,
        lod: AsusLodKind::Standard,
        angle_snapping: true,
        motion_sync: false,
        battery: false,
        battery_quarter_steps: false,
        auto_power_off: false,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x19F4,
        name: "TUF Gaming M4 Wireless",
        wireless: true,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 12_000,
        dpi_step: 100,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::None,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x1A8D,
        name: "TX Gaming Mouse Wireless",
        wireless: true,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 12_000,
        dpi_step: 100,
        dpi_xy: false,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::None,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x1AF3,
        name: "TX Gaming Mouse Mini (Wired)",
        wireless: false,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 12_000,
        dpi_step: 50,
        dpi_xy: true,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::None,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x1AF5,
        name: "TX Gaming Mouse Mini (Wireless)",
        wireless: true,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 12_000,
        dpi_step: 50,
        dpi_xy: true,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::None,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x1C56,
        name: "TUF Gaming Mini Miku Edition (Wired)",
        wireless: false,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 12_000,
        dpi_step: 50,
        dpi_xy: true,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::None,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
    AsusMouseSpec {
        pid: 0x1C57,
        name: "TUF Gaming Mini Miku Edition (Wireless)",
        wireless: true,
        dpi_stages: 4,
        min_dpi: 100,
        max_dpi: 12_000,
        dpi_step: 50,
        dpi_xy: true,
        dpi_colors: false,
        default_polling_rates: POLLING_RATES_1K,
        supports_booster: false,
        debounce: true,
        lod: AsusLodKind::None,
        angle_snapping: true,
        motion_sync: false,
        battery: true,
        battery_quarter_steps: false,
        auto_power_off: true,
        report_id: ASUS_DIRECT_REPORT_ID,
    },
];

/// Resolves a direct-USB or dedicated-dongle ASUS mouse spec by PID.
pub fn lookup_asus_direct_model(pid: u16) -> Option<AsusMouseSpec> {
    ASUS_DIRECT_MODELS.iter().copied().find(|m| m.pid == pid)
}

/// Resolves a mouse paired to a **ROG Omni Receiver** (`0x0B05:0x1ACE`) from the pair-list PID
/// reported by `A0 00` (table cross-checked against G-Helper `PeripheralsProvider.MouseFromOmniPid`).
pub fn lookup_asus_omni_mouse(paired_pid: u16) -> Option<AsusMouseSpec> {
    let mut spec = match paired_pid {
        0x1A94 => aimpoint_spec(
            paired_pid,
            "ROG Harpe Ace Aim Lab Edition (Omni)",
            true,
            50,
            36_000,
            true,
            true,
        ),
        0x1B68 | 0x1B69 => aimpoint_spec(
            paired_pid,
            "ROG Harpe Ace Extreme (Omni)",
            true,
            50,
            42_000,
            true,
            true,
        ),
        0x1B65 => aimpoint_spec(
            paired_pid,
            "ROG Harpe Ace Mini (Omni)",
            true,
            100,
            42_000,
            false,
            true,
        ),
        0x1B18 | 0x1B1A => aimpoint_spec(
            paired_pid,
            "ROG Keris II Ace (Omni)",
            true,
            100,
            42_000,
            false,
            true,
        ),
        0x1C0E => aimpoint_spec(
            paired_pid,
            "ROG Keris II Origin (Omni)",
            true,
            100,
            42_000,
            false,
            true,
        ),
        0x1D4E => aimpoint_spec(
            paired_pid,
            "ROG Keris II Origin KJP (Omni)",
            true,
            100,
            42_000,
            false,
            true,
        ),
        0x1A68 | 0x1A6A => aimpoint_spec(
            paired_pid,
            "ROG Keris Wireless AimPoint (Omni)",
            true,
            100,
            36_000,
            true,
            true,
        ),
        0x1A72 => aimpoint_spec(
            paired_pid,
            "ROG Gladius III AimPoint (Omni)",
            true,
            100,
            36_000,
            true,
            true,
        ),
        0x1AD7 => aimpoint_spec(
            paired_pid,
            "ROG Strix Impact III Wireless (Omni)",
            true,
            100,
            36_000,
            false,
            false,
        ),
        _ => return None,
    };
    spec.report_id = ASUS_OMNI_MOUSE_REPORT_ID;
    Some(spec)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsusOmniSlot {
    pub product_id: u16,
    pub report_id: u8,
    pub notify_report_id: u8,
}

/// Decodes a ROG Omni receiver pair-list reply (`A0 00`, payload without leading Report ID byte).
pub fn decode_omni_paired_devices(data: &[u8]) -> Option<Vec<AsusOmniSlot>> {
    if data.len() < 4 || data[0] != 0xA0 {
        return None;
    }
    let count = usize::from(u16::from_le_bytes([data[2], data[3]]));
    let max_slots = data.len().saturating_sub(4) / 4;
    let mut slots = Vec::new();
    for i in 0..count.min(max_slots) {
        let off = 4 + i * 4;
        let pid = u16::from_le_bytes([data[off], data[off + 1]]);
        if pid == 0 {
            break;
        }
        slots.push(AsusOmniSlot {
            product_id: pid,
            report_id: data[off + 2],
            notify_report_id: data[off + 3],
        });
    }
    Some(slots)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedAsusSettings {
    pub dpi_stages_x: Vec<u16>,
    pub polling_rate_hz: u16,
    pub debounce_ms: Option<u8>,
    pub angle_snapping: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedAsusBattery {
    pub battery: BatteryStatus,
    pub sleep_timeout_secs: Option<u16>,
    pub low_battery_warn_raw: u8,
}

/// Field index inside `51 31 <field> 00 <val>` (`0 = rate`, `1 = debounce`, `2 = angle snapping`).
#[inline]
fn setting_field(spec: &AsusMouseSpec, field: u8) -> u8 {
    spec.dpi_stages + field
}

/// Byte offset inside the `12 04 00` reply payload (0-indexed after stripping Report ID).
#[inline]
fn setting_offset(spec: &AsusMouseSpec, field: usize) -> usize {
    4 + usize::from(spec.dpi_stages) * 2 + field * 2
}

/// Decodes an ASUS `12 04 00` settings block (payload without leading Report ID byte).
pub fn decode_asus_settings(
    spec: &AsusMouseSpec,
    data: &[u8],
    booster: bool,
) -> Option<DecodedAsusSettings> {
    let snap_off = setting_offset(spec, 2);
    if data.len() <= snap_off || data[0] != 0x12 || data[1] != 0x04 || data[2] != 0x00 {
        return None;
    }

    let mut dpi_stages_x = Vec::with_capacity(usize::from(spec.dpi_stages));
    for i in 0..usize::from(spec.dpi_stages) {
        let off = 4 + i * 2;
        let raw = u16::from_le_bytes([data[off], data[off + 1]]);
        let dpi = raw
            .saturating_add(1)
            .saturating_mul(spec.dpi_step)
            .clamp(spec.min_dpi, spec.max_dpi);
        dpi_stages_x.push(dpi);
    }

    let rate_raw = data[setting_offset(spec, 0)];
    let high_nibble = (rate_raw >> 4) & 0x0F;
    let rate_idx = if booster && high_nibble > 0 {
        usize::from(high_nibble)
    } else {
        usize::from(rate_raw & 0x07)
    };
    let polling_rate_hz = *ASUS_WIRE_POLLING_RATES.get(rate_idx)?;

    let debounce_raw = data[setting_offset(spec, 1)];
    let debounce_ms = if spec.debounce {
        match debounce_raw {
            0x02..=0x07 => Some(debounce_raw * 4 + 4),
            0x01 => Some(8),
            0x00 => Some(0),
            _ => None,
        }
    } else {
        None
    };

    let angle_snapping = data[snap_off] == 0x01;

    Some(DecodedAsusSettings {
        dpi_stages_x,
        polling_rate_hz,
        debounce_ms,
        angle_snapping,
    })
}

/// Decodes an ASUS `12 04 02` X/Y DPI block (`4` bytes `[x_lo, x_hi, y_lo, y_hi]` per stage).
pub fn decode_asus_dpi_xy(spec: &AsusMouseSpec, data: &[u8]) -> Option<Vec<(u16, u16)>> {
    let needed = 4 + usize::from(spec.dpi_stages) * 4;
    if data.len() < needed || data[0] != 0x12 || data[1] != 0x04 || data[2] != 0x02 {
        return None;
    }
    let mut stages = Vec::with_capacity(usize::from(spec.dpi_stages));
    for i in 0..usize::from(spec.dpi_stages) {
        let off = 4 + i * 4;
        let rx = u16::from_le_bytes([data[off], data[off + 1]]);
        let ry = u16::from_le_bytes([data[off + 2], data[off + 3]]);
        let dx = rx
            .saturating_add(1)
            .saturating_mul(spec.dpi_step)
            .clamp(spec.min_dpi, spec.max_dpi);
        let dy = ry
            .saturating_add(1)
            .saturating_mul(spec.dpi_step)
            .clamp(spec.min_dpi, spec.max_dpi);
        stages.push((dx, dy));
    }
    Some(stages)
}

/// Decodes an ASUS `12 04 03` DPI stage RGB indicator colors block (`3` bytes RGB per stage).
pub fn decode_asus_dpi_colors(spec: &AsusMouseSpec, data: &[u8]) -> Option<Vec<[u8; 3]>> {
    let needed = 4 + usize::from(spec.dpi_stages) * 3;
    if data.len() < needed || data[0] != 0x12 || data[1] != 0x04 || data[2] != 0x03 {
        return None;
    }
    let mut colors = Vec::with_capacity(usize::from(spec.dpi_stages));
    for i in 0..usize::from(spec.dpi_stages) {
        let off = 4 + i * 3;
        colors.push([data[off], data[off + 1], data[off + 2]]);
    }
    Some(colors)
}

/// Decodes the 1-based active DPI stage (`1..=dpi_stages`) from `12 00` reply.
pub fn decode_asus_active_dpi_stage(spec: &AsusMouseSpec, data: &[u8]) -> Option<u8> {
    if data.len() < 12 || data[0] != 0x12 || data[1] != 0x00 || data[2] != 0x00 {
        return None;
    }
    let stage = data[11];
    if (1..=spec.dpi_stages).contains(&stage) {
        Some(stage)
    } else {
        None
    }
}

/// Decodes Lift-Off Distance (`Low -> 1.0 mm`, `High -> 2.0 mm`) from `12 06` reply.
pub fn decode_asus_lod(kind: AsusLodKind, data: &[u8]) -> Option<LiftOffDistance> {
    let offset = match kind {
        AsusLodKind::None => return None,
        AsusLodKind::Standard => 7,
        AsusLodKind::LegacySlot0 => 4,
    };
    if data.len() <= offset || data[0] != 0x12 || data[1] != 0x06 {
        return None;
    }
    match data[offset] {
        0 => Some(LiftOffDistance::Mm10),
        1 => Some(LiftOffDistance::Mm20),
        _ => None,
    }
}

/// Decodes sleep timeout seconds from ASUS `PowerOffSetting` byte (`12 07` `data[5]`).
pub fn decode_asus_sleep_secs(code: u8) -> Option<u16> {
    match code {
        0 => Some(60),
        1 => Some(120),
        2 => Some(180),
        3 => Some(300),
        4 => Some(600),
        _ => None, // 0xFF = Never / disabled
    }
}

/// Encodes desired sleep timeout seconds to the nearest ASUS `PowerOffSetting` enum byte.
pub fn encode_asus_sleep_code(secs: u16) -> u8 {
    match secs {
        0..=90 => 0,    // 1 min
        91..=150 => 1,  // 2 min
        151..=240 => 2, // 3 min
        241..=450 => 3, // 5 min
        _ => 4,         // 10 min
    }
}

/// Decodes battery percentage, charging state, and sleep timeout from `12 07` reply.
pub fn decode_asus_battery(spec: &AsusMouseSpec, data: &[u8]) -> Option<DecodedAsusBattery> {
    if data.len() < 10 || data[0] != 0x12 || data[1] != 0x07 {
        return None;
    }
    let raw_pct = data[4];
    let percent = if spec.battery_quarter_steps {
        raw_pct.saturating_mul(25).min(100)
    } else {
        raw_pct.min(100)
    };
    let charging = data[9] > 0;
    // Sleeping wireless mice answer `12 07` with 0% and not charging.
    if spec.wireless && percent == 0 && !charging {
        return None;
    }
    let sleep_timeout_secs = if spec.auto_power_off {
        decode_asus_sleep_secs(data[5])
    } else {
        None
    };
    Some(DecodedAsusBattery {
        battery: BatteryStatus {
            percent,
            charging,
            voltage_mv: None,
        },
        sleep_timeout_secs,
        low_battery_warn_raw: data[6],
    })
}

/// Encodes an ASUS `51 31 <stage>` DPI write request payload (without leading Report ID byte).
pub fn encode_asus_set_dpi(
    spec: &AsusMouseSpec,
    stage_0based: u8,
    dpi: u16,
    color: Option<[u8; 3]>,
) -> Vec<u8> {
    let clamped = dpi.clamp(spec.min_dpi, spec.max_dpi);
    let encoded = (clamped / spec.dpi_step).saturating_sub(1);
    let mut req = vec![
        0x51,
        0x31,
        stage_0based,
        0x00,
        (encoded & 0xFF) as u8,
        (encoded >> 8) as u8,
    ];
    if let Some([r, g, b]) = color {
        req.extend_from_slice(&[r, g, b]);
    }
    req
}

/// Encodes an ASUS polling rate wire index (`0..=6`) for `51 31 <field_rate> 00 <idx>`.
pub fn encode_asus_polling_index(hz: u16) -> Option<u8> {
    ASUS_WIRE_POLLING_RATES
        .iter()
        .position(|&r| r == hz)
        .map(|i| i as u8)
}

/// Snaps a debounce value in ms to the nearest valid ASUS debounce wire byte (`0x02..=0x07`).
pub fn encode_asus_debounce_byte(ms: u8) -> u8 {
    let snapped = ASUS_DEBOUNCE_STEPS_MS
        .iter()
        .copied()
        .min_by_key(|&v| v.abs_diff(ms))
        .unwrap_or(12);
    snapped / 4 - 1
}

/// Strips the trailing collection suffix (`&col01`, `&col03`) from a Windows HID device path so
/// sibling collections on the same physical ROG Omni dongle can be paired accurately.
fn omni_instance_key(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('#').collect();
    if parts.len() < 3 {
        return lower;
    }
    let inst = parts[2];
    match inst.rfind('&') {
        Some(idx) if idx > 0 => inst[..idx].to_string(),
        _ => inst.to_string(),
    }
}

pub struct AsusRogDevice {
    hid: HidDevice,
    spec: AsusMouseSpec,
    usb_pid: u16,
    report_id: u8,
    payload_len: usize,
    booster: bool,
    display_name: String,
}

impl AsusRogDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.display_name.clone(),
            message: message.into(),
        }
    }

    fn exchange_raw(
        hid: &HidDevice,
        report_id: u8,
        payload_len: usize,
        request: &[u8],
        match_len: usize,
    ) -> std::result::Result<Vec<u8>, String> {
        // Drain any queued async notifications (button bitmap `12 01`, profile button events).
        let mut drain = [0u8; 128];
        for _ in 0..8 {
            match hid.read_timeout(&mut drain, 0) {
                Ok(n) if n > 0 => continue,
                _ => break,
            }
        }

        let mut send = vec![0u8; payload_len + 1];
        send[0] = report_id;
        let copy_len = request.len().min(payload_len);
        send[1..1 + copy_len].copy_from_slice(&request[..copy_len]);

        hid.write(&send)
            .map_err(|e| format!("ASUS HID write (report 0x{report_id:02x}) failed: {e}"))?;

        let deadline = Instant::now() + Duration::from_millis(QUERY_TIMEOUT_MS);
        let mut buf = [0u8; 128];
        let check_len = match_len.min(request.len());

        while Instant::now() < deadline {
            let rem = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .clamp(20, 250) as i32;
            let n = hid
                .read_timeout(&mut buf, rem)
                .map_err(|e| format!("ASUS HID read failed: {e}"))?;
            if n < 2 {
                continue;
            }

            let body: &[u8] = if report_id != 0 {
                if buf[0] != report_id {
                    continue;
                }
                &buf[1..n]
            } else if n > payload_len && buf[0] == 0x00 {
                &buf[1..n]
            } else {
                &buf[..n]
            };

            if body.len() < 2 {
                continue;
            }
            if body[0] == 0xFF && body[1] == 0xAA {
                return Err(
                    "Mouse is asleep or refused the HID request (move mouse to wake it)."
                        .to_string(),
                );
            }
            if body.len() >= 3 && body[0] == 0 && body[1] == 0 && body[2] == 0 {
                return Err(
                    "Mouse returned an empty packet (wireless link asleep — move mouse to wake it)."
                        .to_string(),
                );
            }
            if body.len() >= check_len && body[..check_len] == request[..check_len] {
                return Ok(body.to_vec());
            }
        }

        Err(format!(
            "Timed out waiting for ASUS HID reply to {:02x} {:02x} (move mouse to wake it)",
            request.first().copied().unwrap_or(0),
            request.get(1).copied().unwrap_or(0),
        ))
    }

    fn exchange(&self, request: &[u8], match_len: usize) -> Result<Vec<u8>> {
        Self::exchange_raw(
            &self.hid,
            self.report_id,
            self.payload_len,
            request,
            match_len,
        )
        .map_err(|m| self.dev_err(m))
    }

    fn commit(&self) -> Result<()> {
        let _ = self.exchange(&[0x50, 0x03], 2)?;
        Ok(())
    }

    fn supported_polling_rates(&self) -> Vec<u16> {
        if self.booster && self.spec.supports_booster {
            POLLING_RATES_8K.to_vec()
        } else {
            self.spec.default_polling_rates.to_vec()
        }
    }
}

impl MouseDevice for AsusRogDevice {
    fn info(&self) -> DeviceInfo {
        let connection = if self.usb_pid == ASUS_OMNI_PID {
            if self.booster {
                "ROG Omni 8K Booster (0xFF01)".to_string()
            } else {
                "ROG Omni Wireless (0xFF01)".to_string()
            }
        } else if self.spec.wireless {
            "2.4 GHz Wireless Dongle (0xFF01)".to_string()
        } else {
            "USB Wired (0xFF01)".to_string()
        };
        DeviceInfo {
            id: format!("asus_rog:{:04x}:{:04x}", ASUS_VID, self.spec.pid),
            name: self.display_name.clone(),
            vendor: "ASUS ROG".to_string(),
            vid_pid: Some(format!("{:04x}:{:04x}", ASUS_VID, self.usb_pid)),
            connection,
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let lod_options = match self.spec.lod {
            AsusLodKind::None => Vec::new(),
            AsusLodKind::Standard | AsusLodKind::LegacySlot0 => {
                vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20]
            }
        };
        DeviceCapabilities {
            dpi_min: self.spec.min_dpi,
            dpi_max: self.spec.max_dpi,
            dpi_step: self.spec.dpi_step,
            supports_xy_dpi: false,
            polling_rates_hz: self.supported_polling_rates(),
            lod_options,
            supports_debounce: self.spec.debounce,
            debounce_min_ms: if self.spec.debounce { 12 } else { 0 },
            debounce_max_ms: if self.spec.debounce { 32 } else { 0 },
            supports_motion_sync: self.spec.motion_sync,
            supports_angle_snapping: self.spec.angle_snapping,
            supports_ripple_control: false,
            supports_sleep_timeout: self.spec.auto_power_off,
            sleep_timeout_min_secs: if self.spec.auto_power_off { 60 } else { 0 },
            sleep_timeout_max_secs: if self.spec.auto_power_off { 600 } else { 0 },
            dpi_stage_count: self.spec.dpi_stages,
            supports_dpi_lock: true,
            supports_glass_mode: false,
            supports_volatile_writes: false,
            flash_write_warning: Some(
                "ASUS ROG/TUF backend is implemented from open-source protocol specs (OpenMouse / G-Helper / libratbag) and saves changes to onboard memory (`50 03`). Confirm only if `polled --detect` accurately read your mouse."
                    .to_string(),
            ),
        }
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        let prof_rsp = self.exchange(&[0x12, 0x00], 3)?;
        let active_stage = decode_asus_active_dpi_stage(&self.spec, &prof_rsp)
            .ok_or_else(|| self.dev_err("Malformed ASUS 12 00 profile reply"))?;

        let set_rsp = self.exchange(&[0x12, 0x04, 0x00], 3)?;
        let decoded = decode_asus_settings(&self.spec, &set_rsp, self.booster)
            .ok_or_else(|| self.dev_err("Malformed ASUS 12 04 00 settings reply"))?;

        let stage_idx = usize::from(active_stage.saturating_sub(1));
        let dpi = if self.spec.dpi_xy {
            let xy_rsp = self.exchange(&[0x12, 0x04, 0x02], 3)?;
            let xy = decode_asus_dpi_xy(&self.spec, &xy_rsp)
                .ok_or_else(|| self.dev_err("Malformed ASUS 12 04 02 X/Y DPI reply"))?;
            xy.get(stage_idx).map(|(x, _)| *x)
        } else {
            decoded.dpi_stages_x.get(stage_idx).copied()
        };

        let lod = if self.spec.lod != AsusLodKind::None {
            self.exchange(&[0x12, 0x06], 2)
                .ok()
                .and_then(|r| decode_asus_lod(self.spec.lod, &r))
        } else {
            None
        };

        let motion_sync = if self.spec.motion_sync {
            self.exchange(&[0x12, 0x04, 0x04], 3)
                .ok()
                .filter(|r| r.len() > 4)
                .map(|r| r[4] == 0x01)
        } else {
            None
        };

        let sleep_timeout_secs = if self.spec.auto_power_off {
            self.exchange(&[0x12, 0x07], 2)
                .ok()
                .and_then(|r| decode_asus_battery(&self.spec, &r))
                .and_then(|b| b.sleep_timeout_secs)
        } else {
            None
        };

        Ok(MouseSettings {
            dpi,
            dpi_y: None,
            polling_rate_hz: Some(decoded.polling_rate_hz),
            lod,
            debounce_ms: decoded.debounce_ms,
            motion_sync,
            angle_snapping: self.spec.angle_snapping.then_some(decoded.angle_snapping),
            ripple_control: None,
            sleep_timeout_secs,
            dpi_stage: Some(active_stage),
            lock_dpi_stages: None,
            glass_mode: None,
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        let caps = self.capabilities();
        caps.validate(&self.display_name, settings)?;

        let mut wrote_any = false;

        // 1. Active DPI stage switch (if requested)
        let target_stage_1based = if let Some(stage) = settings.dpi_stage {
            let clamped = stage.clamp(1, self.spec.dpi_stages);
            self.exchange(&[0x51, 0x31, 0x09, 0x00, clamped], 2)?;
            wrote_any = true;
            Some(clamped)
        } else {
            None
        };

        // 2. DPI value write (preserving existing stage RGB colors when supported)
        if let Some(dpi) = settings.dpi {
            let active_1based = match target_stage_1based {
                Some(s) => s,
                None => {
                    let prof = self.exchange(&[0x12, 0x00], 3)?;
                    decode_asus_active_dpi_stage(&self.spec, &prof).unwrap_or(1)
                }
            };
            let colors = if self.spec.dpi_colors {
                self.exchange(&[0x12, 0x04, 0x03], 3)
                    .ok()
                    .and_then(|r| decode_asus_dpi_colors(&self.spec, &r))
            } else {
                None
            };

            if settings.lock_dpi_stages == Some(true) {
                for stage_0 in 0..self.spec.dpi_stages {
                    let color = colors
                        .as_ref()
                        .and_then(|c| c.get(usize::from(stage_0)).copied());
                    let req = encode_asus_set_dpi(&self.spec, stage_0, dpi, color);
                    self.exchange(&req, 2)?;
                    wrote_any = true;
                }
            } else {
                let stage_0 = active_1based.saturating_sub(1);
                let color = colors
                    .as_ref()
                    .and_then(|c| c.get(usize::from(stage_0)).copied());
                let req = encode_asus_set_dpi(&self.spec, stage_0, dpi, color);
                self.exchange(&req, 2)?;
                wrote_any = true;
            }
        }

        // 3. Polling rate
        if let Some(hz) = settings.polling_rate_hz {
            let idx = encode_asus_polling_index(hz)
                .ok_or_else(|| self.dev_err(format!("Unsupported ASUS polling rate {hz} Hz")))?;
            let field = setting_field(&self.spec, 0);
            self.exchange(&[0x51, 0x31, field, 0x00, idx], 2)?;
            wrote_any = true;
        }

        // 4. Debounce
        if let Some(ms) = settings.debounce_ms {
            if self.spec.debounce {
                let raw = encode_asus_debounce_byte(ms);
                let field = setting_field(&self.spec, 1);
                self.exchange(&[0x51, 0x31, field, 0x00, raw], 2)?;
                wrote_any = true;
            }
        }

        // 5. Angle snapping
        if let Some(enabled) = settings.angle_snapping {
            if self.spec.angle_snapping {
                let field = setting_field(&self.spec, 2);
                self.exchange(&[0x51, 0x31, field, 0x00, u8::from(enabled)], 2)?;
                wrote_any = true;
            }
        }

        // 6. Motion Sync (Harpe II Ace — firmware rejects enabling Motion Sync at 8000 Hz)
        if let Some(enabled) = settings.motion_sync {
            if self.spec.motion_sync {
                let effective_hz = settings.polling_rate_hz.unwrap_or(1000);
                if !(enabled && effective_hz >= 8000) {
                    self.exchange(&[0x51, 0x31, 0x12, 0x00, u8::from(enabled)], 2)?;
                    wrote_any = true;
                }
            }
        }

        // 7. Lift-Off Distance
        if let Some(lod) = settings.lod {
            let val = match lod {
                LiftOffDistance::Mm07 | LiftOffDistance::Mm10 => 0u8,
                LiftOffDistance::Mm20 => 1u8,
            };
            match self.spec.lod {
                AsusLodKind::Standard => {
                    self.exchange(&[0x51, 0x35, 0xFF, 0x00, 0xFF, val], 2)?;
                    wrote_any = true;
                }
                AsusLodKind::LegacySlot0 => {
                    self.exchange(&[0x51, 0x35, 0x00, 0x00, val], 2)?;
                    wrote_any = true;
                }
                AsusLodKind::None => {}
            }
        }

        // 8. Auto power-off / sleep timeout (preserving existing low-battery warning threshold)
        if let Some(secs) = settings.sleep_timeout_secs {
            if self.spec.auto_power_off {
                let low_batt_warn = self
                    .exchange(&[0x12, 0x07], 2)
                    .ok()
                    .and_then(|r| decode_asus_battery(&self.spec, &r))
                    .map(|b| b.low_battery_warn_raw)
                    .unwrap_or(20);
                let code = encode_asus_sleep_code(secs);
                self.exchange(&[0x51, 0x37, 0x00, 0x00, code, 0x00, low_batt_warn], 2)?;
                wrote_any = true;
            }
        }

        if wrote_any {
            self.commit()?;
        }

        Ok(())
    }

    fn read_battery(&mut self) -> Option<BatteryStatus> {
        if !self.spec.battery {
            return None;
        }
        let rsp = self.exchange(&[0x12, 0x07], 2).ok()?;
        decode_asus_battery(&self.spec, &rsp).map(|b| b.battery)
    }
}

pub struct AsusRogProtocolDriver;

impl DeviceDriver for AsusRogProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "ASUS ROG / TUF 64B & Omni"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();
        let mut seen_paths = HashSet::new();

        // 1. Direct USB wired & dedicated wireless receivers (`usage_page == 0xFF01`, `usage == 0x0001`).
        let mut seen_direct_pids = HashSet::new();
        for info in api.device_list() {
            if info.vendor_id() != ASUS_VID || info.product_id() == ASUS_OMNI_PID {
                continue;
            }
            if info.usage_page() != ASUS_MOUSE_USAGE_PAGE || info.usage() != ASUS_USAGE {
                continue;
            }
            let Some(mut spec) = lookup_asus_direct_model(info.product_id()) else {
                continue;
            };
            let path = info.path().to_string_lossy().to_string();
            if !seen_paths.insert(path.clone()) || !seen_direct_pids.insert(info.product_id()) {
                continue;
            }

            let Ok(hid) = api.open_path(info.path()) else {
                continue;
            };

            // Verify descriptor supports our target report ID with 63/64-byte output & input reports.
            let mut desc = [0u8; 4096];
            let payload_len = match hid.get_report_descriptor(&mut desc) {
                Ok(n) if n > 0 => {
                    let parsed = parse_hid_report_descriptor(&desc[..n]);
                    let out_len = parsed.output_reports.get(&spec.report_id).copied();
                    let in_len = parsed.input_reports.get(&spec.report_id).copied();
                    match (out_len, in_len) {
                        (Some(o), Some(i)) if (63..=64).contains(&o) && (63..=64).contains(&i) => o,
                        _ => continue,
                    }
                }
                _ => {
                    if spec.report_id == 0 {
                        ASUS_DIRECT_PAYLOAD_LEN
                    } else {
                        ASUS_OMNI_PAYLOAD_LEN
                    }
                }
            };

            let mut display_name = spec.name.to_string();
            // Harpe II Extreme Edition 20 shares 0x1AD0 with Harpe II Ace Wireless.
            if info.product_id() == ASUS_HARPE_II_WIRELESS_PID {
                if let Some(prod) = info.product_string() {
                    if prod.to_ascii_uppercase().contains("EXTREME") {
                        spec.max_dpi = 65_000;
                        spec.dpi_step = 1;
                        display_name = "ROG Harpe II Extreme Edition (Wireless)".to_string();
                    }
                }
            }

            found.push(Box::new(AsusRogDevice {
                hid,
                spec,
                usb_pid: info.product_id(),
                report_id: spec.report_id,
                payload_len,
                booster: false,
                display_name,
            }));
        }

        // 2. ROG Omni Multi-Device Receivers (`0x0B05:0x1ACE`):
        //    Pair each `0xFF02` control collection (`Report ID 0x01`) with its sibling `0xFF01`
        //    mouse collection (`Report ID 0x03`) on the same USB dongle instance.
        let mut omni_ctrl: HashMap<String, &hidapi::DeviceInfo> = HashMap::new();
        let mut omni_mouse_cols: HashMap<String, Vec<&hidapi::DeviceInfo>> = HashMap::new();

        for info in api.device_list() {
            if info.vendor_id() != ASUS_VID || info.product_id() != ASUS_OMNI_PID {
                continue;
            }
            let path = info.path().to_string_lossy().to_string();
            let key = omni_instance_key(&path);
            if info.usage_page() == ASUS_OMNI_USAGE_PAGE && info.usage() == ASUS_USAGE {
                omni_ctrl.entry(key).or_insert(info);
            } else if info.usage_page() == ASUS_MOUSE_USAGE_PAGE && info.usage() == ASUS_USAGE {
                omni_mouse_cols.entry(key).or_default().push(info);
            }
        }

        for (instance_key, ctrl_info) in omni_ctrl {
            let Some(mouse_candidates) = omni_mouse_cols.get(&instance_key) else {
                continue;
            };
            let Ok(ctrl_hid) = api.open_path(ctrl_info.path()) else {
                continue;
            };

            let pair_rsp = match AsusRogDevice::exchange_raw(
                &ctrl_hid,
                ASUS_OMNI_CTRL_REPORT_ID,
                ASUS_OMNI_PAYLOAD_LEN,
                &[0xA0, 0x00],
                2,
            ) {
                Ok(rsp) => rsp,
                Err(_) => continue,
            };
            drop(ctrl_hid);

            let Some(slots) = decode_omni_paired_devices(&pair_rsp) else {
                continue;
            };

            let Some((slot, spec)) = slots
                .into_iter()
                .find_map(|s| lookup_asus_omni_mouse(s.product_id).map(|m| (s, m)))
            else {
                continue;
            };

            let mouse_report_id = if slot.report_id != 0 {
                slot.report_id
            } else {
                ASUS_OMNI_MOUSE_REPORT_ID
            };

            // Pick the sibling mouse collection that advertises `mouse_report_id`.
            for cand in mouse_candidates {
                let path = cand.path().to_string_lossy().to_string();
                if seen_paths.contains(&path) {
                    continue;
                }
                let Ok(mouse_hid) = api.open_path(cand.path()) else {
                    continue;
                };
                let mut desc = [0u8; 4096];
                let payload_len = match mouse_hid.get_report_descriptor(&mut desc) {
                    Ok(n) if n > 0 => {
                        let parsed = parse_hid_report_descriptor(&desc[..n]);
                        match (
                            parsed.output_reports.get(&mouse_report_id).copied(),
                            parsed.input_reports.get(&mouse_report_id).copied(),
                        ) {
                            (Some(o), Some(i))
                                if (63..=64).contains(&o) && (63..=64).contains(&i) =>
                            {
                                o
                            }
                            _ => continue,
                        }
                    }
                    _ => ASUS_OMNI_PAYLOAD_LEN,
                };

                let booster = if spec.supports_booster {
                    AsusRogDevice::exchange_raw(
                        &mouse_hid,
                        mouse_report_id,
                        payload_len,
                        &[0x7D, 0x20, 0x02],
                        3,
                    )
                    .ok()
                    .and_then(|r| r.get(4).copied())
                        == Some(0x01)
                } else {
                    false
                };

                seen_paths.insert(path.clone());
                let display_name = if booster {
                    format!("{} · 8K Booster", spec.name)
                } else {
                    spec.name.to_string()
                };
                found.push(Box::new(AsusRogDevice {
                    hid: mouse_hid,
                    spec,
                    usb_pid: ASUS_OMNI_PID,
                    report_id: mouse_report_id,
                    payload_len,
                    booster,
                    display_name,
                }));
                break;
            }
        }

        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_4stage_aimpoint_settings_xy_dpi_colors_and_booster_polling() {
        let spec = lookup_asus_direct_model(0x1A94).unwrap(); // ROG Harpe Ace Aim Lab
        assert_eq!(spec.dpi_stages, 4);
        assert!(spec.dpi_xy);
        assert!(spec.dpi_colors);

        // `12 04 00`: 4 DPI stages at bytes 4..12, rate at 12, debounce at 14, snap at 16.
        let mut set = [0u8; 64];
        set[0] = 0x12;
        set[1] = 0x04;
        set[2] = 0x00;
        // High nibble 0x06 (8000 Hz) with booster on, low nibble 0x03 (1000 Hz) without booster.
        set[12] = 0x63;
        set[14] = 0x03; // 3 * 4 + 4 = 16 ms
        set[16] = 0x01; // angle snapping on

        let no_boost = decode_asus_settings(&spec, &set, false).unwrap();
        assert_eq!(no_boost.polling_rate_hz, 1000);
        assert_eq!(no_boost.debounce_ms, Some(16));
        assert!(no_boost.angle_snapping);

        let boosted = decode_asus_settings(&spec, &set, true).unwrap();
        assert_eq!(boosted.polling_rate_hz, 8000);

        // `12 04 02`: 4 stages * 4 bytes X/Y (800 -> raw 15, 1600 -> raw 31)
        let mut xy = [0u8; 64];
        xy[0] = 0x12;
        xy[1] = 0x04;
        xy[2] = 0x02;
        xy[4..6].copy_from_slice(&15u16.to_le_bytes());
        xy[6..8].copy_from_slice(&15u16.to_le_bytes());
        xy[8..10].copy_from_slice(&31u16.to_le_bytes());
        xy[10..12].copy_from_slice(&31u16.to_le_bytes());
        let decoded_xy = decode_asus_dpi_xy(&spec, &xy).unwrap();
        assert_eq!(decoded_xy[0], (800, 800));
        assert_eq!(decoded_xy[1], (1600, 1600));

        // `12 04 03`: stage colors preserved during DPI writes
        let mut cols = [0u8; 64];
        cols[0] = 0x12;
        cols[1] = 0x04;
        cols[2] = 0x03;
        cols[4..7].copy_from_slice(&[0xFF, 0x00, 0x00]);
        cols[7..10].copy_from_slice(&[0x00, 0xFF, 0x80]);
        let decoded_cols = decode_asus_dpi_colors(&spec, &cols).unwrap();
        assert_eq!(decoded_cols[0], [0xFF, 0x00, 0x00]);
        assert_eq!(decoded_cols[1], [0x00, 0xFF, 0x80]);

        let write_pkt = encode_asus_set_dpi(&spec, 1, 1600, Some(decoded_cols[1]));
        assert_eq!(
            write_pkt,
            vec![0x51, 0x31, 0x01, 0x00, 31, 0, 0x00, 0xFF, 0x80]
        );
    }

    #[test]
    fn decodes_2stage_gladius_ii_offsets_and_debounce() {
        let spec = lookup_asus_direct_model(0x1845).unwrap(); // ROG Gladius II
        assert_eq!(spec.dpi_stages, 2);

        let mut set = [0u8; 64];
        set[0] = 0x12;
        set[1] = 0x04;
        set[2] = 0x00;
        set[4..6].copy_from_slice(&7u16.to_le_bytes()); // (7+1)*100 = 800 DPI
        set[6..8].copy_from_slice(&15u16.to_le_bytes()); // (15+1)*100 = 1600 DPI
        set[8] = 0x02; // 500 Hz
        set[10] = 0x04; // 20 ms
        set[12] = 0x00; // snap off

        let dec = decode_asus_settings(&spec, &set, false).unwrap();
        assert_eq!(dec.dpi_stages_x, vec![800, 1600]);
        assert_eq!(dec.polling_rate_hz, 500);
        assert_eq!(dec.debounce_ms, Some(20));
        assert!(!dec.angle_snapping);
        assert_eq!(encode_asus_debounce_byte(14), 0x02); // snaps to 12 ms
        assert_eq!(encode_asus_debounce_byte(20), 0x04);
    }

    #[test]
    fn decodes_omni_pair_list_and_skips_keyboards() {
        let mut rsp = [0u8; 63];
        rsp[0] = 0xA0;
        rsp[2..4].copy_from_slice(&2u16.to_le_bytes());
        // Slot 0: ROG Azoth keyboard (0x1A85) on report ID 2
        rsp[4..6].copy_from_slice(&0x1A85u16.to_le_bytes());
        rsp[6] = 0x02;
        rsp[7] = 0x04;
        // Slot 1: ROG Keris II Ace mouse (0x1B18) on report ID 3
        rsp[8..10].copy_from_slice(&0x1B18u16.to_le_bytes());
        rsp[10] = 0x03;
        rsp[11] = 0x05;

        let slots = decode_omni_paired_devices(&rsp).unwrap();
        assert_eq!(slots.len(), 2);
        assert!(lookup_asus_omni_mouse(slots[0].product_id).is_none());
        let mouse = lookup_asus_omni_mouse(slots[1].product_id).unwrap();
        assert_eq!(mouse.name, "ROG Keris II Ace (Omni)");
        assert_eq!(slots[1].report_id, 3);
    }

    #[test]
    fn decodes_battery_sleep_and_detects_sleeping_wireless_mouse() {
        let harpe = lookup_asus_direct_model(0x1A94).unwrap();
        let mut rsp = [0u8; 64];
        rsp[0] = 0x12;
        rsp[1] = 0x07;
        rsp[4] = 84;
        rsp[5] = 3; // 5 min = 300s
        rsp[6] = 20;
        rsp[9] = 1; // charging
        let batt = decode_asus_battery(&harpe, &rsp).unwrap();
        assert_eq!(batt.battery.percent, 84);
        assert!(batt.battery.charging);
        assert_eq!(batt.sleep_timeout_secs, Some(300));
        assert_eq!(batt.low_battery_warn_raw, 20);

        // Sleeping wireless mouse replies 0% + not charging -> None.
        rsp[4] = 0;
        rsp[9] = 0;
        assert!(decode_asus_battery(&harpe, &rsp).is_none());

        // Legacy Keris Wireless reports 25% steps.
        let keris = lookup_asus_direct_model(0x1960).unwrap();
        rsp[4] = 3; // 75%
        let kb = decode_asus_battery(&keris, &rsp).unwrap();
        assert_eq!(kb.battery.percent, 75);
    }
}
