// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::{PolledError, Result};
use crate::win::{
    AccessibilityKind, DisplayModeInfo, MouseAccelParams, PowerSchemeInfo, PowerValueIndex,
    ProcessSample, RegHive, SystemEnv, TimerResolutionSample, UsbControllerInfo, UsbMouseTopology,
    UsbPowerDevice, WindowsVersion,
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
    accessibility_flags: HashMap<AccessibilityKind, u32>,
    active_power_scheme: String,
    power_schemes: Vec<PowerSchemeInfo>,
    power_settings: HashMap<(String, String, String), PowerValueIndex>,
    processes: Vec<ProcessSample>,
    windows_version: WindowsVersion,
    displays: Vec<DisplayModeInfo>,
    timer_sample: TimerResolutionSample,
    usb_power_devices: Vec<UsbPowerDevice>,
    usb_controllers: Vec<UsbControllerInfo>,
    usb_topology: Vec<UsbMouseTopology>,
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
            usb_topology: Vec::new(),
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

    pub fn set_usb_topology(&self, topology: Vec<UsbMouseTopology>) {
        self.state.lock().unwrap().usb_topology = topology;
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

    fn measure_timer_resolution(&self) -> Result<TimerResolutionSample> {
        Ok(self.state.lock().unwrap().timer_sample.clone())
    }

    fn list_usb_power_devices(&self) -> Result<Vec<UsbPowerDevice>> {
        Ok(self.state.lock().unwrap().usb_power_devices.clone())
    }

    fn list_usb_controllers(&self) -> Result<Vec<UsbControllerInfo>> {
        Ok(self.state.lock().unwrap().usb_controllers.clone())
    }

    fn list_usb_topology(&self) -> Result<Vec<UsbMouseTopology>> {
        Ok(self.state.lock().unwrap().usb_topology.clone())
    }
}
