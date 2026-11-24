// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Finalmouse UltralightX (ULX) & Starlight X (SLX) HID output/input report protocol driver.
//!
//! # Protocol Attribution & Licenses (AGPL-3.0 compatible)
//! Report IDs, command numbers, 63-byte framing, PAW lift-off mappings, and telemetry decoders
//! are ported from:
//! - `OpenMouse-Project/mouse-protocol` (<https://github.com/OpenMouse-Project/mouse-protocol>)
//!   - `src/finalmouse/index.ts`, `src/drivers/finalmouse/hid.ts`
//!   - **AGPL-3.0-or-later**, Copyright (C) OpenMouse Project Contributors
//!
//! No HID bytes are guessed.

use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    BatteryStatus, DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice,
    MouseSettings, SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};

pub const FINALMOUSE_VID: u16 = 0x361D;
pub const FINALMOUSE_ULX_DONGLE_PID: u16 = 0x0100;
pub const FINALMOUSE_SLX_DONGLE_PIDS: &[u16] = &[0x0300, 0x0301];

pub const REPORT_ULX_OUT: u8 = 0x04;
pub const REPORT_ULX_IN: u8 = 0x05;
pub const REPORT_SLX_OUT: u8 = 0x01;
pub const REPORT_SLX_IN: u8 = 0x02;

const OUTPUT_PAYLOAD_LEN: usize = 63;

const CMD_DPI: u8 = 16;
const CMD_POLLING_RATE: u8 = 17;
const CMD_MOTION_SYNC: u8 = 18;
const CMD_LOD_ULX: u8 = 21;
const CMD_LOD_SLX_PAW: u8 = 25;
const CMD_WAKE_ALL: u8 = 96;

const ULX_POLLING_RATES: &[u16] = &[500, 1000, 2000, 4000, 8000];

/// Finalmouse ULX battery-voltage calibration curve (from OpenMouse / xpanel).
pub fn finalmouse_battery_percent(millivolts: u16) -> u8 {
    if millivolts == 0 {
        return 0;
    }
    let v = f64::from(millivolts) / 1000.0;
    const VOLTAGES: [f64; 7] = [3.0, 3.62, 3.66, 3.74, 3.88, 4.17, 4.38];
    const PCTS: [f64; 7] = [0.2, 5.0, 10.0, 25.0, 50.0, 75.0, 100.0];
    if v >= VOLTAGES[6] {
        return 100;
    }
    if v <= VOLTAGES[0] {
        return 0;
    }
    for i in 0..6 {
        if v >= VOLTAGES[i] && v <= VOLTAGES[i + 1] {
            let frac = (v - VOLTAGES[i]) / (VOLTAGES[i + 1] - VOLTAGES[i]);
            return (PCTS[i] + (PCTS[i + 1] - PCTS[i]) * frac)
                .round()
                .clamp(0.0, 100.0) as u8;
        }
    }
    0
}

/// Builds a 64-byte Finalmouse HID output report (`[report_id, 2 + len, 0x80 | cmd, len, payload...]`).
pub fn build_finalmouse_packet(
    report_id: u8,
    command: u8,
    payload: &[u8],
) -> [u8; OUTPUT_PAYLOAD_LEN + 1] {
    let mut pkt = [0u8; OUTPUT_PAYLOAD_LEN + 1];
    let n = payload.len().min(OUTPUT_PAYLOAD_LEN - 3);
    pkt[0] = report_id;
    pkt[1] = (2 + n) as u8;
    pkt[2] = 0x80 | (command & 0x7F);
    pkt[3] = n as u8;
    pkt[4..4 + n].copy_from_slice(&payload[..n]);
    pkt
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FinalmouseTelemetry {
    pub dpi: Option<u16>,
    pub polling_rate_hz: Option<u16>,
    pub ulx_lod_mm: Option<u8>,
    pub slx_paw_lod: Option<LiftOffDistance>,
    pub motion_sync: Option<bool>,
    pub battery_soc: Option<u8>,
    pub battery_percent: Option<u8>,
    pub battery_voltage_mv: Option<u16>,
    pub charging_state: Option<u8>,
}

/// Decodes one Finalmouse input report (with leading `report_id` at byte 0).
pub fn decode_finalmouse_packet(raw: &[u8], out: &mut FinalmouseTelemetry) -> bool {
    if raw.len() < 4 {
        return false;
    }
    let report_id = raw[0];
    if report_id != REPORT_ULX_IN && report_id != REPORT_SLX_IN {
        return false;
    }
    let data = &raw[1..];
    let inner_len = usize::from(data[0]).min(data.len().saturating_sub(1));
    if inner_len < 2 {
        return false;
    }
    let body = &data[1..1 + inner_len];
    let cmd = body[0];
    let payload_len = usize::from(body[1]).min(body.len().saturating_sub(2));
    let payload = &body[2..2 + payload_len];

    match cmd {
        3 | CMD_DPI if payload.len() >= 2 => {
            out.dpi = Some(u16::from_le_bytes([payload[0], payload[1]]));
            true
        }
        4 | CMD_POLLING_RATE if payload.len() >= 2 => {
            out.polling_rate_hz = Some(u16::from_le_bytes([payload[0], payload[1]]));
            true
        }
        5 if payload.len() >= 2 => {
            let mv = u16::from_le_bytes([payload[0], payload[1]]);
            out.battery_voltage_mv = Some(mv);
            out.battery_percent = Some(finalmouse_battery_percent(mv));
            true
        }
        CMD_MOTION_SYNC if !payload.is_empty() => {
            out.motion_sync = Some(payload[0] == 1);
            true
        }
        CMD_LOD_ULX if !payload.is_empty() => {
            out.ulx_lod_mm = Some(payload[0]);
            true
        }
        CMD_LOD_SLX_PAW if !payload.is_empty() => {
            out.slx_paw_lod = decode_slx_paw_lod(payload[0]);
            true
        }
        37 if !payload.is_empty() => {
            out.charging_state = Some(payload[0]);
            true
        }
        38 if payload.len() >= 3 => {
            out.battery_soc = Some(payload[0].min(100));
            let mv = u16::from_le_bytes([payload[1], payload[2]]);
            if mv > 0 {
                out.battery_voltage_mv = Some(mv);
            }
            true
        }
        _ => false,
    }
}

pub fn decode_slx_paw_lod(raw: u8) -> Option<LiftOffDistance> {
    match raw {
        1 | 7..=9 => Some(LiftOffDistance::Mm07),
        2 | 10..=14 => Some(LiftOffDistance::Mm10),
        3 | 15..=20 => Some(LiftOffDistance::Mm20),
        _ => None,
    }
}

pub fn encode_slx_paw_lod(lod: LiftOffDistance) -> u8 {
    match lod {
        LiftOffDistance::Mm07 => 7,
        LiftOffDistance::Mm10 => 2,
        LiftOffDistance::Mm20 => 3,
    }
}

/// Hardware driver for Finalmouse UltralightX & Starlight X dongles.
pub struct FinalmouseDevice {
    hid: HidDevice,
    pid: u16,
    is_slx: bool,
    display_name: String,
    vid_pid: String,
}

impl FinalmouseDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.display_name.clone(),
            message: message.into(),
        }
    }

    fn tx_report(&self) -> u8 {
        if self.is_slx {
            REPORT_SLX_OUT
        } else {
            REPORT_ULX_OUT
        }
    }

    fn send_cmd(&self, command: u8, payload: &[u8]) -> Result<()> {
        let pkt = build_finalmouse_packet(self.tx_report(), command, payload);
        self.hid
            .write(&pkt)
            .map_err(|e| self.dev_err(format!("Finalmouse HID write cmd={command} failed: {e}")))?;
        Ok(())
    }

    fn query_telemetry(&self) -> Result<FinalmouseTelemetry> {
        // Request LOD and trigger wakeAll telemetry burst.
        if self.is_slx {
            self.send_cmd(CMD_LOD_SLX_PAW, &[])?;
        } else {
            self.send_cmd(CMD_LOD_ULX, &[])?;
        }
        self.send_cmd(CMD_WAKE_ALL, &[])?;

        let mut telemetry = FinalmouseTelemetry::default();
        let deadline = Instant::now() + Duration::from_millis(900);
        let mut buf = [0u8; 64];
        let mut received_any = false;

        while Instant::now() < deadline {
            let timeout = if received_any { 80 } else { 250 };
            let n = self
                .hid
                .read_timeout(&mut buf, timeout)
                .map_err(|e| self.dev_err(format!("Finalmouse HID read failed: {e}")))?;
            if n == 0 {
                if received_any {
                    break;
                }
                continue;
            }
            if decode_finalmouse_packet(&buf[..n], &mut telemetry) {
                received_any = true;
            }
        }

        if telemetry.dpi.is_none() || telemetry.polling_rate_hz.is_none() {
            return Err(self.dev_err(
                "Finalmouse did not return DPI/polling telemetry (ensure mouse is awake and close XPANEL)",
            ));
        }
        Ok(telemetry)
    }
}

impl MouseDevice for FinalmouseDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: format!("finalmouse_{:04x}", self.pid),
            name: self.display_name.clone(),
            vendor: "Finalmouse".to_string(),
            vid_pid: Some(self.vid_pid.clone()),
            connection: "Wireless 2.4 GHz Receiver".to_string(),
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            dpi_min: 50,
            dpi_max: 26_000,
            dpi_step: 50,
            // Starlight X uses fixed PerfectPolling; UltralightX exposes 500..8000 Hz.
            polling_rates_hz: if self.is_slx {
                Vec::new()
            } else {
                ULX_POLLING_RATES.to_vec()
            },
            lod_options: if self.is_slx {
                LiftOffDistance::ALL.to_vec()
            } else {
                vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20]
            },
            supports_debounce: false,
            debounce_min_ms: 0,
            debounce_max_ms: 0,
            supports_motion_sync: true,
            supports_angle_snapping: false,
            supports_ripple_control: false,
            supports_volatile_writes: false,
            flash_write_warning: Some(format!(
                "{} persists settings to onboard memory. Polled diffs unchanged values and rate-limits rapid writes.",
                self.display_name
            )),
        }
    }

    fn read_battery(&mut self) -> Option<BatteryStatus> {
        let t = self.query_telemetry().ok()?;
        let percent = t.battery_soc.or(t.battery_percent)?;
        let charging = matches!(t.charging_state, Some(1 | 2));
        Some(BatteryStatus {
            percent: percent.min(100),
            charging,
            voltage_mv: t.battery_voltage_mv.filter(|&mv| mv > 0),
        })
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        let t = self.query_telemetry()?;
        let lod = if self.is_slx {
            t.slx_paw_lod
        } else {
            match t.ulx_lod_mm {
                Some(1) => Some(LiftOffDistance::Mm10),
                Some(2) => Some(LiftOffDistance::Mm20),
                _ => None,
            }
        };
        Ok(MouseSettings {
            dpi: t.dpi,
            polling_rate_hz: t.polling_rate_hz,
            lod,
            debounce_ms: None,
            motion_sync: t.motion_sync,
            angle_snapping: None,
            ripple_control: None,
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        let current = self.read_settings()?;

        if let Some(target_dpi) = settings.dpi {
            if current.dpi != Some(target_dpi) {
                self.send_cmd(CMD_DPI, &target_dpi.to_le_bytes())?;
            }
        }
        if let Some(target_hz) = settings.polling_rate_hz {
            if !self.is_slx && current.polling_rate_hz != Some(target_hz) {
                self.send_cmd(CMD_POLLING_RATE, &target_hz.to_le_bytes())?;
            }
        }
        if let Some(target_lod) = settings.lod {
            if current.lod != Some(target_lod) {
                if self.is_slx {
                    self.send_cmd(CMD_LOD_SLX_PAW, &[encode_slx_paw_lod(target_lod)])?;
                } else {
                    let mm = match target_lod {
                        LiftOffDistance::Mm10 => 1u8,
                        LiftOffDistance::Mm20 => 2u8,
                        LiftOffDistance::Mm07 => {
                            return Err(PolledError::UnsupportedSetting {
                                device: self.display_name.clone(),
                                detail: "UltralightX only supports 1.0 mm or 2.0 mm LOD"
                                    .to_string(),
                            });
                        }
                    };
                    self.send_cmd(CMD_LOD_ULX, &[mm])?;
                }
            }
        }
        if let Some(target_ms) = settings.motion_sync {
            if current.motion_sync != Some(target_ms) {
                self.send_cmd(CMD_MOTION_SYNC, &[u8::from(target_ms)])?;
            }
        }

        Ok(())
    }
}

/// Protocol-family auto-detector for Finalmouse UltralightX & Starlight X dongles.
pub struct FinalmouseProtocolDriver;

impl DeviceDriver for FinalmouseProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "Finalmouse ULX / Starlight X (0x361D)"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();
        for dev in api.device_list() {
            if dev.vendor_id() != FINALMOUSE_VID || dev.usage_page() != 0xFF00 {
                continue;
            }
            let pid = dev.product_id();
            let is_ulx = pid == FINALMOUSE_ULX_DONGLE_PID && dev.usage() == 0x0001;
            let is_slx = FINALMOUSE_SLX_DONGLE_PIDS.contains(&pid);
            if !is_ulx && !is_slx {
                continue;
            }
            let Ok(hid) = dev.open_device(api) else {
                continue;
            };
            let display_name = if is_slx {
                "Finalmouse Starlight X".to_string()
            } else {
                "Finalmouse UltralightX".to_string()
            };
            found.push(Box::new(FinalmouseDevice {
                hid,
                pid,
                is_slx,
                display_name,
                vid_pid: format!("{:04x}:{:04x}", FINALMOUSE_VID, pid),
            }));
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_ulx_and_slx_packets() {
        let pkt = build_finalmouse_packet(REPORT_ULX_OUT, CMD_DPI, &1600u16.to_le_bytes());
        assert_eq!(pkt[0], REPORT_ULX_OUT);
        assert_eq!(pkt[1], 4);
        assert_eq!(pkt[2], 0x80 | CMD_DPI);
        assert_eq!(pkt[3], 2);
        assert_eq!(&pkt[4..6], &[0x40, 0x06]);

        let mut t = FinalmouseTelemetry::default();
        // ULX DPI input report (cmd 3, u16le = 1600)
        let dpi_in = [REPORT_ULX_IN, 4, 3, 2, 0x40, 0x06];
        assert!(decode_finalmouse_packet(&dpi_in, &mut t));
        assert_eq!(t.dpi, Some(1600));

        // SLX polling input report (cmd 17, u16le = 4000)
        let poll_in = [REPORT_SLX_IN, 4, CMD_POLLING_RATE, 2, 0xA0, 0x0F];
        assert!(decode_finalmouse_packet(&poll_in, &mut t));
        assert_eq!(t.polling_rate_hz, Some(4000));

        assert_eq!(
            decode_slx_paw_lod(encode_slx_paw_lod(LiftOffDistance::Mm07)),
            Some(LiftOffDistance::Mm07)
        );
        assert_eq!(
            decode_slx_paw_lod(encode_slx_paw_lod(LiftOffDistance::Mm10)),
            Some(LiftOffDistance::Mm10)
        );
        assert_eq!(
            decode_slx_paw_lod(encode_slx_paw_lod(LiftOffDistance::Mm20)),
            Some(LiftOffDistance::Mm20)
        );
    }
}
