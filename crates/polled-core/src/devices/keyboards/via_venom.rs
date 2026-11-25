// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Geonworks Venom (`chcbaram/baram-qmk-8k`) + Universal QMK / VIA raw-HID backend (`0xFF60:0x61`).
//!
//! - **Geonworks Venom (`VID 0x0483`, PIDs `0x5204/0x5205/0x5206/0x5207/0x520B`)**:
//!   Reads the open-source `0xB0` Venom dashboard protocol (`INFO`, `USB_HEALTH`: firmware string,
//!   matrix size, debounce type Gaming/Typing, debounce ms, USB bus re-enums / SOF stalls / SOF rate)
//!   and supports safe, read-back-verified VIA custom debounce tuning (`VIA 0x07/0x09`, channel 13).
//! - **Universal QMK / VIA (`0xFF60:0x61`, any vendor not already claimed)**:
//!   Reads VIA protocol version, layer count, firmware version, keycodes version, uptime, macro
//!   slots, and macro buffer size (zero destructive commands; EEPROM reset `0x0A`, bootloader jump
//!   `0x0B`, and macro reset `0x10` are NEVER encoded).

use std::collections::HashSet;
use std::ffi::CString;

use hidapi::{DeviceInfo as HidDeviceInfo, HidApi, HidDevice};

use crate::devices::keyboards::{
    device_dedup_key, exchange_report0, kb_err, kb_unsupported, kb_verify_err, KeyboardBrand,
    KeyboardCapabilities, KeyboardDevice, KeyboardStatus,
};
use crate::devices::SupportTier;
use crate::error::Result;

pub const VIA_USAGE_PAGE: u16 = 0xFF60;
pub const VIA_USAGE: u16 = 0x0061;
const VIA_UNHANDLED: u8 = 0xFF;

const VIA_CMD_PROTOCOL_VERSION: u8 = 0x01;
const VIA_CMD_GET_KB_VALUE: u8 = 0x02;
const VIA_CMD_CUSTOM_SET: u8 = 0x07;
const VIA_CMD_CUSTOM_SAVE: u8 = 0x09;
const VIA_CMD_MACRO_COUNT: u8 = 0x0C;
const VIA_CMD_MACRO_BUF_SIZE: u8 = 0x0D;
const VIA_CMD_LAYER_COUNT: u8 = 0x11;

const VIA_VAL_UPTIME: u8 = 0x01;
const VIA_VAL_FIRMWARE_VERSION: u8 = 0x04;
const VIA_VAL_KEYCODES_VERSION: u8 = 0x06;

// Geonworks Venom (`chcbaram/baram-qmk-8k`) custom dashboard constants.
pub const GEONWORKS_VENOM_VID: u16 = 0x0483;
const VENOM_RAW_CMD: u8 = 0xB0;
const VENOM_SUB_INFO: u8 = 0x01;
const VENOM_SUB_USB_HEALTH: u8 = 0x06;
const VENOM_CH_DEBOUNCE: u8 = 13;
const VENOM_DBV_TYPE: u8 = 1;
const VENOM_DBV_TIME: u8 = 2;

pub fn lookup_geonworks_venom(vid: u16, pid: u16) -> Option<&'static str> {
    if vid != GEONWORKS_VENOM_VID {
        return None;
    }
    match pid {
        0x5206 => Some("Geonworks VENOM-60MX (6.25U)"),
        0x5204 => Some("Geonworks VENOM-60MX (7U)"),
        0x5205 => Some("Geonworks VENOM-80MX (6.25U)"),
        0x5207 => Some("Geonworks VENOM-80MX (7U)"),
        0x520B => Some("Geonworks WISH45-8K"),
        _ => None,
    }
}

pub fn encode_via_frame(cmd: u8, args: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[0] = cmd;
    let n = args.len().min(31);
    out[1..1 + n].copy_from_slice(&args[..n]);
    out
}

pub fn encode_venom_frame(sub: u8, args: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[0] = VENOM_RAW_CMD;
    out[1] = sub;
    let n = args.len().min(30);
    out[2..2 + n].copy_from_slice(&args[..n]);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenomInfoReply {
    pub api_version: u8,
    pub debounce_type: u8,
    pub debounce_ms: u8,
    pub matrix_rows: u8,
    pub matrix_cols: u8,
    pub firmware: Option<String>,
}

pub fn decode_venom_info(reply: &[u8]) -> Option<VenomInfoReply> {
    if reply.len() < 26 || reply[0] != VENOM_RAW_CMD || reply[1] != VENOM_SUB_INFO {
        return None;
    }
    let ver_raw = &reply[10..26];
    let end = ver_raw
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(ver_raw.len());
    let fw = String::from_utf8_lossy(&ver_raw[..end]).trim().to_string();
    Some(VenomInfoReply {
        api_version: reply[2],
        debounce_type: reply[3],
        debounce_ms: reply[4],
        matrix_rows: reply[5],
        matrix_cols: reply[6],
        firmware: (!fw.is_empty()).then_some(fw),
    })
}

pub struct ViaOrVenomKeyboardDevice {
    id: String,
    brand: KeyboardBrand,
    name: String,
    vid: u16,
    pid: u16,
    is_venom: bool,
    path: CString,
}

impl ViaOrVenomKeyboardDevice {
    fn open(&self) -> Result<HidDevice> {
        let api = HidApi::new().map_err(|e| kb_err(&self.name, e.to_string()))?;
        api.open_path(self.path.as_c_str())
            .map_err(|e| kb_err(&self.name, format!("open failed: {e}")))
    }

    fn via_call(dev: &HidDevice, cmd: u8, args: &[u8], header_len: usize) -> Option<Vec<u8>> {
        let req = encode_via_frame(cmd, args);
        let rep = exchange_report0(dev, &req, 450, |r| {
            if r.first().copied() == Some(VIA_UNHANDLED) {
                return true;
            }
            if r.len() < 1 + header_len || r[0] != cmd {
                return false;
            }
            r[1..1 + header_len] == req[1..1 + header_len]
        })?;
        if rep.first().copied() == Some(VIA_UNHANDLED) {
            return None;
        }
        Some(rep)
    }

    fn venom_call(dev: &HidDevice, sub: u8, args: &[u8]) -> Option<Vec<u8>> {
        let req = encode_venom_frame(sub, args);
        exchange_report0(dev, &req, 450, |r| {
            r.len() >= 2 && r[0] == VENOM_RAW_CMD && r[1] == sub
        })
    }
}

impl KeyboardDevice for ViaOrVenomKeyboardDevice {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn brand(&self) -> KeyboardBrand {
        self.brand
    }

    fn support_tier(&self) -> SupportTier {
        SupportTier::ImplementedFromDocs
    }

    fn read_status(&self) -> KeyboardStatus {
        let mut firmware_version = None;
        let mut protocol_label = if self.is_venom {
            "Geonworks Venom 0xB0 + VIA".to_string()
        } else {
            "QMK / VIA Raw-HID".to_string()
        };
        let mut debounce_mode = None;
        let mut debounce_ms = None;
        let mut supports_debounce_tuning = false;
        let mut extra = Vec::new();

        if let Ok(dev) = self.open() {
            // Standard VIA reads.
            if let Some(pv) = Self::via_call(&dev, VIA_CMD_PROTOCOL_VERSION, &[], 0) {
                if pv.len() >= 3 {
                    let ver = u16::from_be_bytes([pv[1], pv[2]]);
                    if !self.is_venom {
                        protocol_label = format!("VIA Protocol v{ver}");
                    } else {
                        extra.push(("VIA Protocol".to_string(), format!("v{ver}")));
                    }
                    if ver >= 8 {
                        if let Some(lc) = Self::via_call(&dev, VIA_CMD_LAYER_COUNT, &[], 0) {
                            if let Some(&layers) = lc.get(1) {
                                extra.push(("Keymap Layers".to_string(), layers.to_string()));
                            }
                        }
                    } else {
                        extra.push(("Keymap Layers".to_string(), "4 (legacy)".to_string()));
                    }
                }
            }
            if let Some(fw) =
                Self::via_call(&dev, VIA_CMD_GET_KB_VALUE, &[VIA_VAL_FIRMWARE_VERSION], 1)
            {
                if fw.len() >= 6 {
                    let v = u32::from_be_bytes([fw[2], fw[3], fw[4], fw[5]]);
                    if v != 0 {
                        firmware_version = Some(format!("{v}"));
                    }
                }
            }
            if let Some(kv) =
                Self::via_call(&dev, VIA_CMD_GET_KB_VALUE, &[VIA_VAL_KEYCODES_VERSION], 1)
            {
                if kv.len() >= 6 {
                    let v = u32::from_be_bytes([kv[2], kv[3], kv[4], kv[5]]);
                    if v != 0 {
                        extra.push(("Keycodes Rev".to_string(), format!("0x{v:08X}")));
                    }
                }
            }
            if let Some(up) = Self::via_call(&dev, VIA_CMD_GET_KB_VALUE, &[VIA_VAL_UPTIME], 1) {
                if up.len() >= 6 {
                    let ms = u32::from_be_bytes([up[2], up[3], up[4], up[5]]);
                    let mins = ms / 60_000;
                    extra.push(("Board Uptime".to_string(), format!("{mins} min")));
                }
            }
            if let Some(mc) = Self::via_call(&dev, VIA_CMD_MACRO_COUNT, &[], 0) {
                if let Some(&count) = mc.get(1) {
                    let buf_sz = Self::via_call(&dev, VIA_CMD_MACRO_BUF_SIZE, &[], 0)
                        .and_then(|r| (r.len() >= 3).then(|| u16::from_be_bytes([r[1], r[2]])));
                    let label = match buf_sz {
                        Some(sz) => format!("{count} slots ({sz} B)"),
                        None => format!("{count} slots"),
                    };
                    extra.push(("Macros".to_string(), label));
                }
            }

            // Geonworks Venom 8K (`baram-qmk-8k`) custom `0xB0` dashboard telemetry.
            if self.is_venom {
                if let Some(rep) = Self::venom_call(&dev, VENOM_SUB_INFO, &[]) {
                    if let Some(info) = decode_venom_info(&rep) {
                        if let Some(fw) = info.firmware {
                            firmware_version = Some(fw);
                        }
                        extra.push((
                            "Matrix".to_string(),
                            format!("{} x {}", info.matrix_rows, info.matrix_cols),
                        ));
                        extra.push(("Venom API".to_string(), format!("v{}", info.api_version)));
                        if info.debounce_type != 0xFF {
                            supports_debounce_tuning = true;
                            debounce_mode = Some(if info.debounce_type == 1 {
                                "Typing".to_string()
                            } else {
                                "Gaming".to_string()
                            });
                            debounce_ms = Some(info.debounce_ms);
                        }
                        if info.api_version >= 2 {
                            if let Some(usb) = Self::venom_call(&dev, VENOM_SUB_USB_HEALTH, &[0]) {
                                if usb.len() >= 13 {
                                    let resets = u16::from_le_bytes([usb[3], usb[4]]);
                                    let stalls = u16::from_le_bytes([usb[7], usb[8]]);
                                    let sof_rate = u16::from_le_bytes([usb[11], usb[12]]);
                                    extra.push((
                                        "USB SOF Heartbeat".to_string(),
                                        format!("{sof_rate} SOF/s"),
                                    ));
                                    extra.push((
                                        "USB Link Errors".to_string(),
                                        format!("{resets} re-enum · {stalls} stalls"),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        let capabilities = KeyboardCapabilities {
            supports_debounce_tuning,
            ..Default::default()
        };

        KeyboardStatus {
            id: self.id.clone(),
            brand: self.brand,
            name: self.name.clone(),
            vid_pid: format!("{:04x}:{:04x}", self.vid, self.pid),
            support_tier: SupportTier::ImplementedFromDocs,
            protocol_label,
            chip_type: if self.is_venom {
                "STM32 8K HS (baram-qmk-8k)"
            } else {
                "QMK / VIA MCU"
            }
            .to_string(),
            switch_tech: if self.is_venom {
                "Mechanical 8KHz (MX)"
            } else {
                "QMK / VIA"
            }
            .to_string(),
            firmware_version,
            layout: None,
            active_profile: None,
            profiles: Vec::new(),
            actuation_mm: None,
            actuation_range_mm: None,
            rapid_trigger_enabled: None,
            rapid_trigger_mm: None,
            polling_rate_hz: self.is_venom.then_some(8000),
            battery: None,
            system_mode: None,
            socd_enabled: None,
            tachyon_enabled: None,
            debounce_mode,
            debounce_ms,
            extra_telemetry: extra,
            status_note: Some(if self.is_venom {
                "Implemented from open-source Geonworks baram-qmk-8k protocol docs; debounce writes are confirm-gated and verified by read-back."
                    .to_string()
            } else {
                "Universal read-only VIA raw-HID telemetry (0xFF60:0x61); works with any QMK/VIA custom keyboard."
                    .to_string()
            }),
            capabilities,
            wooting: None,
        }
    }

    fn set_debounce_mode(&self, typing_mode: bool, debounce_ms: u8) -> Result<()> {
        if !self.is_venom {
            return Err(kb_unsupported(&self.name, "Venom runtime debounce tuning"));
        }
        let clamped_ms = debounce_ms.clamp(5, 40);
        let target_type = u8::from(typing_mode);
        let dev = self.open()?;

        Self::via_call(
            &dev,
            VIA_CMD_CUSTOM_SET,
            &[VENOM_CH_DEBOUNCE, VENOM_DBV_TYPE, target_type],
            0,
        )
        .ok_or_else(|| kb_err(&self.name, "Keyboard did not acknowledge debounce type"))?;
        Self::via_call(
            &dev,
            VIA_CMD_CUSTOM_SET,
            &[VENOM_CH_DEBOUNCE, VENOM_DBV_TIME, clamped_ms],
            0,
        )
        .ok_or_else(|| kb_err(&self.name, "Keyboard did not acknowledge debounce time"))?;
        Self::via_call(&dev, VIA_CMD_CUSTOM_SAVE, &[VENOM_CH_DEBOUNCE], 0).ok_or_else(|| {
            kb_err(
                &self.name,
                "Keyboard did not acknowledge debounce EEPROM save",
            )
        })?;

        let readback =
            Self::venom_call(&dev, VENOM_SUB_INFO, &[]).and_then(|r| decode_venom_info(&r));
        match readback {
            Some(info) if info.debounce_type == target_type && info.debounce_ms == clamped_ms => {
                Ok(())
            }
            other => Err(kb_verify_err(
                &self.name,
                format!(
                    "{} · {clamped_ms} ms",
                    if typing_mode { "Typing" } else { "Gaming" }
                ),
                format!("{other:?}"),
            )),
        }
    }
}

pub(crate) fn detect_via_and_venom_keyboards(
    api: &HidApi,
    infos: &[&HidDeviceInfo],
    claimed: &mut HashSet<(u16, u16, String)>,
) -> Vec<ViaOrVenomKeyboardDevice> {
    let mut found = Vec::new();
    for info in infos {
        if info.usage_page() != VIA_USAGE_PAGE || info.usage() != VIA_USAGE {
            continue;
        }
        let key = device_dedup_key(info);
        if claimed.contains(&key) {
            continue;
        }
        let Ok(dev) = api.open_path(info.path()) else {
            continue;
        };
        // Verify the interface actually answers VIA GET_PROTOCOL_VERSION (0x01) so non-VIA
        // devices that happen to use 0xFF60:0x61 are never misidentified.
        let pv = ViaOrVenomKeyboardDevice::via_call(&dev, VIA_CMD_PROTOCOL_VERSION, &[], 0);
        if pv.is_none() {
            continue;
        }
        claimed.insert(key);
        let vid = info.vendor_id();
        let pid = info.product_id();
        let venom_name = lookup_geonworks_venom(vid, pid);
        let (brand, name, is_venom) = match venom_name {
            Some(n) => (KeyboardBrand::Geonworks, n.to_string(), true),
            None => {
                let prod = info
                    .product_string()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("QMK/VIA Keyboard ({vid:04x}:{pid:04x})"));
                (KeyboardBrand::QmkVia, prod, false)
            }
        };
        found.push(ViaOrVenomKeyboardDevice {
            id: format!(
                "{}:{vid:04x}:{pid:04x}",
                if is_venom { "geonworks" } else { "via" }
            ),
            brand,
            name,
            vid,
            pid,
            is_venom,
            path: info.path().to_owned(),
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geonworks_venom_catalog_and_info_decoder_match_baram_qmk_8k() {
        assert_eq!(
            lookup_geonworks_venom(0x0483, 0x5206),
            Some("Geonworks VENOM-60MX (6.25U)")
        );
        assert_eq!(
            lookup_geonworks_venom(0x0483, 0x520B),
            Some("Geonworks WISH45-8K")
        );
        assert!(lookup_geonworks_venom(0x0483, 0x9999).is_none());

        let mut rep = [0u8; 32];
        rep[0] = VENOM_RAW_CMD;
        rep[1] = VENOM_SUB_INFO;
        rep[2] = 2; // API v2
        rep[3] = 0; // Gaming
        rep[4] = 5; // 5 ms
        rep[5] = 5; // 5 rows
        rep[6] = 14; // 14 cols
        rep[10..17].copy_from_slice(b"v2.1.0\0");
        let parsed = decode_venom_info(&rep).unwrap();
        assert_eq!(parsed.api_version, 2);
        assert_eq!(parsed.debounce_type, 0);
        assert_eq!(parsed.debounce_ms, 5);
        assert_eq!(parsed.matrix_rows, 5);
        assert_eq!(parsed.matrix_cols, 14);
        assert_eq!(parsed.firmware.as_deref(), Some("v2.1.0"));
    }
}
