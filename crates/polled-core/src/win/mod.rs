// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(windows)]
pub mod audio;
#[cfg(windows)]
pub mod display;
pub mod mock;
#[cfg(windows)]
pub mod network;
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
#[cfg(windows)]
pub mod usb_topology;

use serde::{Deserialize, Serialize};

use crate::checks::Severity;
use crate::error::Result;

#[cfg(windows)]
pub use audio::{duck_app_audio_sessions, restore_app_audio_sessions, SavedAppAudioVolume};

#[cfg(not(windows))]
#[derive(Debug, Clone, PartialEq)]
pub struct SavedAppAudioVolume {
    pub exe_name: String,
    pub pid: u32,
    pub original_volume: f32,
    pub original_muted: bool,
}

#[cfg(not(windows))]
pub fn duck_app_audio_sessions(
    _target_exes: &[String],
    _target_volume_pct: u8,
) -> Result<Vec<SavedAppAudioVolume>> {
    Ok(Vec::new())
}

#[cfg(not(windows))]
pub fn restore_app_audio_sessions(_saved: &[SavedAppAudioVolume]) -> Result<usize> {
    Ok(0)
}

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

/// Single supported `(width, height, refresh_hz)` mode exposed by a monitor driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DisplayResolutionMode {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
}

impl DisplayResolutionMode {
    pub fn label(self) -> String {
        format!("{}×{} @ {} Hz", self.width, self.height, self.refresh_hz)
    }

    pub fn aspect_tag(self) -> &'static str {
        let w = self.width;
        let h = self.height;
        if w * 9 == h * 16 {
            "16:9"
        } else if w * 3 == h * 4 {
            "4:3"
        } else if w * 4 == h * 5 {
            "5:4"
        } else if w * 10 == h * 16 {
            "16:10"
        } else if w * 9 == h * 21 || w * 1440 == h * 3440 {
            "21:9"
        } else {
            ""
        }
    }
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

/// Device category audited by the GPU & Audio PCI MSI check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PciMsiCategory {
    Gpu,
    Audio,
}

impl PciMsiCategory {
    pub fn label(self) -> &'static str {
        match self {
            PciMsiCategory::Gpu => "GPU",
            PciMsiCategory::Audio => "Audio",
        }
    }
}

/// PCI GPU or HD Audio controller interrupt delivery state (MSI vs legacy line-based IRQ).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PciMsiDevice {
    pub name: String,
    pub category: PciMsiCategory,
    /// `Some(true)` = MSI enabled (`MSISupported=1`), `Some(false)` = legacy IRQ, `None` = unset.
    pub msi_supported: Option<bool>,
    /// HKLM relative subkey path to `MessageSignaledInterruptProperties` for safe reversible toggling.
    #[serde(default)]
    pub msi_reg_path: String,
}

/// High-bandwidth USB device class that can contend with 4K/8K mouse polling on a shared hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsbSharedDeviceKind {
    CameraOrCapture,
    UsbAudio,
    UsbStorage,
}

impl UsbSharedDeviceKind {
    pub fn label(self) -> &'static str {
        match self {
            UsbSharedDeviceKind::CameraOrCapture => "Webcam / Capture",
            UsbSharedDeviceKind::UsbAudio => "USB Audio",
            UsbSharedDeviceKind::UsbStorage => "USB Storage",
        }
    }
}

/// High-bandwidth USB device sharing the same hub as a connected mouse or receiver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbSharedDevice {
    pub name: String,
    pub kind: UsbSharedDeviceKind,
}

/// Active USB hub & port placement for one connected HID mouse or wireless dongle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbMouseTopology {
    pub name: String,
    pub vid_pid: String,
    pub port_number: Option<u32>,
    pub hub_number: Option<u32>,
    pub hub_name: String,
    pub behind_external_hub: bool,
    pub shared_heavy_devices: Vec<UsbSharedDevice>,
}

/// Direction of a Windows MMDevice audio endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFlow {
    Render,
    Capture,
}

/// Read-only snapshot of an active Windows audio playback or capture endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioEndpointInfo {
    pub id: String,
    pub guid: String,
    pub name: String,
    pub flow: AudioFlow,
    pub is_default: bool,
    pub volume_percent: Option<u8>,
    pub muted: Option<bool>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u16>,
    pub bits_per_sample: Option<u16>,
    pub default_period_ms: Option<f64>,
    pub min_period_ms: Option<f64>,
    pub enhancements_enabled: Option<bool>,
    pub is_bluetooth_hands_free: bool,
}

/// Result of an on-demand 1-second silent WASAPI audio engine latency & wake-jitter benchmark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioLatencyBenchmark {
    pub device_name: String,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub default_period_ms: f64,
    pub min_period_ms: f64,
    pub active_buffer_ms: f64,
    pub elapsed_ms: f64,
    pub samples_ms: Vec<f64>,
    pub min_ms: f64,
    pub median_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub glitch_count: usize,
    pub severity: Severity,
}

/// Physical link medium of an active Windows network adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAdapterKind {
    Ethernet,
    Wifi,
    Other,
}

impl NetworkAdapterKind {
    pub fn label(self) -> &'static str {
        match self {
            NetworkAdapterKind::Ethernet => "Ethernet",
            NetworkAdapterKind::Wifi => "Wi-Fi",
            NetworkAdapterKind::Other => "Other",
        }
    }
}

/// Windows keyboard repeat delay (`0..=3`, 0 ≈ 250 ms) and repeat rate (`0..=31`, 31 ≈ 30 Hz).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyboardRepeatParams {
    pub delay: u32,
    pub speed: u32,
}

/// Read-only snapshot of an active Windows network adapter, gateway, DNS, and NIC latency flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkAdapterInfo {
    pub name: String,
    pub description: String,
    pub adapter_guid: String,
    pub kind: NetworkAdapterKind,
    pub is_primary: bool,
    pub link_speed_mbps: u64,
    pub ipv4_address: Option<String>,
    pub gateway_ipv4: Option<String>,
    pub dns_servers: Vec<String>,
    pub latency_flags: Vec<String>,
    pub nagle_disabled: bool,
}

/// Single-hop ICMP ping latency, jitter, and packet-loss metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PingHopStats {
    pub label: String,
    pub target_ip: String,
    pub sent: usize,
    pub received: usize,
    pub lost: usize,
    pub loss_pct: f64,
    pub samples_ms: Vec<Option<f64>>,
    pub min_ms: Option<f64>,
    pub median_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
    pub severity: Severity,
}

/// Result of an on-demand dual-hop ICMP ping benchmark separating local LAN/Wi-Fi vs ISP jitter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkJitterBenchmark {
    pub adapter_name: String,
    pub adapter_kind: NetworkAdapterKind,
    pub elapsed_ms: f64,
    pub gateway: Option<PingHopStats>,
    pub internet: PingHopStats,
    pub diagnosis: String,
    pub severity: Severity,
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

    fn get_keyboard_repeat(&self) -> Result<KeyboardRepeatParams>;
    fn set_keyboard_repeat(&self, params: KeyboardRepeatParams) -> Result<()>;

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
    fn list_supported_display_modes(&self, device_name: &str)
        -> Result<Vec<DisplayResolutionMode>>;
    fn set_display_mode(
        &self,
        device_name: &str,
        width: u32,
        height: u32,
        refresh_hz: u32,
    ) -> Result<()>;
    fn measure_timer_resolution(&self) -> Result<TimerResolutionSample>;
    fn list_usb_power_devices(&self) -> Result<Vec<UsbPowerDevice>>;
    fn list_usb_controllers(&self) -> Result<Vec<UsbControllerInfo>>;
    fn list_pci_msi_devices(&self) -> Result<Vec<PciMsiDevice>>;
    fn list_usb_topology(&self) -> Result<Vec<UsbMouseTopology>>;
    fn list_audio_endpoints(&self, flow: AudioFlow) -> Result<Vec<AudioEndpointInfo>>;
    fn set_default_audio_endpoint(&self, endpoint_id: &str) -> Result<()>;
    fn get_audio_endpoint_volume(&self, flow: AudioFlow, endpoint_id: Option<&str>) -> Result<u8>;
    fn set_audio_endpoint_volume(
        &self,
        flow: AudioFlow,
        endpoint_id: Option<&str>,
        percent: u8,
    ) -> Result<()>;
    fn get_audio_endpoint_mute(&self, flow: AudioFlow, endpoint_id: Option<&str>) -> Result<bool>;
    fn set_audio_endpoint_mute(
        &self,
        flow: AudioFlow,
        endpoint_id: Option<&str>,
        muted: bool,
    ) -> Result<()>;
    fn toggle_audio_endpoint_mute(
        &self,
        flow: AudioFlow,
        endpoint_id: Option<&str>,
    ) -> Result<(String, bool)>;
    fn list_network_adapters(&self) -> Result<Vec<NetworkAdapterInfo>>;
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

    fn get_keyboard_repeat(&self) -> Result<KeyboardRepeatParams> {
        spi::get_keyboard_repeat()
    }

    fn set_keyboard_repeat(&self, params: KeyboardRepeatParams) -> Result<()> {
        spi::set_keyboard_repeat(params)
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

    fn list_supported_display_modes(
        &self,
        device_name: &str,
    ) -> Result<Vec<DisplayResolutionMode>> {
        display::list_supported_display_modes(device_name)
    }

    fn set_display_mode(
        &self,
        device_name: &str,
        width: u32,
        height: u32,
        refresh_hz: u32,
    ) -> Result<()> {
        display::set_display_mode(device_name, width, height, refresh_hz)
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

    fn list_pci_msi_devices(&self) -> Result<Vec<PciMsiDevice>> {
        usb_msi::list_pci_msi_devices()
    }

    fn list_usb_topology(&self) -> Result<Vec<UsbMouseTopology>> {
        usb_topology::list_usb_topology()
    }

    fn list_audio_endpoints(&self, flow: AudioFlow) -> Result<Vec<AudioEndpointInfo>> {
        audio::list_audio_endpoints(flow)
    }

    fn set_default_audio_endpoint(&self, endpoint_id: &str) -> Result<()> {
        audio::set_default_audio_endpoint(endpoint_id)
    }

    fn get_audio_endpoint_volume(&self, flow: AudioFlow, endpoint_id: Option<&str>) -> Result<u8> {
        audio::get_audio_endpoint_volume(flow, endpoint_id)
    }

    fn set_audio_endpoint_volume(
        &self,
        flow: AudioFlow,
        endpoint_id: Option<&str>,
        percent: u8,
    ) -> Result<()> {
        audio::set_audio_endpoint_volume(flow, endpoint_id, percent)
    }

    fn get_audio_endpoint_mute(&self, flow: AudioFlow, endpoint_id: Option<&str>) -> Result<bool> {
        audio::get_audio_endpoint_mute(flow, endpoint_id)
    }

    fn set_audio_endpoint_mute(
        &self,
        flow: AudioFlow,
        endpoint_id: Option<&str>,
        muted: bool,
    ) -> Result<()> {
        audio::set_audio_endpoint_mute(flow, endpoint_id, muted)
    }

    fn toggle_audio_endpoint_mute(
        &self,
        flow: AudioFlow,
        endpoint_id: Option<&str>,
    ) -> Result<(String, bool)> {
        audio::toggle_audio_endpoint_mute(flow, endpoint_id)
    }

    fn list_network_adapters(&self) -> Result<Vec<NetworkAdapterInfo>> {
        network::list_network_adapters()
    }
}
