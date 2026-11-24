// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Windows display inspection and safe session-only mode switching using
//! `EnumDisplayDevicesW`, `EnumDisplaySettingsW`, and `ChangeDisplaySettingsExW`.
//!
//! Display mode changes are strictly temporary (`CDS_UPDATEREGISTRY` is **never** passed)
//! and pre-validated with `CDS_TEST`, so crashes or reboots always recover native desktop settings.

use std::collections::BTreeSet;
use std::mem::size_of;

use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{
    ChangeDisplaySettingsExW, EnumDisplayDevicesW, EnumDisplaySettingsW, CDS_TEST, CDS_TYPE,
    DEVMODEW, DISPLAY_DEVICEW, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICE_PRIMARY_DEVICE,
    DISP_CHANGE, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
    ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE,
};

use crate::error::{PolledError, Result};
use crate::win::{DisplayModeInfo, DisplayResolutionMode};

fn wide_z_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end]).trim().to_string()
}

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn describe_disp_change(code: DISP_CHANGE) -> String {
    match code.0 {
        0 => "DISP_CHANGE_SUCCESSFUL".to_string(),
        1 => "DISP_CHANGE_RESTART (reboot required)".to_string(),
        -1 => "DISP_CHANGE_FAILED (display driver rejected mode)".to_string(),
        -2 => "DISP_CHANGE_BADMODE (resolution or refresh rate unsupported)".to_string(),
        -3 => "DISP_CHANGE_NOTUPDATED".to_string(),
        -4 => "DISP_CHANGE_BADFLAGS".to_string(),
        -5 => "DISP_CHANGE_BADPARAM".to_string(),
        -6 => "DISP_CHANGE_BADDUALVIEW".to_string(),
        other => format!("DISP_CHANGE({other})"),
    }
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

/// Enumerates all 32-bit desktop modes (`width >= 800`, `height >= 600`) supported by `device_name`,
/// deduplicated and sorted descending by resolution and refresh rate.
pub fn list_supported_display_modes(device_name: &str) -> Result<Vec<DisplayResolutionMode>> {
    let wide = to_wide_null(device_name);
    let mut seen: BTreeSet<(u32, u32, u32)> = BTreeSet::new();
    let mut mode_num: u32 = 0;

    loop {
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        // SAFETY: `wide` is null-terminated UTF-16 and `mode.dmSize` is initialized.
        let more = unsafe {
            EnumDisplaySettingsW(
                PCWSTR(wide.as_ptr()),
                ENUM_DISPLAY_SETTINGS_MODE(mode_num),
                &mut mode,
            )
            .as_bool()
        };
        if !more {
            break;
        }
        mode_num += 1;
        if mode.dmBitsPerPel >= 32
            && mode.dmPelsWidth >= 800
            && mode.dmPelsHeight >= 600
            && mode.dmDisplayFrequency > 1
        {
            seen.insert((mode.dmPelsWidth, mode.dmPelsHeight, mode.dmDisplayFrequency));
        }
    }

    let mut modes: Vec<DisplayResolutionMode> = seen
        .into_iter()
        .map(|(width, height, refresh_hz)| DisplayResolutionMode {
            width,
            height,
            refresh_hz,
        })
        .collect();
    modes.sort_by_key(|a| std::cmp::Reverse((a.width, a.height, a.refresh_hz)));
    Ok(modes)
}

/// Temporarily switches `device_name` to `width x height @ refresh_hz` without persisting to the
/// Windows registry (`CDS_UPDATEREGISTRY` is never set). Validates with `CDS_TEST` first.
pub fn set_display_mode(device_name: &str, width: u32, height: u32, refresh_hz: u32) -> Result<()> {
    if width < 800 || height < 600 || refresh_hz < 30 {
        return Err(PolledError::Win32 {
            context: "ChangeDisplaySettingsExW".to_string(),
            message: format!(
                "Refusing unsafe display mode {width}x{height} @ {refresh_hz} Hz (min 800x600 @ 30 Hz)"
            ),
        });
    }

    let wide = to_wide_null(device_name);
    let mut current = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: `wide` is null-terminated UTF-16 and `current.dmSize` is initialized.
    let has_current = unsafe {
        EnumDisplaySettingsW(PCWSTR(wide.as_ptr()), ENUM_CURRENT_SETTINGS, &mut current).as_bool()
    };
    if !has_current {
        return Err(PolledError::Win32 {
            context: "EnumDisplaySettingsW".to_string(),
            message: format!("Display '{device_name}' is not active or could not be queried"),
        });
    }

    if current.dmPelsWidth == width
        && current.dmPelsHeight == height
        && current.dmDisplayFrequency == refresh_hz
    {
        return Ok(());
    }

    current.dmPelsWidth = width;
    current.dmPelsHeight = height;
    current.dmDisplayFrequency = refresh_hz;
    current.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;

    // 1. Validate first with CDS_TEST (zero visual disruption if unsupported).
    // SAFETY: `wide` and `current` remain valid for the duration of the Win32 call.
    let test_rc = unsafe {
        ChangeDisplaySettingsExW(PCWSTR(wide.as_ptr()), Some(&current), None, CDS_TEST, None)
    };
    if test_rc != DISP_CHANGE_SUCCESSFUL {
        return Err(PolledError::Win32 {
            context: "ChangeDisplaySettingsExW(CDS_TEST)".to_string(),
            message: format!(
                "Mode {width}x{height} @ {refresh_hz} Hz rejected for {device_name}: {}",
                describe_disp_change(test_rc)
            ),
        });
    }

    // 2. Apply temporary session-only display mode (`CDS_TYPE(0)` — never CDS_UPDATEREGISTRY).
    // SAFETY: `wide` and `current` remain valid for the duration of the Win32 call.
    let apply_rc = unsafe {
        ChangeDisplaySettingsExW(
            PCWSTR(wide.as_ptr()),
            Some(&current),
            None,
            CDS_TYPE(0),
            None,
        )
    };
    if apply_rc != DISP_CHANGE_SUCCESSFUL {
        return Err(PolledError::Win32 {
            context: "ChangeDisplaySettingsExW".to_string(),
            message: format!(
                "Failed to apply {width}x{height} @ {refresh_hz} Hz on {device_name}: {}",
                describe_disp_change(apply_rc)
            ),
        });
    }
    Ok(())
}
