// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use notify_rust::Notification;
use polled_core::checks::state::PolledState;
use polled_core::win::{SystemEnv, WindowsEnv};
use polled_core::{AppPaths, HealthEngine, ScanSummary};

use crate::app::WorkerMsg;

/// Sends a Windows notification *only* if one or more health checks regressed.
pub fn notify_if_regressed(summary: &ScanSummary) {
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

    let _ = Notification::new().summary(title).body(&body).show();
}

/// Sends a one-shot Windows notification when a connected wireless mouse battery drops to or below threshold.
pub fn notify_low_battery(device_name: &str, percent: u8) {
    let title = format!("Polled: Low mouse battery ({percent}%)");
    let body = format!(
        "{device_name} is at {percent}% battery. Plug in or dock it soon to avoid wireless dropouts."
    );
    let _ = Notification::new().summary(&title).body(&body).show();
}

/// Sends an optional Windows toast notification when Polled activates a game or desktop profile.
pub fn notify_profile_switched(profile_name: &str, details: &str) {
    let title = format!("Polled: {profile_name}");
    let body = if details.trim().is_empty() {
        "Profile active.".to_string()
    } else {
        details.to_string()
    };
    let _ = Notification::new().summary(&title).body(&body).show();
}

/// Spawns a lightweight watcher that periodically checks whether the Windows build/UBR
/// changed (e.g., after a Windows Update) and triggers a re-scan + regression alert.
pub fn spawn_windows_build_watcher(tx: Sender<WorkerMsg>, wake: Arc<dyn Fn() + Send + Sync>) {
    std::thread::spawn(move || {
        let env = WindowsEnv;
        let paths = AppPaths::default_user_dir();
        let engine = HealthEngine::with_default_checks();

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
                    notify_if_regressed(&summary);
                    let _ = tx.send(WorkerMsg::ScanFinished {
                        result: Ok(summary),
                        notify_regressions: false,
                    });
                    wake();
                }
            }
        }
    });
}
