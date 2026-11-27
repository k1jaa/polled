// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pulsar Pro 64-byte (`Report ID 0x00`) HID Input/Output protocol driver.
//!
//! Covers the newer Pulsar Pro 64-byte protocol family distinct from both the 17-byte CompX
//! protocol ([`crate::devices::compx`]) and the 64-byte Fusion 2 / Sonix feature-report protocol
//! ([`crate::devices::pulsar_fusion`]):
//! - **Pulsar Pro 8K Wireless Receiver (`3710:5405`)** — `usage_page 0x0001`, `usage 0x0000`,
//!   64-byte `Report ID 0x00` Input/Output reports (requires `0xC3 0x01` save-allow after writes).
//! - **Pulsar ZywOo The Chosen X Wired (`3710:7401`)** — 64-byte `Report ID 0x00` Input/Output
//!   control collection (applies writes directly without `0xC3`, supports `125–8000 Hz` wired).
//!
//! # Protocol Attribution & License (AGPL-3.0 compatible)
//! Every USB VID/PID, command opcode, response offset, busy (`0xA7`) / reject (`0xA6`) handling,
//! wireless `0xC3` save-allow commit, and wired direct-write rule comes verbatim from:
//! - `OpenMouse-Project/mouse-protocol` (<https://github.com/OpenMouse-Project/mouse-protocol>)
//!   - `src/pulsar/index.ts` (`PULSAR_PRO_PRODUCT_IDS`, `PULSAR_PRO_COMMAND`)
//!   - `src/drivers/pulsar/pulsar-pro-hid.ts` (`PulsarProHidClient`)
//!   - **AGPL-3.0-or-later**, Copyright (C) OpenMouse Project Contributors
//!
//! No HID bytes are guessed. Destructive opcodes (factory reset / firmware flash) are never
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

pub const PULSAR_PRO_VID: u16 = 0x3710;
pub const PULSAR_PRO_WIRELESS_PID: u16 = 0x5405;
pub const PULSAR_PRO_ZYWOO_WIRED_PID: u16 = 0x7401;

pub const PULSAR_PRO_REPORT_ID: u8 = 0x00;
pub const PULSAR_PRO_PACKET_LEN: usize = 64;

// --- Verified Pulsar Pro command opcodes ---
const CMD_DPI: u8 = 0xB1;
const CMD_POLLING: u8 = 0xB2;
const CMD_LOD: u8 = 0xB3;
const CMD_BATTERY: u8 = 0xB4;
const CMD_MOTION_SYNC: u8 = 0xB5;
const CMD_RIPPLE: u8 = 0xB6;
const CMD_ANGLE_SNAP: u8 = 0xB7;
const CMD_POWER_SAVING: u8 = 0xC2;
const CMD_SAVE_ALLOW: u8 = 0xC3;
const CMD_DEBOUNCE: u8 = 0xC5;

// --- Receiver reply status bytes (rsp[0]) ---
const RSP_REJECTED: u8 = 0xA6;
const RSP_BUSY: u8 = 0xA7;

const MAX_BUSY_RETRIES: usize = 8;
const BUSY_RETRY_DELAY: Duration = Duration::from_millis(50);
const DEFAULT_CMD_TIMEOUT: Duration = Duration::from_millis(1_500);
const OPTIONAL_CMD_TIMEOUT: Duration = Duration::from_millis(500);

const POLLING_RATES_8K: &[u16] = &[125, 250, 500, 1000, 2000, 4000, 8000];
const LOD_OPTIONS: &[LiftOffDistance] = &[
    LiftOffDistance::Mm07,
    LiftOffDistance::Mm10,
    LiftOffDistance::Mm20,
];
/// Verified sleep timeout values in milliseconds supported by `CMD_POWER_SAVING (0xC2)`.
pub const PULSAR_PRO_SLEEP_STEPS_SECS: &[u16] = &[10, 30, 60, 120, 300, 600, 1800];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PulsarProProductSpec {
    pub pid: u16,
    pub model: &'static str,
    pub wired: bool,
}

pub const PULSAR_PRO_PRODUCTS: &[PulsarProProductSpec] = &[
    PulsarProProductSpec {
        pid: PULSAR_PRO_WIRELESS_PID,
        model: "Pulsar Pro 8K Wireless Receiver",
        wired: false,
    },
    PulsarProProductSpec {
        pid: PULSAR_PRO_ZYWOO_WIRED_PID,
        model: "Pulsar ZywOo The Chosen X (Wired)",
        wired: true,
    },
];

pub fn lookup_pulsar_pro_product(vid: u16, pid: u16) -> Option<PulsarProProductSpec> {
    if vid != PULSAR_PRO_VID {
        return None;
    }
    PULSAR_PRO_PRODUCTS.iter().copied().find(|p| p.pid == pid)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulsarProDpiConfig {
    pub stage_count: u8,
    /// 0-based active DPI stage index (`0..stage_count`).
    pub active_stage: u8,
    /// Per-stage `(x_dpi, y_dpi)` pairs (`1..=6` entries).
    pub stages: Vec<(u16, u16)>,
}

/// Builds a 64-byte Pulsar Pro command packet (`pkt[0] = cmd`, `pkt[1..] = params`).
pub fn build_command_packet(cmd: u8, params: &[u8]) -> [u8; PULSAR_PRO_PACKET_LEN] {
    let mut pkt = [0u8; PULSAR_PRO_PACKET_LEN];
    pkt[0] = cmd;
    let copy_len = params.len().min(PULSAR_PRO_PACKET_LEN - 1);
    pkt[1..1 + copy_len].copy_from_slice(&params[..copy_len]);
    pkt
}

/// Parses a `0xB1` (`CMD_DPI`) response payload.
pub fn decode_dpi_config(rsp: &[u8; PULSAR_PRO_PACKET_LEN]) -> Option<PulsarProDpiConfig> {
    if rsp[0] != CMD_DPI {
        return None;
    }
    let stage_count = rsp[2];
    let active_stage = rsp[3];
    if !(1..=6).contains(&stage_count) || active_stage >= stage_count {
        return None;
    }
    let count = usize::from(stage_count);
    let mut stages = Vec::with_capacity(count);
    for i in 0..count {
        let off = 4 + i * 4;
        if off + 3 >= rsp.len() {
            return None;
        }
        let x = u16::from_le_bytes([rsp[off], rsp[off + 1]]);
        let y = u16::from_le_bytes([rsp[off + 2], rsp[off + 3]]);
        stages.push((x, y));
    }
    Some(PulsarProDpiConfig {
        stage_count,
        active_stage,
        stages,
    })
}

/// Encodes the parameters for writing a `0xB1` (`CMD_DPI`) table:
/// `[0x01, stage_count, active_0based, (x_le, y_le) * stage_count]`.
pub fn encode_dpi_write_params(cfg: &PulsarProDpiConfig) -> Vec<u8> {
    let count = cfg.stages.len().clamp(1, 6);
    let active = cfg.active_stage.min((count - 1) as u8);
    let mut params = Vec::with_capacity(3 + count * 4);
    params.push(0x01);
    params.push(count as u8);
    params.push(active);
    for &(x, y) in cfg.stages.iter().take(count) {
        let xb = x.to_le_bytes();
        let yb = y.to_le_bytes();
        params.push(xb[0]);
        params.push(xb[1]);
        params.push(yb[0]);
        params.push(yb[1]);
    }
    params
}

pub fn decode_lod(code: u8) -> Option<LiftOffDistance> {
    match code {
        1 => Some(LiftOffDistance::Mm07),
        2 => Some(LiftOffDistance::Mm10),
        3 => Some(LiftOffDistance::Mm20),
        _ => None,
    }
}

pub fn encode_lod(lod: LiftOffDistance) -> u8 {
    match lod {
        LiftOffDistance::Mm07 => 1,
        LiftOffDistance::Mm10 => 2,
        LiftOffDistance::Mm20 => 3,
    }
}

/// Decodes a `0xB4` (`CMD_BATTERY`) response into [`BatteryStatus`].
pub fn decode_battery(rsp: &[u8; PULSAR_PRO_PACKET_LEN]) -> Option<BatteryStatus> {
    if rsp[0] != CMD_BATTERY {
        return None;
    }
    let raw_pct = rsp[1];
    if raw_pct == 0 || raw_pct > 100 {
        return None;
    }
    Some(BatteryStatus {
        percent: raw_pct.min(100),
        charging: rsp[6] == 1,
        voltage_mv: None,
    })
}

/// Snaps a sleep timeout in seconds to the nearest verified Pulsar Pro step (`10..=1800` s)
/// and returns its millisecond representation for `CMD_POWER_SAVING (0xC2)`.
pub fn snap_sleep_timeout_ms(secs: u16) -> u32 {
    let nearest = PULSAR_PRO_SLEEP_STEPS_SECS
        .iter()
        .copied()
        .min_by_key(|&step| (step.abs_diff(secs), step))
        .unwrap_or(60);
    u32::from(nearest) * 1_000
}

pub fn decode_sleep_timeout_secs(rsp: &[u8; PULSAR_PRO_PACKET_LEN]) -> Option<u16> {
    if rsp[0] != CMD_POWER_SAVING {
        return None;
    }
    let ms = u32::from_le_bytes([rsp[2], rsp[3], rsp[4], rsp[5]]);
    let secs = ms / 1_000;
    if (10..=1800).contains(&secs) {
        Some(secs as u16)
    } else {
        None
    }
}

#[derive(Debug)]
enum ExchangeError {
    Rejected(u8),
    Busy,
    Io(String),
}

/// Hardware driver for Pulsar Pro 64-byte (`Report ID 0x00`) mice and wireless receivers.
pub struct PulsarProDevice {
    hid: HidDevice,
    spec: PulsarProProductSpec,
    display_name: String,
    vid_pid: String,
    rejected_optional_cmds: HashSet<u8>,
}

impl PulsarProDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.display_name.clone(),
            message: message.into(),
        }
    }

    fn flush_input(&self) {
        let mut scratch = [0u8; PULSAR_PRO_PACKET_LEN + 1];
        for _ in 0..8 {
            match self.hid.read_timeout(&mut scratch, 0) {
                Ok(n) if n > 0 => continue,
                _ => break,
            }
        }
    }

    fn exchange_once(
        &self,
        cmd: u8,
        params: &[u8],
        timeout: Duration,
    ) -> std::result::Result<[u8; PULSAR_PRO_PACKET_LEN], ExchangeError> {
        self.flush_input();

        let pkt = build_command_packet(cmd, params);
        let mut out = [0u8; PULSAR_PRO_PACKET_LEN + 1];
        out[0] = PULSAR_PRO_REPORT_ID;
        out[1..].copy_from_slice(&pkt);

        self.hid.write(&out).map_err(|e| {
            ExchangeError::Io(format!("Pulsar Pro HID write 0x{cmd:02x} failed: {e}"))
        })?;

        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; PULSAR_PRO_PACKET_LEN + 1];
        while Instant::now() < deadline {
            let remaining_ms = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .clamp(1, 200) as i32;
            match self.hid.read_timeout(&mut buf, remaining_ms) {
                Ok(0) => continue,
                Ok(n) => {
                    let slice = if n == PULSAR_PRO_PACKET_LEN + 1 && buf[0] == PULSAR_PRO_REPORT_ID
                    {
                        &buf[1..=PULSAR_PRO_PACKET_LEN]
                    } else if n >= PULSAR_PRO_PACKET_LEN {
                        &buf[..PULSAR_PRO_PACKET_LEN]
                    } else {
                        continue;
                    };
                    let mut rsp = [0u8; PULSAR_PRO_PACKET_LEN];
                    rsp.copy_from_slice(slice);

                    if rsp[0] == RSP_REJECTED {
                        return Err(ExchangeError::Rejected(cmd));
                    }
                    if rsp[0] == RSP_BUSY {
                        return Err(ExchangeError::Busy);
                    }
                    if rsp[0] == cmd {
                        return Ok(rsp);
                    }
                }
                Err(e) => {
                    return Err(ExchangeError::Io(format!(
                        "Pulsar Pro HID read 0x{cmd:02x} failed: {e}"
                    )));
                }
            }
        }

        Err(ExchangeError::Io(format!(
            "Timed out waiting for Pulsar Pro reply to 0x{cmd:02x} (move mouse to wake)"
        )))
    }

    fn command(
        &self,
        cmd: u8,
        params: &[u8],
        timeout: Duration,
    ) -> std::result::Result<[u8; PULSAR_PRO_PACKET_LEN], ExchangeError> {
        for attempt in 0..MAX_BUSY_RETRIES {
            match self.exchange_once(cmd, params, timeout) {
                Ok(rsp) => return Ok(rsp),
                Err(ExchangeError::Busy) if attempt + 1 < MAX_BUSY_RETRIES => {
                    thread::sleep(BUSY_RETRY_DELAY);
                }
                Err(ExchangeError::Busy) => {
                    return Err(ExchangeError::Io(format!(
                        "Pulsar Pro stayed busy on command 0x{cmd:02x}"
                    )));
                }
                Err(err) => return Err(err),
            }
        }
        Err(ExchangeError::Io(format!(
            "Pulsar Pro stayed busy on command 0x{cmd:02x}"
        )))
    }

    fn query(&self, cmd: u8) -> Result<[u8; PULSAR_PRO_PACKET_LEN]> {
        self.command(cmd, &[0x00], DEFAULT_CMD_TIMEOUT)
            .map_err(|e| match e {
                ExchangeError::Rejected(c) => {
                    self.dev_err(format!("Pulsar Pro rejected command 0x{c:02x}"))
                }
                ExchangeError::Busy => self.dev_err(format!("Pulsar Pro busy on 0x{cmd:02x}")),
                ExchangeError::Io(m) => self.dev_err(m),
            })
    }

    fn query_optional(
        &mut self,
        cmd: u8,
        timeout: Duration,
    ) -> Option<[u8; PULSAR_PRO_PACKET_LEN]> {
        if self.rejected_optional_cmds.contains(&cmd) {
            return None;
        }
        match self.command(cmd, &[0x00], timeout) {
            Ok(rsp) => Some(rsp),
            Err(ExchangeError::Rejected(_)) => {
                self.rejected_optional_cmds.insert(cmd);
                None
            }
            Err(_) => None,
        }
    }

    fn commit_wireless_if_needed(&self) -> Result<()> {
        if !self.spec.wired {
            // Wireless Pulsar Pro receiver requires 0xC3 0x01 (save-allow) to apply writes.
            // Wired ZywOo The Chosen X applies writes directly without 0xC3.
            self.command(CMD_SAVE_ALLOW, &[0x01], DEFAULT_CMD_TIMEOUT)
                .map_err(|e| match e {
                    ExchangeError::Rejected(c) => {
                        self.dev_err(format!("Pulsar Pro rejected save-allow 0x{c:02x}"))
                    }
                    ExchangeError::Busy => self.dev_err("Pulsar Pro busy on save-allow 0xC3"),
                    ExchangeError::Io(m) => self.dev_err(m),
                })?;
        }
        Ok(())
    }

    fn set_value(&self, cmd: u8, value: &[u8]) -> Result<()> {
        let mut params = Vec::with_capacity(1 + value.len());
        params.push(0x01);
        params.extend_from_slice(value);
        self.command(cmd, &params, DEFAULT_CMD_TIMEOUT)
            .map_err(|e| match e {
                ExchangeError::Rejected(c) => {
                    self.dev_err(format!("Pulsar Pro rejected write command 0x{c:02x}"))
                }
                ExchangeError::Busy => self.dev_err(format!("Pulsar Pro busy writing 0x{cmd:02x}")),
                ExchangeError::Io(m) => self.dev_err(m),
            })?;
        self.commit_wireless_if_needed()
    }
}

impl MouseDevice for PulsarProDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: format!("pulsar_pro_{:04x}", self.spec.pid),
            name: self.display_name.clone(),
            vendor: "Pulsar".to_string(),
            vid_pid: Some(self.vid_pid.clone()),
            connection: if self.spec.wired {
                "Wired USB 8K (Pulsar Pro)".to_string()
            } else {
                "Wireless 8K Dongle (Pulsar Pro)".to_string()
            },
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            dpi_min: 50,
            dpi_max: 32_000,
            dpi_step: 50,
            supports_xy_dpi: true,
            polling_rates_hz: POLLING_RATES_8K.to_vec(),
            lod_options: LOD_OPTIONS.to_vec(),
            supports_debounce: true,
            debounce_min_ms: 0,
            debounce_max_ms: 20,
            supports_motion_sync: true,
            supports_angle_snapping: true,
            supports_ripple_control: true,
            supports_sleep_timeout: !self.spec.wired,
            sleep_timeout_min_secs: 10,
            sleep_timeout_max_secs: 1800,
            dpi_stage_count: 6,
            supports_dpi_lock: true,
            supports_glass_mode: false,
            supports_volatile_writes: false,
            flash_write_warning: Some(format!(
                "{} writes settings to onboard memory (Pulsar Pro 64B HID). Polled diffs every register, enforces burst rate-limiting, and verifies writes via read-back.",
                self.display_name
            )),
        }
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        let dpi_rsp = self.query(CMD_DPI)?;
        let dpi_cfg = decode_dpi_config(&dpi_rsp).ok_or_else(|| {
            self.dev_err("Pulsar Pro reported an invalid DPI stage configuration (0xB1)")
        })?;
        let active_idx = usize::from(dpi_cfg.active_stage);
        let (dpi_x, dpi_y) = dpi_cfg.stages[active_idx];
        let all_locked = dpi_cfg.stages.len() > 1
            && dpi_cfg
                .stages
                .iter()
                .all(|&(x, y)| x == dpi_x && y == dpi_y);

        let polling_rsp = self.query(CMD_POLLING)?;
        let polling_rate_hz = Some(u16::from_le_bytes([polling_rsp[2], polling_rsp[3]]))
            .filter(|hz| POLLING_RATES_8K.contains(hz));

        let lod_rsp = self.query(CMD_LOD)?;
        let lod = decode_lod(lod_rsp[2]);

        let motion_sync = self
            .query_optional(CMD_MOTION_SYNC, OPTIONAL_CMD_TIMEOUT)
            .map(|r| r[2] == 1);
        let ripple_control = self
            .query_optional(CMD_RIPPLE, OPTIONAL_CMD_TIMEOUT)
            .map(|r| r[2] == 1);
        let angle_snapping = self
            .query_optional(CMD_ANGLE_SNAP, OPTIONAL_CMD_TIMEOUT)
            .map(|r| r[2] == 1);
        let debounce_ms = self
            .query_optional(CMD_DEBOUNCE, DEFAULT_CMD_TIMEOUT)
            .map(|r| r[2]);
        let sleep_timeout_secs = if self.spec.wired {
            None
        } else {
            self.query_optional(CMD_POWER_SAVING, DEFAULT_CMD_TIMEOUT)
                .and_then(|r| decode_sleep_timeout_secs(&r))
        };

        Ok(MouseSettings {
            dpi: Some(dpi_x),
            dpi_y: if dpi_y != dpi_x { Some(dpi_y) } else { None },
            polling_rate_hz,
            lod,
            debounce_ms,
            motion_sync,
            angle_snapping,
            ripple_control,
            sleep_timeout_secs,
            dpi_stage: Some(dpi_cfg.active_stage + 1),
            lock_dpi_stages: if all_locked { Some(true) } else { None },
            glass_mode: None,
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        if settings.dpi.is_some()
            || settings.dpi_y.is_some()
            || settings.dpi_stage.is_some()
            || settings.lock_dpi_stages == Some(true)
        {
            let dpi_rsp = self.query(CMD_DPI)?;
            let mut cfg = decode_dpi_config(&dpi_rsp).ok_or_else(|| {
                self.dev_err("Pulsar Pro reported an invalid DPI stage configuration (0xB1)")
            })?;

            if let Some(stage_1based) = settings.dpi_stage {
                let target_0based = stage_1based.saturating_sub(1);
                if target_0based >= cfg.stage_count {
                    return Err(PolledError::UnsupportedSetting {
                        device: self.display_name.clone(),
                        detail: format!(
                            "DPI stage #{stage_1based} exceeds this mouse's {}-stage cycle",
                            cfg.stage_count
                        ),
                    });
                }
                cfg.active_stage = target_0based;
            }

            let active_idx = usize::from(cfg.active_stage);
            let (cur_x, cur_y) = cfg.stages[active_idx];
            let next_x = settings.dpi.unwrap_or(cur_x);
            let next_y = settings.dpi_y.unwrap_or_else(|| {
                if settings.dpi.is_some() {
                    next_x
                } else {
                    cur_y
                }
            });

            if settings.lock_dpi_stages == Some(true) {
                cfg.stages.fill((next_x, next_y));
            } else if settings.dpi.is_some() || settings.dpi_y.is_some() {
                cfg.stages[active_idx] = (next_x, next_y);
            }

            let params = encode_dpi_write_params(&cfg);
            self.command(CMD_DPI, &params, DEFAULT_CMD_TIMEOUT)
                .map_err(|e| match e {
                    ExchangeError::Rejected(c) => {
                        self.dev_err(format!("Pulsar Pro rejected DPI write 0x{c:02x}"))
                    }
                    ExchangeError::Busy => self.dev_err("Pulsar Pro busy writing DPI 0xB1"),
                    ExchangeError::Io(m) => self.dev_err(m),
                })?;
            self.commit_wireless_if_needed()?;
        }

        if let Some(hz) = settings.polling_rate_hz {
            let bytes = hz.to_le_bytes();
            self.set_value(CMD_POLLING, &bytes)?;
        }

        if let Some(lod) = settings.lod {
            self.set_value(CMD_LOD, &[encode_lod(lod)])?;
        }

        if let Some(enabled) = settings.motion_sync {
            self.set_value(CMD_MOTION_SYNC, &[u8::from(enabled)])?;
        }

        if let Some(enabled) = settings.ripple_control {
            self.set_value(CMD_RIPPLE, &[u8::from(enabled)])?;
        }

        if let Some(enabled) = settings.angle_snapping {
            self.set_value(CMD_ANGLE_SNAP, &[u8::from(enabled)])?;
        }

        if let Some(ms) = settings.debounce_ms {
            self.set_value(CMD_DEBOUNCE, &[ms])?;
        }

        if let Some(secs) = settings.sleep_timeout_secs {
            let ms = snap_sleep_timeout_ms(secs);
            self.set_value(CMD_POWER_SAVING, &ms.to_le_bytes())?;
        }

        Ok(())
    }

    fn read_battery(&mut self) -> Option<BatteryStatus> {
        let rsp = self
            .command(CMD_BATTERY, &[0x00], OPTIONAL_CMD_TIMEOUT)
            .ok()?;
        decode_battery(&rsp)
    }
}

/// Discovery driver for Pulsar Pro 64-byte (`3710:5405` receiver and `3710:7401` wired).
pub struct PulsarProProtocolDriver;

impl DeviceDriver for PulsarProProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "pulsar-pro-64b"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();
        let mut seen_pids: HashSet<u16> = HashSet::new();

        for dev in api.device_list() {
            let vid = dev.vendor_id();
            let pid = dev.product_id();
            let Some(spec) = lookup_pulsar_pro_product(vid, pid) else {
                continue;
            };
            if seen_pids.contains(&pid) {
                continue;
            }

            let up = dev.usage_page();
            let u = dev.usage();
            if spec.wired {
                // Exclude standard mouse/keyboard/consumer/system-control collections on wired USB.
                if (up == 0x0001 && matches!(u, 0x0002 | 0x0006 | 0x0080)) || up == 0x000C {
                    continue;
                }
            } else {
                // Wireless Pulsar Pro receiver strictly exposes control on usage_page=1, usage=0.
                if up != 0x0001 || u != 0x0000 {
                    continue;
                }
            }

            let Ok(hid) = api.open_path(dev.path()) else {
                continue;
            };

            // Verify Report ID 0x00 64-byte Input & Output support when descriptor is accessible.
            let mut desc_buf = [0u8; 4096];
            if let Ok(len) = hid.get_report_descriptor(&mut desc_buf) {
                if len > 0 {
                    let parsed = parse_hid_report_descriptor(&desc_buf[..len]);
                    let has_any = !parsed.input_reports.is_empty()
                        || !parsed.output_reports.is_empty()
                        || !parsed.feature_reports.is_empty();
                    if has_any {
                        let has_in_64 = parsed.input_reports.get(&PULSAR_PRO_REPORT_ID)
                            == Some(&PULSAR_PRO_PACKET_LEN);
                        let has_out_64 = parsed.output_reports.get(&PULSAR_PRO_REPORT_ID)
                            == Some(&PULSAR_PRO_PACKET_LEN);
                        if !(has_in_64 && has_out_64) {
                            continue;
                        }
                    }
                }
            }

            let prod_name = dev
                .product_string()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| spec.model.to_string());

            let candidate = PulsarProDevice {
                hid,
                spec,
                display_name: prod_name,
                vid_pid: format!("{vid:04x}:{pid:04x}"),
                rejected_optional_cmds: HashSet::new(),
            };

            // For wired devices (always awake) whose descriptor might be hidden by Windows, verify
            // read-only `0xB1` DPI query so we never attach to an unrelated collection.
            if spec.wired
                && candidate
                    .command(CMD_DPI, &[0x00], OPTIONAL_CMD_TIMEOUT)
                    .is_err()
            {
                continue;
            }

            seen_pids.insert(pid);
            found.push(Box::new(candidate));
        }

        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_catalog_excludes_compx_and_fusion2_pids() {
        assert!(lookup_pulsar_pro_product(0x3710, 0x5405).is_some_and(|p| !p.wired));
        assert!(lookup_pulsar_pro_product(0x3710, 0x7401).is_some_and(|p| p.wired));
        // Must never overlap CompX 17B (5406/3414) or Fusion 2 (5403/5402/5404/3409).
        for other in [0x5406, 0x3414, 0x5403, 0x5402, 0x5404, 0x3409] {
            assert!(lookup_pulsar_pro_product(0x3710, other).is_none());
        }
        assert!(lookup_pulsar_pro_product(0x3554, 0x5405).is_none());
    }

    #[test]
    fn dpi_table_decode_and_encode_roundtrip() {
        let mut rsp = [0u8; PULSAR_PRO_PACKET_LEN];
        rsp[0] = CMD_DPI;
        rsp[2] = 4; // 4 stages
        rsp[3] = 1; // stage 1 active (0-based -> Stage #2)
        let stages = [(400u16, 400u16), (800, 1200), (1600, 1600), (3200, 3200)];
        for (i, &(x, y)) in stages.iter().enumerate() {
            let off = 4 + i * 4;
            rsp[off..off + 2].copy_from_slice(&x.to_le_bytes());
            rsp[off + 2..off + 4].copy_from_slice(&y.to_le_bytes());
        }

        let cfg = decode_dpi_config(&rsp).expect("valid DPI config");
        assert_eq!(cfg.stage_count, 4);
        assert_eq!(cfg.active_stage, 1);
        assert_eq!(cfg.stages, stages);

        let params = encode_dpi_write_params(&cfg);
        assert_eq!(params[0], 0x01); // write flag
        assert_eq!(params[1], 4);
        assert_eq!(params[2], 1);
        assert_eq!(params.len(), 3 + 4 * 4);
        assert_eq!(u16::from_le_bytes([params[7], params[8]]), 800);
        assert_eq!(u16::from_le_bytes([params[9], params[10]]), 1200);

        // Reject invalid stage count or out-of-range active stage.
        let mut bad = rsp;
        bad[3] = 4;
        assert!(decode_dpi_config(&bad).is_none());
        bad[2] = 0;
        assert!(decode_dpi_config(&bad).is_none());
    }

    #[test]
    fn lod_battery_and_sleep_codecs() {
        assert_eq!(decode_lod(1), Some(LiftOffDistance::Mm07));
        assert_eq!(decode_lod(2), Some(LiftOffDistance::Mm10));
        assert_eq!(decode_lod(3), Some(LiftOffDistance::Mm20));
        assert_eq!(decode_lod(0), None);
        assert_eq!(encode_lod(LiftOffDistance::Mm07), 1);
        assert_eq!(encode_lod(LiftOffDistance::Mm10), 2);
        assert_eq!(encode_lod(LiftOffDistance::Mm20), 3);

        let mut batt = [0u8; PULSAR_PRO_PACKET_LEN];
        batt[0] = CMD_BATTERY;
        batt[1] = 87;
        batt[6] = 1;
        assert_eq!(
            decode_battery(&batt),
            Some(BatteryStatus {
                percent: 87,
                charging: true,
                voltage_mv: None,
            })
        );
        batt[1] = 0;
        assert_eq!(decode_battery(&batt), None);

        assert_eq!(snap_sleep_timeout_ms(10), 10_000);
        assert_eq!(snap_sleep_timeout_ms(45), 30_000);
        assert_eq!(snap_sleep_timeout_ms(300), 300_000);
        assert_eq!(snap_sleep_timeout_ms(1800), 1_800_000);

        let mut sleep_rsp = [0u8; PULSAR_PRO_PACKET_LEN];
        sleep_rsp[0] = CMD_POWER_SAVING;
        sleep_rsp[2..6].copy_from_slice(&300_000u32.to_le_bytes());
        assert_eq!(decode_sleep_timeout_secs(&sleep_rsp), Some(300));
    }
}
