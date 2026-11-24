// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(windows)]
pub mod display;
pub mod mock;
#[cfg(windows)]
pub mod os_info;
#[cfg(windows)]
pub mod power;
pub mod processes;
#[cfg(windows)]
pub mod registry;
#[cfg(windows)]
pub mod spi;
#[cfg(windows)]
pub mod timer;
#[cfg(windows)]
pub mod usb_msi;
#[cfg(windows)]
pub mod usb_power;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Supported Windows Registry root hives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RegHive {
    Hkcu,
    Hklm,
}

/// Live parameters for `SPI_GETMOUSE` / `SPI_SETMOUSE` (`[threshold1, threshold2, speed]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseAccelParams {
    pub threshold1: i32,
    pub threshold2: i32,
    pub speed: i32,
}

/// Windows accessibility keyboard/mouse features whose hotkey shortcuts can disrupt games.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccessibilityKind {
    StickyKeys,
    FilterKeys,
    MouseKeys,
}

/// Metadata describing a Windows power plan (scheme).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerSchemeInfo {
    /// Lowercase hyphenated GUID without braces, e.g. `381b4222-f694-41f0-9685-ff5bb260df2e`.
    pub guid: String,
    pub name: String,
}

/// AC and DC numeric values for a Windows power setting (e.g. USB selective suspend).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerValueIndex {
    pub ac: Option<u32>,
    pub dc: Option<u32>,
}

/// Snapshot of a single running process's resource consumption.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessSample {
    pub pid: u32,
    /// Process executable name, e.g. `lghub_agent.exe`.
    pub name: String,
    /// Task-Manager-style total CPU percentage (0.0 ..= 100.0).
    pub cpu_percent: f32,
    /// Resident working set memory in bytes.
    pub memory_bytes: u64,
}

/// Windows OS build number + Update Build Revision (UBR) used to detect Windows Updates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowsVersion {
    pub build: String,
    pub ubr: u32,
}

impl WindowsVersion {
    /// Formats as `<build>.<ubr>`, e.g. `26200.9711`.
    pub fn display_string(&self) -> String {
        format!("{}.{}", self.build, self.ubr)
    }
}

/// Active display resolution and refresh-rate metadata returned by `EnumDisplaySettingsW`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayModeInfo {
    pub device_name: String,
    pub friendly_name: String,
    pub is_primary: bool,
    pub width: u32,
    pub height: u32,
    pub current_hz: u32,
    pub max_supported_hz: u32,
}

/// Statistics from a short non-elevated Windows 1 ms timer wake-jitter micro-test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimerResolutionSample {
    pub requested_ms: u32,
    pub samples: usize,
    pub min_ms: f64,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

/// Distinguishes connected mice/receivers from USB root hubs in USB power inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsbPowerDeviceKind {
    MouseOrDongle,
    RootHub,
}

/// Power-saving state inspected from a USB mouse/dongle or USB Root Hub device node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbPowerDevice {
    pub name: String,
    pub kind: UsbPowerDeviceKind,
    pub power_saving_enabled: bool,
    pub detail: String,
}

/// PCI xHCI USB 3.x host controller interrupt delivery state (MSI vs legacy line IRQ).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbControllerInfo {
    pub name: String,
    /// `Some(true)` = MSI enabled (`MSISupported=1`), `Some(false)` = legacy IRQ, `None` = unset.
    pub msi_supported: Option<bool>,
}

/// Abstraction over all Windows system APIs used by Polled.
///
/// Production code passes `WindowsEnv` (real `windows-rs` + `sysinfo` calls);
/// unit tests pass `mock::MockEnv` so tests are deterministic and never mutate your OS.
pub trait SystemEnv: Send + Sync {
    // --- Registry ---
    fn reg_get_string(&self, hive: RegHive, subkey: &str, name: &str) -> Result<Option<String>>;
    fn reg_set_string(&self, hive: RegHive, subkey: &str, name: &str, value: &str) -> Result<()>;

    fn reg_get_dword(&self, hive: RegHive, subkey: &str, name: &str) -> Result<Option<u32>>;
    fn reg_set_dword(&self, hive: RegHive, subkey: &str, name: &str, value: u32) -> Result<()>;

    fn reg_get_binary(&self, hive: RegHive, subkey: &str, name: &str) -> Result<Option<Vec<u8>>>;
    fn reg_set_binary(&self, hive: RegHive, subkey: &str, name: &str, value: &[u8]) -> Result<()>;

    fn reg_delete_value(&self, hive: RegHive, subkey: &str, name: &str) -> Result<()>;

    // --- Live SystemParametersInfoW ---
    fn get_mouse_accel(&self) -> Result<MouseAccelParams>;
    fn set_mouse_accel(&self, params: MouseAccelParams) -> Result<()>;

    fn get_mouse_speed(&self) -> Result<u32>;
    fn set_mouse_speed(&self, speed: u32) -> Result<()>;

    fn get_accessibility_flags(&self, kind: AccessibilityKind) -> Result<u32>;
    fn set_accessibility_flags(&self, kind: AccessibilityKind, flags: u32) -> Result<()>;

    // --- Power API (powrprof.dll) ---
    fn get_active_power_scheme(&self) -> Result<PowerSchemeInfo>;
    fn list_power_schemes(&self) -> Result<Vec<PowerSchemeInfo>>;
    fn set_active_power_scheme(&self, scheme_guid: &str) -> Result<()>;

    fn get_power_setting_index(
        &self,
        scheme_guid: &str,
        subgroup_guid: &str,
        setting_guid: &str,
    ) -> Result<PowerValueIndex>;

    fn set_power_setting_index(
        &self,
        scheme_guid: &str,
        subgroup_guid: &str,
        setting_guid: &str,
        values: PowerValueIndex,
    ) -> Result<()>;

    // --- Process, display, timer & USB power inspection ---
    fn list_processes(&self) -> Result<Vec<ProcessSample>>;
    fn windows_version(&self) -> Result<WindowsVersion>;
    fn list_display_modes(&self) -> Result<Vec<DisplayModeInfo>>;
    fn measure_timer_resolution(&self) -> Result<TimerResolutionSample>;
    fn list_usb_power_devices(&self) -> Result<Vec<UsbPowerDevice>>;
    fn list_usb_controllers(&self) -> Result<Vec<UsbControllerInfo>>;
}

/// Real Windows implementation backed by `windows-rs` and `sysinfo`.
#[cfg(windows)]
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsEnv;

#[cfg(windows)]
impl SystemEnv for WindowsEnv {
    fn reg_get_string(&self, hive: RegHive, subkey: &str, name: &str) -> Result<Option<String>> {
        registry::get_string(hive, subkey, name)
    }

    fn reg_set_string(&self, hive: RegHive, subkey: &str, name: &str, value: &str) -> Result<()> {
        registry::set_string(hive, subkey, name, value)
    }

    fn reg_get_dword(&self, hive: RegHive, subkey: &str, name: &str) -> Result<Option<u32>> {
        registry::get_dword(hive, subkey, name)
    }

    fn reg_set_dword(&self, hive: RegHive, subkey: &str, name: &str, value: u32) -> Result<()> {
        registry::set_dword(hive, subkey, name, value)
    }

    fn reg_get_binary(&self, hive: RegHive, subkey: &str, name: &str) -> Result<Option<Vec<u8>>> {
        registry::get_binary(hive, subkey, name)
    }

    fn reg_set_binary(&self, hive: RegHive, subkey: &str, name: &str, value: &[u8]) -> Result<()> {
        registry::set_binary(hive, subkey, name, value)
    }

    fn reg_delete_value(&self, hive: RegHive, subkey: &str, name: &str) -> Result<()> {
        registry::delete_value(hive, subkey, name)
    }

    fn get_mouse_accel(&self) -> Result<MouseAccelParams> {
        spi::get_mouse_accel()
    }

    fn set_mouse_accel(&self, params: MouseAccelParams) -> Result<()> {
        spi::set_mouse_accel(params)
    }

    fn get_mouse_speed(&self) -> Result<u32> {
        spi::get_mouse_speed()
    }

    fn set_mouse_speed(&self, speed: u32) -> Result<()> {
        spi::set_mouse_speed(speed)
    }

    fn get_accessibility_flags(&self, kind: AccessibilityKind) -> Result<u32> {
        spi::get_accessibility_flags(kind)
    }

    fn set_accessibility_flags(&self, kind: AccessibilityKind, flags: u32) -> Result<()> {
        spi::set_accessibility_flags(kind, flags)
    }

    fn get_active_power_scheme(&self) -> Result<PowerSchemeInfo> {
        power::get_active_power_scheme()
    }

    fn list_power_schemes(&self) -> Result<Vec<PowerSchemeInfo>> {
        power::list_power_schemes()
    }

    fn set_active_power_scheme(&self, scheme_guid: &str) -> Result<()> {
        power::set_active_power_scheme(scheme_guid)
    }

    fn get_power_setting_index(
        &self,
        scheme_guid: &str,
        subgroup_guid: &str,
        setting_guid: &str,
    ) -> Result<PowerValueIndex> {
        power::get_power_setting_index(scheme_guid, subgroup_guid, setting_guid)
    }

    fn set_power_setting_index(
        &self,
        scheme_guid: &str,
        subgroup_guid: &str,
        setting_guid: &str,
        values: PowerValueIndex,
    ) -> Result<()> {
        power::set_power_setting_index(scheme_guid, subgroup_guid, setting_guid, values)
    }

    fn list_processes(&self) -> Result<Vec<ProcessSample>> {
        processes::sample_processes()
    }

    fn windows_version(&self) -> Result<WindowsVersion> {
        os_info::read_windows_version()
    }

    fn list_display_modes(&self) -> Result<Vec<DisplayModeInfo>> {
        display::list_display_modes()
    }

    fn measure_timer_resolution(&self) -> Result<TimerResolutionSample> {
        timer::measure_timer_resolution()
    }

    fn list_usb_power_devices(&self) -> Result<Vec<UsbPowerDevice>> {
        usb_power::list_usb_power_devices()
    }

    fn list_usb_controllers(&self) -> Result<Vec<UsbControllerInfo>> {
        usb_msi::list_usb_controllers()
    }
}
