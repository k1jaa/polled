// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::{PolledError, Result};
use crate::win::{
    AccessibilityKind, AudioEndpointInfo, AudioFlow, DisplayModeInfo, DisplayResolutionMode,
    KeyboardRepeatParams, MouseAccelParams, NetworkAdapterInfo, NetworkAdapterKind, PciMsiCategory,
    PciMsiDevice, PowerSchemeInfo, PowerValueIndex, ProcessSample, RegHive, SystemEnv,
    TimerResolutionSample, UsbControllerInfo, UsbMouseTopology, UsbPowerDevice, WindowsVersion,
};

#[derive(Debug, Clone)]
enum MockRegValue {
    String(String),
    Dword(u32),
    Binary(Vec<u8>),
}

#[derive(Debug, Clone)]
struct MockState {
    registry: HashMap<(RegHive, String, String), MockRegValue>,
    mouse_accel: MouseAccelParams,
    mouse_speed: u32,
    keyboard_repeat: KeyboardRepeatParams,
    accessibility_flags: HashMap<AccessibilityKind, u32>,
    active_power_scheme: String,
    power_schemes: Vec<PowerSchemeInfo>,
    power_settings: HashMap<(String, String, String), PowerValueIndex>,
    processes: Vec<ProcessSample>,
    windows_version: WindowsVersion,
    displays: Vec<DisplayModeInfo>,
    supported_display_modes: Vec<DisplayResolutionMode>,
    timer_sample: TimerResolutionSample,
    usb_power_devices: Vec<UsbPowerDevice>,
    usb_controllers: Vec<UsbControllerInfo>,
    pci_msi_devices: Vec<PciMsiDevice>,
    usb_topology: Vec<UsbMouseTopology>,
    audio_endpoints: Vec<AudioEndpointInfo>,
    network_adapters: Vec<NetworkAdapterInfo>,
    require_elevation_for_power_writes: bool,
}

impl Default for MockState {
    fn default() -> Self {
        let balanced = "381b4222-f694-41f0-9685-ff5bb260df2e".to_string();
        let high_perf = "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c".to_string();

        let mut accessibility_flags = HashMap::new();
        accessibility_flags.insert(AccessibilityKind::StickyKeys, 0x01FE);
        accessibility_flags.insert(AccessibilityKind::FilterKeys, 0x007E);
        accessibility_flags.insert(AccessibilityKind::MouseKeys, 0x003E);

        Self {
            registry: HashMap::new(),
            mouse_accel: MouseAccelParams {
                threshold1: 0,
                threshold2: 0,
                speed: 0,
            },
            mouse_speed: 10,
            keyboard_repeat: KeyboardRepeatParams {
                delay: 1,
                speed: 31,
            },
            accessibility_flags,
            active_power_scheme: balanced.clone(),
            power_schemes: vec![
                PowerSchemeInfo {
                    guid: balanced,
                    name: "Balanced".to_string(),
                },
                PowerSchemeInfo {
                    guid: high_perf,
                    name: "High performance".to_string(),
                },
            ],
            power_settings: HashMap::new(),
            processes: Vec::new(),
            windows_version: WindowsVersion {
                build: "26200".to_string(),
                ubr: 1000,
            },
            displays: vec![DisplayModeInfo {
                device_name: r"\\.\DISPLAY1".to_string(),
                friendly_name: "Primary Monitor".to_string(),
                is_primary: true,
                width: 2560,
                height: 1440,
                current_hz: 240,
                max_supported_hz: 240,
            }],
            supported_display_modes: vec![
                DisplayResolutionMode {
                    width: 2560,
                    height: 1440,
                    refresh_hz: 240,
                },
                DisplayResolutionMode {
                    width: 1920,
                    height: 1080,
                    refresh_hz: 240,
                },
                DisplayResolutionMode {
                    width: 1280,
                    height: 960,
                    refresh_hz: 240,
                },
                DisplayResolutionMode {
                    width: 2560,
                    height: 1440,
                    refresh_hz: 60,
                },
            ],
            timer_sample: TimerResolutionSample {
                requested_ms: 1,
                samples: 48,
                min_ms: 1.01,
                median_ms: 1.50,
                p95_ms: 1.98,
                max_ms: 2.10,
            },
            usb_power_devices: Vec::new(),
            usb_controllers: vec![UsbControllerInfo {
                name: "USB xHCI Compliant Host Controller".to_string(),
                msi_supported: Some(true),
            }],
            pci_msi_devices: vec![
                PciMsiDevice {
                    name: "Mock GPU RTX".to_string(),
                    category: PciMsiCategory::Gpu,
                    msi_supported: Some(true),
                },
                PciMsiDevice {
                    name: "High Definition Audio Controller".to_string(),
                    category: PciMsiCategory::Audio,
                    msi_supported: Some(true),
                },
            ],
            usb_topology: Vec::new(),
            audio_endpoints: vec![
                AudioEndpointInfo {
                    id: "{0.0.0.00000000}.{mock-render-1}".to_string(),
                    guid: "{mock-render-1}".to_string(),
                    name: "Headphones (USB DAC)".to_string(),
                    flow: AudioFlow::Render,
                    is_default: true,
                    volume_percent: Some(60),
                    muted: Some(false),
                    sample_rate_hz: Some(48_000),
                    channels: Some(2),
                    bits_per_sample: Some(32),
                    default_period_ms: Some(10.0),
                    min_period_ms: Some(2.67),
                    enhancements_enabled: Some(false),
                    is_bluetooth_hands_free: false,
                },
                AudioEndpointInfo {
                    id: "{0.0.1.00000000}.{mock-capture-1}".to_string(),
                    guid: "{mock-capture-1}".to_string(),
                    name: "Microphone (USB Mic)".to_string(),
                    flow: AudioFlow::Capture,
                    is_default: true,
                    volume_percent: Some(80),
                    muted: Some(false),
                    sample_rate_hz: Some(48_000),
                    channels: Some(1),
                    bits_per_sample: Some(32),
                    default_period_ms: Some(10.0),
                    min_period_ms: Some(2.67),
                    enhancements_enabled: Some(false),
                    is_bluetooth_hands_free: false,
                },
            ],
            network_adapters: vec![NetworkAdapterInfo {
                name: "Ethernet".to_string(),
                description: "Realtek PCIe 2.5GbE Family Controller".to_string(),
                adapter_guid: "{mock-nic-1}".to_string(),
                kind: NetworkAdapterKind::Ethernet,
                is_primary: true,
                link_speed_mbps: 1000,
                ipv4_address: Some("192.168.1.42".to_string()),
                gateway_ipv4: Some("192.168.1.1".to_string()),
                dns_servers: vec!["1.1.1.1".to_string(), "1.0.0.1".to_string()],
                latency_flags: Vec::new(),
                nagle_disabled: false,
            }],
            require_elevation_for_power_writes: false,
        }
    }
}

/// In-memory `SystemEnv` double for deterministic unit tests without touching Windows.
#[derive(Debug, Default)]
pub struct MockEnv {
    state: Mutex<MockState>,
}

impl MockEnv {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(hive: RegHive, subkey: &str, name: &str) -> (RegHive, String, String) {
        (hive, subkey.to_ascii_lowercase(), name.to_ascii_lowercase())
    }

    pub fn set_windows_version(&self, build: impl Into<String>, ubr: u32) {
        let mut s = self.state.lock().unwrap();
        s.windows_version = WindowsVersion {
            build: build.into(),
            ubr,
        };
    }

    pub fn set_power_schemes(&self, active_guid: &str, schemes: Vec<PowerSchemeInfo>) {
        let mut s = self.state.lock().unwrap();
        s.active_power_scheme = active_guid.to_ascii_lowercase();
        s.power_schemes = schemes;
    }

    pub fn set_processes(&self, processes: Vec<ProcessSample>) {
        self.state.lock().unwrap().processes = processes;
    }

    pub fn set_displays(&self, displays: Vec<DisplayModeInfo>) {
        self.state.lock().unwrap().displays = displays;
    }

    pub fn set_timer_sample(&self, sample: TimerResolutionSample) {
        self.state.lock().unwrap().timer_sample = sample;
    }

    pub fn set_usb_power_devices(&self, devices: Vec<UsbPowerDevice>) {
        self.state.lock().unwrap().usb_power_devices = devices;
    }

    pub fn set_usb_controllers(&self, controllers: Vec<UsbControllerInfo>) {
        self.state.lock().unwrap().usb_controllers = controllers;
    }

    pub fn set_pci_msi_devices(&self, devices: Vec<PciMsiDevice>) {
        self.state.lock().unwrap().pci_msi_devices = devices;
    }

    pub fn set_supported_display_modes(&self, modes: Vec<DisplayResolutionMode>) {
        self.state.lock().unwrap().supported_display_modes = modes;
    }

    pub fn set_usb_topology(&self, topology: Vec<UsbMouseTopology>) {
        self.state.lock().unwrap().usb_topology = topology;
    }

    pub fn set_audio_endpoints(&self, endpoints: Vec<AudioEndpointInfo>) {
        self.state.lock().unwrap().audio_endpoints = endpoints;
    }

    pub fn set_network_adapters(&self, adapters: Vec<NetworkAdapterInfo>) {
        self.state.lock().unwrap().network_adapters = adapters;
    }

    pub fn set_require_elevation_for_power_writes(&self, required: bool) {
        self.state
            .lock()
            .unwrap()
            .require_elevation_for_power_writes = required;
    }
}

impl SystemEnv for MockEnv {
    fn reg_get_string(&self, hive: RegHive, subkey: &str, name: &str) -> Result<Option<String>> {
        let s = self.state.lock().unwrap();
        match s.registry.get(&Self::key(hive, subkey, name)) {
            Some(MockRegValue::String(v)) => Ok(Some(v.clone())),
            Some(_) => Err(PolledError::Win32 {
                context: "MockEnv::reg_get_string".to_string(),
                message: "Type mismatch".to_string(),
            }),
            None => Ok(None),
        }
    }

    fn reg_set_string(&self, hive: RegHive, subkey: &str, name: &str, value: &str) -> Result<()> {
        self.state.lock().unwrap().registry.insert(
            Self::key(hive, subkey, name),
            MockRegValue::String(value.to_string()),
        );
        Ok(())
    }

    fn reg_get_dword(&self, hive: RegHive, subkey: &str, name: &str) -> Result<Option<u32>> {
        let s = self.state.lock().unwrap();
        match s.registry.get(&Self::key(hive, subkey, name)) {
            Some(MockRegValue::Dword(v)) => Ok(Some(*v)),
            Some(_) => Err(PolledError::Win32 {
                context: "MockEnv::reg_get_dword".to_string(),
                message: "Type mismatch".to_string(),
            }),
            None => Ok(None),
        }
    }

    fn reg_set_dword(&self, hive: RegHive, subkey: &str, name: &str, value: u32) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .registry
            .insert(Self::key(hive, subkey, name), MockRegValue::Dword(value));
        Ok(())
    }

    fn reg_get_binary(&self, hive: RegHive, subkey: &str, name: &str) -> Result<Option<Vec<u8>>> {
        let s = self.state.lock().unwrap();
        match s.registry.get(&Self::key(hive, subkey, name)) {
            Some(MockRegValue::Binary(v)) => Ok(Some(v.clone())),
            Some(_) => Err(PolledError::Win32 {
                context: "MockEnv::reg_get_binary".to_string(),
                message: "Type mismatch".to_string(),
            }),
            None => Ok(None),
        }
    }

    fn reg_set_binary(&self, hive: RegHive, subkey: &str, name: &str, value: &[u8]) -> Result<()> {
        self.state.lock().unwrap().registry.insert(
            Self::key(hive, subkey, name),
            MockRegValue::Binary(value.to_vec()),
        );
        Ok(())
    }

    fn reg_delete_value(&self, hive: RegHive, subkey: &str, name: &str) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .registry
            .remove(&Self::key(hive, subkey, name));
        Ok(())
    }

    fn get_mouse_accel(&self) -> Result<MouseAccelParams> {
        Ok(self.state.lock().unwrap().mouse_accel)
    }

    fn set_mouse_accel(&self, params: MouseAccelParams) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.mouse_accel = params;
        // Mirror SPIF_UPDATEINIFILE side effect into HKCU\Control Panel\Mouse.
        s.registry.insert(
            Self::key(RegHive::Hkcu, r"Control Panel\Mouse", "MouseThreshold1"),
            MockRegValue::String(params.threshold1.to_string()),
        );
        s.registry.insert(
            Self::key(RegHive::Hkcu, r"Control Panel\Mouse", "MouseThreshold2"),
            MockRegValue::String(params.threshold2.to_string()),
        );
        s.registry.insert(
            Self::key(RegHive::Hkcu, r"Control Panel\Mouse", "MouseSpeed"),
            MockRegValue::String(params.speed.to_string()),
        );
        Ok(())
    }

    fn get_mouse_speed(&self) -> Result<u32> {
        Ok(self.state.lock().unwrap().mouse_speed)
    }

    fn set_mouse_speed(&self, speed: u32) -> Result<()> {
        let clamped = speed.clamp(1, 20);
        let mut s = self.state.lock().unwrap();
        s.mouse_speed = clamped;
        s.registry.insert(
            Self::key(RegHive::Hkcu, r"Control Panel\Mouse", "MouseSensitivity"),
            MockRegValue::String(clamped.to_string()),
        );
        Ok(())
    }

    fn get_keyboard_repeat(&self) -> Result<KeyboardRepeatParams> {
        Ok(self.state.lock().unwrap().keyboard_repeat)
    }

    fn set_keyboard_repeat(&self, params: KeyboardRepeatParams) -> Result<()> {
        self.state.lock().unwrap().keyboard_repeat = KeyboardRepeatParams {
            delay: params.delay.min(3),
            speed: params.speed.min(31),
        };
        Ok(())
    }

    fn get_accessibility_flags(&self, kind: AccessibilityKind) -> Result<u32> {
        Ok(*self
            .state
            .lock()
            .unwrap()
            .accessibility_flags
            .get(&kind)
            .unwrap_or(&0))
    }

    fn set_accessibility_flags(&self, kind: AccessibilityKind, flags: u32) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .accessibility_flags
            .insert(kind, flags);
        Ok(())
    }

    fn get_active_power_scheme(&self) -> Result<PowerSchemeInfo> {
        let s = self.state.lock().unwrap();
        s.power_schemes
            .iter()
            .find(|p| p.guid.eq_ignore_ascii_case(&s.active_power_scheme))
            .cloned()
            .ok_or_else(|| PolledError::InvalidGuid(s.active_power_scheme.clone()))
    }

    fn list_power_schemes(&self) -> Result<Vec<PowerSchemeInfo>> {
        Ok(self.state.lock().unwrap().power_schemes.clone())
    }

    fn set_active_power_scheme(&self, scheme_guid: &str) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        if s.require_elevation_for_power_writes {
            return Err(PolledError::ElevationRequired {
                operation: "PowerSetActiveScheme".to_string(),
                reason: "Simulated elevation requirement".to_string(),
            });
        }
        let target = scheme_guid.to_ascii_lowercase();
        if !s
            .power_schemes
            .iter()
            .any(|p| p.guid.eq_ignore_ascii_case(&target))
        {
            return Err(PolledError::InvalidGuid(scheme_guid.to_string()));
        }
        s.active_power_scheme = target;
        Ok(())
    }

    fn get_power_setting_index(
        &self,
        scheme_guid: &str,
        subgroup_guid: &str,
        setting_guid: &str,
    ) -> Result<PowerValueIndex> {
        let s = self.state.lock().unwrap();
        Ok(s.power_settings
            .get(&(
                scheme_guid.to_ascii_lowercase(),
                subgroup_guid.to_ascii_lowercase(),
                setting_guid.to_ascii_lowercase(),
            ))
            .copied()
            .unwrap_or(PowerValueIndex { ac: None, dc: None }))
    }

    fn set_power_setting_index(
        &self,
        scheme_guid: &str,
        subgroup_guid: &str,
        setting_guid: &str,
        values: PowerValueIndex,
    ) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        if s.require_elevation_for_power_writes {
            return Err(PolledError::ElevationRequired {
                operation: "PowerWriteACValueIndex".to_string(),
                reason: "Simulated elevation requirement".to_string(),
            });
        }
        let key = (
            scheme_guid.to_ascii_lowercase(),
            subgroup_guid.to_ascii_lowercase(),
            setting_guid.to_ascii_lowercase(),
        );
        let entry = s
            .power_settings
            .entry(key)
            .or_insert(PowerValueIndex { ac: None, dc: None });
        if let Some(ac) = values.ac {
            entry.ac = Some(ac);
        }
        if let Some(dc) = values.dc {
            entry.dc = Some(dc);
        }
        Ok(())
    }

    fn list_processes(&self) -> Result<Vec<ProcessSample>> {
        Ok(self.state.lock().unwrap().processes.clone())
    }

    fn windows_version(&self) -> Result<WindowsVersion> {
        Ok(self.state.lock().unwrap().windows_version.clone())
    }

    fn list_display_modes(&self) -> Result<Vec<DisplayModeInfo>> {
        Ok(self.state.lock().unwrap().displays.clone())
    }

    fn list_supported_display_modes(
        &self,
        _device_name: &str,
    ) -> Result<Vec<DisplayResolutionMode>> {
        Ok(self.state.lock().unwrap().supported_display_modes.clone())
    }

    fn set_display_mode(
        &self,
        device_name: &str,
        width: u32,
        height: u32,
        refresh_hz: u32,
    ) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        let supported = s
            .supported_display_modes
            .iter()
            .any(|m| m.width == width && m.height == height && m.refresh_hz == refresh_hz);
        if !supported {
            return Err(PolledError::Win32 {
                context: "MockEnv::set_display_mode".to_string(),
                message: format!("Unsupported mock mode {width}x{height} @ {refresh_hz} Hz"),
            });
        }
        if let Some(d) = s
            .displays
            .iter_mut()
            .find(|d| d.device_name.eq_ignore_ascii_case(device_name))
        {
            d.width = width;
            d.height = height;
            d.current_hz = refresh_hz;
        }
        Ok(())
    }

    fn measure_timer_resolution(&self) -> Result<TimerResolutionSample> {
        Ok(self.state.lock().unwrap().timer_sample.clone())
    }

    fn list_usb_power_devices(&self) -> Result<Vec<UsbPowerDevice>> {
        Ok(self.state.lock().unwrap().usb_power_devices.clone())
    }

    fn list_usb_controllers(&self) -> Result<Vec<UsbControllerInfo>> {
        Ok(self.state.lock().unwrap().usb_controllers.clone())
    }

    fn list_pci_msi_devices(&self) -> Result<Vec<PciMsiDevice>> {
        Ok(self.state.lock().unwrap().pci_msi_devices.clone())
    }

    fn list_usb_topology(&self) -> Result<Vec<UsbMouseTopology>> {
        Ok(self.state.lock().unwrap().usb_topology.clone())
    }

    fn list_audio_endpoints(&self, flow: AudioFlow) -> Result<Vec<AudioEndpointInfo>> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .audio_endpoints
            .iter()
            .filter(|e| e.flow == flow)
            .cloned()
            .collect())
    }

    fn set_default_audio_endpoint(&self, endpoint_id: &str) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        let target_flow = s
            .audio_endpoints
            .iter()
            .find(|e| e.id == endpoint_id)
            .map(|e| e.flow);
        if let Some(flow) = target_flow {
            for ep in &mut s.audio_endpoints {
                if ep.flow == flow {
                    ep.is_default = ep.id == endpoint_id;
                }
            }
        }
        Ok(())
    }

    fn get_audio_endpoint_volume(&self, flow: AudioFlow, endpoint_id: Option<&str>) -> Result<u8> {
        let s = self.state.lock().unwrap();
        let found = s.audio_endpoints.iter().find(|e| {
            e.flow == flow
                && match endpoint_id {
                    Some(id) => e.id == id,
                    None => e.is_default,
                }
        });
        Ok(found.and_then(|e| e.volume_percent).unwrap_or(50))
    }

    fn set_audio_endpoint_volume(
        &self,
        flow: AudioFlow,
        endpoint_id: Option<&str>,
        percent: u8,
    ) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        if let Some(ep) = s.audio_endpoints.iter_mut().find(|e| {
            e.flow == flow
                && match endpoint_id {
                    Some(id) => e.id == id,
                    None => e.is_default,
                }
        }) {
            ep.volume_percent = Some(percent.min(100));
        }
        Ok(())
    }

    fn list_network_adapters(&self) -> Result<Vec<NetworkAdapterInfo>> {
        Ok(self.state.lock().unwrap().network_adapters.clone())
    }
}
