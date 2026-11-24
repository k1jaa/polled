// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plain monochrome Black & White UI for Phase 2 Per-Game Peripheral Profiles.
//! Only the four semantic statuses (`OK`, `INFO`, `WARN`, `BAD`) carry chroma.

use std::collections::{BTreeMap, HashSet};

use eframe::egui::text::LayoutJob;
use eframe::egui::{
    self, Color32, CornerRadius, FontId, Margin, RichText, Stroke, TextFormat, Vec2,
};
use polled_core::{
    discover_installed_games, discover_running_apps, export_profile_json, import_profile_json,
    AppPaths, DeviceCapabilities, DiscoveredGame, GameProfile, LiftOffDistance, MouseSettings,
    PowerPlanPreset, ProfileStore, Severity, SupportTier,
};

use crate::profile_runtime::{
    BackendSelection, ProfileRuntimeCmd, ProfileRuntimeHandle, ProfileRuntimeSnapshot,
};
use crate::theme::{Palette, ThemeMode};

#[derive(Debug, Clone)]
enum PendingUnverifiedWrite {
    EnableAutoSwitch {
        /// `(device_id, display_name, is_flash_only)`
        devices: Vec<(String, String, bool)>,
    },
    ApplyProfileNow {
        profile_id: String,
        devices: Vec<(String, String, bool)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum GamePickerTab {
    #[default]
    Installed,
    Running,
}

pub struct ProfilesUiState {
    pub store: ProfileStore,
    pub selected_profile_id: String,
    pub editor: ProfileDraft,
    pub runtime_snapshot: Option<ProfileRuntimeSnapshot>,
    pub export_modal_json: Option<String>,
    pub import_modal_open: bool,
    pub import_text: String,
    game_picker_open: bool,
    game_picker_tab: GamePickerTab,
    game_picker_search: String,
    discovered_installed: Vec<DiscoveredGame>,
    discovered_running: Vec<DiscoveredGame>,
    pub notice: Option<(Severity, String)>,
    acknowledged_unverified_devices: HashSet<String>,
    pending_unverified_action: Option<PendingUnverifiedWrite>,
}

#[derive(Debug, Clone)]
pub struct ProfileDraft {
    pub id: String,
    pub name: String,
    pub is_desktop_default: bool,
    pub executables_csv: String,
    pub default_mouse: MouseSettings,
    pub device_overrides: BTreeMap<String, MouseSettings>,
    /// `None` = editing shared default settings; `Some(device_id)` = editing per-mouse override.
    pub editing_device_id: Option<String>,
    pub set_dpi: bool,
    pub dpi: u16,
    pub set_dpi_y: bool,
    pub dpi_y: u16,
    pub set_polling: bool,
    pub polling_rate_hz: u16,
    pub set_lod: bool,
    pub lod: LiftOffDistance,
    pub set_debounce: bool,
    pub debounce_ms: u8,
    pub set_motion_sync: bool,
    pub motion_sync: bool,
    pub set_angle_snapping: bool,
    pub angle_snapping: bool,
    pub set_ripple_control: bool,
    pub ripple_control: bool,
    pub set_sleep: bool,
    pub sleep_timeout_secs: u16,
    pub set_win_speed: bool,
    pub win_speed: u32,
    pub set_power_plan: bool,
    pub power_plan: PowerPlanPreset,
    pub audio_output_id: Option<String>,
    pub audio_output_name: Option<String>,
    pub set_audio_output_vol: bool,
    pub audio_output_vol: u8,
    pub audio_input_id: Option<String>,
    pub audio_input_name: Option<String>,
    pub set_audio_input_vol: bool,
    pub audio_input_vol: u8,
    pub lock_mic_volume: bool,
    pub author: String,
    pub notes: String,
}

impl ProfileDraft {
    pub fn from_profile(p: &GameProfile) -> Self {
        let mut draft = Self {
            id: p.id.clone(),
            name: p.name.clone(),
            is_desktop_default: p.is_desktop_default,
            executables_csv: p.executables.join(", "),
            default_mouse: p.mouse.clone(),
            device_overrides: p.device_overrides.clone(),
            editing_device_id: None,
            set_dpi: false,
            dpi: 800,
            set_dpi_y: false,
            dpi_y: 800,
            set_polling: false,
            polling_rate_hz: 1000,
            set_lod: false,
            lod: LiftOffDistance::Mm10,
            set_debounce: false,
            debounce_ms: 4,
            set_motion_sync: false,
            motion_sync: true,
            set_angle_snapping: false,
            angle_snapping: false,
            set_ripple_control: false,
            ripple_control: false,
            set_sleep: false,
            sleep_timeout_secs: 300,
            set_win_speed: p.windows_pointer_speed.is_some(),
            win_speed: p.windows_pointer_speed.unwrap_or(10),
            set_power_plan: p.windows_power_plan.is_some(),
            power_plan: p
                .windows_power_plan
                .unwrap_or(PowerPlanPreset::HighPerformance),
            audio_output_id: p.audio_output_device_id.clone(),
            audio_output_name: p.audio_output_device_name.clone(),
            set_audio_output_vol: p.audio_output_volume.is_some(),
            audio_output_vol: p.audio_output_volume.unwrap_or(75),
            audio_input_id: p.audio_input_device_id.clone(),
            audio_input_name: p.audio_input_device_name.clone(),
            set_audio_input_vol: p.audio_input_volume.is_some(),
            audio_input_vol: p.audio_input_volume.unwrap_or(85),
            lock_mic_volume: p.lock_mic_volume,
            author: p.author.clone().unwrap_or_default(),
            notes: p.notes.clone().unwrap_or_default(),
        };
        draft.load_mouse_fields(&p.mouse);
        draft
    }

    fn load_mouse_fields(&mut self, m: &MouseSettings) {
        self.set_dpi = m.dpi.is_some();
        self.dpi = m.dpi.unwrap_or(800);
        self.set_dpi_y = m.dpi_y.is_some();
        self.dpi_y = m.dpi_y.unwrap_or(self.dpi);
        self.set_polling = m.polling_rate_hz.is_some();
        self.polling_rate_hz = m.polling_rate_hz.unwrap_or(1000);
        self.set_lod = m.lod.is_some();
        self.lod = m.lod.unwrap_or(LiftOffDistance::Mm10);
        self.set_debounce = m.debounce_ms.is_some();
        self.debounce_ms = m.debounce_ms.unwrap_or(4);
        self.set_motion_sync = m.motion_sync.is_some();
        self.motion_sync = m.motion_sync.unwrap_or(true);
        self.set_angle_snapping = m.angle_snapping.is_some();
        self.angle_snapping = m.angle_snapping.unwrap_or(false);
        self.set_ripple_control = m.ripple_control.is_some();
        self.ripple_control = m.ripple_control.unwrap_or(false);
        self.set_sleep = m.sleep_timeout_secs.is_some();
        self.sleep_timeout_secs = m.sleep_timeout_secs.unwrap_or(300);
    }

    fn current_mouse_fields(&self) -> MouseSettings {
        MouseSettings {
            dpi: self.set_dpi.then_some(self.dpi),
            dpi_y: self.set_dpi_y.then_some(self.dpi_y),
            polling_rate_hz: self.set_polling.then_some(self.polling_rate_hz),
            lod: self.set_lod.then_some(self.lod),
            debounce_ms: self.set_debounce.then_some(self.debounce_ms),
            motion_sync: self.set_motion_sync.then_some(self.motion_sync),
            angle_snapping: self.set_angle_snapping.then_some(self.angle_snapping),
            ripple_control: self.set_ripple_control.then_some(self.ripple_control),
            sleep_timeout_secs: self.set_sleep.then_some(self.sleep_timeout_secs),
        }
    }

    fn commit_current_mouse_fields(&mut self) {
        let current = self.current_mouse_fields();
        if let Some(dev_id) = &self.editing_device_id {
            self.device_overrides.insert(dev_id.clone(), current);
        } else {
            self.default_mouse = current;
        }
    }

    fn switch_editing_target(
        &mut self,
        next: Option<String>,
        target_caps: Option<&DeviceCapabilities>,
    ) {
        if self.editing_device_id == next {
            return;
        }
        self.commit_current_mouse_fields();
        match next {
            None => {
                self.editing_device_id = None;
                let base = self.default_mouse.clone();
                self.load_mouse_fields(&base);
            }
            Some(dev_id) => {
                let settings = if let Some(existing) = self.device_overrides.get(&dev_id) {
                    existing.clone()
                } else {
                    let seed = match target_caps {
                        Some(caps) => caps.adapt_settings(&self.default_mouse).0,
                        None => self.default_mouse.clone(),
                    };
                    self.device_overrides.insert(dev_id.clone(), seed.clone());
                    seed
                };
                self.editing_device_id = Some(dev_id);
                self.load_mouse_fields(&settings);
            }
        }
    }

    fn remove_current_override(&mut self) {
        if let Some(dev_id) = self.editing_device_id.take() {
            self.device_overrides.remove(&dev_id);
            let base = self.default_mouse.clone();
            self.load_mouse_fields(&base);
        }
    }

    pub fn to_profile(&self) -> GameProfile {
        let mut copy = self.clone();
        copy.commit_current_mouse_fields();

        let executables = if copy.is_desktop_default {
            Vec::new()
        } else {
            copy.executables_csv
                .split([',', ';', '\n'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        };

        GameProfile {
            id: copy.id,
            name: if copy.name.trim().is_empty() {
                "Untitled Profile".to_string()
            } else {
                copy.name.trim().to_string()
            },
            is_desktop_default: copy.is_desktop_default,
            executables,
            mouse: copy.default_mouse,
            device_overrides: copy.device_overrides,
            windows_pointer_speed: copy.set_win_speed.then_some(copy.win_speed.clamp(1, 20)),
            windows_power_plan: copy.set_power_plan.then_some(copy.power_plan),
            audio_output_device_id: copy.audio_output_id,
            audio_output_device_name: copy.audio_output_name,
            audio_output_volume: copy
                .set_audio_output_vol
                .then_some(copy.audio_output_vol.min(100)),
            audio_input_device_id: copy.audio_input_id,
            audio_input_device_name: copy.audio_input_name,
            audio_input_volume: copy
                .set_audio_input_vol
                .then_some(copy.audio_input_vol.min(100)),
            lock_mic_volume: copy.set_audio_input_vol && copy.lock_mic_volume,
            author: (!copy.author.trim().is_empty()).then(|| copy.author.trim().to_string()),
            notes: (!copy.notes.trim().is_empty()).then(|| copy.notes.trim().to_string()),
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
            game_picker_open: false,
            game_picker_tab: GamePickerTab::Installed,
            game_picker_search: String::new(),
            discovered_installed: Vec::new(),
            discovered_running: Vec::new(),
            notice: None,
            acknowledged_unverified_devices: HashSet::new(),
            pending_unverified_action: None,
        }
    }

    fn unacknowledged_unverified_devices(
        &self,
        snap: Option<&ProfileRuntimeSnapshot>,
    ) -> Vec<(String, String, bool)> {
        let Some(s) = snap else {
            return Vec::new();
        };
        s.active_devices
            .iter()
            .filter(|d| {
                d.info.support_tier == SupportTier::ImplementedFromDocs
                    && !self.acknowledged_unverified_devices.contains(&d.info.id)
            })
            .map(|d| {
                let label = match &d.info.vid_pid {
                    Some(vp) => format!("{} ({vp})", d.info.name),
                    None => d.info.name.clone(),
                };
                (d.info.id.clone(), label, !d.caps.supports_volatile_writes)
            })
            .collect()
    }

    fn select_profile(&mut self, id: &str) {
        if let Some(profile) = self.store.profiles.iter().find(|p| p.id == id).cloned() {
            self.selected_profile_id = profile.id.clone();
            self.editor = ProfileDraft::from_profile(&profile);
        }
    }

    pub fn apply_read_from_mouse(
        &mut self,
        device_name: Option<String>,
        settings: Option<MouseSettings>,
        error: Option<String>,
    ) {
        if let Some(err) = error {
            self.notice = Some((Severity::Warn, err));
            return;
        }
        let Some(live) = settings else {
            self.notice = Some((
                Severity::Warn,
                "Mouse did not return readable onboard settings.".to_string(),
            ));
            return;
        };
        if live.is_empty() {
            self.notice = Some((
                Severity::Info,
                "Mouse returned no configurable fields.".to_string(),
            ));
            return;
        }
        self.editor.load_mouse_fields(&live);
        self.editor.commit_current_mouse_fields();
        let dev_label = device_name.unwrap_or_else(|| "mouse".to_string());
        self.notice = Some((
            Severity::Ok,
            format!(
                "Loaded live settings from {dev_label} ({}) into editor. Click Save to keep.",
                live.summary_string()
            ),
        ));
    }

    pub fn render(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        p: Palette,
        mode: ThemeMode,
        runtime: &ProfileRuntimeHandle,
        low_battery_threshold: u8,
    ) {
        // Compute authoritative width directly from the live viewport rect so resizing the window
        // immediately reflows every card, banner, pill row, and log line with zero clipping.
        let content_w = (ctx.screen_rect().width() - 36.0).max(320.0);
        ui.set_max_width(content_w);

        let snap = self.runtime_snapshot.clone();
        let stacked = content_w < 680.0;

        if stacked {
            egui::ScrollArea::vertical()
                .id_salt("profiles_stacked_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let stack_w = (content_w - 14.0).max(280.0);
                    ui.set_max_width(stack_w);
                    self.render_device_card(
                        ui,
                        p,
                        mode,
                        stack_w,
                        snap.as_ref(),
                        runtime,
                        low_battery_threshold,
                    );
                    if let Some((sev, msg)) = self.notice.clone() {
                        ui.add_space(8.0);
                        semantic_banner(ui, p, mode, stack_w, sev, &msg);
                    }
                    ui.add_space(10.0);
                    self.render_profile_list_card(ui, p, stack_w, 190.0, snap.as_ref(), runtime);
                    ui.add_space(10.0);
                    self.render_profile_editor_card(ui, p, stack_w, 310.0, snap.as_ref(), runtime);
                    ui.add_space(10.0);
                    self.render_activity_log_card(ctx, ui, p, stack_w, Some(150.0), snap.as_ref());
                });
        } else {
            // --- DEVICE & AUTO-SWITCH CONTROL CARD ---
            self.render_device_card(
                ui,
                p,
                mode,
                content_w,
                snap.as_ref(),
                runtime,
                low_battery_threshold,
            );

            if let Some((sev, msg)) = self.notice.clone() {
                ui.add_space(8.0);
                semantic_banner(ui, p, mode, content_w, sev, &msg);
            }

            ui.add_space(10.0);

            // --- TWO-COLUMN LAYOUT: PROFILE LIST + PROFILE EDITOR ---
            let gap = 10.0;
            let left_w = (content_w * 0.26).clamp(195.0, 260.0);
            let right_w = (content_w - left_w - gap).max(320.0);
            let rem_h = ui.available_height();
            let mid_h = if rem_h >= 480.0 {
                324.0
            } else {
                ((rem_h - 10.0) * 0.68).clamp(185.0, 324.0)
            };
            let inner_col_h = (mid_h - 24.0).max(160.0);

            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;

                ui.allocate_ui_with_layout(
                    Vec2::new(left_w, mid_h),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        self.render_profile_list_card(
                            ui,
                            p,
                            left_w,
                            inner_col_h,
                            snap.as_ref(),
                            runtime,
                        );
                    },
                );

                ui.allocate_ui_with_layout(
                    Vec2::new(right_w, mid_h),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        self.render_profile_editor_card(
                            ui,
                            p,
                            right_w,
                            inner_col_h,
                            snap.as_ref(),
                            runtime,
                        );
                    },
                );
            });

            ui.add_space(10.0);

            // --- LIVE ACTIVITY LOG (fills remaining window height down to footer) ---
            self.render_activity_log_card(ctx, ui, p, content_w, None, snap.as_ref());
        }

        // --- EXPORT JSON MODAL ---
        let modal_w = (ctx.screen_rect().width() - 48.0).clamp(320.0, 520.0);
        let mut close_export = false;
        if let Some(json) = self.export_modal_json.clone() {
            egui::Window::new("Export Profile JSON (polled.profile.v1)")
                .collapsible(false)
                .resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .default_width(modal_w)
                .max_width(modal_w)
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
                .default_width(modal_w)
                .max_width(modal_w)
                .frame(
                    egui::Frame::NONE
                        .fill(p.surface)
                        .stroke(Stroke::new(1.5_f32, p.text))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(16)),
                )
                .show(ctx, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Paste a Polled profile JSON card (`polled.profile.v1`):",
                            )
                            .size(12.5)
                            .color(p.text_muted),
                        )
                        .wrap(),
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

        // --- SMART GAME PICKER MODAL ---
        if self.game_picker_open {
            let mut close_picker = false;
            let mut refresh_picker = false;
            let mut added_exe: Option<(String, String)> = None;
            let picker_w = (ctx.screen_rect().width() - 48.0).clamp(360.0, 560.0);
            egui::Window::new("Smart Game Picker (Installed & Running)")
                .collapsible(false)
                .resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .default_width(picker_w)
                .max_width(picker_w)
                .frame(
                    egui::Frame::NONE
                        .fill(p.surface)
                        .stroke(Stroke::new(1.5_f32, p.text))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(16)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        let inst_lbl = format!("Installed ({})", self.discovered_installed.len());
                        let inst_btn = if self.game_picker_tab == GamePickerTab::Installed {
                            primary_btn(&inst_lbl, p)
                        } else {
                            secondary_btn(&inst_lbl, p)
                        };
                        if ui.add(inst_btn).clicked() {
                            self.game_picker_tab = GamePickerTab::Installed;
                        }
                        let run_lbl = format!("Running Apps ({})", self.discovered_running.len());
                        let run_btn = if self.game_picker_tab == GamePickerTab::Running {
                            primary_btn(&run_lbl, p)
                        } else {
                            secondary_btn(&run_lbl, p)
                        };
                        if ui.add(run_btn).clicked() {
                            self.game_picker_tab = GamePickerTab::Running;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(secondary_btn("Refresh", p)).clicked() {
                                refresh_picker = true;
                            }
                        });
                    });
                    ui.add_space(6.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.game_picker_search)
                            .desired_width(f32::INFINITY)
                            .hint_text("Filter by game or .exe name..."),
                    );
                    ui.add_space(8.0);

                    let query = self.game_picker_search.trim().to_ascii_lowercase();
                    let list = match self.game_picker_tab {
                        GamePickerTab::Installed => &self.discovered_installed,
                        GamePickerTab::Running => &self.discovered_running,
                    };
                    let existing: Vec<String> = self
                        .editor
                        .executables_csv
                        .split([',', ';', '\n'])
                        .map(|s| s.trim().to_ascii_lowercase())
                        .filter(|s| !s.is_empty())
                        .collect();

                    egui::ScrollArea::vertical()
                        .id_salt("smart_game_picker_scroll")
                        .max_height(260.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let mut shown = 0usize;
                            for g in list {
                                if !query.is_empty()
                                    && !g.display_name.to_ascii_lowercase().contains(&query)
                                    && !g.exe_name.contains(&query)
                                {
                                    continue;
                                }
                                shown += 1;
                                let already = existing.iter().any(|e| e == &g.exe_name);
                                egui::Frame::NONE
                                    .fill(p.surface_alt)
                                    .stroke(Stroke::new(1.0_f32, p.border))
                                    .corner_radius(CornerRadius::same(6))
                                    .inner_margin(Margin::symmetric(10, 6))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    RichText::new(&g.display_name)
                                                        .strong()
                                                        .size(12.5)
                                                        .color(p.text),
                                                );
                                                ui.label(
                                                    RichText::new(format!(
                                                        "{} · {}",
                                                        g.exe_name,
                                                        g.source.label()
                                                    ))
                                                    .size(11.0)
                                                    .monospace()
                                                    .color(p.text_muted),
                                                );
                                            });
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    if already {
                                                        ui.label(
                                                            RichText::new("ADDED")
                                                                .size(10.5)
                                                                .strong()
                                                                .color(p.ok),
                                                        );
                                                    } else if ui
                                                        .add(primary_btn("+ Add", p))
                                                        .clicked()
                                                    {
                                                        added_exe = Some((
                                                            g.display_name.clone(),
                                                            g.exe_name.clone(),
                                                        ));
                                                    }
                                                },
                                            );
                                        });
                                    });
                                ui.add_space(4.0);
                            }
                            if shown == 0 {
                                ui.label(
                                    RichText::new(
                                        "No matching applications found. You can also type any .exe manually.",
                                    )
                                    .size(12.0)
                                    .color(p.text_muted),
                                );
                            }
                        });

                    ui.add_space(10.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(secondary_btn("Done", p)).clicked() {
                            close_picker = true;
                        }
                    });
                });

            if refresh_picker {
                self.discovered_installed = discover_installed_games();
                self.discovered_running = discover_running_apps();
            }
            if let Some((title, exe)) = added_exe {
                let mut cur: Vec<String> = self
                    .editor
                    .executables_csv
                    .split([',', ';', '\n'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("game.exe"))
                    .map(str::to_string)
                    .collect();
                if !cur.iter().any(|e| e.eq_ignore_ascii_case(&exe)) {
                    cur.push(exe.clone());
                }
                self.editor.executables_csv = cur.join(", ");
                if self.editor.name.starts_with("Game Profile") {
                    self.editor.name = title.clone();
                }
                self.notice = Some((
                    Severity::Ok,
                    format!("Added '{exe}' ({title}) to profile draft — click Save to persist."),
                ));
            }
            if close_picker {
                self.game_picker_open = false;
            }
        }

        // --- UNVERIFIED HARDWARE FIRST-WRITE WARNING MODAL ---
        let mut confirm_unverified = false;
        let mut cancel_unverified = false;
        if let Some(pending) = &self.pending_unverified_action {
            let devices = match pending {
                PendingUnverifiedWrite::EnableAutoSwitch { devices }
                | PendingUnverifiedWrite::ApplyProfileNow { devices, .. } => devices,
            };
            let warn_w = (ctx.screen_rect().width() - 48.0).clamp(340.0, 490.0);
            egui::Window::new("Unverified Hardware Warning")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .default_width(warn_w)
                .max_width(warn_w)
                .frame(
                    egui::Frame::NONE
                        .fill(p.surface)
                        .stroke(Stroke::new(1.5_f32, p.text))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(16)),
                )
                .show(ctx, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "The following connected mouse uses an open-source protocol specification ('Implemented from docs') that has not yet been tested on this exact physical hardware:",
                            )
                            .size(12.5)
                            .color(p.text_muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(8.0);

                    egui::Frame::NONE
                        .fill(p.surface_alt)
                        .stroke(Stroke::new(1.0_f32, p.border))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            for (_, label, is_flash) in devices {
                                let mem_note = if *is_flash {
                                    "Onboard flash (diffed & rate-limited)"
                                } else {
                                    "Volatile RAM (zero flash wear)"
                                };
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!("• {label} — {mem_note}"))
                                            .size(12.0)
                                            .monospace()
                                            .color(p.text),
                                    )
                                    .wrap(),
                                );
                            }
                        });

                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Polled validates bounds, skips unchanged registers, never flashes firmware, and automatically rolls back if read-back verification fails. However, unexpected firmware quirks are still possible. Close vendor software (G HUB / Pulsar / LAMZU) before testing.",
                            )
                            .size(12.0)
                            .color(p.warn),
                        )
                        .wrap(),
                    );
                    ui.add_space(12.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(primary_btn("I Understand — Proceed", p)).clicked() {
                            confirm_unverified = true;
                        }
                        if ui.add(secondary_btn("Cancel", p)).clicked() {
                            cancel_unverified = true;
                        }
                    });
                });
        }
        if cancel_unverified {
            self.pending_unverified_action = None;
        } else if confirm_unverified {
            if let Some(pending) = self.pending_unverified_action.take() {
                match pending {
                    PendingUnverifiedWrite::EnableAutoSwitch { devices } => {
                        for (id, _, _) in devices {
                            self.acknowledged_unverified_devices.insert(id);
                        }
                        runtime.send(ProfileRuntimeCmd::SetAutoSwitch(true));
                    }
                    PendingUnverifiedWrite::ApplyProfileNow {
                        profile_id,
                        devices,
                    } => {
                        for (id, _, _) in devices {
                            self.acknowledged_unverified_devices.insert(id);
                        }
                        runtime.send(ProfileRuntimeCmd::ApplyProfileNow(profile_id));
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_device_card(
        &mut self,
        ui: &mut egui::Ui,
        p: Palette,
        mode: ThemeMode,
        card_w: f32,
        snap: Option<&ProfileRuntimeSnapshot>,
        runtime: &ProfileRuntimeHandle,
        low_battery_threshold: u8,
    ) {
        let inner_w = (card_w - 24.0).max(120.0);
        card_frame(ui, p, card_w, |ui| {
            let split_controls = inner_w < 690.0;
            let combo_w = if split_controls {
                (inner_w - 140.0).clamp(150.0, 360.0)
            } else {
                (inner_w * 0.34).clamp(180.0, 300.0)
            };

            let render_device_picker = |ui: &mut egui::Ui| {
                ui.label(RichText::new("Device:").strong().color(p.text));

                let current_label = snap
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
                    .width(combo_w)
                    .show_ui(ui, |ui| {
                        if let Some(s) = snap {
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
            };

            let auto_on = snap.is_some_and(|s| s.auto_switch_enabled);
            let locked = snap.is_some_and(|s| s.profile_locked);
            let mut debounce_ms = self.store.debounce_ms;
            let mut debounce_committed = false;
            let mut toggle_auto_clicked = false;
            let mut toggle_lock_clicked = false;

            if split_controls {
                ui.horizontal(|ui| {
                    render_device_picker(ui);
                });
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
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
                        toggle_auto_clicked = true;
                    }
                    let lock_label = if locked { "Locked" } else { "Lock Profile" };
                    let lock_btn = if locked {
                        primary_btn(lock_label, p)
                    } else {
                        secondary_btn(lock_label, p)
                    };
                    if ui
                        .add(lock_btn)
                        .on_hover_text(
                            "Hold the current active profile and pause automatic focus switches",
                        )
                        .clicked()
                    {
                        toggle_lock_clicked = true;
                    }
                    ui.label(RichText::new("Debounce:").color(p.text_muted).size(12.0));
                    let resp = ui.add(
                        egui::DragValue::new(&mut debounce_ms)
                            .range(500..=5000)
                            .speed(50)
                            .suffix(" ms"),
                    );
                    if resp.drag_stopped() || resp.lost_focus() {
                        debounce_committed = true;
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    render_device_picker(ui);
                    ui.add_space(4.0);
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
                        toggle_auto_clicked = true;
                    }
                    let lock_label = if locked { "Locked" } else { "Lock" };
                    let lock_btn = if locked {
                        primary_btn(lock_label, p)
                    } else {
                        secondary_btn(lock_label, p)
                    };
                    if ui
                        .add(lock_btn)
                        .on_hover_text(
                            "Hold the current active profile and pause automatic focus switches",
                        )
                        .clicked()
                    {
                        toggle_lock_clicked = true;
                    }
                    ui.label(RichText::new("Debounce:").color(p.text_muted).size(12.0));
                    let resp = ui.add(
                        egui::DragValue::new(&mut debounce_ms)
                            .range(500..=5000)
                            .speed(50)
                            .suffix(" ms"),
                    );
                    if resp.drag_stopped() || resp.lost_focus() {
                        debounce_committed = true;
                    }
                });
            }

            if toggle_lock_clicked {
                runtime.send(ProfileRuntimeCmd::SetProfileLock(!locked));
            }

            if toggle_auto_clicked {
                if !auto_on {
                    let unverified = self.unacknowledged_unverified_devices(snap);
                    if !unverified.is_empty() {
                        self.pending_unverified_action =
                            Some(PendingUnverifiedWrite::EnableAutoSwitch {
                                devices: unverified,
                            });
                    } else {
                        runtime.send(ProfileRuntimeCmd::SetAutoSwitch(true));
                    }
                } else {
                    runtime.send(ProfileRuntimeCmd::SetAutoSwitch(false));
                }
            }
            if debounce_ms != self.store.debounce_ms {
                self.store.debounce_ms = debounce_ms;
            }
            if debounce_committed {
                runtime.send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
            }

            if let Some(s) = snap {
                ui.add_space(6.0);
                let mut pills: Vec<(String, String)> = Vec::new();
                pills.push((
                    "FOREGROUND".to_string(),
                    s.foreground_exe.as_deref().unwrap_or("(none)").to_string(),
                ));
                pills.push((
                    "ACTIVE PROFILE".to_string(),
                    if s.profile_locked {
                        format!("{} [LOCKED]", s.active_profile_name)
                    } else {
                        s.active_profile_name.clone()
                    },
                ));
                let multi_dev = s.active_devices.len() > 1;
                for dev in &s.active_devices {
                    let summary = dev
                        .settings
                        .as_ref()
                        .map(|st| st.summary_string())
                        .unwrap_or_else(|| "asleep / move mouse".to_string());
                    pills.push((dev.info.name.to_uppercase(), summary));
                    if let Some(batt) = dev.battery {
                        let lbl = if multi_dev {
                            format!("{} BATT", dev.info.name.to_uppercase())
                        } else {
                            "BATTERY".to_string()
                        };
                        let val = match (batt.charging, batt.voltage_mv) {
                            (true, _) => format!("{}% CHG", batt.percent),
                            (false, Some(mv)) if mv > 0 => {
                                format!("{}% ({:.2}V)", batt.percent, f32::from(mv) / 1000.0)
                            }
                            (false, _) => format!("{}%", batt.percent),
                        };
                        pills.push((lbl, val));
                    }
                }
                if let Some(pending) = &s.pending_exe {
                    pills.push(("DEBOUNCING".to_string(), pending.clone()));
                }
                if let Some((_, audio_name, _)) = s.audio_outputs.iter().find(|(_, _, def)| *def) {
                    pills.push(("AUDIO".to_string(), audio_name.clone()));
                }
                if let Some(locked_pct) = s.mic_lock_active {
                    pills.push(("MIC LOCK".to_string(), format!("{locked_pct}%")));
                }
                if s.any_flash_only {
                    pills.push((
                        "FLASH WRITES".to_string(),
                        s.total_flash_writes_session.to_string(),
                    ));
                }
                render_status_pill_rows(ui, p, inner_w, &pills);

                let low_batt_msgs: Vec<String> = s
                    .active_devices
                    .iter()
                    .filter_map(|d| {
                        d.battery
                            .filter(|b| !b.charging && b.percent <= low_battery_threshold)
                            .map(|b| format!("{} ({}%)", d.info.name, b.percent))
                    })
                    .collect();
                if !low_batt_msgs.is_empty() {
                    ui.add_space(6.0);
                    semantic_banner(
                        ui,
                        p,
                        mode,
                        inner_w,
                        Severity::Warn,
                        &format!(
                            "Low mouse battery: {}. Plug in or dock soon to prevent wireless polling dropouts.",
                            low_batt_msgs.join(", ")
                        ),
                    );
                }

                let unverified_names: Vec<&str> = s
                    .active_devices
                    .iter()
                    .filter(|d| d.info.support_tier == SupportTier::ImplementedFromDocs)
                    .map(|d| d.info.name.as_str())
                    .collect();
                if !unverified_names.is_empty() {
                    ui.add_space(6.0);
                    semantic_banner(
                        ui,
                        p,
                        mode,
                        inner_w,
                        Severity::Warn,
                        &format!(
                            "Unverified hardware ({}): implemented from open-source docs and not yet verified on physical hardware. Close vendor suites and review docs/COMPATIBILITY.md before writing.",
                            unverified_names.join(", ")
                        ),
                    );
                }

                if let Some(warn) = &s.flash_warning {
                    ui.add_space(6.0);
                    semantic_banner(ui, p, mode, inner_w, Severity::Warn, warn);
                }
            }
        });
    }

    fn render_profile_list_card(
        &mut self,
        ui: &mut egui::Ui,
        p: Palette,
        card_w: f32,
        inner_col_h: f32,
        snap: Option<&ProfileRuntimeSnapshot>,
        runtime: &ProfileRuntimeHandle,
    ) {
        let inner_w = (card_w - 24.0).max(120.0);
        card_frame(ui, p, card_w, |ui| {
            ui.set_min_height(inner_col_h);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Profiles").strong().color(p.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                                dpi_y: None,
                                polling_rate_hz: Some(4000),
                                lod: Some(LiftOffDistance::Mm10),
                                debounce_ms: Some(1),
                                motion_sync: None,
                                angle_snapping: None,
                                ripple_control: None,
                                sleep_timeout_secs: None,
                            },
                            device_overrides: BTreeMap::new(),
                            windows_pointer_speed: None,
                            windows_power_plan: None,
                            audio_output_device_id: None,
                            audio_output_device_name: None,
                            audio_output_volume: None,
                            audio_input_device_id: None,
                            audio_input_device_name: None,
                            audio_input_volume: None,
                            lock_mic_volume: false,
                            author: None,
                            notes: None,
                        };
                        let new_id = new_prof.id.clone();
                        self.store.upsert_profile(new_prof);
                        self.select_profile(&new_id);
                        runtime.send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
                    }
                });
            });
            ui.add_space(8.0);

            let active_id = snap.map(|s| s.active_profile_id.as_str()).unwrap_or("");
            let mut clicked_id = None;

            egui::ScrollArea::vertical()
                .id_salt("profile_list_scroll")
                .max_height((inner_col_h - 34.0).max(110.0))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let item_outer_w = (inner_w - 8.0).max(120.0);
                    let item_inner_w = (item_outer_w - 20.0).max(100.0);
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
                                ui.set_min_width(item_inner_w);
                                ui.set_max_width(item_inner_w);
                                ui.horizontal(|ui| {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if is_active {
                                                ui.label(
                                                    RichText::new("ACTIVE")
                                                        .size(10.0)
                                                        .strong()
                                                        .color(p.ok),
                                                );
                                            }
                                            ui.with_layout(
                                                egui::Layout::left_to_right(egui::Align::Center),
                                                |ui| {
                                                    ui.add(
                                                        egui::Label::new(
                                                            RichText::new(&prof.name)
                                                                .strong()
                                                                .color(p.text),
                                                        )
                                                        .truncate(),
                                                    );
                                                },
                                            );
                                        },
                                    );
                                });
                                let base_sub = if prof.is_desktop_default {
                                    "Fallback when no game matches".to_string()
                                } else if prof.executables.is_empty() {
                                    "(no executables)".to_string()
                                } else {
                                    prof.executables.join(", ")
                                };
                                let sub = if prof.device_overrides.is_empty() {
                                    base_sub
                                } else {
                                    let n = prof.device_overrides.len();
                                    format!(
                                        "{base_sub} · {n} mouse override{}",
                                        if n == 1 { "" } else { "s" }
                                    )
                                };
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(sub)
                                            .size(11.0)
                                            .monospace()
                                            .color(p.text_muted),
                                    )
                                    .wrap(),
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
    }

    fn render_profile_editor_card(
        &mut self,
        ui: &mut egui::Ui,
        p: Palette,
        card_w: f32,
        inner_col_h: f32,
        snap: Option<&ProfileRuntimeSnapshot>,
        runtime: &ProfileRuntimeHandle,
    ) {
        let inner_w = (card_w - 24.0).max(200.0);
        card_frame(ui, p, card_w, |ui| {
            ui.set_min_height(inner_col_h);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !self.editor.is_desktop_default
                        && ui.add(secondary_btn("Delete", p)).clicked()
                    {
                        let del_id = self.editor.id.clone();
                        if self.store.remove_profile(&del_id) {
                            self.select_profile("desktop");
                            runtime.send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
                            self.notice =
                                Some((Severity::Info, format!("Deleted profile '{del_id}'.")));
                        }
                    }
                    if ui.add(secondary_btn("Export", p)).clicked() {
                        let profile = self.editor.to_profile();
                        match export_profile_json(&profile) {
                            Ok(json) => self.export_modal_json = Some(json),
                            Err(err) => self.notice = Some((Severity::Bad, err.to_string())),
                        }
                    }
                    if ui.add(secondary_btn("Apply Now", p)).clicked() {
                        let profile = self.editor.to_profile();
                        let id = profile.id.clone();
                        self.store.upsert_profile(profile);
                        runtime.send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
                        let unverified = self.unacknowledged_unverified_devices(snap);
                        if !unverified.is_empty() {
                            self.pending_unverified_action =
                                Some(PendingUnverifiedWrite::ApplyProfileNow {
                                    profile_id: id,
                                    devices: unverified,
                                });
                        } else {
                            runtime.send(ProfileRuntimeCmd::ApplyProfileNow(id));
                        }
                    }
                    if ui.add(primary_btn("Save", p)).clicked() {
                        let profile = self.editor.to_profile();
                        let name = profile.name.clone();
                        let id = profile.id.clone();
                        self.store.upsert_profile(profile);
                        self.select_profile(&id);
                        runtime.send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
                        self.notice = Some((Severity::Ok, format!("Saved profile '{name}'.")));
                    }

                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(if self.editor.is_desktop_default {
                                    "Desktop Default Profile"
                                } else {
                                    "Edit Game Profile"
                                })
                                .size(15.0)
                                .strong()
                                .color(p.text),
                            )
                            .truncate(),
                        );
                    });
                });
            });

            ui.add_space(8.0);

            let active_target_caps: Option<DeviceCapabilities> =
                self.editor.editing_device_id.as_deref().and_then(|dev_id| {
                    snap.and_then(|s| {
                        s.active_devices
                            .iter()
                            .find(|d| d.info.id == dev_id)
                            .map(|d| d.caps.clone())
                    })
                });
            let dpi_min = active_target_caps.as_ref().map_or(10, |c| c.dpi_min);
            let dpi_max = active_target_caps.as_ref().map_or(32000, |c| c.dpi_max);
            let dpi_step = active_target_caps.as_ref().map_or(10, |c| c.dpi_step);
            let rates = active_target_caps
                .as_ref()
                .map(|c| c.polling_rates_hz.clone())
                .unwrap_or_else(|| vec![125, 250, 500, 1000, 2000, 4000, 8000]);
            let lod_options = active_target_caps
                .as_ref()
                .map(|c| c.lod_options.clone())
                .unwrap_or_else(|| LiftOffDistance::ALL.to_vec());
            let supports_debounce = active_target_caps
                .as_ref()
                .is_none_or(|c| c.supports_debounce);
            let db_min = active_target_caps.as_ref().map_or(0, |c| c.debounce_min_ms);
            let db_max = active_target_caps
                .as_ref()
                .map_or(20, |c| c.debounce_max_ms);
            let supports_motion_sync = active_target_caps
                .as_ref()
                .is_none_or(|c| c.supports_motion_sync);
            let supports_angle_snapping = active_target_caps
                .as_ref()
                .is_none_or(|c| c.supports_angle_snapping);
            let supports_ripple_control = active_target_caps
                .as_ref()
                .is_none_or(|c| c.supports_ripple_control);
            let supports_xy_dpi = active_target_caps
                .as_ref()
                .is_none_or(|c| c.supports_xy_dpi);
            let supports_sleep = active_target_caps
                .as_ref()
                .is_none_or(|c| c.supports_sleep_timeout);
            let sleep_min = active_target_caps
                .as_ref()
                .map_or(10, |c| c.sleep_timeout_min_secs);
            let sleep_max = active_target_caps
                .as_ref()
                .map_or(1800, |c| c.sleep_timeout_max_secs);

            let mut next_target: Option<(Option<String>, Option<DeviceCapabilities>)> = None;
            let mut remove_override = false;
            let mut read_from_mouse = false;
            let mut open_game_picker = false;
            let mut refresh_audio = false;

            egui::ScrollArea::vertical()
                .id_salt("profile_editor_scroll")
                .max_height((inner_col_h - 34.0).max(120.0))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let field_w = (inner_w - 175.0).clamp(140.0, 420.0);
                    let target_combo_w = (field_w
                        - 126.0
                        - if self.editor.editing_device_id.is_some() {
                            74.0
                        } else {
                            0.0
                        })
                    .clamp(115.0, 240.0);

                    egui::Grid::new("profile_editor_grid")
                        .num_columns(2)
                        .spacing([14.0, 7.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new("Name").color(p.text_muted));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.editor.name)
                                    .desired_width(field_w),
                            );
                            ui.end_row();

                            if !self.editor.is_desktop_default {
                                ui.label(RichText::new("Executables").color(p.text_muted));
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.editor.executables_csv)
                                            .desired_width((field_w - 94.0).max(110.0))
                                            .hint_text("cs2.exe, valorant-win64-shipping.exe"),
                                    );
                                    if ui
                                        .add(secondary_btn("Pick Game...", p))
                                        .on_hover_text(
                                            "Browse installed Steam / Riot / Epic games or running apps",
                                        )
                                        .clicked()
                                    {
                                        open_game_picker = true;
                                    }
                                });
                                ui.end_row();
                            }

                            ui.label(RichText::new("Mouse Target").color(p.text_muted));
                            ui.horizontal(|ui| {
                                let target_label = match &self.editor.editing_device_id {
                                    None => "All Mice (Default)".to_string(),
                                    Some(dev_id) => {
                                        let friendly = snap
                                            .and_then(|s| {
                                                s.active_devices
                                                    .iter()
                                                    .find(|d| d.info.id == *dev_id)
                                                    .map(|d| d.info.name.clone())
                                            })
                                            .unwrap_or_else(|| dev_id.clone());
                                        format!("{friendly} [Override]")
                                    }
                                };

                                egui::ComboBox::from_id_salt("mouse_target_combo")
                                    .selected_text(target_label)
                                    .width(target_combo_w)
                                    .show_ui(ui, |ui| {
                                        if ui
                                            .selectable_label(
                                                self.editor.editing_device_id.is_none(),
                                                "All Mice (Default)",
                                            )
                                            .clicked()
                                        {
                                            next_target = Some((None, None));
                                        }

                                        let mut seen_ids = Vec::new();
                                        if let Some(s) = snap {
                                            for dev in &s.active_devices {
                                                seen_ids.push(dev.info.id.clone());
                                                let has_ov = self
                                                    .editor
                                                    .device_overrides
                                                    .contains_key(&dev.info.id);
                                                let label = if has_ov {
                                                    format!("{} [Override]", dev.info.name)
                                                } else {
                                                    format!("{} (+ add override)", dev.info.name)
                                                };
                                                let selected =
                                                    self.editor.editing_device_id.as_deref()
                                                        == Some(dev.info.id.as_str());
                                                if ui.selectable_label(selected, label).clicked() {
                                                    next_target = Some((
                                                        Some(dev.info.id.clone()),
                                                        Some(dev.caps.clone()),
                                                    ));
                                                }
                                            }
                                        }

                                        for dev_id in self.editor.device_overrides.keys() {
                                            if seen_ids.iter().any(|seen| seen == dev_id) {
                                                continue;
                                            }
                                            let selected = self.editor.editing_device_id.as_deref()
                                                == Some(dev_id.as_str());
                                            if ui
                                                .selectable_label(
                                                    selected,
                                                    format!("{dev_id} [Override · offline]"),
                                                )
                                                .clicked()
                                            {
                                                next_target = Some((Some(dev_id.clone()), None));
                                            }
                                        }
                                    });

                                if ui
                                    .add(secondary_btn("Read from Mouse", p))
                                    .on_hover_text(
                                        "Read live DPI, polling rate, LOD, debounce, and sensor toggles directly from the connected mouse into this editor",
                                    )
                                    .clicked()
                                {
                                    read_from_mouse = true;
                                }

                                if self.editor.editing_device_id.is_some()
                                    && ui.add(secondary_btn("Remove", p)).clicked()
                                {
                                    remove_override = true;
                                }
                            });
                            ui.end_row();

                            ui.checkbox(&mut self.editor.set_dpi, "DPI");
                            ui.add_enabled(
                                self.editor.set_dpi,
                                egui::DragValue::new(&mut self.editor.dpi)
                                    .range(dpi_min..=dpi_max)
                                    .speed(f64::from(dpi_step))
                                    .suffix(" DPI"),
                            );
                            ui.end_row();

                            if supports_xy_dpi {
                                ui.checkbox(&mut self.editor.set_dpi_y, "Separate Y DPI");
                                ui.add_enabled(
                                    self.editor.set_dpi_y,
                                    egui::DragValue::new(&mut self.editor.dpi_y)
                                        .range(dpi_min..=dpi_max)
                                        .speed(f64::from(dpi_step))
                                        .suffix(" DPI (vertical)"),
                                );
                                ui.end_row();
                            }

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

                            if lod_options.is_empty() {
                                ui.label(RichText::new("Lift-Off Distance").color(p.text_muted));
                                ui.add(
                                    egui::Label::new(
                                        RichText::new("Not configurable on this mouse")
                                            .size(12.0)
                                            .color(p.text_muted),
                                    )
                                    .wrap(),
                                );
                            } else {
                                ui.checkbox(&mut self.editor.set_lod, "Lift-Off Distance");
                                ui.add_enabled_ui(self.editor.set_lod, |ui| {
                                    egui::ComboBox::from_id_salt("lod_combo")
                                        .selected_text(self.editor.lod.label())
                                        .show_ui(ui, |ui| {
                                            for opt in &lod_options {
                                                ui.selectable_value(
                                                    &mut self.editor.lod,
                                                    *opt,
                                                    opt.label(),
                                                );
                                            }
                                        });
                                });
                            }
                            ui.end_row();

                            if !supports_debounce {
                                ui.label(RichText::new("Debounce").color(p.text_muted));
                                ui.add(
                                    egui::Label::new(
                                        RichText::new("Not configurable on this mouse")
                                            .size(12.0)
                                            .color(p.text_muted),
                                    )
                                    .wrap(),
                                );
                            } else {
                                ui.checkbox(&mut self.editor.set_debounce, "Debounce");
                                ui.add_enabled(
                                    self.editor.set_debounce,
                                    egui::DragValue::new(&mut self.editor.debounce_ms)
                                        .range(db_min..=db_max)
                                        .suffix(" ms"),
                                );
                            }
                            ui.end_row();

                            render_bool_sensor_row(
                                ui,
                                p,
                                "Motion Sync",
                                supports_motion_sync,
                                &mut self.editor.set_motion_sync,
                                &mut self.editor.motion_sync,
                            );
                            ui.end_row();

                            render_bool_sensor_row(
                                ui,
                                p,
                                "Angle Snapping",
                                supports_angle_snapping,
                                &mut self.editor.set_angle_snapping,
                                &mut self.editor.angle_snapping,
                            );
                            ui.end_row();

                            render_bool_sensor_row(
                                ui,
                                p,
                                "Ripple Control",
                                supports_ripple_control,
                                &mut self.editor.set_ripple_control,
                                &mut self.editor.ripple_control,
                            );
                            ui.end_row();

                            if !supports_sleep {
                                ui.label(RichText::new("Idle Sleep").color(p.text_muted));
                                ui.add(
                                    egui::Label::new(
                                        RichText::new("Not configurable on this mouse")
                                            .size(12.0)
                                            .color(p.text_muted),
                                    )
                                    .wrap(),
                                );
                            } else {
                                ui.checkbox(&mut self.editor.set_sleep, "Idle Sleep");
                                ui.add_enabled_ui(self.editor.set_sleep, |ui| {
                                    const SLEEP_PRESETS: &[(u16, &str)] = &[
                                        (10, "10 s"),
                                        (30, "30 s"),
                                        (60, "1 min"),
                                        (120, "2 min"),
                                        (180, "3 min"),
                                        (300, "5 min"),
                                        (600, "10 min"),
                                        (900, "15 min"),
                                        (1800, "30 min"),
                                    ];
                                    let current_lbl = format_sleep_secs(self.editor.sleep_timeout_secs);
                                    egui::ComboBox::from_id_salt("sleep_timeout_combo")
                                        .selected_text(current_lbl)
                                        .show_ui(ui, |ui| {
                                            for &(secs, lbl) in SLEEP_PRESETS {
                                                if secs >= sleep_min && secs <= sleep_max {
                                                    ui.selectable_value(
                                                        &mut self.editor.sleep_timeout_secs,
                                                        secs,
                                                        lbl,
                                                    );
                                                }
                                            }
                                        });
                                });
                            }
                            ui.end_row();

                            ui.checkbox(&mut self.editor.set_win_speed, "Windows Speed");
                            ui.add_enabled(
                                self.editor.set_win_speed,
                                egui::DragValue::new(&mut self.editor.win_speed)
                                    .range(1..=20)
                                    .suffix("/20 (10 = 6/11)"),
                            );
                            ui.end_row();

                            ui.checkbox(&mut self.editor.set_power_plan, "Power Plan");
                            ui.add_enabled_ui(self.editor.set_power_plan, |ui| {
                                egui::ComboBox::from_id_salt("power_plan_combo")
                                    .selected_text(self.editor.power_plan.label())
                                    .show_ui(ui, |ui| {
                                        for preset in PowerPlanPreset::ALL {
                                            ui.selectable_value(
                                                &mut self.editor.power_plan,
                                                preset,
                                                preset.label(),
                                            );
                                        }
                                    });
                            });
                            ui.end_row();

                            let audio_combo_w = (field_w - 80.0).clamp(140.0, 300.0);

                            ui.label(RichText::new("Audio Output").color(p.text_muted));
                            ui.horizontal(|ui| {
                                let out_selected_label = match &self.editor.audio_output_id {
                                    None => "Leave unchanged".to_string(),
                                    Some(sel_id) => snap
                                        .and_then(|s| {
                                            s.audio_outputs
                                                .iter()
                                                .find(|(id, _, _)| id == sel_id)
                                                .map(|(_, name, _)| name.clone())
                                        })
                                        .or_else(|| {
                                            self.editor
                                                .audio_output_name
                                                .as_ref()
                                                .map(|n| format!("{n} [offline]"))
                                        })
                                        .unwrap_or_else(|| "Saved endpoint [offline]".to_string()),
                                };
                                egui::ComboBox::from_id_salt("audio_output_combo")
                                    .selected_text(out_selected_label)
                                    .width(audio_combo_w)
                                    .show_ui(ui, |ui| {
                                        if ui
                                            .selectable_label(
                                                self.editor.audio_output_id.is_none(),
                                                "Leave unchanged",
                                            )
                                            .clicked()
                                        {
                                            self.editor.audio_output_id = None;
                                            self.editor.audio_output_name = None;
                                        }
                                        if let Some(s) = snap {
                                            for (id, name, is_default) in &s.audio_outputs {
                                                let lbl = if *is_default {
                                                    format!("{name} (default)")
                                                } else {
                                                    name.clone()
                                                };
                                                let selected = self
                                                    .editor
                                                    .audio_output_id
                                                    .as_deref()
                                                    == Some(id.as_str());
                                                if ui.selectable_label(selected, lbl).clicked() {
                                                    self.editor.audio_output_id = Some(id.clone());
                                                    self.editor.audio_output_name =
                                                        Some(name.clone());
                                                }
                                            }
                                        }
                                    });
                                if ui
                                    .add(secondary_btn("Refresh", p))
                                    .on_hover_text("Re-scan Windows playback & microphone endpoints")
                                    .clicked()
                                {
                                    refresh_audio = true;
                                }
                            });
                            ui.end_row();

                            ui.checkbox(&mut self.editor.set_audio_output_vol, "Output Volume");
                            ui.add_enabled(
                                self.editor.set_audio_output_vol,
                                egui::DragValue::new(&mut self.editor.audio_output_vol)
                                    .range(0..=100)
                                    .speed(1)
                                    .suffix("%"),
                            );
                            ui.end_row();

                            ui.label(RichText::new("Microphone").color(p.text_muted));
                            let in_selected_label = match &self.editor.audio_input_id {
                                None => "Leave unchanged".to_string(),
                                Some(sel_id) => snap
                                    .and_then(|s| {
                                        s.audio_inputs
                                            .iter()
                                            .find(|(id, _, _)| id == sel_id)
                                            .map(|(_, name, _)| name.clone())
                                    })
                                    .or_else(|| {
                                        self.editor
                                            .audio_input_name
                                            .as_ref()
                                            .map(|n| format!("{n} [offline]"))
                                    })
                                    .unwrap_or_else(|| "Saved microphone [offline]".to_string()),
                            };
                            egui::ComboBox::from_id_salt("audio_input_combo")
                                .selected_text(in_selected_label)
                                .width(audio_combo_w)
                                .show_ui(ui, |ui| {
                                    if ui
                                        .selectable_label(
                                            self.editor.audio_input_id.is_none(),
                                            "Leave unchanged",
                                        )
                                        .clicked()
                                    {
                                        self.editor.audio_input_id = None;
                                        self.editor.audio_input_name = None;
                                    }
                                    if let Some(s) = snap {
                                        for (id, name, is_default) in &s.audio_inputs {
                                            let lbl = if *is_default {
                                                format!("{name} (default)")
                                            } else {
                                                name.clone()
                                            };
                                            let selected = self.editor.audio_input_id.as_deref()
                                                == Some(id.as_str());
                                            if ui.selectable_label(selected, lbl).clicked() {
                                                self.editor.audio_input_id = Some(id.clone());
                                                self.editor.audio_input_name = Some(name.clone());
                                            }
                                        }
                                    }
                                });
                            ui.end_row();

                            ui.checkbox(&mut self.editor.set_audio_input_vol, "Mic Volume");
                            ui.add_enabled_ui(self.editor.set_audio_input_vol, |ui| {
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::DragValue::new(&mut self.editor.audio_input_vol)
                                            .range(0..=100)
                                            .speed(1)
                                            .suffix("%"),
                                    );
                                    ui.checkbox(&mut self.editor.lock_mic_volume, "Lock Gain")
                                        .on_hover_text(
                                            "Enforces this microphone level every 2 seconds while active so games, Steam Voice, or Discord cannot auto-adjust your mic slider",
                                        );
                                });
                            });
                            ui.end_row();

                            ui.label(RichText::new("Notes (optional)").color(p.text_muted));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.editor.notes)
                                    .desired_width(field_w)
                                    .hint_text("Saved in exported profile JSON cards"),
                            );
                            ui.end_row();
                        });
                });

            if refresh_audio {
                runtime.send(ProfileRuntimeCmd::RefreshAudioDevices);
            }
            if read_from_mouse {
                runtime.send(ProfileRuntimeCmd::ReadFromMouseForEditor(
                    self.editor.editing_device_id.clone(),
                ));
            }
            if open_game_picker {
                self.game_picker_open = true;
                self.game_picker_search.clear();
                self.discovered_running = discover_running_apps();
                if self.discovered_installed.is_empty() {
                    self.discovered_installed = discover_installed_games();
                }
            }
            if remove_override {
                self.editor.remove_current_override();
            } else if let Some((target_id, caps)) = next_target {
                self.editor.switch_editing_target(target_id, caps.as_ref());
            }
        });
    }

    fn render_activity_log_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        p: Palette,
        card_w: f32,
        fixed_scroll_h: Option<f32>,
        snap: Option<&ProfileRuntimeSnapshot>,
    ) {
        let inner_w = (card_w - 24.0).max(120.0);
        card_frame(ui, p, card_w, |ui| {
            let log_entries = snap.map(|s| s.log.as_slice()).unwrap_or(&[]);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Activity Log").strong().color(p.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!log_entries.is_empty(), secondary_btn("Copy Log", p))
                        .clicked()
                    {
                        let text = log_entries
                            .iter()
                            .map(|entry| {
                                format!(
                                    "{} [{}] {}",
                                    entry.timestamp,
                                    entry.severity.as_str().to_uppercase(),
                                    entry.message
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        ctx.copy_text(text);
                        self.notice = Some((
                            Severity::Ok,
                            format!(
                                "Copied {} activity log {} to clipboard.",
                                log_entries.len(),
                                if log_entries.len() == 1 {
                                    "line"
                                } else {
                                    "lines"
                                }
                            ),
                        ));
                    }
                });
            });
            ui.add_space(4.0);
            let log_scroll_h =
                fixed_scroll_h.unwrap_or_else(|| (ui.available_height() - 4.0).max(60.0));
            let row_w = (inner_w - 14.0).max(120.0);
            egui::ScrollArea::vertical()
                .id_salt("profile_activity_log")
                .min_scrolled_height(log_scroll_h)
                .max_height(log_scroll_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_max_width(row_w);
                    if log_entries.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(
                                    "No activity yet. Switch windows or click Apply Now to test.",
                                )
                                .size(11.5)
                                .color(p.text_muted),
                            )
                            .wrap(),
                        );
                    }
                    for entry in log_entries {
                        let mut job = LayoutJob::default();
                        job.wrap.max_width = row_w;
                        job.append(
                            &format!("{}  ", entry.timestamp),
                            0.0,
                            TextFormat {
                                font_id: FontId::monospace(11.0),
                                color: p.text_muted,
                                ..Default::default()
                            },
                        );
                        job.append(
                            &format!("[{}]  ", entry.severity.as_str().to_uppercase()),
                            0.0,
                            TextFormat {
                                font_id: FontId::monospace(11.0),
                                color: p.severity_fg(entry.severity),
                                ..Default::default()
                            },
                        );
                        job.append(
                            &entry.message,
                            0.0,
                            TextFormat {
                                font_id: FontId::proportional(11.5),
                                color: p.text,
                                ..Default::default()
                            },
                        );
                        ui.add(egui::Label::new(job).wrap());
                        ui.add_space(2.0);
                    }
                });
        });
    }
}

fn card_frame(
    ui: &mut egui::Ui,
    p: Palette,
    outer_w: f32,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let inner_w = (outer_w - 24.0).max(120.0);
    egui::Frame::NONE
        .fill(p.surface)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(inner_w);
            ui.set_max_width(inner_w);
            add_contents(ui);
        });
}

fn primary_btn(label: &str, p: Palette) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(p.btn_primary_fg).strong())
        .fill(p.btn_primary_bg)
        .corner_radius(CornerRadius::same(6))
}

fn format_sleep_secs(secs: u16) -> String {
    if secs >= 60 && secs.is_multiple_of(60) {
        format!("{} min", secs / 60)
    } else {
        format!("{secs} s")
    }
}

fn secondary_btn(label: &str, p: Palette) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(p.text))
        .fill(p.surface_alt)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(6))
}

fn render_bool_sensor_row(
    ui: &mut egui::Ui,
    p: Palette,
    label: &str,
    supported: bool,
    enabled: &mut bool,
    value: &mut bool,
) {
    if !supported {
        ui.label(RichText::new(label).color(p.text_muted));
        ui.add(
            egui::Label::new(
                RichText::new("Not configurable on this mouse")
                    .size(12.0)
                    .color(p.text_muted),
            )
            .wrap(),
        );
    } else {
        ui.checkbox(enabled, label);
        ui.add_enabled_ui(*enabled, |ui| {
            ui.horizontal(|ui| {
                let on_btn = if *value {
                    primary_btn("ON", p)
                } else {
                    secondary_btn("ON", p)
                };
                if ui.add(on_btn).clicked() {
                    *value = true;
                }
                let off_btn = if !*value {
                    primary_btn("OFF", p)
                } else {
                    secondary_btn("OFF", p)
                };
                if ui.add(off_btn).clicked() {
                    *value = false;
                }
            });
        });
    }
}

/// Pre-measures all status pills and packs them into rows strictly bounded by `max_row_w`
/// so pills always wrap reactively when the user resizes the window.
fn render_status_pill_rows(
    ui: &mut egui::Ui,
    p: Palette,
    max_row_w: f32,
    pills: &[(String, String)],
) {
    let pad_x = 8.0_f32;
    let pad_y = 4.0_f32;
    let inner_gap = 6.0_f32;
    let pill_gap_x = 6.0_f32;
    let pill_gap_y = 6.0_f32;
    let row_limit = max_row_w.max(120.0);

    struct MeasuredPill {
        label_text: String,
        value_text: String,
        label_galley: std::sync::Arc<egui::Galley>,
        value_galley: std::sync::Arc<egui::Galley>,
        full_w: f32,
        clamped_w: f32,
        height: f32,
    }

    let measured: Vec<MeasuredPill> = pills
        .iter()
        .map(|(label, value)| {
            let label_galley = ui.painter().layout_no_wrap(
                label.clone(),
                FontId::proportional(10.0),
                p.text_muted,
            );
            let value_galley =
                ui.painter()
                    .layout_no_wrap(value.clone(), FontId::monospace(11.0), p.text);
            let full_w = label_galley.size().x + inner_gap + value_galley.size().x + pad_x * 2.0;
            let clamped_w = full_w.min(row_limit);
            let height = label_galley.size().y.max(value_galley.size().y) + pad_y * 2.0;
            MeasuredPill {
                label_text: label.clone(),
                value_text: value.clone(),
                label_galley,
                value_galley,
                full_w,
                clamped_w,
                height,
            }
        })
        .collect();

    // Group pills into rows strictly fitting inside `row_limit`.
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut current_row: Vec<usize> = Vec::new();
    let mut current_w = 0.0_f32;

    for (idx, item) in measured.iter().enumerate() {
        let added = if current_row.is_empty() {
            item.clamped_w
        } else {
            pill_gap_x + item.clamped_w
        };
        if !current_row.is_empty() && current_w + added > row_limit {
            rows.push(std::mem::take(&mut current_row));
            current_w = 0.0;
        }
        current_w += if current_row.is_empty() {
            item.clamped_w
        } else {
            pill_gap_x + item.clamped_w
        };
        current_row.push(idx);
    }
    if !current_row.is_empty() {
        rows.push(current_row);
    }

    for (row_idx, row) in rows.iter().enumerate() {
        if row_idx > 0 {
            ui.add_space(pill_gap_y);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = pill_gap_x;
            for &idx in row {
                let item = &measured[idx];
                let (rect, resp) = ui.allocate_exact_size(
                    Vec2::new(item.clamped_w, item.height),
                    egui::Sense::hover(),
                );
                if ui.is_rect_visible(rect) {
                    let painter = ui.painter_at(rect);
                    painter.rect(
                        rect,
                        CornerRadius::same(6),
                        p.surface_alt,
                        Stroke::new(1.0_f32, p.border),
                        egui::StrokeKind::Inside,
                    );
                    let label_pos = egui::pos2(
                        rect.left() + pad_x,
                        rect.center().y - item.label_galley.size().y * 0.5,
                    );
                    let value_pos = egui::pos2(
                        label_pos.x + item.label_galley.size().x + inner_gap,
                        rect.center().y - item.value_galley.size().y * 0.5,
                    );
                    painter.galley(label_pos, item.label_galley.clone(), p.text_muted);
                    painter.galley(value_pos, item.value_galley.clone(), p.text);
                }
                if item.full_w > item.clamped_w + 1.0 {
                    resp.on_hover_text(format!("{}: {}", item.label_text, item.value_text));
                }
            }
        });
    }
}

fn semantic_banner(
    ui: &mut egui::Ui,
    p: Palette,
    mode: ThemeMode,
    outer_w: f32,
    severity: Severity,
    text: &str,
) {
    let fg = p.severity_fg(severity);
    let bg = p.severity_bg(severity, mode);
    let border = Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), 120);
    let inner_w = (outer_w - 20.0).max(120.0);
    egui::Frame::NONE
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_min_width(inner_w);
            ui.set_max_width(inner_w);
            let mut job = LayoutJob::default();
            job.wrap.max_width = inner_w;
            job.append(
                &format!("{}  ", severity.as_str().to_uppercase()),
                0.0,
                TextFormat {
                    font_id: FontId::proportional(11.0),
                    color: fg,
                    ..Default::default()
                },
            );
            job.append(
                text,
                0.0,
                TextFormat {
                    font_id: FontId::proportional(12.0),
                    color: p.text,
                    ..Default::default()
                },
            );
            ui.add(egui::Label::new(job).wrap());
        });
}
