// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plain monochrome black & white Dark / Light palette for Polled.
//! Only the four semantic status colors (`OK`, `INFO`, `WARN`, `BAD`) carry chroma.

use eframe::egui::{self, Color32, CornerRadius, Stroke, Vec2};
use polled_core::{Severity, ThemePreference};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
}

impl From<ThemePreference> for ThemeMode {
    fn from(pref: ThemePreference) -> Self {
        match pref {
            ThemePreference::Dark => ThemeMode::Dark,
            ThemePreference::Light => ThemeMode::Light,
        }
    }
}

impl From<ThemeMode> for ThemePreference {
    fn from(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => ThemePreference::Dark,
            ThemeMode::Light => ThemePreference::Light,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    pub surface_alt: Color32,
    pub border: Color32,
    pub border_subtle: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    /// Primary monochrome button fill (white in dark mode, black in light mode).
    pub btn_primary_bg: Color32,
    pub btn_primary_fg: Color32,
    /// Semantic status colors (the only non-grayscale colors in the UI).
    pub ok: Color32,
    pub info: Color32,
    pub warn: Color32,
    pub bad: Color32,
}

impl ThemeMode {
    pub fn toggle(self) -> Self {
        match self {
            ThemeMode::Dark => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::Dark => "Light mode",
            ThemeMode::Light => "Dark mode",
        }
    }

    pub fn palette(self) -> Palette {
        match self {
            ThemeMode::Dark => Palette {
                bg: Color32::from_rgb(9, 9, 11),
                surface: Color32::from_rgb(15, 15, 18),
                surface_alt: Color32::from_rgb(24, 24, 28),
                border: Color32::from_rgb(42, 42, 48),
                border_subtle: Color32::from_rgb(30, 30, 35),
                text: Color32::from_rgb(245, 245, 247),
                text_muted: Color32::from_rgb(148, 148, 158),
                btn_primary_bg: Color32::from_rgb(245, 245, 247),
                btn_primary_fg: Color32::from_rgb(9, 9, 11),
                ok: Color32::from_rgb(34, 197, 94),
                info: Color32::from_rgb(56, 189, 248),
                warn: Color32::from_rgb(245, 158, 11),
                bad: Color32::from_rgb(239, 68, 68),
            },
            ThemeMode::Light => Palette {
                bg: Color32::from_rgb(250, 250, 251),
                surface: Color32::from_rgb(255, 255, 255),
                surface_alt: Color32::from_rgb(242, 242, 245),
                border: Color32::from_rgb(218, 218, 224),
                border_subtle: Color32::from_rgb(232, 232, 236),
                text: Color32::from_rgb(12, 12, 14),
                text_muted: Color32::from_rgb(92, 92, 102),
                btn_primary_bg: Color32::from_rgb(15, 15, 18),
                btn_primary_fg: Color32::from_rgb(255, 255, 255),
                ok: Color32::from_rgb(21, 128, 61),
                info: Color32::from_rgb(3, 105, 161),
                warn: Color32::from_rgb(180, 83, 9),
                bad: Color32::from_rgb(185, 28, 28),
            },
        }
    }

    pub fn apply_to(self, ctx: &egui::Context) {
        let p = self.palette();
        let mut visuals = match self {
            ThemeMode::Dark => egui::Visuals::dark(),
            ThemeMode::Light => egui::Visuals::light(),
        };
        visuals.panel_fill = p.bg;
        visuals.window_fill = p.surface;
        visuals.window_stroke = Stroke::new(1.0_f32, p.border);
        visuals.window_corner_radius = CornerRadius::same(10);
        visuals.override_text_color = Some(p.text);

        visuals.widgets.noninteractive.bg_fill = p.surface;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border_subtle);
        visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);

        visuals.widgets.inactive.bg_fill = p.surface_alt;
        visuals.widgets.inactive.weak_bg_fill = p.surface_alt;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
        visuals.widgets.inactive.corner_radius = CornerRadius::same(6);

        visuals.widgets.hovered.bg_fill = p.border;
        visuals.widgets.hovered.weak_bg_fill = p.border;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.text_muted);
        visuals.widgets.hovered.corner_radius = CornerRadius::same(6);

        visuals.widgets.active.bg_fill = p.border;
        visuals.widgets.active.weak_bg_fill = p.border;
        visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.text);
        visuals.widgets.active.corner_radius = CornerRadius::same(6);

        visuals.selection.bg_fill = p.border;
        visuals.selection.stroke = Stroke::new(1.0_f32, p.text);
        ctx.set_visuals(visuals);

        ctx.style_mut(|style| {
            style.spacing.item_spacing = Vec2::new(8.0, 6.0);
            style.spacing.button_padding = Vec2::new(10.0, 5.0);
            style.spacing.combo_height = 240.0;
            style.spacing.scroll = egui::style::ScrollStyle::thin();
        });
    }
}

impl Palette {
    pub fn severity_fg(self, severity: Severity) -> Color32 {
        match severity {
            Severity::Ok => self.ok,
            Severity::Info => self.info,
            Severity::Warn => self.warn,
            Severity::Bad => self.bad,
        }
    }

    pub fn severity_bg(self, severity: Severity, mode: ThemeMode) -> Color32 {
        let c = self.severity_fg(severity);
        let alpha = match mode {
            ThemeMode::Dark => 28,
            ThemeMode::Light => 20,
        };
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
    }
}
