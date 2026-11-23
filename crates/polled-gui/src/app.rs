// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Vec2};
use polled_core::win::WindowsEnv;
use polled_core::{
    is_autostart_enabled, set_autostart_enabled, AppPaths, AppSettings, CheckReport, HealthEngine,
    PolledError, ScanSummary, Severity, ThemePreference,
};

use crate::elevation::{run_elevated_cli, ElevatedAction};
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterMode {
    All,
    Issues,
    ReportOnly,
    Undoable,
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
        elevation_prompt: Option<PendingElevation>,
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

    profiles_ui: ProfilesUiState,
    profile_runtime: ProfileRuntimeHandle,

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
            profiles_ui,
            profile_runtime,
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
            let engine = HealthEngine::with_default_checks();
            let result = engine
                .scan_all(&WindowsEnv, &AppPaths::default_user_dir(), true)
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
            let engine = HealthEngine::with_default_checks();

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
                            elevation_prompt: Some(PendingElevation {
                                target: ElevationTarget::Fix(check.id.clone()),
                                title: format!("Fix \"{}\"", check.name),
                                reason,
                                action_label: "Continue with UAC".to_string(),
                            }),
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
            let engine = HealthEngine::with_default_checks();
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
            let engine = HealthEngine::with_default_checks();

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
                            elevation_prompt: Some(PendingElevation {
                                target: ElevationTarget::Undo(check.id.clone()),
                                title: format!("Undo \"{}\"", check.name),
                                reason,
                                action_label: "Restore with UAC".to_string(),
                            }),
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
            let engine = HealthEngine::with_default_checks();

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
                                elevation_prompt: Some(PendingElevation {
                                    target: ElevationTarget::UndoAll,
                                    title: "Restore remaining system-level settings".to_string(),
                                    reason: "One or more settings in your snapshot require administrator permissions to restore."
                                        .to_string(),
                                    action_label: "Restore remaining with UAC".to_string(),
                                }),
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
                    if elevation_prompt.is_some() {
                        self.pending_elevation = elevation_prompt;
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
                    if c.can_fix && !c.requires_elevation && c.severity >= Severity::Warn {
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
                            RichText::new("Close [X] minimizes to tray • No telemetry • GPL-3.0")
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

                // Filter tabs + severity summary counters (reflows cleanly on narrow windows)
                let total_count = self.summary.as_ref().map_or(0, |s| s.checks.len());
                let issues_count = warn_count + bad_count;
                let undo_count = undoable_set.len();

                if content_w < 680.0 {
                    ui.horizontal_wrapped(|ui| {
                        filter_tab(
                            ui,
                            p,
                            &mut self.filter,
                            FilterMode::All,
                            &format!("All ({total_count})"),
                        );
                        filter_tab(
                            ui,
                            p,
                            &mut self.filter,
                            FilterMode::Issues,
                            &format!("Issues ({issues_count})"),
                        );
                        filter_tab(
                            ui,
                            p,
                            &mut self.filter,
                            FilterMode::ReportOnly,
                            "Report Only",
                        );
                        filter_tab(
                            ui,
                            p,
                            &mut self.filter,
                            FilterMode::Undoable,
                            &format!("Undoable ({undo_count})"),
                        );
                    });
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
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
                        filter_tab(
                            ui,
                            p,
                            &mut self.filter,
                            FilterMode::All,
                            &format!("All ({total_count})"),
                        );
                        filter_tab(
                            ui,
                            p,
                            &mut self.filter,
                            FilterMode::Issues,
                            &format!("Issues ({issues_count})"),
                        );
                        filter_tab(
                            ui,
                            p,
                            &mut self.filter,
                            FilterMode::ReportOnly,
                            "Report Only",
                        );
                        filter_tab(
                            ui,
                            p,
                            &mut self.filter,
                            FilterMode::Undoable,
                            &format!("Undoable ({undo_count})"),
                        );

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
                            );
                            ui.add_space(8.0);
                        }
                    });
            });

        // --- UAC CONFIRMATION MODAL (plain monochrome) ---
        let mut confirm_elevation = false;
        let mut cancel_elevation = false;
        if let Some(pending) = &self.pending_elevation {
            egui::Window::new("Administrator permission needed")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .default_width(440.0)
                .max_width(440.0)
                .frame(
                    egui::Frame::NONE
                        .fill(p.surface)
                        .stroke(Stroke::new(1.5_f32, p.text))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(18)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        pill(ui, "UAC", p.surface_alt, p.text, p.border);
                        ui.label(
                            RichText::new(&pending.title)
                                .size(15.0)
                                .strong()
                                .color(p.text),
                        );
                    });
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(RichText::new(&pending.reason).size(13.0).color(p.text_muted))
                            .wrap(),
                    );
                    ui.add_space(10.0);

                    egui::Frame::NONE
                        .fill(p.surface_alt)
                        .stroke(Stroke::new(1.0_f32, p.border))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
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
                    if self.active_view == ActiveView::Profiles {
                        self.profiles_ui.notice = Some((Severity::Ok, msg));
                    } else {
                        self.status_message = Some(msg);
                        self.error_message = None;
                    }
                }
                Err(err) => {
                    if self.active_view == ActiveView::Profiles {
                        self.profiles_ui.notice = Some((Severity::Bad, err));
                    } else {
                        self.error_message = Some(err);
                    }
                }
            }
        }

        if cancel_elevation {
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
                self.pending_elevation = Some(PendingElevation {
                    target: ElevationTarget::Fix(check.id.clone()),
                    title: format!("Fix \"{}\"", check.name),
                    reason: check.elevation_reason.clone().unwrap_or_else(|| {
                        "Windows requires administrator permissions for this setting.".to_string()
                    }),
                    action_label: "Continue with UAC".to_string(),
                });
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
) {
    let sev_fg = p.severity_fg(check.severity);
    let sev_bg = p.severity_bg(check.severity, mode);
    let show_fix = check.can_fix
        && (check.severity >= Severity::Warn
            || (check.id == "power_plan" && check.severity == Severity::Info));

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
                    let label = if check.requires_elevation {
                        "Fix [Admin]"
                    } else {
                        "Fix"
                    };
                    if ui.add_enabled(!busy, primary_button(label, p)).clicked() {
                        *clicked_fix = Some(check.clone());
                    }
                }

                if is_undoable && ui.add_enabled(!busy, secondary_button("Undo", p)).clicked() {
                    *clicked_undo = Some(check.clone());
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
fn pill(ui: &mut egui::Ui, text: &str, bg: Color32, fg: Color32, border: Color32) {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_string(), egui::FontId::proportional(11.0), fg);
    let pad_x = 8.0;
    let pad_y = 3.0;
    let desired = Vec2::new(galley.size().x + pad_x * 2.0, galley.size().y + pad_y * 2.0);
    let (rect, _resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
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
}
