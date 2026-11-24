// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Logitech HID++ 2.0 protocol-family driver (G Pro X Superlight 1/2, G305, G502, G703,
//! G403, G903, G203, MX series, wired USB & Lightspeed / Unifying / Bolt receivers).
//!
//! # Protocol Attribution & Licenses (all AGPL-3.0 compatible)
//! Every HID report ID, frame layout, feature ID, function ID, bitmask, and error code in this
//! module is verified directly from documented open-source implementations:
//! - `libratbag/libratbag` (<https://github.com/libratbag/libratbag>)
//!   - `src/hidpp-generic.h`, `src/hidpp20.h`, `src/hidpp20.c`, `src/driver-hidpp20.c`
//!   - **MIT License**, Copyright (c) 2013-2017 Red Hat, Inc., David Herrmann, Benjamin Tissoires
//! - `pwr-Solaar/Solaar` (<https://github.com/pwr-Solaar/Solaar>)
//!   - `lib/logitech_receiver/hidpp20_constants.py`, `hidpp20.py`, `settings_templates.py`
//!   - **GPL-2.0-or-later**, Copyright (C) 2012-2024 Daniel Pavel & Solaar Contributors
//!
//! No HID bytes are guessed. Onboard memory flash sectors (`0x8100` write commands `0x60`/`0x70`/`0x80`)
//! are **never** written; all DPI, polling rate, and extended LOD updates are applied strictly to
//! **volatile RAM** (zero flash wear).

use std::collections::HashSet;

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice, MouseSettings,
    SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};
use crate::hid::parse_hid_report_descriptor;

// --- Transport Constants (`libratbag/src/hidpp-generic.h`) ---
pub const LOGITECH_VID: u16 = 0x046D;
pub const HIDPP_USAGE_PAGE: u16 = 0xFF00;

pub const REPORT_ID_SHORT: u8 = 0x10;
pub const REPORT_ID_LONG: u8 = 0x11;
pub const SHORT_MSG_LEN: usize = 7;
pub const LONG_MSG_LEN: usize = 20;

/// Direct/wired HID++ 2.0 device index (receiver itself on wireless dongles).
pub const DEVICE_IDX_DIRECT: u8 = 0xFF;
/// Wireless receiver paired slot indices (`1..=6` on Lightspeed / Unifying / Bolt receivers).
pub const RECEIVER_SLOTS: std::ops::RangeInclusive<u8> = 1..=6;

/// 4-bit software identifier placed in the low nibble of byte 3 (matches `libratbag`'s `0x8`).
pub const SW_ID: u8 = 0x08;

const ERR_HIDPP10_TAG: u8 = 0x8F;
const ERR_HIDPP20_TAG: u8 = 0xFF;
const HIDPP10_ERR_INVALID_SUBID: u8 = 0x01;
const HIDPP10_ERR_UNKNOWN_DEVICE: u8 = 0x08;
const HIDPP10_ERR_RESOURCE_ERROR: u8 = 0x09;

// --- HID++ 2.0 Feature IDs (`libratbag/src/hidpp20.h` & `Solaar/hidpp20_constants.py`) ---
pub const FEATURE_ROOT: u16 = 0x0000;
pub const FEATURE_DEVICE_NAME: u16 = 0x0005;
pub const FEATURE_ADJUSTABLE_DPI: u16 = 0x2201;
pub const FEATURE_EXTENDED_ADJUSTABLE_DPI: u16 = 0x2202;
pub const FEATURE_REPORT_RATE: u16 = 0x8060;
pub const FEATURE_EXTENDED_REPORT_RATE: u16 = 0x8061;
pub const FEATURE_ONBOARD_PROFILES: u16 = 0x8100;

// --- Function IDs (high nibble of byte 3) ---
const FN_ROOT_GET_FEATURE: u8 = 0x00;
const FN_ROOT_GET_PROTOCOL_VERSION: u8 = 0x10;

const FN_NAME_GET_COUNT: u8 = 0x00;
const FN_NAME_GET_FRAGMENT: u8 = 0x10;

const FN_DPI_GET_SENSOR_COUNT: u8 = 0x00;
const FN_DPI_GET_DPI_LIST: u8 = 0x10;
const FN_DPI_GET_SENSOR_DPI: u8 = 0x20;
const FN_DPI_SET_SENSOR_DPI: u8 = 0x30;

const FN_EXT_DPI_GET_CAPABILITIES: u8 = 0x10;
const FN_EXT_DPI_GET_DPI_LIST: u8 = 0x20;
const FN_EXT_DPI_GET_SENSOR_DPI: u8 = 0x50;
const FN_EXT_DPI_SET_SENSOR_DPI: u8 = 0x60;

const FN_RATE_GET_LIST: u8 = 0x00;
const FN_RATE_GET: u8 = 0x10;
const FN_RATE_SET: u8 = 0x20;

const FN_EXT_RATE_GET_LIST: u8 = 0x10;
const FN_EXT_RATE_GET: u8 = 0x20;
const FN_EXT_RATE_SET: u8 = 0x30;

const FN_ONBOARD_SET_MODE: u8 = 0x10;
const FN_ONBOARD_GET_MODE: u8 = 0x20;
const ONBOARD_MODE_HOST: u8 = 0x02;

const PROBE_TIMEOUT_MS: i32 = 250;
const IO_TIMEOUT_MS: i32 = 1000;
const MAX_SKIPPED_REPORTS: usize = 12;

/// DPI list encoding parsed from `0x2201` (`getSensorDpiList`) or `0x2202` (`getDpiList`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDpiRange {
    pub dpi_min: u16,
    pub dpi_max: u16,
    pub dpi_step: u16,
    pub discrete_values: Vec<u16>,
}

/// Builds a 20-byte HID++ 2.0 Long Report (`Report ID 0x11`).
pub fn build_long_request(
    device_idx: u8,
    feature_index: u8,
    function_id: u8,
    params: &[u8],
) -> [u8; LONG_MSG_LEN] {
    let mut pkt = [0u8; LONG_MSG_LEN];
    pkt[0] = REPORT_ID_LONG;
    pkt[1] = device_idx;
    pkt[2] = feature_index;
    pkt[3] = (function_id & 0xF0) | (SW_ID & 0x0F);
    let n = params.len().min(LONG_MSG_LEN - 4);
    pkt[4..4 + n].copy_from_slice(&params[..n]);
    pkt
}

/// Inspects an incoming HID++ reply against `req`.
///
/// Returns:
/// - `Some(Ok(()))` if this packet is the matching success reply
/// - `Some(Err((is_v2_err, code)))` if this packet is a matching HID++ 1.0 (`0x8F`) or 2.0 (`0xFF`) error
/// - `None` if this packet is an unrelated notification/event and should be skipped
pub fn match_reply(
    req: &[u8; LONG_MSG_LEN],
    resp: &[u8],
) -> Option<std::result::Result<(), (bool, u8)>> {
    if resp.len() < SHORT_MSG_LEN {
        return None;
    }
    if resp[0] != REPORT_ID_SHORT && resp[0] != REPORT_ID_LONG {
        return None;
    }
    if resp[1] != req[1] {
        return None;
    }
    if resp[2] == req[2] && resp[3] == req[3] {
        return Some(Ok(()));
    }
    if (resp[2] == ERR_HIDPP20_TAG || resp[2] == ERR_HIDPP10_TAG)
        && resp[3] == req[2]
        && resp[4] == req[3]
    {
        return Some(Err((resp[2] == ERR_HIDPP20_TAG, resp[5])));
    }
    None
}

/// Parses big-endian `u16` DPI list bytes from `0x2201` (`params[1..]`) or `0x2202` (`params[3..]`).
///
/// Values `> 0xE000` encode a step increment `val - 0xE000` between the previous min and next max
/// (verified in `libratbag/src/hidpp20.c` lines 1565-1571 & `Solaar/settings_templates.py` lines 1049-1054).
pub fn parse_dpi_list_bytes(raw: &[u8]) -> Option<ParsedDpiRange> {
    let mut dpi_min = u16::MAX;
    let mut dpi_max = 0u16;
    let mut dpi_step = 0u16;
    let mut discrete_values = Vec::new();

    for chunk in raw.as_chunks::<2>().0 {
        let val = u16::from_be_bytes(*chunk);
        if val == 0 {
            break;
        }
        if val > 0xE000 {
            dpi_step = val - 0xE000;
        } else {
            dpi_min = dpi_min.min(val);
            dpi_max = dpi_max.max(val);
            discrete_values.push(val);
        }
    }

    if dpi_min == u16::MAX || dpi_max == 0 {
        return None;
    }

    // If the mouse returned discrete values without an explicit step marker, infer GCD-like step.
    if dpi_step == 0 {
        dpi_step = discrete_values
            .windows(2)
            .map(|w| w[1].saturating_sub(w[0]))
            .filter(|&d| d > 0)
            .min()
            .unwrap_or(50);
    }

    Some(ParsedDpiRange {
        dpi_min,
        dpi_max,
        dpi_step: dpi_step.max(1),
        discrete_values,
    })
}

/// Decodes `0x8060` (`REPORT_RATE`) bitmask byte into sorted standard Hz rates.
///
/// Bit `i` corresponds to period `i + 1` ms (`bit 0 = 1ms = 1000Hz`, `bit 1 = 2ms = 500Hz`,
/// `bit 3 = 4ms = 250Hz`, `bit 7 = 8ms = 125Hz`; verified in `libratbag/src/driver-hidpp20.c`).
pub fn decode_report_rate_list_8060(bitflags: u8) -> Vec<u16> {
    let mut rates = Vec::new();
    if (bitflags & 0x80) != 0 {
        rates.push(125);
    }
    if (bitflags & 0x08) != 0 {
        rates.push(250);
    }
    if (bitflags & 0x02) != 0 {
        rates.push(500);
    }
    if (bitflags & 0x01) != 0 {
        rates.push(1000);
    }
    rates
}

pub fn decode_report_rate_8060(period_ms: u8) -> Option<u16> {
    match period_ms {
        1 => Some(1000),
        2 | 3 => Some(500),
        4 | 5 => Some(250),
        6..=8 => Some(125),
        _ => None,
    }
}

pub fn encode_report_rate_8060(hz: u16) -> Option<u8> {
    match hz {
        1000 => Some(1),
        500 => Some(2),
        250 => Some(4),
        125 => Some(8),
        _ => None,
    }
}

/// Decodes `0x8061` (`EXTENDED_ADJUSTABLE_REPORT_RATE`) 16-bit bitmask into sorted Hz rates
/// (verified in `Solaar/settings_templates.py` lines 633-652).
pub fn decode_report_rate_list_8061(bitflags: u16) -> Vec<u16> {
    const TABLE: [(u8, u16); 7] = [
        (0, 125),
        (1, 250),
        (2, 500),
        (3, 1000),
        (4, 2000),
        (5, 4000),
        (6, 8000),
    ];
    TABLE
        .iter()
        .filter_map(|&(bit, hz)| ((bitflags & (1u16 << bit)) != 0).then_some(hz))
        .collect()
}

pub fn decode_report_rate_8061(index: u8) -> Option<u16> {
    match index {
        0 => Some(125),
        1 => Some(250),
        2 => Some(500),
        3 => Some(1000),
        4 => Some(2000),
        5 => Some(4000),
        6 => Some(8000),
        _ => None,
    }
}

pub fn encode_report_rate_8061(hz: u16) -> Option<u8> {
    match hz {
        125 => Some(0),
        250 => Some(1),
        500 => Some(2),
        1000 => Some(3),
        2000 => Some(4),
        4000 => Some(5),
        8000 => Some(6),
        _ => None,
    }
}

/// Maps `0x2202` (`EXTENDED_ADJUSTABLE_DPI`) LOD codes (`0=LOW`, `1=MEDIUM`, `2=HIGH`) to `LiftOffDistance`.
pub fn decode_ext_dpi_lod(code: u8) -> Option<LiftOffDistance> {
    match code {
        0 => Some(LiftOffDistance::Mm07),
        1 => Some(LiftOffDistance::Mm10),
        2 => Some(LiftOffDistance::Mm20),
        _ => None,
    }
}

pub fn encode_ext_dpi_lod(lod: LiftOffDistance) -> u8 {
    match lod {
        LiftOffDistance::Mm07 => 0,
        LiftOffDistance::Mm10 => 1,
        LiftOffDistance::Mm20 => 2,
    }
}

#[derive(Debug, Clone, Copy)]
enum DpiFeature {
    Standard2201 {
        feature_idx: u8,
    },
    Extended2202 {
        feature_idx: u8,
        has_y: bool,
        has_lod: bool,
    },
}

#[derive(Debug, Clone, Copy)]
enum ReportRateFeature {
    Standard8060 { feature_idx: u8 },
    Extended8061 { feature_idx: u8 },
}

fn slugify_id(name: &str, vid: u16, pid: u16, device_idx: u8) -> String {
    let mut slug = String::from("logitech_");
    let mut prev_us = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_us = false;
        } else if !prev_us {
            slug.push('_');
            prev_us = true;
        }
    }
    let slug = slug.trim_end_matches('_');
    if slug.len() <= "logitech".len() {
        format!("logitech_{vid:04x}_{pid:04x}_slot{device_idx}")
    } else {
        slug.to_string()
    }
}

/// Hardware backend for Logitech HID++ 2.0 mice (volatile RAM writes — zero onboard flash wear).
pub struct Hidpp20Device {
    hid: HidDevice,
    device_idx: u8,
    id_slug: String,
    name: String,
    vid_pid: String,
    connection: String,
    dpi_feature: DpiFeature,
    rate_feature: Option<ReportRateFeature>,
    onboard_profiles_idx: Option<u8>,
    dpi_range: ParsedDpiRange,
    polling_rates_hz: Vec<u16>,
    lod_options: Vec<LiftOffDistance>,
}

impl Hidpp20Device {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.name.clone(),
            message: message.into(),
        }
    }

    fn request(
        &mut self,
        feature_idx: u8,
        function_id: u8,
        params: &[u8],
    ) -> Result<[u8; LONG_MSG_LEN]> {
        hidpp20_request_raw(
            &self.hid,
            self.device_idx,
            feature_idx,
            function_id,
            params,
            IO_TIMEOUT_MS,
        )
        .map_err(|msg| self.dev_err(msg))
    }

    /// Ensures the mouse is in volatile Host Mode (`0x02`) if `0x8100` Onboard Profiles is present,
    /// without ever erasing or modifying onboard flash sectors.
    fn ensure_volatile_host_mode(&mut self) -> Result<()> {
        let Some(idx) = self.onboard_profiles_idx else {
            return Ok(());
        };
        if let Ok(reply) = self.request(idx, FN_ONBOARD_GET_MODE, &[]) {
            if reply[4] != ONBOARD_MODE_HOST {
                let _ = self.request(idx, FN_ONBOARD_SET_MODE, &[ONBOARD_MODE_HOST]);
            }
        }
        Ok(())
    }

    fn read_dpi_and_lod(&mut self) -> Result<(u16, Option<LiftOffDistance>)> {
        match self.dpi_feature {
            DpiFeature::Standard2201 { feature_idx } => {
                let reply = self.request(feature_idx, FN_DPI_GET_SENSOR_DPI, &[0x00])?;
                let current = u16::from_be_bytes([reply[5], reply[6]]);
                let default_dpi = u16::from_be_bytes([reply[7], reply[8]]);
                let dpi = if current != 0 { current } else { default_dpi };
                Ok((dpi, None))
            }
            DpiFeature::Extended2202 {
                feature_idx,
                has_lod,
                ..
            } => {
                let reply = self.request(feature_idx, FN_EXT_DPI_GET_SENSOR_DPI, &[0x00])?;
                let cur_x = u16::from_be_bytes([reply[5], reply[6]]);
                let def_x = u16::from_be_bytes([reply[7], reply[8]]);
                let dpi = if cur_x != 0 { cur_x } else { def_x };
                let lod = if has_lod {
                    decode_ext_dpi_lod(reply[13])
                } else {
                    None
                };
                Ok((dpi, lod))
            }
        }
    }

    fn write_dpi_and_lod(&mut self, dpi: u16, lod: Option<LiftOffDistance>) -> Result<()> {
        match self.dpi_feature {
            DpiFeature::Standard2201 { feature_idx } => {
                let be = dpi.to_be_bytes();
                let _ = self.request(feature_idx, FN_DPI_SET_SENSOR_DPI, &[0x00, be[0], be[1]])?;
                Ok(())
            }
            DpiFeature::Extended2202 {
                feature_idx,
                has_y,
                has_lod,
            } => {
                let be = dpi.to_be_bytes();
                let (y_hi, y_lo) = if has_y { (be[0], be[1]) } else { (0, 0) };
                let lod_byte = if has_lod {
                    lod.map(encode_ext_dpi_lod).unwrap_or(1)
                } else {
                    0
                };
                let _ = self.request(
                    feature_idx,
                    FN_EXT_DPI_SET_SENSOR_DPI,
                    &[0x00, be[0], be[1], y_hi, y_lo, lod_byte],
                )?;
                Ok(())
            }
        }
    }

    fn read_polling_rate(&mut self) -> Result<Option<u16>> {
        match self.rate_feature {
            None => Ok(None),
            Some(ReportRateFeature::Standard8060 { feature_idx }) => {
                let reply = self.request(feature_idx, FN_RATE_GET, &[0x00])?;
                Ok(decode_report_rate_8060(reply[4]))
            }
            Some(ReportRateFeature::Extended8061 { feature_idx }) => {
                let reply = self.request(feature_idx, FN_EXT_RATE_GET, &[])?;
                Ok(decode_report_rate_8061(reply[4]))
            }
        }
    }

    fn write_polling_rate(&mut self, hz: u16) -> Result<()> {
        match self.rate_feature {
            None => Err(PolledError::UnsupportedSetting {
                device: self.name.clone(),
                detail: format!("Polling rate {hz} Hz is not adjustable on {}", self.name),
            }),
            Some(ReportRateFeature::Standard8060 { feature_idx }) => {
                let period =
                    encode_report_rate_8060(hz).ok_or_else(|| PolledError::UnsupportedSetting {
                        device: self.name.clone(),
                        detail: format!("Unsupported HID++ 0x8060 polling rate {hz} Hz"),
                    })?;
                let _ = self.request(feature_idx, FN_RATE_SET, &[period])?;
                Ok(())
            }
            Some(ReportRateFeature::Extended8061 { feature_idx }) => {
                let code =
                    encode_report_rate_8061(hz).ok_or_else(|| PolledError::UnsupportedSetting {
                        device: self.name.clone(),
                        detail: format!("Unsupported HID++ 0x8061 polling rate {hz} Hz"),
                    })?;
                let _ = self.request(feature_idx, FN_EXT_RATE_SET, &[code])?;
                Ok(())
            }
        }
    }
}

impl MouseDevice for Hidpp20Device {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: self.id_slug.clone(),
            name: self.name.clone(),
            vendor: "Logitech".to_string(),
            vid_pid: Some(self.vid_pid.clone()),
            connection: self.connection.clone(),
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            dpi_min: self.dpi_range.dpi_min,
            dpi_max: self.dpi_range.dpi_max,
            dpi_step: self.dpi_range.dpi_step,
            polling_rates_hz: self.polling_rates_hz.clone(),
            lod_options: self.lod_options.clone(),
            supports_debounce: false,
            debounce_min_ms: 0,
            debounce_max_ms: 0,
            supports_volatile_writes: true,
            flash_write_warning: None,
        }
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        let (dpi, lod) = self.read_dpi_and_lod()?;
        let polling_rate_hz = self.read_polling_rate()?;
        Ok(MouseSettings {
            dpi: Some(dpi),
            polling_rate_hz,
            lod,
            debounce_ms: None,
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        let current = self.read_settings()?;
        self.ensure_volatile_host_mode()?;

        let dpi_changed = settings.dpi.is_some() && settings.dpi != current.dpi;
        let lod_changed = settings.lod.is_some() && settings.lod != current.lod;
        if dpi_changed || lod_changed {
            let target_dpi = settings.dpi.or(current.dpi).unwrap_or(800);
            let target_lod = settings.lod.or(current.lod);
            self.write_dpi_and_lod(target_dpi, target_lod)?;
        }

        if let Some(target_hz) = settings.polling_rate_hz {
            if current.polling_rate_hz != Some(target_hz) {
                self.write_polling_rate(target_hz)?;
            }
        }

        Ok(())
    }
}

fn hidpp20_request_raw(
    hid: &HidDevice,
    device_idx: u8,
    feature_idx: u8,
    function_id: u8,
    params: &[u8],
    timeout_ms: i32,
) -> std::result::Result<[u8; LONG_MSG_LEN], String> {
    let req = build_long_request(device_idx, feature_idx, function_id, params);
    hid.write(&req)
        .map_err(|e| format!("HID++ write failed (feat=0x{feature_idx:02x}): {e}"))?;

    let mut buf = [0u8; 64];
    for _ in 0..MAX_SKIPPED_REPORTS {
        let n = hid
            .read_timeout(&mut buf, timeout_ms)
            .map_err(|e| format!("HID++ read failed (feat=0x{feature_idx:02x}): {e}"))?;
        if n == 0 {
            return Err(format!(
                "Timed out waiting for HID++ reply (idx=0x{device_idx:02x}, feat=0x{feature_idx:02x})"
            ));
        }
        if let Some(outcome) = match_reply(&req, &buf[..n]) {
            match outcome {
                Ok(()) => {
                    let mut reply = [0u8; LONG_MSG_LEN];
                    let copy_len = n.min(LONG_MSG_LEN);
                    reply[..copy_len].copy_from_slice(&buf[..copy_len]);
                    return Ok(reply);
                }
                Err((is_v2, code)) => {
                    return Err(format!(
                        "HID++ {} error 0x{code:02x} (idx=0x{device_idx:02x}, feat=0x{feature_idx:02x})",
                        if is_v2 { "2.0" } else { "1.0" }
                    ));
                }
            }
        }
    }

    Err(format!(
        "No matching HID++ reply after {MAX_SKIPPED_REPORTS} packets"
    ))
}

/// Checks if a target `device_idx` answers `IRoot.getProtocolVersion` with HID++ >= 2.0.
/// Returns `Ok(Some((major, minor)))` for HID++ 2.0+, `Ok(None)` for HID++ 1.0 or absent slot.
fn probe_protocol_version(hid: &HidDevice, device_idx: u8) -> Option<(u8, u8)> {
    let req = build_long_request(
        device_idx,
        0x00,
        FN_ROOT_GET_PROTOCOL_VERSION,
        &[0, 0, 0xAA],
    );
    if hid.write(&req).is_err() {
        return None;
    }
    let mut buf = [0u8; 64];
    for _ in 0..MAX_SKIPPED_REPORTS {
        let Ok(n) = hid.read_timeout(&mut buf, PROBE_TIMEOUT_MS) else {
            return None;
        };
        if n == 0 {
            return None;
        }
        if let Some(outcome) = match_reply(&req, &buf[..n]) {
            return match outcome {
                Ok(()) if buf[4] >= 2 => Some((buf[4], buf[5])),
                Err((false, HIDPP10_ERR_INVALID_SUBID))
                | Err((false, HIDPP10_ERR_UNKNOWN_DEVICE))
                | Err((false, HIDPP10_ERR_RESOURCE_ERROR)) => None,
                _ => None,
            };
        }
    }
    None
}

fn root_get_feature_idx(hid: &HidDevice, device_idx: u8, feature_id: u16) -> Option<u8> {
    let be = feature_id.to_be_bytes();
    let reply = hidpp20_request_raw(
        hid,
        device_idx,
        0x00,
        FN_ROOT_GET_FEATURE,
        &[be[0], be[1]],
        PROBE_TIMEOUT_MS,
    )
    .ok()?;
    let idx = reply[4];
    (idx != 0).then_some(idx)
}

fn read_device_name(hid: &HidDevice, device_idx: u8) -> Option<String> {
    let name_idx = root_get_feature_idx(hid, device_idx, FEATURE_DEVICE_NAME)?;
    let count_reply = hidpp20_request_raw(
        hid,
        device_idx,
        name_idx,
        FN_NAME_GET_COUNT,
        &[],
        PROBE_TIMEOUT_MS,
    )
    .ok()?;
    let total_len = usize::from(count_reply[4]);
    if total_len == 0 || total_len > 64 {
        return None;
    }
    let mut bytes = Vec::with_capacity(total_len);
    while bytes.len() < total_len {
        let offset = bytes.len() as u8;
        let frag = hidpp20_request_raw(
            hid,
            device_idx,
            name_idx,
            FN_NAME_GET_FRAGMENT,
            &[offset],
            PROBE_TIMEOUT_MS,
        )
        .ok()?;
        let remaining = total_len - bytes.len();
        let take = remaining.min(LONG_MSG_LEN - 4);
        bytes.extend_from_slice(&frag[4..4 + take]);
    }
    let s = String::from_utf8_lossy(&bytes).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn probe_slot_device(
    api: &HidApi,
    path: &std::ffi::CStr,
    vid: u16,
    pid: u16,
    os_product: Option<&str>,
    device_idx: u8,
) -> Option<Hidpp20Device> {
    let hid = api.open_path(path).ok()?;
    let (major, minor) = probe_protocol_version(&hid, device_idx)?;

    // Require at least one adjustable DPI feature so keyboards/headsets on Bolt/Unifying are skipped.
    let (dpi_feature, dpi_range, lod_options) = if let Some(ext_idx) =
        root_get_feature_idx(&hid, device_idx, FEATURE_EXTENDED_ADJUSTABLE_DPI)
    {
        let cap_reply = hidpp20_request_raw(
            &hid,
            device_idx,
            ext_idx,
            FN_EXT_DPI_GET_CAPABILITIES,
            &[0x00],
            PROBE_TIMEOUT_MS,
        )
        .ok()?;
        let has_y = (cap_reply[6] & 0x01) != 0;
        let has_lod = (cap_reply[6] & 0x02) != 0;

        let mut list_bytes = Vec::new();
        for chunk_idx in 0u8..16 {
            let rep = hidpp20_request_raw(
                &hid,
                device_idx,
                ext_idx,
                FN_EXT_DPI_GET_DPI_LIST,
                &[0x00, 0x00, chunk_idx],
                PROBE_TIMEOUT_MS,
            )
            .ok()?;
            list_bytes.extend_from_slice(&rep[7..LONG_MSG_LEN]);
            if list_bytes.ends_with(&[0x00, 0x00]) {
                break;
            }
        }
        let range = parse_dpi_list_bytes(&list_bytes)?;
        let lods = if has_lod {
            LiftOffDistance::ALL.to_vec()
        } else {
            Vec::new()
        };
        (
            DpiFeature::Extended2202 {
                feature_idx: ext_idx,
                has_y,
                has_lod,
            },
            range,
            lods,
        )
    } else {
        let std_idx = root_get_feature_idx(&hid, device_idx, FEATURE_ADJUSTABLE_DPI)?;
        let count_reply = hidpp20_request_raw(
            &hid,
            device_idx,
            std_idx,
            FN_DPI_GET_SENSOR_COUNT,
            &[],
            PROBE_TIMEOUT_MS,
        )
        .ok()?;
        if count_reply[4] == 0 {
            return None;
        }
        let list_reply = hidpp20_request_raw(
            &hid,
            device_idx,
            std_idx,
            FN_DPI_GET_DPI_LIST,
            &[0x00],
            PROBE_TIMEOUT_MS,
        )
        .ok()?;
        let range = parse_dpi_list_bytes(&list_reply[5..LONG_MSG_LEN])?;
        (
            DpiFeature::Standard2201 {
                feature_idx: std_idx,
            },
            range,
            Vec::new(),
        )
    };

    // Detect polling-rate feature (prefer 0x8061 Extended up to 8K, fallback to 0x8060 up to 1K).
    let (rate_feature, polling_rates_hz) = if let Some(ext_rate_idx) =
        root_get_feature_idx(&hid, device_idx, FEATURE_EXTENDED_REPORT_RATE)
    {
        let rep = hidpp20_request_raw(
            &hid,
            device_idx,
            ext_rate_idx,
            FN_EXT_RATE_GET_LIST,
            &[],
            PROBE_TIMEOUT_MS,
        )
        .ok();
        let rates = rep
            .map(|r| decode_report_rate_list_8061(u16::from_be_bytes([r[4], r[5]])))
            .unwrap_or_default();
        if rates.is_empty() {
            (None, Vec::new())
        } else {
            (
                Some(ReportRateFeature::Extended8061 {
                    feature_idx: ext_rate_idx,
                }),
                rates,
            )
        }
    } else if let Some(rate_idx) = root_get_feature_idx(&hid, device_idx, FEATURE_REPORT_RATE) {
        let rep = hidpp20_request_raw(
            &hid,
            device_idx,
            rate_idx,
            FN_RATE_GET_LIST,
            &[],
            PROBE_TIMEOUT_MS,
        )
        .ok();
        let rates = rep
            .map(|r| decode_report_rate_list_8060(r[4]))
            .unwrap_or_default();
        if rates.is_empty() {
            (None, Vec::new())
        } else {
            (
                Some(ReportRateFeature::Standard8060 {
                    feature_idx: rate_idx,
                }),
                rates,
            )
        }
    } else {
        (None, Vec::new())
    };

    let onboard_profiles_idx = root_get_feature_idx(&hid, device_idx, FEATURE_ONBOARD_PROFILES);

    let name = read_device_name(&hid, device_idx)
        .or_else(|| {
            os_product
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| format!("Logitech HID++ Mouse ({vid:04x}:{pid:04x})"));

    let connection = if device_idx == DEVICE_IDX_DIRECT {
        format!("USB Wired (HID++ {major}.{minor})")
    } else {
        format!("Wireless Receiver slot {device_idx} (HID++ {major}.{minor})")
    };

    Some(Hidpp20Device {
        hid,
        device_idx,
        id_slug: slugify_id(&name, vid, pid, device_idx),
        name,
        vid_pid: format!("{vid:04x}:{pid:04x}"),
        connection,
        dpi_feature,
        rate_feature,
        onboard_profiles_idx,
        dpi_range,
        polling_rates_hz,
        lod_options,
    })
}

/// Protocol-family auto-detector for Logitech HID++ 2.0 mice (`VID 0x046D`, `usage_page 0xFF00`).
pub struct Hidpp20ProtocolDriver;

impl DeviceDriver for Hidpp20ProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "Logitech HID++ 2.0 (0xFF00)"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();
        let mut seen_paths: HashSet<Vec<u8>> = HashSet::new();

        for dev in api.device_list() {
            if dev.vendor_id() != LOGITECH_VID || dev.usage_page() != HIDPP_USAGE_PAGE {
                continue;
            }
            let path_key = dev.path().to_bytes().to_vec();
            if !seen_paths.insert(path_key) {
                continue;
            }

            let Ok(probe_hid) = dev.open_device(api) else {
                continue;
            };

            // Verify this collection owns 20-byte Long Report ID 0x11 (In + Out).
            let mut desc_buf = [0u8; 4096];
            let has_long_report = probe_hid
                .get_report_descriptor(&mut desc_buf)
                .ok()
                .map(|len| parse_hid_report_descriptor(&desc_buf[..len]))
                .is_some_and(|summary| {
                    summary.input_reports.get(&REPORT_ID_LONG) == Some(&LONG_MSG_LEN)
                        && summary.output_reports.get(&REPORT_ID_LONG) == Some(&LONG_MSG_LEN)
                });
            drop(probe_hid);

            if !has_long_report {
                continue;
            }

            let vid = dev.vendor_id();
            let pid = dev.product_id();
            let product = dev.product_string();

            // 1. Try direct USB device index (0xFF) first (wired Logitech G mice).
            if let Some(direct) =
                probe_slot_device(api, dev.path(), vid, pid, product, DEVICE_IDX_DIRECT)
            {
                found.push(Box::new(direct));
                continue;
            }

            // 2. Otherwise probe receiver slots 1..=6 (Lightspeed / Unifying / Bolt receivers).
            for slot in RECEIVER_SLOTS {
                if let Some(wireless) = probe_slot_device(api, dev.path(), vid, pid, product, slot)
                {
                    found.push(Box::new(wireless));
                }
            }
        }

        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_long_request_and_matches_success_or_error_replies() {
        let req = build_long_request(0x01, 0x07, FN_DPI_SET_SENSOR_DPI, &[0x00, 0x06, 0x40]);
        assert_eq!(req[0], REPORT_ID_LONG);
        assert_eq!(req[1], 0x01);
        assert_eq!(req[2], 0x07);
        assert_eq!(req[3], 0x38); // FN 0x30 | SW_ID 0x08
        assert_eq!(&req[4..7], &[0x00, 0x06, 0x40]);

        // Matching success reply.
        let mut ok_resp = [0u8; LONG_MSG_LEN];
        ok_resp[0] = REPORT_ID_LONG;
        ok_resp[1] = 0x01;
        ok_resp[2] = 0x07;
        ok_resp[3] = 0x38;
        assert_eq!(match_reply(&req, &ok_resp), Some(Ok(())));

        // Unrelated notification (different SW_ID / feature) is ignored.
        let mut notif = ok_resp;
        notif[3] = 0x30;
        assert_eq!(match_reply(&req, &notif), None);

        // HID++ 2.0 error reply (`0xFF`, echoed feature_idx, echoed addr, err_code).
        let mut err_resp = [0u8; LONG_MSG_LEN];
        err_resp[0] = REPORT_ID_LONG;
        err_resp[1] = 0x01;
        err_resp[2] = ERR_HIDPP20_TAG;
        err_resp[3] = 0x07;
        err_resp[4] = 0x38;
        err_resp[5] = 0x02; // INVALID_ARGUMENT
        assert_eq!(match_reply(&req, &err_resp), Some(Err((true, 0x02))));
    }

    #[test]
    fn parses_range_and_discrete_dpi_lists() {
        // Range-encoded list (e.g. G Pro X Superlight: 100..=25600 step 50 => [100, 0xE032, 25600, 0]).
        let range_raw = [0x00, 0x64, 0xE0, 0x32, 0x64, 0x00, 0x00, 0x00];
        let parsed = parse_dpi_list_bytes(&range_raw).unwrap();
        assert_eq!(parsed.dpi_min, 100);
        assert_eq!(parsed.dpi_max, 25600);
        assert_eq!(parsed.dpi_step, 50);

        // Discrete DPI list (e.g. 400, 800, 1600, 3200, 0).
        let discrete_raw = [0x01, 0x90, 0x03, 0x20, 0x06, 0x40, 0x0C, 0x80, 0x00, 0x00];
        let parsed2 = parse_dpi_list_bytes(&discrete_raw).unwrap();
        assert_eq!(parsed2.dpi_min, 400);
        assert_eq!(parsed2.dpi_max, 3200);
        assert_eq!(parsed2.dpi_step, 400);
        assert_eq!(parsed2.discrete_values, vec![400, 800, 1600, 3200]);
    }

    #[test]
    fn encodes_and_decodes_8060_and_8061_polling_rates() {
        // 0x8060 bitmask: bits 0 (1ms=1000), 1 (2ms=500), 3 (4ms=250), 7 (8ms=125) = 0x8B.
        assert_eq!(
            decode_report_rate_list_8060(0x8B),
            vec![125, 250, 500, 1000]
        );
        for hz in [125u16, 250, 500, 1000] {
            let period = encode_report_rate_8060(hz).unwrap();
            assert_eq!(decode_report_rate_8060(period), Some(hz));
        }

        // 0x8061 bitmask: bits 0..=6 = 0x007F (125..=8000 Hz, e.g., G Pro X Superlight 2).
        assert_eq!(
            decode_report_rate_list_8061(0x007F),
            vec![125, 250, 500, 1000, 2000, 4000, 8000]
        );
        for hz in [125u16, 250, 500, 1000, 2000, 4000, 8000] {
            let idx = encode_report_rate_8061(hz).unwrap();
            assert_eq!(decode_report_rate_8061(idx), Some(hz));
        }
    }

    #[test]
    fn maps_extended_dpi_lod_codes_and_slugs() {
        for lod in LiftOffDistance::ALL {
            assert_eq!(decode_ext_dpi_lod(encode_ext_dpi_lod(lod)), Some(lod));
        }
        assert_eq!(
            slugify_id("PRO X SUPERLIGHT 2", 0x046D, 0xC54D, 1),
            "logitech_pro_x_superlight_2"
        );
    }
}
