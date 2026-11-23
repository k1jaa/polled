// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod controller;
pub mod dry_run;
pub mod pulsar;

use std::fmt;

use hidapi::HidApi;
use serde::{Deserialize, Serialize};

use crate::error::{PolledError, Result};

pub use controller::{ApplyOutcome, SafeDeviceController};
pub use dry_run::DryRunDevice;
pub use pulsar::{PulsarX2CrazyLightDevice, PulsarX2CrazyLightDriver};

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
}

impl fmt::Display for LiftOffDistance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Hardware mouse settings managed by a profile.
///
/// Any `None` field means "leave unchanged" when applying a profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dpi: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub polling_rate_hz: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lod: Option<LiftOffDistance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debounce_ms: Option<u8>,
}

impl MouseSettings {
    pub fn is_empty(&self) -> bool {
        self.dpi.is_none()
            && self.polling_rate_hz.is_none()
            && self.lod.is_none()
            && self.debounce_ms.is_none()
    }

    /// Returns `true` if every `Some(...)` field in `desired` matches `self`.
    pub fn matches_desired(&self, desired: &MouseSettings) -> bool {
        if let Some(dpi) = desired.dpi {
            if self.dpi != Some(dpi) {
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
        true
    }

    /// Produces a merged settings struct where `Some(...)` fields from `desired` override `self`.
    pub fn merged_with(&self, desired: &MouseSettings) -> MouseSettings {
        MouseSettings {
            dpi: desired.dpi.or(self.dpi),
            polling_rate_hz: desired.polling_rate_hz.or(self.polling_rate_hz),
            lod: desired.lod.or(self.lod),
            debounce_ms: desired.debounce_ms.or(self.debounce_ms),
        }
    }

    pub fn summary_string(&self) -> String {
        let mut parts = Vec::new();
        if let Some(dpi) = self.dpi {
            parts.push(format!("{dpi} DPI"));
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
    pub polling_rates_hz: Vec<u16>,
    pub lod_options: Vec<LiftOffDistance>,
    pub debounce_min_ms: u8,
    pub debounce_max_ms: u8,
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

        Ok(())
    }
}

/// Core hardware abstraction implemented by every mouse backend (and `DryRunDevice`).
pub trait MouseDevice: Send {
    fn info(&self) -> DeviceInfo;
    fn capabilities(&self) -> DeviceCapabilities;
    fn read_settings(&mut self) -> Result<MouseSettings>;
    fn apply_settings(&mut self, settings: &MouseSettings, mode: WriteMode) -> Result<()>;
}

/// Discovery factory that scans connected HID interfaces for devices it supports.
pub trait DeviceDriver: Send + Sync {
    fn driver_name(&self) -> &'static str;
    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>>;
}

/// Runs all registered hardware drivers against connected USB/HID devices.
pub fn detect_hardware_devices() -> Result<Vec<Box<dyn MouseDevice>>> {
    let api = HidApi::new().map_err(|e| PolledError::Win32 {
        context: "HidApi::new".to_string(),
        message: e.to_string(),
    })?;
    let drivers: Vec<Box<dyn DeviceDriver>> = vec![Box::new(PulsarX2CrazyLightDriver)];
    let mut devices = Vec::new();
    for driver in &drivers {
        if let Ok(found) = driver.detect(&api) {
            devices.extend(found);
        }
    }
    Ok(devices)
}
