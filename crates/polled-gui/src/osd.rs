// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pure-Win32 monochrome On-Screen Switch Pill (OSD).
//!
//! - Uses a native Win32 layered topmost popup window (`WS_EX_TOPMOST | WS_EX_TOOLWINDOW |
//!   WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED` + `SW_SHOWNOACTIVATE`).
//! - Zero DirectX/Vulkan hooking, zero DLL injection, never steals keyboard/mouse focus,
//!   100% click-through, hidden from Alt+Tab and taskbar, and 0% CPU when idle (`SW_HIDE`).

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const PILL_WIDTH: i32 = 360;
const PILL_HEIGHT: i32 = 56;
const PILL_TOP_MARGIN: i32 = 28;
const HOLD_DURATION: Duration = Duration::from_millis(1_850);
const FADE_DURATION: Duration = Duration::from_millis(400);
const MAX_ALPHA: u8 = 238;

#[derive(Debug, Clone)]
struct OsdPayload {
    title: String,
    subtitle: String,
    dark_mode: bool,
    shown_at: Instant,
}

/// Thread-safe handle to the background monochrome Win32 OSD pill window.
pub struct OsdController {
    payload: Arc<Mutex<Option<OsdPayload>>>,
    hwnd: Arc<AtomicIsize>,
    handle: Option<JoinHandle<()>>,
}

impl OsdController {
    pub fn start() -> Self {
        let payload = Arc::new(Mutex::new(None));
        let hwnd = Arc::new(AtomicIsize::new(0));

        let payload_bg = Arc::clone(&payload);
        let hwnd_bg = Arc::clone(&hwnd);

        let handle = std::thread::Builder::new()
            .name("polled-osd".to_string())
            .spawn(move || {
                #[cfg(windows)]
                run_osd_loop(payload_bg, hwnd_bg);
                #[cfg(not(windows))]
                let _ = (payload_bg, hwnd_bg);
            })
            .ok();

        Self {
            payload,
            hwnd,
            handle,
        }
    }

    /// Displays the click-through monochrome OSD pill for ~2.2 seconds without stealing focus.
    pub fn show(&self, title: impl Into<String>, subtitle: impl Into<String>, dark_mode: bool) {
        if let Ok(mut guard) = self.payload.lock() {
            *guard = Some(OsdPayload {
                title: title.into(),
                subtitle: subtitle.into(),
                dark_mode,
                shown_at: Instant::now(),
            });
        }
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

            let raw = self.hwnd.load(Ordering::SeqCst);
            if raw != 0 {
                let h = HWND(raw as *mut core::ffi::c_void);
                // SAFETY: Posting custom `WM_APP + 1` to our own OSD window thread.
                unsafe {
                    let _ = PostMessageW(Some(h), WM_APP + 1, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
}

impl Drop for OsdController {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

            let raw = self.hwnd.load(Ordering::SeqCst);
            if raw != 0 {
                let h = HWND(raw as *mut core::ffi::c_void);
                // SAFETY: Asking our own OSD window thread to destroy and exit cleanly.
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
fn run_osd_loop(payload: Arc<Mutex<Option<OsdPayload>>>, hwnd_slot: Arc<AtomicIsize>) {
    use std::cell::RefCell;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateFontW, CreatePen, CreateRoundRectRgn, CreateSolidBrush, DeleteObject,
        DrawTextW, EndPaint, InvalidateRect, RoundRect, SelectObject, SetBkMode, SetTextColor,
        SetWindowRgn, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH,
        DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER, FF_SWISS, FW_NORMAL, FW_SEMIBOLD,
        OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        GetSystemMetrics, KillTimer, PostQuitMessage, RegisterClassW, SetLayeredWindowAttributes,
        SetTimer, SetWindowPos, ShowWindow, TranslateMessage, UnregisterClassW, HWND_TOPMOST,
        LWA_ALPHA, MSG, SM_CXSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE,
        WM_APP, WM_CLOSE, WM_DESTROY, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    };

    const WM_OSD_SHOW: u32 = WM_APP + 1;
    const TIMER_FADE_ID: usize = 1;

    thread_local! {
        static SHARED_PAYLOAD: RefCell<Option<Arc<Mutex<Option<OsdPayload>>>>> = const { RefCell::new(None) };
    }

    fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
        COLORREF(u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16))
    }

    fn to_wide_vec(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_OSD_SHOW => {
                let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(PILL_WIDTH + 32);
                let x = (screen_w - PILL_WIDTH) / 2;
                let y = PILL_TOP_MARGIN;
                unsafe {
                    let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), MAX_ALPHA, LWA_ALPHA);
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        x,
                        y,
                        PILL_WIDTH,
                        PILL_HEIGHT,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    let _ = InvalidateRect(Some(hwnd), None, true);
                    let _ = SetTimer(Some(hwnd), TIMER_FADE_ID, 33, None);
                }
                LRESULT(0)
            }
            WM_TIMER => {
                let shown_at = SHARED_PAYLOAD.with(|cell| {
                    cell.borrow().as_ref().and_then(|arc| {
                        arc.lock().ok().and_then(|g| g.as_ref().map(|p| p.shown_at))
                    })
                });
                let Some(start) = shown_at else {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_FADE_ID);
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                    return LRESULT(0);
                };
                let elapsed = start.elapsed();
                if elapsed >= HOLD_DURATION + FADE_DURATION {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_FADE_ID);
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                } else if elapsed > HOLD_DURATION {
                    let fade_elapsed = elapsed.saturating_sub(HOLD_DURATION).as_secs_f32();
                    let frac = (1.0 - fade_elapsed / FADE_DURATION.as_secs_f32()).clamp(0.0, 1.0);
                    let alpha = ((f32::from(MAX_ALPHA)) * frac).round() as u8;
                    unsafe {
                        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
                    }
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let snap = SHARED_PAYLOAD.with(|cell| {
                    cell.borrow()
                        .as_ref()
                        .and_then(|arc| arc.lock().ok().and_then(|g| g.clone()))
                });

                let mut ps = PAINTSTRUCT::default();
                let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
                if let Some(data) = snap {
                    let (bg_col, border_col, accent_col, title_col, sub_col) = if data.dark_mode {
                        (
                            rgb(17, 17, 20),
                            rgb(63, 63, 70),
                            rgb(244, 244, 245),
                            rgb(250, 250, 250),
                            rgb(161, 161, 170),
                        )
                    } else {
                        (
                            rgb(255, 255, 255),
                            rgb(212, 212, 216),
                            rgb(24, 24, 27),
                            rgb(17, 17, 19),
                            rgb(82, 82, 91),
                        )
                    };

                    unsafe {
                        let bg_brush = CreateSolidBrush(bg_col);
                        let border_pen = CreatePen(PS_SOLID, 1, border_col);
                        let old_brush = SelectObject(hdc, bg_brush.into());
                        let old_pen = SelectObject(hdc, border_pen.into());
                        let _ = RoundRect(hdc, 0, 0, PILL_WIDTH, PILL_HEIGHT, 14, 14);
                        SelectObject(hdc, old_brush);
                        SelectObject(hdc, old_pen);
                        let _ = DeleteObject(bg_brush.into());
                        let _ = DeleteObject(border_pen.into());

                        // Minimal monochrome left indicator bar.
                        let accent_brush = CreateSolidBrush(accent_col);
                        let accent_pen = CreatePen(PS_SOLID, 1, accent_col);
                        let ob = SelectObject(hdc, accent_brush.into());
                        let op = SelectObject(hdc, accent_pen.into());
                        let _ = RoundRect(hdc, 12, 13, 15, PILL_HEIGHT - 13, 3, 3);
                        SelectObject(hdc, ob);
                        SelectObject(hdc, op);
                        let _ = DeleteObject(accent_brush.into());
                        let _ = DeleteObject(accent_pen.into());

                        let _ = SetBkMode(hdc, TRANSPARENT);
                        let font_face = to_wide_vec("Segoe UI");
                        let title_font = CreateFontW(
                            -15,
                            0,
                            0,
                            0,
                            FW_SEMIBOLD.0 as i32,
                            0,
                            0,
                            0,
                            DEFAULT_CHARSET,
                            OUT_DEFAULT_PRECIS,
                            CLIP_DEFAULT_PRECIS,
                            CLEARTYPE_QUALITY,
                            (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
                            PCWSTR(font_face.as_ptr()),
                        );
                        let sub_font = CreateFontW(
                            -12,
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
                            CLEARTYPE_QUALITY,
                            (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
                            PCWSTR(font_face.as_ptr()),
                        );

                        let old_font = SelectObject(hdc, title_font.into());
                        let _ = SetTextColor(hdc, title_col);
                        let mut title_w: Vec<u16> = data.title.encode_utf16().collect();
                        let mut title_rect = RECT {
                            left: 26,
                            top: 8,
                            right: PILL_WIDTH - 16,
                            bottom: 30,
                        };
                        let _ = DrawTextW(
                            hdc,
                            &mut title_w,
                            &mut title_rect,
                            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                        );

                        SelectObject(hdc, sub_font.into());
                        let _ = SetTextColor(hdc, sub_col);
                        let mut sub_w: Vec<u16> = data.subtitle.encode_utf16().collect();
                        let mut sub_rect = RECT {
                            left: 26,
                            top: 30,
                            right: PILL_WIDTH - 16,
                            bottom: PILL_HEIGHT - 8,
                        };
                        let _ = DrawTextW(
                            hdc,
                            &mut sub_w,
                            &mut sub_rect,
                            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                        );

                        SelectObject(hdc, old_font);
                        let _ = DeleteObject(title_font.into());
                        let _ = DeleteObject(sub_font.into());
                    }
                }
                let _ = unsafe { EndPaint(hwnd, &ps) };
                LRESULT(0)
            }
            WM_CLOSE => {
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_FADE_ID);
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

    SHARED_PAYLOAD.with(|cell| {
        *cell.borrow_mut() = Some(payload);
    });

    let class_str = format!(
        "PolledOsdPill_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let class_w = to_wide_vec(&class_str);
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

    let ex_style =
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED;

    let hwnd = match unsafe {
        CreateWindowExW(
            ex_style,
            PCWSTR(class_w.as_ptr()),
            PCWSTR::null(),
            WS_POPUP,
            0,
            PILL_TOP_MARGIN,
            PILL_WIDTH,
            PILL_HEIGHT,
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
        let rgn = CreateRoundRectRgn(0, 0, PILL_WIDTH + 1, PILL_HEIGHT + 1, 14, 14);
        if !rgn.is_invalid() {
            // Ownership of `rgn` transfers to the window when `SetWindowRgn` succeeds.
            let _ = SetWindowRgn(hwnd, Some(rgn), false);
        }
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), MAX_ALPHA, LWA_ALPHA);
        let _ = ShowWindow(hwnd, SW_HIDE);
    }

    hwnd_slot.store(hwnd.0 as isize, Ordering::SeqCst);

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
    SHARED_PAYLOAD.with(|cell| {
        cell.borrow_mut().take();
    });
}
