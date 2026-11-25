// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dedicated monochrome Keyboard view for Wooting Analog / Hall-Effect keyboards:
//! - Full live telemetry (firmware, layout, chip, switch tech, real onboard profile names,
//!   base actuation, Rapid Trigger, Tachyon, tick rate, per-key overrides, SOCD / AKC bindings,
//!   RGB brightness & effect layers, global settings, and flash health)
//! - Instant RAM-only onboard profile switching (zero flash wear)
//! - Opt-in live analog key-travel visualizer (`0xFF53` / `0xFF54` stream, 0% CPU when stopped)
//! - Confirm-gated ARM Base Analog & Global Settings tuning with mandatory read-back verification

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use chrono::Local;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Margin, Pos2, Rect, RichText, Stroke, Vec2,
};
use polled_core::win::{SystemEnv, WindowsEnv};
use polled_core::{
    dump_hid_devices, format_unsupported_mouse_issue, AppPaths, Severity, WootingAnalogBasePatch,
    WootingAnalogKeyReading, WootingAnalogStream, WootingGlobalSettingsPatch, WootingKeyboardInfo,
};

use crate::profile_runtime::{ProfileRuntimeCmd, ProfileRuntimeHandle, ProfileRuntimeSnapshot};
use crate::theme::{Palette, ThemeMode};

const ANALOG_SMOOTH_RATE: f32 = 18.0;
const ANALOG_RELEASE_HOLD: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone)]
struct SmoothedAnalogKey {
    label: String,
    matrix_index: Option<u16>,
    target_mm: f32,
    display_mm: f32,
    peak_mm: f32,
    raw_value: u16,
    raw_max: u16,
    actuated: Option<bool>,
    is_pressed: bool,
    last_active: Instant,
}

pub struct KeyboardUiState {
    selected_index: usize,
    analog_stream: Option<WootingAnalogStream>,
    smoothed_keys: BTreeMap<u16, SmoothedAnalogKey>,
    last_analog_frame: Instant,
    stream_error: Option<String>,
    // Draft values for ARM base analog tuning (synced when keyboard or active profile changes).
    synced_key: Option<(String, Option<u8>)>,
    draft_actuation_mm: f32,
    draft_rt_enabled: bool,
    draft_rt_sens_mm: f32,
    confirm_analog_flash: bool,
    // Draft values for ARM global settings.
    draft_debounce_filter: bool,
    draft_invert_num_lock: bool,
    draft_rgb_dim_min: u32,
    draft_rgb_off_min: u32,
    draft_rgb_turn_off: bool,
    // Serial-redacted keyboard/HID report modal.
    report_modal: Option<String>,
    pub notice: Option<(Severity, String)>,
}

impl Default for KeyboardUiState {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyboardUiState {
    pub fn new() -> Self {
        Self {
            selected_index: 0,
            analog_stream: None,
            smoothed_keys: BTreeMap::new(),
            last_analog_frame: Instant::now(),
            stream_error: None,
            synced_key: None,
            draft_actuation_mm: 0.40,
            draft_rt_enabled: true,
            draft_rt_sens_mm: 0.15,
            confirm_analog_flash: false,
            draft_debounce_filter: false,
            draft_invert_num_lock: false,
            draft_rgb_dim_min: 5,
            draft_rgb_off_min: 10,
            draft_rgb_turn_off: true,
            report_modal: None,
            notice: None,
        }
    }

    pub fn is_streaming_analog(&self) -> bool {
        self.analog_stream.is_some()
    }

    pub fn stop_analog_stream(&mut self) {
        if let Some(mut s) = self.analog_stream.take() {
            s.stop();
        }
        self.smoothed_keys.clear();
    }

    fn update_smoothed_analog(&mut self, ctx: &egui::Context, live: &[WootingAnalogKeyReading]) {
        let now = Instant::now();
        let dt = now
            .saturating_duration_since(self.last_analog_frame)
            .as_secs_f32()
            .clamp(0.001, 0.08);
        self.last_analog_frame = now;
        let alpha = 1.0 - (-dt * ANALOG_SMOOTH_RATE).exp();

        let incoming: HashMap<u16, &WootingAnalogKeyReading> =
            live.iter().map(|k| (k.stable_id, k)).collect();

        for (id, entry) in self.smoothed_keys.iter_mut() {
            if let Some(sample) = incoming.get(id) {
                entry.label = sample.key_label.clone();
                entry.matrix_index = sample.matrix_index;
                entry.target_mm = sample.travel_mm;
                entry.raw_value = sample.raw_value;
                entry.raw_max = sample.raw_max;
                entry.actuated = sample.actuated;
                if !entry.is_pressed || sample.travel_mm > entry.peak_mm {
                    entry.peak_mm = sample.travel_mm;
                }
                entry.is_pressed = true;
                entry.last_active = now;
            } else {
                entry.target_mm = 0.0;
                entry.raw_value = 0;
                entry.actuated = Some(false);
                entry.is_pressed = false;
            }
            entry.display_mm += (entry.target_mm - entry.display_mm) * alpha;
            if entry.display_mm < 0.005 && !entry.is_pressed {
                entry.display_mm = 0.0;
            }
        }

        for sample in live {
            self.smoothed_keys
                .entry(sample.stable_id)
                .or_insert_with(|| SmoothedAnalogKey {
                    label: sample.key_label.clone(),
                    matrix_index: sample.matrix_index,
                    target_mm: sample.travel_mm,
                    display_mm: sample.travel_mm * alpha.max(0.25),
                    peak_mm: sample.travel_mm,
                    raw_value: sample.raw_value,
                    raw_max: sample.raw_max,
                    actuated: sample.actuated,
                    is_pressed: true,
                    last_active: now,
                });
        }

        self.smoothed_keys.retain(|_, entry| {
            entry.is_pressed
                || entry.display_mm > 0.01
                || now.saturating_duration_since(entry.last_active) < ANALOG_RELEASE_HOLD
        });

        // Keep rendering at smooth monitor refresh while any key is active or gliding back to 0.
        if !self.smoothed_keys.is_empty() {
            ctx.request_repaint();
        }
    }

    pub fn on_action_result(&mut self, ok: bool, message: String) {
        self.confirm_analog_flash = false;
        self.synced_key = None; // Force re-sync from refreshed hardware read-back.
        self.notice = Some((if ok { Severity::Ok } else { Severity::Warn }, message));
    }

    fn sync_drafts_from(&mut self, kb: &WootingKeyboardInfo) {
        let key = (kb.id.clone(), kb.active_profile);
        if self.synced_key.as_ref() == Some(&key) {
            return;
        }
        self.synced_key = Some(key);
        self.confirm_analog_flash = false;
        if let Some(ap) = &kb.analog_profile {
            if let Some(mm) = ap.actuation_mm {
                self.draft_actuation_mm = (mm * 100.0).round() / 100.0;
            }
            if let Some(rt) = ap.rapid_trigger_enabled {
                self.draft_rt_enabled = rt;
            }
            if let Some(sens) = ap.rapid_trigger_sensitivity_mm {
                self.draft_rt_sens_mm = (sens * 100.0).round() / 100.0;
            }
        }
        if let Some(gs) = &kb.global_settings {
            if let Some(db) = gs.debounce_filter {
                self.draft_debounce_filter = db;
            }
            if let Some(inv) = gs.invert_num_lock {
                self.draft_invert_num_lock = inv;
            }
            if let Some(dim) = gs.rgb_dim_minutes {
                self.draft_rgb_dim_min = dim;
            }
            if let Some(off) = gs.rgb_off_minutes {
                self.draft_rgb_off_min = off;
            }
            if let Some(to) = gs.rgb_turn_off {
                self.draft_rgb_turn_off = to;
            }
        }
    }

    pub fn render(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        p: Palette,
        mode: ThemeMode,
        snap: Option<&ProfileRuntimeSnapshot>,
        runtime: &ProfileRuntimeHandle,
    ) {
        if let Some(stream) = &self.analog_stream {
            let (keys, err) = stream.snapshot();
            if let Some(e) = err {
                self.stream_error = Some(e);
                self.stop_analog_stream();
            } else {
                self.update_smoothed_analog(ctx, &keys);
            }
        }

        let content_w = ui.available_width().max(320.0);
        ui.set_max_width(content_w);

        let boards: &[WootingKeyboardInfo] =
            snap.map(|s| s.wooting_keyboards.as_slice()).unwrap_or(&[]);
        if !boards.is_empty() && self.selected_index >= boards.len() {
            self.selected_index = 0;
        }
        let active_kb = boards.get(self.selected_index).cloned();
        if let Some(ref kb) = active_kb {
            self.sync_drafts_from(kb);
        }

        // --- RESPONSIVE PAGE HEADER ---
        let mut report_clicked = false;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(secondary_btn("Refresh", p)).clicked() {
                    self.synced_key = None;
                    runtime.send(ProfileRuntimeCmd::RefreshWooting);
                }
                if ui
                    .add(secondary_btn("Report Keyboard", p))
                    .on_hover_text(
                        "Generate a serial-redacted HID + USB topology markdown report for requesting support",
                    )
                    .clicked()
                {
                    report_clicked = true;
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.label(RichText::new("Keyboard").size(20.0).strong().color(p.text));
                    if let Some(ref kb) = active_kb {
                        ui.add_space(6.0);
                        let meta = format!(
                            "{} ({}) · {}{}",
                            kb.name,
                            kb.vid_pid,
                            kb.chip_type,
                            kb.firmware_version
                                .as_deref()
                                .map(|v| format!(" · v{v}"))
                                .unwrap_or_default()
                        );
                        ui.add(
                            egui::Label::new(RichText::new(meta).size(12.0).color(p.text_muted))
                                .truncate(),
                        );
                    }
                });
            });
        });

        if report_clicked {
            match dump_hid_devices(false, None) {
                Ok(dump) => {
                    let env = WindowsEnv;
                    let topology = env.list_usb_topology().unwrap_or_default();
                    let win_ver = env.windows_version().ok().map(|v| v.display_string());
                    let detected = snap.map(|s| s.detected_hardware.as_slice()).unwrap_or(&[]);
                    self.report_modal = Some(format_unsupported_mouse_issue(
                        &dump,
                        &topology,
                        detected,
                        win_ver.as_deref(),
                    ));
                }
                Err(err) => {
                    self.notice = Some((Severity::Bad, format!("HID enumeration failed: {err}")));
                }
            }
        }

        ui.add_space(10.0);

        egui::ScrollArea::vertical()
            .id_salt("keyboard_view_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let scroll_w = (content_w - 8.0).max(280.0);
                ui.set_max_width(scroll_w);

                if let Some((sev, msg)) = self.notice.clone() {
                    semantic_banner(ui, p, mode, scroll_w, sev, &msg);
                    ui.add_space(10.0);
                }

                let Some(kb) = active_kb else {
                    self.render_empty_state(ui, p, scroll_w, runtime);
                    return;
                };

                // Optional multi-keyboard switcher if user has > 1 Wooting board (e.g. 60HE+ + UwU)
                if boards.len() > 1 {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Keyboard:").size(12.0).color(p.text_muted));
                        for (idx, b) in boards.iter().enumerate() {
                            let label = format!("{} ({})", b.name, b.vid_pid);
                            let btn = if idx == self.selected_index {
                                primary_btn(&label, p)
                            } else {
                                secondary_btn(&label, p)
                            };
                            if ui.add(btn).clicked() {
                                self.selected_index = idx;
                                self.synced_key = None;
                            }
                        }
                    });
                    ui.add_space(10.0);
                }

                // --- 4 HERO TELEMETRY TILES ---
                self.render_hero_strip(ui, p, scroll_w, &kb);
                ui.add_space(12.0);

                // --- ONBOARD PROFILE SWITCHER (RAM-ONLY, ZERO FLASH WEAR) ---
                self.render_profile_switcher_card(ui, p, scroll_w, &kb, runtime);
                ui.add_space(12.0);

                // --- LIVE ANALOG KEY-TRAVEL VISUALIZER ---
                self.render_analog_stream_card(ctx, ui, p, mode, scroll_w, &kb);
                ui.add_space(12.0);

                // --- TWO-COLUMN DETAIL CARDS: PROFILE TUNING & AKC | GLOBAL SETTINGS, RGB & DIAGNOSTICS ---
                if scroll_w < 840.0 {
                    self.render_profile_tuning_card(ui, p, mode, scroll_w, &kb, runtime);
                    ui.add_space(12.0);
                    self.render_device_settings_card(ui, p, scroll_w, &kb, runtime);
                } else {
                    let gap = 12.0;
                    let half_w = ((scroll_w - gap) * 0.5).max(260.0);
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        ui.allocate_ui_with_layout(
                            Vec2::new(half_w, 0.0),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                self.render_profile_tuning_card(ui, p, mode, half_w, &kb, runtime);
                            },
                        );
                        ui.allocate_ui_with_layout(
                            Vec2::new(half_w, 0.0),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                self.render_device_settings_card(ui, p, half_w, &kb, runtime);
                            },
                        );
                    });
                }

                ui.add_space(8.0);
            });

        // --- SERIAL-REDACTED KEYBOARD REPORT MODAL ---
        let mut close_report = false;
        if let Some(md) = self.report_modal.clone() {
            let report_w = (ctx.screen_rect().width() - 48.0).clamp(380.0, 620.0);
            egui::Window::new("Report Keyboard / HID Devices (Serial-Redacted)")
                .collapsible(false)
                .resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .default_width(report_w)
                .max_width(report_w)
                .frame(
                    egui::Frame::NONE
                        .fill(p.surface)
                        .stroke(Stroke::new(1.5_f32, p.text))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(16)),
                )
                .show(ctx, |ui| {
                    let mut preview = md.clone();
                    egui::ScrollArea::vertical()
                        .id_salt("kb_report_scroll")
                        .max_height(280.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut preview)
                                    .code_editor()
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    ui.add_space(10.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(secondary_btn("Close", p)).clicked() {
                            close_report = true;
                        }
                        if ui.add(secondary_btn("Save to Exports", p)).clicked() {
                            let dir = AppPaths::default_user_dir().exports_dir();
                            let path = dir.join(format!(
                                "polled_keyboard_report_{}.md",
                                Local::now().format("%Y%m%d_%H%M%S")
                            ));
                            match std::fs::create_dir_all(&dir)
                                .and_then(|()| std::fs::write(&path, &md))
                            {
                                Ok(()) => {
                                    self.notice = Some((
                                        Severity::Ok,
                                        format!("Saved report to {}", path.display()),
                                    ));
                                }
                                Err(err) => {
                                    self.notice = Some((Severity::Bad, err.to_string()));
                                }
                            }
                        }
                        if ui.add(primary_btn("Copy Markdown", p)).clicked() {
                            ctx.copy_text(md.clone());
                            self.notice = Some((
                                Severity::Ok,
                                "Copied serial-redacted HID report to clipboard.".to_string(),
                            ));
                        }
                    });
                });
        }
        if close_report {
            self.report_modal = None;
        }
    }

    fn render_empty_state(
        &self,
        ui: &mut egui::Ui,
        p: Palette,
        card_w: f32,
        runtime: &ProfileRuntimeHandle,
    ) {
        card_frame(ui, p, card_w, |ui| {
            ui.label(
                RichText::new("No Wooting Keyboard Detected")
                    .size(15.0)
                    .strong()
                    .color(p.text),
            );
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Connect a supported Wooting keyboard over USB to inspect live actuation, Rapid Trigger, Tachyon mode, per-key overrides, SOCD / AKC bindings, onboard profile names, RGB core, global settings, and real-time analog key travel.",
                    )
                    .size(12.5)
                    .color(p.text_muted),
                )
                .wrap(),
            );
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Supported models: Wooting 60HE, 60HE+, 60HE v2, 80HE, 80HE+, Two HE (ARM / AVR), Two Lekker, UwU / UwU RGB, One, and Two. Close Wootility (desktop or web tab) if it is currently open.",
                    )
                    .size(11.5)
                    .color(p.text_muted),
                )
                .wrap(),
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.add(primary_btn("Redetect Keyboards", p)).clicked() {
                    runtime.send(ProfileRuntimeCmd::RefreshWooting);
                }
            });
        });
    }

    fn render_hero_strip(
        &self,
        ui: &mut egui::Ui,
        p: Palette,
        card_w: f32,
        kb: &WootingKeyboardInfo,
    ) {
        let active_val = kb
            .active_profile
            .map(|idx| kb.profile_display_name(idx))
            .unwrap_or_else(|| "—".to_string());
        let active_sub = format!(
            "{} onboard slot{} · {}",
            kb.profiles.len(),
            if kb.profiles.len() == 1 { "" } else { "s" },
            kb.layout.as_deref().unwrap_or("USB HID")
        );

        let ap = kb.analog_profile.as_ref();
        let act_val = ap
            .and_then(|a| a.actuation_mm)
            .map(|mm| format!("{mm:.2} mm"))
            .unwrap_or_else(|| "—".to_string());
        let act_sub = if kb.per_key_actuation.is_empty() {
            "Profile base actuation".to_string()
        } else {
            format!(
                "{} per-key override{}",
                kb.per_key_actuation.len(),
                if kb.per_key_actuation.len() == 1 {
                    ""
                } else {
                    "s"
                }
            )
        };

        let (rt_val, rt_col) = match ap.and_then(|a| a.rapid_trigger_enabled) {
            Some(true) => {
                let s = ap
                    .and_then(|a| a.rapid_trigger_sensitivity_mm)
                    .map(|mm| format!("ON · {mm:.2} mm"))
                    .unwrap_or_else(|| "ON".to_string());
                (s, p.ok)
            }
            Some(false) => ("OFF".to_string(), p.text_muted),
            None => ("—".to_string(), p.text),
        };
        let rt_keys: usize = kb
            .per_key_rapid_trigger
            .iter()
            .map(|g| g.key_labels.len())
            .sum();
        let rt_sub = if rt_keys > 0 {
            format!(
                "{rt_keys} per-key RT override{} · {} AKC",
                if rt_keys == 1 { "" } else { "s" },
                kb.advanced_keys.len()
            )
        } else if !kb.advanced_keys.is_empty() {
            format!("{} advanced key(s) active", kb.advanced_keys.len())
        } else {
            match ap.and_then(|a| a.strict_actuation_range) {
                Some(true) => "Strict actuation range".to_string(),
                Some(false) => "Continuous range".to_string(),
                None => "Base Rapid Trigger".to_string(),
            }
        };

        let (tach_val, tach_col) = match ap.and_then(|a| a.tachyon_enabled) {
            Some(true) => ("Tachyon ON".to_string(), p.ok),
            Some(false) => ("Tachyon OFF".to_string(), p.text),
            None => ("—".to_string(), p.text),
        };
        let mut tach_parts = Vec::new();
        if let Some(tr) = ap.and_then(|a| a.tick_rate) {
            tach_parts.push(format!("Tick {tr}"));
        }
        if let Some(pct) = kb.rgb.as_ref().and_then(|r| r.brightness_pct) {
            tach_parts.push(format!("RGB {pct}%"));
        }
        let tach_sub = if tach_parts.is_empty() {
            kb.switch_tech.clone()
        } else {
            tach_parts.join(" · ")
        };

        let tiles = [
            ("ACTIVE PROFILE", active_val, active_sub, p.text),
            ("ACTUATION", act_val, act_sub, p.text),
            ("RAPID TRIGGER", rt_val, rt_sub, rt_col),
            ("TACHYON & RGB", tach_val, tach_sub, tach_col),
        ];

        let gap = 10.0;
        if card_w < 820.0 {
            let half = ((card_w - gap) * 0.5).max(160.0);
            for (row_idx, pair) in tiles.chunks(2).enumerate() {
                if row_idx > 0 {
                    ui.add_space(8.0);
                }
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for (lbl, val, sub, col) in pair {
                        render_kpi_tile(ui, p, half, lbl, val, sub, *col);
                    }
                });
            }
        } else {
            let tile_w = ((card_w - gap * 3.0) / 4.0).max(150.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (lbl, val, sub, col) in &tiles {
                    render_kpi_tile(ui, p, tile_w, lbl, val, sub, *col);
                }
            });
        }
    }

    fn render_profile_switcher_card(
        &self,
        ui: &mut egui::Ui,
        p: Palette,
        card_w: f32,
        kb: &WootingKeyboardInfo,
        runtime: &ProfileRuntimeHandle,
    ) {
        card_frame(ui, p, card_w, |ui| {
            ui.label(
                RichText::new("Onboard Profile Switcher")
                    .size(14.0)
                    .strong()
                    .color(p.text),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Volatile RAM switch · zero flash wear · bind per-game in Profiles -> Keyboard",
                    )
                    .size(11.5)
                    .color(p.text_muted),
                )
                .wrap(),
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                for slot in &kb.profiles {
                    let is_active = kb.active_profile == Some(slot.index);
                    let lbl = format!("P{} · {}", slot.index + 1, slot.name);
                    let btn = if is_active {
                        primary_btn(&lbl, p)
                    } else {
                        secondary_btn(&lbl, p)
                    };
                    if ui.add(btn).clicked() && !is_active {
                        runtime.send(ProfileRuntimeCmd::SwitchWootingProfile {
                            device_id: Some(kb.id.clone()),
                            slot: slot.index,
                        });
                    }
                }
            });
        });
    }

    fn render_analog_stream_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        p: Palette,
        mode: ThemeMode,
        card_w: f32,
        kb: &WootingKeyboardInfo,
    ) {
        let inner_w = (card_w - 24.0).max(160.0);
        let streaming = self.analog_stream.is_some();
        let base_act_mm = kb
            .analog_profile
            .as_ref()
            .and_then(|a| a.actuation_mm)
            .unwrap_or(self.draft_actuation_mm);

        card_frame(ui, p, card_w, |ui| {
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if streaming {
                        if ui.add(primary_btn("Stop Analog Test", p)).clicked() {
                            self.stop_analog_stream();
                        }
                    } else if ui.add(primary_btn("Start Analog Test", p)).clicked() {
                        self.stream_error = None;
                        self.smoothed_keys.clear();
                        self.last_analog_frame = Instant::now();
                        let ctx_bg = ctx.clone();
                        self.analog_stream = Some(WootingAnalogStream::start(move || {
                            ctx_bg.request_repaint();
                        }));
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("Live Analog Key-Travel Visualizer")
                                .size(14.0)
                                .strong()
                                .color(p.text),
                        );
                    });
                });
            });
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Real-time 0.00-4.00 mm Hall-Effect depth (10-bit V2 0xFF53 / V1 0xFF54 · live actuation & peak hold · zero keystrokes stored)",
                    )
                    .size(11.5)
                    .color(p.text_muted),
                )
                .wrap(),
            );

            if let Some(err) = self.stream_error.clone() {
                ui.add_space(8.0);
                semantic_banner(ui, p, mode, inner_w, Severity::Warn, &err);
            }

            if streaming {
                ui.add_space(10.0);
                if self.smoothed_keys.is_empty() {
                    ui.label(
                        RichText::new(
                            "Press and hold one or multiple keys on your Wooting keyboard to see smooth real-time actuation depth...",
                        )
                        .size(12.0)
                        .color(p.text_muted),
                    );
                } else {
                    for entry in self.smoothed_keys.values() {
                        // Use per-key actuation override if configured for this physical switch.
                        let key_act_mm = entry
                            .matrix_index
                            .and_then(|idx| {
                                kb.per_key_actuation
                                    .iter()
                                    .find(|ov| ov.matrix_index == idx)
                                    .map(|ov| ov.actuation_mm)
                            })
                            .unwrap_or(base_act_mm);

                        let is_actuated = entry
                            .actuated
                            .unwrap_or(entry.display_mm >= key_act_mm && entry.is_pressed);

                        ui.horizontal(|ui| {
                            let badge_w = 66.0;
                            let val_w = if inner_w < 500.0 { 116.0 } else { 166.0 };
                            let bar_w = (inner_w - badge_w - val_w - 18.0).max(80.0);

                            // Keycap pill
                            let (badge_rect, _) = ui.allocate_exact_size(
                                Vec2::new(badge_w, 24.0),
                                egui::Sense::hover(),
                            );
                            let bp = ui.painter_at(badge_rect);
                            bp.rect_filled(badge_rect, CornerRadius::same(5), p.surface_alt);
                            bp.rect_stroke(
                                badge_rect,
                                CornerRadius::same(5),
                                Stroke::new(1.0_f32, if is_actuated { p.ok } else { p.border }),
                                egui::StrokeKind::Inside,
                            );
                            bp.text(
                                badge_rect.center(),
                                Align2::CENTER_CENTER,
                                &entry.label,
                                FontId::monospace(11.5),
                                if entry.is_pressed {
                                    p.text
                                } else {
                                    p.text_muted
                                },
                            );

                            // Smooth depth progress bar with 1/2/3mm ticks, peak marker, and per-key actuation line
                            let (bar_rect, _) = ui
                                .allocate_exact_size(Vec2::new(bar_w, 18.0), egui::Sense::hover());
                            let painter = ui.painter_at(bar_rect);
                            painter.rect_filled(bar_rect, CornerRadius::same(4), p.surface_alt);

                            // Subtle 1.0 / 2.0 / 3.0 mm grid ticks
                            for mm_tick in [1.0_f32, 2.0, 3.0] {
                                let tx = bar_rect.left() + bar_rect.width() * (mm_tick / 4.0);
                                painter.vline(
                                    tx,
                                    (bar_rect.top() + 2.0)..=(bar_rect.bottom() - 2.0),
                                    Stroke::new(1.0_f32, p.border_subtle),
                                );
                            }

                            // Smoothly interpolated live fill
                            let frac = (entry.display_mm / 4.0).clamp(0.0, 1.0);
                            if frac > 0.002 {
                                let fill_rect = Rect::from_min_max(
                                    bar_rect.min,
                                    Pos2::new(
                                        bar_rect.left() + bar_rect.width() * frac,
                                        bar_rect.bottom(),
                                    ),
                                );
                                let fill_col = if is_actuated {
                                    p.text
                                } else if entry.is_pressed {
                                    p.text_muted
                                } else {
                                    p.border
                                };
                                painter.rect_filled(fill_rect, CornerRadius::same(4), fill_col);
                            }

                            // Peak hold marker line
                            let peak_frac = (entry.peak_mm / 4.0).clamp(0.0, 1.0);
                            if peak_frac > frac + 0.015 {
                                let px = bar_rect.left() + bar_rect.width() * peak_frac;
                                painter.vline(
                                    px,
                                    (bar_rect.top() + 1.0)..=(bar_rect.bottom() - 1.0),
                                    Stroke::new(1.5_f32, p.text_muted),
                                );
                            }

                            // Per-key (or base) actuation threshold tick line
                            let act_x = bar_rect.left()
                                + bar_rect.width() * (key_act_mm / 4.0).clamp(0.0, 1.0);
                            painter.vline(
                                act_x,
                                bar_rect.top()..=bar_rect.bottom(),
                                Stroke::new(1.5_f32, p.ok),
                            );

                            painter.rect_stroke(
                                bar_rect,
                                CornerRadius::same(4),
                                Stroke::new(1.0_f32, p.border_subtle),
                                egui::StrokeKind::Inside,
                            );

                            // Crisp, fixed-width readable readout (smoothed mm + peak mm + ACT flag)
                            let shown_mm = if entry.is_pressed {
                                entry.display_mm
                            } else {
                                0.0
                            };
                            let readout = if inner_w < 500.0 {
                                format!("{shown_mm:4.2}mm  pk {pk:4.2}", pk = entry.peak_mm)
                            } else {
                                let tag = if is_actuated { " ACT" } else { "    " };
                                format!("{shown_mm:4.2} mm · pk {pk:4.2}{tag}", pk = entry.peak_mm)
                            };
                            ui.add_sized(
                                Vec2::new(val_w, 20.0),
                                egui::Label::new(
                                    RichText::new(readout).size(11.5).monospace().color(
                                        if is_actuated {
                                            p.ok
                                        } else if entry.is_pressed {
                                            p.text
                                        } else {
                                            p.text_muted
                                        },
                                    ),
                                )
                                .extend(),
                            );
                        });
                        ui.add_space(4.0);
                    }
                }
            }
        });
    }

    fn render_profile_tuning_card(
        &mut self,
        ui: &mut egui::Ui,
        p: Palette,
        mode: ThemeMode,
        card_w: f32,
        kb: &WootingKeyboardInfo,
        runtime: &ProfileRuntimeHandle,
    ) {
        let inner_w = (card_w - 24.0).max(160.0);
        card_frame(ui, p, card_w, |ui| {
            ui.label(
                RichText::new("Active Profile Tuning & Advanced Keys")
                    .size(14.0)
                    .strong()
                    .color(p.text),
            );
            ui.add_space(8.0);

            if let Some(ap) = &kb.analog_profile {
                egui::Grid::new("wooting_analog_readout_grid")
                    .num_columns(2)
                    .spacing([14.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Base Actuation").color(p.text_muted));
                        if kb.supports_arm_writes {
                            ui.add(
                                egui::DragValue::new(&mut self.draft_actuation_mm)
                                    .range(0.10..=4.00)
                                    .speed(0.05)
                                    .fixed_decimals(2)
                                    .suffix(" mm"),
                            );
                        } else {
                            ui.label(
                                ap.actuation_mm
                                    .map(|m| format!("{m:.2} mm"))
                                    .unwrap_or_else(|| "—".to_string()),
                            );
                        }
                        ui.end_row();

                        ui.label(RichText::new("Rapid Trigger").color(p.text_muted));
                        if kb.supports_arm_writes {
                            ui.checkbox(&mut self.draft_rt_enabled, "Enabled");
                        } else {
                            ui.label(match ap.rapid_trigger_enabled {
                                Some(true) => "Enabled",
                                Some(false) => "Disabled",
                                None => "—",
                            });
                        }
                        ui.end_row();

                        ui.label(RichText::new("RT Sensitivity").color(p.text_muted));
                        if kb.supports_arm_writes {
                            ui.add_enabled(
                                self.draft_rt_enabled,
                                egui::DragValue::new(&mut self.draft_rt_sens_mm)
                                    .range(0.10..=4.00)
                                    .speed(0.05)
                                    .fixed_decimals(2)
                                    .suffix(" mm"),
                            );
                        } else {
                            ui.label(
                                ap.rapid_trigger_sensitivity_mm
                                    .map(|m| format!("{m:.2} mm"))
                                    .unwrap_or_else(|| "—".to_string()),
                            );
                        }
                        ui.end_row();

                        ui.label(RichText::new("Tachyon / Tick Rate").color(p.text_muted));
                        let tach = match ap.tachyon_enabled {
                            Some(true) => "Tachyon ON",
                            Some(false) => "Tachyon OFF",
                            None => "—",
                        };
                        let tick = ap
                            .tick_rate
                            .map(|t| format!(" · Tick {t}"))
                            .unwrap_or_default();
                        ui.label(RichText::new(format!("{tach}{tick}")).color(p.text));
                        ui.end_row();

                        ui.label(RichText::new("Digital Output").color(p.text_muted));
                        ui.label(
                            RichText::new(match ap.digital_output_enabled {
                                Some(true) => "Enabled",
                                Some(false) => "Disabled",
                                None => "—",
                            })
                            .color(p.text),
                        );
                        ui.end_row();
                    });

                if kb.supports_arm_writes {
                    ui.add_space(8.0);
                    if self.confirm_analog_flash {
                        semantic_banner(
                            ui,
                            p,
                            mode,
                            inner_w,
                            Severity::Warn,
                            &format!(
                                "Write {:.2} mm actuation & Rapid Trigger ({}, {:.2} mm) to onboard flash on active slot {}? Polled preserves all other fields and verifies via read-back.",
                                self.draft_actuation_mm,
                                if self.draft_rt_enabled { "ON" } else { "OFF" },
                                self.draft_rt_sens_mm,
                                kb.active_profile.map(|i| kb.profile_display_name(i)).unwrap_or_else(|| "P1".to_string())
                            ),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.add(primary_btn("Confirm Flash Save", p)).clicked() {
                                runtime.send(ProfileRuntimeCmd::WriteWootingAnalogBase {
                                    device_id: Some(kb.id.clone()),
                                    patch: WootingAnalogBasePatch {
                                        actuation_mm: Some(self.draft_actuation_mm),
                                        rapid_trigger_enabled: Some(self.draft_rt_enabled),
                                        rapid_trigger_sensitivity_mm: Some(self.draft_rt_sens_mm),
                                    },
                                });
                                self.confirm_analog_flash = false;
                            }
                            if ui.add(secondary_btn("Cancel", p)).clicked() {
                                self.confirm_analog_flash = false;
                            }
                        });
                    } else if ui
                        .add(secondary_btn("Save Base Analog to Active Slot...", p))
                        .clicked()
                    {
                        self.confirm_analog_flash = true;
                    }
                }
            } else {
                ui.label(
                    RichText::new("Base analog profile fields were not returned by this firmware.")
                        .size(12.0)
                        .color(p.text_muted),
                );
            }

            // Per-key Actuation Overrides (0x31)
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "PER-KEY ACTUATION OVERRIDES ({})",
                    kb.per_key_actuation.len()
                ))
                .size(10.5)
                .strong()
                .color(p.text_muted),
            );
            ui.add_space(4.0);
            if kb.per_key_actuation.is_empty() {
                ui.label(
                    RichText::new("None — all keys follow profile base actuation.")
                        .size(11.5)
                        .color(p.text_muted),
                );
            } else {
                ui.horizontal_wrapped(|ui| {
                    for ov in &kb.per_key_actuation {
                        mono_chip(ui, p, &format!("{} {:.2}mm", ov.key_label, ov.actuation_mm));
                    }
                });
            }

            // Per-key Rapid Trigger Groups (0x3D)
            ui.add_space(10.0);
            ui.label(
                RichText::new(format!(
                    "PER-KEY RAPID TRIGGER GROUPS ({})",
                    kb.per_key_rapid_trigger.len()
                ))
                .size(10.5)
                .strong()
                .color(p.text_muted),
            );
            ui.add_space(4.0);
            if kb.per_key_rapid_trigger.is_empty() {
                ui.label(
                    RichText::new("None — all keys follow base Rapid Trigger setting.")
                        .size(11.5)
                        .color(p.text_muted),
                );
            } else {
                for g in &kb.per_key_rapid_trigger {
                    let detail = if let Some(mm) = g.primary_sensitivity_mm {
                        let cont = match g.continuous_mode {
                            Some(true) => " · Continuous",
                            Some(false) => " · Strict",
                            None => "",
                        };
                        format!("{mm:.2} mm{cont}")
                    } else if let Some(en) = g.enabled {
                        if en { "Enabled (base)" } else { "Disabled" }.to_string()
                    } else {
                        "Custom".to_string()
                    };
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("{} -> {detail}", g.key_labels.join(", ")))
                                .size(11.5)
                                .monospace()
                                .color(p.text),
                        )
                        .wrap(),
                    );
                }
            }

            // Advanced Keys / AKC (SOCD, Rappy Snappy, DKS, Mod Tap, Toggle)
            ui.add_space(10.0);
            ui.label(
                RichText::new(format!("ADVANCED KEYS / AKC ({})", kb.advanced_keys.len()))
                    .size(10.5)
                    .strong()
                    .color(p.text_muted),
            );
            ui.add_space(4.0);
            if kb.advanced_keys.is_empty() {
                ui.label(
                    RichText::new(
                        "No SOCD, Rappy Snappy, DKS, Mod Tap, or Toggle bindings on active slot.",
                    )
                    .size(11.5)
                    .color(p.text_muted),
                );
            } else {
                for akc in &kb.advanced_keys {
                    ui.horizontal_wrapped(|ui| {
                        mono_chip(ui, p, &akc.kind);
                        ui.label(
                            RichText::new(format!("{} (L{})", akc.detail, akc.layer))
                                .size(11.5)
                                .color(p.text),
                        );
                    });
                }
            }
        });
    }

    fn render_device_settings_card(
        &mut self,
        ui: &mut egui::Ui,
        p: Palette,
        card_w: f32,
        kb: &WootingKeyboardInfo,
        runtime: &ProfileRuntimeHandle,
    ) {
        card_frame(ui, p, card_w, |ui| {
            ui.label(
                RichText::new("Global Device Settings, RGB & Diagnostics")
                    .size(14.0)
                    .strong()
                    .color(p.text),
            );
            ui.add_space(8.0);

            if let Some(gs) = &kb.global_settings {
                egui::Grid::new("wooting_global_settings_grid")
                    .num_columns(2)
                    .spacing([14.0, 6.0])
                    .show(ui, |ui| {
                        if let Some(ref dn) = gs.device_name {
                            ui.label(RichText::new("Device Name").color(p.text_muted));
                            ui.label(RichText::new(dn).color(p.text));
                            ui.end_row();
                        }

                        ui.label(RichText::new("Debounce Filter").color(p.text_muted));
                        if kb.supports_arm_writes {
                            ui.checkbox(&mut self.draft_debounce_filter, "Enabled");
                        } else {
                            ui.label(match gs.debounce_filter {
                                Some(true) => "Enabled",
                                Some(false) => "Disabled",
                                None => "—",
                            });
                        }
                        ui.end_row();

                        ui.label(RichText::new("Invert Num Lock").color(p.text_muted));
                        if kb.supports_arm_writes {
                            ui.checkbox(&mut self.draft_invert_num_lock, "Enabled");
                        } else {
                            ui.label(match gs.invert_num_lock {
                                Some(true) => "Enabled",
                                Some(false) => "Disabled",
                                None => "—",
                            });
                        }
                        ui.end_row();

                        ui.label(RichText::new("RGB Dim Timer").color(p.text_muted));
                        if kb.supports_arm_writes {
                            ui.add(
                                egui::DragValue::new(&mut self.draft_rgb_dim_min)
                                    .range(0..=120)
                                    .suffix(" min"),
                            );
                        } else {
                            ui.label(
                                gs.rgb_dim_minutes
                                    .map(|m| format!("{m} min"))
                                    .unwrap_or_else(|| "—".to_string()),
                            );
                        }
                        ui.end_row();

                        ui.label(RichText::new("RGB Off Timer").color(p.text_muted));
                        if kb.supports_arm_writes {
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::DragValue::new(&mut self.draft_rgb_off_min)
                                        .range(0..=240)
                                        .suffix(" min"),
                                );
                                ui.checkbox(&mut self.draft_rgb_turn_off, "Turn Off");
                            });
                        } else {
                            ui.label(
                                gs.rgb_off_minutes
                                    .map(|m| format!("{m} min"))
                                    .unwrap_or_else(|| "—".to_string()),
                            );
                        }
                        ui.end_row();
                    });

                if kb.supports_arm_writes {
                    ui.add_space(8.0);
                    if ui
                        .add(secondary_btn(
                            "Apply Global Settings (Read-Back Verified)",
                            p,
                        ))
                        .clicked()
                    {
                        runtime.send(ProfileRuntimeCmd::WriteWootingGlobalSettings {
                            device_id: Some(kb.id.clone()),
                            patch: WootingGlobalSettingsPatch {
                                rgb_dim_minutes: Some(self.draft_rgb_dim_min),
                                rgb_off_minutes: Some(self.draft_rgb_off_min),
                                rgb_turn_off: Some(self.draft_rgb_turn_off),
                                invert_num_lock: Some(self.draft_invert_num_lock),
                                debounce_filter: Some(self.draft_debounce_filter),
                            },
                        });
                    }
                }
            } else {
                ui.label(
                    RichText::new("GlobalSettings (0x33) is available on ARM Wooting firmware.")
                        .size(11.5)
                        .color(p.text_muted),
                );
            }

            // RGB Profile Core (0x32)
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(
                RichText::new("RGB PROFILE CORE (READ-ONLY)")
                    .size(10.5)
                    .strong()
                    .color(p.text_muted),
            );
            ui.add_space(4.0);
            if let Some(rgb) = &kb.rgb {
                let b_str = rgb
                    .brightness_pct
                    .map(|pct| format!("{pct}%"))
                    .unwrap_or_else(|| "—".to_string());
                let effs = if rgb.effect_layers.is_empty() {
                    "Static / None".to_string()
                } else {
                    rgb.effect_layers.join(" + ")
                };
                ui.label(
                    RichText::new(format!("Brightness: {b_str}  ·  Layers: {effs}"))
                        .size(12.0)
                        .color(p.text),
                );
            } else {
                ui.label(
                    RichText::new("RGB core unavailable or non-RGB model.")
                        .size(11.5)
                        .color(p.text_muted),
                );
            }

            // Hardware Diagnostics
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(
                RichText::new("HARDWARE DIAGNOSTICS")
                    .size(10.5)
                    .strong()
                    .color(p.text_muted),
            );
            ui.add_space(6.0);
            let d = &kb.diagnostics;
            let mut diag_rows = vec![
                ("Protocol", kb.protocol.label().to_string()),
                (
                    "Chip / Switch",
                    format!("{} · {}", kb.chip_type, kb.switch_tech),
                ),
                (
                    "Firmware",
                    kb.firmware_version
                        .clone()
                        .unwrap_or_else(|| "Unknown".to_string()),
                ),
                (
                    "Physical Layout",
                    kb.layout.clone().unwrap_or_else(|| "Unknown".to_string()),
                ),
            ];
            if let Some(keys) = d.key_count {
                diag_rows.push(("Matrix Keys", keys.to_string()));
            }
            diag_rows.push((
                "Flash Health",
                match (
                    d.flash_chip_connected,
                    d.flash_used_pages,
                    d.flash_total_pages,
                ) {
                    (Some(true), Some(u), Some(t)) => format!("OK ({u}/{t} pages)"),
                    (Some(true), _, _) => "Connected (OK)".to_string(),
                    (Some(false), _, _) => "Disconnected".to_string(),
                    _ => "—".to_string(),
                },
            ));
            diag_rows.push((
                "Flash Default Slot",
                kb.flash_default_profile
                    .map(|i| kb.profile_display_name(i))
                    .unwrap_or_else(|| "—".to_string()),
            ));
            egui::Grid::new("wooting_diag_grid")
                .num_columns(2)
                .spacing([14.0, 5.0])
                .show(ui, |ui| {
                    for (k, v) in diag_rows {
                        ui.label(RichText::new(k).size(11.5).color(p.text_muted));
                        ui.label(RichText::new(v).size(11.5).monospace().color(p.text));
                        ui.end_row();
                    }
                });
        });
    }
}

fn card_frame(ui: &mut egui::Ui, p: Palette, outer_w: f32, body: impl FnOnce(&mut egui::Ui)) {
    let inner_w = (outer_w - 24.0).max(120.0);
    egui::Frame::NONE
        .fill(p.surface)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(inner_w);
            ui.set_max_width(inner_w);
            body(ui);
        });
}

fn render_kpi_tile(
    ui: &mut egui::Ui,
    p: Palette,
    tile_w: f32,
    label: &str,
    value: &str,
    sub: &str,
    val_col: Color32,
) {
    let inner_w = (tile_w - 22.0).max(80.0);
    egui::Frame::NONE
        .fill(p.surface)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(11, 10))
        .show(ui, |ui| {
            // Explicitly use top-down vertical layout so tiles inside `horizontal_top` stack cleanly.
            ui.allocate_ui_with_layout(
                Vec2::new(inner_w, 54.0),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.set_min_width(inner_w);
                    ui.set_max_width(inner_w);
                    ui.add(
                        egui::Label::new(
                            RichText::new(label).size(10.0).strong().color(p.text_muted),
                        )
                        .truncate(),
                    );
                    ui.add_space(3.0);
                    ui.add(
                        egui::Label::new(RichText::new(value).size(16.0).strong().color(val_col))
                            .truncate(),
                    );
                    ui.add_space(2.0);
                    ui.add(
                        egui::Label::new(RichText::new(sub).size(11.0).color(p.text_muted))
                            .truncate(),
                    );
                },
            );
        });
}

fn mono_chip(ui: &mut egui::Ui, p: Palette, text: &str) {
    // Use `TextWrapMode::Extend` on a non-interactive pill button so `horizontal_wrapped`
    // always measures full chip width and jumps to the next row instead of splitting letters vertically.
    ui.add(
        egui::Button::new(RichText::new(text).size(11.0).monospace().color(p.text))
            .wrap_mode(egui::TextWrapMode::Extend)
            .fill(p.surface_alt)
            .stroke(Stroke::new(1.0_f32, p.border))
            .corner_radius(CornerRadius::same(4))
            .sense(egui::Sense::hover()),
    );
}

fn primary_btn<'a>(label: &'a str, p: Palette) -> egui::Button<'a> {
    egui::Button::new(
        RichText::new(label)
            .size(12.0)
            .color(p.btn_primary_fg)
            .strong(),
    )
    .fill(p.btn_primary_bg)
    .stroke(Stroke::new(1.0_f32, p.btn_primary_bg))
    .corner_radius(CornerRadius::same(6))
}

fn secondary_btn<'a>(label: &'a str, p: Palette) -> egui::Button<'a> {
    egui::Button::new(RichText::new(label).size(12.0).color(p.text))
        .fill(p.surface_alt)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(6))
}

fn semantic_banner(
    ui: &mut egui::Ui,
    p: Palette,
    mode: ThemeMode,
    outer_w: f32,
    sev: Severity,
    text: &str,
) {
    let fg = p.severity_fg(sev);
    let bg = p.severity_bg(sev, mode);
    let inner_w = (outer_w - 22.0).max(100.0);
    egui::Frame::NONE
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, fg))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(11, 8))
        .show(ui, |ui| {
            ui.set_min_width(inner_w);
            ui.set_max_width(inner_w);
            ui.add(egui::Label::new(RichText::new(text).size(12.0).color(fg)).wrap());
        });
}
