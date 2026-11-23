// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

// Prevents a console window from flashing when launching the GUI/tray app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod elevation;
mod icon;
mod notifications;
mod tray;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("Polled — Input Health Check")
            .with_inner_size([940.0, 700.0])
            .with_min_inner_size([720.0, 500.0])
            .with_icon(icon::window_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "Polled",
        native_options,
        Box::new(|cc| Ok(Box::new(app::PolledApp::new(cc)))),
    )
}
