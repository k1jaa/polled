// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Event-driven Windows USB / HID hot-plug watcher built on `WM_DEVICECHANGE` and
//! `RegisterDeviceNotificationW` (0% idle CPU, zero kernel drivers, anti-cheat safe).
//!
//! Uses a hidden top-level window (never shown) because Windows does not broadcast
//! `WM_DEVICECHANGE` to `HWND_MESSAGE` windows. Immediately notifies `ProfileRuntimeCmd`
//! when a USB mouse, 4K/8K dongle, wired charging cable, or USB audio device attaches or detaches.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::profile_runtime::ProfileRuntimeCmd;

pub struct UsbHotplugWatcher {
    hwnd: Arc<AtomicIsize>,
    handle: Option<JoinHandle<()>>,
}

impl UsbHotplugWatcher {
    pub fn start(cmd_tx: Sender<ProfileRuntimeCmd>) -> Self {
        let hwnd = Arc::new(AtomicIsize::new(0));
        let hwnd_bg = Arc::clone(&hwnd);

        let handle = std::thread::Builder::new()
            .name("polled-usb-hotplug".to_string())
            .spawn(move || {
                #[cfg(windows)]
                run_win32_hotplug_loop(hwnd_bg, cmd_tx);
                #[cfg(not(windows))]
                {
                    let _ = (hwnd_bg, cmd_tx);
                }
            })
            .ok();

        Self { hwnd, handle }
    }
}

impl Drop for UsbHotplugWatcher {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

            let raw = self.hwnd.load(Ordering::SeqCst);
            if raw != 0 {
                let hwnd = HWND(raw as *mut core::ffi::c_void);
                // SAFETY: Closing our own hidden watcher window triggers cleanup & loop exit.
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
fn run_win32_hotplug_loop(hwnd_store: Arc<AtomicIsize>, cmd_tx: Sender<ProfileRuntimeCmd>) {
    use std::cell::RefCell;
    use std::ffi::OsStr;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;

    use windows::core::{GUID, PCWSTR};
    use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        PostQuitMessage, RegisterClassW, RegisterDeviceNotificationW, TranslateMessage,
        UnregisterClassW, UnregisterDeviceNotification, DBT_DEVICEARRIVAL,
        DBT_DEVICEREMOVECOMPLETE, DBT_DEVNODES_CHANGED, DBT_DEVTYP_DEVICEINTERFACE,
        DEVICE_NOTIFY_WINDOW_HANDLE, DEV_BROADCAST_DEVICEINTERFACE_W, HDEVNOTIFY, MSG,
        WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_DEVICECHANGE, WNDCLASSW, WS_OVERLAPPED,
    };

    // Standard Microsoft device interface class GUIDs for HID and USB devices.
    const GUID_DEVINTERFACE_HID: GUID = GUID::from_u128(0x4d1e55b2_f16f_11cf_88cb_001111000030);
    const GUID_DEVINTERFACE_USB_DEVICE: GUID =
        GUID::from_u128(0xa5dcbf10_6530_11d2_901f_00c04fb951ed);

    thread_local! {
        static HOTPLUG_TX: RefCell<Option<Sender<ProfileRuntimeCmd>>> = const { RefCell::new(None) };
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_DEVICECHANGE => {
                let evt = wparam.0 as u32;
                if evt == DBT_DEVICEARRIVAL
                    || evt == DBT_DEVICEREMOVECOMPLETE
                    || evt == DBT_DEVNODES_CHANGED
                {
                    HOTPLUG_TX.with(|cell| {
                        if let Some(tx) = cell.borrow().as_ref() {
                            let _ = tx.send(ProfileRuntimeCmd::UsbDeviceChanged);
                        }
                    });
                }
                LRESULT(1)
            }
            WM_CLOSE => {
                unsafe {
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
        "PolledHotplug_{}",
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

    HOTPLUG_TX.with(|cell| {
        *cell.borrow_mut() = Some(cmd_tx);
    });

    // Unparented hidden window (never passed to ShowWindow) receives broadcast WM_DEVICECHANGE.
    let hwnd = match unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_w.as_ptr()),
            PCWSTR::null(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )
    } {
        Ok(h) if !h.is_invalid() => h,
        _ => {
            HOTPLUG_TX.with(|cell| {
                *cell.borrow_mut() = None;
            });
            unsafe {
                let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
            }
            return;
        }
    };

    hwnd_store.store(hwnd.0 as isize, Ordering::SeqCst);

    let mut reg_handles: Vec<HDEVNOTIFY> = Vec::new();
    for guid in [GUID_DEVINTERFACE_HID, GUID_DEVINTERFACE_USB_DEVICE] {
        let filter = DEV_BROADCAST_DEVICEINTERFACE_W {
            dbcc_size: size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
            dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
            dbcc_reserved: 0,
            dbcc_classguid: guid,
            dbcc_name: [0],
        };
        // SAFETY: Valid HWND cast to HANDLE and properly sized `DEV_BROADCAST_DEVICEINTERFACE_W`.
        if let Ok(handle) = unsafe {
            RegisterDeviceNotificationW(
                HANDLE(hwnd.0),
                (&filter as *const DEV_BROADCAST_DEVICEINTERFACE_W).cast(),
                DEVICE_NOTIFY_WINDOW_HANDLE,
            )
        } {
            reg_handles.push(handle);
        }
    }

    let mut msg = MSG::default();
    loop {
        // SAFETY: Standard Win32 blocking message loop (0% idle CPU).
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 <= 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    for h in reg_handles {
        unsafe {
            let _ = UnregisterDeviceNotification(h);
        }
    }
    hwnd_store.store(0, Ordering::SeqCst);
    HOTPLUG_TX.with(|cell| {
        *cell.borrow_mut() = None;
    });
    unsafe {
        let _ = UnregisterClassW(PCWSTR(class_w.as_ptr()), Some(hinstance));
    }
}
