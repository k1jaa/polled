// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Custom pure-Win32 monochrome tray popup menu styled directly in Polled's Dark / Light theme.
//!
//! - Zero OpenGL / zero GPU memory / zero extra crates / 0% idle CPU when closed.
//! - Renders anti-aliased rounded cards, soft outer shadow, modern pill toggle switches,
//!   active status dots, and crisp `Segoe UI` typography via `UpdateLayeredWindow` (`ULW_ALPHA`).
//! - Supports hover-triggered submenus (`Switch Profile`, `Mouse Quick Controls`, `Keyboard`,
//!   `Audio Output`), monitor work-area clamping, `Escape` & click-outside auto-dismiss, and
//!   DPI-native scaling alongside Explorer's taskbar.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::profile_runtime::format_battery_hours_left;
use crate::tray::{TrayAction, TrayMouseState, TrayWootingState};

/// Complete data snapshot used to render Polled's custom tray menu and its submenus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMenuModel {
    pub dark_mode: bool,
    pub auto_switch_enabled: bool,
    pub profile_locked: bool,
    pub mic_muted: bool,
    pub taskbar_widget_enabled: bool,
    pub active_display_override: Option<String>,
    pub active_profile_id: String,
    pub active_profile_name: String,
    pub available_profiles: Vec<(String, String)>,
    pub audio_outputs: Vec<(String, String, bool)>,
    pub primary_mouse: Option<TrayMouseState>,
    pub primary_wooting: Option<TrayWootingState>,
    pub available_update_version: Option<String>,
}

impl Default for TrayMenuModel {
    fn default() -> Self {
        Self {
            dark_mode: true,
            auto_switch_enabled: false,
            profile_locked: false,
            mic_muted: false,
            taskbar_widget_enabled: false,
            active_display_override: None,
            active_profile_id: String::new(),
            active_profile_name: "Desktop (Default)".to_string(),
            available_profiles: Vec::new(),
            audio_outputs: Vec::new(),
            primary_mouse: None,
            primary_wooting: None,
            available_update_version: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubmenuKind {
    Profiles,
    Mouse,
    Keyboard,
    Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccentTone {
    Neutral,
    Ok,
    Info,
    Warn,
    Bad,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MenuEntry {
    HeaderCard {
        title: String,
        subtitle: String,
        badge: Option<(String, AccentTone)>,
    },
    SectionHeader(String),
    Separator,
    Action {
        label: String,
        right_hint: Option<String>,
        action: TrayAction,
        active_dot: bool,
        tone: AccentTone,
    },
    Toggle {
        label: String,
        checked: bool,
        on_tone: AccentTone,
        action: TrayAction,
    },
    Submenu {
        label: String,
        right_hint: Option<String>,
        kind: SubmenuKind,
    },
    Disabled(String),
}

/// Exact Dark / Light RGB palette mirroring `theme.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MenuPalette {
    pub surface: (u8, u8, u8),
    pub header_bg: (u8, u8, u8),
    pub hover_bg: (u8, u8, u8),
    pub border: (u8, u8, u8),
    pub separator: (u8, u8, u8),
    pub text: (u8, u8, u8),
    pub text_muted: (u8, u8, u8),
    pub ok: (u8, u8, u8),
    pub info: (u8, u8, u8),
    pub warn: (u8, u8, u8),
    pub bad: (u8, u8, u8),
    pub toggle_off_track: (u8, u8, u8),
    pub toggle_off_thumb: (u8, u8, u8),
    pub toggle_on_thumb: (u8, u8, u8),
}

impl MenuPalette {
    pub fn for_mode(dark_mode: bool) -> Self {
        if dark_mode {
            Self {
                surface: (15, 15, 18),
                header_bg: (22, 22, 27),
                hover_bg: (32, 32, 39),
                border: (44, 44, 52),
                separator: (30, 30, 36),
                text: (245, 245, 247),
                text_muted: (148, 148, 158),
                ok: (34, 197, 94),
                info: (56, 189, 248),
                warn: (245, 158, 11),
                bad: (239, 68, 68),
                toggle_off_track: (46, 46, 54),
                toggle_off_thumb: (170, 170, 180),
                toggle_on_thumb: (255, 255, 255),
            }
        } else {
            Self {
                surface: (255, 255, 255),
                header_bg: (244, 244, 248),
                hover_bg: (236, 236, 242),
                border: (216, 216, 224),
                separator: (232, 232, 238),
                text: (12, 12, 14),
                text_muted: (92, 92, 102),
                ok: (21, 128, 61),
                info: (3, 105, 161),
                warn: (180, 83, 9),
                bad: (185, 28, 28),
                toggle_off_track: (206, 206, 214),
                toggle_off_thumb: (255, 255, 255),
                toggle_on_thumb: (255, 255, 255),
            }
        }
    }

    pub fn tone_rgb(self, tone: AccentTone) -> (u8, u8, u8) {
        match tone {
            AccentTone::Neutral => self.text,
            AccentTone::Ok => self.ok,
            AccentTone::Info => self.info,
            AccentTone::Warn => self.warn,
            AccentTone::Bad => self.bad,
        }
    }
}

/// Builds the top-level menu rows from the current `TrayMenuModel`.
pub(crate) fn build_main_entries(m: &TrayMenuModel) -> Vec<MenuEntry> {
    let badge = if m.mic_muted {
        Some(("MIC MUTED".to_string(), AccentTone::Bad))
    } else if m.profile_locked {
        Some(("LOCKED".to_string(), AccentTone::Warn))
    } else if m.auto_switch_enabled {
        Some(("AUTO".to_string(), AccentTone::Ok))
    } else {
        None
    };

    let subtitle = match &m.primary_mouse {
        Some(mouse) => {
            let mut parts = vec![mouse.name.clone()];
            if let Some(batt) = mouse.battery {
                parts.push(batt.compact_label());
            }
            if let Some(hz) = mouse.current_polling_hz {
                parts.push(format!("{hz} Hz"));
            }
            parts.join(" · ")
        }
        None => "No supported mouse connected".to_string(),
    };

    let mouse_hint = m.primary_mouse.as_ref().and_then(|mouse| {
        let mut parts = Vec::new();
        if let Some(dpi) = mouse.current_dpi {
            parts.push(format!("{dpi}"));
        }
        if let Some(hz) = mouse.current_polling_hz {
            if hz >= 1000 && hz.is_multiple_of(1000) {
                parts.push(format!("{}K", hz / 1000));
            } else {
                parts.push(format!("{hz}Hz"));
            }
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    });

    let kb_hint = m
        .primary_wooting
        .as_ref()
        .and_then(|kb| kb.active_profile.map(|p| format!("P{}", p + 1)));

    let mut entries = vec![
        MenuEntry::HeaderCard {
            title: m.active_profile_name.clone(),
            subtitle,
            badge,
        },
        MenuEntry::Action {
            label: "Open Polled".to_string(),
            right_hint: None,
            action: TrayAction::Open,
            active_dot: false,
            tone: AccentTone::Neutral,
        },
        MenuEntry::Action {
            label: "Scan Now".to_string(),
            right_hint: None,
            action: TrayAction::ScanNow,
            active_dot: false,
            tone: AccentTone::Neutral,
        },
        MenuEntry::Separator,
        MenuEntry::Submenu {
            label: "Switch Profile".to_string(),
            right_hint: Some(m.available_profiles.len().to_string()),
            kind: SubmenuKind::Profiles,
        },
        MenuEntry::Submenu {
            label: "Mouse Quick Controls".to_string(),
            right_hint: mouse_hint,
            kind: SubmenuKind::Mouse,
        },
        MenuEntry::Submenu {
            label: "Keyboard".to_string(),
            right_hint: kb_hint,
            kind: SubmenuKind::Keyboard,
        },
        MenuEntry::Submenu {
            label: "Audio Output".to_string(),
            right_hint: None,
            kind: SubmenuKind::Audio,
        },
        MenuEntry::Separator,
        MenuEntry::Toggle {
            label: "Mute Microphone".to_string(),
            checked: m.mic_muted,
            on_tone: AccentTone::Bad,
            action: TrayAction::ToggleMicMute,
        },
    ];

    if let Some(mode_lbl) = &m.active_display_override {
        entries.push(MenuEntry::Action {
            label: format!("Restore Display ({mode_lbl})"),
            right_hint: None,
            action: TrayAction::RestoreNativeDisplay,
            active_dot: false,
            tone: AccentTone::Warn,
        });
    }

    entries.extend([
        MenuEntry::Toggle {
            label: "Lock Current Profile".to_string(),
            checked: m.profile_locked,
            on_tone: AccentTone::Warn,
            action: TrayAction::ToggleProfileLock,
        },
        MenuEntry::Toggle {
            label: "Auto-Switch Profiles".to_string(),
            checked: m.auto_switch_enabled,
            on_tone: AccentTone::Ok,
            action: TrayAction::ToggleAutoSwitch,
        },
        MenuEntry::Toggle {
            label: "Taskbar Battery Widget".to_string(),
            checked: m.taskbar_widget_enabled,
            on_tone: AccentTone::Ok,
            action: TrayAction::ToggleTaskbarWidget,
        },
        MenuEntry::Separator,
    ]);

    match &m.available_update_version {
        Some(ver) if !ver.trim().is_empty() => {
            entries.push(MenuEntry::Action {
                label: format!("Update Available (v{ver})"),
                right_hint: Some("Install".to_string()),
                action: TrayAction::CheckUpdates,
                active_dot: true,
                tone: AccentTone::Info,
            });
        }
        _ => {
            entries.push(MenuEntry::Action {
                label: "Check for Updates".to_string(),
                right_hint: None,
                action: TrayAction::CheckUpdates,
                active_dot: false,
                tone: AccentTone::Neutral,
            });
        }
    }

    entries.push(MenuEntry::Action {
        label: "Quit Polled".to_string(),
        right_hint: None,
        action: TrayAction::Quit,
        active_dot: false,
        tone: AccentTone::Bad,
    });

    entries
}

/// Builds the rows for an expanded submenu card.
pub(crate) fn build_submenu_entries(m: &TrayMenuModel, kind: SubmenuKind) -> Vec<MenuEntry> {
    match kind {
        SubmenuKind::Profiles => {
            if m.available_profiles.is_empty() {
                return vec![MenuEntry::Disabled("No profiles configured".to_string())];
            }
            m.available_profiles
                .iter()
                .map(|(id, name)| MenuEntry::Action {
                    label: name.clone(),
                    right_hint: None,
                    action: TrayAction::ApplyProfile(id.clone()),
                    active_dot: id == &m.active_profile_id,
                    tone: AccentTone::Neutral,
                })
                .collect()
        }
        SubmenuKind::Mouse => {
            let mut entries = Vec::new();
            if let Some(mouse) = &m.primary_mouse {
                let mut hdr = vec![mouse.name.clone()];
                if let Some(batt) = mouse.battery {
                    let lbl = match mouse.battery_mins_left.filter(|&mins| mins > 0) {
                        Some(mins) => format!(
                            "{} ({})",
                            batt.compact_label(),
                            format_battery_hours_left(mins as f32 / 60.0)
                        ),
                        None => batt.compact_label(),
                    };
                    hdr.push(lbl);
                }
                entries.push(MenuEntry::SectionHeader(hdr.join(" · ")));

                if !mouse.polling_rates_hz.is_empty() {
                    entries.push(MenuEntry::Separator);
                    for &hz in &mouse.polling_rates_hz {
                        entries.push(MenuEntry::Action {
                            label: format!("Polling: {hz} Hz"),
                            right_hint: None,
                            action: TrayAction::QuickSetMousePolling(hz),
                            active_dot: mouse.current_polling_hz == Some(hz),
                            tone: AccentTone::Neutral,
                        });
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
                    entries.push(MenuEntry::Separator);
                    for dpi in dpi_presets {
                        entries.push(MenuEntry::Action {
                            label: format!("DPI: {dpi}"),
                            right_hint: None,
                            action: TrayAction::QuickSetMouseDpi(dpi),
                            active_dot: mouse.current_dpi == Some(dpi),
                            tone: AccentTone::Neutral,
                        });
                    }
                }
                entries.push(MenuEntry::Separator);
            } else {
                entries.push(MenuEntry::Disabled(
                    "No supported mouse connected".to_string(),
                ));
                entries.push(MenuEntry::Separator);
            }
            entries.push(MenuEntry::Action {
                label: "Refresh Mouse & Battery".to_string(),
                right_hint: None,
                action: TrayAction::RefreshMouseAndBattery,
                active_dot: false,
                tone: AccentTone::Neutral,
            });
            entries
        }
        SubmenuKind::Keyboard => {
            let mut entries = Vec::new();
            if let Some(kb) = &m.primary_wooting {
                entries.push(MenuEntry::SectionHeader(kb.name.clone()));
                if !kb.profiles.is_empty() {
                    entries.push(MenuEntry::Separator);
                    for (slot, name) in &kb.profiles {
                        entries.push(MenuEntry::Action {
                            label: format!("P{} · {name}", slot + 1),
                            right_hint: None,
                            action: TrayAction::SwitchKeyboardProfile(*slot),
                            active_dot: kb.active_profile == Some(*slot),
                            tone: AccentTone::Neutral,
                        });
                    }
                }
                entries.push(MenuEntry::Separator);
            } else {
                entries.push(MenuEntry::Disabled(
                    "No supported keyboard connected".to_string(),
                ));
                entries.push(MenuEntry::Separator);
            }
            entries.push(MenuEntry::Action {
                label: "Refresh Keyboards".to_string(),
                right_hint: None,
                action: TrayAction::RefreshKeyboards,
                active_dot: false,
                tone: AccentTone::Neutral,
            });
            entries
        }
        SubmenuKind::Audio => {
            if m.audio_outputs.is_empty() {
                return vec![MenuEntry::Disabled(
                    "No active playback devices".to_string(),
                )];
            }
            m.audio_outputs
                .iter()
                .map(|(id, name, is_default)| MenuEntry::Action {
                    label: name.clone(),
                    right_hint: None,
                    action: TrayAction::SwitchAudioOutput(id.clone()),
                    active_dot: *is_default,
                    tone: AccentTone::Neutral,
                })
                .collect()
        }
    }
}

/// Scaled vertical metrics for a rendered menu card.
#[derive(Debug, Clone)]
pub(crate) struct MenuLayout {
    pub scale: f32,
    pub shadow: i32,
    pub pad_y: i32,
    pub card_w: i32,
    pub card_h: i32,
    pub total_w: i32,
    pub total_h: i32,
    /// `(top_y, bottom_y)` in window coordinates (including shadow offset) for each entry.
    pub row_rects: Vec<(i32, i32)>,
}

impl MenuLayout {
    pub fn compute(entries: &[MenuEntry], base_card_w: f32, scale: f32) -> Self {
        let shadow = (8.0 * scale).round() as i32;
        let pad_y = (6.0 * scale).round() as i32;
        let card_w = (base_card_w * scale).round() as i32;

        let mut y = shadow + pad_y;
        let mut row_rects = Vec::with_capacity(entries.len());
        for entry in entries {
            let h = match entry {
                MenuEntry::HeaderCard { .. } => (48.0 * scale).round() as i32,
                MenuEntry::SectionHeader(_) => (24.0 * scale).round() as i32,
                MenuEntry::Separator => (9.0 * scale).round() as i32,
                MenuEntry::Action { .. }
                | MenuEntry::Toggle { .. }
                | MenuEntry::Submenu { .. }
                | MenuEntry::Disabled(_) => (30.0 * scale).round() as i32,
            };
            row_rects.push((y, y + h));
            y += h;
        }
        let card_h = (y + pad_y) - shadow;
        Self {
            scale,
            shadow,
            pad_y,
            card_w,
            card_h,
            total_w: card_w + shadow * 2,
            total_h: card_h + shadow * 2,
            row_rects,
        }
    }

    /// Hit-tests window-local `(x, y)` and returns the hovered interactive entry index.
    pub fn hit_test(&self, entries: &[MenuEntry], x: i32, y: i32) -> Option<usize> {
        let card_left = self.shadow;
        let card_right = self.shadow + self.card_w;
        if x < card_left || x >= card_right {
            return None;
        }
        for (idx, &(top, bottom)) in self.row_rects.iter().enumerate() {
            if y >= top && y < bottom {
                return match &entries[idx] {
                    MenuEntry::Action { .. }
                    | MenuEntry::Toggle { .. }
                    | MenuEntry::Submenu { .. } => Some(idx),
                    _ => None,
                };
            }
        }
        None
    }
}

/// Computes the clamped window `(x, y)` for the main tray popup anchored near `(cursor_x, cursor_y)`
/// inside monitor work area `(work_left, work_top, work_right, work_bottom)`.
pub(crate) fn position_main_popup(
    cursor_x: i32,
    cursor_y: i32,
    layout: &MenuLayout,
    work: (i32, i32, i32, i32),
) -> (i32, i32) {
    let (wl, wt, wr, wb) = work;
    let margin = (6.0 * layout.scale).round() as i32;

    // Anchor card's right edge near cursor (or right edge of screen) and bottom just above cursor/taskbar.
    let mut card_x = cursor_x - layout.card_w + (24.0 * layout.scale).round() as i32;
    if card_x + layout.card_w > wr - margin {
        card_x = wr - margin - layout.card_w;
    }
    if card_x < wl + margin {
        card_x = wl + margin;
    }

    let mut card_y = cursor_y - layout.card_h - (8.0 * layout.scale).round() as i32;
    if card_y < wt + margin {
        // Taskbar at top of screen: open downwards below cursor.
        card_y = cursor_y + (8.0 * layout.scale).round() as i32;
    }
    if card_y + layout.card_h > wb - margin {
        card_y = (wb - margin - layout.card_h).max(wt + margin);
    }

    (card_x - layout.shadow, card_y - layout.shadow)
}

/// Computes the clamped window `(x, y)` for a submenu popping beside `main_card_rect` aligned with
/// `anchor_row_screen_y`.
pub(crate) fn position_submenu_popup(
    main_card_left: i32,
    main_card_right: i32,
    anchor_row_screen_y: i32,
    sub_layout: &MenuLayout,
    work: (i32, i32, i32, i32),
) -> (i32, i32) {
    let (wl, wt, wr, wb) = work;
    let gap = (4.0 * sub_layout.scale).round() as i32;
    let margin = (6.0 * sub_layout.scale).round() as i32;

    // Prefer opening to the left (natural for right-aligned Windows system tray), falling back to right.
    let left_x = main_card_left - sub_layout.card_w - gap;
    let right_x = main_card_right + gap;
    let card_x = if left_x >= wl + margin {
        left_x
    } else if right_x + sub_layout.card_w <= wr - margin {
        right_x
    } else {
        left_x.max(wl + margin)
    };

    let mut card_y = anchor_row_screen_y - sub_layout.pad_y;
    if card_y + sub_layout.card_h > wb - margin {
        card_y = wb - margin - sub_layout.card_h;
    }
    if card_y < wt + margin {
        card_y = wt + margin;
    }

    (card_x - sub_layout.shadow, card_y - sub_layout.shadow)
}

// --- Pure-Rust SDF Premultiplied BGRA Drawing Primitives ---

#[inline]
fn blend_premul(dst: &mut [u8], off: usize, r: u8, g: u8, b: u8, alpha: f32) {
    let a = alpha.clamp(0.0, 1.0);
    if a <= 0.001 || off + 3 >= dst.len() {
        return;
    }
    let inv = 1.0 - a;
    let ob = (f32::from(b) * a + f32::from(dst[off]) * inv).clamp(0.0, 255.0);
    let og = (f32::from(g) * a + f32::from(dst[off + 1]) * inv).clamp(0.0, 255.0);
    let or = (f32::from(r) * a + f32::from(dst[off + 2]) * inv).clamp(0.0, 255.0);
    let oa = (255.0 * a + f32::from(dst[off + 3]) * inv).clamp(0.0, 255.0);
    dst[off] = ob.round() as u8;
    dst[off + 1] = og.round() as u8;
    dst[off + 2] = or.round() as u8;
    dst[off + 3] = oa.round() as u8;
}

#[inline]
fn sd_rounded_rect(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let cx = (x0 + x1) * 0.5;
    let cy = (y0 + y1) * 0.5;
    let hx = ((x1 - x0) * 0.5).max(0.5);
    let hy = ((y1 - y0) * 0.5).max(0.5);
    let rr = r.min(hx).min(hy);
    let qx = (px - cx).abs() - (hx - rr);
    let qy = (py - cy).abs() - (hy - rr);
    let out = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let inn = qx.max(qy).min(0.0);
    out + inn - rr
}

#[inline]
fn sd_seg(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let pax = px - ax;
    let pay = py - ay;
    let bax = bx - ax;
    let bay = by - ay;
    let denom = bax * bax + bay * bay;
    let h = if denom > 1e-6 {
        ((pax * bax + pay * bay) / denom).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let dx = pax - bax * h;
    let dy = pay - bay * h;
    (dx * dx + dy * dy).sqrt()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_rounded_card_with_shadow(
    pixels: &mut [u8],
    w: usize,
    h: usize,
    shadow: i32,
    radius: f32,
    fill: (u8, u8, u8),
    border: (u8, u8, u8),
    dark_mode: bool,
) {
    let x0 = shadow as f32;
    let y0 = shadow as f32;
    let x1 = (w as i32 - shadow) as f32;
    let y1 = (h as i32 - shadow) as f32;
    let sh_f = (shadow as f32).max(1.0);
    let shadow_peak = if dark_mode { 0.42_f32 } else { 0.16_f32 };

    for y in 0..h {
        let py = y as f32 + 0.5;
        for x in 0..w {
            let px = x as f32 + 0.5;
            let d = sd_rounded_rect(px, py, x0, y0, x1, y1, radius);
            let off = (y * w + x) * 4;
            if d > 0.5 {
                // Soft outer drop shadow slightly offset downwards by ~1.5px.
                let ds = sd_rounded_rect(px, py - 1.5, x0, y0, x1, y1, radius);
                if ds > 0.0 && ds < sh_f {
                    let t = 1.0 - (ds / sh_f);
                    let a = shadow_peak * t * t;
                    blend_premul(pixels, off, 0, 0, 0, a);
                }
            } else {
                let cov = (0.5 - d).clamp(0.0, 1.0);
                // 1px inner crisp border along the card perimeter.
                let is_border = d > -1.25;
                let col = if is_border { border } else { fill };
                blend_premul(pixels, off, col.0, col.1, col.2, cov);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn fill_rounded_box(
    pixels: &mut [u8],
    w: usize,
    h: usize,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    radius: f32,
    fill: (u8, u8, u8),
    alpha: f32,
    stroke: Option<(u8, u8, u8)>,
) {
    let min_x = (x0.floor() as i32 - 1).max(0) as usize;
    let max_x = ((x1.ceil() as i32 + 1) as usize).min(w);
    let min_y = (y0.floor() as i32 - 1).max(0) as usize;
    let max_y = ((y1.ceil() as i32 + 1) as usize).min(h);
    for y in min_y..max_y {
        let py = y as f32 + 0.5;
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let d = sd_rounded_rect(px, py, x0, y0, x1, y1, radius);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.001 {
                let off = (y * w + x) * 4;
                let col = match stroke {
                    Some(sc) if d > -1.1 => sc,
                    _ => fill,
                };
                blend_premul(pixels, off, col.0, col.1, col.2, cov * alpha);
            }
        }
    }
}

pub(crate) fn draw_dot(
    pixels: &mut [u8],
    w: usize,
    h: usize,
    cx: f32,
    cy: f32,
    radius: f32,
    rgb: (u8, u8, u8),
) {
    let min_x = ((cx - radius - 1.5).floor().max(0.0)) as usize;
    let max_x = ((cx + radius + 1.5).ceil() as usize).min(w);
    let min_y = ((cy - radius - 1.5).floor().max(0.0)) as usize;
    let max_y = ((cy + radius + 1.5).ceil() as usize).min(h);
    for y in min_y..max_y {
        let dy = (y as f32 + 0.5) - cy;
        for x in min_x..max_x {
            let dx = (x as f32 + 0.5) - cx;
            let d = dx.hypot(dy) - radius;
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.001 {
                blend_premul(pixels, (y * w + x) * 4, rgb.0, rgb.1, rgb.2, cov);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_chevron_right(
    pixels: &mut [u8],
    w: usize,
    h: usize,
    cx: f32,
    cy: f32,
    size: f32,
    stroke_half: f32,
    rgb: (u8, u8, u8),
) {
    let hw = size * 0.30;
    let hh = size * 0.52;
    let min_x = ((cx - hw - stroke_half - 2.0).floor().max(0.0)) as usize;
    let max_x = ((cx + hw + stroke_half + 2.0).ceil() as usize).min(w);
    let min_y = ((cy - hh - stroke_half - 2.0).floor().max(0.0)) as usize;
    let max_y = ((cy + hh + stroke_half + 2.0).ceil() as usize).min(h);
    for y in min_y..max_y {
        let py = y as f32 + 0.5;
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let d1 = sd_seg(px, py, cx - hw, cy - hh, cx + hw, cy);
            let d2 = sd_seg(px, py, cx + hw, cy, cx - hw, cy + hh);
            let d = d1.min(d2) - stroke_half;
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.001 {
                blend_premul(pixels, (y * w + x) * 4, rgb.0, rgb.1, rgb.2, cov);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_toggle_pill(
    pixels: &mut [u8],
    w: usize,
    h: usize,
    right_x: f32,
    cy: f32,
    scale: f32,
    checked: bool,
    on_rgb: (u8, u8, u8),
    pal: MenuPalette,
) {
    let pill_w = 26.0 * scale;
    let pill_h = 14.0 * scale;
    let x0 = right_x - pill_w;
    let y0 = cy - pill_h * 0.5;
    let x1 = right_x;
    let y1 = cy + pill_h * 0.5;
    let track_col = if checked {
        on_rgb
    } else {
        pal.toggle_off_track
    };
    fill_rounded_box(
        pixels,
        w,
        h,
        x0,
        y0,
        x1,
        y1,
        pill_h * 0.5,
        track_col,
        1.0,
        None,
    );
    let thumb_r = (pill_h * 0.36).max(3.5);
    let thumb_cx = if checked {
        x1 - pill_h * 0.5
    } else {
        x0 + pill_h * 0.5
    };
    let thumb_col = if checked {
        pal.toggle_on_thumb
    } else {
        pal.toggle_off_thumb
    };
    draw_dot(pixels, w, h, thumb_cx, cy, thumb_r, thumb_col);
}

/// Thread-safe handle to Polled's custom pure-Win32 dark/light tray context menu.
pub struct TrayMenuController {
    model: Arc<Mutex<TrayMenuModel>>,
    hwnd: Arc<AtomicIsize>,
    handle: Option<JoinHandle<()>>,
}

impl TrayMenuController {
    pub fn start(
        initial_model: TrayMenuModel,
        on_action: Arc<dyn Fn(TrayAction) + Send + Sync>,
    ) -> Self {
        let model = Arc::new(Mutex::new(initial_model));
        let hwnd = Arc::new(AtomicIsize::new(0));

        let model_bg = Arc::clone(&model);
        let hwnd_bg = Arc::clone(&hwnd);

        let handle = std::thread::Builder::new()
            .name("polled-tray-menu".to_string())
            .spawn(move || {
                #[cfg(windows)]
                run_tray_menu_loop(model_bg, hwnd_bg, on_action);
                #[cfg(not(windows))]
                let _ = (model_bg, hwnd_bg, on_action);
            })
            .ok();

        Self {
            model,
            hwnd,
            handle,
        }
    }

    pub fn update_model(&self, next: TrayMenuModel) {
        if let Ok(mut guard) = self.model.lock() {
            *guard = next;
        }
    }

    /// Opens the custom Polled-themed tray popup menu anchored at the current cursor position.
    pub fn open_at_cursor(&self) {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{
                AllowSetForegroundWindow, PostMessageW, WM_APP,
            };

            let raw = self.hwnd.load(Ordering::SeqCst);
            if raw != 0 {
                let h = HWND(raw as *mut core::ffi::c_void);
                // SAFETY: Allows our own tray-menu thread to take foreground focus so clicking
                // anywhere outside the popup triggers `WM_ACTIVATE(WA_INACTIVE)` and dismisses it.
                unsafe {
                    let _ = AllowSetForegroundWindow(std::process::id());
                    let _ = PostMessageW(Some(h), WM_APP + 20, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
}

impl Drop for TrayMenuController {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

            let raw = self.hwnd.load(Ordering::SeqCst);
            if raw != 0 {
                let h = HWND(raw as *mut core::ffi::c_void);
                unsafe {
                    let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(windows)]
fn run_tray_menu_loop(
    shared_model: Arc<Mutex<TrayMenuModel>>,
    hwnd_slot: Arc<AtomicIsize>,
    on_action: Arc<dyn Fn(TrayAction) + Send + Sync>,
) {
    use std::cell::RefCell;
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW,
        GetDC, GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SelectObject, SetBkColor, SetBkMode,
        SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, BLENDFUNCTION, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS,
        DT_CALCRECT, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
        FF_SWISS, FW_NORMAL, FW_SEMIBOLD, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        OUT_DEFAULT_PRECIS, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW,
        GetCursorPos, GetMessageW, GetWindowRect, LoadCursorW, PostQuitMessage, RegisterClassW,
        SetCursor, SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage,
        UnregisterClassW, UpdateLayeredWindow, HWND_TOPMOST, IDC_ARROW, IDC_HAND, MSG,
        SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, ULW_ALPHA,
        WA_INACTIVE, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONUP,
        WM_MOUSEMOVE, WM_SETCURSOR, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
        WS_POPUP,
    };

    const WM_MENU_OPEN: u32 = WM_APP + 20;
    const WM_MOUSELEAVE: u32 = 0x02A3;
    const VK_ESCAPE_RAW: usize = 0x1B;

    fn to_wide_null(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    fn current_taskbar_scale() -> f32 {
        let shell_cls = to_wide_null("Shell_TrayWnd");
        if let Ok(tb) = unsafe { FindWindowW(PCWSTR(shell_cls.as_ptr()), PCWSTR::null()) } {
            let mut rc = RECT::default();
            if !tb.is_invalid() && unsafe { GetWindowRect(tb, &mut rc) }.is_ok() {
                let h = (rc.bottom - rc.top).min(rc.right - rc.left);
                if h >= 24 {
                    return (h as f32 / 48.0).clamp(0.85, 2.5);
                }
            }
        }
        1.0
    }

    fn work_area_at(pt: POINT) -> (i32, i32, i32, i32) {
        let hmon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
            let r = mi.rcWork;
            (r.left, r.top, r.right, r.bottom)
        } else {
            (0, 0, 1920, 1040)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text_onto_bgra(
        text_dc: windows::Win32::Graphics::Gdi::HDC,
        txt_slice: &mut [u8],
        dst_slice: &mut [u8],
        w: usize,
        h: usize,
        text: &str,
        font_px: i32,
        bold: bool,
        rect: RECT,
        right_align: bool,
        rgb: (u8, u8, u8),
    ) {
        if text.is_empty() || rect.right <= rect.left || rect.bottom <= rect.top {
            return;
        }
        let x0 = rect.left.max(0) as usize;
        let x1 = (rect.right.max(0) as usize).min(w);
        let y0 = rect.top.max(0) as usize;
        let y1 = (rect.bottom.max(0) as usize).min(h);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        // Zero only the bounding box in the scratch DIB for fast compositing.
        for y in y0..y1 {
            let row_start = (y * w + x0) * 4;
            let row_end = (y * w + x1) * 4;
            txt_slice[row_start..row_end].fill(0);
        }

        let face = to_wide_null("Segoe UI");
        let weight = if bold { FW_SEMIBOLD.0 } else { FW_NORMAL.0 } as i32;
        let font = unsafe {
            CreateFontW(
                -font_px,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
                PCWSTR(face.as_ptr()),
            )
        };
        let old_font = unsafe { SelectObject(text_dc, font.into()) };
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        let mut draw_rc = rect;
        let align = if right_align { DT_RIGHT } else { DT_LEFT };
        unsafe {
            let _ = SetBkMode(text_dc, TRANSPARENT);
            let _ = SetBkColor(text_dc, COLORREF(0));
            let _ = SetTextColor(text_dc, COLORREF(0x00FF_FFFF));
            let _ = DrawTextW(
                text_dc,
                &mut wide,
                &mut draw_rc,
                align | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            SelectObject(text_dc, old_font);
            let _ = DeleteObject(font.into());
        }

        for y in y0..y1 {
            let row_base = y * w;
            for x in x0..x1 {
                let off = (row_base + x) * 4;
                let cov = txt_slice[off + 2];
                if cov > 0 {
                    blend_premul(dst_slice, off, rgb.0, rgb.1, rgb.2, f32::from(cov) / 255.0);
                }
            }
        }
    }

    fn measure_text_width(
        text_dc: windows::Win32::Graphics::Gdi::HDC,
        text: &str,
        font_px: i32,
        bold: bool,
    ) -> i32 {
        let face = to_wide_null("Segoe UI");
        let weight = if bold { FW_SEMIBOLD.0 } else { FW_NORMAL.0 } as i32;
        let font = unsafe {
            CreateFontW(
                -font_px,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
                PCWSTR(face.as_ptr()),
            )
        };
        let old = unsafe { SelectObject(text_dc, font.into()) };
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        let mut rc = RECT::default();
        unsafe {
            let _ = DrawTextW(
                text_dc,
                &mut wide,
                &mut rc,
                DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
            );
            SelectObject(text_dc, old);
            let _ = DeleteObject(font.into());
        }
        (rc.right - rc.left).max(0)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_menu_window(
        hwnd: HWND,
        win_x: i32,
        win_y: i32,
        entries: &[MenuEntry],
        layout: &MenuLayout,
        hovered_idx: Option<usize>,
        active_submenu: Option<SubmenuKind>,
        dark_mode: bool,
    ) {
        let w = layout.total_w.max(1) as usize;
        let h = layout.total_h.max(1) as usize;
        let scale = layout.scale;
        let pal = MenuPalette::for_mode(dark_mode);

        let screen_dc = unsafe { GetDC(None) };
        if screen_dc.is_invalid() {
            return;
        }
        let mem_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
        let text_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
        if mem_dc.is_invalid() || text_dc.is_invalid() {
            unsafe {
                if !mem_dc.is_invalid() {
                    let _ = DeleteDC(mem_dc);
                }
                if !text_dc.is_invalid() {
                    let _ = DeleteDC(text_dc);
                }
                let _ = ReleaseDC(None, screen_dc);
            }
            return;
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: layout.total_w,
                biHeight: -layout.total_h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut dst_bits: *mut c_void = ptr::null_mut();
        let mut txt_bits: *mut c_void = ptr::null_mut();
        let dst_bmp = unsafe {
            CreateDIBSection(
                Some(screen_dc),
                &bmi,
                DIB_RGB_COLORS,
                &mut dst_bits,
                None,
                0,
            )
        };
        let txt_bmp = unsafe {
            CreateDIBSection(
                Some(screen_dc),
                &bmi,
                DIB_RGB_COLORS,
                &mut txt_bits,
                None,
                0,
            )
        };

        if let (Ok(dst_bmp), Ok(txt_bmp)) = (dst_bmp, txt_bmp) {
            if !dst_bits.is_null() && !txt_bits.is_null() {
                let byte_len = w * h * 4;
                let dst_slice =
                    unsafe { std::slice::from_raw_parts_mut(dst_bits as *mut u8, byte_len) };
                let txt_slice =
                    unsafe { std::slice::from_raw_parts_mut(txt_bits as *mut u8, byte_len) };
                dst_slice.fill(0);
                txt_slice.fill(0);

                let old_txt_bmp = unsafe { SelectObject(text_dc, txt_bmp.into()) };

                // 1. Rounded monochrome Polled card + soft drop shadow.
                let card_radius = (10.0 * scale).clamp(6.0, 18.0);
                draw_rounded_card_with_shadow(
                    dst_slice,
                    w,
                    h,
                    layout.shadow,
                    card_radius,
                    pal.surface,
                    pal.border,
                    dark_mode,
                );

                let card_left = layout.shadow;
                let card_right = layout.shadow + layout.card_w;
                let row_inset = (6.0 * scale).round() as i32;
                let text_pad = (12.0 * scale).round() as i32;
                let font_main = (13.0 * scale).round().clamp(11.0, 28.0) as i32;
                let font_sub = (11.0 * scale).round().clamp(9.0, 22.0) as i32;
                let font_badge = (9.5 * scale).round().clamp(8.0, 18.0) as i32;

                // 2. Render every row cleanly.
                for (idx, entry) in entries.iter().enumerate() {
                    let (top, bottom) = layout.row_rects[idx];
                    let cy = (top + bottom) as f32 * 0.5;
                    let is_hovered = hovered_idx == Some(idx);

                    match entry {
                        MenuEntry::HeaderCard {
                            title,
                            subtitle,
                            badge,
                        } => {
                            let bx0 = (card_left + row_inset) as f32;
                            let by0 = (top + (2.0 * scale).round() as i32) as f32;
                            let bx1 = (card_right - row_inset) as f32;
                            let by1 = (bottom - (4.0 * scale).round() as i32) as f32;
                            fill_rounded_box(
                                dst_slice,
                                w,
                                h,
                                bx0,
                                by0,
                                bx1,
                                by1,
                                (7.0 * scale).clamp(4.0, 12.0),
                                pal.header_bg,
                                1.0,
                                Some(pal.separator),
                            );

                            let mut title_right =
                                card_right - row_inset - (10.0 * scale).round() as i32;
                            if let Some((badge_txt, tone)) = badge {
                                let bw = measure_text_width(text_dc, badge_txt, font_badge, true)
                                    + (12.0 * scale).round() as i32;
                                let bh = (16.0 * scale).round() as i32;
                                let pill_right =
                                    card_right - row_inset - (8.0 * scale).round() as i32;
                                let pill_left = pill_right - bw;
                                let pill_top = top + (8.0 * scale).round() as i32;
                                let tone_col = pal.tone_rgb(*tone);
                                fill_rounded_box(
                                    dst_slice,
                                    w,
                                    h,
                                    pill_left as f32,
                                    pill_top as f32,
                                    pill_right as f32,
                                    (pill_top + bh) as f32,
                                    (4.0 * scale).clamp(3.0, 8.0),
                                    tone_col,
                                    if dark_mode { 0.18 } else { 0.14 },
                                    None,
                                );
                                draw_text_onto_bgra(
                                    text_dc,
                                    txt_slice,
                                    dst_slice,
                                    w,
                                    h,
                                    badge_txt,
                                    font_badge,
                                    true,
                                    RECT {
                                        left: pill_left + (6.0 * scale).round() as i32,
                                        top: pill_top,
                                        right: pill_right - (4.0 * scale).round() as i32,
                                        bottom: pill_top + bh,
                                    },
                                    false,
                                    tone_col,
                                );
                                title_right = pill_left - (6.0 * scale).round() as i32;
                            }

                            let inner_left = card_left + row_inset + (10.0 * scale).round() as i32;
                            draw_text_onto_bgra(
                                text_dc,
                                txt_slice,
                                dst_slice,
                                w,
                                h,
                                title,
                                font_main,
                                true,
                                RECT {
                                    left: inner_left,
                                    top: top + (5.0 * scale).round() as i32,
                                    right: title_right,
                                    bottom: top + (24.0 * scale).round() as i32,
                                },
                                false,
                                pal.text,
                            );
                            draw_text_onto_bgra(
                                text_dc,
                                txt_slice,
                                dst_slice,
                                w,
                                h,
                                subtitle,
                                font_sub,
                                false,
                                RECT {
                                    left: inner_left,
                                    top: top + (23.0 * scale).round() as i32,
                                    right: card_right - row_inset - (10.0 * scale).round() as i32,
                                    bottom: bottom - (6.0 * scale).round() as i32,
                                },
                                false,
                                pal.text_muted,
                            );
                        }
                        MenuEntry::SectionHeader(text) => {
                            draw_text_onto_bgra(
                                text_dc,
                                txt_slice,
                                dst_slice,
                                w,
                                h,
                                text,
                                font_sub,
                                true,
                                RECT {
                                    left: card_left + text_pad,
                                    top,
                                    right: card_right - text_pad,
                                    bottom,
                                },
                                false,
                                pal.text_muted,
                            );
                        }
                        MenuEntry::Separator => {
                            let sep_y = ((top + bottom) / 2).clamp(0, h as i32 - 1) as usize;
                            let x0 = (card_left + (10.0 * scale).round() as i32).max(0) as usize;
                            let x1 = ((card_right - (10.0 * scale).round() as i32).max(0) as usize)
                                .min(w);
                            for x in x0..x1 {
                                blend_premul(
                                    dst_slice,
                                    (sep_y * w + x) * 4,
                                    pal.separator.0,
                                    pal.separator.1,
                                    pal.separator.2,
                                    1.0,
                                );
                            }
                        }
                        MenuEntry::Disabled(text) => {
                            draw_text_onto_bgra(
                                text_dc,
                                txt_slice,
                                dst_slice,
                                w,
                                h,
                                text,
                                font_main,
                                false,
                                RECT {
                                    left: card_left + text_pad,
                                    top,
                                    right: card_right - text_pad,
                                    bottom,
                                },
                                false,
                                pal.text_muted,
                            );
                        }
                        MenuEntry::Action {
                            label,
                            right_hint,
                            active_dot,
                            tone,
                            ..
                        } => {
                            if is_hovered {
                                let hover_col = if *tone == AccentTone::Bad {
                                    pal.bad
                                } else {
                                    pal.hover_bg
                                };
                                let hover_alpha = if *tone == AccentTone::Bad {
                                    if dark_mode {
                                        0.18
                                    } else {
                                        0.12
                                    }
                                } else {
                                    1.0
                                };
                                fill_rounded_box(
                                    dst_slice,
                                    w,
                                    h,
                                    (card_left + row_inset) as f32,
                                    (top + 1) as f32,
                                    (card_right - row_inset) as f32,
                                    (bottom - 1) as f32,
                                    (6.0 * scale).clamp(4.0, 10.0),
                                    hover_col,
                                    hover_alpha,
                                    None,
                                );
                            }

                            let mut left_x = card_left + text_pad;
                            if *active_dot {
                                let dot_col = match tone {
                                    AccentTone::Neutral => pal.ok,
                                    other => pal.tone_rgb(*other),
                                };
                                draw_dot(
                                    dst_slice,
                                    w,
                                    h,
                                    (left_x + (3.0 * scale).round() as i32) as f32,
                                    cy,
                                    (3.0 * scale).clamp(2.2, 6.0),
                                    dot_col,
                                );
                                left_x += (13.0 * scale).round() as i32;
                            }

                            let mut right_x = card_right - text_pad;
                            if let Some(hint) = right_hint {
                                let hw = measure_text_width(text_dc, hint, font_sub, false)
                                    + (8.0 * scale).round() as i32;
                                let hint_col = if *tone == AccentTone::Info {
                                    pal.info
                                } else {
                                    pal.text_muted
                                };
                                draw_text_onto_bgra(
                                    text_dc,
                                    txt_slice,
                                    dst_slice,
                                    w,
                                    h,
                                    hint,
                                    font_sub,
                                    *tone == AccentTone::Info,
                                    RECT {
                                        left: right_x - hw,
                                        top,
                                        right: right_x,
                                        bottom,
                                    },
                                    true,
                                    hint_col,
                                );
                                right_x -= hw + (4.0 * scale).round() as i32;
                            }

                            let text_col = match tone {
                                AccentTone::Bad if !is_hovered => pal.text_muted,
                                AccentTone::Bad => pal.bad,
                                AccentTone::Neutral => pal.text,
                                other => pal.tone_rgb(*other),
                            };
                            draw_text_onto_bgra(
                                text_dc,
                                txt_slice,
                                dst_slice,
                                w,
                                h,
                                label,
                                font_main,
                                *active_dot || *tone == AccentTone::Info,
                                RECT {
                                    left: left_x,
                                    top,
                                    right: right_x,
                                    bottom,
                                },
                                false,
                                text_col,
                            );
                        }
                        MenuEntry::Toggle {
                            label,
                            checked,
                            on_tone,
                            ..
                        } => {
                            if is_hovered {
                                fill_rounded_box(
                                    dst_slice,
                                    w,
                                    h,
                                    (card_left + row_inset) as f32,
                                    (top + 1) as f32,
                                    (card_right - row_inset) as f32,
                                    (bottom - 1) as f32,
                                    (6.0 * scale).clamp(4.0, 10.0),
                                    pal.hover_bg,
                                    1.0,
                                    None,
                                );
                            }
                            let toggle_right = (card_right - text_pad) as f32;
                            draw_toggle_pill(
                                dst_slice,
                                w,
                                h,
                                toggle_right,
                                cy,
                                scale,
                                *checked,
                                pal.tone_rgb(*on_tone),
                                pal,
                            );
                            draw_text_onto_bgra(
                                text_dc,
                                txt_slice,
                                dst_slice,
                                w,
                                h,
                                label,
                                font_main,
                                false,
                                RECT {
                                    left: card_left + text_pad,
                                    top,
                                    right: card_right - text_pad - (34.0 * scale).round() as i32,
                                    bottom,
                                },
                                false,
                                pal.text,
                            );
                        }
                        MenuEntry::Submenu {
                            label,
                            right_hint,
                            kind,
                        } => {
                            let highlighted = is_hovered || active_submenu == Some(*kind);
                            if highlighted {
                                fill_rounded_box(
                                    dst_slice,
                                    w,
                                    h,
                                    (card_left + row_inset) as f32,
                                    (top + 1) as f32,
                                    (card_right - row_inset) as f32,
                                    (bottom - 1) as f32,
                                    (6.0 * scale).clamp(4.0, 10.0),
                                    pal.hover_bg,
                                    1.0,
                                    None,
                                );
                            }
                            let chev_cx = (card_right - text_pad) as f32 - 3.0 * scale;
                            draw_chevron_right(
                                dst_slice,
                                w,
                                h,
                                chev_cx,
                                cy,
                                8.5 * scale,
                                (0.65 * scale).clamp(0.55, 1.3),
                                if highlighted {
                                    pal.text
                                } else {
                                    pal.text_muted
                                },
                            );

                            let mut right_x = card_right - text_pad - (14.0 * scale).round() as i32;
                            if let Some(hint) = right_hint {
                                let hw = measure_text_width(text_dc, hint, font_sub, false)
                                    + (8.0 * scale).round() as i32;
                                draw_text_onto_bgra(
                                    text_dc,
                                    txt_slice,
                                    dst_slice,
                                    w,
                                    h,
                                    hint,
                                    font_sub,
                                    false,
                                    RECT {
                                        left: right_x - hw,
                                        top,
                                        right: right_x,
                                        bottom,
                                    },
                                    true,
                                    pal.text_muted,
                                );
                                right_x -= hw + (4.0 * scale).round() as i32;
                            }

                            draw_text_onto_bgra(
                                text_dc,
                                txt_slice,
                                dst_slice,
                                w,
                                h,
                                label,
                                font_main,
                                false,
                                RECT {
                                    left: card_left + text_pad,
                                    top,
                                    right: right_x,
                                    bottom,
                                },
                                false,
                                pal.text,
                            );
                        }
                    }
                }

                unsafe {
                    SelectObject(text_dc, old_txt_bmp);
                }

                let old_dst_bmp = unsafe { SelectObject(mem_dc, dst_bmp.into()) };
                let dst_pt = POINT { x: win_x, y: win_y };
                let src_pt = POINT { x: 0, y: 0 };
                let sz = SIZE {
                    cx: layout.total_w,
                    cy: layout.total_h,
                };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                unsafe {
                    let _ = UpdateLayeredWindow(
                        hwnd,
                        Some(screen_dc),
                        Some(&dst_pt),
                        Some(&sz),
                        Some(mem_dc),
                        Some(&src_pt),
                        COLORREF(0),
                        Some(&blend),
                        ULW_ALPHA,
                    );
                    SelectObject(mem_dc, old_dst_bmp);
                }
            }
            unsafe {
                let _ = DeleteObject(dst_bmp.into());
                let _ = DeleteObject(txt_bmp.into());
            }
        }

        unsafe {
            let _ = DeleteDC(text_dc);
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(None, screen_dc);
        }
    }

    struct LocalMenu {
        main_hwnd: HWND,
        sub_hwnd: HWND,
        shared_model: Arc<Mutex<TrayMenuModel>>,
        on_action: Arc<dyn Fn(TrayAction) + Send + Sync>,
        visible: bool,
        model_snap: TrayMenuModel,
        work_area: (i32, i32, i32, i32),
        scale: f32,
        main_entries: Vec<MenuEntry>,
        main_layout: MenuLayout,
        main_pos: (i32, i32),
        main_hover: Option<usize>,
        main_tracking: bool,
        active_sub: Option<SubmenuKind>,
        sub_entries: Vec<MenuEntry>,
        sub_layout: Option<MenuLayout>,
        sub_pos: (i32, i32),
        sub_hover: Option<usize>,
        sub_tracking: bool,
    }

    impl LocalMenu {
        fn hide_all(&mut self) {
            if self.visible {
                unsafe {
                    let _ = ShowWindow(self.sub_hwnd, SW_HIDE);
                    let _ = ShowWindow(self.main_hwnd, SW_HIDE);
                }
            }
            self.visible = false;
            self.main_hover = None;
            self.main_tracking = false;
            self.active_sub = None;
            self.sub_entries.clear();
            self.sub_layout = None;
            self.sub_hover = None;
            self.sub_tracking = false;
        }

        fn hide_submenu(&mut self) {
            if self.active_sub.is_some() {
                unsafe {
                    let _ = ShowWindow(self.sub_hwnd, SW_HIDE);
                }
                self.active_sub = None;
                self.sub_entries.clear();
                self.sub_layout = None;
                self.sub_hover = None;
                self.sub_tracking = false;
            }
        }

        fn redraw_main(&self) {
            if !self.visible {
                return;
            }
            render_menu_window(
                self.main_hwnd,
                self.main_pos.0,
                self.main_pos.1,
                &self.main_entries,
                &self.main_layout,
                self.main_hover,
                self.active_sub,
                self.model_snap.dark_mode,
            );
        }

        fn redraw_sub(&self) {
            if !self.visible || self.active_sub.is_none() {
                return;
            }
            if let Some(sub_layout) = &self.sub_layout {
                render_menu_window(
                    self.sub_hwnd,
                    self.sub_pos.0,
                    self.sub_pos.1,
                    &self.sub_entries,
                    sub_layout,
                    self.sub_hover,
                    None,
                    self.model_snap.dark_mode,
                );
            }
        }

        fn open_submenu(&mut self, kind: SubmenuKind, row_idx: usize) {
            if self.active_sub == Some(kind) {
                return;
            }
            let entries = build_submenu_entries(&self.model_snap, kind);
            let base_w = match kind {
                SubmenuKind::Audio => 264.0,
                SubmenuKind::Mouse | SubmenuKind::Keyboard => 246.0,
                SubmenuKind::Profiles => 232.0,
            };
            let sub_layout = MenuLayout::compute(&entries, base_w, self.scale);
            let main_card_left = self.main_pos.0 + self.main_layout.shadow;
            let main_card_right = main_card_left + self.main_layout.card_w;
            let row_top_screen = self.main_pos.1 + self.main_layout.row_rects[row_idx].0;
            let sub_pos = position_submenu_popup(
                main_card_left,
                main_card_right,
                row_top_screen,
                &sub_layout,
                self.work_area,
            );

            self.active_sub = Some(kind);
            self.sub_entries = entries;
            self.sub_hover = None;
            self.sub_pos = sub_pos;
            self.sub_layout = Some(sub_layout);
            self.redraw_sub();

            if let Some(sl) = &self.sub_layout {
                unsafe {
                    let _ = SetWindowPos(
                        self.sub_hwnd,
                        Some(HWND_TOPMOST),
                        self.sub_pos.0,
                        self.sub_pos.1,
                        sl.total_w,
                        sl.total_h,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                    let _ = ShowWindow(self.sub_hwnd, SW_SHOWNOACTIVATE);
                }
            }
            self.redraw_main();
        }

        fn open_at_cursor(&mut self) {
            self.hide_submenu();
            let snap = self
                .shared_model
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default();
            let mut pt = POINT::default();
            let _ = unsafe { GetCursorPos(&mut pt) };
            let work = work_area_at(pt);
            let scale = current_taskbar_scale();

            let entries = build_main_entries(&snap);
            let layout = MenuLayout::compute(&entries, 276.0, scale);
            let pos = position_main_popup(pt.x, pt.y, &layout, work);

            self.model_snap = snap;
            self.work_area = work;
            self.scale = scale;
            self.main_entries = entries;
            self.main_layout = layout;
            self.main_pos = pos;
            self.main_hover = None;
            self.visible = true;

            self.redraw_main();
            unsafe {
                let _ = SetWindowPos(
                    self.main_hwnd,
                    Some(HWND_TOPMOST),
                    self.main_pos.0,
                    self.main_pos.1,
                    self.main_layout.total_w,
                    self.main_layout.total_h,
                    SWP_SHOWWINDOW,
                );
                let _ = ShowWindow(self.main_hwnd, SW_SHOW);
                let _ = SetForegroundWindow(self.main_hwnd);
            }
        }
    }

    thread_local! {
        static MENU: RefCell<Option<LocalMenu>> = const { RefCell::new(None) };
    }

    fn loword_i32(lp: LPARAM) -> i32 {
        (lp.0 as u32 & 0xFFFF) as i16 as i32
    }
    fn hiword_i32(lp: LPARAM) -> i32 {
        ((lp.0 as u32 >> 16) & 0xFFFF) as i16 as i32
    }

    unsafe extern "system" fn menu_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_MENU_OPEN => {
                MENU.with(|cell| {
                    if let Ok(mut opt) = cell.try_borrow_mut() {
                        if let Some(m) = opt.as_mut() {
                            m.open_at_cursor();
                        }
                    }
                });
                LRESULT(0)
            }
            WM_ACTIVATE => {
                let state = (wparam.0 & 0xFFFF) as u32;
                if state == WA_INACTIVE {
                    let other = HWND(lparam.0 as *mut c_void);
                    MENU.with(|cell| {
                        if let Ok(mut opt) = cell.try_borrow_mut() {
                            if let Some(m) = opt.as_mut() {
                                if other != m.main_hwnd && other != m.sub_hwnd {
                                    m.hide_all();
                                }
                            }
                        }
                    });
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if wparam.0 == VK_ESCAPE_RAW {
                    MENU.with(|cell| {
                        if let Ok(mut opt) = cell.try_borrow_mut() {
                            if let Some(m) = opt.as_mut() {
                                m.hide_all();
                            }
                        }
                    });
                }
                LRESULT(0)
            }
            WM_SETCURSOR => {
                let is_hand = MENU.with(|cell| {
                    cell.borrow().as_ref().is_some_and(|m| {
                        if hwnd == m.main_hwnd {
                            m.main_hover.is_some()
                        } else {
                            m.sub_hover.is_some()
                        }
                    })
                });
                let cursor_id = if is_hand { IDC_HAND } else { IDC_ARROW };
                if let Ok(cur) = unsafe { LoadCursorW(None, cursor_id) } {
                    unsafe {
                        let _ = SetCursor(Some(cur));
                    }
                    return LRESULT(1);
                }
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_MOUSEMOVE => {
                let x = loword_i32(lparam);
                let y = hiword_i32(lparam);
                MENU.with(|cell| {
                    if let Ok(mut opt) = cell.try_borrow_mut() {
                        if let Some(m) = opt.as_mut() {
                            if hwnd == m.main_hwnd {
                                if !m.main_tracking {
                                    let mut tme = TRACKMOUSEEVENT {
                                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                                        dwFlags: TME_LEAVE,
                                        hwndTrack: hwnd,
                                        dwHoverTime: 0,
                                    };
                                    if unsafe { TrackMouseEvent(&mut tme) }.is_ok() {
                                        m.main_tracking = true;
                                    }
                                }
                                let next = m.main_layout.hit_test(&m.main_entries, x, y);
                                if next != m.main_hover {
                                    m.main_hover = next;
                                    if let Some(idx) = next {
                                        if let MenuEntry::Submenu { kind, .. } = m.main_entries[idx]
                                        {
                                            m.open_submenu(kind, idx);
                                        } else {
                                            m.hide_submenu();
                                            m.redraw_main();
                                        }
                                    } else {
                                        m.redraw_main();
                                    }
                                }
                            } else if hwnd == m.sub_hwnd {
                                if !m.sub_tracking {
                                    let mut tme = TRACKMOUSEEVENT {
                                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                                        dwFlags: TME_LEAVE,
                                        hwndTrack: hwnd,
                                        dwHoverTime: 0,
                                    };
                                    if unsafe { TrackMouseEvent(&mut tme) }.is_ok() {
                                        m.sub_tracking = true;
                                    }
                                }
                                if let Some(sl) = &m.sub_layout {
                                    let next = sl.hit_test(&m.sub_entries, x, y);
                                    if next != m.sub_hover {
                                        m.sub_hover = next;
                                        m.redraw_sub();
                                    }
                                }
                            }
                        }
                    }
                });
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                MENU.with(|cell| {
                    if let Ok(mut opt) = cell.try_borrow_mut() {
                        if let Some(m) = opt.as_mut() {
                            if hwnd == m.main_hwnd {
                                m.main_tracking = false;
                                if m.main_hover.is_some() {
                                    m.main_hover = None;
                                    m.redraw_main();
                                }
                            } else if hwnd == m.sub_hwnd {
                                m.sub_tracking = false;
                                if m.sub_hover.is_some() {
                                    m.sub_hover = None;
                                    m.redraw_sub();
                                }
                            }
                        }
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let x = loword_i32(lparam);
                let y = hiword_i32(lparam);
                let mut triggered: Option<TrayAction> = None;
                let mut callback: Option<Arc<dyn Fn(TrayAction) + Send + Sync>> = None;
                MENU.with(|cell| {
                    if let Ok(mut opt) = cell.try_borrow_mut() {
                        if let Some(m) = opt.as_mut() {
                            if hwnd == m.main_hwnd {
                                if let Some(idx) = m.main_layout.hit_test(&m.main_entries, x, y) {
                                    match &m.main_entries[idx] {
                                        MenuEntry::Action { action, .. }
                                        | MenuEntry::Toggle { action, .. } => {
                                            callback = Some(Arc::clone(&m.on_action));
                                            triggered = Some(action.clone());
                                            m.hide_all();
                                        }
                                        MenuEntry::Submenu { kind, .. } => {
                                            let k = *kind;
                                            m.open_submenu(k, idx);
                                        }
                                        _ => {}
                                    }
                                }
                            } else if hwnd == m.sub_hwnd {
                                if let Some(sl) = &m.sub_layout {
                                    if let Some(idx) = sl.hit_test(&m.sub_entries, x, y) {
                                        if let MenuEntry::Action { action, .. }
                                        | MenuEntry::Toggle { action, .. } = &m.sub_entries[idx]
                                        {
                                            callback = Some(Arc::clone(&m.on_action));
                                            triggered = Some(action.clone());
                                            m.hide_all();
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
                if let (Some(cb), Some(act)) = (callback, triggered) {
                    cb(act);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let sub = MENU.with(|cell| cell.borrow().as_ref().map(|m| m.sub_hwnd));
                unsafe {
                    if let Some(sh) = sub {
                        if !sh.is_invalid() && sh != hwnd {
                            let _ = DestroyWindow(sh);
                        }
                    }
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let is_main =
                    MENU.with(|cell| cell.borrow().as_ref().is_some_and(|m| m.main_hwnd == hwnd));
                if is_main {
                    unsafe {
                        PostQuitMessage(0);
                    }
                }
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    let class_str = format!(
        "PolledTrayMenu_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let class_w = to_wide_null(&class_str);
    let hmodule = unsafe { GetModuleHandleW(PCWSTR::null()) }.unwrap_or_default();
    let hinstance = HINSTANCE(hmodule.0);

    let wc = WNDCLASSW {
        lpfnWndProc: Some(menu_wnd_proc),
        hInstance: hinstance,
        lpszClassName: PCWSTR(class_w.as_ptr()),
        ..Default::default()
    };
    if unsafe { RegisterClassW(&wc) } == 0 {
        return;
    }

    let ex_style = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED;
    let main_hwnd = match unsafe {
        CreateWindowExW(
            ex_style,
            PCWSTR(class_w.as_ptr()),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            280,
            320,
            None,
            None,
            Some(hinstance),
            None,
        )
    } {
        Ok(h) if !h.is_invalid() => h,
        _ => {
            unsafe {
                let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
            }
            return;
        }
    };

    let sub_hwnd = match unsafe {
        CreateWindowExW(
            ex_style,
            PCWSTR(class_w.as_ptr()),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            240,
            240,
            Some(main_hwnd),
            None,
            Some(hinstance),
            None,
        )
    } {
        Ok(h) if !h.is_invalid() => h,
        _ => {
            unsafe {
                let _ = DestroyWindow(main_hwnd);
                let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
            }
            return;
        }
    };

    let default_layout = MenuLayout::compute(&[], 276.0, 1.0);
    MENU.with(|cell| {
        *cell.borrow_mut() = Some(LocalMenu {
            main_hwnd,
            sub_hwnd,
            shared_model,
            on_action,
            visible: false,
            model_snap: TrayMenuModel::default(),
            work_area: (0, 0, 1920, 1040),
            scale: 1.0,
            main_entries: Vec::new(),
            main_layout: default_layout,
            main_pos: (0, 0),
            main_hover: None,
            main_tracking: false,
            active_sub: None,
            sub_entries: Vec::new(),
            sub_layout: None,
            sub_pos: (0, 0),
            sub_hover: None,
            sub_tracking: false,
        });
    });

    unsafe {
        let _ = ShowWindow(main_hwnd, SW_HIDE);
        let _ = ShowWindow(sub_hwnd, SW_HIDE);
    }
    hwnd_slot.store(main_hwnd.0 as isize, Ordering::SeqCst);

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0).0 > 0 } {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    hwnd_slot.store(0, Ordering::SeqCst);
    unsafe {
        let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
    }
    MENU.with(|cell| {
        cell.borrow_mut().take();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use polled_core::BatteryStatus;

    #[test]
    fn builds_main_and_submenu_entries_with_hit_testing_and_clamping() {
        let model = TrayMenuModel {
            dark_mode: true,
            auto_switch_enabled: true,
            profile_locked: false,
            mic_muted: false,
            taskbar_widget_enabled: true,
            active_display_override: Some("1280x960 @ 360Hz".to_string()),
            active_profile_id: "cs2".to_string(),
            active_profile_name: "CS2".to_string(),
            available_profiles: vec![
                ("desktop".to_string(), "Desktop (Default)".to_string()),
                ("cs2".to_string(), "CS2".to_string()),
            ],
            audio_outputs: vec![("spk".to_string(), "Speakers".to_string(), true)],
            primary_mouse: Some(TrayMouseState {
                name: "Pulsar X2 CrazyLight Mini".to_string(),
                is_simulator: false,
                current_dpi: Some(1600),
                current_polling_hz: Some(4000),
                dpi_min: 100,
                dpi_max: 32000,
                polling_rates_hz: vec![1000, 4000, 8000],
                battery: Some(BatteryStatus {
                    percent: 55,
                    charging: false,
                    voltage_mv: None,
                }),
                battery_mins_left: Some(240),
            }),
            primary_wooting: None,
            available_update_version: Some("0.16.0".to_string()),
        };

        let entries = build_main_entries(&model);
        let layout = MenuLayout::compute(&entries, 276.0, 1.0);
        assert_eq!(layout.row_rects.len(), entries.len());

        // Header card (index 0) is non-interactive; Open Polled (index 1) is interactive.
        let (h_top, h_bot) = layout.row_rects[0];
        assert_eq!(
            layout.hit_test(&entries, layout.shadow + 20, (h_top + h_bot) / 2),
            None
        );
        let (o_top, o_bot) = layout.row_rects[1];
        assert_eq!(
            layout.hit_test(&entries, layout.shadow + 20, (o_top + o_bot) / 2),
            Some(1)
        );

        // Positioning clamps cleanly within a 1920x1032 work area above the taskbar.
        let (wx, wy) = position_main_popup(1880, 1055, &layout, (0, 0, 1920, 1032));
        let card_right = wx + layout.shadow + layout.card_w;
        let card_bottom = wy + layout.shadow + layout.card_h;
        assert!(card_right <= 1920 - 6);
        assert!(card_bottom <= 1032 - 6);

        // Submenu opens cleanly to the left of the right-edge main menu.
        let sub_entries = build_submenu_entries(&model, SubmenuKind::Mouse);
        assert!(sub_entries.iter().any(|e| matches!(
            e,
            MenuEntry::Action {
                action: TrayAction::QuickSetMousePolling(4000),
                active_dot: true,
                ..
            }
        )));
        let sub_layout = MenuLayout::compute(&sub_entries, 246.0, 1.0);
        let main_left = wx + layout.shadow;
        let main_right = main_left + layout.card_w;
        let (sx, _sy) = position_submenu_popup(
            main_left,
            main_right,
            wy + layout.row_rects[5].0,
            &sub_layout,
            (0, 0, 1920, 1032),
        );
        assert!(sx + sub_layout.shadow + sub_layout.card_w < main_left);
    }

    #[test]
    fn card_and_controls_rasterize_valid_premultiplied_bgra() {
        let w = 160usize;
        let h = 100usize;
        let mut buf = vec![0u8; w * h * 4];
        let pal = MenuPalette::for_mode(true);
        draw_rounded_card_with_shadow(&mut buf, w, h, 8, 10.0, pal.surface, pal.border, true);
        draw_toggle_pill(&mut buf, w, h, 140.0, 50.0, 1.0, true, pal.ok, pal);
        draw_chevron_right(&mut buf, w, h, 130.0, 72.0, 8.5, 0.65, pal.text);

        for px in buf.as_chunks::<4>().0 {
            let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
            assert!(b <= a && g <= a && r <= a, "invalid premul pixel {:?}", px);
        }
        // Center pixel of the card is opaque dark surface.
        let mid = ((h / 2) * w + (w / 2)) * 4;
        assert_eq!(buf[mid + 3], 255);
    }
}
