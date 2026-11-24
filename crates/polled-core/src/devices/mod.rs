// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod compx;
pub mod compx64;
pub mod controller;
pub mod dry_run;
pub mod endgame_gear;
pub mod finalmouse;
pub mod hidpp20;
pub mod incott;
pub mod keychron;
pub mod mchose;
pub mod ninjutso;
pub mod razer;
pub mod vaxee;

use std::fmt;

use hidapi::HidApi;
use serde::{Deserialize, Serialize};

use crate::error::{PolledError, Result};

pub use compx::{CompxDevice, CompxProtocolDriver};
pub use compx64::{Compx64Device, Compx64ProtocolDriver};
pub use controller::{ApplyOutcome, SafeDeviceController};
pub use dry_run::DryRunDevice;
pub use endgame_gear::{EndgameGearOp1Device, EndgameGearOp1ProtocolDriver};
pub use finalmouse::{FinalmouseDevice, FinalmouseProtocolDriver};
pub use hidpp20::{Hidpp20Device, Hidpp20ProtocolDriver};
pub use incott::{IncottDevice, IncottProtocolDriver};
pub use keychron::{KeychronDevice, KeychronProtocolDriver};
pub use mchose::{MchoseDevice, MchoseProtocolDriver};
pub use ninjutso::{NinjutsoDevice, NinjutsoProtocolDriver};
pub use razer::{RazerDevice, RazerProtocolDriver};
pub use vaxee::{VaxeeDevice, VaxeeProtocolDriver};

/// Explicitly communicates how a device backend was verified (surfaced in UI and README).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportTier {
    TestedOnHardware,
    ImplementedFromDocs,
    Simulator,
}

impl SupportTier {
    pub fn label(self) -> &'static str {
        match self {
            SupportTier::TestedOnHardware => "Tested on hardware",
            SupportTier::ImplementedFromDocs => "Implemented from docs",
            SupportTier::Simulator => "Dry-run simulator",
        }
    }
}

/// Distinguishes volatile RAM updates from persistent onboard flash writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// Writes to volatile memory only (no flash wear).
    VolatileRam,
    /// Writes to onboard flash memory (must be diffed & rate-limited to avoid wear).
    OnboardFlash,
}

/// Supported lift-off distance presets, stored as an exact enum to avoid float rounding bugs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LiftOffDistance {
    #[serde(rename = "0.7mm")]
    Mm07,
    #[serde(rename = "1.0mm")]
    Mm10,
    #[serde(rename = "2.0mm")]
    Mm20,
}

impl LiftOffDistance {
    pub fn label(self) -> &'static str {
        match self {
            LiftOffDistance::Mm07 => "0.7 mm",
            LiftOffDistance::Mm10 => "1.0 mm",
            LiftOffDistance::Mm20 => "2.0 mm",
        }
    }

    pub const ALL: [LiftOffDistance; 3] = [
        LiftOffDistance::Mm07,
        LiftOffDistance::Mm10,
        LiftOffDistance::Mm20,
    ];

    /// Exact lift-off height in tenths of a millimeter (avoids floating-point comparisons).
    pub fn tenths_mm(self) -> u8 {
        match self {
            LiftOffDistance::Mm07 => 7,
            LiftOffDistance::Mm10 => 10,
            LiftOffDistance::Mm20 => 20,
        }
    }
}

impl fmt::Display for LiftOffDistance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Read-only wireless mouse battery snapshot reported by supported HID protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatteryStatus {
    /// Charge percentage (`0..=100`).
    pub percent: u8,
    /// `true` when charging over USB or on a charging dock.
    pub charging: bool,
    /// Optional cell voltage in millivolts when reported by firmware.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voltage_mv: Option<u16>,
}

impl BatteryStatus {
    pub fn compact_label(self) -> String {
        if self.charging {
            format!("{}% CHG", self.percent)
        } else {
            format!("{}%", self.percent)
        }
    }

    pub fn detail_label(self) -> String {
        let state = if self.charging {
            "Charging"
        } else {
            "Discharging"
        };
        match self.voltage_mv {
            Some(mv) if mv > 0 => format!("{}% ({state}, {mv} mV)", self.percent),
            _ => format!("{}% ({state})", self.percent),
        }
    }
}

/// Hardware mouse settings managed by a profile.
///
/// Any `None` field means "leave unchanged" when applying a profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<u16>,
    /// Optional independent vertical (Y-axis) DPI. `None` means uniform (`Y == X`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi_y: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polling_rate_hz: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lod: Option<LiftOffDistance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debounce_ms: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_sync: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_snapping: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ripple_control: Option<bool>,
    /// Optional wireless idle sleep timeout in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleep_timeout_secs: Option<u16>,
}

impl MouseSettings {
    pub fn is_empty(&self) -> bool {
        self.dpi.is_none()
            && self.dpi_y.is_none()
            && self.polling_rate_hz.is_none()
            && self.lod.is_none()
            && self.debounce_ms.is_none()
            && self.motion_sync.is_none()
            && self.angle_snapping.is_none()
            && self.ripple_control.is_none()
            && self.sleep_timeout_secs.is_none()
    }

    /// Returns `true` if every `Some(...)` field in `desired` matches `self`.
    pub fn matches_desired(&self, desired: &MouseSettings) -> bool {
        if let Some(dpi) = desired.dpi {
            if self.dpi != Some(dpi) {
                return false;
            }
            if desired.dpi_y.is_none() && self.dpi_y.is_some_and(|y| y != dpi) {
                return false;
            }
        }
        if let Some(dpi_y) = desired.dpi_y {
            let actual_y = self.dpi_y.or(self.dpi);
            if actual_y != Some(dpi_y) {
                return false;
            }
        }
        if let Some(hz) = desired.polling_rate_hz {
            if self.polling_rate_hz != Some(hz) {
                return false;
            }
        }
        if let Some(lod) = desired.lod {
            if self.lod != Some(lod) {
                return false;
            }
        }
        if let Some(db) = desired.debounce_ms {
            if self.debounce_ms != Some(db) {
                return false;
            }
        }
        if let Some(ms) = desired.motion_sync {
            if self.motion_sync != Some(ms) {
                return false;
            }
        }
        if let Some(ang) = desired.angle_snapping {
            if self.angle_snapping != Some(ang) {
                return false;
            }
        }
        if let Some(rip) = desired.ripple_control {
            if self.ripple_control != Some(rip) {
                return false;
            }
        }
        if let Some(sleep) = desired.sleep_timeout_secs {
            if self.sleep_timeout_secs != Some(sleep) {
                return false;
            }
        }
        true
    }

    /// Produces a merged settings struct where `Some(...)` fields from `desired` override `self`.
    pub fn merged_with(&self, desired: &MouseSettings) -> MouseSettings {
        let next_dpi = desired.dpi.or(self.dpi);
        let next_dpi_y = if let Some(y) = desired.dpi_y {
            if next_dpi == Some(y) {
                None
            } else {
                Some(y)
            }
        } else if desired.dpi.is_some() {
            // Setting a uniform DPI clears any previous separate Y DPI.
            None
        } else {
            self.dpi_y
        };
        MouseSettings {
            dpi: next_dpi,
            dpi_y: next_dpi_y,
            polling_rate_hz: desired.polling_rate_hz.or(self.polling_rate_hz),
            lod: desired.lod.or(self.lod),
            debounce_ms: desired.debounce_ms.or(self.debounce_ms),
            motion_sync: desired.motion_sync.or(self.motion_sync),
            angle_snapping: desired.angle_snapping.or(self.angle_snapping),
            ripple_control: desired.ripple_control.or(self.ripple_control),
            sleep_timeout_secs: desired.sleep_timeout_secs.or(self.sleep_timeout_secs),
        }
    }

    pub fn summary_string(&self) -> String {
        let mut parts = Vec::new();
        match (self.dpi, self.dpi_y) {
            (Some(x), Some(y)) if x != y => parts.push(format!("{x}x{y} DPI")),
            (Some(x), _) => parts.push(format!("{x} DPI")),
            (None, Some(y)) => parts.push(format!("Y {y} DPI")),
            (None, None) => {}
        }
        if let Some(hz) = self.polling_rate_hz {
            parts.push(format!("{hz} Hz"));
        }
        if let Some(lod) = self.lod {
            parts.push(format!("LOD {lod}"));
        }
        if let Some(db) = self.debounce_ms {
            parts.push(format!("Debounce {db} ms"));
        }
        if let Some(ms) = self.motion_sync {
            parts.push(format!("Motion Sync {}", if ms { "ON" } else { "OFF" }));
        }
        if let Some(ang) = self.angle_snapping {
            parts.push(format!("Angle Snap {}", if ang { "ON" } else { "OFF" }));
        }
        if let Some(rip) = self.ripple_control {
            parts.push(format!("Ripple {}", if rip { "ON" } else { "OFF" }));
        }
        if let Some(secs) = self.sleep_timeout_secs {
            if secs >= 60 && secs.is_multiple_of(60) {
                parts.push(format!("Sleep {}m", secs / 60));
            } else {
                parts.push(format!("Sleep {secs}s"));
            }
        }
        if parts.is_empty() {
            "(no mouse changes)".to_string()
        } else {
            parts.join(", ")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub vid_pid: Option<String>,
    pub connection: String,
    pub support_tier: SupportTier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCapabilities {
    pub dpi_min: u16,
    pub dpi_max: u16,
    pub dpi_step: u16,
    #[serde(default)]
    pub supports_xy_dpi: bool,
    pub polling_rates_hz: Vec<u16>,
    pub lod_options: Vec<LiftOffDistance>,
    pub supports_debounce: bool,
    pub debounce_min_ms: u8,
    pub debounce_max_ms: u8,
    #[serde(default)]
    pub supports_motion_sync: bool,
    #[serde(default)]
    pub supports_angle_snapping: bool,
    #[serde(default)]
    pub supports_ripple_control: bool,
    #[serde(default)]
    pub supports_sleep_timeout: bool,
    #[serde(default)]
    pub sleep_timeout_min_secs: u16,
    #[serde(default)]
    pub sleep_timeout_max_secs: u16,
    /// If `false`, writes touch onboard flash and MUST be diffed and rate-limited.
    pub supports_volatile_writes: bool,
    pub flash_write_warning: Option<String>,
}

impl DeviceCapabilities {
    pub fn validate(&self, device_name: &str, settings: &MouseSettings) -> Result<()> {
        if let Some(dpi) = settings.dpi {
            if dpi < self.dpi_min || dpi > self.dpi_max {
                return Err(PolledError::UnsupportedSetting {
                    device: device_name.to_string(),
                    detail: format!(
                        "DPI {dpi} out of supported range {}..={}",
                        self.dpi_min, self.dpi_max
                    ),
                });
            }
            if self.dpi_step > 1 && !(dpi - self.dpi_min).is_multiple_of(self.dpi_step) {
                return Err(PolledError::UnsupportedSetting {
                    device: device_name.to_string(),
                    detail: format!(
                        "DPI {dpi} must be in increments of {} starting from {}",
                        self.dpi_step, self.dpi_min
                    ),
                });
            }
        }

        if let Some(hz) = settings.polling_rate_hz {
            if !self.polling_rates_hz.contains(&hz) {
                return Err(PolledError::UnsupportedSetting {
                    device: device_name.to_string(),
                    detail: format!(
                        "Polling rate {hz} Hz not supported (supported: {:?})",
                        self.polling_rates_hz
                    ),
                });
            }
        }

        if let Some(lod) = settings.lod {
            if !self.lod_options.contains(&lod) {
                return Err(PolledError::UnsupportedSetting {
                    device: device_name.to_string(),
                    detail: format!("LOD {lod} not supported by {device_name}"),
                });
            }
        }

        if let Some(db) = settings.debounce_ms {
            if !self.supports_debounce {
                return Err(PolledError::UnsupportedSetting {
                    device: device_name.to_string(),
                    detail: format!("Debounce is not configurable on {device_name}"),
                });
            }
            if db < self.debounce_min_ms || db > self.debounce_max_ms {
                return Err(PolledError::UnsupportedSetting {
                    device: device_name.to_string(),
                    detail: format!(
                        "Debounce {db} ms out of supported range {}..={} ms",
                        self.debounce_min_ms, self.debounce_max_ms
                    ),
                });
            }
        }

        if settings.motion_sync.is_some() && !self.supports_motion_sync {
            return Err(PolledError::UnsupportedSetting {
                device: device_name.to_string(),
                detail: format!("Motion Sync is not configurable on {device_name}"),
            });
        }
        if settings.angle_snapping.is_some() && !self.supports_angle_snapping {
            return Err(PolledError::UnsupportedSetting {
                device: device_name.to_string(),
                detail: format!("Angle Snapping is not configurable on {device_name}"),
            });
        }
        if settings.ripple_control.is_some() && !self.supports_ripple_control {
            return Err(PolledError::UnsupportedSetting {
                device: device_name.to_string(),
                detail: format!("Ripple Control is not configurable on {device_name}"),
            });
        }

        if let Some(dpi_y) = settings.dpi_y {
            if !self.supports_xy_dpi {
                return Err(PolledError::UnsupportedSetting {
                    device: device_name.to_string(),
                    detail: format!("Independent Y DPI is not configurable on {device_name}"),
                });
            }
            if dpi_y < self.dpi_min || dpi_y > self.dpi_max {
                return Err(PolledError::UnsupportedSetting {
                    device: device_name.to_string(),
                    detail: format!(
                        "Y DPI {dpi_y} out of supported range {}..={}",
                        self.dpi_min, self.dpi_max
                    ),
                });
            }
            if self.dpi_step > 1 && !(dpi_y - self.dpi_min).is_multiple_of(self.dpi_step) {
                return Err(PolledError::UnsupportedSetting {
                    device: device_name.to_string(),
                    detail: format!(
                        "Y DPI {dpi_y} must be in increments of {} starting from {}",
                        self.dpi_step, self.dpi_min
                    ),
                });
            }
        }

        if let Some(secs) = settings.sleep_timeout_secs {
            if !self.supports_sleep_timeout {
                return Err(PolledError::UnsupportedSetting {
                    device: device_name.to_string(),
                    detail: format!("Idle sleep timeout is not configurable on {device_name}"),
                });
            }
            if secs < self.sleep_timeout_min_secs || secs > self.sleep_timeout_max_secs {
                return Err(PolledError::UnsupportedSetting {
                    device: device_name.to_string(),
                    detail: format!(
                        "Sleep timeout {secs}s out of supported range {}..={}s",
                        self.sleep_timeout_min_secs, self.sleep_timeout_max_secs
                    ),
                });
            }
        }

        Ok(())
    }

    /// Adapts `desired` to the nearest settings supported by this mouse so shared profiles still
    /// switch cleanly across mice with different capabilities (e.g., LOD `0.7 mm` -> `1.0 mm`,
    /// `8000 Hz` -> `4000 Hz`, or 10-step DPI snapped to 50-step).
    ///
    /// Returns `(adapted_settings, human_readable_adjustments)`.
    pub fn adapt_settings(&self, desired: &MouseSettings) -> (MouseSettings, Vec<String>) {
        let mut out = desired.clone();
        let mut notes = Vec::new();

        if let Some(dpi) = desired.dpi {
            let clamped = dpi.clamp(self.dpi_min, self.dpi_max);
            let snapped = if self.dpi_step > 1 {
                let step = u32::from(self.dpi_step);
                let offset = u32::from(clamped - self.dpi_min);
                let rounded = ((offset + step / 2) / step) * step;
                let max_valid = u32::from(self.dpi_min)
                    + (u32::from(self.dpi_max - self.dpi_min) / step) * step;
                (u32::from(self.dpi_min) + rounded).min(max_valid) as u16
            } else {
                clamped
            };
            if snapped != dpi {
                notes.push(format!("DPI {dpi} -> {snapped}"));
            }
            out.dpi = Some(snapped);
        }

        if let Some(hz) = desired.polling_rate_hz {
            if !self.polling_rates_hz.contains(&hz) {
                if let Some(&adapted) = self
                    .polling_rates_hz
                    .iter()
                    .filter(|&&r| r <= hz)
                    .max()
                    .or_else(|| self.polling_rates_hz.iter().min())
                {
                    notes.push(format!("Polling {hz} Hz -> {adapted} Hz"));
                    out.polling_rate_hz = Some(adapted);
                } else {
                    notes.push(format!("Polling {hz} Hz skipped"));
                    out.polling_rate_hz = None;
                }
            }
        }

        if let Some(lod) = desired.lod {
            if !self.lod_options.contains(&lod) {
                if let Some(&adapted) = self.lod_options.iter().min_by_key(|&&opt| {
                    (opt.tenths_mm().abs_diff(lod.tenths_mm()), opt.tenths_mm())
                }) {
                    notes.push(format!("LOD {lod} -> {adapted}"));
                    out.lod = Some(adapted);
                } else {
                    notes.push(format!("LOD {lod} skipped"));
                    out.lod = None;
                }
            }
        }

        if let Some(db) = desired.debounce_ms {
            if !self.supports_debounce {
                notes.push(format!("Debounce {db} ms skipped"));
                out.debounce_ms = None;
            } else {
                let clamped = db.clamp(self.debounce_min_ms, self.debounce_max_ms);
                if clamped != db {
                    notes.push(format!("Debounce {db} ms -> {clamped} ms"));
                }
                out.debounce_ms = Some(clamped);
            }
        }

        if desired.motion_sync.is_some() && !self.supports_motion_sync {
            notes.push("Motion Sync skipped".to_string());
            out.motion_sync = None;
        }
        if desired.angle_snapping.is_some() && !self.supports_angle_snapping {
            notes.push("Angle Snapping skipped".to_string());
            out.angle_snapping = None;
        }
        if desired.ripple_control.is_some() && !self.supports_ripple_control {
            notes.push("Ripple Control skipped".to_string());
            out.ripple_control = None;
        }

        if let Some(dpi_y) = desired.dpi_y {
            if !self.supports_xy_dpi {
                notes.push(format!("Y DPI {dpi_y} skipped"));
                out.dpi_y = None;
            } else {
                let clamped = dpi_y.clamp(self.dpi_min, self.dpi_max);
                let snapped = if self.dpi_step > 1 {
                    let step = u32::from(self.dpi_step);
                    let offset = u32::from(clamped - self.dpi_min);
                    let rounded = ((offset + step / 2) / step) * step;
                    let max_valid = u32::from(self.dpi_min)
                        + (u32::from(self.dpi_max - self.dpi_min) / step) * step;
                    (u32::from(self.dpi_min) + rounded).min(max_valid) as u16
                } else {
                    clamped
                };
                if snapped != dpi_y {
                    notes.push(format!("Y DPI {dpi_y} -> {snapped}"));
                }
                out.dpi_y = Some(snapped);
            }
        }

        if let Some(secs) = desired.sleep_timeout_secs {
            if !self.supports_sleep_timeout {
                notes.push(format!("Sleep {secs}s skipped"));
                out.sleep_timeout_secs = None;
            } else {
                let clamped = secs.clamp(self.sleep_timeout_min_secs, self.sleep_timeout_max_secs);
                if clamped != secs {
                    notes.push(format!("Sleep {secs}s -> {clamped}s"));
                }
                out.sleep_timeout_secs = Some(clamped);
            }
        }

        (out, notes)
    }
}

/// Core hardware abstraction implemented by every mouse backend (and `DryRunDevice`).
pub trait MouseDevice: Send {
    fn info(&self) -> DeviceInfo;
    fn capabilities(&self) -> DeviceCapabilities;
    fn read_settings(&mut self) -> Result<MouseSettings>;
    fn apply_settings(&mut self, settings: &MouseSettings, mode: WriteMode) -> Result<()>;

    /// Optional read-only battery query. Returns `None` for wired mice, sleeping receivers,
    /// or protocols without a verified battery command.
    fn read_battery(&mut self) -> Option<BatteryStatus> {
        None
    }
}

/// Discovery factory that scans connected HID interfaces for devices it supports.
pub trait DeviceDriver: Send + Sync {
    fn driver_name(&self) -> &'static str;
    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>>;
}

/// Runs all registered protocol-family drivers against connected USB/HID devices.
pub fn detect_hardware_devices() -> Result<Vec<Box<dyn MouseDevice>>> {
    let api = HidApi::new().map_err(|e| PolledError::Win32 {
        context: "HidApi::new".to_string(),
        message: e.to_string(),
    })?;
    let drivers: Vec<Box<dyn DeviceDriver>> = vec![
        Box::new(CompxProtocolDriver),
        Box::new(Compx64ProtocolDriver),
        Box::new(Hidpp20ProtocolDriver),
        Box::new(RazerProtocolDriver),
        Box::new(EndgameGearOp1ProtocolDriver),
        Box::new(FinalmouseProtocolDriver),
        Box::new(VaxeeProtocolDriver),
        Box::new(NinjutsoProtocolDriver),
        Box::new(MchoseProtocolDriver),
        Box::new(IncottProtocolDriver),
        Box::new(KeychronProtocolDriver),
    ];
    let mut devices = Vec::new();
    for driver in &drivers {
        if let Ok(found) = driver.detect(&api) {
            devices.extend(found);
        }
    }
    Ok(devices)
}

/// Lightweight fingerprint of currently attached USB HID VID:PID pairs (zero open device handles),
/// used by the GUI runtime to detect USB mouse plug/unplug events automatically.
pub fn hid_topology_fingerprint() -> Vec<(u16, u16, u16, u16)> {
    let Ok(api) = HidApi::new() else {
        return Vec::new();
    };
    let mut fp: Vec<(u16, u16, u16, u16)> = api
        .device_list()
        .filter(|d| d.usage_page() >= 0xFF00 || (d.usage_page() == 0x01 && d.usage() == 0x02))
        .map(|d| (d.vendor_id(), d.product_id(), d.usage_page(), d.usage()))
        .collect();
    fp.sort_unstable();
    fp.dedup();
    fp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapts_unsupported_pulsar_settings_cleanly_for_lamzu_4k() {
        let lamzu_caps = DeviceCapabilities {
            dpi_min: 50,
            dpi_max: 26000,
            dpi_step: 50,
            supports_xy_dpi: true,
            polling_rates_hz: vec![125, 250, 500, 1000, 2000, 4000],
            lod_options: vec![LiftOffDistance::Mm10, LiftOffDistance::Mm20],
            supports_debounce: true,
            debounce_min_ms: 0,
            debounce_max_ms: 15,
            supports_motion_sync: true,
            supports_angle_snapping: true,
            supports_ripple_control: true,
            supports_sleep_timeout: true,
            sleep_timeout_min_secs: 10,
            sleep_timeout_max_secs: 1800,
            supports_volatile_writes: false,
            flash_write_warning: None,
        };

        let pulsar_profile = MouseSettings {
            dpi: Some(810),
            dpi_y: Some(1010),
            polling_rate_hz: Some(8000),
            lod: Some(LiftOffDistance::Mm07),
            debounce_ms: Some(20),
            motion_sync: Some(true),
            angle_snapping: Some(false),
            ripple_control: Some(false),
            sleep_timeout_secs: Some(300),
        };

        let (adapted, notes) = lamzu_caps.adapt_settings(&pulsar_profile);
        assert_eq!(adapted.dpi, Some(800));
        assert_eq!(adapted.dpi_y, Some(1000));
        assert_eq!(adapted.polling_rate_hz, Some(4000));
        assert_eq!(adapted.lod, Some(LiftOffDistance::Mm10));
        assert_eq!(adapted.debounce_ms, Some(15));
        assert_eq!(adapted.motion_sync, Some(true));
        assert_eq!(adapted.sleep_timeout_secs, Some(300));
        assert_eq!(notes.len(), 5);
        assert!(lamzu_caps.validate("LAMZU 4K", &adapted).is_ok());
    }

    #[test]
    fn skips_unsupported_lod_and_debounce_for_logitech_hidpp20() {
        let logi_caps = DeviceCapabilities {
            dpi_min: 100,
            dpi_max: 25600,
            dpi_step: 50,
            supports_xy_dpi: false,
            polling_rates_hz: vec![125, 250, 500, 1000],
            lod_options: Vec::new(),
            supports_debounce: false,
            debounce_min_ms: 0,
            debounce_max_ms: 0,
            supports_motion_sync: false,
            supports_angle_snapping: false,
            supports_ripple_control: false,
            supports_sleep_timeout: false,
            sleep_timeout_min_secs: 0,
            sleep_timeout_max_secs: 0,
            supports_volatile_writes: true,
            flash_write_warning: None,
        };

        let desired = MouseSettings {
            dpi: Some(1600),
            dpi_y: Some(1800),
            polling_rate_hz: Some(4000),
            lod: Some(LiftOffDistance::Mm07),
            debounce_ms: Some(4),
            motion_sync: Some(true),
            angle_snapping: Some(false),
            ripple_control: Some(false),
            sleep_timeout_secs: Some(300),
        };

        let (adapted, notes) = logi_caps.adapt_settings(&desired);
        assert_eq!(adapted.dpi, Some(1600));
        assert_eq!(adapted.dpi_y, None);
        assert_eq!(adapted.polling_rate_hz, Some(1000));
        assert_eq!(adapted.lod, None);
        assert_eq!(adapted.debounce_ms, None);
        assert_eq!(adapted.motion_sync, None);
        assert_eq!(adapted.angle_snapping, None);
        assert_eq!(adapted.ripple_control, None);
        assert_eq!(adapted.sleep_timeout_secs, None);
        assert_eq!(notes.len(), 8);
        assert!(logi_caps.validate("PRO X SUPERLIGHT", &adapted).is_ok());
    }
}
