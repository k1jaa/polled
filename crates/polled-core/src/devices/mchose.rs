// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! MCHOSE A7 V2 / A7 Ultra / L7 Pro HID feature-report protocol driver (`0x3837` / `0x5253`).
//!
//! Uses bit-inverted (`^ 0xFF`) command/reply frames on vendor collection `0xFF01:0x0001`,
//! Feature Reports `0x11` (short 20-token channel) and `0x12` (long 64-token channel).
//!
//! # Protocol Attribution & Licenses (AGPL-3.0 compatible)
//! Command opcodes, bit-inverted framing, config blob layout, performance bitfield, and model
//! catalog are ported from:
//! - `OpenMouse-Project/mouse-protocol` (<https://github.com/OpenMouse-Project/mouse-protocol>)
//!   - `src/mchose/index.ts`, `src/drivers/mchose/hid.ts`
//!   - **AGPL-3.0-or-later**, Copyright (C) OpenMouse Project Contributors
//!
//! No HID bytes are guessed. MCHOSE MagDock (`0x1012`) and A7 V3 (`0x4D` output-report) PIDs are
//! explicitly excluded so incompatible frames are never sent to them.

use std::thread;
use std::time::Duration;

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    BatteryStatus, DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice,
    MouseSettings, SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};
use crate::hid::parse_hid_report_descriptor;

pub const MCHOSE_VID: u16 = 0x3837;
pub const MCHOSE_REALTEK_VID: u16 = 0x5253;
pub const MCHOSE_REALTEK_PIDS: &[u16] = &[0x1020, 0x1011];

pub const MCHOSE_USAGE_PAGE: u16 = 0xFF01;
pub const MCHOSE_USAGE: u16 = 0x0001;

pub const MCHOSE_SHORT_REPORT_ID: u8 = 0x11;
pub const MCHOSE_LONG_REPORT_ID: u8 = 0x12;
pub const MCHOSE_SHORT_TOKENS: usize = 20;
pub const MCHOSE_LONG_TOKENS: usize = 64;
pub const MCHOSE_REPORT_LEN: usize = 64;

const MCHOSE_DOCK_PID: u16 = 0x1012;
/// Known V3-generation PIDs (separate `0x4D` output-report protocol) excluded from V2 driver.
const MCHOSE_V3_PIDS: &[u16] = &[
    0x4026, 0x4027, 0x4028, 0x4029, 0x402A, 0x4030, 0x4031, 0x4032, 0x4033, 0x4034, 0x4035, 0x4036,
    0x4037, 0x4038,
];

const CMD_BATTERY: u8 = 0x06;
const CMD_SET_PERFORMANCE: u8 = 0x42;
const CMD_WRITE_CONFIG: u8 = 0x57;
const CMD_READ_CONFIG: u8 = 0x67;

const SENSOR_LOD_MASK: u8 = 0x03;
const SENSOR_RIPPLE: u8 = 0x04;
const SENSOR_LINEAR: u8 = 0x08;
const SENSOR_MOTION_SYNC: u8 = 0x10;

const DPI_MIN: u16 = 50;
const DPI_STEP: u16 = 50;
const DPI_STAGES: usize = 6;
const DEBOUNCE_MAX_MS: u8 = 20;

const POLL_DELAY_MS: u64 = 60;
const POLL_ATTEMPTS: usize = 14;
const WRITE_SETTLE_MS: u64 = 350;
const PERFORMANCE_SETTLE_MS: u64 = 1200;

const LINK_BLUETOOTH_PID: u16 = 0x100A;

const RATES_1K: &[u16] = &[125, 500, 1000];
const RATES_4K: &[u16] = &[125, 500, 1000, 2000, 4000];
const RATES_8K: &[u16] = &[125, 500, 1000, 2000, 4000, 8000];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MchoseProductSpec {
    pub name: &'static str,
    pub product_id: u16,
    pub dpi_max: u16,
    pub has_07mm_lod: bool,
    pub lod_index_base: u8,
    pub max_polling_hz: u16,
}

pub const MCHOSE_PRODUCTS: &[MchoseProductSpec] = &[
    MchoseProductSpec {
        name: "A7 V2 Pro",
        product_id: 0x4018,
        dpi_max: 26_000,
        has_07mm_lod: false,
        lod_index_base: 0,
        max_polling_hz: 8000,
    },
    MchoseProductSpec {
        name: "A7 V2 Pro+",
        product_id: 0x4023,
        dpi_max: 26_000,
        has_07mm_lod: false,
        lod_index_base: 0,
        max_polling_hz: 8000,
    },
    MchoseProductSpec {
        name: "A7 V2 Ultra",
        product_id: 0x4019,
        dpi_max: 42_000,
        has_07mm_lod: true,
        lod_index_base: 0,
        max_polling_hz: 8000,
    },
    MchoseProductSpec {
        name: "A7 V2 Ultra+",
        product_id: 0x4021,
        dpi_max: 42_000,
        has_07mm_lod: true,
        lod_index_base: 0,
        max_polling_hz: 8000,
    },
    MchoseProductSpec {
        name: "A7 Ultra",
        product_id: 0x1011,
        dpi_max: 42_000,
        has_07mm_lod: true,
        lod_index_base: 0,
        max_polling_hz: 8000,
    },
    MchoseProductSpec {
        name: "L7 Pro",
        product_id: 0x00B0,
        dpi_max: 26_000,
        has_07mm_lod: false,
        lod_index_base: 0,
        max_polling_hz: 8000,
    },
    MchoseProductSpec {
        name: "L7 Pro+",
        product_id: 0x4015,
        dpi_max: 26_000,
        has_07mm_lod: false,
        lod_index_base: 1,
        max_polling_hz: 8000,
    },
];

pub fn lookup_mchose_product(
    mouse_pid: Option<u16>,
    os_name: Option<&str>,
) -> Option<MchoseProductSpec> {
    if let Some(pid) = mouse_pid {
        if let Some(&spec) = MCHOSE_PRODUCTS.iter().find(|p| p.product_id == pid) {
            return Some(spec);
        }
    }
    let upper = os_name?.trim().to_ascii_uppercase();
    if upper.is_empty() {
        return None;
    }
    let mut sorted: Vec<MchoseProductSpec> = MCHOSE_PRODUCTS.to_vec();
    sorted.sort_by_key(|p| std::cmp::Reverse(p.name.len()));
    sorted
        .into_iter()
        .find(|p| upper.contains(&p.name.to_ascii_uppercase()))
}

/// Encodes a bit-inverted 64-byte MCHOSE feature report body (`^ 0xFF` over `token_count` bytes).
pub fn encode_mchose_command(tokens: &[u8], token_count: usize) -> [u8; MCHOSE_REPORT_LEN] {
    let mut body = [0u8; MCHOSE_REPORT_LEN];
    let spelled = token_count.min(MCHOSE_REPORT_LEN);
    for (idx, slot) in body.iter_mut().take(spelled).enumerate() {
        let raw = tokens.get(idx).copied().unwrap_or(0);
        *slot = !raw;
    }
    body
}

/// Decodes an MCHOSE feature report reply (handles with or without leading report ID).
pub fn decode_mchose_reply(raw: &[u8], expected_report_id: u8) -> Option<(u8, Vec<u8>)> {
    let body = if raw.first() == Some(&expected_report_id) && raw.len() >= 3 {
        &raw[1..]
    } else if raw.len() >= 2 {
        raw
    } else {
        return None;
    };
    let command = !body[0];
    let payload: Vec<u8> = body[1..].iter().map(|&b| !b).collect();
    Some((command, payload))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MchoseConfig {
    pub wired_rate_index: u8,
    pub wired_dpi_index: u8,
    pub wireless_rate_index: u8,
    pub wireless_dpi_index: u8,
    pub dpi_stages: [u16; DPI_STAGES],
    pub stage_count: u8,
    pub sensor: u8,
    pub lod_index: u8,
    pub motion_sync: bool,
    pub angle_snapping: bool,
    pub ripple_control: bool,
    pub debounce_ms: u8,
}

pub fn decode_mchose_config(payload: &[u8]) -> Option<MchoseConfig> {
    if payload.len() < 20 {
        return None;
    }
    let wired = payload[1];
    let wireless = payload[2];
    let mut dpi_stages = [0u16; DPI_STAGES];
    for (stage, slot) in dpi_stages.iter_mut().enumerate() {
        let off = 4 + stage * 2;
        *slot = u16::from_le_bytes([payload[off], payload[off + 1]]);
    }
    let sensor = payload[17];
    Some(MchoseConfig {
        wired_dpi_index: wired & 0x0F,
        wired_rate_index: (wired >> 4) & 0x0F,
        wireless_dpi_index: wireless & 0x0F,
        wireless_rate_index: (wireless >> 4) & 0x0F,
        dpi_stages,
        stage_count: payload[16],
        sensor,
        lod_index: sensor & SENSOR_LOD_MASK,
        ripple_control: (sensor & SENSOR_RIPPLE) != 0,
        angle_snapping: (sensor & SENSOR_LINEAR) != 0,
        motion_sync: (sensor & SENSOR_MOTION_SYNC) != 0,
        debounce_ms: payload[18],
    })
}

pub fn pack_mchose_link_byte(rate_idx: u8, dpi_idx: u8) -> u8 {
    ((rate_idx & 0x0F) << 4) | (dpi_idx & 0x0F)
}

/// Builds a `0x57` write-config token slice preserving every untouched byte from `before_payload`.
pub fn encode_mchose_config_write(
    before_payload: &[u8],
    wired_link: (u8, u8),
    wireless_link: (u8, u8),
    dpi_stages: &[u16; DPI_STAGES],
    debounce_ms: u8,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + before_payload.len());
    out.push(CMD_WRITE_CONFIG);
    out.extend_from_slice(before_payload);
    if out.len() > 19 {
        out[2] = pack_mchose_link_byte(wired_link.0, wired_link.1);
        out[3] = pack_mchose_link_byte(wireless_link.0, wireless_link.1);
        for (stage, &dpi) in dpi_stages.iter().enumerate() {
            let le = dpi.to_le_bytes();
            out[5 + stage * 2] = le[0];
            out[5 + stage * 2 + 1] = le[1];
        }
        out[19] = debounce_ms;
    }
    out
}

/// Builds the `0x42` performance command: `[0x42, lod_index, ripple, angleSnap, motionSync, 0, 0, 0, 0, 0]`.
pub fn encode_mchose_performance(
    lod_index: u8,
    motion_sync: Option<bool>,
    angle_snapping: Option<bool>,
    ripple_control: Option<bool>,
) -> [u8; 10] {
    let toggle = |opt: Option<bool>| match opt {
        None => 0u8,
        Some(true) => 1,
        Some(false) => 2,
    };
    [
        CMD_SET_PERFORMANCE,
        lod_index & SENSOR_LOD_MASK,
        toggle(ripple_control),
        toggle(angle_snapping),
        toggle(motion_sync),
        0,
        0,
        0,
        0,
        0,
    ]
}

pub fn decode_mchose_battery(payload: &[u8]) -> Option<(u16, BatteryStatus)> {
    if payload.len() < 11 {
        return None;
    }
    let mouse_pid = u16::from_le_bytes([payload[2], payload[3]]);
    let percent = payload[9];
    if percent > 100 {
        return None;
    }
    let charging = payload[10] != 0;
    Some((
        mouse_pid,
        BatteryStatus {
            percent,
            charging,
            voltage_mv: None,
        },
    ))
}

fn decode_lod_index(spec: Option<MchoseProductSpec>, raw_idx: u8) -> Option<LiftOffDistance> {
    let base = spec.map_or(0, |s| s.lod_index_base);
    let rel = raw_idx.checked_sub(base)?;
    let has_07 = spec.is_none_or(|s| s.has_07mm_lod);
    if has_07 {
        match rel {
            0 => Some(LiftOffDistance::Mm07),
            1 => Some(LiftOffDistance::Mm10),
            2 => Some(LiftOffDistance::Mm20),
            _ => None,
        }
    } else {
        match rel {
            0 => Some(LiftOffDistance::Mm10),
            1 => Some(LiftOffDistance::Mm20),
            _ => None,
        }
    }
}

fn encode_lod_index(spec: Option<MchoseProductSpec>, lod: LiftOffDistance) -> Option<u8> {
    let base = spec.map_or(0, |s| s.lod_index_base);
    let has_07 = spec.is_none_or(|s| s.has_07mm_lod);
    let rel = if has_07 {
        match lod {
            LiftOffDistance::Mm07 => 0,
            LiftOffDistance::Mm10 => 1,
            LiftOffDistance::Mm20 => 2,
        }
    } else {
        match lod {
            LiftOffDistance::Mm10 => 0,
            LiftOffDistance::Mm20 => 1,
            LiftOffDistance::Mm07 => return None,
        }
    };
    Some(base.saturating_add(rel))
}

/// Native Windows `hidapi` driver for MCHOSE A7 V2 / A7 Ultra / L7 Pro mice.
pub struct MchoseDevice {
    hid: HidDevice,
    vid: u16,
    pid: u16,
    os_name: String,
    product: Option<MchoseProductSpec>,
    display_name: String,
    vid_pid: String,
}

impl MchoseDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.display_name.clone(),
            message: message.into(),
        }
    }

    fn is_wired(&self) -> bool {
        MCHOSE_PRODUCTS.iter().any(|p| p.product_id == self.pid)
    }

    fn supported_rates(&self) -> &'static [u16] {
        if self.pid == LINK_BLUETOOTH_PID {
            return RATES_1K;
        }
        match self.product.map_or(8000, |p| p.max_polling_hz) {
            1000 => RATES_1K,
            4000 => RATES_4K,
            _ => RATES_8K,
        }
    }

    fn send_report(&self, report_id: u8, tokens: &[u8], token_count: usize) -> Result<()> {
        let body = encode_mchose_command(tokens, token_count);
        let mut buf = [0u8; MCHOSE_REPORT_LEN + 1];
        buf[0] = report_id;
        buf[1..].copy_from_slice(&body);
        self.hid.send_feature_report(&buf).map_err(|e| {
            self.dev_err(format!(
                "MCHOSE feature write 0x{report_id:02x} failed: {e}"
            ))
        })
    }

    fn request<F>(
        &self,
        report_id: u8,
        command: u8,
        token_count: usize,
        accept: F,
    ) -> Result<Vec<u8>>
    where
        F: Fn(&[u8]) -> bool,
    {
        let mut previous: Option<Vec<u8>> = None;
        for _ in 0..POLL_ATTEMPTS {
            self.send_report(report_id, &[command], token_count)?;
            thread::sleep(Duration::from_millis(POLL_DELAY_MS));

            let mut recv = [0u8; MCHOSE_REPORT_LEN + 2];
            recv[0] = report_id;
            let Ok(n) = self.hid.get_feature_report(&mut recv) else {
                continue;
            };
            let Some((reply_cmd, payload)) = decode_mchose_reply(&recv[..n], report_id) else {
                continue;
            };
            if reply_cmd != command || !accept(&payload) {
                continue;
            }
            // Confirm two consecutive identical replies to guard against shared-buffer stale reads.
            if previous.as_deref() == Some(payload.as_slice()) {
                return Ok(payload);
            }
            previous = Some(payload);
        }
        Err(self.dev_err(format!(
            "Timed out waiting for MCHOSE cmd 0x{command:02x} (move mouse to wake)"
        )))
    }

    fn read_config_payload(&self) -> Result<Vec<u8>> {
        self.request(
            MCHOSE_LONG_REPORT_ID,
            CMD_READ_CONFIG,
            MCHOSE_LONG_TOKENS,
            |payload| {
                if payload.len() < 20 {
                    return false;
                }
                let first_dpi = u16::from_le_bytes([payload[4], payload[5]]);
                (DPI_MIN..=42_000).contains(&first_dpi)
            },
        )
    }

    fn try_identify_from_battery(&mut self) -> Option<BatteryStatus> {
        let payload = self
            .request(
                MCHOSE_SHORT_REPORT_ID,
                CMD_BATTERY,
                MCHOSE_SHORT_TOKENS,
                |p| p.len() >= 11 && p[..11].iter().any(|&b| b != 0),
            )
            .ok()?;
        let (mouse_pid, status) = decode_mchose_battery(&payload)?;
        if let Some(found) = lookup_mchose_product(Some(mouse_pid), Some(&self.os_name)) {
            self.product = Some(found);
            self.display_name = format!("MCHOSE {}", found.name);
        }
        Some(status)
    }
}

impl MouseDevice for MchoseDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: format!("mchose_{:04x}_{:04x}", self.vid, self.pid),
            name: self.display_name.clone(),
            vendor: "MCHOSE".to_string(),
            vid_pid: Some(self.vid_pid.clone()),
            connection: if self.is_wired() {
                "Wired USB (MCHOSE)".to_string()
            } else {
                "Wireless 2.4 GHz Receiver (MCHOSE)".to_string()
            },
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let lod_options = if self.product.is_none_or(|p| p.has_07mm_lod) {
            LiftOffDistance::ALL.to_vec()
        } else {
            vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20]
        };
        DeviceCapabilities {
            dpi_min: DPI_MIN,
            dpi_max: self.product.map_or(26_000, |p| p.dpi_max),
            dpi_step: DPI_STEP,
            polling_rates_hz: self.supported_rates().to_vec(),
            lod_options,
            supports_debounce: true,
            debounce_min_ms: 0,
            debounce_max_ms: DEBOUNCE_MAX_MS,
            supports_motion_sync: true,
            supports_angle_snapping: true,
            supports_ripple_control: true,
            supports_volatile_writes: false,
            flash_write_warning: Some(format!(
                "{} persists profile settings to onboard memory. Polled diffs unchanged values and rate-limits rapid writes.",
                self.display_name
            )),
        }
    }

    fn read_battery(&mut self) -> Option<BatteryStatus> {
        self.try_identify_from_battery()
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        if self.product.is_none() {
            let _ = self.try_identify_from_battery();
        }
        let payload = self.read_config_payload()?;
        let cfg = decode_mchose_config(&payload)
            .ok_or_else(|| self.dev_err("Could not decode MCHOSE 0x67 config blob"))?;
        let wired = self.is_wired();
        let stage_idx = usize::from(if wired {
            cfg.wired_dpi_index
        } else {
            cfg.wireless_dpi_index
        })
        .min(DPI_STAGES - 1);
        let rate_idx = usize::from(if wired {
            cfg.wired_rate_index
        } else {
            cfg.wireless_rate_index
        });
        let rates = self.supported_rates();
        let polling_rate_hz = rates.get(rate_idx).copied();
        let lod = decode_lod_index(self.product, cfg.lod_index);

        Ok(MouseSettings {
            dpi: Some(cfg.dpi_stages[stage_idx]),
            polling_rate_hz,
            lod,
            debounce_ms: Some(cfg.debounce_ms),
            motion_sync: Some(cfg.motion_sync),
            angle_snapping: Some(cfg.angle_snapping),
            ripple_control: Some(cfg.ripple_control),
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        let before_payload = self.read_config_payload()?;
        let cfg = decode_mchose_config(&before_payload)
            .ok_or_else(|| self.dev_err("Could not decode MCHOSE 0x67 config blob"))?;
        let wired = self.is_wired();
        let rates = self.supported_rates();

        let mut wired_link = (cfg.wired_rate_index, cfg.wired_dpi_index);
        let mut wireless_link = (cfg.wireless_rate_index, cfg.wireless_dpi_index);
        let mut stages = cfg.dpi_stages;
        let mut debounce = cfg.debounce_ms;
        let mut config_dirty = false;

        let active_stage = usize::from(if wired {
            cfg.wired_dpi_index
        } else {
            cfg.wireless_dpi_index
        })
        .min(DPI_STAGES - 1);

        if let Some(target_dpi) = settings.dpi {
            if stages[active_stage] != target_dpi {
                stages[active_stage] = target_dpi;
                config_dirty = true;
            }
        }

        if let Some(target_hz) = settings.polling_rate_hz {
            let rate_idx = rates.iter().position(|&r| r == target_hz).ok_or_else(|| {
                PolledError::UnsupportedSetting {
                    device: self.display_name.clone(),
                    detail: format!("Unsupported MCHOSE polling rate {target_hz} Hz"),
                }
            })? as u8;
            if wired && wired_link.0 != rate_idx {
                wired_link.0 = rate_idx;
                config_dirty = true;
            } else if !wired && wireless_link.0 != rate_idx {
                wireless_link.0 = rate_idx;
                config_dirty = true;
            }
        }

        if let Some(target_db) = settings.debounce_ms {
            if debounce != target_db {
                debounce = target_db;
                config_dirty = true;
            }
        }

        if config_dirty {
            let tokens = encode_mchose_config_write(
                &before_payload,
                wired_link,
                wireless_link,
                &stages,
                debounce,
            );
            self.send_report(MCHOSE_LONG_REPORT_ID, &tokens, MCHOSE_LONG_TOKENS)?;
            thread::sleep(Duration::from_millis(WRITE_SETTLE_MS));
        }

        let target_lod_idx = match settings.lod {
            Some(lod) => Some(encode_lod_index(self.product, lod).ok_or_else(|| {
                PolledError::UnsupportedSetting {
                    device: self.display_name.clone(),
                    detail: format!("LOD {lod} not supported on {}", self.display_name),
                }
            })?),
            None => None,
        };
        let lod_changed = target_lod_idx.is_some_and(|idx| idx != cfg.lod_index);
        let ms_changed = settings.motion_sync.is_some_and(|v| v != cfg.motion_sync);
        let ang_changed = settings
            .angle_snapping
            .is_some_and(|v| v != cfg.angle_snapping);
        let rip_changed = settings
            .ripple_control
            .is_some_and(|v| v != cfg.ripple_control);

        if lod_changed || ms_changed || ang_changed || rip_changed {
            let perf = encode_mchose_performance(
                target_lod_idx.unwrap_or(cfg.lod_index),
                ms_changed.then_some(settings.motion_sync.unwrap()),
                ang_changed.then_some(settings.angle_snapping.unwrap()),
                rip_changed.then_some(settings.ripple_control.unwrap()),
            );
            self.send_report(MCHOSE_SHORT_REPORT_ID, &perf, MCHOSE_SHORT_TOKENS)?;
            thread::sleep(Duration::from_millis(PERFORMANCE_SETTLE_MS));
        }

        Ok(())
    }
}

/// Protocol-family auto-detector for MCHOSE A7 V2 / A7 Ultra / L7 Pro mice.
pub struct MchoseProtocolDriver;

impl DeviceDriver for MchoseProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "MCHOSE A7 V2 / L7 (0x3837 / 0x5253)"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();
        for dev in api.device_list() {
            let vid = dev.vendor_id();
            let pid = dev.product_id();
            let vendor_ok = vid == MCHOSE_VID
                || (vid == MCHOSE_REALTEK_VID && MCHOSE_REALTEK_PIDS.contains(&pid));
            if !vendor_ok
                || pid == MCHOSE_DOCK_PID
                || MCHOSE_V3_PIDS.contains(&pid)
                || dev.usage_page() != MCHOSE_USAGE_PAGE
                || dev.usage() != MCHOSE_USAGE
            {
                continue;
            }
            let Ok(hid) = dev.open_device(api) else {
                continue;
            };
            let mut desc_buf = [0u8; 4096];
            let has_v2_reports = hid
                .get_report_descriptor(&mut desc_buf)
                .ok()
                .map(|len| parse_hid_report_descriptor(&desc_buf[..len]))
                .is_some_and(|s| {
                    s.feature_reports.contains_key(&MCHOSE_SHORT_REPORT_ID)
                        && s.feature_reports.contains_key(&MCHOSE_LONG_REPORT_ID)
                });
            if !has_v2_reports {
                continue;
            }

            let os_name = dev.product_string().unwrap_or_default().trim().to_string();
            let product = lookup_mchose_product(Some(pid), Some(&os_name));
            let display_name = if let Some(p) = product {
                format!("MCHOSE {}", p.name)
            } else if !os_name.is_empty() {
                if os_name.to_ascii_lowercase().starts_with("mchose") {
                    os_name.clone()
                } else {
                    format!("MCHOSE {os_name}")
                }
            } else {
                "MCHOSE Mouse".to_string()
            };

            let mut device = MchoseDevice {
                hid,
                vid,
                pid,
                os_name,
                product,
                display_name,
                vid_pid: format!("{vid:04x}:{pid:04x}"),
            };
            let _ = device.try_identify_from_battery();
            found.push(Box::new(device));
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_inverted_mchose_frames_and_config() {
        let body = encode_mchose_command(&[CMD_READ_CONFIG], MCHOSE_LONG_TOKENS);
        assert_eq!(body[0], !CMD_READ_CONFIG);
        assert_eq!(body[1], 0xFF);

        let mut fake_payload = [0u8; 63];
        fake_payload[1] = pack_mchose_link_byte(2, 1); // wired 1000 Hz, stage 1
        fake_payload[2] = pack_mchose_link_byte(4, 0); // wireless 4000 Hz, stage 0
        fake_payload[4..6].copy_from_slice(&800u16.to_le_bytes());
        fake_payload[6..8].copy_from_slice(&1600u16.to_le_bytes());
        fake_payload[16] = 4;
        fake_payload[17] = 0x01 | SENSOR_MOTION_SYNC | SENSOR_RIPPLE;
        fake_payload[18] = 2;

        let mut wire = vec![MCHOSE_LONG_REPORT_ID, !CMD_READ_CONFIG];
        wire.extend(fake_payload.iter().map(|&b| !b));

        let (cmd, decoded_payload) = decode_mchose_reply(&wire, MCHOSE_LONG_REPORT_ID).unwrap();
        assert_eq!(cmd, CMD_READ_CONFIG);
        let cfg = decode_mchose_config(&decoded_payload).unwrap();
        assert_eq!(cfg.dpi_stages[0], 800);
        assert_eq!(cfg.dpi_stages[1], 1600);
        assert_eq!(cfg.wireless_rate_index, 4);
        assert_eq!(cfg.lod_index, 1);
        assert!(cfg.motion_sync);
        assert!(cfg.ripple_control);
        assert!(!cfg.angle_snapping);
        assert_eq!(cfg.debounce_ms, 2);

        let perf = encode_mchose_performance(0, Some(true), Some(false), None);
        assert_eq!(perf[0], CMD_SET_PERFORMANCE);
        assert_eq!(perf[1], 0);
        assert_eq!(perf[2], 0); // ripple untouched
        assert_eq!(perf[3], 2); // angle snap off
        assert_eq!(perf[4], 1); // motion sync on
    }

    #[test]
    fn decodes_mchose_battery_and_resolves_products() {
        let mut payload = [0u8; 16];
        payload[2..4].copy_from_slice(&0x4021u16.to_le_bytes());
        payload[9] = 73;
        payload[10] = 1;
        let (pid, batt) = decode_mchose_battery(&payload).unwrap();
        assert_eq!(pid, 0x4021);
        assert_eq!(batt.percent, 73);
        assert!(batt.charging);

        let spec = lookup_mchose_product(Some(pid), None).unwrap();
        assert_eq!(spec.name, "A7 V2 Ultra+");
        assert_eq!(spec.dpi_max, 42_000);
    }
}
