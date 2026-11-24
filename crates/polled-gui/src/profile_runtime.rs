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
use polled_core::win::{SystemEnv, WindowsEnv};
use polled_core::{
    detect_hardware_devices, hid_topology_fingerprint, match_profile, AppPaths, ApplyOutcome,
    DeviceCapabilities, DeviceInfo, DryRunDevice, FocusDebouncer, FocusEvent, ForegroundWatcher,
    GameProfile, MouseSettings, PolledError, ProfileStore, SafeDeviceController, Severity,
};

use crate::app::WorkerMsg;

const MAX_LOG_ENTRIES: usize = 50;
const HOTPLUG_CHECK_INTERVAL: Duration = Duration::from_secs(2);

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
}

#[derive(Debug, Clone)]
pub struct ProfileRuntimeSnapshot {
    pub auto_switch_enabled: bool,
    pub selection: BackendSelection,
    pub detected_hardware: Vec<DeviceInfo>,
    pub active_devices: Vec<ConnectedDeviceStatus>,
    pub any_flash_only: bool,
    pub flash_warning: Option<String>,
    pub foreground_exe: Option<String>,
    pub pending_exe: Option<String>,
    pub active_profile_id: String,
    pub active_profile_name: String,
    pub total_flash_writes_session: u64,
    pub log: Vec<ProfileLogEntry>,
}

pub enum ProfileRuntimeCmd {
    SetAutoSwitch(bool),
    UpdateStore(ProfileStore),
    SelectBackend(BackendSelection),
    RedetectHardware,
    ApplyProfileNow(String),
    Shutdown,
}

pub struct ProfileRuntimeHandle {
    cmd_tx: Sender<ProfileRuntimeCmd>,
}

impl ProfileRuntimeHandle {
    pub fn start(
        initial_store: ProfileStore,
        initial_auto_switch: bool,
        ui_tx: Sender<WorkerMsg>,
        ctx: egui::Context,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();

        std::thread::Builder::new()
            .name("polled-profile-runtime".to_string())
            .spawn(move || {
                run_runtime_loop(initial_store, initial_auto_switch, cmd_rx, ui_tx, ctx);
            })
            .expect("failed to spawn profile runtime thread");

        Self { cmd_tx }
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
}

struct RuntimeState {
    store: ProfileStore,
    auto_switch_enabled: bool,
    selection: BackendSelection,
    detected_hardware_info: Vec<DeviceInfo>,
    controllers: Vec<ManagedController>,
    last_topology: Vec<(u16, u16, u16, u16)>,
    last_topology_check: Instant,
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

        ProfileRuntimeSnapshot {
            auto_switch_enabled: self.auto_switch_enabled,
            selection: self.selection.clone(),
            detected_hardware: self.detected_hardware_info.clone(),
            active_devices,
            any_flash_only,
            flash_warning,
            foreground_exe: self.foreground_exe.clone(),
            pending_exe,
            active_profile_id: self.active_profile_id.clone(),
            active_profile_name: self.active_profile_name.clone(),
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

    fn rebuild_controllers(&mut self, selection: BackendSelection, reason: &str) {
        let mut hardware = detect_hardware_devices().unwrap_or_default();
        self.detected_hardware_info = hardware.iter().map(|d| d.info()).collect();
        self.last_topology = hid_topology_fingerprint();
        self.last_topology_check = Instant::now();

        let mut next_controllers: Vec<ManagedController> = Vec::new();
        let effective_selection = match selection {
            BackendSelection::AutoConnected => {
                if hardware.is_empty() {
                    let mut ctrl =
                        SafeDeviceController::new(Box::new(DryRunDevice::new_volatile()));
                    let settings = ctrl.read_settings().ok();
                    next_controllers.push(ManagedController {
                        controller: ctrl,
                        last_settings: settings,
                    });
                } else {
                    for dev in hardware {
                        let mut ctrl = SafeDeviceController::new(dev);
                        let settings = ctrl.read_settings().ok();
                        next_controllers.push(ManagedController {
                            controller: ctrl,
                            last_settings: settings,
                        });
                    }
                }
                BackendSelection::AutoConnected
            }
            BackendSelection::HardwareIndex(idx) => {
                if idx < hardware.len() {
                    let mut ctrl = SafeDeviceController::new(hardware.remove(idx));
                    let settings = ctrl.read_settings().ok();
                    next_controllers.push(ManagedController {
                        controller: ctrl,
                        last_settings: settings,
                    });
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
                let mut ctrl = SafeDeviceController::new(Box::new(DryRunDevice::new_volatile()));
                let settings = ctrl.read_settings().ok();
                next_controllers.push(ManagedController {
                    controller: ctrl,
                    last_settings: settings,
                });
                BackendSelection::DryRunVolatile
            }
            BackendSelection::DryRunFlashOnly => {
                let mut ctrl = SafeDeviceController::new(Box::new(DryRunDevice::new_flash_only()));
                let settings = ctrl.read_settings().ok();
                next_controllers.push(ManagedController {
                    controller: ctrl,
                    last_settings: settings,
                });
                BackendSelection::DryRunFlashOnly
            }
        };

        self.selection = effective_selection;
        self.controllers = next_controllers;

        let summary: Vec<String> = self
            .controllers
            .iter()
            .map(|m| {
                let info = m.controller.info();
                match &m.last_settings {
                    Some(s) => format!("{} ({})", info.name, s.summary_string()),
                    None => format!("{} (asleep / move mouse to wake)", info.name),
                }
            })
            .collect();

        self.push_log(
            Severity::Info,
            format!("Devices ({reason}): {}", summary.join(" | ")),
        );
    }

    /// Checks whether USB HID devices were plugged in or unplugged and auto-refreshes when changed.
    fn check_hotplug(&mut self) -> bool {
        if !matches!(
            self.selection,
            BackendSelection::AutoConnected | BackendSelection::HardwareIndex(_)
        ) {
            return false;
        }
        let now = Instant::now();
        if now.saturating_duration_since(self.last_topology_check) < HOTPLUG_CHECK_INTERVAL {
            return false;
        }
        self.last_topology_check = now;
        let current_fp = hid_topology_fingerprint();
        if current_fp == self.last_topology {
            return false;
        }
        let sel = self.selection.clone();
        self.rebuild_controllers(sel, "USB hot-plug");
        true
    }

    fn apply_profile(&mut self, profile: &GameProfile, reason: &str) {
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
        }

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
        }

        for (sev, msg) in logs {
            self.push_log(sev, msg);
        }
    }
}

fn run_runtime_loop(
    initial_store: ProfileStore,
    initial_auto_switch: bool,
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
        auto_switch_enabled: initial_auto_switch,
        selection: BackendSelection::AutoConnected,
        detected_hardware_info: Vec::new(),
        controllers: Vec::new(),
        last_topology: Vec::new(),
        last_topology_check: Instant::now(),
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
    if initial_auto_switch {
        state.start_watcher();
    }
    state.emit(&ui_tx, &ctx);

    loop {
        let mut changed = state.check_hotplug();

        if let Some(rx) = &state.focus_rx {
            while let Ok(evt) = rx.try_recv() {
                state.foreground_exe = evt.exe_name.clone();
                state
                    .debouncer
                    .observe(evt.exe_name.as_deref(), evt.observed_at);
                changed = true;
            }
        }

        if state.auto_switch_enabled {
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
                        state.apply_profile(&profile, &reason);
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
                            state.apply_profile(&profile, "cooldown retry");
                            changed = true;
                        }
                    }
                }
            }
        }

        if changed {
            state.emit(&ui_tx, &ctx);
        }

        let wait = if !state.auto_switch_enabled {
            // Still check USB hot-plug every 2s so plugging in a new mouse updates the UI live.
            HOTPLUG_CHECK_INTERVAL
        } else {
            state
                .debouncer
                .time_until_ready(Instant::now())
                .map(|rem| rem.min(Duration::from_millis(100)))
                .unwrap_or(Duration::from_millis(150))
        };

        match cmd_rx.recv_timeout(wait) {
            Ok(ProfileRuntimeCmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                state.stop_watcher();
                break;
            }
            Err(RecvTimeoutError::Timeout) => continue,
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
                    state.apply_profile(&profile, "manual test");
                    state.emit(&ui_tx, &ctx);
                }
            }
        }
    }
}
