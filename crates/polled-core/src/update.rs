// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Strictly on-demand GitHub Releases update checker backed by native Win32 `WinHTTP`
//! (zero external HTTP crates, zero background telemetry or startup calls).

use serde::{Deserialize, Serialize};

use crate::error::{PolledError, Result};

const GITHUB_API_HOST: &str = "api.github.com";
const GITHUB_LATEST_PATH: &str = "/repos/k1jaa/polled/releases/latest";
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// Public GitHub Releases page opened when the user chooses to download an update.
pub const GITHUB_RELEASES_WEB_URL: &str = "https://github.com/k1jaa/polled/releases";

/// Result of an explicit user-triggered check against GitHub Releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub release_url: String,
    pub published_at: Option<String>,
    pub notes_preview: String,
}

#[derive(Debug, Deserialize)]
struct GitHubReleasePayload {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    body: Option<String>,
}

/// Parses a `vX.Y.Z` or `X.Y.Z` version string into `(major, minor, patch)`.
pub fn parse_semver_tuple(raw: &str) -> Option<(u32, u32, u32)> {
    let trimmed = raw
        .trim()
        .strip_prefix('v')
        .or_else(|| raw.trim().strip_prefix('V'))
        .unwrap_or(raw.trim());
    let core = trimmed.split('-').next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;
    let patch = parts.next()?.parse::<u32>().ok()?;
    Some((major, minor, patch))
}

/// Returns `true` if `latest` is strictly newer than `current` using SemVer numeric comparison.
pub fn is_newer_version(current: &str, latest: &str) -> bool {
    match (parse_semver_tuple(current), parse_semver_tuple(latest)) {
        (Some(cur), Some(lat)) => lat > cur,
        _ => false,
    }
}

fn summarize_release_notes(body: Option<&str>) -> String {
    let raw = body.unwrap_or_default().trim();
    if raw.is_empty() {
        return "See the GitHub release page for full changelog and SHA256SUMS.txt.".to_string();
    }
    let lines: Vec<&str> = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(6)
        .collect();
    let joined = lines.join("\n");
    if joined.len() > 420 {
        format!("{}...", &joined[..417])
    } else {
        joined
    }
}

pub fn parse_github_release_json(
    json_bytes: &[u8],
    current_version: &str,
) -> Result<UpdateCheckResult> {
    let payload: GitHubReleasePayload = serde_json::from_slice(json_bytes)?;
    let latest_clean = payload
        .tag_name
        .trim()
        .trim_start_matches(['v', 'V'])
        .to_string();
    let update_available = is_newer_version(current_version, &latest_clean);
    Ok(UpdateCheckResult {
        current_version: current_version.to_string(),
        latest_version: latest_clean,
        update_available,
        release_url: payload.html_url,
        published_at: payload.published_at,
        notes_preview: summarize_release_notes(payload.body.as_deref()),
    })
}

/// Queries GitHub Releases API once over TLS (`WinHTTP`) to compare against `CARGO_PKG_VERSION`.
///
/// Only ever invoked when the user explicitly clicks **Check for Updates** or passes `--check-update`.
pub fn check_latest_github_release() -> Result<UpdateCheckResult> {
    #[cfg(windows)]
    {
        let body = winhttp_get_https(GITHUB_API_HOST, GITHUB_LATEST_PATH)?;
        parse_github_release_json(&body, env!("CARGO_PKG_VERSION"))
    }
    #[cfg(not(windows))]
    {
        Err(PolledError::Win32 {
            context: "WinHTTP".to_string(),
            message: "Update check is only available on Windows.".to_string(),
        })
    }
}

#[cfg(windows)]
fn winhttp_get_https(host: &str, path: &str) -> Result<Vec<u8>> {
    use windows::core::PCWSTR;
    use windows::Win32::Networking::WinHttp::{
        WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpReadData,
        WinHttpReceiveResponse, WinHttpSendRequest, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
        WINHTTP_FLAG_SECURE,
    };

    struct WinHttpHandle(*mut core::ffi::c_void);
    impl Drop for WinHttpHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: Handle was returned by a `WinHttp*` open/connect function.
                unsafe {
                    let _ = WinHttpCloseHandle(self.0);
                }
            }
        }
    }

    let to_wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };

    let ua = to_wide(&format!("Polled/{}", env!("CARGO_PKG_VERSION")));
    let host_w = to_wide(host);
    let verb_w = to_wide("GET");
    let path_w = to_wide(path);
    let headers_w: Vec<u16> = "Accept: application/vnd.github+json\r\n"
        .encode_utf16()
        .collect();

    // SAFETY: All wide strings stay alive across the synchronous WinHTTP calls; handles close on Drop.
    let session = WinHttpHandle(unsafe {
        WinHttpOpen(
            PCWSTR(ua.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    });
    if session.0.is_null() {
        return Err(PolledError::Win32 {
            context: "WinHttpOpen".to_string(),
            message: "Failed to initialize WinHTTP session".to_string(),
        });
    }

    let connect =
        WinHttpHandle(unsafe { WinHttpConnect(session.0, PCWSTR(host_w.as_ptr()), 443, 0) });
    if connect.0.is_null() {
        return Err(PolledError::Win32 {
            context: "WinHttpConnect".to_string(),
            message: format!("Could not connect to {host}"),
        });
    }

    let request = WinHttpHandle(unsafe {
        WinHttpOpenRequest(
            connect.0,
            PCWSTR(verb_w.as_ptr()),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            core::ptr::null(),
            WINHTTP_FLAG_SECURE,
        )
    });
    if request.0.is_null() {
        return Err(PolledError::Win32 {
            context: "WinHttpOpenRequest".to_string(),
            message: "Failed to create HTTPS request".to_string(),
        });
    }

    unsafe {
        WinHttpSendRequest(request.0, Some(&headers_w), None, 0, 0, 0).map_err(|e| {
            PolledError::Win32 {
                context: "WinHttpSendRequest".to_string(),
                message: format!("HTTPS request failed (offline or blocked): {e}"),
            }
        })?;
        WinHttpReceiveResponse(request.0, core::ptr::null_mut()).map_err(|e| {
            PolledError::Win32 {
                context: "WinHttpReceiveResponse".to_string(),
                message: format!("No HTTPS response from GitHub: {e}"),
            }
        })?;
    }

    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let mut read = 0u32;
        unsafe {
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut read,
            )
            .map_err(|e| PolledError::Win32 {
                context: "WinHttpReadData".to_string(),
                message: format!("Failed reading HTTPS response: {e}"),
            })?;
        }
        if read == 0 {
            break;
        }
        let take = (read as usize).min(MAX_RESPONSE_BYTES.saturating_sub(out.len()));
        out.extend_from_slice(&chunk[..take]);
        if out.len() >= MAX_RESPONSE_BYTES {
            break;
        }
    }

    if out.is_empty() {
        return Err(PolledError::Win32 {
            context: "WinHttpReadData".to_string(),
            message: "Empty response from GitHub Releases API".to_string(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_semver_versions_accurately() {
        assert!(is_newer_version("0.11.0", "0.12.0"));
        assert!(is_newer_version("0.12.0", "v1.0.0"));
        assert!(!is_newer_version("0.12.0", "v0.12.0"));
        assert!(!is_newer_version("1.0.0", "0.12.0"));
        assert!(!is_newer_version("0.12.0", "invalid"));
    }

    #[test]
    fn parses_github_release_json_payload() {
        let json = br###"{
            "tag_name": "v1.0.0",
            "html_url": "https://github.com/k1jaa/polled/releases/tag/v1.0.0",
            "published_at": "2026-11-26T12:00:00Z",
            "body": "## Highlights\n- Reversible PCI MSI mode\n- Wooting keyboard switching"
        }"###;
        let res = parse_github_release_json(json, "0.12.0").unwrap();
        assert!(res.update_available);
        assert_eq!(res.latest_version, "1.0.0");
        assert!(res.notes_preview.contains("Wooting"));
    }
}
