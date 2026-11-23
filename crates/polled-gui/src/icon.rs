// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

//! Procedurally renders Polled's crisp monochrome black-and-white crosshair icon as RGBA bytes
//! (zero external image-decoding dependencies).

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
