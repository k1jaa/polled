// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Keychron Launcher "8K" mouse HID protocol driver (`0x3434`, `0xFFC1:0x0001`, Reports `0xB3`/`0xB4`/`0xB5`/`0xB6`).
//!
//! Covers Keychron M1/M2/M3/M4/M5/M6/M7/M8, G3/G4/G5/G6/G9/G10, LM7 (wired + Link / Ultra-Link /
//! TurboLink receivers) exposing the `0xFFC1` Launcher 8K protocol collection.
//!
//! # Protocol Attribution & Licenses (AGPL-3.0 compatible)
//! Status report layout (`0x06`), DPI (`0x40`/`0x48`/`0x49`), polling (`0x41`/`0x4A`/`0x4B`),
//! sensor flags (`0x42`), debounce (`0x43`), battery byte `[19]`, and model catalog are ported from:
//! - `OpenMouse-Project/mouse-protocol` (<https://github.com/OpenMouse-Project/mouse-protocol>)
//!   - `src/keychron/index.ts`, `src/drivers/keychron/launcher-mouse.ts`, `src/drivers/keychron/mouse-8k-hid.ts`
//!   - **AGPL-3.0-or-later**, Copyright (C) OpenMouse Project Contributors
//!
//! No HID bytes are guessed. Keychron 8K Nordic models (`0xD077`, `0x2801`, `0xD05B`, `0xD078`)
//! are excluded so they are never mis-claimed on `0xFFC1`.

use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    BatteryStatus, DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice,
    MouseSettings, SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};
use crate::hid::parse_hid_report_descriptor;

pub const KEYCHRON_VID: u16 = 0x3434;
pub const KEYCHRON_USAGE_PAGE: u16 = 0xFFC1;
pub const KEYCHRON_USAGE: u16 = 0x0001;

pub const REPORT_CMD_OUT: u8 = 0xB3;
pub const REPORT_CMD_IN: u8 = 0xB4;
pub const REPORT_SET_OUT: u8 = 0xB5;
pub const REPORT_SET_IN: u8 = 0xB6;

const CMD_PACKET_LEN: usize = 63;
const SET_PACKET_LEN: usize = 20;

const ACK_BYTE: u8 = 0xE4;
const CMD_STATUS: u8 = 0x06;
const CMD_WRITE_XY: u8 = 0x48;
const CMD_READ_XY: u8 = 0x49;

const SET_VERSION: u8 = 0x02;
const SET_RECEIVER_STATE: u8 = 0x03;
const SET_DPI: u8 = 0x40;
const SET_POLLING: u8 = 0x41;
const SET_SENSOR: u8 = 0x42;
const SET_DEBOUNCE: u8 = 0x43;
const SET_WRITE_POLLING_SETS: u8 = 0x4A;
const SET_READ_POLLING_SETS: u8 = 0x4B;

const DPI_STAGE_COUNT: usize = 5;
const POLLING_GEARS: usize = 6;
const DEBOUNCE_MAX_MS: u8 = 20;
const FLAGS_IN_VERSION_REPLY: u16 = 6;

const QUERY_TIMEOUT_MS: u64 = 1200;

/// Launcher's `POLLING_RATE_VALUE_SCALE` indexed by polling gear bytes.
pub const KEYCHRON_POLLING_TABLE: [u16; 6] = [125, 500, 1000, 2000, 4000, 8000];

/// Excluded 8K Nordic PIDs that speak the separate `0xFF0A` DMS v2 protocol.
const EXCLUDED_8K_NORDIC_PIDS: &[u16] = &[0xD077, 0x2801, 0xD05B, 0xD078];

const KEYCHRON_RECEIVER_PIDS: &[u16] = &[
    0xD024, 0xD026, 0xD027, 0xD028, 0xD029, 0xD030, 0xD031, 0xD05A,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeychronModelSpec {
    pub pid: u16,
    pub name: &'static str,
    pub dpi_min: u16,
    pub dpi_max: u16,
    pub max_polling_hz: u16,
    pub has_07mm_lod: bool,
    pub no_sensor_options: bool,
}

const fn km(
    pid: u16,
    name: &'static str,
    dpi_min: u16,
    dpi_max: u16,
    max_polling_hz: u16,
    has_07mm_lod: bool,
) -> KeychronModelSpec {
    KeychronModelSpec {
        pid,
        name,
        dpi_min,
        dpi_max,
        max_polling_hz,
        has_07mm_lod,
        no_sensor_options: false,
    }
}

const fn km_basic(
    pid: u16,
    name: &'static str,
    dpi_min: u16,
    dpi_max: u16,
    max_polling_hz: u16,
) -> KeychronModelSpec {
    KeychronModelSpec {
        pid,
        name,
        dpi_min,
        dpi_max,
        max_polling_hz,
        has_07mm_lod: false,
        no_sensor_options: true,
    }
}

pub const KEYCHRON_MODELS: &[KeychronModelSpec] = &[
    km(0xD033, "Keychron M3", 100, 26_000, 1000, false),
    km(0xD035, "Keychron M1", 100, 26_000, 1000, false),
    km(0xD036, "Keychron M3 Mini", 100, 26_000, 1000, false),
    km(0xD03B, "Keychron M2", 100, 26_000, 1000, false),
    km(0xD03D, "Keychron M2 Mini", 100, 26_000, 1000, false),
    km(0xD03F, "Keychron M6", 100, 26_000, 1000, false),
    km(0xD043, "Keychron M4", 100, 26_000, 1000, false),
    km(0xD044, "Keychron M7", 100, 26_000, 1000, false),
    km(0xD048, "Keychron M5 8K", 50, 30_000, 8000, true),
    km(0xD049, "Keychron M6 8K", 50, 30_000, 8000, true),
    km(0xD04A, "Keychron M3 KM", 50, 26_000, 1000, false),
    km_basic(0xD04C, "Keychron M3 Combo", 50, 12_000, 1000),
    km(0xD04D, "Keychron M2 Mini Combo", 50, 26_000, 1000, false),
    km_basic(0xD04E, "Keychron M3 Combo", 50, 12_000, 1000),
    km_basic(0xD04F, "Keychron M3 Mini", 50, 12_000, 1000),
    km(0xD050, "Keychron M3 8K", 50, 30_000, 8000, true),
    km(0xD051, "Keychron M3 Mini 8K", 50, 30_000, 8000, true),
    km(0xD052, "Keychron M2 8K", 50, 30_000, 8000, true),
    km(0xD053, "Keychron M4 8K", 50, 30_000, 8000, true),
    km(0xD054, "Keychron M1 8K", 50, 30_000, 8000, true),
    km(0xD055, "Keychron M2 Mini 8K", 50, 30_000, 8000, true),
    km(0xD056, "Keychron M7 8K", 50, 30_000, 8000, true),
    km(0xD059, "Keychron M1", 100, 26_000, 1000, false),
    km(0xD060, "Keychron M6", 100, 26_000, 1000, true),
    km(0xD06B, "Keychron LM7", 50, 26_000, 8000, false),
    km(0xD06C, "Keychron LM7 Ultra", 50, 30_000, 8000, true),
    km(0xD06D, "Keychron G4", 50, 30_000, 8000, true),
    km(0xD06E, "Keychron G3", 50, 30_000, 8000, true),
    km(0xD06F, "Keychron G5", 50, 30_000, 8000, true),
    km(0xD074, "Keychron G3 HE", 50, 30_000, 8000, true),
    km(0xD075, "Keychron M8", 50, 30_000, 8000, true),
    km(0xD076, "Keychron M3 V2", 50, 26_000, 8000, false),
    km(0xD080, "Keychron G4 HE", 50, 30_000, 8000, true),
    km(0xD083, "Keychron G3 HE", 50, 40_000, 8000, true),
    km(0xD085, "JamesDonkey RSR7 Ultra+", 50, 30_000, 8000, true),
    km(0xD086, "Keychron G6 HE 8K", 50, 40_000, 8000, true),
    km(0xD087, "Keychron G5 HE", 50, 40_000, 8000, true),
    km(0xD08A, "Keychron M6 HE 8K", 50, 30_000, 8000, true),
    km(0xD08C, "Keychron G10 HE", 50, 30_000, 8000, true),
    km(0xD091, "Keychron M8 HE", 50, 30_000, 8000, true),
    km(0xD092, "Keychron G9 HE 8K", 50, 30_000, 8000, true),
    km(0xD09D, "Keychron G6 HE 8K", 50, 40_000, 8000, true),
    km(0xD0A2, "Keychron G6 HE 8K", 50, 40_000, 8000, true),
    km(0xD0A3, "Keychron G3 HE", 50, 40_000, 8000, true),
];

pub fn lookup_keychron_model(pid: u16) -> Option<KeychronModelSpec> {
    KEYCHRON_MODELS.iter().copied().find(|m| m.pid == pid)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeychronStatus {
    pub levels: [u8; 3],
    pub dpi_stages: [u16; DPI_STAGE_COUNT],
    pub stage_count: u8,
    pub lod_code: u8,
    pub ripple_control: bool,
    pub angle_snapping: bool,
    pub motion_sync: bool,
    pub scroll_reversed: bool,
    pub debounce_ms: u8,
    pub battery_percent: u8,
    pub charging: bool,
    pub polling_table: [u8; POLLING_GEARS],
    pub polling_count: u8,
    pub dpi_max_reported: u16,
    pub dpi_step_reported: u8,
    pub max_speed: bool,
    pub lod_level: u8,
    pub feature_dpi_limits: bool,
    pub feature_polling_gears: bool,
    pub feature_separate_dpi: bool,
    pub feature_separate_polling: bool,
    pub feature_lod_level: bool,
}

pub fn decode_keychron_status(
    bytes: &[u8],
    proto_version: u16,
    ver_flags: [u8; 4],
) -> Option<KeychronStatus> {
    if bytes.len() < 51 || bytes[0] != CMD_STATUS {
        return None;
    }
    let flags_byte = bytes[15];
    let stage_count = if bytes[16] == 0 {
        DPI_STAGE_COUNT as u8
    } else {
        bytes[16].min(DPI_STAGE_COUNT as u8)
    };
    let mut dpi_stages = [800u16; DPI_STAGE_COUNT];
    for (stage, slot) in dpi_stages.iter_mut().enumerate() {
        let off = 5 + stage * 2;
        *slot = u16::from_le_bytes([bytes[off], bytes[off + 1]]);
    }
    let mut polling_table = [2u8; POLLING_GEARS];
    polling_table.copy_from_slice(&bytes[43..43 + POLLING_GEARS]);
    let polling_count = if bytes[49] == 0 {
        POLLING_GEARS as u8
    } else {
        bytes[49].min(POLLING_GEARS as u8)
    };
    let feat = if proto_version >= FLAGS_IN_VERSION_REPLY {
        ver_flags
    } else {
        [
            bytes.get(26).copied().unwrap_or(0),
            bytes.get(53).copied().unwrap_or(0),
            bytes.get(60).copied().unwrap_or(0),
            0,
        ]
    };

    Some(KeychronStatus {
        levels: [bytes[2], bytes[3], bytes[4]],
        dpi_stages,
        stage_count,
        lod_code: flags_byte & 0x03,
        ripple_control: (flags_byte & 0x04) != 0,
        angle_snapping: (flags_byte & 0x08) != 0,
        motion_sync: (flags_byte & 0x10) != 0,
        scroll_reversed: (flags_byte & 0x40) != 0,
        debounce_ms: bytes[17],
        battery_percent: bytes[19] & 0x7F,
        charging: (bytes[19] & 0x80) != 0,
        polling_table,
        polling_count,
        dpi_max_reported: u16::from_le_bytes([bytes[40], bytes[41]]),
        dpi_step_reported: bytes[42],
        max_speed: (bytes.get(52).copied().unwrap_or(0) & 0x01) != 0,
        lod_level: bytes.get(61).copied().unwrap_or(0) & 0x0F,
        feature_dpi_limits: (feat[0] & 0x08) != 0,
        feature_polling_gears: (feat[0] & 0x10) != 0,
        feature_separate_dpi: (feat[2] & 0x01) != 0,
        feature_separate_polling: (feat[2] & 0x02) != 0,
        feature_lod_level: (feat[3] & 0x10) != 0,
    })
}

pub fn encode_keychron_dpi_packet(
    active_stage: u8,
    dpi_stages: &[u16; DPI_STAGE_COUNT],
    stage_count: u8,
) -> [u8; SET_PACKET_LEN] {
    let mut pkt = [0u8; SET_PACKET_LEN];
    pkt[0] = SET_DPI;
    pkt[1] = active_stage;
    pkt[2] = active_stage;
    pkt[3] = active_stage;
    for (stage, &dpi) in dpi_stages.iter().enumerate() {
        let le = dpi.to_le_bytes();
        pkt[4 + stage * 2] = le[0];
        pkt[5 + stage * 2] = le[1];
    }
    pkt[14] = stage_count;
    pkt
}

pub fn encode_keychron_sensor_packet(
    lod_code: u8,
    ripple_control: bool,
    angle_snapping: bool,
    motion_sync: bool,
    scroll_reversed: bool,
    max_speed: Option<bool>,
    lod_level: Option<u8>,
) -> [u8; SET_PACKET_LEN] {
    let mut pkt = [0u8; SET_PACKET_LEN];
    pkt[0] = SET_SENSOR;
    pkt[1] = lod_code;
    pkt[2] = if ripple_control { 1 } else { 2 };
    pkt[3] = if angle_snapping { 1 } else { 2 };
    pkt[4] = if motion_sync { 1 } else { 2 };
    pkt[6] = if scroll_reversed { 2 } else { 1 };
    if let Some(ms) = max_speed {
        pkt[8] = if ms { 2 } else { 1 };
    }
    if let Some(lvl) = lod_level {
        pkt[11] = lvl;
    }
    pkt
}

fn decode_keychron_lod_code(code: u8) -> Option<LiftOffDistance> {
    match code {
        3 => Some(LiftOffDistance::Mm07),
        1 => Some(LiftOffDistance::Mm10),
        2 => Some(LiftOffDistance::Mm20),
        _ => None,
    }
}

fn encode_keychron_lod_code(lod: LiftOffDistance) -> u8 {
    match lod {
        LiftOffDistance::Mm07 => 3,
        LiftOffDistance::Mm10 => 1,
        LiftOffDistance::Mm20 => 2,
    }
}

/// Native Windows `hidapi` driver for Keychron Launcher 8K mice (`0x3434`, `0xFFC1`).
pub struct KeychronDevice {
    hid: HidDevice,
    pid: u16,
    work_mode: u8,
    proto_version: u16,
    ver_flags: [u8; 4],
    model: Option<KeychronModelSpec>,
    display_name: String,
    vid_pid: String,
    cached_dpi_max: u16,
    cached_dpi_step: u16,
}

impl KeychronDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.display_name.clone(),
            message: message.into(),
        }
    }

    fn query_raw<F>(
        hid: &HidDevice,
        out_report_id: u8,
        in_report_id: u8,
        payload_len: usize,
        payload: &[u8],
        matches: F,
    ) -> std::result::Result<Vec<u8>, String>
    where
        F: Fn(&[u8]) -> bool,
    {
        let mut send = vec![0u8; payload_len + 1];
        send[0] = out_report_id;
        let copy_len = payload.len().min(payload_len);
        send[1..1 + copy_len].copy_from_slice(&payload[..copy_len]);
        hid.write(&send)
            .map_err(|e| format!("Keychron HID write 0x{out_report_id:02x} failed: {e}"))?;

        let deadline = Instant::now() + Duration::from_millis(QUERY_TIMEOUT_MS);
        let mut buf = [0u8; 128];
        while Instant::now() < deadline {
            let rem = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .clamp(20, 250) as i32;
            let n = hid
                .read_timeout(&mut buf, rem)
                .map_err(|e| format!("Keychron HID read failed: {e}"))?;
            if n < 2 {
                continue;
            }
            let body =
                if buf[0] == in_report_id || buf[0] == REPORT_CMD_IN || buf[0] == REPORT_SET_IN {
                    &buf[1..n]
                } else {
                    &buf[..n]
                };
            if matches(body) {
                return Ok(body.to_vec());
            }
        }
        Err(format!(
            "Timed out waiting for Keychron reply to 0x{:02x}",
            payload.first().copied().unwrap_or(0)
        ))
    }

    fn query_cmd<F>(&self, payload: &[u8], matches: F) -> Result<Vec<u8>>
    where
        F: Fn(&[u8]) -> bool,
    {
        Self::query_raw(
            &self.hid,
            REPORT_CMD_OUT,
            REPORT_CMD_IN,
            CMD_PACKET_LEN,
            payload,
            matches,
        )
        .map_err(|m| self.dev_err(m))
    }

    fn query_settings(&self, command: u8, args: &[u8]) -> Result<Vec<u8>> {
        let mut payload = Vec::with_capacity(1 + args.len());
        payload.push(command);
        payload.extend_from_slice(args);
        Self::query_raw(
            &self.hid,
            REPORT_SET_OUT,
            REPORT_SET_IN,
            SET_PACKET_LEN,
            &payload,
            |b| b.first() == Some(&command),
        )
        .map_err(|m| self.dev_err(m))
    }

    fn write_settings(&self, packet: &[u8; SET_PACKET_LEN]) -> Result<()> {
        let cmd = packet[0];
        let reply = Self::query_raw(
            &self.hid,
            REPORT_SET_OUT,
            REPORT_SET_IN,
            SET_PACKET_LEN,
            packet,
            |b| b.first() == Some(&cmd) || (b.first() == Some(&ACK_BYTE) && b.get(2) == Some(&cmd)),
        )
        .map_err(|m| self.dev_err(m))?;
        if reply.first() == Some(&ACK_BYTE) && reply.get(1).copied().unwrap_or(0) != 0 {
            return Err(self.dev_err(format!(
                "Keychron rejected command 0x{cmd:02x} (code {})",
                reply[1]
            )));
        }
        Ok(())
    }

    fn read_status(&mut self) -> Result<KeychronStatus> {
        let bytes = self.query_cmd(&[CMD_STATUS], |b| b.first() == Some(&CMD_STATUS))?;
        let st = decode_keychron_status(&bytes, self.proto_version, self.ver_flags)
            .ok_or_else(|| self.dev_err("Invalid Keychron 0x06 status report"))?;
        if st.feature_dpi_limits && st.dpi_max_reported > 100 {
            self.cached_dpi_max = st.dpi_max_reported;
        }
        if st.feature_dpi_limits && st.dpi_step_reported > 0 {
            self.cached_dpi_step = u16::from(st.dpi_step_reported);
        }
        Ok(st)
    }

    fn active_level_byte(&self, st: &KeychronStatus) -> u8 {
        st.levels[usize::from(self.work_mode).min(2)]
    }

    fn probe_identity(&mut self) {
        let fallback_mode = if KEYCHRON_RECEIVER_PIDS.contains(&self.pid) {
            1
        } else {
            0
        };
        if let Ok(ver) = self.query_settings(SET_VERSION, &[fallback_mode]) {
            if ver.len() >= 16 {
                self.work_mode = ver[9] & 0x07;
                self.proto_version = u16::from_le_bytes([ver[1], ver[2]]);
                self.ver_flags = [ver[11], ver[12], ver[13], ver[15]];
                let reported_pid = u16::from_le_bytes([ver[5], ver[6]]);
                if self.model.is_none() {
                    if let Some(m) = lookup_keychron_model(reported_pid) {
                        self.model = Some(m);
                        self.display_name = m.name.to_string();
                        self.cached_dpi_max = m.dpi_max;
                    }
                }
            }
        }
        if self.model.is_none() && self.work_mode == 1 {
            if let Ok(list) = self.query_settings(SET_RECEIVER_STATE, &[]) {
                let count = usize::from(list.get(1).copied().unwrap_or(0))
                    .min(list.len().saturating_sub(2) / 5);
                for idx in 0..count {
                    let off = 2 + idx * 5;
                    if list[off + 4] == 1 {
                        let paired_pid = u16::from_le_bytes([list[off + 2], list[off + 3]]);
                        if let Some(m) = lookup_keychron_model(paired_pid) {
                            self.model = Some(m);
                            self.display_name = m.name.to_string();
                            self.cached_dpi_max = m.dpi_max;
                            break;
                        }
                    }
                }
            }
        }
    }
}

impl MouseDevice for KeychronDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: format!("keychron_{:04x}", self.pid),
            name: self.display_name.clone(),
            vendor: "Keychron".to_string(),
            vid_pid: Some(self.vid_pid.clone()),
            connection: match self.work_mode {
                0 => "Wired USB (Keychron 8K)".to_string(),
                2 => "Bluetooth (Keychron)".to_string(),
                _ => "Wireless 2.4 GHz Receiver (Keychron)".to_string(),
            },
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let max_hz = self.model.map_or(8000, |m| m.max_polling_hz);
        let polling_rates_hz: Vec<u16> = KEYCHRON_POLLING_TABLE
            .iter()
            .copied()
            .filter(|&r| r <= max_hz)
            .collect();
        let no_sensor = self.model.is_some_and(|m| m.no_sensor_options);
        let lod_options = if no_sensor {
            Vec::new()
        } else if self.model.is_none_or(|m| m.has_07mm_lod) {
            LiftOffDistance::ALL.to_vec()
        } else {
            vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20]
        };

        DeviceCapabilities {
            dpi_min: self.model.map_or(100, |m| m.dpi_min),
            dpi_max: self.cached_dpi_max,
            dpi_step: self.cached_dpi_step,
            polling_rates_hz,
            lod_options,
            supports_debounce: true,
            debounce_min_ms: 0,
            debounce_max_ms: DEBOUNCE_MAX_MS,
            supports_motion_sync: !no_sensor,
            supports_angle_snapping: !no_sensor,
            supports_ripple_control: !no_sensor,
            supports_volatile_writes: false,
            flash_write_warning: Some(format!(
                "{} persists profile settings to onboard memory. Polled diffs unchanged values and rate-limits rapid writes.",
                self.display_name
            )),
        }
    }

    fn read_battery(&mut self) -> Option<BatteryStatus> {
        let st = self.read_status().ok()?;
        if st.battery_percent > 100 {
            return None;
        }
        Some(BatteryStatus {
            percent: st.battery_percent,
            charging: st.charging,
            voltage_mv: None,
        })
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        let st = self.read_status()?;
        let level = self.active_level_byte(&st);
        let mut active_stage = usize::from(level & 0x0F).min(usize::from(st.stage_count) - 1);
        let mut dpi = st.dpi_stages[active_stage];

        if st.feature_separate_dpi {
            if let Ok(xy) = self.query_cmd(&[CMD_READ_XY], |b| b.first() == Some(&CMD_READ_XY)) {
                if xy.len() >= 15 {
                    let count = usize::from(if xy[4] == 0 { 5 } else { xy[4].min(5) });
                    let mode_idx = 1 + usize::from(self.work_mode).min(2);
                    active_stage = usize::from(xy[mode_idx] & 0x0F).min(count - 1);
                    let off = 5 + active_stage * 2;
                    dpi = u16::from_le_bytes([xy[off], xy[off + 1]]);
                }
            }
        }

        let polling_rate_hz = if st.feature_separate_polling {
            self.query_settings(SET_READ_POLLING_SETS, &[])
                .ok()
                .and_then(|b| {
                    if b.len() < 17 {
                        return None;
                    }
                    let set_idx = usize::from(self.work_mode == 1);
                    let gear = usize::from(b[1 + set_idx]).min(POLLING_GEARS - 1);
                    let rate_code = usize::from(b[5 + set_idx * POLLING_GEARS + gear]);
                    KEYCHRON_POLLING_TABLE.get(rate_code).copied()
                })
        } else {
            let gear = usize::from((level >> 4) & 0x0F).min(POLLING_GEARS - 1);
            let rate_code = usize::from(st.polling_table[gear]);
            KEYCHRON_POLLING_TABLE.get(rate_code).copied()
        };

        let no_sensor = self.model.is_some_and(|m| m.no_sensor_options);
        let lod = if no_sensor || st.feature_lod_level {
            None
        } else {
            decode_keychron_lod_code(st.lod_code)
        };

        Ok(MouseSettings {
            dpi: Some(dpi),
            polling_rate_hz,
            lod,
            debounce_ms: Some(st.debounce_ms),
            motion_sync: (!no_sensor).then_some(st.motion_sync),
            angle_snapping: (!no_sensor).then_some(st.angle_snapping),
            ripple_control: (!no_sensor).then_some(st.ripple_control),
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        let st = self.read_status()?;
        let level = self.active_level_byte(&st);

        if let Some(target_dpi) = settings.dpi {
            if st.feature_separate_dpi {
                let xy = self.query_cmd(&[CMD_READ_XY], |b| b.first() == Some(&CMD_READ_XY))?;
                if xy.len() >= 38 {
                    let count = if xy[4] == 0 { 5 } else { xy[4].min(5) };
                    let mode_idx = 1 + usize::from(self.work_mode).min(2);
                    let active = (xy[mode_idx] & 0x0F).min(count - 1);
                    let mut pkt = [0u8; CMD_PACKET_LEN];
                    let copy_n = xy.len().min(CMD_PACKET_LEN);
                    pkt[..copy_n].copy_from_slice(&xy[..copy_n]);
                    pkt[0] = CMD_WRITE_XY;
                    let stage_byte = if self.proto_version >= FLAGS_IN_VERSION_REPLY {
                        active
                    } else {
                        (active << 4) | active
                    };
                    pkt[1] = stage_byte;
                    pkt[2] = stage_byte;
                    pkt[3] = stage_byte;
                    pkt[4] = count;
                    let le = target_dpi.to_le_bytes();
                    let x_off = 5 + usize::from(active) * 2;
                    let y_off = 21 + usize::from(active) * 2;
                    pkt[x_off] = le[0];
                    pkt[x_off + 1] = le[1];
                    pkt[y_off] = le[0];
                    pkt[y_off + 1] = le[1];
                    let ack = self.query_cmd(&pkt, |b| {
                        b.first() == Some(&ACK_BYTE) && b.get(2) == Some(&CMD_WRITE_XY)
                    })?;
                    if ack.get(1).copied().unwrap_or(0) != 0 {
                        return Err(self.dev_err("Keychron rejected 0x48 XY DPI write"));
                    }
                }
            } else {
                let active = (level & 0x0F).min(st.stage_count - 1);
                let mut stages = st.dpi_stages;
                if stages[usize::from(active)] != target_dpi {
                    stages[usize::from(active)] = target_dpi;
                    let pkt = encode_keychron_dpi_packet(active, &stages, st.stage_count);
                    self.write_settings(&pkt)?;
                }
            }
        }

        if let Some(target_hz) = settings.polling_rate_hz {
            let rate_code = KEYCHRON_POLLING_TABLE
                .iter()
                .position(|&r| r == target_hz)
                .ok_or_else(|| PolledError::UnsupportedSetting {
                    device: self.display_name.clone(),
                    detail: format!("Unsupported Keychron polling rate {target_hz} Hz"),
                })? as u8;

            if st.feature_separate_polling {
                let b = self.query_settings(SET_READ_POLLING_SETS, &[])?;
                if b.len() >= 17 {
                    let set_idx = usize::from(self.work_mode == 1);
                    let mut pkt = [0u8; SET_PACKET_LEN];
                    pkt[0] = SET_WRITE_POLLING_SETS;
                    pkt[1..17].copy_from_slice(&b[1..17]);
                    let count = usize::from(pkt[3 + set_idx].clamp(1, POLLING_GEARS as u8));
                    let table_start = 5 + set_idx * POLLING_GEARS;
                    if let Some(gear) = pkt[table_start..table_start + count]
                        .iter()
                        .position(|&c| c == rate_code)
                    {
                        pkt[1 + set_idx] = gear as u8;
                    } else if st.feature_polling_gears {
                        let gear = usize::from(pkt[1 + set_idx]).min(count - 1);
                        pkt[table_start + gear] = rate_code;
                    }
                    self.write_settings(&pkt)?;
                }
            } else {
                let count = usize::from(st.polling_count.clamp(1, POLLING_GEARS as u8));
                let mut table = st.polling_table;
                let mut gear = usize::from((level >> 4) & 0x0F).min(count - 1);
                if let Some(existing_gear) = table[..count].iter().position(|&c| c == rate_code) {
                    gear = existing_gear;
                } else if st.feature_polling_gears {
                    table[gear] = rate_code;
                }
                let mut pkt = [0u8; SET_PACKET_LEN];
                pkt[0] = SET_POLLING;
                pkt[1] = gear as u8;
                pkt[2] = gear as u8;
                pkt[3..3 + count].copy_from_slice(&table[..count]);
                pkt[9] = count as u8;
                self.write_settings(&pkt)?;
            }
        }

        if let Some(target_db) = settings.debounce_ms {
            if st.debounce_ms != target_db {
                let mut pkt = [0u8; SET_PACKET_LEN];
                pkt[0] = SET_DEBOUNCE;
                pkt[1] = target_db;
                self.write_settings(&pkt)?;
            }
        }

        let target_lod_code = settings.lod.map(encode_keychron_lod_code);
        let sensor_dirty = target_lod_code.is_some_and(|c| c != st.lod_code)
            || settings.motion_sync.is_some_and(|v| v != st.motion_sync)
            || settings
                .angle_snapping
                .is_some_and(|v| v != st.angle_snapping)
            || settings
                .ripple_control
                .is_some_and(|v| v != st.ripple_control);

        if sensor_dirty {
            let pkt = encode_keychron_sensor_packet(
                if st.feature_lod_level {
                    0
                } else {
                    target_lod_code.unwrap_or(st.lod_code)
                },
                settings.ripple_control.unwrap_or(st.ripple_control),
                settings.angle_snapping.unwrap_or(st.angle_snapping),
                settings.motion_sync.unwrap_or(st.motion_sync),
                st.scroll_reversed,
                Some(st.max_speed),
                if st.feature_lod_level {
                    Some(st.lod_level)
                } else {
                    Some(0)
                },
            );
            self.write_settings(&pkt)?;
        }

        Ok(())
    }
}

/// Protocol-family auto-detector for Keychron Launcher 8K mice (`0x3434`, `0xFFC1`).
pub struct KeychronProtocolDriver;

impl DeviceDriver for KeychronProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "Keychron Launcher 8K (0x3434)"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();
        for dev in api.device_list() {
            let vid = dev.vendor_id();
            let pid = dev.product_id();
            if vid != KEYCHRON_VID
                || EXCLUDED_8K_NORDIC_PIDS.contains(&pid)
                || dev.usage_page() != KEYCHRON_USAGE_PAGE
                || dev.usage() != KEYCHRON_USAGE
            {
                continue;
            }
            let Ok(hid) = dev.open_device(api) else {
                continue;
            };
            let mut desc_buf = [0u8; 4096];
            let has_8k_reports = hid
                .get_report_descriptor(&mut desc_buf)
                .ok()
                .map(|len| parse_hid_report_descriptor(&desc_buf[..len]))
                .is_some_and(|s| {
                    s.output_reports.contains_key(&REPORT_CMD_OUT)
                        && s.input_reports.contains_key(&REPORT_CMD_IN)
                });
            if !has_8k_reports {
                continue;
            }

            let model = lookup_keychron_model(pid);
            let display_name = model
                .map(|m| m.name.to_string())
                .or_else(|| {
                    dev.product_string()
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| s.trim().to_string())
                })
                .unwrap_or_else(|| format!("Keychron Mouse ({pid:04x})"));
            let fallback_mode = if KEYCHRON_RECEIVER_PIDS.contains(&pid) {
                1
            } else {
                0
            };

            let mut device = KeychronDevice {
                hid,
                pid,
                work_mode: fallback_mode,
                proto_version: 0,
                ver_flags: [0; 4],
                model,
                display_name,
                vid_pid: format!("{vid:04x}:{pid:04x}"),
                cached_dpi_max: model.map_or(26_000, |m| m.dpi_max),
                cached_dpi_step: 50,
            };
            device.probe_identity();
            found.push(Box::new(device));
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_keychron_status_and_encodes_dpi_and_sensor_packets() {
        let mut raw = [0u8; 63];
        raw[0] = CMD_STATUS;
        raw[2] = 0x21; // USB: gear 2 (1000Hz), stage 1
        raw[5..7].copy_from_slice(&400u16.to_le_bytes());
        raw[7..9].copy_from_slice(&1600u16.to_le_bytes());
        raw[15] = 0x03 | 0x10 | 0x04; // 0.7mm LOD + Motion Sync + Ripple
        raw[16] = 4;
        raw[17] = 3; // debounce 3ms
        raw[19] = 0x80 | 68; // 68% charging
        raw[43..49].copy_from_slice(&[0, 1, 2, 3, 4, 5]);
        raw[49] = 6;

        let st = decode_keychron_status(&raw, 0, [0; 4]).unwrap();
        assert_eq!(st.dpi_stages[1], 1600);
        assert_eq!(st.lod_code, 3);
        assert!(st.motion_sync);
        assert!(st.ripple_control);
        assert!(!st.angle_snapping);
        assert_eq!(st.debounce_ms, 3);
        assert_eq!(st.battery_percent, 68);
        assert!(st.charging);

        let dpi_pkt = encode_keychron_dpi_packet(1, &[400, 1600, 3200, 6400, 800], 4);
        assert_eq!(dpi_pkt[0], SET_DPI);
        assert_eq!(u16::from_le_bytes([dpi_pkt[6], dpi_pkt[7]]), 1600);
        assert_eq!(dpi_pkt[14], 4);

        let sensor_pkt =
            encode_keychron_sensor_packet(3, true, false, true, false, Some(false), Some(0));
        assert_eq!(sensor_pkt[0], SET_SENSOR);
        assert_eq!(sensor_pkt[1], 3);
        assert_eq!(sensor_pkt[2], 1); // ripple on
        assert_eq!(sensor_pkt[3], 2); // angle snap off
        assert_eq!(sensor_pkt[4], 1); // motion sync on

        assert_eq!(
            lookup_keychron_model(0xD050).unwrap().name,
            "Keychron M3 8K"
        );
    }
}
