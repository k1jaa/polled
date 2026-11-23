// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Vec2};
use polled_core::win::WindowsEnv;
use polled_core::{AppPaths, CheckReport, HealthEngine, PolledError, ScanSummary, Severity};

use crate::elevation::{run_elevated_cli, ElevatedAction};
use crate::notifications::{notify_if_regressed, spawn_windows_build_watcher};
use crate::tray::TrayController;

// Theme palette matching Polled's dark utility aesthetic.
const BG_DARK: Color32 = Color32::from_rgb(13, 16, 21);
const SURFACE: Color32 = Color32::from_rgb(21, 26, 34);
const SURFACE_ALT: Color32 = Color32::from_rgb(27, 34, 45);
const BORDER: Color32 = Color32::from_rgb(38, 48, 64);
const TEXT_PRIMARY: Color32 = Color32::from_rgb(230, 237, 245);
const TEXT_MUTED: Color32 = Color32::from_rgb(148, 163, 184);

const COLOR_OK: Color32 = Color32::from_rgb(34, 197, 94);
const COLOR_INFO: Color32 = Color32::from_rgb(56, 189, 248);
const COLOR_WARN: Color32 = Color32::from_rgb(245, 158, 11);
const COLOR_BAD: Color32 = Color32::from_rgb(239, 68, 68);
const COLOR_ACCENT: Color32 = Color32::from_rgb(16, 185, 129);
const COLOR_ADMIN: Color32 = Color32::from_rgb(217, 119, 6);

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
    TrayScanRequested,
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
}

pub struct PolledApp {
    summary: Option<ScanSummary>,
    snapshot_path: String,
    filter: FilterMode,
    busy: bool,
    busy_label: String,
    status_message: Option<String>,
    error_message: Option<String>,
    pending_elevation: Option<PendingElevation>,

    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
    quit_requested: Arc<AtomicBool>,
    _tray: TrayController,
}

impl PolledApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_dark_theme(&cc.egui_ctx);

        let (tx, rx) = mpsc::channel();
        let quit_requested = Arc::new(AtomicBool::new(false));
        let tray =
            TrayController::init(cc.egui_ctx.clone(), tx.clone(), Arc::clone(&quit_requested));
        spawn_windows_build_watcher(tx.clone(), cc.egui_ctx.clone());

        let snapshot_path = AppPaths::default_user_dir()
            .baseline_snapshot_path()
            .display()
            .to_string();

        let mut app = Self {
            summary: None,
            snapshot_path,
            filter: FilterMode::All,
            busy: false,
            busy_label: String::new(),
            status_message: None,
            error_message: None,
            pending_elevation: None,
            tx,
            rx,
            quit_requested,
            _tray: tray,
        };

        // Run initial health check on startup (notifies only if something regressed).
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
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::TrayScanRequested => self.start_scan(ctx, true),
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
            }
        }
    }
}

impl eframe::App for PolledApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Minimize to system tray instead of exiting when clicking the window [X] button.
        if ctx.input(|i| i.viewport().close_requested())
            && !self.quit_requested.load(Ordering::SeqCst)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        self.drain_worker_messages(ctx);

        // Compute counts & undoable set from latest scan.
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

        // Deferred UI actions (collected while rendering, executed right after).
        let mut trigger_rescan = false;
        let mut trigger_fix_all_safe = false;
        let mut trigger_undo_all = false;
        let mut clicked_fix: Option<CheckReport> = None;
        let mut clicked_undo: Option<CheckReport> = None;

        // --- TOP HEADER BAR ---
        egui::TopBottomPanel::top("topbar")
            .frame(
                egui::Frame::NONE
                    .fill(SURFACE)
                    .inner_margin(Margin::symmetric(18, 14))
                    .stroke(Stroke::new(1.0_f32, BORDER)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Brand + Windows build badge
                    let (dot_rect, _) =
                        ui.allocate_exact_size(Vec2::splat(12.0), egui::Sense::hover());
                    ui.painter()
                        .circle_filled(dot_rect.center(), 5.5, COLOR_ACCENT);

                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Polled")
                                    .size(18.0)
                                    .strong()
                                    .color(TEXT_PRIMARY),
                            );
                            if let Some(s) = &self.summary {
                                pill(
                                    ui,
                                    &format!("Win {}", s.windows_version),
                                    SURFACE_ALT,
                                    TEXT_MUTED,
                                );
                                if s.windows_build_changed {
                                    pill(
                                        ui,
                                        "Windows Updated",
                                        Color32::from_rgba_unmultiplied(245, 158, 11, 35),
                                        COLOR_WARN,
                                    );
                                }
                            }
                        });
                        ui.label(
                            RichText::new("Input Settings Health Check")
                                .size(12.0)
                                .color(TEXT_MUTED),
                        );
                    });

                    // Action buttons on right
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let undo_count = undoable_set.len();
                        let undo_btn = egui::Button::new(
                            RichText::new(format!("Undo All ({undo_count})"))
                                .color(if undo_count > 0 {
                                    Color32::from_rgb(252, 165, 165)
                                } else {
                                    TEXT_MUTED
                                })
                                .strong(),
                        )
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::new(
                            1.0_f32,
                            if undo_count > 0 {
                                Color32::from_rgba_unmultiplied(239, 68, 68, 130)
                            } else {
                                BORDER
                            },
                        ))
                        .corner_radius(CornerRadius::same(6));

                        if ui
                            .add_enabled(!self.busy && undo_count > 0, undo_btn)
                            .clicked()
                        {
                            trigger_undo_all = true;
                        }

                        let fix_safe_btn = egui::Button::new(
                            RichText::new(format!("Fix Safe Issues ({fixable_safe})"))
                                .color(Color32::from_rgb(4, 19, 12))
                                .strong(),
                        )
                        .fill(COLOR_ACCENT)
                        .corner_radius(CornerRadius::same(6));

                        if ui
                            .add_enabled(!self.busy && fixable_safe > 0, fix_safe_btn)
                            .clicked()
                        {
                            trigger_fix_all_safe = true;
                        }

                        let rescan_btn =
                            egui::Button::new(RichText::new("Rescan").color(TEXT_PRIMARY).strong())
                                .fill(SURFACE_ALT)
                                .stroke(Stroke::new(1.0_f32, BORDER))
                                .corner_radius(CornerRadius::same(6));

                        if ui.add_enabled(!self.busy, rescan_btn).clicked() {
                            trigger_rescan = true;
                        }
                    });
                });
            });

        // --- FOOTER ---
        egui::TopBottomPanel::bottom("footer")
            .frame(
                egui::Frame::NONE
                    .fill(SURFACE)
                    .inner_margin(Margin::symmetric(18, 8))
                    .stroke(Stroke::new(1.0_f32, BORDER)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("Snapshot: {}", self.snapshot_path))
                            .size(11.0)
                            .monospace()
                            .color(TEXT_MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("Close [X] minimizes to tray • No telemetry • GPL-3.0")
                                .size(11.0)
                                .color(TEXT_MUTED),
                        );
                    });
                });
            });

        // --- MAIN CONTENT ---
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(BG_DARK)
                    .inner_margin(Margin::symmetric(18, 14)),
            )
            .show(ctx, |ui| {
                // Regression alert banner
                if let Some(summary) = &self.summary {
                    if !summary.regressions.is_empty() {
                        banner_frame(ui, COLOR_WARN, |ui| {
                            let heading = if summary.windows_build_changed {
                                "Windows Update changed your input settings:"
                            } else {
                                "Regressions detected since your previous scan:"
                            };
                            ui.label(RichText::new(heading).strong().color(COLOR_WARN));
                            for reg in &summary.regressions {
                                ui.label(
                                    RichText::new(format!(
                                        "• {}: {} -> {} ({})",
                                        reg.name,
                                        reg.previous.as_str().to_uppercase(),
                                        reg.current.as_str().to_uppercase(),
                                        reg.current_value
                                    ))
                                    .size(12.5)
                                    .color(TEXT_PRIMARY),
                                );
                            }
                        });
                        ui.add_space(8.0);
                    }
                }

                // Transient status / error banners
                if let Some(msg) = self.status_message.clone() {
                    let mut dismiss = false;
                    banner_frame(ui, COLOR_OK, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&msg).color(Color32::from_rgb(134, 239, 172)));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Dismiss").clicked() {
                                        dismiss = true;
                                    }
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
                    banner_frame(ui, COLOR_BAD, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&err).color(Color32::from_rgb(254, 202, 202)));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Dismiss").clicked() {
                                        dismiss = true;
                                    }
                                },
                            );
                        });
                    });
                    if dismiss {
                        self.error_message = None;
                    }
                    ui.add_space(8.0);
                }

                // Filter tabs + summary counters
                let total_count = self.summary.as_ref().map_or(0, |s| s.checks.len());
                let issues_count = warn_count + bad_count;
                let undo_count = undoable_set.len();

                ui.horizontal(|ui| {
                    filter_tab(
                        ui,
                        &mut self.filter,
                        FilterMode::All,
                        &format!("All ({total_count})"),
                    );
                    filter_tab(
                        ui,
                        &mut self.filter,
                        FilterMode::Issues,
                        &format!("Issues ({issues_count})"),
                    );
                    filter_tab(ui, &mut self.filter, FilterMode::ReportOnly, "Report Only");
                    filter_tab(
                        ui,
                        &mut self.filter,
                        FilterMode::Undoable,
                        &format!("Undoable ({undo_count})"),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.busy {
                            ui.spinner();
                            ui.label(RichText::new(&self.busy_label).size(12.0).color(TEXT_MUTED));
                        } else {
                            pill(ui, &format!("{bad_count} Bad"), SURFACE, COLOR_BAD);
                            pill(ui, &format!("{warn_count} Warn"), SURFACE, COLOR_WARN);
                            pill(ui, &format!("{info_count} Info"), SURFACE, COLOR_INFO);
                            pill(ui, &format!("{ok_count} OK"), SURFACE, COLOR_OK);
                        }
                    });
                });

                ui.add_space(10.0);

                // Scrollable check list
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let Some(summary) = &self.summary else {
                            ui.centered_and_justified(|ui| {
                                ui.label(
                                    RichText::new("Scanning Windows input settings...")
                                        .color(TEXT_MUTED),
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
                                .fill(SURFACE)
                                .stroke(Stroke::new(1.0_f32, BORDER))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::same(28))
                                .show(ui, |ui| {
                                    ui.centered_and_justified(|ui| {
                                        ui.label(
                                            RichText::new("No checks match the selected filter.")
                                                .color(TEXT_MUTED),
                                        );
                                    });
                                });
                            return;
                        }

                        for check in visible {
                            let is_undoable = undoable_set.contains(&check.id);
                            render_check_card(
                                ui,
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

        // --- UAC CONFIRMATION MODAL ---
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
                        .fill(SURFACE)
                        .stroke(Stroke::new(1.5_f32, COLOR_ADMIN))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(18)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        pill(
                            ui,
                            "UAC",
                            Color32::from_rgba_unmultiplied(245, 158, 11, 35),
                            COLOR_WARN,
                        );
                        ui.label(
                            RichText::new(&pending.title)
                                .size(15.0)
                                .strong()
                                .color(TEXT_PRIMARY),
                        );
                    });
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(&pending.reason)
                            .size(13.0)
                            .color(TEXT_MUTED),
                    );
                    ui.add_space(10.0);

                    egui::Frame::NONE
                        .fill(SURFACE_ALT)
                        .stroke(Stroke::new(1.0_f32, BORDER))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(
                                    "Polled stays non-elevated and launches polled.exe only for this single action. Your original setting is saved to baseline_snapshot.json before any change.",
                                )
                                .size(12.0)
                                .color(TEXT_MUTED),
                            );
                        });

                    ui.add_space(14.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let confirm_btn = egui::Button::new(
                            RichText::new(&pending.action_label)
                                .color(Color32::WHITE)
                                .strong(),
                        )
                        .fill(COLOR_ADMIN)
                        .corner_radius(CornerRadius::same(6));

                        if ui.add_enabled(!self.busy, confirm_btn).clicked() {
                            confirm_elevation = true;
                        }

                        let cancel_btn =
                            egui::Button::new(RichText::new("Cancel").color(TEXT_PRIMARY))
                                .fill(SURFACE_ALT)
                                .stroke(Stroke::new(1.0_f32, BORDER))
                                .corner_radius(CornerRadius::same(6));
                        if ui.add_enabled(!self.busy, cancel_btn).clicked() {
                            cancel_elevation = true;
                        }
                    });
                });
        }

        // Execute any deferred user actions cleanly.
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

fn render_check_card(
    ui: &mut egui::Ui,
    check: &CheckReport,
    is_undoable: bool,
    busy: bool,
    clicked_fix: &mut Option<CheckReport>,
    clicked_undo: &mut Option<CheckReport>,
) {
    let sev_color = severity_color(check.severity);
    let sev_bg = severity_bg(check.severity);
    let show_fix = check.can_fix
        && (check.severity >= Severity::Warn
            || (check.id == "power_plan" && check.severity == Severity::Info));

    let frame = egui::Frame::NONE
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(13));

    let resp = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());

        // Header row
        ui.horizontal(|ui| {
            pill(
                ui,
                &check.severity.as_str().to_uppercase(),
                sev_bg,
                sev_color,
            );
            ui.label(
                RichText::new(&check.name)
                    .size(14.5)
                    .strong()
                    .color(TEXT_PRIMARY),
            );
            ui.label(
                RichText::new(&check.id)
                    .size(11.5)
                    .monospace()
                    .color(TEXT_MUTED),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if show_fix {
                    let (label, bg, fg) = if check.requires_elevation {
                        ("Fix (Admin)", COLOR_ADMIN, Color32::WHITE)
                    } else {
                        ("Fix", COLOR_ACCENT, Color32::from_rgb(4, 19, 12))
                    };
                    let btn = egui::Button::new(RichText::new(label).color(fg).strong())
                        .fill(bg)
                        .corner_radius(CornerRadius::same(6));
                    if ui.add_enabled(!busy, btn).clicked() {
                        *clicked_fix = Some(check.clone());
                    }
                }

                if is_undoable {
                    let undo_btn = egui::Button::new(RichText::new("Undo").color(TEXT_PRIMARY))
                        .fill(SURFACE_ALT)
                        .stroke(Stroke::new(1.0_f32, BORDER))
                        .corner_radius(CornerRadius::same(6));
                    if ui.add_enabled(!busy, undo_btn).clicked() {
                        *clicked_undo = Some(check.clone());
                    }
                }

                if check.requires_relogin && check.severity >= Severity::Warn {
                    pill(
                        ui,
                        "Sign-out required",
                        Color32::from_rgba_unmultiplied(245, 158, 11, 28),
                        COLOR_WARN,
                    );
                }

                if !check.can_fix {
                    pill(ui, "Report only", SURFACE_ALT, TEXT_MUTED);
                }
            });
        });

        ui.add_space(8.0);

        // Current vs Recommended comparison boxes
        ui.columns(2, |cols| {
            value_box(&mut cols[0], "CURRENT", &check.current_value, TEXT_PRIMARY);
            value_box(
                &mut cols[1],
                "RECOMMENDED",
                &check.recommended_value,
                Color32::from_rgb(203, 213, 225),
            );
        });

        ui.add_space(7.0);
        ui.label(RichText::new(&check.reason).size(12.2).color(TEXT_MUTED));
    });

    // Crisp left severity stripe inside the card edge
    let rect = resp.response.rect;
    let stripe = egui::Rect::from_min_max(
        rect.left_top() + egui::vec2(1.0, 2.0),
        egui::pos2(rect.left() + 4.5, rect.bottom() - 2.0),
    );
    ui.painter()
        .rect_filled(stripe, CornerRadius::same(2), sev_color);
}

fn value_box(ui: &mut egui::Ui, label: &str, value: &str, text_color: Color32) {
    egui::Frame::NONE
        .fill(SURFACE_ALT)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(label).size(10.0).strong().color(TEXT_MUTED));
            ui.label(RichText::new(value).size(12.5).color(text_color));
        });
}

fn banner_frame(ui: &mut egui::Ui, accent: Color32, add_contents: impl FnOnce(&mut egui::Ui)) {
    let fill = Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 28);
    let border = Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 110);
    egui::Frame::NONE
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui);
        });
}

fn filter_tab(ui: &mut egui::Ui, current: &mut FilterMode, target: FilterMode, label: &str) {
    let selected = *current == target;
    let btn = egui::Button::new(
        RichText::new(label)
            .size(12.5)
            .color(if selected { TEXT_PRIMARY } else { TEXT_MUTED })
            .strong(),
    )
    .fill(if selected { SURFACE_ALT } else { SURFACE })
    .stroke(Stroke::new(
        1.0_f32,
        if selected {
            BORDER
        } else {
            Color32::TRANSPARENT
        },
    ))
    .corner_radius(CornerRadius::same(6));

    if ui.add(btn).clicked() {
        *current = target;
    }
}

fn pill(ui: &mut egui::Ui, text: &str, bg: Color32, fg: Color32) {
    egui::Frame::NONE
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).strong().color(fg));
        });
}

fn severity_color(severity: Severity) -> Color32 {
    match severity {
        Severity::Ok => COLOR_OK,
        Severity::Info => COLOR_INFO,
        Severity::Warn => COLOR_WARN,
        Severity::Bad => COLOR_BAD,
    }
}

fn severity_bg(severity: Severity) -> Color32 {
    let c = severity_color(severity);
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 36)
}

fn apply_dark_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG_DARK;
    visuals.window_fill = SURFACE;
    visuals.override_text_color = Some(TEXT_PRIMARY);
    visuals.widgets.noninteractive.bg_fill = SURFACE;
    visuals.widgets.inactive.bg_fill = SURFACE_ALT;
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(36, 46, 61);
    visuals.widgets.active.bg_fill = Color32::from_rgb(44, 56, 74);
    ctx.set_visuals(visuals);
}
