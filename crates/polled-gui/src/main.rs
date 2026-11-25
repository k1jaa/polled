// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

// Prevents a console window from flashing when launching the GUI/tray app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod elevation;
mod hotkeys;
mod hotplug;
mod icon;
mod input_lab_ui;
mod notifications;
mod osd;
mod profile_runtime;
mod profiles_ui;
mod single_instance;
mod theme;
mod tray;

use eframe::egui;
use single_instance::{InstanceLock, SingleInstanceGuard};

fn main() -> eframe::Result<()> {
    let start_minimized = std::env::args().any(|arg| arg == "--minimized");

    let guard = match SingleInstanceGuard::acquire(!start_minimized) {
        InstanceLock::Primary(guard) => guard,
        InstanceLock::AlreadyRunning => return Ok(()),
    };

    let window_title = format!("Polled v{} (Experimental)", env!("CARGO_PKG_VERSION"));
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title(window_title)
            .with_inner_size([1120.0, 740.0])
            .with_min_inner_size([740.0, 540.0])
            .with_visible(!start_minimized)
            .with_icon(icon::window_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "Polled",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::PolledApp::new(cc, guard)))),
    )
}
