// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Shareable Input Lab report exports:
//! - RFC-4180 CSV export of raw per-packet polling intervals, deltas, speeds, and saturation flags
//! - Zero-dependency pure-Rust monochrome PNG benchmark card renderer (960×560, 4-bit indexed PNG)

use chrono::Local;

use crate::checks::{CheckReport, Severity};
use crate::diagnostics::{
    DiagnosticsSnapshot, PinnedBenchmark, TimerJitterBenchmark, BOUNCE_THRESHOLD_MS,
};
use crate::win::{AudioLatencyBenchmark, NetworkJitterBenchmark, UsbMouseTopology};

const IMG_W: usize = 960;
const IMG_H: usize = 560;
const RIG_W: usize = 1040;
const RIG_H: usize = 620;

/// Combined snapshot of Windows health, mouse polling, USB topology, audio, timer, and network
/// benchmarks rendered into a single shareable All-in-One Rig Scorecard PNG.
pub struct RigScorecardInput<'a> {
    pub windows_version: Option<&'a str>,
    pub health_checks: &'a [CheckReport],
    pub diagnostics: &'a DiagnosticsSnapshot,
    pub configured_dpi: u32,
    pub usb_topology: &'a [UsbMouseTopology],
    pub timer: Option<&'a TimerJitterBenchmark>,
    pub audio: Option<&'a AudioLatencyBenchmark>,
    pub network: Option<&'a NetworkJitterBenchmark>,
}

// 16-entry indexed palette (strict monochrome B&W + 4 semantic status colors only).
const C_BG: u8 = 0;
const C_SURFACE: u8 = 1;
const C_SURFACE_ALT: u8 = 2;
const C_BORDER: u8 = 3;
const C_GRID: u8 = 4;
const C_TEXT: u8 = 5;
const C_MUTED: u8 = 6;
const C_DIM: u8 = 7;
const C_OK: u8 = 8;
const C_INFO: u8 = 9;
const C_WARN: u8 = 10;
const C_BAD: u8 = 11;
const C_OK_BG: u8 = 12;
const C_INFO_BG: u8 = 13;
const C_WARN_BG: u8 = 14;
const C_BAD_BG: u8 = 15;

const DARK_PALETTE: [[u8; 3]; 16] = [
    [11, 11, 12],    // C_BG
    [20, 20, 22],    // C_SURFACE
    [30, 30, 34],    // C_SURFACE_ALT
    [48, 48, 54],    // C_BORDER
    [36, 36, 42],    // C_GRID
    [244, 244, 246], // C_TEXT
    [156, 156, 166], // C_MUTED
    [106, 106, 116], // C_DIM
    [56, 201, 120],  // C_OK
    [90, 169, 255],  // C_INFO
    [240, 180, 58],  // C_WARN
    [240, 90, 90],   // C_BAD
    [18, 42, 28],    // C_OK_BG
    [18, 34, 54],    // C_INFO_BG
    [46, 35, 16],    // C_WARN_BG
    [48, 22, 24],    // C_BAD_BG
];

const LIGHT_PALETTE: [[u8; 3]; 16] = [
    [246, 246, 247], // C_BG
    [255, 255, 255], // C_SURFACE
    [239, 239, 242], // C_SURFACE_ALT
    [208, 208, 214], // C_BORDER
    [226, 226, 232], // C_GRID
    [17, 17, 19],    // C_TEXT
    [94, 94, 104],   // C_MUTED
    [142, 142, 152], // C_DIM
    [24, 134, 75],   // C_OK
    [26, 102, 194],  // C_INFO
    [179, 107, 0],   // C_WARN
    [198, 40, 40],   // C_BAD
    [230, 245, 236], // C_OK_BG
    [230, 240, 250], // C_INFO_BG
    [253, 242, 220], // C_WARN_BG
    [252, 232, 232], // C_BAD_BG
];

/// Formats raw motion intervals and summary header comments as CSV suitable for spreadsheets or analysis.
pub fn format_intervals_csv(snap: &DiagnosticsSnapshot, configured_dpi: u32) -> String {
    let p = &snap.polling;
    let dpi_f = f64::from(configured_dpi.max(1));
    let mut out = String::with_capacity(512 + p.recent_samples.len() * 48);

    out.push_str("# Polled Bench Raw Motion Intervals\n");
    out.push_str(&format!(
        "# target_hz={},effective_hz={:.1},peak_hz={:.1},configured_dpi={}\n",
        p.nominal_hz, p.effective_hz, p.peak_hz, configured_dpi
    ));
    out.push_str(&format!(
        "# min_ms={:.3},median_ms={:.3},p95_ms={:.3},p99_ms={:.3},max_ms={:.3},sigma_ms={:.3}\n",
        p.min_us / 1000.0,
        p.median_us / 1000.0,
        p.p95_us / 1000.0,
        p.p99_us / 1000.0,
        p.max_us / 1000.0,
        p.std_dev_us / 1000.0
    ));
    out.push_str(&format!(
        "# saturated_pct={:.1},sat_within_15_pct={:.1},true_dropouts={},slow_slots={},bunched={},total_packets={}\n",
        p.saturated_pct,
        p.saturated_within_15_pct,
        p.true_dropout_count,
        p.slow_slot_count,
        p.bunch_count,
        p.total_motion_packets
    ));
    out.push_str("index,dt_us,inst_hz,dx,dy,speed_ips,saturated\n");

    for (idx, s) in p.recent_samples.iter().enumerate() {
        let inst_hz = if s.dt_us > 0.0 {
            1_000_000.0 / s.dt_us
        } else {
            0.0
        };
        let speed_ips = s.speed_cps / dpi_f;
        out.push_str(&format!(
            "{idx},{:.2},{:.1},{},{},{:.2},{}\n",
            s.dt_us, inst_hz, s.dx, s.dy, speed_ips, s.saturated
        ));
    }

    out
}

/// Renders a shareable 960×560 monochrome benchmark card PNG summarizing the live Input Lab session.
pub fn render_report_png(
    snap: &DiagnosticsSnapshot,
    configured_dpi: u32,
    dpi_ruler_inches: Option<f64>,
    timer: Option<&TimerJitterBenchmark>,
    dark_mode: bool,
) -> Vec<u8> {
    let mut canvas = Canvas::new(IMG_W, IMG_H, C_BG);
    let p = &snap.polling;
    let tr = &snap.tracking;
    let dpi_f = f64::from(configured_dpi.max(1));

    // Outer frame border
    canvas.stroke_rect(0, 0, IMG_W, IMG_H, C_BORDER);

    // --- HEADER BAR (y: 16..56) ---
    canvas.draw_text_scaled(24, 18, "POLLED BENCH", C_TEXT, 2);
    let subtitle = format!("v{} · RAW INPUT DIAGNOSTIC CARD", env!("CARGO_PKG_VERSION"));
    canvas.draw_text(24, 38, &subtitle, C_MUTED);

    let (badge_text, badge_fg, badge_bg) = match p.health_severity {
        Some(Severity::Ok) => ("OK · HEALTHY", C_OK, C_OK_BG),
        Some(Severity::Info) => ("INFO · NOTICE", C_INFO, C_INFO_BG),
        Some(Severity::Warn) => ("WARN · JITTER", C_WARN, C_WARN_BG),
        Some(Severity::Bad) => ("BAD · DROPOUTS", C_BAD, C_BAD_BG),
        None => ("NO MOTION DATA", C_MUTED, C_SURFACE_ALT),
    };
    let badge_w = text_width(badge_text, 1) + 20;
    let badge_x = IMG_W - 24 - badge_w;
    canvas.fill_rect(badge_x, 18, badge_w, 22, badge_bg);
    canvas.stroke_rect(badge_x, 18, badge_w, 22, badge_fg);
    canvas.draw_text(badge_x + 10, 26, badge_text, badge_fg);

    let ts = Local::now().format("%Y-%m-%d %H:%M").to_string();
    let ts_w = text_width(&ts, 1);
    canvas.draw_text(badge_x.saturating_sub(ts_w + 14), 26, &ts, C_MUTED);

    canvas.hline(24, IMG_W - 24, 54, C_BORDER);

    // --- 6 KPI TILES (y: 66..134) ---
    let tile_w = 144usize;
    let tile_gap = 9usize;
    let tile_y = 66usize;
    let tile_h = 66usize;

    let eff_val = if p.effective_hz >= 1.0 {
        format!("{:.0} Hz", p.effective_hz)
    } else {
        "--".to_string()
    };
    let target_sub = if p.nominal_hz > 0 {
        format!("TARGET {} Hz", p.nominal_hz)
    } else {
        format!("TARGET {}", snap.target.label().to_uppercase())
    };

    let consistency_val = if p.saturated_intervals > 0 {
        format!("{:.1}%", p.saturated_within_15_pct)
    } else if p.active_intervals > 0 {
        format!("{:.1}%", p.within_15_pct)
    } else {
        "--".to_string()
    };
    let sat_sub = if p.active_intervals > 0 {
        format!("SATURATED {:.0}%", p.saturated_pct)
    } else {
        "SATURATED --".to_string()
    };

    let dropout_val = p.true_dropout_count.to_string();
    let dropout_color = if p.true_dropout_count > 0 {
        C_BAD
    } else if p.bunch_count > 0 {
        C_WARN
    } else {
        C_TEXT
    };
    let dropout_sub = format!("BUNCHED {}", p.bunch_count);

    let med_val = if p.active_intervals > 0 {
        format!("{:.3} ms", p.median_us / 1000.0)
    } else {
        "--".to_string()
    };
    let p99_sub = if p.active_intervals > 0 {
        format!("P99 {:.3} ms", p.p99_us / 1000.0)
    } else {
        "P99 --".to_string()
    };

    let peak_ips = tr.peak_ips(dpi_f);
    let peak_g = tr.peak_accel_g(dpi_f);
    let track_val = if peak_ips >= 0.1 {
        format!("{:.1} IPS", peak_ips)
    } else {
        "--".to_string()
    };
    let track_sub = if peak_ips >= 0.1 {
        format!("{:.1} G @ {}", peak_g, configured_dpi)
    } else {
        format!("@ {} DPI", configured_dpi)
    };

    let dpi_eval = dpi_ruler_inches.and_then(|dist| snap.dpi_measurement.evaluate(dpi_f, dist));
    let (tile6_lbl, tile6_val, tile6_sub, tile6_col) = if let Some(cal) = dpi_eval {
        (
            "MEASURED DPI",
            format!("{:.0} DPI", cal.measured_dpi),
            format!("DEV {:+.1}%", cal.deviation_pct),
            severity_color(cal.severity),
        )
    } else if let Some(t) = timer {
        (
            "TIMER 1S TEST",
            format!("{:.2} ms", t.median_ms),
            format!("P99 {:.2} ms", t.p99_ms),
            severity_color(t.severity),
        )
    } else {
        (
            "CLICK DEBOUNCE",
            format!("{} PRESS", snap.clicks.total_presses),
            format!("{} FAULTS", snap.clicks.total_bounce_faults),
            if snap.clicks.total_bounce_faults > 0 {
                C_BAD
            } else {
                C_TEXT
            },
        )
    };

    let tiles = [
        ("EFFECTIVE RATE", eff_val, target_sub, C_TEXT),
        ("SAT +/-15%", consistency_val, sat_sub, C_TEXT),
        ("DROPOUTS", dropout_val, dropout_sub, dropout_color),
        ("MEDIAN INT", med_val, p99_sub, C_TEXT),
        ("PEAK SPEED", track_val, track_sub, C_TEXT),
        (tile6_lbl, tile6_val, tile6_sub, tile6_col),
    ];

    for (idx, (lbl, val, sub, val_col)) in tiles.iter().enumerate() {
        let x = 24 + idx * (tile_w + tile_gap);
        canvas.fill_rect(x, tile_y, tile_w, tile_h, C_SURFACE);
        canvas.stroke_rect(x, tile_y, tile_w, tile_h, C_BORDER);
        canvas.draw_text(x + 10, tile_y + 10, lbl, C_MUTED);
        // Scale 2 fits up to ~10 chars in 124px; fall back to scale 1 for longer values.
        let val_scale = if text_width(val, 2) <= tile_w - 18 {
            2
        } else {
            1
        };
        let val_y = if val_scale == 2 {
            tile_y + 26
        } else {
            tile_y + 30
        };
        canvas.draw_text_scaled(x + 10, val_y, val, *val_col, val_scale);
        canvas.draw_text_clipped(x + 10, tile_y + 50, sub, C_DIM, tile_w - 18);
    }

    // --- MAIN CHARTS ROW (y: 146..364) ---
    draw_intervals_panel(&mut canvas, 24, 146, 600, 218, snap);
    draw_histogram_panel(&mut canvas, 636, 146, 300, 218, snap);

    // --- BOTTOM ROW (y: 376..514) ---
    draw_xy_trace_panel(&mut canvas, 24, 376, 316, 138, snap);
    draw_summary_panel(
        &mut canvas,
        352,
        376,
        584,
        138,
        snap,
        configured_dpi,
        dpi_ruler_inches,
        timer,
    );

    // --- FOOTER (y: 526..548) ---
    canvas.hline(24, IMG_W - 24, 524, C_BORDER);
    canvas.draw_text(
        24,
        535,
        "POLLED · OPEN-SOURCE WINDOWS INPUT DIAGNOSTICS (AGPL-3.0)",
        C_DIM,
    );
    let right_foot = "github.com/k1jaa/polled";
    canvas.draw_text(
        IMG_W - 24 - text_width(right_foot, 1),
        535,
        right_foot,
        C_DIM,
    );

    let palette = if dark_mode {
        &DARK_PALETTE
    } else {
        &LIGHT_PALETTE
    };
    encode_indexed_4bit_png(&canvas.pixels, IMG_W, IMG_H, palette)
}

/// Renders a 960×560 monochrome Before (A) vs After (B) Input Lab comparison card PNG.
pub fn render_ab_comparison_png(
    baseline: &PinnedBenchmark,
    current: &DiagnosticsSnapshot,
    current_label: &str,
    current_dpi: u32,
    dark_mode: bool,
) -> Vec<u8> {
    let mut canvas = Canvas::new(IMG_W, IMG_H, C_BG);
    canvas.stroke_rect(0, 0, IMG_W, IMG_H, C_BORDER);

    // Header
    canvas.draw_text_scaled(24, 18, "POLLED BENCH · A/B DIFF", C_TEXT, 2);
    let sub = format!(
        "v{} · BEFORE (A) VS AFTER (B) BENCHMARK COMPARISON",
        env!("CARGO_PKG_VERSION")
    );
    canvas.draw_text(24, 38, &sub, C_MUTED);
    let ts = Local::now().format("%Y-%m-%d %H:%M").to_string();
    canvas.draw_text(IMG_W - 24 - text_width(&ts, 1), 26, &ts, C_MUTED);
    canvas.hline(24, IMG_W - 24, 54, C_BORDER);

    // Comparison table panel (y: 66..318)
    let tx = 24usize;
    let ty = 66usize;
    let tw = IMG_W - 48;
    let th = 252usize;
    canvas.fill_rect(tx, ty, tw, th, C_SURFACE);
    canvas.stroke_rect(tx, ty, tw, th, C_BORDER);

    let col_m = tx + 16;
    let col_a = tx + 240;
    let col_b = tx + 480;
    let col_d = tx + 720;

    canvas.fill_rect(tx + 1, ty + 1, tw - 2, 26, C_SURFACE_ALT);
    canvas.hline(tx, tx + tw, ty + 27, C_BORDER);
    canvas.draw_text(col_m, ty + 10, "METRIC", C_MUTED);
    canvas.draw_text_clipped(
        col_a,
        ty + 10,
        &format!("A: {}", baseline.label),
        C_INFO,
        224,
    );
    canvas.draw_text_clipped(col_b, ty + 10, &format!("B: {current_label}"), C_TEXT, 224);
    canvas.draw_text(col_d, ty + 10, "DELTA (B - A)", C_MUTED);

    let pa = &baseline.snapshot.polling;
    let pb = &current.polling;
    let ta = &baseline.snapshot.tracking;
    let tb = &current.tracking;
    let dpi_a = f64::from(baseline.configured_dpi.max(1));
    let dpi_b = f64::from(current_dpi.max(1));

    let sat_a = if pa.saturated_intervals > 0 {
        pa.saturated_within_15_pct
    } else {
        pa.within_15_pct
    };
    let sat_b = if pb.saturated_intervals > 0 {
        pb.saturated_within_15_pct
    } else {
        pb.within_15_pct
    };

    let delta_color = |diff: f64, higher_is_better: bool, eps: f64| -> u8 {
        if diff.abs() <= eps {
            C_MUTED
        } else if (diff > 0.0) == higher_is_better {
            C_OK
        } else {
            C_BAD
        }
    };

    let rows: Vec<(&str, String, String, String, u8)> = vec![
        (
            "TARGET POLLING",
            format!("{} Hz", pa.nominal_hz),
            format!("{} Hz", pb.nominal_hz),
            format!("{:+} Hz", pb.nominal_hz as i64 - pa.nominal_hz as i64),
            C_MUTED,
        ),
        (
            "EFFECTIVE RATE",
            format!("{:.0} Hz", pa.effective_hz),
            format!("{:.0} Hz", pb.effective_hz),
            format!("{:+.0} Hz", pb.effective_hz - pa.effective_hz),
            delta_color(pb.effective_hz - pa.effective_hz, true, 15.0),
        ),
        (
            "SATURATED +/-15%",
            format!("{:.1}%", sat_a),
            format!("{:.1}%", sat_b),
            format!("{:+.1}%", sat_b - sat_a),
            delta_color(sat_b - sat_a, true, 0.5),
        ),
        (
            "MEDIAN INTERVAL",
            format!("{:.3} ms", pa.median_us / 1000.0),
            format!("{:.3} ms", pb.median_us / 1000.0),
            format!("{:+.3} ms", (pb.median_us - pa.median_us) / 1000.0),
            delta_color(pb.median_us - pa.median_us, false, 5.0),
        ),
        (
            "P99 INTERVAL",
            format!("{:.3} ms", pa.p99_us / 1000.0),
            format!("{:.3} ms", pb.p99_us / 1000.0),
            format!("{:+.3} ms", (pb.p99_us - pa.p99_us) / 1000.0),
            delta_color(pb.p99_us - pa.p99_us, false, 15.0),
        ),
        (
            "JITTER SIGMA",
            format!("{:.3} ms", pa.std_dev_us / 1000.0),
            format!("{:.3} ms", pb.std_dev_us / 1000.0),
            format!("{:+.3} ms", (pb.std_dev_us - pa.std_dev_us) / 1000.0),
            delta_color(pb.std_dev_us - pa.std_dev_us, false, 8.0),
        ),
        (
            "TRUE DROPOUTS",
            pa.true_dropout_count.to_string(),
            pb.true_dropout_count.to_string(),
            format!(
                "{:+}",
                pb.true_dropout_count as i64 - pa.true_dropout_count as i64
            ),
            delta_color(
                pb.true_dropout_count as f64 - pa.true_dropout_count as f64,
                false,
                0.0,
            ),
        ),
        (
            "BUNCHED BURSTS",
            pa.bunch_count.to_string(),
            pb.bunch_count.to_string(),
            format!("{:+}", pb.bunch_count as i64 - pa.bunch_count as i64),
            delta_color(pb.bunch_count as f64 - pa.bunch_count as f64, false, 0.0),
        ),
        (
            "PEAK TRACKING",
            format!(
                "{:.1} IPS / {:.1} G",
                ta.peak_ips(dpi_a),
                ta.peak_accel_g(dpi_a)
            ),
            format!(
                "{:.1} IPS / {:.1} G",
                tb.peak_ips(dpi_b),
                tb.peak_accel_g(dpi_b)
            ),
            format!("{:+.1} IPS", tb.peak_ips(dpi_b) - ta.peak_ips(dpi_a)),
            C_MUTED,
        ),
    ];

    for (idx, (metric, va, vb, vd, dcol)) in rows.iter().enumerate() {
        let ry = ty + 36 + idx * 23;
        if idx > 0 {
            canvas.hline(tx + 10, tx + tw - 10, ry - 6, C_GRID);
        }
        canvas.draw_text(col_m, ry, metric, C_MUTED);
        canvas.draw_text(col_a, ry, va, C_TEXT);
        canvas.draw_text(col_b, ry, vb, C_TEXT);
        canvas.draw_text(col_d, ry, vd, *dcol);
    }

    // Side-by-side Histograms (y: 330..514)
    draw_labeled_histogram_panel(
        &mut canvas,
        24,
        330,
        450,
        184,
        &format!("HISTOGRAM A: {}", baseline.label),
        &baseline.snapshot,
    );
    draw_labeled_histogram_panel(
        &mut canvas,
        486,
        330,
        450,
        184,
        &format!("HISTOGRAM B: {current_label}"),
        current,
    );

    canvas.hline(24, IMG_W - 24, 524, C_BORDER);
    canvas.draw_text(
        24,
        535,
        "POLLED · OPEN-SOURCE WINDOWS INPUT DIAGNOSTICS (AGPL-3.0)",
        C_DIM,
    );
    let right_foot = "github.com/k1jaa/polled";
    canvas.draw_text(
        IMG_W - 24 - text_width(right_foot, 1),
        535,
        right_foot,
        C_DIM,
    );

    let palette = if dark_mode {
        &DARK_PALETTE
    } else {
        &LIGHT_PALETTE
    };
    encode_indexed_4bit_png(&canvas.pixels, IMG_W, IMG_H, palette)
}

fn draw_labeled_histogram_panel(
    c: &mut Canvas,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    title: &str,
    snap: &DiagnosticsSnapshot,
) {
    draw_histogram_panel(c, x, y, w, h, snap);
    c.fill_rect(x + 10, y + 8, w - 20, 12, C_SURFACE);
    c.draw_text_clipped(x + 12, y + 10, title, C_TEXT, w - 24);
}

fn draw_intervals_panel(
    c: &mut Canvas,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    snap: &DiagnosticsSnapshot,
) {
    c.fill_rect(x, y, w, h, C_SURFACE);
    c.stroke_rect(x, y, w, h, C_BORDER);
    c.draw_text(x + 12, y + 10, "POLLING INTERVALS (MS)", C_TEXT);

    // Legend on right side of header
    let legend_y = y + 10;
    c.fill_rect(x + w - 258, legend_y + 1, 6, 6, C_TEXT);
    c.draw_text(x + w - 249, legend_y, "SAT", C_MUTED);
    c.fill_rect(x + w - 216, legend_y + 1, 6, 6, C_DIM);
    c.draw_text(x + w - 207, legend_y, "SLOW", C_MUTED);
    c.fill_rect(x + w - 166, legend_y + 1, 6, 6, C_BAD);
    c.draw_text(x + w - 157, legend_y, "DROPOUT", C_MUTED);
    c.fill_rect(x + w - 92, legend_y + 1, 6, 6, C_WARN);
    c.draw_text(x + w - 83, legend_y, "BUNCHED", C_MUTED);

    let px = x + 56;
    let py = y + 28;
    let pw = w - 70;
    let ph = h - 46;
    c.fill_rect(px, py, pw, ph, C_SURFACE_ALT);
    c.stroke_rect(px, py, pw, ph, C_BORDER);

    let p = &snap.polling;
    let nominal_us = if p.nominal_interval_us > 0.0 {
        p.nominal_interval_us
    } else {
        1000.0
    };
    let y_max_us = (nominal_us * 2.8).max(250.0);

    // Gridlines at 0x, 1x, 2x, 2.8x nominal
    for &(mult, col) in &[
        (0.0, C_BORDER),
        (1.0, C_INFO),
        (2.0, C_GRID),
        (2.8, C_BORDER),
    ] {
        let us = nominal_us * mult;
        let gy =
            py + ph - ((us / y_max_us).clamp(0.0, 1.0) * (ph as f64 - 2.0)).round() as usize - 1;
        if mult > 0.0 && mult < 2.8 {
            c.hline_dashed(px + 1, px + pw - 1, gy, col, 4, 3);
        }
        let lbl = format!("{:.2}", us / 1000.0);
        let lw = text_width(&lbl, 1);
        c.draw_text(px.saturating_sub(lw + 6), gy.saturating_sub(3), &lbl, C_DIM);
    }

    if p.recent_samples.is_empty() {
        let msg = "START CAPTURE & MOVE MOUSE TO PLOT INTERVALS";
        let mw = text_width(msg, 1);
        c.draw_text(px + pw.saturating_sub(mw) / 2, py + ph / 2 - 4, msg, C_DIM);
        return;
    }

    let n = p.recent_samples.len();
    for (i, sample) in p.recent_samples.iter().enumerate() {
        let sx = if n <= 1 {
            px + pw / 2
        } else {
            px + 2 + (i * (pw - 5)) / (n - 1)
        };
        let norm_y = (sample.dt_us / y_max_us).clamp(0.0, 1.0);
        let sy = py + ph - 2 - (norm_y * (ph as f64 - 4.0)).round() as usize;

        let is_dropout = sample.saturated && sample.dt_us > nominal_us * 1.85;
        let is_bunched = sample.dt_us < nominal_us * 0.35;

        let (col, size) = if is_dropout {
            (C_BAD, 3usize)
        } else if is_bunched {
            (C_WARN, 3usize)
        } else if sample.saturated {
            (C_TEXT, 2usize)
        } else {
            (C_DIM, 2usize)
        };
        c.fill_rect(
            sx.saturating_sub(size / 2),
            sy.saturating_sub(size / 2),
            size,
            size,
            col,
        );
    }

    let foot = format!(
        "{} RECENT SAMPLES · 1.0x = {:.3} ms",
        n,
        nominal_us / 1000.0
    );
    c.draw_text(px, py + ph + 5, &foot, C_DIM);
}

fn draw_histogram_panel(
    c: &mut Canvas,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    snap: &DiagnosticsSnapshot,
) {
    c.fill_rect(x, y, w, h, C_SURFACE);
    c.stroke_rect(x, y, w, h, C_BORDER);
    c.draw_text(x + 12, y + 10, "INTERVAL HISTOGRAM", C_TEXT);

    let px = x + 12;
    let py = y + 28;
    let pw = w - 24;
    let ph = h - 46;
    c.fill_rect(px, py, pw, ph, C_SURFACE_ALT);
    c.stroke_rect(px, py, pw, ph, C_BORDER);

    let bins = &snap.polling.histogram_bins;
    let max_count = bins.iter().map(|b| b.total_count()).max().unwrap_or(0);
    if bins.is_empty() || max_count == 0 {
        let msg = "NO HISTOGRAM DATA";
        let mw = text_width(msg, 1);
        c.draw_text(px + pw.saturating_sub(mw) / 2, py + ph / 2 - 4, msg, C_DIM);
        return;
    }

    let bin_count = bins.len();
    let usable_w = pw.saturating_sub(8);
    let slot_w = (usable_w as f64) / (bin_count as f64);

    for (idx, bin) in bins.iter().enumerate() {
        let total = bin.total_count();
        if total == 0 {
            continue;
        }
        let bx0 = px + 4 + (idx as f64 * slot_w).floor() as usize;
        let bw = (slot_w - 1.5).max(2.0).floor() as usize;

        let total_h = ((f64::from(total) / f64::from(max_count)) * (ph as f64 - 8.0))
            .round()
            .max(1.0) as usize;
        let sat_h = ((f64::from(bin.saturated_count) / f64::from(max_count)) * (ph as f64 - 8.0))
            .round() as usize;
        let unsat_h = total_h.saturating_sub(sat_h);

        let sat_col = if bin.is_overflow || idx >= 19 {
            C_BAD
        } else if idx <= 3 {
            C_WARN
        } else if (9..=11).contains(&idx) {
            C_OK
        } else {
            C_TEXT
        };

        let bottom = py + ph - 2;
        if unsat_h > 0 {
            c.fill_rect(bx0, bottom.saturating_sub(unsat_h), bw, unsat_h, C_DIM);
        }
        if sat_h > 0 {
            let sat_top = bottom.saturating_sub(unsat_h + sat_h);
            c.fill_rect(bx0, sat_top, bw, sat_h, sat_col);
        }
    }

    // X-axis reference ticks: 0x, 1.0x (bin 10), 2.0x (bin 20), 2.8x+
    let tick_y = py + ph + 5;
    c.draw_text(px, tick_y, "0x", C_DIM);
    let x1 = px + 4 + (10.0 * slot_w) as usize;
    c.draw_text(x1.saturating_sub(10), tick_y, "1.0x", C_INFO);
    let x2 = px + 4 + (20.0 * slot_w) as usize;
    c.draw_text(x2.saturating_sub(10), tick_y, "2.0x", C_DIM);
    let end_lbl = "2.8x+";
    c.draw_text(px + pw - text_width(end_lbl, 1), tick_y, end_lbl, C_DIM);
}

fn draw_xy_trace_panel(
    c: &mut Canvas,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    snap: &DiagnosticsSnapshot,
) {
    c.fill_rect(x, y, w, h, C_SURFACE);
    c.stroke_rect(x, y, w, h, C_BORDER);
    c.draw_text(x + 12, y + 10, "2D XY MOTION TRACE", C_TEXT);

    let px = x + 12;
    let py = y + 26;
    let pw = w - 24;
    let ph = h - 38;
    c.fill_rect(px, py, pw, ph, C_SURFACE_ALT);
    c.stroke_rect(px, py, pw, ph, C_BORDER);

    // Subtle crosshair
    c.hline_dashed(px + 1, px + pw - 1, py + ph / 2, C_GRID, 3, 3);
    c.vline_dashed(px + pw / 2, py + 1, py + ph - 1, C_GRID, 3, 3);

    let pts = &snap.tracking.xy_trace;
    if pts.len() < 2 {
        let msg = "NO TRACE";
        let mw = text_width(msg, 1);
        c.draw_text(px + pw.saturating_sub(mw) / 2, py + ph / 2 - 4, msg, C_DIM);
        return;
    }

    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for pt in pts {
        min_x = min_x.min(pt.x);
        max_x = max_x.max(pt.x);
        min_y = min_y.min(pt.y);
        max_y = max_y.max(pt.y);
    }
    let span_x = (max_x - min_x).max(1.0);
    let span_y = (max_y - min_y).max(1.0);
    let pad = 8.0_f32;
    let scale = ((pw as f32 - pad * 2.0) / span_x).min((ph as f32 - pad * 2.0) / span_y);
    let cx = px as f32 + pw as f32 * 0.5;
    let cy = py as f32 + ph as f32 * 0.5;
    let mid_x = (min_x + max_x) * 0.5;
    let mid_y = (min_y + max_y) * 0.5;

    let map_pt = |pt_x: f32, pt_y: f32| -> (i32, i32) {
        let sx = (cx + (pt_x - mid_x) * scale).round() as i32;
        let sy = (cy + (pt_y - mid_y) * scale).round() as i32;
        (
            sx.clamp(px as i32 + 2, (px + pw) as i32 - 3),
            sy.clamp(py as i32 + 2, (py + ph) as i32 - 3),
        )
    };

    for pair in pts.windows(2) {
        let (x0, y0) = map_pt(pair[0].x, pair[0].y);
        let (x1, y1) = map_pt(pair[1].x, pair[1].y);
        c.draw_line(x0, y0, x1, y1, C_TEXT);
    }
    if let Some(last) = pts.last() {
        let (lx, ly) = map_pt(last.x, last.y);
        c.fill_rect(
            (lx - 2).max(0) as usize,
            (ly - 2).max(0) as usize,
            5,
            5,
            C_INFO,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_summary_panel(
    c: &mut Canvas,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    snap: &DiagnosticsSnapshot,
    configured_dpi: u32,
    dpi_ruler_inches: Option<f64>,
    timer: Option<&TimerJitterBenchmark>,
) {
    c.fill_rect(x, y, w, h, C_SURFACE);
    c.stroke_rect(x, y, w, h, C_BORDER);
    c.draw_text(x + 12, y + 10, "DIAGNOSTIC SUMMARY", C_TEXT);

    let p = &snap.polling;
    let max_text_w = w - 24;
    let mut cy = y + 28;

    let line1 = format!(
        "INTERVALS  min {:.3} | med {:.3} | p95 {:.3} | p99 {:.3} | max {:.3} | sig {:.3} ms",
        p.min_us / 1000.0,
        p.median_us / 1000.0,
        p.p95_us / 1000.0,
        p.p99_us / 1000.0,
        p.max_us / 1000.0,
        p.std_dev_us / 1000.0
    );
    c.draw_text_clipped(x + 12, cy, &line1, C_TEXT, max_text_w);
    cy += 15;

    let line2 = format!(
        "ANOMALIES  dropouts {} | slow slots {} | bunched {} | packets {}",
        p.true_dropout_count, p.slow_slot_count, p.bunch_count, p.total_motion_packets
    );
    c.draw_text_clipped(x + 12, cy, &line2, C_MUTED, max_text_w);
    cy += 15;

    let fastest_click = snap
        .clicks
        .buttons
        .iter()
        .filter_map(|b| b.min_release_to_press_ms)
        .reduce(f64::min);
    let line3 = match fastest_click {
        Some(ms) => format!(
            "CLICKS     {} presses | {} bounce faults (<{:.0}ms) | fastest {:.1} ms",
            snap.clicks.total_presses, snap.clicks.total_bounce_faults, BOUNCE_THRESHOLD_MS, ms
        ),
        None => format!(
            "CLICKS     {} presses | {} bounce faults (<{:.0}ms)",
            snap.clicks.total_presses, snap.clicks.total_bounce_faults, BOUNCE_THRESHOLD_MS
        ),
    };
    c.draw_text_clipped(x + 12, cy, &line3, C_MUTED, max_text_w);
    cy += 15;

    let dpi_line = match dpi_ruler_inches.and_then(|dist| {
        snap.dpi_measurement
            .evaluate(f64::from(configured_dpi.max(1)), dist)
    }) {
        Some(cal) => format!(
            "DPI CAL    {} -> {:.0} DPI ({:+.1}%) | straight {:.1}% | sens {:.3}x",
            configured_dpi,
            cal.measured_dpi,
            cal.deviation_pct,
            snap.dpi_measurement.straightness_pct,
            cal.sensitivity_multiplier
        ),
        None => format!("DPI CAL    not run (configured {} DPI)", configured_dpi),
    };
    c.draw_text_clipped(x + 12, cy, &dpi_line, C_MUTED, max_text_w);
    cy += 15;

    let timer_line = match timer {
        Some(t) => format!(
            "TIMER 1S   med {:.2}ms | p95 {:.2}ms | p99 {:.2}ms | max {:.2}ms | >3ms: {}",
            t.median_ms, t.p95_ms, t.p99_ms, t.max_ms, t.spike_count_over_3ms
        ),
        None => "TIMER 1S   not run".to_string(),
    };
    c.draw_text_clipped(x + 12, cy, &timer_line, C_MUTED, max_text_w);
    cy += 16;

    // Assessment box at bottom of summary panel
    let (box_fg, box_bg) = match p.health_severity {
        Some(s) => (severity_color(s), severity_bg_color(s)),
        None => (C_MUTED, C_SURFACE_ALT),
    };
    let box_h = (y + h).saturating_sub(cy + 8);
    if box_h >= 16 {
        c.fill_rect(x + 12, cy, w - 24, box_h, box_bg);
        c.stroke_rect(x + 12, cy, w - 24, box_h, box_fg);
        let summary_text = if p.health_summary.is_empty() {
            "Move the mouse smoothly across the pad while capturing to evaluate polling health."
        } else {
            p.health_summary.as_str()
        };
        c.draw_text_clipped(
            x + 18,
            cy + (box_h.saturating_sub(7)) / 2,
            summary_text,
            C_TEXT,
            w - 36,
        );
    }
}

/// Renders a shareable 1040×620 monochrome All-in-One Rig Scorecard PNG combining Windows Health,
/// 4K/8K Mouse Polling + OS Stall Correlation, USB PnP Topology, WASAPI Audio / Timer, and Dual-Hop Ping.
pub fn render_rig_scorecard_png(input: &RigScorecardInput<'_>, dark_mode: bool) -> Vec<u8> {
    let mut c = Canvas::new(RIG_W, RIG_H, C_BG);
    c.stroke_rect(0, 0, RIG_W, RIG_H, C_BORDER);

    let (mut ok_n, mut info_n, mut warn_n, mut bad_n) = (0usize, 0usize, 0usize, 0usize);
    for ch in input.health_checks {
        match ch.severity {
            Severity::Ok => ok_n += 1,
            Severity::Info => info_n += 1,
            Severity::Warn => warn_n += 1,
            Severity::Bad => bad_n += 1,
        }
    }

    let worst = [
        if bad_n > 0 {
            Some(Severity::Bad)
        } else if warn_n > 0 {
            Some(Severity::Warn)
        } else if !input.health_checks.is_empty() {
            Some(Severity::Ok)
        } else {
            None
        },
        input.diagnostics.polling.health_severity,
        input.timer.map(|t| t.severity),
        input.audio.map(|a| a.severity),
        input.network.map(|n| n.severity),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(Severity::Info);

    // --- HEADER (y: 18..62) ---
    c.draw_text_scaled(26, 20, "POLLED RIG SCORECARD", C_TEXT, 2);
    let win_ver = input.windows_version.unwrap_or("Windows");
    let sub = format!(
        "v{} · ALL-IN-ONE LATENCY & INPUT AUDIT · {}",
        env!("CARGO_PKG_VERSION"),
        win_ver
    );
    c.draw_text_clipped(26, 40, &sub, C_MUTED, 620);

    let (badge_txt, badge_fg, badge_bg) = match worst {
        Severity::Ok => ("COMPETITIVE READY", C_OK, C_OK_BG),
        Severity::Info => ("BASELINE CAPTURED", C_INFO, C_INFO_BG),
        Severity::Warn => ("ATTENTION NEEDED", C_WARN, C_WARN_BG),
        Severity::Bad => ("ISSUES DETECTED", C_BAD, C_BAD_BG),
    };
    let badge_w = text_width(badge_txt, 1) + 22;
    let badge_x = RIG_W - 26 - badge_w;
    c.fill_rect(badge_x, 20, badge_w, 24, badge_bg);
    c.stroke_rect(badge_x, 20, badge_w, 24, badge_fg);
    c.draw_text(badge_x + 11, 29, badge_txt, badge_fg);

    let ts = Local::now().format("%Y-%m-%d %H:%M").to_string();
    let ts_w = text_width(&ts, 1);
    c.draw_text(badge_x.saturating_sub(ts_w + 16), 29, &ts, C_MUTED);
    c.hline(26, RIG_W - 26, 60, C_BORDER);

    // Helper for bento panels.
    let draw_card = |c: &mut Canvas,
                     x: usize,
                     y: usize,
                     w: usize,
                     h: usize,
                     title: &str,
                     sev: Option<Severity>| {
        c.fill_rect(x, y, w, h, C_SURFACE);
        c.stroke_rect(x, y, w, h, C_BORDER);
        c.draw_text(x + 14, y + 14, title, C_MUTED);
        if let Some(s) = sev {
            let tag = s.as_str().to_uppercase();
            let tw = text_width(&tag, 1) + 12;
            let tx = x + w - 14 - tw;
            c.fill_rect(tx, y + 10, tw, 16, severity_bg_color(s));
            c.stroke_rect(tx, y + 10, tw, 16, severity_color(s));
            c.draw_text(tx + 6, y + 15, &tag, severity_color(s));
        }
        c.hline(x + 14, x + w - 14, y + 32, C_BORDER);
    };

    // --- ROW 1: HEALTH CHECK (left) + 4K/8K MOUSE POLLING & STALLS (right) ---
    let r1_y = 74;
    let r1_h = 248;
    let left_w = 480;
    let right_x = 26 + left_w + 16;
    let right_w = RIG_W - 26 - right_x;

    // 1. Windows Health Check Card
    let hc_sev = if bad_n > 0 {
        Some(Severity::Bad)
    } else if warn_n > 0 {
        Some(Severity::Warn)
    } else if !input.health_checks.is_empty() {
        Some(Severity::Ok)
    } else {
        None
    };
    draw_card(
        &mut c,
        26,
        r1_y,
        left_w,
        r1_h,
        "1. WINDOWS HEALTH AUDIT",
        hc_sev,
    );
    if input.health_checks.is_empty() {
        c.draw_text(40, r1_y + 52, "No health scan data yet.", C_DIM);
    } else {
        let score_txt = format!("{ok_n}/{} CHECKS CLEAN", input.health_checks.len());
        c.draw_text_scaled(40, r1_y + 46, &score_txt, C_TEXT, 2);
        let counts = format!("{ok_n} OK  |  {info_n} INFO  |  {warn_n} WARN  |  {bad_n} BAD");
        c.draw_text(40, r1_y + 72, &counts, C_MUTED);

        let issues: Vec<&CheckReport> = input
            .health_checks
            .iter()
            .filter(|ch| ch.severity >= Severity::Warn)
            .take(5)
            .collect();
        if issues.is_empty() {
            c.draw_text(
                40,
                r1_y + 102,
                "All audited mouse, USB, power, audio & NIC",
                C_OK,
            );
            c.draw_text(40, r1_y + 118, "settings passed with zero warnings.", C_OK);
        } else {
            c.draw_text(40, r1_y + 98, "ACTIVE WARNINGS:", C_MUTED);
            for (idx, issue) in issues.iter().enumerate() {
                let y = r1_y + 116 + idx * 22;
                let col = severity_color(issue.severity);
                c.fill_rect(40, y + 1, 5, 6, col);
                let line = format!("{} - {}", issue.name, issue.current_value);
                c.draw_text_clipped(52, y, &line, C_TEXT, left_w - 40);
            }
        }
    }

    // 2. Mouse Polling & CPU/OS Stall Correlation Card
    let p = &input.diagnostics.polling;
    let sys = &input.diagnostics.system_load;
    draw_card(
        &mut c,
        right_x,
        r1_y,
        right_w,
        r1_h,
        "2. MOUSE POLLING & OS STALL BENCH",
        p.health_severity,
    );
    if p.total_motion_packets == 0 {
        c.draw_text(
            right_x + 14,
            r1_y + 52,
            "Run Bench Capture to record 1K-8K mouse stability.",
            C_DIM,
        );
    } else {
        let rate_big = format!("{:.0} HZ", p.effective_hz);
        c.draw_text_scaled(right_x + 14, r1_y + 46, &rate_big, C_TEXT, 2);
        let meta = format!(
            "TARGET {} HZ  |  PEAK {:.0} HZ  |  {} DPI",
            p.nominal_hz, p.peak_hz, input.configured_dpi
        );
        c.draw_text_clipped(right_x + 14, r1_y + 72, &meta, C_MUTED, right_w - 28);

        let lines = [
            format!(
                "INTERVALS   med {:.3}ms | p95 {:.3}ms | p99 {:.3}ms | sd {:.3}ms",
                p.median_us / 1000.0,
                p.p95_us / 1000.0,
                p.p99_us / 1000.0,
                p.std_dev_us / 1000.0
            ),
            format!(
                "STABILITY   sat {:.0}% | sat +/-15% {:.1}% | dropouts {} | bunched {}",
                p.saturated_pct, p.saturated_within_15_pct, p.true_dropout_count, p.bunch_count
            ),
            format!(
                "TRACKING    peak {:.1} IPS ({:.1} G) | click faults {}",
                input
                    .diagnostics
                    .tracking
                    .peak_ips(f64::from(input.configured_dpi.max(1))),
                input
                    .diagnostics
                    .tracking
                    .peak_accel_g(f64::from(input.configured_dpi.max(1))),
                input.diagnostics.clicks.total_bounce_faults
            ),
            format!(
                "OS STALLS   avg CPU {:.0}% (peak {:.0}%, kernel {:.0}%) | stalls {}",
                sys.avg_cpu_pct, sys.peak_cpu_pct, sys.peak_kernel_pct, sys.scheduler_stalls
            ),
            format!(
                "CAUSE SPLIT {} OS-linked vs {} wireless/USB-only dropouts",
                sys.os_correlated_dropouts, sys.transport_only_dropouts
            ),
        ];
        for (i, ln) in lines.iter().enumerate() {
            c.draw_text_clipped(
                right_x + 14,
                r1_y + 102 + i * 24,
                ln,
                if i == 4 && sys.os_correlated_dropouts > 0 {
                    C_WARN
                } else {
                    C_TEXT
                },
                right_w - 28,
            );
        }
    }

    // --- ROW 2: USB TOPOLOGY (left) + AUDIO & TIMER (mid) + DUAL-HOP NETWORK (right) ---
    let r2_y = r1_y + r1_h + 16;
    let r2_h = RIG_H - r2_y - 40;
    let col_gap = 14;
    let col_w = (RIG_W - 52 - col_gap * 2) / 3;
    let c1_x = 26;
    let c2_x = c1_x + col_w + col_gap;
    let c3_x = c2_x + col_w + col_gap;

    // 3. USB PnP Topology
    let usb_sev = if input.usb_topology.is_empty() {
        None
    } else if input
        .usb_topology
        .iter()
        .any(|u| u.behind_external_hub || !u.shared_heavy_devices.is_empty())
    {
        Some(Severity::Warn)
    } else {
        Some(Severity::Ok)
    };
    draw_card(
        &mut c,
        c1_x,
        r2_y,
        col_w,
        r2_h,
        "3. USB PNP TOPOLOGY",
        usb_sev,
    );
    if input.usb_topology.is_empty() {
        c.draw_text(c1_x + 14, r2_y + 52, "No USB mouse topology loaded.", C_DIM);
    } else {
        for (idx, dev) in input.usb_topology.iter().take(2).enumerate() {
            let y = r2_y + 46 + idx * 84;
            c.draw_text_clipped(c1_x + 14, y, &dev.name, C_TEXT, col_w - 28);
            let port_line = match (dev.port_number, dev.hub_number) {
                (Some(port), Some(hub)) => format!("Port #{port}, Hub #{hub}"),
                _ => "Direct USB".to_string(),
            };
            c.draw_text_clipped(c1_x + 14, y + 16, &port_line, C_MUTED, col_w - 28);
            c.draw_text_clipped(c1_x + 14, y + 32, &dev.hub_name, C_MUTED, col_w - 28);
            let status = if dev.behind_external_hub {
                ("Behind external hub", C_WARN)
            } else if !dev.shared_heavy_devices.is_empty() {
                ("Shared with heavy USB device", C_WARN)
            } else {
                ("Direct & uncontested", C_OK)
            };
            c.draw_text_clipped(c1_x + 14, y + 48, status.0, status.1, col_w - 28);
        }
    }

    // 4. Audio & OS Timer
    let aud_timer_sev = match (
        input.audio.map(|a| a.severity),
        input.timer.map(|t| t.severity),
    ) {
        (Some(a), Some(t)) => Some(a.max(t)),
        (a, t) => a.or(t),
    };
    draw_card(
        &mut c,
        c2_x,
        r2_y,
        col_w,
        r2_h,
        "4. WASAPI AUDIO & TIMER",
        aud_timer_sev,
    );
    if let Some(a) = input.audio {
        c.draw_text_clipped(c2_x + 14, r2_y + 46, &a.device_name, C_TEXT, col_w - 28);
        let fmt_line = format!(
            "{} Hz | {:.2}ms period | {} glitches",
            a.sample_rate_hz, a.default_period_ms, a.glitch_count
        );
        c.draw_text_clipped(c2_x + 14, r2_y + 64, &fmt_line, C_MUTED, col_w - 28);
        let wake_line = format!("Wake med {:.2}ms | p99 {:.2}ms", a.median_ms, a.p99_ms);
        c.draw_text_clipped(
            c2_x + 14,
            r2_y + 82,
            &wake_line,
            severity_color(a.severity),
            col_w - 28,
        );
    } else {
        c.draw_text(c2_x + 14, r2_y + 52, "Audio bench not run yet.", C_DIM);
    }
    c.hline(c2_x + 14, c2_x + col_w - 14, r2_y + 114, C_BORDER);
    if let Some(t) = input.timer {
        let t1 = format!(
            "OS Timer 1s: med {:.2}ms | p99 {:.2}ms",
            t.median_ms, t.p99_ms
        );
        let t2 = format!(
            "Max {:.2}ms | spikes >3ms: {}",
            t.max_ms, t.spike_count_over_3ms
        );
        c.draw_text_clipped(c2_x + 14, r2_y + 130, &t1, C_TEXT, col_w - 28);
        c.draw_text_clipped(
            c2_x + 14,
            r2_y + 148,
            &t2,
            severity_color(t.severity),
            col_w - 28,
        );
    } else {
        c.draw_text(
            c2_x + 14,
            r2_y + 132,
            "OS Timer 1s test not run yet.",
            C_DIM,
        );
    }

    // 5. Dual-Hop Network Ping
    draw_card(
        &mut c,
        c3_x,
        r2_y,
        col_w,
        r2_h,
        "5. DUAL-HOP NETWORK PING",
        input.network.map(|n| n.severity),
    );
    if let Some(net) = input.network {
        let link = format!("{} - {}", net.adapter_kind.label(), net.adapter_name);
        c.draw_text_clipped(c3_x + 14, r2_y + 46, &link, C_TEXT, col_w - 28);
        if let Some(gw) = &net.gateway {
            let gw_line = format!(
                "GW {}: {:.2}ms (jit {:.2}ms)",
                gw.target_ip,
                gw.median_ms.unwrap_or(0.0),
                gw.jitter_ms.unwrap_or(0.0)
            );
            c.draw_text_clipped(c3_x + 14, r2_y + 68, &gw_line, C_INFO, col_w - 28);
        }
        let inet_line = format!(
            "NET {}: {:.1}ms | p95 {:.1}ms",
            net.internet.target_ip,
            net.internet.median_ms.unwrap_or(0.0),
            net.internet.p95_ms.unwrap_or(0.0)
        );
        let loss_line = format!(
            "Jitter {:.1}ms | Loss {:.0}% ({}/{})",
            net.internet.jitter_ms.unwrap_or(0.0),
            net.internet.loss_pct,
            net.internet.lost,
            net.internet.sent
        );
        c.draw_text_clipped(c3_x + 14, r2_y + 90, &inet_line, C_TEXT, col_w - 28);
        c.draw_text_clipped(
            c3_x + 14,
            r2_y + 110,
            &loss_line,
            severity_color(net.severity),
            col_w - 28,
        );
        c.draw_text_clipped(c3_x + 14, r2_y + 140, &net.diagnosis, C_MUTED, col_w - 28);
    } else {
        c.draw_text(
            c3_x + 14,
            r2_y + 52,
            "Run 5s Dual-Hop Ping in Bench.",
            C_DIM,
        );
    }

    // --- FOOTER ---
    c.draw_text(
        26,
        RIG_H - 22,
        "GENERATED LOCALLY BY POLLED · OPEN-SOURCE AGPL-3.0 · ZERO TELEMETRY",
        C_DIM,
    );

    let palette = if dark_mode {
        &DARK_PALETTE
    } else {
        &LIGHT_PALETTE
    };
    encode_indexed_4bit_png(&c.pixels, RIG_W, RIG_H, palette)
}

fn severity_color(s: Severity) -> u8 {
    match s {
        Severity::Ok => C_OK,
        Severity::Info => C_INFO,
        Severity::Warn => C_WARN,
        Severity::Bad => C_BAD,
    }
}

fn severity_bg_color(s: Severity) -> u8 {
    match s {
        Severity::Ok => C_OK_BG,
        Severity::Info => C_INFO_BG,
        Severity::Warn => C_WARN_BG,
        Severity::Bad => C_BAD_BG,
    }
}

struct Canvas {
    w: usize,
    h: usize,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(w: usize, h: usize, bg: u8) -> Self {
        Self {
            w,
            h,
            pixels: vec![bg; w * h],
        }
    }

    #[inline]
    fn set_px(&mut self, x: usize, y: usize, col: u8) {
        if x < self.w && y < self.h {
            self.pixels[y * self.w + x] = col & 0x0F;
        }
    }

    fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, col: u8) {
        let x1 = (x + w).min(self.w);
        let y1 = (y + h).min(self.h);
        for py in y.min(self.h)..y1 {
            let row = py * self.w;
            for px in x.min(self.w)..x1 {
                self.pixels[row + px] = col & 0x0F;
            }
        }
    }

    fn stroke_rect(&mut self, x: usize, y: usize, w: usize, h: usize, col: u8) {
        if w == 0 || h == 0 {
            return;
        }
        self.hline(x, x + w, y, col);
        self.hline(x, x + w, y + h - 1, col);
        self.vline(x, y, y + h, col);
        self.vline(x + w - 1, y, y + h, col);
    }

    fn hline(&mut self, x0: usize, x1: usize, y: usize, col: u8) {
        if y >= self.h {
            return;
        }
        let end = x1.min(self.w);
        let row = y * self.w;
        for px in x0.min(self.w)..end {
            self.pixels[row + px] = col & 0x0F;
        }
    }

    fn vline(&mut self, x: usize, y0: usize, y1: usize, col: u8) {
        if x >= self.w {
            return;
        }
        let end = y1.min(self.h);
        for py in y0.min(self.h)..end {
            self.pixels[py * self.w + x] = col & 0x0F;
        }
    }

    fn hline_dashed(&mut self, x0: usize, x1: usize, y: usize, col: u8, dash: usize, gap: usize) {
        if y >= self.h {
            return;
        }
        let period = (dash + gap).max(1);
        for (i, px) in (x0.min(self.w)..x1.min(self.w)).enumerate() {
            if i % period < dash {
                self.pixels[y * self.w + px] = col & 0x0F;
            }
        }
    }

    fn vline_dashed(&mut self, x: usize, y0: usize, y1: usize, col: u8, dash: usize, gap: usize) {
        if x >= self.w {
            return;
        }
        let period = (dash + gap).max(1);
        for (i, py) in (y0.min(self.h)..y1.min(self.h)).enumerate() {
            if i % period < dash {
                self.pixels[py * self.w + x] = col & 0x0F;
            }
        }
    }

    fn draw_line(&mut self, mut x0: i32, mut y0: i32, x1: i32, y1: i32, col: u8) {
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;

        loop {
            if x0 >= 0 && y0 >= 0 {
                self.set_px(x0 as usize, y0 as usize, col);
            }
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }

    fn draw_text(&mut self, x: usize, y: usize, text: &str, col: u8) {
        self.draw_text_scaled(x, y, text, col, 1);
    }

    fn draw_text_clipped(&mut self, x: usize, y: usize, text: &str, col: u8, max_w: usize) {
        let max_chars = max_w / 6;
        if text.chars().count() <= max_chars {
            self.draw_text(x, y, text, col);
        } else if max_chars >= 3 {
            let mut clipped: String = text.chars().take(max_chars - 2).collect();
            clipped.push_str("..");
            self.draw_text(x, y, &clipped, col);
        }
    }

    fn draw_text_scaled(&mut self, x: usize, y: usize, text: &str, col: u8, scale: usize) {
        let mut cursor_x = x;
        let advance = 6 * scale;
        for ch in text.chars() {
            let glyph = glyph_5x7(ch);
            for (row_idx, &row_bits) in glyph.iter().enumerate() {
                for bit_idx in 0..5 {
                    if (row_bits & (1 << (4 - bit_idx))) != 0 {
                        self.fill_rect(
                            cursor_x + bit_idx * scale,
                            y + row_idx * scale,
                            scale,
                            scale,
                            col,
                        );
                    }
                }
            }
            cursor_x += advance;
        }
    }
}

fn text_width(text: &str, scale: usize) -> usize {
    let count = text.chars().count();
    if count == 0 {
        0
    } else {
        (count * 6 - 1) * scale
    }
}

/// Crisp 5×7 monochrome bitmap font covering ASCII + common punctuation (`·`, `±`, `—`, `→`).
fn glyph_5x7(ch: char) -> [u8; 7] {
    let ch = match ch {
        '·' | '•' => '\u{00B7}',
        '—' | '–' => '-',
        '±' => '~',
        'σ' => 's',
        other => other,
    };
    match ch {
        ' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04],
        '"' => [0x0A, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00],
        '#' => [0x0A, 0x1F, 0x0A, 0x0A, 0x1F, 0x0A, 0x00],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        '&' => [0x0C, 0x12, 0x14, 0x08, 0x15, 0x12, 0x0D],
        '\'' => [0x04, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '*' => [0x00, 0x04, 0x15, 0x0E, 0x15, 0x04, 0x00],
        '+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x08],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x06],
        '/' => [0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        ':' => [0x00, 0x06, 0x06, 0x00, 0x06, 0x06, 0x00],
        ';' => [0x00, 0x06, 0x06, 0x00, 0x06, 0x02, 0x04],
        '<' => [0x02, 0x04, 0x08, 0x10, 0x08, 0x04, 0x02],
        '=' => [0x00, 0x00, 0x1F, 0x00, 0x1F, 0x00, 0x00],
        '>' => [0x08, 0x04, 0x02, 0x01, 0x02, 0x04, 0x08],
        '?' => [0x0E, 0x11, 0x01, 0x06, 0x04, 0x00, 0x04],
        '@' => [0x0E, 0x11, 0x17, 0x15, 0x17, 0x10, 0x0E],
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '[' => [0x0E, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0E],
        ']' => [0x0E, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0E],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F],
        'a' => [0x00, 0x00, 0x0E, 0x01, 0x0F, 0x11, 0x0F],
        'b' => [0x10, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x1E],
        'c' => [0x00, 0x00, 0x0E, 0x10, 0x10, 0x11, 0x0E],
        'd' => [0x01, 0x01, 0x0F, 0x11, 0x11, 0x11, 0x0F],
        'e' => [0x00, 0x00, 0x0E, 0x11, 0x1F, 0x10, 0x0E],
        'f' => [0x06, 0x08, 0x1E, 0x08, 0x08, 0x08, 0x08],
        'g' => [0x00, 0x00, 0x0F, 0x11, 0x0F, 0x01, 0x0E],
        'h' => [0x10, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x11],
        'i' => [0x04, 0x00, 0x0C, 0x04, 0x04, 0x04, 0x0E],
        'j' => [0x02, 0x00, 0x06, 0x02, 0x02, 0x12, 0x0C],
        'k' => [0x10, 0x10, 0x12, 0x14, 0x18, 0x14, 0x12],
        'l' => [0x0C, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'm' => [0x00, 0x00, 0x1A, 0x15, 0x15, 0x11, 0x11],
        'n' => [0x00, 0x00, 0x1E, 0x11, 0x11, 0x11, 0x11],
        'o' => [0x00, 0x00, 0x0E, 0x11, 0x11, 0x11, 0x0E],
        'p' => [0x00, 0x00, 0x1E, 0x11, 0x1E, 0x10, 0x10],
        'q' => [0x00, 0x00, 0x0F, 0x11, 0x0F, 0x01, 0x01],
        'r' => [0x00, 0x00, 0x16, 0x19, 0x10, 0x10, 0x10],
        's' => [0x00, 0x00, 0x0F, 0x10, 0x0E, 0x01, 0x1E],
        't' => [0x08, 0x08, 0x1E, 0x08, 0x08, 0x09, 0x06],
        'u' => [0x00, 0x00, 0x11, 0x11, 0x11, 0x13, 0x0D],
        'v' => [0x00, 0x00, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'w' => [0x00, 0x00, 0x11, 0x11, 0x15, 0x15, 0x0A],
        'x' => [0x00, 0x00, 0x11, 0x0A, 0x04, 0x0A, 0x11],
        'y' => [0x00, 0x00, 0x11, 0x11, 0x0F, 0x01, 0x0E],
        'z' => [0x00, 0x00, 0x1F, 0x02, 0x04, 0x08, 0x1F],
        '|' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        '~' => [0x04, 0x0E, 0x04, 0x00, 0x0E, 0x00, 0x00],
        '\u{00B7}' => [0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00],
        _ => [0x0E, 0x11, 0x01, 0x06, 0x04, 0x00, 0x04],
    }
}

/// Encodes a 4-bit indexed PNG image using standard zlib uncompressed DEFLATE blocks.
fn encode_indexed_4bit_png(
    pixels: &[u8],
    width: usize,
    height: usize,
    palette: &[[u8; 3]; 16],
) -> Vec<u8> {
    let row_bytes = width.div_ceil(2);
    let mut raw = Vec::with_capacity(height * (1 + row_bytes));
    for y in 0..height {
        raw.push(0u8); // Filter type 0: None
        let row = &pixels[y * width..(y + 1) * width];
        let mut x = 0;
        while x < width {
            let hi = row[x] & 0x0F;
            let lo = if x + 1 < width { row[x + 1] & 0x0F } else { 0 };
            raw.push((hi << 4) | lo);
            x += 2;
        }
    }

    let zlib = zlib_stored(&raw);

    let mut out = Vec::with_capacity(128 + zlib.len());
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);

    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&(width as u32).to_be_bytes());
    ihdr[4..8].copy_from_slice(&(height as u32).to_be_bytes());
    ihdr[8] = 4; // bit depth = 4
    ihdr[9] = 3; // color type = 3 (indexed palette)
    ihdr[10] = 0; // compression = deflate
    ihdr[11] = 0; // filter = adaptive
    ihdr[12] = 0; // interlace = none
    write_png_chunk(&mut out, b"IHDR", &ihdr);

    let mut plte = Vec::with_capacity(16 * 3);
    for rgb in palette {
        plte.extend_from_slice(rgb);
    }
    write_png_chunk(&mut out, b"PLTE", &plte);
    write_png_chunk(&mut out, b"IDAT", &zlib);
    write_png_chunk(&mut out, b"IEND", &[]);
    out
}

fn write_png_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    let mut crc = crc32_update(0xFFFF_FFFF, chunk_type);
    crc = crc32_update(crc, data);
    out.extend_from_slice(&(!crc).to_be_bytes());
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    const MAX_BLOCK: usize = 65_535;
    let block_count = data.len().div_ceil(MAX_BLOCK).max(1);
    let mut out = Vec::with_capacity(2 + data.len() + block_count * 5 + 4);
    // CMF=0x78 (deflate, 32K window), FLG=0x01 (check bits so 0x7801 % 31 == 0)
    out.push(0x78);
    out.push(0x01);

    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
    } else {
        let mut offset = 0;
        while offset < data.len() {
            let end = (offset + MAX_BLOCK).min(data.len());
            let chunk = &data[offset..end];
            let is_final = end == data.len();
            let len = chunk.len() as u16;
            let nlen = !len;
            out.push(if is_final { 0x01 } else { 0x00 });
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&nlen.to_le_bytes());
            out.extend_from_slice(chunk);
            offset = end;
        }
    }

    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

fn adler32(bytes: &[u8]) -> u32 {
    const MOD_ADLER: u32 = 65_521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in bytes {
        a = (a + u32::from(byte)) % MOD_ADLER;
        b = (b + a) % MOD_ADLER;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{PollingAnalyzer, PollingTarget};

    fn sample_snapshot() -> DiagnosticsSnapshot {
        let mut analyzer = PollingAnalyzer::new(PollingTarget::FixedHz(4000));
        let mut t_ns = 1_000_000_000u64;
        for i in 0..80u64 {
            let dt = if i == 40 { 500_000 } else { 250_000 };
            t_ns += dt;
            analyzer.record_motion(t_ns, 4, if i % 2 == 0 { 1 } else { -1 });
        }
        DiagnosticsSnapshot {
            capturing: false,
            target: PollingTarget::FixedHz(4000),
            polling: analyzer.snapshot(Some(t_ns)),
            tracking: analyzer.tracking_snapshot(Some(t_ns)),
            dpi_measurement: Default::default(),
            clicks: Default::default(),
            keyboard: Default::default(),
            system_load: Default::default(),
            error: None,
        }
    }

    #[test]
    fn csv_export_contains_metadata_header_and_interval_rows() {
        let snap = sample_snapshot();
        let csv = format_intervals_csv(&snap, 1600);
        assert!(csv.contains("# Polled Bench Raw Motion Intervals"));
        assert!(csv.contains("index,dt_us,inst_hz,dx,dy,speed_ips,saturated"));
        let data_rows: Vec<&str> = csv
            .lines()
            .filter(|l| !l.starts_with('#') && !l.starts_with("index,"))
            .collect();
        assert_eq!(data_rows.len(), snap.polling.recent_samples.len());
        assert!(data_rows[0].split(',').count() == 7);
    }

    #[test]
    fn png_export_writes_valid_signature_chunks_dimensions_and_crcs() {
        let snap = sample_snapshot();
        let bytes = render_report_png(&snap, 1600, None, None, true);
        assert_eq!(&bytes[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);

        // Walk every PNG chunk and verify CRC-32 + presence of IHDR, PLTE, IDAT, IEND.
        let mut offset = 8;
        let mut seen = Vec::new();
        while offset < bytes.len() {
            let len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            let chunk_type = &bytes[offset + 4..offset + 8];
            let data = &bytes[offset + 8..offset + 8 + len];
            let stored_crc = u32::from_be_bytes(
                bytes[offset + 8 + len..offset + 12 + len]
                    .try_into()
                    .unwrap(),
            );
            let mut calc = crc32_update(0xFFFF_FFFF, chunk_type);
            calc = crc32_update(calc, data);
            assert_eq!(
                stored_crc,
                !calc,
                "CRC mismatch for chunk {:?}",
                std::str::from_utf8(chunk_type)
            );
            if chunk_type == b"IHDR" {
                let w = u32::from_be_bytes(data[0..4].try_into().unwrap());
                let h = u32::from_be_bytes(data[4..8].try_into().unwrap());
                assert_eq!(w, IMG_W as u32);
                assert_eq!(h, IMG_H as u32);
                assert_eq!(data[8], 4);
                assert_eq!(data[9], 3);
            }
            seen.push(std::str::from_utf8(chunk_type).unwrap().to_string());
            offset += 12 + len;
        }
        assert_eq!(offset, bytes.len());
        assert_eq!(seen, vec!["IHDR", "PLTE", "IDAT", "IEND"]);
    }

    #[test]
    fn ab_comparison_png_renders_valid_960x560_png() {
        let snap_a = sample_snapshot();
        let snap_b = sample_snapshot();
        let base = PinnedBenchmark {
            label: "Front USB 4K".to_string(),
            configured_dpi: 1600,
            snapshot: snap_a,
            timer: None,
        };
        let bytes = render_ab_comparison_png(&base, &snap_b, "Rear CPU USB 4K", 1600, false);
        assert_eq!(&bytes[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        assert!(bytes.len() > 1000);
    }

    #[test]
    fn rig_scorecard_png_renders_valid_1040x620_png() {
        let snap = sample_snapshot();
        let input = RigScorecardInput {
            windows_version: Some("11 24H2 (26100)"),
            health_checks: &[],
            diagnostics: &snap,
            configured_dpi: 1600,
            usb_topology: &[],
            timer: None,
            audio: None,
            network: None,
        };
        let bytes = render_rig_scorecard_png(&input, true);
        assert_eq!(&bytes[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let h = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        assert_eq!(w, RIG_W as u32);
        assert_eq!(h, RIG_H as u32);
    }
}
