// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

//! Plain monochrome Black & White UI for Phase 2 Per-Game Peripheral Profiles.
//! Only the four semantic statuses (`OK`, `INFO`, `WARN`, `BAD`) carry chroma.

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Vec2};
use polled_core::{
    export_profile_json, import_profile_json, AppPaths, GameProfile, LiftOffDistance,
    MouseSettings, ProfileStore, Severity,
};

use crate::profile_runtime::{
    BackendSelection, ProfileRuntimeCmd, ProfileRuntimeHandle, ProfileRuntimeSnapshot,
};
use crate::theme::{Palette, ThemeMode};

pub struct ProfilesUiState {
    pub store: ProfileStore,
    pub selected_profile_id: String,
    pub editor: ProfileDraft,
    pub runtime_snapshot: Option<ProfileRuntimeSnapshot>,
    pub export_modal_json: Option<String>,
    pub import_modal_open: bool,
    pub import_text: String,
    pub notice: Option<(Severity, String)>,
}

#[derive(Debug, Clone)]
pub struct ProfileDraft {
    pub id: String,
    pub name: String,
    pub is_desktop_default: bool,
    pub executables_csv: String,
    pub set_dpi: bool,
    pub dpi: u16,
    pub set_polling: bool,
    pub polling_rate_hz: u16,
    pub set_lod: bool,
    pub lod: LiftOffDistance,
    pub set_debounce: bool,
    pub debounce_ms: u8,
    pub set_win_speed: bool,
    pub win_speed: u32,
    pub author: String,
    pub notes: String,
}

impl ProfileDraft {
    pub fn from_profile(p: &GameProfile) -> Self {
        Self {
            id: p.id.clone(),
            name: p.name.clone(),
            is_desktop_default: p.is_desktop_default,
            executables_csv: p.executables.join(", "),
            set_dpi: p.mouse.dpi.is_some(),
            dpi: p.mouse.dpi.unwrap_or(800),
            set_polling: p.mouse.polling_rate_hz.is_some(),
            polling_rate_hz: p.mouse.polling_rate_hz.unwrap_or(1000),
            set_lod: p.mouse.lod.is_some(),
            lod: p.mouse.lod.unwrap_or(LiftOffDistance::Mm10),
            set_debounce: p.mouse.debounce_ms.is_some(),
            debounce_ms: p.mouse.debounce_ms.unwrap_or(4),
            set_win_speed: p.windows_pointer_speed.is_some(),
            win_speed: p.windows_pointer_speed.unwrap_or(10),
            author: p.author.clone().unwrap_or_default(),
            notes: p.notes.clone().unwrap_or_default(),
        }
    }

    pub fn to_profile(&self) -> GameProfile {
        let executables = if self.is_desktop_default {
            Vec::new()
        } else {
            self.executables_csv
                .split([',', ';', '\n'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        };

        GameProfile {
            id: self.id.clone(),
            name: if self.name.trim().is_empty() {
                "Untitled Profile".to_string()
            } else {
                self.name.trim().to_string()
            },
            is_desktop_default: self.is_desktop_default,
            executables,
            mouse: MouseSettings {
                dpi: self.set_dpi.then_some(self.dpi),
                polling_rate_hz: self.set_polling.then_some(self.polling_rate_hz),
                lod: self.set_lod.then_some(self.lod),
                debounce_ms: self.set_debounce.then_some(self.debounce_ms),
            },
            windows_pointer_speed: self.set_win_speed.then_some(self.win_speed.clamp(1, 20)),
            author: (!self.author.trim().is_empty()).then(|| self.author.trim().to_string()),
            notes: (!self.notes.trim().is_empty()).then(|| self.notes.trim().to_string()),
        }
        .sanitized()
    }
}

impl ProfilesUiState {
    pub fn load() -> Self {
        let path = AppPaths::default_user_dir().profiles_path();
        let store = ProfileStore::load_or_default(&path).unwrap_or_default();
        let first = store
            .profiles
            .first()
            .cloned()
            .unwrap_or_else(GameProfile::desktop_default);
        Self {
            selected_profile_id: first.id.clone(),
            editor: ProfileDraft::from_profile(&first),
            store,
            runtime_snapshot: None,
            export_modal_json: None,
            import_modal_open: false,
            import_text: String::new(),
            notice: None,
        }
    }

    fn select_profile(&mut self, id: &str) {
        if let Some(profile) = self.store.profiles.iter().find(|p| p.id == id).cloned() {
            self.selected_profile_id = profile.id.clone();
            self.editor = ProfileDraft::from_profile(&profile);
        }
    }

    pub fn render(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        p: Palette,
        mode: ThemeMode,
        runtime: &ProfileRuntimeHandle,
    ) {
        // --- DEVICE & AUTO-SWITCH CONTROL CARD ---
        let snap = self.runtime_snapshot.clone();
        card_frame(ui, p, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Device:").strong().color(p.text));

                let current_label = snap
                    .as_ref()
                    .map(|s| match s.selection {
                        BackendSelection::AutoConnected => {
                            if s.detected_hardware.is_empty() {
                                "Auto (No hardware — Dry-Run active)".to_string()
                            } else if s.detected_hardware.len() == 1 {
                                format!("Auto: {}", s.detected_hardware[0].name)
                            } else {
                                format!("Auto ({} mice connected)", s.detected_hardware.len())
                            }
                        }
                        BackendSelection::HardwareIndex(idx) => s
                            .detected_hardware
                            .get(idx)
                            .map(|hw| format!("{} [{}]", hw.name, hw.support_tier.label()))
                            .unwrap_or_else(|| "Hardware".to_string()),
                        BackendSelection::DryRunVolatile => {
                            "Dry-Run Virtual Mouse (Volatile RAM)".to_string()
                        }
                        BackendSelection::DryRunFlashOnly => {
                            "Dry-Run Virtual Mouse (Flash-Only)".to_string()
                        }
                    })
                    .unwrap_or_else(|| "Detecting...".to_string());

                egui::ComboBox::from_id_salt("device_selector")
                    .selected_text(current_label)
                    .width(340.0)
                    .show_ui(ui, |ui| {
                        if let Some(s) = &snap {
                            if ui
                                .selectable_label(
                                    s.selection == BackendSelection::AutoConnected,
                                    "Auto (All Connected Mice — Hot-Plug)",
                                )
                                .clicked()
                            {
                                runtime.send(ProfileRuntimeCmd::SelectBackend(
                                    BackendSelection::AutoConnected,
                                ));
                            }
                            for (idx, hw) in s.detected_hardware.iter().enumerate() {
                                let text = format!(
                                    "{} ({}) [{}]",
                                    hw.name,
                                    hw.vid_pid.as_deref().unwrap_or("HID"),
                                    hw.support_tier.label()
                                );
                                if ui
                                    .selectable_label(
                                        s.selection == BackendSelection::HardwareIndex(idx),
                                        text,
                                    )
                                    .clicked()
                                {
                                    runtime.send(ProfileRuntimeCmd::SelectBackend(
                                        BackendSelection::HardwareIndex(idx),
                                    ));
                                }
                            }
                            if ui
                                .selectable_label(
                                    s.selection == BackendSelection::DryRunVolatile,
                                    "Dry-Run Virtual Mouse (Volatile RAM)",
                                )
                                .clicked()
                            {
                                runtime.send(ProfileRuntimeCmd::SelectBackend(
                                    BackendSelection::DryRunVolatile,
                                ));
                            }
                            if ui
                                .selectable_label(
                                    s.selection == BackendSelection::DryRunFlashOnly,
                                    "Dry-Run Virtual Mouse (Flash-Only)",
                                )
                                .clicked()
                            {
                                runtime.send(ProfileRuntimeCmd::SelectBackend(
                                    BackendSelection::DryRunFlashOnly,
                                ));
                            }
                        }
                    });

                if ui.add(secondary_btn("Redetect", p)).clicked() {
                    runtime.send(ProfileRuntimeCmd::RedetectHardware);
                }

                ui.separator();

                let auto_on = snap.as_ref().is_some_and(|s| s.auto_switch_enabled);
                let toggle_label = if auto_on {
                    "Auto-Switch: ON"
                } else {
                    "Auto-Switch: OFF"
                };
                let btn = if auto_on {
                    primary_btn(toggle_label, p)
                } else {
                    secondary_btn(toggle_label, p)
                };
                if ui.add(btn).clicked() {
                    runtime.send(ProfileRuntimeCmd::SetAutoSwitch(!auto_on));
                }

                ui.label(RichText::new("Debounce:").color(p.text_muted).size(12.0));
                let mut debounce_ms = self.store.debounce_ms;
                let resp = ui.add(
                    egui::DragValue::new(&mut debounce_ms)
                        .range(500..=5000)
                        .speed(50)
                        .suffix(" ms"),
                );
                if resp.changed() {
                    self.store.debounce_ms = debounce_ms;
                }
                if resp.drag_stopped() || resp.lost_focus() {
                    runtime.send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
                }
            });

            if let Some(s) = &snap {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    let fg_text = s.foreground_exe.as_deref().unwrap_or("(none)");
                    status_pill(ui, p, "FOREGROUND", fg_text);
                    status_pill(ui, p, "ACTIVE PROFILE", &s.active_profile_name);
                    for dev in &s.active_devices {
                        let summary = dev
                            .settings
                            .as_ref()
                            .map(|st| st.summary_string())
                            .unwrap_or_else(|| "asleep / move mouse".to_string());
                        status_pill(ui, p, &dev.info.name.to_uppercase(), &summary);
                    }
                    if let Some(pending) = &s.pending_exe {
                        status_pill(ui, p, "DEBOUNCING", pending);
                    }
                    if s.any_flash_only {
                        status_pill(
                            ui,
                            p,
                            "FLASH WRITES",
                            &s.total_flash_writes_session.to_string(),
                        );
                    }
                });

                if let Some(warn) = &s.flash_warning {
                    ui.add_space(6.0);
                    semantic_banner(ui, p, mode, Severity::Warn, warn);
                }
            }
        });

        if let Some((sev, msg)) = self.notice.clone() {
            ui.add_space(8.0);
            semantic_banner(ui, p, mode, sev, &msg);
        }

        ui.add_space(10.0);

        // --- TWO-COLUMN LAYOUT: PROFILE LIST + PROFILE EDITOR ---
        let available_h = (ui.available_height() - 155.0).max(260.0);
        ui.horizontal_top(|ui| {
            // Left: Profile list
            ui.allocate_ui_with_layout(
                Vec2::new(235.0, available_h),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    card_frame(ui, p, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Profiles").strong().color(p.text));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.add(secondary_btn("Import", p)).clicked() {
                                        self.import_modal_open = true;
                                        self.import_text.clear();
                                    }
                                    if ui.add(primary_btn("+ New", p)).clicked() {
                                        let next_num = self.store.profiles.len();
                                        let new_prof = GameProfile {
                                            id: format!("game_{next_num}"),
                                            name: format!("Game Profile {next_num}"),
                                            is_desktop_default: false,
                                            executables: vec!["game.exe".to_string()],
                                            mouse: MouseSettings {
                                                dpi: Some(800),
                                                polling_rate_hz: Some(4000),
                                                lod: Some(LiftOffDistance::Mm07),
                                                debounce_ms: Some(1),
                                            },
                                            windows_pointer_speed: None,
                                            author: None,
                                            notes: None,
                                        };
                                        let new_id = new_prof.id.clone();
                                        self.store.upsert_profile(new_prof);
                                        self.select_profile(&new_id);
                                        runtime.send(ProfileRuntimeCmd::UpdateStore(
                                            self.store.clone(),
                                        ));
                                    }
                                },
                            );
                        });
                        ui.add_space(8.0);

                        let active_id = snap
                            .as_ref()
                            .map(|s| s.active_profile_id.as_str())
                            .unwrap_or("");
                        let mut clicked_id = None;

                        egui::ScrollArea::vertical()
                            .id_salt("profile_list_scroll")
                            .max_height((available_h - 56.0).max(180.0))
                            .show(ui, |ui| {
                                for prof in &self.store.profiles {
                                    let selected = prof.id == self.selected_profile_id;
                                    let is_active = prof.id == active_id;
                                    let fill = if selected { p.surface_alt } else { p.surface };
                                    let border = if selected { p.text } else { p.border };

                                    let resp = egui::Frame::NONE
                                        .fill(fill)
                                        .stroke(Stroke::new(1.0_f32, border))
                                        .corner_radius(CornerRadius::same(6))
                                        .inner_margin(Margin::symmetric(10, 8))
                                        .show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    RichText::new(&prof.name)
                                                        .strong()
                                                        .color(p.text),
                                                );
                                                if is_active {
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            ui.label(
                                                                RichText::new("ACTIVE")
                                                                    .size(10.0)
                                                                    .strong()
                                                                    .color(p.ok),
                                                            );
                                                        },
                                                    );
                                                }
                                            });
                                            let sub = if prof.is_desktop_default {
                                                "Fallback when no game matches".to_string()
                                            } else if prof.executables.is_empty() {
                                                "(no executables)".to_string()
                                            } else {
                                                prof.executables.join(", ")
                                            };
                                            ui.label(
                                                RichText::new(sub)
                                                    .size(11.0)
                                                    .monospace()
                                                    .color(p.text_muted),
                                            );
                                        })
                                        .response
                                        .interact(egui::Sense::click());

                                    if resp.clicked() {
                                        clicked_id = Some(prof.id.clone());
                                    }
                                    ui.add_space(6.0);
                                }
                            });

                        if let Some(id) = clicked_id {
                            self.select_profile(&id);
                        }
                    });
                },
            );

            ui.add_space(10.0);

            // Right: Selected profile editor
            ui.vertical(|ui| {
                card_frame(ui, p, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(if self.editor.is_desktop_default {
                                "Edit Desktop Default Profile"
                            } else {
                                "Edit Game Profile"
                            })
                            .size(15.0)
                            .strong()
                            .color(p.text),
                        );

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if !self.editor.is_desktop_default
                                && ui.add(secondary_btn("Delete", p)).clicked()
                            {
                                let del_id = self.editor.id.clone();
                                if self.store.remove_profile(&del_id) {
                                    self.select_profile("desktop");
                                    runtime
                                        .send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
                                    self.notice = Some((
                                        Severity::Info,
                                        format!("Deleted profile '{del_id}'."),
                                    ));
                                }
                            }
                            if ui.add(secondary_btn("Export JSON", p)).clicked() {
                                let profile = self.editor.to_profile();
                                match export_profile_json(&profile) {
                                    Ok(json) => self.export_modal_json = Some(json),
                                    Err(err) => {
                                        self.notice = Some((Severity::Bad, err.to_string()))
                                    }
                                }
                            }
                            if ui.add(secondary_btn("Test Apply Now", p)).clicked() {
                                let profile = self.editor.to_profile();
                                let id = profile.id.clone();
                                self.store.upsert_profile(profile);
                                runtime.send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
                                runtime.send(ProfileRuntimeCmd::ApplyProfileNow(id));
                            }
                            if ui.add(primary_btn("Save Profile", p)).clicked() {
                                let profile = self.editor.to_profile();
                                let name = profile.name.clone();
                                let id = profile.id.clone();
                                self.store.upsert_profile(profile);
                                self.select_profile(&id);
                                runtime.send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
                                self.notice =
                                    Some((Severity::Ok, format!("Saved profile '{name}'.")));
                            }
                        });
                    });

                    ui.add_space(10.0);

                    let caps = snap.as_ref().map(|s| s.primary_caps.clone());
                    let dpi_min = caps.as_ref().map_or(50, |c| c.dpi_min);
                    let dpi_max = caps.as_ref().map_or(32000, |c| c.dpi_max);
                    let dpi_step = caps.as_ref().map_or(10, |c| c.dpi_step);
                    let rates = caps
                        .as_ref()
                        .map(|c| c.polling_rates_hz.clone())
                        .unwrap_or_else(|| vec![125, 250, 500, 1000, 2000, 4000, 8000]);
                    let db_min = caps.as_ref().map_or(0, |c| c.debounce_min_ms);
                    let db_max = caps.as_ref().map_or(20, |c| c.debounce_max_ms);

                    egui::Grid::new("profile_editor_grid")
                        .num_columns(2)
                        .spacing([14.0, 8.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new("Name").color(p.text_muted));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.editor.name)
                                    .desired_width(280.0),
                            );
                            ui.end_row();

                            if !self.editor.is_desktop_default {
                                ui.label(RichText::new("Executables").color(p.text_muted));
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.editor.executables_csv)
                                        .desired_width(360.0)
                                        .hint_text("cs2.exe, valorant-win64-shipping.exe"),
                                );
                                ui.end_row();
                            }

                            ui.checkbox(&mut self.editor.set_dpi, "DPI");
                            ui.add_enabled(
                                self.editor.set_dpi,
                                egui::DragValue::new(&mut self.editor.dpi)
                                    .range(dpi_min..=dpi_max)
                                    .speed(f64::from(dpi_step))
                                    .suffix(" DPI"),
                            );
                            ui.end_row();

                            ui.checkbox(&mut self.editor.set_polling, "Polling Rate");
                            ui.add_enabled_ui(self.editor.set_polling, |ui| {
                                egui::ComboBox::from_id_salt("polling_rate_combo")
                                    .selected_text(format!("{} Hz", self.editor.polling_rate_hz))
                                    .show_ui(ui, |ui| {
                                        for hz in &rates {
                                            ui.selectable_value(
                                                &mut self.editor.polling_rate_hz,
                                                *hz,
                                                format!("{hz} Hz"),
                                            );
                                        }
                                    });
                            });
                            ui.end_row();

                            ui.checkbox(&mut self.editor.set_lod, "Lift-Off Distance");
                            ui.add_enabled_ui(self.editor.set_lod, |ui| {
                                egui::ComboBox::from_id_salt("lod_combo")
                                    .selected_text(self.editor.lod.label())
                                    .show_ui(ui, |ui| {
                                        for opt in LiftOffDistance::ALL {
                                            ui.selectable_value(
                                                &mut self.editor.lod,
                                                opt,
                                                opt.label(),
                                            );
                                        }
                                    });
                            });
                            ui.end_row();

                            ui.checkbox(&mut self.editor.set_debounce, "Debounce");
                            ui.add_enabled(
                                self.editor.set_debounce,
                                egui::DragValue::new(&mut self.editor.debounce_ms)
                                    .range(db_min..=db_max)
                                    .suffix(" ms"),
                            );
                            ui.end_row();

                            ui.checkbox(&mut self.editor.set_win_speed, "Windows Speed");
                            ui.add_enabled(
                                self.editor.set_win_speed,
                                egui::DragValue::new(&mut self.editor.win_speed)
                                    .range(1..=20)
                                    .suffix("/20 (10 = 6/11)"),
                            );
                            ui.end_row();

                            ui.label(RichText::new("Notes (optional)").color(p.text_muted));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.editor.notes)
                                    .desired_width(360.0)
                                    .hint_text("Saved in exported profile JSON cards"),
                            );
                            ui.end_row();
                        });
                });
            });
        });

        ui.add_space(10.0);

        // --- LIVE ACTIVITY LOG (verifications, diff skips, flash guards, rollbacks) ---
        card_frame(ui, p, |ui| {
            ui.label(RichText::new("Activity Log").strong().color(p.text));
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .id_salt("profile_activity_log")
                .max_height(105.0)
                .show(ui, |ui| {
                    if let Some(s) = &snap {
                        for entry in &s.log {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(&entry.timestamp)
                                        .size(11.0)
                                        .monospace()
                                        .color(p.text_muted),
                                );
                                ui.label(
                                    RichText::new(format!(
                                        "[{}]",
                                        entry.severity.as_str().to_uppercase()
                                    ))
                                    .size(11.0)
                                    .strong()
                                    .color(p.severity_fg(entry.severity)),
                                );
                                ui.label(RichText::new(&entry.message).size(11.5).color(p.text));
                            });
                        }
                    }
                });
        });

        // --- EXPORT JSON MODAL ---
        let mut close_export = false;
        if let Some(json) = self.export_modal_json.clone() {
            egui::Window::new("Export Profile JSON (polled.profile.v1)")
                .collapsible(false)
                .resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .default_width(500.0)
                .frame(
                    egui::Frame::NONE
                        .fill(p.surface)
                        .stroke(Stroke::new(1.5_f32, p.text))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(16)),
                )
                .show(ctx, |ui| {
                    let mut preview = json.clone();
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
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
                            close_export = true;
                        }
                        if ui.add(primary_btn("Copy JSON", p)).clicked() {
                            ctx.copy_text(json.clone());
                            self.notice = Some((
                                Severity::Ok,
                                "Copied versioned profile JSON to clipboard.".to_string(),
                            ));
                        }
                    });
                });
        }
        if close_export {
            self.export_modal_json = None;
        }

        // --- IMPORT JSON MODAL ---
        if self.import_modal_open {
            let mut close_import = false;
            egui::Window::new("Import Profile JSON")
                .collapsible(false)
                .resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .default_width(500.0)
                .frame(
                    egui::Frame::NONE
                        .fill(p.surface)
                        .stroke(Stroke::new(1.5_f32, p.text))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(16)),
                )
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new("Paste a Polled profile JSON card (`polled.profile.v1`):")
                            .size(12.5)
                            .color(p.text_muted),
                    );
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut self.import_text)
                                    .code_editor()
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    ui.add_space(10.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(primary_btn("Import Profile", p)).clicked() {
                            match import_profile_json(&self.import_text) {
                                Ok(profile) => {
                                    let name = profile.name.clone();
                                    let id = profile.id.clone();
                                    self.store.upsert_profile(profile);
                                    self.select_profile(&id);
                                    runtime
                                        .send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
                                    self.notice =
                                        Some((Severity::Ok, format!("Imported profile '{name}'.")));
                                    close_import = true;
                                }
                                Err(err) => {
                                    self.notice = Some((Severity::Bad, err.to_string()));
                                }
                            }
                        }
                        if ui.add(secondary_btn("Cancel", p)).clicked() {
                            close_import = true;
                        }
                    });
                });
            if close_import {
                self.import_modal_open = false;
            }
        }
    }
}

fn card_frame(ui: &mut egui::Ui, p: Palette, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(p.surface)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui);
        });
}

fn primary_btn(label: &str, p: Palette) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(p.btn_primary_fg).strong())
        .fill(p.btn_primary_bg)
        .corner_radius(CornerRadius::same(6))
}

fn secondary_btn(label: &str, p: Palette) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(p.text))
        .fill(p.surface_alt)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(6))
}

fn status_pill(ui: &mut egui::Ui, p: Palette, label: &str, value: &str) {
    egui::Frame::NONE
        .fill(p.surface_alt)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(10.0).strong().color(p.text_muted));
                ui.label(RichText::new(value).size(11.5).monospace().color(p.text));
            });
        });
}

fn semantic_banner(ui: &mut egui::Ui, p: Palette, mode: ThemeMode, severity: Severity, text: &str) {
    let fg = p.severity_fg(severity);
    let bg = p.severity_bg(severity, mode);
    let border = Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), 120);
    egui::Frame::NONE
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(severity.as_str().to_uppercase())
                        .size(11.0)
                        .strong()
                        .color(fg),
                );
                ui.label(RichText::new(text).size(12.0).color(p.text));
            });
        });
}
