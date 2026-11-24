// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Procedurally renders Polled's crisp monochrome crosshair icon and dynamic live wireless
//! battery percentage tray icons directly as RGBA bytes (zero external image dependencies).

use eframe::egui::IconData;
use tray_icon::Icon as TrayIconImage;

pub fn render_rgba(size: u32) -> Vec<u8> {
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let s = size as f32;
    let cx = (s - 1.0) / 2.0;
    let cy = (s - 1.0) / 2.0;
    let r_outer = s * 0.46;
    let r_ring_outer = s * 0.34;
    let r_ring_inner = s * 0.24;
    let r_dot = s * 0.11;
    let tick_half = (s * 0.038).max(1.0);

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let d = dx.hypot(dy);
            let idx = ((y * size + x) * 4) as usize;

            let (r, g, b, a) = if d <= r_outer {
                let mut color = (10u8, 10u8, 12u8, 255u8); // near-black badge
                if d >= r_outer - (s * 0.045).max(1.2) {
                    color = (72, 72, 78, 255); // neutral rim
                }
                let on_ring = d <= r_ring_outer && d >= r_ring_inner;
                let on_crosshair = (dx.abs() <= tick_half
                    && dy.abs() >= r_dot * 1.45
                    && dy.abs() <= r_ring_outer * 1.15)
                    || (dy.abs() <= tick_half
                        && dx.abs() >= r_dot * 1.45
                        && dx.abs() <= r_ring_outer * 1.15);
                if on_ring || on_crosshair || d <= r_dot {
                    color = (248, 248, 250, 255); // crisp white crosshair
                }
                color
            } else {
                (0, 0, 0, 0)
            };

            rgba[idx] = r;
            rgba[idx + 1] = g;
            rgba[idx + 2] = b;
            rgba[idx + 3] = a;
        }
    }

    rgba
}

/// 5x7 crisp pixel glyphs for digits `0..=9` (MSB = leftmost column of 5 bits).
const DIGITS_5X7: [[u8; 7]; 10] = [
    [
        0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
    ], // 0
    [
        0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
    ], // 1
    [
        0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111,
    ], // 2
    [
        0b01110, 0b10001, 0b00001, 0b00110, 0b00001, 0b10001, 0b01110,
    ], // 3
    [
        0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
    ], // 4
    [
        0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
    ], // 5
    [
        0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
    ], // 6
    [
        0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
    ], // 7
    [
        0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
    ], // 8
    [
        0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
    ], // 9
];

/// Compact 3x7 / 4x7 glyphs used exclusively for `"100"` so 3 digits fit cleanly at 2x scale in 32x32.
const DIGIT_1_COMPACT: [u8; 7] = [0b010, 0b110, 0b010, 0b010, 0b010, 0b010, 0b111];
const DIGIT_0_COMPACT: [u8; 7] = [0b0110, 0b1001, 0b1001, 0b1001, 0b1001, 0b1001, 0b0110];

fn set_pixel(rgba: &mut [u8], size: u32, x: i32, y: i32, color: (u8, u8, u8, u8)) {
    if x < 0 || y < 0 || x >= size as i32 || y >= size as i32 {
        return;
    }
    let idx = (((y as u32) * size + (x as u32)) * 4) as usize;
    rgba[idx] = color.0;
    rgba[idx + 1] = color.1;
    rgba[idx + 2] = color.2;
    rgba[idx + 3] = color.3;
}

fn fill_rect(
    rgba: &mut [u8],
    size: u32,
    x0: i32,
    y0: i32,
    w: i32,
    h: i32,
    color: (u8, u8, u8, u8),
) {
    for y in y0..(y0 + h) {
        for x in x0..(x0 + w) {
            set_pixel(rgba, size, x, y, color);
        }
    }
}

fn draw_glyph_scaled(
    rgba: &mut [u8],
    size: u32,
    glyph: &[u8; 7],
    cols: i32,
    origin: (i32, i32),
    scale: i32,
    color: (u8, u8, u8, u8),
) {
    let (origin_x, origin_y) = origin;
    for (row_idx, &bits) in glyph.iter().enumerate() {
        for col in 0..cols {
            let mask = 1u8 << (cols - 1 - col);
            if (bits & mask) != 0 {
                fill_rect(
                    rgba,
                    size,
                    origin_x + col * scale,
                    origin_y + (row_idx as i32) * scale,
                    scale,
                    scale,
                    color,
                );
            }
        }
    }
}

/// Procedurally renders a 32x32 monochrome battery tray icon displaying the exact wireless
/// percentage (`0..=100`) and a bottom battery charge bar with semantic status accent.
pub fn render_battery_rgba(percent: u8, charging: bool, low_threshold: u8) -> Vec<u8> {
    let size: u32 = 32;
    let pct = percent.min(100);
    let low = low_threshold.clamp(5, 50);
    let mut rgba = vec![0u8; (size * size * 4) as usize];

    let bg = (10u8, 10u8, 12u8, 255u8);
    let fg = (248u8, 248u8, 250u8, 255u8);
    let rim_neutral = (78u8, 78u8, 86u8, 255u8);
    let ok_green = (34u8, 197u8, 94u8, 255u8);
    let warn_amber = (245u8, 158u8, 11u8, 255u8);
    let bad_red = (239u8, 68u8, 68u8, 255u8);

    let (bar_color, rim_color) = if charging {
        (ok_green, ok_green)
    } else if pct <= 5 {
        (bad_red, bad_red)
    } else if pct <= low {
        (warn_amber, warn_amber)
    } else {
        (fg, rim_neutral)
    };

    // 1. Rounded square dark badge (radius 4 px) with 1px semantic/neutral rim.
    let s = size as i32;
    for y in 0..s {
        for x in 0..s {
            let dx = if x < 4 {
                4 - x
            } else if x >= s - 4 {
                x - (s - 5)
            } else {
                0
            };
            let dy = if y < 4 {
                4 - y
            } else if y >= s - 4 {
                y - (s - 5)
            } else {
                0
            };
            let corner_d2 = dx * dx + dy * dy;
            if corner_d2 > 20 {
                continue;
            }
            let on_border = x == 0 || y == 0 || x == s - 1 || y == s - 1 || corner_d2 >= 13;
            set_pixel(
                &mut rgba,
                size,
                x,
                y,
                if on_border { rim_color } else { bg },
            );
        }
    }

    // 2. Bold 2x-scaled percentage digits in upper half (`y = 4..18`).
    let text_color = if !charging && pct <= 5 {
        bad_red
    } else if !charging && pct <= low {
        warn_amber
    } else {
        fg
    };

    if pct >= 100 {
        // "100" using compact 3x7 / 4x7 glyphs at 2x scale: widths 6 + 1 + 8 + 1 + 8 = 24px -> x = 4.
        draw_glyph_scaled(&mut rgba, size, &DIGIT_1_COMPACT, 3, (4, 4), 2, text_color);
        draw_glyph_scaled(&mut rgba, size, &DIGIT_0_COMPACT, 4, (11, 4), 2, text_color);
        draw_glyph_scaled(&mut rgba, size, &DIGIT_0_COMPACT, 4, (20, 4), 2, text_color);
    } else if pct >= 10 {
        let tens = (pct / 10) as usize;
        let ones = (pct % 10) as usize;
        // Two 5x7 glyphs at 2x scale: 10 + 2 + 10 = 22px wide -> centered at x = 5.
        draw_glyph_scaled(&mut rgba, size, &DIGITS_5X7[tens], 5, (5, 4), 2, text_color);
        draw_glyph_scaled(
            &mut rgba,
            size,
            &DIGITS_5X7[ones],
            5,
            (17, 4),
            2,
            text_color,
        );
    } else {
        let ones = pct as usize;
        // Single 5x7 glyph at 2x scale: 10px wide -> centered at x = 11.
        draw_glyph_scaled(
            &mut rgba,
            size,
            &DIGITS_5X7[ones],
            5,
            (11, 4),
            2,
            text_color,
        );
    }

    // 3. Bottom horizontal battery gauge (`x = 4..26`, `y = 21..28`) + terminal nub (`x = 26..28`).
    let box_outline = if charging { ok_green } else { rim_neutral };
    fill_rect(&mut rgba, size, 4, 21, 22, 1, box_outline); // top
    fill_rect(&mut rgba, size, 4, 27, 22, 1, box_outline); // bottom
    fill_rect(&mut rgba, size, 4, 21, 1, 7, box_outline); // left
    fill_rect(&mut rgba, size, 25, 21, 1, 7, box_outline); // right
    fill_rect(&mut rgba, size, 26, 23, 2, 3, box_outline); // battery positive terminal

    // Inner fill bar (`x = 6..24`, width = 18 px, height = 3 px).
    if pct > 0 {
        let fill_w = (((u32::from(pct) * 18) + 50) / 100).clamp(1, 18) as i32;
        fill_rect(&mut rgba, size, 6, 23, fill_w, 3, bar_color);
    }

    rgba
}

pub fn window_icon() -> IconData {
    let size = 64;
    IconData {
        rgba: render_rgba(size),
        width: size,
        height: size,
    }
}

pub fn tray_icon_image() -> Option<TrayIconImage> {
    let size = 32;
    TrayIconImage::from_rgba(render_rgba(size), size, size).ok()
}

pub fn tray_battery_icon_image(
    percent: u8,
    charging: bool,
    low_threshold: u8,
) -> Option<TrayIconImage> {
    TrayIconImage::from_rgba(
        render_battery_rgba(percent, charging, low_threshold),
        32,
        32,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_distinct_valid_32x32_battery_icons() {
        let full = render_battery_rgba(100, false, 15);
        let mid = render_battery_rgba(68, false, 15);
        let low = render_battery_rgba(12, false, 15);
        let crit = render_battery_rgba(4, false, 15);
        let chg = render_battery_rgba(68, true, 15);

        for buf in [&full, &mid, &low, &crit, &chg] {
            assert_eq!(buf.len(), 32 * 32 * 4);
            // Corner pixel (0,0) is transparent rounded cutout.
            assert_eq!(buf[3], 0);
        }
        assert_ne!(full, mid);
        assert_ne!(mid, chg);
        assert_ne!(low, crit);
    }
}
