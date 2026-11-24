// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::ffi::OsStr;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use polled_core::AppPaths;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

/// Actions that may be delegated to an on-demand elevated `polled.exe` process via UAC.
pub enum ElevatedAction<'a> {
    Fix(&'a str),
    Undo(&'a str),
    UndoAll,
}

fn to_wide_null(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

/// Validates that a check ID contains only safe ASCII identifier characters.
fn validate_check_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(format!("Invalid check identifier: '{id}'"));
    }
    Ok(())
}

/// Locates `polled.exe` beside `polled-gui.exe` (falling back to `polled.exe` on `PATH`).
fn find_cli_binary() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("polled.exe");
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from("polled.exe")
}

/// Spawns `polled.exe` with the `"runas"` verb (Windows UAC) for a single explicit action,
/// passing the current user's `--data-dir` so snapshots stay in `%LOCALAPPDATA%\Polled`.
pub fn run_elevated_cli(action: ElevatedAction<'_>, paths: &AppPaths) -> Result<(), String> {
    let data_dir = paths.root().display().to_string();
    let args = match action {
        ElevatedAction::Fix(id) => {
            validate_check_id(id)?;
            format!(r#"--data-dir "{data_dir}" --fix {id}"#)
        }
        ElevatedAction::Undo(id) => {
            validate_check_id(id)?;
            format!(r#"--data-dir "{data_dir}" --undo {id}"#)
        }
        ElevatedAction::UndoAll => format!(r#"--data-dir "{data_dir}" --undo-all"#),
    };

    let cli_path = find_cli_binary();
    let verb_w = to_wide_null("runas");
    let file_w = to_wide_null(&cli_path.display().to_string());
    let params_w = to_wide_null(&args);

    let mut sei = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb_w.as_ptr()),
        lpFile: PCWSTR(file_w.as_ptr()),
        lpParameters: PCWSTR(params_w.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    // SAFETY: All UTF-16 buffers stay alive until after `ShellExecuteExW` returns.
    if let Err(err) = unsafe { ShellExecuteExW(&mut sei) } {
        if err.code() == ERROR_CANCELLED.to_hresult() {
            return Err("Administrator elevation was cancelled by the user.".to_string());
        }
        return Err(format!(
            "Failed to launch elevated helper '{}': {err}",
            cli_path.display()
        ));
    }

    if sei.hProcess.is_invalid() {
        return Err("Elevated helper launched without a valid process handle.".to_string());
    }

    // SAFETY: `sei.hProcess` is a valid process handle returned by `ShellExecuteExW`.
    let exit_code = unsafe {
        WaitForSingleObject(sei.hProcess, INFINITE);
        let mut code = 1u32;
        let _ = GetExitCodeProcess(sei.hProcess, &mut code);
        let _ = CloseHandle(sei.hProcess);
        code
    };

    if exit_code == 0 {
        Ok(())
    } else {
        Err(format!(
            "Elevated helper exited with code {exit_code}. Ensure '{}' is next to polled-gui.exe.",
            cli_path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_check_ids_strictly() {
        assert!(validate_check_id("usb_selective_suspend").is_ok());
        assert!(validate_check_id("pointer_speed").is_ok());
        assert!(validate_check_id("").is_err());
        assert!(validate_check_id("foo; calc.exe").is_err());
        assert!(validate_check_id("UPPER_CASE").is_err());
        assert!(validate_check_id("../escape").is_err());
    }
}
