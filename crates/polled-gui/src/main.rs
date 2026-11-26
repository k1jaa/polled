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
mod theme;
mod tray;

use eframe::egui;
use single_instance::{InstanceLock, SingleInstanceGuard};

enum LaunchMode {
    /// Pure-Win32 zero-OpenGL background tray daemon (default on Windows).
    Daemon { start_minimized: bool },
    /// On-demand GPU GUI window connected to the running daemon over local Win32 Named Pipes.
    UiClient { daemon_pid: u32, open_bench: bool },
    /// Single-process fallback (`--standalone` or non-Windows).
    Standalone { start_minimized: bool },
}

fn parse_launch_mode() -> LaunchMode {
    let args: Vec<String> = std::env::args().collect();
    let start_minimized = args.iter().any(|a| a == "--minimized");
    let open_bench = args.iter().any(|a| a == "--open-bench");

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
        return LaunchMode::Standalone { start_minimized };
    }

    LaunchMode::Daemon { start_minimized }
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
        LaunchMode::Daemon { start_minimized } => {
            if let Err(_err) = daemon::run_daemon(start_minimized) {
                // Graceful fallback to single-process mode if daemon setup unexpectedly fails.
                let guard = match SingleInstanceGuard::acquire(!start_minimized) {
                    InstanceLock::Primary(guard) => guard,
                    InstanceLock::AlreadyRunning => return Ok(()),
                };
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
        LaunchMode::Standalone { start_minimized } => {
            let guard = match SingleInstanceGuard::acquire(!start_minimized) {
                InstanceLock::Primary(guard) => guard,
                InstanceLock::AlreadyRunning => return Ok(()),
            };
            eframe::run_native(
                "Polled",
                build_native_options(!start_minimized),
                Box::new(move |cc| Ok(Box::new(app::PolledApp::new_standalone(cc, guard)))),
            )
        }
    }
}
