// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use chrono::Local;
use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Vec2};
use polled_core::win::{RegHive, WindowsEnv};
use polled_core::{
    check_latest_github_release, curated_custom_tweak_presets, export_setup_bundle_json,
    is_autostart_enabled, parse_setup_bundle_json, restore_setup_bundle, set_autostart_enabled,
    AppPaths, AppSettings, CheckReport, CustomTweakDef, CustomTweakStore, CustomValueKind,
    HealthEngine, HotkeyBinding, MouseSettings, PolledError, ScanSummary, Severity, SupportTier,
    ThemePreference, UpdateCheckResult, GITHUB_RELEASES_WEB_URL,
};

use crate::elevation::{run_elevated_cli, ElevatedAction};
use crate::hotkeys::{HotkeyAction, HotkeyManager};
use crate::input_lab_ui::{InputLabNavAction, InputLabUiState};
use crate::ipc::{DaemonToUiMsg, IpcClient, UiToDaemonMsg};
use crate::keyboard_ui::KeyboardUiState;
use crate::notifications::{notify_if_regressed, spawn_windows_build_watcher};
use crate::osd::OsdController;
use crate::profile_runtime::{ProfileRuntimeCmd, ProfileRuntimeHandle, ProfileRuntimeSnapshot};
use crate::profiles_ui::ProfilesUiState;
use crate::single_instance::SingleInstanceGuard;
use crate::theme::{Palette, ThemeMode};
use crate::tray::{find_own_main_hwnd, TrayAction, TrayController};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveView {
    HealthCheck,
    Profiles,
    Keyboard,
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

/// Messages sent from background worker/tray/IPC threads back to the `egui` UI thread.
pub(crate) enum WorkerMsg {
    TraySyncAutoSwitch(bool),
    TrayAction(TrayAction),
    HotkeyTriggered(HotkeyAction),
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
    ReadFromMouseFinished {
        device_name: Option<String>,
        settings: Option<MouseSettings>,
        error: Option<String>,
    },
    DisplayTestStarted {
        device_name: String,
        applied_label: String,
        previous_label: String,
        error: Option<String>,
    },
    DisplayTestReverted(String),
    ShowOsdPill {
        title: String,
        subtitle: String,
    },
    WootingActionResult {
        ok: bool,
        message: String,
    },
    UpdateCheckFinished(Result<UpdateCheckResult, String>),
    DaemonCachedScan(ScanSummary),
    DaemonFocusWindow,
    DaemonShutdownUi,
}

struct StandaloneHost {
    hotkey_manager: HotkeyManager,
    osd: OsdController,
    tray: TrayController,
    _instance_guard: SingleInstanceGuard,
}

enum AppHost {
    Remote { ipc: IpcClient },
    Standalone(Box<StandaloneHost>),
}

pub struct PolledApp {
    settings: AppSettings,
    autostart_enabled: bool,
    theme: ThemeMode,
    active_view: ActiveView,
    summary: Option<ScanSummary>,
    snapshot_path: String,
    filter: FilterMode,
    health_search: String,
    expanded_checks: HashSet<String>,
    busy: bool,
    busy_label: String,
    status_message: Option<String>,
    error_message: Option<String>,
    pending_elevation: Option<PendingElevation>,
    pending_confirm: Option<PendingConfirm>,
    show_latency_tips: bool,
    hotkeys_modal_open: bool,
    setup_backup_export_json: Option<String>,
    setup_backup_import_open: bool,
    setup_backup_import_text: String,
    setup_backup_error: Option<String>,
    update_checking: bool,
    update_result: Option<Result<UpdateCheckResult, String>>,
    custom_tweak_draft: CustomTweakDraft,

    profiles_ui: ProfilesUiState,
    profile_runtime: ProfileRuntimeHandle,
    keyboard_ui: KeyboardUiState,
    input_lab_ui: InputLabUiState,
    host: AppHost,

    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
    main_hwnd: Arc<AtomicIsize>,
    auto_switch_state: Arc<AtomicBool>,
    profile_locked_state: Arc<AtomicBool>,
    quit_requested: Arc<AtomicBool>,
}

impl PolledApp {
    /// Starts an on-demand GUI window (`polled-gui.exe --ui --ipc-pid <pid>`) attached to the
    /// pure-Win32 background tray daemon over local Named Pipes.
    pub fn new_remote(
        cc: &eframe::CreationContext<'_>,
        daemon_pid: u32,
        open_bench: bool,
    ) -> Result<Self, String> {
        let paths = AppPaths::default_user_dir();
        let settings = AppSettings::load_or_default(&paths.settings_path());
        let theme = ThemeMode::from(settings.theme);
        theme.apply_to(&cc.egui_ctx);

        let autostart_enabled = is_autostart_enabled(&WindowsEnv).unwrap_or(false);

        let (tx, rx) = mpsc::channel();
        let main_hwnd = Arc::new(AtomicIsize::new(find_own_main_hwnd()));
        let auto_switch_state = Arc::new(AtomicBool::new(settings.auto_switch_on_startup));
        let profile_locked_state = Arc::new(AtomicBool::new(false));
        let quit_requested = Arc::new(AtomicBool::new(false));

        let tx_in = tx.clone();
        let ctx_in = cc.egui_ctx.clone();
        let tx_disc = tx.clone();
        let ctx_disc = cc.egui_ctx.clone();

        let ipc = IpcClient::connect(
            daemon_pid,
            move |msg| {
                let worker_msg = match msg {
                    DaemonToUiMsg::RuntimeSnapshot(snap) => WorkerMsg::ProfileRuntimeUpdated(snap),
                    DaemonToUiMsg::CachedScanSummary(scan) => WorkerMsg::DaemonCachedScan(scan),
                    DaemonToUiMsg::TraySyncAutoSwitch(enabled) => {
                        WorkerMsg::TraySyncAutoSwitch(enabled)
                    }
                    DaemonToUiMsg::ReadFromMouseFinished {
                        device_name,
                        settings,
                        error,
                    } => WorkerMsg::ReadFromMouseFinished {
                        device_name,
                        settings,
                        error,
                    },
                    DaemonToUiMsg::DisplayTestStarted {
                        device_name,
                        applied_label,
                        previous_label,
                        error,
                    } => WorkerMsg::DisplayTestStarted {
                        device_name,
                        applied_label,
                        previous_label,
                        error,
                    },
                    DaemonToUiMsg::DisplayTestReverted(msg) => WorkerMsg::DisplayTestReverted(msg),
                    DaemonToUiMsg::WootingActionResult { ok, message } => {
                        WorkerMsg::WootingActionResult { ok, message }
                    }
                    DaemonToUiMsg::ScanFinished {
                        result,
                        notify_regressions,
                    } => WorkerMsg::ScanFinished {
                        result,
                        notify_regressions,
                    },
                    DaemonToUiMsg::FocusWindow => WorkerMsg::DaemonFocusWindow,
                    DaemonToUiMsg::ToggleInputLab => {
                        WorkerMsg::HotkeyTriggered(HotkeyAction::ToggleInputLab)
                    }
                    DaemonToUiMsg::ShutdownUi => WorkerMsg::DaemonShutdownUi,
                };
                let _ = tx_in.send(worker_msg);
                ctx_in.request_repaint();
            },
            move || {
                let _ = tx_disc.send(WorkerMsg::DaemonShutdownUi);
                ctx_disc.request_repaint();
            },
        )?;

        let ipc_tx = ipc
            .sender()
            .ok_or_else(|| "IPC client closed immediately after connecting".to_string())?;
        let profile_runtime = ProfileRuntimeHandle::from_remote_callback(move |cmd| {
            let _ = ipc_tx.send(UiToDaemonMsg::RuntimeCmd(cmd));
        });

        let profiles_ui = ProfilesUiState::load();
        let mut input_lab_ui = InputLabUiState::new();
        let active_view = if open_bench {
            input_lab_ui.start_capture(&cc.egui_ctx);
            ActiveView::InputLab
        } else {
            ActiveView::HealthCheck
        };

        let snapshot_path = paths.baseline_snapshot_path().display().to_string();

        let mut app = Self {
            settings,
            autostart_enabled,
            theme,
            active_view,
            summary: None,
            snapshot_path,
            filter: FilterMode::All,
            health_search: String::new(),
            expanded_checks: HashSet::new(),
            busy: false,
            busy_label: String::new(),
            status_message: None,
            error_message: None,
            pending_elevation: None,
            pending_confirm: None,
            show_latency_tips: false,
            hotkeys_modal_open: false,
            setup_backup_export_json: None,
            setup_backup_import_open: false,
            setup_backup_import_text: String::new(),
            setup_backup_error: None,
            update_checking: false,
            update_result: None,
            custom_tweak_draft: CustomTweakDraft::default(),
            profiles_ui,
            profile_runtime,
            keyboard_ui: KeyboardUiState::new(),
            input_lab_ui,
            host: AppHost::Remote { ipc },
            tx,
            rx,
            main_hwnd,
            auto_switch_state,
            profile_locked_state,
            quit_requested,
        };

        app.sync_theme_to_host();
        app.start_scan(&cc.egui_ctx, false);
        Ok(app)
    }

    /// Starts the single-process fallback (`--standalone` or non-Windows).
    pub fn new_standalone(
        cc: &eframe::CreationContext<'_>,
        mut instance_guard: SingleInstanceGuard,
    ) -> Self {
        let paths = AppPaths::default_user_dir();
        let settings = AppSettings::load_or_default(&paths.settings_path());
        let theme = ThemeMode::from(settings.theme);
        theme.apply_to(&cc.egui_ctx);

        let autostart_enabled = is_autostart_enabled(&WindowsEnv).unwrap_or(false);

        let (tx, rx) = mpsc::channel();
        let main_hwnd = Arc::new(AtomicIsize::new(find_own_main_hwnd()));
        let auto_switch_state = Arc::new(AtomicBool::new(settings.auto_switch_on_startup));
        let profile_locked_state = Arc::new(AtomicBool::new(false));
        let quit_requested = Arc::new(AtomicBool::new(false));

        let ctx_wake = cc.egui_ctx.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            ctx_wake.request_repaint();
        });

        let profiles_ui = ProfilesUiState::load();
        let profile_runtime = ProfileRuntimeHandle::start(
            profiles_ui.store.clone(),
            settings.auto_switch_on_startup,
            settings.alerts.clone(),
            settings.audio_cycle_device_ids.clone(),
            tx.clone(),
            Arc::clone(&wake),
        );
        let hotkey_manager =
            HotkeyManager::start(settings.hotkeys.clone(), tx.clone(), Arc::clone(&wake));

        let show_hwnd = Arc::clone(&main_hwnd);
        let show_ctx = cc.egui_ctx.clone();
        instance_guard.start_show_listener(Arc::new(move || {
            crate::tray::show_and_focus(&show_hwnd, &show_ctx);
        }));

        let tx_tray = tx.clone();
        let wake_tray = Arc::clone(&wake);
        let tray_hwnd = Arc::clone(&main_hwnd);
        let tray_ctx = cc.egui_ctx.clone();
        let tray_quit = Arc::clone(&quit_requested);
        let tray = TrayController::new(
            settings.auto_switch_on_startup,
            Arc::new(move |action| {
                match &action {
                    TrayAction::Open => {
                        crate::tray::show_and_focus(&tray_hwnd, &tray_ctx);
                        return;
                    }
                    TrayAction::Quit => {
                        tray_quit.store(true, Ordering::SeqCst);
                        crate::tray::request_native_close(&tray_hwnd);
                        tray_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        tray_ctx.request_repaint();
                        return;
                    }
                    _ => {}
                }
                let _ = tx_tray.send(WorkerMsg::TrayAction(action));
                wake_tray();
            }),
        );
        spawn_windows_build_watcher(tx.clone(), Arc::clone(&wake));

        let snapshot_path = paths.baseline_snapshot_path().display().to_string();

        let mut app = Self {
            settings,
            autostart_enabled,
            theme,
            active_view: ActiveView::HealthCheck,
            summary: None,
            snapshot_path,
            filter: FilterMode::All,
            health_search: String::new(),
            expanded_checks: HashSet::new(),
            busy: false,
            busy_label: String::new(),
            status_message: None,
            error_message: None,
            pending_elevation: None,
            pending_confirm: None,
            show_latency_tips: false,
            hotkeys_modal_open: false,
            setup_backup_export_json: None,
            setup_backup_import_open: false,
            setup_backup_import_text: String::new(),
            setup_backup_error: None,
            update_checking: false,
            update_result: None,
            custom_tweak_draft: CustomTweakDraft::default(),
            profiles_ui,
            profile_runtime,
            keyboard_ui: KeyboardUiState::new(),
            input_lab_ui: InputLabUiState::new(),
            host: AppHost::Standalone(Box::new(StandaloneHost {
                hotkey_manager,
                osd: OsdController::start(),
                tray,
                _instance_guard: instance_guard,
            })),
            tx,
            rx,
            main_hwnd,
            auto_switch_state,
            profile_locked_state,
            quit_requested,
        };

        app.start_scan(&cc.egui_ctx, true);
        app
    }

    fn is_remote_daemon_mode(&self) -> bool {
        matches!(self.host, AppHost::Remote { .. })
    }

    fn update_hotkeys(&self, cfg: polled_core::HotkeyConfig) {
        match &self.host {
            AppHost::Remote { ipc } => ipc.send(UiToDaemonMsg::UpdateHotkeys(cfg)),
            AppHost::Standalone(host) => host.hotkey_manager.update_config(cfg),
        }
    }

    fn show_osd_pill(&self, title: impl Into<String>, subtitle: impl Into<String>) {
        match &self.host {
            AppHost::Remote { ipc } => ipc.send(UiToDaemonMsg::ShowOsdPill {
                title: title.into(),
                subtitle: subtitle.into(),
            }),
            AppHost::Standalone(host) => {
                host.osd
                    .show(title, subtitle, matches!(self.theme, ThemeMode::Dark));
            }
        }
    }

    fn sync_theme_to_host(&self) {
        if let AppHost::Remote { ipc } = &self.host {
            ipc.send(UiToDaemonMsg::UpdateThemeDark(matches!(
                self.theme,
                ThemeMode::Dark
            )));
        }
    }

    fn sync_scan_to_host(&self, summary: &ScanSummary) {
        if let AppHost::Remote { ipc } = &self.host {
            ipc.send(UiToDaemonMsg::SyncScanSummary(summary.clone()));
        }
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
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::TraySyncAutoSwitch(enabled) => {
                    self.settings.auto_switch_on_startup = enabled;
                }
                WorkerMsg::TrayAction(action) => match action {
                    TrayAction::Open => crate::tray::show_and_focus(&self.main_hwnd, ctx),
                    TrayAction::ScanNow => {
                        let tx_scan = self.tx.clone();
                        let ctx_scan = ctx.clone();
                        std::thread::spawn(move || {
                            let paths = AppPaths::default_user_dir();
                            let engine = HealthEngine::with_default_and_custom_checks(&paths);
                            let result = engine
                                .scan_all(&WindowsEnv, &paths, true)
                                .map_err(|e| e.to_string());
                            if let Ok(summary) = &result {
                                notify_if_regressed(summary);
                            }
                            let _ = tx_scan.send(WorkerMsg::ScanFinished {
                                result,
                                notify_regressions: false,
                            });
                            ctx_scan.request_repaint();
                        });
                    }
                    TrayAction::ApplyProfile(id) => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::ApplyProfileNow(id));
                    }
                    TrayAction::QuickSetMousePolling(hz) => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::QuickSetMousePolling(hz));
                    }
                    TrayAction::QuickSetMouseDpi(dpi) => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::QuickSetMouseDpi(dpi));
                    }
                    TrayAction::RefreshMouseAndBattery => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::RefreshMouseAndBattery);
                    }
                    TrayAction::SwitchKeyboardProfile(slot) => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::SwitchWootingProfile {
                                device_id: None,
                                slot,
                            });
                    }
                    TrayAction::RefreshKeyboards => {
                        self.profile_runtime.send(ProfileRuntimeCmd::RefreshWooting);
                    }
                    TrayAction::SwitchAudioOutput(audio_id) => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::SwitchAudioOutput(audio_id));
                    }
                    TrayAction::ToggleMicMute => {
                        self.profile_runtime.send(ProfileRuntimeCmd::ToggleMicMute);
                    }
                    TrayAction::ToggleProfileLock => {
                        let next = !self.profile_locked_state.load(Ordering::SeqCst);
                        self.profile_locked_state.store(next, Ordering::SeqCst);
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::SetProfileLock(next));
                    }
                    TrayAction::ToggleAutoSwitch => {
                        let next = !self.auto_switch_state.load(Ordering::SeqCst);
                        self.auto_switch_state.store(next, Ordering::SeqCst);
                        self.settings.auto_switch_on_startup = next;
                        let _ = self
                            .settings
                            .save_atomic(&AppPaths::default_user_dir().settings_path());
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::SetAutoSwitch(next));
                    }
                    TrayAction::Quit => {
                        self.quit_requested.store(true, Ordering::SeqCst);
                        crate::tray::request_native_close(&self.main_hwnd);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                },
                WorkerMsg::HotkeyTriggered(action) => match action {
                    HotkeyAction::CycleProfile => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::CycleNextProfile);
                    }
                    HotkeyAction::ToggleProfileLock => {
                        let next = !self.profile_locked_state.load(Ordering::SeqCst);
                        self.profile_locked_state.store(next, Ordering::SeqCst);
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::SetProfileLock(next));
                    }
                    HotkeyAction::ToggleInputLab => {
                        if self.input_lab_ui.is_capturing() {
                            self.input_lab_ui.stop_capture();
                        } else {
                            self.active_view = ActiveView::InputLab;
                            self.input_lab_ui.start_capture(ctx);
                        }
                    }
                    HotkeyAction::CycleAudioOutput => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::CycleAudioOutput);
                    }
                    HotkeyAction::CycleWootingProfile => {
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::CycleWootingProfile);
                    }
                    HotkeyAction::ToggleMicMute => {
                        self.profile_runtime.send(ProfileRuntimeCmd::ToggleMicMute);
                    }
                },
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
                            self.sync_scan_to_host(&summary);
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
                        self.sync_scan_to_host(&scan);
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
                    self.profile_locked_state
                        .store(snap.profile_locked, Ordering::SeqCst);
                    if self.settings.auto_switch_on_startup != snap.auto_switch_enabled {
                        self.settings.auto_switch_on_startup = snap.auto_switch_enabled;
                        if !self.is_remote_daemon_mode() {
                            let _ = self
                                .settings
                                .save_atomic(&AppPaths::default_user_dir().settings_path());
                        }
                    }
                    self.profiles_ui.runtime_snapshot = Some(*snap);
                    self.sync_tray_from_snapshot();
                }
                WorkerMsg::ReadFromMouseFinished {
                    device_name,
                    settings,
                    error,
                } => {
                    self.profiles_ui
                        .apply_read_from_mouse(device_name, settings, error);
                }
                WorkerMsg::DisplayTestStarted {
                    device_name,
                    applied_label,
                    previous_label,
                    error,
                } => {
                    self.profiles_ui.on_display_test_started(
                        device_name,
                        applied_label,
                        previous_label,
                        error,
                    );
                }
                WorkerMsg::DisplayTestReverted(msg) => {
                    self.profiles_ui.on_display_test_reverted(msg);
                }
                WorkerMsg::ShowOsdPill { title, subtitle } => {
                    self.show_osd_pill(title, subtitle);
                }
                WorkerMsg::WootingActionResult { ok, message } => {
                    self.keyboard_ui.on_action_result(ok, message);
                }
                WorkerMsg::UpdateCheckFinished(res) => {
                    self.update_checking = false;
                    self.update_result = Some(res);
                }
                WorkerMsg::DaemonCachedScan(scan) => {
                    if self.summary.is_none() {
                        self.summary = Some(scan);
                    }
                }
                WorkerMsg::DaemonFocusWindow => {
                    crate::tray::show_and_focus(&self.main_hwnd, ctx);
                }
                WorkerMsg::DaemonShutdownUi => {
                    self.quit_requested.store(true, Ordering::SeqCst);
                    self.input_lab_ui.stop_background_monitors();
                    self.keyboard_ui.stop_analog_stream();
                    crate::tray::request_native_close(&self.main_hwnd);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn sync_tray_from_snapshot(&mut self) {
        let Some(snap) = self.profiles_ui.runtime_snapshot.as_ref() else {
            return;
        };
        if let AppHost::Standalone(host) = &mut self.host {
            host.tray.sync_from_snapshot(
                snap,
                self.settings.alerts.battery_tray_icon,
                self.settings.alerts.clamped_threshold(),
            );
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

        // In daemon-connected mode (`--ui`), closing `[X]` exits the UI child process cleanly
        // so Windows immediately reclaims 100% of OpenGL/GPU memory while the ~5 MB pure-Win32
        // tray daemon keeps running. In `--standalone` mode, `[X]` hides the window to tray.
        if ctx.input(|i| i.viewport().close_requested()) {
            self.input_lab_ui.stop_background_monitors();
            self.keyboard_ui.stop_analog_stream();
            if !self.is_remote_daemon_mode() && !self.quit_requested.load(Ordering::SeqCst) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
        }

        self.drain_worker_messages(ctx);
        self.input_lab_ui.tick_timers(ctx);

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

        let issues_count = warn_count + bad_count;
        let undo_count = undoable_set.len();
        let screen_w = ctx.screen_rect().width();
        let compact_shell = screen_w < 780.0;

        let nav_mouse_line = self
            .profiles_ui
            .runtime_snapshot
            .as_ref()
            .and_then(|s| {
                s.active_devices
                    .iter()
                    .find(|d| d.info.support_tier != SupportTier::Simulator)
                    .or_else(|| s.active_devices.first())
            })
            .map(|d| {
                let batt = d.battery.map(|b| b.compact_label());
                (d.info.name.clone(), batt)
            });
        let nav_profile_name = self.profiles_ui.runtime_snapshot.as_ref().map(|s| {
            (
                s.active_profile_name.clone(),
                s.auto_switch_enabled,
                s.profile_locked,
            )
        });

        // --- SLEEK LEFT NAVIGATION RAIL (collapses to compact top strip only below 780px) ---
        if compact_shell {
            egui::TopBottomPanel::top("compact_nav")
                .frame(
                    egui::Frame::NONE
                        .fill(p.surface)
                        .inner_margin(Margin::symmetric(14, 10))
                        .stroke(Stroke::new(1.0_f32, p.border_subtle)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Polled").size(16.0).strong().color(p.text));
                        ui.add_space(6.0);
                        for (view, label) in [
                            (ActiveView::HealthCheck, "Health"),
                            (ActiveView::Profiles, "Profiles"),
                            (ActiveView::Keyboard, "Keyboard"),
                            (ActiveView::InputLab, "Bench"),
                        ] {
                            let active = self.active_view == view;
                            let btn = if active {
                                primary_button(label, p)
                            } else {
                                ghost_button(label, p)
                            };
                            if ui.add(btn).clicked() {
                                self.active_view = view;
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(ghost_button(self.theme.label(), p)).clicked() {
                                toggle_theme = true;
                            }
                            if ui.add(ghost_button("Settings", p)).clicked() {
                                self.hotkeys_modal_open = true;
                            }
                        });
                    });
                });
        } else {
            egui::SidePanel::left("nav_rail")
                .resizable(false)
                .exact_width(196.0)
                .frame(
                    egui::Frame::NONE
                        .fill(p.surface)
                        .inner_margin(Margin::symmetric(14, 16))
                        .stroke(Stroke::new(1.0_f32, p.border_subtle)),
                )
                .show(ctx, |ui| {
                    // Brand header
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Polled").size(18.0).strong().color(p.text));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                                    .size(11.0)
                                    .monospace()
                                    .color(p.text_muted),
                            );
                        });
                    });
                    if let Some(s) = &self.summary {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!("Win {}", s.windows_version))
                                        .size(11.0)
                                        .color(p.text_muted),
                                )
                                .truncate(),
                            );
                            if s.windows_build_changed {
                                ui.label(RichText::new("· Updated").size(11.0).color(p.warn));
                            }
                        });
                    }

                    ui.add_space(18.0);

                    // Primary navigation
                    let health_badge = if issues_count > 0 {
                        Some((
                            issues_count.to_string(),
                            if bad_count > 0 { p.bad } else { p.warn },
                        ))
                    } else if self.summary.is_some() {
                        Some(("OK".to_string(), p.ok))
                    } else {
                        None
                    };
                    if nav_rail_item(
                        ui,
                        p,
                        "Health",
                        self.active_view == ActiveView::HealthCheck,
                        health_badge.as_ref().map(|(t, c)| (t.as_str(), *c)),
                    ) {
                        self.active_view = ActiveView::HealthCheck;
                    }
                    ui.add_space(2.0);

                    let prof_badge = nav_profile_name.as_ref().and_then(|(_, auto, locked)| {
                        if *locked {
                            Some(("LOCK", p.warn))
                        } else if *auto {
                            Some(("AUTO", p.ok))
                        } else {
                            None
                        }
                    });
                    if nav_rail_item(
                        ui,
                        p,
                        "Profiles",
                        self.active_view == ActiveView::Profiles,
                        prof_badge,
                    ) {
                        self.active_view = ActiveView::Profiles;
                    }
                    ui.add_space(2.0);

                    let kb_badge_owned = if self.keyboard_ui.is_streaming_analog() {
                        Some(("LIVE".to_string(), p.ok))
                    } else {
                        self.profiles_ui
                            .runtime_snapshot
                            .as_ref()
                            .and_then(|s| s.keyboards.first())
                            .and_then(|kb| kb.sidebar_badge())
                            .map(|txt| (txt, p.info))
                    };
                    if nav_rail_item(
                        ui,
                        p,
                        "Keyboard",
                        self.active_view == ActiveView::Keyboard,
                        kb_badge_owned.as_ref().map(|(t, c)| (t.as_str(), *c)),
                    ) {
                        self.active_view = ActiveView::Keyboard;
                    }
                    ui.add_space(2.0);

                    let bench_badge = if self.input_lab_ui.is_capturing() {
                        Some(("LIVE", p.ok))
                    } else {
                        None
                    };
                    if nav_rail_item(
                        ui,
                        p,
                        "Bench",
                        self.active_view == ActiveView::InputLab,
                        bench_badge,
                    ) {
                        self.active_view = ActiveView::InputLab;
                    }

                    ui.add_space(18.0);
                    ui.separator();
                    ui.add_space(10.0);

                    // Quiet live rig status
                    ui.label(
                        RichText::new("RIG STATUS")
                            .size(9.5)
                            .strong()
                            .color(p.text_muted),
                    );
                    ui.add_space(6.0);
                    if let Some((mouse_name, batt)) = &nav_mouse_line {
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if let Some(b) = batt {
                                        ui.label(
                                            RichText::new(b).size(11.0).monospace().color(p.text),
                                        );
                                    }
                                    ui.with_layout(
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(mouse_name)
                                                        .size(11.5)
                                                        .color(p.text_muted),
                                                )
                                                .truncate(),
                                            );
                                        },
                                    );
                                },
                            );
                        });
                    }
                    if let Some((prof_name, _, _)) = &nav_profile_name {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("Profile: {prof_name}"))
                                    .size(11.5)
                                    .color(p.text_muted),
                            )
                            .truncate(),
                        );
                    }
                    if let Some(kb) = self
                        .profiles_ui
                        .runtime_snapshot
                        .as_ref()
                        .and_then(|s| s.keyboards.first())
                    {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "{} · {}",
                                    kb.name,
                                    kb.compact_status_summary()
                                ))
                                .size(11.0)
                                .color(p.text_muted),
                            )
                            .truncate(),
                        );
                    }

                    // Bottom utility stack
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                        let footer_lbl = if self.is_remote_daemon_mode() {
                            "Tray daemon active · AGPL-3.0"
                        } else {
                            "Tray on close · AGPL-3.0"
                        };
                        let footer = ui.label(
                            RichText::new(footer_lbl).size(10.0).color(p.text_muted),
                        );
                        if self.is_remote_daemon_mode() {
                            footer.on_hover_text(
                                "Closing this window frees 100% of GPU memory while Polled's ultra-light tray daemon stays active. Right-click the tray icon -> Quit to exit completely.",
                            );
                        }
                        ui.add_space(4.0);
                        if nav_utility_item(ui, p, self.theme.label(), None) {
                            toggle_theme = true;
                        }
                        let settings_badge =
                            self.settings.hotkeys.enabled.then_some(("HK", p.info));
                        if nav_utility_item(ui, p, "Settings", settings_badge) {
                            self.hotkeys_modal_open = true;
                        }
                        let startup_badge = if self.autostart_enabled {
                            Some(("ON", p.ok))
                        } else {
                            Some(("OFF", p.text_muted))
                        };
                        if nav_utility_item(ui, p, "Startup", startup_badge) {
                            toggle_autostart = true;
                        }
                        ui.add_space(6.0);
                        ui.separator();
                    });
                });
        }

        // --- MAIN CONTENT WORKSPACE ---
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(24, 18)),
            )
            .show(ctx, |ui| {
                let content_w = ui.available_width().max(320.0);
                ui.set_max_width(content_w);

                if self.active_view == ActiveView::Profiles {
                    self.profiles_ui.render(
                        ctx,
                        ui,
                        p,
                        mode,
                        &self.profile_runtime,
                        self.settings.alerts.clamped_threshold(),
                    );
                    return;
                }
                if self.active_view == ActiveView::Keyboard {
                    self.keyboard_ui.render(
                        ctx,
                        ui,
                        p,
                        mode,
                        self.profiles_ui.runtime_snapshot.as_ref(),
                        &self.profile_runtime,
                    );
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
                    let health_checks: &[CheckReport] = self
                        .summary
                        .as_ref()
                        .map(|s| s.checks.as_slice())
                        .unwrap_or(&[]);
                    let win_ver = self.summary.as_ref().map(|s| s.windows_version.as_str());
                    let primary_kb = self
                        .profiles_ui
                        .runtime_snapshot
                        .as_ref()
                        .and_then(|s| s.keyboards.first());
                    if let Some(nav) = self.input_lab_ui.render(
                        ctx,
                        ui,
                        p,
                        mode,
                        detected_dpi,
                        health_checks,
                        win_ver,
                        primary_kb,
                    ) {
                        match nav {
                            InputLabNavAction::OpenHealthCheck => {
                                self.active_view = ActiveView::HealthCheck;
                                self.filter = FilterMode::Issues;
                            }
                            InputLabNavAction::OpenProfiles => {
                                self.active_view = ActiveView::Profiles;
                            }
                        }
                    }
                    return;
                }

                // --- HEALTH PAGE HEADER (Single calm row) ---
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Health").size(20.0).strong().color(p.text));
                    ui.add_space(10.0);
                    if self.busy {
                        ui.spinner();
                        ui.label(
                            RichText::new(&self.busy_label)
                                .size(12.0)
                                .color(p.text_muted),
                        );
                    } else if self.summary.is_some() {
                        status_dot_count(ui, p.ok, &format!("{ok_count} ok"), p.text_muted);
                        status_dot_count(ui, p.info, &format!("{info_count} info"), p.text_muted);
                        if warn_count > 0 {
                            status_dot_count(ui, p.warn, &format!("{warn_count} warn"), p.warn);
                        }
                        if bad_count > 0 {
                            status_dot_count(ui, p.bad, &format!("{bad_count} bad"), p.bad);
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button(
                            RichText::new("More ▾").size(12.5).color(p.text_muted),
                            |ui| {
                                ui.set_min_width(180.0);
                                if ui
                                    .add_enabled(
                                        !self.busy && undo_count > 0,
                                        egui::Button::new(format!("Undo All ({undo_count})")),
                                    )
                                    .clicked()
                                {
                                    trigger_undo_all = true;
                                    ui.close_menu();
                                }
                                if ui.button("+ Custom Reversible Tweak").clicked() {
                                    self.custom_tweak_draft = CustomTweakDraft {
                                        open: true,
                                        ..Default::default()
                                    };
                                    ui.close_menu();
                                }
                                let tips_lbl = if self.show_latency_tips {
                                    "Hide Latency Checklist"
                                } else {
                                    "Show Latency Checklist"
                                };
                                if ui.button(tips_lbl).clicked() {
                                    self.show_latency_tips = !self.show_latency_tips;
                                    ui.close_menu();
                                }
                                ui.separator();
                                if ui.button("Copy Snapshot Path").clicked() {
                                    ctx.copy_text(self.snapshot_path.clone());
                                    self.status_message = Some(format!(
                                        "Copied snapshot path: {}",
                                        self.snapshot_path
                                    ));
                                    ui.close_menu();
                                }
                            },
                        );

                        if fixable_safe > 0
                            && ui
                                .add_enabled(
                                    !self.busy,
                                    primary_button(&format!("Fix Safe ({fixable_safe})"), p),
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
                });

                ui.add_space(12.0);

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
                                        .size(12.0)
                                        .color(p.text),
                                    )
                                    .wrap(),
                                );
                            }
                        });
                        ui.add_space(10.0);
                    }
                }

                // Transient status / error banners
                if let Some(msg) = self.status_message.clone() {
                    let mut dismiss = false;
                    banner_frame(ui, p, content_w, None, mode, |ui| {
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.add(ghost_button("Dismiss", p)).clicked() {
                                        dismiss = true;
                                    }
                                    ui.with_layout(
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&msg).size(12.5).color(p.text),
                                                )
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
                    ui.add_space(10.0);
                }

                if let Some(err) = self.error_message.clone() {
                    let mut dismiss = false;
                    banner_frame(ui, p, content_w, Some(p.bad), mode, |ui| {
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.add(ghost_button("Dismiss", p)).clicked() {
                                        dismiss = true;
                                    }
                                    ui.with_layout(
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&err).size(12.5).color(p.bad),
                                                )
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
                    ui.add_space(10.0);
                }

                // Segmented filter strip + compact search
                let total_count = self.summary.as_ref().map_or(0, |s| s.checks.len());
                let tweaks_count = self.summary.as_ref().map_or(0, |s| {
                    s.checks.iter().filter(|c| c.is_optional_tweak).count()
                });

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
                        FilterMode::Tweaks,
                        &format!("Tweaks ({tweaks_count})"),
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
                        if !self.health_search.is_empty() && ui.add(ghost_button("×", p)).clicked()
                        {
                            self.health_search.clear();
                        }
                        ui.add(
                            egui::TextEdit::singleline(&mut self.health_search)
                                .desired_width(168.0)
                                .hint_text("Search checks..."),
                        );
                    });
                });

                if self.show_latency_tips {
                    ui.add_space(10.0);
                    render_latency_tips_card(ui, p, content_w);
                }

                ui.add_space(10.0);

                // --- UNIFIED CALM CHECKLIST CONTAINER ---
                let list_w = (content_w - 8.0).max(260.0);
                let query = self.health_search.trim().to_ascii_lowercase();
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
                            .filter(|c| {
                                query.is_empty()
                                    || c.name.to_ascii_lowercase().contains(&query)
                                    || c.id.to_ascii_lowercase().contains(&query)
                                    || c.current_value.to_ascii_lowercase().contains(&query)
                            })
                            .collect();

                        if visible.is_empty() {
                            egui::Frame::NONE
                                .fill(p.surface)
                                .stroke(Stroke::new(1.0_f32, p.border_subtle))
                                .corner_radius(CornerRadius::same(10))
                                .inner_margin(Margin::same(32))
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

                        egui::Frame::NONE
                            .fill(p.surface)
                            .stroke(Stroke::new(1.0_f32, p.border))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::symmetric(0, 4))
                            .show(ui, |ui| {
                                ui.set_min_width(list_w);
                                ui.set_max_width(list_w);
                                let total_vis = visible.len();
                                for (idx, check) in visible.into_iter().enumerate() {
                                    let is_undoable = undoable_set.contains(&check.id);
                                    let expanded = self.expanded_checks.contains(&check.id);
                                    let toggled = render_check_row(
                                        ui,
                                        p,
                                        mode,
                                        list_w,
                                        check,
                                        is_undoable,
                                        expanded,
                                        self.busy,
                                        &mut clicked_fix,
                                        &mut clicked_undo,
                                        &mut clicked_delete_custom,
                                    );
                                    if toggled {
                                        if expanded {
                                            self.expanded_checks.remove(&check.id);
                                        } else {
                                            self.expanded_checks.insert(check.id.clone());
                                        }
                                    }
                                    if idx + 1 < total_vis {
                                        let rect = ui.allocate_space(Vec2::new(list_w, 1.0)).1;
                                        ui.painter().hline(
                                            (rect.left() + 14.0)..=(rect.right() - 14.0),
                                            rect.center().y,
                                            Stroke::new(1.0_f32, p.border_subtle),
                                        );
                                    }
                                }
                            });
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
            self.sync_theme_to_host();
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
                        ActiveView::Keyboard => {
                            self.keyboard_ui.notice = Some((Severity::Ok, msg));
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
                    ActiveView::Keyboard => {
                        self.keyboard_ui.notice = Some((Severity::Bad, err));
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

        // --- SETTINGS MODAL (TRAY, ALERTS, GLOBAL HOTKEYS & SETUP BACKUP) ---
        if self.hotkeys_modal_open {
            let mut close_hk = false;
            let mut save_hk = false;
            let mut export_backup_clicked = false;
            let mut open_import_backup_clicked = false;
            let modal_w = (ctx.screen_rect().width() - 48.0).clamp(380.0, 520.0);
            egui::Window::new("Settings — Tray, Alerts, Hotkeys & Backup")
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
                    let modal_scroll_h = (ctx.screen_rect().height() - 160.0).clamp(260.0, 520.0);
                    egui::ScrollArea::vertical()
                        .id_salt("settings_modal_scroll")
                        .max_height(modal_scroll_h)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                    ui.label(
                        RichText::new("TRAY ICON & NOTIFICATIONS")
                            .size(10.5)
                            .strong()
                            .color(p.text_muted),
                    );
                    ui.add_space(4.0);
                    ui.checkbox(
                        &mut self.settings.alerts.battery_tray_icon,
                        "Show wireless mouse battery % on system tray icon",
                    );
                    ui.horizontal(|ui| {
                        ui.checkbox(
                            &mut self.settings.alerts.low_battery_toast,
                            "Low-battery Windows notification at",
                        );
                        ui.add_enabled(
                            self.settings.alerts.low_battery_toast
                                || self.settings.alerts.low_battery_polling_guard,
                            egui::DragValue::new(
                                &mut self.settings.alerts.low_battery_threshold_pct,
                            )
                            .range(5..=50)
                            .speed(1)
                            .suffix("%"),
                        );
                    });
                    ui.checkbox(
                        &mut self.settings.alerts.low_battery_polling_guard,
                        "Battery polling guard (step 2K/4K/8K mice -> 1000 Hz at threshold, restore on charge)",
                    );
                    ui.checkbox(
                        &mut self.settings.alerts.profile_switch_toast,
                        "Show notification when game/desktop profile switches",
                    );
                    ui.horizontal(|ui| {
                        ui.checkbox(
                            &mut self.settings.alerts.osd_pill_enabled,
                            "Show monochrome on-screen switch pill (OSD, zero game hooks)",
                        );
                        if ui.add(secondary_button("Test Pill", p)).clicked() {
                            self.show_osd_pill(
                                "Polled · Profile Switched",
                                "Counter-Strike 2 · 1600 DPI · 4000 Hz · Wooting P2",
                            );
                        }
                    });

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(8.0);

                    ui.label(
                        RichText::new("GLOBAL HOTKEYS (WIN32 REGISTERHOTKEY)")
                            .size(10.5)
                            .strong()
                            .color(p.text_muted),
                    );
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Uses official Windows RegisterHotKey (zero keyboard hooks, anti-cheat safe). Off by default.",
                            )
                            .size(11.5)
                            .color(p.text_muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(6.0);
                    ui.checkbox(
                        &mut self.settings.hotkeys.enabled,
                        "Enable Global Hotkeys",
                    );
                    ui.add_space(6.0);

                    ui.add_enabled_ui(self.settings.hotkeys.enabled, |ui| {
                        egui::Grid::new("global_hotkeys_grid")
                            .num_columns(2)
                            .spacing([12.0, 8.0])
                            .show(ui, |ui| {
                                render_hotkey_binding_row(
                                    ui,
                                    "Cycle Next Profile",
                                    "hk_cycle",
                                    &mut self.settings.hotkeys.cycle_profile,
                                    "P",
                                );
                                ui.end_row();
                                render_hotkey_binding_row(
                                    ui,
                                    "Lock / Unlock Profile",
                                    "hk_lock",
                                    &mut self.settings.hotkeys.toggle_lock,
                                    "L",
                                );
                                ui.end_row();
                                render_hotkey_binding_row(
                                    ui,
                                    "Start / Stop Bench",
                                    "hk_lab",
                                    &mut self.settings.hotkeys.toggle_input_lab,
                                    "I",
                                );
                                ui.end_row();
                                render_hotkey_binding_row(
                                    ui,
                                    "Cycle Audio Output",
                                    "hk_audio",
                                    &mut self.settings.hotkeys.cycle_audio_output,
                                    "A",
                                );
                                ui.end_row();
                                render_hotkey_binding_row(
                                    ui,
                                    "Cycle Keyboard Profile",
                                    "hk_wooting",
                                    &mut self.settings.hotkeys.cycle_wooting_profile,
                                    "K",
                                );
                                ui.end_row();
                                render_hotkey_binding_row(
                                    ui,
                                    "Toggle Mic Mute",
                                    "hk_mic_mute",
                                    &mut self.settings.hotkeys.toggle_mic_mute,
                                    "M",
                                );
                                ui.end_row();
                            });
                    });

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(8.0);

                    ui.label(
                        RichText::new("AUDIO OUTPUT CYCLE FILTER (HEADPHONES <-> SPEAKERS)")
                            .size(10.5)
                            .strong()
                            .color(p.text_muted),
                    );
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Pin 2+ playback devices below so Cycle Audio Output skips unused HDMI/monitor speakers. Leave all unchecked to cycle every active device.",
                            )
                            .size(11.5)
                            .color(p.text_muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(6.0);
                    let active_outputs: Vec<(String, String, bool)> = self
                        .profiles_ui
                        .runtime_snapshot
                        .as_ref()
                        .map(|s| s.audio_outputs.clone())
                        .unwrap_or_default();
                    if active_outputs.is_empty() {
                        ui.label(
                            RichText::new("No active playback devices detected yet.")
                                .size(11.5)
                                .color(p.text_muted),
                        );
                    } else {
                        for (dev_id, dev_name, is_def) in &active_outputs {
                            let mut pinned = self
                                .settings
                                .audio_cycle_device_ids
                                .iter()
                                .any(|id| id.eq_ignore_ascii_case(dev_id));
                            let lbl = if *is_def {
                                format!("{dev_name} (default)")
                            } else {
                                dev_name.clone()
                            };
                            if ui.checkbox(&mut pinned, lbl).changed() {
                                if pinned {
                                    if !self
                                        .settings
                                        .audio_cycle_device_ids
                                        .iter()
                                        .any(|id| id.eq_ignore_ascii_case(dev_id))
                                    {
                                        self.settings.audio_cycle_device_ids.push(dev_id.clone());
                                    }
                                } else {
                                    self.settings
                                        .audio_cycle_device_ids
                                        .retain(|id| !id.eq_ignore_ascii_case(dev_id));
                                }
                            }
                        }
                        let active_pinned = active_outputs
                            .iter()
                            .filter(|(id, _, _)| {
                                self.settings
                                    .audio_cycle_device_ids
                                    .iter()
                                    .any(|pin| pin.eq_ignore_ascii_case(id))
                            })
                            .count();
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            if self.settings.audio_cycle_device_ids.is_empty() {
                                ui.label(
                                    RichText::new("Cycling all active playback devices.")
                                        .size(11.0)
                                        .color(p.text_muted),
                                );
                            } else if active_pinned < 2 {
                                ui.label(
                                    RichText::new(format!(
                                        "Pin at least 2 active devices ({active_pinned} active pinned)."
                                    ))
                                    .size(11.0)
                                    .color(p.warn),
                                );
                            } else {
                                ui.label(
                                    RichText::new(format!(
                                        "Cycling {active_pinned} pinned devices (unpinned HDMI/monitors skipped)."
                                    ))
                                    .size(11.0)
                                    .color(p.ok),
                                );
                            }
                            if !self.settings.audio_cycle_device_ids.is_empty()
                                && ui.add(secondary_button("Clear Pins", p)).clicked()
                            {
                                self.settings.audio_cycle_device_ids.clear();
                            }
                        });
                    }

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(8.0);

                    ui.label(
                        RichText::new("SETUP BACKUP & RESTORE (POLLED.SETUP.V1)")
                            .size(10.5)
                            .strong()
                            .color(p.text_muted),
                    );
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Bundle all game profiles, custom tweaks, alerts, and hotkey settings into one portable JSON file for moving PCs or clean Windows installs.",
                            )
                            .size(11.5)
                            .color(p.text_muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui.add(secondary_button("Export Setup Backup", p)).clicked() {
                            export_backup_clicked = true;
                        }
                        if ui
                            .add(secondary_button("Restore from Backup...", p))
                            .clicked()
                        {
                            open_import_backup_clicked = true;
                        }
                    });

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(8.0);

                    ui.label(
                        RichText::new("UPDATES (STRICTLY ON-DEMAND)")
                            .size(10.5)
                            .strong()
                            .color(p.text_muted),
                    );
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!(
                                "Current version: v{}. Polled never phones home — update checks only query GitHub Releases when you click Check Now.",
                                env!("CARGO_PKG_VERSION")
                            ))
                            .size(11.5)
                            .color(p.text_muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        let check_lbl = if self.update_checking {
                            "Checking GitHub..."
                        } else {
                            "Check for Updates"
                        };
                        if ui
                            .add_enabled(!self.update_checking, secondary_button(check_lbl, p))
                            .clicked()
                        {
                            self.update_checking = true;
                            self.update_result = None;
                            let tx = self.tx.clone();
                            let ctx_bg = ctx.clone();
                            std::thread::spawn(move || {
                                let res =
                                    check_latest_github_release().map_err(|e| e.to_string());
                                let _ = tx.send(WorkerMsg::UpdateCheckFinished(res));
                                ctx_bg.request_repaint();
                            });
                        }
                        let open_url = match &self.update_result {
                            Some(Ok(info)) => info.release_url.clone(),
                            _ => GITHUB_RELEASES_WEB_URL.to_string(),
                        };
                        if ui.add(secondary_button("Open Releases Page", p)).clicked() {
                            let _ = std::process::Command::new("cmd")
                                .args(["/C", "start", "", &open_url])
                                .spawn();
                        }
                    });
                    if let Some(res) = &self.update_result {
                        ui.add_space(6.0);
                        match res {
                            Ok(info) if info.update_available => {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "New release available: v{} (current v{}).",
                                            info.latest_version, info.current_version
                                        ))
                                        .size(12.0)
                                        .strong()
                                        .color(p.info),
                                    )
                                    .wrap(),
                                );
                                if !info.notes_preview.is_empty() {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&info.notes_preview)
                                                .size(11.0)
                                                .color(p.text_muted),
                                        )
                                        .wrap(),
                                    );
                                }
                            }
                            Ok(info) => {
                                ui.label(
                                    RichText::new(format!(
                                        "Up to date — v{} is the latest GitHub release (latest: v{}).",
                                        info.current_version, info.latest_version
                                    ))
                                    .size(12.0)
                                    .color(p.ok),
                                );
                            }
                            Err(err) => {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!("Update check failed: {err}"))
                                            .size(11.5)
                                            .color(p.warn),
                                    )
                                    .wrap(),
                                );
                            }
                        }
                    }

                    });
                    ui.add_space(10.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(primary_button("Save & Apply", p)).clicked() {
                            save_hk = true;
                        }
                        if ui.add(secondary_button("Close", p)).clicked() {
                            close_hk = true;
                        }
                    });
                });

            if export_backup_clicked {
                let paths = AppPaths::default_user_dir();
                match export_setup_bundle_json(&paths) {
                    Ok(json) => {
                        self.setup_backup_export_json = Some(json);
                        self.hotkeys_modal_open = false;
                    }
                    Err(err) => {
                        self.error_message = Some(format!("Failed to export setup backup: {err}"));
                    }
                }
            } else if open_import_backup_clicked {
                self.setup_backup_import_open = true;
                self.setup_backup_import_text.clear();
                self.setup_backup_error = None;
                self.hotkeys_modal_open = false;
            } else if save_hk {
                self.settings.alerts.low_battery_threshold_pct =
                    self.settings.alerts.clamped_threshold();
                let _ = self
                    .settings
                    .save_atomic(&AppPaths::default_user_dir().settings_path());
                self.update_hotkeys(self.settings.hotkeys.clone());
                self.profile_runtime
                    .send(ProfileRuntimeCmd::UpdateAlertConfig(
                        self.settings.alerts.clone(),
                    ));
                self.profile_runtime
                    .send(ProfileRuntimeCmd::UpdateAudioCycleDevices(
                        self.settings.audio_cycle_device_ids.clone(),
                    ));
                self.sync_tray_from_snapshot();
                self.status_message =
                    Some("Saved tray, notification, and global hotkey settings.".to_string());
                self.hotkeys_modal_open = false;
            } else if close_hk {
                self.hotkeys_modal_open = false;
            }
        }

        // --- EXPORT FULL SETUP BACKUP MODAL ---
        let mut close_backup_export = false;
        if let Some(json) = self.setup_backup_export_json.clone() {
            let modal_w = (ctx.screen_rect().width() - 48.0).clamp(380.0, 560.0);
            egui::Window::new("Export Setup Backup (polled.setup.v1)")
                .collapsible(false)
                .resizable(true)
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
                                "Includes settings.json, profiles.json, and custom_tweaks.json (host-specific baseline snapshots are excluded).",
                            )
                            .size(12.0)
                            .color(p.text_muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(6.0);
                    let mut preview = json.clone();
                    egui::ScrollArea::vertical()
                        .id_salt("backup_export_scroll")
                        .max_height(260.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut preview)
                                    .code_editor()
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    ui.add_space(10.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(secondary_button("Close", p)).clicked() {
                            close_backup_export = true;
                        }
                        if ui.add(secondary_button("Save to Exports", p)).clicked() {
                            let dir = AppPaths::default_user_dir().exports_dir();
                            let filename = format!(
                                "polled_backup_{}.json",
                                Local::now().format("%Y%m%d_%H%M%S")
                            );
                            let path = dir.join(filename);
                            match std::fs::create_dir_all(&dir)
                                .and_then(|()| std::fs::write(&path, &json))
                            {
                                Ok(()) => {
                                    self.status_message = Some(format!(
                                        "Saved setup backup bundle to {}",
                                        path.display()
                                    ));
                                    self.error_message = None;
                                }
                                Err(err) => {
                                    self.error_message =
                                        Some(format!("Failed to save backup file: {err}"));
                                }
                            }
                        }
                        if ui.add(primary_button("Copy JSON", p)).clicked() {
                            ctx.copy_text(json.clone());
                            self.status_message =
                                Some("Copied full setup backup JSON to clipboard.".to_string());
                            self.error_message = None;
                        }
                    });
                });
        }
        if close_backup_export {
            self.setup_backup_export_json = None;
        }

        // --- RESTORE FULL SETUP BACKUP MODAL ---
        if self.setup_backup_import_open {
            let mut close_backup_import = false;
            let mut restore_backup_now = false;
            let modal_w = (ctx.screen_rect().width() - 48.0).clamp(380.0, 560.0);
            egui::Window::new("Restore Setup Backup (polled.setup.v1)")
                .collapsible(false)
                .resizable(true)
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
                                "Paste a Polled setup backup JSON bundle (`polled.setup.v1`). Restores your profiles, custom tweaks, alerts, theme, and hotkeys (never touches Windows registry directly).",
                            )
                            .size(12.0)
                            .color(p.text_muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical()
                        .id_salt("backup_import_scroll")
                        .max_height(230.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut self.setup_backup_import_text)
                                    .code_editor()
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    if let Some(err) = &self.setup_backup_error {
                        ui.add_space(6.0);
                        ui.add(
                            egui::Label::new(RichText::new(err).size(12.0).color(p.bad)).wrap(),
                        );
                    }
                    ui.add_space(10.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(primary_button("Restore Setup", p)).clicked() {
                            restore_backup_now = true;
                        }
                        if ui.add(secondary_button("Cancel", p)).clicked() {
                            close_backup_import = true;
                        }
                    });
                });

            if restore_backup_now {
                let paths = AppPaths::default_user_dir();
                match parse_setup_bundle_json(&self.setup_backup_import_text)
                    .and_then(|bundle| restore_setup_bundle(&paths, &bundle))
                {
                    Ok(restored) => {
                        let summary = restored.summary_counts();
                        self.settings = restored.settings.clone();
                        self.theme = ThemeMode::from(self.settings.theme);
                        self.theme.apply_to(ctx);
                        self.sync_theme_to_host();
                        self.update_hotkeys(self.settings.hotkeys.clone());
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::UpdateAlertConfig(
                                self.settings.alerts.clone(),
                            ));
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::UpdateAudioCycleDevices(
                                self.settings.audio_cycle_device_ids.clone(),
                            ));
                        self.profiles_ui.reload_store(restored.profiles.clone());
                        self.profile_runtime
                            .send(ProfileRuntimeCmd::UpdateStore(restored.profiles));
                        self.sync_tray_from_snapshot();
                        self.setup_backup_import_open = false;
                        self.setup_backup_error = None;
                        self.status_message = Some(format!("Restored setup backup ({summary})."));
                        self.error_message = None;
                        self.start_scan(ctx, false);
                    }
                    Err(err) => {
                        self.setup_backup_error = Some(err.to_string());
                    }
                }
            } else if close_backup_import {
                self.setup_backup_import_open = false;
                self.setup_backup_error = None;
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

fn render_hotkey_binding_row(
    ui: &mut egui::Ui,
    label: &str,
    salt: &str,
    slot: &mut Option<HotkeyBinding>,
    default_key: &str,
) {
    let mut active = slot.is_some();
    if ui.checkbox(&mut active, label).changed() {
        if active {
            *slot = Some(HotkeyBinding::new(true, true, false, default_key));
        } else {
            *slot = None;
        }
    }
    if let Some(binding) = slot.as_mut() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut binding.ctrl, "Ctrl");
            ui.checkbox(&mut binding.alt, "Alt");
            ui.checkbox(&mut binding.shift, "Shift");
            egui::ComboBox::from_id_salt(salt)
                .selected_text(&binding.key)
                .width(56.0)
                .show_ui(ui, |ui| {
                    for &k in HotkeyBinding::PRESET_KEYS {
                        ui.selectable_value(&mut binding.key, k.to_string(), k);
                    }
                });
        });
    } else {
        ui.label(RichText::new("Disabled").size(11.5).weak());
    }
}

/// Clean sidebar navigation row with subtle left active bar and right-aligned semantic badge.
fn nav_rail_item(
    ui: &mut egui::Ui,
    p: Palette,
    label: &str,
    active: bool,
    badge: Option<(&str, Color32)>,
) -> bool {
    let width = ui.available_width().max(120.0);
    let desired = Vec2::new(width, 34.0);
    let (rect, resp) = ui.allocate_exact_size(desired, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let fill = if active {
            p.surface_alt
        } else if resp.hovered() {
            Color32::from_rgba_unmultiplied(p.text.r(), p.text.g(), p.text.b(), 10)
        } else {
            Color32::TRANSPARENT
        };
        if fill != Color32::TRANSPARENT {
            painter.rect_filled(rect, CornerRadius::same(7), fill);
        }
        if active {
            let accent = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 1.0, rect.top() + 7.0),
                egui::pos2(rect.left() + 3.5, rect.bottom() - 7.0),
            );
            painter.rect_filled(accent, CornerRadius::same(2), p.text);
        }
        let text_col = if active || resp.hovered() {
            p.text
        } else {
            p.text_muted
        };
        painter.text(
            egui::pos2(rect.left() + 12.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(13.5),
            text_col,
        );
        if let Some((b_txt, b_col)) = badge {
            painter.text(
                egui::pos2(rect.right() - 10.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                b_txt,
                egui::FontId::monospace(10.5),
                b_col,
            );
        }
    }
    resp.clicked()
}

/// Quiet sidebar bottom utility row (Startup / Settings / Theme).
fn nav_utility_item(
    ui: &mut egui::Ui,
    p: Palette,
    label: &str,
    badge: Option<(&str, Color32)>,
) -> bool {
    let width = ui.available_width().max(120.0);
    let desired = Vec2::new(width, 28.0);
    let (rect, resp) = ui.allocate_exact_size(desired, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        if resp.hovered() {
            painter.rect_filled(rect, CornerRadius::same(6), p.surface_alt);
        }
        let text_col = if resp.hovered() { p.text } else { p.text_muted };
        painter.text(
            egui::pos2(rect.left() + 8.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(12.2),
            text_col,
        );
        if let Some((b_txt, b_col)) = badge {
            painter.text(
                egui::pos2(rect.right() - 8.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                b_txt,
                egui::FontId::monospace(10.0),
                b_col,
            );
        }
    }
    resp.clicked()
}

fn status_dot_count(ui: &mut egui::Ui, dot: Color32, text: &str, text_col: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(8.0, 14.0), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().circle_filled(rect.center(), 3.2, dot);
    }
    ui.label(RichText::new(text).size(12.0).color(text_col));
    ui.add_space(4.0);
}

#[allow(clippy::too_many_arguments)]
fn render_check_row(
    ui: &mut egui::Ui,
    p: Palette,
    mode: ThemeMode,
    outer_w: f32,
    check: &CheckReport,
    is_undoable: bool,
    expanded: bool,
    busy: bool,
    clicked_fix: &mut Option<CheckReport>,
    clicked_undo: &mut Option<CheckReport>,
    clicked_delete_custom: &mut Option<String>,
) -> bool {
    let sev_fg = p.severity_fg(check.severity);
    let is_custom = check.id.starts_with("custom_");
    let show_fix = check.can_fix
        && (check.severity >= Severity::Warn
            || ((check.is_optional_tweak || check.id == "power_plan")
                && check.severity == Severity::Info));

    let inner_w = (outer_w - 28.0).max(160.0);
    let mut toggled = false;

    egui::Frame::NONE
        .fill(if expanded {
            p.surface_alt
        } else {
            Color32::TRANSPARENT
        })
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_min_width(inner_w);
            ui.set_max_width(inner_w);

            // Compact 1-line summary header (click title area to expand/collapse)
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let chev = if expanded { "▾" } else { "▸" };
                    if ui
                        .add(
                            egui::Button::new(RichText::new(chev).size(12.0).color(p.text_muted))
                                .fill(Color32::TRANSPARENT)
                                .stroke(Stroke::NONE),
                        )
                        .clicked()
                    {
                        toggled = true;
                    }

                    if show_fix {
                        let verb = if check.is_optional_tweak {
                            "Apply"
                        } else {
                            "Fix"
                        };
                        let label = if check.requires_elevation {
                            format!("{verb} · Admin")
                        } else {
                            verb.to_string()
                        };
                        let btn = if check.severity >= Severity::Warn {
                            primary_button(&label, p)
                        } else {
                            secondary_button(&label, p)
                        };
                        if ui.add_enabled(!busy, btn).clicked() {
                            *clicked_fix = Some(check.clone());
                        }
                    }

                    if is_undoable && ui.add_enabled(!busy, secondary_button("Undo", p)).clicked() {
                        *clicked_undo = Some(check.clone());
                    }

                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let (dot_r, _) =
                            ui.allocate_exact_size(Vec2::new(10.0, 18.0), egui::Sense::hover());
                        if ui.is_rect_visible(dot_r) {
                            ui.painter().circle_filled(dot_r.center(), 3.8, sev_fg);
                        }

                        let header_resp = ui
                            .vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&check.name)
                                                .size(13.5)
                                                .strong()
                                                .color(p.text),
                                        )
                                        .truncate(),
                                    );
                                    if check.severity >= Severity::Warn {
                                        ui.label(
                                            RichText::new(check.severity.as_str().to_uppercase())
                                                .size(10.0)
                                                .strong()
                                                .monospace()
                                                .color(sev_fg),
                                        );
                                    } else if check.is_optional_tweak {
                                        ui.label(
                                            RichText::new(if is_custom {
                                                "custom"
                                            } else {
                                                "optional"
                                            })
                                            .size(10.5)
                                            .color(p.text_muted),
                                        );
                                    }
                                });
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&check.current_value)
                                            .size(11.8)
                                            .color(p.text_muted),
                                    )
                                    .truncate(),
                                );
                            })
                            .response
                            .interact(egui::Sense::click());
                        if header_resp.clicked() {
                            toggled = true;
                        }
                    });
                });
            });

            if expanded {
                ui.add_space(10.0);
                let col_gap = 10.0;
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

                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(RichText::new(&check.reason).size(12.2).color(p.text_muted))
                        .wrap(),
                );

                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(&check.id)
                            .size(10.5)
                            .monospace()
                            .color(p.text_muted),
                    );
                    if !check.can_fix {
                        pill(ui, "Report only", p.surface, p.text_muted, p.border_subtle);
                    }
                    if check.requires_reboot && check.can_fix {
                        pill(
                            ui,
                            "Reboot required",
                            p.severity_bg(Severity::Info, mode),
                            p.info,
                            p.border_subtle,
                        );
                    }
                    if check.requires_relogin && check.severity >= Severity::Warn {
                        pill(
                            ui,
                            "Sign-out required",
                            p.surface,
                            p.text_muted,
                            p.border_subtle,
                        );
                    }
                    if is_custom
                        && ui
                            .add_enabled(!busy, secondary_button("Delete Tweak", p))
                            .clicked()
                    {
                        *clicked_delete_custom = Some(check.id.clone());
                    }
                });
            }
        });

    toggled
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
                    "1600–3200 DPI cuts motion-initiation latency by ~1–2 ms vs. 400 DPI on micro-adjustments (adjust in-game sensitivity to keep the same cm/360°). Verify 4K/8K saturation & switch debounce in Bench.",
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

fn ghost_button(label: &str, p: Palette) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(p.text_muted))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::NONE)
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
