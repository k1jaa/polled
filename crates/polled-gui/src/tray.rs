// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;

use eframe::egui;
use polled_core::{BatteryStatus, SupportTier};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::icon::{tray_battery_icon_image, tray_icon_image};
use crate::profile_runtime::{format_battery_hours_left, ProfileRuntimeSnapshot};

const ID_STATUS: &str = "status_info";
const ID_OPEN: &str = "open";
const ID_SCAN: &str = "scan";
const ID_AUTO_SWITCH: &str = "auto_switch";
const ID_LOCK_PROFILE: &str = "lock_profile";
const ID_PROFILE_PREFIX: &str = "profile::";
const ID_MOUSE_POLL_PREFIX: &str = "mouse_poll::";
const ID_MOUSE_DPI_PREFIX: &str = "mouse_dpi::";
const ID_MOUSE_REFRESH: &str = "mouse_refresh";
const ID_WOOTING_PREFIX: &str = "wooting::";
const ID_WOOTING_REFRESH: &str = "wooting_refresh";
const ID_AUDIO_PREFIX: &str = "audio::";
const ID_MIC_MUTE: &str = "mic_mute";
const ID_RESTORE_DISPLAY: &str = "restore_display";
const ID_QUIT: &str = "quit";

/// High-level action emitted by the native Windows system tray icon or its context menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAction {
    Open,
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
/// submenu, live battery icon, and rich hover tooltip.
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
    /// Rounded remaining battery runtime in 5-minute steps (keeps `Eq` stable without menu churn).
    pub battery_mins_left: Option<u32>,
}

/// Parses a `tray_icon::menu::MenuEvent` ID string into a typed `TrayAction`.
pub fn parse_menu_action(id_str: &str) -> Option<TrayAction> {
    if let Some(prof_id) = id_str.strip_prefix(ID_PROFILE_PREFIX) {
        return Some(TrayAction::ApplyProfile(prof_id.to_string()));
    }
    if let Some(hz_str) = id_str.strip_prefix(ID_MOUSE_POLL_PREFIX) {
        return hz_str
            .parse::<u16>()
            .ok()
            .map(TrayAction::QuickSetMousePolling);
    }
    if let Some(dpi_str) = id_str.strip_prefix(ID_MOUSE_DPI_PREFIX) {
        return dpi_str
            .parse::<u16>()
            .ok()
            .map(TrayAction::QuickSetMouseDpi);
    }
    if let Some(slot_str) = id_str.strip_prefix(ID_WOOTING_PREFIX) {
        return slot_str
            .parse::<u8>()
            .ok()
            .map(TrayAction::SwitchKeyboardProfile);
    }
    if let Some(audio_id) = id_str.strip_prefix(ID_AUDIO_PREFIX) {
        return Some(TrayAction::SwitchAudioOutput(audio_id.to_string()));
    }
    match id_str {
        ID_OPEN => Some(TrayAction::Open),
        ID_SCAN => Some(TrayAction::ScanNow),
        ID_MOUSE_REFRESH => Some(TrayAction::RefreshMouseAndBattery),
        ID_WOOTING_REFRESH => Some(TrayAction::RefreshKeyboards),
        ID_MIC_MUTE => Some(TrayAction::ToggleMicMute),
        ID_RESTORE_DISPLAY => Some(TrayAction::RestoreNativeDisplay),
        ID_LOCK_PROFILE => Some(TrayAction::ToggleProfileLock),
        ID_AUTO_SWITCH => Some(TrayAction::ToggleAutoSwitch),
        ID_QUIT => Some(TrayAction::Quit),
        _ => None,
    }
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

/// Holds the active Windows system tray icon and dynamic menu items alive for the lifetime of
/// the daemon (or standalone app). Uses native Win32 event callbacks with zero polling threads.
pub struct TrayController {
    tray: Option<TrayIcon>,
    status_item: MenuItem,
    profiles_submenu: Submenu,
    profile_menu_items: Vec<MenuItem>,
    last_profile_key: (String, Vec<(String, String)>),
    mouse_submenu: Submenu,
    mouse_menu_items: Vec<MenuItem>,
    mouse_menu_seps: Vec<PredefinedMenuItem>,
    last_mouse_key: Option<Option<TrayMouseState>>,
    wooting_submenu: Submenu,
    wooting_menu_items: Vec<MenuItem>,
    wooting_menu_seps: Vec<PredefinedMenuItem>,
    last_wooting_key: Option<Option<TrayWootingState>>,
    audio_submenu: Submenu,
    audio_menu_items: Vec<MenuItem>,
    last_audio_key: Vec<(String, String, bool)>,
    mic_mute_item: CheckMenuItem,
    restore_display_item: MenuItem,
    auto_switch_item: CheckMenuItem,
    lock_profile_item: CheckMenuItem,
    /// Cached rendered icon state: `None` = unset, `Some(None)` = default crosshair,
    /// `Some(Some((percent, charging, low_threshold)))` = dynamic battery percentage icon.
    last_icon_key: Option<Option<(u8, bool, u8)>>,
}

impl TrayController {
    /// Creates the system tray icon (must be called on a thread pumping Win32 messages) and
    /// registers event-driven callbacks (`TrayIconEvent::set_event_handler` / `MenuEvent::set_event_handler`)
    /// so tray clicks wake the host immediately with 0% idle CPU and zero polling threads.
    pub fn new(
        initial_auto_switch: bool,
        on_action: Arc<dyn Fn(TrayAction) + Send + Sync>,
    ) -> Self {
        let menu = Menu::new();
        let status_item = MenuItem::with_id(ID_STATUS, "Polled · Initializing...", false, None);
        let sep_status = PredefinedMenuItem::separator();
        let open_item = MenuItem::with_id(ID_OPEN, "Open Polled", true, None);
        let scan_item = MenuItem::with_id(ID_SCAN, "Scan Now", true, None);
        let sep_top = PredefinedMenuItem::separator();
        let profiles_submenu = Submenu::new("Switch Profile", true);
        let mouse_submenu = Submenu::new("Mouse Quick Controls", true);
        let wooting_submenu = Submenu::new("Keyboard", true);
        let audio_submenu = Submenu::new("Audio Output", true);
        let mic_mute_item =
            CheckMenuItem::with_id(ID_MIC_MUTE, "Mute Microphone", true, false, None);
        let restore_display_item =
            MenuItem::with_id(ID_RESTORE_DISPLAY, "Display: Native", false, None);
        let lock_profile_item =
            CheckMenuItem::with_id(ID_LOCK_PROFILE, "Lock Current Profile", true, false, None);
        let auto_switch_item = CheckMenuItem::with_id(
            ID_AUTO_SWITCH,
            "Auto-Switch Profiles",
            true,
            initial_auto_switch,
            None,
        );
        let sep_bottom = PredefinedMenuItem::separator();
        let quit_item = MenuItem::with_id(ID_QUIT, "Quit", true, None);
        let _ = menu.append(&status_item);
        let _ = menu.append(&sep_status);
        let _ = menu.append(&open_item);
        let _ = menu.append(&scan_item);
        let _ = menu.append(&sep_top);
        let _ = menu.append(&profiles_submenu);
        let _ = menu.append(&mouse_submenu);
        let _ = menu.append(&wooting_submenu);
        let _ = menu.append(&audio_submenu);
        let _ = menu.append(&mic_mute_item);
        let _ = menu.append(&restore_display_item);
        let _ = menu.append(&lock_profile_item);
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
            let on_click = Arc::clone(&on_action);
            TrayIconEvent::set_event_handler(Some(move |event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    on_click(TrayAction::Open);
                }
            }));

            let on_menu = on_action;
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if let Some(action) = parse_menu_action(event.id.0.as_str()) {
                    on_menu(action);
                }
            }));
        }

        Self {
            tray,
            status_item,
            profiles_submenu,
            profile_menu_items: Vec::new(),
            last_profile_key: (String::new(), Vec::new()),
            mouse_submenu,
            mouse_menu_items: Vec::new(),
            mouse_menu_seps: Vec::new(),
            last_mouse_key: None,
            wooting_submenu,
            wooting_menu_items: Vec::new(),
            wooting_menu_seps: Vec::new(),
            last_wooting_key: None,
            audio_submenu,
            audio_menu_items: Vec::new(),
            last_audio_key: Vec::new(),
            mic_mute_item,
            restore_display_item,
            auto_switch_item,
            lock_profile_item,
            last_icon_key: Some(None),
        }
    }

    /// Convenience adapter that maps a `ProfileRuntimeSnapshot` directly into `update_status`.
    pub fn sync_from_snapshot(
        &mut self,
        snap: &ProfileRuntimeSnapshot,
        show_battery_icon: bool,
        low_battery_threshold: u8,
    ) {
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
        self.update_status(
            snap.auto_switch_enabled,
            snap.profile_locked,
            snap.mic_muted == Some(true),
            snap.active_display_override.as_deref(),
            &snap.active_profile_id,
            &snap.active_profile_name,
            &snap.available_profiles,
            &snap.audio_outputs,
            primary_mouse.as_ref(),
            primary_wooting.as_ref(),
            battery_summary.as_deref(),
            show_battery_icon,
            low_battery_threshold,
        );
    }

    /// Synchronizes the tray menu status header, profile / mouse / audio submenus, dynamic
    /// battery icon, checkmarks, and hover tooltip.
    #[allow(clippy::too_many_arguments)]
    pub fn update_status(
        &mut self,
        auto_switch_enabled: bool,
        profile_locked: bool,
        mic_muted: bool,
        active_display_override: Option<&str>,
        active_profile_id: &str,
        active_profile_name: &str,
        available_profiles: &[(String, String)],
        audio_outputs: &[(String, String, bool)],
        primary_mouse: Option<&TrayMouseState>,
        primary_wooting: Option<&TrayWootingState>,
        battery_summary: Option<&str>,
        show_battery_icon: bool,
        low_battery_threshold: u8,
    ) {
        self.auto_switch_item.set_checked(auto_switch_enabled);
        self.lock_profile_item.set_checked(profile_locked);
        self.mic_mute_item.set_checked(mic_muted);
        match active_display_override {
            Some(mode_lbl) => {
                self.restore_display_item
                    .set_text(format!("Restore Native Display ({mode_lbl})"));
                self.restore_display_item.set_enabled(true);
            }
            None => {
                self.restore_display_item.set_text("Display: Native");
                self.restore_display_item.set_enabled(false);
            }
        }

        let lock_tag = if profile_locked { " [LOCKED]" } else { "" };
        let mic_tag = if mic_muted { " [MIC MUTED]" } else { "" };
        let header_text = match battery_summary.filter(|s| !s.is_empty()) {
            Some(batt) => format!("{active_profile_name}{lock_tag}{mic_tag} · {batt}"),
            None => format!("Profile: {active_profile_name}{lock_tag}{mic_tag}"),
        };
        self.status_item.set_text(header_text);

        // 1. Dynamic live battery percentage tray icon (only redraws when state changes).
        let desired_icon_key = if show_battery_icon {
            primary_mouse.and_then(|m| {
                m.battery
                    .map(|b| (b.percent.min(100), b.charging, low_battery_threshold))
            })
        } else {
            None
        };
        if self.last_icon_key != Some(desired_icon_key) {
            if let Some(tray) = &self.tray {
                let icon = match desired_icon_key {
                    Some((pct, chg, low)) => tray_battery_icon_image(pct, chg, low),
                    None => tray_icon_image(),
                };
                if let Some(img) = icon {
                    let _ = tray.set_icon(Some(img));
                }
            }
            self.last_icon_key = Some(desired_icon_key);
        }

        // 2. Profile switcher submenu.
        let next_key = (active_profile_id.to_string(), available_profiles.to_vec());
        if next_key != self.last_profile_key {
            for old in self.profile_menu_items.drain(..) {
                let _ = self.profiles_submenu.remove(&old);
            }
            for (id, name) in available_profiles {
                let label = if id == active_profile_id {
                    format!("● {name}")
                } else {
                    format!("   {name}")
                };
                let item = MenuItem::with_id(format!("{ID_PROFILE_PREFIX}{id}"), label, true, None);
                let _ = self.profiles_submenu.append(&item);
                self.profile_menu_items.push(item);
            }
            self.last_profile_key = next_key;
        }

        // 3. Mouse Quick Controls submenu (live Polling Rate + DPI presets + Refresh).
        let next_mouse_key = primary_mouse.cloned();
        if self.last_mouse_key.as_ref() != Some(&next_mouse_key) {
            for old in self.mouse_menu_items.drain(..) {
                let _ = self.mouse_submenu.remove(&old);
            }
            for sep in self.mouse_menu_seps.drain(..) {
                let _ = self.mouse_submenu.remove(&sep);
            }

            if let Some(mouse) = primary_mouse {
                let mut header_parts = vec![mouse.name.clone()];
                if let Some(dpi) = mouse.current_dpi {
                    header_parts.push(format!("{dpi} DPI"));
                }
                if let Some(hz) = mouse.current_polling_hz {
                    header_parts.push(format!("{hz} Hz"));
                }
                if let Some(batt) = mouse.battery {
                    let lbl = match mouse.battery_mins_left.filter(|&m| m > 0) {
                        Some(mins) => format!(
                            "{} ({})",
                            batt.compact_label(),
                            format_battery_hours_left(mins as f32 / 60.0)
                        ),
                        None => batt.compact_label(),
                    };
                    header_parts.push(lbl);
                }
                let header =
                    MenuItem::with_id("mouse_header", header_parts.join(" · "), false, None);
                let _ = self.mouse_submenu.append(&header);
                self.mouse_menu_items.push(header);

                if !mouse.polling_rates_hz.is_empty() {
                    let sep = PredefinedMenuItem::separator();
                    let _ = self.mouse_submenu.append(&sep);
                    self.mouse_menu_seps.push(sep);

                    for &hz in &mouse.polling_rates_hz {
                        let active = mouse.current_polling_hz == Some(hz);
                        let label = if active {
                            format!("● Polling: {hz} Hz")
                        } else {
                            format!("   Polling: {hz} Hz")
                        };
                        let item = MenuItem::with_id(
                            format!("{ID_MOUSE_POLL_PREFIX}{hz}"),
                            label,
                            true,
                            None,
                        );
                        let _ = self.mouse_submenu.append(&item);
                        self.mouse_menu_items.push(item);
                    }
                }

                let mut dpi_presets: Vec<u16> = [400u16, 800, 1600, 3200]
                    .into_iter()
                    .filter(|&d| d >= mouse.dpi_min && d <= mouse.dpi_max)
                    .collect();
                if let Some(cur) = mouse.current_dpi {
                    if cur >= mouse.dpi_min && cur <= mouse.dpi_max && !dpi_presets.contains(&cur) {
                        dpi_presets.push(cur);
                        dpi_presets.sort_unstable();
                    }
                }
                if !dpi_presets.is_empty() {
                    let sep = PredefinedMenuItem::separator();
                    let _ = self.mouse_submenu.append(&sep);
                    self.mouse_menu_seps.push(sep);

                    for dpi in dpi_presets {
                        let active = mouse.current_dpi == Some(dpi);
                        let label = if active {
                            format!("● DPI: {dpi}")
                        } else {
                            format!("   DPI: {dpi}")
                        };
                        let item = MenuItem::with_id(
                            format!("{ID_MOUSE_DPI_PREFIX}{dpi}"),
                            label,
                            true,
                            None,
                        );
                        let _ = self.mouse_submenu.append(&item);
                        self.mouse_menu_items.push(item);
                    }
                }

                let sep_end = PredefinedMenuItem::separator();
                let _ = self.mouse_submenu.append(&sep_end);
                self.mouse_menu_seps.push(sep_end);
            } else {
                let empty =
                    MenuItem::with_id("mouse_empty", "No supported mouse connected", false, None);
                let _ = self.mouse_submenu.append(&empty);
                self.mouse_menu_items.push(empty);
            }

            let refresh =
                MenuItem::with_id(ID_MOUSE_REFRESH, "Refresh Mouse & Battery", true, None);
            let _ = self.mouse_submenu.append(&refresh);
            self.mouse_menu_items.push(refresh);

            self.last_mouse_key = Some(next_mouse_key);
        }

        // 3b. Keyboard quick-switch submenu (real onboard slot names).
        let next_wooting_key = primary_wooting.cloned();
        if self.last_wooting_key.as_ref() != Some(&next_wooting_key) {
            for old in self.wooting_menu_items.drain(..) {
                let _ = self.wooting_submenu.remove(&old);
            }
            for sep in self.wooting_menu_seps.drain(..) {
                let _ = self.wooting_submenu.remove(&sep);
            }

            if let Some(kb) = primary_wooting {
                let header = MenuItem::with_id("wooting_header", &kb.name, false, None);
                let _ = self.wooting_submenu.append(&header);
                self.wooting_menu_items.push(header);

                if !kb.profiles.is_empty() {
                    let sep = PredefinedMenuItem::separator();
                    let _ = self.wooting_submenu.append(&sep);
                    self.wooting_menu_seps.push(sep);

                    for (slot, slot_name) in &kb.profiles {
                        let active = kb.active_profile == Some(*slot);
                        let label = if active {
                            format!("● P{} · {slot_name}", slot + 1)
                        } else {
                            format!("   P{} · {slot_name}", slot + 1)
                        };
                        let item = MenuItem::with_id(
                            format!("{ID_WOOTING_PREFIX}{slot}"),
                            label,
                            true,
                            None,
                        );
                        let _ = self.wooting_submenu.append(&item);
                        self.wooting_menu_items.push(item);
                    }
                }

                let sep_end = PredefinedMenuItem::separator();
                let _ = self.wooting_submenu.append(&sep_end);
                self.wooting_menu_seps.push(sep_end);
            } else {
                let empty = MenuItem::with_id(
                    "wooting_empty",
                    "No supported keyboard connected",
                    false,
                    None,
                );
                let _ = self.wooting_submenu.append(&empty);
                self.wooting_menu_items.push(empty);
            }

            let refresh = MenuItem::with_id(ID_WOOTING_REFRESH, "Refresh Keyboards", true, None);
            let _ = self.wooting_submenu.append(&refresh);
            self.wooting_menu_items.push(refresh);

            self.last_wooting_key = Some(next_wooting_key);
        }

        // 4. Audio output switcher submenu.
        if audio_outputs != self.last_audio_key.as_slice() {
            for old in self.audio_menu_items.drain(..) {
                let _ = self.audio_submenu.remove(&old);
            }
            if audio_outputs.is_empty() {
                let placeholder =
                    MenuItem::with_id("audio_empty", "No active playback devices", false, None);
                let _ = self.audio_submenu.append(&placeholder);
                self.audio_menu_items.push(placeholder);
            } else {
                for (id, name, is_default) in audio_outputs {
                    let label = if *is_default {
                        format!("● {name}")
                    } else {
                        format!("   {name}")
                    };
                    let item =
                        MenuItem::with_id(format!("{ID_AUDIO_PREFIX}{id}"), label, true, None);
                    let _ = self.audio_submenu.append(&item);
                    self.audio_menu_items.push(item);
                }
            }
            self.last_audio_key = audio_outputs.to_vec();
        }

        // 5. Rich hover tooltip (`Polled · Mouse · 68% · 4000 Hz · Profile`).
        if let Some(tray) = &self.tray {
            let mut parts: Vec<String> = vec!["Polled".to_string()];
            if let Some(mouse) = primary_mouse.filter(|m| !m.is_simulator) {
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
            } else if let Some(batt) = battery_summary.filter(|s| !s.is_empty()) {
                parts.push(batt.to_string());
            }

            let profile_part = if profile_locked {
                format!("{active_profile_name} [LOCKED]")
            } else if auto_switch_enabled {
                active_profile_name.to_string()
            } else {
                format!("{active_profile_name} (Auto OFF)")
            };
            parts.push(profile_part);
            if mic_muted {
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
