// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Logitech HID++ 2.0 keyboard backend (`0x046D`, `0xFF00:0x01/0x02` & `0xFF43`).
//!
//! Protocol codec implemented from `OpenMouse-Project/keyboard-protocol` (`src/logitech/`,
//! AGPL-3.0-or-later) and Logitech's public HID++ 2.0 specification:
//! - Direct USB / Bluetooth keyboards (PRO X TKL RAPID, PRO X 60, G915 X, G715, G515 RAPID TKL, etc.)
//! - LIGHTSPEED / Bolt / Unifying receivers (probed strictly for paired cataloged keyboard model IDs
//!   so Logitech mice on the same receiver or another receiver are NEVER claimed!).
//! - Supports firmware, unified battery (`0x1004`), extended polling rate (`0x8061`), brightness
//!   (`0x8040`), and `0x1B08` analog switches (global Rapid Trigger + Key Priority / Snap Tap).

use std::collections::HashSet;
use std::ffi::CString;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo as HidDeviceInfo, HidApi, HidDevice};

use crate::devices::keyboards::{
    device_dedup_key, kb_err, kb_unsupported, kb_verify_err, KeyboardBrand, KeyboardCapabilities,
    KeyboardDevice, KeyboardStatus,
};
use crate::devices::{BatteryStatus, SupportTier};
use crate::error::Result;

pub const LOGITECH_VID: u16 = 0x046D;
const HIDPP_USAGE_PAGE: u16 = 0xFF00;
const HIDPP_BT_USAGE_PAGE: u16 = 0xFF43;

const REPORT_ID_SHORT: u8 = 0x10;
const REPORT_ID_LONG: u8 = 0x11;
const SW_ID: u8 = 0x05;
const DIRECT_DEV_IDX: u8 = 0xFF;

const FEAT_DEVICE_INFO: u16 = 0x0003;
const FEAT_UNIFIED_BATTERY: u16 = 0x1004;
const FEAT_ANALOG_KEYS: u16 = 0x1B08;
const FEAT_BRIGHTNESS: u16 = 0x8040;
const FEAT_EXT_REPORT_RATE: u16 = 0x8061;

#[derive(Debug, Clone, Copy)]
pub struct LogitechKeyboardSpec {
    pub pid: u16,
    pub name: &'static str,
    pub wireless: bool,
}

const fn lg(pid: u16, name: &'static str, wireless: bool) -> LogitechKeyboardSpec {
    LogitechKeyboardSpec {
        pid,
        name,
        wireless,
    }
}

pub const LOGITECH_KEYBOARD_PRODUCTS: &[LogitechKeyboardSpec] = &[
    lg(0xC351, "Logitech PRO X 60", true),
    lg(0xB37B, "Logitech PRO X 60", true),
    lg(0x40AF, "Logitech PRO X 60", true),
    lg(0xC352, "Logitech PRO X TKL", true),
    lg(0xB382, "Logitech PRO X TKL", true),
    lg(0x40B0, "Logitech PRO X TKL", true),
    lg(0xC35B, "Logitech PRO X TKL RAPID", false),
    lg(0xC366, "Logitech G316", false),
    lg(0xC361, "Logitech G316 X", false),
    lg(0xC355, "Logitech G515 LIGHTSPEED TKL", true),
    lg(0xB389, "Logitech G515 LIGHTSPEED TKL", true),
    lg(0x40B4, "Logitech G515 LIGHTSPEED TKL", true),
    lg(0xC358, "Logitech G515 TKL", false),
    lg(0xC363, "Logitech G515 RAPID TKL", false),
    lg(0xC35A, "Logitech G517 LIGHTSPEED TKL", true),
    lg(0xB390, "Logitech G517 LIGHTSPEED TKL", true),
    lg(0x40B9, "Logitech G517 LIGHTSPEED TKL", true),
    lg(0xC348, "Logitech G713", false),
    lg(0xC347, "Logitech G715", true),
    lg(0xB368, "Logitech G715", true),
    lg(0x40A2, "Logitech G715", true),
    lg(0xC359, "Logitech G915 X", true),
    lg(0xC356, "Logitech G915 X LIGHTSPEED", true),
    lg(0xB38A, "Logitech G915 X LIGHTSPEED", true),
    lg(0x40B5, "Logitech G915 X LIGHTSPEED", true),
    lg(0xC357, "Logitech G915 X LIGHTSPEED TKL", true),
    lg(0xB38B, "Logitech G915 X LIGHTSPEED TKL", true),
    lg(0x40B6, "Logitech G915 X LIGHTSPEED TKL", true),
];

const LOGITECH_RECEIVER_PIDS: &[u16] = &[
    0xC52B, 0xC532, 0xC539, 0xC53A, 0xC53D, 0xC53F, 0xC541, 0xC543, 0xC545, 0xC547, 0xC548, 0xC54D,
];

pub fn lookup_logitech_keyboard(pid: u16) -> Option<LogitechKeyboardSpec> {
    LOGITECH_KEYBOARD_PRODUCTS
        .iter()
        .copied()
        .find(|p| p.pid == pid)
}

pub fn decode_hidpp_report_rates_mask(mask: u8) -> Vec<u16> {
    const RATES: [u16; 7] = [125, 250, 500, 1000, 2000, 4000, 8000];
    RATES
        .iter()
        .enumerate()
        .filter_map(|(i, &hz)| ((mask & (1 << i)) != 0).then_some(hz))
        .collect()
}

pub fn decode_hidpp_report_rate_code(code: u8) -> Option<u16> {
    if code < 7 {
        Some(125u16 << code)
    } else {
        None
    }
}

pub fn encode_hidpp_report_rate_code(hz: u16) -> Option<u8> {
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

fn hidpp_exchange(
    dev: &HidDevice,
    device_idx: u8,
    feature_idx: u8,
    fn_id: u8,
    params: &[u8],
    long: bool,
    timeout_ms: u64,
) -> Option<Vec<u8>> {
    let mut drain = [0u8; 32];
    for _ in 0..8 {
        match dev.read_timeout(&mut drain, 0) {
            Ok(n) if n > 0 => continue,
            _ => break,
        }
    }

    let fn_sw = ((fn_id & 0x0F) << 4) | SW_ID;
    let mut tx = if long {
        let mut p = vec![0u8; 20];
        p[0] = REPORT_ID_LONG;
        p
    } else {
        let mut p = vec![0u8; 7];
        p[0] = REPORT_ID_SHORT;
        p
    };
    tx[1] = device_idx;
    tx[2] = feature_idx;
    tx[3] = fn_sw;
    let max_p = tx.len() - 4;
    let n = params.len().min(max_p);
    tx[4..4 + n].copy_from_slice(&params[..n]);
    dev.write(&tx).ok()?;

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut rx = [0u8; 32];
    while Instant::now() < deadline {
        let rem = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(10, 180) as i32;
        match dev.read_timeout(&mut rx, rem) {
            Ok(n) if n >= 7 => {
                if rx[0] != REPORT_ID_SHORT && rx[0] != REPORT_ID_LONG {
                    continue;
                }
                if rx[1] != device_idx {
                    continue;
                }
                if rx[2] == 0xFF && rx[3] == feature_idx && rx[4] == fn_sw {
                    return None;
                }
                if rx[2] == feature_idx && rx[3] == fn_sw {
                    return Some(rx[4..n].to_vec());
                }
            }
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
    None
}

fn hidpp_get_feature(dev: &HidDevice, dev_idx: u8, feature_id: u16, long: bool) -> Option<u8> {
    let hi = (feature_id >> 8) as u8;
    let lo = (feature_id & 0xFF) as u8;
    let p = hidpp_exchange(dev, dev_idx, 0, 0, &[hi, lo, 0], long, 400)?;
    let idx = *p.first()?;
    (idx != 0).then_some(idx)
}

fn decode_bcd_u16(val: u16) -> u16 {
    let d3 = (val >> 12) & 0xF;
    let d2 = (val >> 8) & 0xF;
    let d1 = (val >> 4) & 0xF;
    let d0 = val & 0xF;
    if d3 < 10 && d2 < 10 && d1 < 10 && d0 < 10 {
        d3 * 1000 + d2 * 100 + d1 * 10 + d0
    } else {
        val.min(9999)
    }
}

pub struct LogitechKeyboardDevice {
    id: String,
    spec: LogitechKeyboardSpec,
    device_idx: u8,
    long_reports: bool,
    via_receiver: bool,
    path: CString,
}

impl LogitechKeyboardDevice {
    fn open(&self) -> Result<HidDevice> {
        let api = HidApi::new().map_err(|e| kb_err(self.spec.name, e.to_string()))?;
        api.open_path(self.path.as_c_str())
            .map_err(|e| kb_err(self.spec.name, format!("open failed: {e}")))
    }
}

impl KeyboardDevice for LogitechKeyboardDevice {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        self.spec.name
    }

    fn brand(&self) -> KeyboardBrand {
        KeyboardBrand::Logitech
    }

    fn support_tier(&self) -> SupportTier {
        SupportTier::ImplementedFromDocs
    }

    fn read_status(&self) -> KeyboardStatus {
        let mut firmware_version = None;
        let mut battery = None;
        let mut polling_rate_hz = None;
        let mut supported_rates = Vec::new();
        let mut rapid_trigger_enabled = None;
        let mut socd_enabled = None;
        let mut has_analog = false;
        let mut extra = Vec::new();

        if let Ok(dev) = self.open() {
            let idx = self.device_idx;
            let long = self.long_reports;

            if let Some(f_info) = hidpp_get_feature(&dev, idx, FEAT_DEVICE_INFO, long) {
                if let Some(fw) = hidpp_exchange(&dev, idx, f_info, 1, &[0, 0, 0], long, 450) {
                    if fw.len() >= 8 {
                        let ver = decode_bcd_u16(u16::from_be_bytes([fw[4], fw[5]]));
                        firmware_version = Some(format!("{}.{:02}", ver / 100, ver % 100));
                    }
                }
            }

            if let Some(f_bat) = hidpp_get_feature(&dev, idx, FEAT_UNIFIED_BATTERY, long) {
                if let Some(caps) = hidpp_exchange(&dev, idx, f_bat, 0, &[0, 0, 0], long, 450) {
                    let has_gauge = (caps.get(1).copied().unwrap_or(0) & 0x02) != 0;
                    if let Some(st) = hidpp_exchange(&dev, idx, f_bat, 1, &[0, 0, 0], long, 450) {
                        if has_gauge && st.len() >= 3 {
                            let pct = st[0].min(100);
                            let charging = matches!(st[2], 1 | 2 | 4);
                            battery = Some(BatteryStatus {
                                percent: pct,
                                charging,
                                voltage_mv: None,
                            });
                        }
                    }
                }
            }

            if let Some(f_rate) = hidpp_get_feature(&dev, idx, FEAT_EXT_REPORT_RATE, long) {
                if let Some(list_p) = hidpp_exchange(&dev, idx, f_rate, 1, &[0, 0, 0], long, 450) {
                    if list_p.len() >= 2 {
                        supported_rates = decode_hidpp_report_rates_mask(list_p[1]);
                    }
                }
                if let Some(cur_p) = hidpp_exchange(&dev, idx, f_rate, 2, &[0, 0, 0], long, 450) {
                    if let Some(&code) = cur_p.first() {
                        polling_rate_hz = decode_hidpp_report_rate_code(code);
                    }
                }
            }

            if let Some(f_br) = hidpp_get_feature(&dev, idx, FEAT_BRIGHTNESS, long) {
                if let Some(info_p) = hidpp_exchange(&dev, idx, f_br, 0, &[0, 0, 0], long, 450) {
                    if let Some(val_p) = hidpp_exchange(&dev, idx, f_br, 1, &[0, 0, 0], long, 450) {
                        if info_p.len() >= 2 && val_p.len() >= 2 {
                            let max_v = u16::from_be_bytes([info_p[0], info_p[1]]);
                            let cur_v = u16::from_be_bytes([val_p[0], val_p[1]]);
                            if max_v > 0 {
                                extra.push((
                                    "Backlight".to_string(),
                                    format!("{}%", (cur_v as u32 * 100) / max_v as u32),
                                ));
                            }
                        }
                    }
                }
            }

            if let Some(f_analog) = hidpp_get_feature(&dev, idx, FEAT_ANALOG_KEYS, long) {
                has_analog = true;
                if let Some(rt_p) = hidpp_exchange(&dev, idx, f_analog, 1, &[0, 0, 0], false, 450) {
                    rapid_trigger_enabled = rt_p.first().map(|&b| b != 0);
                }
                if let Some(kp_p) = hidpp_exchange(&dev, idx, f_analog, 4, &[0, 0, 0], false, 450) {
                    socd_enabled = kp_p.first().map(|&b| b != 0);
                }
            }
        }

        if self.via_receiver {
            extra.push(("Transport".to_string(), "LIGHTSPEED / Receiver".to_string()));
        }

        let capabilities = KeyboardCapabilities {
            supports_polling_rate_write: !supported_rates.is_empty(),
            supported_polling_rates_hz: supported_rates,
            supports_rapid_trigger_toggle: has_analog,
            supports_socd_toggle: has_analog,
            socd_label: has_analog.then(|| "Key Priority (SOCD)".to_string()),
            ..Default::default()
        };

        KeyboardStatus {
            id: self.id.clone(),
            brand: KeyboardBrand::Logitech,
            name: self.spec.name.to_string(),
            vid_pid: format!("{:04x}:{:04x}", LOGITECH_VID, self.spec.pid),
            support_tier: SupportTier::ImplementedFromDocs,
            protocol_label: "Logitech HID++ 2.0".to_string(),
            chip_type: if self.spec.wireless || self.via_receiver {
                "LIGHTSPEED / HID++ 2.0"
            } else {
                "Logitech G USB"
            }
            .to_string(),
            switch_tech: if has_analog {
                "Magnetic Analog (0x1B08)"
            } else {
                "Mechanical / Optical"
            }
            .to_string(),
            firmware_version,
            layout: None,
            active_profile: None,
            profiles: Vec::new(),
            actuation_mm: None,
            actuation_range_mm: has_analog.then_some((0.1, 4.0)),
            rapid_trigger_enabled,
            rapid_trigger_mm: None,
            polling_rate_hz,
            battery,
            system_mode: None,
            socd_enabled,
            tachyon_enabled: None,
            debounce_mode: None,
            debounce_ms: None,
            extra_telemetry: extra,
            status_note: Some(
                "Implemented from OpenMouse / Logitech HID++ 2.0 docs; writes are confirm-gated and verified by read-back."
                    .to_string(),
            ),
            capabilities,
            wooting: None,
        }
    }

    fn set_polling_rate(&self, hz: u16) -> Result<u16> {
        let code = encode_hidpp_report_rate_code(hz).ok_or_else(|| {
            kb_err(
                self.spec.name,
                format!("Unsupported HID++ polling rate {hz} Hz"),
            )
        })?;
        let dev = self.open()?;
        let f_rate = hidpp_get_feature(
            &dev,
            self.device_idx,
            FEAT_EXT_REPORT_RATE,
            self.long_reports,
        )
        .ok_or_else(|| kb_unsupported(self.spec.name, "extended report rate (0x8061)"))?;
        hidpp_exchange(
            &dev,
            self.device_idx,
            f_rate,
            3,
            &[code, 0, 0],
            self.long_reports,
            500,
        )
        .ok_or_else(|| kb_err(self.spec.name, "Keyboard rejected report-rate write"))?;
        let confirmed = hidpp_exchange(
            &dev,
            self.device_idx,
            f_rate,
            2,
            &[0, 0, 0],
            self.long_reports,
            500,
        )
        .and_then(|p| p.first().copied())
        .and_then(decode_hidpp_report_rate_code);
        if confirmed != Some(hz) {
            return Err(kb_verify_err(
                self.spec.name,
                format!("{hz} Hz"),
                format!("{confirmed:?}"),
            ));
        }
        Ok(hz)
    }

    fn set_rapid_trigger_enabled(&self, enabled: bool) -> Result<bool> {
        let dev = self.open()?;
        let f_analog =
            hidpp_get_feature(&dev, self.device_idx, FEAT_ANALOG_KEYS, self.long_reports)
                .ok_or_else(|| kb_unsupported(self.spec.name, "0x1B08 analog switches"))?;
        hidpp_exchange(
            &dev,
            self.device_idx,
            f_analog,
            2,
            &[u8::from(enabled), 0, 0],
            false,
            500,
        )
        .ok_or_else(|| kb_err(self.spec.name, "Keyboard rejected Rapid Trigger toggle"))?;
        let confirmed = hidpp_exchange(&dev, self.device_idx, f_analog, 1, &[0, 0, 0], false, 500)
            .and_then(|p| p.first().map(|&b| b != 0));
        if confirmed != Some(enabled) {
            return Err(kb_verify_err(
                self.spec.name,
                if enabled { "ON" } else { "OFF" },
                format!("{confirmed:?}"),
            ));
        }
        Ok(enabled)
    }

    fn set_socd(&self, enabled: bool) -> Result<bool> {
        let dev = self.open()?;
        let f_analog =
            hidpp_get_feature(&dev, self.device_idx, FEAT_ANALOG_KEYS, self.long_reports)
                .ok_or_else(|| kb_unsupported(self.spec.name, "0x1B08 Key Priority"))?;
        hidpp_exchange(
            &dev,
            self.device_idx,
            f_analog,
            5,
            &[u8::from(enabled), 0, 0],
            false,
            500,
        )
        .ok_or_else(|| kb_err(self.spec.name, "Keyboard rejected Key Priority toggle"))?;
        let confirmed = hidpp_exchange(&dev, self.device_idx, f_analog, 4, &[0, 0, 0], false, 500)
            .and_then(|p| p.first().map(|&b| b != 0));
        if confirmed != Some(enabled) {
            return Err(kb_verify_err(
                self.spec.name,
                if enabled { "ON" } else { "OFF" },
                format!("{confirmed:?}"),
            ));
        }
        Ok(enabled)
    }
}

fn probe_receiver_for_keyboard(dev: &HidDevice, long: bool) -> Option<(u8, LogitechKeyboardSpec)> {
    for &slot in &[0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, DIRECT_DEV_IDX] {
        let Some(f_info) = hidpp_get_feature(dev, slot, FEAT_DEVICE_INFO, long) else {
            continue;
        };
        let Some(p) = hidpp_exchange(dev, slot, f_info, 0, &[0, 0, 0], long, 250) else {
            continue;
        };
        if p.len() < 13 {
            continue;
        }
        for off in [7usize, 9, 11] {
            let model_id = u16::from_be_bytes([p[off], p[off + 1]]);
            if let Some(spec) = lookup_logitech_keyboard(model_id) {
                return Some((slot, spec));
            }
        }
    }
    None
}

pub(crate) fn detect_logitech_keyboards(
    api: &HidApi,
    infos: &[&HidDeviceInfo],
    claimed: &mut HashSet<(u16, u16, String)>,
) -> Vec<LogitechKeyboardDevice> {
    let mut found = Vec::new();
    let mut seen_kb_pids: HashSet<u16> = HashSet::new();

    for info in infos {
        if info.vendor_id() != LOGITECH_VID {
            continue;
        }
        let up = info.usage_page();
        let usage = info.usage();
        let is_long_hidpp = (up == HIDPP_USAGE_PAGE && usage == 2) || up == HIDPP_BT_USAGE_PAGE;
        let is_short_hidpp = up == HIDPP_USAGE_PAGE && usage == 1;
        if !is_long_hidpp && !is_short_hidpp {
            continue;
        }

        let pid = info.product_id();
        if let Some(spec) = lookup_logitech_keyboard(pid) {
            // Prefer long report collection when available.
            if !is_long_hidpp
                && infos.iter().any(|other| {
                    other.vendor_id() == LOGITECH_VID
                        && other.product_id() == pid
                        && ((other.usage_page() == HIDPP_USAGE_PAGE && other.usage() == 2)
                            || other.usage_page() == HIDPP_BT_USAGE_PAGE)
                })
            {
                continue;
            }
            let key = device_dedup_key(info);
            if claimed.contains(&key) || !seen_kb_pids.insert(spec.pid) {
                continue;
            }
            if api.open_path(info.path()).is_err() {
                continue;
            }
            claimed.insert(key);
            found.push(LogitechKeyboardDevice {
                id: format!("logitech-kb:046d:{:04x}", spec.pid),
                spec,
                device_idx: DIRECT_DEV_IDX,
                long_reports: is_long_hidpp,
                via_receiver: false,
                path: info.path().to_owned(),
            });
        } else if is_long_hidpp && LOGITECH_RECEIVER_PIDS.contains(&pid) {
            let Ok(dev) = api.open_path(info.path()) else {
                continue;
            };
            if let Some((slot, spec)) = probe_receiver_for_keyboard(&dev, true) {
                if !seen_kb_pids.insert(spec.pid) {
                    continue;
                }
                found.push(LogitechKeyboardDevice {
                    id: format!("logitech-kb:046d:{:04x}:rx{slot}", spec.pid),
                    spec,
                    device_idx: slot,
                    long_reports: true,
                    via_receiver: true,
                    path: info.path().to_owned(),
                });
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logitech_kb_catalog_excludes_mice_and_decodes_rates() {
        // Superlight 2 (0xC09B) & G Pro X Superlight (0xC094) MUST NOT be matched as keyboards.
        assert!(lookup_logitech_keyboard(0xC09B).is_none());
        assert!(lookup_logitech_keyboard(0xC094).is_none());
        // PRO X TKL RAPID IS present.
        assert_eq!(
            lookup_logitech_keyboard(0xC35B).map(|s| s.name),
            Some("Logitech PRO X TKL RAPID")
        );
        assert_eq!(
            decode_hidpp_report_rates_mask(0b0001_1111),
            vec![125, 250, 500, 1000, 2000]
        );
        assert_eq!(decode_hidpp_report_rate_code(6), Some(8000));
        assert_eq!(encode_hidpp_report_rate_code(4000), Some(5));
    }
}
