// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Background profile-switching runtime connecting `ForegroundWatcher` -> `FocusDebouncer`
//! -> `match_profile` -> multi-mouse `SafeDeviceController` (+ optional Windows pointer speed),
//! with automatic USB hot-plug discovery across multiple mice.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use chrono::Local;
use eframe::egui;
use polled_core::win::{AudioFlow, DisplayModeInfo, DisplayResolutionMode, SystemEnv, WindowsEnv};
use polled_core::{
    detect_hardware_devices, detect_wooting_keyboards, hid_topology_fingerprint, match_profile,
    AppPaths, ApplyOutcome, BatteryStatus, DeviceCapabilities, DeviceInfo, DryRunDevice,
    FocusDebouncer, FocusEvent, ForegroundWatcher, GameProfile, MouseSettings, PolledError,
    ProfileStore, SafeDeviceController, Severity, TrayAlertConfig, WootingAnalogBasePatch,
    WootingGlobalSettingsPatch, WootingKeyboardInfo,
};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendSelection {
    /// Automatically detects USB hot-plug changes and applies profiles to all connected supported mice.
    AutoConnected,
    HardwareIndex(usize),
    DryRunVolatile,
    DryRunFlashOnly,
}

#[derive(Debug, Clone)]
pub struct ProfileLogEntry {
    pub timestamp: String,
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ConnectedDeviceStatus {
    pub info: DeviceInfo,
    pub caps: DeviceCapabilities,
    pub settings: Option<MouseSettings>,
    pub battery: Option<BatteryStatus>,
}

#[derive(Debug, Clone)]
pub struct DisplayOption {
    pub info: DisplayModeInfo,
    pub modes: Vec<DisplayResolutionMode>,
}

#[derive(Debug, Clone)]
pub struct ProfileRuntimeSnapshot {
    pub auto_switch_enabled: bool,
    pub profile_locked: bool,
    pub selection: BackendSelection,
    pub detected_hardware: Vec<DeviceInfo>,
    pub active_devices: Vec<ConnectedDeviceStatus>,
    /// Connected Wooting keyboards and their full verified live telemetry snapshot.
    pub wooting_keyboards: Vec<WootingKeyboardInfo>,
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
    /// Active Mic Gain Lock target (`0..=100%`) when the current profile enforces mic gain.
    pub mic_lock_active: Option<u8>,
    /// Attached monitors and their supported `(width, height, refresh_hz)` modes.
    pub displays: Vec<DisplayOption>,
    /// Human-readable label of an active temporary display override (auto-reverts on desktop/exit).
    pub active_display_override: Option<String>,
    pub total_flash_writes_session: u64,
    pub log: Vec<ProfileLogEntry>,
}

pub enum ProfileRuntimeCmd {
    SetAutoSwitch(bool),
    SetProfileLock(bool),
    CycleNextProfile,
    SwitchAudioOutput(String),
    CycleAudioOutput,
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
    SwitchWootingProfile {
        device_id: Option<String>,
        slot: u8,
    },
    WriteWootingAnalogBase {
        device_id: Option<String>,
        patch: WootingAnalogBasePatch,
    },
    WriteWootingGlobalSettings {
        device_id: Option<String>,
        patch: WootingGlobalSettingsPatch,
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

pub struct ProfileRuntimeHandle {
    cmd_tx: Sender<ProfileRuntimeCmd>,
    _hotplug_watcher: UsbHotplugWatcher,
}

impl ProfileRuntimeHandle {
    pub fn start(
        initial_store: ProfileStore,
        initial_auto_switch: bool,
        initial_alerts: TrayAlertConfig,
        ui_tx: Sender<WorkerMsg>,
        ctx: egui::Context,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let hotplug_watcher = UsbHotplugWatcher::start(cmd_tx.clone());

        std::thread::Builder::new()
            .name("polled-profile-runtime".to_string())
            .spawn(move || {
                run_runtime_loop(
                    initial_store,
                    initial_auto_switch,
                    initial_alerts,
                    cmd_rx,
                    ui_tx,
                    ctx,
                );
            })
            .expect("failed to spawn profile runtime thread");

        Self {
            cmd_tx,
            _hotplug_watcher: hotplug_watcher,
        }
    }

    pub fn send(&self, cmd: ProfileRuntimeCmd) {
        let _ = self.cmd_tx.send(cmd);
    }

    pub fn command_sender(&self) -> Sender<ProfileRuntimeCmd> {
        self.cmd_tx.clone()
    }
}

impl Drop for ProfileRuntimeHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(ProfileRuntimeCmd::Shutdown);
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
    wooting_keyboards: Vec<WootingKeyboardInfo>,
    last_topology: Vec<(u16, u16, u16, u16)>,
    last_topology_check: Instant,
    pending_hotplug_at: Option<Instant>,
    last_battery_poll: Instant,
    audio_outputs: Vec<(String, String, bool)>,
    audio_inputs: Vec<(String, String, bool)>,
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
    active_profile_id: String,
    active_profile_name: String,
    /// Queued `(profile_id, retry_at)` when a focus switch was deferred by flash-wear cooldown.
    pending_retry: Option<(String, Instant)>,
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
            wooting_keyboards: self.wooting_keyboards.clone(),
            any_flash_only,
            flash_warning,
            foreground_exe: self.foreground_exe.clone(),
            pending_exe,
            active_profile_id: self.active_profile_id.clone(),
            active_profile_name: self.active_profile_name.clone(),
            available_profiles,
            audio_outputs: self.audio_outputs.clone(),
            audio_inputs: self.audio_inputs.clone(),
            mic_lock_active: self.mic_lock_target.as_ref().map(|(_, pct)| *pct),
            displays: self.displays.clone(),
            active_display_override: self.active_display_override.clone(),
            total_flash_writes_session,
            log: self.log.clone(),
        }
    }

    fn emit(&self, ui_tx: &Sender<WorkerMsg>, ctx: &egui::Context) {
        let _ = ui_tx.send(WorkerMsg::ProfileRuntimeUpdated(Box::new(self.snapshot())));
        ctx.request_repaint();
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

    fn refresh_wooting_keyboards(&mut self) {
        self.wooting_keyboards = detect_wooting_keyboards()
            .unwrap_or_default()
            .into_iter()
            .map(|kb| kb.read_full_status())
            .collect();
    }

    fn rebuild_controllers(&mut self, selection: BackendSelection, reason: &str) {
        let mut hardware = detect_hardware_devices().unwrap_or_default();
        self.detected_hardware_info = hardware.iter().map(|d| d.info()).collect();
        self.refresh_wooting_keyboards();
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

    fn poll_batteries(&mut self) -> bool {
        let now = Instant::now();
        if now.saturating_duration_since(self.last_battery_poll) < BATTERY_POLL_INTERVAL {
            return false;
        }
        self.last_battery_poll = now;
        let alerts = self.alerts.clone();
        let mut any_changed = false;
        for managed in &mut self.controllers {
            if managed.refresh_battery(&alerts) {
                any_changed = true;
            }
        }
        any_changed
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
        let next_inputs: Vec<(String, String, bool)> = env
            .list_audio_endpoints(AudioFlow::Capture)
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.id, e.name, e.is_default))
            .collect();
        self.last_audio_refresh = Instant::now();
        let changed = next_outputs != self.audio_outputs || next_inputs != self.audio_inputs;
        self.audio_outputs = next_outputs;
        self.audio_inputs = next_inputs;
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

        // --- Wooting Keyboard Volatile Profile Switching (0..=7, zero flash wear) ---
        if let Some(slot) = profile.wooting_profile {
            match detect_wooting_keyboards() {
                Ok(boards) if !boards.is_empty() => {
                    let mut updated_infos = Vec::with_capacity(boards.len());
                    for kb in boards {
                        let kb_name = kb.name().to_string();
                        match kb.switch_profile(slot) {
                            Ok(true) => {
                                let full = kb.read_full_status();
                                self.push_log(
                                    Severity::Ok,
                                    format!(
                                        "[{kb_name}] Switched to {} ({reason}, volatile RAM).",
                                        full.profile_display_name(slot)
                                    ),
                                );
                                updated_infos.push(full);
                            }
                            Ok(false) => {
                                updated_infos.push(kb.read_full_status());
                            }
                            Err(err) => {
                                self.push_log(
                                    Severity::Warn,
                                    format!("[{kb_name}] Wooting switch failed: {err}"),
                                );
                                updated_infos.push(kb.read_full_status());
                            }
                        }
                    }
                    self.wooting_keyboards = updated_infos;
                }
                Ok(_) => {
                    self.wooting_keyboards.clear();
                }
                Err(err) => {
                    self.push_log(
                        Severity::Warn,
                        format!("Wooting keyboard detection failed: {err}"),
                    );
                }
            }
        }

        let mut logs: Vec<(Severity, String)> = Vec::new();
        let mut max_rate_limit_wait: Option<u64> = None;
        let mut any_succeeded = false;

        for managed in &mut self.controllers {
            let dev_info = managed.controller.info();
            let dev_name = dev_info.name;
            let caps = managed.controller.capabilities();
            let (raw_desired, used_override) = profile.settings_for_device(&dev_info.id);
            let (effective_desired, adjustments) = caps.adapt_settings(raw_desired);
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
                    if let Ok(actual) = managed.controller.read_settings() {
                        managed.last_settings = Some(actual);
                    }
                    logs.push((
                        Severity::Warn,
                        format!("[{dev_name}] '{}' ({reason}): {err}", profile.name),
                    ));
                }
            }
            managed.refresh_battery(&self.alerts);
        }
        self.last_battery_poll = Instant::now();

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
}

fn run_runtime_loop(
    initial_store: ProfileStore,
    initial_auto_switch: bool,
    initial_alerts: TrayAlertConfig,
    cmd_rx: Receiver<ProfileRuntimeCmd>,
    ui_tx: Sender<WorkerMsg>,
    ctx: egui::Context,
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
        wooting_keyboards: Vec::new(),
        last_topology: Vec::new(),
        last_topology_check: Instant::now(),
        pending_hotplug_at: None,
        last_battery_poll: Instant::now(),
        audio_outputs: Vec::new(),
        audio_inputs: Vec::new(),
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
        active_profile_id: "desktop".to_string(),
        active_profile_name: desktop_name,
        pending_retry: None,
        log: Vec::new(),
    };

    state.rebuild_controllers(BackendSelection::AutoConnected, "startup auto-detect");
    state.refresh_audio_devices();
    state.refresh_displays();
    if initial_auto_switch {
        state.start_watcher();
    }
    state.emit(&ui_tx, &ctx);

    loop {
        let mut changed = state.check_hotplug();
        if state.poll_batteries() {
            changed = true;
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
                state.foreground_exe = evt.exe_name.clone();
                state
                    .debouncer
                    .observe(evt.exe_name.as_deref(), evt.observed_at);
                changed = true;
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
            state.emit(&ui_tx, &ctx);
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

        match cmd_rx.recv_timeout(wait) {
            Ok(ProfileRuntimeCmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                let _ = state.revert_test_display_if_pending("shutdown");
                state.restore_original_display("shutdown");
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
                state.emit(&ui_tx, &ctx);
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
                state.emit(&ui_tx, &ctx);
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
                state.emit(&ui_tx, &ctx);
            }
            Ok(ProfileRuntimeCmd::RefreshMouseAndBattery) => {
                state.force_refresh_mouse_and_battery();
                state.emit(&ui_tx, &ctx);
            }
            Ok(ProfileRuntimeCmd::RefreshWooting) => {
                state.refresh_wooting_keyboards();
                let count = state.wooting_keyboards.len();
                let msg = if count == 0 {
                    "No Wooting keyboards detected (close Wootility if open).".to_string()
                } else {
                    format!(
                        "Refreshed {count} Wooting keyboard{}.",
                        if count == 1 { "" } else { "s" }
                    )
                };
                state.push_log(Severity::Info, msg.clone());
                let _ = ui_tx.send(WorkerMsg::WootingActionResult {
                    ok: count > 0,
                    message: msg,
                });
                state.emit(&ui_tx, &ctx);
            }
            Ok(ProfileRuntimeCmd::SwitchWootingProfile { device_id, slot }) => {
                match detect_wooting_keyboards() {
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
                                        let full = kb.read_full_status();
                                        let slot_name = full.profile_display_name(slot);
                                        let msg = if switched {
                                            format!(
                                                "[{kb_name}] Switched to {slot_name} (volatile RAM, zero flash wear)."
                                            )
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
                                        updated.push(kb.read_full_status());
                                    }
                                }
                            } else {
                                updated.push(kb.read_full_status());
                            }
                        }
                        state.wooting_keyboards = updated;
                        if let Some(sub) = osd_sub {
                            if state.alerts.osd_pill_enabled {
                                let _ = ui_tx.send(WorkerMsg::ShowOsdPill {
                                    title: "Polled · Wooting Keyboard".to_string(),
                                    subtitle: sub,
                                });
                            }
                        }
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
                        state.wooting_keyboards.clear();
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
                state.emit(&ui_tx, &ctx);
            }
            Ok(ProfileRuntimeCmd::WriteWootingAnalogBase { device_id, patch }) => {
                match detect_wooting_keyboards() {
                    Ok(boards) if !boards.is_empty() => {
                        let mut updated = Vec::with_capacity(boards.len());
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
                                        updated.push(full);
                                    }
                                    Err(err) => {
                                        let msg = format!("[{kb_name}] Analog save failed: {err}");
                                        state.push_log(Severity::Warn, msg.clone());
                                        messages.push(msg);
                                        updated.push(kb.read_full_status());
                                    }
                                }
                            } else {
                                updated.push(kb.read_full_status());
                            }
                        }
                        state.wooting_keyboards = updated;
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
                        state.wooting_keyboards.clear();
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
                state.emit(&ui_tx, &ctx);
            }
            Ok(ProfileRuntimeCmd::WriteWootingGlobalSettings { device_id, patch }) => {
                match detect_wooting_keyboards() {
                    Ok(boards) if !boards.is_empty() => {
                        let mut updated = Vec::with_capacity(boards.len());
                        let mut any_ok = false;
                        let mut messages = Vec::new();
                        for kb in boards {
                            let target =
                                device_id.as_deref().is_none_or(|wanted| kb.id() == wanted);
                            if target {
                                let kb_name = kb.name().to_string();
                                match kb.write_global_settings(&patch) {
                                    Ok(()) => {
                                        let full = kb.read_full_status();
                                        let msg = format!(
                                            "[{kb_name}] Applied Global Settings (read-back verified)."
                                        );
                                        state.push_log(Severity::Ok, msg.clone());
                                        messages.push(msg);
                                        any_ok = true;
                                        updated.push(full);
                                    }
                                    Err(err) => {
                                        let msg = format!(
                                            "[{kb_name}] Global Settings write failed: {err}"
                                        );
                                        state.push_log(Severity::Warn, msg.clone());
                                        messages.push(msg);
                                        updated.push(kb.read_full_status());
                                    }
                                }
                            } else {
                                updated.push(kb.read_full_status());
                            }
                        }
                        state.wooting_keyboards = updated;
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
                        state.wooting_keyboards.clear();
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
                state.emit(&ui_tx, &ctx);
            }
            Ok(ProfileRuntimeCmd::ReadFromMouseForEditor(dev_id)) => {
                let (device_name, settings, error) =
                    state.read_from_mouse_for_editor(dev_id.as_deref());
                let _ = ui_tx.send(WorkerMsg::ReadFromMouseFinished {
                    device_name,
                    settings,
                    error,
                });
                state.emit(&ui_tx, &ctx);
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
                state.emit(&ui_tx, &ctx);
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
                state.emit(&ui_tx, &ctx);
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
                    state.emit(&ui_tx, &ctx);
                }
            }
            Ok(ProfileRuntimeCmd::SwitchAudioOutput(endpoint_id)) => {
                state.switch_audio_output(&endpoint_id, "tray switch");
                state.emit(&ui_tx, &ctx);
            }
            Ok(ProfileRuntimeCmd::CycleAudioOutput) => {
                state.refresh_audio_devices();
                if state.audio_outputs.len() >= 2 {
                    let cur_idx = state
                        .audio_outputs
                        .iter()
                        .position(|(_, _, is_def)| *is_def)
                        .unwrap_or(0);
                    let next_idx = (cur_idx + 1) % state.audio_outputs.len();
                    let next_id = state.audio_outputs[next_idx].0.clone();
                    state.switch_audio_output(&next_id, "hotkey cycle");
                } else {
                    state.push_log(
                        Severity::Info,
                        "Audio output cycle skipped: fewer than 2 active playback devices.",
                    );
                }
                state.emit(&ui_tx, &ctx);
            }
            Ok(ProfileRuntimeCmd::RefreshAudioDevices) => {
                state.refresh_audio_devices();
                state.emit(&ui_tx, &ctx);
            }
            Ok(ProfileRuntimeCmd::RefreshDisplays) => {
                state.refresh_displays();
                state.emit(&ui_tx, &ctx);
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
                state.emit(&ui_tx, &ctx);
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
                state.emit(&ui_tx, &ctx);
            }
            Ok(ProfileRuntimeCmd::RevertTestDisplayMode) => {
                if let Some(msg) = state.revert_test_display_if_pending("user/countdown") {
                    let _ = ui_tx.send(WorkerMsg::DisplayTestReverted(msg));
                }
                state.emit(&ui_tx, &ctx);
            }
            Ok(ProfileRuntimeCmd::UpdateStore(new_store)) => {
                state
                    .debouncer
                    .set_delay(Duration::from_millis(new_store.debounce_ms));
                let _ = new_store.save_atomic(&AppPaths::default_user_dir().profiles_path());
                state.store = new_store;
                state.push_log(Severity::Info, "Saved updated profiles.json.");
                state.emit(&ui_tx, &ctx);
            }
            Ok(ProfileRuntimeCmd::SelectBackend(sel)) => {
                state.rebuild_controllers(sel, "manual selection");
                state.emit(&ui_tx, &ctx);
            }
            Ok(ProfileRuntimeCmd::RedetectHardware) => {
                let sel = state.selection.clone();
                state.rebuild_controllers(sel, "manual redetect");
                state.emit(&ui_tx, &ctx);
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
                    state.emit(&ui_tx, &ctx);
                }
            }
        }
    }
}
