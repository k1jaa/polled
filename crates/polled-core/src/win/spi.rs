// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::UI::Accessibility::{FILTERKEYS, MOUSEKEYS, STICKYKEYS, STICKYKEYS_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SPI_GETFILTERKEYS, SPI_GETMOUSE,
    SPI_GETMOUSEKEYS, SPI_GETMOUSESPEED, SPI_GETSTICKYKEYS, SPI_SETFILTERKEYS, SPI_SETMOUSE,
    SPI_SETMOUSEKEYS, SPI_SETMOUSESPEED, SPI_SETSTICKYKEYS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

use crate::error::{PolledError, Result};
use crate::win::{AccessibilityKind, MouseAccelParams};

const PERSIST_AND_BROADCAST: SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS =
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(SPIF_UPDATEINIFILE.0 | SPIF_SENDCHANGE.0);

fn spi_err(context: &str, err: windows::core::Error) -> PolledError {
    PolledError::Win32 {
        context: context.to_string(),
        message: err.to_string(),
    }
}

pub fn get_mouse_accel() -> Result<MouseAccelParams> {
    let mut raw = [0i32; 3];
    // SAFETY: `raw` is a 3-element `i32` array as documented for `SPI_GETMOUSE`.
    unsafe {
        SystemParametersInfoW(
            SPI_GETMOUSE,
            0,
            Some(raw.as_mut_ptr().cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .map_err(|e| spi_err("SPI_GETMOUSE", e))?;

    Ok(MouseAccelParams {
        threshold1: raw[0],
        threshold2: raw[1],
        speed: raw[2],
    })
}

pub fn set_mouse_accel(params: MouseAccelParams) -> Result<()> {
    let mut raw = [params.threshold1, params.threshold2, params.speed];
    // SAFETY: `raw` is a 3-element `i32` array as documented for `SPI_SETMOUSE`.
    unsafe {
        SystemParametersInfoW(
            SPI_SETMOUSE,
            0,
            Some(raw.as_mut_ptr().cast::<c_void>()),
            PERSIST_AND_BROADCAST,
        )
    }
    .map_err(|e| spi_err("SPI_SETMOUSE", e))
}

pub fn get_mouse_speed() -> Result<u32> {
    let mut speed: u32 = 0;
    // SAFETY: `SPI_GETMOUSESPEED` writes a `u32` (1..=20) to `pvParam`.
    unsafe {
        SystemParametersInfoW(
            SPI_GETMOUSESPEED,
            0,
            Some((&mut speed as *mut u32).cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .map_err(|e| spi_err("SPI_GETMOUSESPEED", e))?;
    Ok(speed)
}

pub fn set_mouse_speed(speed: u32) -> Result<()> {
    let clamped = speed.clamp(1, 20);
    // SAFETY: Per Microsoft docs, `SPI_SETMOUSESPEED` passes the speed value directly in `pvParam`,
    // not as a pointer to an integer.
    unsafe {
        SystemParametersInfoW(
            SPI_SETMOUSESPEED,
            0,
            Some(clamped as usize as *mut c_void),
            PERSIST_AND_BROADCAST,
        )
    }
    .map_err(|e| spi_err("SPI_SETMOUSESPEED", e))
}

pub fn get_accessibility_flags(kind: AccessibilityKind) -> Result<u32> {
    match kind {
        AccessibilityKind::StickyKeys => {
            let mut sk = STICKYKEYS {
                cbSize: size_of::<STICKYKEYS>() as u32,
                dwFlags: STICKYKEYS_FLAGS(0),
            };
            // SAFETY: `sk.cbSize` and `uiParam` both match `size_of::<STICKYKEYS>()`.
            unsafe {
                SystemParametersInfoW(
                    SPI_GETSTICKYKEYS,
                    sk.cbSize,
                    Some((&mut sk as *mut STICKYKEYS).cast::<c_void>()),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                )
            }
            .map_err(|e| spi_err("SPI_GETSTICKYKEYS", e))?;
            Ok(sk.dwFlags.0)
        }
        AccessibilityKind::FilterKeys => {
            let mut fk = FILTERKEYS {
                cbSize: size_of::<FILTERKEYS>() as u32,
                ..Default::default()
            };
            // SAFETY: `fk.cbSize` and `uiParam` both match `size_of::<FILTERKEYS>()`.
            unsafe {
                SystemParametersInfoW(
                    SPI_GETFILTERKEYS,
                    fk.cbSize,
                    Some((&mut fk as *mut FILTERKEYS).cast::<c_void>()),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                )
            }
            .map_err(|e| spi_err("SPI_GETFILTERKEYS", e))?;
            Ok(fk.dwFlags)
        }
        AccessibilityKind::MouseKeys => {
            let mut mk = MOUSEKEYS {
                cbSize: size_of::<MOUSEKEYS>() as u32,
                ..Default::default()
            };
            // SAFETY: `mk.cbSize` and `uiParam` both match `size_of::<MOUSEKEYS>()`.
            unsafe {
                SystemParametersInfoW(
                    SPI_GETMOUSEKEYS,
                    mk.cbSize,
                    Some((&mut mk as *mut MOUSEKEYS).cast::<c_void>()),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                )
            }
            .map_err(|e| spi_err("SPI_GETMOUSEKEYS", e))?;
            Ok(mk.dwFlags)
        }
    }
}

/// Updates `dwFlags` while preserving every other field (delays, bounce rates, cursor speed).
pub fn set_accessibility_flags(kind: AccessibilityKind, flags: u32) -> Result<()> {
    match kind {
        AccessibilityKind::StickyKeys => {
            let mut sk = STICKYKEYS {
                cbSize: size_of::<STICKYKEYS>() as u32,
                dwFlags: STICKYKEYS_FLAGS(flags),
            };
            // SAFETY: `sk.cbSize` and `uiParam` match `size_of::<STICKYKEYS>()`.
            unsafe {
                SystemParametersInfoW(
                    SPI_SETSTICKYKEYS,
                    sk.cbSize,
                    Some((&mut sk as *mut STICKYKEYS).cast::<c_void>()),
                    PERSIST_AND_BROADCAST,
                )
            }
            .map_err(|e| spi_err("SPI_SETSTICKYKEYS", e))
        }
        AccessibilityKind::FilterKeys => {
            let mut fk = FILTERKEYS {
                cbSize: size_of::<FILTERKEYS>() as u32,
                ..Default::default()
            };
            // Read current struct first so timing fields are preserved.
            // SAFETY: `fk.cbSize` and `uiParam` match `size_of::<FILTERKEYS>()`.
            unsafe {
                SystemParametersInfoW(
                    SPI_GETFILTERKEYS,
                    fk.cbSize,
                    Some((&mut fk as *mut FILTERKEYS).cast::<c_void>()),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                )
            }
            .map_err(|e| spi_err("SPI_GETFILTERKEYS", e))?;

            fk.dwFlags = flags;
            // SAFETY: `fk` is a valid initialized `FILTERKEYS` struct.
            unsafe {
                SystemParametersInfoW(
                    SPI_SETFILTERKEYS,
                    fk.cbSize,
                    Some((&mut fk as *mut FILTERKEYS).cast::<c_void>()),
                    PERSIST_AND_BROADCAST,
                )
            }
            .map_err(|e| spi_err("SPI_SETFILTERKEYS", e))
        }
        AccessibilityKind::MouseKeys => {
            let mut mk = MOUSEKEYS {
                cbSize: size_of::<MOUSEKEYS>() as u32,
                ..Default::default()
            };
            // Read current struct first so speed/acceleration fields are preserved.
            // SAFETY: `mk.cbSize` and `uiParam` match `size_of::<MOUSEKEYS>()`.
            unsafe {
                SystemParametersInfoW(
                    SPI_GETMOUSEKEYS,
                    mk.cbSize,
                    Some((&mut mk as *mut MOUSEKEYS).cast::<c_void>()),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                )
            }
            .map_err(|e| spi_err("SPI_GETMOUSEKEYS", e))?;

            mk.dwFlags = flags;
            // SAFETY: `mk` is a valid initialized `MOUSEKEYS` struct.
            unsafe {
                SystemParametersInfoW(
                    SPI_SETMOUSEKEYS,
                    mk.cbSize,
                    Some((&mut mk as *mut MOUSEKEYS).cast::<c_void>()),
                    PERSIST_AND_BROADCAST,
                )
            }
            .map_err(|e| spi_err("SPI_SETMOUSEKEYS", e))
        }
    }
}
