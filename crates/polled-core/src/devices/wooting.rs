// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Wooting Analog / Hall-Effect Keyboard USB HID driver & live telemetry reader
//! (Wooting 60HE, 60HE+, 60HE v2, 80HE, 80HE+, Two HE ARM/AVR, UwU / UwU RGB, One / Two).
//!
//! # Protocol Attribution & Licenses (AGPL-3.0-or-later compatible)
//! All command IDs, Standard (`0x1337` / `0xD0`) vs ARM (`0xFF55` / `0xD1`) framing rules,
//! protobuf field numbers, actuation/RT scaling (`raw = 16384 + mm * 4096`), live analog stream
//! (`0xFF53`/`0xFF54`), and RAM-only profile switching sequences are implemented directly from
//! verified open-source references (zero guessed HID bytes):
//! - `OpenMouse-Project/keyboard-protocol` (`src/wooting/index.ts`, `src/drivers/wooting/hid.ts`,
//!   **AGPL-3.0-or-later**)
//! - `clemenscodes/wootswitch` (`docs/hid-protocol.md`, `src/keyboard.rs`, **MIT**)
//! - `WootingKb/wooting-rgb-sdk` (`wooting-usb.c` / `wooting-usb.h`, **MPL-2.0**)
//! - `meeuw/d0da` & community Wooting USB config protocol reference
//!
//! # Safety Invariants
//! - Deliberately NEVER encodes destructive commands: `ResetToBootloader (0x02)`,
//!   feature `KeysOff (0x15)`, `DoSoftReset (0x19)`, or `ResetSettings (0x2B)`.
//! - Profile switching (`WootDevInit 0x21 -> ActivateProfile 0x17 -> 100ms -> ReloadProfile 0x26
//!   -> 100ms -> Verify 0x0B`) runs strictly in volatile RAM with zero onboard flash wear.
//! - ARM Base Analog writes (output `0x11`) and Global Settings writes (output `0x17`) read the
//!   complete existing protobuf first, preserve all unknown fields byte-for-byte, and verify the
//!   result via mandatory read-back.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use hidapi::{HidApi, HidDevice};
use serde::{Deserialize, Serialize};

use crate::error::{PolledError, Result};

/// Official Wooting USB Vendor ID (60HE, 60HE+, 60HE v2, 80HE, Two HE, UwU, One/Two).
pub const WOOTING_VID: u16 = 0x31E3;
/// Legacy Atmel VID used by original Wooting One/Two boards.
pub const WOOTING_LEGACY_VID: u16 = 0x03EB;
pub const WOOTING_LEGACY_PIDS: &[u16] = &[0x2402, 0x2403, 0xFF01, 0xFF02];

/// Vendor config HID usage pages: Standard (`0x1337`) and ARM (`0xFF55`), usage `0x01`.
pub const WOOTING_USAGE_PAGE_STANDARD: u16 = 0x1337;
pub const WOOTING_USAGE_PAGE_ARM: u16 = 0xFF55;
pub const WOOTING_CONFIG_USAGE: u16 = 0x01;

/// Live analog key-travel HID input stream usage pages (`0xFF53` / `0xFF54`).
pub const WOOTING_ANALOG_USAGE_PAGES: &[u16] = &[0xFF53, 0xFF54];

pub const WOOTING_MAGIC_STANDARD: u8 = 0xD0;
pub const WOOTING_MAGIC_ARM: u8 = 0xD1;
pub const WOOTING_MAGIC_1: u8 = 0xDA;
pub const WOOTING_STATUS_OK: u8 = 0x88;
pub const WOOTING_STATUS_ERROR: u8 = 0x66;

/// Verified Wooting HID feature-report command IDs.
pub const CMD_GET_VERSION: u8 = 0x01;
pub const CMD_GET_SERIAL: u8 = 0x03;
pub const CMD_GET_RGB_PROFILE_COUNT: u8 = 0x04;
pub const CMD_GET_DIGITAL_PROFILES_COUNT: u8 = 0x09;
pub const CMD_GET_ANALOG_PROFILES_COUNT: u8 = 0x0A;
pub const CMD_GET_CURRENT_PROFILE_INDEX: u8 = 0x0B;
pub const CMD_GET_NUMBER_OF_KEYS: u8 = 0x10;
pub const CMD_GET_DEVICE_CONFIG: u8 = 0x13;
pub const CMD_ACTIVATE_PROFILE: u8 = 0x17;
pub const CMD_WOOT_DEV_INIT: u8 = 0x21;
pub const CMD_RELOAD_PROFILE: u8 = 0x26;
pub const CMD_GET_KEYBOARD_PROFILE: u8 = 0x27;
pub const CMD_GET_ACTUATION_PROFILE: u8 = 0x31;
pub const CMD_GET_RGB_PROFILE_CORE: u8 = 0x32;
pub const CMD_GET_SETTINGS: u8 = 0x33;
pub const CMD_GET_AKC_PROFILE: u8 = 0x34;
pub const CMD_GET_RAPID_TRIGGER_PROFILE: u8 = 0x36;
pub const CMD_GET_PROFILE_METADATA: u8 = 0x37;
pub const CMD_IS_FLASH_CHIP_CONNECTED: u8 = 0x38;
pub const CMD_GET_FLASH_STATS: u8 = 0x3A;
pub const CMD_GET_FULL_RAPID_TRIGGER_PROFILE: u8 = 0x3D;

/// Verified Wooting ARM HID output-report command IDs (distinct namespace from feature commands!).
pub const OUT_CMD_SET_KEYBOARD_BASE: u8 = 0x11;
pub const OUT_CMD_SET_GLOBAL_SETTINGS: u8 = 0x17;

/// Linear actuation & rapid-trigger scaling confirmed on Wooting ARM hardware:
/// `raw = 16384 + mm * 4096` (`0x4000` = 0.00 mm, `0x8000` = 4.00 mm).
pub const WOOTING_ACTUATION_ZERO: f64 = 16_384.0;
pub const WOOTING_ACTUATION_UNITS_PER_MM: f64 = 4_096.0;
pub const WOOTING_MAX_TRAVEL_MM: f64 = 4.0;

/// Maximum onboard profile index supported by Polled (`0..=7`, up to 8 slots; most boards ship 4).
pub const WOOTING_MAX_PROFILE_INDEX: u8 = 7;

const WOOTING_SWITCH_SETTLE_MS: u64 = 100;
const HID_DRAIN_QUIET_MS: i32 = 8;
const HID_READ_TIMEOUT_MS: i32 = 250;
const HID_RESPONSE_RETRIES: usize = 8;

/// Wire protocol family selected by the HID config interface usage page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WootingProtocol {
    /// Usage page `0x1337`, Report ID `0x00`, Magic `0xD0 0xDA`, profile slot at byte 7.
    Standard,
    /// Usage page `0xFF55`, Report ID `0x01`, Magic `0xD1 0xDA`, profile slot at byte 4.
    Arm,
}

impl WootingProtocol {
    pub fn label(self) -> &'static str {
        match self {
            WootingProtocol::Standard => "Standard (AVR)",
            WootingProtocol::Arm => "ARM",
        }
    }

    pub fn report_id(self) -> u8 {
        match self {
            WootingProtocol::Standard => 0x00,
            WootingProtocol::Arm => 0x01,
        }
    }

    pub fn magic0(self) -> u8 {
        match self {
            WootingProtocol::Standard => WOOTING_MAGIC_STANDARD,
            WootingProtocol::Arm => WOOTING_MAGIC_ARM,
        }
    }
}

/// Static model descriptor matched via `(vid, pid & 0xFFF0)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WootingModelSpec {
    pub name: &'static str,
    pub chip: &'static str,
    pub switch_tech: &'static str,
    pub has_rgb: bool,
}

/// Single onboard profile slot read directly from keyboard firmware (`GetProfileMetadata 0x37`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WootingProfileSlot {
    /// 0-based firmware profile index (`0..=7`).
    pub index: u8,
    /// Custom profile name read from keyboard firmware (or fallback `"Profile N"`).
    pub name: String,
    pub is_active: bool,
}

/// Base analog / actuation / rapid-trigger / Tachyon configuration for the active profile (`0x27`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WootingAnalogProfile {
    pub actuation_mm: Option<f32>,
    pub digital_output_enabled: Option<bool>,
    pub rapid_trigger_enabled: Option<bool>,
    pub rapid_trigger_sensitivity_mm: Option<f32>,
    pub tachyon_enabled: Option<bool>,
    pub tachyon_mode: Option<u32>,
    pub tick_rate: Option<u32>,
    pub strict_actuation_range: Option<bool>,
}

/// Single per-key actuation override decoded from `GetActuationProfile (0x31)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WootingKeyActuationOverride {
    pub matrix_index: u16,
    pub key_label: String,
    pub actuation_mm: f32,
}

/// Decoded per-key rapid-trigger group from `GetFullRapidTriggerProfile (0x3D)` or legacy `0x36`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WootingRapidTriggerGroupInfo {
    pub key_labels: Vec<String>,
    pub enabled: Option<bool>,
    pub primary_sensitivity_mm: Option<f32>,
    pub secondary_sensitivity_mm: Option<f32>,
    pub continuous_mode: Option<bool>,
}

/// Decoded Advanced Key / AKC entry (`SOCD`, `Rappy Snappy`, `DKS`, `Mod Tap`, `Toggle Key`) from `0x34`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WootingAkcSummary {
    pub kind: String,
    pub primary_key: String,
    pub layer: u8,
    pub detail: String,
}

/// Decoded device-wide `GlobalSettings` protobuf from `GetSettings (0x33)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WootingGlobalSettings {
    pub device_name: Option<String>,
    pub rgb_dim_minutes: Option<u32>,
    pub rgb_off_minutes: Option<u32>,
    pub rgb_turn_off: Option<bool>,
    pub invert_num_lock: Option<bool>,
    pub debounce_filter: Option<bool>,
}

/// Decoded RGB core summary from `GetRgbProfileCore (0x32)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WootingRgbSummary {
    pub brightness_raw: Option<u8>,
    pub brightness_pct: Option<u8>,
    pub effect_layers: Vec<String>,
}

/// Scalar hardware diagnostics (`0x10`, `0x04`, `0x0A`, `0x38`, `0x3A`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WootingDiagnostics {
    pub key_count: Option<u8>,
    pub rgb_profile_count: Option<u8>,
    pub analog_profile_count: Option<u8>,
    pub flash_chip_connected: Option<bool>,
    pub flash_used_pages: Option<u16>,
    pub flash_total_pages: Option<u16>,
}

/// Full live status snapshot grabbed from a connected Wooting keyboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WootingKeyboardInfo {
    pub id: String,
    pub name: String,
    pub vid_pid: String,
    pub protocol: WootingProtocol,
    pub chip_type: String,
    pub switch_tech: String,
    pub firmware_version: Option<String>,
    pub layout: Option<String>,
    /// 0-based runtime-active onboard profile index (`0..=7`), if readable.
    pub active_profile: Option<u8>,
    /// Flash-stored default profile index (`0..=7`), if readable.
    pub flash_default_profile: Option<u8>,
    /// Real onboard profile slots & custom names read from keyboard firmware.
    pub profiles: Vec<WootingProfileSlot>,
    pub analog_profile: Option<WootingAnalogProfile>,
    pub per_key_actuation: Vec<WootingKeyActuationOverride>,
    pub per_key_rapid_trigger: Vec<WootingRapidTriggerGroupInfo>,
    pub advanced_keys: Vec<WootingAkcSummary>,
    pub global_settings: Option<WootingGlobalSettings>,
    pub rgb: Option<WootingRgbSummary>,
    pub diagnostics: WootingDiagnostics,
    /// `true` when the board supports safe ARM read-modify-write for base analog & global settings.
    pub supports_arm_writes: bool,
}

impl WootingKeyboardInfo {
    pub fn profile_label(idx: u8) -> &'static str {
        match idx {
            0 => "Profile 1 (Typing / Digital)",
            1 => "Profile 2 (Game 1)",
            2 => "Profile 3 (Game 2)",
            3 => "Profile 4 (Game 3)",
            4 => "Profile 5",
            5 => "Profile 6",
            6 => "Profile 7",
            7 => "Profile 8",
            _ => "Profile",
        }
    }

    /// Returns the real custom profile name for `idx` when read from firmware, else a clean fallback.
    pub fn profile_display_name(&self, idx: u8) -> String {
        if let Some(slot) = self.profiles.iter().find(|p| p.index == idx) {
            if !slot.name.trim().is_empty() {
                return format!("P{} · {}", idx + 1, slot.name.trim());
            }
        }
        format!("P{}", idx + 1)
    }

    /// Compact one-line summary for sidebar / status readouts.
    pub fn compact_status_summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(idx) = self.active_profile {
            parts.push(self.profile_display_name(idx));
        }
        if let Some(ap) = &self.analog_profile {
            if let Some(mm) = ap.actuation_mm {
                parts.push(format!("{mm:.2}mm"));
            }
            if ap.rapid_trigger_enabled == Some(true) {
                if let Some(rt) = ap.rapid_trigger_sensitivity_mm {
                    parts.push(format!("RT {rt:.2}mm"));
                } else {
                    parts.push("RT ON".to_string());
                }
            }
            if ap.tachyon_enabled == Some(true) {
                parts.push("Tachyon".to_string());
            }
        }
        if parts.is_empty() {
            "Connected".to_string()
        } else {
            parts.join(" · ")
        }
    }
}

/// Requested changes for ARM base analog settings (`OUT_CMD_SET_KEYBOARD_BASE 0x11`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WootingAnalogBasePatch {
    pub actuation_mm: Option<f32>,
    pub rapid_trigger_enabled: Option<bool>,
    pub rapid_trigger_sensitivity_mm: Option<f32>,
}

/// Requested changes for ARM global device settings (`OUT_CMD_SET_GLOBAL_SETTINGS 0x17`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WootingGlobalSettingsPatch {
    pub rgb_dim_minutes: Option<u32>,
    pub rgb_off_minutes: Option<u32>,
    pub rgb_turn_off: Option<bool>,
    pub invert_num_lock: Option<bool>,
    pub debounce_filter: Option<bool>,
}

/// One live analog key reading streamed from Wooting's `0xFF53` (V2, 10-bit) or `0xFF54` (V1, 8-bit)
/// analog HID interface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WootingAnalogKeyReading {
    /// Stable per-switch identifier (physical `matrix_pos` on V2 `0xFF53`, HID usage on V1 `0xFF54`).
    pub stable_id: u16,
    /// USB HID keyboard usage ID (`0x04..=0xE7` when namespace == 0).
    pub usage: u16,
    /// Physical switch matrix index `(row << 5) | col` when reported by V2 (`0xFF53`).
    pub matrix_index: Option<u16>,
    pub key_label: String,
    /// Raw hardware analog count (`0..=1023` on V2 `0xFF53`, `0..=255` on V1 `0xFF54`).
    pub raw_value: u16,
    /// Full-scale denominator (`1023` on V2 `0xFF53`, `255` on V1 `0xFF54`).
    pub raw_max: u16,
    pub travel_mm: f32,
    /// Real-time hardware actuation / Rapid Trigger state reported in V2 (`0xFF53`) bit 0.
    pub actuated: Option<bool>,
}

/// Converts a Wooting raw actuation/RT integer to millimetres (`0.0..=4.0`).
///
/// Supports both verified firmware encodings:
/// - High-precision 16-bit Q12 (`raw >= 16384` / `0x4000`): `raw = 16384 + mm * 4096`
/// - Legacy/standard 8-bit scale (`raw <= 255`): `raw = round(mm / 4.0 * 255)` (live-verified on 60HE+ fw v2.13.2)
pub fn wooting_raw_to_mm(raw: u64) -> f32 {
    if raw >= 16_384 {
        (((raw as f64) - WOOTING_ACTUATION_ZERO) / WOOTING_ACTUATION_UNITS_PER_MM)
            .clamp(0.0, WOOTING_MAX_TRAVEL_MM) as f32
    } else if raw <= 255 {
        let mm = (raw as f64 / 255.0) * WOOTING_MAX_TRAVEL_MM;
        // Snap 8-bit quantization (1/255 * 4.0 ≈ 0.0157 mm) to Wootility's 0.05 mm display grid.
        ((mm * 20.0).round() / 20.0).clamp(0.0, WOOTING_MAX_TRAVEL_MM) as f32
    } else {
        0.0
    }
}

/// Converts millimetres (`0.0..=4.0`) to Wooting's 16-bit Q12 raw integer scale (`16384 + mm * 4096`).
pub fn wooting_mm_to_raw(mm: f32) -> u64 {
    let clamped = f64::from(mm).clamp(0.0, WOOTING_MAX_TRAVEL_MM);
    (WOOTING_ACTUATION_ZERO + clamped * WOOTING_ACTUATION_UNITS_PER_MM).round() as u64
}

/// Converts millimetres (`0.0..=4.0`) to Wooting's 8-bit raw integer scale (`round(mm / 4.0 * 255)`).
pub fn wooting_mm_to_raw_u8(mm: f32) -> u64 {
    let clamped = f64::from(mm).clamp(0.0, WOOTING_MAX_TRAVEL_MM);
    ((clamped / WOOTING_MAX_TRAVEL_MM) * 255.0)
        .round()
        .clamp(1.0, 255.0) as u64
}

/// Identifies a Wooting model specification from USB `(vid, pid)` (accepting low-nibble variants).
pub fn resolve_wooting_model_spec(vid: u16, pid: u16) -> Option<WootingModelSpec> {
    if vid == WOOTING_LEGACY_VID {
        return match pid {
            0x2402 | 0xFF01 => Some(WootingModelSpec {
                name: "Wooting One",
                chip: "AVR",
                switch_tech: "Flaretech",
                has_rgb: true,
            }),
            0x2403 | 0xFF02 => Some(WootingModelSpec {
                name: "Wooting Two",
                chip: "AVR",
                switch_tech: "Flaretech",
                has_rgb: true,
            }),
            _ => None,
        };
    }
    if vid != WOOTING_VID {
        return None;
    }
    let family = pid & 0xFFF0;
    let spec = match family {
        0x1100 => WootingModelSpec {
            name: "Wooting One",
            chip: "AVR",
            switch_tech: "Flaretech",
            has_rgb: true,
        },
        0x1200 => WootingModelSpec {
            name: "Wooting Two",
            chip: "AVR",
            switch_tech: "Flaretech",
            has_rgb: true,
        },
        0x1210 => WootingModelSpec {
            name: "Wooting Two Lekker Edition",
            chip: "AVR",
            switch_tech: "Hall Effect",
            has_rgb: true,
        },
        0x1220 => WootingModelSpec {
            name: "Wooting Two HE (AVR)",
            chip: "AVR",
            switch_tech: "Hall Effect",
            has_rgb: true,
        },
        0x1230 => WootingModelSpec {
            name: "Wooting Two HE (ARM)",
            chip: "ARM",
            switch_tech: "Hall Effect",
            has_rgb: true,
        },
        0x1300 => WootingModelSpec {
            name: "Wooting 60HE (AVR)",
            chip: "AVR",
            switch_tech: "Hall Effect",
            has_rgb: true,
        },
        0x1310 => WootingModelSpec {
            name: "Wooting 60HE (ARM)",
            chip: "ARM",
            switch_tech: "Hall Effect",
            has_rgb: true,
        },
        0x1320 => WootingModelSpec {
            name: "Wooting 60HE+",
            chip: "ARM",
            switch_tech: "Hall Effect",
            has_rgb: true,
        },
        0x1340 => WootingModelSpec {
            name: "Wooting 60HE v2",
            chip: "ARM",
            switch_tech: "Hall Effect",
            has_rgb: true,
        },
        0x1400 => WootingModelSpec {
            name: "Wooting 80HE",
            chip: "ARM",
            switch_tech: "Hall Effect",
            has_rgb: true,
        },
        0x1410 => WootingModelSpec {
            name: "Wooting 80HE+",
            chip: "ARM",
            switch_tech: "Hall Effect",
            has_rgb: true,
        },
        0x1500 => WootingModelSpec {
            name: "Wooting UwU",
            chip: "ARM",
            switch_tech: "Hall Effect",
            has_rgb: false,
        },
        0x1510 => WootingModelSpec {
            name: "Wooting UwU RGB",
            chip: "ARM",
            switch_tech: "Hall Effect",
            has_rgb: true,
        },
        _ => return None,
    };
    Some(spec)
}

/// Identifies a friendly Wooting model name from USB `(vid, pid)` and OS product string.
pub fn resolve_wooting_model_name(vid: u16, pid: u16, os_product: Option<&str>) -> String {
    if let Some(spec) = resolve_wooting_model_spec(vid, pid) {
        return spec.name.to_string();
    }
    if let Some(p) = os_product.map(str::trim).filter(|s| !s.is_empty()) {
        return p.to_string();
    }
    format!("Wooting Keyboard ({vid:04x}:{pid:04x})")
}

/// Builds an 8-byte Wooting HID feature report command using exact Standard (`0x00`/`0xD0`) or
/// ARM (`0x01`/`0xD1`) framing: `[report_id, magic0, 0xDA, cmd_id, param3, param2, param1, param0]`.
pub fn build_wooting_feature_cmd(
    protocol: WootingProtocol,
    cmd_id: u8,
    param3: u8,
    param2: u8,
    param1: u8,
    param0: u8,
) -> [u8; 8] {
    [
        protocol.report_id(),
        protocol.magic0(),
        WOOTING_MAGIC_1,
        cmd_id,
        param3,
        param2,
        param1,
        param0,
    ]
}

/// Builds an 8-byte slot-taking command placing `slot` at byte 4 (ARM) or byte 7 (Standard).
pub fn build_wooting_profile_cmd(protocol: WootingProtocol, cmd_id: u8, slot: u8) -> [u8; 8] {
    match protocol {
        WootingProtocol::Arm => build_wooting_feature_cmd(protocol, cmd_id, slot, 0, 0, 0),
        WootingProtocol::Standard => build_wooting_feature_cmd(protocol, cmd_id, 0, 0, 0, slot),
    }
}

/// Normalizes a raw `hidapi` reply buffer by stripping a leading report-ID byte (`0x00` / `0x01`..`0x06`)
/// when present so byte 0 is always `magic0` (`0xD0`/`0xD1`), byte 1 is `0xDA`, byte 2 is `cmd_id`,
/// byte 3 is `status` (`0x88` = OK), and byte 4+ is payload.
pub fn normalize_wooting_reply(raw: &[u8]) -> Option<&[u8]> {
    if raw.len() >= 4
        && (raw[0] == WOOTING_MAGIC_STANDARD || raw[0] == WOOTING_MAGIC_ARM)
        && raw[1] == WOOTING_MAGIC_1
    {
        return Some(raw);
    }
    if raw.len() >= 5
        && (raw[1] == WOOTING_MAGIC_STANDARD || raw[1] == WOOTING_MAGIC_ARM)
        && raw[2] == WOOTING_MAGIC_1
    {
        return Some(&raw[1..]);
    }
    None
}

fn reply_ok_for(raw: &[u8], expected_cmd: u8) -> Option<&[u8]> {
    let norm = normalize_wooting_reply(raw)?;
    if norm.len() >= 4 && norm[2] == expected_cmd && norm[3] == WOOTING_STATUS_OK {
        Some(norm)
    } else {
        None
    }
}

fn reply_blob_body(raw: &[u8], expected_cmd: u8, protocol: WootingProtocol) -> Option<&[u8]> {
    let norm = reply_ok_for(raw, expected_cmd)?;
    match protocol {
        WootingProtocol::Arm => {
            if norm.len() < 6 {
                return None;
            }
            let len = usize::from(norm[4]) | (usize::from(norm[5]) << 8);
            norm.get(6..6 + len)
        }
        WootingProtocol::Standard => {
            if norm.len() < 5 {
                return None;
            }
            let len = usize::from(norm[4]);
            norm.get(5..5 + len)
        }
    }
}

// ---------------------------------------------------------------------------
// Minimal zero-dependency protobuf wire-format decoder & field-preserving patcher
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ProtoField<'a> {
    field: u32,
    wire: u8,
    varint: Option<u64>,
    bytes: Option<&'a [u8]>,
    raw_span: &'a [u8],
}

fn read_varint(data: &[u8], mut at: usize) -> Option<(u64, usize)> {
    let mut result = 0u64;
    let mut shift = 0u32;
    while at < data.len() && shift < 64 {
        let byte = data[at];
        at += 1;
        result |= u64::from(byte & 0x7F) << shift;
        if (byte & 0x80) == 0 {
            return Some((result, at));
        }
        shift += 7;
    }
    None
}

fn encode_varint(mut val: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (val & 0x7F) as u8;
        val >>= 7;
        if val != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if val == 0 {
            break;
        }
    }
}

fn decode_proto_fields(data: &[u8]) -> Vec<ProtoField<'_>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < data.len() {
        let start = at;
        let Some((tag, after_tag)) = read_varint(data, at) else {
            break;
        };
        if tag < 8 {
            break;
        }
        let field = (tag >> 3) as u32;
        let wire = (tag & 7) as u8;
        at = after_tag;
        match wire {
            0 => {
                let Some((val, next)) = read_varint(data, at) else {
                    break;
                };
                at = next;
                out.push(ProtoField {
                    field,
                    wire,
                    varint: Some(val),
                    bytes: None,
                    raw_span: &data[start..at],
                });
            }
            1 => {
                if at + 8 > data.len() {
                    break;
                }
                at += 8;
                out.push(ProtoField {
                    field,
                    wire,
                    varint: None,
                    bytes: None,
                    raw_span: &data[start..at],
                });
            }
            2 => {
                let Some((len, next)) = read_varint(data, at) else {
                    break;
                };
                let len = len as usize;
                at = next;
                if at + len > data.len() {
                    break;
                }
                let slice = &data[at..at + len];
                at += len;
                out.push(ProtoField {
                    field,
                    wire,
                    varint: None,
                    bytes: Some(slice),
                    raw_span: &data[start..at],
                });
            }
            5 => {
                if at + 4 > data.len() {
                    break;
                }
                at += 4;
                out.push(ProtoField {
                    field,
                    wire,
                    varint: None,
                    bytes: None,
                    raw_span: &data[start..at],
                });
            }
            _ => break,
        }
    }
    out
}

fn decode_packed_varints(bytes: &[u8]) -> Option<Vec<u64>> {
    let mut vals = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        let (v, next) = read_varint(bytes, at)?;
        vals.push(v);
        at = next;
    }
    Some(vals)
}

fn replace_proto_varint(message: &[u8], target_field: u32, value: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 10);
    for f in decode_proto_fields(message) {
        if f.field != target_field {
            out.extend_from_slice(f.raw_span);
        }
    }
    encode_varint(u64::from(target_field) << 3, &mut out);
    encode_varint(value, &mut out);
    out
}

fn replace_proto_bytes(message: &[u8], target_field: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + payload.len() + 10);
    for f in decode_proto_fields(message) {
        if f.field != target_field {
            out.extend_from_slice(f.raw_span);
        }
    }
    encode_varint((u64::from(target_field) << 3) | 2, &mut out);
    encode_varint(payload.len() as u64, &mut out);
    out.extend_from_slice(payload);
    out
}

// ---------------------------------------------------------------------------
// Verified response decoders
// ---------------------------------------------------------------------------

/// Decodes `GetVersion (0x01)`: `[D1 DA 01 88 03 00 major minor patch]`.
pub fn decode_wooting_version(raw: &[u8]) -> Option<String> {
    let norm = reply_ok_for(raw, CMD_GET_VERSION)?;
    if norm.len() < 9 {
        return None;
    }
    let (maj, min, pat) = (norm[6], norm[7], norm[8]);
    if maj == 0 && min == 0 && pat == 0 {
        return None;
    }
    Some(format!("{maj}.{min}.{pat}"))
}

/// Decodes `GetDeviceConfig (0x13)` layout byte at offset 9 (without report ID).
pub fn decode_wooting_layout(raw: &[u8]) -> Option<String> {
    let norm = reply_ok_for(raw, CMD_GET_DEVICE_CONFIG)?;
    if norm.len() <= 9 {
        return None;
    }
    // Header-only all-zero replies carry no layout info.
    if norm[4..].iter().all(|&b| b == 0) {
        return None;
    }
    let layout = match norm[9] {
        0 => "ANSI",
        1 => "ISO",
        2 => "JIS",
        3 => "ANSI Split",
        4 => "ISO Split",
        other => return Some(format!("Layout #{other}")),
    };
    Some(layout.to_string())
}

/// Decodes `GetCurrentKeyboardProfileIndex (0x0B)` returning `(runtime_active, flash_default)`.
///
/// On ARM, `data[0]` (norm[4]) is the flash default and `data[2]` (norm[6]) is the runtime-active
/// slot updated by `ActivateProfile`. On Standard, `data[0]` is runtime-active.
pub fn decode_wooting_profile_index(raw: &[u8], protocol: WootingProtocol) -> Option<(u8, u8)> {
    let norm = reply_ok_for(raw, CMD_GET_CURRENT_PROFILE_INDEX)?;
    let flash_default = *norm.get(4)?;
    let active = match protocol {
        WootingProtocol::Arm => norm.get(6).copied().unwrap_or(flash_default),
        WootingProtocol::Standard => flash_default,
    };
    if active <= WOOTING_MAX_PROFILE_INDEX {
        Some((active, flash_default))
    } else {
        None
    }
}

/// Decodes `GetProfileMetadata (0x37)`: returns `Some(None)` if slot absent (`len == 0`),
/// `Some(Some(name))` if slot exists, or `None` if reply is invalid.
pub fn decode_wooting_profile_metadata(raw: &[u8]) -> Option<Option<String>> {
    let norm = reply_ok_for(raw, CMD_GET_PROFILE_METADATA)?;
    if norm.len() < 6 {
        return None;
    }
    let payload_len = usize::from(norm[4]);
    if payload_len == 0 {
        return Some(None);
    }
    let proto = norm.get(6..(6 + payload_len).min(norm.len()))?;
    let name = decode_proto_fields(proto)
        .into_iter()
        .find(|f| f.field == 1 && f.wire == 2)
        .and_then(|f| f.bytes)
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    Some(Some(name))
}

/// Decodes `GetKeyboardProfile (0x27)` base analog/actuation/RT/Tachyon fields.
pub fn decode_wooting_keyboard_profile(
    raw: &[u8],
    protocol: WootingProtocol,
) -> Option<WootingAnalogProfile> {
    let body = reply_blob_body(raw, CMD_GET_KEYBOARD_PROFILE, protocol)?;
    let nested = decode_proto_fields(body)
        .into_iter()
        .find(|f| f.field == 1 && f.wire == 2)?
        .bytes?;
    let fields = decode_proto_fields(nested);
    let get_u64 = |num: u32| -> Option<u64> {
        fields
            .iter()
            .find(|f| f.field == num)
            .and_then(|f| f.varint)
    };
    let get_bool = |num: u32| -> Option<bool> { get_u64(num).map(|v| v != 0) };

    Some(WootingAnalogProfile {
        actuation_mm: get_u64(1).map(wooting_raw_to_mm),
        digital_output_enabled: get_bool(2),
        rapid_trigger_enabled: get_bool(3),
        rapid_trigger_sensitivity_mm: get_u64(4).map(wooting_raw_to_mm),
        tachyon_enabled: get_bool(5),
        tick_rate: get_u64(6).map(|v| v as u32),
        strict_actuation_range: get_bool(7),
        tachyon_mode: get_u64(9).map(|v| v as u32),
    })
}

/// Decodes `GetActuationProfile (0x31)` per-key actuation overrides.
///
/// Handles all three verified protobuf layouts:
/// - Field 4 (`KeyMMPropertyProfile.partialEncoded`, Q12): `actuationRaw << 16 | matrixIndex`
/// - Field 2 (`KeyPropertyProfile.partial`, 8-bit, live-verified on 60HE+ fw v2.13.2): `rawByte << 8 | matrixIndex`
/// - Field 1 (`KeyPropertyProfile.full`, 8-bit matrix rows): row-major bytes (`non-zero` = override)
pub fn decode_wooting_per_key_actuation(
    raw: &[u8],
    protocol: WootingProtocol,
) -> Option<Vec<WootingKeyActuationOverride>> {
    let body = reply_blob_body(raw, CMD_GET_ACTUATION_PROFILE, protocol)?;
    if body.is_empty() {
        return Some(Vec::new());
    }
    let mut out: Vec<WootingKeyActuationOverride> = Vec::new();
    for outer in decode_proto_fields(body) {
        match outer.field {
            // High-precision 16-bit Q12 packed overrides: `raw << 16 | key`
            4 => {
                let Some(inner_bytes) = outer.bytes else {
                    continue;
                };
                let mut packed_vals = Vec::new();
                for inner in decode_proto_fields(inner_bytes) {
                    if inner.field != 1 {
                        continue;
                    }
                    if let Some(v) = inner.varint {
                        packed_vals.push(v);
                    } else if let Some(b) = inner.bytes {
                        if let Some(list) = decode_packed_varints(b) {
                            packed_vals.extend(list);
                        }
                    }
                }
                for v in packed_vals {
                    let matrix_index = (v & 0xFFFF) as u16;
                    let raw_act = v >> 16;
                    out.push(WootingKeyActuationOverride {
                        matrix_index,
                        key_label: wooting_matrix_label(matrix_index),
                        actuation_mm: wooting_raw_to_mm(raw_act),
                    });
                }
            }
            // 8-bit `KeyPropertyProfile.partial` overrides: `rawByte << 8 | key`
            2 => {
                let Some(inner_bytes) = outer.bytes else {
                    continue;
                };
                let mut packed_vals = Vec::new();
                for inner in decode_proto_fields(inner_bytes) {
                    if inner.field != 1 {
                        continue;
                    }
                    if let Some(v) = inner.varint {
                        packed_vals.push(v);
                    } else if let Some(b) = inner.bytes {
                        if let Some(list) = decode_packed_varints(b) {
                            packed_vals.extend(list);
                        }
                    }
                }
                for v in packed_vals {
                    let matrix_index = (v & 0xFF) as u16;
                    let raw_act = (v >> 8) & 0xFF;
                    if raw_act == 0 {
                        continue;
                    }
                    out.push(WootingKeyActuationOverride {
                        matrix_index,
                        key_label: wooting_matrix_label(matrix_index),
                        actuation_mm: wooting_raw_to_mm(raw_act),
                    });
                }
            }
            // 8-bit `KeyPropertyProfile.full` row matrix
            1 => {
                let Some(mat_bytes) = outer.bytes else {
                    continue;
                };
                for (row_idx, row) in decode_proto_fields(mat_bytes)
                    .into_iter()
                    .filter(|f| f.field == 1 && f.bytes.is_some())
                    .enumerate()
                {
                    let Some(rb) = row.bytes else { continue };
                    let Some(vals) = decode_proto_fields(rb)
                        .into_iter()
                        .find(|f| f.field == 1 && f.bytes.is_some())
                        .and_then(|f| f.bytes)
                    else {
                        continue;
                    };
                    for (col_idx, &val) in vals.iter().enumerate() {
                        if val == 0 {
                            continue;
                        }
                        let matrix_index = ((row_idx as u16) << 5) | (col_idx as u16 & 0x1F);
                        out.push(WootingKeyActuationOverride {
                            matrix_index,
                            key_label: wooting_matrix_label(matrix_index),
                            actuation_mm: wooting_raw_to_mm(u64::from(val)),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    Some(out)
}

/// Decodes `GetFullRapidTriggerProfile (0x3D)` ARM per-key rapid trigger groups.
pub fn decode_wooting_full_rt_profile(raw: &[u8]) -> Option<Vec<WootingRapidTriggerGroupInfo>> {
    let body = reply_blob_body(
        raw,
        CMD_GET_FULL_RAPID_TRIGGER_PROFILE,
        WootingProtocol::Arm,
    )?;
    if body.is_empty() {
        return Some(Vec::new());
    }
    let mut groups = Vec::new();
    for outer in decode_proto_fields(body) {
        if outer.field != 1 {
            continue;
        }
        let Some(g_bytes) = outer.bytes else {
            continue;
        };
        let mut keys: Vec<u16> = Vec::new();
        let mut enabled: Option<bool> = None;
        let mut primary_mm: Option<f32> = None;
        let mut secondary_mm: Option<f32> = None;
        let mut continuous: Option<bool> = None;

        for gf in decode_proto_fields(g_bytes) {
            match gf.field {
                1 => {
                    if let Some(v) = gf.varint {
                        keys.push(v as u16);
                    } else if let Some(b) = gf.bytes {
                        if let Some(list) = decode_packed_varints(b) {
                            keys.extend(list.into_iter().map(|v| v as u16));
                        }
                    }
                }
                2 => {
                    if let Some(cfg) = gf.bytes {
                        for cf in decode_proto_fields(cfg) {
                            match cf.field {
                                1 => primary_mm = cf.varint.map(wooting_raw_to_mm),
                                2 => secondary_mm = cf.varint.map(wooting_raw_to_mm),
                                3 => continuous = cf.varint.map(|v| v != 0),
                                _ => {}
                            }
                        }
                    }
                }
                3 => enabled = gf.varint.map(|v| v != 0),
                _ => {}
            }
        }
        if !keys.is_empty() {
            groups.push(WootingRapidTriggerGroupInfo {
                key_labels: keys.into_iter().map(wooting_matrix_label).collect(),
                enabled,
                primary_sensitivity_mm: primary_mm,
                secondary_sensitivity_mm: secondary_mm,
                continuous_mode: continuous,
            });
        }
    }
    Some(groups)
}

/// Decodes `GetAkcProfile (0x34)` Advanced Keys (`SOCD`, `Rappy Snappy`, `DKS`, `Mod Tap`, `Toggle Key`).
pub fn decode_wooting_akc_profile(raw: &[u8]) -> Option<(Vec<WootingAkcSummary>, usize)> {
    let body = reply_blob_body(raw, CMD_GET_AKC_PROFILE, WootingProtocol::Arm)?;
    if body.is_empty() {
        return Some((Vec::new(), 0));
    }
    let outer_fields = decode_proto_fields(body);
    let total_items = outer_fields
        .iter()
        .find(|f| f.field == 3)
        .and_then(|f| f.varint)
        .map(|v| v as usize)
        .unwrap_or(0);

    let mut summaries = Vec::new();
    for outer in outer_fields {
        if outer.field != 1 {
            continue;
        }
        let Some(entry_bytes) = outer.bytes else {
            continue;
        };
        let entry = decode_proto_fields(entry_bytes);
        let key_idx = entry
            .iter()
            .find(|f| f.field == 8)
            .and_then(|f| f.varint)
            .map(|v| v as u16);
        let layer = entry
            .iter()
            .find(|f| f.field == 9)
            .and_then(|f| f.varint)
            .unwrap_or(0) as u8;
        let primary_key = key_idx
            .map(wooting_matrix_label)
            .unwrap_or_else(|| "?".to_string());

        let payload = entry
            .iter()
            .find(|f| (1..=5).contains(&f.field) && f.wire == 2);
        let (kind, detail) = match payload {
            Some(p) if p.field == 1 => ("DKS", format!("{primary_key} · Dynamic Keystroke")),
            Some(p) if p.field == 2 => ("Mod Tap", format!("{primary_key} · Tap / Hold")),
            Some(p) if p.field == 3 => ("Toggle Key", format!("{primary_key} · Toggle Key")),
            Some(p) if p.field == 4 => {
                let sub = decode_proto_fields(p.bytes.unwrap_or_default());
                let sec = sub
                    .iter()
                    .find(|f| f.field == 1)
                    .and_then(|f| f.varint)
                    .map(|v| wooting_matrix_label(v as u16))
                    .unwrap_or_else(|| "?".to_string());
                ("Rappy Snappy", format!("{primary_key} + {sec}"))
            }
            Some(p) if p.field == 5 => {
                let sub = decode_proto_fields(p.bytes.unwrap_or_default());
                let sec = sub
                    .iter()
                    .find(|f| f.field == 1)
                    .and_then(|f| f.varint)
                    .map(|v| wooting_matrix_label(v as u16))
                    .unwrap_or_else(|| "?".to_string());
                let mode = sub
                    .iter()
                    .find(|f| f.field == 2)
                    .and_then(|f| f.varint)
                    .map(|m| match m {
                        0 => "Both",
                        1 => "Neither",
                        2 => "Primary Priority",
                        3 => "Secondary Priority",
                        4 => "Last Input Priority",
                        _ => "Custom",
                    })
                    .unwrap_or("Active");
                ("SOCD", format!("{primary_key} + {sec} ({mode})"))
            }
            _ => ("Advanced Key", "Custom binding".to_string()),
        };
        summaries.push(WootingAkcSummary {
            kind: kind.to_string(),
            primary_key,
            layer,
            detail,
        });
    }
    let total = total_items.max(summaries.len());
    Some((summaries, total))
}

/// Decodes `GetSettings (0x33)` (`GlobalSettings`: RGB sleep, Num Lock, debounce filter, device name).
pub fn decode_wooting_global_settings(raw: &[u8]) -> Option<WootingGlobalSettings> {
    let body = reply_blob_body(raw, CMD_GET_SETTINGS, WootingProtocol::Arm)?;
    if body.is_empty() {
        return None;
    }
    let outer = decode_proto_fields(body);
    let mut dim_min = None;
    let mut off_min = None;
    let mut turn_off = None;
    if let Some(sleep_bytes) = outer
        .iter()
        .find(|f| f.field == 1 && f.wire == 2)
        .and_then(|f| f.bytes)
    {
        for sf in decode_proto_fields(sleep_bytes) {
            match sf.field {
                1 => dim_min = sf.varint.map(|v| v as u32),
                2 => off_min = sf.varint.map(|v| v as u32),
                3 => turn_off = sf.varint.map(|v| v != 0),
                _ => {}
            }
        }
    }
    let invert_num_lock = outer
        .iter()
        .find(|f| f.field == 2)
        .and_then(|f| f.varint)
        .map(|v| v != 0);
    let debounce_filter = outer
        .iter()
        .find(|f| f.field == 3)
        .and_then(|f| f.varint)
        .map(|v| v != 0);
    let device_name = outer
        .iter()
        .find(|f| f.field == 4 && f.wire == 2)
        .and_then(|f| f.bytes)
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Some(WootingGlobalSettings {
        device_name,
        rgb_dim_minutes: dim_min,
        rgb_off_minutes: off_min,
        rgb_turn_off: turn_off,
        invert_num_lock,
        debounce_filter,
    })
}

/// Decodes `GetRgbProfileCore (0x32)` brightness & active effect layer names.
pub fn decode_wooting_rgb_summary(
    raw: &[u8],
    protocol: WootingProtocol,
) -> Option<WootingRgbSummary> {
    let body = reply_blob_body(raw, CMD_GET_RGB_PROFILE_CORE, protocol)?;
    if body.is_empty() {
        return None;
    }
    let mut brightness_raw = None;
    let mut effect_layers = Vec::new();
    for outer in decode_proto_fields(body) {
        if outer.field == 1 {
            if let Some(base_b) = outer.bytes {
                brightness_raw = decode_proto_fields(base_b)
                    .into_iter()
                    .find(|f| f.field == 1)
                    .and_then(|f| f.varint)
                    .map(|v| v.min(255) as u8);
            }
        } else if outer.field == 4 {
            if let Some(v2_b) = outer.bytes {
                for layer in decode_proto_fields(v2_b) {
                    if layer.field != 1 {
                        continue;
                    }
                    if let Some(lb) = layer.bytes {
                        for eff in decode_proto_fields(lb) {
                            let label = match eff.field {
                                1 => "Wave",
                                2 => "Scan",
                                3 => "Breath",
                                4 => "Cycle",
                                5 => "Key Ripple",
                                6 => "Key Trail",
                                7 => "Analog Touch",
                                8 => "Analog Jelly",
                                9 => "Analog Bomb / Static",
                                10 => "Analog Mixing",
                                11 => "Rapid Trigger",
                                12 => "Snake",
                                13 => "Heatmap",
                                14 => "Noise",
                                15 => "Spiral",
                                _ => continue,
                            };
                            effect_layers.push(label.to_string());
                        }
                    }
                }
            }
        }
    }
    let brightness_pct = brightness_raw.map(|b| ((u16::from(b) * 100 + 127) / 255) as u8);
    Some(WootingRgbSummary {
        brightness_raw,
        brightness_pct,
        effect_layers,
    })
}

/// Patches `KeyboardProfile` (`0x27` body) base actuation & rapid-trigger fields while preserving
/// every other protobuf field byte-for-byte.
pub fn patch_wooting_keyboard_base(
    complete_body: &[u8],
    patch: &WootingAnalogBasePatch,
) -> Result<Vec<u8>> {
    let base_bytes = decode_proto_fields(complete_body)
        .into_iter()
        .find(|f| f.field == 1 && f.wire == 2)
        .and_then(|f| f.bytes)
        .ok_or_else(|| PolledError::Device {
            device: "Wooting".to_string(),
            message: "Incomplete Wooting keyboard profile protobuf (missing base field 1)"
                .to_string(),
        })?;
    let existing_fields = decode_proto_fields(base_bytes);
    let existing_f1 = existing_fields
        .iter()
        .find(|f| f.field == 1)
        .and_then(|f| f.varint);
    let existing_f4 = existing_fields
        .iter()
        .find(|f| f.field == 4)
        .and_then(|f| f.varint);
    let existing_f8 = existing_fields
        .iter()
        .find(|f| f.field == 8)
        .and_then(|f| f.varint);
    // Match the board's existing distance scale (`>= 16384` Q12 vs `<= 255` 8-bit).
    let use_q12 = existing_f1.or(existing_f4).is_none_or(|v| v >= 16_384);
    let encode_mm = |mm: f32| {
        if use_q12 {
            wooting_mm_to_raw(mm)
        } else {
            wooting_mm_to_raw_u8(mm)
        }
    };

    let mut edited = base_bytes.to_vec();
    if let Some(mm) = patch.actuation_mm {
        edited = replace_proto_varint(&edited, 1, encode_mm(mm));
    }
    if let Some(rt_on) = patch.rapid_trigger_enabled {
        edited = replace_proto_varint(&edited, 3, u64::from(rt_on));
    }
    if let Some(rt_mm) = patch.rapid_trigger_sensitivity_mm {
        let encoded_rt = encode_mm(rt_mm);
        edited = replace_proto_varint(&edited, 4, encoded_rt);
        // Keep secondary RT sensitivity synced only when it previously matched primary.
        if existing_f8.is_some() && existing_f8 == existing_f4 {
            edited = replace_proto_varint(&edited, 8, encoded_rt);
        }
    }
    Ok(replace_proto_bytes(complete_body, 1, &edited))
}

/// Patches `GlobalSettings` (`0x33` body) while preserving every other protobuf field byte-for-byte.
pub fn patch_wooting_global_settings(
    complete_body: &[u8],
    patch: &WootingGlobalSettingsPatch,
) -> Result<Vec<u8>> {
    let mut result = complete_body.to_vec();
    if patch.rgb_dim_minutes.is_some()
        || patch.rgb_off_minutes.is_some()
        || patch.rgb_turn_off.is_some()
    {
        let mut sleep = decode_proto_fields(&result)
            .into_iter()
            .find(|f| f.field == 1 && f.wire == 2)
            .and_then(|f| f.bytes)
            .unwrap_or_default()
            .to_vec();
        if let Some(dim) = patch.rgb_dim_minutes {
            sleep = replace_proto_varint(&sleep, 1, u64::from(dim));
        }
        if let Some(off) = patch.rgb_off_minutes {
            sleep = replace_proto_varint(&sleep, 2, u64::from(off));
        }
        if let Some(turn_off) = patch.rgb_turn_off {
            sleep = replace_proto_varint(&sleep, 3, u64::from(turn_off));
        }
        result = replace_proto_bytes(&result, 1, &sleep);
    }
    if let Some(inv) = patch.invert_num_lock {
        result = replace_proto_varint(&result, 2, u64::from(inv));
    }
    if let Some(db) = patch.debounce_filter {
        result = replace_proto_varint(&result, 3, u64::from(db));
    }
    if result.len() > 26 {
        return Err(PolledError::Device {
            device: "Wooting".to_string(),
            message: "Patched GlobalSettings exceeds 32-byte ARM output report".to_string(),
        });
    }
    Ok(result)
}

/// Frames an ARM variable-length output report (`[report_id, 0xD1, 0xDA, cmd_id, flags, len_lo, len_hi, payload...]`).
pub fn encode_wooting_arm_output_report(cmd_id: u8, flags: u8, payload: &[u8]) -> Result<Vec<u8>> {
    const LADDER: &[(u8, usize)] = &[(1, 32), (2, 62), (3, 254), (4, 510)];
    let &(report_id, body_len) = LADDER
        .iter()
        .find(|&&(_, sz)| payload.len() + 6 <= sz)
        .ok_or_else(|| PolledError::Device {
            device: "Wooting".to_string(),
            message: "Wooting ARM output payload exceeds supported report size".to_string(),
        })?;
    let mut pkt = vec![0u8; 1 + body_len];
    pkt[0] = report_id;
    pkt[1] = WOOTING_MAGIC_ARM;
    pkt[2] = WOOTING_MAGIC_1;
    pkt[3] = cmd_id;
    pkt[4] = flags;
    pkt[5] = (payload.len() & 0xFF) as u8;
    pkt[6] = ((payload.len() >> 8) & 0xFF) as u8;
    pkt[7..7 + payload.len()].copy_from_slice(payload);
    Ok(pkt)
}

/// Decodes one live analog input frame from Wooting's V1 `0xFF54` interface (48B, 3-byte `[hi, usage, val_u8]` tuples).
pub fn decode_wooting_analog_frame_v1(raw: &[u8]) -> Vec<WootingAnalogKeyReading> {
    let mut keys = Vec::new();
    for chunk in raw.as_chunks::<3>().0 {
        let usage = (u16::from(chunk[0]) << 8) | u16::from(chunk[1]);
        let val = chunk[2];
        if usage != 0 && val != 0 {
            let label = if chunk[0] == 0 {
                hid_usage_label(chunk[1])
            } else {
                format!("Key 0x{usage:04X}")
            };
            keys.push(WootingAnalogKeyReading {
                stable_id: usage,
                usage,
                matrix_index: None,
                key_label: label,
                raw_value: u16::from(val),
                raw_max: 255,
                travel_mm: (f32::from(val) / 255.0) * (WOOTING_MAX_TRAVEL_MM as f32),
                actuated: None,
            });
        }
    }
    keys.sort_by_key(|k| k.stable_id);
    keys
}

/// Decodes one live analog input frame from Wooting's V2 `0xFF53` interface (64B, 4-byte
/// `[matrix_pos, key, packed, value_hi]` tuples; official `WootingKb/wooting-analog-sdk` V2 format).
///
/// Each 4-byte entry provides:
/// - `matrix_pos`: `(row << 5) | col`
/// - `key`: 8-bit keycode within `key_namespace = (packed >> 2) & 0x0F`
/// - `actuated`: `packed & 0x01` (live firmware Rapid Trigger / actuation flag!)
/// - `raw_10bit`: `(value_hi << 2) | ((packed >> 6) & 0x03)` (`0..=1023` over `0.00..=4.00 mm`)
pub fn decode_wooting_analog_frame_v2(raw: &[u8]) -> Vec<WootingAnalogKeyReading> {
    let mut keys = Vec::new();
    for chunk in raw.as_chunks::<4>().0 {
        let matrix_pos = chunk[0];
        let key_lo = chunk[1];
        let packed = chunk[2];
        let value_hi = chunk[3];

        let actuated = (packed & 0x01) != 0;
        let key_namespace = (packed >> 2) & 0x0F;
        let value_part = (packed >> 6) & 0x03;
        let raw_10bit = (u16::from(value_hi) << 2) | u16::from(value_part);

        if raw_10bit == 0 {
            continue;
        }
        let usage = (u16::from(key_namespace) << 8) | u16::from(key_lo);
        let matrix_index = u16::from(matrix_pos);
        let key_label = if key_namespace == 0 && key_lo != 0 {
            hid_usage_label_known(key_lo).unwrap_or_else(|| wooting_matrix_label(matrix_index))
        } else {
            wooting_matrix_label(matrix_index)
        };
        keys.push(WootingAnalogKeyReading {
            stable_id: 0x8000 | matrix_index,
            usage,
            matrix_index: Some(matrix_index),
            key_label,
            raw_value: raw_10bit,
            raw_max: 1023,
            travel_mm: (f32::from(raw_10bit) / 1023.0).min(1.0) * (WOOTING_MAX_TRAVEL_MM as f32),
            actuated: Some(actuated),
        });
    }
    keys.sort_by_key(|k| k.stable_id);
    keys
}

/// Decodes an analog frame based on the opened HID usage page (`0xFF53` V2 4-byte 10-bit vs `0xFF54` V1 3-byte 8-bit).
pub fn decode_wooting_analog_frame(raw: &[u8], usage_page: u16) -> Vec<WootingAnalogKeyReading> {
    if usage_page == 0xFF53 {
        decode_wooting_analog_frame_v2(raw)
    } else {
        decode_wooting_analog_frame_v1(raw)
    }
}

/// Maps a Wooting firmware matrix index `(row << 5) | col` to a readable key name (60%/TKL main block)
/// or `(r,c)` fallback when outside the standard 60% block.
pub fn wooting_matrix_label(matrix_index: u16) -> String {
    let col = (matrix_index & 0x1F) as u8;
    let row = ((matrix_index >> 5) & 0x07) as u8;
    let name = match (row, col) {
        (0, 0) => "Esc",
        (1, 0) => "Esc/`",
        (1, 1) => "1",
        (1, 2) => "2",
        (1, 3) => "3",
        (1, 4) => "4",
        (1, 5) => "5",
        (1, 6) => "6",
        (1, 7) => "7",
        (1, 8) => "8",
        (1, 9) => "9",
        (1, 10) => "0",
        (1, 11) => "-",
        (1, 12) => "=",
        (1, 13) => "Backspace",
        (2, 0) => "Tab",
        (2, 1) => "Q",
        (2, 2) => "W",
        (2, 3) => "E",
        (2, 4) => "R",
        (2, 5) => "T",
        (2, 6) => "Y",
        (2, 7) => "U",
        (2, 8) => "I",
        (2, 9) => "O",
        (2, 10) => "P",
        (2, 11) => "[",
        (2, 12) => "]",
        (2, 13) => "\\",
        (3, 0) => "Caps",
        (3, 1) => "A",
        (3, 2) => "S",
        (3, 3) => "D",
        (3, 4) => "F",
        (3, 5) => "G",
        (3, 6) => "H",
        (3, 7) => "J",
        (3, 8) => "K",
        (3, 9) => "L",
        (3, 10) => ";",
        (3, 11) => "'",
        (3, 13) => "Enter",
        (4, 0) => "LShift",
        (4, 2) => "Z",
        (4, 3) => "X",
        (4, 4) => "C",
        (4, 5) => "V",
        (4, 6) => "B",
        (4, 7) => "N",
        (4, 8) => "M",
        (4, 9) => ",",
        (4, 10) => ".",
        (4, 11) => "/",
        (4, 13) => "RShift",
        (5, 0) => "LCtrl",
        (5, 1) => "LWin",
        (5, 2) => "LAlt",
        (5, 6) => "Space",
        (5, 10) => "RAlt",
        (5, 11) => "RCtrl",
        (5, 12) => "Menu",
        (5, 13) => "RCtrl",
        (5, 14) => "Fn",
        _ => return format!("R{row}C{col}"),
    };
    name.to_string()
}

/// Maps a standard USB HID Keyboard Usage ID (`0x04..=0xE7`) to a crisp label, returning `None`
/// for non-standard / vendor usages so V2 (`0xFF53`) can fall back to the physical matrix label.
fn hid_usage_label_known(usage: u8) -> Option<String> {
    let s = match usage {
        0x04..=0x1D => {
            let ch = (b'A' + (usage - 0x04)) as char;
            return Some(ch.to_string());
        }
        0x1E..=0x26 => {
            let ch = (b'1' + (usage - 0x1E)) as char;
            return Some(ch.to_string());
        }
        0x27 => "0",
        0x28 => "Enter",
        0x29 => "Esc",
        0x2A => "Backspace",
        0x2B => "Tab",
        0x2C => "Space",
        0x2D => "-",
        0x2E => "=",
        0x2F => "[",
        0x30 => "]",
        0x31 => "\\",
        0x33 => ";",
        0x34 => "'",
        0x35 => "`",
        0x36 => ",",
        0x37 => ".",
        0x38 => "/",
        0x39 => "Caps",
        0x3A..=0x45 => return Some(format!("F{}", usage - 0x3A + 1)),
        0x46 => "PrtSc",
        0x47 => "ScrLk",
        0x48 => "Pause",
        0x49 => "Ins",
        0x4A => "Home",
        0x4B => "PgUp",
        0x4C => "Del",
        0x4D => "End",
        0x4E => "PgDn",
        0x4F => "Right",
        0x50 => "Left",
        0x51 => "Down",
        0x52 => "Up",
        0x65 => "Menu",
        0xE0 => "LCtrl",
        0xE1 => "LShift",
        0xE2 => "LAlt",
        0xE3 => "LWin",
        0xE4 => "RCtrl",
        0xE5 => "RShift",
        0xE6 => "RAlt",
        0xE7 => "RWin",
        _ => return None,
    };
    Some(s.to_string())
}

/// Maps a standard USB HID Keyboard Usage ID (`0x04..=0xE7`) to a crisp label for the live analog stream.
pub fn hid_usage_label(usage: u8) -> String {
    hid_usage_label_known(usage).unwrap_or_else(|| format!("Key 0x{usage:02X}"))
}

/// Live handle to a connected Wooting keyboard command interface (`0x1337` Standard or `0xFF55` ARM).
pub struct WootingDevice {
    hid: HidDevice,
    id: String,
    name: String,
    vid_pid: String,
    protocol: WootingProtocol,
    chip_type: String,
    switch_tech: String,
}

impl WootingDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.name.clone(),
            message: message.into(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn protocol(&self) -> WootingProtocol {
        self.protocol
    }

    /// Quiet-drains any buffered notifications/duplicate replies, sends an 8-byte feature report,
    /// and reads back the matching response echoing `cmd_id`.
    fn exchange(&self, pkt: [u8; 8]) -> Result<Vec<u8>> {
        let mut drain = [0u8; 512];
        while self
            .hid
            .read_timeout(&mut drain, HID_DRAIN_QUIET_MS)
            .is_ok_and(|n| n > 0)
        {}

        let cmd_id = pkt[3];
        self.hid.send_feature_report(&pkt).map_err(|e| {
            self.dev_err(format!(
                "Wooting feature report 0x{cmd_id:02x} failed (close Wootility if open): {e}"
            ))
        })?;

        let mut buf = vec![0u8; 512];
        for _ in 0..HID_RESPONSE_RETRIES {
            let n = self
                .hid
                .read_timeout(&mut buf, HID_READ_TIMEOUT_MS)
                .map_err(|e| self.dev_err(format!("Wooting HID read failed: {e}")))?;
            if n == 0 {
                break;
            }
            if let Some(norm) = normalize_wooting_reply(&buf[..n]) {
                if norm[2] == cmd_id {
                    return Ok(buf[..n].to_vec());
                }
            }
        }

        // Fallback: some drivers deliver the reply via get_feature_report.
        let mut feat = vec![0u8; 65];
        feat[0] = self.protocol.report_id();
        if let Ok(n) = self.hid.get_feature_report(&mut feat) {
            if let Some(norm) = normalize_wooting_reply(&feat[..n]) {
                if norm[2] == cmd_id {
                    return Ok(feat[..n].to_vec());
                }
            }
        }

        Err(self.dev_err(format!(
            "Wooting keyboard did not reply to command 0x{cmd_id:02x}"
        )))
    }

    fn send_cmd(&self, cmd_id: u8) -> Result<Vec<u8>> {
        self.exchange(build_wooting_feature_cmd(self.protocol, cmd_id, 0, 0, 0, 0))
    }

    fn send_profile_cmd(&self, cmd_id: u8, slot: u8) -> Result<Vec<u8>> {
        self.exchange(build_wooting_profile_cmd(self.protocol, cmd_id, slot))
    }

    /// Reads the runtime-active 0-based onboard profile index (`0..=7`).
    pub fn read_active_profile(&self) -> Result<u8> {
        let raw = self.send_cmd(CMD_GET_CURRENT_PROFILE_INDEX)?;
        decode_wooting_profile_index(&raw, self.protocol)
            .map(|(active, _)| active)
            .ok_or_else(|| self.dev_err("Could not decode active Wooting profile index"))
    }

    /// Collects the full verified live status snapshot from the keyboard (100% read-only!).
    pub fn read_full_status(&self) -> WootingKeyboardInfo {
        let firmware_version = self
            .send_cmd(CMD_GET_VERSION)
            .ok()
            .and_then(|r| decode_wooting_version(&r));
        let layout = self
            .send_cmd(CMD_GET_DEVICE_CONFIG)
            .ok()
            .and_then(|r| decode_wooting_layout(&r));
        let (active_profile, flash_default_profile) = self
            .send_cmd(CMD_GET_CURRENT_PROFILE_INDEX)
            .ok()
            .and_then(|r| decode_wooting_profile_index(&r, self.protocol))
            .map(|(a, d)| (Some(a), Some(d)))
            .unwrap_or((None, None));

        // Probe real onboard profile names (`GetProfileMetadata 0x37`).
        let mut profiles = Vec::new();
        let max_slots = match self.protocol {
            WootingProtocol::Standard => self
                .send_cmd(CMD_GET_DIGITAL_PROFILES_COUNT)
                .ok()
                .and_then(|r| {
                    reply_ok_for(&r, CMD_GET_DIGITAL_PROFILES_COUNT).and_then(|n| n.get(4).copied())
                })
                .filter(|&c| (1..=8).contains(&c))
                .unwrap_or(4),
            WootingProtocol::Arm => 8,
        };
        for slot in 0..max_slots {
            match self
                .send_profile_cmd(CMD_GET_PROFILE_METADATA, slot)
                .ok()
                .and_then(|r| decode_wooting_profile_metadata(&r))
            {
                Some(Some(name)) => {
                    let display = if name.is_empty() {
                        format!("Profile {}", slot + 1)
                    } else {
                        name
                    };
                    profiles.push(WootingProfileSlot {
                        index: slot,
                        name: display,
                        is_active: active_profile == Some(slot),
                    });
                }
                Some(None) => {
                    if self.protocol == WootingProtocol::Arm {
                        break;
                    }
                }
                None => {
                    if self.protocol == WootingProtocol::Arm {
                        break;
                    }
                }
            }
        }
        if profiles.is_empty() {
            for slot in 0..4u8 {
                profiles.push(WootingProfileSlot {
                    index: slot,
                    name: format!("Profile {}", slot + 1),
                    is_active: active_profile == Some(slot),
                });
            }
        }

        let target_slot = active_profile.unwrap_or(0);

        let analog_profile = self
            .send_profile_cmd(CMD_GET_KEYBOARD_PROFILE, target_slot)
            .ok()
            .and_then(|r| decode_wooting_keyboard_profile(&r, self.protocol));

        let per_key_actuation = self
            .send_profile_cmd(CMD_GET_ACTUATION_PROFILE, target_slot)
            .ok()
            .and_then(|r| decode_wooting_per_key_actuation(&r, self.protocol))
            .unwrap_or_default();

        let per_key_rapid_trigger = if self.protocol == WootingProtocol::Arm {
            self.send_profile_cmd(CMD_GET_FULL_RAPID_TRIGGER_PROFILE, target_slot)
                .ok()
                .and_then(|r| decode_wooting_full_rt_profile(&r))
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let mut advanced_keys = Vec::new();
        if self.protocol == WootingProtocol::Arm {
            if let Ok(first_raw) = self.send_profile_cmd(CMD_GET_AKC_PROFILE, target_slot) {
                if let Some((mut items, total)) = decode_wooting_akc_profile(&first_raw) {
                    while items.len() < total && items.len() < 40 {
                        let start = items.len() as u8;
                        let pkt = build_wooting_feature_cmd(
                            WootingProtocol::Arm,
                            CMD_GET_AKC_PROFILE,
                            target_slot,
                            0,
                            start,
                            0,
                        );
                        let Ok(chunk_raw) = self.exchange(pkt) else {
                            break;
                        };
                        let Some((more, _)) = decode_wooting_akc_profile(&chunk_raw) else {
                            break;
                        };
                        if more.is_empty() {
                            break;
                        }
                        items.extend(more);
                    }
                    advanced_keys = items;
                }
            }
        }

        let global_settings = if self.protocol == WootingProtocol::Arm {
            self.send_cmd(CMD_GET_SETTINGS)
                .ok()
                .and_then(|r| decode_wooting_global_settings(&r))
        } else {
            None
        };

        let rgb = self
            .send_profile_cmd(CMD_GET_RGB_PROFILE_CORE, target_slot)
            .ok()
            .and_then(|r| decode_wooting_rgb_summary(&r, self.protocol));

        let key_count = self
            .send_cmd(CMD_GET_NUMBER_OF_KEYS)
            .ok()
            .and_then(|r| reply_ok_for(&r, CMD_GET_NUMBER_OF_KEYS).and_then(|n| n.get(4).copied()));
        let rgb_profile_count = self.send_cmd(CMD_GET_RGB_PROFILE_COUNT).ok().and_then(|r| {
            reply_ok_for(&r, CMD_GET_RGB_PROFILE_COUNT).and_then(|n| n.get(4).copied())
        });
        let analog_profile_count =
            self.send_cmd(CMD_GET_ANALOG_PROFILES_COUNT)
                .ok()
                .and_then(|r| {
                    reply_ok_for(&r, CMD_GET_ANALOG_PROFILES_COUNT).and_then(|n| n.get(4).copied())
                });
        let flash_chip_connected = self
            .send_cmd(CMD_IS_FLASH_CHIP_CONNECTED)
            .ok()
            .and_then(|r| {
                reply_ok_for(&r, CMD_IS_FLASH_CHIP_CONNECTED)
                    .and_then(|n| n.get(4).map(|&b| b != 0))
            });
        let (flash_used_pages, flash_total_pages) = self
            .send_cmd(CMD_GET_FLASH_STATS)
            .ok()
            .and_then(|r| {
                let n = reply_ok_for(&r, CMD_GET_FLASH_STATS)?;
                if n.len() >= 8 {
                    let used = u16::from_le_bytes([n[4], n[5]]);
                    let total = u16::from_le_bytes([n[6], n[7]]);
                    Some((Some(used), Some(total)))
                } else {
                    None
                }
            })
            .unwrap_or((None, None));

        WootingKeyboardInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            vid_pid: self.vid_pid.clone(),
            protocol: self.protocol,
            chip_type: self.chip_type.clone(),
            switch_tech: self.switch_tech.clone(),
            firmware_version,
            layout,
            active_profile,
            flash_default_profile,
            profiles,
            analog_profile,
            per_key_actuation,
            per_key_rapid_trigger,
            advanced_keys,
            global_settings,
            rgb,
            diagnostics: WootingDiagnostics {
                key_count,
                rgb_profile_count,
                analog_profile_count,
                flash_chip_connected,
                flash_used_pages,
                flash_total_pages,
            },
            supports_arm_writes: self.protocol == WootingProtocol::Arm,
        }
    }

    /// Switches the Wooting keyboard to `profile_idx` (`0..=7`) strictly in volatile RAM
    /// (`WootDevInit 0x21 -> ActivateProfile 0x17 -> 100ms -> ReloadProfile 0x26 -> 100ms -> Verify 0x0B`),
    /// skipping writes if already active. Returns `Ok(true)` if switched, `Ok(false)` if unchanged.
    pub fn switch_profile(&self, profile_idx: u8) -> Result<bool> {
        if profile_idx > WOOTING_MAX_PROFILE_INDEX {
            return Err(PolledError::UnsupportedSetting {
                device: self.name.clone(),
                detail: format!(
                    "Wooting profile index {profile_idx} out of supported range 0..={WOOTING_MAX_PROFILE_INDEX}"
                ),
            });
        }

        if let Ok(current) = self.read_active_profile() {
            if current == profile_idx {
                return Ok(false);
            }
        }

        let _ = self.send_cmd(CMD_WOOT_DEV_INIT)?;
        let act_reply = self.send_profile_cmd(CMD_ACTIVATE_PROFILE, profile_idx)?;
        if reply_ok_for(&act_reply, CMD_ACTIVATE_PROFILE).is_none() {
            return Err(self.dev_err(format!(
                "ActivateProfile was rejected for slot {}",
                profile_idx + 1
            )));
        }
        thread::sleep(Duration::from_millis(WOOTING_SWITCH_SETTLE_MS));

        let rel_reply = self.send_profile_cmd(CMD_RELOAD_PROFILE, profile_idx)?;
        if reply_ok_for(&rel_reply, CMD_RELOAD_PROFILE).is_none() {
            return Err(self.dev_err(format!(
                "ReloadProfile was rejected for slot {}",
                profile_idx + 1
            )));
        }
        thread::sleep(Duration::from_millis(WOOTING_SWITCH_SETTLE_MS));

        let after = self.read_active_profile()?;
        if after != profile_idx {
            return Err(self.dev_err(format!(
                "Wooting profile switch did not stick: requested Profile {}, board reports Profile {} (ensure that onboard slot exists)",
                profile_idx + 1,
                after + 1
            )));
        }
        Ok(true)
    }

    /// Writes base actuation & rapid-trigger defaults to the active profile on ARM boards
    /// (`OUT_CMD_SET_KEYBOARD_BASE 0x11`, save bit set), preserving all unknown protobuf fields
    /// and verifying with a mandatory read-back of `0x27`.
    pub fn write_analog_defaults(&self, patch: &WootingAnalogBasePatch) -> Result<()> {
        if self.protocol != WootingProtocol::Arm {
            return Err(PolledError::UnsupportedSetting {
                device: self.name.clone(),
                detail: "Base analog writes are only verified on ARM Wooting keyboards (60HE+, 80HE, 60HE ARM, Two HE ARM, UwU)".to_string(),
            });
        }
        let slot = self.read_active_profile().unwrap_or(0);
        let original_raw = self.send_profile_cmd(CMD_GET_KEYBOARD_PROFILE, slot)?;
        let body = reply_blob_body(
            &original_raw,
            CMD_GET_KEYBOARD_PROFILE,
            WootingProtocol::Arm,
        )
        .ok_or_else(|| {
            self.dev_err("Could not read complete keyboard profile (0x27) before writing")
        })?;
        let edited = patch_wooting_keyboard_base(body, patch)?;
        // flags: bit 0 = save (1), bits 1..5 = profile_index, bits 6..7 = namespace (0 = onboard)
        let flags = 1u8 | ((slot & 0x1F) << 1);
        let pkt = encode_wooting_arm_output_report(OUT_CMD_SET_KEYBOARD_BASE, flags, &edited)?;
        self.hid
            .write(&pkt)
            .map_err(|e| self.dev_err(format!("Wooting output report 0x11 failed: {e}")))?;
        thread::sleep(Duration::from_millis(60));

        let verify_raw = self.send_profile_cmd(CMD_GET_KEYBOARD_PROFILE, slot)?;
        let actual = decode_wooting_keyboard_profile(&verify_raw, WootingProtocol::Arm)
            .ok_or_else(|| {
                self.dev_err("Read-back verification failed after writing analog defaults")
            })?;

        if let Some(want_mm) = patch.actuation_mm {
            let got = actual.actuation_mm.unwrap_or(-1.0);
            if (got - want_mm).abs() > 0.06 {
                return Err(self.dev_err(format!(
                    "Actuation read-back mismatch: wanted {want_mm:.2} mm, board reported {got:.2} mm"
                )));
            }
        }
        if let Some(want_rt) = patch.rapid_trigger_enabled {
            if actual.rapid_trigger_enabled != Some(want_rt) {
                return Err(self.dev_err("Rapid Trigger enable state did not match on read-back"));
            }
        }
        if let Some(want_sens) = patch.rapid_trigger_sensitivity_mm {
            let got = actual.rapid_trigger_sensitivity_mm.unwrap_or(-1.0);
            if (got - want_sens).abs() > 0.06 {
                return Err(self.dev_err(format!(
                    "Rapid Trigger sensitivity mismatch: wanted {want_sens:.2} mm, board reported {got:.2} mm"
                )));
            }
        }
        Ok(())
    }

    /// Writes device-wide `GlobalSettings` (`OUT_CMD_SET_GLOBAL_SETTINGS 0x17`) on ARM boards,
    /// preserving all unknown fields and verifying with a mandatory read-back of `0x33`.
    pub fn write_global_settings(&self, patch: &WootingGlobalSettingsPatch) -> Result<()> {
        if self.protocol != WootingProtocol::Arm {
            return Err(PolledError::UnsupportedSetting {
                device: self.name.clone(),
                detail: "Global settings writes are only verified on ARM Wooting keyboards"
                    .to_string(),
            });
        }
        let original_raw = self.send_cmd(CMD_GET_SETTINGS)?;
        let body = reply_blob_body(&original_raw, CMD_GET_SETTINGS, WootingProtocol::Arm)
            .ok_or_else(|| self.dev_err("Could not read GlobalSettings (0x33) before writing"))?;
        let edited = patch_wooting_global_settings(body, patch)?;
        let pkt = encode_wooting_arm_output_report(OUT_CMD_SET_GLOBAL_SETTINGS, 0, &edited)?;
        self.hid
            .write(&pkt)
            .map_err(|e| self.dev_err(format!("Wooting output report 0x17 failed: {e}")))?;
        thread::sleep(Duration::from_millis(60));

        let verify_raw = self.send_cmd(CMD_GET_SETTINGS)?;
        let actual = decode_wooting_global_settings(&verify_raw).ok_or_else(|| {
            self.dev_err("Read-back verification failed after writing GlobalSettings")
        })?;

        if let Some(want_db) = patch.debounce_filter {
            if actual.debounce_filter != Some(want_db) {
                return Err(self.dev_err("Debounce filter setting did not match on read-back"));
            }
        }
        if let Some(want_inv) = patch.invert_num_lock {
            if actual.invert_num_lock != Some(want_inv) {
                return Err(self.dev_err("Invert Num Lock setting did not match on read-back"));
            }
        }
        if let Some(want_dim) = patch.rgb_dim_minutes {
            if actual.rgb_dim_minutes != Some(want_dim) {
                return Err(self.dev_err("RGB dim timeout did not match on read-back"));
            }
        }
        if let Some(want_off) = patch.rgb_off_minutes {
            if actual.rgb_off_minutes != Some(want_off) {
                return Err(self.dev_err("RGB off timeout did not match on read-back"));
            }
        }
        Ok(())
    }
}

/// Detects all connected Wooting keyboards exposing a valid config HID interface
/// (`0xFF55` ARM or `0x1337` Standard, usage `0x01`).
pub fn detect_wooting_keyboards() -> Result<Vec<WootingDevice>> {
    let api = HidApi::new().map_err(|e| PolledError::Win32 {
        context: "HidApi::new".to_string(),
        message: e.to_string(),
    })?;

    let mut found = Vec::new();
    let mut seen_keys: HashSet<(u16, u16, String)> = HashSet::new();

    for dev in api.device_list() {
        let vid = dev.vendor_id();
        let pid = dev.product_id();
        let is_wooting =
            vid == WOOTING_VID || (vid == WOOTING_LEGACY_VID && WOOTING_LEGACY_PIDS.contains(&pid));
        if !is_wooting {
            continue;
        }
        let protocol = match dev.usage_page() {
            WOOTING_USAGE_PAGE_ARM => WootingProtocol::Arm,
            WOOTING_USAGE_PAGE_STANDARD => WootingProtocol::Standard,
            _ => continue,
        };
        if dev.usage() != WOOTING_CONFIG_USAGE && dev.usage() != 0 {
            continue;
        }
        let serial_key = dev.serial_number().unwrap_or_default().to_string();
        if !seen_keys.insert((vid, pid, serial_key)) {
            continue;
        }
        let Ok(hid) = dev.open_device(&api) else {
            continue;
        };
        let spec = resolve_wooting_model_spec(vid, pid);
        let name = resolve_wooting_model_name(vid, pid, dev.product_string());
        let chip_type = spec
            .map(|s| s.chip)
            .unwrap_or(match protocol {
                WootingProtocol::Arm => "ARM",
                WootingProtocol::Standard => "AVR",
            })
            .to_string();
        let switch_tech = spec.map(|s| s.switch_tech).unwrap_or("Analog").to_string();
        let id = format!("wooting_{vid:04x}_{pid:04x}");
        let vid_pid = format!("{vid:04x}:{pid:04x}");
        let candidate = WootingDevice {
            hid,
            id,
            name,
            vid_pid,
            protocol,
            chip_type,
            switch_tech,
        };
        // Read-only probe verifies this exact collection speaks the Wooting command protocol.
        if candidate.read_active_profile().is_ok() {
            found.push(candidate);
        }
    }

    Ok(found)
}

/// Live background reader for Wooting's real-time analog key-travel HID stream (`0xFF53` / `0xFF54`).
/// Zero keystrokes stored; stops cleanly on `stop()` or `Drop` (0% CPU when idle).
pub struct WootingAnalogStream {
    keys: Arc<Mutex<Vec<WootingAnalogKeyReading>>>,
    error: Arc<Mutex<Option<String>>>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl WootingAnalogStream {
    pub fn start<F>(on_update: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        let keys = Arc::new(Mutex::new(Vec::new()));
        let error = Arc::new(Mutex::new(None));
        let running = Arc::new(AtomicBool::new(true));

        let keys_bg = Arc::clone(&keys);
        let err_bg = Arc::clone(&error);
        let run_bg = Arc::clone(&running);

        let handle = thread::Builder::new()
            .name("polled-wooting-analog".to_string())
            .spawn(move || {
                let api = match HidApi::new() {
                    Ok(a) => a,
                    Err(e) => {
                        if let Ok(mut g) = err_bg.lock() {
                            *g = Some(format!("HID init failed: {e}"));
                        }
                        (on_update)();
                        return;
                    }
                };
                // Prefer V2 (`0xFF53`, 64B 10-bit + hardware actuation flag) over V1 (`0xFF54`, 48B 8-bit).
                let analog_entry = api
                    .device_list()
                    .find(|d| d.vendor_id() == WOOTING_VID && d.usage_page() == 0xFF53)
                    .or_else(|| {
                        api.device_list().find(|d| {
                            (d.vendor_id() == WOOTING_VID || d.vendor_id() == WOOTING_LEGACY_VID)
                                && d.usage_page() == 0xFF54
                        })
                    })
                    .and_then(|d| {
                        let page = d.usage_page();
                        d.open_device(&api).ok().map(|h| (h, page))
                    });

                let Some((dev, usage_page)) = analog_entry else {
                    if let Ok(mut g) = err_bg.lock() {
                        *g = Some(
                            "No Wooting analog stream interface (0xFF53/0xFF54) found. Make sure your Wooting keyboard is connected."
                                .to_string(),
                        );
                    }
                    (on_update)();
                    return;
                };

                let mut buf = [0u8; 64];
                let mut drain = [0u8; 64];
                while run_bg.load(Ordering::SeqCst) {
                    match dev.read_timeout(&mut buf, 16) {
                        Ok(mut n) if n > 0 => {
                            // Drain any queued HID reports so we always show the freshest frame with zero backlog lag.
                            for _ in 0..32 {
                                match dev.read_timeout(&mut drain, 0) {
                                    Ok(m) if m > 0 => {
                                        buf[..m].copy_from_slice(&drain[..m]);
                                        n = m;
                                    }
                                    _ => break,
                                }
                            }
                            let decoded = decode_wooting_analog_frame(&buf[..n], usage_page);
                            if let Ok(mut g) = keys_bg.lock() {
                                *g = decoded;
                            }
                            (on_update)();
                        }
                        Ok(_) => {}
                        Err(e) => {
                            if let Ok(mut g) = err_bg.lock() {
                                *g = Some(format!("Analog stream stopped: {e}"));
                            }
                            (on_update)();
                            break;
                        }
                    }
                }
            })
            .ok();

        Self {
            keys,
            error,
            running,
            handle,
        }
    }

    pub fn snapshot(&self) -> (Vec<WootingAnalogKeyReading>, Option<String>) {
        let keys = self.keys.lock().map(|g| g.clone()).unwrap_or_default();
        let err = self.error.lock().ok().and_then(|g| g.clone());
        (keys, err)
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for WootingAnalogStream {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_arm_and_standard_feature_packets_with_correct_slot_offsets() {
        let arm_act = build_wooting_profile_cmd(WootingProtocol::Arm, CMD_ACTIVATE_PROFILE, 2);
        assert_eq!(arm_act, [0x01, 0xD1, 0xDA, 0x17, 0x02, 0x00, 0x00, 0x00]);

        let std_act = build_wooting_profile_cmd(WootingProtocol::Standard, CMD_ACTIVATE_PROFILE, 2);
        assert_eq!(std_act, [0x00, 0xD0, 0xDA, 0x17, 0x00, 0x00, 0x00, 0x02]);

        let arm_init =
            build_wooting_feature_cmd(WootingProtocol::Arm, CMD_WOOT_DEV_INIT, 0, 0, 0, 0);
        assert_eq!(arm_init, [0x01, 0xD1, 0xDA, 0x21, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn decodes_live_60he_plus_version_layout_profile_index_and_metadata() {
        // Real 60HE+ ARM reply: v2.13.0
        let ver = [0x01, 0xD1, 0xDA, 0x01, 0x88, 0x03, 0x00, 0x02, 0x0D, 0x00];
        assert_eq!(decode_wooting_version(&ver).as_deref(), Some("2.13.0"));

        // Real 60HE+ device config reply: byte 9 (after magic) = 0x00 = ANSI
        let cfg = [0xD1, 0xDA, 0x13, 0x88, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(decode_wooting_layout(&cfg).as_deref(), Some("ANSI"));

        // ARM profile index: data[0]=0 (flash default), data[2]=2 (runtime active)
        let arm_idx = [0x01, 0xD1, 0xDA, 0x0B, 0x88, 0x00, 0x00, 0x02];
        assert_eq!(
            decode_wooting_profile_index(&arm_idx, WootingProtocol::Arm),
            Some((2, 0))
        );

        // Standard profile index: data[0]=1 (runtime active)
        let std_idx = [0x00, 0xD0, 0xDA, 0x0B, 0x88, 0x01];
        assert_eq!(
            decode_wooting_profile_index(&std_idx, WootingProtocol::Standard),
            Some((1, 1))
        );

        // GetProfileMetadata: protobuf field 1 string "Valorant"
        let meta = [
            0x01, 0xD1, 0xDA, 0x37, 0x88, 0x0A, 0x00, 0x0A, 0x08, b'V', b'a', b'l', b'o', b'r',
            b'a', b'n', b't',
        ];
        assert_eq!(
            decode_wooting_profile_metadata(&meta),
            Some(Some("Valorant".to_string()))
        );
        let empty_slot = [0x01, 0xD1, 0xDA, 0x37, 0x88, 0x00, 0x00];
        assert_eq!(decode_wooting_profile_metadata(&empty_slot), Some(None));
    }

    #[test]
    fn decodes_and_patches_live_60he_plus_keyboard_profile_and_global_settings() {
        // Real 60HE+ 0x27 reply: 0.20mm actuation (17203), digital=1, RT=1, RT sens=0.10mm (16794),
        // tachyon=0, tick_rate=2, strict=0, tachyon_mode=0
        let raw_27: [u8; 28] = [
            0xD1, 0xDA, 0x27, 0x88, 0x16, 0x00, 0x0A, 0x14, 0x08, 0xB3, 0x86, 0x01, 0x10, 0x01,
            0x18, 0x01, 0x20, 0x9A, 0x83, 0x01, 0x28, 0x00, 0x30, 0x02, 0x38, 0x00, 0x48, 0x00,
        ];
        let prof = decode_wooting_keyboard_profile(&raw_27, WootingProtocol::Arm).unwrap();
        assert!((prof.actuation_mm.unwrap() - 0.20).abs() < 0.01);
        assert_eq!(prof.digital_output_enabled, Some(true));
        assert_eq!(prof.rapid_trigger_enabled, Some(true));
        assert!((prof.rapid_trigger_sensitivity_mm.unwrap() - 0.10).abs() < 0.01);
        assert_eq!(prof.tachyon_enabled, Some(false));
        assert_eq!(prof.tick_rate, Some(2));

        // Patch actuation to 0.40mm while preserving tick_rate & digital_output_enabled!
        let patched_body = patch_wooting_keyboard_base(
            &raw_27[6..],
            &WootingAnalogBasePatch {
                actuation_mm: Some(0.40),
                rapid_trigger_enabled: Some(true),
                rapid_trigger_sensitivity_mm: Some(0.15),
            },
        )
        .unwrap();
        let mut fake_reply = vec![0xD1, 0xDA, 0x27, 0x88, patched_body.len() as u8, 0x00];
        fake_reply.extend_from_slice(&patched_body);
        let decoded_after =
            decode_wooting_keyboard_profile(&fake_reply, WootingProtocol::Arm).unwrap();
        assert!((decoded_after.actuation_mm.unwrap() - 0.40).abs() < 0.01);
        assert!((decoded_after.rapid_trigger_sensitivity_mm.unwrap() - 0.15).abs() < 0.01);
        assert_eq!(decoded_after.tick_rate, Some(2));
        assert_eq!(decoded_after.digital_output_enabled, Some(true));

        // Real 60HE+ GlobalSettings 0x33 reply: dim 5 min, off 10 min, invert_num_lock=0
        let raw_33: [u8; 14] = [
            0xD1, 0xDA, 0x33, 0x88, 0x08, 0x00, 0x0A, 0x04, 0x08, 0x05, 0x10, 0x0A, 0x10, 0x00,
        ];
        let gs = decode_wooting_global_settings(&raw_33).unwrap();
        assert_eq!(gs.rgb_dim_minutes, Some(5));
        assert_eq!(gs.rgb_off_minutes, Some(10));
        assert_eq!(gs.invert_num_lock, Some(false));

        let patched_gs = patch_wooting_global_settings(
            &raw_33[6..],
            &WootingGlobalSettingsPatch {
                debounce_filter: Some(true),
                rgb_dim_minutes: Some(15),
                ..Default::default()
            },
        )
        .unwrap();
        let mut fake_gs_reply = vec![0xD1, 0xDA, 0x33, 0x88, patched_gs.len() as u8, 0x00];
        fake_gs_reply.extend_from_slice(&patched_gs);
        let gs_after = decode_wooting_global_settings(&fake_gs_reply).unwrap();
        assert_eq!(gs_after.rgb_dim_minutes, Some(15));
        assert_eq!(gs_after.rgb_off_minutes, Some(10));
        assert_eq!(gs_after.debounce_filter, Some(true));
    }

    #[test]
    fn decodes_live_socd_akc_per_key_rt_and_analog_stream_frames() {
        // Real 60HE+ AKC 0x34 reply: SOCD A (97) + D (99), Last Input Priority (4)
        let raw_akc: [u8; 24] = [
            0xD1, 0xDA, 0x34, 0x88, 0x12, 0x00, 0x0A, 0x0C, 0x2A, 0x06, 0x08, 0x63, 0x10, 0x04,
            0x18, 0x00, 0x40, 0x61, 0x48, 0x00, 0x10, 0x00, 0x18, 0x01,
        ];
        let (combos, total) = decode_wooting_akc_profile(&raw_akc).unwrap();
        assert_eq!(total, 1);
        assert_eq!(combos.len(), 1);
        assert_eq!(combos[0].kind, "SOCD");
        assert_eq!(combos[0].primary_key, "A");
        assert!(combos[0].detail.contains("A + D"));
        assert!(combos[0].detail.contains("Last Input Priority"));

        // Real 60HE+ 0x3D per-key RT group: keys 0x20 & 0xad @ 0.20mm continuous
        let raw_rt: [u8; 21] = [
            0xD1, 0xDA, 0x3D, 0x88, 0x0F, 0x00, 0x0A, 0x0D, 0x0A, 0x03, 0x20, 0xAD, 0x01, 0x12,
            0x06, 0x08, 0xB3, 0x86, 0x01, 0x18, 0x01,
        ];
        let groups = decode_wooting_full_rt_profile(&raw_rt).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].key_labels.len(), 2);
        assert!((groups[0].primary_sensitivity_mm.unwrap() - 0.20).abs() < 0.01);
        assert_eq!(groups[0].continuous_mode, Some(true));

        // V1 (0xFF54) 3-byte analog stream frame: W (0x1A) @ 128 (~2.0mm) + A (0x04) @ 255 (4.0mm)
        let frame_v1 = [0x00, 0x1A, 128, 0x00, 0x04, 255, 0x00, 0x00, 0x00];
        let keys_v1 = decode_wooting_analog_frame(&frame_v1, 0xFF54);
        assert_eq!(keys_v1.len(), 2);
        assert_eq!(keys_v1[0].key_label, "A");
        assert!((keys_v1[0].travel_mm - 4.0).abs() < 0.01);
        assert_eq!(keys_v1[1].key_label, "W");
        assert!((keys_v1[1].travel_mm - 2.0).abs() < 0.05);

        // V2 (0xFF53) 4-byte 10-bit analog stream frame:
        // Key 1: matrix 0x42 (W), usage 0x1A, packed=0xC1 (actuated=1, ns=0, lo2=3), hi=0xFF -> raw=1023 (4.00mm)
        // Key 2: matrix 0x80 (LShift), usage 0xE1, packed=0x00 (actuated=0, ns=0, lo2=0), hi=0x80 -> raw=512 (2.00mm)
        let frame_v2 = [0x42, 0x1A, 0xC1, 0xFF, 0x80, 0xE1, 0x00, 0x80, 0, 0, 0, 0];
        let keys_v2 = decode_wooting_analog_frame(&frame_v2, 0xFF53);
        assert_eq!(keys_v2.len(), 2);
        assert_eq!(keys_v2[0].key_label, "W");
        assert_eq!(keys_v2[0].raw_value, 1023);
        assert_eq!(keys_v2[0].actuated, Some(true));
        assert!((keys_v2[0].travel_mm - 4.0).abs() < 0.01);
        assert_eq!(keys_v2[1].key_label, "LShift");
        assert_eq!(keys_v2[1].raw_value, 512);
        assert_eq!(keys_v2[1].actuated, Some(false));
        assert!((keys_v2[1].travel_mm - 2.0).abs() < 0.02);
    }

    #[test]
    fn decodes_live_60he_plus_v2_13_2_8bit_profile_and_per_key_actuation() {
        // Real 60HE+ (fw v2.13.2) 0x27 reply: 8-bit scale (51 = 0.80mm, RT 32 = 0.50mm, Tachyon ON)
        let raw_27_8bit: [u8; 26] = [
            0xD1, 0xDA, 0x27, 0x88, 0x14, 0x00, 0x0A, 0x12, 0x08, 0x33, 0x10, 0x01, 0x18, 0x01,
            0x20, 0x20, 0x28, 0x01, 0x30, 0x02, 0x38, 0x00, 0x40, 0x20, 0x48, 0x01,
        ];
        let prof = decode_wooting_keyboard_profile(&raw_27_8bit, WootingProtocol::Arm).unwrap();
        assert!((prof.actuation_mm.unwrap() - 0.80).abs() < 0.01);
        assert_eq!(prof.rapid_trigger_enabled, Some(true));
        assert!((prof.rapid_trigger_sensitivity_mm.unwrap() - 0.50).abs() < 0.01);
        assert_eq!(prof.tachyon_enabled, Some(true));
        assert_eq!(prof.tachyon_mode, Some(1));

        // Patching an 8-bit profile preserves 8-bit scale & syncs matched secondary RT field 8
        let patched = patch_wooting_keyboard_base(
            &raw_27_8bit[6..],
            &WootingAnalogBasePatch {
                actuation_mm: Some(0.40),
                rapid_trigger_enabled: Some(true),
                rapid_trigger_sensitivity_mm: Some(0.20),
            },
        )
        .unwrap();
        let mut fake_reply = vec![0xD1, 0xDA, 0x27, 0x88, patched.len() as u8, 0x00];
        fake_reply.extend_from_slice(&patched);
        let prof_after =
            decode_wooting_keyboard_profile(&fake_reply, WootingProtocol::Arm).unwrap();
        assert!((prof_after.actuation_mm.unwrap() - 0.40).abs() < 0.02);
        assert!((prof_after.rapid_trigger_sensitivity_mm.unwrap() - 0.20).abs() < 0.02);
        assert_eq!(prof_after.tachyon_enabled, Some(true));

        // Real 60HE+ (fw v2.13.2) 0x31 reply: 7 per-key overrides in Field 2 (8-bit partial)
        let raw_31_8bit: [u8; 27] = [
            0xD1, 0xDA, 0x31, 0x88, 0x15, 0x00, 0x12, 0x13, 0x0A, 0x11, 0xA0, 0x80, 0x02, 0xC2,
            0x40, 0xE1, 0x40, 0xE2, 0x40, 0xE3, 0x40, 0xA1, 0xFF, 0x02, 0xA6, 0xC1, 0x01,
        ];
        let overrides =
            decode_wooting_per_key_actuation(&raw_31_8bit, WootingProtocol::Arm).unwrap();
        assert_eq!(overrides.len(), 7);
        assert_eq!(overrides[0].key_label, "Esc/`");
        assert!((overrides[0].actuation_mm - 2.00).abs() < 0.01);
        assert_eq!(overrides[1].key_label, "W");
        assert!((overrides[1].actuation_mm - 0.50).abs() < 0.01);
        assert_eq!(overrides[2].key_label, "A");
        assert_eq!(overrides[3].key_label, "S");
        assert_eq!(overrides[4].key_label, "D");
        assert_eq!(overrides[5].key_label, "LWin");
        assert!((overrides[5].actuation_mm - 3.00).abs() < 0.01);
        assert_eq!(overrides[6].key_label, "Space");
        assert!((overrides[6].actuation_mm - 1.50).abs() < 0.01);
    }
}
