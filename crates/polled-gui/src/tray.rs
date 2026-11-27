// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;

use eframe::egui;
use polled_core::{BatteryStatus, SupportTier};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::icon::tray_icon_image;
use crate::profile_runtime::{format_battery_hours_left, ProfileRuntimeSnapshot};
use crate::tray_menu::{TrayMenuController, TrayMenuModel};

/// High-level action emitted by the system tray icon or Polled's custom themed popup menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    OpenContextMenu,
    ScanNow,
    ApplyProfile(String),
    QuickSetMousePolling(u16),
    QuickSetMouseDpi(u16),
    RefreshMouseAndBattery,
    SwitchKeyboardProfile(u8),
    RefreshKeyboards,
    SwitchAudioOutput(String),
    ToggleMicMute,
    RestoreNativeDisplay,
    ToggleProfileLock,
    ToggleAutoSwitch,
    ToggleTaskbarWidget,
    CheckUpdates,
    Quit,
}

/// Snapshot of the primary connected keyboard for the tray quick-switch submenu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayWootingState {
    pub device_id: String,
    pub name: String,
    pub active_profile: Option<u8>,
    pub profiles: Vec<(u8, String)>,
}

/// Snapshot of the primary connected mouse used to populate the tray Mouse Quick Controls
/// submenu and rich hover tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMouseState {
    pub name: String,
    pub is_simulator: bool,
    pub current_dpi: Option<u16>,
    pub current_polling_hz: Option<u16>,
    pub dpi_min: u16,
    pub dpi_max: u16,
    pub polling_rates_hz: Vec<u16>,
    pub battery: Option<BatteryStatus>,
    /// Rounded remaining battery runtime in 5-minute steps (keeps `Eq` stable).
    pub battery_mins_left: Option<u32>,
}

/// Finds Polled's own top-level `HWND` on the current Win32 UI thread (including when hidden).
#[cfg(windows)]
pub(crate) fn find_own_main_hwnd() -> isize {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumThreadWindows, GetWindowLongW, GetWindowTextW, GWL_STYLE, WS_CAPTION,
    };

    struct Search {
        exact_title: isize,
        fallback_captioned: isize,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut buf = [0u16; 64];
        // SAFETY: `hwnd` is supplied by `EnumThreadWindows`; `buf` is valid UTF-16 storage.
        let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
        if len > 0 {
            let title = String::from_utf16_lossy(&buf[..len as usize]);
            if title == "Polled" || title.starts_with("Polled ") {
                search.exact_title = hwnd.0 as isize;
                return BOOL(0);
            }
        }
        if search.fallback_captioned == 0 {
            // SAFETY: Querying window style on our own UI thread's window is read-only.
            let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) };
            if (style & WS_CAPTION.0 as i32) != 0 {
                search.fallback_captioned = hwnd.0 as isize;
            }
        }
        BOOL(1)
    }

    let mut search = Search {
        exact_title: 0,
        fallback_captioned: 0,
    };
    // SAFETY: `GetCurrentThreadId` has no preconditions; `search` lives across the synchronous call.
    unsafe {
        let _ = EnumThreadWindows(
            GetCurrentThreadId(),
            Some(enum_proc),
            LPARAM((&mut search as *mut Search) as isize),
        );
    }
    if search.exact_title != 0 {
        search.exact_title
    } else {
        search.fallback_captioned
    }
}

#[cfg(not(windows))]
pub(crate) fn find_own_main_hwnd() -> isize {
    0
}

/// Holds the active Windows system tray icon (crisp monochrome Polled crosshair) and Polled's
/// custom pure-Win32 themed context menu alive for the lifetime of the daemon or standalone app.
pub struct TrayController {
    tray: Option<TrayIcon>,
    menu: TrayMenuController,
    model: TrayMenuModel,
}

impl TrayController {
    /// Creates the system tray icon (without the legacy white Win32 popup menu) and wires left-click
    /// to open Polled and right-click to open Polled's custom monochrome Dark/Light popup menu.
    pub fn new(
        initial_auto_switch: bool,
        initial_taskbar_widget: bool,
        initial_dark_mode: bool,
        on_action: Arc<dyn Fn(TrayAction) + Send + Sync>,
    ) -> Self {
        let initial_model = TrayMenuModel {
            dark_mode: initial_dark_mode,
            auto_switch_enabled: initial_auto_switch,
            taskbar_widget_enabled: initial_taskbar_widget,
            ..Default::default()
        };
        let menu = TrayMenuController::start(initial_model.clone(), Arc::clone(&on_action));

        let initial_tooltip = if initial_auto_switch {
            "Polled — Auto-Switch ON"
        } else {
            "Polled — Auto-Switch OFF"
        };

        let mut builder = TrayIconBuilder::new()
            .with_tooltip(initial_tooltip)
            .with_menu_on_left_click(false);

        if let Some(icon) = tray_icon_image() {
            builder = builder.with_icon(icon);
        }

        let tray = builder.build().ok();

        if tray.is_some() {
            let on_click = on_action;
            TrayIconEvent::set_event_handler(Some(move |event| match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => on_click(TrayAction::Open),
                TrayIconEvent::Click {
                    button: MouseButton::Right,
                    button_state: MouseButtonState::Up,
                    ..
                } => on_click(TrayAction::OpenContextMenu),
                _ => {}
            }));
        }

        Self {
            tray,
            menu,
            model: initial_model,
        }
    }

    /// Opens Polled's custom monochrome context menu at the current cursor position.
    pub fn open_custom_menu_at_cursor(&self) {
        self.menu.open_at_cursor();
    }

    /// Synchronizes the custom tray popup menu's Dark / Light theme with Polled.
    pub fn set_dark_mode(&mut self, dark_mode: bool) {
        if self.model.dark_mode != dark_mode {
            self.model.dark_mode = dark_mode;
            self.menu.update_model(self.model.clone());
        }
    }

    /// Synchronizes the "Taskbar Battery Widget" toggle state in the custom popup menu.
    pub fn set_taskbar_widget_checked(&mut self, enabled: bool) {
        if self.model.taskbar_widget_enabled != enabled {
            self.model.taskbar_widget_enabled = enabled;
            self.menu.update_model(self.model.clone());
        }
    }

    /// Updates the update row in the custom tray menu to advertise an available release version,
    /// or resets it back to `"Check for Updates"` when up to date.
    pub fn set_available_update(&mut self, latest_version: Option<&str>) {
        let next = latest_version
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if self.model.available_update_version != next {
            self.model.available_update_version = next;
            self.menu.update_model(self.model.clone());
        }
    }

    /// Synchronizes the custom tray menu model and hover tooltip from a `ProfileRuntimeSnapshot`.
    pub fn sync_from_snapshot(&mut self, snap: &ProfileRuntimeSnapshot) {
        let battery_parts: Vec<String> = snap
            .active_devices
            .iter()
            .filter_map(|d| {
                d.battery
                    .map(|b| format!("{} {}", d.info.name, b.compact_label()))
            })
            .collect();
        let battery_summary = if battery_parts.is_empty() {
            None
        } else {
            Some(battery_parts.join(", "))
        };
        let primary_mouse = snap
            .active_devices
            .iter()
            .find(|d| d.info.support_tier != SupportTier::Simulator)
            .or_else(|| snap.active_devices.first())
            .map(|d| TrayMouseState {
                name: d.info.name.clone(),
                is_simulator: d.info.support_tier == SupportTier::Simulator,
                current_dpi: d.settings.as_ref().and_then(|s| s.dpi),
                current_polling_hz: d.settings.as_ref().and_then(|s| s.polling_rate_hz),
                dpi_min: d.caps.dpi_min,
                dpi_max: d.caps.dpi_max,
                polling_rates_hz: d.caps.polling_rates_hz.clone(),
                battery: d.battery,
                battery_mins_left: d
                    .battery_hours_left
                    .map(|h| (((h * 60.0).round() as u32) / 5) * 5),
            });
        let primary_wooting = snap.keyboards.first().map(|kb| TrayWootingState {
            device_id: kb.id.clone(),
            name: kb.name.clone(),
            active_profile: kb.active_profile,
            profiles: if kb.capabilities.supports_profile_switch {
                kb.profiles
                    .iter()
                    .map(|slot| (slot.index, slot.name.clone()))
                    .collect()
            } else {
                Vec::new()
            },
        });

        self.model.auto_switch_enabled = snap.auto_switch_enabled;
        self.model.profile_locked = snap.profile_locked;
        self.model.mic_muted = snap.mic_muted == Some(true);
        self.model.active_display_override = snap.active_display_override.clone();
        self.model.active_profile_id = snap.active_profile_id.clone();
        self.model.active_profile_name = snap.active_profile_name.clone();
        self.model.available_profiles = snap.available_profiles.clone();
        self.model.audio_outputs = snap.audio_outputs.clone();
        self.model.primary_mouse = primary_mouse.clone();
        self.model.primary_wooting = primary_wooting;
        self.menu.update_model(self.model.clone());

        // Rich hover tooltip (`Polled · Mouse · 68% · 4000 Hz · Profile`).
        if let Some(tray) = &self.tray {
            let mut parts: Vec<String> = vec!["Polled".to_string()];
            if let Some(mouse) = primary_mouse.as_ref().filter(|m| !m.is_simulator) {
                parts.push(mouse.name.clone());
                if let Some(batt) = mouse.battery {
                    let lbl = match mouse.battery_mins_left.filter(|&m| m > 0) {
                        Some(mins) => format!(
                            "{} ({})",
                            batt.compact_label(),
                            format_battery_hours_left(mins as f32 / 60.0)
                        ),
                        None => batt.compact_label(),
                    };
                    parts.push(lbl);
                }
                if let Some(hz) = mouse.current_polling_hz {
                    parts.push(format!("{hz} Hz"));
                }
            } else if let Some(batt) = battery_summary.as_deref().filter(|s| !s.is_empty()) {
                parts.push(batt.to_string());
            }

            let profile_part = if snap.profile_locked {
                format!("{} [LOCKED]", snap.active_profile_name)
            } else if snap.auto_switch_enabled {
                snap.active_profile_name.clone()
            } else {
                format!("{} (Auto OFF)", snap.active_profile_name)
            };
            parts.push(profile_part);
            if snap.mic_muted == Some(true) {
                parts.push("MIC MUTED".to_string());
            }

            let mut tooltip = parts.join(" · ");
            // Windows NOTIFYICONDATAW.szTip is capped at 127 UTF-16 code units.
            if tooltip.chars().count() > 120 {
                let truncated: String = tooltip.chars().take(117).collect();
                tooltip = format!("{truncated}...");
            }
            let _ = tray.set_tooltip(Some(tooltip));
        }
    }
}

/// Restores and focuses the Polled window even when it is currently hidden (`WS_VISIBLE == 0`).
///
/// Using Win32 `ShowWindowAsync` directly avoids the `winit`/`eframe` Catch-22 where
/// `ctx.request_repaint()` does not fire `WM_PAINT` on an invisible Win32 window.
pub(crate) fn show_and_focus(main_hwnd: &AtomicIsize, ctx: &egui::Context) {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            SetForegroundWindow, ShowWindowAsync, SW_RESTORE, SW_SHOW,
        };

        let raw = main_hwnd.load(Ordering::SeqCst);
        if raw != 0 {
            let hwnd = HWND(raw as *mut core::ffi::c_void);
            // SAFETY: `hwnd` is our own process's main winit window captured on the UI thread.
            // `ShowWindowAsync` posts asynchronously so calling from a background thread never deadlocks.
            unsafe {
                let _ = ShowWindowAsync(hwnd, SW_SHOW);
                let _ = ShowWindowAsync(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }

    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
}

#[cfg(windows)]
pub(crate) fn request_native_close(main_hwnd: &AtomicIsize) {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

    let raw = main_hwnd.load(Ordering::SeqCst);
    if raw != 0 {
        let hwnd = HWND(raw as *mut core::ffi::c_void);
        // SAFETY: `PostMessageW(WM_CLOSE)` queues a clean close directly onto the UI thread even when hidden.
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

#[cfg(not(windows))]
pub(crate) fn request_native_close(_main_hwnd: &AtomicIsize) {}
