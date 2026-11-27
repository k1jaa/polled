// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pure-Win32 transparent Windows 10/11 Taskbar Battery Widget.
//!
//! Renders a crisp anti-aliased vector mouse silhouette and live battery percentage (`93%`)
//! directly on the Windows taskbar immediately to the left of `TrayNotifyWnd` (`^` chevron):
//! - Uses `UpdateLayeredWindow` (`ULW_ALPHA`) with premultiplied 32-bit BGRA pixels so the
//!   background is completely transparent and blends seamlessly with both dark and light
//!   Windows 10/11 taskbars (including Mica / acrylic transparency).
//! - Automatically scales to the taskbar's live height (100%–200%+ DPI), adapts colors to
//!   `SystemUsesLightTheme`, highlights low battery (amber) and charging (emerald + bolt),
//!   auto-hides whenever a fullscreen app/game covers the monitor or the taskbar auto-hides,
//!   opens Polled on left-click, and consumes **0% CPU** when disabled (`SW_HIDE` + timer killed).
//! - 100% anti-cheat safe: pure user-mode Win32 APIs, zero DLL injection, zero Explorer hooks.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use polled_core::{BatteryStatus, SupportTier};

use crate::profile_runtime::ProfileRuntimeSnapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskbarWidgetState {
    pub enabled: bool,
    pub battery: Option<BatteryStatus>,
    pub low_battery_threshold: u8,
}

impl Default for TaskbarWidgetState {
    fn default() -> Self {
        Self {
            enabled: false,
            battery: None,
            low_battery_threshold: 15,
        }
    }
}

/// Extracts the primary mouse's battery status from a runtime snapshot (preferring physical
/// hardware over the dry-run simulator).
pub fn primary_mouse_battery(snap: &ProfileRuntimeSnapshot) -> Option<BatteryStatus> {
    snap.active_devices
        .iter()
        .find(|d| d.info.support_tier != SupportTier::Simulator && d.battery.is_some())
        .or_else(|| snap.active_devices.iter().find(|d| d.battery.is_some()))
        .and_then(|d| d.battery)
}

/// Computes the exact foreground RGBA tint for the taskbar widget icon & percentage label.
pub(crate) fn widget_foreground_rgb(
    battery: Option<BatteryStatus>,
    low_threshold: u8,
    light_taskbar: bool,
) -> (u8, u8, u8) {
    match battery {
        Some(b) if b.charging => {
            if light_taskbar {
                (5, 150, 105) // deep emerald on light taskbar
            } else {
                (52, 211, 153) // bright emerald on dark taskbar
            }
        }
        Some(b) if b.percent <= low_threshold => {
            if light_taskbar {
                (180, 83, 9) // rich amber on light taskbar
            } else {
                (251, 191, 36) // warm amber on dark taskbar
            }
        }
        Some(_) => {
            if light_taskbar {
                (28, 28, 32) // crisp charcoal on light taskbar
            } else {
                (234, 234, 238) // subtle Windows 11 taskbar off-white
            }
        }
        None => {
            if light_taskbar {
                (113, 113, 122)
            } else {
                (161, 161, 170)
            }
        }
    }
}

/// Formats the short text label displayed beside the vector mouse icon.
pub(crate) fn widget_label_text(battery: Option<BatteryStatus>) -> String {
    match battery {
        Some(b) => format!("{}%", b.percent.min(100)),
        None => "--%".to_string(),
    }
}

/// Blends a non-premultiplied RGBA pixel `(r, g, b, alpha_f32)` over a premultiplied BGRA pixel.
#[inline]
pub(crate) fn blend_over_premul_bgra(
    dst: &mut [u8],
    offset: usize,
    r: u8,
    g: u8,
    b: u8,
    alpha: f32,
) {
    let a = alpha.clamp(0.0, 1.0);
    if a <= 0.001 || offset + 3 >= dst.len() {
        return;
    }
    let inv = 1.0 - a;
    let dst_b = f32::from(dst[offset]);
    let dst_g = f32::from(dst[offset + 1]);
    let dst_r = f32::from(dst[offset + 2]);
    let dst_a = f32::from(dst[offset + 3]);

    let out_b = (f32::from(b) * a + dst_b * inv).clamp(0.0, 255.0);
    let out_g = (f32::from(g) * a + dst_g * inv).clamp(0.0, 255.0);
    let out_r = (f32::from(r) * a + dst_r * inv).clamp(0.0, 255.0);
    let out_a = (255.0 * a + dst_a * inv).clamp(0.0, 255.0);

    dst[offset] = out_b.round() as u8;
    dst[offset + 1] = out_g.round() as u8;
    dst[offset + 2] = out_r.round() as u8;
    dst[offset + 3] = out_a.round() as u8;
}

/// Signed distance to an axis-aligned rounded box centered at `(cx, cy)` with half-extents `(hx, hy)`
/// and corner radius `radius`. Negative inside, zero on boundary, positive outside.
#[inline]
fn sd_rounded_box(px: f32, py: f32, cx: f32, cy: f32, hx: f32, hy: f32, radius: f32) -> f32 {
    let qx = (px - cx).abs() - (hx - radius);
    let qy = (py - cy).abs() - (hy - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let inside = qx.max(qy).min(0.0);
    outside + inside - radius
}

/// Signed distance from point `(px, py)` to line segment `(ax, ay) -> (bx, by)`.
#[inline]
fn sd_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let pax = px - ax;
    let pay = py - ay;
    let bax = bx - ax;
    let bay = by - ay;
    let denom = bax * bax + bay * bay;
    let h = if denom > 1e-6 {
        ((pax * bax + pay * bay) / denom).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let dx = pax - bax * h;
    let dy = pay - bay * h;
    (dx * dx + dy * dy).sqrt()
}

/// Rasterizes a subtle rounded pill hover backdrop (or invisible 1-alpha click hitbox) into a
/// top-down premultiplied 32-bit BGRA buffer.
pub(crate) fn rasterize_pill_backdrop(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    hovered: bool,
    light_taskbar: bool,
    radius: f32,
) {
    let cx = width as f32 * 0.5;
    let cy = height as f32 * 0.5;
    let hx = (width as f32 * 0.5 - 1.0).max(1.0);
    let hy = (height as f32 * 0.5 - 1.0).max(1.0);
    let (bg_r, bg_g, bg_b, bg_a) = if hovered {
        if light_taskbar {
            (0u8, 0u8, 0u8, 0.08_f32)
        } else {
            (255u8, 255u8, 255u8, 0.09_f32)
        }
    } else {
        // Alpha = 1/255 (~0.4%, invisible) ensures the whole pill receives WM_LBUTTONUP clicks.
        (0u8, 0u8, 0u8, 1.0 / 255.0)
    };

    for y in 0..height {
        let py = y as f32 + 0.5;
        for x in 0..width {
            let px = x as f32 + 0.5;
            let d = sd_rounded_box(px, py, cx, cy, hx, hy, radius);
            if d < 0.5 {
                let cov = (0.5 - d).clamp(0.0, 1.0);
                let off = (y * width + x) * 4;
                if hovered {
                    blend_over_premul_bgra(pixels, off, bg_r, bg_g, bg_b, bg_a * cov);
                } else if cov > 0.5 && off + 3 < pixels.len() {
                    pixels[off] = 0;
                    pixels[off + 1] = 0;
                    pixels[off + 2] = 0;
                    pixels[off + 3] = 1;
                }
            }
        }
    }
}

/// Rasterizes a subtle, crisp Fluent-style vector mouse silhouette (slender rounded capsule +
/// single center scroll-wheel tick, plus a tiny charging bolt when plugged in) into a top-down
/// premultiplied 32-bit BGRA buffer.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rasterize_mouse_icon(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    center_x: f32,
    center_y: f32,
    icon_h: f32,
    rgb: (u8, u8, u8),
    charging: bool,
) {
    let icon_w = icon_h * 0.64;
    let hx = icon_w * 0.5;
    let hy = icon_h * 0.5;
    let corner_r = (hx * 0.94).min(hy * 0.58);
    // Subtle ~1.15px stroke at 100% scale, scaling smoothly with DPI to match Explorer's `^` chevron.
    let stroke_half = (icon_h * 0.043).clamp(0.48, 1.25);

    // Minimalist single scroll-wheel tick in the upper center (no heavy horizontal cross-bar).
    let wheel_top = center_y - hy * 0.62;
    let wheel_bot = center_y - hy * 0.14;
    let wheel_half = (stroke_half * 1.05).max(0.55);

    let min_x = ((center_x - hx - stroke_half - 2.0).floor().max(0.0)) as usize;
    let max_x = ((center_x + hx + stroke_half + 2.0).ceil() as usize).min(width);
    let min_y = ((center_y - hy - stroke_half - 2.0).floor().max(0.0)) as usize;
    let max_y = ((center_y + hy + stroke_half + 2.0).ceil() as usize).min(height);

    for y in min_y..max_y {
        let py = y as f32 + 0.5;
        for x in min_x..max_x {
            let px = x as f32 + 0.5;

            let body_sd = sd_rounded_box(px, py, center_x, center_y, hx, hy, corner_r);
            let outline_sd = body_sd.abs() - stroke_half;

            let mut min_sd = outline_sd;
            if body_sd < 0.5 {
                let wheel_sd =
                    sd_segment(px, py, center_x, wheel_top, center_x, wheel_bot) - wheel_half;
                min_sd = min_sd.min(wheel_sd);

                if charging {
                    // Compact lightning bolt in the lower mouse body.
                    let bx = center_x;
                    let by = center_y + hy * 0.34;
                    let bh = hy * 0.34;
                    let bw = hx * 0.34;
                    let s1 = sd_segment(
                        px,
                        py,
                        bx + bw * 0.25,
                        by - bh,
                        bx - bw * 0.45,
                        by + bh * 0.08,
                    );
                    let s2 = sd_segment(
                        px,
                        py,
                        bx - bw * 0.45,
                        by + bh * 0.08,
                        bx + bw * 0.45,
                        by - bh * 0.08,
                    );
                    let s3 = sd_segment(
                        px,
                        py,
                        bx + bw * 0.45,
                        by - bh * 0.08,
                        bx - bw * 0.25,
                        by + bh,
                    );
                    let bolt_sd = s1.min(s2).min(s3) - (stroke_half * 0.85).max(0.5);
                    min_sd = min_sd.min(bolt_sd);
                }
            }

            let cov = (0.5 - min_sd).clamp(0.0, 1.0);
            if cov > 0.001 {
                let off = (y * width + x) * 4;
                blend_over_premul_bgra(pixels, off, rgb.0, rgb.1, rgb.2, cov);
            }
        }
    }
}

/// Thread-safe handle to the opt-in pure-Win32 Windows taskbar battery widget.
pub struct TaskbarWidgetController {
    state: Arc<Mutex<TaskbarWidgetState>>,
    hwnd: Arc<AtomicIsize>,
    handle: Option<JoinHandle<()>>,
}

impl TaskbarWidgetController {
    pub fn start(
        initial_enabled: bool,
        initial_low_threshold: u8,
        on_click: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let state = Arc::new(Mutex::new(TaskbarWidgetState {
            enabled: initial_enabled,
            battery: None,
            low_battery_threshold: initial_low_threshold,
        }));
        let hwnd = Arc::new(AtomicIsize::new(0));

        let state_bg = Arc::clone(&state);
        let hwnd_bg = Arc::clone(&hwnd);

        let handle = std::thread::Builder::new()
            .name("polled-taskbar-widget".to_string())
            .spawn(move || {
                #[cfg(windows)]
                run_taskbar_widget_loop(state_bg, hwnd_bg, on_click);
                #[cfg(not(windows))]
                let _ = (state_bg, hwnd_bg, on_click);
            })
            .ok();

        Self {
            state,
            hwnd,
            handle,
        }
    }

    /// Updates enabled state, battery reading, and low-battery threshold, waking the Win32 widget
    /// thread only when the visible state actually changes.
    pub fn update(&self, enabled: bool, battery: Option<BatteryStatus>, low_threshold: u8) {
        let next = TaskbarWidgetState {
            enabled,
            battery,
            low_battery_threshold: low_threshold,
        };
        let changed = if let Ok(mut guard) = self.state.lock() {
            if *guard == next {
                false
            } else {
                *guard = next;
                true
            }
        } else {
            false
        };
        if changed {
            self.post_refresh();
        }
    }

    /// Convenience adapter syncing directly from a `ProfileRuntimeSnapshot`.
    pub fn sync_from_snapshot(
        &self,
        snap: &ProfileRuntimeSnapshot,
        enabled: bool,
        low_threshold: u8,
    ) {
        self.update(enabled, primary_mouse_battery(snap), low_threshold);
    }

    fn post_refresh(&self) {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

            let raw = self.hwnd.load(Ordering::SeqCst);
            if raw != 0 {
                let h = HWND(raw as *mut core::ffi::c_void);
                // SAFETY: Posting custom `WM_APP + 2` to our own taskbar widget window thread.
                unsafe {
                    let _ = PostMessageW(Some(h), WM_APP + 2, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
}

impl Drop for TaskbarWidgetController {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

            let raw = self.hwnd.load(Ordering::SeqCst);
            if raw != 0 {
                let h = HWND(raw as *mut core::ffi::c_void);
                // SAFETY: Asking our own taskbar widget thread to destroy and exit cleanly.
                unsafe {
                    let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(windows)]
fn run_taskbar_widget_loop(
    shared_state: Arc<Mutex<TaskbarWidgetState>>,
    hwnd_slot: Arc<AtomicIsize>,
    on_click: Arc<dyn Fn() + Send + Sync>,
) {
    use std::cell::RefCell;
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    use polled_core::win::{RegHive, SystemEnv, WindowsEnv};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW,
        GetDC, GetMonitorInfoW, MonitorFromWindow, ReleaseDC, SelectObject, SetBkColor, SetBkMode,
        SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, BLENDFUNCTION, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS,
        DT_CALCRECT, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FF_SWISS, FW_NORMAL,
        MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowExW,
        FindWindowW, GetForegroundWindow, GetMessageW, GetWindow, GetWindowRect, IsWindowVisible,
        KillTimer, LoadCursorW, PostQuitMessage, RegisterClassW, SetCursor, SetTimer, SetWindowPos,
        ShowWindow, TranslateMessage, UnregisterClassW, UpdateLayeredWindow, GW_HWNDPREV,
        HWND_TOPMOST, IDC_HAND, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
        SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WM_APP, WM_CLOSE, WM_DESTROY, WM_LBUTTONUP,
        WM_MOUSEMOVE, WM_SETCURSOR, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };

    const WM_WIDGET_REFRESH: u32 = WM_APP + 2;
    const WM_MOUSELEAVE: u32 = 0x02A3;
    const TIMER_SYNC_ID: usize = 1;
    const TIMER_SYNC_MS: u32 = 1_000;

    fn to_wide_null(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    fn is_light_taskbar() -> bool {
        WindowsEnv
            .reg_get_dword(
                RegHive::Hkcu,
                r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "SystemUsesLightTheme",
            )
            .ok()
            .flatten()
            == Some(1)
    }

    fn is_fullscreen_covering_monitor(taskbar_hwnd: HWND, own_hwnd: HWND, mon: &RECT) -> bool {
        let fg = unsafe { GetForegroundWindow() };
        if fg.is_invalid() || fg == taskbar_hwnd || fg == own_hwnd {
            return false;
        }
        // Exclude Windows desktop shell windows (`Progman` / `WorkerW`).
        let progman = to_wide_null("Progman");
        if let Ok(pm) = unsafe { FindWindowW(PCWSTR(progman.as_ptr()), PCWSTR::null()) } {
            if fg == pm {
                return false;
            }
        }
        let mut fg_rect = RECT::default();
        if unsafe { GetWindowRect(fg, &mut fg_rect) }.is_err() {
            return false;
        }
        fg_rect.left <= mon.left
            && fg_rect.top <= mon.top
            && fg_rect.right >= mon.right
            && fg_rect.bottom >= mon.bottom
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct RenderKey {
        label_hash: u64,
        rgb: (u8, u8, u8),
        charging: bool,
        hovered: bool,
        light_taskbar: bool,
        width: i32,
        height: i32,
    }

    struct LocalWidget {
        shared: Arc<Mutex<TaskbarWidgetState>>,
        on_click: Arc<dyn Fn() + Send + Sync>,
        timer_active: bool,
        visible: bool,
        hovered: bool,
        tracking_leave: bool,
        last_rect: Option<(i32, i32, i32, i32)>,
        last_render: Option<RenderKey>,
    }

    thread_local! {
        static LOCAL: RefCell<Option<LocalWidget>> = const { RefCell::new(None) };
    }

    fn hash_str(s: &str) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in s.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h
    }

    fn measure_label_px(label: &str, font_px: i32) -> i32 {
        let screen_dc = unsafe { GetDC(None) };
        if screen_dc.is_invalid() {
            return (label.len() as i32) * (font_px / 2).max(7);
        }
        let mem_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
        let face = to_wide_null("Segoe UI");
        let font = unsafe {
            CreateFontW(
                -font_px,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
                PCWSTR(face.as_ptr()),
            )
        };
        let old = unsafe { SelectObject(mem_dc, font.into()) };
        let mut wide: Vec<u16> = label.encode_utf16().collect();
        let mut rc = RECT::default();
        unsafe {
            let _ = DrawTextW(
                mem_dc,
                &mut wide,
                &mut rc,
                DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
            );
            SelectObject(mem_dc, old);
            let _ = DeleteObject(font.into());
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(None, screen_dc);
        }
        (rc.right - rc.left).max(16)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_and_present(
        hwnd: HWND,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        scale: f32,
        label: &str,
        rgb: (u8, u8, u8),
        charging: bool,
        hovered: bool,
        light_taskbar: bool,
    ) {
        let w = width.max(1) as usize;
        let h = height.max(1) as usize;
        let screen_dc = unsafe { GetDC(None) };
        if screen_dc.is_invalid() {
            return;
        }
        let mem_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
        let text_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
        if mem_dc.is_invalid() || text_dc.is_invalid() {
            unsafe {
                if !mem_dc.is_invalid() {
                    let _ = DeleteDC(mem_dc);
                }
                if !text_dc.is_invalid() {
                    let _ = DeleteDC(text_dc);
                }
                let _ = ReleaseDC(None, screen_dc);
            }
            return;
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down DIB
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut dst_bits: *mut c_void = ptr::null_mut();
        let mut txt_bits: *mut c_void = ptr::null_mut();
        let dst_bmp = unsafe {
            CreateDIBSection(
                Some(screen_dc),
                &bmi,
                DIB_RGB_COLORS,
                &mut dst_bits,
                None,
                0,
            )
        };
        let txt_bmp = unsafe {
            CreateDIBSection(
                Some(screen_dc),
                &bmi,
                DIB_RGB_COLORS,
                &mut txt_bits,
                None,
                0,
            )
        };

        if let (Ok(dst_bmp), Ok(txt_bmp)) = (dst_bmp, txt_bmp) {
            if !dst_bits.is_null() && !txt_bits.is_null() {
                let byte_len = w * h * 4;
                // SAFETY: Both DIB sections have exact size `w * h * 4` bytes and remain alive
                // until `DeleteObject` below.
                let dst_slice =
                    unsafe { std::slice::from_raw_parts_mut(dst_bits as *mut u8, byte_len) };
                let txt_slice =
                    unsafe { std::slice::from_raw_parts_mut(txt_bits as *mut u8, byte_len) };
                dst_slice.fill(0);
                txt_slice.fill(0);

                // 1. Pill hover highlight (or invisible alpha=1 click hitbox).
                let pill_r = (5.0 * scale).clamp(4.0, 10.0);
                rasterize_pill_backdrop(dst_slice, w, h, hovered, light_taskbar, pill_r);

                // 2. Subtle Fluent-style anti-aliased vector mouse icon on the left side.
                let pad_x = (6.0 * scale).round();
                let icon_h = (13.0 * scale).clamp(10.0, 30.0);
                let icon_w = icon_h * 0.64;
                let icon_cx = pad_x + icon_w * 0.5;
                let icon_cy = h as f32 * 0.5;
                rasterize_mouse_icon(dst_slice, w, h, icon_cx, icon_cy, icon_h, rgb, charging);

                // 3. Render anti-aliased white text onto black scratch DIB, then composite using
                //    exact luminance as per-pixel alpha so GDI never leaves alpha=0 holes.
                let old_txt_bmp = unsafe { SelectObject(text_dc, txt_bmp.into()) };
                let font_px = (12.0 * scale).round().clamp(10.0, 28.0) as i32;
                let face = to_wide_null("Segoe UI");
                let font = unsafe {
                    CreateFontW(
                        -font_px,
                        0,
                        0,
                        0,
                        FW_NORMAL.0 as i32,
                        0,
                        0,
                        0,
                        DEFAULT_CHARSET,
                        OUT_DEFAULT_PRECIS,
                        CLIP_DEFAULT_PRECIS,
                        ANTIALIASED_QUALITY,
                        (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
                        PCWSTR(face.as_ptr()),
                    )
                };
                let old_font = unsafe { SelectObject(text_dc, font.into()) };
                let text_left = (pad_x + icon_w + 4.5 * scale).round() as i32;
                let optical_nudge = -(1.0 * scale).round() as i32;
                let mut text_rc = RECT {
                    left: text_left,
                    top: optical_nudge,
                    right: width - (4.0 * scale).round() as i32,
                    bottom: height + optical_nudge,
                };
                let mut wide: Vec<u16> = label.encode_utf16().collect();
                unsafe {
                    let _ = SetBkMode(text_dc, TRANSPARENT);
                    let _ = SetBkColor(text_dc, COLORREF(0));
                    let _ = SetTextColor(text_dc, COLORREF(0x00FF_FFFF));
                    let _ = DrawTextW(
                        text_dc,
                        &mut wide,
                        &mut text_rc,
                        DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
                    );
                    SelectObject(text_dc, old_font);
                    let _ = DeleteObject(font.into());
                    SelectObject(text_dc, old_txt_bmp);
                }

                for off in (0..byte_len).step_by(4) {
                    // White-on-black antialiased text: R == G == B == glyph coverage 0..=255.
                    let cov_u8 = txt_slice[off + 2];
                    if cov_u8 > 0 {
                        blend_over_premul_bgra(
                            dst_slice,
                            off,
                            rgb.0,
                            rgb.1,
                            rgb.2,
                            f32::from(cov_u8) / 255.0,
                        );
                    }
                }

                // 4. Present premultiplied BGRA bitmap atomically via UpdateLayeredWindow.
                let old_dst_bmp = unsafe { SelectObject(mem_dc, dst_bmp.into()) };
                let dst_pt = POINT { x, y };
                let src_pt = POINT { x: 0, y: 0 };
                let sz = SIZE {
                    cx: width,
                    cy: height,
                };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                unsafe {
                    let _ = UpdateLayeredWindow(
                        hwnd,
                        Some(screen_dc),
                        Some(&dst_pt),
                        Some(&sz),
                        Some(mem_dc),
                        Some(&src_pt),
                        COLORREF(0),
                        Some(&blend),
                        ULW_ALPHA,
                    );
                    SelectObject(mem_dc, old_dst_bmp);
                }
            }
            unsafe {
                let _ = DeleteObject(dst_bmp.into());
                let _ = DeleteObject(txt_bmp.into());
            }
        }

        unsafe {
            let _ = DeleteDC(text_dc);
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(None, screen_dc);
        }
    }

    fn hide_widget(hwnd: HWND, local: &mut LocalWidget, stop_timer: bool) {
        if local.visible {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            local.visible = false;
            local.hovered = false;
            local.tracking_leave = false;
            local.last_rect = None;
        }
        if stop_timer && local.timer_active {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_SYNC_ID);
            }
            local.timer_active = false;
        }
    }

    fn sync_widget(hwnd: HWND, force_redraw: bool) {
        LOCAL.with(|cell| {
            let Ok(mut opt) = cell.try_borrow_mut() else {
                return;
            };
            let Some(local) = opt.as_mut() else {
                return;
            };
            let st = local.shared.lock().map(|g| g.clone()).unwrap_or_default();

            if !st.enabled {
                hide_widget(hwnd, local, true);
                return;
            }

            if !local.timer_active {
                unsafe {
                    let _ = SetTimer(Some(hwnd), TIMER_SYNC_ID, TIMER_SYNC_MS, None);
                }
                local.timer_active = true;
            }

            let shell_cls = to_wide_null("Shell_TrayWnd");
            let Ok(taskbar_hwnd) =
                (unsafe { FindWindowW(PCWSTR(shell_cls.as_ptr()), PCWSTR::null()) })
            else {
                hide_widget(hwnd, local, false);
                return;
            };
            if taskbar_hwnd.is_invalid() || !unsafe { IsWindowVisible(taskbar_hwnd) }.as_bool() {
                hide_widget(hwnd, local, false);
                return;
            }

            let tray_cls = to_wide_null("TrayNotifyWnd");
            let Ok(tray_hwnd) = (unsafe {
                FindWindowExW(
                    Some(taskbar_hwnd),
                    None,
                    PCWSTR(tray_cls.as_ptr()),
                    PCWSTR::null(),
                )
            }) else {
                hide_widget(hwnd, local, false);
                return;
            };
            if tray_hwnd.is_invalid() {
                hide_widget(hwnd, local, false);
                return;
            }

            let mut bar_rect = RECT::default();
            let mut tray_rect = RECT::default();
            if unsafe { GetWindowRect(taskbar_hwnd, &mut bar_rect) }.is_err()
                || unsafe { GetWindowRect(tray_hwnd, &mut tray_rect) }.is_err()
            {
                hide_widget(hwnd, local, false);
                return;
            }

            let bar_w = bar_rect.right - bar_rect.left;
            let bar_h = bar_rect.bottom - bar_rect.top;
            // Only anchor on horizontal taskbars (bottom/top) that are not collapsed off-screen.
            if bar_w <= bar_h || bar_h < 24 {
                hide_widget(hwnd, local, false);
                return;
            }

            // Check monitor bounds: auto-hide if taskbar is auto-hidden off-screen or covered by a
            // fullscreen game/video.
            let hmon = unsafe { MonitorFromWindow(taskbar_hwnd, MONITOR_DEFAULTTONEAREST) };
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
                let mon = mi.rcMonitor;
                let visible_h = bar_rect.bottom.min(mon.bottom) - bar_rect.top.max(mon.top);
                if visible_h < 18 || is_fullscreen_covering_monitor(taskbar_hwnd, hwnd, &mon) {
                    hide_widget(hwnd, local, false);
                    return;
                }
            }

            let scale = (bar_h as f32 / 48.0).clamp(0.75, 2.5);
            let label = widget_label_text(st.battery);
            let font_px = (12.0 * scale).round().clamp(10.0, 28.0) as i32;
            let text_w = measure_label_px(&label, font_px);
            let pad_x = (6.0 * scale).round() as i32;
            let icon_w = ((13.0 * scale).clamp(10.0, 30.0) * 0.64).ceil() as i32;
            let gap_icon_text = (4.5 * scale).round() as i32;
            let widget_w = (pad_x + icon_w + gap_icon_text + text_w + pad_x).clamp(48, 200);
            let widget_h = (bar_h - (10.0 * scale).round() as i32).clamp(22, bar_h);

            let tray_gap = (2.0 * scale).round() as i32;
            let x = (tray_rect.left - widget_w - tray_gap).max(bar_rect.left + 8);
            let y = bar_rect.top + (bar_h - widget_h) / 2;

            let light_taskbar = is_light_taskbar();
            let rgb = widget_foreground_rgb(st.battery, st.low_battery_threshold, light_taskbar);
            let charging = st.battery.is_some_and(|b| b.charging);
            let next_render = RenderKey {
                label_hash: hash_str(&label),
                rgb,
                charging,
                hovered: local.hovered,
                light_taskbar,
                width: widget_w,
                height: widget_h,
            };
            let next_rect = (x, y, widget_w, widget_h);

            let need_redraw =
                force_redraw || !local.visible || local.last_render != Some(next_render);
            let need_reposition = !local.visible || local.last_rect != Some(next_rect);

            if need_redraw {
                render_and_present(
                    hwnd,
                    x,
                    y,
                    widget_w,
                    widget_h,
                    scale,
                    &label,
                    rgb,
                    charging,
                    local.hovered,
                    light_taskbar,
                );
                local.last_render = Some(next_render);
            }

            if need_reposition || need_redraw {
                unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        x,
                        y,
                        widget_w,
                        widget_h,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                    if !local.visible {
                        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    }
                }
                local.visible = true;
                local.last_rect = Some(next_rect);
            } else {
                // If Explorer raised the taskbar above our widget after a taskbar click, gently
                // re-assert topmost without redrawing or activating.
                if let Ok(prev) = unsafe { GetWindow(hwnd, GW_HWNDPREV) } {
                    if !prev.is_invalid() {
                        unsafe {
                            let _ = SetWindowPos(
                                hwnd,
                                Some(HWND_TOPMOST),
                                0,
                                0,
                                0,
                                0,
                                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                            );
                        }
                    }
                }
            }
        });
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_WIDGET_REFRESH => {
                sync_widget(hwnd, true);
                LRESULT(0)
            }
            WM_TIMER => {
                sync_widget(hwnd, false);
                LRESULT(0)
            }
            WM_SETCURSOR => {
                if let Ok(cursor) = unsafe { LoadCursorW(None, IDC_HAND) } {
                    unsafe {
                        let _ = SetCursor(Some(cursor));
                    }
                    return LRESULT(1);
                }
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_MOUSEMOVE => {
                let mut changed = false;
                LOCAL.with(|cell| {
                    if let Ok(mut opt) = cell.try_borrow_mut() {
                        if let Some(local) = opt.as_mut() {
                            if !local.hovered {
                                local.hovered = true;
                                changed = true;
                            }
                            if !local.tracking_leave {
                                let mut tme = TRACKMOUSEEVENT {
                                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                                    dwFlags: TME_LEAVE,
                                    hwndTrack: hwnd,
                                    dwHoverTime: 0,
                                };
                                if unsafe { TrackMouseEvent(&mut tme) }.is_ok() {
                                    local.tracking_leave = true;
                                }
                            }
                        }
                    }
                });
                if changed {
                    sync_widget(hwnd, true);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                let mut changed = false;
                LOCAL.with(|cell| {
                    if let Ok(mut opt) = cell.try_borrow_mut() {
                        if let Some(local) = opt.as_mut() {
                            local.tracking_leave = false;
                            if local.hovered {
                                local.hovered = false;
                                changed = true;
                            }
                        }
                    }
                });
                if changed {
                    sync_widget(hwnd, true);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let cb = LOCAL.with(|cell| {
                    cell.borrow()
                        .as_ref()
                        .map(|local| Arc::clone(&local.on_click))
                });
                if let Some(cb) = cb {
                    cb();
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_SYNC_ID);
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe {
                    PostQuitMessage(0);
                }
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    LOCAL.with(|cell| {
        *cell.borrow_mut() = Some(LocalWidget {
            shared: shared_state,
            on_click,
            timer_active: false,
            visible: false,
            hovered: false,
            tracking_leave: false,
            last_rect: None,
            last_render: None,
        });
    });

    let class_str = format!(
        "PolledTaskbarBattery_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let class_w = to_wide_null(&class_str);
    let hmodule = unsafe { GetModuleHandleW(PCWSTR::null()) }.unwrap_or_default();
    let hinstance = HINSTANCE(hmodule.0);

    let wc = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinstance,
        lpszClassName: PCWSTR(class_w.as_ptr()),
        ..Default::default()
    };
    if unsafe { RegisterClassW(&wc) } == 0 {
        return;
    }

    // Notice we intentionally omit WS_EX_TRANSPARENT so left-clicking opens Polled.
    let ex_style = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED;

    let hwnd = match unsafe {
        CreateWindowExW(
            ex_style,
            PCWSTR(class_w.as_ptr()),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            80,
            36,
            None,
            None,
            Some(hinstance),
            None,
        )
    } {
        Ok(h) if !h.is_invalid() => h,
        _ => {
            unsafe {
                let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
            }
            return;
        }
    };

    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    hwnd_slot.store(hwnd.0 as isize, Ordering::SeqCst);

    // Trigger initial sync (stays hidden & 0% CPU if disabled).
    sync_widget(hwnd, true);

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0).0 > 0 } {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    hwnd_slot.store(0, Ordering::SeqCst);
    unsafe {
        let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
    }
    LOCAL.with(|cell| {
        cell.borrow_mut().take();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_and_theme_color_selection() {
        assert_eq!(widget_label_text(None), "--%");
        assert_eq!(
            widget_label_text(Some(BatteryStatus {
                percent: 93,
                charging: false,
                voltage_mv: None,
            })),
            "93%"
        );
        assert_eq!(
            widget_label_text(Some(BatteryStatus {
                percent: 120,
                charging: true,
                voltage_mv: None,
            })),
            "100%"
        );

        let normal_dark = widget_foreground_rgb(
            Some(BatteryStatus {
                percent: 80,
                charging: false,
                voltage_mv: None,
            }),
            15,
            false,
        );
        let normal_light = widget_foreground_rgb(
            Some(BatteryStatus {
                percent: 80,
                charging: false,
                voltage_mv: None,
            }),
            15,
            true,
        );
        assert_ne!(normal_dark, normal_light);
        assert!(normal_dark.0 > 200);
        assert!(normal_light.0 < 40);

        let low_dark = widget_foreground_rgb(
            Some(BatteryStatus {
                percent: 12,
                charging: false,
                voltage_mv: None,
            }),
            15,
            false,
        );
        let chg_dark = widget_foreground_rgb(
            Some(BatteryStatus {
                percent: 12,
                charging: true,
                voltage_mv: None,
            }),
            15,
            false,
        );
        // Charging overrides low-battery warning amber with emerald green.
        assert!(chg_dark.1 > chg_dark.0);
        assert!(low_dark.0 > low_dark.2);
    }

    #[test]
    fn mouse_icon_and_hitbox_rasterization_produce_valid_premultiplied_bgra() {
        let w = 84usize;
        let h = 40usize;
        let mut buf = vec![0u8; w * h * 4];

        rasterize_pill_backdrop(&mut buf, w, h, false, false, 6.0);
        // Center pixel has alpha == 1 (click hitbox, invisible RGB = 0).
        let center_off = ((h / 2) * w + (w / 2)) * 4;
        assert_eq!(buf[center_off + 3], 1);
        assert_eq!(buf[center_off], 0);

        rasterize_mouse_icon(&mut buf, w, h, 18.0, 20.0, 18.0, (244, 244, 246), true);

        // Verify icon actually drew opaque foreground pixels and preserved premultiplied invariant
        // (`B, G, R <= A` for every pixel).
        let mut bright_pixels = 0usize;
        for px in buf.as_chunks::<4>().0 {
            let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
            assert!(
                b <= a && g <= a && r <= a,
                "non-premultiplied pixel {:?}",
                px
            );
            if a > 180 {
                bright_pixels += 1;
            }
        }
        assert!(bright_pixels > 40, "expected crisp mouse outline pixels");
    }
}
