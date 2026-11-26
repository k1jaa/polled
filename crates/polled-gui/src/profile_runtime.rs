// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Background profile-switching runtime connecting `ForegroundWatcher` -> `FocusDebouncer`
//! -> `match_profile` -> multi-mouse `SafeDeviceController` (+ optional Windows pointer speed),
//! with automatic USB hot-plug discovery across multiple mice.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::Local;
use polled_core::win::{
    AudioFlow, DisplayModeInfo, DisplayResolutionMode, RegHive, SystemEnv, WindowsEnv,
};
use polled_core::{
    detect_all_keyboards, detect_hardware_devices, detect_wooting_keyboards,
    duck_app_audio_sessions, hid_topology_fingerprint, match_profile, normalize_exe_name,
    restore_app_audio_sessions, AppPaths, ApplyOutcome, BatteryStatus, DeviceCapabilities,
    DeviceInfo, DryRunDevice, FocusDebouncer, FocusEvent, ForegroundWatcher, GameProfile,
    KeyboardStatus, MouseSettings, PolledError, ProfileStore, SafeDeviceController,
    SavedAppAudioVolume, Severity, SupportTier, TrayAlertConfig, WootingAnalogBasePatch,
    WootingGlobalSettingsPatch,
};
use serde::{Deserialize, Serialize};

use crate::app::WorkerMsg;
use crate::hotplug::UsbHotplugWatcher;
use crate::notifications::{notify_low_battery, notify_profile_switched};

const MAX_LOG_ENTRIES: usize = 50;
const HOTPLUG_FALLBACK_POLL_INTERVAL: Duration = Duration::from_secs(5);
const HOTPLUG_EVENT_DEBOUNCE: Duration = Duration::from_millis(300);
const BATTERY_POLL_INTERVAL: Duration = Duration::from_secs(60);
const MIC_LOCK_INTERVAL: Duration = Duration::from_secs(2);
const AUDIO_REFRESH_INTERVAL: Duration = Duration::from_secs(15);
const DISPLAY_TEST_FAILSAFE_TIMEOUT: Duration = Duration::from_secs(11);
const WAKE_RETRY_WINDOW: Duration = Duration::from_secs(90);
const WAKE_RETRY_POLL_INTERVAL: Duration = Duration::from_secs(2);
const GPU_PREF_SUBKEY: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
const PUSH_NOTIF_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\PushNotifications";
const PUSH_NOTIF_VALUE: &str = "ToastEnabled";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendSelection {
    /// Automatically detects USB hot-plug changes and applies profiles to all connected supported mice.
    AutoConnected,
    HardwareIndex(usize),
    DryRunVolatile,
    DryRunFlashOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileLogEntry {
    pub timestamp: String,
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedDeviceStatus {
    pub info: DeviceInfo,
    pub caps: DeviceCapabilities,
    pub settings: Option<MouseSettings>,
    pub battery: Option<BatteryStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayOption {
    pub info: DisplayModeInfo,
    pub modes: Vec<DisplayResolutionMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileRuntimeSnapshot {
    pub auto_switch_enabled: bool,
    pub profile_locked: bool,
    pub selection: BackendSelection,
    pub detected_hardware: Vec<DeviceInfo>,
    pub active_devices: Vec<ConnectedDeviceStatus>,
    /// Connected supported keyboards (Wooting, Wuque/SparkLink, Keychron HE, SteelSeries, Razer,
    /// Arbiter, Logitech, Kochi, Geonworks Venom, QMK/VIA) and their live telemetry snapshots.
    pub keyboards: Vec<KeyboardStatus>,
    pub any_flash_only: bool,
    pub flash_warning: Option<String>,
    pub foreground_exe: Option<String>,
    pub pending_exe: Option<String>,
    pub active_profile_id: String,
    pub active_profile_name: String,
    pub available_profiles: Vec<(String, String)>,
    /// Active playback endpoints as `(id, friendly_name, is_default)`.
    pub audio_outputs: Vec<(String, String, bool)>,
    /// Active microphone endpoints as `(id, friendly_name, is_default)`.
    pub audio_inputs: Vec<(String, String, bool)>,
    /// Current mute state of the default Windows microphone (`Some(true)` = muted).
    #[serde(default)]
    pub mic_muted: Option<bool>,
    /// Active Mic Gain Lock target (`0..=100%`) when the current profile enforces mic gain.
    pub mic_lock_active: Option<u8>,
    /// Attached monitors and their supported `(width, height, refresh_hz)` modes.
    pub displays: Vec<DisplayOption>,
    /// Human-readable label of an active temporary display override (auto-reverts on desktop/exit).
    pub active_display_override: Option<String>,
    pub total_flash_writes_session: u64,
    pub log: Vec<ProfileLogEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProfileRuntimeCmd {
    SetAutoSwitch(bool),
    SetProfileLock(bool),
    CycleNextProfile,
    SwitchAudioOutput(String),
    CycleAudioOutput,
    UpdateAudioCycleDevices(Vec<String>),
    ToggleMicMute,
    RefreshAudioDevices,
    RefreshDisplays,
    TestDisplayMode {
        device_name: Option<String>,
        width: u32,
        height: u32,
        refresh_hz: u32,
    },
    KeepTestDisplayMode,
    RevertTestDisplayMode,
    QuickSetMousePolling(u16),
    QuickSetMouseDpi(u16),
    RefreshMouseAndBattery,
    RefreshWooting,
    CycleWootingProfile,
    SwitchWootingProfile {
        device_id: Option<String>,
        slot: u8,
    },
    WriteWootingAnalogBase {
        device_id: Option<String>,
        patch: WootingAnalogBasePatch,
    },
    WriteWootingPerKeyActuation {
        device_id: Option<String>,
        updates: Vec<(u16, Option<f32>)>,
    },
    WriteWootingGlobalSettings {
        device_id: Option<String>,
        patch: WootingGlobalSettingsPatch,
    },
    SetKeyboardPollingRate {
        device_id: Option<String>,
        hz: u16,
    },
    SetKeyboardBaseActuation {
        device_id: Option<String>,
        actuation_mm: f32,
        rt_enabled: Option<bool>,
        rt_mm: Option<f32>,
    },
    SetKeyboardRapidTrigger {
        device_id: Option<String>,
        enabled: bool,
    },
    SetKeyboardSystemMode {
        device_id: Option<String>,
        mac_mode: bool,
    },
    SetKeyboardSocd {
        device_id: Option<String>,
        enabled: bool,
    },
    SetKeyboardDebounce {
        device_id: Option<String>,
        typing_mode: bool,
        debounce_ms: u8,
    },
    ReadFromMouseForEditor(Option<String>),
    UpdateAlertConfig(TrayAlertConfig),
    UsbDeviceChanged,
    UpdateStore(ProfileStore),
    SelectBackend(BackendSelection),
    RedetectHardware,
    ApplyProfileNow(String),
    Shutdown,
}

enum RuntimeBackend {
    Local {
        cmd_tx: Sender<ProfileRuntimeCmd>,
        hotplug_watcher: Option<UsbHotplugWatcher>,
        thread_handle: Option<JoinHandle<()>>,
    },
    Remote {
        send_fn: Box<dyn Fn(ProfileRuntimeCmd) + Send + Sync>,
    },
}

pub struct ProfileRuntimeHandle {
    backend: RuntimeBackend,
}

impl ProfileRuntimeHandle {
    pub fn start(
        initial_store: ProfileStore,
        initial_auto_switch: bool,
        initial_alerts: TrayAlertConfig,
        initial_audio_cycle_ids: Vec<String>,
        ui_tx: Sender<WorkerMsg>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let hotplug_watcher = UsbHotplugWatcher::start(cmd_tx.clone());

        let thread_handle = std::thread::Builder::new()
            .name("polled-profile-runtime".to_string())
            .spawn(move || {
                run_runtime_loop(
                    initial_store,
                    initial_auto_switch,
                    initial_alerts,
                    initial_audio_cycle_ids,
                    cmd_rx,
                    ui_tx,
                    wake,
                );
            })
            .expect("failed to spawn profile runtime thread");

        Self {
            backend: RuntimeBackend::Local {
                cmd_tx,
                hotplug_watcher: Some(hotplug_watcher),
                thread_handle: Some(thread_handle),
            },
        }
    }

    /// Creates a lightweight proxy handle that forwards commands over local IPC to the
    /// pure-Win32 background daemon. Dropping this handle does **not** shut down the daemon.
    pub fn from_remote_callback<F>(send_fn: F) -> Self
    where
        F: Fn(ProfileRuntimeCmd) + Send + Sync + 'static,
    {
        Self {
            backend: RuntimeBackend::Remote {
                send_fn: Box::new(send_fn),
            },
        }
    }

    pub fn send(&self, cmd: ProfileRuntimeCmd) {
        match &self.backend {
            RuntimeBackend::Local { cmd_tx, .. } => {
                let _ = cmd_tx.send(cmd);
            }
            RuntimeBackend::Remote { send_fn } => {
                send_fn(cmd);
            }
        }
    }
}

impl Drop for ProfileRuntimeHandle {
    fn drop(&mut self) {
        if let RuntimeBackend::Local {
            cmd_tx,
            hotplug_watcher,
            thread_handle,
        } = &mut self.backend
        {
            let _ = cmd_tx.send(ProfileRuntimeCmd::Shutdown);
            hotplug_watcher.take();
            if let Some(handle) = thread_handle.take() {
                let _ = handle.join();
            }
        }
    }
}

struct ManagedController {
    controller: SafeDeviceController,
    last_settings: Option<MouseSettings>,
    last_battery: Option<BatteryStatus>,
    low_battery_notified: bool,
}

impl ManagedController {
    fn init(mut controller: SafeDeviceController, alerts: &TrayAlertConfig) -> Self {
        let last_settings = controller.read_settings().ok();
        let last_battery = controller.read_battery();
        let mut low_battery_notified = false;
        let warn_pct = alerts.clamped_threshold();
        if alerts.low_battery_toast {
            if let Some(batt) = last_battery {
                if !batt.charging && batt.percent <= warn_pct {
                    notify_low_battery(&controller.info().name, batt.percent);
                    low_battery_notified = true;
                }
            }
        }
        Self {
            controller,
            last_settings,
            last_battery,
            low_battery_notified,
        }
    }

    fn refresh_battery(&mut self, alerts: &TrayAlertConfig) -> bool {
        let next = self.controller.read_battery();
        let warn_pct = alerts.clamped_threshold();
        let reset_pct = alerts.reset_threshold();
        if let Some(batt) = next {
            if batt.charging || batt.percent >= reset_pct {
                self.low_battery_notified = false;
            } else if alerts.low_battery_toast
                && batt.percent <= warn_pct
                && !self.low_battery_notified
            {
                notify_low_battery(&self.controller.info().name, batt.percent);
                self.low_battery_notified = true;
            }
        }
        if next != self.last_battery {
            self.last_battery = next;
            true
        } else {
            false
        }
    }
}

struct RuntimeState {
    store: ProfileStore,
    alerts: TrayAlertConfig,
    auto_switch_enabled: bool,
    profile_locked: bool,
    selection: BackendSelection,
    detected_hardware_info: Vec<DeviceInfo>,
    controllers: Vec<ManagedController>,
    keyboards: Vec<KeyboardStatus>,
    last_topology: Vec<(u16, u16, u16, u16)>,
    last_topology_check: Instant,
    pending_hotplug_at: Option<Instant>,
    last_battery_poll: Instant,
    audio_outputs: Vec<(String, String, bool)>,
    audio_inputs: Vec<(String, String, bool)>,
    audio_cycle_device_ids: Vec<String>,
    mic_muted: Option<bool>,
    last_audio_refresh: Instant,
    /// `(optional_endpoint_id, target_volume_pct)` when Mic Gain Lock is active.
    mic_lock_target: Option<(Option<String>, u8)>,
    last_mic_lock_check: Instant,
    displays: Vec<DisplayOption>,
    /// Native desktop `(device_name, width, height, refresh_hz)` captured before a profile overrides display mode.
    original_display_mode: Option<(String, u32, u32, u32)>,
    active_display_override: Option<String>,
    /// Failsafe auto-revert `(device_name, prev_w, prev_h, prev_hz, deadline)` for GUI "Test Mode (10s)".
    pending_display_test_revert: Option<(String, u32, u32, u32, Instant)>,
    debouncer: FocusDebouncer,
    watcher: Option<ForegroundWatcher>,
    focus_rx: Option<Receiver<FocusEvent>>,
    foreground_exe: Option<String>,
    /// Maps lowercase `.exe` basename -> full Win32 executable path observed by `ForegroundWatcher`.
    exe_paths: HashMap<String, String>,
    active_profile_id: String,
    active_profile_name: String,
    /// Queued `(profile_id, retry_at)` when a focus switch was deferred by flash-wear cooldown.
    pending_retry: Option<(String, Instant)>,
    /// Queued `(profile_id, expires_at, next_check_at)` when a wireless mouse was asleep during switch.
    pending_wake_retry: Option<(String, Instant, Instant)>,
    /// Captured original `HKCU\...\PushNotifications\ToastEnabled` when a profile enables Quiet Notifications.
    original_toast_enabled: Option<Option<u32>>,
    /// Captured original Windows Volume Mixer levels for background apps ducked by the active profile.
    saved_ducked_apps: Vec<SavedAppAudioVolume>,
    /// Device IDs currently stepped down to 1000 Hz by the opt-in low-battery polling guard.
    battery_guard_active: HashSet<String>,
    log: Vec<ProfileLogEntry>,
}

impl RuntimeState {
    fn push_log(&mut self, severity: Severity, message: impl Into<String>) {
        self.log.insert(
            0,
            ProfileLogEntry {
                timestamp: Local::now().format("%H:%M:%S").to_string(),
                severity,
                message: message.into(),
            },
        );
        self.log.truncate(MAX_LOG_ENTRIES);
    }

    fn snapshot(&self) -> ProfileRuntimeSnapshot {
        let pending_exe = self.debouncer.pending_exe().flatten().map(str::to_string);

        let active_devices: Vec<ConnectedDeviceStatus> = self
            .controllers
            .iter()
            .map(|m| ConnectedDeviceStatus {
                info: m.controller.info(),
                caps: m.controller.capabilities(),
                settings: m.last_settings.clone(),
                battery: m.last_battery,
            })
            .collect();

        let any_flash_only = self
            .controllers
            .iter()
            .any(|m| !m.controller.capabilities().supports_volatile_writes);

        let flash_warning = self
            .controllers
            .iter()
            .find_map(|m| m.controller.capabilities().flash_write_warning);

        let total_flash_writes_session = self
            .controllers
            .iter()
            .map(|m| m.controller.flash_writes_this_session())
            .sum();

        let available_profiles: Vec<(String, String)> = self
            .store
            .profiles
            .iter()
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();

        ProfileRuntimeSnapshot {
            auto_switch_enabled: self.auto_switch_enabled,
            profile_locked: self.profile_locked,
            selection: self.selection.clone(),
            detected_hardware: self.detected_hardware_info.clone(),
            active_devices,
            keyboards: self.keyboards.clone(),
            any_flash_only,
            flash_warning,
            foreground_exe: self.foreground_exe.clone(),
            pending_exe,
            active_profile_id: self.active_profile_id.clone(),
            active_profile_name: self.active_profile_name.clone(),
            available_profiles,
            audio_outputs: self.audio_outputs.clone(),
            audio_inputs: self.audio_inputs.clone(),
            mic_muted: self.mic_muted,
            mic_lock_active: self.mic_lock_target.as_ref().map(|(_, pct)| *pct),
            displays: self.displays.clone(),
            active_display_override: self.active_display_override.clone(),
            total_flash_writes_session,
            log: self.log.clone(),
        }
    }

    fn emit(&self, ui_tx: &Sender<WorkerMsg>, wake: &Arc<dyn Fn() + Send + Sync>) {
        let _ = ui_tx.send(WorkerMsg::ProfileRuntimeUpdated(Box::new(self.snapshot())));
        wake();
    }

    fn start_watcher(&mut self) {
        if self.watcher.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        match ForegroundWatcher::start(tx) {
            Ok(watcher) => {
                self.watcher = Some(watcher);
                self.focus_rx = Some(rx);
                self.push_log(
                    Severity::Ok,
                    "Started foreground window watcher (SetWinEventHook, out-of-context).",
                );
            }
            Err(err) => {
                self.auto_switch_enabled = false;
                self.push_log(
                    Severity::Bad,
                    format!("Could not start foreground window hook: {err}"),
                );
            }
        }
    }

    fn stop_watcher(&mut self) {
        if let Some(mut watcher) = self.watcher.take() {
            watcher.stop();
        }
        self.focus_rx = None;
    }

    fn refresh_keyboards(&mut self) {
        self.keyboards = detect_all_keyboards()
            .unwrap_or_default()
            .into_iter()
            .map(|kb| kb.read_status())
            .collect();
    }

    fn rebuild_controllers(&mut self, selection: BackendSelection, reason: &str) {
        let mut hardware = detect_hardware_devices().unwrap_or_default();
        self.detected_hardware_info = hardware.iter().map(|d| d.info()).collect();
        self.refresh_keyboards();
        self.last_topology = hid_topology_fingerprint();
        self.last_topology_check = Instant::now();

        let mut next_controllers: Vec<ManagedController> = Vec::new();
        let alerts = self.alerts.clone();
        let effective_selection = match selection {
            BackendSelection::AutoConnected => {
                if hardware.is_empty() {
                    next_controllers.push(ManagedController::init(
                        SafeDeviceController::new(Box::new(DryRunDevice::new_volatile())),
                        &alerts,
                    ));
                } else {
                    for dev in hardware {
                        next_controllers.push(ManagedController::init(
                            SafeDeviceController::new(dev),
                            &alerts,
                        ));
                    }
                }
                BackendSelection::AutoConnected
            }
            BackendSelection::HardwareIndex(idx) => {
                if idx < hardware.len() {
                    next_controllers.push(ManagedController::init(
                        SafeDeviceController::new(hardware.remove(idx)),
                        &alerts,
                    ));
                    BackendSelection::HardwareIndex(idx)
                } else {
                    self.push_log(
                        Severity::Warn,
                        "Selected mouse was disconnected; switching back to Auto (Connected Mice).",
                    );
                    return self.rebuild_controllers(BackendSelection::AutoConnected, reason);
                }
            }
            BackendSelection::DryRunVolatile => {
                next_controllers.push(ManagedController::init(
                    SafeDeviceController::new(Box::new(DryRunDevice::new_volatile())),
                    &alerts,
                ));
                BackendSelection::DryRunVolatile
            }
            BackendSelection::DryRunFlashOnly => {
                next_controllers.push(ManagedController::init(
                    SafeDeviceController::new(Box::new(DryRunDevice::new_flash_only())),
                    &alerts,
                ));
                BackendSelection::DryRunFlashOnly
            }
        };

        self.selection = effective_selection;
        self.controllers = next_controllers;
        self.last_battery_poll = Instant::now();

        let summary: Vec<String> = self
            .controllers
            .iter()
            .map(|m| {
                let info = m.controller.info();
                let batt_suffix = m
                    .last_battery
                    .map(|b| format!(" · {}", b.compact_label()))
                    .unwrap_or_default();
                match &m.last_settings {
                    Some(s) => format!("{} ({}{batt_suffix})", info.name, s.summary_string()),
                    None => format!("{} (asleep / move mouse to wake{batt_suffix})", info.name),
                }
            })
            .collect();

        self.push_log(
            Severity::Info,
            format!("Devices ({reason}): {}", summary.join(" | ")),
        );
    }

    fn poll_batteries(&mut self) -> (bool, Option<(String, String)>) {
        let now = Instant::now();
        if now.saturating_duration_since(self.last_battery_poll) < BATTERY_POLL_INTERVAL {
            return (false, None);
        }
        self.last_battery_poll = now;
        let alerts = self.alerts.clone();
        let mut any_changed = false;
        for managed in &mut self.controllers {
            if managed.refresh_battery(&alerts) {
                any_changed = true;
            }
        }
        let (guard_changed, osd) = self.evaluate_battery_polling_guard();
        (any_changed || guard_changed, osd)
    }

    fn evaluate_battery_polling_guard(&mut self) -> (bool, Option<(String, String)>) {
        let enabled = self.alerts.low_battery_polling_guard;
        let threshold = self.alerts.clamped_threshold();
        let reset_threshold = self.alerts.reset_threshold();
        let active_profile = self
            .store
            .profiles
            .iter()
            .find(|p| p.id == self.active_profile_id)
            .cloned();

        let mut logs: Vec<(Severity, String)> = Vec::new();
        let mut osd: Option<(String, String)> = None;
        let mut changed = false;

        for managed in &mut self.controllers {
            let info = managed.controller.info();
            if info.support_tier == SupportTier::Simulator {
                continue;
            }
            let Some(bat) = managed.last_battery else {
                continue;
            };
            let caps = managed.controller.capabilities();
            let dev_id = info.id;
            let dev_name = info.name;
            let already_guarded = self.battery_guard_active.contains(&dev_id);

            if enabled && !bat.charging && bat.percent <= threshold {
                let current_hz = managed
                    .last_settings
                    .as_ref()
                    .and_then(|s| s.polling_rate_hz)
                    .unwrap_or(0);
                if !already_guarded && current_hz > 1000 && caps.polling_rates_hz.contains(&1000) {
                    let step_down = MouseSettings {
                        polling_rate_hz: Some(1000),
                        ..Default::default()
                    };
                    match managed.controller.apply_profile_settings(&step_down) {
                        Ok(ApplyOutcome::Applied { verified, .. }) => {
                            managed.last_settings = Some(verified);
                            self.battery_guard_active.insert(dev_id);
                            changed = true;
                            logs.push((
                                Severity::Warn,
                                format!(
                                    "[{dev_name}] Battery Polling Guard ({}% <= {threshold}%): stepped down {current_hz} Hz -> 1000 Hz to save battery.",
                                    bat.percent
                                ),
                            ));
                            if self.alerts.osd_pill_enabled {
                                osd = Some((
                                    "Polled · Battery Guard".to_string(),
                                    format!("{dev_name} ({}%) -> 1000 Hz", bat.percent),
                                ));
                            }
                        }
                        Ok(ApplyOutcome::SkippedUnchanged { current }) => {
                            managed.last_settings = Some(current);
                            self.battery_guard_active.insert(dev_id);
                        }
                        Ok(ApplyOutcome::SkippedNoSettings) => {}
                        Err(err) => {
                            logs.push((
                                Severity::Warn,
                                format!(
                                    "[{dev_name}] Battery Polling Guard step-down failed: {err}"
                                ),
                            ));
                        }
                    }
                }
            } else if already_guarded
                && (!enabled || bat.charging || bat.percent >= reset_threshold)
            {
                self.battery_guard_active.remove(&dev_id);
                if let Some(prof) = &active_profile {
                    let (raw, _) = prof.settings_for_device(&dev_id);
                    if let Some(target_hz) = raw.polling_rate_hz.filter(|&hz| hz > 1000) {
                        let restore_req = MouseSettings {
                            polling_rate_hz: Some(target_hz),
                            ..Default::default()
                        };
                        let (adapted, _) = caps.adapt_settings(&restore_req);
                        if let Ok(ApplyOutcome::Applied { verified, .. }) =
                            managed.controller.apply_profile_settings(&adapted)
                        {
                            managed.last_settings = Some(verified);
                            changed = true;
                            let why = if !enabled {
                                "guard disabled"
                            } else if bat.charging {
                                "charging"
                            } else {
                                "battery recovered"
                            };
                            logs.push((
                                Severity::Ok,
                                format!(
                                    "[{dev_name}] Battery Polling Guard ({why}, {}%): restored {} Hz.",
                                    bat.percent, target_hz
                                ),
                            ));
                            if self.alerts.osd_pill_enabled {
                                osd = Some((
                                    "Polled · Battery Restored".to_string(),
                                    format!("{dev_name} -> {target_hz} Hz"),
                                ));
                            }
                        }
                    }
                }
            }
        }

        for (sev, msg) in logs {
            self.push_log(sev, msg);
        }
        (changed, osd)
    }

    fn force_refresh_mouse_and_battery(&mut self) {
        let alerts = self.alerts.clone();
        let mut details = Vec::new();
        for managed in &mut self.controllers {
            if let Ok(settings) = managed.controller.read_settings() {
                managed.last_settings = Some(settings);
            }
            managed.refresh_battery(&alerts);
            let name = managed.controller.info().name;
            let batt = managed
                .last_battery
                .map(|b| format!(" · {}", b.compact_label()))
                .unwrap_or_default();
            let s = managed
                .last_settings
                .as_ref()
                .map(MouseSettings::summary_string)
                .unwrap_or_else(|| "asleep".to_string());
            details.push(format!("{name} ({s}{batt})"));
        }
        self.last_battery_poll = Instant::now();
        if !details.is_empty() {
            self.push_log(
                Severity::Info,
                format!("Refreshed mouse & battery: {}", details.join(" | ")),
            );
        }
    }

    fn quick_apply_mouse_setting(&mut self, desired: &MouseSettings, label: &str) {
        let alerts = self.alerts.clone();
        let mut logs: Vec<(Severity, String)> = Vec::new();
        for managed in &mut self.controllers {
            let dev_name = managed.controller.info().name;
            let caps = managed.controller.capabilities();
            let (adapted, _) = caps.adapt_settings(desired);
            if adapted.is_empty() {
                continue;
            }
            match managed.controller.apply_profile_settings(&adapted) {
                Ok(ApplyOutcome::Applied { mode, verified, .. }) => {
                    managed.last_settings = Some(verified.clone());
                    logs.push((
                        Severity::Ok,
                        format!(
                            "[{dev_name}] Tray quick-set {label} via {mode:?} & verified ({}).",
                            verified.summary_string()
                        ),
                    ));
                }
                Ok(ApplyOutcome::SkippedUnchanged { current }) => {
                    managed.last_settings = Some(current);
                    logs.push((
                        Severity::Info,
                        format!("[{dev_name}] Tray quick-set {label}: already active (0 writes)."),
                    ));
                }
                Ok(ApplyOutcome::SkippedNoSettings) => {}
                Err(err) => {
                    logs.push((
                        Severity::Warn,
                        format!("[{dev_name}] Tray quick-set {label} failed: {err}"),
                    ));
                }
            }
            managed.refresh_battery(&alerts);
        }
        self.last_battery_poll = Instant::now();
        for (sev, msg) in logs {
            self.push_log(sev, msg);
        }
    }

    fn read_from_mouse_for_editor(
        &mut self,
        target_device_id: Option<&str>,
    ) -> (Option<String>, Option<MouseSettings>, Option<String>) {
        let managed = match target_device_id {
            Some(id) => self
                .controllers
                .iter_mut()
                .find(|m| m.controller.info().id == id),
            None => self.controllers.first_mut(),
        };
        let Some(managed) = managed else {
            return (
                None,
                None,
                Some("No connected mouse is available to read from.".to_string()),
            );
        };
        let name = managed.controller.info().name;
        match managed.controller.read_settings() {
            Ok(settings) => {
                managed.last_settings = Some(settings.clone());
                let alerts = self.alerts.clone();
                managed.refresh_battery(&alerts);
                self.push_log(
                    Severity::Ok,
                    format!(
                        "[{name}] Read live settings into profile editor: {}.",
                        settings.summary_string()
                    ),
                );
                (Some(name), Some(settings), None)
            }
            Err(err) => {
                let msg = format!(
                    "Could not read live settings from {name} (move mouse to wake it if wireless): {err}"
                );
                self.push_log(Severity::Warn, msg.clone());
                (Some(name), None, Some(msg))
            }
        }
    }

    /// Checks whether USB HID devices were plugged in or unplugged via either an immediate
    /// `WM_DEVICECHANGE` notification (debounced 300ms) or the 5s safety fallback poll.
    fn check_hotplug(&mut self) -> bool {
        let now = Instant::now();
        let event_due = self.pending_hotplug_at.is_some_and(|at| now >= at);
        let fallback_due = now.saturating_duration_since(self.last_topology_check)
            >= HOTPLUG_FALLBACK_POLL_INTERVAL;

        if !event_due && !fallback_due {
            return false;
        }
        if event_due {
            self.pending_hotplug_at = None;
        }
        self.last_topology_check = now;

        let mut any_changed = false;
        if event_due && self.refresh_audio_devices() {
            any_changed = true;
        }

        if matches!(
            self.selection,
            BackendSelection::AutoConnected | BackendSelection::HardwareIndex(_)
        ) {
            let current_fp = hid_topology_fingerprint();
            if current_fp != self.last_topology {
                let sel = self.selection.clone();
                let reason = if event_due {
                    "USB hot-plug (instant)"
                } else {
                    "USB hot-plug"
                };
                self.rebuild_controllers(sel, reason);
                self.refresh_audio_devices();
                any_changed = true;
            }
        }
        any_changed
    }

    fn refresh_audio_devices(&mut self) -> bool {
        let env = WindowsEnv;
        let next_outputs: Vec<(String, String, bool)> = env
            .list_audio_endpoints(AudioFlow::Render)
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.id, e.name, e.is_default))
            .collect();
        let raw_inputs = env
            .list_audio_endpoints(AudioFlow::Capture)
            .unwrap_or_default();
        let next_mic_muted = raw_inputs
            .iter()
            .find(|e| e.is_default)
            .and_then(|e| e.muted);
        let next_inputs: Vec<(String, String, bool)> = raw_inputs
            .into_iter()
            .map(|e| (e.id, e.name, e.is_default))
            .collect();
        self.last_audio_refresh = Instant::now();
        let changed = next_outputs != self.audio_outputs
            || next_inputs != self.audio_inputs
            || next_mic_muted != self.mic_muted;
        self.audio_outputs = next_outputs;
        self.audio_inputs = next_inputs;
        self.mic_muted = next_mic_muted;
        changed
    }

    fn refresh_displays(&mut self) {
        let env = WindowsEnv;
        let list = env.list_display_modes().unwrap_or_default();
        self.displays = list
            .into_iter()
            .map(|info| {
                let modes = env
                    .list_supported_display_modes(&info.device_name)
                    .unwrap_or_default();
                DisplayOption { info, modes }
            })
            .collect();
    }

    fn resolve_target_display(&self, preferred: Option<&str>) -> Option<DisplayModeInfo> {
        let env = WindowsEnv;
        let current_list = env.list_display_modes().unwrap_or_default();
        if let Some(name) = preferred {
            if let Some(found) = current_list
                .iter()
                .find(|d| d.device_name.eq_ignore_ascii_case(name))
            {
                return Some(found.clone());
            }
        }
        current_list
            .iter()
            .find(|d| d.is_primary)
            .or_else(|| current_list.first())
            .cloned()
    }

    fn restore_original_display(&mut self, reason: &str) {
        if let Some((dev, w, h, hz)) = self.original_display_mode.take() {
            let env = WindowsEnv;
            match env.set_display_mode(&dev, w, h, hz) {
                Ok(()) => {
                    self.active_display_override = None;
                    self.refresh_displays();
                    self.push_log(
                        Severity::Ok,
                        format!("Restored display {dev} -> {w}×{h} @ {hz} Hz ({reason})."),
                    );
                }
                Err(err) => {
                    self.active_display_override = None;
                    self.push_log(
                        Severity::Warn,
                        format!("Failed to restore display {dev} to {w}×{h} @ {hz} Hz: {err}"),
                    );
                }
            }
        }
    }

    fn revert_test_display_if_pending(&mut self, reason: &str) -> Option<String> {
        let (dev, w, h, hz, _) = self.pending_display_test_revert.take()?;
        let env = WindowsEnv;
        let msg = match env.set_display_mode(&dev, w, h, hz) {
            Ok(()) => {
                self.refresh_displays();
                let m = format!("Reverted display test on {dev} -> {w}×{h} @ {hz} Hz ({reason}).");
                self.push_log(Severity::Info, m.clone());
                m
            }
            Err(err) => {
                let m = format!("Failed to revert display test on {dev}: {err}");
                self.push_log(Severity::Warn, m.clone());
                m
            }
        };
        Some(msg)
    }

    fn start_test_display_mode(
        &mut self,
        device_name: Option<&str>,
        width: u32,
        height: u32,
        refresh_hz: u32,
    ) -> (String, String, String, Option<String>) {
        // Revert any prior test first so we never lose the true original desktop mode.
        let _ = self.revert_test_display_if_pending("starting new test");

        let Some(target) = self.resolve_target_display(device_name) else {
            return (
                String::new(),
                format!("{width}×{height} @ {refresh_hz} Hz"),
                String::new(),
                Some("No active Windows monitor found to test display mode.".to_string()),
            );
        };
        let prev_label = format!(
            "{}×{} @ {} Hz",
            target.width, target.height, target.current_hz
        );
        let applied_label = format!("{width}×{height} @ {refresh_hz} Hz");
        let env = WindowsEnv;
        match env.set_display_mode(&target.device_name, width, height, refresh_hz) {
            Ok(()) => {
                self.pending_display_test_revert = Some((
                    target.device_name.clone(),
                    target.width,
                    target.height,
                    target.current_hz,
                    Instant::now() + DISPLAY_TEST_FAILSAFE_TIMEOUT,
                ));
                self.refresh_displays();
                self.push_log(
                    Severity::Info,
                    format!(
                        "Testing display mode on {} -> {applied_label} (auto-revert armed).",
                        target.device_name
                    ),
                );
                (target.device_name, applied_label, prev_label, None)
            }
            Err(err) => {
                let msg = format!("Display mode test rejected: {err}");
                self.push_log(Severity::Warn, msg.clone());
                (target.device_name, applied_label, prev_label, Some(msg))
            }
        }
    }

    fn poll_audio_and_mic_lock(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;

        if let Some((ref target_id, target_pct)) = self.mic_lock_target {
            if now.saturating_duration_since(self.last_mic_lock_check) >= MIC_LOCK_INTERVAL {
                self.last_mic_lock_check = now;
                let env = WindowsEnv;
                if let Ok(cur) =
                    env.get_audio_endpoint_volume(AudioFlow::Capture, target_id.as_deref())
                {
                    if cur != target_pct {
                        match env.set_audio_endpoint_volume(
                            AudioFlow::Capture,
                            target_id.as_deref(),
                            target_pct,
                        ) {
                            Ok(()) => {
                                self.push_log(
                                    Severity::Info,
                                    format!(
                                        "Mic Gain Lock restored microphone level {cur}% -> {target_pct}%."
                                    ),
                                );
                                changed = true;
                            }
                            Err(e) => {
                                self.push_log(
                                    Severity::Warn,
                                    format!("Mic Gain Lock failed to restore mic level: {e}"),
                                );
                                changed = true;
                            }
                        }
                    }
                }
            }
        }

        if now.saturating_duration_since(self.last_audio_refresh) >= AUDIO_REFRESH_INTERVAL
            && self.refresh_audio_devices()
        {
            changed = true;
        }

        changed
    }

    fn switch_audio_output(&mut self, endpoint_id: &str, reason: &str) {
        let env = WindowsEnv;
        let label = self
            .audio_outputs
            .iter()
            .find(|(id, _, _)| id == endpoint_id)
            .map(|(_, name, _)| name.clone())
            .unwrap_or_else(|| endpoint_id.to_string());
        match env.set_default_audio_endpoint(endpoint_id) {
            Ok(()) => {
                self.refresh_audio_devices();
                self.push_log(Severity::Ok, format!("Audio output -> {label} ({reason})"));
            }
            Err(e) => {
                self.push_log(
                    Severity::Warn,
                    format!("Failed to switch audio output to '{label}': {e}"),
                );
            }
        }
    }

    fn apply_profile(&mut self, profile: &GameProfile, reason: &str) -> Option<(String, String)> {
        self.pending_retry = None;

        if let Some(speed) = profile.windows_pointer_speed {
            let env = WindowsEnv;
            if let Ok(current) = env.get_mouse_speed() {
                if current != speed {
                    match env.set_mouse_speed(speed) {
                        Ok(()) => self.push_log(
                            Severity::Info,
                            format!("Windows pointer speed -> {speed}/20 ({reason})"),
                        ),
                        Err(e) => self.push_log(
                            Severity::Warn,
                            format!("Failed to set Windows pointer speed: {e}"),
                        ),
                    }
                }
            }
        }

        if let Some(preset) = profile.windows_power_plan {
            let env = WindowsEnv;
            match env.list_power_schemes() {
                Ok(schemes) => {
                    if let Some(target) = preset.resolve_scheme(&schemes) {
                        let already_active = env
                            .get_active_power_scheme()
                            .ok()
                            .is_some_and(|cur| cur.guid.eq_ignore_ascii_case(&target.guid));
                        if !already_active {
                            match env.set_active_power_scheme(&target.guid) {
                                Ok(()) => self.push_log(
                                    Severity::Info,
                                    format!("Windows power plan -> {} ({reason})", target.name),
                                ),
                                Err(e) => self.push_log(
                                    Severity::Warn,
                                    format!(
                                        "Failed to switch Windows power plan to '{}': {e}",
                                        target.name
                                    ),
                                ),
                            }
                        }
                    } else {
                        self.push_log(
                            Severity::Info,
                            format!(
                                "Windows power plan '{}' is not available on this host (skipped).",
                                preset.label()
                            ),
                        );
                    }
                }
                Err(e) => self.push_log(
                    Severity::Warn,
                    format!("Could not list Windows power plans: {e}"),
                ),
            }
        }

        // --- Per-Game Audio Automation (Output, Mic, Volumes, Mic Gain Lock) ---
        {
            let env = WindowsEnv;
            let mut audio_switched = false;

            if let Some(out_id) = profile.audio_output_device_id.as_deref() {
                let already_default = self
                    .audio_outputs
                    .iter()
                    .any(|(id, _, is_def)| *is_def && id == out_id);
                if !already_default {
                    let out_name = profile
                        .audio_output_device_name
                        .as_deref()
                        .or_else(|| {
                            self.audio_outputs
                                .iter()
                                .find(|(id, _, _)| id == out_id)
                                .map(|(_, n, _)| n.as_str())
                        })
                        .unwrap_or(out_id);
                    match env.set_default_audio_endpoint(out_id) {
                        Ok(()) => {
                            audio_switched = true;
                            self.push_log(
                                Severity::Info,
                                format!("Audio output -> {out_name} ({reason})"),
                            );
                        }
                        Err(e) => self.push_log(
                            Severity::Warn,
                            format!("Failed to switch audio output to '{out_name}': {e}"),
                        ),
                    }
                }
            }

            if let Some(vol) = profile.audio_output_volume {
                let target_id = profile.audio_output_device_id.as_deref();
                let current = env
                    .get_audio_endpoint_volume(AudioFlow::Render, target_id)
                    .ok();
                if current != Some(vol) {
                    match env.set_audio_endpoint_volume(AudioFlow::Render, target_id, vol) {
                        Ok(()) => self.push_log(
                            Severity::Info,
                            format!("Audio output volume -> {vol}% ({reason})"),
                        ),
                        Err(e) => self.push_log(
                            Severity::Warn,
                            format!("Failed to set audio output volume: {e}"),
                        ),
                    }
                }
            }

            if let Some(in_id) = profile.audio_input_device_id.as_deref() {
                let already_default = self
                    .audio_inputs
                    .iter()
                    .any(|(id, _, is_def)| *is_def && id == in_id);
                if !already_default {
                    let in_name = profile
                        .audio_input_device_name
                        .as_deref()
                        .or_else(|| {
                            self.audio_inputs
                                .iter()
                                .find(|(id, _, _)| id == in_id)
                                .map(|(_, n, _)| n.as_str())
                        })
                        .unwrap_or(in_id);
                    match env.set_default_audio_endpoint(in_id) {
                        Ok(()) => {
                            audio_switched = true;
                            self.push_log(
                                Severity::Info,
                                format!("Microphone -> {in_name} ({reason})"),
                            );
                        }
                        Err(e) => self.push_log(
                            Severity::Warn,
                            format!("Failed to switch microphone to '{in_name}': {e}"),
                        ),
                    }
                }
            }

            if let Some(vol) = profile.audio_input_volume {
                let target_id = profile.audio_input_device_id.as_deref();
                let current = env
                    .get_audio_endpoint_volume(AudioFlow::Capture, target_id)
                    .ok();
                if current != Some(vol) {
                    match env.set_audio_endpoint_volume(AudioFlow::Capture, target_id, vol) {
                        Ok(()) => self.push_log(
                            Severity::Info,
                            format!("Microphone volume -> {vol}% ({reason})"),
                        ),
                        Err(e) => self.push_log(
                            Severity::Warn,
                            format!("Failed to set microphone volume: {e}"),
                        ),
                    }
                }
            }

            if profile.lock_mic_volume {
                if let Some(vol) = profile.audio_input_volume {
                    self.mic_lock_target = Some((profile.audio_input_device_id.clone(), vol));
                    self.last_mic_lock_check = Instant::now();
                } else {
                    self.mic_lock_target = None;
                }
            } else {
                self.mic_lock_target = None;
            }

            if audio_switched {
                self.refresh_audio_devices();
            }
        }

        // --- Per-Game Temporary Display Mode Switching (zero registry writes) ---
        if let Some((opt_dev, w, h, hz)) = profile.display_mode_override() {
            if let Some(target) = self.resolve_target_display(opt_dev) {
                if self
                    .original_display_mode
                    .as_ref()
                    .is_some_and(|(orig_dev, _, _, _)| {
                        !orig_dev.eq_ignore_ascii_case(&target.device_name)
                    })
                {
                    self.restore_original_display("switching monitor");
                }
                if self.original_display_mode.is_none() {
                    self.original_display_mode = Some((
                        target.device_name.clone(),
                        target.width,
                        target.height,
                        target.current_hz,
                    ));
                }
                let already_active =
                    target.width == w && target.height == h && target.current_hz == hz;
                if !already_active {
                    let env = WindowsEnv;
                    match env.set_display_mode(&target.device_name, w, h, hz) {
                        Ok(()) => {
                            self.active_display_override = Some(format!("{w}×{h} @ {hz} Hz"));
                            self.refresh_displays();
                            self.push_log(
                                Severity::Ok,
                                format!(
                                    "Display {} -> {w}×{h} @ {hz} Hz ({reason}, temporary).",
                                    target.device_name
                                ),
                            );
                        }
                        Err(err) => {
                            self.push_log(
                                Severity::Warn,
                                format!(
                                    "Display switch to {w}×{h} @ {hz} Hz failed on {}: {err}",
                                    target.device_name
                                ),
                            );
                        }
                    }
                } else {
                    self.active_display_override = Some(format!("{w}×{h} @ {hz} Hz"));
                }
            }
        } else if self.original_display_mode.is_some() {
            self.restore_original_display(reason);
        }

        // --- Per-Game High-Performance GPU Preference (`HKCU\...\UserGpuPreferences`, zero admin) ---
        if profile.force_high_perf_gpu && !profile.is_desktop_default {
            let full_path = self
                .foreground_exe
                .as_deref()
                .map(normalize_exe_name)
                .filter(|fg| {
                    profile
                        .executables
                        .iter()
                        .any(|cand| normalize_exe_name(cand) == *fg)
                })
                .and_then(|fg| self.exe_paths.get(&fg).cloned())
                .or_else(|| {
                    profile
                        .executables
                        .iter()
                        .find_map(|cand| self.exe_paths.get(&normalize_exe_name(cand)).cloned())
                });
            if let Some(exe_path) = full_path {
                let env = WindowsEnv;
                let existing = env
                    .reg_get_string(RegHive::Hkcu, GPU_PREF_SUBKEY, &exe_path)
                    .ok()
                    .flatten();
                let updated = upsert_gpu_preference_high_perf(existing.as_deref());
                if existing.as_deref() != Some(updated.as_str()) {
                    match env.reg_set_string(RegHive::Hkcu, GPU_PREF_SUBKEY, &exe_path, &updated) {
                        Ok(()) => {
                            self.push_log(
                                Severity::Ok,
                                format!(
                                    "Windows Graphics Preference -> High Performance GPU for {exe_path} (applies on launch)."
                                ),
                            );
                        }
                        Err(err) => {
                            self.push_log(
                                Severity::Warn,
                                format!("Could not set High-Perf GPU preference: {err}"),
                            );
                        }
                    }
                }
            }
        }

        // --- Per-Game Quiet Notifications / Do Not Disturb (zero admin, auto-reverts on desktop/exit) ---
        if profile.quiet_notifications {
            let env = WindowsEnv;
            if self.original_toast_enabled.is_none() {
                let orig = env
                    .reg_get_dword(RegHive::Hkcu, PUSH_NOTIF_SUBKEY, PUSH_NOTIF_VALUE)
                    .ok()
                    .flatten();
                self.original_toast_enabled = Some(orig);
            }
            if env
                .reg_get_dword(RegHive::Hkcu, PUSH_NOTIF_SUBKEY, PUSH_NOTIF_VALUE)
                .ok()
                .flatten()
                != Some(0)
            {
                match env.reg_set_dword(RegHive::Hkcu, PUSH_NOTIF_SUBKEY, PUSH_NOTIF_VALUE, 0) {
                    Ok(()) => {
                        self.push_log(
                            Severity::Ok,
                            format!(
                                "Muted Windows toast notifications while '{}' is active (auto-restores on desktop/exit).",
                                profile.name
                            ),
                        );
                    }
                    Err(err) => {
                        self.push_log(
                            Severity::Warn,
                            format!("Could not mute Windows notifications: {err}"),
                        );
                    }
                }
            }
        } else if self.original_toast_enabled.is_some() {
            self.restore_quiet_notifications(reason);
        }

        // --- Per-Game Background App Mixer Ducking (Core Audio IAudioSessionManager2, zero admin) ---
        if !self.saved_ducked_apps.is_empty() {
            self.restore_ducked_audio_apps(reason);
        }
        if !profile.duck_audio_apps.is_empty() {
            match duck_app_audio_sessions(&profile.duck_audio_apps, profile.duck_audio_volume_pct) {
                Ok(saved) if !saved.is_empty() => {
                    let names: Vec<String> = saved.iter().map(|s| s.exe_name.clone()).collect();
                    let target_lbl = if profile.duck_audio_volume_pct == 0 {
                        "Muted".to_string()
                    } else {
                        format!("{}%", profile.duck_audio_volume_pct)
                    };
                    self.push_log(
                        Severity::Ok,
                        format!(
                            "App Mixer Duck ({}): {} -> {target_lbl} (auto-restores on desktop/exit).",
                            profile.name,
                            names.join(", ")
                        ),
                    );
                    self.saved_ducked_apps = saved;
                }
                Ok(_) => {}
                Err(err) => {
                    self.push_log(
                        Severity::Warn,
                        format!("Could not duck background app audio sessions: {err}"),
                    );
                }
            }
        }

        // --- Keyboard Onboard Profile Switching + Optional Per-Game Tuning Presets ---
        let has_kb_preset = profile
            .keyboard_preset
            .as_ref()
            .is_some_and(|kp| !kp.is_empty());
        if profile.wooting_profile.is_some() || has_kb_preset {
            match detect_all_keyboards() {
                Ok(boards) if !boards.is_empty() => {
                    let mut updated_infos = Vec::with_capacity(boards.len());
                    for kb in boards {
                        let kb_name = kb.name().to_string();
                        let mut cur = kb.read_status();

                        // 1. Switch onboard profile slot first (if requested & supported).
                        if let Some(slot) = profile.wooting_profile {
                            if cur.capabilities.supports_profile_switch {
                                match kb.switch_profile(slot) {
                                    Ok(true) => {
                                        cur = kb.read_status();
                                        let mode_note = if cur.capabilities.profile_switch_volatile
                                        {
                                            "volatile RAM"
                                        } else {
                                            "onboard slot"
                                        };
                                        self.push_log(
                                            Severity::Ok,
                                            format!(
                                                "[{kb_name}] Switched to {} ({reason}, {mode_note}).",
                                                cur.profile_display_name(slot)
                                            ),
                                        );
                                    }
                                    Ok(false) => {}
                                    Err(err) => {
                                        self.push_log(
                                            Severity::Warn,
                                            format!(
                                                "[{kb_name}] Keyboard profile switch failed: {err}"
                                            ),
                                        );
                                    }
                                }
                            }
                        }

                        // 2. Apply optional per-game keyboard tuning overrides (diffed vs live state).
                        if let Some(preset) =
                            profile.keyboard_preset.as_ref().filter(|kp| !kp.is_empty())
                        {
                            let mut any_preset_write = false;

                            if let Some(hz) = preset.polling_rate_hz {
                                let allowed = cur.capabilities.supports_polling_rate_write
                                    && (cur.capabilities.supported_polling_rates_hz.is_empty()
                                        || cur
                                            .capabilities
                                            .supported_polling_rates_hz
                                            .contains(&hz));
                                if allowed && cur.polling_rate_hz != Some(hz) {
                                    match kb.set_polling_rate(hz) {
                                        Ok(confirmed) => {
                                            any_preset_write = true;
                                            self.push_log(
                                                Severity::Ok,
                                                format!(
                                                    "[{kb_name}] Polling rate -> {confirmed} Hz ({reason})."
                                                ),
                                            );
                                        }
                                        Err(err) => {
                                            self.push_log(
                                                Severity::Warn,
                                                format!(
                                                    "[{kb_name}] Polling rate {hz} Hz failed: {err}"
                                                ),
                                            );
                                        }
                                    }
                                }
                            }

                            let req_act = preset.actuation_mm();
                            let req_rt_on = preset.rapid_trigger_enabled;
                            let req_rt_mm = preset.rapid_trigger_mm();
                            if (req_act.is_some() || req_rt_mm.is_some())
                                && cur.capabilities.supports_base_actuation_write
                            {
                                let target_act = req_act.or(cur.actuation_mm).unwrap_or(2.0);
                                let act_diff = req_act.is_some_and(|want| {
                                    cur.actuation_mm.is_none_or(|c| (c - want).abs() >= 0.005)
                                });
                                let rt_en_diff = req_rt_on
                                    .is_some_and(|want| cur.rapid_trigger_enabled != Some(want));
                                let rt_mm_diff = req_rt_mm.is_some_and(|want| {
                                    cur.rapid_trigger_mm
                                        .is_none_or(|c| (c - want).abs() >= 0.005)
                                });
                                if act_diff || rt_en_diff || rt_mm_diff {
                                    match kb.set_base_actuation(target_act, req_rt_on, req_rt_mm) {
                                        Ok(()) => {
                                            any_preset_write = true;
                                            self.push_log(
                                                Severity::Ok,
                                                format!(
                                                    "[{kb_name}] Applied keyboard preset ({}) ({reason}).",
                                                    preset.summary_string()
                                                ),
                                            );
                                        }
                                        Err(err) => {
                                            self.push_log(
                                                Severity::Warn,
                                                format!(
                                                    "[{kb_name}] Actuation/RT preset failed: {err}"
                                                ),
                                            );
                                        }
                                    }
                                }
                            } else if let Some(rt_on) = req_rt_on {
                                if cur.capabilities.supports_rapid_trigger_toggle
                                    && cur.rapid_trigger_enabled != Some(rt_on)
                                {
                                    match kb.set_rapid_trigger_enabled(rt_on) {
                                        Ok(confirmed) => {
                                            any_preset_write = true;
                                            self.push_log(
                                                Severity::Ok,
                                                format!(
                                                    "[{kb_name}] Rapid Trigger -> {} ({reason}).",
                                                    if confirmed { "ON" } else { "OFF" }
                                                ),
                                            );
                                        }
                                        Err(err) => {
                                            self.push_log(
                                                Severity::Warn,
                                                format!("[{kb_name}] Rapid Trigger toggle failed: {err}"),
                                            );
                                        }
                                    }
                                }
                            }

                            if let Some(socd) = preset.socd_enabled {
                                if cur.capabilities.supports_socd_toggle
                                    && cur.socd_enabled != Some(socd)
                                {
                                    match kb.set_socd(socd) {
                                        Ok(confirmed) => {
                                            any_preset_write = true;
                                            self.push_log(
                                                Severity::Ok,
                                                format!(
                                                    "[{kb_name}] SOCD -> {} ({reason}).",
                                                    if confirmed { "ON" } else { "OFF" }
                                                ),
                                            );
                                        }
                                        Err(err) => {
                                            self.push_log(
                                                Severity::Warn,
                                                format!("[{kb_name}] SOCD toggle failed: {err}"),
                                            );
                                        }
                                    }
                                }
                            }

                            if cur.capabilities.supports_debounce_tuning
                                && (preset.debounce_typing_mode.is_some()
                                    || preset.debounce_ms.is_some())
                            {
                                let cur_typing = cur
                                    .debounce_mode
                                    .as_deref()
                                    .is_some_and(|m| m.eq_ignore_ascii_case("typing"));
                                let want_typing = preset.debounce_typing_mode.unwrap_or(cur_typing);
                                let want_ms = preset.debounce_ms.or(cur.debounce_ms).unwrap_or(10);
                                if cur_typing != want_typing || cur.debounce_ms != Some(want_ms) {
                                    match kb.set_debounce_mode(want_typing, want_ms) {
                                        Ok(()) => {
                                            any_preset_write = true;
                                            self.push_log(
                                                Severity::Ok,
                                                format!(
                                                    "[{kb_name}] Debounce -> {} · {want_ms} ms ({reason}).",
                                                    if want_typing { "Typing" } else { "Gaming" }
                                                ),
                                            );
                                        }
                                        Err(err) => {
                                            self.push_log(
                                                Severity::Warn,
                                                format!(
                                                    "[{kb_name}] Debounce preset failed: {err}"
                                                ),
                                            );
                                        }
                                    }
                                }
                            }

                            if any_preset_write {
                                cur = kb.read_status();
                            }
                        }

                        updated_infos.push(cur);
                    }
                    self.keyboards = updated_infos;
                }
                Ok(_) => {
                    self.keyboards.clear();
                }
                Err(err) => {
                    self.push_log(Severity::Warn, format!("Keyboard detection failed: {err}"));
                }
            }
        }

        let mut logs: Vec<(Severity, String)> = Vec::new();
        let mut max_rate_limit_wait: Option<u64> = None;
        let mut any_succeeded = false;
        let mut any_asleep_timeout = false;

        for managed in &mut self.controllers {
            let dev_info = managed.controller.info();
            let dev_name = dev_info.name;
            let caps = managed.controller.capabilities();
            let (raw_desired, used_override) = profile.settings_for_device(&dev_info.id);
            let (mut effective_desired, adjustments) = caps.adapt_settings(raw_desired);
            if self.alerts.low_battery_polling_guard
                && dev_info.support_tier != SupportTier::Simulator
                && caps.polling_rates_hz.contains(&1000)
            {
                if let Some(bat) = managed.last_battery {
                    let threshold = self.alerts.clamped_threshold();
                    if !bat.charging
                        && bat.percent <= threshold
                        && effective_desired
                            .polling_rate_hz
                            .is_some_and(|hz| hz > 1000)
                    {
                        effective_desired.polling_rate_hz = Some(1000);
                        self.battery_guard_active.insert(dev_info.id.clone());
                        logs.push((
                            Severity::Warn,
                            format!(
                                "[{dev_name}] Battery Polling Guard ({}% <= {threshold}%): capped polling rate to 1000 Hz.",
                                bat.percent
                            ),
                        ));
                    }
                }
            }
            let source_tag = if used_override {
                " [mouse override]"
            } else {
                ""
            };
            if !adjustments.is_empty() {
                logs.push((
                    Severity::Info,
                    format!(
                        "[{dev_name}] Adapted '{}' to device limits: {} (set a per-mouse override to customize).",
                        profile.name,
                        adjustments.join(", ")
                    ),
                ));
            }
            match managed
                .controller
                .apply_profile_settings(&effective_desired)
            {
                Ok(ApplyOutcome::SkippedNoSettings) => {
                    any_succeeded = true;
                    logs.push((
                        Severity::Info,
                        format!(
                            "[{dev_name}] '{}'{source_tag}: no mouse changes.",
                            profile.name
                        ),
                    ));
                }
                Ok(ApplyOutcome::SkippedUnchanged { current }) => {
                    any_succeeded = true;
                    managed.last_settings = Some(current.clone());
                    logs.push((
                        Severity::Ok,
                        format!(
                            "[{dev_name}] '{}'{source_tag} ({reason}): already matches ({}) — 0 writes.",
                            profile.name,
                            current.summary_string()
                        ),
                    ));
                }
                Ok(ApplyOutcome::Applied {
                    mode,
                    verified,
                    flash_writes_this_session,
                }) => {
                    any_succeeded = true;
                    managed.last_settings = Some(verified.clone());
                    logs.push((
                        Severity::Ok,
                        format!(
                            "[{dev_name}] Applied '{}'{source_tag} ({reason}) via {mode:?} & verified: {} (flash writes: {flash_writes_this_session}).",
                            profile.name,
                            verified.summary_string()
                        ),
                    ));
                }
                Err(PolledError::FlashRateLimited { wait_secs, .. }) => {
                    max_rate_limit_wait =
                        Some(max_rate_limit_wait.map_or(wait_secs, |cur| cur.max(wait_secs)));
                    let suffix = if self.auto_switch_enabled {
                        format!(" — auto-retry queued in {wait_secs}s.")
                    } else {
                        format!(" — wait {wait_secs}s before next write.")
                    };
                    logs.push((
                        Severity::Warn,
                        format!(
                            "[{dev_name}] '{}' ({reason}): flash-wear cooldown active{suffix}",
                            profile.name
                        ),
                    ));
                }
                Err(err) => {
                    let err_str = err.to_string();
                    let lower = err_str.to_ascii_lowercase();
                    if lower.contains("timed out")
                        || lower.contains("asleep")
                        || lower.contains("wake")
                        || lower.contains("did not reply")
                    {
                        any_asleep_timeout = true;
                        managed.last_settings = None;
                    } else if let Ok(actual) = managed.controller.read_settings() {
                        managed.last_settings = Some(actual);
                    }
                    logs.push((
                        Severity::Warn,
                        format!("[{dev_name}] '{}' ({reason}): {err_str}", profile.name),
                    ));
                }
            }
            managed.refresh_battery(&self.alerts);
        }
        self.last_battery_poll = Instant::now();

        if any_asleep_timeout && max_rate_limit_wait.is_none() {
            let now = Instant::now();
            self.pending_wake_retry = Some((
                profile.id.clone(),
                now + WAKE_RETRY_WINDOW,
                now + WAKE_RETRY_POLL_INTERVAL,
            ));
            logs.push((
                Severity::Info,
                format!(
                    "Wireless mouse appears asleep — will auto-apply '{}' as soon as it wakes up.",
                    profile.name
                ),
            ));
        } else if !any_asleep_timeout {
            self.pending_wake_retry = None;
        }

        let switched_profile = self.active_profile_id != profile.id;
        let mut osd_msg: Option<(String, String)> = None;
        if let Some(wait_secs) = max_rate_limit_wait {
            if self.auto_switch_enabled {
                self.pending_retry = Some((
                    profile.id.clone(),
                    Instant::now() + Duration::from_secs(wait_secs) + Duration::from_millis(250),
                ));
            }
        } else if any_succeeded || self.controllers.is_empty() {
            self.active_profile_id = profile.id.clone();
            self.active_profile_name = profile.name.clone();
            let should_notify =
                switched_profile || reason == "hotkey cycle" || reason == "manual test";
            if should_notify && (self.alerts.profile_switch_toast || self.alerts.osd_pill_enabled) {
                let mut parts = Vec::new();
                if let Some(dpi) = profile.mouse.dpi {
                    parts.push(format!("{dpi} DPI"));
                }
                if let Some(hz) = profile.mouse.polling_rate_hz {
                    parts.push(format!("{hz} Hz"));
                }
                if let Some((_, w, h, hz)) = profile.display_mode_override() {
                    parts.push(format!("{w}×{h}@{hz}Hz"));
                }
                if let Some(slot) = profile.wooting_profile {
                    parts.push(format!("Wooting P{}", slot + 1));
                }
                if let Some(plan) = profile.windows_power_plan {
                    parts.push(plan.label().to_string());
                }
                if let Some(out) = profile.audio_output_device_name.as_deref() {
                    parts.push(out.to_string());
                }
                let subtitle = if parts.is_empty() {
                    reason.to_string()
                } else {
                    parts.join(" · ")
                };
                if self.alerts.profile_switch_toast {
                    notify_profile_switched(&profile.name, &subtitle);
                }
                if self.alerts.osd_pill_enabled {
                    osd_msg = Some((format!("Polled · {}", profile.name), subtitle));
                }
            }
        }

        for (sev, msg) in logs {
            self.push_log(sev, msg);
        }
        osd_msg
    }

    fn restore_quiet_notifications(&mut self, reason: &str) {
        let Some(orig) = self.original_toast_enabled.take() else {
            return;
        };
        let env = WindowsEnv;
        let res = match orig {
            Some(v) => env.reg_set_dword(RegHive::Hkcu, PUSH_NOTIF_SUBKEY, PUSH_NOTIF_VALUE, v),
            None => env.reg_delete_value(RegHive::Hkcu, PUSH_NOTIF_SUBKEY, PUSH_NOTIF_VALUE),
        };
        match res {
            Ok(()) => self.push_log(
                Severity::Info,
                format!("Restored Windows notification settings ({reason})."),
            ),
            Err(err) => self.push_log(
                Severity::Warn,
                format!("Could not restore Windows notifications ({reason}): {err}"),
            ),
        }
    }

    fn restore_ducked_audio_apps(&mut self, reason: &str) {
        if self.saved_ducked_apps.is_empty() {
            return;
        }
        let saved = std::mem::take(&mut self.saved_ducked_apps);
        match restore_app_audio_sessions(&saved) {
            Ok(count) if count > 0 => self.push_log(
                Severity::Info,
                format!(
                    "Restored Windows Volume Mixer level for {count} ducked app{} ({reason}).",
                    if count == 1 { "" } else { "s" }
                ),
            ),
            Ok(_) => {}
            Err(err) => self.push_log(
                Severity::Warn,
                format!("Could not restore ducked app audio levels ({reason}): {err}"),
            ),
        }
    }
}

fn upsert_gpu_preference_high_perf(existing: Option<&str>) -> String {
    let mut tokens: Vec<String> = existing
        .unwrap_or("")
        .split(';')
        .map(str::trim)
        .filter(|t| !t.is_empty() && !t.to_ascii_lowercase().starts_with("gpupreference="))
        .map(str::to_string)
        .collect();
    tokens.push("GpuPreference=2".to_string());
    format!("{};", tokens.join(";"))
}

fn run_generic_keyboard_action<F>(
    state: &mut RuntimeState,
    ui_tx: &Sender<WorkerMsg>,
    device_id: Option<&str>,
    action: F,
) where
    F: Fn(&dyn polled_core::KeyboardDevice) -> polled_core::Result<String>,
{
    match detect_all_keyboards() {
        Ok(boards) if !boards.is_empty() => {
            let mut updated = Vec::with_capacity(boards.len());
            let mut any_ok = false;
            let mut messages = Vec::new();
            for kb in boards {
                let target = device_id.is_none_or(|wanted| kb.id() == wanted);
                if target {
                    match action(kb.as_ref()) {
                        Ok(msg) => {
                            state.push_log(Severity::Ok, msg.clone());
                            messages.push(msg);
                            any_ok = true;
                        }
                        Err(err) => {
                            let msg = format!("[{}] Keyboard update failed: {err}", kb.name());
                            state.push_log(Severity::Warn, msg.clone());
                            messages.push(msg);
                        }
                    }
                }
                updated.push(kb.read_status());
            }
            state.keyboards = updated;
            let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                ok: any_ok,
                message: if messages.is_empty() {
                    "Target keyboard was not found.".to_string()
                } else {
                    messages.join(" ")
                },
            });
        }
        Ok(_) => {
            state.keyboards.clear();
            let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                ok: false,
                message: "No supported keyboard detected.".to_string(),
            });
        }
        Err(err) => {
            let msg = format!("Keyboard detection failed: {err}");
            state.push_log(Severity::Warn, msg.clone());
            let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                ok: false,
                message: msg,
            });
        }
    }
}

fn run_runtime_loop(
    initial_store: ProfileStore,
    initial_auto_switch: bool,
    initial_alerts: TrayAlertConfig,
    initial_audio_cycle_ids: Vec<String>,
    cmd_rx: Receiver<ProfileRuntimeCmd>,
    ui_tx: Sender<WorkerMsg>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let debounce = Duration::from_millis(initial_store.debounce_ms);
    let desktop_name = initial_store
        .profiles
        .iter()
        .find(|p| p.is_desktop_default)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "Desktop (Default)".to_string());

    let mut state = RuntimeState {
        store: initial_store,
        alerts: initial_alerts,
        auto_switch_enabled: initial_auto_switch,
        profile_locked: false,
        selection: BackendSelection::AutoConnected,
        detected_hardware_info: Vec::new(),
        controllers: Vec::new(),
        keyboards: Vec::new(),
        last_topology: Vec::new(),
        last_topology_check: Instant::now(),
        pending_hotplug_at: None,
        last_battery_poll: Instant::now(),
        audio_outputs: Vec::new(),
        audio_inputs: Vec::new(),
        audio_cycle_device_ids: initial_audio_cycle_ids,
        mic_muted: None,
        last_audio_refresh: Instant::now(),
        mic_lock_target: None,
        last_mic_lock_check: Instant::now(),
        displays: Vec::new(),
        original_display_mode: None,
        active_display_override: None,
        pending_display_test_revert: None,
        debouncer: FocusDebouncer::new(debounce),
        watcher: None,
        focus_rx: None,
        foreground_exe: None,
        exe_paths: HashMap::new(),
        active_profile_id: "desktop".to_string(),
        active_profile_name: desktop_name,
        pending_retry: None,
        pending_wake_retry: None,
        original_toast_enabled: None,
        saved_ducked_apps: Vec::new(),
        battery_guard_active: HashSet::new(),
        log: Vec::new(),
    };

    state.rebuild_controllers(BackendSelection::AutoConnected, "startup auto-detect");
    state.refresh_audio_devices();
    state.refresh_displays();
    if initial_auto_switch {
        state.start_watcher();
    }
    state.emit(&ui_tx, &wake);

    loop {
        let mut changed = state.check_hotplug();
        let (batt_changed, batt_osd) = state.poll_batteries();
        if batt_changed {
            changed = true;
        }
        if let Some((title, subtitle)) = batt_osd {
            let _ = ui_tx.send(WorkerMsg::ShowOsdPill { title, subtitle });
        }
        if state.poll_audio_and_mic_lock() {
            changed = true;
        }
        if state
            .pending_display_test_revert
            .as_ref()
            .is_some_and(|(_, _, _, _, deadline)| Instant::now() >= *deadline)
        {
            if let Some(msg) = state.revert_test_display_if_pending("10s auto-revert failsafe") {
                let _ = ui_tx.send(WorkerMsg::DisplayTestReverted(msg));
                changed = true;
            }
        }

        if let Some(rx) = &state.focus_rx {
            while let Ok(evt) = rx.try_recv() {
                if let (Some(name), Some(path)) = (&evt.exe_name, &evt.exe_path) {
                    state.exe_paths.insert(name.clone(), path.clone());
                }
                state.foreground_exe = evt.exe_name.clone();
                state
                    .debouncer
                    .observe(evt.exe_name.as_deref(), evt.observed_at);
                changed = true;
            }
        }

        // Auto-retry a missed profile switch once a sleeping wireless mouse wakes up.
        if let Some((wake_profile_id, expires_at, next_check_at)) = state.pending_wake_retry.clone()
        {
            let now = Instant::now();
            if now >= expires_at {
                state.pending_wake_retry = None;
            } else if now >= next_check_at {
                let mut woke = false;
                for m in &mut state.controllers {
                    if m.last_settings.is_none() {
                        if let Ok(actual) = m.controller.read_settings() {
                            m.last_settings = Some(actual);
                            woke = true;
                        }
                    }
                }
                if woke {
                    state.pending_wake_retry = None;
                    let still_wanted = !state.auto_switch_enabled
                        || state.profile_locked
                        || match_profile(&state.store, state.foreground_exe.as_deref())
                            .is_some_and(|p| p.id == wake_profile_id);
                    if still_wanted {
                        if let Some(profile) = state
                            .store
                            .profiles
                            .iter()
                            .find(|p| p.id == wake_profile_id)
                            .cloned()
                        {
                            if let Some((title, subtitle)) =
                                state.apply_profile(&profile, "mouse wake retry")
                            {
                                let _ = ui_tx.send(WorkerMsg::ShowOsdPill { title, subtitle });
                            }
                            changed = true;
                        }
                    }
                } else {
                    state.pending_wake_retry =
                        Some((wake_profile_id, expires_at, now + WAKE_RETRY_POLL_INTERVAL));
                }
            }
        }

        if state.auto_switch_enabled && !state.profile_locked {
            let now = Instant::now();
            if let Some(committed_exe) = state.debouncer.poll_ready(now) {
                let matched = match_profile(&state.store, committed_exe.as_deref()).cloned();
                if let Some(profile) = matched {
                    if profile.id == state.active_profile_id {
                        if state.pending_retry.take().is_some() {
                            state.push_log(
                                Severity::Info,
                                format!(
                                    "Cancelled queued flash retry — focus returned to '{}'.",
                                    profile.name
                                ),
                            );
                            changed = true;
                        }
                    } else if state
                        .pending_retry
                        .as_ref()
                        .is_some_and(|(queued_id, _)| queued_id == &profile.id)
                    {
                        // Already queued for this profile once cooldown clears; avoid duplicate log noise.
                    } else {
                        let reason = committed_exe
                            .as_deref()
                            .map(|e| format!("focus: {e}"))
                            .unwrap_or_else(|| "focus: desktop".to_string());
                        if let Some((title, subtitle)) = state.apply_profile(&profile, &reason) {
                            let _ = ui_tx.send(WorkerMsg::ShowOsdPill { title, subtitle });
                        }
                        changed = true;
                    }
                }
            } else if let Some((retry_profile_id, retry_at)) = state.pending_retry.clone() {
                if now >= retry_at {
                    state.pending_retry = None;
                    let still_matches =
                        match_profile(&state.store, state.foreground_exe.as_deref())
                            .is_some_and(|p| p.id == retry_profile_id);
                    if still_matches {
                        if let Some(profile) = state
                            .store
                            .profiles
                            .iter()
                            .find(|p| p.id == retry_profile_id)
                            .cloned()
                        {
                            if let Some((title, subtitle)) =
                                state.apply_profile(&profile, "cooldown retry")
                            {
                                let _ = ui_tx.send(WorkerMsg::ShowOsdPill { title, subtitle });
                            }
                            changed = true;
                        }
                    }
                }
            }
        }

        if changed {
            state.emit(&ui_tx, &wake);
        }

        let mut wait = if !state.auto_switch_enabled {
            HOTPLUG_FALLBACK_POLL_INTERVAL
        } else {
            state
                .debouncer
                .time_until_ready(Instant::now())
                .map(|rem| rem.min(Duration::from_millis(100)))
                .unwrap_or(Duration::from_millis(150))
        };
        if let Some(hp_at) = state.pending_hotplug_at {
            let rem = hp_at.saturating_duration_since(Instant::now());
            wait = wait.min(rem.max(Duration::from_millis(10)));
        }
        if let Some((_, _, _, _, deadline)) = state.pending_display_test_revert {
            let rem = deadline.saturating_duration_since(Instant::now());
            wait = wait.min(rem.max(Duration::from_millis(50)));
        }
        if let Some((_, _, next_wake)) = state.pending_wake_retry {
            let rem = next_wake.saturating_duration_since(Instant::now());
            wait = wait.min(rem.max(Duration::from_millis(100)));
        }

        match cmd_rx.recv_timeout(wait) {
            Ok(ProfileRuntimeCmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                let _ = state.revert_test_display_if_pending("shutdown");
                state.restore_original_display("shutdown");
                state.restore_quiet_notifications("shutdown");
                state.restore_ducked_audio_apps("shutdown");
                state.stop_watcher();
                break;
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Ok(ProfileRuntimeCmd::UsbDeviceChanged) => {
                state.pending_hotplug_at = Some(Instant::now() + HOTPLUG_EVENT_DEBOUNCE);
            }
            Ok(ProfileRuntimeCmd::UpdateAlertConfig(next_alerts)) => {
                state.alerts = next_alerts;
                for m in &mut state.controllers {
                    m.low_battery_notified = false;
                }
                let (_, osd) = state.evaluate_battery_polling_guard();
                if let Some((title, subtitle)) = osd {
                    let _ = ui_tx.send(WorkerMsg::ShowOsdPill { title, subtitle });
                }
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::QuickSetMousePolling(hz)) => {
                let desired = MouseSettings {
                    polling_rate_hz: Some(hz),
                    ..Default::default()
                };
                state.quick_apply_mouse_setting(&desired, &format!("{hz} Hz"));
                if state.alerts.osd_pill_enabled {
                    let _ = ui_tx.send(WorkerMsg::ShowOsdPill {
                        title: "Polled · Mouse Quick-Set".to_string(),
                        subtitle: format!("Polling Rate -> {hz} Hz"),
                    });
                }
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::QuickSetMouseDpi(dpi)) => {
                let desired = MouseSettings {
                    dpi: Some(dpi),
                    ..Default::default()
                };
                state.quick_apply_mouse_setting(&desired, &format!("{dpi} DPI"));
                if state.alerts.osd_pill_enabled {
                    let _ = ui_tx.send(WorkerMsg::ShowOsdPill {
                        title: "Polled · Mouse Quick-Set".to_string(),
                        subtitle: format!("DPI -> {dpi}"),
                    });
                }
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::RefreshMouseAndBattery) => {
                state.force_refresh_mouse_and_battery();
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::RefreshWooting) => {
                state.refresh_keyboards();
                let count = state.keyboards.len();
                let msg = if count == 0 {
                    "No supported keyboards detected (close vendor web/desktop apps if open)."
                        .to_string()
                } else {
                    format!(
                        "Refreshed {count} supported keyboard{}.",
                        if count == 1 { "" } else { "s" }
                    )
                };
                state.push_log(Severity::Info, msg.clone());
                let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                    ok: count > 0,
                    message: msg,
                });
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::CycleWootingProfile) => {
                match detect_all_keyboards() {
                    Ok(boards) if !boards.is_empty() => {
                        let mut updated = Vec::with_capacity(boards.len());
                        let mut osd_sub: Option<String> = None;
                        let mut any_ok = false;
                        let mut messages = Vec::new();
                        for kb in boards {
                            let before = kb.read_status();
                            let slots: Vec<u8> = before.profiles.iter().map(|p| p.index).collect();
                            if before.capabilities.supports_profile_switch && slots.len() >= 2 {
                                let cur = before.active_profile.unwrap_or(slots[0]);
                                let pos = slots.iter().position(|&s| s == cur).unwrap_or(0);
                                let next_slot = slots[(pos + 1) % slots.len()];
                                let kb_name = kb.name().to_string();
                                match kb.switch_profile(next_slot) {
                                    Ok(_) => {
                                        let full = kb.read_status();
                                        let lbl = full.profile_display_name(next_slot);
                                        let msg = format!("[{kb_name}] Cycled to {lbl} (hotkey).");
                                        state.push_log(Severity::Ok, msg.clone());
                                        messages.push(msg);
                                        osd_sub = Some(format!("{kb_name} -> {lbl}"));
                                        any_ok = true;
                                        updated.push(full);
                                    }
                                    Err(err) => {
                                        let msg =
                                            format!("[{kb_name}] Keyboard cycle failed: {err}");
                                        state.push_log(Severity::Warn, msg.clone());
                                        messages.push(msg);
                                        updated.push(before);
                                    }
                                }
                            } else {
                                updated.push(before);
                            }
                        }
                        state.keyboards = updated;
                        if let Some(sub) = osd_sub {
                            if state.alerts.osd_pill_enabled {
                                let _ = ui_tx.send(WorkerMsg::ShowOsdPill {
                                    title: "Polled · Keyboard".to_string(),
                                    subtitle: sub,
                                });
                            }
                        }
                        if !messages.is_empty() {
                            let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                                ok: any_ok,
                                message: messages.join(" "),
                            });
                        }
                    }
                    Ok(_) => {
                        state.keyboards.clear();
                    }
                    Err(err) => {
                        state.push_log(
                            Severity::Warn,
                            format!("Keyboard detection failed on hotkey cycle: {err}"),
                        );
                    }
                }
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::SwitchWootingProfile { device_id, slot }) => {
                match detect_all_keyboards() {
                    Ok(boards) if !boards.is_empty() => {
                        let mut updated = Vec::with_capacity(boards.len());
                        let mut any_ok = false;
                        let mut messages = Vec::new();
                        let mut osd_sub: Option<String> = None;
                        for kb in boards {
                            let target =
                                device_id.as_deref().is_none_or(|wanted| kb.id() == wanted);
                            if target {
                                let kb_name = kb.name().to_string();
                                match kb.switch_profile(slot) {
                                    Ok(switched) => {
                                        let full = kb.read_status();
                                        let slot_name = full.profile_display_name(slot);
                                        let note = if full.capabilities.profile_switch_volatile {
                                            "volatile RAM, zero flash wear"
                                        } else {
                                            "verified read-back"
                                        };
                                        let msg = if switched {
                                            format!("[{kb_name}] Switched to {slot_name} ({note}).")
                                        } else {
                                            format!("[{kb_name}] Already on {slot_name}.")
                                        };
                                        state.push_log(Severity::Ok, msg.clone());
                                        messages.push(msg);
                                        if switched {
                                            osd_sub = Some(format!("{kb_name} -> {slot_name}"));
                                        }
                                        any_ok = true;
                                        updated.push(full);
                                    }
                                    Err(err) => {
                                        let msg = format!("[{kb_name}] Switch failed: {err}");
                                        state.push_log(Severity::Warn, msg.clone());
                                        messages.push(msg);
                                        updated.push(kb.read_status());
                                    }
                                }
                            } else {
                                updated.push(kb.read_status());
                            }
                        }
                        state.keyboards = updated;
                        if let Some(sub) = osd_sub {
                            if state.alerts.osd_pill_enabled {
                                let _ = ui_tx.send(WorkerMsg::ShowOsdPill {
                                    title: "Polled · Keyboard".to_string(),
                                    subtitle: sub,
                                });
                            }
                        }
                        let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                            ok: any_ok,
                            message: if messages.is_empty() {
                                "Target keyboard was not found.".to_string()
                            } else {
                                messages.join(" ")
                            },
                        });
                    }
                    Ok(_) => {
                        state.keyboards.clear();
                        let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                            ok: false,
                            message: "No supported keyboard detected.".to_string(),
                        });
                    }
                    Err(err) => {
                        let msg = format!("Keyboard detection failed: {err}");
                        state.push_log(Severity::Warn, msg.clone());
                        let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                            ok: false,
                            message: msg,
                        });
                    }
                }
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::WriteWootingAnalogBase { device_id, patch }) => {
                match detect_wooting_keyboards() {
                    Ok(boards) if !boards.is_empty() => {
                        let mut any_ok = false;
                        let mut messages = Vec::new();
                        for kb in boards {
                            let target =
                                device_id.as_deref().is_none_or(|wanted| kb.id() == wanted);
                            if target {
                                let kb_name = kb.name().to_string();
                                match kb.write_analog_defaults(&patch) {
                                    Ok(()) => {
                                        let full = kb.read_full_status();
                                        let slot_lbl = full
                                            .active_profile
                                            .map(|s| full.profile_display_name(s))
                                            .unwrap_or_else(|| "active slot".to_string());
                                        let msg = format!(
                                            "[{kb_name}] Saved base analog & Rapid Trigger settings to {slot_lbl} (read-back verified)."
                                        );
                                        state.push_log(Severity::Ok, msg.clone());
                                        messages.push(msg);
                                        any_ok = true;
                                    }
                                    Err(err) => {
                                        let msg = format!("[{kb_name}] Analog save failed: {err}");
                                        state.push_log(Severity::Warn, msg.clone());
                                        messages.push(msg);
                                    }
                                }
                            }
                        }
                        state.refresh_keyboards();
                        let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                            ok: any_ok,
                            message: if messages.is_empty() {
                                "Target Wooting keyboard was not found.".to_string()
                            } else {
                                messages.join(" ")
                            },
                        });
                    }
                    Ok(_) => {
                        state.refresh_keyboards();
                        let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                            ok: false,
                            message: "No Wooting keyboard detected.".to_string(),
                        });
                    }
                    Err(err) => {
                        let msg = format!("Wooting detection failed: {err}");
                        state.push_log(Severity::Warn, msg.clone());
                        let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                            ok: false,
                            message: msg,
                        });
                    }
                }
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::WriteWootingPerKeyActuation { device_id, updates }) => {
                match detect_wooting_keyboards() {
                    Ok(boards) if !boards.is_empty() => {
                        let mut any_ok = false;
                        let mut messages = Vec::new();
                        for kb in boards {
                            let target =
                                device_id.as_deref().is_none_or(|wanted| kb.id() == wanted);
                            if target {
                                let kb_name = kb.name().to_string();
                                match kb.write_per_key_actuation(&updates) {
                                    Ok(()) => {
                                        let full = kb.read_full_status();
                                        let slot_lbl = full
                                            .active_profile
                                            .map(|s| full.profile_display_name(s))
                                            .unwrap_or_else(|| "active slot".to_string());
                                        let msg = format!(
                                            "[{kb_name}] Updated {} per-key actuation override{} on {slot_lbl} (read-back verified).",
                                            updates.len(),
                                            if updates.len() == 1 { "" } else { "s" }
                                        );
                                        state.push_log(Severity::Ok, msg.clone());
                                        messages.push(msg);
                                        any_ok = true;
                                    }
                                    Err(err) => {
                                        let msg = format!(
                                            "[{kb_name}] Per-key actuation save failed: {err}"
                                        );
                                        state.push_log(Severity::Warn, msg.clone());
                                        messages.push(msg);
                                    }
                                }
                            }
                        }
                        state.refresh_keyboards();
                        let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                            ok: any_ok,
                            message: if messages.is_empty() {
                                "Target Wooting keyboard was not found.".to_string()
                            } else {
                                messages.join(" ")
                            },
                        });
                    }
                    Ok(_) => {
                        state.refresh_keyboards();
                        let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                            ok: false,
                            message: "No Wooting keyboard detected.".to_string(),
                        });
                    }
                    Err(err) => {
                        let msg = format!("Wooting detection failed: {err}");
                        state.push_log(Severity::Warn, msg.clone());
                        let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                            ok: false,
                            message: msg,
                        });
                    }
                }
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::WriteWootingGlobalSettings { device_id, patch }) => {
                match detect_wooting_keyboards() {
                    Ok(boards) if !boards.is_empty() => {
                        let mut any_ok = false;
                        let mut messages = Vec::new();
                        for kb in boards {
                            let target =
                                device_id.as_deref().is_none_or(|wanted| kb.id() == wanted);
                            if target {
                                let kb_name = kb.name().to_string();
                                match kb.write_global_settings(&patch) {
                                    Ok(()) => {
                                        let msg = format!(
                                            "[{kb_name}] Applied Global Settings (read-back verified)."
                                        );
                                        state.push_log(Severity::Ok, msg.clone());
                                        messages.push(msg);
                                        any_ok = true;
                                    }
                                    Err(err) => {
                                        let msg = format!(
                                            "[{kb_name}] Global Settings write failed: {err}"
                                        );
                                        state.push_log(Severity::Warn, msg.clone());
                                        messages.push(msg);
                                    }
                                }
                            }
                        }
                        state.refresh_keyboards();
                        let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                            ok: any_ok,
                            message: if messages.is_empty() {
                                "Target Wooting keyboard was not found.".to_string()
                            } else {
                                messages.join(" ")
                            },
                        });
                    }
                    Ok(_) => {
                        state.refresh_keyboards();
                        let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                            ok: false,
                            message: "No Wooting keyboard detected.".to_string(),
                        });
                    }
                    Err(err) => {
                        let msg = format!("Wooting detection failed: {err}");
                        state.push_log(Severity::Warn, msg.clone());
                        let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                            ok: false,
                            message: msg,
                        });
                    }
                }
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::SetKeyboardPollingRate { device_id, hz }) => {
                run_generic_keyboard_action(&mut state, &ui_tx, device_id.as_deref(), |kb| {
                    let confirmed = kb.set_polling_rate(hz)?;
                    Ok(format!(
                        "[{}] Polling rate -> {confirmed} Hz (read-back verified).",
                        kb.name()
                    ))
                });
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::SetKeyboardBaseActuation {
                device_id,
                actuation_mm,
                rt_enabled,
                rt_mm,
            }) => {
                run_generic_keyboard_action(&mut state, &ui_tx, device_id.as_deref(), |kb| {
                    kb.set_base_actuation(actuation_mm, rt_enabled, rt_mm)?;
                    Ok(format!(
                        "[{}] Saved actuation {:.2} mm (read-back verified).",
                        kb.name(),
                        actuation_mm
                    ))
                });
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::SetKeyboardRapidTrigger { device_id, enabled }) => {
                run_generic_keyboard_action(&mut state, &ui_tx, device_id.as_deref(), |kb| {
                    let confirmed = kb.set_rapid_trigger_enabled(enabled)?;
                    Ok(format!(
                        "[{}] Rapid Trigger -> {} (read-back verified).",
                        kb.name(),
                        if confirmed { "ON" } else { "OFF" }
                    ))
                });
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::SetKeyboardSystemMode {
                device_id,
                mac_mode,
            }) => {
                run_generic_keyboard_action(&mut state, &ui_tx, device_id.as_deref(), |kb| {
                    let mode = kb.set_system_mode(mac_mode)?;
                    Ok(format!(
                        "[{}] System mode -> {mode} (read-back verified).",
                        kb.name()
                    ))
                });
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::SetKeyboardSocd { device_id, enabled }) => {
                run_generic_keyboard_action(&mut state, &ui_tx, device_id.as_deref(), |kb| {
                    let confirmed = kb.set_socd(enabled)?;
                    Ok(format!(
                        "[{}] SOCD / Key Priority -> {} (read-back verified).",
                        kb.name(),
                        if confirmed { "ON" } else { "OFF" }
                    ))
                });
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::SetKeyboardDebounce {
                device_id,
                typing_mode,
                debounce_ms,
            }) => {
                run_generic_keyboard_action(&mut state, &ui_tx, device_id.as_deref(), |kb| {
                    kb.set_debounce_mode(typing_mode, debounce_ms)?;
                    Ok(format!(
                        "[{}] Debounce -> {} · {debounce_ms} ms (read-back verified).",
                        kb.name(),
                        if typing_mode { "Typing" } else { "Gaming" }
                    ))
                });
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::ReadFromMouseForEditor(dev_id)) => {
                let (device_name, settings, error) =
                    state.read_from_mouse_for_editor(dev_id.as_deref());
                let _ = ui_tx.send(WorkerMsg::ReadFromMouseFinished {
                    device_name,
                    settings,
                    error,
                });
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::SetAutoSwitch(enabled)) => {
                state.auto_switch_enabled = enabled;
                if enabled {
                    state.start_watcher();
                } else {
                    state.stop_watcher();
                    state.pending_retry = None;
                    state.push_log(Severity::Info, "Paused automatic per-game switching.");
                }
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::SetProfileLock(locked)) => {
                state.profile_locked = locked;
                state.pending_retry = None;
                if locked {
                    state.push_log(
                        Severity::Info,
                        format!(
                            "Locked active profile '{}' (auto-switch paused until unlocked).",
                            state.active_profile_name
                        ),
                    );
                } else {
                    state.push_log(
                        Severity::Info,
                        "Unlocked active profile (auto-switch resumed).".to_string(),
                    );
                }
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::CycleNextProfile) => {
                if !state.store.profiles.is_empty() {
                    let cur_idx = state
                        .store
                        .profiles
                        .iter()
                        .position(|p| p.id == state.active_profile_id)
                        .unwrap_or(0);
                    let next_idx = (cur_idx + 1) % state.store.profiles.len();
                    let next_profile = state.store.profiles[next_idx].clone();
                    if let Some((title, subtitle)) =
                        state.apply_profile(&next_profile, "hotkey cycle")
                    {
                        let _ = ui_tx.send(WorkerMsg::ShowOsdPill { title, subtitle });
                    }
                    state.emit(&ui_tx, &wake);
                }
            }
            Ok(ProfileRuntimeCmd::SwitchAudioOutput(endpoint_id)) => {
                state.switch_audio_output(&endpoint_id, "tray switch");
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::CycleAudioOutput) => {
                state.refresh_audio_devices();
                if let Some((next_id, next_name)) =
                    select_next_audio_output(&state.audio_outputs, &state.audio_cycle_device_ids)
                {
                    let next_id = next_id.to_string();
                    let next_name = next_name.to_string();
                    state.switch_audio_output(&next_id, "hotkey cycle");
                    if state.alerts.osd_pill_enabled {
                        let _ = ui_tx.send(WorkerMsg::ShowOsdPill {
                            title: "Polled · Audio Output".to_string(),
                            subtitle: next_name,
                        });
                    }
                } else if !state.audio_cycle_device_ids.is_empty() {
                    state.push_log(
                        Severity::Info,
                        "Audio output cycle skipped: fewer than 2 pinned playback devices are currently active.",
                    );
                } else {
                    state.push_log(
                        Severity::Info,
                        "Audio output cycle skipped: fewer than 2 active playback devices.",
                    );
                }
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::UpdateAudioCycleDevices(ids)) => {
                state.audio_cycle_device_ids = ids;
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::ToggleMicMute) => {
                let env = WindowsEnv;
                match env.toggle_audio_endpoint_mute(AudioFlow::Capture, None) {
                    Ok((mic_name, now_muted)) => {
                        state.mic_muted = Some(now_muted);
                        let state_lbl = if now_muted { "MUTED" } else { "LIVE" };
                        state.push_log(
                            Severity::Ok,
                            format!("Microphone -> {state_lbl} ({mic_name})."),
                        );
                        if state.alerts.osd_pill_enabled {
                            let _ = ui_tx.send(WorkerMsg::ShowOsdPill {
                                title: "Polled · Microphone".to_string(),
                                subtitle: format!("{state_lbl} · {mic_name}"),
                            });
                        }
                    }
                    Err(err) => {
                        state.push_log(
                            Severity::Warn,
                            format!("Could not toggle microphone mute: {err}"),
                        );
                    }
                }
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::RefreshAudioDevices) => {
                state.refresh_audio_devices();
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::RefreshDisplays) => {
                state.refresh_displays();
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::TestDisplayMode {
                device_name,
                width,
                height,
                refresh_hz,
            }) => {
                let (resolved_dev, applied_label, previous_label, error) = state
                    .start_test_display_mode(device_name.as_deref(), width, height, refresh_hz);
                let _ = ui_tx.send(WorkerMsg::DisplayTestStarted {
                    device_name: resolved_dev,
                    applied_label,
                    previous_label,
                    error,
                });
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::KeepTestDisplayMode) => {
                if let Some((dev, w, h, hz, _)) = state.pending_display_test_revert.take() {
                    if state.original_display_mode.is_none() {
                        state.original_display_mode = Some((dev.clone(), w, h, hz));
                    }
                    state.push_log(
                        Severity::Ok,
                        format!(
                            "Confirmed temporary display test on {dev} (will still restore {w}×{h} @ {hz} Hz on desktop/exit)."
                        ),
                    );
                }
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::RevertTestDisplayMode) => {
                if let Some(msg) = state.revert_test_display_if_pending("user/countdown") {
                    let _ = ui_tx.send(WorkerMsg::DisplayTestReverted(msg));
                }
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::UpdateStore(new_store)) => {
                state
                    .debouncer
                    .set_delay(Duration::from_millis(new_store.debounce_ms));
                let _ = new_store.save_atomic(&AppPaths::default_user_dir().profiles_path());
                state.store = new_store;
                state.push_log(Severity::Info, "Saved updated profiles.json.");
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::SelectBackend(sel)) => {
                state.rebuild_controllers(sel, "manual selection");
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::RedetectHardware) => {
                let sel = state.selection.clone();
                state.rebuild_controllers(sel, "manual redetect");
                state.emit(&ui_tx, &wake);
            }
            Ok(ProfileRuntimeCmd::ApplyProfileNow(profile_id)) => {
                if let Some(profile) = state
                    .store
                    .profiles
                    .iter()
                    .find(|p| p.id == profile_id)
                    .cloned()
                {
                    if let Some((title, subtitle)) = state.apply_profile(&profile, "manual test") {
                        let _ = ui_tx.send(WorkerMsg::ShowOsdPill { title, subtitle });
                    }
                    state.emit(&ui_tx, &wake);
                }
            }
        }
    }
}

/// Selects the next playback endpoint for `CycleAudioOutput`, honoring optional user-pinned
/// endpoint IDs (`Headphones <-> Speakers`, skipping unpinned HDMI/monitor speakers).
pub(crate) fn select_next_audio_output<'a>(
    outputs: &'a [(String, String, bool)],
    pinned_ids: &[String],
) -> Option<(&'a str, &'a str)> {
    let eligible: Vec<&(String, String, bool)> = if pinned_ids.is_empty() {
        outputs.iter().collect()
    } else {
        outputs
            .iter()
            .filter(|(id, _, _)| pinned_ids.iter().any(|p| p.eq_ignore_ascii_case(id)))
            .collect()
    };

    if eligible.len() < 2 {
        return None;
    }

    let next_idx = match eligible.iter().position(|(_, _, is_def)| *is_def) {
        Some(cur) => (cur + 1) % eligible.len(),
        None => 0,
    };
    let chosen = eligible[next_idx];
    Some((chosen.0.as_str(), chosen.1.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_cycle_honors_pinned_devices_and_skips_hdmi_outputs() {
        let outputs = vec![
            (
                "id_headset".to_string(),
                "Headphones (DAC)".to_string(),
                true,
            ),
            (
                "id_hdmi1".to_string(),
                "ASUS XG27AQDMG (NVIDIA)".to_string(),
                false,
            ),
            (
                "id_speakers".to_string(),
                "Desktop Speakers".to_string(),
                false,
            ),
            (
                "id_hdmi2".to_string(),
                "BenQ Monitor Audio".to_string(),
                false,
            ),
        ];

        // Unpinned cycles all 4 in order.
        assert_eq!(
            select_next_audio_output(&outputs, &[]),
            Some(("id_hdmi1", "ASUS XG27AQDMG (NVIDIA)"))
        );

        // Pinned strictly toggles Headphones <-> Speakers, skipping both HDMI monitors.
        let pins = vec!["id_headset".to_string(), "id_speakers".to_string()];
        assert_eq!(
            select_next_audio_output(&outputs, &pins),
            Some(("id_speakers", "Desktop Speakers"))
        );

        // If user is currently on an unpinned HDMI monitor, cycle snaps straight to first pin.
        let on_hdmi = vec![
            (
                "id_headset".to_string(),
                "Headphones (DAC)".to_string(),
                false,
            ),
            (
                "id_hdmi1".to_string(),
                "ASUS XG27AQDMG (NVIDIA)".to_string(),
                true,
            ),
            (
                "id_speakers".to_string(),
                "Desktop Speakers".to_string(),
                false,
            ),
        ];
        assert_eq!(
            select_next_audio_output(&on_hdmi, &pins),
            Some(("id_headset", "Headphones (DAC)"))
        );

        // Fewer than 2 active pinned devices returns None cleanly.
        let one_pin = vec!["id_headset".to_string(), "id_unplugged".to_string()];
        assert_eq!(select_next_audio_output(&outputs, &one_pin), None);
    }
}
