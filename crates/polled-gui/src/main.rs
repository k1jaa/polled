// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

// Prevents a console window from flashing when launching the GUI/tray app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod daemon;
mod elevation;
mod hotkeys;
mod hotplug;
mod icon;
mod input_lab_ui;
mod ipc;
mod keyboard_ui;
mod notifications;
mod osd;
mod profile_runtime;
mod profiles_ui;
mod single_instance;
mod taskbar_widget;
mod theme;
mod tray;

use std::time::Duration;

use eframe::egui;
use single_instance::{InstanceLock, SingleInstanceGuard};

/// Acquires the single-instance lock.
///
/// - When relaunching after an in-place update (`--after-update`), waits quietly up to 8 seconds
///   for the previous daemon to release the mutex without waking it.
/// - When launching a strictly newer version while an older daemon holds the mutex (detected via
///   `%LOCALAPPDATA%\Polled\daemon.json`), signals the older daemon to quit cleanly and waits up
///   to 8 seconds to take over automatically instead of popping open the stale tray instance.
pub(crate) fn acquire_instance_guard(
    signal_existing_to_show: bool,
    wait_for_previous_exit: bool,
) -> InstanceLock {
    if !wait_for_previous_exit {
        match SingleInstanceGuard::acquire(false) {
            InstanceLock::Primary(guard) => return InstanceLock::Primary(guard),
            InstanceLock::AlreadyRunning => {
                let paths = polled_core::AppPaths::default_user_dir();
                let current_pid = std::process::id();
                let current_version = env!("CARGO_PKG_VERSION");

                if let Some(running) =
                    single_instance::read_running_daemon_info(&paths.daemon_state_path())
                {
                    if single_instance::should_take_over_running_daemon(
                        &running,
                        current_pid,
                        current_version,
                    ) {
                        // Prefer the zero-contention Win32 named quit event (works even if a `--ui`
                        // window is open or in standalone mode); fall back to IPC if unavailable.
                        let signaled = SingleInstanceGuard::signal_existing_to_quit();
                        if !signaled {
                            if let Ok(client) = ipc::IpcClient::connect(running.pid, |_| {}, || {})
                            {
                                client.send(ipc::UiToDaemonMsg::RequestQuitDaemon);
                                std::thread::sleep(Duration::from_millis(120));
                            }
                        }
                        for _ in 0..80 {
                            if let InstanceLock::Primary(guard) =
                                SingleInstanceGuard::acquire(false)
                            {
                                return InstanceLock::Primary(guard);
                            }
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                }

                if signal_existing_to_show {
                    let _ = SingleInstanceGuard::signal_existing_to_show();
                }
                return InstanceLock::AlreadyRunning;
            }
        }
    }

    for _ in 0..80 {
        if let InstanceLock::Primary(guard) = SingleInstanceGuard::acquire(false) {
            return InstanceLock::Primary(guard);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    SingleInstanceGuard::acquire(signal_existing_to_show)
}

/// Cleans up leftover `*.exe.old` backups from an in-place update (with brief retries right after
/// an update handoff in case the exiting old binary still holds a handle for a moment).
pub(crate) fn spawn_stale_backup_cleanup(retry_after_update: bool) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent().map(|p| p.to_path_buf()) else {
        return;
    };
    polled_core::cleanup_stale_update_backups(&dir);
    if retry_after_update {
        std::thread::spawn(move || {
            for ms in [500u64, 1_500, 3_000] {
                std::thread::sleep(Duration::from_millis(ms));
                polled_core::cleanup_stale_update_backups(&dir);
            }
        });
    }
}

enum LaunchMode {
    /// Pure-Win32 zero-OpenGL background tray daemon (default on Windows).
    Daemon {
        start_minimized: bool,
        after_update: bool,
    },
    /// On-demand GPU GUI window connected to the running daemon over local Win32 Named Pipes.
    UiClient { daemon_pid: u32, open_bench: bool },
    /// Single-process fallback (`--standalone` or non-Windows).
    Standalone {
        start_minimized: bool,
        after_update: bool,
    },
}

fn parse_launch_mode() -> LaunchMode {
    let args: Vec<String> = std::env::args().collect();
    let start_minimized = args.iter().any(|a| a == "--minimized");
    let open_bench = args.iter().any(|a| a == "--open-bench");
    let after_update = args.iter().any(|a| a == "--after-update");

    if args.iter().any(|a| a == "--ui") {
        let daemon_pid = args
            .windows(2)
            .find(|w| w[0] == "--ipc-pid")
            .and_then(|w| w[1].parse::<u32>().ok());
        if let Some(pid) = daemon_pid {
            return LaunchMode::UiClient {
                daemon_pid: pid,
                open_bench,
            };
        }
    }

    if args.iter().any(|a| a == "--standalone") || !cfg!(windows) {
        return LaunchMode::Standalone {
            start_minimized,
            after_update,
        };
    }

    LaunchMode::Daemon {
        start_minimized,
        after_update,
    }
}

fn build_native_options(visible: bool) -> eframe::NativeOptions {
    let window_title = format!("Polled v{} (Experimental)", env!("CARGO_PKG_VERSION"));
    eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title(window_title)
            .with_inner_size([1120.0, 740.0])
            .with_min_inner_size([740.0, 540.0])
            .with_visible(visible)
            .with_icon(icon::window_icon()),
        ..Default::default()
    }
}

fn main() -> eframe::Result<()> {
    match parse_launch_mode() {
        LaunchMode::Daemon {
            start_minimized,
            after_update,
        } => {
            if let Err(_err) = daemon::run_daemon(start_minimized, after_update) {
                // Graceful fallback to single-process mode if daemon setup unexpectedly fails.
                let guard = match acquire_instance_guard(!start_minimized, after_update) {
                    InstanceLock::Primary(guard) => guard,
                    InstanceLock::AlreadyRunning => return Ok(()),
                };
                spawn_stale_backup_cleanup(after_update);
                return eframe::run_native(
                    "Polled",
                    build_native_options(!start_minimized),
                    Box::new(move |cc| Ok(Box::new(app::PolledApp::new_standalone(cc, guard)))),
                );
            }
            Ok(())
        }
        LaunchMode::UiClient {
            daemon_pid,
            open_bench,
        } => eframe::run_native(
            "Polled",
            build_native_options(true),
            Box::new(move |cc| {
                let app = app::PolledApp::new_remote(cc, daemon_pid, open_bench)
                    .map_err(|err| -> Box<dyn std::error::Error + Send + Sync> { err.into() })?;
                Ok(Box::new(app))
            }),
        ),
        LaunchMode::Standalone {
            start_minimized,
            after_update,
        } => {
            let guard = match acquire_instance_guard(!start_minimized, after_update) {
                InstanceLock::Primary(guard) => guard,
                InstanceLock::AlreadyRunning => return Ok(()),
            };
            spawn_stale_backup_cleanup(after_update);
            eframe::run_native(
                "Polled",
                build_native_options(!start_minimized),
                Box::new(move |cc| Ok(Box::new(app::PolledApp::new_standalone(cc, guard)))),
            )
        }
    }
}
