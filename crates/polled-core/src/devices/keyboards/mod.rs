// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Multi-brand keyboard architecture (`KeyboardDevice` trait, `KeyboardStatus`, and ordered
//! collision-safe HID detection registry).
//!
//! Protocol encodings for non-Wooting brands are independently implemented in Rust from verified
//! open-source protocol documentation and codecs in `OpenMouse-Project/keyboard-protocol`
//! (AGPL-3.0-or-later) and `chcbaram/baram-qmk-8k` (open source).
//!
//! Safety invariants enforced across every backend:
//! - Non-Wooting backends are explicitly tagged [`SupportTier::ImplementedFromDocs`].
//! - Destructive commands (factory reset, bootloader jump, EEPROM wipe, firmware flash) are
//!   NEVER encoded or sent.
//! - Razer and Logitech keyboard detectors strictly filter against curated keyboard product ID
//!   tables (and HID++ device-info verification on receivers) so gaming mice are NEVER claimed.
//! - Universal QMK/VIA (`0xFF60:0x61`) runs last and skips any `(vid, pid, serial)` already
//!   claimed by a dedicated driver (Wooting, Keychron HE, etc.).

pub mod arbiter;
pub mod keychron_he;
pub mod kochi;
pub mod logitech_kb;
pub mod razer_kb;
pub mod steelseries;
pub mod via_venom;
pub mod wooting_adapter;
pub mod wuque;

use std::collections::HashSet;
use std::fmt;
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo as HidDeviceInfo, HidApi, HidDevice};
use serde::{Deserialize, Serialize};

use crate::devices::{BatteryStatus, SupportTier, WootingKeyboardInfo};
use crate::error::{PolledError, Result};

/// Supported keyboard manufacturer / protocol family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardBrand {
    Wooting,
    Wuque,
    Keychron,
    SteelSeries,
    Razer,
    Arbiter,
    Logitech,
    Kochi,
    Geonworks,
    QmkVia,
}

impl KeyboardBrand {
    pub fn display_name(self) -> &'static str {
        match self {
            KeyboardBrand::Wooting => "Wooting",
            KeyboardBrand::Wuque => "Wuque Studio",
            KeyboardBrand::Keychron => "Keychron",
            KeyboardBrand::SteelSeries => "SteelSeries",
            KeyboardBrand::Razer => "Razer",
            KeyboardBrand::Arbiter => "Arbiter Studio",
            KeyboardBrand::Logitech => "Logitech",
            KeyboardBrand::Kochi => "Kochi",
            KeyboardBrand::Geonworks => "Geonworks",
            KeyboardBrand::QmkVia => "QMK / VIA",
        }
    }
}

impl fmt::Display for KeyboardBrand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

/// Onboard profile slot exposed by a keyboard (0-based index).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyboardProfileSlot {
    pub index: u8,
    pub name: String,
    pub is_active: bool,
}

/// Capability matrix describing which safe controls a detected keyboard supports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyboardCapabilities {
    pub supports_profile_switch: bool,
    /// `true` when onboard profile switching is RAM-only / zero flash wear (e.g. Wooting).
    pub profile_switch_volatile: bool,
    pub supports_base_actuation_write: bool,
    pub supports_per_key_actuation_write: bool,
    pub supports_rapid_trigger_toggle: bool,
    pub supports_polling_rate_write: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_polling_rates_hz: Vec<u16>,
    pub supports_system_mode_toggle: bool,
    pub supports_socd_toggle: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socd_label: Option<String>,
    pub supports_live_analog_stream: bool,
    pub supports_global_settings_write: bool,
    pub supports_debounce_tuning: bool,
}

/// Unified live keyboard snapshot consumed by the Keyboard tab, Tray, Profiles, Scorecard, and CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyboardStatus {
    pub id: String,
    pub brand: KeyboardBrand,
    pub name: String,
    pub vid_pid: String,
    pub support_tier: SupportTier,
    pub protocol_label: String,
    pub chip_type: String,
    pub switch_tech: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<u8>,
    #[serde(default)]
    pub profiles: Vec<KeyboardProfileSlot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actuation_mm: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actuation_range_mm: Option<(f32, f32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rapid_trigger_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rapid_trigger_mm: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polling_rate_hz: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<BatteryStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socd_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tachyon_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debounce_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debounce_ms: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_telemetry: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_note: Option<String>,
    pub capabilities: KeyboardCapabilities,
    /// Full Wooting-specific telemetry payload (present for Wooting keyboards so the interactive
    /// keymap, analog stream, counter-strafe analyzer, AKC, RGB, and per-key editor work identically).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wooting: Option<WootingKeyboardInfo>,
}

impl KeyboardStatus {
    pub fn profile_display_name(&self, slot: u8) -> String {
        if let Some(w) = &self.wooting {
            return w.profile_display_name(slot);
        }
        if let Some(p) = self.profiles.iter().find(|p| p.index == slot) {
            if !p.name.is_empty() {
                return format!("P{} · {}", slot + 1, p.name);
            }
        }
        format!("P{}", slot + 1)
    }

    pub fn sidebar_badge(&self) -> Option<String> {
        if let Some(p) = self.active_profile {
            return Some(format!("P{}", p + 1));
        }
        if let Some(hz) = self.polling_rate_hz {
            return Some(if hz >= 1000 {
                format!("{}K", hz / 1000)
            } else {
                format!("{hz}")
            });
        }
        if let Some(mode) = &self.system_mode {
            return Some(if mode.eq_ignore_ascii_case("macOS") {
                "MAC".to_string()
            } else {
                "WIN".to_string()
            });
        }
        Some("KB".to_string())
    }

    pub fn compact_status_summary(&self) -> String {
        if let Some(w) = &self.wooting {
            return w.compact_status_summary();
        }
        let mut parts = Vec::new();
        if let Some(slot) = self.active_profile {
            parts.push(self.profile_display_name(slot));
        }
        if let Some(mm) = self.actuation_mm {
            parts.push(format!("{mm:.2}mm"));
        }
        if let Some(true) = self.rapid_trigger_enabled {
            if let Some(rt) = self.rapid_trigger_mm {
                parts.push(format!("RT {rt:.2}mm"));
            } else {
                parts.push("RT ON".to_string());
            }
        }
        if let Some(hz) = self.polling_rate_hz {
            parts.push(format!("{hz} Hz"));
        }
        if let Some(bat) = self.battery {
            parts.push(bat.compact_label());
        }
        if let Some(mode) = &self.system_mode {
            parts.push(mode.clone());
        }
        if let Some(db) = &self.debounce_mode {
            if let Some(ms) = self.debounce_ms {
                parts.push(format!("{db} {ms}ms"));
            } else {
                parts.push(db.clone());
            }
        }
        if parts.is_empty() {
            self.protocol_label.clone()
        } else {
            parts.join(" · ")
        }
    }
}

/// Pluggable hardware keyboard device backend.
pub trait KeyboardDevice: Send {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn brand(&self) -> KeyboardBrand;
    fn support_tier(&self) -> SupportTier;
    fn read_status(&self) -> KeyboardStatus;

    fn switch_profile(&self, _slot: u8) -> Result<bool> {
        Err(kb_unsupported(self.name(), "onboard profile switching"))
    }

    fn set_polling_rate(&self, _hz: u16) -> Result<u16> {
        Err(kb_unsupported(self.name(), "polling rate configuration"))
    }

    fn set_base_actuation(
        &self,
        _actuation_mm: f32,
        _rt_enabled: Option<bool>,
        _rt_mm: Option<f32>,
    ) -> Result<()> {
        Err(kb_unsupported(self.name(), "actuation tuning"))
    }

    fn set_rapid_trigger_enabled(&self, _enabled: bool) -> Result<bool> {
        Err(kb_unsupported(self.name(), "Rapid Trigger toggle"))
    }

    fn set_system_mode(&self, _mac_mode: bool) -> Result<String> {
        Err(kb_unsupported(self.name(), "Windows/macOS mode switch"))
    }

    fn set_socd(&self, _enabled: bool) -> Result<bool> {
        Err(kb_unsupported(self.name(), "SOCD / key priority toggle"))
    }

    fn set_debounce_mode(&self, _typing_mode: bool, _debounce_ms: u8) -> Result<()> {
        Err(kb_unsupported(self.name(), "debounce tuning"))
    }
}

pub(crate) fn kb_err(device: impl Into<String>, message: impl Into<String>) -> PolledError {
    PolledError::Device {
        device: device.into(),
        message: message.into(),
    }
}

pub(crate) fn kb_unsupported(device: impl Into<String>, detail: impl Into<String>) -> PolledError {
    PolledError::UnsupportedSetting {
        device: device.into(),
        detail: detail.into(),
    }
}

pub(crate) fn kb_verify_err(
    device: impl Into<String>,
    expected: impl Into<String>,
    actual: impl Into<String>,
) -> PolledError {
    PolledError::VerificationFailed {
        device: device.into(),
        expected: expected.into(),
        actual: actual.into(),
        rollback_status: "not attempted (no prior snapshot)".to_string(),
    }
}

/// Stable deduplication key for a physical USB HID keyboard across interfaces.
pub(crate) fn device_dedup_key(info: &HidDeviceInfo) -> (u16, u16, String) {
    let serial = info.serial_number().unwrap_or_default().trim().to_string();
    (info.vendor_id(), info.product_id(), serial)
}

/// Sends a fixed-size output report on Report ID 0 (`[0x00, payload...]`) and reads replies
/// until `matcher(payload)` returns `true` or `timeout_ms` elapses.
///
/// Automatically normalizes Windows `hidapi` replies whether or not a leading `0x00` report ID
/// byte is present.
pub(crate) fn exchange_report0<const N: usize, F>(
    dev: &HidDevice,
    payload: &[u8; N],
    timeout_ms: u64,
    mut matcher: F,
) -> Option<Vec<u8>>
where
    F: FnMut(&[u8]) -> bool,
{
    // Drain any stale queued reports without blocking.
    let mut drain = [0u8; 128];
    for _ in 0..8 {
        match dev.read_timeout(&mut drain, 0) {
            Ok(n) if n > 0 => continue,
            _ => break,
        }
    }

    let mut wire = Vec::with_capacity(N + 1);
    wire.push(0x00);
    wire.extend_from_slice(payload);
    dev.write(&wire).ok()?;

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut buf = [0u8; 128];
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(10, 200) as i32;
        match dev.read_timeout(&mut buf, remaining) {
            Ok(n) if n > 0 => {
                let raw = &buf[..n];
                let slice = if n == N + 1 && raw[0] == 0x00 {
                    &raw[1..]
                } else {
                    raw
                };
                if matcher(slice) {
                    return Some(slice.to_vec());
                }
            }
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
    None
}

/// Sends a Report ID 0 feature report and reads back the Report ID 0 feature reply.
/// Returns the payload bytes (with the leading report ID byte stripped).
pub(crate) fn feature_exchange_report0(
    dev: &HidDevice,
    payload: &[u8],
    reply_payload_len: usize,
) -> Option<Vec<u8>> {
    let mut tx = Vec::with_capacity(payload.len() + 1);
    tx.push(0x00);
    tx.extend_from_slice(payload);
    dev.send_feature_report(&tx).ok()?;

    let mut rx = vec![0u8; reply_payload_len + 1];
    rx[0] = 0x00;
    let n = dev.get_feature_report(&mut rx).ok()?;
    if n <= 1 {
        return None;
    }
    Some(rx[1..n].to_vec())
}

/// Detects all connected supported keyboards across every registered brand backend in strict
/// collision-free priority order:
/// 1. Wooting (tested on hardware)
/// 2. Wuque Studio / SparkLink HE
/// 3. Keychron Hall Effect (before generic VIA)
/// 4. SteelSeries Apex Pro
/// 5. Razer Keyboards (strict keyboard PID catalog)
/// 6. Arbiter Studio
/// 7. Logitech HID++ 2.0 Keyboards (strict keyboard PID catalog + receiver probe)
/// 8. Kochi Hummingbird 68
/// 9. Geonworks Venom + Universal QMK / VIA (`0xFF60:0x61`, unclaimed boards only)
pub fn detect_all_keyboards() -> Result<Vec<Box<dyn KeyboardDevice>>> {
    let mut results: Vec<Box<dyn KeyboardDevice>> = Vec::new();
    let mut claimed: HashSet<(u16, u16, String)> = HashSet::new();

    // 1. Wooting first (existing hardware-tested implementation, zero regressions).
    if let Ok(wooting_boards) = crate::devices::wooting::detect_wooting_keyboards() {
        for dev in wooting_boards {
            let (vid, pid) = dev.vendor_product_ids();
            claimed.insert((vid, pid, String::new()));
            results.push(Box::new(wooting_adapter::WootingKeyboardAdapter::new(dev)));
        }
    }

    let api = HidApi::new().map_err(|e| kb_err("Keyboards", e.to_string()))?;

    // Collect metadata snapshot once.
    let infos: Vec<&HidDeviceInfo> = api.device_list().collect();

    // Mark Wooting serials too so universal VIA never double-claims a Wooting interface.
    for info in &infos {
        if info.vendor_id() == 0x31E3 || info.vendor_id() == 0x03EB {
            claimed.insert(device_dedup_key(info));
        }
    }

    // 2. Wuque Studio / SparkLink HE
    for dev in wuque::detect_wuque_keyboards(&api, &infos, &mut claimed) {
        results.push(Box::new(dev));
    }

    // 3. Keychron Hall Effect (must run before generic QMK/VIA on 0xFF60:0x61)
    for dev in keychron_he::detect_keychron_he_keyboards(&api, &infos, &mut claimed) {
        results.push(Box::new(dev));
    }

    // 4. SteelSeries Apex Pro
    for dev in steelseries::detect_steelseries_keyboards(&api, &infos, &mut claimed) {
        results.push(Box::new(dev));
    }

    // 5. Razer Keyboards (strict keyboard PID filter so mice are never claimed)
    for dev in razer_kb::detect_razer_keyboards(&api, &infos, &mut claimed) {
        results.push(Box::new(dev));
    }

    // 6. Arbiter Studio
    for dev in arbiter::detect_arbiter_keyboards(&api, &infos, &mut claimed) {
        results.push(Box::new(dev));
    }

    // 7. Logitech HID++ 2.0 Keyboards (strict keyboard PID filter + receiver pairing check)
    for dev in logitech_kb::detect_logitech_keyboards(&api, &infos, &mut claimed) {
        results.push(Box::new(dev));
    }

    // 8. Kochi Hummingbird 68
    for dev in kochi::detect_kochi_keyboards(&api, &infos, &mut claimed) {
        results.push(Box::new(dev));
    }

    // 9. Geonworks Venom + Universal QMK / VIA (runs last, skips claimed devices)
    for dev in via_venom::detect_via_and_venom_keyboards(&api, &infos, &mut claimed) {
        results.push(Box::new(dev));
    }

    Ok(results)
}
