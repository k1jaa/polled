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

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::time::{Duration, Instant};

use chrono::Local;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Margin, Pos2, Rect, RichText, Stroke, Vec2,
};
use polled_core::win::{SystemEnv, WindowsEnv};
use polled_core::{
    dump_hid_devices, format_unsupported_mouse_issue, AppPaths, KeyboardStatus, Severity,
    SupportTier, WootingAnalogBasePatch, WootingAnalogKeyReading, WootingAnalogStream,
    WootingGlobalSettingsPatch, WootingKeyboardInfo,
};

#[derive(Debug, Clone, PartialEq)]
enum GenericConfirmAction {
    SwitchProfile(u8),
    SetPollingRate(u16),
    SetBaseActuation {
        actuation_mm: f32,
        rt_enabled: Option<bool>,
        rt_mm: Option<f32>,
    },
    SetRapidTrigger(bool),
    SetSystemMode(bool),
    SetSocd(bool),
    SetDebounce {
        typing_mode: bool,
        debounce_ms: u8,
    },
}

use crate::profile_runtime::{ProfileRuntimeCmd, ProfileRuntimeHandle, ProfileRuntimeSnapshot};
use crate::theme::{Palette, ThemeMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardNavAction {
    OpenChatterTester,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PerKeyConfirmAction {
    SetMm(f32),
    ResetToBase,
}

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

#[derive(Debug, Clone)]
struct StrafeSample {
    direction: &'static str,
    /// Negative = overlap (ms both A & D actuated), Positive = gap/dead-time (ms), 0 = instant transition.
    signed_ms: f64,
}

#[derive(Debug, Default)]
struct CounterStrafeTracker {
    prev_a: bool,
    prev_d: bool,
    overlap_start: Option<(Instant, &'static str)>,
    last_release: Option<(Instant, char)>,
    samples: VecDeque<StrafeSample>,
}

impl CounterStrafeTracker {
    fn reset(&mut self) {
        self.prev_a = false;
        self.prev_d = false;
        self.overlap_start = None;
        self.last_release = None;
        self.samples.clear();
    }

    fn record(&mut self, a_act: bool, d_act: bool, now: Instant) {
        let a_rise = a_act && !self.prev_a;
        let a_fall = !a_act && self.prev_a;
        let d_rise = d_act && !self.prev_d;
        let d_fall = !d_act && self.prev_d;

        // Instant swap on the exact same packet (e.g. SOCD Last Input Priority or frame-perfect release)
        if a_fall && d_rise {
            self.push(StrafeSample {
                direction: "A->D",
                signed_ms: 0.0,
            });
            self.overlap_start = None;
            self.last_release = None;
        } else if d_fall && a_rise {
            self.push(StrafeSample {
                direction: "D->A",
                signed_ms: 0.0,
            });
            self.overlap_start = None;
            self.last_release = None;
        } else {
            // Overlap start: opposite key pressed while current key still actuated
            if d_rise && a_act {
                self.overlap_start = Some((now, "A->D"));
                self.last_release = None;
            } else if a_rise && d_act {
                self.overlap_start = Some((now, "D->A"));
                self.last_release = None;
            }

            // Overlap finish: first key finally released while opposite stays held
            if (a_fall && d_act) || (d_fall && a_act) {
                if let Some((t0, dir)) = self.overlap_start.take() {
                    let ms = now.saturating_duration_since(t0).as_secs_f64() * 1000.0;
                    if ms <= 500.0 {
                        self.push(StrafeSample {
                            direction: dir,
                            signed_ms: -ms,
                        });
                    }
                }
            }

            // Clean gap transition: first key released while opposite is up
            if a_fall && !d_act {
                self.overlap_start = None;
                self.last_release = Some((now, 'A'));
            } else if d_fall && !a_act {
                self.overlap_start = None;
                self.last_release = Some((now, 'D'));
            }

            // Opposite key pressed after clean release -> measure gap ms
            if d_rise && !a_act {
                if let Some((t0, 'A')) = self.last_release.take() {
                    let ms = now.saturating_duration_since(t0).as_secs_f64() * 1000.0;
                    if ms <= 500.0 {
                        self.push(StrafeSample {
                            direction: "A->D",
                            signed_ms: ms,
                        });
                    }
                }
            } else if a_rise && !d_act {
                if let Some((t0, 'D')) = self.last_release.take() {
                    let ms = now.saturating_duration_since(t0).as_secs_f64() * 1000.0;
                    if ms <= 500.0 {
                        self.push(StrafeSample {
                            direction: "D->A",
                            signed_ms: ms,
                        });
                    }
                }
            }
        }

        self.prev_a = a_act;
        self.prev_d = d_act;
    }

    fn push(&mut self, s: StrafeSample) {
        self.samples.push_back(s);
        while self.samples.len() > 20 {
            self.samples.pop_front();
        }
    }
}

pub struct KeyboardUiState {
    selected_index: usize,
    analog_stream: Option<WootingAnalogStream>,
    smoothed_keys: BTreeMap<u16, SmoothedAnalogKey>,
    last_analog_frame: Instant,
    strafe_tracker: CounterStrafeTracker,
    stream_error: Option<String>,
    // Draft values for ARM base analog tuning (synced when keyboard or active profile changes).
    synced_key: Option<(String, Option<u8>)>,
    draft_actuation_mm: f32,
    draft_rt_enabled: bool,
    draft_rt_sens_mm: f32,
    confirm_analog_flash: bool,
    // Interactive visual keymap per-key actuation selection & confirm-gated flash write.
    selected_keys: BTreeSet<u16>,
    draft_per_key_mm: f32,
    confirm_per_key_action: Option<PerKeyConfirmAction>,
    // Draft values for ARM global settings.
    draft_debounce_filter: bool,
    draft_invert_num_lock: bool,
    draft_rgb_dim_min: u32,
    draft_rgb_off_min: u32,
    draft_rgb_turn_off: bool,
    // Draft values & confirm gate for non-Wooting multi-brand keyboards (ImplementedFromDocs).
    draft_venom_typing: bool,
    draft_venom_debounce_ms: u8,
    pending_generic_confirm: Option<GenericConfirmAction>,
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
            strafe_tracker: CounterStrafeTracker::default(),
            stream_error: None,
            synced_key: None,
            draft_actuation_mm: 0.40,
            draft_rt_enabled: true,
            draft_rt_sens_mm: 0.15,
            confirm_analog_flash: false,
            selected_keys: BTreeSet::new(),
            draft_per_key_mm: 0.40,
            confirm_per_key_action: None,
            draft_debounce_filter: false,
            draft_invert_num_lock: false,
            draft_rgb_dim_min: 5,
            draft_rgb_off_min: 10,
            draft_rgb_turn_off: true,
            draft_venom_typing: false,
            draft_venom_debounce_ms: 5,
            pending_generic_confirm: None,
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
        self.strafe_tracker.reset();
    }

    fn update_smoothed_analog(
        &mut self,
        ctx: &egui::Context,
        live: &[WootingAnalogKeyReading],
        kb: Option<&WootingKeyboardInfo>,
    ) {
        let now = Instant::now();
        // Feed raw (unsmoothed) A & D hardware actuation state into Counter-Strafe Tracker
        // so overlap & gap timings have crisp 1 ms accuracy.
        let base_act = kb
            .and_then(|k| k.analog_profile.as_ref())
            .and_then(|a| a.actuation_mm)
            .unwrap_or(self.draft_actuation_mm);
        let a_act_thresh = kb
            .and_then(|k| k.per_key_actuation.iter().find(|o| o.matrix_index == 0x61))
            .map(|o| o.actuation_mm)
            .unwrap_or(base_act);
        let d_act_thresh = kb
            .and_then(|k| k.per_key_actuation.iter().find(|o| o.matrix_index == 0x63))
            .map(|o| o.actuation_mm)
            .unwrap_or(base_act);

        let a_is_act = live
            .iter()
            .find(|k| k.matrix_index == Some(0x61) || k.usage == 0x04)
            .is_some_and(|k| k.actuated.unwrap_or(k.travel_mm >= a_act_thresh));
        let d_is_act = live
            .iter()
            .find(|k| k.matrix_index == Some(0x63) || k.usage == 0x07)
            .is_some_and(|k| k.actuated.unwrap_or(k.travel_mm >= d_act_thresh));
        self.strafe_tracker.record(a_is_act, d_is_act, now);
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
        self.confirm_per_key_action = None;
        self.pending_generic_confirm = None;
        self.synced_key = None; // Force re-sync from refreshed hardware read-back.
        self.notice = Some((if ok { Severity::Ok } else { Severity::Warn }, message));
    }

    fn sync_generic_drafts_from(&mut self, kb: &KeyboardStatus) {
        let key = (kb.id.clone(), kb.active_profile);
        if self.synced_key.as_ref() == Some(&key) {
            return;
        }
        self.synced_key = Some(key);
        self.pending_generic_confirm = None;
        if let Some(mm) = kb.actuation_mm {
            self.draft_actuation_mm = (mm * 100.0).round() / 100.0;
        }
        if let Some(rt) = kb.rapid_trigger_enabled {
            self.draft_rt_enabled = rt;
        }
        if let Some(rt_mm) = kb.rapid_trigger_mm {
            self.draft_rt_sens_mm = (rt_mm * 100.0).round() / 100.0;
        }
        if let Some(db) = kb.debounce_mode.as_deref() {
            self.draft_venom_typing = db.eq_ignore_ascii_case("Typing");
        }
        if let Some(ms) = kb.debounce_ms {
            self.draft_venom_debounce_ms = ms.clamp(5, 40);
        }
    }

    fn sync_drafts_from(&mut self, kb: &WootingKeyboardInfo) {
        let key = (kb.id.clone(), kb.active_profile);
        if self.synced_key.as_ref() == Some(&key) {
            return;
        }
        self.synced_key = Some(key);
        self.confirm_analog_flash = false;
        self.confirm_per_key_action = None;
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
    ) -> Option<KeyboardNavAction> {
        let mut nav_action = None;
        let boards: &[KeyboardStatus] = snap.map(|s| s.keyboards.as_slice()).unwrap_or(&[]);
        if !boards.is_empty() && self.selected_index >= boards.len() {
            self.selected_index = 0;
        }
        let active_kb = boards.get(self.selected_index).cloned();
        let active_wooting = active_kb.as_ref().and_then(|k| k.wooting.as_ref());

        if let Some(stream) = &self.analog_stream {
            let (keys, err) = stream.snapshot();
            if let Some(e) = err {
                self.stream_error = Some(e);
                self.stop_analog_stream();
            } else {
                self.update_smoothed_analog(ctx, &keys, active_wooting);
            }
        }

        let content_w = ui.available_width().max(320.0);
        ui.set_max_width(content_w);

        if let Some(ref kb) = active_kb {
            if let Some(ref w) = kb.wooting {
                self.sync_drafts_from(w);
            } else {
                self.sync_generic_drafts_from(kb);
            }
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
                if ui
                    .add(secondary_btn("Chatter & Polling Test", p))
                    .on_hover_text(
                        "Open Bench -> Aim & Switches to test key chatter / double-typing and USB scan rate",
                    )
                    .clicked()
                {
                    nav_action = Some(KeyboardNavAction::OpenChatterTester);
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

                // Optional multi-keyboard switcher if user has > 1 supported board connected
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
                                self.pending_generic_confirm = None;
                            }
                        }
                    });
                    ui.add_space(10.0);
                }

                if let Some(ref wkb) = kb.wooting {
                    // --- FULL HARDWARE-VERIFIED WOOTING EXPERIENCE (100% PRESERVED) ---
                    self.render_hero_strip(ui, p, scroll_w, wkb);
                    ui.add_space(12.0);

                    self.render_profile_switcher_card(ui, p, scroll_w, wkb, runtime);
                    ui.add_space(12.0);

                    self.render_visual_keymap_card(ui, p, mode, scroll_w, wkb, runtime);
                    ui.add_space(12.0);

                    self.render_analog_stream_card(ctx, ui, p, mode, scroll_w, wkb);
                    ui.add_space(12.0);

                    if scroll_w < 840.0 {
                        self.render_profile_tuning_card(ui, p, mode, scroll_w, wkb, runtime);
                        ui.add_space(12.0);
                        self.render_device_settings_card(ui, p, scroll_w, wkb, runtime);
                    } else {
                        let gap = 12.0;
                        let half_w = ((scroll_w - gap) * 0.5).max(260.0);
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            ui.allocate_ui_with_layout(
                                Vec2::new(half_w, 0.0),
                                egui::Layout::top_down(egui::Align::LEFT),
                                |ui| {
                                    self.render_profile_tuning_card(
                                        ui, p, mode, half_w, wkb, runtime,
                                    );
                                },
                            );
                            ui.allocate_ui_with_layout(
                                Vec2::new(half_w, 0.0),
                                egui::Layout::top_down(egui::Align::LEFT),
                                |ui| {
                                    self.render_device_settings_card(ui, p, half_w, wkb, runtime);
                                },
                            );
                        });
                    }
                } else {
                    // --- MULTI-BRAND OPEN-SOURCE KEYBOARD DASHBOARD ---
                    self.render_generic_keyboard_dashboard(ui, p, mode, scroll_w, &kb, runtime);
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
        nav_action
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
                RichText::new("No Supported Keyboard Detected")
                    .size(15.0)
                    .strong()
                    .color(p.text),
            );
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Connect a supported keyboard over USB (or LIGHTSPEED receiver for Logitech) to inspect live settings, switch onboard profiles, and apply read-back verified tuning.",
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
                        "Supported brands: Wooting (60HE / 60HE+ / 80HE / Two HE / UwU - tested on hardware), Wuque Studio / SparkLink HE (BABAO60/80, ZOOM TIGA), Keychron Hall Effect (Q/K/V/J/C HE & TMR), SteelSeries Apex Pro TKL, Razer Keyboards (Huntsman V3 Pro / BlackWidow V4), Arbiter Studio (Polar 65/75 PRO, Nimbus 65), Logitech HID++ 2.0 (PRO X TKL RAPID, G915 X, G715), Kochi HB68, Geonworks Venom (60MX/80MX/WISH45-8K), and Universal QMK / VIA.",
                    )
                    .size(11.5)
                    .color(p.text_muted),
                )
                .wrap(),
            );
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Using a closed-firmware board (e.g. Geonworks Venom HE RISC-V or Tekkusai Titan 75 HE)? Click 'Report Keyboard' above to generate a serial-redacted HID dump.",
                    )
                    .size(11.0)
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

    fn render_generic_keyboard_dashboard(
        &mut self,
        ui: &mut egui::Ui,
        p: Palette,
        mode: ThemeMode,
        scroll_w: f32,
        kb: &KeyboardStatus,
        runtime: &ProfileRuntimeHandle,
    ) {
        if kb.support_tier == SupportTier::ImplementedFromDocs {
            let note = kb.status_note.as_deref().unwrap_or(
                "Implemented from verified open-source protocol documentation; not yet tested on physical hardware by the Polled author. All writes are confirm-gated and verified by read-back.",
            );
            semantic_banner(ui, p, mode, scroll_w, Severity::Info, note);
            ui.add_space(12.0);
        }

        // --- 4 HERO KPI TILES ---
        let gap = 10.0;
        let cols: usize = if scroll_w >= 760.0 { 4 } else { 2 };
        let tile_w = ((scroll_w - gap * (cols as f32 - 1.0)) / cols as f32).max(130.0);

        let t1_val = kb.brand.display_name().to_string();
        let t1_sub = kb.protocol_label.clone();

        let (t2_lbl, t2_val, t2_sub, t2_col) = if let Some(slot) = kb.active_profile {
            (
                "ACTIVE PROFILE",
                kb.profile_display_name(slot),
                format!("{} onboard slots", kb.profiles.len()),
                p.text,
            )
        } else if let Some(sys) = &kb.system_mode {
            (
                "SYSTEM MODE",
                sys.clone(),
                "Host OS layout mode".to_string(),
                p.info,
            )
        } else if let Some(db) = &kb.debounce_mode {
            (
                "DEBOUNCE MODE",
                db.clone(),
                kb.debounce_ms
                    .map(|m| format!("{m} ms threshold"))
                    .unwrap_or_else(|| "Venom 8K".to_string()),
                p.ok,
            )
        } else {
            (
                "FIRMWARE",
                kb.firmware_version
                    .as_deref()
                    .map(|v| format!("v{v}"))
                    .unwrap_or_else(|| "Detected".to_string()),
                kb.vid_pid.clone(),
                p.text,
            )
        };

        let (t3_lbl, t3_val, t3_sub, t3_col) = if let Some(mm) = kb.actuation_mm {
            let rt_sub = match (kb.rapid_trigger_enabled, kb.rapid_trigger_mm) {
                (Some(true), Some(rt)) => format!("RT ON · {rt:.2} mm"),
                (Some(true), None) => "Rapid Trigger ON".to_string(),
                (Some(false), _) => "Rapid Trigger OFF".to_string(),
                _ => kb.switch_tech.clone(),
            };
            ("ACTUATION", format!("{mm:.2} mm"), rt_sub, p.text)
        } else if let Some(hz) = kb.polling_rate_hz {
            (
                "POLLING RATE",
                format!("{hz} Hz"),
                if kb.capabilities.supports_polling_rate_write {
                    "Configurable".to_string()
                } else {
                    "Reported by firmware".to_string()
                },
                if hz >= 4000 { p.ok } else { p.text },
            )
        } else if let Some(rt) = kb.rapid_trigger_enabled {
            (
                "RAPID TRIGGER",
                if rt { "ON" } else { "OFF" }.to_string(),
                kb.socd_enabled
                    .map(|s| format!("SOCD {}", if s { "ON" } else { "OFF" }))
                    .unwrap_or_else(|| kb.switch_tech.clone()),
                if rt { p.ok } else { p.text_muted },
            )
        } else {
            (
                "SWITCH TECH",
                kb.switch_tech.clone(),
                kb.chip_type.clone(),
                p.text,
            )
        };

        let (t4_lbl, t4_val, t4_sub, t4_col) = if let Some(bat) = kb.battery {
            (
                "BATTERY",
                bat.compact_label(),
                if bat.charging {
                    "Charging".to_string()
                } else {
                    "Wireless".to_string()
                },
                if bat.percent <= 20 && !bat.charging {
                    p.warn
                } else {
                    p.ok
                },
            )
        } else if let Some(socd) = kb.socd_enabled {
            (
                "SOCD / PRIORITY",
                if socd { "ON" } else { "OFF" }.to_string(),
                kb.capabilities
                    .socd_label
                    .clone()
                    .unwrap_or_else(|| "Last Input Priority".to_string()),
                if socd { p.info } else { p.text_muted },
            )
        } else {
            (
                "SUPPORT TIER",
                kb.support_tier.label().to_string(),
                "Confirm-gated writes".to_string(),
                p.info,
            )
        };

        if cols == 4 {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                render_kpi_tile(ui, p, tile_w, "BRAND & PROTOCOL", &t1_val, &t1_sub, p.text);
                render_kpi_tile(ui, p, tile_w, t2_lbl, &t2_val, &t2_sub, t2_col);
                render_kpi_tile(ui, p, tile_w, t3_lbl, &t3_val, &t3_sub, t3_col);
                render_kpi_tile(ui, p, tile_w, t4_lbl, &t4_val, &t4_sub, t4_col);
            });
        } else {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                render_kpi_tile(ui, p, tile_w, "BRAND & PROTOCOL", &t1_val, &t1_sub, p.text);
                render_kpi_tile(ui, p, tile_w, t2_lbl, &t2_val, &t2_sub, t2_col);
            });
            ui.add_space(gap);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                render_kpi_tile(ui, p, tile_w, t3_lbl, &t3_val, &t3_sub, t3_col);
                render_kpi_tile(ui, p, tile_w, t4_lbl, &t4_val, &t4_sub, t4_col);
            });
        }
        ui.add_space(12.0);

        // --- ONBOARD PROFILE SWITCHER (Wuque / Keychron HE / Arbiter PRO) ---
        if kb.capabilities.supports_profile_switch && !kb.profiles.is_empty() {
            card_frame(ui, p, scroll_w, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Onboard Profile Switcher")
                            .size(13.5)
                            .strong()
                            .color(p.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("Confirm-gated · bind per-game in Profiles -> Keyboard")
                                .size(11.0)
                                .color(p.text_muted),
                        );
                    });
                });
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    for slot in &kb.profiles {
                        let label = format!("P{} · {}", slot.index + 1, slot.name);
                        let is_active = kb.active_profile == Some(slot.index);
                        let btn = if is_active {
                            primary_btn(&label, p)
                        } else {
                            secondary_btn(&label, p)
                        };
                        if ui.add(btn).clicked() && !is_active {
                            self.pending_generic_confirm =
                                Some(GenericConfirmAction::SwitchProfile(slot.index));
                        }
                    }
                });
            });
            ui.add_space(12.0);
        }

        // --- TWO-COLUMN CONTROLS & DIAGNOSTICS ---
        let has_controls = kb.capabilities.supports_polling_rate_write
            || kb.capabilities.supports_base_actuation_write
            || kb.capabilities.supports_rapid_trigger_toggle
            || kb.capabilities.supports_socd_toggle
            || kb.capabilities.supports_system_mode_toggle
            || kb.capabilities.supports_debounce_tuning;

        if has_controls && scroll_w >= 820.0 {
            let col_gap = 12.0;
            let half_w = ((scroll_w - col_gap) * 0.5).max(260.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = col_gap;
                ui.allocate_ui_with_layout(
                    Vec2::new(half_w, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        self.render_generic_controls_card(ui, p, mode, half_w, kb, runtime);
                    },
                );
                ui.allocate_ui_with_layout(
                    Vec2::new(half_w, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        self.render_generic_diagnostics_card(ui, p, half_w, kb);
                    },
                );
            });
        } else {
            if has_controls {
                self.render_generic_controls_card(ui, p, mode, scroll_w, kb, runtime);
                ui.add_space(12.0);
            }
            self.render_generic_diagnostics_card(ui, p, scroll_w, kb);
        }

        // --- CONFIRMATION BAR FOR IMPLEMENTED-FROM-DOCS WRITES ---
        if let Some(pending) = self.pending_generic_confirm.clone() {
            ui.add_space(12.0);
            let desc = match &pending {
                GenericConfirmAction::SwitchProfile(slot) => {
                    format!(
                        "Switch {} to onboard {}?",
                        kb.name,
                        kb.profile_display_name(*slot)
                    )
                }
                GenericConfirmAction::SetPollingRate(hz) => {
                    format!("Set {} polling rate to {hz} Hz?", kb.name)
                }
                GenericConfirmAction::SetBaseActuation {
                    actuation_mm,
                    rt_enabled,
                    ..
                } => format!(
                    "Write actuation {:.2} mm{} to {}?",
                    actuation_mm,
                    match rt_enabled {
                        Some(true) => " + Rapid Trigger ON",
                        Some(false) => " + Rapid Trigger OFF",
                        None => "",
                    },
                    kb.name
                ),
                GenericConfirmAction::SetRapidTrigger(on) => format!(
                    "Set {} Rapid Trigger to {}?",
                    kb.name,
                    if *on { "ON" } else { "OFF" }
                ),
                GenericConfirmAction::SetSystemMode(mac) => format!(
                    "Switch {} host mode to {}?",
                    kb.name,
                    if *mac { "macOS" } else { "Windows" }
                ),
                GenericConfirmAction::SetSocd(on) => format!(
                    "Set {} {} to {}?",
                    kb.name,
                    kb.capabilities.socd_label.as_deref().unwrap_or("SOCD"),
                    if *on { "ON" } else { "OFF" }
                ),
                GenericConfirmAction::SetDebounce {
                    typing_mode,
                    debounce_ms,
                } => format!(
                    "Save {} debounce to {} · {debounce_ms} ms (EEPROM)?",
                    kb.name,
                    if *typing_mode { "Typing" } else { "Gaming" }
                ),
            };
            card_frame(ui, p, scroll_w, |ui| {
                ui.label(
                    RichText::new("Confirm Hardware Write (Read-Back Verified)")
                        .size(13.0)
                        .strong()
                        .color(p.warn),
                );
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(desc).size(12.0).color(p.text)).wrap());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(primary_btn("Confirm & Write", p)).clicked() {
                        let dev_id = Some(kb.id.clone());
                        match pending {
                            GenericConfirmAction::SwitchProfile(slot) => {
                                runtime.send(ProfileRuntimeCmd::SwitchWootingProfile {
                                    device_id: dev_id,
                                    slot,
                                });
                            }
                            GenericConfirmAction::SetPollingRate(hz) => {
                                runtime.send(ProfileRuntimeCmd::SetKeyboardPollingRate {
                                    device_id: dev_id,
                                    hz,
                                });
                            }
                            GenericConfirmAction::SetBaseActuation {
                                actuation_mm,
                                rt_enabled,
                                rt_mm,
                            } => {
                                runtime.send(ProfileRuntimeCmd::SetKeyboardBaseActuation {
                                    device_id: dev_id,
                                    actuation_mm,
                                    rt_enabled,
                                    rt_mm,
                                });
                            }
                            GenericConfirmAction::SetRapidTrigger(enabled) => {
                                runtime.send(ProfileRuntimeCmd::SetKeyboardRapidTrigger {
                                    device_id: dev_id,
                                    enabled,
                                });
                            }
                            GenericConfirmAction::SetSystemMode(mac_mode) => {
                                runtime.send(ProfileRuntimeCmd::SetKeyboardSystemMode {
                                    device_id: dev_id,
                                    mac_mode,
                                });
                            }
                            GenericConfirmAction::SetSocd(enabled) => {
                                runtime.send(ProfileRuntimeCmd::SetKeyboardSocd {
                                    device_id: dev_id,
                                    enabled,
                                });
                            }
                            GenericConfirmAction::SetDebounce {
                                typing_mode,
                                debounce_ms,
                            } => {
                                runtime.send(ProfileRuntimeCmd::SetKeyboardDebounce {
                                    device_id: dev_id,
                                    typing_mode,
                                    debounce_ms,
                                });
                            }
                        }
                        self.pending_generic_confirm = None;
                    }
                    if ui.add(secondary_btn("Cancel", p)).clicked() {
                        self.pending_generic_confirm = None;
                    }
                });
            });
        }
    }

    fn render_generic_controls_card(
        &mut self,
        ui: &mut egui::Ui,
        p: Palette,
        _mode: ThemeMode,
        card_w: f32,
        kb: &KeyboardStatus,
        _runtime: &ProfileRuntimeHandle,
    ) {
        card_frame(ui, p, card_w, |ui| {
            ui.label(
                RichText::new("Keyboard Tuning & Controls")
                    .size(13.5)
                    .strong()
                    .color(p.text),
            );
            ui.add_space(8.0);

            if kb.capabilities.supports_polling_rate_write
                && !kb.capabilities.supported_polling_rates_hz.is_empty()
            {
                ui.label(
                    RichText::new("USB POLLING RATE")
                        .size(10.5)
                        .strong()
                        .color(p.text_muted),
                );
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    for &hz in &kb.capabilities.supported_polling_rates_hz {
                        let active = kb.polling_rate_hz == Some(hz);
                        let lbl = format!("{hz} Hz");
                        let btn = if active {
                            primary_btn(&lbl, p)
                        } else {
                            secondary_btn(&lbl, p)
                        };
                        if ui.add(btn).clicked() && !active {
                            self.pending_generic_confirm =
                                Some(GenericConfirmAction::SetPollingRate(hz));
                        }
                    }
                });
                ui.add_space(10.0);
            }

            if kb.capabilities.supports_base_actuation_write {
                let (min_mm, max_mm) = kb.actuation_range_mm.unwrap_or((0.1, 3.8));
                ui.label(
                    RichText::new("GLOBAL ACTUATION & RAPID TRIGGER")
                        .size(10.5)
                        .strong()
                        .color(p.text_muted),
                );
                ui.add_space(6.0);
                egui::Grid::new("generic_kb_actuation_grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Actuation").color(p.text_muted));
                        ui.add(
                            egui::DragValue::new(&mut self.draft_actuation_mm)
                                .range(min_mm..=max_mm)
                                .speed(0.05)
                                .fixed_decimals(2)
                                .suffix(" mm"),
                        );
                        ui.end_row();

                        if kb.capabilities.supports_rapid_trigger_toggle {
                            ui.label(RichText::new("Rapid Trigger").color(p.text_muted));
                            ui.checkbox(&mut self.draft_rt_enabled, "Enabled");
                            ui.end_row();
                        }

                        if kb.rapid_trigger_mm.is_some()
                            || kb.capabilities.supports_rapid_trigger_toggle
                        {
                            ui.label(RichText::new("RT Sensitivity").color(p.text_muted));
                            ui.add(
                                egui::DragValue::new(&mut self.draft_rt_sens_mm)
                                    .range(0.10..=2.40)
                                    .speed(0.05)
                                    .fixed_decimals(2)
                                    .suffix(" mm"),
                            );
                            ui.end_row();
                        }
                    });
                ui.add_space(6.0);
                if ui
                    .add(secondary_btn("Apply Actuation Settings...", p))
                    .clicked()
                {
                    self.pending_generic_confirm = Some(GenericConfirmAction::SetBaseActuation {
                        actuation_mm: self.draft_actuation_mm,
                        rt_enabled: kb
                            .capabilities
                            .supports_rapid_trigger_toggle
                            .then_some(self.draft_rt_enabled),
                        rt_mm: Some(self.draft_rt_sens_mm),
                    });
                }
                ui.add_space(10.0);
            } else if kb.capabilities.supports_rapid_trigger_toggle {
                ui.label(
                    RichText::new("FIRMWARE RAPID TRIGGER")
                        .size(10.5)
                        .strong()
                        .color(p.text_muted),
                );
                ui.add_space(4.0);
                let cur = kb.rapid_trigger_enabled.unwrap_or(false);
                ui.horizontal(|ui| {
                    if ui
                        .add(if cur {
                            primary_btn("Rapid Trigger ON", p)
                        } else {
                            secondary_btn("Rapid Trigger ON", p)
                        })
                        .clicked()
                        && !cur
                    {
                        self.pending_generic_confirm =
                            Some(GenericConfirmAction::SetRapidTrigger(true));
                    }
                    if ui
                        .add(if !cur {
                            primary_btn("Rapid Trigger OFF", p)
                        } else {
                            secondary_btn("Rapid Trigger OFF", p)
                        })
                        .clicked()
                        && cur
                    {
                        self.pending_generic_confirm =
                            Some(GenericConfirmAction::SetRapidTrigger(false));
                    }
                });
                ui.add_space(10.0);
            }

            if kb.capabilities.supports_socd_toggle {
                let lbl = kb
                    .capabilities
                    .socd_label
                    .as_deref()
                    .unwrap_or("SOCD / Key Priority");
                ui.label(
                    RichText::new(lbl.to_uppercase())
                        .size(10.5)
                        .strong()
                        .color(p.text_muted),
                );
                ui.add_space(4.0);
                let cur = kb.socd_enabled.unwrap_or(false);
                ui.horizontal(|ui| {
                    if ui
                        .add(if cur {
                            primary_btn("Enabled", p)
                        } else {
                            secondary_btn("Enabled", p)
                        })
                        .clicked()
                        && !cur
                    {
                        self.pending_generic_confirm = Some(GenericConfirmAction::SetSocd(true));
                    }
                    if ui
                        .add(if !cur {
                            primary_btn("Disabled", p)
                        } else {
                            secondary_btn("Disabled", p)
                        })
                        .clicked()
                        && cur
                    {
                        self.pending_generic_confirm = Some(GenericConfirmAction::SetSocd(false));
                    }
                });
                ui.add_space(10.0);
            }

            if kb.capabilities.supports_system_mode_toggle {
                ui.label(
                    RichText::new("HOST OS SYSTEM MODE")
                        .size(10.5)
                        .strong()
                        .color(p.text_muted),
                );
                ui.add_space(4.0);
                let is_mac = kb
                    .system_mode
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case("macOS"));
                ui.horizontal(|ui| {
                    if ui
                        .add(if !is_mac {
                            primary_btn("Windows Mode", p)
                        } else {
                            secondary_btn("Windows Mode", p)
                        })
                        .clicked()
                        && is_mac
                    {
                        self.pending_generic_confirm =
                            Some(GenericConfirmAction::SetSystemMode(false));
                    }
                    if ui
                        .add(if is_mac {
                            primary_btn("macOS Mode", p)
                        } else {
                            secondary_btn("macOS Mode", p)
                        })
                        .clicked()
                        && !is_mac
                    {
                        self.pending_generic_confirm =
                            Some(GenericConfirmAction::SetSystemMode(true));
                    }
                });
                ui.add_space(10.0);
            }

            if kb.capabilities.supports_debounce_tuning {
                ui.label(
                    RichText::new("GEONWORKS VENOM 8K DEBOUNCE")
                        .size(10.5)
                        .strong()
                        .color(p.text_muted),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(if !self.draft_venom_typing {
                            primary_btn("Gaming (Eager)", p)
                        } else {
                            secondary_btn("Gaming (Eager)", p)
                        })
                        .clicked()
                    {
                        self.draft_venom_typing = false;
                    }
                    if ui
                        .add(if self.draft_venom_typing {
                            primary_btn("Typing (Defer)", p)
                        } else {
                            secondary_btn("Typing (Defer)", p)
                        })
                        .clicked()
                    {
                        self.draft_venom_typing = true;
                    }
                    ui.add(
                        egui::DragValue::new(&mut self.draft_venom_debounce_ms)
                            .range(5..=40)
                            .suffix(" ms"),
                    );
                });
                ui.add_space(6.0);
                if ui
                    .add(secondary_btn("Save Debounce to EEPROM...", p))
                    .clicked()
                {
                    self.pending_generic_confirm = Some(GenericConfirmAction::SetDebounce {
                        typing_mode: self.draft_venom_typing,
                        debounce_ms: self.draft_venom_debounce_ms,
                    });
                }
            }
        });
    }

    fn render_generic_diagnostics_card(
        &self,
        ui: &mut egui::Ui,
        p: Palette,
        card_w: f32,
        kb: &KeyboardStatus,
    ) {
        card_frame(ui, p, card_w, |ui| {
            ui.label(
                RichText::new("Hardware Diagnostics & Telemetry")
                    .size(13.5)
                    .strong()
                    .color(p.text),
            );
            ui.add_space(8.0);
            let mut rows = vec![
                ("Brand", kb.brand.display_name().to_string()),
                ("Model", kb.name.clone()),
                ("USB VID:PID", kb.vid_pid.clone()),
                ("Protocol", kb.protocol_label.clone()),
                ("Controller", kb.chip_type.clone()),
                ("Switch Tech", kb.switch_tech.clone()),
                ("Verification Tier", kb.support_tier.label().to_string()),
            ];
            if let Some(fw) = &kb.firmware_version {
                rows.push(("Firmware", fw.clone()));
            }
            if let Some(bat) = kb.battery {
                rows.push(("Battery", bat.detail_label()));
            }
            for (k, v) in &kb.extra_telemetry {
                rows.push((k.as_str(), v.clone()));
            }
            egui::Grid::new("generic_kb_diag_grid")
                .num_columns(2)
                .spacing([14.0, 5.0])
                .show(ui, |ui| {
                    for (k, v) in rows {
                        ui.label(RichText::new(k).size(11.5).color(p.text_muted));
                        ui.label(RichText::new(v).size(11.5).monospace().color(p.text));
                        ui.end_row();
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

                // --- LIVE A <-> D COUNTER-STRAFE TIMING ANALYZER ---
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("A <-> D COUNTER-STRAFE TIMING")
                            .size(10.5)
                            .strong()
                            .color(p.text_muted),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.strafe_tracker.samples.is_empty()
                            && ui.add(secondary_btn("Reset Strafes", p)).clicked()
                        {
                            self.strafe_tracker.reset();
                        }
                    });
                });
                ui.add_space(4.0);
                if self.strafe_tracker.samples.is_empty() {
                    ui.label(
                        RichText::new(
                            "Alternate A and D to measure exact overlap (-ms) vs gap (+ms) timing from hardware actuation flags.",
                        )
                        .size(11.5)
                        .color(p.text_muted),
                    );
                } else {
                    let count = self.strafe_tracker.samples.len();
                    let last = self.strafe_tracker.samples.back().unwrap();
                    let avg_abs: f64 = self
                        .strafe_tracker
                        .samples
                        .iter()
                        .map(|s| s.signed_ms.abs())
                        .sum::<f64>()
                        / (count as f64);
                    let clean_cnt = self
                        .strafe_tracker
                        .samples
                        .iter()
                        .filter(|s| s.signed_ms.abs() <= 20.0)
                        .count();
                    let overlap_cnt = self
                        .strafe_tracker
                        .samples
                        .iter()
                        .filter(|s| s.signed_ms < -1.0)
                        .count();

                    let last_txt = if last.signed_ms.abs() < 0.5 {
                        format!("{}: 0 ms (perfect)", last.direction)
                    } else if last.signed_ms < 0.0 {
                        format!("{}: {:.0} ms overlap", last.direction, -last.signed_ms)
                    } else {
                        format!("{}: +{:.0} ms gap", last.direction, last.signed_ms)
                    };
                    let last_col = if last.signed_ms.abs() <= 15.0 {
                        p.ok
                    } else if last.signed_ms.abs() <= 35.0 {
                        p.warn
                    } else {
                        p.bad
                    };

                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(last_txt)
                                .size(12.0)
                                .strong()
                                .monospace()
                                .color(last_col),
                        );
                        ui.label(
                            RichText::new(format!(
                                "  ·  Avg |delta|: {avg_abs:.1} ms  ·  Clean (<=20ms): {clean_cnt}/{count}  ·  Overlaps: {overlap_cnt}"
                            ))
                            .size(11.5)
                            .monospace()
                            .color(p.text_muted),
                        );
                    });

                    // Centered timeline bar plot (-60 ms overlap .. 0 ms .. +60 ms gap)
                    ui.add_space(6.0);
                    let plot_h = 46.0;
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(inner_w, plot_h), egui::Sense::hover());
                    let painter = ui.painter_at(rect);
                    painter.rect_filled(rect, CornerRadius::same(6), p.surface_alt);
                    painter.rect_stroke(
                        rect,
                        CornerRadius::same(6),
                        Stroke::new(1.0_f32, p.border_subtle),
                        egui::StrokeKind::Inside,
                    );
                    let mid_y = rect.center().y;
                    painter.hline(
                        (rect.left() + 6.0)..=(rect.right() - 6.0),
                        mid_y,
                        Stroke::new(1.0_f32, p.border),
                    );
                    let slot_w = ((rect.width() - 12.0) / 20.0).max(8.0);
                    for (i, s) in self.strafe_tracker.samples.iter().enumerate() {
                        let x = rect.left() + 6.0 + (i as f32) * slot_w;
                        let mag = ((s.signed_ms.abs() / 60.0).clamp(0.08, 1.0) as f32)
                            * ((plot_h * 0.5) - 6.0);
                        let col = if s.signed_ms.abs() <= 15.0 {
                            p.ok
                        } else if s.signed_ms.abs() <= 35.0 {
                            p.warn
                        } else {
                            p.bad
                        };
                        let bar = if s.signed_ms < 0.0 {
                            // Overlap extends downward from center line
                            Rect::from_min_max(
                                Pos2::new(x + 1.0, mid_y + 1.0),
                                Pos2::new((x + slot_w - 3.0).max(x + 4.0), mid_y + 1.0 + mag),
                            )
                        } else {
                            // Gap extends upward from center line
                            Rect::from_min_max(
                                Pos2::new(x + 1.0, mid_y - 1.0 - mag),
                                Pos2::new((x + slot_w - 3.0).max(x + 4.0), mid_y - 1.0),
                            )
                        };
                        painter.rect_filled(bar, CornerRadius::same(2), col);
                    }
                    painter.text(
                        Pos2::new(rect.right() - 8.0, rect.top() + 8.0),
                        Align2::RIGHT_CENTER,
                        "+gap",
                        FontId::monospace(9.5),
                        p.text_muted,
                    );
                    painter.text(
                        Pos2::new(rect.right() - 8.0, rect.bottom() - 8.0),
                        Align2::RIGHT_CENTER,
                        "-overlap",
                        FontId::monospace(9.5),
                        p.text_muted,
                    );
                }
            }
        });
    }

    fn render_visual_keymap_card(
        &mut self,
        ui: &mut egui::Ui,
        p: Palette,
        mode: ThemeMode,
        card_w: f32,
        kb: &WootingKeyboardInfo,
        runtime: &ProfileRuntimeHandle,
    ) {
        #[derive(Clone, Copy)]
        struct KeySpec {
            label: &'static str,
            width_u: f32,
            /// Wooting matrix index `(row << 5) | col`, or `0xFFFF` for an empty layout spacer.
            matrix: u16,
            alt_matrix: Option<u16>,
            hid_usage: u16,
        }
        const fn key(
            label: &'static str,
            width_u: f32,
            matrix: u16,
            alt_matrix: Option<u16>,
            hid_usage: u16,
        ) -> KeySpec {
            KeySpec {
                label,
                width_u,
                matrix,
                alt_matrix,
                hid_usage,
            }
        }
        const fn gap_u(width_u: f32) -> KeySpec {
            KeySpec {
                label: "",
                width_u,
                matrix: 0xFFFF,
                alt_matrix: None,
                hid_usage: 0,
            }
        }

        let is_iso = kb
            .layout
            .as_deref()
            .is_some_and(|l| l.to_ascii_uppercase().contains("ISO"));
        let is_tkl = kb.name.contains("80HE") || kb.name.contains("Two") || kb.name.contains("One");

        let layout_badge = match (is_tkl, is_iso) {
            (false, false) => "60% ANSI",
            (false, true) => "60% ISO",
            (true, false) => "80HE / TKL ANSI",
            (true, true) => "80HE / TKL ISO",
        };

        // Build rows dynamically for 60% ANSI/ISO or 80HE/TKL ANSI/ISO.
        let mut rows: Vec<Vec<KeySpec>> = Vec::with_capacity(6);
        if is_tkl {
            rows.push(vec![
                key("Esc", 1.0, 0x00, None, 0x29),
                gap_u(0.75),
                key("F1", 1.0, 0x02, None, 0x3A),
                key("F2", 1.0, 0x03, None, 0x3B),
                key("F3", 1.0, 0x04, None, 0x3C),
                key("F4", 1.0, 0x05, None, 0x3D),
                gap_u(0.5),
                key("F5", 1.0, 0x06, None, 0x3E),
                key("F6", 1.0, 0x07, None, 0x3F),
                key("F7", 1.0, 0x08, None, 0x40),
                key("F8", 1.0, 0x09, None, 0x41),
                gap_u(0.5),
                key("F9", 1.0, 0x0A, None, 0x42),
                key("F10", 1.0, 0x0B, None, 0x43),
                key("F11", 1.0, 0x0C, None, 0x44),
                key("F12", 1.0, 0x0D, None, 0x45),
                gap_u(0.5),
                key("Prt", 1.0, 0x0E, None, 0x46),
                key("Pau", 1.0, 0x0F, None, 0x48),
                key("Mod", 1.0, 0x10, None, 0x00),
            ]);
        }

        // Row 1: Numbers
        let mut r1 = vec![
            key(
                if is_tkl { "`" } else { "Esc" },
                1.0,
                0x20,
                None,
                if is_tkl { 0x35 } else { 0x29 },
            ),
            key("1", 1.0, 0x21, None, 0x1E),
            key("2", 1.0, 0x22, None, 0x1F),
            key("3", 1.0, 0x23, None, 0x20),
            key("4", 1.0, 0x24, None, 0x21),
            key("5", 1.0, 0x25, None, 0x22),
            key("6", 1.0, 0x26, None, 0x23),
            key("7", 1.0, 0x27, None, 0x24),
            key("8", 1.0, 0x28, None, 0x25),
            key("9", 1.0, 0x29, None, 0x26),
            key("0", 1.0, 0x2A, None, 0x27),
            key("-", 1.0, 0x2B, None, 0x2D),
            key("=", 1.0, 0x2C, None, 0x2E),
            key("Bksp", 2.0, 0x2D, None, 0x2A),
        ];
        if is_tkl {
            r1.extend([
                gap_u(0.5),
                key("Ins", 1.0, 0x2E, None, 0x49),
                key("Hm", 1.0, 0x2F, None, 0x4A),
                key("PU", 1.0, 0x30, None, 0x4B),
            ]);
        }
        rows.push(r1);

        // Row 2: QWERTY
        let mut r2 = vec![
            key("Tab", 1.5, 0x40, None, 0x2B),
            key("Q", 1.0, 0x41, None, 0x14),
            key("W", 1.0, 0x42, None, 0x1A),
            key("E", 1.0, 0x43, None, 0x08),
            key("R", 1.0, 0x44, None, 0x15),
            key("T", 1.0, 0x45, None, 0x17),
            key("Y", 1.0, 0x46, None, 0x1C),
            key("U", 1.0, 0x47, None, 0x18),
            key("I", 1.0, 0x48, None, 0x0C),
            key("O", 1.0, 0x49, None, 0x12),
            key("P", 1.0, 0x4A, None, 0x13),
            key("[", 1.0, 0x4B, None, 0x2F),
            key("]", 1.0, 0x4C, None, 0x30),
            if is_iso {
                key("Enter", 1.5, 0x6D, None, 0x28)
            } else {
                key("\\", 1.5, 0x4D, None, 0x31)
            },
        ];
        if is_tkl {
            r2.extend([
                gap_u(0.5),
                key("Del", 1.0, 0x4E, None, 0x4C),
                key("End", 1.0, 0x4F, None, 0x4D),
                key("PD", 1.0, 0x50, None, 0x4E),
            ]);
        }
        rows.push(r2);

        // Row 3: ASDF
        let mut r3 = vec![
            key("Caps", 1.75, 0x60, None, 0x39),
            key("A", 1.0, 0x61, None, 0x04),
            key("S", 1.0, 0x62, None, 0x16),
            key("D", 1.0, 0x63, None, 0x07),
            key("F", 1.0, 0x64, None, 0x09),
            key("G", 1.0, 0x65, None, 0x0A),
            key("H", 1.0, 0x66, None, 0x0B),
            key("J", 1.0, 0x67, None, 0x0D),
            key("K", 1.0, 0x68, None, 0x0E),
            key("L", 1.0, 0x69, None, 0x0F),
            key(";", 1.0, 0x6A, None, 0x33),
            key("'", 1.0, 0x6B, None, 0x34),
        ];
        if is_iso {
            r3.push(key("#", 1.0, 0x6C, None, 0x32));
            r3.push(key("Ret", 1.25, 0x6D, None, 0x28));
        } else {
            r3.push(key("Enter", 2.25, 0x6D, None, 0x28));
        }
        if is_tkl {
            r3.push(gap_u(3.5));
        }
        rows.push(r3);

        // Row 4: ZXCV
        let mut r4 = Vec::with_capacity(16);
        if is_iso {
            r4.push(key("LShf", 1.25, 0x80, None, 0xE1));
            r4.push(key("ISO\\", 1.0, 0x81, None, 0x64));
        } else {
            r4.push(key("LShift", 2.25, 0x80, None, 0xE1));
        }
        r4.extend([
            key("Z", 1.0, 0x82, None, 0x1D),
            key("X", 1.0, 0x83, None, 0x1B),
            key("C", 1.0, 0x84, None, 0x06),
            key("V", 1.0, 0x85, None, 0x19),
            key("B", 1.0, 0x86, None, 0x05),
            key("N", 1.0, 0x87, None, 0x11),
            key("M", 1.0, 0x88, None, 0x10),
            key(",", 1.0, 0x89, None, 0x36),
            key(".", 1.0, 0x8A, None, 0x37),
            key("/", 1.0, 0x8B, None, 0x38),
            key("RShift", 2.75, 0x8D, None, 0xE5),
        ]);
        if is_tkl {
            r4.extend([gap_u(1.5), key("Up", 1.0, 0x8F, None, 0x52), gap_u(1.0)]);
        }
        rows.push(r4);

        // Row 5: Bottom modifiers + Space
        let mut r5 = vec![
            key("LCtrl", 1.25, 0xA0, None, 0xE0),
            key("Win", 1.25, 0xA1, None, 0xE3),
            key("LAlt", 1.25, 0xA2, None, 0xE2),
            key("Space", 6.25, 0xA6, None, 0x2C),
            key("RAlt", 1.25, 0xAA, None, 0xE6),
            key("Menu", 1.25, 0xAC, None, 0x65),
            key("RCtrl", 1.25, 0xAB, Some(0xAD), 0xE4),
            key("Fn", 1.25, 0xAE, None, 0x00),
        ];
        if is_tkl {
            r5.extend([
                gap_u(0.5),
                key("Lt", 1.0, 0xB1, Some(0xAE), 0x50),
                key("Dn", 1.0, 0xAF, None, 0x51),
                key("Rt", 1.0, 0xB0, None, 0x4F),
            ]);
        }
        rows.push(r5);

        // Extract linked SOCD / Rappy Snappy pairs (e.g. `("A", "D")`, `("W", "S")`) for hover pairing.
        let socd_pairs: Vec<(String, String, String)> = kb
            .advanced_keys
            .iter()
            .filter(|a| a.kind == "SOCD" || a.kind == "Rappy Snappy")
            .filter_map(|a| {
                let (left, right_rest) = a.detail.split_once(" + ")?;
                let right = right_rest.split(" (").next().unwrap_or(right_rest).trim();
                Some((
                    left.trim().to_string(),
                    right.to_string(),
                    format!("{}: {}", a.kind, a.detail),
                ))
            })
            .collect();

        let inner_w = (card_w - 24.0).max(200.0);
        let base_act = kb
            .analog_profile
            .as_ref()
            .and_then(|a| a.actuation_mm)
            .unwrap_or(self.draft_actuation_mm);

        card_frame(ui, p, card_w, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("Interactive Visual Keymap & Live Analog Heatmap")
                        .size(14.0)
                        .strong()
                        .color(p.text),
                );
                mono_chip(ui, p, layout_badge);
                if kb.supports_arm_writes {
                    if ui
                        .add(secondary_btn("Select WASD", p))
                        .on_hover_text("Select W, A, S, D to edit their per-key actuation together")
                        .clicked()
                    {
                        self.selected_keys.clear();
                        for m in [0x42u16, 0x61, 0x62, 0x63] {
                            self.selected_keys.insert(m);
                        }
                        self.draft_per_key_mm = kb
                            .per_key_actuation
                            .iter()
                            .find(|o| o.matrix_index == 0x42)
                            .map(|o| o.actuation_mm)
                            .unwrap_or(base_act);
                        self.confirm_per_key_action = None;
                    }
                    if !self.selected_keys.is_empty()
                        && ui.add(secondary_btn("Clear Selection", p)).clicked()
                    {
                        self.selected_keys.clear();
                        self.confirm_per_key_action = None;
                    }
                }
            });
            ui.add_space(3.0);
            ui.add(
                egui::Label::new(
                    RichText::new(if kb.supports_arm_writes {
                        "Click any keys to select and edit per-key actuation (0x31 / OUT 0x15) · hover SOCD keys to highlight pairs · start Analog Test below for live travel"
                    } else {
                        "Hover any key for per-key actuation & SOCD/AKC details · start Analog Test below for live depth fill"
                    })
                    .size(11.5)
                    .color(p.text_muted),
                )
                .wrap(),
            );
            ui.add_space(8.0);

            let total_units = if is_tkl { 18.5_f32 } else { 15.0_f32 };
            let gap = 4.0_f32;
            let max_board_w = inner_w.min(if is_tkl { 900.0 } else { 760.0 });
            let cols_approx = if is_tkl { 17.0_f32 } else { 13.0_f32 };
            let unit_w = ((max_board_w - cols_approx * gap) / total_units).max(14.0);
            let key_h = (unit_w * 0.88).clamp(22.0, 42.0);
            let board_w = total_units * unit_w + cols_approx * gap;
            let board_h =
                (rows.len() as f32) * key_h + ((rows.len().saturating_sub(1)) as f32) * gap;
            let left_pad = ((inner_w - board_w) * 0.5).max(0.0);

            let (alloc_rect, board_resp) =
                ui.allocate_exact_size(Vec2::new(inner_w, board_h), egui::Sense::click());

            let pointer_pos = ui.input(|i| i.pointer.hover_pos());
            let clicked_pos = if board_resp.clicked() {
                board_resp.interact_pointer_pos()
            } else {
                None
            };

            struct PlacedKey {
                spec: KeySpec,
                rect: Rect,
                ov_mm: Option<f32>,
                is_hovered: bool,
            }

            let mut placed: Vec<PlacedKey> = Vec::with_capacity(90);
            for (r_idx, row) in rows.iter().enumerate() {
                let y = alloc_rect.top() + (r_idx as f32) * (key_h + gap);
                let mut x = alloc_rect.left() + left_pad;
                let non_gap_gaps = (row.len().saturating_sub(1)) as f32;
                let extra_space = ((cols_approx - non_gap_gaps) * gap).max(0.0);
                let row_units: f32 = row.iter().map(|k| k.width_u).sum();

                for &spec in row {
                    let kw =
                        spec.width_u * unit_w + (spec.width_u / row_units.max(1.0)) * extra_space;
                    let key_rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(kw, key_h));
                    x += kw + gap;
                    if spec.matrix == 0xFFFF {
                        continue;
                    }
                    let ov_mm = kb
                        .per_key_actuation
                        .iter()
                        .find(|o| {
                            o.matrix_index == spec.matrix || spec.alt_matrix == Some(o.matrix_index)
                        })
                        .map(|o| o.actuation_mm);
                    let is_hovered = pointer_pos.is_some_and(|pos| key_rect.contains(pos));
                    if kb.supports_arm_writes
                        && clicked_pos.is_some_and(|pos| key_rect.contains(pos))
                    {
                        if !self.selected_keys.remove(&spec.matrix) {
                            if self.selected_keys.is_empty() {
                                self.draft_per_key_mm = ov_mm.unwrap_or(base_act);
                            }
                            self.selected_keys.insert(spec.matrix);
                        }
                        self.confirm_per_key_action = None;
                    }
                    placed.push(PlacedKey {
                        spec,
                        rect: key_rect,
                        ov_mm,
                        is_hovered,
                    });
                }
            }

            // Detect if the currently hovered key belongs to a linked SOCD / Rappy Snappy pair.
            let hovered_label = placed.iter().find(|k| k.is_hovered).map(|k| k.spec.label);
            let active_socd_pair = hovered_label.and_then(|hl| {
                socd_pairs
                    .iter()
                    .find(|(a, b, _)| a.eq_ignore_ascii_case(hl) || b.eq_ignore_ascii_case(hl))
                    .cloned()
            });

            let mut hovered_tooltip: Option<String> = None;
            let mut pair_rects: Vec<Rect> = Vec::new();
            let painter = ui.painter_at(alloc_rect);

            for pk in &placed {
                let spec = pk.spec;
                let key_rect = pk.rect;
                let ov_mm = pk.ov_mm;

                let akc_match = kb.advanced_keys.iter().find(|a| {
                    a.primary_key.eq_ignore_ascii_case(spec.label)
                        || (spec.label == "Esc" && a.primary_key.starts_with("Esc"))
                        || (spec.label == "Win" && a.primary_key == "LWin")
                        || ((a.kind == "SOCD" || a.kind == "Rappy Snappy")
                            && a.detail.split_whitespace().any(|tok| tok == spec.label))
                });

                let is_socd_highlighted = active_socd_pair.as_ref().is_some_and(|(a, b, _)| {
                    a.eq_ignore_ascii_case(spec.label) || b.eq_ignore_ascii_case(spec.label)
                });
                if is_socd_highlighted {
                    pair_rects.push(key_rect);
                }

                let live = self.smoothed_keys.values().find(|s| {
                    s.matrix_index == Some(spec.matrix)
                        || (spec.alt_matrix.is_some() && s.matrix_index == spec.alt_matrix)
                        || (s.matrix_index.is_none()
                            && spec.hid_usage != 0
                            && s.label.eq_ignore_ascii_case(spec.label))
                });

                let depth_frac = live.map_or(0.0, |s| (s.display_mm / 4.0).clamp(0.0, 1.0));
                let is_act = live.is_some_and(|s| {
                    s.actuated
                        .unwrap_or(s.display_mm >= ov_mm.unwrap_or(base_act) && s.is_pressed)
                });
                let is_selected = self.selected_keys.contains(&spec.matrix);

                let key_fill = if is_selected {
                    Color32::from_rgba_unmultiplied(p.text.r(), p.text.g(), p.text.b(), 28)
                } else if is_socd_highlighted {
                    Color32::from_rgba_unmultiplied(p.info.r(), p.info.g(), p.info.b(), 36)
                } else {
                    p.surface_alt
                };
                painter.rect_filled(key_rect, CornerRadius::same(5), key_fill);

                // Live bottom-to-top analog fill
                if depth_frac > 0.004 {
                    let fill_h = key_rect.height() * depth_frac;
                    let fill_rect = Rect::from_min_max(
                        Pos2::new(key_rect.left() + 1.0, key_rect.bottom() - fill_h),
                        Pos2::new(key_rect.right() - 1.0, key_rect.bottom() - 1.0),
                    );
                    let fill_col = if is_act {
                        Color32::from_rgba_unmultiplied(p.ok.r(), p.ok.g(), p.ok.b(), 72)
                    } else {
                        Color32::from_rgba_unmultiplied(p.text.r(), p.text.g(), p.text.b(), 44)
                    };
                    painter.rect_filled(fill_rect, CornerRadius::same(4), fill_col);
                }

                let border_col = if is_act {
                    p.ok
                } else if is_socd_highlighted {
                    p.info
                } else if is_selected || pk.is_hovered {
                    p.text
                } else if ov_mm.is_some() {
                    p.text_muted
                } else {
                    p.border
                };
                let stroke_w = if is_act || is_socd_highlighted || is_selected {
                    1.8_f32
                } else if pk.is_hovered {
                    1.5_f32
                } else {
                    1.0_f32
                };
                painter.rect_stroke(
                    key_rect,
                    CornerRadius::same(5),
                    Stroke::new(stroke_w, border_col),
                    egui::StrokeKind::Inside,
                );

                // Top-right dot for SOCD / AKC bindings
                if let Some(akc) = akc_match {
                    let dot_col = if akc.kind == "SOCD" || akc.kind == "Rappy Snappy" {
                        p.info
                    } else {
                        p.text_muted
                    };
                    painter.circle_filled(
                        Pos2::new(key_rect.right() - 5.0, key_rect.top() + 5.0),
                        2.5,
                        dot_col,
                    );
                }

                // Key label + optional tiny override mm label
                let font_sz = if unit_w < 28.0 { 8.5 } else { 10.5 };
                let label_y = if ov_mm.is_some() && key_h >= 29.0 {
                    key_rect.center().y - 5.0
                } else {
                    key_rect.center().y
                };
                painter.text(
                    Pos2::new(key_rect.center().x, label_y),
                    Align2::CENTER_CENTER,
                    spec.label,
                    FontId::monospace(font_sz),
                    if is_act || pk.is_hovered || is_selected || is_socd_highlighted {
                        p.text
                    } else {
                        p.text_muted
                    },
                );
                if let Some(mm) = ov_mm {
                    if key_h >= 29.0 {
                        painter.text(
                            Pos2::new(key_rect.center().x, key_rect.bottom() - 6.5),
                            Align2::CENTER_CENTER,
                            format!("{mm:.2}"),
                            FontId::monospace(8.5),
                            p.text,
                        );
                    }
                }

                if pk.is_hovered {
                    let mut lines = vec![format!(
                        "{} (0x{:02X})  ·  Actuation: {:.2} mm{}",
                        spec.label,
                        spec.matrix,
                        ov_mm.unwrap_or(base_act),
                        if ov_mm.is_some() {
                            " (per-key override)"
                        } else {
                            " (base)"
                        }
                    )];
                    if let Some(akc) = akc_match {
                        lines.push(format!("AKC: {} — {}", akc.kind, akc.detail));
                    }
                    if let Some(lv) = live {
                        lines.push(format!(
                            "Live travel: {:.2} mm (peak {:.2} mm){}",
                            lv.display_mm,
                            lv.peak_mm,
                            if is_act { " · ACTUATED" } else { "" }
                        ));
                    }
                    hovered_tooltip = Some(lines.join("\n"));
                }
            }

            // Connect linked SOCD / Rappy Snappy pair keys visually on hover.
            if pair_rects.len() == 2 {
                let p0 = Pos2::new(pair_rects[0].center().x, pair_rects[0].top() + 2.0);
                let p1 = Pos2::new(pair_rects[1].center().x, pair_rects[1].top() + 2.0);
                painter.line_segment([p0, p1], Stroke::new(2.0_f32, p.info));
                painter.circle_filled(p0, 3.0, p.info);
                painter.circle_filled(p1, 3.0, p.info);
            }

            if let Some(tip) = hovered_tooltip {
                ui.add_space(6.0);
                ui.label(RichText::new(tip).size(11.5).monospace().color(p.text));
            } else {
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        RichText::new("Legend: Bright border/number = per-key override  ·  White frame = selected  ·  Blue link = SOCD / Rappy Snappy pair  ·  Green = live ACT")
                            .size(10.5)
                            .color(p.text_muted),
                    )
                    .wrap(),
                );
            }

            // Inline Confirm-Gated Per-Key Actuation Editor (ARM boards)
            if kb.supports_arm_writes && !self.selected_keys.is_empty() {
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                let sel_labels: Vec<String> = self
                    .selected_keys
                    .iter()
                    .map(|&m| {
                        placed
                            .iter()
                            .find(|k| k.spec.matrix == m)
                            .map(|k| k.spec.label.to_string())
                            .unwrap_or_else(|| format!("0x{m:02X}"))
                    })
                    .collect();

                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "Selected ({}): {}",
                            sel_labels.len(),
                            sel_labels.join(", ")
                        ))
                        .size(12.0)
                        .strong()
                        .color(p.text),
                    );
                    ui.add_space(8.0);
                    ui.label(RichText::new("Actuation:").size(11.5).color(p.text_muted));
                    ui.add(
                        egui::DragValue::new(&mut self.draft_per_key_mm)
                            .range(0.10..=4.00)
                            .speed(0.05)
                            .fixed_decimals(2)
                            .suffix(" mm"),
                    );
                    if ui.add(primary_btn("Set Per-Key...", p)).clicked() {
                        self.confirm_per_key_action =
                            Some(PerKeyConfirmAction::SetMm(self.draft_per_key_mm));
                    }
                    if ui.add(secondary_btn("Reset to Base...", p)).clicked() {
                        self.confirm_per_key_action = Some(PerKeyConfirmAction::ResetToBase);
                    }
                });

                if let Some(action) = self.confirm_per_key_action {
                    ui.add_space(6.0);
                    let slot_lbl = kb
                        .active_profile
                        .map(|i| kb.profile_display_name(i))
                        .unwrap_or_else(|| "active slot".to_string());
                    let desc = match action {
                        PerKeyConfirmAction::SetMm(mm) => format!(
                            "Write {:.2} mm per-key actuation for {} ({}) to onboard flash on {}? Polled preserves untouched keys and verifies via 0x31 read-back.",
                            mm,
                            sel_labels.join(", "),
                            sel_labels.len(),
                            slot_lbl
                        ),
                        PerKeyConfirmAction::ResetToBase => format!(
                            "Remove per-key actuation overrides for {} ({}) on {} so they follow profile base ({:.2} mm)? Verified via 0x31 read-back.",
                            sel_labels.join(", "),
                            sel_labels.len(),
                            slot_lbl,
                            base_act
                        ),
                    };
                    semantic_banner(ui, p, mode, inner_w, Severity::Warn, &desc);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.add(primary_btn("Confirm Flash Save", p)).clicked() {
                            let target_mm = match action {
                                PerKeyConfirmAction::SetMm(mm) => Some(mm),
                                PerKeyConfirmAction::ResetToBase => None,
                            };
                            let updates: Vec<(u16, Option<f32>)> = self
                                .selected_keys
                                .iter()
                                .copied()
                                .map(|m| (m, target_mm))
                                .collect();
                            runtime.send(ProfileRuntimeCmd::WriteWootingPerKeyActuation {
                                device_id: Some(kb.id.clone()),
                                updates,
                            });
                            self.confirm_per_key_action = None;
                        }
                        if ui.add(secondary_btn("Cancel", p)).clicked() {
                            self.confirm_per_key_action = None;
                        }
                    });
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
