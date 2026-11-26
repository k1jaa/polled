// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Anti-cheat-safe global desktop hotkey listener built on Win32 `RegisterHotKey`.
//!
//! Uses a kernel-blocking `HWND_MESSAGE` thread (0% idle CPU, zero keyboard hooks) and cleanly
//! unregisters every hotkey when disabled, updated, or dropped.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use polled_core::HotkeyConfig;
use serde::{Deserialize, Serialize};

use crate::app::WorkerMsg;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HotkeyAction {
    CycleProfile,
    ToggleProfileLock,
    ToggleInputLab,
    CycleAudioOutput,
    CycleWootingProfile,
    ToggleMicMute,
}

#[cfg(windows)]
const WM_POLLED_HOTKEY_RELOAD: u32 = 0x8002; // WM_APP + 2
#[cfg(windows)]
const HK_ID_CYCLE: i32 = 1;
#[cfg(windows)]
const HK_ID_LOCK: i32 = 2;
#[cfg(windows)]
const HK_ID_LAB: i32 = 3;
#[cfg(windows)]
const HK_ID_AUDIO: i32 = 4;
#[cfg(windows)]
const HK_ID_WOOTING: i32 = 5;
#[cfg(windows)]
const HK_ID_MIC_MUTE: i32 = 6;

pub struct HotkeyManager {
    config: Arc<Mutex<HotkeyConfig>>,
    msg_hwnd: Arc<AtomicIsize>,
    handle: Option<JoinHandle<()>>,
}

impl HotkeyManager {
    pub fn start(
        initial: HotkeyConfig,
        ui_tx: Sender<WorkerMsg>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let config = Arc::new(Mutex::new(initial));
        let msg_hwnd = Arc::new(AtomicIsize::new(0));

        let cfg_bg = Arc::clone(&config);
        let hwnd_bg = Arc::clone(&msg_hwnd);

        let handle = std::thread::Builder::new()
            .name("polled-hotkeys".to_string())
            .spawn(move || {
                #[cfg(windows)]
                run_win32_hotkey_loop(cfg_bg, hwnd_bg, ui_tx, wake);
                #[cfg(not(windows))]
                {
                    let _ = (cfg_bg, hwnd_bg, ui_tx, wake);
                }
            })
            .ok();

        Self {
            config,
            msg_hwnd,
            handle,
        }
    }

    pub fn update_config(&self, next: HotkeyConfig) {
        if let Ok(mut guard) = self.config.lock() {
            *guard = next;
        }
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

            let raw = self.msg_hwnd.load(Ordering::SeqCst);
            if raw != 0 {
                let hwnd = HWND(raw as *mut core::ffi::c_void);
                // SAFETY: Posting custom reload message to our own message-only window.
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_POLLED_HOTKEY_RELOAD, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
}

impl Drop for HotkeyManager {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

            let raw = self.msg_hwnd.load(Ordering::SeqCst);
            if raw != 0 {
                let hwnd = HWND(raw as *mut core::ffi::c_void);
                // SAFETY: Closing our own message-only window triggers cleanup & loop exit.
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(windows)]
fn run_win32_hotkey_loop(
    config: Arc<Mutex<HotkeyConfig>>,
    msg_hwnd: Arc<AtomicIsize>,
    ui_tx: Sender<WorkerMsg>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    use std::cell::RefCell;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        PostQuitMessage, RegisterClassW, TranslateMessage, UnregisterClassW, HWND_MESSAGE, MSG,
        WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_DESTROY, WM_HOTKEY, WNDCLASSW,
    };

    struct LoopState {
        config: Arc<Mutex<HotkeyConfig>>,
        ui_tx: Sender<WorkerMsg>,
        wake: Arc<dyn Fn() + Send + Sync>,
    }

    thread_local! {
        static HK_STATE: RefCell<Option<LoopState>> = const { RefCell::new(None) };
    }

    fn apply_registrations(hwnd: HWND, cfg: &HotkeyConfig) {
        // SAFETY: Always unregister previous IDs on our own HWND before re-registering.
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), HK_ID_CYCLE);
            let _ = UnregisterHotKey(Some(hwnd), HK_ID_LOCK);
            let _ = UnregisterHotKey(Some(hwnd), HK_ID_LAB);
            let _ = UnregisterHotKey(Some(hwnd), HK_ID_AUDIO);
            let _ = UnregisterHotKey(Some(hwnd), HK_ID_WOOTING);
            let _ = UnregisterHotKey(Some(hwnd), HK_ID_MIC_MUTE);
        }
        if !cfg.enabled {
            return;
        }
        let entries = [
            (HK_ID_CYCLE, cfg.cycle_profile.as_ref()),
            (HK_ID_LOCK, cfg.toggle_lock.as_ref()),
            (HK_ID_LAB, cfg.toggle_input_lab.as_ref()),
            (HK_ID_AUDIO, cfg.cycle_audio_output.as_ref()),
            (HK_ID_WOOTING, cfg.cycle_wooting_profile.as_ref()),
            (HK_ID_MIC_MUTE, cfg.toggle_mic_mute.as_ref()),
        ];
        for (id, binding) in entries {
            if let Some(b) = binding {
                if let Some(vk) = b.win32_vk() {
                    let mods = HOT_KEY_MODIFIERS(b.win32_modifiers());
                    // SAFETY: Registering standard desktop shortcut on our own message window.
                    unsafe {
                        let _ = RegisterHotKey(Some(hwnd), id, mods, vk);
                    }
                }
            }
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_HOTKEY => {
                let action = match wparam.0 as i32 {
                    HK_ID_CYCLE => Some(HotkeyAction::CycleProfile),
                    HK_ID_LOCK => Some(HotkeyAction::ToggleProfileLock),
                    HK_ID_LAB => Some(HotkeyAction::ToggleInputLab),
                    HK_ID_AUDIO => Some(HotkeyAction::CycleAudioOutput),
                    HK_ID_WOOTING => Some(HotkeyAction::CycleWootingProfile),
                    HK_ID_MIC_MUTE => Some(HotkeyAction::ToggleMicMute),
                    _ => None,
                };
                if let Some(act) = action {
                    HK_STATE.with(|cell| {
                        if let Some(st) = cell.borrow().as_ref() {
                            let _ = st.ui_tx.send(WorkerMsg::HotkeyTriggered(act));
                            (st.wake)();
                        }
                    });
                }
                LRESULT(0)
            }
            WM_POLLED_HOTKEY_RELOAD => {
                HK_STATE.with(|cell| {
                    if let Some(st) = cell.borrow().as_ref() {
                        if let Ok(cfg) = st.config.lock() {
                            apply_registrations(hwnd, &cfg);
                        }
                    }
                });
                LRESULT(0)
            }
            WM_CLOSE => {
                unsafe {
                    let _ = UnregisterHotKey(Some(hwnd), HK_ID_CYCLE);
                    let _ = UnregisterHotKey(Some(hwnd), HK_ID_LOCK);
                    let _ = UnregisterHotKey(Some(hwnd), HK_ID_LAB);
                    let _ = UnregisterHotKey(Some(hwnd), HK_ID_AUDIO);
                    let _ = UnregisterHotKey(Some(hwnd), HK_ID_WOOTING);
                    let _ = UnregisterHotKey(Some(hwnd), HK_ID_MIC_MUTE);
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

    let class_str = format!(
        "PolledHotkeys_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let class_w: Vec<u16> = OsStr::new(&class_str)
        .encode_wide()
        .chain(Some(0))
        .collect();

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

    let hwnd = match unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_w.as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
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

    if let Ok(cfg) = config.lock() {
        apply_registrations(hwnd, &cfg);
    }

    HK_STATE.with(|cell| {
        *cell.borrow_mut() = Some(LoopState {
            config,
            ui_tx,
            wake,
        });
    });
    msg_hwnd.store(hwnd.0 as isize, Ordering::SeqCst);

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0).0 > 0 } {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    msg_hwnd.store(0, Ordering::SeqCst);
    HK_STATE.with(|cell| {
        cell.borrow_mut().take();
    });
    unsafe {
        let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
    }
}
