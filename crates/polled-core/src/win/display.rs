// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Read-only Windows display refresh-rate inspection using `EnumDisplayDevicesW` and
//! `EnumDisplaySettingsW`.

use std::mem::size_of;

use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplaySettingsW, DEVMODEW, DISPLAY_DEVICEW,
    DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICE_PRIMARY_DEVICE, ENUM_CURRENT_SETTINGS,
    ENUM_DISPLAY_SETTINGS_MODE,
};

use crate::error::Result;
use crate::win::DisplayModeInfo;

fn wide_z_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end]).trim().to_string()
}

/// Enumerates all attached desktop monitors, their active resolution + refresh rate, and
/// the highest refresh rate Windows exposes at each monitor's current resolution.
pub fn list_display_modes() -> Result<Vec<DisplayModeInfo>> {
    let mut displays = Vec::new();
    let mut dev_num: u32 = 0;

    loop {
        let mut dd = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // SAFETY: `dd.cb` is initialized to `size_of::<DISPLAY_DEVICEW>()`.
        let ok = unsafe { EnumDisplayDevicesW(PCWSTR::null(), dev_num, &mut dd, 0).as_bool() };
        if !ok {
            break;
        }
        dev_num += 1;

        if (dd.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP).0 == 0 {
            continue;
        }

        let is_primary = (dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE).0 != 0;
        let device_name = wide_z_to_string(&dd.DeviceName);

        let mut current = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        // SAFETY: `dd.DeviceName` is null-terminated UTF-16; `current.dmSize` is initialized.
        let has_current = unsafe {
            EnumDisplaySettingsW(
                PCWSTR(dd.DeviceName.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut current,
            )
            .as_bool()
        };
        if !has_current || current.dmPelsWidth == 0 || current.dmPelsHeight == 0 {
            continue;
        }

        // Skip virtual/RDP displays that report sentinel frequency 0 or 1.
        if current.dmDisplayFrequency <= 1 {
            continue;
        }

        let mut max_hz = current.dmDisplayFrequency;
        let mut mode_num: u32 = 0;
        loop {
            let mut mode = DEVMODEW {
                dmSize: size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            // SAFETY: `dd.DeviceName` is null-terminated UTF-16; `mode.dmSize` is initialized.
            let more = unsafe {
                EnumDisplaySettingsW(
                    PCWSTR(dd.DeviceName.as_ptr()),
                    ENUM_DISPLAY_SETTINGS_MODE(mode_num),
                    &mut mode,
                )
                .as_bool()
            };
            if !more {
                break;
            }
            mode_num += 1;
            if mode.dmPelsWidth == current.dmPelsWidth
                && mode.dmPelsHeight == current.dmPelsHeight
                && mode.dmDisplayFrequency > 1
            {
                max_hz = max_hz.max(mode.dmDisplayFrequency);
            }
        }

        // Query monitor child device 0 for a friendlier monitor string if available.
        let mut monitor_dd = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // SAFETY: `dd.DeviceName` is null-terminated UTF-16; `monitor_dd.cb` is initialized.
        let has_monitor = unsafe {
            EnumDisplayDevicesW(PCWSTR(dd.DeviceName.as_ptr()), 0, &mut monitor_dd, 0).as_bool()
        };
        let friendly = if has_monitor {
            let s = wide_z_to_string(&monitor_dd.DeviceString);
            if s.is_empty() {
                device_name.clone()
            } else {
                s
            }
        } else {
            device_name.clone()
        };

        displays.push(DisplayModeInfo {
            device_name,
            friendly_name: friendly,
            is_primary,
            width: current.dmPelsWidth,
            height: current.dmPelsHeight,
            current_hz: current.dmDisplayFrequency,
            max_supported_hz: max_hz,
        });
    }

    Ok(displays)
}
