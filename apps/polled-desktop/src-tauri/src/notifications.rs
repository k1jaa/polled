// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Duration;

use polled_core::checks::state::PolledState;
use polled_core::win::{SystemEnv, WindowsEnv};
use polled_core::{AppPaths, HealthEngine, ScanSummary};
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

/// Sends a Windows notification *only* if one or more health checks got worse since the last scan.
pub fn notify_if_regressed(app: &AppHandle, summary: &ScanSummary) {
    if summary.regressions.is_empty() {
        return;
    }

    let names: Vec<&str> = summary
        .regressions
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    let title = if summary.windows_build_changed {
        "Polled: Windows update changed input settings"
    } else {
        "Polled: Input setting regression detected"
    };

    let body = if names.len() == 1 {
        format!(
            "{} regressed ({}). Open Polled to inspect or fix.",
            names[0], summary.regressions[0].current_value
        )
    } else {
        format!(
            "{} settings got worse: {}. Open Polled to review.",
            names.len(),
            names.join(", ")
        )
    };

    let _ = app.notification().builder().title(title).body(body).show();
}

/// Starts a lightweight background thread that:
/// 1. Runs an initial health scan on startup (notifying *only* on regressions).
/// 2. Periodically checks whether the Windows build/UBR changed (e.g. after Windows Update)
///    and re-scans if so.
pub fn spawn_startup_and_build_watcher(app: AppHandle) {
    std::thread::spawn(move || {
        let env = WindowsEnv;
        let paths = AppPaths::default_user_dir();
        let engine = HealthEngine::with_default_checks();

        if let Ok(summary) = engine.scan_all(&env, &paths, true) {
            notify_if_regressed(&app, &summary);
        }

        // Light, low-overhead build number check every 6 hours (only reads 2 registry values).
        loop {
            std::thread::sleep(Duration::from_secs(6 * 60 * 60));

            let current_build = match env.windows_version() {
                Ok(v) => v.display_string(),
                Err(_) => continue,
            };

            let build_changed = PolledState::load(&paths.state_path())
                .ok()
                .flatten()
                .is_some_and(|s| s.last_windows_version != current_build);

            if build_changed {
                if let Ok(summary) = engine.scan_all(&env, &paths, true) {
                    notify_if_regressed(&app, &summary);
                }
            }
        }
    });
}
