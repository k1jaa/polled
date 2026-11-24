// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Vec2};
use polled_core::win::{RegHive, WindowsEnv};
use polled_core::{
    curated_custom_tweak_presets, is_autostart_enabled, set_autostart_enabled, AppPaths,
    AppSettings, CheckReport, CustomTweakDef, CustomTweakStore, CustomValueKind, HealthEngine,
    PolledError, ScanSummary, Severity, ThemePreference,
};

use crate::elevation::{run_elevated_cli, ElevatedAction};
use crate::input_lab_ui::InputLabUiState;
use crate::notifications::{notify_if_regressed, spawn_windows_build_watcher};
use crate::profile_runtime::{ProfileRuntimeHandle, ProfileRuntimeSnapshot};
use crate::profiles_ui::ProfilesUiState;
use crate::single_instance::SingleInstanceGuard;
use crate::theme::{Palette, ThemeMode};
use crate::tray::{find_own_main_hwnd, TrayController};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveView {
    HealthCheck,
    Profiles,
    InputLab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterMode {
    All,
    Issues,
    Tweaks,
    ReportOnly,
    Undoable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CustomDraftType {
    Dword,
    String,
}

#[derive(Debug, Clone)]
struct CustomTweakDraft {
    open: bool,
    preset_id: Option<String>,
    name: String,
    hive: RegHive,
    subkey: String,
    value_name: String,
    val_type: CustomDraftType,
    value_text: String,
    requires_reboot: bool,
    notes: String,
    error: Option<String>,
}

impl Default for CustomTweakDraft {
    fn default() -> Self {
        Self {
            open: false,
            preset_id: None,
            name: String::new(),
            hive: RegHive::Hkcu,
            subkey: String::new(),
            value_name: String::new(),
            val_type: CustomDraftType::Dword,
            value_text: "1".to_string(),
            requires_reboot: false,
            notes: String::new(),
            error: None,
        }
    }
}

impl CustomTweakDraft {
    fn fill_from_preset(&mut self, def: &CustomTweakDef) {
        self.preset_id = Some(def.id.clone());
        self.name = def.name.clone();
        self.hive = def.hive;
        self.subkey = def.subkey.clone();
        self.value_name = def.value_name.clone();
        match &def.target {
            CustomValueKind::Dword(v) => {
                self.val_type = CustomDraftType::Dword;
                self.value_text = format!("0x{v:X}");
            }
            CustomValueKind::String(s) => {
                self.val_type = CustomDraftType::String;
                self.value_text = s.clone();
            }
        }
        self.requires_reboot = def.requires_reboot;
        self.notes = def.notes.clone();
        self.error = None;
    }

    fn build_def(&self) -> Result<CustomTweakDef, String> {
        let target = match self.val_type {
            CustomDraftType::Dword => {
                let raw = self.value_text.trim();
                let parsed = if let Some(hex) =
                    raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X"))
                {
                    u32::from_str_radix(hex, 16)
                } else {
                    raw.parse::<u32>()
                }
                .map_err(|_| {
                    format!("Invalid DWORD value '{raw}' (use decimal like 64 or hex like 0x2A).")
                })?;
                CustomValueKind::Dword(parsed)
            }
            CustomDraftType::String => CustomValueKind::String(self.value_text.clone()),
        };

        let id = self
            .preset_id
            .clone()
            .unwrap_or_else(|| CustomTweakDef::make_id(&self.name));
        let def = CustomTweakDef {
            id,
            name: self.name.trim().to_string(),
            hive: self.hive,
            subkey: self.subkey.trim().trim_matches('\\').to_string(),
            value_name: self.value_name.trim().to_string(),
            target,
            requires_reboot: self.requires_reboot,
            notes: self.notes.trim().to_string(),
        };
        def.validate()?;
        Ok(def)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ElevationTarget {
    Fix(String),
    Undo(String),
    UndoAll,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingElevation {
    target: ElevationTarget,
    title: String,
    reason: String,
    action_label: String,
    change_summary: Option<String>,
    impact_tags: Vec<String>,
    restore_point_hint: bool,
}

#[derive(Debug, Clone)]
struct PendingConfirm {
    check: CheckReport,
    warning: String,
}

fn build_fix_impact_tags(check: &CheckReport) -> Vec<String> {
    let mut tags = Vec::new();
    if check.id.starts_with("custom_") {
        tags.push("Custom tweak".to_string());
    } else if check.is_optional_tweak {
        tags.push("Optional tweak".to_string());
    }
    if check.requires_elevation {
        tags.push("System-wide (HKLM)".to_string());
    }
    if check.requires_reboot {
        tags.push("Reboot required".to_string());
    }
    if check.requires_relogin {
        tags.push("Sign-out required".to_string());
    }
    tags
}

fn build_elevation_prompt_for_fix(check: &CheckReport, reason: String) -> PendingElevation {
    let verb = if check.is_optional_tweak {
        "Apply"
    } else {
        "Fix"
    };
    PendingElevation {
        target: ElevationTarget::Fix(check.id.clone()),
        title: format!("{verb} \"{}\"", check.name),
        reason,
        action_label: "Continue with UAC".to_string(),
        change_summary: Some(format!(
            "{}  ->  {}",
            check.current_value, check.recommended_value
        )),
        impact_tags: build_fix_impact_tags(check),
        restore_point_hint: true,
    }
}

fn build_non_elevated_confirm_warning(check: &CheckReport) -> String {
    if check.id.starts_with("custom_") {
        "This user-defined custom tweak writes directly to the Windows registry. Verify the target path and value before applying.".to_string()
    } else if check.requires_reboot {
        "This change modifies a Windows setting that requires a reboot to take effect.".to_string()
    } else if check.requires_relogin {
        "This change resets Windows registry values and requires signing out and back in to take full effect.".to_string()
    } else {
        "This is an optional tweak and is never applied automatically by Fix Safe Issues."
            .to_string()
    }
}

/// Messages sent from background worker/tray threads back to the `egui` UI thread.
pub(crate) enum WorkerMsg {
    TraySyncAutoSwitch(bool),
    ScanFinished {
        result: Result<ScanSummary, String>,
        notify_regressions: bool,
    },
    ActionFinished {
        status: Option<String>,
        error: Option<String>,
        elevation_prompt: Option<Box<PendingElevation>>,
        updated_scan: Option<ScanSummary>,
    },
    ProfileRuntimeUpdated(Box<ProfileRuntimeSnapshot>),
}

pub struct PolledApp {
    settings: AppSettings,
    autostart_enabled: bool,
    theme: ThemeMode,
    active_view: ActiveView,
    summary: Option<ScanSummary>,
    snapshot_path: String,
    filter: FilterMode,
    busy: bool,
    busy_label: String,
    status_message: Option<String>,
    error_message: Option<String>,
    pending_elevation: Option<PendingElevation>,
    pending_confirm: Option<PendingConfirm>,
    show_latency_tips: bool,
    custom_tweak_draft: CustomTweakDraft,

    profiles_ui: ProfilesUiState,
    profile_runtime: ProfileRuntimeHandle,
    input_lab_ui: InputLabUiState,

    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
    main_hwnd: Arc<AtomicIsize>,
    auto_switch_state: Arc<AtomicBool>,
    quit_requested: Arc<AtomicBool>,
    _instance_guard: SingleInstanceGuard,
    tray: TrayController,
}

impl PolledApp {
    pub fn new(cc: &eframe::CreationContext<'_>, mut instance_guard: SingleInstanceGuard) -> Self {
        let paths = AppPaths::default_user_dir();
        let settings = AppSettings::load_or_default(&paths.settings_path());
        let theme = ThemeMode::from(settings.theme);
        theme.apply_to(&cc.egui_ctx);

        let autostart_enabled = is_autostart_enabled(&WindowsEnv).unwrap_or(false);

        let (tx, rx) = mpsc::channel();
        let main_hwnd = Arc::new(AtomicIsize::new(find_own_main_hwnd()));
        let auto_switch_state = Arc::new(AtomicBool::new(settings.auto_switch_on_startup));
        let quit_requested = Arc::new(AtomicBool::new(false));

        let profiles_ui = ProfilesUiState::load();
        let profile_runtime = ProfileRuntimeHandle::start(
            profiles_ui.store.clone(),
            settings.auto_switch_on_startup,
            tx.clone(),
            cc.egui_ctx.clone(),
        );

        instance_guard.start_show_listener(
            Arc::clone(&main_hwnd),
            cc.egui_ctx.clone(),
            Arc::clone(&quit_requested),
        );
        let tray = TrayController::init(
            cc.egui_ctx.clone(),
            tx.clone(),
            profile_runtime.command_sender(),
            Arc::clone(&main_hwnd),
            Arc::clone(&auto_switch_state),
            Arc::clone(&quit_requested),
        );
        spawn_windows_build_watcher(tx.clone(), cc.egui_ctx.clone());

        let snapshot_path = paths.baseline_snapshot_path().display().to_string();

        let mut app = Self {
            settings,
            autostart_enabled,
            theme,
            active_view: ActiveView::HealthCheck,
            summary: None,
            snapshot_path,
            filter: FilterMode::All,
            busy: false,
            busy_label: String::new(),
            status_message: None,
            error_message: None,
            pending_elevation: None,
            pending_confirm: None,
            show_latency_tips: false,
            custom_tweak_draft: CustomTweakDraft::default(),
            profiles_ui,
            profile_runtime,
            input_lab_ui: InputLabUiState::new(),
            tx,
            rx,
            main_hwnd,
            auto_switch_state,
            quit_requested,
            _instance_guard: instance_guard,
            tray,
        };

        app.start_scan(&cc.egui_ctx, true);
        app
    }

    fn start_scan(&mut self, ctx: &egui::Context, notify_regressions: bool) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.busy_label = "Scanning Windows input settings...".to_string();
        self.error_message = None;

        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let paths = AppPaths::default_user_dir();
            let engine = HealthEngine::with_default_and_custom_checks(&paths);
            let result = engine
                .scan_all(&WindowsEnv, &paths, true)
                .map_err(|e| e.to_string());
            let _ = tx.send(WorkerMsg::ScanFinished {
                result,
                notify_regressions,
            });
            ctx.request_repaint();
        });
    }

    fn start_fix_check(&mut self, ctx: &egui::Context, check: CheckReport, elevated: bool) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.busy_label = if elevated {
            format!("Waiting for UAC to fix {}...", check.name)
        } else {
            format!("Fixing {}...", check.name)
        };
        self.status_message = None;
        self.error_message = None;

        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let paths = AppPaths::default_user_dir();
            let engine = HealthEngine::with_default_and_custom_checks(&paths);

            let msg = if elevated {
                match run_elevated_cli(ElevatedAction::Fix(&check.id), &paths) {
                    Ok(()) => {
                        let scan = engine.scan_all(&WindowsEnv, &paths, true).ok();
                        WorkerMsg::ActionFinished {
                            status: Some(format!(
                                "Applied elevated fix for \"{}\". Original setting saved to snapshot.",
                                check.name
                            )),
                            error: None,
                            elevation_prompt: None,
                            updated_scan: scan,
                        }
                    }
                    Err(err) => WorkerMsg::ActionFinished {
                        status: None,
                        error: Some(err),
                        elevation_prompt: None,
                        updated_scan: None,
                    },
                }
            } else {
                match engine.fix_check(&check.id, &WindowsEnv, &paths) {
                    Ok(_) => {
                        let scan = engine.scan_all(&WindowsEnv, &paths, true).ok();
                        WorkerMsg::ActionFinished {
                            status: Some(format!(
                                "Fixed \"{}\". Original value saved to undo snapshot.",
                                check.name
                            )),
                            error: None,
                            elevation_prompt: None,
                            updated_scan: scan,
                        }
                    }
                    Err(PolledError::ElevationRequired { reason, .. }) => {
                        WorkerMsg::ActionFinished {
                            status: None,
                            error: None,
                            elevation_prompt: Some(Box::new(build_elevation_prompt_for_fix(
                                &check, reason,
                            ))),
                            updated_scan: None,
                        }
                    }
                    Err(err) => WorkerMsg::ActionFinished {
                        status: None,
                        error: Some(err.to_string()),
                        elevation_prompt: None,
                        updated_scan: None,
                    },
                }
            };

            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    fn start_fix_all_safe(&mut self, ctx: &egui::Context) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.busy_label = "Applying safe user-level fixes...".to_string();
        self.status_message = None;
        self.error_message = None;

        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let paths = AppPaths::default_user_dir();
            let engine = HealthEngine::with_default_and_custom_checks(&paths);
            let msg = match engine.fix_all_safe(&WindowsEnv, &paths) {
                Ok(fixed) => {
                    let scan = engine.scan_all(&WindowsEnv, &paths, true).ok();
                    WorkerMsg::ActionFinished {
                        status: Some(format!(
                            "Applied {} safe fix(es). Original values saved to undo snapshot.",
                            fixed.len()
                        )),
                        error: None,
                        elevation_prompt: None,
                        updated_scan: scan,
                    }
                }
                Err(err) => WorkerMsg::ActionFinished {
                    status: None,
                    error: Some(err.to_string()),
                    elevation_prompt: None,
                    updated_scan: None,
                },
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    fn start_undo_check(&mut self, ctx: &egui::Context, check: CheckReport, elevated: bool) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.busy_label = format!("Restoring {}...", check.name);
        self.status_message = None;
        self.error_message = None;

        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let paths = AppPaths::default_user_dir();
            let engine = HealthEngine::with_default_and_custom_checks(&paths);

            let msg = if elevated {
                match run_elevated_cli(ElevatedAction::Undo(&check.id), &paths) {
                    Ok(()) => {
                        let scan = engine.scan_all(&WindowsEnv, &paths, true).ok();
                        WorkerMsg::ActionFinished {
                            status: Some(format!(
                                "Restored \"{}\" from baseline snapshot.",
                                check.name
                            )),
                            error: None,
                            elevation_prompt: None,
                            updated_scan: scan,
                        }
                    }
                    Err(err) => WorkerMsg::ActionFinished {
                        status: None,
                        error: Some(err),
                        elevation_prompt: None,
                        updated_scan: None,
                    },
                }
            } else {
                match engine.undo_check(&check.id, &WindowsEnv, &paths) {
                    Ok(_) => {
                        let scan = engine.scan_all(&WindowsEnv, &paths, true).ok();
                        WorkerMsg::ActionFinished {
                            status: Some(format!(
                                "Restored \"{}\" from baseline snapshot.",
                                check.name
                            )),
                            error: None,
                            elevation_prompt: None,
                            updated_scan: scan,
                        }
                    }
                    Err(PolledError::ElevationRequired { reason, .. }) => {
                        WorkerMsg::ActionFinished {
                            status: None,
                            error: None,
                            elevation_prompt: Some(Box::new(PendingElevation {
                                target: ElevationTarget::Undo(check.id.clone()),
                                title: format!("Undo \"{}\"", check.name),
                                reason,
                                action_label: "Restore with UAC".to_string(),
                                change_summary: None,
                                impact_tags: Vec::new(),
                                restore_point_hint: false,
                            })),
                            updated_scan: None,
                        }
                    }
                    Err(err) => WorkerMsg::ActionFinished {
                        status: None,
                        error: Some(err.to_string()),
                        elevation_prompt: None,
                        updated_scan: None,
                    },
                }
            };

            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    fn start_undo_all(&mut self, ctx: &egui::Context, elevated: bool) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.busy_label = if elevated {
            "Waiting for UAC to restore remaining settings...".to_string()
        } else {
            "Restoring all settings from baseline snapshot...".to_string()
        };
        self.status_message = None;
        self.error_message = None;

        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let paths = AppPaths::default_user_dir();
            let engine = HealthEngine::with_default_and_custom_checks(&paths);

            let msg = if elevated {
                match run_elevated_cli(ElevatedAction::UndoAll, &paths) {
                    Ok(()) => {
                        let scan = engine.scan_all(&WindowsEnv, &paths, true).ok();
                        WorkerMsg::ActionFinished {
                            status: Some(
                                "Restored all remaining settings from baseline snapshot."
                                    .to_string(),
                            ),
                            error: None,
                            elevation_prompt: None,
                            updated_scan: scan,
                        }
                    }
                    Err(err) => WorkerMsg::ActionFinished {
                        status: None,
                        error: Some(err),
                        elevation_prompt: None,
                        updated_scan: None,
                    },
                }
            } else {
                match engine.undo_all(&WindowsEnv, &paths) {
                    Ok(undo) => {
                        let scan = engine.scan_all(&WindowsEnv, &paths, true).ok();
                        let needs_admin = undo.failed.iter().any(|f| f.requires_elevation);
                        if needs_admin {
                            WorkerMsg::ActionFinished {
                                status: None,
                                error: None,
                                elevation_prompt: Some(Box::new(PendingElevation {
                                    target: ElevationTarget::UndoAll,
                                    title: "Restore remaining system-level settings".to_string(),
                                    reason: "One or more settings in your snapshot require administrator permissions to restore."
                                        .to_string(),
                                    action_label: "Restore remaining with UAC".to_string(),
                                    change_summary: None,
                                    impact_tags: Vec::new(),
                                    restore_point_hint: false,
                                })),
                                updated_scan: scan,
                            }
                        } else if !undo.failed.is_empty() {
                            WorkerMsg::ActionFinished {
                                status: None,
                                error: Some(format!(
                                    "Restored {} setting(s), but {} failed.",
                                    undo.restored_ids.len(),
                                    undo.failed.len()
                                )),
                                elevation_prompt: None,
                                updated_scan: scan,
                            }
                        } else {
                            WorkerMsg::ActionFinished {
                                status: Some(format!(
                                    "Restored all {} original setting(s) from baseline snapshot.",
                                    undo.restored_ids.len()
                                )),
                                error: None,
                                elevation_prompt: None,
                                updated_scan: scan,
                            }
                        }
                    }
                    Err(err) => WorkerMsg::ActionFinished {
                        status: None,
                        error: Some(err.to_string()),
                        elevation_prompt: None,
                        updated_scan: None,
                    },
                }
            };

            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    fn drain_worker_messages(&mut self, ctx: &egui::Context) {
        let _ = ctx;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::TraySyncAutoSwitch(enabled) => {
                    self.settings.auto_switch_on_startup = enabled;
                }
                WorkerMsg::ScanFinished {
                    result,
                    notify_regressions,
                } => {
                    self.busy = false;
                    self.busy_label.clear();
                    match result {
                        Ok(summary) => {
                            if notify_regressions {
                                notify_if_regressed(&summary);
                            }
                            self.summary = Some(summary);
                        }
                        Err(err) => self.error_message = Some(err),
                    }
                }
                WorkerMsg::ActionFinished {
                    status,
                    error,
                    elevation_prompt,
                    updated_scan,
                } => {
                    self.busy = false;
                    self.busy_label.clear();
                    if let Some(scan) = updated_scan {
                        self.summary = Some(scan);
                    }
                    self.status_message = status;
                    self.error_message = error;
                    if let Some(prompt) = elevation_prompt {
                        self.pending_elevation = Some(*prompt);
                    }
                }
                WorkerMsg::ProfileRuntimeUpdated(snap) => {
                    self.auto_switch_state
                        .store(snap.auto_switch_enabled, Ordering::SeqCst);
                    self.tray
                        .update_status(snap.auto_switch_enabled, &snap.active_profile_name);
                    if self.settings.auto_switch_on_startup != snap.auto_switch_enabled {
                        self.settings.auto_switch_on_startup = snap.auto_switch_enabled;
                        let _ = self
                            .settings
                            .save_atomic(&AppPaths::default_user_dir().settings_path());
                    }
                    self.profiles_ui.runtime_snapshot = Some(*snap);
                }
            }
        }
    }
}

impl eframe::App for PolledApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.main_hwnd.load(Ordering::Relaxed) == 0 {
            let found = find_own_main_hwnd();
            if found != 0 {
                self.main_hwnd.store(found, Ordering::SeqCst);
            }
        }

        // Minimize to system tray instead of exiting when clicking the window [X] button.
        if ctx.input(|i| i.viewport().close_requested())
            && !self.quit_requested.load(Ordering::SeqCst)
        {
            self.input_lab_ui.stop_capture();
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        self.drain_worker_messages(ctx);

        let p = self.theme.palette();
        let mode = self.theme;

        let (ok_count, info_count, warn_count, bad_count, fixable_safe) = match &self.summary {
            Some(s) => {
                let mut counts = (0usize, 0usize, 0usize, 0usize, 0usize);
                for c in &s.checks {
                    match c.severity {
                        Severity::Ok => counts.0 += 1,
                        Severity::Info => counts.1 += 1,
                        Severity::Warn => counts.2 += 1,
                        Severity::Bad => counts.3 += 1,
                    }
                    if c.can_fix
                        && !c.is_optional_tweak
                        && !c.requires_elevation
                        && c.severity >= Severity::Warn
                    {
                        counts.4 += 1;
                    }
                }
                counts
            }
            None => (0, 0, 0, 0, 0),
        };

        let undoable_set: HashSet<String> = self
            .summary
            .as_ref()
            .map(|s| s.undoable_check_ids.iter().cloned().collect())
            .unwrap_or_default();

        let mut toggle_theme = false;
        let mut toggle_autostart = false;
        let mut trigger_rescan = false;
        let mut trigger_fix_all_safe = false;
        let mut trigger_undo_all = false;
        let mut clicked_fix: Option<CheckReport> = None;
        let mut clicked_undo: Option<CheckReport> = None;
        let mut clicked_delete_custom: Option<String> = None;

        // --- TOP HEADER BAR ---
        egui::TopBottomPanel::top("topbar")
            .frame(
                egui::Frame::NONE
                    .fill(p.surface)
                    .inner_margin(Margin::symmetric(18, 12))
                    .stroke(Stroke::new(1.0_f32, p.border)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;

                // Row 1: App title & Windows build (left) | Startup & Theme (right)
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Polled").size(18.0).strong().color(p.text));
                    pill(
                        ui,
                        &format!("v{} · Experimental", env!("CARGO_PKG_VERSION")),
                        p.surface_alt,
                        p.text_muted,
                        p.border,
                    )
                    .on_hover_text(
                        "Early-development (0.x) experimental software. Original Windows values are snapshotted before changes; provided AS-IS under AGPL-3.0.",
                    );
                    if let Some(s) = &self.summary {
                        pill(
                            ui,
                            &format!("Win {}", s.windows_version),
                            p.surface_alt,
                            p.text_muted,
                            p.border,
                        );
                        if s.windows_build_changed {
                            pill(
                                ui,
                                "Windows Updated",
                                p.severity_bg(Severity::Warn, mode),
                                p.warn,
                                p.border,
                            );
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(secondary_button(self.theme.label(), p)).clicked() {
                            toggle_theme = true;
                        }

                        let startup_btn = if self.autostart_enabled {
                            primary_button("Startup: ON", p)
                        } else {
                            secondary_button("Startup: OFF", p)
                        };
                        if ui
                            .add(startup_btn)
                            .on_hover_text(
                                "Start Polled minimized to the system tray on Windows sign-in (HKCU Run, no admin required)",
                            )
                            .clicked()
                        {
                            toggle_autostart = true;
                        }
                    });
                });

                // Row 2: Navigation tabs (left) | Health Check bulk actions (right)
                ui.horizontal(|ui| {
                    let hc_btn = if self.active_view == ActiveView::HealthCheck {
                        primary_button("Health Check", p)
                    } else {
                        secondary_button("Health Check", p)
                    };
                    if ui.add(hc_btn).clicked() {
                        self.active_view = ActiveView::HealthCheck;
                    }

                    let prof_btn = if self.active_view == ActiveView::Profiles {
                        primary_button("Profiles", p)
                    } else {
                        secondary_button("Profiles", p)
                    };
                    if ui.add(prof_btn).clicked() {
                        self.active_view = ActiveView::Profiles;
                    }

                    let lab_label = if self.input_lab_ui.is_capturing() {
                        "Input Lab ●"
                    } else {
                        "Input Lab"
                    };
                    let lab_btn = if self.active_view == ActiveView::InputLab {
                        primary_button(lab_label, p)
                    } else {
                        secondary_button(lab_label, p)
                    };
                    if ui.add(lab_btn).clicked() {
                        self.active_view = ActiveView::InputLab;
                    }

                    if self.active_view == ActiveView::HealthCheck {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let undo_count = undoable_set.len();
                            if ui
                                .add_enabled(
                                    !self.busy && undo_count > 0,
                                    secondary_button(&format!("Undo All ({undo_count})"), p),
                                )
                                .clicked()
                            {
                                trigger_undo_all = true;
                            }

                            if ui
                                .add_enabled(
                                    !self.busy && fixable_safe > 0,
                                    primary_button(&format!("Fix Safe Issues ({fixable_safe})"), p),
                                )
                                .clicked()
                            {
                                trigger_fix_all_safe = true;
                            }

                            if ui
                                .add_enabled(!self.busy, secondary_button("Rescan", p))
                                .clicked()
                            {
                                trigger_rescan = true;
                            }
                        });
                    }
                });
            });

        // --- FOOTER ---
        egui::TopBottomPanel::bottom("footer")
            .frame(
                egui::Frame::NONE
                    .fill(p.surface)
                    .inner_margin(Margin::symmetric(18, 8))
                    .stroke(Stroke::new(1.0_f32, p.border)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("Close [X] minimizes to tray • No telemetry • AGPL-3.0")
                                .size(11.0)
                                .color(p.text_muted),
                        );
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!("Snapshot: {}", self.snapshot_path))
                                        .size(11.0)
                                        .monospace()
                                        .color(p.text_muted),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&self.snapshot_path);
                        });
                    });
                });
            });

        // --- MAIN CONTENT ---
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(18, 14)),
            )
            .show(ctx, |ui| {
                let content_w = (ctx.screen_rect().width() - 36.0).max(320.0);
                ui.set_max_width(content_w);

                if self.active_view == ActiveView::Profiles {
                    self.profiles_ui
                        .render(ctx, ui, p, mode, &self.profile_runtime);
                    return;
                }
                if self.active_view == ActiveView::InputLab {
                    let detected_dpi = self
                        .profiles_ui
                        .runtime_snapshot
                        .as_ref()
                        .and_then(|s| s.active_devices.first())
                        .and_then(|d| d.settings.as_ref())
                        .and_then(|st| st.dpi);
                    self.input_lab_ui.render(ctx, ui, p, mode, detected_dpi);
                    return;
                }

                // Regression alert banner
                if let Some(summary) = &self.summary {
                    if !summary.regressions.is_empty() {
                        banner_frame(ui, p, content_w, Some(p.warn), mode, |ui| {
                            let heading = if summary.windows_build_changed {
                                "Windows Update changed your input settings:"
                            } else {
                                "Regressions detected since your previous scan:"
                            };
                            ui.add(
                                egui::Label::new(RichText::new(heading).strong().color(p.warn))
                                    .wrap(),
                            );
                            for reg in &summary.regressions {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "• {}: {} -> {} ({})",
                                            reg.name,
                                            reg.previous.as_str().to_uppercase(),
                                            reg.current.as_str().to_uppercase(),
                                            reg.current_value
                                        ))
                                        .size(12.5)
                                        .color(p.text),
                                    )
                                    .wrap(),
                                );
                            }
                        });
                        ui.add_space(8.0);
                    }
                }

                // Transient status / error banners (neutral B&W for status, BAD red for errors)
                if let Some(msg) = self.status_message.clone() {
                    let mut dismiss = false;
                    banner_frame(ui, p, content_w, None, mode, |ui| {
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.add(secondary_button("Dismiss", p)).clicked() {
                                        dismiss = true;
                                    }
                                    ui.with_layout(
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            ui.add(
                                                egui::Label::new(RichText::new(&msg).color(p.text))
                                                    .wrap(),
                                            );
                                        },
                                    );
                                },
                            );
                        });
                    });
                    if dismiss {
                        self.status_message = None;
                    }
                    ui.add_space(8.0);
                }

                if let Some(err) = self.error_message.clone() {
                    let mut dismiss = false;
                    banner_frame(ui, p, content_w, Some(p.bad), mode, |ui| {
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.add(secondary_button("Dismiss", p)).clicked() {
                                        dismiss = true;
                                    }
                                    ui.with_layout(
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            ui.add(
                                                egui::Label::new(RichText::new(&err).color(p.bad))
                                                    .wrap(),
                                            );
                                        },
                                    );
                                },
                            );
                        });
                    });
                    if dismiss {
                        self.error_message = None;
                    }
                    ui.add_space(8.0);
                }

                // Filter tabs + Custom Tweak / Tips buttons + severity counters
                let total_count = self.summary.as_ref().map_or(0, |s| s.checks.len());
                let issues_count = warn_count + bad_count;
                let tweaks_count = self.summary.as_ref().map_or(0, |s| {
                    s.checks.iter().filter(|c| c.is_optional_tweak).count()
                });
                let undo_count = undoable_set.len();

                let draw_filter_tabs = |ui: &mut egui::Ui, filter: &mut FilterMode| {
                    filter_tab(
                        ui,
                        p,
                        filter,
                        FilterMode::All,
                        &format!("All ({total_count})"),
                    );
                    filter_tab(
                        ui,
                        p,
                        filter,
                        FilterMode::Issues,
                        &format!("Issues ({issues_count})"),
                    );
                    filter_tab(
                        ui,
                        p,
                        filter,
                        FilterMode::Tweaks,
                        &format!("Tweaks ({tweaks_count})"),
                    );
                    filter_tab(ui, p, filter, FilterMode::ReportOnly, "Report Only");
                    filter_tab(
                        ui,
                        p,
                        filter,
                        FilterMode::Undoable,
                        &format!("Undoable ({undo_count})"),
                    );
                };

                if content_w < 900.0 {
                    ui.horizontal_wrapped(|ui| {
                        draw_filter_tabs(ui, &mut self.filter);
                    });
                    ui.add_space(5.0);
                    ui.horizontal_wrapped(|ui| {
                        let tips_btn = if self.show_latency_tips {
                            primary_button("Latency Tips", p)
                        } else {
                            secondary_button("Latency Tips", p)
                        };
                        if ui.add(tips_btn).clicked() {
                            self.show_latency_tips = !self.show_latency_tips;
                        }
                        if ui.add(secondary_button("+ Custom Tweak", p)).clicked() {
                            self.custom_tweak_draft = CustomTweakDraft {
                                open: true,
                                ..Default::default()
                            };
                        }
                        ui.add_space(4.0);
                        if self.busy {
                            ui.spinner();
                            ui.label(
                                RichText::new(&self.busy_label)
                                    .size(12.0)
                                    .color(p.text_muted),
                            );
                        } else {
                            pill(ui, &format!("{ok_count} OK"), p.surface, p.ok, p.border);
                            pill(
                                ui,
                                &format!("{info_count} Info"),
                                p.surface,
                                p.info,
                                p.border,
                            );
                            pill(
                                ui,
                                &format!("{warn_count} Warn"),
                                p.surface,
                                p.warn,
                                p.border,
                            );
                            pill(ui, &format!("{bad_count} Bad"), p.surface, p.bad, p.border);
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        draw_filter_tabs(ui, &mut self.filter);
                        ui.add_space(4.0);
                        let tips_btn = if self.show_latency_tips {
                            primary_button("Latency Tips", p)
                        } else {
                            secondary_button("Latency Tips", p)
                        };
                        if ui.add(tips_btn).clicked() {
                            self.show_latency_tips = !self.show_latency_tips;
                        }
                        if ui.add(secondary_button("+ Custom Tweak", p)).clicked() {
                            self.custom_tweak_draft = CustomTweakDraft {
                                open: true,
                                ..Default::default()
                            };
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if self.busy {
                                ui.spinner();
                                ui.label(
                                    RichText::new(&self.busy_label)
                                        .size(12.0)
                                        .color(p.text_muted),
                                );
                            } else {
                                pill(ui, &format!("{bad_count} Bad"), p.surface, p.bad, p.border);
                                pill(
                                    ui,
                                    &format!("{warn_count} Warn"),
                                    p.surface,
                                    p.warn,
                                    p.border,
                                );
                                pill(
                                    ui,
                                    &format!("{info_count} Info"),
                                    p.surface,
                                    p.info,
                                    p.border,
                                );
                                pill(ui, &format!("{ok_count} OK"), p.surface, p.ok, p.border);
                            }
                        });
                    });
                }

                if self.show_latency_tips {
                    ui.add_space(8.0);
                    render_latency_tips_card(ui, p, content_w);
                }

                ui.add_space(10.0);

                let card_w = (content_w - 14.0).max(240.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let Some(summary) = &self.summary else {
                            ui.centered_and_justified(|ui| {
                                ui.label(
                                    RichText::new("Scanning Windows input settings...")
                                        .color(p.text_muted),
                                );
                            });
                            return;
                        };

                        let visible: Vec<&CheckReport> = summary
                            .checks
                            .iter()
                            .filter(|c| match self.filter {
                                FilterMode::All => true,
                                FilterMode::Issues => c.severity >= Severity::Warn,
                                FilterMode::Tweaks => c.is_optional_tweak,
                                FilterMode::ReportOnly => !c.can_fix,
                                FilterMode::Undoable => undoable_set.contains(&c.id),
                            })
                            .collect();

                        if visible.is_empty() {
                            egui::Frame::NONE
                                .fill(p.surface)
                                .stroke(Stroke::new(1.0_f32, p.border))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::same(28))
                                .show(ui, |ui| {
                                    ui.centered_and_justified(|ui| {
                                        ui.label(
                                            RichText::new("No checks match the selected filter.")
                                                .color(p.text_muted),
                                        );
                                    });
                                });
                            return;
                        }

                        for check in visible {
                            let is_undoable = undoable_set.contains(&check.id);
                            render_check_card(
                                ui,
                                p,
                                mode,
                                card_w,
                                check,
                                is_undoable,
                                self.busy,
                                &mut clicked_fix,
                                &mut clicked_undo,
                                &mut clicked_delete_custom,
                            );
                            ui.add_space(8.0);
                        }
                    });
            });

        // --- CUSTOM TWEAK BUILDER MODAL ---
        let mut save_custom_tweak = false;
        let mut close_custom_modal = false;
        if self.custom_tweak_draft.open {
            let modal_w = (ctx.screen_rect().width() - 48.0).clamp(340.0, 560.0);
            egui::Window::new("Add Custom Reversible Tweak")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .default_width(modal_w)
                .max_width(modal_w)
                .frame(
                    egui::Frame::NONE
                        .fill(p.surface)
                        .stroke(Stroke::new(1.5_f32, p.text))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(16)),
                )
                .show(ctx, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Every custom tweak automatically snapshots its original Windows value into baseline_snapshot.json before applying, so you can revert it cleanly anytime.",
                            )
                            .size(12.0)
                            .color(p.text_muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Caution: Incorrect registry edits can affect Windows stability. Verify the hive, subkey, type, and value (and create a Windows System Restore Point before editing HKLM).",
                            )
                            .size(11.5)
                            .color(p.warn),
                        )
                        .wrap(),
                    );
                    ui.add_space(8.0);

                    ui.label(
                        RichText::new("Quick-fill curated presets:")
                            .size(11.0)
                            .strong()
                            .color(p.text_muted),
                    );
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        for preset in curated_custom_tweak_presets() {
                            if ui.add(secondary_button(&preset.name, p)).clicked() {
                                self.custom_tweak_draft.fill_from_preset(&preset);
                            }
                        }
                    });
                    ui.add_space(10.0);

                    let field_w = (modal_w - 150.0).clamp(180.0, 380.0);
                    egui::Grid::new("custom_tweak_grid")
                        .num_columns(2)
                        .spacing([12.0, 7.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new("Name").color(p.text_muted));
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.custom_tweak_draft.name)
                                        .desired_width(field_w)
                                        .hint_text("e.g. Disable Power Throttling"),
                                )
                                .changed()
                            {
                                self.custom_tweak_draft.preset_id = None;
                            }
                            ui.end_row();

                            ui.label(RichText::new("Registry Hive").color(p.text_muted));
                            ui.horizontal(|ui| {
                                for (hive, lbl) in [
                                    (RegHive::Hkcu, "HKCU (User, no admin)"),
                                    (RegHive::Hklm, "HKLM (System, UAC)"),
                                ] {
                                    let btn = if self.custom_tweak_draft.hive == hive {
                                        primary_button(lbl, p)
                                    } else {
                                        secondary_button(lbl, p)
                                    };
                                    if ui.add(btn).clicked() {
                                        self.custom_tweak_draft.hive = hive;
                                    }
                                }
                            });
                            ui.end_row();

                            ui.label(RichText::new("Subkey Path").color(p.text_muted));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.custom_tweak_draft.subkey)
                                    .desired_width(field_w)
                                    .hint_text(r"SYSTEM\CurrentControlSet\Control\Power\PowerThrottling"),
                            );
                            ui.end_row();

                            ui.label(RichText::new("Value Name").color(p.text_muted));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.custom_tweak_draft.value_name)
                                    .desired_width(field_w)
                                    .hint_text("PowerThrottlingOff"),
                            );
                            ui.end_row();

                            ui.label(RichText::new("Value Type").color(p.text_muted));
                            ui.horizontal(|ui| {
                                for (vt, lbl) in [
                                    (CustomDraftType::Dword, "DWORD (dec or 0x hex)"),
                                    (CustomDraftType::String, "String (REG_SZ)"),
                                ] {
                                    let btn = if self.custom_tweak_draft.val_type == vt {
                                        primary_button(lbl, p)
                                    } else {
                                        secondary_button(lbl, p)
                                    };
                                    if ui.add(btn).clicked() {
                                        self.custom_tweak_draft.val_type = vt;
                                    }
                                }
                            });
                            ui.end_row();

                            ui.label(RichText::new("Target Value").color(p.text_muted));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.custom_tweak_draft.value_text)
                                    .desired_width(field_w)
                                    .hint_text("1 or 0x2A"),
                            );
                            ui.end_row();

                            ui.label(RichText::new("Notes").color(p.text_muted));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.custom_tweak_draft.notes)
                                    .desired_width(field_w)
                                    .hint_text("Why you use this tweak"),
                            );
                            ui.end_row();

                            ui.label("");
                            ui.checkbox(
                                &mut self.custom_tweak_draft.requires_reboot,
                                "Requires reboot to take effect",
                            );
                            ui.end_row();
                        });

                    if let Some(err) = &self.custom_tweak_draft.error {
                        ui.add_space(8.0);
                        ui.add(
                            egui::Label::new(RichText::new(err).size(12.0).color(p.bad)).wrap(),
                        );
                    }

                    ui.add_space(12.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(primary_button("Save Tweak", p)).clicked() {
                            save_custom_tweak = true;
                        }
                        if ui.add(secondary_button("Cancel", p)).clicked() {
                            close_custom_modal = true;
                        }
                    });
                });
        }
        if close_custom_modal {
            self.custom_tweak_draft.open = false;
        } else if save_custom_tweak {
            match self.custom_tweak_draft.build_def() {
                Ok(def) => {
                    let paths = AppPaths::default_user_dir();
                    let mut store = CustomTweakStore::load_or_default(&paths.custom_tweaks_path());
                    let name = def.name.clone();
                    match store.upsert(def).and_then(|_| {
                        store
                            .save_atomic(&paths.custom_tweaks_path())
                            .map_err(|e| e.to_string())
                    }) {
                        Ok(()) => {
                            self.custom_tweak_draft.open = false;
                            self.filter = FilterMode::Tweaks;
                            self.status_message = Some(format!(
                                "Saved custom tweak \"{name}\". Click Apply on its card to test it (full undo is saved automatically)."
                            ));
                            self.error_message = None;
                            self.start_scan(ctx, false);
                        }
                        Err(err) => self.custom_tweak_draft.error = Some(err),
                    }
                }
                Err(err) => self.custom_tweak_draft.error = Some(err),
            }
        }

        if let Some(del_id) = clicked_delete_custom {
            if undoable_set.contains(&del_id) {
                self.error_message = Some(
                    "Undo this custom tweak first before deleting it, so its baseline snapshot stays cleanly restorable."
                        .to_string(),
                );
            } else {
                let paths = AppPaths::default_user_dir();
                let mut store = CustomTweakStore::load_or_default(&paths.custom_tweaks_path());
                if store.remove(&del_id) {
                    if let Err(err) = store.save_atomic(&paths.custom_tweaks_path()) {
                        self.error_message = Some(err.to_string());
                    } else {
                        self.status_message = Some(format!("Deleted custom tweak '{del_id}'."));
                        self.start_scan(ctx, false);
                    }
                }
            }
        }

        // --- CONFIRM RISKY / OPTIONAL / CUSTOM NON-ELEVATED CHANGE MODAL ---
        let mut confirm_risky = false;
        let mut cancel_risky = false;
        if let Some(pending) = &self.pending_confirm {
            let modal_w = (ctx.screen_rect().width() - 48.0).clamp(340.0, 470.0);
            let tags = build_fix_impact_tags(&pending.check);
            egui::Window::new("Confirm System Change")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .default_width(modal_w)
                .max_width(modal_w)
                .frame(
                    egui::Frame::NONE
                        .fill(p.surface)
                        .stroke(Stroke::new(1.5_f32, p.text))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(18)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        pill(
                            ui,
                            "WARNING",
                            p.severity_bg(Severity::Warn, mode),
                            p.warn,
                            p.border,
                        );
                        for tag in &tags {
                            pill(ui, tag, p.surface_alt, p.text_muted, p.border);
                        }
                    });
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(&pending.check.name)
                            .size(15.0)
                            .strong()
                            .color(p.text),
                    );
                    ui.add_space(6.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&pending.warning)
                                .size(12.5)
                                .color(p.text_muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(8.0);

                    egui::Frame::NONE
                        .fill(p.surface_alt)
                        .stroke(Stroke::new(1.0_f32, p.border))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("PLANNED CHANGE")
                                    .size(10.0)
                                    .strong()
                                    .color(p.text_muted),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{}  ->  {}",
                                        pending.check.current_value,
                                        pending.check.recommended_value
                                    ))
                                    .size(12.0)
                                    .monospace()
                                    .color(p.text),
                                )
                                .wrap(),
                            );
                            ui.add_space(4.0);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(
                                        "Your exact original value is saved to baseline_snapshot.json before any change so you can click Undo anytime.",
                                    )
                                    .size(11.5)
                                    .color(p.text_muted),
                                )
                                .wrap(),
                            );
                        });

                    ui.add_space(14.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!self.busy, primary_button("Apply Change", p))
                            .clicked()
                        {
                            confirm_risky = true;
                        }
                        if ui
                            .add_enabled(!self.busy, secondary_button("Cancel", p))
                            .clicked()
                        {
                            cancel_risky = true;
                        }
                    });
                });
        }

        // --- UAC CONFIRMATION MODAL (plain monochrome) ---
        let mut confirm_elevation = false;
        let mut cancel_elevation = false;
        if let Some(pending) = &self.pending_elevation {
            let modal_w = (ctx.screen_rect().width() - 48.0).clamp(340.0, 480.0);
            egui::Window::new("Administrator permission needed")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .default_width(modal_w)
                .max_width(modal_w)
                .frame(
                    egui::Frame::NONE
                        .fill(p.surface)
                        .stroke(Stroke::new(1.5_f32, p.text))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(18)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        pill(
                            ui,
                            "UAC",
                            p.severity_bg(Severity::Warn, mode),
                            p.warn,
                            p.border,
                        );
                        for tag in &pending.impact_tags {
                            pill(ui, tag, p.surface_alt, p.text_muted, p.border);
                        }
                    });
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(&pending.title)
                            .size(15.0)
                            .strong()
                            .color(p.text),
                    );
                    ui.add_space(6.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&pending.reason).size(12.5).color(p.text_muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(10.0);

                    egui::Frame::NONE
                        .fill(p.surface_alt)
                        .stroke(Stroke::new(1.0_f32, p.border))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            if let Some(summary) = &pending.change_summary {
                                ui.label(
                                    RichText::new("PLANNED CHANGE")
                                        .size(10.0)
                                        .strong()
                                        .color(p.text_muted),
                                );
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(summary)
                                            .size(12.0)
                                            .monospace()
                                            .color(p.text),
                                    )
                                    .wrap(),
                                );
                                ui.add_space(6.0);
                            }
                            ui.add(
                                egui::Label::new(
                                    RichText::new(
                                        "Polled stays non-elevated and launches polled.exe only for this single action. Your original setting is saved to baseline_snapshot.json before any change.",
                                    )
                                    .size(12.0)
                                    .color(p.text_muted),
                                )
                                .wrap(),
                            );
                            if pending.restore_point_hint {
                                ui.add_space(4.0);
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(
                                            "Tip: Consider creating a Windows System Restore Point before making system-wide (HKLM) changes.",
                                        )
                                        .size(11.5)
                                        .color(p.warn),
                                    )
                                    .wrap(),
                                );
                            }
                        });

                    ui.add_space(14.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!self.busy, primary_button(&pending.action_label, p))
                            .clicked()
                        {
                            confirm_elevation = true;
                        }
                        if ui
                            .add_enabled(!self.busy, secondary_button("Cancel", p))
                            .clicked()
                        {
                            cancel_elevation = true;
                        }
                    });
                });
        }

        if toggle_theme {
            self.theme = self.theme.toggle();
            self.theme.apply_to(ctx);
            self.settings.theme = ThemePreference::from(self.theme);
            let _ = self
                .settings
                .save_atomic(&AppPaths::default_user_dir().settings_path());
        }

        if toggle_autostart {
            let target = !self.autostart_enabled;
            let result = std::env::current_exe()
                .map_err(|e| format!("Could not resolve current executable path: {e}"))
                .and_then(|exe| {
                    set_autostart_enabled(&WindowsEnv, target, &exe).map_err(|e| e.to_string())
                });
            match result {
                Ok(()) => {
                    self.autostart_enabled = target;
                    let msg = if target {
                        "Enabled Start with Windows (launches minimized to tray via HKCU\\...\\Run)."
                            .to_string()
                    } else {
                        "Disabled Start with Windows (removed HKCU\\...\\Run entry).".to_string()
                    };
                    match self.active_view {
                        ActiveView::Profiles => {
                            self.profiles_ui.notice = Some((Severity::Ok, msg));
                        }
                        ActiveView::InputLab => {
                            self.input_lab_ui.notice = Some((Severity::Ok, msg));
                        }
                        ActiveView::HealthCheck => {
                            self.status_message = Some(msg);
                            self.error_message = None;
                        }
                    }
                }
                Err(err) => match self.active_view {
                    ActiveView::Profiles => {
                        self.profiles_ui.notice = Some((Severity::Bad, err));
                    }
                    ActiveView::InputLab => {
                        self.input_lab_ui.notice = Some((Severity::Bad, err));
                    }
                    ActiveView::HealthCheck => {
                        self.error_message = Some(err);
                    }
                },
            }
        }

        if cancel_risky {
            self.pending_confirm = None;
        } else if confirm_risky {
            if let Some(pending) = self.pending_confirm.take() {
                self.start_fix_check(ctx, pending.check, false);
            }
        } else if cancel_elevation {
            self.pending_elevation = None;
        } else if confirm_elevation {
            if let Some(pending) = self.pending_elevation.take() {
                match pending.target {
                    ElevationTarget::Fix(id) => {
                        if let Some(check) = self
                            .summary
                            .as_ref()
                            .and_then(|s| s.checks.iter().find(|c| c.id == id))
                            .cloned()
                        {
                            self.start_fix_check(ctx, check, true);
                        }
                    }
                    ElevationTarget::Undo(id) => {
                        if let Some(check) = self
                            .summary
                            .as_ref()
                            .and_then(|s| s.checks.iter().find(|c| c.id == id))
                            .cloned()
                        {
                            self.start_undo_check(ctx, check, true);
                        }
                    }
                    ElevationTarget::UndoAll => self.start_undo_all(ctx, true),
                }
            }
        } else if trigger_rescan {
            self.start_scan(ctx, false);
        } else if trigger_fix_all_safe {
            self.start_fix_all_safe(ctx);
        } else if trigger_undo_all {
            self.start_undo_all(ctx, false);
        } else if let Some(check) = clicked_fix {
            if check.requires_elevation {
                let reason = check.elevation_reason.clone().unwrap_or_else(|| {
                    "Windows requires administrator permissions for this setting.".to_string()
                });
                self.pending_elevation = Some(build_elevation_prompt_for_fix(&check, reason));
            } else if check.id.starts_with("custom_")
                || check.is_optional_tweak
                || check.requires_reboot
                || check.requires_relogin
            {
                let warning = build_non_elevated_confirm_warning(&check);
                self.pending_confirm = Some(PendingConfirm { check, warning });
            } else {
                self.start_fix_check(ctx, check, false);
            }
        } else if let Some(check) = clicked_undo {
            if check.requires_elevation {
                self.pending_elevation = Some(PendingElevation {
                    target: ElevationTarget::Undo(check.id.clone()),
                    title: format!("Undo \"{}\"", check.name),
                    reason: check.elevation_reason.clone().unwrap_or_else(|| {
                        "Restoring this system setting requires administrator permissions."
                            .to_string()
                    }),
                    action_label: "Restore with UAC".to_string(),
                    change_summary: None,
                    impact_tags: Vec::new(),
                    restore_point_hint: false,
                });
            } else {
                self.start_undo_check(ctx, check, false);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_check_card(
    ui: &mut egui::Ui,
    p: Palette,
    mode: ThemeMode,
    outer_w: f32,
    check: &CheckReport,
    is_undoable: bool,
    busy: bool,
    clicked_fix: &mut Option<CheckReport>,
    clicked_undo: &mut Option<CheckReport>,
    clicked_delete_custom: &mut Option<String>,
) {
    let sev_fg = p.severity_fg(check.severity);
    let sev_bg = p.severity_bg(check.severity, mode);
    let is_custom = check.id.starts_with("custom_");
    let show_fix = check.can_fix
        && (check.severity >= Severity::Warn
            || ((check.is_optional_tweak || check.id == "power_plan")
                && check.severity == Severity::Info));

    let inner_w = (outer_w - 26.0).max(160.0);
    let frame = egui::Frame::NONE
        .fill(p.surface)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(13));

    let resp = frame.show(ui, |ui| {
        ui.set_min_width(inner_w);
        ui.set_max_width(inner_w);

        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if show_fix {
                    let verb = if check.is_optional_tweak {
                        "Apply"
                    } else {
                        "Fix"
                    };
                    let label = if check.requires_elevation {
                        format!("{verb} [Admin]")
                    } else {
                        verb.to_string()
                    };
                    if ui.add_enabled(!busy, primary_button(&label, p)).clicked() {
                        *clicked_fix = Some(check.clone());
                    }
                }

                if is_undoable && ui.add_enabled(!busy, secondary_button("Undo", p)).clicked() {
                    *clicked_undo = Some(check.clone());
                }

                if is_custom
                    && ui
                        .add_enabled(!busy, secondary_button("Delete", p))
                        .clicked()
                {
                    *clicked_delete_custom = Some(check.id.clone());
                }

                if check.requires_reboot && check.can_fix {
                    pill(ui, "Reboot", p.surface_alt, p.text_muted, p.border);
                }

                if check.requires_relogin && check.severity >= Severity::Warn {
                    pill(
                        ui,
                        "Sign-out required",
                        p.surface_alt,
                        p.text_muted,
                        p.border,
                    );
                }

                if check.is_optional_tweak {
                    pill(
                        ui,
                        if is_custom {
                            "Custom tweak"
                        } else {
                            "Optional tweak"
                        },
                        p.surface_alt,
                        p.text_muted,
                        p.border,
                    );
                }

                if !check.can_fix {
                    pill(ui, "Report only", p.surface_alt, p.text_muted, p.border);
                }

                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    pill(
                        ui,
                        &check.severity.as_str().to_uppercase(),
                        sev_bg,
                        sev_fg,
                        p.border,
                    );
                    ui.add(
                        egui::Label::new(
                            RichText::new(&check.name).size(14.5).strong().color(p.text),
                        )
                        .truncate(),
                    );
                    if inner_w >= 640.0 {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&check.id)
                                    .size(11.5)
                                    .monospace()
                                    .color(p.text_muted),
                            )
                            .truncate(),
                        );
                    }
                });
            });
        });

        ui.add_space(8.0);

        if inner_w < 540.0 {
            value_box(ui, p, inner_w, "CURRENT", &check.current_value, p.text);
            ui.add_space(6.0);
            value_box(
                ui,
                p,
                inner_w,
                "RECOMMENDED",
                &check.recommended_value,
                p.text_muted,
            );
        } else {
            let col_gap = 8.0;
            let col_w = ((inner_w - col_gap) * 0.5).max(120.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = col_gap;
                value_box(ui, p, col_w, "CURRENT", &check.current_value, p.text);
                value_box(
                    ui,
                    p,
                    col_w,
                    "RECOMMENDED",
                    &check.recommended_value,
                    p.text_muted,
                );
            });
        }

        ui.add_space(7.0);
        ui.add(
            egui::Label::new(RichText::new(&check.reason).size(12.2).color(p.text_muted)).wrap(),
        );
    });

    let rect = resp.response.rect;
    let stripe = egui::Rect::from_min_max(
        rect.left_top() + egui::vec2(1.0, 2.0),
        egui::pos2(rect.left() + 4.5, rect.bottom() - 2.0),
    );
    ui.painter()
        .rect_filled(stripe, CornerRadius::same(2), sev_fg);
}

fn render_latency_tips_card(ui: &mut egui::Ui, p: Palette, outer_w: f32) {
    let inner_w = (outer_w - 24.0).max(160.0);
    egui::Frame::NONE
        .fill(p.surface)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(inner_w);
            ui.set_max_width(inner_w);
            ui.label(
                RichText::new("Minimum Mouse Input Delay Checklist (Hardware, USB, GPU & In-Game)")
                    .size(13.5)
                    .strong()
                    .color(p.text),
            );
            ui.add_space(6.0);
            let tips = [
                (
                    "USB & Wireless:",
                    "Plug your 4K/8K receiver directly into a rear motherboard CPU-lane USB port (avoid front-panel or unpowered hubs). Keep the dongle extender < 20 cm from your mousepad and away from 2.4 GHz Wi-Fi routers.",
                ),
                (
                    "Sensor & DPI:",
                    "1600–3200 DPI cuts motion-initiation latency by ~1–2 ms vs. 400 DPI on micro-adjustments (adjust in-game sensitivity to keep the same cm/360°). Verify 4K/8K saturation & switch debounce in Input Lab.",
                ),
                (
                    "GPU & Frame Pacing:",
                    "Avoid 99% GPU saturation (render queue buildup adds 10–30 ms of mouse delay). Enable NVIDIA Reflex (On+Boost) or AMD Anti-Lag, cap FPS slightly below your GPU/monitor limit, and disable V-Sync unless paired with G-Sync/FreeSync + Reflex.",
                ),
                (
                    "Display & OS:",
                    "Use Exclusive Fullscreen or Windows 11 Borderless with Flip-Model Windowed Optimizations enabled. Keep USB Selective Suspend and PCIe ASPM off on AC, and close heavy vendor RGB/peripheral suites after saving to onboard memory.",
                ),
            ];
            for (heading, body) in tips {
                let mut job = egui::text::LayoutJob::default();
                job.wrap.max_width = inner_w;
                job.append(
                    &format!("• {heading} "),
                    0.0,
                    egui::TextFormat {
                        font_id: egui::FontId::proportional(12.0),
                        color: p.text,
                        ..Default::default()
                    },
                );
                job.append(
                    body,
                    0.0,
                    egui::TextFormat {
                        font_id: egui::FontId::proportional(12.0),
                        color: p.text_muted,
                        ..Default::default()
                    },
                );
                ui.add(egui::Label::new(job).wrap());
                ui.add_space(3.0);
            }
        });
}

fn primary_button(label: &str, p: Palette) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(p.btn_primary_fg).strong())
        .fill(p.btn_primary_bg)
        .corner_radius(CornerRadius::same(6))
}

fn secondary_button(label: &str, p: Palette) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(p.text))
        .fill(p.surface_alt)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(6))
}

fn value_box(
    ui: &mut egui::Ui,
    p: Palette,
    outer_w: f32,
    label: &str,
    value: &str,
    text_color: Color32,
) {
    let inner_w = (outer_w - 20.0).max(100.0);
    egui::Frame::NONE
        .fill(p.surface_alt)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_min_width(inner_w);
            ui.set_max_width(inner_w);
            ui.label(RichText::new(label).size(10.0).strong().color(p.text_muted));
            ui.add(egui::Label::new(RichText::new(value).size(12.5).color(text_color)).wrap());
        });
}

fn banner_frame(
    ui: &mut egui::Ui,
    p: Palette,
    outer_w: f32,
    semantic_accent: Option<Color32>,
    mode: ThemeMode,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let (fill, border) = match semantic_accent {
        Some(c) => {
            let alpha = match mode {
                ThemeMode::Dark => 28,
                ThemeMode::Light => 20,
            };
            (
                Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha),
                Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 120),
            )
        }
        None => (p.surface, p.border),
    };
    let inner_w = (outer_w - 24.0).max(120.0);
    egui::Frame::NONE
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_min_width(inner_w);
            ui.set_max_width(inner_w);
            add_contents(ui);
        });
}

fn filter_tab(
    ui: &mut egui::Ui,
    p: Palette,
    current: &mut FilterMode,
    target: FilterMode,
    label: &str,
) {
    let selected = *current == target;
    let btn = egui::Button::new(
        RichText::new(label)
            .size(12.5)
            .color(if selected { p.text } else { p.text_muted })
            .strong(),
    )
    .fill(if selected { p.surface_alt } else { p.surface })
    .stroke(Stroke::new(
        1.0_f32,
        if selected {
            p.border
        } else {
            Color32::TRANSPARENT
        },
    ))
    .corner_radius(CornerRadius::same(6));

    if ui.add(btn).clicked() {
        *current = target;
    }
}

/// Pre-measured pill badge so `ui.horizontal_wrapped` wraps pills cleanly on resize.
fn pill(
    ui: &mut egui::Ui,
    text: &str,
    bg: Color32,
    fg: Color32,
    border: Color32,
) -> egui::Response {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_string(), egui::FontId::proportional(11.0), fg);
    let pad_x = 8.0;
    let pad_y = 3.0;
    let desired = Vec2::new(galley.size().x + pad_x * 2.0, galley.size().y + pad_y * 2.0);
    let (rect, resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        painter.rect(
            rect,
            CornerRadius::same(10),
            bg,
            Stroke::new(1.0_f32, border),
            egui::StrokeKind::Inside,
        );
        let pos = egui::pos2(rect.left() + pad_x, rect.center().y - galley.size().y * 0.5);
        painter.galley(pos, galley, fg);
    }
    resp
}
