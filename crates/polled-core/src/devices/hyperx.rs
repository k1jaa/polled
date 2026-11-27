// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! HyperX / Kingston Pulsefire Haste 64-byte (`usage_page 0xFF00`, `usage 0x0001`) HID protocol driver.
//!
//! Covers both Kingston-era and HP-era Pulsefire Haste wired and wireless models:
//! - **HyperX Pulsefire Haste Wired (Kingston)** — `0951:1727`
//! - **HyperX Pulsefire Haste Wired (HP)** — `03f0:0f8f`, `03f0:048e`
//! - **HyperX Pulsefire Haste Wireless Dongle (HP)** — `03f0:028e`
//!
//! # Volatile RAM & Read-Back Safety
//! - The Pulsefire Haste operates in direct RAM mode: `0xD3` writes take effect immediately in live
//!   memory without touching onboard flash unless `0xDE 0xFF` (`SAVE`) is sent. Polled therefore
//!   uses `WriteMode::VolatileRam` during automatic profile switching (zero flash wear) and only
//!   issues `0xDE 0xFF` when `WriteMode::OnboardFlash` is explicitly requested.
//! - Every writable field exposed by this backend (DPI `200–16000` step 100, active DPI stage 1–5,
//!   DPI stage lock, and Lift-Off Distance `1.0 / 2.0 mm`) is verified via mandatory read-back of
//!   `GET_DPI (0x53)`. Polling rate (`0xD0`) is deliberately **not** exposed because Pulsefire
//!   Haste firmware provides no read-back command for polling rate.
//!
//! # Protocol Attribution & License (AGPL-3.0 compatible)
//! Every USB VID/PID, 64-byte interrupt OUT/IN command, BCD firmware layout, battery layout, and
//! DPI/LOD encoding comes verbatim from:
//! - `OpenMouse-Project/mouse-protocol` (<https://github.com/OpenMouse-Project/mouse-protocol>)
//!   - `src/hyperx/index.ts` (adapted from `evan-razzaque/open-pulsefire-haste` and
//!     `santeri3700/hyperx_pulsefire_dart_reverse_engineering`)
//!   - **AGPL-3.0-or-later**, Copyright (C) OpenMouse Project Contributors
//!
//! No HID bytes are guessed. Destructive opcodes are never encoded or issued.

use std::collections::HashSet;
use std::thread;
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    BatteryStatus, DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice,
    MouseSettings, SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};

pub const HYPERX_VID_KINGSTON: u16 = 0x0951;
pub const HYPERX_VID_HP: u16 = 0x03F0;

pub const HYPERX_USAGE_PAGE: u16 = 0xFF00;
pub const HYPERX_USAGE: u16 = 0x0001;
pub const HYPERX_REPORT_ID: u8 = 0x00;
pub const HYPERX_PACKET_LEN: usize = 64;

// --- Verified HyperX Pulsefire Haste opcodes ---
const CMD_GET_HARDWARE_INFO: u8 = 0x50;
const CMD_GET_BATTERY: u8 = 0x51;
const CMD_GET_DPI: u8 = 0x53;

const CMD_SET_DPI: u8 = 0xD3;
const CMD_SAVE: u8 = 0xDE;

const DPI_MIN: u16 = 200;
const DPI_MAX: u16 = 16_000;
const DPI_STEP: u16 = 100;
const STAGE_COUNT: usize = 5;

const READ_TIMEOUT: Duration = Duration::from_millis(800);
const WRITE_SETTLE_DELAY: Duration = Duration::from_millis(20);

const LOD_OPTIONS: &[LiftOffDistance] = &[LiftOffDistance::Mm10, LiftOffDistance::Mm20];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HyperxProductSpec {
    pub vid: u16,
    pub pid: u16,
    pub model: &'static str,
    pub wireless: bool,
}

pub const HYPERX_PRODUCTS: &[HyperxProductSpec] = &[
    HyperxProductSpec {
        vid: HYPERX_VID_KINGSTON,
        pid: 0x1727,
        model: "HyperX Pulsefire Haste",
        wireless: false,
    },
    HyperxProductSpec {
        vid: HYPERX_VID_HP,
        pid: 0x0F8F,
        model: "HyperX Pulsefire Haste",
        wireless: false,
    },
    HyperxProductSpec {
        vid: HYPERX_VID_HP,
        pid: 0x048E,
        model: "HyperX Pulsefire Haste",
        wireless: false,
    },
    HyperxProductSpec {
        vid: HYPERX_VID_HP,
        pid: 0x028E,
        model: "HyperX Pulsefire Haste Wireless",
        wireless: true,
    },
];

pub fn lookup_hyperx_product(vid: u16, pid: u16) -> Option<HyperxProductSpec> {
    HYPERX_PRODUCTS
        .iter()
        .copied()
        .find(|p| p.vid == vid && p.pid == pid)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperxHardwareInfo {
    pub vid: u16,
    pub pid: u16,
    pub firmware: String,
    pub product: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperxDpiState {
    /// Active stage index (`0..5`).
    pub active_stage: u8,
    /// 5-bit enabled stage bitmask (`bit i` = stage `i` enabled).
    pub enabled_mask: u8,
    /// DPI for each of the 5 onboard stages (0 if unset).
    pub stages: [u16; STAGE_COUNT],
    pub lod: Option<LiftOffDistance>,
    pub max_dpi: u16,
}

fn bcd_byte(byte: u8) -> u8 {
    ((byte >> 4) & 0x0F) * 10 + (byte & 0x0F)
}

pub fn decode_hardware_info(rsp: &[u8; HYPERX_PACKET_LEN]) -> Option<HyperxHardwareInfo> {
    if rsp[0] != CMD_GET_HARDWARE_INFO {
        return None;
    }
    let pid = u16::from_le_bytes([rsp[4], rsp[5]]);
    let vid = u16::from_le_bytes([rsp[6], rsp[7]]);
    let firmware = format!(
        "{}.{}.{}.{}",
        bcd_byte(rsp[11]),
        bcd_byte(rsp[10]),
        bcd_byte(rsp[9]),
        bcd_byte(rsp[8])
    );
    let end = rsp[20..]
        .iter()
        .position(|&b| b == 0)
        .map(|off| 20 + off)
        .unwrap_or(HYPERX_PACKET_LEN);
    let product = String::from_utf8_lossy(&rsp[20..end]).trim().to_string();
    Some(HyperxHardwareInfo {
        vid,
        pid,
        firmware,
        product,
    })
}

pub fn decode_battery(rsp: &[u8; HYPERX_PACKET_LEN]) -> Option<BatteryStatus> {
    if rsp[0] != CMD_GET_BATTERY {
        return None;
    }
    let pct = rsp[4];
    if pct == 0 || pct > 100 {
        return None;
    }
    let charging = matches!(rsp[5], 0x01 | 0x02);
    Some(BatteryStatus {
        percent: pct,
        charging,
        voltage_mv: None,
    })
}

pub fn decode_lod(mm: u8) -> Option<LiftOffDistance> {
    match mm {
        1 => Some(LiftOffDistance::Mm10),
        2 => Some(LiftOffDistance::Mm20),
        _ => None,
    }
}

pub fn encode_lod(lod: LiftOffDistance) -> u8 {
    match lod {
        LiftOffDistance::Mm07 | LiftOffDistance::Mm10 => 1,
        LiftOffDistance::Mm20 => 2,
    }
}

pub fn decode_dpi_state(rsp: &[u8; HYPERX_PACKET_LEN]) -> Option<HyperxDpiState> {
    if rsp[0] != CMD_GET_DPI {
        return None;
    }
    let mut stages = [0u16; STAGE_COUNT];
    for (i, slot) in stages.iter_mut().enumerate() {
        let raw = u16::from_le_bytes([rsp[12 + i * 2], rsp[13 + i * 2]]);
        *slot = raw.saturating_mul(DPI_STEP);
    }
    let active_stage = if rsp[4] < STAGE_COUNT as u8 {
        rsp[4]
    } else {
        0
    };
    let raw_max = u16::from(rsp[8]).saturating_mul(DPI_STEP);
    let max_dpi = if (DPI_MIN..=DPI_MAX).contains(&raw_max) {
        raw_max
    } else {
        DPI_MAX
    };
    Some(HyperxDpiState {
        active_stage,
        enabled_mask: rsp[5] & 0x1F,
        stages,
        lod: decode_lod(rsp[37]),
        max_dpi,
    })
}

pub fn build_query_packet(cmd: u8) -> [u8; HYPERX_PACKET_LEN] {
    let mut pkt = [0u8; HYPERX_PACKET_LEN];
    pkt[0] = cmd;
    pkt
}

/// `[0xD3, 0x00, 0x00, 0x01, stage_0based]`
pub fn build_select_stage_packet(stage_0based: u8) -> [u8; HYPERX_PACKET_LEN] {
    let mut pkt = [0u8; HYPERX_PACKET_LEN];
    pkt[0] = CMD_SET_DPI;
    pkt[1] = 0x00;
    pkt[3] = 0x01;
    pkt[4] = stage_0based & 0x1F;
    pkt
}

/// `[0xD3, 0x01, 0x00, 0x01, bitmask]`
pub fn build_enable_stages_packet(bitmask: u8) -> [u8; HYPERX_PACKET_LEN] {
    let mut pkt = [0u8; HYPERX_PACKET_LEN];
    pkt[0] = CMD_SET_DPI;
    pkt[1] = 0x01;
    pkt[3] = 0x01;
    pkt[4] = bitmask & 0x1F;
    pkt
}

/// `[0xD3, 0x02, stage_0based, 0x02, (dpi/100)_lo, (dpi/100)_hi]`
pub fn build_set_dpi_packet(stage_0based: u8, dpi: u16) -> Option<[u8; HYPERX_PACKET_LEN]> {
    if !(DPI_MIN..=DPI_MAX).contains(&dpi) || !dpi.is_multiple_of(DPI_STEP) {
        return None;
    }
    let enc = (dpi / DPI_STEP).to_le_bytes();
    let mut pkt = [0u8; HYPERX_PACKET_LEN];
    pkt[0] = CMD_SET_DPI;
    pkt[1] = 0x02;
    pkt[2] = stage_0based & 0x1F;
    pkt[3] = 0x02;
    pkt[4] = enc[0];
    pkt[5] = enc[1];
    Some(pkt)
}

/// `[0xD3, 0x05, 0x00, 0x01, mm, mm]`
pub fn build_set_lod_packet(lod: LiftOffDistance) -> [u8; HYPERX_PACKET_LEN] {
    let mm = encode_lod(lod);
    let mut pkt = [0u8; HYPERX_PACKET_LEN];
    pkt[0] = CMD_SET_DPI;
    pkt[1] = 0x05;
    pkt[3] = 0x01;
    pkt[4] = mm;
    pkt[5] = mm;
    pkt
}

/// `[0xDE, 0xFF]` saves live settings to onboard memory.
pub fn build_save_packet() -> [u8; HYPERX_PACKET_LEN] {
    let mut pkt = [0u8; HYPERX_PACKET_LEN];
    pkt[0] = CMD_SAVE;
    pkt[1] = 0xFF;
    pkt
}

/// Hardware driver for HyperX Pulsefire Haste wired and wireless mice.
pub struct HyperxDevice {
    hid: HidDevice,
    spec: HyperxProductSpec,
    display_name: String,
    vid_pid: String,
    max_dpi: u16,
}

impl HyperxDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.display_name.clone(),
            message: message.into(),
        }
    }

    fn flush_input(&self) {
        let mut scratch = [0u8; HYPERX_PACKET_LEN + 1];
        for _ in 0..8 {
            match self.hid.read_timeout(&mut scratch, 0) {
                Ok(n) if n > 0 => continue,
                _ => break,
            }
        }
    }

    fn write_packet(&self, pkt: &[u8; HYPERX_PACKET_LEN]) -> Result<()> {
        let mut out = [0u8; HYPERX_PACKET_LEN + 1];
        out[0] = HYPERX_REPORT_ID;
        out[1..].copy_from_slice(pkt);
        self.hid
            .write(&out)
            .map_err(|e| self.dev_err(format!("HyperX HID write 0x{:02x} failed: {e}", pkt[0])))?;
        Ok(())
    }

    fn query(&self, cmd: u8) -> Result<[u8; HYPERX_PACKET_LEN]> {
        self.flush_input();
        self.write_packet(&build_query_packet(cmd))?;

        let deadline = Instant::now() + READ_TIMEOUT;
        let mut buf = [0u8; HYPERX_PACKET_LEN + 1];
        while Instant::now() < deadline {
            let remaining_ms = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .clamp(1, 150) as i32;
            match self.hid.read_timeout(&mut buf, remaining_ms) {
                Ok(0) => continue,
                Ok(n) => {
                    let slice = if n == HYPERX_PACKET_LEN + 1 && buf[0] == HYPERX_REPORT_ID {
                        &buf[1..=HYPERX_PACKET_LEN]
                    } else if n >= HYPERX_PACKET_LEN {
                        &buf[..HYPERX_PACKET_LEN]
                    } else {
                        continue;
                    };
                    if slice[0] == cmd {
                        let mut rsp = [0u8; HYPERX_PACKET_LEN];
                        rsp.copy_from_slice(slice);
                        return Ok(rsp);
                    }
                }
                Err(e) => {
                    return Err(self.dev_err(format!("HyperX HID read 0x{cmd:02x} failed: {e}")));
                }
            }
        }

        Err(self.dev_err(format!(
            "Timed out waiting for HyperX reply 0x{cmd:02x} (move mouse to wake)"
        )))
    }
}

impl MouseDevice for HyperxDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: format!("hyperx_{:04x}_{:04x}", self.spec.vid, self.spec.pid),
            name: self.display_name.clone(),
            vendor: "HyperX".to_string(),
            vid_pid: Some(self.vid_pid.clone()),
            connection: if self.spec.wireless {
                "Wireless 2.4 GHz Dongle (NGenuity2)".to_string()
            } else {
                "Wired USB (NGenuity2)".to_string()
            },
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            dpi_min: DPI_MIN,
            dpi_max: self.max_dpi,
            dpi_step: DPI_STEP,
            supports_xy_dpi: false,
            // Pulsefire Haste has no GET_POLLING_RATE opcode; omit polling writes to preserve
            // Polled's mandatory read-back verification invariant.
            polling_rates_hz: Vec::new(),
            lod_options: LOD_OPTIONS.to_vec(),
            supports_debounce: false,
            debounce_min_ms: 0,
            debounce_max_ms: 0,
            supports_motion_sync: false,
            supports_angle_snapping: false,
            supports_ripple_control: false,
            supports_sleep_timeout: false,
            sleep_timeout_min_secs: 0,
            sleep_timeout_max_secs: 0,
            dpi_stage_count: STAGE_COUNT as u8,
            supports_dpi_lock: true,
            supports_glass_mode: false,
            // Direct RAM mode: writes apply live without touching flash unless 0xDE 0xFF is sent.
            supports_volatile_writes: true,
            flash_write_warning: None,
        }
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        let rsp = self.query(CMD_GET_DPI)?;
        let state = decode_dpi_state(&rsp)
            .ok_or_else(|| self.dev_err("HyperX returned an invalid GET_DPI (0x53) response"))?;
        self.max_dpi = state.max_dpi;

        let active_idx = usize::from(state.active_stage);
        let active_dpi = state.stages[active_idx];
        let enabled_stages: Vec<u16> = (0..STAGE_COUNT)
            .filter(|&i| (state.enabled_mask & (1 << i)) != 0 || i == active_idx)
            .map(|i| state.stages[i])
            .collect();
        let all_locked = enabled_stages.len() > 1
            && active_dpi >= DPI_MIN
            && enabled_stages.iter().all(|&d| d == active_dpi);

        Ok(MouseSettings {
            dpi: (active_dpi >= DPI_MIN).then_some(active_dpi),
            dpi_y: None,
            polling_rate_hz: None,
            lod: state.lod,
            debounce_ms: None,
            motion_sync: None,
            angle_snapping: None,
            ripple_control: None,
            sleep_timeout_secs: None,
            dpi_stage: Some(state.active_stage + 1),
            lock_dpi_stages: if all_locked { Some(true) } else { None },
            glass_mode: None,
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, mode: WriteMode) -> Result<()> {
        let state = decode_dpi_state(&self.query(CMD_GET_DPI)?)
            .ok_or_else(|| self.dev_err("HyperX returned an invalid GET_DPI (0x53) response"))?;

        let mut wrote_any = false;
        let target_stage = settings
            .dpi_stage
            .map(|s| s.saturating_sub(1).min((STAGE_COUNT - 1) as u8))
            .unwrap_or(state.active_stage);

        if settings.dpi_stage.is_some() && target_stage != state.active_stage {
            // Ensure the target stage bit is enabled before selecting it.
            let bit = 1u8 << target_stage;
            if (state.enabled_mask & bit) == 0 {
                self.write_packet(&build_enable_stages_packet(state.enabled_mask | bit))?;
                thread::sleep(WRITE_SETTLE_DELAY);
            }
            self.write_packet(&build_select_stage_packet(target_stage))?;
            thread::sleep(WRITE_SETTLE_DELAY);
            wrote_any = true;
        }

        if let Some(dpi) = settings.dpi {
            if settings.lock_dpi_stages == Some(true) {
                for stage in 0..STAGE_COUNT {
                    if (state.enabled_mask & (1 << stage)) != 0
                        || stage == usize::from(target_stage)
                    {
                        let pkt = build_set_dpi_packet(stage as u8, dpi).ok_or_else(|| {
                            PolledError::UnsupportedSetting {
                                device: self.display_name.clone(),
                                detail: format!("DPI {dpi} unsupported on HyperX Pulsefire Haste"),
                            }
                        })?;
                        self.write_packet(&pkt)?;
                        thread::sleep(WRITE_SETTLE_DELAY);
                        wrote_any = true;
                    }
                }
            } else {
                let pkt = build_set_dpi_packet(target_stage, dpi).ok_or_else(|| {
                    PolledError::UnsupportedSetting {
                        device: self.display_name.clone(),
                        detail: format!("DPI {dpi} unsupported on HyperX Pulsefire Haste"),
                    }
                })?;
                self.write_packet(&pkt)?;
                thread::sleep(WRITE_SETTLE_DELAY);
                wrote_any = true;
            }
        } else if settings.lock_dpi_stages == Some(true) {
            let active_dpi = state.stages[usize::from(target_stage)];
            if (DPI_MIN..=self.max_dpi).contains(&active_dpi) && active_dpi.is_multiple_of(DPI_STEP)
            {
                for stage in 0..STAGE_COUNT {
                    if (state.enabled_mask & (1 << stage)) != 0
                        && stage != usize::from(target_stage)
                    {
                        if let Some(pkt) = build_set_dpi_packet(stage as u8, active_dpi) {
                            self.write_packet(&pkt)?;
                            thread::sleep(WRITE_SETTLE_DELAY);
                            wrote_any = true;
                        }
                    }
                }
            }
        }

        if let Some(lod) = settings.lod {
            self.write_packet(&build_set_lod_packet(lod))?;
            thread::sleep(WRITE_SETTLE_DELAY);
            wrote_any = true;
        }

        if wrote_any && mode == WriteMode::OnboardFlash {
            self.write_packet(&build_save_packet())?;
            thread::sleep(WRITE_SETTLE_DELAY);
        }

        Ok(())
    }

    fn read_battery(&mut self) -> Option<BatteryStatus> {
        if !self.spec.wireless {
            return None;
        }
        let rsp = self.query(CMD_GET_BATTERY).ok()?;
        decode_battery(&rsp)
    }
}

/// Discovery driver for HyperX Pulsefire Haste mice (Kingston & HP VIDs).
pub struct HyperxProtocolDriver;

impl DeviceDriver for HyperxProtocolDriver {
    fn driver_name(&self) -> &'static str {
        "hyperx-pulsefire-haste"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();
        let mut seen: HashSet<(u16, u16)> = HashSet::new();

        for dev in api.device_list() {
            let vid = dev.vendor_id();
            let pid = dev.product_id();
            let Some(spec) = lookup_hyperx_product(vid, pid) else {
                continue;
            };
            if dev.usage_page() != HYPERX_USAGE_PAGE || dev.usage() != HYPERX_USAGE {
                continue;
            }
            if seen.contains(&(vid, pid)) {
                continue;
            }
            let Ok(hid) = api.open_path(dev.path()) else {
                continue;
            };

            let fallback_name = dev
                .product_string()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| spec.model.to_string());

            let mut device = HyperxDevice {
                hid,
                spec,
                display_name: fallback_name,
                vid_pid: format!("{vid:04x}:{pid:04x}"),
                max_dpi: DPI_MAX,
            };

            // Read-only hardware info refinement if the mouse is awake.
            if let Ok(hw_rsp) = device.query(CMD_GET_HARDWARE_INFO) {
                if let Some(hw) = decode_hardware_info(&hw_rsp) {
                    if !hw.product.is_empty() {
                        device.display_name = hw.product;
                    }
                }
            }

            seen.insert((vid, pid));
            found.push(Box::new(device));
        }

        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_catalog_matches_kingston_and_hp_pids() {
        assert!(lookup_hyperx_product(0x0951, 0x1727).is_some_and(|p| !p.wireless));
        assert!(lookup_hyperx_product(0x03F0, 0x0F8F).is_some_and(|p| !p.wireless));
        assert!(lookup_hyperx_product(0x03F0, 0x048E).is_some_and(|p| !p.wireless));
        assert!(lookup_hyperx_product(0x03F0, 0x028E).is_some_and(|p| p.wireless));
        assert!(lookup_hyperx_product(0x03F0, 0x9999).is_none());
    }

    #[test]
    fn hardware_info_bcd_and_battery_decode() {
        let mut hw = [0u8; HYPERX_PACKET_LEN];
        hw[0] = CMD_GET_HARDWARE_INFO;
        hw[4..6].copy_from_slice(&0x028Eu16.to_le_bytes());
        hw[6..8].copy_from_slice(&0x03F0u16.to_le_bytes());
        // 4.1.0.9 -> [0x09, 0x00, 0x01, 0x04]
        hw[8] = 0x09;
        hw[9] = 0x00;
        hw[10] = 0x01;
        hw[11] = 0x04;
        let name = b"HyperX Pulsefire Haste Wireless";
        hw[20..20 + name.len()].copy_from_slice(name);

        let info = decode_hardware_info(&hw).expect("valid hardware info");
        assert_eq!(info.vid, 0x03F0);
        assert_eq!(info.pid, 0x028E);
        assert_eq!(info.firmware, "4.1.0.9");
        assert_eq!(info.product, "HyperX Pulsefire Haste Wireless");

        let mut batt = [0u8; HYPERX_PACKET_LEN];
        batt[0] = CMD_GET_BATTERY;
        batt[4] = 64;
        batt[5] = 0x00;
        assert_eq!(
            decode_battery(&batt),
            Some(BatteryStatus {
                percent: 64,
                charging: false,
                voltage_mv: None,
            })
        );
        batt[4] = 100;
        batt[5] = 0x02;
        assert_eq!(
            decode_battery(&batt),
            Some(BatteryStatus {
                percent: 100,
                charging: true,
                voltage_mv: None,
            })
        );
        batt[4] = 0;
        assert_eq!(decode_battery(&batt), None);
    }

    #[test]
    fn dpi_lod_and_write_packet_codecs() {
        let mut rsp = [0u8; HYPERX_PACKET_LEN];
        rsp[0] = CMD_GET_DPI;
        rsp[4] = 2; // stage 2 active (0-based -> Stage #3)
        rsp[5] = 0x0F; // first 4 stages enabled
        rsp[8] = 0xA0; // 160 * 100 = 16000 max DPI
        let encoded_stages = [4u16, 8, 16, 32, 0];
        for (i, &val) in encoded_stages.iter().enumerate() {
            rsp[12 + i * 2..14 + i * 2].copy_from_slice(&val.to_le_bytes());
        }
        rsp[37] = 2; // 2.0 mm LOD

        let st = decode_dpi_state(&rsp).expect("valid DPI state");
        assert_eq!(st.active_stage, 2);
        assert_eq!(st.enabled_mask, 0x0F);
        assert_eq!(st.stages, [400, 800, 1600, 3200, 0]);
        assert_eq!(st.lod, Some(LiftOffDistance::Mm20));
        assert_eq!(st.max_dpi, 16_000);

        let set_dpi = build_set_dpi_packet(2, 1600).expect("valid DPI packet");
        assert_eq!(&set_dpi[..6], &[0xD3, 0x02, 0x02, 0x02, 16, 0]);
        assert!(build_set_dpi_packet(2, 150).is_none());
        assert!(build_set_dpi_packet(2, 850).is_none());

        let sel = build_select_stage_packet(1);
        assert_eq!(&sel[..5], &[0xD3, 0x00, 0x00, 0x01, 0x01]);

        let lod = build_set_lod_packet(LiftOffDistance::Mm10);
        assert_eq!(&lod[..6], &[0xD3, 0x05, 0x00, 0x01, 1, 1]);

        let save = build_save_packet();
        assert_eq!(&save[..2], &[0xDE, 0xFF]);
    }
}
