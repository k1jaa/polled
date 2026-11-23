// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::mem::size_of;

use windows::core::GUID;
use windows::Win32::Foundation::{
    LocalFree, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS,
    HLOCAL, WIN32_ERROR,
};
use windows::Win32::System::Power::{
    PowerEnumerate, PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex,
    PowerReadFriendlyName, PowerSetActiveScheme, PowerWriteACValueIndex, PowerWriteDCValueIndex,
    ACCESS_SCHEME,
};

use crate::error::{PolledError, Result};
use crate::win::{PowerSchemeInfo, PowerValueIndex};

/// Parses a hyphenated GUID string (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`, with or without braces).
pub fn parse_guid(s: &str) -> Result<GUID> {
    let trimmed = s.trim().trim_start_matches('{').trim_end_matches('}');
    let parts: Vec<&str> = trimmed.split('-').collect();
    if parts.len() != 5
        || parts[0].len() != 8
        || parts[1].len() != 4
        || parts[2].len() != 4
        || parts[3].len() != 4
        || parts[4].len() != 12
    {
        return Err(PolledError::InvalidGuid(s.to_string()));
    }

    let data1 =
        u32::from_str_radix(parts[0], 16).map_err(|_| PolledError::InvalidGuid(s.to_string()))?;
    let data2 =
        u16::from_str_radix(parts[1], 16).map_err(|_| PolledError::InvalidGuid(s.to_string()))?;
    let data3 =
        u16::from_str_radix(parts[2], 16).map_err(|_| PolledError::InvalidGuid(s.to_string()))?;

    let mut data4 = [0u8; 8];
    let tail = format!("{}{}", parts[3], parts[4]);
    for (i, slot) in data4.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&tail[i * 2..i * 2 + 2], 16)
            .map_err(|_| PolledError::InvalidGuid(s.to_string()))?;
    }

    Ok(GUID {
        data1,
        data2,
        data3,
        data4,
    })
}

/// Formats a Win32 `GUID` as lowercase hyphenated text without braces.
pub fn format_guid(g: &GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7],
    )
}

fn map_power_err(err: WIN32_ERROR, op: &str) -> PolledError {
    if err == ERROR_ACCESS_DENIED {
        return PolledError::ElevationRequired {
            operation: op.to_string(),
            reason: "Windows requires administrator privileges to modify this power setting."
                .to_string(),
        };
    }
    PolledError::Win32 {
        context: op.to_string(),
        message: format!("powrprof error code {}", err.0),
    }
}

fn read_friendly_name(scheme: &GUID) -> String {
    let mut size: u32 = 0;
    // SAFETY: Querying buffer size with `None` buffer is documented for `PowerReadFriendlyName`.
    let status = unsafe { PowerReadFriendlyName(None, Some(scheme), None, None, None, &mut size) };
    if (status.is_err() && status != ERROR_MORE_DATA) || size == 0 {
        return format_guid(scheme);
    }

    let mut buf = vec![0u8; size as usize];
    // SAFETY: `buf` has length `size` matching the first call.
    let status = unsafe {
        PowerReadFriendlyName(
            None,
            Some(scheme),
            None,
            None,
            Some(buf.as_mut_ptr()),
            &mut size,
        )
    };
    if status.is_err() {
        return format_guid(scheme);
    }

    let wide: Vec<u16> = buf[..size as usize]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .collect();
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    let name = String::from_utf16_lossy(&wide[..end]).trim().to_string();
    if name.is_empty() {
        format_guid(scheme)
    } else {
        name
    }
}

pub fn get_active_power_scheme() -> Result<PowerSchemeInfo> {
    let mut ptr: *mut GUID = std::ptr::null_mut();
    // SAFETY: `PowerGetActiveScheme` writes a `LocalAlloc`-allocated GUID pointer to `ptr`.
    let status = unsafe { PowerGetActiveScheme(None, &mut ptr) };
    if status.is_err() || ptr.is_null() {
        return Err(map_power_err(status, "PowerGetActiveScheme"));
    }

    // SAFETY: `ptr` is non-null and points to a valid GUID allocated by `PowerGetActiveScheme`.
    let guid = unsafe { *ptr };
    // SAFETY: `ptr` must be released with `LocalFree` per Microsoft documentation.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(ptr.cast())));
    }

    Ok(PowerSchemeInfo {
        guid: format_guid(&guid),
        name: read_friendly_name(&guid),
    })
}

pub fn list_power_schemes() -> Result<Vec<PowerSchemeInfo>> {
    let mut schemes = Vec::new();
    let mut index: u32 = 0;

    loop {
        let mut guid = GUID::zeroed();
        let mut size = size_of::<GUID>() as u32;
        // SAFETY: `guid` provides `size_of::<GUID>()` writable bytes for `ACCESS_SCHEME`.
        let status = unsafe {
            PowerEnumerate(
                None,
                None,
                None,
                ACCESS_SCHEME,
                index,
                Some((&mut guid as *mut GUID).cast::<u8>()),
                &mut size,
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status.is_err() {
            return Err(map_power_err(status, "PowerEnumerate"));
        }

        schemes.push(PowerSchemeInfo {
            guid: format_guid(&guid),
            name: read_friendly_name(&guid),
        });
        index += 1;
    }

    Ok(schemes)
}

pub fn set_active_power_scheme(scheme_guid: &str) -> Result<()> {
    let guid = parse_guid(scheme_guid)?;
    // SAFETY: `guid` is a valid GUID struct.
    let status = unsafe { PowerSetActiveScheme(None, Some(&guid)) };
    if status.is_err() {
        return Err(map_power_err(status, "PowerSetActiveScheme"));
    }
    Ok(())
}

pub fn get_power_setting_index(
    scheme_guid: &str,
    subgroup_guid: &str,
    setting_guid: &str,
) -> Result<PowerValueIndex> {
    let scheme = parse_guid(scheme_guid)?;
    let subgroup = parse_guid(subgroup_guid)?;
    let setting = parse_guid(setting_guid)?;

    let mut ac_val = 0u32;
    // SAFETY: All GUID references and out-pointer are valid.
    let ac_status = unsafe {
        PowerReadACValueIndex(
            None,
            Some(&scheme),
            Some(&subgroup),
            Some(&setting),
            &mut ac_val,
        )
    };
    let ac = if ac_status.is_ok() {
        Some(ac_val)
    } else if ac_status == ERROR_FILE_NOT_FOUND {
        None
    } else {
        return Err(map_power_err(ac_status, "PowerReadACValueIndex"));
    };

    let mut dc_val = 0u32;
    // SAFETY: All GUID references and out-pointer are valid.
    let dc_status = WIN32_ERROR(unsafe {
        PowerReadDCValueIndex(
            None,
            Some(&scheme),
            Some(&subgroup),
            Some(&setting),
            &mut dc_val,
        )
    });
    let dc = if dc_status.is_ok() {
        Some(dc_val)
    } else if dc_status == ERROR_FILE_NOT_FOUND {
        None
    } else {
        return Err(map_power_err(dc_status, "PowerReadDCValueIndex"));
    };

    Ok(PowerValueIndex { ac, dc })
}

pub fn set_power_setting_index(
    scheme_guid: &str,
    subgroup_guid: &str,
    setting_guid: &str,
    values: PowerValueIndex,
) -> Result<()> {
    let scheme = parse_guid(scheme_guid)?;
    let subgroup = parse_guid(subgroup_guid)?;
    let setting = parse_guid(setting_guid)?;

    if let Some(ac) = values.ac {
        // SAFETY: All GUID references are valid.
        let status =
            unsafe { PowerWriteACValueIndex(None, &scheme, Some(&subgroup), Some(&setting), ac) };
        if status.is_err() {
            return Err(map_power_err(status, "PowerWriteACValueIndex"));
        }
    }

    if let Some(dc) = values.dc {
        // SAFETY: All GUID references are valid.
        let status = WIN32_ERROR(unsafe {
            PowerWriteDCValueIndex(None, &scheme, Some(&subgroup), Some(&setting), dc)
        });
        if status.is_err() {
            return Err(map_power_err(status, "PowerWriteDCValueIndex"));
        }
    }

    // Re-apply the active scheme so the running power manager picks up the new index immediately.
    // SAFETY: `scheme` is a valid GUID.
    let apply_status = unsafe { PowerSetActiveScheme(None, Some(&scheme)) };
    if apply_status.is_err() {
        return Err(map_power_err(apply_status, "PowerSetActiveScheme"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_guid_formatting() {
        let input = "381b4222-f694-41f0-9685-ff5bb260df2e";
        let guid = parse_guid(input).unwrap();
        assert_eq!(format_guid(&guid), input);

        let braced = "{381B4222-F694-41F0-9685-FF5BB260DF2E}";
        let guid2 = parse_guid(braced).unwrap();
        assert_eq!(format_guid(&guid2), input);
    }

    #[test]
    fn rejects_invalid_guid() {
        assert!(parse_guid("not-a-guid").is_err());
    }
}
