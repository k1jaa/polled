// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

//! Background profile-switching runtime connecting `ForegroundWatcher` -> `FocusDebouncer`
//! -> `match_profile` -> `SafeDeviceController` (+ optional Windows pointer speed).

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use chrono::Local;
use eframe::egui;
use polled_core::win::{SystemEnv, WindowsEnv};
use polled_core::{
    detect_hardware_devices, match_profile, AppPaths, ApplyOutcome, DeviceCapabilities, DeviceInfo,
    DryRunDevice, FocusDebouncer, FocusEvent, ForegroundWatcher, GameProfile, MouseDevice,
    MouseSettings, ProfileStore, SafeDeviceController, Severity,
};

use crate::app::WorkerMsg;

const MAX_LOG_ENTRIES: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendSelection {
    DryRunVolatile,
    DryRunFlashOnly,
    HardwareIndex(usize),
}

#[derive(Debug, Clone)]
pub struct ProfileLogEntry {
    pub timestamp: String,
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ProfileRuntimeSnapshot {
    pub auto_switch_enabled: bool,
    pub selection: BackendSelection,
    pub detected_hardware: Vec<DeviceInfo>,
    pub active_device_info: DeviceInfo,
    pub active_device_caps: DeviceCapabilities,
    pub current_device_settings: Option<MouseSettings>,
    pub foreground_exe: Option<String>,
    pub pending_exe: Option<String>,
    pub active_profile_id: String,
    pub active_profile_name: String,
    pub flash_writes_this_session: u64,
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
        ui_tx: Sender<WorkerMsg>,
        ctx: egui::Context,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();

        std::thread::Builder::new()
            .name("polled-profile-runtime".to_string())
            .spawn(move || {
                run_runtime_loop(initial_store, cmd_rx, ui_tx, ctx);
            })
            .expect("failed to spawn profile runtime thread");

        Self { cmd_tx }
    }

    pub fn send(&self, cmd: ProfileRuntimeCmd) {
        let _ = self.cmd_tx.send(cmd);
    }
}

impl Drop for ProfileRuntimeHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(ProfileRuntimeCmd::Shutdown);
    }
}

struct RuntimeState {
    store: ProfileStore,
    auto_switch_enabled: bool,
    selection: BackendSelection,
    detected_hardware_info: Vec<DeviceInfo>,
    controller: SafeDeviceController,
    current_device_settings: Option<MouseSettings>,
    debouncer: FocusDebouncer,
    watcher: Option<ForegroundWatcher>,
    focus_rx: Option<Receiver<FocusEvent>>,
    foreground_exe: Option<String>,
    active_profile_id: String,
    active_profile_name: String,
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
        ProfileRuntimeSnapshot {
            auto_switch_enabled: self.auto_switch_enabled,
            selection: self.selection.clone(),
            detected_hardware: self.detected_hardware_info.clone(),
            active_device_info: self.controller.info(),
            active_device_caps: self.controller.capabilities(),
            current_device_settings: self.current_device_settings.clone(),
            foreground_exe: self.foreground_exe.clone(),
            pending_exe,
            active_profile_id: self.active_profile_id.clone(),
            active_profile_name: self.active_profile_name.clone(),
            flash_writes_this_session: self.controller.flash_writes_this_session(),
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

    fn apply_profile(&mut self, profile: &GameProfile, reason: &str) {
        self.active_profile_id = profile.id.clone();
        self.active_profile_name = profile.name.clone();

        // Optional Windows pointer speed override (user-level SPI, no admin).
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

        // Apply mouse hardware settings through SafeDeviceController (diff + flash guard + read-back).
        match self.controller.apply_profile_settings(&profile.mouse) {
            Ok(ApplyOutcome::SkippedNoSettings) => {
                self.push_log(
                    Severity::Info,
                    format!("Profile '{}' ({reason}): no mouse changes.", profile.name),
                );
            }
            Ok(ApplyOutcome::SkippedUnchanged { current }) => {
                self.current_device_settings = Some(current.clone());
                self.push_log(
                    Severity::Ok,
                    format!(
                        "Profile '{}' ({reason}): mouse already matches ({}) — 0 writes.",
                        profile.name,
                        current.summary_string()
                    ),
                );
            }
            Ok(ApplyOutcome::Applied {
                mode,
                verified,
                flash_writes_this_session,
            }) => {
                self.current_device_settings = Some(verified.clone());
                self.push_log(
                    Severity::Ok,
                    format!(
                        "Applied '{}' ({reason}) via {mode:?} & verified read-back: {} (session flash writes: {flash_writes_this_session}).",
                        profile.name,
                        verified.summary_string()
                    ),
                );
            }
            Err(err) => {
                // Re-read current state after an error/rollback so UI reflects hardware truth.
                if let Ok(actual) = self.controller.read_settings() {
                    self.current_device_settings = Some(actual);
                }
                self.push_log(
                    Severity::Bad,
                    format!("Profile '{}' ({reason}): {err}", profile.name),
                );
            }
        }
    }

    fn select_backend(&mut self, selection: BackendSelection) {
        let mut hardware = detect_hardware_devices().unwrap_or_default();
        self.detected_hardware_info = hardware.iter().map(|d| d.info()).collect();

        let (chosen_selection, device): (BackendSelection, Box<dyn MouseDevice>) = match selection {
            BackendSelection::DryRunVolatile => (
                BackendSelection::DryRunVolatile,
                Box::new(DryRunDevice::new_volatile()),
            ),
            BackendSelection::DryRunFlashOnly => (
                BackendSelection::DryRunFlashOnly,
                Box::new(DryRunDevice::new_flash_only()),
            ),
            BackendSelection::HardwareIndex(idx) => {
                if idx < hardware.len() {
                    (BackendSelection::HardwareIndex(idx), hardware.remove(idx))
                } else {
                    self.push_log(
                        Severity::Warn,
                        "Selected hardware mouse is no longer connected; falling back to Dry-Run.",
                    );
                    (
                        BackendSelection::DryRunVolatile,
                        Box::new(DryRunDevice::new_volatile()),
                    )
                }
            }
        };

        let info = device.info();
        self.selection = chosen_selection;
        self.controller = SafeDeviceController::new(device);
        self.current_device_settings = self.controller.read_settings().ok();

        self.push_log(
            Severity::Info,
            format!(
                "Selected device: {} [{}]",
                info.name,
                info.support_tier.label()
            ),
        );
    }
}

fn run_runtime_loop(
    initial_store: ProfileStore,
    cmd_rx: Receiver<ProfileRuntimeCmd>,
    ui_tx: Sender<WorkerMsg>,
    ctx: egui::Context,
) {
    let mut hardware = detect_hardware_devices().unwrap_or_default();
    let detected_hardware_info: Vec<DeviceInfo> = hardware.iter().map(|d| d.info()).collect();

    // Auto-select detected hardware if present; otherwise default safely to Dry-Run simulator.
    let (selection, initial_device): (BackendSelection, Box<dyn MouseDevice>) =
        if !hardware.is_empty() {
            (BackendSelection::HardwareIndex(0), hardware.remove(0))
        } else {
            (
                BackendSelection::DryRunVolatile,
                Box::new(DryRunDevice::new_volatile()),
            )
        };

    let mut controller = SafeDeviceController::new(initial_device);
    let current_device_settings = controller.read_settings().ok();
    let debounce = Duration::from_millis(initial_store.debounce_ms);

    let desktop_name = initial_store
        .profiles
        .iter()
        .find(|p| p.is_desktop_default)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "Desktop (Default)".to_string());

    let mut state = RuntimeState {
        store: initial_store,
        auto_switch_enabled: false,
        selection,
        detected_hardware_info,
        controller,
        current_device_settings,
        debouncer: FocusDebouncer::new(debounce),
        watcher: None,
        focus_rx: None,
        foreground_exe: None,
        active_profile_id: "desktop".to_string(),
        active_profile_name: desktop_name,
        log: Vec::new(),
    };

    state.push_log(
        Severity::Info,
        format!(
            "Ready on {} ({}). Auto-switching is paused until enabled.",
            state.controller.info().name,
            state.controller.info().support_tier.label()
        ),
    );
    state.emit(&ui_tx, &ctx);

    loop {
        // Drain any foreground window events pushed by the Win32 hook thread.
        let mut focus_changed = false;
        if let Some(rx) = &state.focus_rx {
            while let Ok(evt) = rx.try_recv() {
                state.foreground_exe = evt.exe_name.clone();
                state
                    .debouncer
                    .observe(evt.exe_name.as_deref(), evt.observed_at);
                focus_changed = true;
            }
        }

        // Check if the debounced focus change is now stable and ready to commit.
        if state.auto_switch_enabled {
            if let Some(committed_exe) = state.debouncer.poll_ready(Instant::now()) {
                let matched = match_profile(&state.store, committed_exe.as_deref()).cloned();
                if let Some(profile) = matched {
                    let reason = committed_exe
                        .as_deref()
                        .map(|e| format!("focus: {e}"))
                        .unwrap_or_else(|| "focus: desktop".to_string());
                    state.apply_profile(&profile, &reason);
                    focus_changed = true;
                }
            }
        }

        if focus_changed {
            state.emit(&ui_tx, &ctx);
        }

        // Choose wait timeout:
        // - If a focus change is pending debounce, wake precisely when ready (capped at 100ms).
        // - If watcher is running without a pending change, wake every 150ms to check `focus_rx`.
        // - If auto-switch is off, block indefinitely on `cmd_rx` (0.0% CPU!).
        let cmd_result = if !state.auto_switch_enabled {
            cmd_rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            let wait = state
                .debouncer
                .time_until_ready(Instant::now())
                .map(|rem| rem.min(Duration::from_millis(100)))
                .unwrap_or(Duration::from_millis(150));
            cmd_rx.recv_timeout(wait)
        };

        match cmd_result {
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
                state.select_backend(sel);
                state.emit(&ui_tx, &ctx);
            }
            Ok(ProfileRuntimeCmd::RedetectHardware) => {
                let current_sel = state.selection.clone();
                state.select_backend(current_sel);
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
