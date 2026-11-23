// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

// Prevents a console window from flashing when launching the GUI/tray app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod elevation;
mod icon;
mod notifications;
mod profile_runtime;
mod profiles_ui;
mod theme;
mod tray;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("Polled")
            .with_inner_size([980.0, 720.0])
            .with_min_inner_size([760.0, 540.0])
            .with_icon(icon::window_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "Polled",
        native_options,
        Box::new(|cc| Ok(Box::new(app::PolledApp::new(cc)))),
    )
}
