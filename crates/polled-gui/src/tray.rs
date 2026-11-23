// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::app::WorkerMsg;
use crate::icon::tray_icon_image;

const ID_OPEN: &str = "open";
const ID_SCAN: &str = "scan";
const ID_AUTO_SWITCH: &str = "auto_switch";
const ID_QUIT: &str = "quit";

/// Holds the active Windows system tray icon and dynamic menu items alive for the lifetime of the app.
pub struct TrayController {
    tray: Option<TrayIcon>,
    auto_switch_item: CheckMenuItem,
}

impl TrayController {
    /// Creates the system tray icon (must be called on the main Win32 UI thread) and
    /// starts a tiny listener thread that wakes the `egui` window on tray interactions.
    pub fn init(
        ctx: egui::Context,
        tx: Sender<WorkerMsg>,
        quit_requested: Arc<AtomicBool>,
        initial_auto_switch: bool,
    ) -> Self {
        let menu = Menu::new();
        let open_item = MenuItem::with_id(ID_OPEN, "Open Polled", true, None);
        let scan_item = MenuItem::with_id(ID_SCAN, "Scan Now", true, None);
        let sep_top = PredefinedMenuItem::separator();
        let auto_switch_item = CheckMenuItem::with_id(
            ID_AUTO_SWITCH,
            "Auto-Switch Profiles",
            true,
            initial_auto_switch,
            None,
        );
        let sep_bottom = PredefinedMenuItem::separator();
        let quit_item = MenuItem::with_id(ID_QUIT, "Quit", true, None);
        let _ = menu.append(&open_item);
        let _ = menu.append(&scan_item);
        let _ = menu.append(&sep_top);
        let _ = menu.append(&auto_switch_item);
        let _ = menu.append(&sep_bottom);
        let _ = menu.append(&quit_item);

        let initial_tooltip = if initial_auto_switch {
            "Polled — Auto-Switch ON"
        } else {
            "Polled — Auto-Switch OFF"
        };

        let mut builder = TrayIconBuilder::new()
            .with_tooltip(initial_tooltip)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false);

        if let Some(icon) = tray_icon_image() {
            builder = builder.with_icon(icon);
        }

        let tray = builder.build().ok();

        if tray.is_some() {
            std::thread::spawn(move || {
                let tray_rx = TrayIconEvent::receiver();
                let menu_rx = MenuEvent::receiver();

                loop {
                    if quit_requested.load(Ordering::SeqCst) {
                        break;
                    }

                    while let Ok(event) = tray_rx.try_recv() {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            show_and_focus(&ctx);
                        }
                    }

                    while let Ok(event) = menu_rx.try_recv() {
                        match event.id.0.as_str() {
                            ID_OPEN => show_and_focus(&ctx),
                            ID_SCAN => {
                                let _ = tx.send(WorkerMsg::TrayScanRequested);
                                ctx.request_repaint();
                            }
                            ID_AUTO_SWITCH => {
                                let _ = tx.send(WorkerMsg::TrayToggleAutoSwitch);
                                ctx.request_repaint();
                            }
                            ID_QUIT => {
                                quit_requested.store(true, Ordering::SeqCst);
                                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                ctx.request_repaint();
                                return;
                            }
                            _ => {}
                        }
                    }

                    std::thread::sleep(Duration::from_millis(50));
                }
            });
        }

        Self {
            tray,
            auto_switch_item,
        }
    }

    /// Synchronizes the tray menu checkmark and hover tooltip with the live profile runtime.
    pub fn update_status(&self, auto_switch_enabled: bool, active_profile_name: &str) {
        self.auto_switch_item.set_checked(auto_switch_enabled);
        if let Some(tray) = &self.tray {
            let tooltip = if auto_switch_enabled {
                format!("Polled — Auto-Switch ON · {active_profile_name}")
            } else {
                "Polled — Auto-Switch OFF".to_string()
            };
            let _ = tray.set_tooltip(Some(tooltip));
        }
    }
}

pub(crate) fn show_and_focus(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
}
