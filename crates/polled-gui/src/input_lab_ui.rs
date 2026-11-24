// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plain monochrome Black & White UI for the live Input Lab diagnostics tab.
//! Only the four semantic statuses (`OK`, `INFO`, `WARN`, `BAD`) carry chroma.

use std::sync::mpsc::{self, Receiver};

use eframe::egui::text::LayoutJob;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Margin, Pos2, Rect, RichText, Stroke, TextFormat,
    Vec2,
};
use polled_core::{
    format_diagnostic_report, run_timer_jitter_benchmark, DiagnosticsSnapshot, PollingStats,
    PollingTarget, RawInputCaptureSession, Severity, TimerJitterBenchmark, TrackingStats,
    BOUNCE_THRESHOLD_MS, HISTOGRAM_BASE_BINS, STANDARD_RATES_HZ,
};

use crate::theme::{Palette, ThemeMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ChartViewMode {
    #[default]
    Split,
    Intervals,
    Histogram,
    Velocity,
    XyTrace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum RulerUnit {
    #[default]
    Cm,
    Inches,
}

impl RulerUnit {
    fn label(self) -> &'static str {
        match self {
            RulerUnit::Cm => "cm",
            RulerUnit::Inches => "in",
        }
    }

    fn to_inches(self, value: f64) -> f64 {
        match self {
            RulerUnit::Cm => value / 2.54,
            RulerUnit::Inches => value,
        }
    }
}

pub struct InputLabUiState {
    session: Option<RawInputCaptureSession>,
    target: PollingTarget,
    chart_mode: ChartViewMode,
    saturated_only: bool,
    configured_dpi: u32,
    dpi_manually_edited: bool,
    ruler_distance: f64,
    ruler_unit: RulerUnit,
    dpi_measuring: bool,
    last_snapshot: DiagnosticsSnapshot,
    timer_benchmark: Option<TimerJitterBenchmark>,
    timer_running: bool,
    timer_rx: Option<Receiver<Result<TimerJitterBenchmark, String>>>,
    pub notice: Option<(Severity, String)>,
}

impl Default for InputLabUiState {
    fn default() -> Self {
        Self::new()
    }
}

impl InputLabUiState {
    pub fn new() -> Self {
        Self {
            session: None,
            target: PollingTarget::Auto,
            chart_mode: ChartViewMode::Split,
            saturated_only: false,
            configured_dpi: 1600,
            dpi_manually_edited: false,
            ruler_distance: 10.0,
            ruler_unit: RulerUnit::Cm,
            dpi_measuring: false,
            last_snapshot: DiagnosticsSnapshot::default(),
            timer_benchmark: None,
            timer_running: false,
            timer_rx: None,
            notice: None,
        }
    }

    pub fn is_capturing(&self) -> bool {
        self.session.is_some()
    }

    pub fn start_capture(&mut self, ctx: &egui::Context) {
        if self.session.is_some() {
            return;
        }
        let ctx_bg = ctx.clone();
        let session = RawInputCaptureSession::start(self.target, move || {
            ctx_bg.request_repaint();
        });
        if self.dpi_measuring {
            session.set_dpi_measuring(true);
        }
        self.last_snapshot = session.snapshot();
        self.session = Some(session);
        self.notice = None;
    }

    pub fn stop_capture(&mut self) {
        self.dpi_measuring = false;
        if let Some(mut session) = self.session.take() {
            session.set_dpi_measuring(false);
            self.last_snapshot = session.snapshot();
            session.stop();
            self.last_snapshot.capturing = false;
            self.last_snapshot.dpi_measurement.active = false;
        }
    }

    fn reset_capture(&mut self) {
        self.dpi_measuring = false;
        if let Some(session) = &self.session {
            session.set_dpi_measuring(false);
            session.request_reset();
            self.last_snapshot = DiagnosticsSnapshot {
                capturing: true,
                target: self.target,
                ..Default::default()
            };
        } else {
            self.last_snapshot = DiagnosticsSnapshot {
                capturing: false,
                target: self.target,
                ..Default::default()
            };
        }
        self.notice = None;
    }

    fn set_target(&mut self, target: PollingTarget) {
        self.target = target;
        self.last_snapshot.target = target;
        if let Some(session) = &self.session {
            session.set_target(target);
        }
    }

    fn toggle_dpi_measurement(&mut self, ctx: &egui::Context) {
        if self.dpi_measuring {
            self.dpi_measuring = false;
            if let Some(session) = &self.session {
                session.set_dpi_measuring(false);
                self.last_snapshot = session.snapshot();
            }
            self.last_snapshot.dpi_measurement.active = false;
        } else {
            self.dpi_measuring = true;
            if self.session.is_none() {
                self.start_capture(ctx);
            } else if let Some(session) = &self.session {
                session.set_dpi_measuring(true);
            }
        }
    }

    fn reset_dpi_measurement(&mut self) {
        self.dpi_measuring = false;
        if let Some(session) = &self.session {
            session.set_dpi_measuring(false);
            session.request_dpi_reset();
        }
        self.last_snapshot.dpi_measurement = Default::default();
    }

    fn start_timer_benchmark(&mut self, ctx: &egui::Context) {
        if self.timer_running {
            return;
        }
        self.timer_running = true;
        let (tx, rx) = mpsc::channel();
        self.timer_rx = Some(rx);
        let ctx_bg = ctx.clone();
        std::thread::spawn(move || {
            let res = run_timer_jitter_benchmark(1_000).map_err(|e| e.to_string());
            let _ = tx.send(res);
            ctx_bg.request_repaint();
        });
    }

    fn poll_background(&mut self) {
        if let Some(session) = &self.session {
            self.last_snapshot = session.snapshot();
            if let Some(err) = self.last_snapshot.error.clone() {
                self.notice = Some((Severity::Bad, err));
                self.stop_capture();
            }
        }
        if let Some(rx) = &self.timer_rx {
            if let Ok(result) = rx.try_recv() {
                self.timer_running = false;
                self.timer_rx = None;
                match result {
                    Ok(bench) => {
                        self.timer_benchmark = Some(bench);
                    }
                    Err(err) => {
                        self.notice = Some((Severity::Bad, err));
                    }
                }
            }
        }
    }

    pub fn render(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        p: Palette,
        mode: ThemeMode,
        detected_dpi: Option<u16>,
    ) {
        self.poll_background();

        // Auto-sync configured DPI from the active detected mouse until the user edits it manually.
        if !self.dpi_manually_edited {
            if let Some(dpi) = detected_dpi {
                if dpi >= 100 {
                    self.configured_dpi = u32::from(dpi);
                }
            }
        }

        // Allow hands-free Space key toggle during an active DPI ruler slide so you don't nudge
        // the mouse while clicking Stop at the end of the ruler.
        if self.dpi_measuring
            && !ctx.wants_keyboard_input()
            && ctx.input(|i| i.key_pressed(egui::Key::Space) || i.key_pressed(egui::Key::Enter))
        {
            self.toggle_dpi_measurement(ctx);
        }

        // Live viewport-driven width so resizing smaller/larger immediately reflows every chart and card.
        let content_w = (ctx.screen_rect().width() - 36.0).max(320.0);
        ui.set_max_width(content_w);

        egui::ScrollArea::vertical()
            .id_salt("input_lab_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let scroll_w = (content_w - 14.0).max(280.0);
                ui.set_max_width(scroll_w);

                // --- 1. CONTROL & LIVE SUMMARY CARD ---
                self.render_controls_card(ctx, ui, p, scroll_w, detected_dpi);

                if let Some((sev, msg)) = self.notice.clone() {
                    ui.add_space(8.0);
                    semantic_banner(ui, p, mode, scroll_w, sev, &msg);
                }

                ui.add_space(10.0);

                // --- 2. POLLING CONSISTENCY, HISTOGRAM, VELOCITY & XY TRACE CARD ---
                self.render_polling_chart_card(ui, p, mode, scroll_w);

                ui.add_space(10.0);

                // --- 3. MEASURED DPI / CPI CALIBRATOR CARD ---
                self.render_dpi_calibrator_card(ctx, ui, p, mode, scroll_w);

                ui.add_space(10.0);

                // --- 4. BOTTOM TWO-COLUMN: CLICK DEBOUNCE + 1S TIMER BENCHMARK ---
                if scroll_w < 720.0 {
                    self.render_click_bounce_card(ui, p, mode, scroll_w);
                    ui.add_space(10.0);
                    self.render_timer_benchmark_card(ctx, ui, p, mode, scroll_w);
                } else {
                    let gap = 10.0;
                    let half_w = ((scroll_w - gap) * 0.5).max(260.0);
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        ui.allocate_ui_with_layout(
                            Vec2::new(half_w, 0.0),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                self.render_click_bounce_card(ui, p, mode, half_w);
                            },
                        );
                        ui.allocate_ui_with_layout(
                            Vec2::new(half_w, 0.0),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                self.render_timer_benchmark_card(ctx, ui, p, mode, half_w);
                            },
                        );
                    });
                }

                ui.add_space(4.0);
            });
    }

    fn render_controls_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        p: Palette,
        card_w: f32,
        detected_dpi: Option<u16>,
    ) {
        let inner_w = (card_w - 24.0).max(120.0);
        let capturing = self.is_capturing();
        let mut toggle_capture = false;
        let mut do_reset = false;
        let mut do_copy = false;
        let mut next_target = self.target;
        let mut dpi_val = self.configured_dpi;
        let mut dpi_changed = false;

        card_frame(ui, p, card_w, |ui| {
            let split = inner_w < 740.0;
            let has_data = self.last_snapshot.polling.total_motion_packets > 0
                || self.last_snapshot.clicks.total_presses > 0
                || self.last_snapshot.dpi_measurement.packet_count > 0
                || self.timer_benchmark.is_some();

            let draw_primary_actions =
                |ui: &mut egui::Ui, toggle_capture: &mut bool, do_reset: &mut bool| {
                    let cap_label = if capturing {
                        "Stop Capture"
                    } else {
                        "Start Capture"
                    };
                    if ui.add(primary_btn(cap_label, p)).clicked() {
                        *toggle_capture = true;
                    }
                    if ui.add(secondary_btn("Reset", p)).clicked() {
                        *do_reset = true;
                    }
                };

            let draw_secondary_actions =
                |ui: &mut egui::Ui,
                 next_target: &mut PollingTarget,
                 dpi_val: &mut u32,
                 dpi_changed: &mut bool,
                 do_copy: &mut bool| {
                    ui.label(RichText::new("Target:").color(p.text_muted).size(12.0));
                    egui::ComboBox::from_id_salt("input_lab_target_hz")
                        .selected_text(next_target.label())
                        .width(108.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(next_target, PollingTarget::Auto, "Auto-detect");
                            for hz in STANDARD_RATES_HZ {
                                ui.selectable_value(
                                    next_target,
                                    PollingTarget::FixedHz(hz),
                                    format!("{hz} Hz"),
                                );
                            }
                        });

                    ui.label(RichText::new("DPI:").color(p.text_muted).size(12.0));
                    let dpi_resp = ui
                    .add(
                        egui::DragValue::new(dpi_val)
                            .range(100..=32000)
                            .speed(50)
                            .suffix(" DPI"),
                    )
                    .on_hover_text(match detected_dpi {
                        Some(d) => format!(
                            "Used for IPS, G-force, and DPI accuracy (auto-detected {d} DPI from mouse)"
                        ),
                        None => "Configured mouse DPI used for IPS, G-force, and DPI accuracy"
                            .to_string(),
                    });
                    if dpi_resp.changed() {
                        *dpi_changed = true;
                    }

                    if ui
                        .add_enabled(has_data, secondary_btn("Copy Report", p))
                        .clicked()
                    {
                        *do_copy = true;
                    }
                };

            if split {
                ui.horizontal(|ui| {
                    draw_primary_actions(ui, &mut toggle_capture, &mut do_reset);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (status_text, status_col) = if capturing {
                            ("LIVE RAW INPUT (QPC)", p.ok)
                        } else {
                            ("IDLE · 0% CPU", p.text_muted)
                        };
                        ui.label(
                            RichText::new(status_text)
                                .size(11.0)
                                .strong()
                                .monospace()
                                .color(status_col),
                        );
                    });
                });
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    draw_secondary_actions(
                        ui,
                        &mut next_target,
                        &mut dpi_val,
                        &mut dpi_changed,
                        &mut do_copy,
                    );
                });
            } else {
                ui.horizontal(|ui| {
                    draw_primary_actions(ui, &mut toggle_capture, &mut do_reset);
                    ui.add_space(6.0);
                    draw_secondary_actions(
                        ui,
                        &mut next_target,
                        &mut dpi_val,
                        &mut dpi_changed,
                        &mut do_copy,
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (status_text, status_col) = if capturing {
                            ("LIVE RAW INPUT (QPC)", p.ok)
                        } else {
                            ("IDLE · 0% CPU", p.text_muted)
                        };
                        ui.label(
                            RichText::new(status_text)
                                .size(11.0)
                                .strong()
                                .monospace()
                                .color(status_col),
                        );
                    });
                });
            }

            ui.add_space(6.0);

            let ps = &self.last_snapshot.polling;
            let tr = &self.last_snapshot.tracking;
            let dpi_f = f64::from(dpi_val.max(1));

            let eff_str = if ps.effective_hz >= 1.0 {
                format!("{:.0} Hz", ps.effective_hz)
            } else {
                "—".to_string()
            };
            let peak_str = if ps.peak_hz >= 1.0 {
                format!("{:.0} Hz", ps.peak_hz)
            } else {
                "—".to_string()
            };
            let target_str = if ps.nominal_hz > 0 {
                format!("{} Hz", ps.nominal_hz)
            } else {
                self.target.label()
            };
            let sat_str = if ps.active_intervals > 0 {
                format!("{:.0}%", ps.saturated_pct)
            } else {
                "—".to_string()
            };
            let consistency_str = if ps.saturated_intervals > 0 {
                format!("{:.1}%", ps.saturated_within_15_pct)
            } else if ps.active_intervals > 0 {
                format!("{:.1}%", ps.within_15_pct)
            } else {
                "—".to_string()
            };
            let peak_ips = tr.peak_ips(dpi_f);
            let peak_g = tr.peak_accel_g(dpi_f);
            let speed_str = if peak_ips >= 0.1 {
                format!("{:.1} IPS · {:.1} G", peak_ips, peak_g)
            } else {
                "—".to_string()
            };

            let pills = vec![
                ("EFFECTIVE".to_string(), eff_str),
                ("PEAK".to_string(), peak_str),
                ("TARGET".to_string(), target_str),
                ("SATURATED".to_string(), sat_str),
                ("SAT ±15%".to_string(), consistency_str),
                ("DROPOUTS".to_string(), ps.true_dropout_count.to_string()),
                ("SLOW SLOTS".to_string(), ps.slow_slot_count.to_string()),
                ("BUNCHED".to_string(), ps.bunch_count.to_string()),
                ("PEAK TRACKING".to_string(), speed_str),
                ("PACKETS".to_string(), ps.total_motion_packets.to_string()),
            ];
            render_status_pill_rows(ui, p, inner_w, &pills);
        });

        if dpi_changed {
            self.configured_dpi = dpi_val;
            self.dpi_manually_edited = true;
        }
        if next_target != self.target {
            self.set_target(next_target);
        }
        if toggle_capture {
            if capturing {
                self.stop_capture();
            } else {
                self.start_capture(ctx);
            }
        }
        if do_reset {
            self.reset_capture();
        }
        if do_copy {
            let ruler_in = (self.last_snapshot.dpi_measurement.packet_count > 0)
                .then(|| self.ruler_unit.to_inches(self.ruler_distance));
            let report = format_diagnostic_report(
                &self.last_snapshot,
                self.configured_dpi,
                ruler_in,
                self.timer_benchmark.as_ref(),
            );
            ctx.copy_text(report);
            self.notice = Some((
                Severity::Ok,
                "Copied Input Lab diagnostic report to clipboard.".to_string(),
            ));
        }
    }

    fn render_polling_chart_card(
        &mut self,
        ui: &mut egui::Ui,
        p: Palette,
        mode: ThemeMode,
        card_w: f32,
    ) {
        let inner_w = (card_w - 24.0).max(120.0);
        let ps = &self.last_snapshot.polling;
        let tr = &self.last_snapshot.tracking;
        let dpi_f = f64::from(self.configured_dpi.max(1));

        card_frame(ui, p, card_w, |ui| {
            let narrow_hdr = inner_w < 860.0;
            let show_sat_toggle = matches!(
                self.chart_mode,
                ChartViewMode::Split | ChartViewMode::Intervals | ChartViewMode::Histogram
            );

            let modes = [
                (ChartViewMode::Split, "Split"),
                (ChartViewMode::Intervals, "Intervals"),
                (ChartViewMode::Histogram, "Histogram"),
                (ChartViewMode::Velocity, "Velocity"),
                (ChartViewMode::XyTrace, "XY Trace"),
            ];

            if narrow_hdr {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Polling Consistency & Sensor Tracking")
                            .size(14.0)
                            .strong()
                            .color(p.text),
                    );
                    if let Some(sev) = ps.health_severity {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(health_badge_label(sev))
                                    .size(11.0)
                                    .strong()
                                    .color(p.severity_fg(sev)),
                            );
                        });
                    }
                });
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    for (m, lbl) in modes {
                        let btn = if self.chart_mode == m {
                            primary_btn(lbl, p)
                        } else {
                            secondary_btn(lbl, p)
                        };
                        if ui.add(btn).clicked() {
                            self.chart_mode = m;
                        }
                    }
                    if show_sat_toggle {
                        let sat_btn = if self.saturated_only {
                            primary_btn("Saturated only", p)
                        } else {
                            secondary_btn("Saturated only", p)
                        };
                        if ui
                            .add(sat_btn)
                            .on_hover_text(
                                "Hide slow-hand empty slots and show only motion-saturated intervals",
                            )
                            .clicked()
                        {
                            self.saturated_only = !self.saturated_only;
                        }
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Polling Consistency & Sensor Tracking")
                            .size(14.0)
                            .strong()
                            .color(p.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(sev) = ps.health_severity {
                            ui.label(
                                RichText::new(health_badge_label(sev))
                                    .size(11.0)
                                    .strong()
                                    .color(p.severity_fg(sev)),
                            );
                            ui.add_space(4.0);
                        }
                        if show_sat_toggle {
                            let sat_btn = if self.saturated_only {
                                primary_btn("Saturated only", p)
                            } else {
                                secondary_btn("Saturated only", p)
                            };
                            if ui
                                .add(sat_btn)
                                .on_hover_text(
                                    "Hide slow-hand empty slots and show only motion-saturated intervals",
                                )
                                .clicked()
                            {
                                self.saturated_only = !self.saturated_only;
                            }
                        }
                        for (m, lbl) in modes.into_iter().rev() {
                            let btn = if self.chart_mode == m {
                                primary_btn(lbl, p)
                            } else {
                                secondary_btn(lbl, p)
                            };
                            if ui.add(btn).clicked() {
                                self.chart_mode = m;
                            }
                        }
                    });
                });
            }

            if ps.active_intervals > 0 {
                ui.add_space(4.0);
                let stats_text = match self.chart_mode {
                    ChartViewMode::Velocity | ChartViewMode::XyTrace => format!(
                        "Live {:.1} IPS ({:.2} m/s) · Peak {:.1} IPS ({:.2} m/s) · Peak Accel {:.1} G · Configured {} DPI",
                        tr.current_ips(dpi_f),
                        tr.current_ips(dpi_f) * 0.0254,
                        tr.peak_ips(dpi_f),
                        tr.peak_mps(dpi_f),
                        tr.peak_accel_g(dpi_f),
                        self.configured_dpi,
                    ),
                    _ => format!(
                        "Min {:.3} ms · Median {:.3} ms · p95 {:.3} ms · p99 {:.3} ms · Max {:.3} ms · σ {:.3} ms",
                        ps.min_us / 1000.0,
                        ps.median_us / 1000.0,
                        ps.p95_us / 1000.0,
                        ps.p99_us / 1000.0,
                        ps.max_us / 1000.0,
                        ps.std_dev_us / 1000.0,
                    ),
                };
                ui.add(
                    egui::Label::new(
                        RichText::new(stats_text)
                            .size(11.5)
                            .monospace()
                            .color(p.text_muted),
                    )
                    .wrap(),
                );
            }

            ui.add_space(6.0);

            match self.chart_mode {
                ChartViewMode::Intervals => {
                    paint_interval_chart(ui, p, inner_w, 190.0, ps, self.saturated_only);
                }
                ChartViewMode::Histogram => {
                    paint_histogram_chart(ui, p, inner_w, 190.0, ps, self.saturated_only);
                }
                ChartViewMode::Velocity => {
                    paint_velocity_chart(ui, p, inner_w, 200.0, ps, tr, dpi_f);
                }
                ChartViewMode::XyTrace => {
                    paint_xy_trace_chart(ui, p, inner_w, 220.0, tr, dpi_f);
                }
                ChartViewMode::Split => {
                    if inner_w >= 760.0 {
                        let gap = 10.0;
                        let left_w = ((inner_w - gap) * 0.59).max(300.0);
                        let right_w = (inner_w - gap - left_w).max(220.0);
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            paint_interval_chart(ui, p, left_w, 185.0, ps, self.saturated_only);
                            paint_histogram_chart(ui, p, right_w, 185.0, ps, self.saturated_only);
                        });
                    } else {
                        paint_interval_chart(ui, p, inner_w, 165.0, ps, self.saturated_only);
                        ui.add_space(8.0);
                        paint_histogram_chart(ui, p, inner_w, 155.0, ps, self.saturated_only);
                    }
                }
            }

            ui.add_space(8.0);
            let sev = ps.health_severity.unwrap_or(Severity::Info);
            let msg = if ps.health_summary.is_empty() {
                "Click Start Capture and move your mouse in brisk, steady circles anywhere on screen to test 1K–8K polling consistency, IPS/G tracking, and wireless dropouts."
            } else {
                &ps.health_summary
            };
            semantic_banner(ui, p, mode, inner_w, sev, msg);
        });
    }

    fn render_dpi_calibrator_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        p: Palette,
        mode: ThemeMode,
        card_w: f32,
    ) {
        let inner_w = (card_w - 24.0).max(160.0);
        let snap = self.last_snapshot.dpi_measurement.clone();
        let dist_in = self.ruler_unit.to_inches(self.ruler_distance);
        let eval = snap.evaluate(f64::from(self.configured_dpi.max(1)), dist_in);

        let mut toggle_run = false;
        let mut clear_run = false;

        card_frame(ui, p, card_w, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Measured DPI / CPI Calibrator")
                        .size(14.0)
                        .strong()
                        .color(p.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.dpi_measuring {
                        ui.label(
                            RichText::new("MEASURING · PRESS SPACE TO FINISH")
                                .size(11.0)
                                .strong()
                                .monospace()
                                .color(p.ok),
                        );
                    } else if let Some(res) = eval {
                        ui.label(
                            RichText::new(format!(
                                "{:.0} DPI ({:+.1}%)",
                                res.measured_dpi, res.deviation_pct
                            ))
                            .size(11.5)
                            .strong()
                            .monospace()
                            .color(p.severity_fg(res.severity)),
                        );
                    }
                });
            });

            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Place a ruler next to your mouse, click Start DPI Slide, glide straight for the exact distance below, then press Space (or click Finish) without nudging the mouse.",
                    )
                    .size(11.5)
                    .color(p.text_muted),
                )
                .wrap(),
            );
            ui.add_space(8.0);

            ui.horizontal_wrapped(|ui| {
                let run_label = if self.dpi_measuring {
                    "Finish Slide (Space)"
                } else {
                    "Start DPI Slide"
                };
                if ui.add(primary_btn(run_label, p)).clicked() {
                    toggle_run = true;
                }
                if ui
                    .add_enabled(
                        snap.packet_count > 0 || self.dpi_measuring,
                        secondary_btn("Clear", p),
                    )
                    .clicked()
                {
                    clear_run = true;
                }

                ui.add_space(6.0);
                ui.label(
                    RichText::new("Ruler distance:")
                        .color(p.text_muted)
                        .size(12.0),
                );
                ui.add(
                    egui::DragValue::new(&mut self.ruler_distance)
                        .range(1.0..=100.0)
                        .speed(0.5)
                        .fixed_decimals(1),
                );

                for unit in [RulerUnit::Cm, RulerUnit::Inches] {
                    let btn = if self.ruler_unit == unit {
                        primary_btn(unit.label(), p)
                    } else {
                        secondary_btn(unit.label(), p)
                    };
                    if ui.add(btn).clicked() && self.ruler_unit != unit {
                        // Convert existing ruler distance cleanly when switching units.
                        self.ruler_distance = match unit {
                            RulerUnit::Inches => (self.ruler_distance / 2.54 * 10.0).round() / 10.0,
                            RulerUnit::Cm => (self.ruler_distance * 2.54 * 10.0).round() / 10.0,
                        }
                        .clamp(1.0, 100.0);
                        self.ruler_unit = unit;
                    }
                }
            });

            if snap.packet_count > 0 {
                ui.add_space(8.0);
                let mut pills = vec![
                    (
                        "NET COUNTS".to_string(),
                        format!(
                            "{:.0} (dx {}, dy {})",
                            snap.net_counts, snap.net_x, snap.net_y
                        ),
                    ),
                    (
                        "STRAIGHTNESS".to_string(),
                        format!("{:.1}%", snap.straightness_pct),
                    ),
                ];
                if let Some(res) = eval {
                    pills.push((
                        "MEASURED DPI".to_string(),
                        format!("{:.0} DPI", res.measured_dpi),
                    ));
                    pills.push((
                        "DEVIATION".to_string(),
                        format!("{:+.2}%", res.deviation_pct),
                    ));
                    pills.push((
                        "SENS MATCH".to_string(),
                        format!("{:.3}×", res.sensitivity_multiplier),
                    ));
                }
                render_status_pill_rows(ui, p, inner_w, &pills);

                if let Some(res) = eval {
                    ui.add_space(6.0);
                    let straight_note = if snap.straightness_pct < 96.0 {
                        " Tip: straightness was below 96% — try sliding against a straight edge for higher precision."
                    } else {
                        ""
                    };
                    let summary = format!(
                        "Configured {} DPI measured {:.0} DPI ({:+.2}%) over {:.1} {}. Multiply in-game sensitivity by {:.3}× to match a true {} DPI baseline.{straight_note}",
                        self.configured_dpi,
                        res.measured_dpi,
                        res.deviation_pct,
                        self.ruler_distance,
                        self.ruler_unit.label(),
                        res.sensitivity_multiplier,
                        self.configured_dpi,
                    );
                    semantic_banner(ui, p, mode, inner_w, res.severity, &summary);
                }
            }
        });

        if toggle_run {
            self.toggle_dpi_measurement(ctx);
        }
        if clear_run {
            self.reset_dpi_measurement();
        }
    }

    fn render_click_bounce_card(
        &self,
        ui: &mut egui::Ui,
        p: Palette,
        mode: ThemeMode,
        card_w: f32,
    ) {
        let inner_w = (card_w - 24.0).max(160.0);
        let clicks = &self.last_snapshot.clicks;

        card_frame(ui, p, card_w, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Click Debounce & Double-Click Tester")
                        .size(14.0)
                        .strong()
                        .color(p.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (badge, col) = if clicks.total_bounce_faults > 0 {
                        (
                            format!(
                                "{} BOUNCE FAULT{}",
                                clicks.total_bounce_faults,
                                if clicks.total_bounce_faults == 1 {
                                    ""
                                } else {
                                    "S"
                                }
                            ),
                            p.bad,
                        )
                    } else if clicks.total_presses > 0 {
                        ("0 FAULTS".to_string(), p.ok)
                    } else {
                        ("READY".to_string(), p.text_muted)
                    };
                    ui.label(RichText::new(badge).size(11.0).strong().color(col));
                });
            });

            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "Flags unintentional release -> press rebounds faster than {:.0} ms while capturing. Click anywhere to test your mouse debounce.",
                        BOUNCE_THRESHOLD_MS
                    ))
                    .size(11.5)
                    .color(p.text_muted),
                )
                .wrap(),
            );
            ui.add_space(8.0);

            egui::Grid::new("click_bounce_grid")
                .num_columns(5)
                .spacing([12.0, 5.0])
                .show(ui, |ui| {
                    for hdr in ["Button", "Presses", "Hold", "Fastest Gap", "Faults"] {
                        ui.label(RichText::new(hdr).size(10.5).strong().color(p.text_muted));
                    }
                    ui.end_row();

                    for btn in &clicks.buttons {
                        ui.label(RichText::new(btn.button.label()).size(12.0).color(p.text));
                        ui.label(
                            RichText::new(btn.press_count.to_string())
                                .size(11.5)
                                .monospace()
                                .color(p.text),
                        );
                        let hold = btn
                            .last_hold_ms
                            .map(|v| format!("{v:.1} ms"))
                            .unwrap_or_else(|| "—".to_string());
                        ui.label(
                            RichText::new(hold)
                                .size(11.5)
                                .monospace()
                                .color(p.text_muted),
                        );
                        let fastest = btn
                            .min_release_to_press_ms
                            .map(|v| format!("{v:.1} ms"))
                            .unwrap_or_else(|| "—".to_string());
                        let fastest_col = match btn.min_release_to_press_ms {
                            Some(v) if v < BOUNCE_THRESHOLD_MS => p.bad,
                            Some(_) => p.text,
                            None => p.text_muted,
                        };
                        ui.label(
                            RichText::new(fastest)
                                .size(11.5)
                                .monospace()
                                .color(fastest_col),
                        );
                        let fault_col = if btn.bounce_faults > 0 {
                            p.bad
                        } else if btn.press_count > 0 {
                            p.ok
                        } else {
                            p.text_muted
                        };
                        ui.label(
                            RichText::new(btn.bounce_faults.to_string())
                                .size(11.5)
                                .monospace()
                                .strong()
                                .color(fault_col),
                        );
                        ui.end_row();
                    }
                });

            ui.add_space(8.0);
            if clicks.recent_events.is_empty() {
                semantic_banner(
                    ui,
                    p,
                    mode,
                    inner_w,
                    Severity::Info,
                    "No clicks recorded yet. Start Capture and rapidly click left/right buttons to verify your debounce setting.",
                );
            } else {
                egui::Frame::NONE
                    .fill(p.bg)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        let box_w = (inner_w - 16.0).max(120.0);
                        ui.set_min_width(box_w);
                        ui.set_max_width(box_w);
                        egui::ScrollArea::vertical()
                            .id_salt("click_bounce_recent")
                            .max_height(88.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for ev in clicks.recent_events.iter().take(12) {
                                    let gap = ev
                                        .release_to_press_ms
                                        .map(|v| format!("gap {v:.1} ms"))
                                        .unwrap_or_else(|| "first press".to_string());
                                    let hold = ev
                                        .hold_ms
                                        .map(|v| format!(" · hold {v:.1} ms"))
                                        .unwrap_or_default();
                                    let mut job = LayoutJob::default();
                                    job.wrap.max_width = (box_w - 12.0).max(100.0);
                                    job.append(
                                        &format!("{:<7} ", ev.button.label()),
                                        0.0,
                                        TextFormat {
                                            font_id: FontId::monospace(11.0),
                                            color: p.text,
                                            ..Default::default()
                                        },
                                    );
                                    job.append(
                                        &format!("{gap}{hold}"),
                                        0.0,
                                        TextFormat {
                                            font_id: FontId::monospace(11.0),
                                            color: if ev.is_bounce_fault {
                                                p.bad
                                            } else {
                                                p.text_muted
                                            },
                                            ..Default::default()
                                        },
                                    );
                                    if ev.is_bounce_fault {
                                        job.append(
                                            "  [BOUNCE]",
                                            0.0,
                                            TextFormat {
                                                font_id: FontId::monospace(11.0),
                                                color: p.bad,
                                                ..Default::default()
                                            },
                                        );
                                    }
                                    ui.add(egui::Label::new(job).wrap());
                                }
                            });
                    });
            }
        });
    }

    fn render_timer_benchmark_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        p: Palette,
        mode: ThemeMode,
        card_w: f32,
    ) {
        let inner_w = (card_w - 24.0).max(160.0);
        let mut trigger_bench = false;

        card_frame(ui, p, card_w, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("OS Timer Jitter (1s Test)")
                        .size(14.0)
                        .strong()
                        .color(p.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.timer_running {
                        ui.spinner();
                        ui.label(
                            RichText::new("Measuring 1s...")
                                .size(11.5)
                                .color(p.text_muted),
                        );
                    } else if ui.add(primary_btn("Run 1s Test", p)).clicked() {
                        trigger_bench = true;
                    }
                });
            });

            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Requests a 1 ms timer for 1 second (restored automatically) to expose OS scheduling spikes and DPC stutter without admin rights.",
                    )
                    .size(11.5)
                    .color(p.text_muted),
                )
                .wrap(),
            );
            ui.add_space(8.0);

            if let Some(bench) = &self.timer_benchmark {
                let stats_line = format!(
                    "Median {:.2} ms · p95 {:.2} ms · p99 {:.2} ms · Max {:.2} ms · Spikes >3ms: {}",
                    bench.median_ms,
                    bench.p95_ms,
                    bench.p99_ms,
                    bench.max_ms,
                    bench.spike_count_over_3ms
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(stats_line)
                            .size(11.5)
                            .monospace()
                            .color(p.text),
                    )
                    .wrap(),
                );
                ui.add_space(6.0);
                paint_timer_sparkline(ui, p, inner_w, 126.0, bench);
                ui.add_space(8.0);

                let verdict = match bench.severity {
                    Severity::Ok => format!(
                        "Steady 1 ms timer scheduling across {} wakes ({:.0} ms run).",
                        bench.samples_ms.len(),
                        bench.elapsed_ms
                    ),
                    Severity::Warn => format!(
                        "Noticeable wake jitter ({} spikes > 3 ms). Heavy background apps or DPC-heavy drivers can cause micro-stutters.",
                        bench.spike_count_over_3ms
                    ),
                    Severity::Bad => format!(
                        "Severe timer delay (median {:.2} ms, max {:.2} ms). Check power saving or noisy drivers.",
                        bench.median_ms, bench.max_ms
                    ),
                    Severity::Info => "Timer benchmark completed.".to_string(),
                };
                semantic_banner(ui, p, mode, inner_w, bench.severity, &verdict);
            } else {
                paint_empty_chart_box(
                    ui,
                    p,
                    inner_w,
                    148.0,
                    "Click Run 1s Test to plot ~600 real Windows 1 ms sleep wake intervals.",
                );
            }
        });

        if trigger_bench {
            self.start_timer_benchmark(ctx);
        }
    }
}

fn health_badge_label(sev: Severity) -> &'static str {
    match sev {
        Severity::Ok => "CLEAN",
        Severity::Info => "COLLECTING",
        Severity::Warn => "JITTER",
        Severity::Bad => "UNSTABLE",
    }
}

/// Custom monochrome interval scatter + stem chart distinguishing true saturated dropouts
/// (red) from harmless slow-hand empty slots (muted gray).
fn paint_interval_chart(
    ui: &mut egui::Ui,
    p: Palette,
    width: f32,
    height: f32,
    ps: &PollingStats,
    saturated_only: bool,
) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        CornerRadius::same(6),
        p.bg,
        Stroke::new(1.0_f32, p.border),
        egui::StrokeKind::Inside,
    );

    let visible: Vec<_> = ps
        .recent_samples
        .iter()
        .copied()
        .filter(|s| !saturated_only || s.saturated)
        .collect();

    if visible.is_empty() {
        let msg = if ps.recent_samples.is_empty() {
            "Click Start Capture and move in brisk circles to graph interval timing"
        } else {
            "No saturated intervals yet — swipe slightly faster to saturate this polling rate"
        };
        let galley = painter.layout(
            msg.to_string(),
            FontId::proportional(11.5),
            p.text_muted,
            (width - 28.0).max(80.0),
        );
        painter.galley(
            Pos2::new(
                rect.center().x - galley.size().x * 0.5,
                rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            p.text_muted,
        );
        return;
    }

    let pad_left = 50.0;
    let pad_right = 10.0;
    let pad_top = 12.0;
    let pad_bottom = 18.0;
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + pad_left, rect.top() + pad_top),
        Pos2::new(rect.right() - pad_right, rect.bottom() - pad_bottom),
    );
    if plot.width() <= 10.0 || plot.height() <= 10.0 {
        return;
    }

    let nominal = if ps.nominal_interval_us > 0.0 {
        ps.nominal_interval_us
    } else {
        ps.median_us.max(250.0)
    };
    let y_max = (nominal * 3.0).max(ps.p99_us * 1.15).min(nominal * 8.0);
    let y_to_px = |us: f64| -> f32 {
        let clamped = (us / y_max).clamp(0.0, 1.0) as f32;
        plot.bottom() - clamped * plot.height()
    };

    let band_top = y_to_px(nominal * 1.15);
    let band_bot = y_to_px(nominal * 0.85);
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(plot.left(), band_top),
            Pos2::new(plot.right(), band_bot),
        ),
        CornerRadius::ZERO,
        p.surface_alt,
    );

    for (mult, label_col) in [(1.0, p.text_muted), (2.0, p.border)] {
        let val = nominal * mult;
        if val <= y_max {
            let y = y_to_px(val);
            painter.line_segment(
                [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
                Stroke::new(1.0_f32, p.border),
            );
            painter.text(
                Pos2::new(plot.left() - 5.0, y),
                Align2::RIGHT_CENTER,
                format!("{:.2}ms", val / 1000.0),
                FontId::monospace(9.5),
                label_col,
            );
        }
    }
    painter.text(
        Pos2::new(plot.left() - 5.0, plot.top()),
        Align2::RIGHT_TOP,
        format!("{:.2}ms", y_max / 1000.0),
        FontId::monospace(9.5),
        p.text_muted,
    );
    painter.text(
        Pos2::new(plot.left() - 5.0, plot.bottom()),
        Align2::RIGHT_BOTTOM,
        "0.00ms",
        FontId::monospace(9.5),
        p.text_muted,
    );

    let n = visible.len();
    let step_x = if n > 1 {
        plot.width() / ((n - 1) as f32)
    } else {
        0.0
    };
    let nominal_y = y_to_px(nominal);

    for (i, sample) in visible.iter().enumerate() {
        let x = plot.left() + (i as f32) * step_x;
        let y = y_to_px(sample.dt_us);
        let is_bunched = sample.dt_us < nominal * 0.35;
        let is_gap = sample.dt_us > nominal * 1.85;
        let is_true_fault = is_bunched || (is_gap && sample.saturated);
        let is_warn = !is_true_fault
            && sample.saturated
            && (sample.dt_us > nominal * 1.15 || sample.dt_us < nominal * 0.85);

        let col = if is_true_fault {
            p.bad
        } else if is_warn {
            p.warn
        } else if sample.saturated {
            p.text
        } else {
            p.text_muted
        };

        if is_true_fault || is_warn {
            painter.line_segment(
                [Pos2::new(x, nominal_y), Pos2::new(x, y)],
                Stroke::new(1.0_f32, col),
            );
        }
        painter.circle_filled(
            Pos2::new(x, y),
            if is_true_fault {
                2.4
            } else if sample.saturated {
                1.6
            } else {
                1.2
            },
            col,
        );
    }

    let filter_tag = if saturated_only {
        "saturated only"
    } else {
        "gray = slow hand"
    };
    painter.text(
        Pos2::new(plot.right(), rect.bottom() - 3.0),
        Align2::RIGHT_BOTTOM,
        format!("Intervals ({n} pts · {filter_tag})"),
        FontId::monospace(9.5),
        p.text_muted,
    );
}

/// Custom monochrome binned interval distribution histogram exposing 1x nominal peak vs.
/// 2x dropped-frame harmonics and USB bottlenecks.
fn paint_histogram_chart(
    ui: &mut egui::Ui,
    p: Palette,
    width: f32,
    height: f32,
    ps: &PollingStats,
    saturated_only: bool,
) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        CornerRadius::same(6),
        p.bg,
        Stroke::new(1.0_f32, p.border),
        egui::StrokeKind::Inside,
    );

    if ps.histogram_bins.is_empty() {
        let galley = painter.layout(
            "Interval histogram (1x peak vs. 2x dropped-frame harmonic)".to_string(),
            FontId::proportional(11.0),
            p.text_muted,
            (width - 24.0).max(80.0),
        );
        painter.galley(
            Pos2::new(
                rect.center().x - galley.size().x * 0.5,
                rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            p.text_muted,
        );
        return;
    }

    let pad_left = 10.0;
    let pad_right = 10.0;
    let pad_top = 18.0;
    let pad_bottom = 26.0;
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + pad_left, rect.top() + pad_top),
        Pos2::new(rect.right() - pad_right, rect.bottom() - pad_bottom),
    );
    if plot.width() <= 20.0 || plot.height() <= 16.0 {
        return;
    }

    let max_count = ps
        .histogram_bins
        .iter()
        .map(|b| {
            if saturated_only {
                b.saturated_count
            } else {
                b.total_count()
            }
        })
        .max()
        .unwrap_or(0);

    if max_count == 0 {
        let galley = painter.layout(
            "Swipe faster to populate saturated histogram bins".to_string(),
            FontId::proportional(11.0),
            p.text_muted,
            (width - 24.0).max(80.0),
        );
        painter.galley(
            Pos2::new(
                rect.center().x - galley.size().x * 0.5,
                rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            p.text_muted,
        );
        return;
    }

    let nominal = ps.nominal_interval_us.max(125.0);
    let bin_count = ps.histogram_bins.len();
    let slot_w = plot.width() / (bin_count as f32);
    let bar_w = (slot_w - 1.5).max(2.0);

    painter.text(
        Pos2::new(plot.left(), rect.top() + 4.0),
        Align2::LEFT_TOP,
        "Distribution",
        FontId::monospace(9.5),
        p.text_muted,
    );
    painter.text(
        Pos2::new(plot.right(), rect.top() + 4.0),
        Align2::RIGHT_TOP,
        format!("peak {max_count}"),
        FontId::monospace(9.5),
        p.text_muted,
    );

    painter.line_segment(
        [
            Pos2::new(plot.left(), plot.bottom()),
            Pos2::new(plot.right(), plot.bottom()),
        ],
        Stroke::new(1.0_f32, p.border),
    );

    let hover_pos = resp.hover_pos();
    let mut hovered_tip: Option<String> = None;

    for (i, bin) in ps.histogram_bins.iter().enumerate() {
        let x0 = plot.left() + (i as f32) * slot_w + (slot_w - bar_w) * 0.5;
        let x1 = x0 + bar_w;

        let total_c = if saturated_only {
            bin.saturated_count
        } else {
            bin.total_count()
        };
        let sat_c = bin.saturated_count;

        let mid_mult = if bin.is_overflow {
            3.0
        } else {
            ((bin.start_us + bin.end_us) * 0.5) / nominal
        };

        let sat_col = if mid_mult < 0.35 || mid_mult > 1.85 {
            p.bad
        } else if !(0.85..=1.15).contains(&mid_mult) {
            p.warn
        } else {
            p.text
        };

        if total_c > 0 {
            let total_h = ((total_c as f32) / (max_count as f32) * plot.height()).max(2.0);
            if !saturated_only && bin.unsaturated_count > 0 {
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(x0, plot.bottom() - total_h),
                        Pos2::new(x1, plot.bottom()),
                    ),
                    CornerRadius::same(1),
                    p.border,
                );
            }
            if sat_c > 0 {
                let sat_h = ((sat_c as f32) / (max_count as f32) * plot.height()).max(2.0);
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(x0, plot.bottom() - sat_h),
                        Pos2::new(x1, plot.bottom()),
                    ),
                    CornerRadius::same(1),
                    sat_col,
                );
            }
        }

        if let Some(hp) = hover_pos {
            if hp.x >= x0 - 1.0 && hp.x <= x1 + 1.0 && hp.y >= plot.top() && hp.y <= plot.bottom() {
                let range_str = if bin.is_overflow {
                    format!(">= {:.2} ms (>2.8x)", bin.start_us / 1000.0)
                } else {
                    format!("{:.2}–{:.2} ms", bin.start_us / 1000.0, bin.end_us / 1000.0)
                };
                hovered_tip = Some(format!(
                    "{range_str}\nSaturated: {} · Slow hand: {}",
                    bin.saturated_count, bin.unsaturated_count
                ));
            }
        }
    }

    let tick_y = plot.bottom() + 4.0;
    let draw_tick = |bin_pos: f32, label: &str, align: Align2, col: Color32| {
        let x = plot.left() + bin_pos * slot_w;
        painter.text(
            Pos2::new(x, tick_y),
            align,
            label,
            FontId::monospace(9.5),
            col,
        );
    };
    draw_tick(0.0, "0", Align2::LEFT_TOP, p.text_muted);
    draw_tick(
        10.0,
        &format!("1x ({:.2}ms)", nominal / 1000.0),
        Align2::CENTER_TOP,
        p.text,
    );
    draw_tick(20.0, "2x", Align2::CENTER_TOP, p.warn);
    draw_tick(
        (HISTOGRAM_BASE_BINS as f32) + 0.5,
        "2.8x+",
        Align2::RIGHT_TOP,
        p.text_muted,
    );

    if let Some(tip) = hovered_tip {
        resp.on_hover_text(tip);
    }
}

/// MouseTester-style Velocity (IPS) line + signed `dx`/`dy` counts strip.
fn paint_velocity_chart(
    ui: &mut egui::Ui,
    p: Palette,
    width: f32,
    height: f32,
    ps: &PollingStats,
    tr: &TrackingStats,
    dpi: f64,
) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        CornerRadius::same(6),
        p.bg,
        Stroke::new(1.0_f32, p.border),
        egui::StrokeKind::Inside,
    );

    if ps.recent_samples.is_empty() {
        paint_centered_hint(
            &painter,
            rect,
            p,
            "Start Capture and swipe your mouse to plot IPS velocity and raw dx/dy counts",
        );
        return;
    }

    let pad_left = 54.0;
    let pad_right = 10.0;
    let pad_top = 12.0;
    let pad_bottom = 18.0;

    let inner_h = (height - pad_top - pad_bottom).max(40.0);
    let vel_h = inner_h * 0.58;
    let counts_top = rect.top() + pad_top + vel_h + 10.0;

    let vel_plot = Rect::from_min_max(
        Pos2::new(rect.left() + pad_left, rect.top() + pad_top),
        Pos2::new(rect.right() - pad_right, rect.top() + pad_top + vel_h),
    );
    let cnt_plot = Rect::from_min_max(
        Pos2::new(rect.left() + pad_left, counts_top),
        Pos2::new(rect.right() - pad_right, rect.bottom() - pad_bottom),
    );

    let max_ips = ps
        .recent_samples
        .iter()
        .map(|s| s.speed_cps / dpi)
        .fold(10.0_f64, f64::max)
        .max(tr.peak_ips(dpi) * 0.5)
        * 1.1;

    // Velocity plot horizontal guides
    for frac in [0.0_f32, 0.5, 1.0] {
        let y = vel_plot.bottom() - frac * vel_plot.height();
        painter.line_segment(
            [
                Pos2::new(vel_plot.left(), y),
                Pos2::new(vel_plot.right(), y),
            ],
            Stroke::new(1.0_f32, p.border),
        );
        painter.text(
            Pos2::new(vel_plot.left() - 5.0, y),
            Align2::RIGHT_CENTER,
            format!("{:.0} IPS", max_ips * f64::from(frac)),
            FontId::monospace(9.5),
            p.text_muted,
        );
    }

    let n = ps.recent_samples.len();
    let step_x = if n > 1 {
        vel_plot.width() / ((n - 1) as f32)
    } else {
        0.0
    };

    let mut prev_pt: Option<Pos2> = None;
    for (i, s) in ps.recent_samples.iter().enumerate() {
        let ips = (s.speed_cps / dpi).clamp(0.0, max_ips);
        let x = vel_plot.left() + (i as f32) * step_x;
        let y = vel_plot.bottom() - ((ips / max_ips) as f32) * vel_plot.height();
        let pt = Pos2::new(x, y);
        if let Some(p0) = prev_pt {
            painter.line_segment([p0, pt], Stroke::new(1.5_f32, p.text));
        }
        prev_pt = Some(pt);
    }

    // Lower strip: signed dx (crisp monochrome) & dy (semantic info blue) raw counts.
    let cnt_mid_y = cnt_plot.center().y;
    let max_cnt = ps
        .recent_samples
        .iter()
        .map(|s| s.dx.abs().max(s.dy.abs()))
        .max()
        .unwrap_or(1)
        .max(4) as f32;

    painter.line_segment(
        [
            Pos2::new(cnt_plot.left(), cnt_mid_y),
            Pos2::new(cnt_plot.right(), cnt_mid_y),
        ],
        Stroke::new(1.0_f32, p.border),
    );
    painter.text(
        Pos2::new(cnt_plot.left() - 5.0, cnt_plot.top()),
        Align2::RIGHT_TOP,
        format!("+{max_cnt:.0}"),
        FontId::monospace(9.5),
        p.text_muted,
    );
    painter.text(
        Pos2::new(cnt_plot.left() - 5.0, cnt_plot.bottom()),
        Align2::RIGHT_BOTTOM,
        format!("-{max_cnt:.0}"),
        FontId::monospace(9.5),
        p.text_muted,
    );

    let half_h = cnt_plot.height() * 0.48;
    for (i, s) in ps.recent_samples.iter().enumerate() {
        let x = cnt_plot.left() + (i as f32) * step_x;
        if s.dx != 0 {
            let y_dx = cnt_mid_y - ((s.dx as f32) / max_cnt).clamp(-1.0, 1.0) * half_h;
            painter.line_segment(
                [Pos2::new(x, cnt_mid_y), Pos2::new(x, y_dx)],
                Stroke::new(1.1_f32, p.text),
            );
        }
        if s.dy != 0 {
            let y_dy = cnt_mid_y - ((s.dy as f32) / max_cnt).clamp(-1.0, 1.0) * half_h;
            painter.line_segment(
                [Pos2::new(x, cnt_mid_y), Pos2::new(x, y_dy)],
                Stroke::new(1.1_f32, p.info),
            );
        }
    }

    painter.text(
        Pos2::new(cnt_plot.right(), rect.bottom() - 3.0),
        Align2::RIGHT_BOTTOM,
        format!("Top: Smoothed IPS · Bottom: raw dx (mono) & dy (blue) · last {n} reports"),
        FontId::monospace(9.5),
        p.text_muted,
    );
}

/// Aspect-ratio-preserving 2D XY motion path plot exposing angle snapping, jitter, and spinouts.
fn paint_xy_trace_chart(
    ui: &mut egui::Ui,
    p: Palette,
    width: f32,
    height: f32,
    tr: &TrackingStats,
    dpi: f64,
) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        CornerRadius::same(6),
        p.bg,
        Stroke::new(1.0_f32, p.border),
        egui::StrokeKind::Inside,
    );

    if tr.xy_trace.len() < 2 {
        paint_centered_hint(
            &painter,
            rect,
            p,
            "Start Capture and draw circles or straight lines to inspect your 2D XY tracking path",
        );
        return;
    }

    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + 14.0, rect.top() + 18.0),
        Pos2::new(rect.right() - 14.0, rect.bottom() - 20.0),
    );
    if plot.width() <= 20.0 || plot.height() <= 20.0 {
        return;
    }

    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for pt in &tr.xy_trace {
        min_x = min_x.min(pt.x);
        max_x = max_x.max(pt.x);
        min_y = min_y.min(pt.y);
        max_y = max_y.max(pt.y);
    }

    let span_x = (max_x - min_x).max(20.0);
    let span_y = (max_y - min_y).max(20.0);
    // Uniform scale keeps circles truly circular (1:1 X/Y aspect ratio).
    let scale = (plot.width() / span_x).min(plot.height() / span_y) * 0.92;
    let mid_x = (min_x + max_x) * 0.5;
    let mid_y = (min_y + max_y) * 0.5;

    let to_screen = |x: f32, y: f32| -> Pos2 {
        Pos2::new(
            plot.center().x + (x - mid_x) * scale,
            plot.center().y + (y - mid_y) * scale,
        )
    };

    // Subtle center crosshair
    painter.line_segment(
        [
            Pos2::new(plot.left(), plot.center().y),
            Pos2::new(plot.right(), plot.center().y),
        ],
        Stroke::new(1.0_f32, p.surface_alt),
    );
    painter.line_segment(
        [
            Pos2::new(plot.center().x, plot.top()),
            Pos2::new(plot.center().x, plot.bottom()),
        ],
        Stroke::new(1.0_f32, p.surface_alt),
    );

    let peak_cps = tr.peak_cps.max(1.0) as f32;
    for pair in tr.xy_trace.windows(2) {
        let a = to_screen(pair[0].x, pair[0].y);
        let b = to_screen(pair[1].x, pair[1].y);
        let col = if pair[1].speed_cps > peak_cps * 0.8 && peak_cps > 15_000.0 {
            p.info
        } else {
            p.text
        };
        painter.line_segment([a, b], Stroke::new(1.4_f32, col));
    }

    // Highlight trail start (muted) and live cursor head (ok green).
    if let Some(first) = tr.xy_trace.first() {
        painter.circle_filled(to_screen(first.x, first.y), 2.5, p.text_muted);
    }
    if let Some(last) = tr.xy_trace.last() {
        painter.circle_filled(to_screen(last.x, last.y), 3.5, p.ok);
    }

    let cm_x = (f64::from(span_x) / dpi) * 2.54;
    let cm_y = (f64::from(span_y) / dpi) * 2.54;
    painter.text(
        Pos2::new(plot.left(), rect.top() + 4.0),
        Align2::LEFT_TOP,
        "2D XY Motion Trace (1:1 aspect)",
        FontId::monospace(9.5),
        p.text_muted,
    );
    painter.text(
        Pos2::new(plot.right(), rect.bottom() - 3.0),
        Align2::RIGHT_BOTTOM,
        format!(
            "Span {:.0}×{:.0} counts ({:.1}×{:.1} cm) · {} pts",
            span_x,
            span_y,
            cm_x,
            cm_y,
            tr.xy_trace.len()
        ),
        FontId::monospace(9.5),
        p.text_muted,
    );
}

fn paint_centered_hint(painter: &egui::Painter, rect: Rect, p: Palette, msg: &str) {
    let galley = painter.layout(
        msg.to_string(),
        FontId::proportional(11.5),
        p.text_muted,
        (rect.width() - 28.0).max(80.0),
    );
    painter.galley(
        Pos2::new(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        p.text_muted,
    );
}

fn paint_timer_sparkline(
    ui: &mut egui::Ui,
    p: Palette,
    width: f32,
    height: f32,
    bench: &TimerJitterBenchmark,
) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        CornerRadius::same(6),
        p.bg,
        Stroke::new(1.0_f32, p.border),
        egui::StrokeKind::Inside,
    );

    if bench.samples_ms.is_empty() {
        return;
    }

    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + 44.0, rect.top() + 10.0),
        Pos2::new(rect.right() - 10.0, rect.bottom() - 16.0),
    );
    if plot.width() <= 10.0 || plot.height() <= 10.0 {
        return;
    }

    let y_max = bench.max_ms.clamp(4.0, 20.0);
    let y_to_px = |ms: f64| -> f32 {
        let clamped = (ms / y_max).clamp(0.0, 1.0) as f32;
        plot.bottom() - clamped * plot.height()
    };

    for (ref_ms, col) in [(1.0, p.text_muted), (3.0, p.warn)] {
        if ref_ms <= y_max {
            let y = y_to_px(ref_ms);
            painter.line_segment(
                [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
                Stroke::new(1.0_f32, p.border),
            );
            painter.text(
                Pos2::new(plot.left() - 4.0, y),
                Align2::RIGHT_CENTER,
                format!("{ref_ms:.0}ms"),
                FontId::monospace(9.5),
                col,
            );
        }
    }
    painter.text(
        Pos2::new(plot.left() - 4.0, plot.top()),
        Align2::RIGHT_TOP,
        format!("{y_max:.1}ms"),
        FontId::monospace(9.5),
        p.text_muted,
    );

    let n = bench.samples_ms.len();
    let step_x = if n > 1 {
        plot.width() / ((n - 1) as f32)
    } else {
        0.0
    };

    let mut prev: Option<Pos2> = None;
    for (i, &ms) in bench.samples_ms.iter().enumerate() {
        let pt = Pos2::new(plot.left() + (i as f32) * step_x, y_to_px(ms));
        let col = if ms > 5.0 {
            p.bad
        } else if ms > 3.0 {
            p.warn
        } else {
            p.text
        };
        if let Some(p0) = prev {
            painter.line_segment([p0, pt], Stroke::new(1.1_f32, col));
        }
        if ms > 3.0 {
            painter.circle_filled(pt, 2.0, col);
        }
        prev = Some(pt);
    }

    painter.text(
        Pos2::new(plot.right(), rect.bottom() - 3.0),
        Align2::RIGHT_BOTTOM,
        format!("{n} consecutive 1 ms wakes"),
        FontId::monospace(9.5),
        p.text_muted,
    );
}

fn paint_empty_chart_box(ui: &mut egui::Ui, p: Palette, width: f32, height: f32, msg: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        CornerRadius::same(6),
        p.bg,
        Stroke::new(1.0_f32, p.border),
        egui::StrokeKind::Inside,
    );
    paint_centered_hint(&painter, rect, p, msg);
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

fn secondary_btn(label: &str, p: Palette) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(p.text))
        .fill(p.surface_alt)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(6))
}

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
        label_galley: std::sync::Arc<egui::Galley>,
        value_galley: std::sync::Arc<egui::Galley>,
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
            MeasuredPill {
                clamped_w: full_w.min(row_limit),
                height: label_galley.size().y.max(value_galley.size().y) + pad_y * 2.0,
                label_galley,
                value_galley,
            }
        })
        .collect();

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
                let (rect, _) = ui.allocate_exact_size(
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
                    let label_pos = Pos2::new(
                        rect.left() + pad_x,
                        rect.center().y - item.label_galley.size().y * 0.5,
                    );
                    let value_pos = Pos2::new(
                        label_pos.x + item.label_galley.size().x + inner_gap,
                        rect.center().y - item.value_galley.size().y * 0.5,
                    );
                    painter.galley(label_pos, item.label_galley.clone(), p.text_muted);
                    painter.galley(value_pos, item.value_galley.clone(), p.text);
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
