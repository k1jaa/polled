// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Razer keyboard backend (`0x1532`, 90-byte feature report control protocol on `0x0001:0x0002`).
//!
//! Protocol codec implemented from `OpenMouse-Project/keyboard-protocol` (`src/razer/`,
//! AGPL-3.0-or-later) / OpenRazer.
//!
//! Safety: Strictly matches ONLY cataloged standalone Razer keyboard PIDs so Razer mice sharing
//! `VID 0x1532` and `0x0001:0x0002` are NEVER claimed by this backend.

use std::collections::HashSet;
use std::ffi::CString;
use std::thread;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo as HidDeviceInfo, HidApi, HidDevice};

use crate::devices::keyboards::{
    device_dedup_key, kb_err, kb_unsupported, kb_verify_err, KeyboardBrand, KeyboardCapabilities,
    KeyboardDevice, KeyboardStatus,
};
use crate::devices::SupportTier;
use crate::error::Result;

pub const RAZER_VID: u16 = 0x1532;
const RAZER_REPORT_LEN: usize = 90;
const TX_IDS: [u8; 4] = [0x1F, 0x3F, 0x9F, 0xFF];

const STATUS_BUSY: u8 = 0x01;
const STATUS_OK: u8 = 0x02;

#[derive(Debug, Clone, Copy)]
pub struct RazerKeyboardSpec {
    pub pid: u16,
    pub name: &'static str,
    pub wireless: bool,
    pub v3_rapid_trigger: bool,
}

const fn rz(pid: u16, name: &'static str, wireless: bool, v3_rt: bool) -> RazerKeyboardSpec {
    RazerKeyboardSpec {
        pid,
        name,
        wireless,
        v3_rapid_trigger: v3_rt,
    }
}

/// Standalone Razer keyboards speaking the 90-byte OpenRazer control report.
/// Mice, keypads, and laptops are strictly excluded.
pub const RAZER_KEYBOARD_PRODUCTS: &[RazerKeyboardSpec] = &[
    rz(0x0226, "Razer Huntsman Elite", false, false),
    rz(0x0227, "Razer Huntsman", false, false),
    rz(0x0228, "Razer BlackWidow Elite", false, false),
    rz(0x022A, "Razer Cynosa Chroma", false, false),
    rz(0x0235, "Razer BlackWidow Lite", false, false),
    rz(0x0237, "Razer BlackWidow Essential", false, false),
    rz(0x023F, "Razer Cynosa Lite", false, false),
    rz(0x0241, "Razer BlackWidow 2019", false, false),
    rz(0x0243, "Razer Huntsman Tournament Edition", false, false),
    rz(0x024E, "Razer BlackWidow V3", false, false),
    rz(0x0257, "Razer Huntsman Mini", false, false),
    rz(0x025A, "Razer BlackWidow V3 Pro (Wired)", false, false),
    rz(0x025C, "Razer BlackWidow V3 Pro (Wireless)", true, false),
    rz(0x025D, "Razer Ornata V2", false, false),
    rz(0x025E, "Razer Cynosa V2", false, false),
    rz(0x0266, "Razer Huntsman V2 Analog", false, false),
    rz(0x026B, "Razer Huntsman V2 TKL", false, false),
    rz(0x026C, "Razer Huntsman V2", false, false),
    rz(0x0282, "Razer Huntsman Mini Analog", false, false),
    rz(0x0287, "Razer BlackWidow V4", false, false),
    rz(0x028D, "Razer BlackWidow V4 Pro", false, false),
    rz(0x028F, "Razer Ornata V3", false, false),
    rz(0x0290, "Razer DeathStalker V2 Pro (Wireless)", true, false),
    rz(0x0292, "Razer DeathStalker V2 Pro (Wired)", false, false),
    rz(0x0293, "Razer BlackWidow V4 X", false, false),
    rz(0x0294, "Razer Ornata V3 X", false, false),
    rz(0x0295, "Razer DeathStalker V2", false, false),
    rz(
        0x0296,
        "Razer DeathStalker V2 Pro TKL (Wireless)",
        true,
        false,
    ),
    rz(
        0x0298,
        "Razer DeathStalker V2 Pro TKL (Wired)",
        false,
        false,
    ),
    rz(0x02A1, "Razer Ornata V3", false, false),
    rz(0x02A2, "Razer Ornata V3 X", false, false),
    rz(0x02A3, "Razer Ornata V3 TKL", false, false),
    rz(0x02A5, "Razer BlackWidow V4 75%", false, false),
    rz(0x02A6, "Razer Huntsman V3 Pro", false, true),
    rz(0x02A7, "Razer Huntsman V3 Pro TKL", false, true),
    rz(0x02B0, "Razer Huntsman V3 Pro Mini", false, true),
    rz(
        0x02B9,
        "Razer BlackWidow V4 Mini HyperSpeed (Wired)",
        false,
        false,
    ),
    rz(
        0x02BA,
        "Razer BlackWidow V4 Mini HyperSpeed (Wireless)",
        true,
        false,
    ),
    rz(0x02CF, "Razer Huntsman V3 Pro 8KHz", false, true),
    rz(
        0x02D5,
        "Razer BlackWidow V4 TKL HyperSpeed (Wireless)",
        true,
        false,
    ),
    rz(
        0x02D7,
        "Razer BlackWidow V4 TKL HyperSpeed (Wired)",
        false,
        false,
    ),
    rz(0x0A24, "Razer BlackWidow V3 TK", false, false),
];

pub fn lookup_razer_keyboard(pid: u16) -> Option<RazerKeyboardSpec> {
    RAZER_KEYBOARD_PRODUCTS
        .iter()
        .copied()
        .find(|p| p.pid == pid)
}

pub fn razer_kb_checksum(frame: &[u8; RAZER_REPORT_LEN]) -> u8 {
    frame[2..88].iter().fold(0u8, |acc, &b| acc ^ b)
}

pub fn encode_razer_kb_request(
    tx_id: u8,
    class: u8,
    cmd: u8,
    data_size: u8,
    args: &[u8],
) -> [u8; RAZER_REPORT_LEN] {
    let mut frame = [0u8; RAZER_REPORT_LEN];
    frame[1] = tx_id;
    frame[5] = data_size;
    frame[6] = class;
    frame[7] = cmd;
    let n = args.len().min(80);
    frame[8..8 + n].copy_from_slice(&args[..n]);
    frame[88] = razer_kb_checksum(&frame);
    frame
}

pub fn decode_razer_polling_code(code: u8, extended: bool) -> Option<u16> {
    if extended {
        match code {
            0x01 => Some(8000),
            0x02 => Some(4000),
            0x04 => Some(2000),
            0x08 => Some(1000),
            0x10 => Some(500),
            0x20 => Some(250),
            0x40 => Some(125),
            _ => None,
        }
    } else {
        match code {
            0x01 => Some(1000),
            0x02 => Some(500),
            0x08 => Some(125),
            _ => None,
        }
    }
}

pub fn encode_razer_polling_code(hz: u16, extended: bool) -> Option<u8> {
    if extended {
        match hz {
            8000 => Some(0x01),
            4000 => Some(0x02),
            2000 => Some(0x04),
            1000 => Some(0x08),
            500 => Some(0x10),
            250 => Some(0x20),
            125 => Some(0x40),
            _ => None,
        }
    } else {
        match hz {
            1000 => Some(0x01),
            500 => Some(0x02),
            125 => Some(0x08),
            _ => None,
        }
    }
}

pub struct RazerKeyboardDevice {
    id: String,
    spec: RazerKeyboardSpec,
    path: CString,
}

impl RazerKeyboardDevice {
    fn open(&self) -> Result<HidDevice> {
        let api = HidApi::new().map_err(|e| kb_err(self.spec.name, e.to_string()))?;
        api.open_path(self.path.as_c_str())
            .map_err(|e| kb_err(self.spec.name, format!("open failed: {e}")))
    }

    fn exchange(
        dev: &HidDevice,
        tx_id: u8,
        class: u8,
        cmd: u8,
        data_size: u8,
        args: &[u8],
    ) -> Option<Vec<u8>> {
        let frame = encode_razer_kb_request(tx_id, class, cmd, data_size, args);
        let mut tx = [0u8; RAZER_REPORT_LEN + 1];
        tx[0] = 0x00;
        tx[1..].copy_from_slice(&frame);
        dev.send_feature_report(&tx).ok()?;

        let deadline = Instant::now() + Duration::from_millis(500);
        let mut rx = [0u8; RAZER_REPORT_LEN + 1];
        while Instant::now() < deadline {
            rx.fill(0);
            if let Ok(n) = dev.get_feature_report(&mut rx) {
                if n > RAZER_REPORT_LEN {
                    let rep = &rx[1..1 + RAZER_REPORT_LEN];
                    let status = rep[0];
                    if status == STATUS_OK {
                        let dlen = (rep[5] as usize).min(80);
                        return Some(rep[8..8 + dlen].to_vec());
                    }
                    if status == STATUS_BUSY {
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    return None;
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        None
    }

    fn resolve_tx_id(dev: &HidDevice) -> Option<(u8, String)> {
        for tx_id in TX_IDS {
            if let Some(args) = Self::exchange(dev, tx_id, 0x00, 0x81, 0x02, &[]) {
                if args.len() >= 2 {
                    return Some((tx_id, format!("{}.{}", args[0], args[1])));
                }
            }
        }
        None
    }

    fn read_polling(dev: &HidDevice, tx_id: u8) -> Option<(u16, bool)> {
        if let Some(args) = Self::exchange(dev, tx_id, 0x00, 0xC0, 0x01, &[]) {
            let code = args.get(1).copied().or_else(|| args.first().copied())?;
            if let Some(hz) = decode_razer_polling_code(code, true) {
                return Some((hz, true));
            }
        }
        if let Some(args) = Self::exchange(dev, tx_id, 0x00, 0x85, 0x01, &[]) {
            let code = *args.first()?;
            if let Some(hz) = decode_razer_polling_code(code, false) {
                return Some((hz, false));
            }
        }
        None
    }
}

impl KeyboardDevice for RazerKeyboardDevice {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        self.spec.name
    }

    fn brand(&self) -> KeyboardBrand {
        KeyboardBrand::Razer
    }

    fn support_tier(&self) -> SupportTier {
        SupportTier::ImplementedFromDocs
    }

    fn read_status(&self) -> KeyboardStatus {
        let mut firmware_version = None;
        let mut polling_rate_hz = None;
        let mut supported_rates = Vec::new();
        let mut rapid_trigger_enabled = None;
        let mut extra = Vec::new();

        if let Ok(dev) = self.open() {
            if let Some((tx_id, fw)) = Self::resolve_tx_id(&dev) {
                firmware_version = Some(fw);
                if let Some((hz, extended)) = Self::read_polling(&dev, tx_id) {
                    polling_rate_hz = Some(hz);
                    supported_rates = if extended {
                        vec![125, 250, 500, 1000, 2000, 4000, 8000]
                    } else {
                        vec![125, 500, 1000]
                    };
                }
                if let Some(args) = Self::exchange(&dev, tx_id, 0x0F, 0x84, 0x03, &[0x01, 0x05])
                    .or_else(|| Self::exchange(&dev, tx_id, 0x03, 0x83, 0x03, &[0x01, 0x05]))
                {
                    if let Some(&b) = args.get(2) {
                        extra.push((
                            "Backlight Brightness".to_string(),
                            format!("{}%", (b as u32 * 100) / 255),
                        ));
                    }
                }
                if self.spec.v3_rapid_trigger {
                    if let Some(args) =
                        Self::exchange(&dev, tx_id, 0x02, 0x9A, 0x03, &[0x00, 0x00, 0xFF])
                    {
                        if args.len() >= 4 && args[2] == 0xFF {
                            rapid_trigger_enabled = Some(args[3] != 0);
                        }
                    }
                }
            }
        }

        let capabilities = KeyboardCapabilities {
            supports_polling_rate_write: !supported_rates.is_empty(),
            supported_polling_rates_hz: supported_rates,
            supports_rapid_trigger_toggle: self.spec.v3_rapid_trigger,
            ..Default::default()
        };

        KeyboardStatus {
            id: self.id.clone(),
            brand: KeyboardBrand::Razer,
            name: self.spec.name.to_string(),
            vid_pid: format!("{:04x}:{:04x}", RAZER_VID, self.spec.pid),
            support_tier: SupportTier::ImplementedFromDocs,
            protocol_label: "Razer 90B Control".to_string(),
            chip_type: if self.spec.wireless {
                "Razer HyperSpeed"
            } else {
                "Razer USB"
            }
            .to_string(),
            switch_tech: if self.spec.v3_rapid_trigger {
                "Analog Optical Gen-2"
            } else {
                "Optical / Mechanical"
            }
            .to_string(),
            firmware_version,
            layout: None,
            active_profile: None,
            profiles: Vec::new(),
            actuation_mm: None,
            actuation_range_mm: self.spec.v3_rapid_trigger.then_some((0.1, 4.0)),
            rapid_trigger_enabled,
            rapid_trigger_mm: None,
            polling_rate_hz,
            battery: None,
            system_mode: None,
            socd_enabled: None,
            tachyon_enabled: None,
            debounce_mode: None,
            debounce_ms: None,
            extra_telemetry: extra,
            status_note: Some(
                "Implemented from OpenRazer / OpenMouse 90-byte protocol docs; writes are confirm-gated and verified by read-back."
                    .to_string(),
            ),
            capabilities,
            wooting: None,
        }
    }

    fn set_polling_rate(&self, hz: u16) -> Result<u16> {
        let dev = self.open()?;
        let (tx_id, _) = Self::resolve_tx_id(&dev).ok_or_else(|| {
            kb_err(
                self.spec.name,
                "Keyboard did not answer Razer firmware probe",
            )
        })?;
        let extended = Self::read_polling(&dev, tx_id)
            .map(|(_, ext)| ext)
            .unwrap_or(true);
        let code = encode_razer_polling_code(hz, extended).ok_or_else(|| {
            kb_err(
                self.spec.name,
                format!("Unsupported Razer polling rate {hz} Hz"),
            )
        })?;
        if extended {
            Self::exchange(&dev, tx_id, 0x00, 0x40, 0x02, &[0x00, code])
        } else {
            Self::exchange(&dev, tx_id, 0x00, 0x05, 0x01, &[code])
        }
        .ok_or_else(|| kb_err(self.spec.name, "Keyboard rejected polling rate write"))?;
        let confirmed = Self::read_polling(&dev, tx_id).map(|(r, _)| r);
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
        if !self.spec.v3_rapid_trigger {
            return Err(kb_unsupported(self.spec.name, "firmware Rapid Trigger"));
        }
        let dev = self.open()?;
        let (tx_id, _) = Self::resolve_tx_id(&dev).ok_or_else(|| {
            kb_err(
                self.spec.name,
                "Keyboard did not answer Razer firmware probe",
            )
        })?;
        Self::exchange(
            &dev,
            tx_id,
            0x02,
            0x1A,
            0x08,
            &[0x00, 0x00, 0xFF, u8::from(enabled), 0, 0, 0, 0],
        )
        .ok_or_else(|| {
            kb_err(
                self.spec.name,
                "Keyboard did not acknowledge Rapid Trigger toggle",
            )
        })?;
        let actual = Self::exchange(&dev, tx_id, 0x02, 0x9A, 0x03, &[0x00, 0x00, 0xFF]);
        let ok = actual
            .as_ref()
            .is_some_and(|a| a.len() >= 4 && a[2] == 0xFF && (a[3] != 0) == enabled);
        if !ok {
            return Err(kb_verify_err(
                self.spec.name,
                if enabled { "ON" } else { "OFF" },
                format!("{actual:?}"),
            ));
        }
        Ok(enabled)
    }
}

pub(crate) fn detect_razer_keyboards(
    api: &HidApi,
    infos: &[&HidDeviceInfo],
    claimed: &mut HashSet<(u16, u16, String)>,
) -> Vec<RazerKeyboardDevice> {
    let mut found = Vec::new();
    for info in infos {
        if info.vendor_id() != RAZER_VID {
            continue;
        }
        // Strictly match cataloged keyboard PIDs only so Razer mice are never matched.
        let Some(spec) = lookup_razer_keyboard(info.product_id()) else {
            continue;
        };
        if info.usage_page() != 0x0001 || info.usage() != 0x0002 {
            continue;
        }
        let key = device_dedup_key(info);
        if claimed.contains(&key) {
            continue;
        }
        if api.open_path(info.path()).is_err() {
            continue;
        }
        claimed.insert(key);
        found.push(RazerKeyboardDevice {
            id: format!("razer-kb:1532:{:04x}", spec.pid),
            spec,
            path: info.path().to_owned(),
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn razer_kb_catalog_excludes_mice_and_computes_xor_checksum() {
        // Common Razer mouse PIDs (Viper V3 Pro, DeathAdder V3, etc.) MUST NOT appear here.
        assert!(lookup_razer_keyboard(0x00C0).is_none());
        assert!(lookup_razer_keyboard(0x00B6).is_none());
        // Huntsman V3 Pro TKL is present and supports V3 Rapid Trigger.
        let v3 = lookup_razer_keyboard(0x02A7).unwrap();
        assert!(v3.v3_rapid_trigger);

        let frame = encode_razer_kb_request(0x1F, 0x00, 0x81, 0x02, &[]);
        assert_eq!(frame[88], 0x02 ^ 0x81);
    }
}
