// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_DWORD,
    REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE,
};

use crate::error::{PolledError, Result};
use crate::win::RegHive;

/// RAII wrapper that guarantees `RegCloseKey` is called when the key goes out of scope.
struct OwnedHkey(HKEY);

impl Drop for OwnedHkey {
    fn drop(&mut self) {
        // SAFETY: `self.0` was opened via RegOpenKeyExW or RegCreateKeyExW and has not been closed.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn to_wide_null(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn root_hkey(hive: RegHive) -> HKEY {
    match hive {
        RegHive::Hkcu => HKEY_CURRENT_USER,
        RegHive::Hklm => HKEY_LOCAL_MACHINE,
    }
}

fn hive_label(hive: RegHive) -> &'static str {
    match hive {
        RegHive::Hkcu => "HKCU",
        RegHive::Hklm => "HKLM",
    }
}

fn map_reg_error(err: WIN32_ERROR, hive: RegHive, subkey: &str, op: &str) -> PolledError {
    if err == ERROR_ACCESS_DENIED {
        return PolledError::ElevationRequired {
            operation: format!("{op} {}\\{subkey}", hive_label(hive)),
            reason: "Windows denied write access to this registry location without elevation."
                .to_string(),
        };
    }
    PolledError::Win32 {
        context: format!("{op} {}\\{subkey}", hive_label(hive)),
        message: format!("Win32 error code {}", err.0),
    }
}

fn open_read(hive: RegHive, subkey: &str) -> Result<Option<OwnedHkey>> {
    let subkey_w = to_wide_null(subkey);
    let mut handle = HKEY::default();
    // SAFETY: `subkey_w` is null-terminated UTF-16 and `handle` is valid out-pointer storage.
    let status = unsafe {
        RegOpenKeyExW(
            root_hkey(hive),
            PCWSTR(subkey_w.as_ptr()),
            Some(0),
            KEY_READ,
            &mut handle,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status.is_err() {
        return Err(map_reg_error(status, hive, subkey, "RegOpenKeyExW"));
    }
    Ok(Some(OwnedHkey(handle)))
}

fn create_or_open_write(hive: RegHive, subkey: &str) -> Result<OwnedHkey> {
    let subkey_w = to_wide_null(subkey);
    let mut handle = HKEY::default();
    // SAFETY: `subkey_w` is null-terminated UTF-16 and `handle` is valid out-pointer storage.
    let status = unsafe {
        RegCreateKeyExW(
            root_hkey(hive),
            PCWSTR(subkey_w.as_ptr()),
            Some(0),
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut handle,
            None,
        )
    };
    if status.is_err() {
        return Err(map_reg_error(status, hive, subkey, "RegCreateKeyExW"));
    }
    Ok(OwnedHkey(handle))
}

fn query_raw(hive: RegHive, subkey: &str, name: &str) -> Result<Option<(REG_VALUE_TYPE, Vec<u8>)>> {
    let Some(key) = open_read(hive, subkey)? else {
        return Ok(None);
    };
    let name_w = to_wide_null(name);
    let mut value_type = REG_VALUE_TYPE::default();
    let mut size: u32 = 0;

    // First call queries the required byte length.
    // SAFETY: `key.0` is an open HKEY, `name_w` is null-terminated UTF-16.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(name_w.as_ptr()),
            None,
            Some(&mut value_type),
            None,
            Some(&mut size),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status.is_err() {
        return Err(map_reg_error(status, hive, subkey, "RegQueryValueExW"));
    }

    let mut buf = vec![0u8; size as usize];
    // SAFETY: `buf` has capacity `size` bytes matching the first call.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(name_w.as_ptr()),
            None,
            Some(&mut value_type),
            Some(buf.as_mut_ptr()),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return Err(map_reg_error(status, hive, subkey, "RegQueryValueExW"));
    }
    buf.truncate(size as usize);
    Ok(Some((value_type, buf)))
}

pub fn get_string(hive: RegHive, subkey: &str, name: &str) -> Result<Option<String>> {
    let Some((value_type, buf)) = query_raw(hive, subkey, name)? else {
        return Ok(None);
    };
    if value_type != REG_SZ {
        return Err(PolledError::Win32 {
            context: format!("RegQueryValueExW {}\\{subkey}\\{name}", hive_label(hive)),
            message: format!("Expected REG_SZ, got registry type {}", value_type.0),
        });
    }
    let wide: Vec<u16> = buf
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&chunk| u16::from_le_bytes(chunk))
        .collect();
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    Ok(Some(String::from_utf16_lossy(&wide[..end])))
}

pub fn set_string(hive: RegHive, subkey: &str, name: &str, value: &str) -> Result<()> {
    let key = create_or_open_write(hive, subkey)?;
    let name_w = to_wide_null(name);
    let value_w = to_wide_null(value);
    let bytes: Vec<u8> = value_w.iter().flat_map(|w| w.to_le_bytes()).collect();

    // SAFETY: `key.0` is open with KEY_SET_VALUE; `bytes` is a valid UTF-16LE null-terminated buffer.
    let status = unsafe {
        RegSetValueExW(
            key.0,
            PCWSTR(name_w.as_ptr()),
            Some(0),
            REG_SZ,
            Some(&bytes),
        )
    };
    if status.is_err() {
        return Err(map_reg_error(status, hive, subkey, "RegSetValueExW"));
    }
    Ok(())
}

pub fn get_dword(hive: RegHive, subkey: &str, name: &str) -> Result<Option<u32>> {
    let Some((value_type, buf)) = query_raw(hive, subkey, name)? else {
        return Ok(None);
    };
    if value_type != REG_DWORD || buf.len() < 4 {
        return Err(PolledError::Win32 {
            context: format!("RegQueryValueExW {}\\{subkey}\\{name}", hive_label(hive)),
            message: format!(
                "Expected 4-byte REG_DWORD, got type {} ({} bytes)",
                value_type.0,
                buf.len()
            ),
        });
    }
    Ok(Some(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])))
}

pub fn set_dword(hive: RegHive, subkey: &str, name: &str, value: u32) -> Result<()> {
    let key = create_or_open_write(hive, subkey)?;
    let name_w = to_wide_null(name);
    let bytes = value.to_le_bytes();

    // SAFETY: `key.0` is open with KEY_SET_VALUE; `bytes` holds 4 bytes for REG_DWORD.
    let status = unsafe {
        RegSetValueExW(
            key.0,
            PCWSTR(name_w.as_ptr()),
            Some(0),
            REG_DWORD,
            Some(&bytes),
        )
    };
    if status.is_err() {
        return Err(map_reg_error(status, hive, subkey, "RegSetValueExW"));
    }
    Ok(())
}

pub fn get_binary(hive: RegHive, subkey: &str, name: &str) -> Result<Option<Vec<u8>>> {
    let Some((value_type, buf)) = query_raw(hive, subkey, name)? else {
        return Ok(None);
    };
    if value_type != REG_BINARY {
        return Err(PolledError::Win32 {
            context: format!("RegQueryValueExW {}\\{subkey}\\{name}", hive_label(hive)),
            message: format!("Expected REG_BINARY, got registry type {}", value_type.0),
        });
    }
    Ok(Some(buf))
}

pub fn set_binary(hive: RegHive, subkey: &str, name: &str, value: &[u8]) -> Result<()> {
    let key = create_or_open_write(hive, subkey)?;
    let name_w = to_wide_null(name);

    // SAFETY: `key.0` is open with KEY_SET_VALUE; `value` is a valid byte slice.
    let status = unsafe {
        RegSetValueExW(
            key.0,
            PCWSTR(name_w.as_ptr()),
            Some(0),
            REG_BINARY,
            Some(value),
        )
    };
    if status.is_err() {
        return Err(map_reg_error(status, hive, subkey, "RegSetValueExW"));
    }
    Ok(())
}

pub fn delete_value(hive: RegHive, subkey: &str, name: &str) -> Result<()> {
    let key = create_or_open_write(hive, subkey)?;
    let name_w = to_wide_null(name);

    // SAFETY: `key.0` is open with KEY_SET_VALUE; `name_w` is null-terminated UTF-16.
    let status = unsafe { RegDeleteValueW(key.0, PCWSTR(name_w.as_ptr())) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    if status.is_err() {
        return Err(map_reg_error(status, hive, subkey, "RegDeleteValueW"));
    }
    Ok(())
}
