// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use polled_core::win::WindowsEnv;
use polled_core::{AppPaths, AppSettings, HealthEngine};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::app::WorkerMsg;
use crate::icon::tray_icon_image;
use crate::notifications::notify_if_regressed;
use crate::profile_runtime::ProfileRuntimeCmd;

const ID_OPEN: &str = "open";
const ID_SCAN: &str = "scan";
const ID_AUTO_SWITCH: &str = "auto_switch";
const ID_QUIT: &str = "quit";

/// Finds Polled's own top-level `HWND` on the current Win32 UI thread (including when hidden).
#[cfg(windows)]
pub(crate) fn find_own_main_hwnd() -> isize {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumThreadWindows, GetWindowLongW, GetWindowTextW, GWL_STYLE, WS_CAPTION,
    };

    struct Search {
        exact_title: isize,
        fallback_captioned: isize,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut buf = [0u16; 64];
        // SAFETY: `hwnd` is supplied by `EnumThreadWindows`; `buf` is valid UTF-16 storage.
        let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
        if len > 0 {
            let title = String::from_utf16_lossy(&buf[..len as usize]);
            if title == "Polled" {
                search.exact_title = hwnd.0 as isize;
                return BOOL(0);
            }
        }
        if search.fallback_captioned == 0 {
            // SAFETY: Querying window style on our own UI thread's window is read-only.
            let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) };
            if (style & WS_CAPTION.0 as i32) != 0 {
                search.fallback_captioned = hwnd.0 as isize;
            }
        }
        BOOL(1)
    }

    let mut search = Search {
        exact_title: 0,
        fallback_captioned: 0,
    };
    // SAFETY: `GetCurrentThreadId` has no preconditions; `search` lives across the synchronous call.
    unsafe {
        let _ = EnumThreadWindows(
            GetCurrentThreadId(),
            Some(enum_proc),
            LPARAM((&mut search as *mut Search) as isize),
        );
    }
    if search.exact_title != 0 {
        search.exact_title
    } else {
        search.fallback_captioned
    }
}

#[cfg(not(windows))]
pub(crate) fn find_own_main_hwnd() -> isize {
    0
}

/// Holds the active Windows system tray icon and dynamic menu items alive for the lifetime of the app.
pub struct TrayController {
    tray: Option<TrayIcon>,
    auto_switch_item: CheckMenuItem,
}

impl TrayController {
    /// Creates the system tray icon (must be called on the main Win32 UI thread) and
    /// starts a tiny listener thread that wakes the `egui` window on tray interactions.
    pub fn init(
        ctx: egui::Context,
        tx: Sender<WorkerMsg>,
        profile_cmd_tx: Sender<ProfileRuntimeCmd>,
        main_hwnd: Arc<AtomicIsize>,
        auto_switch_state: Arc<AtomicBool>,
        quit_requested: Arc<AtomicBool>,
    ) -> Self {
        let initial_auto_switch = auto_switch_state.load(Ordering::SeqCst);
        let menu = Menu::new();
        let open_item = MenuItem::with_id(ID_OPEN, "Open Polled", true, None);
        let scan_item = MenuItem::with_id(ID_SCAN, "Scan Now", true, None);
        let sep_top = PredefinedMenuItem::separator();
        let auto_switch_item = CheckMenuItem::with_id(
            ID_AUTO_SWITCH,
            "Auto-Switch Profiles",
            true,
            initial_auto_switch,
            None,
        );
        let sep_bottom = PredefinedMenuItem::separator();
        let quit_item = MenuItem::with_id(ID_QUIT, "Quit", true, None);
        let _ = menu.append(&open_item);
        let _ = menu.append(&scan_item);
        let _ = menu.append(&sep_top);
        let _ = menu.append(&auto_switch_item);
        let _ = menu.append(&sep_bottom);
        let _ = menu.append(&quit_item);

        let initial_tooltip = if initial_auto_switch {
            "Polled — Auto-Switch ON"
        } else {
            "Polled — Auto-Switch OFF"
        };

        let mut builder = TrayIconBuilder::new()
            .with_tooltip(initial_tooltip)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false);

        if let Some(icon) = tray_icon_image() {
            builder = builder.with_icon(icon);
        }

        let tray = builder.build().ok();

        if tray.is_some() {
            std::thread::spawn(move || {
                let tray_rx = TrayIconEvent::receiver();
                let menu_rx = MenuEvent::receiver();

                loop {
                    if quit_requested.load(Ordering::SeqCst) {
                        break;
                    }

                    while let Ok(event) = tray_rx.try_recv() {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            show_and_focus(&main_hwnd, &ctx);
                        }
                    }

                    while let Ok(event) = menu_rx.try_recv() {
                        match event.id.0.as_str() {
                            ID_OPEN => show_and_focus(&main_hwnd, &ctx),
                            ID_SCAN => {
                                // Run directly on a background thread so tray scans work even
                                // while the main window is hidden in the tray.
                                let tx_scan = tx.clone();
                                let ctx_scan = ctx.clone();
                                std::thread::spawn(move || {
                                    let paths = AppPaths::default_user_dir();
                                    let engine =
                                        HealthEngine::with_default_and_custom_checks(&paths);
                                    let result = engine
                                        .scan_all(&WindowsEnv, &paths, true)
                                        .map_err(|e| e.to_string());
                                    if let Ok(summary) = &result {
                                        notify_if_regressed(summary);
                                    }
                                    let _ = tx_scan.send(WorkerMsg::ScanFinished {
                                        result,
                                        notify_regressions: false,
                                    });
                                    ctx_scan.request_repaint();
                                });
                            }
                            ID_AUTO_SWITCH => {
                                // Toggle runtime directly so it works even while window is hidden.
                                let next = !auto_switch_state.load(Ordering::SeqCst);
                                auto_switch_state.store(next, Ordering::SeqCst);
                                let paths = AppPaths::default_user_dir();
                                let mut saved =
                                    AppSettings::load_or_default(&paths.settings_path());
                                saved.auto_switch_on_startup = next;
                                let _ = saved.save_atomic(&paths.settings_path());
                                let _ = profile_cmd_tx.send(ProfileRuntimeCmd::SetAutoSwitch(next));
                                let _ = tx.send(WorkerMsg::TraySyncAutoSwitch(next));
                                ctx.request_repaint();
                            }
                            ID_QUIT => {
                                quit_requested.store(true, Ordering::SeqCst);
                                request_native_close(&main_hwnd);
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                ctx.request_repaint();
                                return;
                            }
                            _ => {}
                        }
                    }

                    std::thread::sleep(Duration::from_millis(50));
                }
            });
        }

        Self {
            tray,
            auto_switch_item,
        }
    }

    /// Synchronizes the tray menu checkmark and hover tooltip with the live profile runtime.
    pub fn update_status(&self, auto_switch_enabled: bool, active_profile_name: &str) {
        self.auto_switch_item.set_checked(auto_switch_enabled);
        if let Some(tray) = &self.tray {
            let tooltip = if auto_switch_enabled {
                format!("Polled — Auto-Switch ON · {active_profile_name}")
            } else {
                "Polled — Auto-Switch OFF".to_string()
            };
            let _ = tray.set_tooltip(Some(tooltip));
        }
    }
}

/// Restores and focuses the Polled window even when it is currently hidden (`WS_VISIBLE == 0`).
///
/// Using Win32 `ShowWindowAsync` directly avoids the `winit`/`eframe` Catch-22 where
/// `ctx.request_repaint()` does not fire `WM_PAINT` on an invisible Win32 window.
pub(crate) fn show_and_focus(main_hwnd: &AtomicIsize, ctx: &egui::Context) {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            SetForegroundWindow, ShowWindowAsync, SW_RESTORE, SW_SHOW,
        };

        let raw = main_hwnd.load(Ordering::SeqCst);
        if raw != 0 {
            let hwnd = HWND(raw as *mut core::ffi::c_void);
            // SAFETY: `hwnd` is our own process's main winit window captured on the UI thread.
            // `ShowWindowAsync` posts asynchronously so calling from a background thread never deadlocks.
            unsafe {
                let _ = ShowWindowAsync(hwnd, SW_SHOW);
                let _ = ShowWindowAsync(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }

    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
}

#[cfg(windows)]
fn request_native_close(main_hwnd: &AtomicIsize) {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

    let raw = main_hwnd.load(Ordering::SeqCst);
    if raw != 0 {
        let hwnd = HWND(raw as *mut core::ffi::c_void);
        // SAFETY: `PostMessageW(WM_CLOSE)` queues a clean close directly onto the UI thread even when hidden.
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

#[cfg(not(windows))]
fn request_native_close(_main_hwnd: &AtomicIsize) {}
