// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Ninjutso NinjaForce HID feature-report protocol driver (Sora V2, Sora V3, TEN / TEN AIR).
//!
//! # Protocol Attribution & Licenses (AGPL-3.0 compatible)
//! Legacy Sora V2 (`0x1915`, Report `0x05`/`0x04`) and current Sora V3 / TEN (`0x093A`,
//! Report `0x06`/`0x03`) packet layouts, DPI encodings, polling codes, and checksums are
//! ported from:
//! - `OpenMouse-Project/mouse-protocol` (<https://github.com/OpenMouse-Project/mouse-protocol>)
//!   - `src/ninjutso/index.ts`, `src/drivers/ninjutso/hid.ts`
//!   - **AGPL-3.0-or-later**, Copyright (C) OpenMouse Project Contributors
//!
//! No HID bytes are guessed.

use std::thread;
use std::time::Duration;

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    BatteryStatus, DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice,
    MouseSettings, SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};
use crate::hid::parse_hid_report_descriptor;

pub const NINJUTSO_LEGACY_VID: u16 = 0x1915;
pub const NINJUTSO_VID: u16 = 0x093A;

pub const LEGACY_MOUSE_PIDS: &[u16] = &[0xAE11, 0xAE12, 0xAE13, 0xAE14, 0xAE15, 0xAE16];
pub const LEGACY_RECEIVER_PIDS: &[u16] = &[0xAE1C, 0xAE8C, 0xAE8A];
pub const LEGACY_HIGH_RATE_RECEIVERS: &[u16] = &[0xAE8C, 0xAE8A];
pub const SORA_V2_3950_PIDS: &[u16] = &[0xAE14, 0xAE15, 0xAE16];

pub const CURRENT_MOUSE_PIDS: &[u16] = &[0xE020, 0xE010, 0xEA01];
pub const CURRENT_RECEIVER_PIDS: &[u16] = &[0xEB01, 0xEB02];
pub const SORA_V3_PIDS: &[u16] = &[0xE010, 0xEB02];

pub const REPORT_LEGACY_CMD: u8 = 5;
pub const REPORT_LEGACY_BLOB: u8 = 4;
pub const LEGACY_PAYLOAD_LEN: usize = 31;

pub const REPORT_CURRENT: u8 = 6;
pub const CURRENT_PAYLOAD_LEN: usize = 15;
pub const REPORT_CONTROL: u8 = 3;
pub const CONTROL_PAYLOAD_LEN: usize = 63;

const CMD_PROFILE: u8 = 16;
const CMD_BATTERY_CHARGING: u8 = 17;
const CMD_BATTERY_PERCENT: u8 = 18;
const CMD_ACTIVE_DPI_STAGE: u8 = 28;
const CMD_DPI_GET: u8 = 4;
const CMD_DPI_SET: u8 = 3;
const CMD_POLLING_GET: u8 = 6;
const CMD_POLLING_SET: u8 = 5;
const CMD_LOD_GET: u8 = 8;
const CMD_LOD_SET: u8 = 7;
const CMD_MOTION_SYNC_SET: u8 = 13;
const CMD_MOTION_SYNC_GET: u8 = 14;
const CMD_PAIRED_PID: u8 = 167;

const RESPONSE_DELAY_MS: u64 = 30;
const RETRY_DELAY_MS: u64 = 60;

/// Builds a 16-byte current-gen Ninjutso feature report (`[0x06, cmd, 0, 0, 1, 0, args_len, profile, args...]`).
pub fn build_ninjutso_current_request(
    command: u8,
    profile: u8,
    args: &[u8],
) -> [u8; CURRENT_PAYLOAD_LEN + 1] {
    let mut pkt = [0u8; CURRENT_PAYLOAD_LEN + 1];
    let n = args.len().min(8);
    pkt[0] = REPORT_CURRENT;
    pkt[1] = command;
    pkt[4] = 1;
    pkt[6] = n as u8;
    pkt[7] = profile;
    pkt[8..8 + n].copy_from_slice(&args[..n]);
    pkt
}

/// Extracts response data slice (bytes `8..`) from a current-gen Ninjutso feature reply.
pub fn decode_ninjutso_current_response(raw: &[u8], command: u8) -> Option<Vec<u8>> {
    let normalized: Vec<u8> = if raw.get(1) != Some(&command)
        && raw.first() == Some(&command)
        && raw.get(5).copied().unwrap_or(0) != 0
    {
        let mut v = Vec::with_capacity(raw.len() + 1);
        v.push(REPORT_CURRENT);
        v.extend_from_slice(raw);
        v
    } else {
        raw.to_vec()
    };
    if normalized.len() < 9 || normalized[1] != command {
        return None;
    }
    Some(normalized[8..].to_vec())
}

pub fn encode_ninjutso_dpi(dpi: u16, direct: bool) -> Option<[u8; 3]> {
    let (min, max, step) = if direct {
        (1, 45_000, 1)
    } else {
        (50, 30_000, 50)
    };
    if dpi < min || dpi > max || !dpi.is_multiple_of(step) {
        return None;
    }
    let encoded = if direct { dpi } else { dpi / 50 - 1 };
    let le = encoded.to_le_bytes();
    Some([le[0], le[1], 0])
}

pub fn decode_ninjutso_dpi(lo: u8, hi: u8, direct: bool) -> u16 {
    let encoded = u16::from_le_bytes([lo, hi]);
    if direct {
        encoded
    } else {
        encoded.saturating_add(1).saturating_mul(50)
    }
}

pub fn encode_ninjutso_polling(hz: u16) -> Option<u8> {
    match hz {
        1000 => Some(1),
        2000 => Some(2),
        4000 => Some(3),
        8000 => Some(4),
        _ => None,
    }
}

pub fn decode_ninjutso_polling(code: u8) -> Option<u16> {
    match code {
        1 => Some(1000),
        2 => Some(2000),
        3 => Some(4000),
        4 => Some(8000),
        _ => None,
    }
}

pub fn build_ninjutso_legacy_request(
    command: u8,
    profile: u8,
    value: Option<u8>,
) -> [u8; LEGACY_PAYLOAD_LEN + 1] {
    let mut pkt = [0u8; LEGACY_PAYLOAD_LEN + 1];
    pkt[0] = REPORT_LEGACY_CMD;
    pkt[1] = command;
    pkt[4] = 1;
    pkt[6] = profile;
    pkt[7] = 22;
    if let Some(v) = value {
        pkt[9] = v;
    }
    pkt
}

pub fn encode_ninjutso_legacy_dpi(dpi: u16) -> Option<[u8; 2]> {
    if !(50..=30_000).contains(&dpi) || !dpi.is_multiple_of(50) {
        return None;
    }
    Some((dpi / 50 - 1).to_le_bytes())
}

pub fn decode_ninjutso_legacy_dpi(lo: u8, hi: u8) -> u16 {
    u16::from_le_bytes([lo, hi])
        .saturating_mul(50)
        .saturating_add(50)
}

fn decode_lod(code: u8) -> Option<LiftOffDistance> {
    match code {
        0 => Some(LiftOffDistance::Mm07),
        1 => Some(LiftOffDistance::Mm10),
        2 => Some(LiftOffDistance::Mm20),
        _ => None,
    }
}

fn encode_lod(lod: LiftOffDistance) -> u8 {
    match lod {
        LiftOffDistance::Mm07 => 0,
        LiftOffDistance::Mm10 => 1,
        LiftOffDistance::Mm20 => 2,
    }
}

/// Hardware driver for Ninjutso Sora V2, Sora V3, and TEN / TEN AIR mice.
pub struct NinjutsoDevice {
    hid: HidDevice,
    vid: u16,
    pid: u16,
    product_name_os: String,
    effective_pid: u16,
    legacy: bool,
    wireless: bool,
    profile: u8,
    display_name: String,
    vid_pid: String,
}

impl NinjutsoDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.display_name.clone(),
            message: message.into(),
        }
    }

    fn is_direct_dpi(&self) -> bool {
        !self.legacy && SORA_V3_PIDS.contains(&self.effective_pid)
    }

    fn read_current(&self, command: u8, args: &[u8], attempts: usize) -> Result<Vec<u8>> {
        let profile = if matches!(command, CMD_PROFILE | CMD_PAIRED_PID) {
            0
        } else {
            self.profile
        };
        let req = build_ninjutso_current_request(command, profile, args);
        for attempt in 0..attempts {
            self.hid.send_feature_report(&req).map_err(|e| {
                self.dev_err(format!("Ninjutso write cmd=0x{command:02x} failed: {e}"))
            })?;
            thread::sleep(Duration::from_millis(if attempt == 0 {
                RESPONSE_DELAY_MS
            } else {
                RETRY_DELAY_MS
            }));
            let mut buf = [0u8; CURRENT_PAYLOAD_LEN + 2];
            buf[0] = REPORT_CURRENT;
            let n = self.hid.get_feature_report(&mut buf).map_err(|e| {
                self.dev_err(format!("Ninjutso read cmd=0x{command:02x} failed: {e}"))
            })?;
            if let Some(val) = decode_ninjutso_current_response(&buf[..n], command) {
                return Ok(val);
            }
        }
        Err(self.dev_err(format!(
            "Ninjutso mouse did not answer cmd=0x{command:02x} (move mouse to wake)"
        )))
    }

    fn send_current(&self, command: u8, args: &[u8]) -> Result<()> {
        let req = build_ninjutso_current_request(command, self.profile, args);
        self.hid
            .send_feature_report(&req)
            .map_err(|e| self.dev_err(format!("Ninjutso write cmd=0x{command:02x} failed: {e}")))
    }

    fn set_control_pause(&self, resume: bool) -> Result<()> {
        let mut pkt = [0u8; CONTROL_PAYLOAD_LEN + 1];
        pkt[0] = REPORT_CONTROL;
        if resume {
            pkt[1..6].copy_from_slice(&[28, 27, 0, 0, 1]);
        } else {
            pkt[1..6].copy_from_slice(&[27, 26, 0, 0, 1]);
        }
        self.hid
            .send_feature_report(&pkt)
            .map_err(|e| self.dev_err(format!("Ninjutso control report failed: {e}")))?;
        thread::sleep(Duration::from_millis(RESPONSE_DELAY_MS));
        Ok(())
    }

    fn read_legacy_settings(&self) -> Result<(u8, Vec<u8>)> {
        let prof_req = build_ninjutso_legacy_request(13, 0, None);
        let mut prof_buf = [0u8; LEGACY_PAYLOAD_LEN + 2];
        let mut profile = 0u8;
        for _ in 0..2 {
            self.hid
                .send_feature_report(&prof_req)
                .map_err(|e| self.dev_err(format!("Sora V2 profile query failed: {e}")))?;
            thread::sleep(Duration::from_millis(15));
            prof_buf[0] = REPORT_LEGACY_CMD;
            let n = self
                .hid
                .get_feature_report(&mut prof_buf)
                .map_err(|e| self.dev_err(format!("Sora V2 profile read failed: {e}")))?;
            let off = if prof_buf[0] == REPORT_LEGACY_CMD {
                10
            } else {
                9
            };
            if n > off {
                profile = prof_buf[off];
            }
            if profile != 0 {
                break;
            }
        }

        let compact = self.product_name_os == "Ninjutso Sora V2 8K";
        let payload_len = if compact { 73 } else { 703 };
        let mut req = vec![0u8; payload_len + 1];
        req[0] = REPORT_LEGACY_BLOB;
        req[1..7].copy_from_slice(&[38, 0, 0, 1, 0, profile]);
        self.hid
            .send_feature_report(&req)
            .map_err(|e| self.dev_err(format!("Sora V2 blob query failed: {e}")))?;
        thread::sleep(Duration::from_millis(90));

        let mut resp = vec![0u8; payload_len + 2];
        resp[0] = REPORT_LEGACY_BLOB;
        let n = self
            .hid
            .get_feature_report(&mut resp)
            .map_err(|e| self.dev_err(format!("Sora V2 blob read failed: {e}")))?;
        let bytes = if resp[0] == REPORT_LEGACY_BLOB && n > payload_len {
            &resp[1..=payload_len]
        } else {
            &resp[..n]
        };

        let sum_end = if compact { 71 } else { 701 };
        let chk_off = if compact { 72 } else { 702 };
        if bytes.len() <= chk_off + 1 {
            return Err(self.dev_err("Truncated Sora V2 settings report"));
        }
        let sum: u32 = bytes[..sum_end].iter().map(|&b| u32::from(b)).sum();
        let expected = u32::from(u16::from_le_bytes([bytes[chk_off], bytes[chk_off + 1]]));
        if (sum & 0xFFFF) != expected {
            return Err(self.dev_err("Sora V2 settings checksum mismatch"));
        }
        Ok((profile, bytes[9..].to_vec()))
    }
}

impl MouseDevice for NinjutsoDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: format!("ninjutso_{:04x}_{:04x}", self.vid, self.pid),
            name: self.display_name.clone(),
            vendor: "Ninjutso".to_string(),
            vid_pid: Some(self.vid_pid.clone()),
            connection: if self.wireless {
                "Wireless 2.4 GHz Receiver".to_string()
            } else {
                "Wired USB".to_string()
            },
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let direct = self.is_direct_dpi();
        let polling_rates_hz = if !self.legacy || LEGACY_HIGH_RATE_RECEIVERS.contains(&self.pid) {
            vec![1000, 2000, 4000, 8000]
        } else {
            vec![1000]
        };
        let lod_options = if self.legacy && !SORA_V2_3950_PIDS.contains(&self.effective_pid) {
            vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20]
        } else {
            LiftOffDistance::ALL.to_vec()
        };
        DeviceCapabilities {
            dpi_min: 50,
            dpi_max: if direct { 45_000 } else { 30_000 },
            dpi_step: 50,
            supports_xy_dpi: false,
            polling_rates_hz,
            lod_options,
            supports_debounce: false,
            debounce_min_ms: 0,
            debounce_max_ms: 0,
            supports_motion_sync: true,
            supports_angle_snapping: false,
            supports_ripple_control: false,
            supports_sleep_timeout: false,
            sleep_timeout_min_secs: 0,
            sleep_timeout_max_secs: 0,
            supports_volatile_writes: false,
            flash_write_warning: Some(format!(
                "{} persists profile settings to onboard memory. Polled diffs unchanged values and rate-limits rapid writes.",
                self.display_name
            )),
        }
    }

    fn read_battery(&mut self) -> Option<BatteryStatus> {
        if self.legacy {
            return None;
        }
        let pct_reply = self.read_current(CMD_BATTERY_PERCENT, &[], 2).ok()?;
        let percent = (*pct_reply.first()?).min(100);
        let charging = self
            .read_current(CMD_BATTERY_CHARGING, &[], 2)
            .ok()
            .and_then(|r| r.first().copied())
            .is_some_and(|c| c != 0);
        Some(BatteryStatus {
            percent,
            charging,
            voltage_mv: None,
        })
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        if self.legacy {
            let (profile, data) = self.read_legacy_settings()?;
            self.profile = profile;
            let stage_count = usize::from(data.get(11).copied().unwrap_or(1).clamp(1, 4));
            let stage = usize::from(data.get(20).copied().unwrap_or(0)).min(stage_count - 1);
            let off = 12 + stage * 2;
            let dpi = decode_ninjutso_legacy_dpi(
                data.get(off).copied().unwrap_or(0),
                data.get(off + 1).copied().unwrap_or(0),
            );
            let rate = if LEGACY_HIGH_RATE_RECEIVERS.contains(&self.pid) {
                match data.get(24).copied().unwrap_or(0) {
                    1 => 2000,
                    2 => 4000,
                    3 => 8000,
                    _ => 1000,
                }
            } else {
                1000
            };
            let lod = decode_lod(data.get(22).copied().unwrap_or(1));
            let motion_sync = data.get(23).copied().map(|v| v == 1);
            return Ok(MouseSettings {
                dpi: Some(dpi),
                dpi_y: None,
                polling_rate_hz: Some(rate),
                lod,
                debounce_ms: None,
                motion_sync,
                angle_snapping: None,
                ripple_control: None,
                sleep_timeout_secs: None,
            });
        }

        self.profile = self
            .read_current(CMD_PROFILE, &[], 4)?
            .first()
            .copied()
            .filter(|&p| p != 0)
            .unwrap_or(1);
        let stage = self
            .read_current(CMD_ACTIVE_DPI_STAGE, &[], 4)?
            .first()
            .copied()
            .unwrap_or(0);
        let dpi_reply = self.read_current(CMD_DPI_GET, &[stage], 4)?;
        let dpi = decode_ninjutso_dpi(
            dpi_reply.first().copied().unwrap_or(0),
            dpi_reply.get(1).copied().unwrap_or(0),
            self.is_direct_dpi(),
        );
        let poll_reply = self.read_current(CMD_POLLING_GET, &[], 4)?;
        let polling_rate_hz = poll_reply
            .first()
            .copied()
            .and_then(decode_ninjutso_polling)
            .unwrap_or(1000);
        let lod_reply = self.read_current(CMD_LOD_GET, &[], 4)?;
        let lod = lod_reply.first().copied().and_then(decode_lod);
        let motion_sync = self
            .read_current(CMD_MOTION_SYNC_GET, &[], 2)
            .ok()
            .and_then(|r| r.first().copied())
            .map(|v| v == 1);

        Ok(MouseSettings {
            dpi: Some(dpi),
            dpi_y: None,
            polling_rate_hz: Some(polling_rate_hz),
            lod,
            debounce_ms: None,
            motion_sync,
            angle_snapping: None,
            ripple_control: None,
            sleep_timeout_secs: None,
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        let current = self.read_settings()?;

        if self.legacy {
            if let Some(target_dpi) = settings.dpi {
                if current.dpi != Some(target_dpi) {
                    let (profile, data) = self.read_legacy_settings()?;
                    let stage_count = usize::from(data.get(11).copied().unwrap_or(1).clamp(1, 4));
                    let stage =
                        usize::from(data.get(20).copied().unwrap_or(0)).min(stage_count - 1);
                    let mut stages = [800u16; 4];
                    for (idx, slot) in stages.iter_mut().enumerate() {
                        let off = 12 + idx * 2;
                        *slot = decode_ninjutso_legacy_dpi(
                            data.get(off).copied().unwrap_or(0),
                            data.get(off + 1).copied().unwrap_or(0),
                        );
                    }
                    stages[stage] = target_dpi;

                    let mut pkt = build_ninjutso_legacy_request(5, profile, None);
                    pkt[9] = stage_count as u8;
                    for (idx, &val) in stages.iter().enumerate() {
                        let enc = if idx < stage_count {
                            encode_ninjutso_legacy_dpi(val).unwrap_or([0, 0])
                        } else {
                            [0, 0]
                        };
                        pkt[10 + idx * 2] = enc[0];
                        pkt[11 + idx * 2] = enc[1];
                    }
                    self.hid
                        .send_feature_report(&pkt)
                        .map_err(|e| self.dev_err(format!("Sora V2 DPI write failed: {e}")))?;
                }
            }
            if let Some(target_hz) = settings.polling_rate_hz {
                if current.polling_rate_hz != Some(target_hz) {
                    let code = match target_hz {
                        1000 => 0u8,
                        2000 => 1,
                        4000 => 2,
                        8000 => 3,
                        _ => {
                            return Err(PolledError::UnsupportedSetting {
                                device: self.display_name.clone(),
                                detail: format!("Unsupported Sora V2 polling rate {target_hz} Hz"),
                            });
                        }
                    };
                    let pkt = build_ninjutso_legacy_request(10, self.profile, Some(code));
                    self.hid
                        .send_feature_report(&pkt)
                        .map_err(|e| self.dev_err(format!("Sora V2 polling write failed: {e}")))?;
                }
            }
            if let Some(target_lod) = settings.lod {
                if current.lod != Some(target_lod) {
                    let code = encode_lod(target_lod);
                    let pkt = build_ninjutso_legacy_request(11, self.profile, Some(code));
                    self.hid
                        .send_feature_report(&pkt)
                        .map_err(|e| self.dev_err(format!("Sora V2 LOD write failed: {e}")))?;
                }
            }
            if let Some(target_ms) = settings.motion_sync {
                if current.motion_sync != Some(target_ms) {
                    let pkt =
                        build_ninjutso_legacy_request(6, self.profile, Some(u8::from(target_ms)));
                    self.hid.send_feature_report(&pkt).map_err(|e| {
                        self.dev_err(format!("Sora V2 Motion Sync write failed: {e}"))
                    })?;
                }
            }
            return Ok(());
        }

        if let Some(target_dpi) = settings.dpi {
            if current.dpi != Some(target_dpi) {
                let stage = self
                    .read_current(CMD_ACTIVE_DPI_STAGE, &[], 4)?
                    .first()
                    .copied()
                    .unwrap_or(0);
                let enc =
                    encode_ninjutso_dpi(target_dpi, self.is_direct_dpi()).ok_or_else(|| {
                        PolledError::UnsupportedSetting {
                            device: self.display_name.clone(),
                            detail: format!("Unsupported Ninjutso DPI {target_dpi}"),
                        }
                    })?;
                self.set_control_pause(false)?;
                let write_res = self.send_current(CMD_DPI_SET, &[stage, enc[0], enc[1], enc[2]]);
                let _ = self.set_control_pause(true);
                write_res?;
            }
        }

        if let Some(target_hz) = settings.polling_rate_hz {
            if current.polling_rate_hz != Some(target_hz) {
                let code = encode_ninjutso_polling(target_hz).ok_or_else(|| {
                    PolledError::UnsupportedSetting {
                        device: self.display_name.clone(),
                        detail: format!("Unsupported Ninjutso polling rate {target_hz} Hz"),
                    }
                })?;
                self.send_current(CMD_POLLING_SET, &[code])?;
            }
        }

        if let Some(target_lod) = settings.lod {
            if current.lod != Some(target_lod) {
                self.send_current(CMD_LOD_SET, &[encode_lod(target_lod)])?;
            }
        }

        if let Some(target_ms) = settings.motion_sync {
            if current.motion_sync != Some(target_ms) {
                self.send_current(CMD_MOTION_SYNC_SET, &[u8::from(target_ms)])?;
            }
        }

        Ok(())
    }
}

/// Protocol-family auto-detector for Ninjutso Sora V2 / Sora V3 / TEN mice.
pub struct NinjutsoProtocolDriver;

impl DeviceDriver for NinjutsoProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "Ninjutso NinjaForce (Sora V2/V3 & TEN)"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();
        for dev in api.device_list() {
            let vid = dev.vendor_id();
            let pid = dev.product_id();
            let (legacy, wireless) = if vid == NINJUTSO_VID
                && (CURRENT_MOUSE_PIDS.contains(&pid) || CURRENT_RECEIVER_PIDS.contains(&pid))
            {
                (false, CURRENT_RECEIVER_PIDS.contains(&pid))
            } else if vid == NINJUTSO_LEGACY_VID
                && (LEGACY_MOUSE_PIDS.contains(&pid) || LEGACY_RECEIVER_PIDS.contains(&pid))
            {
                (true, LEGACY_RECEIVER_PIDS.contains(&pid))
            } else {
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
            let matches_reports = if legacy {
                summary.feature_reports.contains_key(&REPORT_LEGACY_CMD)
                    && summary.feature_reports.contains_key(&REPORT_LEGACY_BLOB)
            } else {
                summary.feature_reports.contains_key(&REPORT_CURRENT)
            };
            if !matches_reports {
                continue;
            }

            let product_name_os = dev.product_string().unwrap_or_default().trim().to_string();
            let initial_name = if legacy {
                "Ninjutso Sora V2".to_string()
            } else if SORA_V3_PIDS.contains(&pid) {
                "Ninjutso Sora V3".to_string()
            } else {
                "Ninjutso TEN / TEN AIR".to_string()
            };

            let mut device = NinjutsoDevice {
                hid,
                vid,
                pid,
                product_name_os,
                effective_pid: pid,
                legacy,
                wireless,
                profile: 1,
                display_name: initial_name,
                vid_pid: format!("{vid:04x}:{pid:04x}"),
            };

            if !legacy && wireless {
                if let Ok(resp) = device.read_current(CMD_PAIRED_PID, &[], 2) {
                    if resp.len() >= 2 {
                        let paired = u16::from_le_bytes([resp[0], resp[1]]);
                        device.effective_pid = paired;
                        if SORA_V3_PIDS.contains(&paired) {
                            device.display_name = "Ninjutso Sora V3".to_string();
                        } else {
                            device.display_name = "Ninjutso TEN / TEN AIR".to_string();
                        }
                    }
                }
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
    fn encodes_and_decodes_current_and_legacy_ninjutso_frames() {
        let req = build_ninjutso_current_request(CMD_DPI_SET, 1, &[0, 0x1F, 0x00, 0x00]);
        assert_eq!(req[0], REPORT_CURRENT);
        assert_eq!(req[1], CMD_DPI_SET);
        assert_eq!(req[4], 1);
        assert_eq!(req[6], 4);
        assert_eq!(req[7], 1);

        let raw_reply = [REPORT_CURRENT, CMD_POLLING_GET, 0, 0, 1, 0, 1, 1, 3];
        let val = decode_ninjutso_current_response(&raw_reply, CMD_POLLING_GET).unwrap();
        assert_eq!(decode_ninjutso_polling(val[0]), Some(4000));

        // Non-direct (50-step) and direct (Sora V3) DPI roundtrips
        let enc50 = encode_ninjutso_dpi(1600, false).unwrap();
        assert_eq!(decode_ninjutso_dpi(enc50[0], enc50[1], false), 1600);
        let enc_direct = encode_ninjutso_dpi(1600, true).unwrap();
        assert_eq!(
            decode_ninjutso_dpi(enc_direct[0], enc_direct[1], true),
            1600
        );

        let leg = encode_ninjutso_legacy_dpi(800).unwrap();
        assert_eq!(decode_ninjutso_legacy_dpi(leg[0], leg[1]), 800);
    }
}
