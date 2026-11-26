// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plain monochrome Black & White UI for Phase 2 Per-Game Peripheral Profiles.
//! Only the four semantic statuses (`OK`, `INFO`, `WARN`, `BAD`) carry chroma.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use chrono::Local;
use eframe::egui::text::LayoutJob;
use eframe::egui::{
    self, Color32, CornerRadius, FontId, Margin, RichText, Stroke, TextFormat, Vec2,
};
use polled_core::win::{SystemEnv, WindowsEnv};
use polled_core::{
    compute_sensitivity, discover_installed_games, discover_running_apps, dump_hid_devices,
    export_profile_json, format_unsupported_mouse_issue, import_profile_json, AppPaths,
    DeviceCapabilities, DiscoveredGame, GameProfile, GameSource, KeyboardProfileSettings,
    LiftOffDistance, MouseSettings, PowerPlanPreset, ProfileStore, SensitivityGame, Severity,
    SupportTier, WootingKeyboardInfo,
};

use crate::profile_runtime::{
    BackendSelection, ProfileRuntimeCmd, ProfileRuntimeHandle, ProfileRuntimeSnapshot,
};
use crate::theme::{Palette, ThemeMode};

const DISPLAY_TEST_UI_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
struct DisplayTestCountdown {
    device_name: String,
    applied_label: String,
    previous_label: String,
    expires_at: Instant,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum GamePickerMode {
    #[default]
    AddExeToCurrent,
    CreateProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ProfileEditorTab {
    #[default]
    Mouse,
    Keyboard,
    AudioPower,
    General,
}

pub struct ProfilesUiState {
    pub store: ProfileStore,
    pub selected_profile_id: String,
    pub editor: ProfileDraft,
    editor_tab: ProfileEditorTab,
    activity_log_open: bool,
    pub runtime_snapshot: Option<ProfileRuntimeSnapshot>,
    pub export_modal_json: Option<String>,
    pub import_modal_open: bool,
    pub import_text: String,
    game_picker_open: bool,
    game_picker_mode: GamePickerMode,
    game_picker_tab: GamePickerTab,
    game_picker_source_filter: Option<GameSource>,
    game_picker_search: String,
    discovered_installed: Vec<DiscoveredGame>,
    discovered_running: Vec<DiscoveredGame>,
    pub notice: Option<(Severity, String)>,
    report_mouse_modal: Option<String>,
    display_test_countdown: Option<DisplayTestCountdown>,
    acknowledged_unverified_devices: HashSet<String>,
    pending_unverified_action: Option<PendingUnverifiedWrite>,
    sens_helper_open: bool,
    sens_helper_game: SensitivityGame,
    sens_helper_base_dpi: u16,
    sens_helper_in_game: f64,
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
    pub set_dpi_stage: bool,
    pub dpi_stage: u8,
    pub lock_dpi_stages: bool,
    pub set_glass_mode: bool,
    pub glass_mode: bool,
    pub set_display_mode: bool,
    pub display_device_name: Option<String>,
    pub display_width: u32,
    pub display_height: u32,
    pub display_refresh_hz: u32,
    pub wooting_profile: Option<u8>,
    pub set_kb_polling: bool,
    pub kb_polling_rate_hz: u16,
    pub set_kb_actuation: bool,
    pub kb_actuation_mm: f32,
    pub set_kb_rt_toggle: bool,
    pub kb_rt_enabled: bool,
    pub set_kb_rt_mm: bool,
    pub kb_rt_mm: f32,
    pub set_kb_socd: bool,
    pub kb_socd_enabled: bool,
    pub set_kb_debounce: bool,
    pub kb_debounce_typing: bool,
    pub kb_debounce_ms: u8,
    pub force_high_perf_gpu: bool,
    pub quiet_notifications: bool,
    pub duck_audio_apps_csv: String,
    pub duck_audio_volume_pct: u8,
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
            set_dpi_stage: false,
            dpi_stage: 1,
            lock_dpi_stages: false,
            set_glass_mode: false,
            glass_mode: false,
            set_display_mode: p.display_mode_override().is_some(),
            display_device_name: p.display_device_name.clone(),
            display_width: p.display_width.unwrap_or(1920),
            display_height: p.display_height.unwrap_or(1080),
            display_refresh_hz: p.display_refresh_hz.unwrap_or(240),
            wooting_profile: p.wooting_profile,
            set_kb_polling: p
                .keyboard_preset
                .as_ref()
                .is_some_and(|k| k.polling_rate_hz.is_some()),
            kb_polling_rate_hz: p
                .keyboard_preset
                .as_ref()
                .and_then(|k| k.polling_rate_hz)
                .unwrap_or(1000),
            set_kb_actuation: p
                .keyboard_preset
                .as_ref()
                .is_some_and(|k| k.actuation_hundredths_mm.is_some()),
            kb_actuation_mm: p
                .keyboard_preset
                .as_ref()
                .and_then(KeyboardProfileSettings::actuation_mm)
                .unwrap_or(0.40),
            set_kb_rt_toggle: p
                .keyboard_preset
                .as_ref()
                .is_some_and(|k| k.rapid_trigger_enabled.is_some()),
            kb_rt_enabled: p
                .keyboard_preset
                .as_ref()
                .and_then(|k| k.rapid_trigger_enabled)
                .unwrap_or(true),
            set_kb_rt_mm: p
                .keyboard_preset
                .as_ref()
                .is_some_and(|k| k.rapid_trigger_hundredths_mm.is_some()),
            kb_rt_mm: p
                .keyboard_preset
                .as_ref()
                .and_then(KeyboardProfileSettings::rapid_trigger_mm)
                .unwrap_or(0.15),
            set_kb_socd: p
                .keyboard_preset
                .as_ref()
                .is_some_and(|k| k.socd_enabled.is_some()),
            kb_socd_enabled: p
                .keyboard_preset
                .as_ref()
                .and_then(|k| k.socd_enabled)
                .unwrap_or(false),
            set_kb_debounce: p
                .keyboard_preset
                .as_ref()
                .is_some_and(|k| k.debounce_typing_mode.is_some() || k.debounce_ms.is_some()),
            kb_debounce_typing: p
                .keyboard_preset
                .as_ref()
                .and_then(|k| k.debounce_typing_mode)
                .unwrap_or(false),
            kb_debounce_ms: p
                .keyboard_preset
                .as_ref()
                .and_then(|k| k.debounce_ms)
                .unwrap_or(10),
            force_high_perf_gpu: p.force_high_perf_gpu,
            quiet_notifications: p.quiet_notifications,
            duck_audio_apps_csv: p.duck_audio_apps.join(", "),
            duck_audio_volume_pct: p.duck_audio_volume_pct,
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
        self.set_dpi_stage = m.dpi_stage.is_some();
        self.dpi_stage = m.dpi_stage.unwrap_or(1).max(1);
        self.lock_dpi_stages = m.lock_dpi_stages == Some(true);
        self.set_glass_mode = m.glass_mode.is_some();
        self.glass_mode = m.glass_mode.unwrap_or(false);
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
            dpi_stage: self.set_dpi_stage.then_some(self.dpi_stage.max(1)),
            lock_dpi_stages: self.lock_dpi_stages.then_some(true),
            glass_mode: self.set_glass_mode.then_some(self.glass_mode),
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
            display_device_name: copy
                .set_display_mode
                .then_some(copy.display_device_name)
                .flatten(),
            display_width: copy.set_display_mode.then_some(copy.display_width),
            display_height: copy.set_display_mode.then_some(copy.display_height),
            display_refresh_hz: copy.set_display_mode.then_some(copy.display_refresh_hz),
            wooting_profile: copy.wooting_profile,
            keyboard_preset: KeyboardProfileSettings {
                polling_rate_hz: copy.set_kb_polling.then_some(copy.kb_polling_rate_hz),
                actuation_hundredths_mm: copy
                    .set_kb_actuation
                    .then_some((copy.kb_actuation_mm.clamp(0.10, 4.00) * 100.0).round() as u16),
                rapid_trigger_enabled: copy.set_kb_rt_toggle.then_some(copy.kb_rt_enabled),
                rapid_trigger_hundredths_mm: copy
                    .set_kb_rt_mm
                    .then_some((copy.kb_rt_mm.clamp(0.10, 2.50) * 100.0).round() as u16),
                socd_enabled: copy.set_kb_socd.then_some(copy.kb_socd_enabled),
                debounce_typing_mode: copy.set_kb_debounce.then_some(copy.kb_debounce_typing),
                debounce_ms: copy
                    .set_kb_debounce
                    .then_some(copy.kb_debounce_ms.clamp(5, 40)),
            }
            .sanitized(),
            force_high_perf_gpu: !copy.is_desktop_default && copy.force_high_perf_gpu,
            quiet_notifications: copy.quiet_notifications,
            duck_audio_apps: copy
                .duck_audio_apps_csv
                .split([',', ';', '\n'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            duck_audio_volume_pct: copy.duck_audio_volume_pct.min(100),
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
            editor_tab: ProfileEditorTab::Mouse,
            activity_log_open: false,
            store,
            runtime_snapshot: None,
            export_modal_json: None,
            import_modal_open: false,
            import_text: String::new(),
            game_picker_open: false,
            game_picker_mode: GamePickerMode::AddExeToCurrent,
            game_picker_tab: GamePickerTab::Installed,
            game_picker_source_filter: None,
            game_picker_search: String::new(),
            discovered_installed: Vec::new(),
            discovered_running: Vec::new(),
            notice: None,
            report_mouse_modal: None,
            display_test_countdown: None,
            acknowledged_unverified_devices: HashSet::new(),
            pending_unverified_action: None,
            sens_helper_open: false,
            sens_helper_game: SensitivityGame::Cs2,
            sens_helper_base_dpi: 800,
            sens_helper_in_game: 1.0,
        }
    }

    pub fn reload_store(&mut self, store: ProfileStore) {
        self.store = store;
        let target_id = if self
            .store
            .profiles
            .iter()
            .any(|p| p.id == self.selected_profile_id)
        {
            self.selected_profile_id.clone()
        } else {
            self.store
                .profiles
                .first()
                .map(|p| p.id.clone())
                .unwrap_or_else(|| "desktop".to_string())
        };
        self.select_profile(&target_id);
    }

    pub fn on_display_test_started(
        &mut self,
        device_name: String,
        applied_label: String,
        previous_label: String,
        error: Option<String>,
    ) {
        if let Some(err) = error {
            self.display_test_countdown = None;
            self.notice = Some((Severity::Warn, err));
            return;
        }
        self.display_test_countdown = Some(DisplayTestCountdown {
            device_name,
            applied_label,
            previous_label,
            expires_at: Instant::now() + DISPLAY_TEST_UI_TIMEOUT,
        });
    }

    pub fn on_display_test_reverted(&mut self, message: String) {
        self.display_test_countdown = None;
        self.notice = Some((Severity::Info, message));
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
        let content_w = ui.available_width().max(320.0);
        ui.set_max_width(content_w);

        let snap = self.runtime_snapshot.clone();
        let stacked = content_w < 640.0;

        // Clean single-row Page Header
        ui.horizontal(|ui| {
            ui.label(RichText::new("Profiles").size(20.0).strong().color(p.text));
            if let Some(s) = snap.as_ref() {
                ui.add_space(8.0);
                let active_lbl = if s.profile_locked {
                    format!("{} · Locked", s.active_profile_name)
                } else {
                    s.active_profile_name.clone()
                };
                ui.label(
                    RichText::new(active_lbl)
                        .size(12.5)
                        .color(if s.profile_locked {
                            p.warn
                        } else {
                            p.text_muted
                        }),
                );
                if let Some(exe) = &s.foreground_exe {
                    ui.label(
                        RichText::new(format!("· {exe}"))
                            .size(11.5)
                            .monospace()
                            .color(p.text_muted),
                    );
                }
            }
        });
        ui.add_space(10.0);

        if stacked {
            egui::ScrollArea::vertical()
                .id_salt("profiles_stacked_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let stack_w = (content_w - 10.0).max(280.0);
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
                    self.render_profile_editor_card(ui, p, stack_w, 340.0, snap.as_ref(), runtime);
                    ui.add_space(10.0);
                    self.render_activity_log_card(ctx, ui, p, stack_w, Some(150.0), snap.as_ref());
                });
        } else {
            // --- COMPACT DEVICE & AUTO-SWITCH BAR ---
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

            ui.add_space(12.0);

            // --- TWO-COLUMN LAYOUT: PROFILE LIST + TABBED PROFILE EDITOR ---
            let gap = 12.0;
            let left_w = (content_w * 0.27).clamp(195.0, 250.0);
            let right_w = (content_w - left_w - gap).max(320.0);
            let rem_h = ui.available_height();
            let log_reserve = if self.activity_log_open { 172.0 } else { 52.0 };
            let mid_h = (rem_h - log_reserve - 12.0).clamp(220.0, 520.0);
            let inner_col_h = (mid_h - 24.0).max(180.0);

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

            // --- COLLAPSIBLE ACTIVITY LOG STRIP ---
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

        // --- SMART GAME PICKER MODAL (1-CLICK CREATE PROFILE OR ADD EXE) ---
        if self.game_picker_open {
            let mut close_picker = false;
            let mut refresh_picker = false;
            let mut picked_game: Option<(String, String)> = None;
            let is_create_mode = self.game_picker_mode == GamePickerMode::CreateProfile;
            let modal_title = if is_create_mode {
                "New Profile from Installed Game / App"
            } else {
                "Smart Game Picker (Installed & Running)"
            };
            let picker_w = (ctx.screen_rect().width() - 48.0).clamp(380.0, 580.0);
            egui::Window::new(modal_title)
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

                    if self.game_picker_tab == GamePickerTab::Installed
                        && !self.discovered_installed.is_empty()
                    {
                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            let all_btn = if self.game_picker_source_filter.is_none() {
                                primary_btn("All Stores", p)
                            } else {
                                secondary_btn("All Stores", p)
                            };
                            if ui.add(all_btn).clicked() {
                                self.game_picker_source_filter = None;
                            }
                            for src in [
                                GameSource::Steam,
                                GameSource::Riot,
                                GameSource::Epic,
                                GameSource::BattleNet,
                                GameSource::Ubisoft,
                                GameSource::EaApp,
                            ] {
                                let count = self
                                    .discovered_installed
                                    .iter()
                                    .filter(|g| g.source == src)
                                    .count();
                                if count == 0 {
                                    continue;
                                }
                                let lbl = format!("{} ({count})", src.label());
                                let btn = if self.game_picker_source_filter == Some(src) {
                                    primary_btn(&lbl, p)
                                } else {
                                    secondary_btn(&lbl, p)
                                };
                                if ui.add(btn).clicked() {
                                    self.game_picker_source_filter = Some(src);
                                }
                            }
                        });
                    }

                    ui.add_space(6.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.game_picker_search)
                            .desired_width(f32::INFINITY)
                            .hint_text("Filter by game or .exe name (Steam, Riot, Epic, Battle.net, Ubisoft, EA)..."),
                    );
                    ui.add_space(8.0);

                    let query = self.game_picker_search.trim().to_ascii_lowercase();
                    let list = match self.game_picker_tab {
                        GamePickerTab::Installed => &self.discovered_installed,
                        GamePickerTab::Running => &self.discovered_running,
                    };
                    let existing_in_draft: Vec<String> = self
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
                                if self.game_picker_tab == GamePickerTab::Installed {
                                    if let Some(filter_src) = self.game_picker_source_filter {
                                        if g.source != filter_src {
                                            continue;
                                        }
                                    }
                                }
                                if !query.is_empty()
                                    && !g.display_name.to_ascii_lowercase().contains(&query)
                                    && !g.exe_name.contains(&query)
                                {
                                    continue;
                                }
                                shown += 1;
                                let already = if is_create_mode {
                                    self.store.profiles.iter().any(|prof| {
                                        prof.executables
                                            .iter()
                                            .any(|e| e.eq_ignore_ascii_case(&g.exe_name))
                                    })
                                } else {
                                    existing_in_draft.iter().any(|e| e == &g.exe_name)
                                };
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
                                                            RichText::new(if is_create_mode {
                                                                "PROFILE EXISTS"
                                                            } else {
                                                                "ADDED"
                                                            })
                                                            .size(10.5)
                                                            .strong()
                                                            .color(p.ok),
                                                        );
                                                    } else {
                                                        let btn_lbl = if is_create_mode {
                                                            "+ Create Profile"
                                                        } else {
                                                            "+ Add"
                                                        };
                                                        if ui.add(primary_btn(btn_lbl, p)).clicked()
                                                        {
                                                            picked_game = Some((
                                                                g.display_name.clone(),
                                                                g.exe_name.clone(),
                                                            ));
                                                        }
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
                                        "No matching applications found. You can also create a blank profile and type any .exe manually.",
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
            if let Some((title, exe)) = picked_game {
                if is_create_mode {
                    let base_slug: String = title
                        .chars()
                        .map(|c| {
                            if c.is_ascii_alphanumeric() {
                                c.to_ascii_lowercase()
                            } else {
                                '_'
                            }
                        })
                        .collect::<String>()
                        .split('_')
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("_");
                    let slug = if base_slug.is_empty() {
                        format!("game_{}", self.store.profiles.len())
                    } else {
                        base_slug
                    };
                    let mut unique_id = slug.clone();
                    let mut suffix = 2usize;
                    while self.store.profiles.iter().any(|p| p.id == unique_id) {
                        unique_id = format!("{slug}_{suffix}");
                        suffix += 1;
                    }
                    let new_prof = GameProfile {
                        id: unique_id.clone(),
                        name: title.clone(),
                        is_desktop_default: false,
                        executables: vec![exe.clone()],
                        mouse: MouseSettings {
                            dpi: Some(800),
                            polling_rate_hz: Some(4000),
                            lod: Some(LiftOffDistance::Mm10),
                            debounce_ms: Some(1),
                            ..Default::default()
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
                        display_device_name: None,
                        display_width: None,
                        display_height: None,
                        display_refresh_hz: None,
                        wooting_profile: None,
                        keyboard_preset: None,
                        force_high_perf_gpu: false,
                        quiet_notifications: false,
                        duck_audio_apps: Vec::new(),
                        duck_audio_volume_pct: 15,
                        author: None,
                        notes: None,
                    }
                    .sanitized();
                    self.store.upsert_profile(new_prof);
                    self.select_profile(&unique_id);
                    runtime.send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
                    self.notice = Some((
                        Severity::Ok,
                        format!("Created & saved profile '{title}' ({exe}). Tune Mouse, Keyboard, or Display tabs anytime."),
                    ));
                    close_picker = true;
                } else {
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
                        format!(
                            "Added '{exe}' ({title}) to profile draft — click Save to persist."
                        ),
                    ));
                }
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

        // --- DISPLAY MODE TEST 10-SECOND AUTO-REVERT GUARD MODAL ---
        if let Some(test) = self.display_test_countdown.clone() {
            let now = Instant::now();
            if now >= test.expires_at {
                self.display_test_countdown = None;
                runtime.send(ProfileRuntimeCmd::RevertTestDisplayMode);
            } else {
                let rem_secs = test.expires_at.saturating_duration_since(now).as_secs_f32();
                ctx.request_repaint_after(Duration::from_millis(100));
                let mut revert_now = false;
                let mut keep_active = false;
                let guard_w = (ctx.screen_rect().width() - 48.0).clamp(340.0, 460.0);
                egui::Window::new("Display Mode Test — Safe Auto-Revert")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(guard_w)
                    .max_width(guard_w)
                    .frame(
                        egui::Frame::NONE
                            .fill(p.surface)
                            .stroke(Stroke::new(1.5_f32, p.text))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::same(16)),
                    )
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Switched {} to {}",
                                test.device_name, test.applied_label
                            ))
                            .size(14.5)
                            .strong()
                            .color(p.text),
                        );
                        ui.add_space(6.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "Automatically reverting to {} in {:.0}s if you don't respond.",
                                    test.previous_label,
                                    rem_secs.ceil()
                                ))
                                .size(12.5)
                                .color(p.warn),
                            )
                            .wrap(),
                        );
                        ui.add_space(6.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(
                                    "Display mode switches are strictly temporary (never written to the Windows registry). Click Save on your profile to use this mode automatically in-game.",
                                )
                                .size(11.5)
                                .color(p.text_muted),
                            )
                            .wrap(),
                        );
                        ui.add_space(12.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(primary_btn("Revert Now", p)).clicked() {
                                revert_now = true;
                            }
                            if ui.add(secondary_btn("Keep Active", p)).clicked() {
                                keep_active = true;
                            }
                        });
                    });
                if revert_now {
                    self.display_test_countdown = None;
                    runtime.send(ProfileRuntimeCmd::RevertTestDisplayMode);
                } else if keep_active {
                    self.display_test_countdown = None;
                    runtime.send(ProfileRuntimeCmd::KeepTestDisplayMode);
                    self.notice = Some((
                        Severity::Ok,
                        format!(
                            "Kept {} temporarily (will still restore {} on desktop switch or exit).",
                            test.applied_label, test.previous_label
                        ),
                    ));
                }
            }
        }

        // --- REPORT UNSUPPORTED MOUSE MODAL (SERIAL-REDACTED) ---
        let mut close_report = false;
        if let Some(md) = self.report_mouse_modal.clone() {
            let report_w = (ctx.screen_rect().width() - 48.0).clamp(380.0, 620.0);
            egui::Window::new("Report Unsupported Mouse (Serial-Redacted)")
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
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Paste this serial-redacted HID + USB topology report into a GitHub Issue so support for your mouse can be added safely:",
                            )
                            .size(12.0)
                            .color(p.text_muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(8.0);
                    let mut preview = md.clone();
                    egui::ScrollArea::vertical()
                        .id_salt("report_mouse_scroll")
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
                            let filename = format!(
                                "polled_mouse_report_{}.md",
                                Local::now().format("%Y%m%d_%H%M%S")
                            );
                            let path = dir.join(filename);
                            match std::fs::create_dir_all(&dir)
                                .and_then(|()| std::fs::write(&path, &md))
                            {
                                Ok(()) => {
                                    self.notice = Some((
                                        Severity::Ok,
                                        format!("Saved mouse report to {}", path.display()),
                                    ));
                                }
                                Err(err) => {
                                    self.notice = Some((
                                        Severity::Bad,
                                        format!("Failed to save mouse report: {err}"),
                                    ));
                                }
                            }
                        }
                        if ui.add(primary_btn("Copy Markdown", p)).clicked() {
                            ctx.copy_text(md.clone());
                            self.notice = Some((
                                Severity::Ok,
                                "Copied serial-redacted mouse report to clipboard.".to_string(),
                            ));
                        }
                    });
                });
        }
        if close_report {
            self.report_mouse_modal = None;
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

            let mut report_mouse_clicked = false;
            let render_device_picker = |ui: &mut egui::Ui, report_clicked: &mut bool| {
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
                if ui
                    .add(secondary_btn("Report Mouse", p))
                    .on_hover_text(
                        "Generate a serial-redacted HID + USB topology markdown report for requesting support for an unsupported mouse",
                    )
                    .clicked()
                {
                    *report_clicked = true;
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
                    render_device_picker(ui, &mut report_mouse_clicked);
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
                    render_device_picker(ui, &mut report_mouse_clicked);
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

            if report_mouse_clicked {
                match dump_hid_devices(false, None) {
                    Ok(dump) => {
                        let env = WindowsEnv;
                        let topology = env.list_usb_topology().unwrap_or_default();
                        let win_ver = env.windows_version().ok().map(|v| v.display_string());
                        let detected = snap.map(|s| s.detected_hardware.as_slice()).unwrap_or(&[]);
                        self.report_mouse_modal = Some(format_unsupported_mouse_issue(
                            &dump,
                            &topology,
                            detected,
                            win_ver.as_deref(),
                        ));
                    }
                    Err(err) => {
                        self.notice = Some((
                            Severity::Bad,
                            format!("Could not enumerate HID devices: {err}"),
                        ));
                    }
                }
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
                let mut status_chunks: Vec<String> = Vec::new();
                for dev in &s.active_devices {
                    let summary = dev
                        .settings
                        .as_ref()
                        .map(|st| st.summary_string())
                        .unwrap_or_else(|| "asleep".to_string());
                    let batt = dev
                        .battery
                        .map(|b| match dev.battery_hours_left {
                            Some(hrs) => format!(
                                " · {} ({})",
                                b.compact_label(),
                                crate::profile_runtime::format_battery_hours_left(hrs)
                            ),
                            None => format!(" · {}", b.compact_label()),
                        })
                        .unwrap_or_default();
                    status_chunks.push(format!("{} ({summary}{batt})", dev.info.name));
                }
                if let Some((_, audio_name, _)) = s.audio_outputs.iter().find(|(_, _, def)| *def) {
                    status_chunks.push(format!("Audio: {audio_name}"));
                }
                if s.mic_muted == Some(true) {
                    status_chunks.push("Mic: MUTED".to_string());
                }
                if let Some(locked_pct) = s.mic_lock_active {
                    status_chunks.push(format!("Mic Lock: {locked_pct}%"));
                }
                if let Some(disp) = &s.active_display_override {
                    status_chunks.push(format!("Display: {disp} (temp)"));
                }
                for kb in &s.keyboards {
                    status_chunks.push(format!("{} · {}", kb.name, kb.compact_status_summary()));
                }
                if s.any_flash_only && s.total_flash_writes_session > 0 {
                    status_chunks.push(format!("Flash writes: {}", s.total_flash_writes_session));
                }
                if let Some(pending) = &s.pending_exe {
                    status_chunks.push(format!("Debouncing: {pending}"));
                }
                ui.add(
                    egui::Label::new(
                        RichText::new(status_chunks.join("  ·  "))
                            .size(11.5)
                            .color(p.text_muted),
                    )
                    .wrap(),
                );

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
        let stack_header = inner_w < 285.0;
        card_frame(ui, p, card_w, |ui| {
            ui.set_min_height(inner_col_h);
            let mut open_import = false;
            let mut add_blank = false;
            let mut open_game_picker = false;

            if stack_header {
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(secondary_btn("Import", p)).clicked() {
                            open_import = true;
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(RichText::new("Profiles").strong().color(p.text))
                                    .truncate(),
                            );
                        });
                    });
                });
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add(primary_btn("+ Game", p))
                        .on_hover_text(
                            "1-click create a profile from an installed Steam, Riot, Epic, Battle.net, Ubisoft, or EA game",
                        )
                        .clicked()
                    {
                        open_game_picker = true;
                    }
                    if ui
                        .add(secondary_btn("+ Blank", p))
                        .on_hover_text("Create a blank custom profile")
                        .clicked()
                    {
                        add_blank = true;
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(secondary_btn("Import", p)).clicked() {
                            open_import = true;
                        }
                        if ui
                            .add(secondary_btn("+ Blank", p))
                            .on_hover_text("Create a blank custom profile")
                            .clicked()
                        {
                            add_blank = true;
                        }
                        if ui
                            .add(primary_btn("+ Game", p))
                            .on_hover_text(
                                "1-click create a profile from an installed Steam, Riot, Epic, Battle.net, Ubisoft, or EA game",
                            )
                            .clicked()
                        {
                            open_game_picker = true;
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new("Profiles").strong().color(p.text),
                                )
                                .truncate(),
                            );
                        });
                    });
                });
            }

            if open_import {
                self.import_modal_open = true;
                self.import_text.clear();
            }
            if add_blank {
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
                        dpi_stage: None,
                        lock_dpi_stages: None,
                        glass_mode: None,
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
                    display_device_name: None,
                    display_width: None,
                    display_height: None,
                    display_refresh_hz: None,
                    wooting_profile: None,
                    keyboard_preset: None,
                    force_high_perf_gpu: false,
                    quiet_notifications: false,
                    duck_audio_apps: Vec::new(),
                    duck_audio_volume_pct: 15,
                    author: None,
                    notes: None,
                };
                let new_id = new_prof.id.clone();
                self.store.upsert_profile(new_prof);
                self.select_profile(&new_id);
                runtime.send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
            }
            if open_game_picker {
                self.game_picker_mode = GamePickerMode::CreateProfile;
                self.game_picker_tab = GamePickerTab::Installed;
                self.game_picker_source_filter = None;
                self.game_picker_search.clear();
                self.game_picker_open = true;
                self.discovered_running = discover_running_apps();
                if self.discovered_installed.is_empty() {
                    self.discovered_installed = discover_installed_games();
                }
            }
            ui.add_space(8.0);

            let active_id = snap.map(|s| s.active_profile_id.as_str()).unwrap_or("");
            let mut clicked_id = None;
            let list_header_reserve = if stack_header { 66.0 } else { 34.0 };

            egui::ScrollArea::vertical()
                .id_salt("profile_list_scroll")
                .max_height((inner_col_h - list_header_reserve).max(110.0))
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
        let stack_header = inner_w < 540.0;
        card_frame(ui, p, card_w, |ui| {
            ui.set_min_height(inner_col_h);
            let title_text = if self.editor.is_desktop_default {
                "Desktop Default Profile"
            } else {
                "Edit Game Profile"
            };
            let mut delete_clicked = false;
            let mut duplicate_clicked = false;
            let mut export_clicked = false;
            let mut apply_clicked = false;
            let mut save_clicked = false;

            if stack_header {
                ui.add(
                    egui::Label::new(RichText::new(title_text).size(15.0).strong().color(p.text))
                        .truncate(),
                );
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.add(primary_btn("Save", p)).clicked() {
                        save_clicked = true;
                    }
                    if ui.add(secondary_btn("Apply Now", p)).clicked() {
                        apply_clicked = true;
                    }
                    if ui.add(secondary_btn("Export", p)).clicked() {
                        export_clicked = true;
                    }
                    if ui
                        .add(secondary_btn("Duplicate", p))
                        .on_hover_text(
                            "Clone this profile's mouse, keyboard, audio, and display settings into a new game profile",
                        )
                        .clicked()
                    {
                        duplicate_clicked = true;
                    }
                    if !self.editor.is_desktop_default
                        && ui.add(secondary_btn("Delete", p)).clicked()
                    {
                        delete_clicked = true;
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.editor.is_desktop_default
                            && ui.add(secondary_btn("Delete", p)).clicked()
                        {
                            delete_clicked = true;
                        }
                        if ui
                            .add(secondary_btn("Duplicate", p))
                            .on_hover_text(
                                "Clone this profile's mouse, keyboard, audio, and display settings into a new game profile",
                            )
                            .clicked()
                        {
                            duplicate_clicked = true;
                        }
                        if ui.add(secondary_btn("Export", p)).clicked() {
                            export_clicked = true;
                        }
                        if ui.add(secondary_btn("Apply Now", p)).clicked() {
                            apply_clicked = true;
                        }
                        if ui.add(primary_btn("Save", p)).clicked() {
                            save_clicked = true;
                        }

                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(title_text).size(15.0).strong().color(p.text),
                                )
                                .truncate(),
                            );
                        });
                    });
                });
            }

            if delete_clicked {
                let del_id = self.editor.id.clone();
                if self.store.remove_profile(&del_id) {
                    self.select_profile("desktop");
                    runtime.send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
                    self.notice = Some((Severity::Info, format!("Deleted profile '{del_id}'.")));
                }
            }
            if duplicate_clicked {
                let mut cloned = self.editor.to_profile();
                let base_id = if cloned.id == "desktop" {
                    "game".to_string()
                } else {
                    format!("{}_copy", cloned.id)
                };
                let mut new_id = base_id.clone();
                let mut n = 2usize;
                while self.store.profiles.iter().any(|pr| pr.id == new_id) {
                    new_id = format!("{base_id}_{n}");
                    n += 1;
                }
                cloned.id = new_id.clone();
                cloned.name = format!("{} (Copy)", cloned.name);
                cloned.is_desktop_default = false;
                cloned.executables.clear();
                let cloned_name = cloned.name.clone();
                self.store.upsert_profile(cloned);
                self.select_profile(&new_id);
                self.editor_tab = ProfileEditorTab::General;
                runtime.send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
                self.notice = Some((
                    Severity::Ok,
                    format!(
                        "Duplicated into '{cloned_name}' — pick a game executable in General and click Save."
                    ),
                ));
            }
            if export_clicked {
                let profile = self.editor.to_profile();
                match export_profile_json(&profile) {
                    Ok(json) => self.export_modal_json = Some(json),
                    Err(err) => self.notice = Some((Severity::Bad, err.to_string())),
                }
            }
            if apply_clicked {
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
            if save_clicked {
                let profile = self.editor.to_profile();
                let name = profile.name.clone();
                let id = profile.id.clone();
                self.store.upsert_profile(profile);
                self.select_profile(&id);
                runtime.send(ProfileRuntimeCmd::UpdateStore(self.store.clone()));
                self.notice = Some((Severity::Ok, format!("Saved profile '{name}'.")));
            }

            ui.add_space(8.0);

            // Clean segmented sub-tabs inside Profile Editor (`Mouse` | `Keyboard` | `Display, Audio & Power` | `General`)
            ui.horizontal_wrapped(|ui| {
                for (tab, label) in [
                    (ProfileEditorTab::Mouse, "Mouse"),
                    (ProfileEditorTab::Keyboard, "Keyboard"),
                    (ProfileEditorTab::AudioPower, "Display, Audio & Power"),
                    (ProfileEditorTab::General, "General"),
                ] {
                    let selected = self.editor_tab == tab;
                    let btn = egui::Button::new(
                        RichText::new(label)
                            .size(12.0)
                            .color(if selected { p.text } else { p.text_muted })
                            .strong(),
                    )
                    .fill(if selected {
                        p.surface_alt
                    } else {
                        Color32::TRANSPARENT
                    })
                    .stroke(Stroke::new(
                        1.0_f32,
                        if selected {
                            p.border
                        } else {
                            Color32::TRANSPARENT
                        },
                    ))
                    .corner_radius(CornerRadius::same(6));
                    if ui.add(btn).clicked() {
                        self.editor_tab = tab;
                    }
                }
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
            let dpi_stage_count = active_target_caps.as_ref().map_or(6, |c| c.dpi_stage_count);
            let supports_dpi_lock = active_target_caps
                .as_ref()
                .is_none_or(|c| c.supports_dpi_lock);
            let supports_glass_mode = active_target_caps
                .as_ref()
                .is_none_or(|c| c.supports_glass_mode);

            let mut next_target: Option<(Option<String>, Option<DeviceCapabilities>)> = None;
            let mut remove_override = false;
            let mut read_from_mouse = false;
            let mut open_game_picker = false;
            let mut refresh_audio = false;
            let mut refresh_displays = false;
            let mut test_display_clicked = false;

            let editor_header_reserve = if stack_header { 100.0 } else { 68.0 };
            egui::ScrollArea::vertical()
                .id_salt("profile_editor_scroll")
                .max_height((inner_col_h - editor_header_reserve).max(120.0))
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
                        .spacing([14.0, 8.0])
                        .show(ui, |ui| {
                            if self.editor_tab == ProfileEditorTab::General {
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
                                            egui::TextEdit::singleline(
                                                &mut self.editor.executables_csv,
                                            )
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

                                ui.label(RichText::new("Notes (optional)").color(p.text_muted));
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.editor.notes)
                                        .desired_width(field_w)
                                        .hint_text("Saved in exported profile JSON cards"),
                                );
                                ui.end_row();
                            }

                            if self.editor_tab == ProfileEditorTab::Mouse {
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
                            ui.vertical(|ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.add_enabled(
                                        self.editor.set_dpi,
                                        egui::DragValue::new(&mut self.editor.dpi)
                                            .range(dpi_min..=dpi_max)
                                            .speed(f64::from(dpi_step))
                                            .suffix(" DPI"),
                                    );
                                    let calc_lbl = if self.sens_helper_open {
                                        "Hide cm/360"
                                    } else {
                                        "cm/360"
                                    };
                                    if ui
                                        .add(secondary_btn(calc_lbl, p))
                                        .on_hover_text(
                                            "Show live eDPI, cm/360, and exact in-game sensitivity conversion when changing DPI",
                                        )
                                        .clicked()
                                    {
                                        self.sens_helper_open = !self.sens_helper_open;
                                        if self.sens_helper_open {
                                            self.sens_helper_base_dpi = self.editor.dpi.max(100);
                                            self.sens_helper_game = guess_sensitivity_game(
                                                &self.editor.name,
                                                &self.editor.executables_csv,
                                            );
                                        }
                                    }
                                });
                                if self.sens_helper_open {
                                    ui.add_space(4.0);
                                    egui::Frame::NONE
                                        .fill(p.surface_alt)
                                        .stroke(Stroke::new(1.0_f32, p.border))
                                        .corner_radius(CornerRadius::same(6))
                                        .inner_margin(Margin::symmetric(8, 6))
                                        .show(ui, |ui| {
                                            ui.horizontal_wrapped(|ui| {
                                                egui::ComboBox::from_id_salt("prof_sens_game")
                                                    .selected_text(self.sens_helper_game.label())
                                                    .width(138.0)
                                                    .show_ui(ui, |ui| {
                                                        for g in SensitivityGame::ALL {
                                                            ui.selectable_value(
                                                                &mut self.sens_helper_game,
                                                                g,
                                                                g.label(),
                                                            );
                                                        }
                                                    });
                                                ui.label(
                                                    RichText::new("Old DPI:")
                                                        .size(11.0)
                                                        .color(p.text_muted),
                                                );
                                                ui.add(
                                                    egui::DragValue::new(
                                                        &mut self.sens_helper_base_dpi,
                                                    )
                                                    .range(100..=32000)
                                                    .speed(50),
                                                );
                                                ui.label(
                                                    RichText::new("Sens:")
                                                        .size(11.0)
                                                        .color(p.text_muted),
                                                );
                                                ui.add(
                                                    egui::DragValue::new(
                                                        &mut self.sens_helper_in_game,
                                                    )
                                                    .range(0.001..=100.0)
                                                    .speed(0.01)
                                                    .fixed_decimals(3),
                                                );
                                            });
                                            if let Some(conv) = compute_sensitivity(
                                                self.sens_helper_game,
                                                f64::from(self.sens_helper_base_dpi),
                                                self.sens_helper_in_game,
                                                self.sens_helper_game,
                                                f64::from(self.editor.dpi.max(1)),
                                            ) {
                                                ui.add_space(4.0);
                                                ui.horizontal_wrapped(|ui| {
                                                    ui.label(
                                                        RichText::new(format!(
                                                            "{:.2} cm/360 · {:.2} in/360 · {:.0} eDPI",
                                                            conv.cm_per_360,
                                                            conv.inches_per_360,
                                                            conv.source_edpi
                                                        ))
                                                        .size(11.5)
                                                        .monospace()
                                                        .color(p.text),
                                                    );
                                                });
                                                if self.editor.dpi != self.sens_helper_base_dpi {
                                                    ui.horizontal_wrapped(|ui| {
                                                        ui.label(
                                                            RichText::new(format!(
                                                                "At {} DPI -> set sens to {:.4}",
                                                                self.editor.dpi,
                                                                conv.target_sens_same_game
                                                            ))
                                                            .size(11.5)
                                                            .strong()
                                                            .monospace()
                                                            .color(p.info),
                                                        );
                                                        if ui
                                                            .add(secondary_btn("Copy Sens", p))
                                                            .clicked()
                                                        {
                                                            ui.ctx().copy_text(format!(
                                                                "{:.4}",
                                                                conv.target_sens_same_game
                                                            ));
                                                            self.notice = Some((
                                                                Severity::Ok,
                                                                format!(
                                                                    "Copied converted sensitivity {:.4} for {} DPI.",
                                                                    conv.target_sens_same_game,
                                                                    self.editor.dpi
                                                                ),
                                                            ));
                                                        }
                                                    });
                                                }
                                            }
                                        });
                                }
                            });
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

                            if dpi_stage_count > 0 {
                                ui.checkbox(&mut self.editor.set_dpi_stage, "DPI Stage Slot");
                                ui.add_enabled_ui(self.editor.set_dpi_stage, |ui| {
                                    let clamped = self.editor.dpi_stage.clamp(1, dpi_stage_count);
                                    self.editor.dpi_stage = clamped;
                                    egui::ComboBox::from_id_salt("dpi_stage_combo")
                                        .selected_text(format!("Stage #{clamped}"))
                                        .show_ui(ui, |ui| {
                                            for stage in 1..=dpi_stage_count {
                                                ui.selectable_value(
                                                    &mut self.editor.dpi_stage,
                                                    stage,
                                                    format!("Stage #{stage}"),
                                                );
                                            }
                                        });
                                });
                                ui.end_row();
                            }

                            if supports_dpi_lock {
                                ui.checkbox(&mut self.editor.lock_dpi_stages, "Lock DPI Button");
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(
                                            "Syncs profile DPI to all onboard stages (prevents accidental DPI clicks)",
                                        )
                                        .size(11.5)
                                        .color(p.text_muted),
                                    )
                                    .wrap(),
                                );
                                ui.end_row();
                            }

                            if supports_glass_mode {
                                render_bool_sensor_row(
                                    ui,
                                    p,
                                    "Glass Pad Mode",
                                    true,
                                    &mut self.editor.set_glass_mode,
                                    &mut self.editor.glass_mode,
                                );
                                ui.end_row();
                            }
                            }

                            if self.editor_tab == ProfileEditorTab::Keyboard {
                                let primary_kb = snap.and_then(|s| s.keyboards.first());
                                ui.label(RichText::new("Keyboard Profile").color(p.text_muted));
                                ui.vertical(|ui| {
                                    let wooting_lbl = match self.editor.wooting_profile {
                                        None => "Leave unchanged".to_string(),
                                        Some(idx) => primary_kb
                                            .map(|kb| kb.profile_display_name(idx))
                                            .unwrap_or_else(|| {
                                                WootingKeyboardInfo::profile_label(idx).to_string()
                                            }),
                                    };
                                    egui::ComboBox::from_id_salt("wooting_profile_combo")
                                        .selected_text(wooting_lbl)
                                        .width((field_w - 40.0).clamp(180.0, 320.0))
                                        .show_ui(ui, |ui| {
                                            if ui
                                                .selectable_label(
                                                    self.editor.wooting_profile.is_none(),
                                                    "Leave unchanged",
                                                )
                                                .clicked()
                                            {
                                                self.editor.wooting_profile = None;
                                            }
                                            if let Some(kb) = primary_kb.filter(|k| !k.profiles.is_empty()) {
                                                for slot in &kb.profiles {
                                                    let lbl = format!(
                                                        "P{} · {}",
                                                        slot.index + 1,
                                                        slot.name
                                                    );
                                                    if ui
                                                        .selectable_label(
                                                            self.editor.wooting_profile
                                                                == Some(slot.index),
                                                            lbl,
                                                        )
                                                        .clicked()
                                                    {
                                                        self.editor.wooting_profile =
                                                            Some(slot.index);
                                                    }
                                                }
                                            } else {
                                                for slot in 0..=3u8 {
                                                    if ui
                                                        .selectable_label(
                                                            self.editor.wooting_profile
                                                                == Some(slot),
                                                            WootingKeyboardInfo::profile_label(slot),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.editor.wooting_profile = Some(slot);
                                                    }
                                                }
                                            }
                                        });
                                    ui.add_space(4.0);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(
                                                "Automatically switches onboard keyboard profile slot when this game takes focus (Wooting RAM-only, Wuque/SparkLink, Keychron HE, Arbiter PRO).",
                                            )
                                            .size(11.0)
                                            .color(p.text_muted),
                                        )
                                        .wrap(),
                                    );
                                });
                                ui.end_row();

                                ui.label(RichText::new("Detected Board").color(p.text_muted));
                                ui.vertical(|ui| {
                                    if let Some(kb) = primary_kb {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(format!(
                                                    "{} ({}) · {}",
                                                    kb.name,
                                                    kb.brand.display_name(),
                                                    kb.compact_status_summary()
                                                ))
                                                .size(12.0)
                                                .strong()
                                                .color(p.text),
                                            )
                                            .wrap(),
                                        );
                                        let mut details = Vec::new();
                                        details.push(kb.support_tier.label().to_string());
                                        if let Some(fw) = &kb.firmware_version {
                                            details.push(format!("FW v{fw}"));
                                        }
                                        if let Some(layout) = &kb.layout {
                                            details.push(layout.clone());
                                        }
                                        if let Some(w) = &kb.wooting {
                                            if !w.per_key_actuation.is_empty() {
                                                details.push(format!(
                                                    "{} per-key actuation overrides",
                                                    w.per_key_actuation.len()
                                                ));
                                            }
                                            if !w.advanced_keys.is_empty() {
                                                details.push(format!(
                                                    "{} AKC/SOCD bindings",
                                                    w.advanced_keys.len()
                                                ));
                                            }
                                        }
                                        if !details.is_empty() {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(details.join("  ·  "))
                                                        .size(11.0)
                                                        .color(p.text_muted),
                                                )
                                                .wrap(),
                                            );
                                        }
                                    } else {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(
                                                    "No supported keyboard connected. Supports Wooting, Wuque/SparkLink HE, Keychron HE, SteelSeries Apex Pro, Razer, Arbiter Studio, Logitech HID++ 2.0, Kochi HB68, Geonworks Venom, and Universal QMK/VIA.",
                                                )
                                                .size(11.5)
                                                .color(p.text_muted),
                                            )
                                            .wrap(),
                                        );
                                    }
                                    ui.add_space(4.0);
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(
                                                    "Open the dedicated Keyboard tab in the left sidebar for interactive Wooting keymap, live analog depth, and one-off tuning.",
                                                )
                                                .size(11.0)
                                                .color(p.text_muted),
                                            )
                                            .wrap(),
                                        );
                                    });
                                ui.end_row();

                                // --- PER-GAME KEYBOARD HARDWARE OVERRIDES (BEYOND SLOT SWITCHING) ---
                                let kb_caps = primary_kb.map(|kb| &kb.capabilities);
                                let show_kb_polling =
                                    kb_caps.is_none_or(|c| c.supports_polling_rate_write);
                                let show_kb_actuation =
                                    kb_caps.is_none_or(|c| c.supports_base_actuation_write);
                                let show_kb_rt = kb_caps.is_none_or(|c| {
                                    c.supports_base_actuation_write
                                        || c.supports_rapid_trigger_toggle
                                });
                                let show_kb_socd =
                                    kb_caps.is_none_or(|c| c.supports_socd_toggle);
                                let show_kb_debounce =
                                    kb_caps.is_none_or(|c| c.supports_debounce_tuning);

                                ui.label(
                                    RichText::new("Per-Game Overrides")
                                        .size(11.0)
                                        .strong()
                                        .color(p.text_muted),
                                );
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(
                                            "Optional tuning applied when this game launches (diffed vs live state; skips unsupported controls automatically).",
                                        )
                                        .size(11.0)
                                        .color(p.text_muted),
                                    )
                                    .wrap(),
                                );
                                ui.end_row();

                                if show_kb_polling {
                                    let kb_rates: Vec<u16> = kb_caps
                                        .filter(|c| !c.supported_polling_rates_hz.is_empty())
                                        .map(|c| c.supported_polling_rates_hz.clone())
                                        .unwrap_or_else(|| {
                                            vec![125, 250, 500, 1000, 2000, 4000, 8000]
                                        });
                                    ui.checkbox(&mut self.editor.set_kb_polling, "KB Polling Rate");
                                    ui.add_enabled_ui(self.editor.set_kb_polling, |ui| {
                                        egui::ComboBox::from_id_salt("kb_preset_polling_combo")
                                            .selected_text(format!(
                                                "{} Hz",
                                                self.editor.kb_polling_rate_hz
                                            ))
                                            .show_ui(ui, |ui| {
                                                for hz in &kb_rates {
                                                    ui.selectable_value(
                                                        &mut self.editor.kb_polling_rate_hz,
                                                        *hz,
                                                        format!("{hz} Hz"),
                                                    );
                                                }
                                            });
                                    });
                                    ui.end_row();
                                }

                                if show_kb_actuation {
                                    let (act_min, act_max) = primary_kb
                                        .and_then(|k| k.actuation_range_mm)
                                        .unwrap_or((0.10, 4.00));
                                    ui.checkbox(
                                        &mut self.editor.set_kb_actuation,
                                        "Base Actuation",
                                    );
                                    ui.add_enabled(
                                        self.editor.set_kb_actuation,
                                        egui::DragValue::new(&mut self.editor.kb_actuation_mm)
                                            .range(act_min..=act_max)
                                            .speed(0.05)
                                            .fixed_decimals(2)
                                            .suffix(" mm"),
                                    );
                                    ui.end_row();
                                }

                                if show_kb_rt {
                                    ui.checkbox(
                                        &mut self.editor.set_kb_rt_toggle,
                                        "Rapid Trigger",
                                    );
                                    ui.horizontal_wrapped(|ui| {
                                        ui.add_enabled_ui(self.editor.set_kb_rt_toggle, |ui| {
                                            let lbl = if self.editor.kb_rt_enabled {
                                                "ON"
                                            } else {
                                                "OFF"
                                            };
                                            if ui.add(secondary_btn(lbl, p)).clicked() {
                                                self.editor.kb_rt_enabled =
                                                    !self.editor.kb_rt_enabled;
                                            }
                                        });
                                        if show_kb_actuation {
                                            ui.checkbox(
                                                &mut self.editor.set_kb_rt_mm,
                                                "Sensitivity",
                                            );
                                            ui.add_enabled(
                                                self.editor.set_kb_rt_mm,
                                                egui::DragValue::new(&mut self.editor.kb_rt_mm)
                                                    .range(0.10..=2.50)
                                                    .speed(0.05)
                                                    .fixed_decimals(2)
                                                    .suffix(" mm"),
                                            );
                                        }
                                    });
                                    ui.end_row();
                                }

                                if show_kb_socd {
                                    let socd_lbl = kb_caps
                                        .and_then(|c| c.socd_label.as_deref())
                                        .unwrap_or("SOCD / Snap Tap");
                                    ui.checkbox(&mut self.editor.set_kb_socd, socd_lbl);
                                    ui.add_enabled_ui(self.editor.set_kb_socd, |ui| {
                                        let lbl = if self.editor.kb_socd_enabled {
                                            "ON"
                                        } else {
                                            "OFF"
                                        };
                                        if ui.add(secondary_btn(lbl, p)).clicked() {
                                            self.editor.kb_socd_enabled =
                                                !self.editor.kb_socd_enabled;
                                        }
                                    });
                                    ui.end_row();
                                }

                                if show_kb_debounce {
                                    ui.checkbox(
                                        &mut self.editor.set_kb_debounce,
                                        "KB Debounce",
                                    );
                                    ui.add_enabled_ui(self.editor.set_kb_debounce, |ui| {
                                        ui.horizontal(|ui| {
                                            let mode_lbl = if self.editor.kb_debounce_typing {
                                                "Typing"
                                            } else {
                                                "Gaming"
                                            };
                                            if ui.add(secondary_btn(mode_lbl, p)).clicked() {
                                                self.editor.kb_debounce_typing =
                                                    !self.editor.kb_debounce_typing;
                                            }
                                            ui.add(
                                                egui::DragValue::new(
                                                    &mut self.editor.kb_debounce_ms,
                                                )
                                                .range(5..=40)
                                                .speed(1)
                                                .suffix(" ms"),
                                            );
                                        });
                                    });
                                    ui.end_row();
                                }
                            }

                            if self.editor_tab == ProfileEditorTab::AudioPower {
                            let disp_combo_w = (field_w - 148.0).clamp(130.0, 260.0);
                            let displays = snap.map(|s| s.displays.as_slice()).unwrap_or(&[]);
                            let selected_display = self
                                .editor
                                .display_device_name
                                .as_deref()
                                .and_then(|name| {
                                    displays
                                        .iter()
                                        .find(|d| d.info.device_name.eq_ignore_ascii_case(name))
                                })
                                .or_else(|| displays.iter().find(|d| d.info.is_primary))
                                .or_else(|| displays.first());

                            ui.checkbox(&mut self.editor.set_display_mode, "Display Mode");
                            ui.vertical(|ui| {
                                ui.add_enabled_ui(self.editor.set_display_mode, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        let mon_label = match &self.editor.display_device_name {
                                            None => selected_display
                                                .map(|d| format!("Primary ({})", d.info.friendly_name))
                                                .unwrap_or_else(|| "Primary Monitor".to_string()),
                                            Some(dev) => displays
                                                .iter()
                                                .find(|d| d.info.device_name.eq_ignore_ascii_case(dev))
                                                .map(|d| {
                                                    format!(
                                                        "{} ({})",
                                                        d.info.friendly_name, d.info.device_name
                                                    )
                                                })
                                                .unwrap_or_else(|| dev.clone()),
                                        };
                                        egui::ComboBox::from_id_salt("display_monitor_combo")
                                            .selected_text(mon_label)
                                            .width(disp_combo_w)
                                            .show_ui(ui, |ui| {
                                                if ui
                                                    .selectable_label(
                                                        self.editor.display_device_name.is_none(),
                                                        "Primary Monitor (Auto)",
                                                    )
                                                    .clicked()
                                                {
                                                    self.editor.display_device_name = None;
                                                }
                                                for d in displays {
                                                    let lbl = if d.info.is_primary {
                                                        format!(
                                                            "{} · Primary ({}×{} @ {} Hz)",
                                                            d.info.friendly_name,
                                                            d.info.width,
                                                            d.info.height,
                                                            d.info.current_hz
                                                        )
                                                    } else {
                                                        format!(
                                                            "{} ({}×{} @ {} Hz)",
                                                            d.info.friendly_name,
                                                            d.info.width,
                                                            d.info.height,
                                                            d.info.current_hz
                                                        )
                                                    };
                                                    let sel = self
                                                        .editor
                                                        .display_device_name
                                                        .as_deref()
                                                        .is_some_and(|n| {
                                                            n.eq_ignore_ascii_case(
                                                                &d.info.device_name,
                                                            )
                                                        });
                                                    if ui.selectable_label(sel, lbl).clicked() {
                                                        self.editor.display_device_name =
                                                            Some(d.info.device_name.clone());
                                                    }
                                                }
                                            });
                                        if ui
                                            .add(secondary_btn("Refresh", p))
                                            .on_hover_text("Re-scan connected displays & supported modes")
                                            .clicked()
                                        {
                                            refresh_displays = true;
                                        }
                                    });
                                    ui.add_space(4.0);
                                    ui.horizontal_wrapped(|ui| {
                                        let mode_label = format!(
                                            "{}×{} @ {} Hz",
                                            self.editor.display_width,
                                            self.editor.display_height,
                                            self.editor.display_refresh_hz
                                        );
                                        egui::ComboBox::from_id_salt("display_mode_combo")
                                            .selected_text(mode_label)
                                            .width(disp_combo_w)
                                            .show_ui(ui, |ui| {
                                                if let Some(disp) = selected_display {
                                                    for m in &disp.modes {
                                                        let tag = m.aspect_tag();
                                                        let lbl = if tag.is_empty() {
                                                            m.label()
                                                        } else {
                                                            format!("{}  ({tag})", m.label())
                                                        };
                                                        let sel = self.editor.display_width
                                                            == m.width
                                                            && self.editor.display_height
                                                                == m.height
                                                            && self.editor.display_refresh_hz
                                                                == m.refresh_hz;
                                                        if ui.selectable_label(sel, lbl).clicked() {
                                                            self.editor.display_width = m.width;
                                                            self.editor.display_height = m.height;
                                                            self.editor.display_refresh_hz =
                                                                m.refresh_hz;
                                                        }
                                                    }
                                                } else {
                                                    ui.label("No display modes detected");
                                                }
                                            });
                                        if ui
                                            .add(secondary_btn("Test (10s)", p))
                                            .on_hover_text(
                                                "Temporarily test this resolution & refresh rate with an automatic 10-second revert safety guard",
                                            )
                                            .clicked()
                                        {
                                            test_display_clicked = true;
                                        }
                                    });
                                });
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(
                                            "Temporary session-only switch (never saved to registry) — auto-reverts to native desktop on tab-out or exit.",
                                        )
                                        .size(11.0)
                                        .color(p.text_muted),
                                    )
                                    .wrap(),
                                );
                            });
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

                            if !self.editor.is_desktop_default {
                                ui.label(RichText::new("Windows GPU").color(p.text_muted));
                                ui.checkbox(
                                    &mut self.editor.force_high_perf_gpu,
                                    "Pin High-Performance GPU (HKCU Graphics Settings)",
                                )
                                .on_hover_text(
                                    "Writes GpuPreference=2; for this game's exact .exe under HKCU\\Software\\Microsoft\\DirectX\\UserGpuPreferences (zero admin, anti-cheat safe).",
                                );
                                ui.end_row();
                            }

                            ui.label(RichText::new("Notifications").color(p.text_muted));
                            ui.checkbox(
                                &mut self.editor.quiet_notifications,
                                "Quiet Mode (mute Windows toast popups while active)",
                            )
                            .on_hover_text(
                                "Temporarily disables Windows toast popups while this profile is active and restores your original setting automatically on desktop or exit.",
                            );
                            ui.end_row();

                            ui.label(RichText::new("App Mixer Duck").color(p.text_muted));
                            ui.vertical(|ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(
                                            &mut self.editor.duck_audio_apps_csv,
                                        )
                                        .desired_width((field_w - 110.0).clamp(110.0, 260.0))
                                        .hint_text("spotify.exe, chrome.exe"),
                                    );
                                    ui.label(RichText::new("to").size(11.5).color(p.text_muted));
                                    let duck_suffix = if self.editor.duck_audio_volume_pct == 0 {
                                        "% (Mute)"
                                    } else {
                                        "%"
                                    };
                                    ui.add(
                                        egui::DragValue::new(&mut self.editor.duck_audio_volume_pct)
                                            .range(0..=100)
                                            .speed(1)
                                            .suffix(duck_suffix),
                                    );
                                });
                                ui.add_space(3.0);
                                ui.horizontal_wrapped(|ui| {
                                    for (label, exe) in [
                                        ("+ Spotify", "spotify.exe"),
                                        ("+ Chrome", "chrome.exe"),
                                        ("+ Edge", "msedge.exe"),
                                        ("+ Firefox", "firefox.exe"),
                                        ("+ Discord", "discord.exe"),
                                    ] {
                                        if ui.add(secondary_btn(label, p)).clicked() {
                                            let already = self
                                                .editor
                                                .duck_audio_apps_csv
                                                .split([',', ';'])
                                                .any(|s| s.trim().eq_ignore_ascii_case(exe));
                                            if !already {
                                                if !self.editor.duck_audio_apps_csv.trim().is_empty()
                                                {
                                                    self.editor.duck_audio_apps_csv.push_str(", ");
                                                }
                                                self.editor.duck_audio_apps_csv.push_str(exe);
                                            }
                                        }
                                    }
                                });
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(
                                            "Lowers or mutes (0%) matching apps in Windows Volume Mixer while active; restores exact original sliders on desktop/exit.",
                                        )
                                        .size(11.0)
                                        .color(p.text_muted),
                                    )
                                    .wrap(),
                                );
                            });
                            ui.end_row();
                            }
                        });
                });

            if refresh_audio {
                runtime.send(ProfileRuntimeCmd::RefreshAudioDevices);
            }
            if refresh_displays {
                runtime.send(ProfileRuntimeCmd::RefreshDisplays);
            }
            if test_display_clicked {
                runtime.send(ProfileRuntimeCmd::TestDisplayMode {
                    device_name: self.editor.display_device_name.clone(),
                    width: self.editor.display_width,
                    height: self.editor.display_height,
                    refresh_hz: self.editor.display_refresh_hz,
                });
            }
            if read_from_mouse {
                runtime.send(ProfileRuntimeCmd::ReadFromMouseForEditor(
                    self.editor.editing_device_id.clone(),
                ));
            }
            if open_game_picker {
                self.game_picker_mode = GamePickerMode::AddExeToCurrent;
                self.game_picker_source_filter = None;
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
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let toggle_lbl = if self.activity_log_open {
                        "Hide Log".to_string()
                    } else {
                        format!("Log ({})", log_entries.len())
                    };
                    if ui.add(secondary_btn(&toggle_lbl, p)).clicked() {
                        self.activity_log_open = !self.activity_log_open;
                    }
                    if self.activity_log_open
                        && ui
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

                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("Activity")
                                .size(12.0)
                                .strong()
                                .color(p.text_muted),
                        );
                        if !self.activity_log_open {
                            if let Some(latest) = log_entries.first() {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "{}  {}",
                                            latest.timestamp, latest.message
                                        ))
                                        .size(11.5)
                                        .color(p.text_muted),
                                    )
                                    .truncate(),
                                );
                            } else {
                                ui.label(
                                    RichText::new("Idle — no profile switches yet.")
                                        .size(11.5)
                                        .color(p.text_muted),
                                );
                            }
                        }
                    });
                });
            });

            if !self.activity_log_open {
                return;
            }

            ui.add_space(6.0);
            let log_scroll_h =
                fixed_scroll_h.unwrap_or_else(|| (ui.available_height() - 6.0).clamp(90.0, 145.0));
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

fn guess_sensitivity_game(name: &str, executables_csv: &str) -> SensitivityGame {
    let hay = format!("{name} {executables_csv}").to_ascii_lowercase();
    if hay.contains("valorant") {
        SensitivityGame::Valorant
    } else if hay.contains("apex") || hay.contains("r5apex") {
        SensitivityGame::ApexLegends
    } else if hay.contains("overwatch") || hay.contains("cod.exe") || hay.contains("call of duty") {
        SensitivityGame::Overwatch2
    } else if hay.contains("rainbow") || hay.contains("siege") {
        SensitivityGame::RainbowSix
    } else if hay.contains("fortnite") {
        SensitivityGame::Fortnite
    } else {
        SensitivityGame::Cs2
    }
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

/// Pre-measured semantic status banner (`OK`, `INFO`, `WARN`, `BAD`).
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
