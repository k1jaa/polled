// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Endgame Gear OP1 8K / XM2 8K / OP1w 4K / XM2w 4K (v1 & v2) HID feature-report driver.
//!
//! # Protocol Attribution & Licenses (AGPL-3.0 compatible)
//! Product catalog, Report `0xA0`/`0xA1` framing, config blob offsets, wireless 4K block writes,
//! Windows wire-header normalization, and CPI/LOD rules are ported from:
//! - `OpenMouse-Project/mouse-protocol` (<https://github.com/OpenMouse-Project/mouse-protocol>)
//!   - `src/endgame-gear/op1.ts`, `src/drivers/endgame/egg-op1-hid.ts`
//!   - **AGPL-3.0-or-later**, Copyright (C) OpenMouse Project Contributors
//! - `johanneszab/endgame-op1w` protocol documentation referenced therein.
//!
//! No HID bytes are guessed. Factory reset (`0x13`) is never issued.

use std::thread;
use std::time::Duration;

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice, MouseSettings,
    SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};
use crate::hid::parse_hid_report_descriptor;

pub const EGG_VENDOR_ID: u16 = 0x3367;
/// Shared 2.4 GHz dongle PID used by OP1w/XM2w 4K v1 and v2.
pub const EGG_4K_DONGLE_PID: u16 = 0x1970;

pub const EGG_REPORT_CONFIG: u8 = 0xA0;
pub const EGG_REPORT_COMMAND: u8 = 0xA1;
pub const EGG_CONFIG_SIZE: usize = 1041;
pub const EGG_COMMAND_SIZE: usize = 64;

const OP_MOUSE_INFO: u8 = 0x0E;
const OP_STORE: u8 = 0x11;
const OP_LOAD: u8 = 0x12;
const OP_WRITE_SENSOR: u8 = 0x14;
const OP_WRITE_POWER: u8 = 0x15;
const OP_WRITE_BUTTONS: u8 = 0x16;

const STATUS_OK: u8 = 0x01;
const STATUS_BUSY: u8 = 0x03;
const STATUS_REJECTED: u8 = 0x07;
const STATUS_SLEEP: u8 = 0x08;

const OFFSET_POLLING_DIVIDER: usize = 21;
const OFFSET_LOD: usize = 25;
const OFFSET_MOTION_SYNC: usize = 28;
const OFFSET_CPI_LEVELS: usize = 30;
const OFFSET_FIRST_CPI_SPLIT: usize = 51;
const OFFSET_GLASS_MODE: usize = 127;

const POWER_SAVE_POLLING_CODE: u8 = 0x80;

const RATES_WIRED_8K: &[u16] = &[125, 250, 500, 1000, 2000, 4000, 8000];
const RATES_4K_V1: &[u16] = &[1000, 2000, 4000];
const RATES_4K_V2: &[u16] = &[125, 1000, 2000, 4000];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EggConfigFamily {
    V1,
    V2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EggDeviceProfile {
    pub pid: u16,
    pub name: &'static str,
    pub config_family: EggConfigFamily,
    pub cpi_min: u16,
    pub cpi_max: u16,
    pub cpi_step_low: u16,
    pub cpi_step_high: u16,
    pub has_glass_mode: bool,
    pub motion_sync_at_8k: bool,
    pub max_polling_hz: u16,
    pub wireless_4k: bool,
}

impl EggDeviceProfile {
    pub fn is_4k_v2(self) -> bool {
        self.wireless_4k && self.config_family == EggConfigFamily::V2
    }

    pub fn supported_polling_rates(self) -> Vec<u16> {
        if self.wireless_4k {
            if self.is_4k_v2() {
                RATES_4K_V2.to_vec()
            } else {
                RATES_4K_V1.to_vec()
            }
        } else {
            RATES_WIRED_8K
                .iter()
                .copied()
                .filter(|&hz| hz <= self.max_polling_hz)
                .collect()
        }
    }

    pub fn cpi_step_for(self, cpi: u16) -> u16 {
        if self.config_family == EggConfigFamily::V2 && cpi <= 10_000 {
            self.cpi_step_low
        } else {
            self.cpi_step_high
        }
    }

    pub fn is_valid_cpi(self, cpi: u16) -> bool {
        if cpi < self.cpi_min || cpi > self.cpi_max {
            return false;
        }
        let step = self.cpi_step_for(cpi);
        step == 0 || cpi.is_multiple_of(step)
    }

    pub fn lod_options(self) -> Vec<LiftOffDistance> {
        if self.wireless_4k && self.config_family == EggConfigFamily::V1 {
            vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20]
        } else if self.config_family == EggConfigFamily::V2 {
            // V2 normal scale exposes 0.7..=1.7 mm (11 steps); 0.7 mm and 1.0 mm map cleanly.
            vec![LiftOffDistance::Mm07, LiftOffDistance::Mm10]
        } else {
            vec![
                LiftOffDistance::Mm07,
                LiftOffDistance::Mm10,
                LiftOffDistance::Mm20,
            ]
        }
    }
}

pub const EGG_PROFILES: &[EggDeviceProfile] = &[
    EggDeviceProfile {
        pid: 0x1964,
        name: "Endgame Gear OP1 8K",
        config_family: EggConfigFamily::V1,
        cpi_min: 50,
        cpi_max: 26_000,
        cpi_step_low: 50,
        cpi_step_high: 50,
        has_glass_mode: false,
        motion_sync_at_8k: false,
        max_polling_hz: 8000,
        wireless_4k: false,
    },
    EggDeviceProfile {
        pid: 0x1966,
        name: "Endgame Gear XM2 8K",
        config_family: EggConfigFamily::V1,
        cpi_min: 50,
        cpi_max: 26_000,
        cpi_step_low: 50,
        cpi_step_high: 50,
        has_glass_mode: false,
        motion_sync_at_8k: false,
        max_polling_hz: 8000,
        wireless_4k: false,
    },
    EggDeviceProfile {
        pid: 0x1976,
        name: "Endgame Gear OP1 8K Purple Frost",
        config_family: EggConfigFamily::V1,
        cpi_min: 50,
        cpi_max: 30_000,
        cpi_step_low: 50,
        cpi_step_high: 50,
        has_glass_mode: true,
        motion_sync_at_8k: true,
        max_polling_hz: 8000,
        wireless_4k: false,
    },
    EggDeviceProfile {
        pid: 0x1978,
        name: "Endgame Gear OP1 8K v2",
        config_family: EggConfigFamily::V2,
        cpi_min: 10,
        cpi_max: 30_000,
        cpi_step_low: 10,
        cpi_step_high: 50,
        has_glass_mode: true,
        motion_sync_at_8k: true,
        max_polling_hz: 8000,
        wireless_4k: false,
    },
    EggDeviceProfile {
        pid: 0x1980,
        name: "Endgame Gear XM2 8K v2",
        config_family: EggConfigFamily::V2,
        cpi_min: 10,
        cpi_max: 30_000,
        cpi_step_low: 10,
        cpi_step_high: 50,
        has_glass_mode: true,
        motion_sync_at_8k: true,
        max_polling_hz: 8000,
        wireless_4k: false,
    },
    EggDeviceProfile {
        pid: 0x1972,
        name: "Endgame Gear OP1w 4K",
        config_family: EggConfigFamily::V1,
        cpi_min: 50,
        cpi_max: 26_000,
        cpi_step_low: 50,
        cpi_step_high: 50,
        has_glass_mode: false,
        motion_sync_at_8k: true,
        max_polling_hz: 4000,
        wireless_4k: true,
    },
    EggDeviceProfile {
        pid: 0x1968,
        name: "Endgame Gear XM2w 4K",
        config_family: EggConfigFamily::V1,
        cpi_min: 50,
        cpi_max: 26_000,
        cpi_step_low: 50,
        cpi_step_high: 50,
        has_glass_mode: false,
        motion_sync_at_8k: true,
        max_polling_hz: 4000,
        wireless_4k: true,
    },
    EggDeviceProfile {
        pid: 0x1984,
        name: "Endgame Gear OP1w 4K v2",
        config_family: EggConfigFamily::V2,
        cpi_min: 10,
        cpi_max: 30_000,
        cpi_step_low: 10,
        cpi_step_high: 50,
        has_glass_mode: true,
        motion_sync_at_8k: true,
        max_polling_hz: 4000,
        wireless_4k: true,
    },
    EggDeviceProfile {
        pid: 0x1982,
        name: "Endgame Gear XM2w 4K v2",
        config_family: EggConfigFamily::V2,
        cpi_min: 10,
        cpi_max: 30_000,
        cpi_step_low: 10,
        cpi_step_high: 50,
        has_glass_mode: true,
        motion_sync_at_8k: true,
        max_polling_hz: 4000,
        wireless_4k: true,
    },
    EggDeviceProfile {
        pid: EGG_4K_DONGLE_PID,
        name: "Endgame Gear OP1w/XM2w 4K",
        config_family: EggConfigFamily::V2,
        cpi_min: 10,
        cpi_max: 30_000,
        cpi_step_low: 10,
        cpi_step_high: 50,
        has_glass_mode: true,
        motion_sync_at_8k: true,
        max_polling_hz: 4000,
        wireless_4k: true,
    },
];

pub fn lookup_egg_profile(pid: u16) -> Option<EggDeviceProfile> {
    EGG_PROFILES.iter().copied().find(|p| p.pid == pid)
}

/// Normalizes a received Endgame Gear feature report across Windows descriptor quirks.
pub fn normalize_egg_feature_report(
    raw: &[u8],
    report_id: u8,
    expected_total: usize,
    payload_len: usize,
) -> Vec<u8> {
    let has_wire_header = raw.len() == payload_len
        && raw.len() >= 2
        && (raw[0] == 0 || raw[0] == EGG_REPORT_CONFIG || raw[0] == EGG_REPORT_COMMAND)
        && matches!(
            raw[1],
            STATUS_OK | STATUS_BUSY | STATUS_REJECTED | STATUS_SLEEP
        );
    if has_wire_header {
        let mut out = vec![0u8; expected_total.max(raw.len())];
        out[..raw.len()].copy_from_slice(raw);
        out[0] = report_id;
        return out;
    }

    let includes_id = if payload_len > 0 && raw.len() == payload_len {
        false
    } else if payload_len > 0 && raw.len() == payload_len + 1 && raw[0] == report_id {
        true
    } else {
        !raw.is_empty() && raw[0] == report_id
    };

    let mut out = vec![0u8; expected_total.max(raw.len() + usize::from(!includes_id))];
    if includes_id {
        out[..raw.len()].copy_from_slice(raw);
    } else {
        out[0] = report_id;
        out[1..1 + raw.len()].copy_from_slice(raw);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EggBlockWrite {
    pub command: u8,
    pub declared_length: u8,
    pub chunk: u8,
    pub payload: Vec<u8>,
}

/// Builds the 4K wireless block writes (sensor, power, buttons chunk 1/2) from a 1041B config blob.
pub fn build_egg_4k_block_writes(config: &[u8], has_glass_mode: bool) -> Vec<EggBlockWrite> {
    let at = |offset: usize| -> u8 { config.get(16 + offset).copied().unwrap_or(0) };

    let mut sensor = vec![0u8; 28];
    sensor[..8].copy_from_slice(&[
        at(0x07),
        at(0x08),
        at(0x09),
        at(0x0A),
        at(0x0B),
        at(0x01),
        at(0x0E),
        at(0x0D),
    ]);
    if config.len() >= 16 + 0x37 {
        sensor[8..28].copy_from_slice(&config[16 + 0x23..16 + 0x37]);
    }

    let mut power = Vec::with_capacity(if has_glass_mode { 11 } else { 10 });
    power.extend_from_slice(&[at(0x0C), at(0x05), at(0x06), at(0x04)]);
    for btn in 0..5 {
        power.push(at(0x3D + btn * 7));
    }
    power.push(at(0x03));
    if has_glass_mode {
        power.push(at(0x6F));
    }

    let buttons = if config.len() >= 16 + 0x6F {
        &config[16 + 0x37..16 + 0x6F]
    } else {
        &[0u8; 56]
    };

    vec![
        EggBlockWrite {
            command: OP_WRITE_SENSOR,
            declared_length: 28,
            chunk: 0,
            payload: sensor,
        },
        EggBlockWrite {
            command: OP_WRITE_POWER,
            declared_length: 10,
            chunk: 0,
            payload: power,
        },
        EggBlockWrite {
            command: OP_WRITE_BUTTONS,
            declared_length: 28,
            chunk: 1,
            payload: buttons[..28].to_vec(),
        },
        EggBlockWrite {
            command: OP_WRITE_BUTTONS,
            declared_length: 28,
            chunk: 2,
            payload: buttons[28..56].to_vec(),
        },
    ]
}

fn read_u16_le(bytes: &[u8], offset: usize) -> u16 {
    let lo = bytes.get(offset).copied().unwrap_or(0);
    let hi = bytes.get(offset + 1).copied().unwrap_or(0);
    u16::from_le_bytes([lo, hi])
}

fn write_u16_le(bytes: &mut [u8], offset: usize, value: u16) {
    let le = value.to_le_bytes();
    if offset + 1 < bytes.len() {
        bytes[offset] = le[0];
        bytes[offset + 1] = le[1];
    }
}

/// Updates all enabled CPI stages in an Endgame Gear config buffer (wired 8K does not expose active stage).
pub fn write_enabled_cpi_stages(config: &mut [u8], cpi: u16) {
    let levels = usize::from(config[OFFSET_CPI_LEVELS].clamp(1, 4));
    for level in 0..levels {
        let offset = OFFSET_FIRST_CPI_SPLIT + level * 5;
        config[offset] = 0; // X == Y
        write_u16_le(config, offset + 1, cpi);
        write_u16_le(config, offset + 3, cpi);
    }
}

pub fn decode_egg_polling_rate(profile: EggDeviceProfile, divider: u8) -> Option<u16> {
    if divider == 0 {
        return None;
    }
    if profile.wireless_4k && divider == POWER_SAVE_POLLING_CODE {
        return Some(1000);
    }
    Some(8000 / u16::from(divider))
}

fn lod_base(profile: EggDeviceProfile, glass_mode: bool) -> u8 {
    if profile.wireless_4k && (glass_mode || !profile.is_4k_v2()) {
        1
    } else {
        0
    }
}

pub fn decode_egg_lod(
    profile: EggDeviceProfile,
    raw_lod: u8,
    glass_mode: bool,
) -> Option<LiftOffDistance> {
    let idx = raw_lod.checked_sub(lod_base(profile, glass_mode))?;
    if glass_mode && profile.has_glass_mode {
        return match idx {
            0 => Some(LiftOffDistance::Mm10),
            1 => Some(LiftOffDistance::Mm20),
            _ => None,
        };
    }
    if profile.wireless_4k && profile.config_family == EggConfigFamily::V1 {
        return match idx {
            0 => Some(LiftOffDistance::Mm10),
            1 => Some(LiftOffDistance::Mm20),
            _ => None,
        };
    }
    match profile.config_family {
        EggConfigFamily::V1 => match idx {
            0 => Some(LiftOffDistance::Mm07),
            1 => Some(LiftOffDistance::Mm10),
            2 => Some(LiftOffDistance::Mm20),
            _ => None,
        },
        EggConfigFamily::V2 => match idx {
            0 => Some(LiftOffDistance::Mm07), // 0.7 mm
            3 => Some(LiftOffDistance::Mm10), // 1.0 mm
            _ => None,
        },
    }
}

pub fn encode_egg_lod(
    profile: EggDeviceProfile,
    lod: LiftOffDistance,
    glass_mode: bool,
) -> Option<u8> {
    let idx = if (glass_mode && profile.has_glass_mode)
        || (profile.wireless_4k && profile.config_family == EggConfigFamily::V1)
    {
        match lod {
            LiftOffDistance::Mm10 => Some(0u8),
            LiftOffDistance::Mm20 => Some(1),
            LiftOffDistance::Mm07 => None,
        }
    } else {
        match profile.config_family {
            EggConfigFamily::V1 => match lod {
                LiftOffDistance::Mm07 => Some(0),
                LiftOffDistance::Mm10 => Some(1),
                LiftOffDistance::Mm20 => Some(2),
            },
            EggConfigFamily::V2 => match lod {
                LiftOffDistance::Mm07 => Some(0),
                LiftOffDistance::Mm10 => Some(3),
                LiftOffDistance::Mm20 => None,
            },
        }
    }?;
    Some(idx + lod_base(profile, glass_mode))
}

/// Hardware driver for Endgame Gear OP1/XM2 8K & 4K mice.
pub struct EndgameGearOp1Device {
    hid: HidDevice,
    vid_pid: String,
    profile: EggDeviceProfile,
    model_known: bool,
    config_payload_len: usize,
    command_payload_len: usize,
}

impl EndgameGearOp1Device {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.profile.name.to_string(),
            message: message.into(),
        }
    }

    fn send_command(&self, operation: u8, header: &[u8], payload: Option<&[u8]>) -> Result<()> {
        let mut buf = vec![0u8; self.command_payload_len + 1];
        buf[0] = EGG_REPORT_COMMAND;
        buf[1] = operation;
        let hdr_n = header.len().min(buf.len().saturating_sub(2));
        buf[2..2 + hdr_n].copy_from_slice(&header[..hdr_n]);
        if let Some(p) = payload {
            // Wire offset 16 = index 16 when report ID is at index 0.
            let n = p.len().min(buf.len().saturating_sub(16));
            buf[16..16 + n].copy_from_slice(&p[..n]);
        }
        self.hid
            .send_feature_report(&buf)
            .map_err(|e| self.dev_err(format!("EGG command 0x{operation:02x} write failed: {e}")))
    }

    fn receive_feature(
        &self,
        report_id: u8,
        expected_total: usize,
        payload_len: usize,
    ) -> Result<(Vec<u8>, usize)> {
        let mut buf = vec![0u8; expected_total.max(payload_len + 1)];
        buf[0] = report_id;
        let n = self
            .hid
            .get_feature_report(&mut buf)
            .map_err(|e| self.dev_err(format!("EGG feature read 0x{report_id:02x} failed: {e}")))?;
        let norm = normalize_egg_feature_report(&buf[..n], report_id, expected_total, payload_len);
        Ok((norm, n))
    }

    fn poll_command_ok(&self, attempts: usize) -> bool {
        let mut backoff = 60u64;
        for attempt in 0..attempts {
            if let Ok((resp, _)) = self.receive_feature(
                EGG_REPORT_COMMAND,
                EGG_COMMAND_SIZE,
                self.command_payload_len,
            ) {
                if resp.get(1) == Some(&STATUS_OK) {
                    return true;
                }
                if resp.get(1) != Some(&STATUS_BUSY) && attempt > 1 {
                    return false;
                }
            }
            thread::sleep(Duration::from_millis(backoff));
            backoff = (backoff * 2).min(400);
        }
        false
    }

    fn identify_paired_mouse(&mut self) {
        if self.model_known {
            return;
        }
        if self.send_command(OP_MOUSE_INFO, &[], None).is_err() {
            return;
        }
        thread::sleep(Duration::from_millis(150));
        for attempt in 0..3u64 {
            if let Ok((reply, _)) = self.receive_feature(
                EGG_REPORT_COMMAND,
                EGG_COMMAND_SIZE,
                self.command_payload_len,
            ) {
                if reply.get(1) == Some(&STATUS_OK) && read_u16_le(&reply, 16) == EGG_VENDOR_ID {
                    let paired_pid = read_u16_le(&reply, 18);
                    if let Some(paired) = lookup_egg_profile(paired_pid) {
                        if paired.wireless_4k {
                            self.profile = paired;
                        }
                    }
                    self.model_known = true;
                    return;
                }
                if !matches!(reply.get(1), Some(&STATUS_BUSY) | Some(&STATUS_OK)) {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(200 * (attempt + 1)));
        }
    }

    fn is_valid_config(&self, config: &[u8], raw_len: usize) -> bool {
        if raw_len < 131 || config.len() < 131 {
            return false;
        }
        let stages = config[OFFSET_CPI_LEVELS];
        let divider = config[OFFSET_POLLING_DIVIDER];
        let cpi = read_u16_le(config, OFFSET_FIRST_CPI_SPLIT + 1);
        config[1] == STATUS_OK
            && (1..=4).contains(&stages)
            && divider >= 1
            && self.profile.is_valid_cpi(cpi)
    }

    fn read_config_raw(&self) -> Result<Vec<u8>> {
        for _ in 0..3 {
            self.send_command(OP_LOAD, &[], None)?;
            thread::sleep(Duration::from_millis(80));
            let mut backoff = 60u64;
            for _ in 0..6 {
                for report_id in [EGG_REPORT_COMMAND, EGG_REPORT_CONFIG] {
                    if let Ok((mut bytes, raw_len)) =
                        self.receive_feature(report_id, EGG_CONFIG_SIZE, self.config_payload_len)
                    {
                        if self.is_valid_config(&bytes, raw_len) {
                            bytes[0] = EGG_REPORT_CONFIG;
                            return Ok(bytes);
                        }
                    }
                }
                thread::sleep(Duration::from_millis(backoff));
                backoff = (backoff * 2).min(400);
            }
        }
        Err(self.dev_err("Did not return a valid Endgame Gear configuration (move mouse to wake)"))
    }

    fn write_config_raw(&self, config: &[u8]) -> Result<()> {
        let mut send_buf = vec![0u8; self.config_payload_len + 1];
        send_buf[0] = EGG_REPORT_CONFIG;
        let copy_len = config.len().saturating_sub(1).min(self.config_payload_len);
        send_buf[1..1 + copy_len].copy_from_slice(&config[1..1 + copy_len]);
        send_buf[1] = OP_STORE;

        let mut last_err = None;
        for _ in 0..4 {
            match self.hid.send_feature_report(&send_buf) {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    last_err = Some(e.to_string());
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
        if let Some(err) = last_err {
            return Err(self.dev_err(format!("EGG config write failed: {err}")));
        }
        thread::sleep(Duration::from_millis(300));
        if !self.poll_command_ok(8) {
            return Err(self.dev_err("Endgame Gear mouse did not acknowledge config write"));
        }
        Ok(())
    }

    fn write_config_blocks(&self, before: &[u8], after: &[u8]) -> Result<()> {
        let old = build_egg_4k_block_writes(before, self.profile.has_glass_mode);
        let next = build_egg_4k_block_writes(after, self.profile.has_glass_mode);

        let changed_cmds: Vec<u8> = next
            .iter()
            .zip(old.iter())
            .filter(|(n, o)| n.payload != o.payload)
            .map(|(n, _)| n.command)
            .collect();

        for w in next.iter().filter(|w| changed_cmds.contains(&w.command)) {
            self.send_command(
                w.command,
                &[0x0F, w.declared_length, 0x00, 0x00, w.chunk],
                Some(&w.payload),
            )?;
            thread::sleep(Duration::from_millis(150));
            if !self.poll_command_ok(8) {
                return Err(self.dev_err("Endgame Gear 4K mouse did not acknowledge block write"));
            }
        }
        Ok(())
    }
}

impl MouseDevice for EndgameGearOp1Device {
    fn info(&self) -> DeviceInfo {
        let slug = format!("egg_{:04x}", self.profile.pid);
        DeviceInfo {
            id: slug,
            name: self.profile.name.to_string(),
            vendor: "Endgame Gear".to_string(),
            vid_pid: Some(self.vid_pid.clone()),
            connection: if self.profile.wireless_4k {
                "Wireless 2.4 GHz (4K)".to_string()
            } else {
                "Wired USB (8K)".to_string()
            },
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            dpi_min: self.profile.cpi_min,
            dpi_max: self.profile.cpi_max,
            dpi_step: self.profile.cpi_step_high,
            polling_rates_hz: self.profile.supported_polling_rates(),
            lod_options: self.profile.lod_options(),
            supports_debounce: false,
            debounce_min_ms: 0,
            debounce_max_ms: 0,
            supports_volatile_writes: false,
            flash_write_warning: Some(format!(
                "{} stores settings in onboard flash. Polled diffs unchanged values and rate-limits rapid writes.",
                self.profile.name
            )),
        }
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        let config = self.read_config_raw()?;
        if !self.model_known {
            self.identify_paired_mouse();
        }
        let dpi = read_u16_le(&config, OFFSET_FIRST_CPI_SPLIT + 1);
        let polling_rate_hz = decode_egg_polling_rate(self.profile, config[OFFSET_POLLING_DIVIDER])
            .ok_or_else(|| self.dev_err("Invalid Endgame Gear polling divider"))?;
        let glass_mode = self.profile.has_glass_mode && config[OFFSET_GLASS_MODE] != 0;
        let lod = decode_egg_lod(self.profile, config[OFFSET_LOD], glass_mode);

        Ok(MouseSettings {
            dpi: Some(dpi),
            polling_rate_hz: Some(polling_rate_hz),
            lod,
            debounce_ms: None,
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        if !self.model_known {
            self.identify_paired_mouse();
        }
        if !self.model_known {
            return Err(self.dev_err(
                "Paired 4K mouse behind dongle has not identified itself yet (move mouse to wake)",
            ));
        }

        let mut config = self.read_config_raw()?;
        let before = config.clone();
        let glass_mode = self.profile.has_glass_mode && config[OFFSET_GLASS_MODE] != 0;

        if let Some(target_dpi) = settings.dpi {
            if !self.profile.is_valid_cpi(target_dpi) {
                return Err(PolledError::UnsupportedSetting {
                    device: self.profile.name.to_string(),
                    detail: format!("CPI {target_dpi} is not valid for {}", self.profile.name),
                });
            }
            write_enabled_cpi_stages(&mut config, target_dpi);
        }

        if let Some(target_hz) = settings.polling_rate_hz {
            if glass_mode {
                return Err(self.dev_err(
                    "Polling rate cannot be changed while Glass Mode is active on this mouse",
                ));
            }
            let divider =
                u8::try_from(8000 / target_hz).map_err(|_| PolledError::UnsupportedSetting {
                    device: self.profile.name.to_string(),
                    detail: format!("Unsupported polling rate {target_hz} Hz"),
                })?;
            config[OFFSET_POLLING_DIVIDER] = divider;
            if target_hz == 8000 && !self.profile.motion_sync_at_8k {
                config[OFFSET_MOTION_SYNC] = 0;
            }
        }

        if let Some(target_lod) = settings.lod {
            let encoded =
                encode_egg_lod(self.profile, target_lod, glass_mode).ok_or_else(|| {
                    PolledError::UnsupportedSetting {
                        device: self.profile.name.to_string(),
                        detail: format!("LOD {target_lod} not supported in current sensor mode"),
                    }
                })?;
            config[OFFSET_LOD] = encoded;
        }

        if config == before {
            return Ok(());
        }

        if self.profile.wireless_4k {
            self.write_config_blocks(&before, &config)
        } else {
            self.write_config_raw(&config)
        }
    }
}

/// Protocol-family auto-detector for Endgame Gear OP1/XM2 8K & 4K mice.
pub struct EndgameGearOp1ProtocolDriver;

impl DeviceDriver for EndgameGearOp1ProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "Endgame Gear OP1/XM2 8K & 4K (0x3367)"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();

        for dev in api.device_list() {
            if dev.vendor_id() != EGG_VENDOR_ID {
                continue;
            }
            let Some(profile) = lookup_egg_profile(dev.product_id()) else {
                continue;
            };
            let Ok(hid) = dev.open_device(api) else {
                continue;
            };

            let mut desc_buf = [0u8; 4096];
            let Ok(desc_len) = hid.get_report_descriptor(&mut desc_buf) else {
                continue;
            };
            let summary = parse_hid_report_descriptor(&desc_buf[..desc_len]);
            // Must declare Feature Report 0xA1 (keeps 0x1970 dongle separate from older OP1we).
            let Some(&cmd_total) = summary.feature_reports.get(&EGG_REPORT_COMMAND) else {
                continue;
            };
            let cfg_total = summary
                .feature_reports
                .get(&EGG_REPORT_CONFIG)
                .copied()
                .unwrap_or(EGG_CONFIG_SIZE);

            let mut device = EndgameGearOp1Device {
                hid,
                vid_pid: format!("{:04x}:{:04x}", EGG_VENDOR_ID, profile.pid),
                profile,
                model_known: profile.pid != EGG_4K_DONGLE_PID,
                config_payload_len: cfg_total.saturating_sub(1).max(EGG_CONFIG_SIZE - 1),
                command_payload_len: cmd_total.saturating_sub(1).max(EGG_COMMAND_SIZE - 1),
            };
            if !device.model_known {
                device.identify_paired_mouse();
            }
            found.push(Box::new(device));
        }

        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_windows_wire_header_and_raw_payloads() {
        let mut raw = vec![0u8; 63];
        raw[0] = EGG_REPORT_COMMAND;
        raw[1] = STATUS_OK;
        raw[16] = 0x67;
        raw[17] = 0x33;
        let norm = normalize_egg_feature_report(&raw, EGG_REPORT_COMMAND, EGG_COMMAND_SIZE, 63);
        assert_eq!(norm[0], EGG_REPORT_COMMAND);
        assert_eq!(norm[1], STATUS_OK);
        assert_eq!(read_u16_le(&norm, 16), EGG_VENDOR_ID);
    }

    #[test]
    fn encodes_and_decodes_polling_lod_and_cpi_stages() {
        let op1_v2 = lookup_egg_profile(0x1978).unwrap();
        assert!(op1_v2.is_valid_cpi(810));
        assert!(!op1_v2.is_valid_cpi(10_010));
        assert!(op1_v2.is_valid_cpi(10_050));
        assert_eq!(decode_egg_polling_rate(op1_v2, 1), Some(8000));
        assert_eq!(decode_egg_polling_rate(op1_v2, 8), Some(1000));

        let mut cfg = vec![0u8; EGG_CONFIG_SIZE];
        cfg[OFFSET_CPI_LEVELS] = 3;
        write_enabled_cpi_stages(&mut cfg, 1600);
        for lvl in 0..3 {
            let off = OFFSET_FIRST_CPI_SPLIT + lvl * 5;
            assert_eq!(read_u16_le(&cfg, off + 1), 1600);
            assert_eq!(read_u16_le(&cfg, off + 3), 1600);
        }

        let code = encode_egg_lod(op1_v2, LiftOffDistance::Mm10, false).unwrap();
        assert_eq!(code, 3);
        assert_eq!(
            decode_egg_lod(op1_v2, code, false),
            Some(LiftOffDistance::Mm10)
        );
    }

    #[test]
    fn builds_four_block_writes_for_wireless_4k_v2() {
        let mut cfg = vec![0u8; EGG_CONFIG_SIZE];
        cfg[16 + 0x6F] = 1; // glass mode
        let writes = build_egg_4k_block_writes(&cfg, true);
        assert_eq!(writes.len(), 4);
        assert_eq!(writes[0].command, OP_WRITE_SENSOR);
        assert_eq!(writes[0].payload.len(), 28);
        assert_eq!(writes[1].command, OP_WRITE_POWER);
        assert_eq!(writes[1].payload.len(), 11);
        assert_eq!(writes[2].chunk, 1);
        assert_eq!(writes[3].chunk, 2);
    }
}
