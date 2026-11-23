// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::error::{PolledError, Result};
use crate::win::{registry, RegHive, WindowsVersion};

const CURRENT_VERSION_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

/// Reads `CurrentBuildNumber` and `UBR` (Update Build Revision) from HKLM (read-only, no admin).
pub fn read_windows_version() -> Result<WindowsVersion> {
    let build = registry::get_string(RegHive::Hklm, CURRENT_VERSION_KEY, "CurrentBuildNumber")?
        .ok_or_else(|| PolledError::Win32 {
            context: "read_windows_version".to_string(),
            message: "CurrentBuildNumber missing in registry".to_string(),
        })?;
    let ubr = registry::get_dword(RegHive::Hklm, CURRENT_VERSION_KEY, "UBR")?.unwrap_or(0);

    Ok(WindowsVersion { build, ubr })
}
