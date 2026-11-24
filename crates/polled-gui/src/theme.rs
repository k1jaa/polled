// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plain monochrome black & white Dark / Light palette for Polled.
//! Only the four semantic status colors (`OK`, `INFO`, `WARN`, `BAD`) carry chroma.

use eframe::egui::{self, Color32};
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
            ThemeMode::Dark => "Light theme",
            ThemeMode::Light => "Dark theme",
        }
    }

    pub fn palette(self) -> Palette {
        match self {
            ThemeMode::Dark => Palette {
                bg: Color32::from_rgb(9, 9, 11),
                surface: Color32::from_rgb(18, 18, 20),
                surface_alt: Color32::from_rgb(26, 26, 29),
                border: Color32::from_rgb(46, 46, 51),
                text: Color32::from_rgb(245, 245, 247),
                text_muted: Color32::from_rgb(161, 161, 170),
                btn_primary_bg: Color32::from_rgb(245, 245, 247),
                btn_primary_fg: Color32::from_rgb(9, 9, 11),
                ok: Color32::from_rgb(34, 197, 94),
                info: Color32::from_rgb(56, 189, 248),
                warn: Color32::from_rgb(245, 158, 11),
                bad: Color32::from_rgb(239, 68, 68),
            },
            ThemeMode::Light => Palette {
                bg: Color32::from_rgb(255, 255, 255),
                surface: Color32::from_rgb(248, 248, 249),
                surface_alt: Color32::from_rgb(240, 240, 242),
                border: Color32::from_rgb(212, 212, 216),
                text: Color32::from_rgb(12, 12, 14),
                text_muted: Color32::from_rgb(82, 82, 91),
                btn_primary_bg: Color32::from_rgb(12, 12, 14),
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
        visuals.override_text_color = Some(p.text);
        visuals.widgets.noninteractive.bg_fill = p.surface;
        visuals.widgets.inactive.bg_fill = p.surface_alt;
        visuals.widgets.hovered.bg_fill = p.border;
        visuals.widgets.active.bg_fill = p.border;
        visuals.selection.bg_fill = p.border;
        visuals.selection.stroke.color = p.text;
        ctx.set_visuals(visuals);
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
            ThemeMode::Dark => 34,
            ThemeMode::Light => 26,
        };
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
    }
}
