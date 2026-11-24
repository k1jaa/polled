// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Incott G23 / G23 V2 / G24 / Ghero / Zero 29 / Zero 39 HID feature-report driver (`0x093A:522C` / `622C`).
//!
//! Uses Report ID `0x09` 8-byte feature-report commands (`0x0N` write / `0x8N` query) on usage
//! page `0xFF05` with strict command + sub-command response matching to prevent stale-buffer reads.
//!
//! # Protocol Attribution & Licenses (AGPL-3.0 compatible)
//! Command opcodes, DPI stage table semantics, PAW3395 vs PAW3950 LOD wire mappings, polling codes,
//! sensor toggles, and dual-`0xFF05` collection probing are ported from:
//! - `OpenMouse-Project/mouse-protocol` (<https://github.com/OpenMouse-Project/mouse-protocol>)
//!   - `src/incott/index.ts`, `src/drivers/incott/hid.ts`
//!   - **AGPL-3.0-or-later**, Copyright (C) OpenMouse Project Contributors
//!
//! No HID bytes are guessed.

use std::thread;
use std::time::Duration;

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice, MouseSettings,
    SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};
use crate::hid::parse_hid_report_descriptor;

pub const INCOTT_VID: u16 = 0x093A;
pub const INCOTT_WIRELESS_PID: u16 = 0x522C;
pub const INCOTT_WIRED_PID: u16 = 0x622C;

pub const INCOTT_USAGE_PAGE: u16 = 0xFF05;
pub const INCOTT_REPORT_ID: u8 = 0x09;
pub const INCOTT_PAYLOAD_LEN: usize = 8;
pub const INCOTT_RESPONSE_LEN: usize = 64;

const CMD_SET_POLLING: u8 = 0x01;
const CMD_SET_DPI: u8 = 0x02;
const CMD_SET_SENSOR: u8 = 0x04;
const CMD_SET_TIMING: u8 = 0x05;

const CMD_QUERY_POLLING: u8 = 0x81;
const CMD_QUERY_DPI_STAGE_VALUE: u8 = 0x82;
const CMD_QUERY_DPI_STAGE: u8 = 0x83;
const CMD_QUERY_SENSOR: u8 = 0x84;
const CMD_QUERY_TIMING: u8 = 0x85;
const CMD_QUERY_IDENTITY: u8 = 0x8F;

const SUB_NONE: u8 = 0x00;
const SUB_LOD: u8 = 0x01;
const SUB_RIPPLE: u8 = 0x02;
const SUB_ANGLE_SNAP: u8 = 0x03;
const SUB_MOTION_SYNC: u8 = 0x04;
const SUB_DEBOUNCE: u8 = 0x01;

const DPI_MIN: u16 = 50;
const DPI_STEP: u16 = 50;
const DPI_MAX_PAW3395: u16 = 32_000;
const DPI_MAX_PAW3950: u16 = 45_000;
const DPI_STAGE_COUNT: u8 = 6;
const DEBOUNCE_MAX_MS: u8 = 30;

const SETTLE_MS: u64 = 45;
const QUERY_ATTEMPTS: usize = 10;

const RATES_WIRED: &[u16] = &[125, 250, 500, 1000];
const RATES_WIRELESS: &[u16] = &[125, 250, 500, 1000, 2000, 4000, 8000];

/// Builds a 9-byte Incott feature report (`[0x09, cmd, sub, args...]`).
pub fn build_incott_packet(cmd: u8, sub: u8, args: &[u8]) -> [u8; INCOTT_PAYLOAD_LEN + 1] {
    let mut pkt = [0u8; INCOTT_PAYLOAD_LEN + 1];
    pkt[0] = INCOTT_REPORT_ID;
    pkt[1] = cmd;
    pkt[2] = sub;
    let n = args.len().min(INCOTT_PAYLOAD_LEN - 2);
    pkt[3..3 + n].copy_from_slice(&args[..n]);
    pkt
}

/// Normalizes a `get_feature_report` buffer so byte 0 is always `INCOTT_REPORT_ID (0x09)`.
pub fn normalize_incott_reply(raw: &[u8]) -> Vec<u8> {
    if raw.first() == Some(&INCOTT_REPORT_ID) {
        raw.to_vec()
    } else {
        let mut out = Vec::with_capacity(raw.len() + 1);
        out.push(INCOTT_REPORT_ID);
        out.extend_from_slice(raw);
        out
    }
}

pub fn incott_frame_matches(frame: &[u8], cmd: u8, sub: Option<u8>) -> bool {
    if frame.len() < 4 || frame[0] != INCOTT_REPORT_ID || frame[1] != cmd {
        return false;
    }
    if let Some(expected_sub) = sub {
        if frame[2] != expected_sub {
            return false;
        }
    }
    true
}

pub fn encode_incott_polling(hz: u16) -> Option<u8> {
    match hz {
        1000 => Some(0),
        500 => Some(1),
        250 => Some(2),
        125 => Some(3),
        8000 => Some(4),
        4000 => Some(5),
        2000 => Some(6),
        _ => None,
    }
}

pub fn decode_incott_polling(wire: u8) -> Option<u16> {
    match wire {
        0 => Some(1000),
        1 => Some(500),
        2 => Some(250),
        3 => Some(125),
        4 => Some(8000),
        5 => Some(4000),
        6 => Some(2000),
        _ => None,
    }
}

pub fn encode_incott_lod(lod: LiftOffDistance, is_paw3395: bool) -> Option<u8> {
    match lod {
        LiftOffDistance::Mm10 => Some(0),
        LiftOffDistance::Mm20 => Some(1),
        LiftOffDistance::Mm07 if !is_paw3395 => Some(2),
        LiftOffDistance::Mm07 => None,
    }
}

pub fn decode_incott_lod(wire: u8, is_paw3395: bool) -> Option<LiftOffDistance> {
    match wire {
        0 => Some(LiftOffDistance::Mm10),
        1 => Some(LiftOffDistance::Mm20),
        2 if !is_paw3395 => Some(LiftOffDistance::Mm07),
        _ => None,
    }
}

pub fn decode_incott_dpi(lo: u8, hi: u8) -> Option<u16> {
    let wire = u16::from_le_bytes([lo, hi]);
    let dpi = wire.checked_add(1)?.checked_mul(DPI_STEP)?;
    (DPI_MIN..=DPI_MAX_PAW3950).contains(&dpi).then_some(dpi)
}

pub fn encode_incott_dpi_args(dpi: u16) -> Option<[u8; 6]> {
    if !(DPI_MIN..=DPI_MAX_PAW3950).contains(&dpi) || !dpi.is_multiple_of(DPI_STEP) {
        return None;
    }
    let wire = dpi / DPI_STEP - 1;
    let le = wire.to_le_bytes();
    // [lo, hi, 0, 0, 0, axis=0 (both)]
    Some([le[0], le[1], 0, 0, 0, 0])
}

fn decode_incott_model(code: u8) -> Option<&'static str> {
    match code {
        0x01 => Some("Ghero"),
        0x02 => Some("G23"),
        0x03 => Some("G24"),
        0x06 => Some("Zero 29"),
        0x08 | 0x0E => Some("G23 V2"),
        0x09 => Some("Zero 39"),
        _ => None,
    }
}

/// Native Windows `hidapi` driver for Incott G23 / G24 / Ghero / Zero mice.
pub struct IncottDevice {
    hid: HidDevice,
    pid: u16,
    is_paw3395: bool,
    display_name: String,
    vid_pid: String,
}

impl IncottDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.display_name.clone(),
            message: message.into(),
        }
    }

    fn is_wired(&self) -> bool {
        self.pid == INCOTT_WIRED_PID
    }

    fn read_once(hid: &HidDevice) -> Option<Vec<u8>> {
        let mut buf = [0u8; INCOTT_RESPONSE_LEN + 2];
        buf[0] = INCOTT_REPORT_ID;
        let n = hid.get_feature_report(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        Some(normalize_incott_reply(&buf[..n]))
    }

    fn query_raw(
        hid: &HidDevice,
        cmd: u8,
        sub: u8,
        match_sub: Option<u8>,
        attempts: usize,
    ) -> std::result::Result<Vec<u8>, String> {
        // Discard any previously latched response buffer before issuing the query.
        let _ = Self::read_once(hid);
        let pkt = build_incott_packet(cmd, sub, &[]);
        hid.send_feature_report(&pkt)
            .map_err(|e| format!("Incott feature write 0x{cmd:02x}/0x{sub:02x} failed: {e}"))?;

        for _ in 0..attempts {
            thread::sleep(Duration::from_millis(SETTLE_MS));
            if let Some(frame) = Self::read_once(hid) {
                if incott_frame_matches(&frame, cmd, match_sub) {
                    return Ok(frame);
                }
            }
        }
        Err(format!(
            "Timed out waiting for Incott reply 0x{cmd:02x}/0x{sub:02x} (move mouse to wake)"
        ))
    }

    fn query(&self, cmd: u8, sub: u8, match_sub: Option<u8>) -> Result<Vec<u8>> {
        Self::query_raw(&self.hid, cmd, sub, match_sub, QUERY_ATTEMPTS)
            .map_err(|msg| self.dev_err(msg))
    }

    fn send_write(&self, cmd: u8, sub: u8, args: &[u8]) -> Result<()> {
        let pkt = build_incott_packet(cmd, sub, args);
        self.hid.send_feature_report(&pkt).map_err(|e| {
            self.dev_err(format!("Incott write 0x{cmd:02x}/0x{sub:02x} failed: {e}"))
        })?;
        thread::sleep(Duration::from_millis(SETTLE_MS));
        Ok(())
    }

    fn read_active_stage(&self) -> Result<u8> {
        let frame = self.query(CMD_QUERY_DPI_STAGE, SUB_NONE, None)?;
        let count = frame[2].clamp(1, DPI_STAGE_COUNT);
        let active = frame[3].min(count - 1);
        Ok(active)
    }
}

impl MouseDevice for IncottDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: format!("incott_{:04x}", self.pid),
            name: self.display_name.clone(),
            vendor: "Incott".to_string(),
            vid_pid: Some(self.vid_pid.clone()),
            connection: if self.is_wired() {
                "Wired USB (1K max)".to_string()
            } else {
                "Wireless 2.4 GHz Receiver (8K)".to_string()
            },
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            dpi_min: DPI_MIN,
            dpi_max: if self.is_paw3395 {
                DPI_MAX_PAW3395
            } else {
                DPI_MAX_PAW3950
            },
            dpi_step: DPI_STEP,
            polling_rates_hz: if self.is_wired() {
                RATES_WIRED.to_vec()
            } else {
                RATES_WIRELESS.to_vec()
            },
            lod_options: if self.is_paw3395 {
                vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20]
            } else {
                LiftOffDistance::ALL.to_vec()
            },
            supports_debounce: true,
            debounce_min_ms: 0,
            debounce_max_ms: DEBOUNCE_MAX_MS,
            supports_motion_sync: true,
            supports_angle_snapping: true,
            supports_ripple_control: true,
            supports_volatile_writes: false,
            flash_write_warning: Some(format!(
                "{} persists settings to onboard memory. Polled diffs unchanged values and rate-limits rapid writes.",
                self.display_name
            )),
        }
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        let stage = self.read_active_stage()?;
        let dpi_frame = self.query(CMD_QUERY_DPI_STAGE_VALUE, stage, Some(stage))?;
        let dpi = if dpi_frame.len() >= 5 {
            decode_incott_dpi(dpi_frame[3], dpi_frame[4])
        } else {
            None
        };

        let poll_frame = self.query(CMD_QUERY_POLLING, SUB_NONE, None)?;
        let polling_rate_hz = decode_incott_polling(poll_frame[2]);

        let lod = self
            .query(CMD_QUERY_SENSOR, SUB_LOD, Some(SUB_LOD))
            .ok()
            .and_then(|f| decode_incott_lod(f[3], self.is_paw3395));

        let debounce_ms = self
            .query(CMD_QUERY_TIMING, SUB_DEBOUNCE, Some(SUB_DEBOUNCE))
            .ok()
            .map(|f| f[3])
            .filter(|&v| v <= DEBOUNCE_MAX_MS);

        let read_toggle = |sub: u8| -> Option<bool> {
            let f = self.query(CMD_QUERY_SENSOR, sub, Some(sub)).ok()?;
            match f[3] {
                0 => Some(false),
                1 => Some(true),
                _ => None,
            }
        };

        let motion_sync = read_toggle(SUB_MOTION_SYNC);
        let angle_snapping = read_toggle(SUB_ANGLE_SNAP);
        let ripple_control = read_toggle(SUB_RIPPLE);

        Ok(MouseSettings {
            dpi,
            polling_rate_hz,
            lod,
            debounce_ms,
            motion_sync,
            angle_snapping,
            ripple_control,
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        let current = self.read_settings()?;

        if let Some(target_dpi) = settings.dpi {
            if current.dpi != Some(target_dpi) {
                let stage = self.read_active_stage()?;
                let args = encode_incott_dpi_args(target_dpi).ok_or_else(|| {
                    PolledError::UnsupportedSetting {
                        device: self.display_name.clone(),
                        detail: format!("Unsupported Incott DPI {target_dpi}"),
                    }
                })?;
                self.send_write(CMD_SET_DPI, stage, &args)?;
            }
        }

        if let Some(target_hz) = settings.polling_rate_hz {
            if current.polling_rate_hz != Some(target_hz) {
                let wire = encode_incott_polling(target_hz).ok_or_else(|| {
                    PolledError::UnsupportedSetting {
                        device: self.display_name.clone(),
                        detail: format!("Unsupported Incott polling rate {target_hz} Hz"),
                    }
                })?;
                self.send_write(CMD_SET_POLLING, wire, &[])?;
            }
        }

        if let Some(target_lod) = settings.lod {
            if current.lod != Some(target_lod) {
                let wire = encode_incott_lod(target_lod, self.is_paw3395).ok_or_else(|| {
                    PolledError::UnsupportedSetting {
                        device: self.display_name.clone(),
                        detail: format!("LOD {target_lod} not supported on {}", self.display_name),
                    }
                })?;
                self.send_write(CMD_SET_SENSOR, SUB_LOD, &[wire])?;
            }
        }

        if let Some(target_db) = settings.debounce_ms {
            if current.debounce_ms != Some(target_db) {
                self.send_write(CMD_SET_TIMING, SUB_DEBOUNCE, &[target_db])?;
            }
        }

        if let Some(target_ms) = settings.motion_sync {
            if current.motion_sync != Some(target_ms) {
                self.send_write(CMD_SET_SENSOR, SUB_MOTION_SYNC, &[u8::from(target_ms)])?;
            }
        }

        if let Some(target_ang) = settings.angle_snapping {
            if current.angle_snapping != Some(target_ang) {
                self.send_write(CMD_SET_SENSOR, SUB_ANGLE_SNAP, &[u8::from(target_ang)])?;
            }
        }

        if let Some(target_rip) = settings.ripple_control {
            if current.ripple_control != Some(target_rip) {
                self.send_write(CMD_SET_SENSOR, SUB_RIPPLE, &[u8::from(target_rip)])?;
            }
        }

        Ok(())
    }
}

/// Protocol-family auto-detector for Incott G23 / G24 / Ghero / Zero mice.
pub struct IncottProtocolDriver;

impl DeviceDriver for IncottProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "Incott G23 / G24 / Ghero (0x093A)"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();
        let mut claimed_pids: Vec<u16> = Vec::new();

        for dev in api.device_list() {
            let vid = dev.vendor_id();
            let pid = dev.product_id();
            if vid != INCOTT_VID
                || !matches!(pid, INCOTT_WIRELESS_PID | INCOTT_WIRED_PID)
                || dev.usage_page() < 0xFF00
                || claimed_pids.contains(&pid)
            {
                continue;
            }
            let Ok(hid) = dev.open_device(api) else {
                continue;
            };
            let mut desc_buf = [0u8; 4096];
            let has_feat_09 = hid
                .get_report_descriptor(&mut desc_buf)
                .ok()
                .map(|len| parse_hid_report_descriptor(&desc_buf[..len]))
                .is_some_and(|s| s.feature_reports.contains_key(&INCOTT_REPORT_ID));
            if !has_feat_09 {
                continue;
            }

            // Wired Incott exposes two 0xFF05 collections; probe read-only 0x8F identity
            // so only the real responding collection is claimed.
            let Ok(id_frame) = IncottDevice::query_raw(&hid, CMD_QUERY_IDENTITY, SUB_NONE, None, 3)
            else {
                continue;
            };

            claimed_pids.push(pid);
            let mut is_paw3395 = false;
            let mut display_name = if pid == INCOTT_WIRED_PID {
                "Incott Mouse (Wired)".to_string()
            } else {
                "Incott 8K Wireless Mouse".to_string()
            };

            if id_frame.len() >= 8 && id_frame[2] == 0x01 {
                let model = decode_incott_model(id_frame[3]);
                // Sensor byte is 0xF1 for PAW3950 ("Pro"), else PAW3395.
                let is_pro = id_frame.get(5).copied() == Some(0xF1)
                    || id_frame.get(6).copied() == Some(0xF1);
                is_paw3395 = !is_pro;
                if let Some(m) = model {
                    display_name = if is_pro {
                        format!("Incott {m} Pro")
                    } else {
                        format!("Incott {m}")
                    };
                }
            }

            found.push(Box::new(IncottDevice {
                hid,
                pid,
                is_paw3395,
                display_name,
                vid_pid: format!("{vid:04x}:{pid:04x}"),
            }));
        }

        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_incott_dpi_polling_and_lod() {
        let args = encode_incott_dpi_args(1600).unwrap();
        assert_eq!(args[0], 0x1F);
        assert_eq!(args[1], 0x00);
        assert_eq!(decode_incott_dpi(args[0], args[1]), Some(1600));

        let args25k = encode_incott_dpi_args(25_000).unwrap();
        assert_eq!(args25k[0], 0xF3);
        assert_eq!(args25k[1], 0x01);
        assert_eq!(decode_incott_dpi(args25k[0], args25k[1]), Some(25_000));

        for hz in [125u16, 250, 500, 1000, 2000, 4000, 8000] {
            assert_eq!(
                decode_incott_polling(encode_incott_polling(hz).unwrap()),
                Some(hz)
            );
        }

        // PAW3950: 0=1mm, 1=2mm, 2=0.7mm
        assert_eq!(encode_incott_lod(LiftOffDistance::Mm07, false), Some(2));
        assert_eq!(encode_incott_lod(LiftOffDistance::Mm10, false), Some(0));
        assert_eq!(encode_incott_lod(LiftOffDistance::Mm20, false), Some(1));
        assert_eq!(decode_incott_lod(2, false), Some(LiftOffDistance::Mm07));
        // PAW3395 does not offer 0.7mm
        assert_eq!(encode_incott_lod(LiftOffDistance::Mm07, true), None);
        assert_eq!(decode_incott_lod(2, true), None);
    }

    #[test]
    fn validates_incott_frame_matching_and_normalization() {
        let raw_no_id = [CMD_QUERY_SENSOR, SUB_MOTION_SYNC, 1, 0, 0, 0];
        let norm = normalize_incott_reply(&raw_no_id);
        assert!(incott_frame_matches(
            &norm,
            CMD_QUERY_SENSOR,
            Some(SUB_MOTION_SYNC)
        ));
        assert!(!incott_frame_matches(
            &norm,
            CMD_QUERY_SENSOR,
            Some(SUB_LOD)
        ));
    }
}
