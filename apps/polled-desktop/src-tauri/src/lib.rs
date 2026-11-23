// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

mod elevation;
mod notifications;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use polled_core::win::WindowsEnv;
use polled_core::{AppPaths, CheckReport, HealthEngine, PolledError, ScanSummary, UndoSummary};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

use crate::elevation::{run_elevated_cli, ElevatedAction};

const MAIN_WINDOW_LABEL: &str = "main";

/// Reopens the main WebView window lazily if it was destroyed on close-to-tray,
/// or focuses it if it is already open.
fn open_or_focus_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }

    let _ = WebviewWindowBuilder::new(app, MAIN_WINDOW_LABEL, WebviewUrl::App("index.html".into()))
        .title("Polled — Input Health Check")
        .inner_size(940.0, 700.0)
        .min_inner_size(720.0, 520.0)
        .center()
        .build();
}

#[tauri::command]
fn scan_health() -> Result<ScanSummary, String> {
    let env = WindowsEnv;
    let paths = AppPaths::default_user_dir();
    let engine = HealthEngine::with_default_checks();
    engine
        .scan_all(&env, &paths, true)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_snapshot_path() -> String {
    AppPaths::default_user_dir()
        .baseline_snapshot_path()
        .display()
        .to_string()
}

#[tauri::command]
fn fix_check(check_id: String, elevated: bool) -> Result<CheckReport, String> {
    let env = WindowsEnv;
    let paths = AppPaths::default_user_dir();
    let engine = HealthEngine::with_default_checks();

    if elevated {
        run_elevated_cli(ElevatedAction::Fix(&check_id), &paths)?;
        let summary = engine
            .scan_all(&env, &paths, true)
            .map_err(|e| e.to_string())?;
        return summary
            .checks
            .into_iter()
            .find(|c| c.id == check_id)
            .ok_or_else(|| format!("Check '{check_id}' not found after fix"));
    }

    match engine.fix_check(&check_id, &env, &paths) {
        Ok(report) => Ok(report),
        Err(PolledError::ElevationRequired { reason, .. }) => {
            Err(format!("ELEVATION_REQUIRED:{reason}"))
        }
        Err(err) => Err(err.to_string()),
    }
}

#[tauri::command]
fn fix_all_safe() -> Result<ScanSummary, String> {
    let env = WindowsEnv;
    let paths = AppPaths::default_user_dir();
    let engine = HealthEngine::with_default_checks();
    engine
        .fix_all_safe(&env, &paths)
        .map_err(|e| e.to_string())?;
    engine
        .scan_all(&env, &paths, true)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn undo_check(check_id: String, elevated: bool) -> Result<ScanSummary, String> {
    let env = WindowsEnv;
    let paths = AppPaths::default_user_dir();
    let engine = HealthEngine::with_default_checks();

    if elevated {
        run_elevated_cli(ElevatedAction::Undo(&check_id), &paths)?;
    } else if let Err(err) = engine.undo_check(&check_id, &env, &paths) {
        if let PolledError::ElevationRequired { reason, .. } = err {
            return Err(format!("ELEVATION_REQUIRED:{reason}"));
        }
        return Err(err.to_string());
    }

    engine
        .scan_all(&env, &paths, true)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn undo_all(elevated: bool) -> Result<UndoSummary, String> {
    let env = WindowsEnv;
    let paths = AppPaths::default_user_dir();
    let engine = HealthEngine::with_default_checks();

    if elevated {
        run_elevated_cli(ElevatedAction::UndoAll, &paths)?;
        return Ok(UndoSummary {
            restored_ids: Vec::new(),
            failed: Vec::new(),
            remaining_in_snapshot: usize::from(paths.baseline_snapshot_path().exists()),
        });
    }

    engine.undo_all(&env, &paths).map_err(|e| e.to_string())
}

pub fn run() {
    let quit_requested = Arc::new(AtomicBool::new(false));
    let quit_for_menu = Arc::clone(&quit_requested);
    let quit_for_loop = Arc::clone(&quit_requested);

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            scan_health,
            get_snapshot_path,
            fix_check,
            fix_all_safe,
            undo_check,
            undo_all,
        ])
        .setup(move |app| {
            let open_item = MenuItem::with_id(app, "open", "Open Polled", true, None::<&str>)?;
            let scan_item = MenuItem::with_id(app, "scan", "Scan Now", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_item, &scan_item, &quit_item])?;

            let quit_flag = Arc::clone(&quit_for_menu);
            let mut tray_builder = TrayIconBuilder::new()
                .tooltip("Polled — Input Health Check")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        open_or_focus_main_window(tray.app_handle());
                    }
                })
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "open" => open_or_focus_main_window(app),
                    "scan" => {
                        let handle = app.clone();
                        std::thread::spawn(move || {
                            let engine = HealthEngine::with_default_checks();
                            if let Ok(summary) =
                                engine.scan_all(&WindowsEnv, &AppPaths::default_user_dir(), true)
                            {
                                notifications::notify_if_regressed(&handle, &summary);
                            }
                        });
                    }
                    "quit" => {
                        quit_flag.store(true, Ordering::SeqCst);
                        app.exit(0);
                    }
                    _ => {}
                });

            if let Some(icon) = app.default_window_icon() {
                tray_builder = tray_builder.icon(icon.clone());
            }
            let _tray = tray_builder.build(app)?;

            notifications::spawn_startup_and_build_watcher(app.handle().clone());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build Polled desktop application");

    // Keep the lightweight tray process alive when the user closes the window.
    // Because we do NOT intercept WindowEvent::CloseRequested, closing [X] actually destroys
    // the WebView2 instance and frees its RAM until the user clicks the tray icon again.
    app.run(move |_app_handle, event| {
        if let RunEvent::ExitRequested { api, .. } = event {
            if !quit_for_loop.load(Ordering::SeqCst) {
                api.prevent_exit();
            }
        }
    });
}
