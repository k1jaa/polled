// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! On-demand (and optional opt-in startup) GitHub Releases update checker and
//! SHA-256-verified in-app portable binary updater backed by native Win32 `WinHTTP`
//! (zero external HTTP/crypto crates, zero `.bat`/PowerShell scripts, offline by default).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{PolledError, Result};

const GITHUB_API_HOST: &str = "api.github.com";
const GITHUB_LATEST_PATH: &str = "/repos/k1jaa/polled/releases/latest";
const ALLOWED_RELEASE_DOWNLOAD_PREFIX: &str = "https://github.com/k1jaa/polled/releases/download/";
const MAX_METADATA_BYTES: usize = 256 * 1024;
const MAX_RELEASE_ZIP_BYTES: usize = 64 * 1024 * 1024;
const MIN_GUI_EXE_BYTES: u64 = 100 * 1024;
const MIN_CLI_EXE_BYTES: u64 = 32 * 1024;

/// Public GitHub Releases page opened when the user chooses to browse releases manually.
pub const GITHUB_RELEASES_WEB_URL: &str = "https://github.com/k1jaa/polled/releases";

/// Result of a check against GitHub Releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub release_url: String,
    pub published_at: Option<String>,
    pub notes_preview: String,
    #[serde(default)]
    pub zip_file_name: Option<String>,
    #[serde(default)]
    pub zip_download_url: Option<String>,
    #[serde(default)]
    pub checksums_download_url: Option<String>,
}

impl UpdateCheckResult {
    /// Returns `true` when both the portable Windows ZIP and `SHA256SUMS.txt` assets were
    /// advertised on the GitHub release with valid official download URLs.
    pub fn can_self_update(&self) -> bool {
        self.update_available
            && self
                .zip_download_url
                .as_deref()
                .is_some_and(is_allowed_release_download_url)
            && self
                .checksums_download_url
                .as_deref()
                .is_some_and(is_allowed_release_download_url)
            && self.zip_file_name.as_deref().is_some_and(|n| !n.is_empty())
    }
}

/// Result returned once new release binaries have been SHA-256 verified and swapped into place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledUpdate {
    pub version: String,
    pub install_dir: PathBuf,
    pub new_gui_exe: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Deserialize)]
struct GitHubReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Deserialize)]
struct GitHubReleasePayload {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GitHubReleaseAsset>,
}

/// Strictly validates that a release asset URL belongs to the official `k1jaa/polled` repo.
pub fn is_allowed_release_download_url(url: &str) -> bool {
    let trimmed = url.trim();
    trimmed.starts_with(ALLOWED_RELEASE_DOWNLOAD_PREFIX)
        && !trimmed.contains("..")
        && !trimmed.chars().any(char::is_whitespace)
}

/// Parses an `https://host/path` URL into `(host, path_and_query)`.
fn parse_https_url(url: &str) -> Result<(&str, &str)> {
    let rest = url
        .trim()
        .strip_prefix("https://")
        .ok_or_else(|| PolledError::Update(format!("Expected https:// URL, got '{url}'")))?;
    let (host, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    if host.is_empty() || host.contains('@') || host.contains(':') {
        return Err(PolledError::Update(format!(
            "Unsupported HTTPS host in '{url}'"
        )));
    }
    Ok((host, path))
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
        let mut end = 417;
        while !joined.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}...", &joined[..end])
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

    let expected_zip_name = format!("polled-v{latest_clean}-windows-x64.zip");
    let zip_asset = payload
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(&expected_zip_name))
        .or_else(|| {
            payload.assets.iter().find(|a| {
                let lower = a.name.to_ascii_lowercase();
                lower.starts_with("polled-") && lower.ends_with("-windows-x64.zip")
            })
        })
        .filter(|a| is_allowed_release_download_url(&a.browser_download_url));

    let checksums_asset = payload
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case("SHA256SUMS.txt"))
        .filter(|a| is_allowed_release_download_url(&a.browser_download_url));

    Ok(UpdateCheckResult {
        current_version: current_version.to_string(),
        latest_version: latest_clean,
        update_available,
        release_url: payload.html_url,
        published_at: payload.published_at,
        notes_preview: summarize_release_notes(payload.body.as_deref()),
        zip_file_name: zip_asset.map(|a| a.name.clone()),
        zip_download_url: zip_asset.map(|a| a.browser_download_url.clone()),
        checksums_download_url: checksums_asset.map(|a| a.browser_download_url.clone()),
    })
}

/// Computes a standard FIPS 180-4 SHA-256 digest as a 64-character lowercase hex string.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((data.len() + 9).div_ceil(64) * 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for block in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, chunk) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*chunk);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut hex = String::with_capacity(64);
    for word in h {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{word:08x}");
    }
    hex
}

/// Parses a `SHA256SUMS.txt` file and extracts the 64-char lowercase hex hash for `target_filename`.
pub fn parse_sha256sums_for_file(text: &str, target_filename: &str) -> Option<String> {
    let cleaned = text.trim_start_matches('\u{feff}');
    for line in cleaned.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let hash = parts.next()?.trim();
        let file = parts
            .next()?
            .trim()
            .trim_start_matches('*')
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default();
        if hash.len() == 64
            && hash.bytes().all(|b| b.is_ascii_hexdigit())
            && file.eq_ignore_ascii_case(target_filename)
        {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// Removes leftover `polled-gui.exe.old` / `polled.exe.old` backup files created during an
/// in-place binary swap. Best-effort and completely silent on error.
pub fn cleanup_stale_update_backups(install_dir: &Path) {
    for name in ["polled-gui.exe.old", "polled.exe.old"] {
        let p = install_dir.join(name);
        if p.exists() {
            let _ = fs::remove_file(p);
        }
    }
}

/// Verifies that `dir` is writable by the current (non-elevated) user before downloading.
fn ensure_dir_writable(dir: &Path) -> Result<()> {
    let probe = dir.join(format!(".polled-update-probe-{}.tmp", std::process::id()));
    match fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(_) => Err(PolledError::Update(format!(
            "Cannot write to '{}' (try moving Polled out of Program Files, or click Open Release to update manually).",
            dir.display()
        ))),
    }
}

/// Validates that `candidate` is a genuine Windows PE executable (`MZ` header, above minimum size).
fn validate_pe_binary(candidate: &Path, min_bytes: u64, label: &str) -> Result<()> {
    let meta = fs::metadata(candidate).map_err(|_| {
        PolledError::Update(format!("Downloaded release archive is missing '{label}'."))
    })?;
    if meta.len() < min_bytes {
        return Err(PolledError::Update(format!(
            "Extracted '{label}' is unexpectedly small ({} bytes); aborting update.",
            meta.len()
        )));
    }
    let header = fs::read(candidate).map_err(|source| PolledError::Io {
        path: candidate.to_path_buf(),
        source,
    })?;
    if header.len() < 2 || &header[..2] != b"MZ" {
        return Err(PolledError::Update(format!(
            "Extracted '{label}' is not a valid Windows executable; aborting update."
        )));
    }
    Ok(())
}

/// Locates the extracted release root (either directly in `stage_dir` or one folder deeper).
fn resolve_extracted_root(stage_dir: &Path) -> Result<PathBuf> {
    if stage_dir.join("polled-gui.exe").is_file() {
        return Ok(stage_dir.to_path_buf());
    }
    if let Ok(entries) = fs::read_dir(stage_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("polled-gui.exe").is_file() {
                return Ok(path);
            }
        }
    }
    Err(PolledError::Update(
        "Downloaded release ZIP did not contain polled-gui.exe.".to_string(),
    ))
}

/// Swaps `polled-gui.exe` (and `polled.exe` + bundled docs) from `extracted_root` into
/// `install_dir` using Windows's native rename-in-place trick (`*.exe` -> `*.exe.old`)
/// with automatic rollback if copying fails.
pub fn swap_extracted_binaries(install_dir: &Path, extracted_root: &Path) -> Result<PathBuf> {
    let src_gui = extracted_root.join("polled-gui.exe");
    let src_cli = extracted_root.join("polled.exe");
    validate_pe_binary(&src_gui, MIN_GUI_EXE_BYTES, "polled-gui.exe")?;
    validate_pe_binary(&src_cli, MIN_CLI_EXE_BYTES, "polled.exe")?;

    // Guard against zip-slip symlinks escaping `extracted_root`.
    let root_canon = fs::canonicalize(extracted_root).map_err(|source| PolledError::Io {
        path: extracted_root.to_path_buf(),
        source,
    })?;
    for p in [&src_gui, &src_cli] {
        let canon = fs::canonicalize(p).map_err(|source| PolledError::Io {
            path: p.to_path_buf(),
            source,
        })?;
        if !canon.starts_with(&root_canon) {
            return Err(PolledError::Update(
                "Refusing to install binary outside extracted staging directory.".to_string(),
            ));
        }
    }

    cleanup_stale_update_backups(install_dir);

    let dst_gui = install_dir.join("polled-gui.exe");
    let bak_gui = install_dir.join("polled-gui.exe.old");
    let dst_cli = install_dir.join("polled.exe");
    let bak_cli = install_dir.join("polled.exe.old");

    let had_gui = dst_gui.exists();
    if had_gui {
        fs::rename(&dst_gui, &bak_gui).map_err(|source| PolledError::Io {
            path: dst_gui.clone(),
            source,
        })?;
    }

    if let Err(source) = fs::copy(&src_gui, &dst_gui) {
        let _ = fs::remove_file(&dst_gui);
        if had_gui {
            let _ = fs::rename(&bak_gui, &dst_gui);
        }
        return Err(PolledError::Io {
            path: dst_gui,
            source,
        });
    }

    let had_cli = dst_cli.exists();
    if had_cli {
        if let Err(source) = fs::rename(&dst_cli, &bak_cli) {
            // Roll back `polled-gui.exe` so GUI and CLI stay in version lockstep.
            let _ = fs::remove_file(&dst_gui);
            if had_gui {
                let _ = fs::rename(&bak_gui, &dst_gui);
            }
            return Err(PolledError::Io {
                path: dst_cli,
                source,
            });
        }
    }

    if let Err(source) = fs::copy(&src_cli, &dst_cli) {
        let _ = fs::remove_file(&dst_cli);
        if had_cli {
            let _ = fs::rename(&bak_cli, &dst_cli);
        }
        let _ = fs::remove_file(&dst_gui);
        if had_gui {
            let _ = fs::rename(&bak_gui, &dst_gui);
        }
        return Err(PolledError::Io {
            path: dst_cli,
            source,
        });
    }

    // Best-effort refresh of bundled README / CHANGELOG / LICENSE / docs.
    for rel in ["LICENSE", "README.md", "CHANGELOG.md"] {
        let src = extracted_root.join(rel);
        if src.is_file() {
            let _ = fs::copy(&src, install_dir.join(rel));
        }
    }
    let src_docs = extracted_root.join("docs");
    if src_docs.is_dir() {
        let dst_docs = install_dir.join("docs");
        let _ = fs::create_dir_all(&dst_docs);
        for rel in ["COMPATIBILITY.md", "SYSTEM_MODIFICATIONS.md"] {
            let src = src_docs.join(rel);
            if src.is_file() {
                let _ = fs::copy(&src, dst_docs.join(rel));
            }
        }
    }

    Ok(dst_gui)
}

/// Downloads the release ZIP + `SHA256SUMS.txt` from GitHub Releases over WinHTTP,
/// verifies the SHA-256 checksum byte-for-byte, extracts with native Windows `tar.exe`,
/// and atomically swaps the binaries inside `install_dir`.
pub fn download_and_install_update<F>(
    info: &UpdateCheckResult,
    install_dir: &Path,
    mut on_progress: F,
) -> Result<InstalledUpdate>
where
    F: FnMut(&str),
{
    #[cfg(not(windows))]
    {
        let _ = (info, install_dir, &mut on_progress);
        Err(PolledError::Win32 {
            context: "UpdateInstaller".to_string(),
            message: "In-app update installation is only supported on Windows.".to_string(),
        })
    }

    #[cfg(windows)]
    {
        if !info.can_self_update() {
            return Err(PolledError::Update(
                "This release does not expose both a Windows x64 ZIP and SHA256SUMS.txt; use Open Release instead."
                    .to_string(),
            ));
        }
        let zip_name = info.zip_file_name.as_deref().unwrap_or_default();
        let zip_url = info.zip_download_url.as_deref().unwrap_or_default();
        let sums_url = info.checksums_download_url.as_deref().unwrap_or_default();

        ensure_dir_writable(install_dir)?;

        on_progress("Downloading SHA256SUMS.txt...");
        let sums_bytes = winhttp_get_https_url(sums_url, "text/plain", MAX_METADATA_BYTES)?;
        let sums_text = String::from_utf8(sums_bytes)
            .map_err(|_| PolledError::Update("SHA256SUMS.txt was not valid UTF-8.".to_string()))?;
        let expected_hash = parse_sha256sums_for_file(&sums_text, zip_name).ok_or_else(|| {
            PolledError::Update(format!(
                "SHA256SUMS.txt does not contain a checksum entry for '{zip_name}'."
            ))
        })?;

        on_progress(&format!("Downloading {zip_name}..."));
        let zip_bytes =
            winhttp_get_https_url(zip_url, "application/octet-stream", MAX_RELEASE_ZIP_BYTES)?;

        on_progress("Verifying SHA-256 checksum...");
        let actual_hash = sha256_hex(&zip_bytes);
        if !actual_hash.eq_ignore_ascii_case(&expected_hash) {
            return Err(PolledError::Update(format!(
                "SHA-256 mismatch for '{zip_name}'! Expected {expected_hash}, got {actual_hash}. Update aborted."
            )));
        }

        // Extract inside a private temp directory, then always clean it up via RAII guard.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let temp_root =
            std::env::temp_dir().join(format!("polled-update-{}-{nonce}", std::process::id()));
        let stage_dir = temp_root.join("stage");
        fs::create_dir_all(&stage_dir).map_err(|source| PolledError::Io {
            path: stage_dir.clone(),
            source,
        })?;

        struct TempDirGuard(PathBuf);
        impl Drop for TempDirGuard {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _temp_guard = TempDirGuard(temp_root.clone());

        let zip_path = temp_root.join(zip_name);
        fs::write(&zip_path, &zip_bytes).map_err(|source| PolledError::Io {
            path: zip_path.clone(),
            source,
        })?;

        on_progress("Extracting verified archive...");
        extract_zip_windows_tar(&zip_path, &stage_dir)?;
        let extracted_root = resolve_extracted_root(&stage_dir)?;

        on_progress("Replacing Polled binaries...");
        let new_gui_exe = swap_extracted_binaries(install_dir, &extracted_root)?;

        on_progress(&format!("Restarting into v{}...", info.latest_version));
        Ok(InstalledUpdate {
            version: info.latest_version.clone(),
            install_dir: install_dir.to_path_buf(),
            new_gui_exe,
            sha256: actual_hash,
        })
    }
}

#[cfg(windows)]
fn extract_zip_windows_tar(zip_path: &Path, stage_dir: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let system_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let tar_exe = system_root.join("System32").join("tar.exe");
    if !tar_exe.is_file() {
        return Err(PolledError::Win32 {
            context: "tar.exe".to_string(),
            message:
                "Windows built-in tar.exe was not found in System32; click Open Release to update manually."
                    .to_string(),
        });
    }

    let output = Command::new(&tar_exe)
        .arg("-xf")
        .arg(zip_path)
        .arg("-C")
        .arg(stage_dir)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|source| PolledError::Io {
            path: tar_exe,
            source,
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(PolledError::Win32 {
            context: "tar.exe".to_string(),
            message: if stderr.is_empty() {
                format!(
                    "Archive extraction failed (exit {:?}).",
                    output.status.code()
                )
            } else {
                format!("Archive extraction failed: {stderr}")
            },
        });
    }
    Ok(())
}

/// Queries GitHub Releases API once over TLS (`WinHTTP`) to compare against `CARGO_PKG_VERSION`.
///
/// Only invoked when the user explicitly clicks **Check for Updates**, passes `--check-update`,
/// or opts in to **Check for updates once on startup** in Settings.
pub fn check_latest_github_release() -> Result<UpdateCheckResult> {
    #[cfg(windows)]
    {
        let body = winhttp_get_https(
            GITHUB_API_HOST,
            GITHUB_LATEST_PATH,
            "application/vnd.github+json",
            MAX_METADATA_BYTES,
        )?;
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
fn winhttp_get_https_url(url: &str, accept: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let (host, path) = parse_https_url(url)?;
    winhttp_get_https(host, path, accept, max_bytes)
}

#[cfg(windows)]
fn winhttp_get_https(host: &str, path: &str, accept: &str, max_bytes: usize) -> Result<Vec<u8>> {
    use windows::core::PCWSTR;
    use windows::Win32::Networking::WinHttp::{
        WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
        WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER,
        WINHTTP_QUERY_STATUS_CODE,
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
    let headers_w: Vec<u16> = format!("Accept: {accept}\r\n").encode_utf16().collect();

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

    unsafe {
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 30_000, 30_000);
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

    let mut status_code: u32 = 0;
    let mut status_len: u32 = std::mem::size_of::<u32>() as u32;
    let status_ok = unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status_code as *mut u32).cast()),
            &mut status_len,
            core::ptr::null_mut(),
        )
        .is_ok()
    };
    if status_ok && status_code != 200 {
        return Err(PolledError::Win32 {
            context: "WinHttpQueryHeaders".to_string(),
            message: format!("GitHub returned HTTP {status_code} for {path}"),
        });
    }

    let mut out = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
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
        if out.len().saturating_add(read as usize) > max_bytes {
            return Err(PolledError::Update(format!(
                "Response from {host} exceeded safety cap ({max_bytes} bytes)."
            )));
        }
        out.extend_from_slice(&chunk[..read as usize]);
    }

    if out.is_empty() {
        return Err(PolledError::Win32 {
            context: "WinHttpReadData".to_string(),
            message: "Empty response from GitHub".to_string(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn compares_semver_versions_accurately() {
        assert!(is_newer_version("0.11.0", "0.12.0"));
        assert!(is_newer_version("0.12.0", "v1.0.0"));
        assert!(!is_newer_version("0.12.0", "v0.12.0"));
        assert!(!is_newer_version("1.0.0", "0.12.0"));
        assert!(!is_newer_version("0.12.0", "invalid"));
    }

    #[test]
    fn parses_github_release_json_payload_and_assets() {
        let json = br###"{
            "tag_name": "v1.0.0",
            "html_url": "https://github.com/k1jaa/polled/releases/tag/v1.0.0",
            "published_at": "2026-11-26T12:00:00Z",
            "body": "## Highlights\n- Reversible PCI MSI mode\n- Wooting keyboard switching",
            "assets": [
                {
                    "name": "polled-v1.0.0-windows-x64.zip",
                    "browser_download_url": "https://github.com/k1jaa/polled/releases/download/v1.0.0/polled-v1.0.0-windows-x64.zip"
                },
                {
                    "name": "SHA256SUMS.txt",
                    "browser_download_url": "https://github.com/k1jaa/polled/releases/download/v1.0.0/SHA256SUMS.txt"
                }
            ]
        }"###;
        let res = parse_github_release_json(json, "0.12.0").unwrap();
        assert!(res.update_available);
        assert!(res.can_self_update());
        assert_eq!(res.latest_version, "1.0.0");
        assert_eq!(
            res.zip_file_name.as_deref(),
            Some("polled-v1.0.0-windows-x64.zip")
        );
        assert!(res.notes_preview.contains("Wooting"));

        // Reject untrusted third-party download URLs even if returned in JSON.
        let evil = br###"{
            "tag_name": "v1.0.0",
            "html_url": "https://github.com/k1jaa/polled/releases/tag/v1.0.0",
            "assets": [
                {
                    "name": "polled-v1.0.0-windows-x64.zip",
                    "browser_download_url": "https://evil.example.com/polled-v1.0.0-windows-x64.zip"
                },
                {
                    "name": "SHA256SUMS.txt",
                    "browser_download_url": "https://github.com/k1jaa/polled/releases/download/v1.0.0/SHA256SUMS.txt"
                }
            ]
        }"###;
        let evil_res = parse_github_release_json(evil, "0.12.0").unwrap();
        assert!(!evil_res.can_self_update());
    }

    #[test]
    fn sha256_matches_nist_test_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn parses_sha256sums_lines_cleanly() {
        let content = "\u{feff}BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD  *polled-v0.14.3-windows-x64.zip\r\n";
        assert_eq!(
            parse_sha256sums_for_file(content, "polled-v0.14.3-windows-x64.zip").as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(parse_sha256sums_for_file(content, "other.zip"), None);
    }

    #[test]
    fn swaps_validated_pe_binaries_and_cleans_up_backups() {
        let install = TempDir::new().unwrap();
        let stage = TempDir::new().unwrap();

        // Seed existing old binaries.
        fs::write(install.path().join("polled-gui.exe"), b"MZold_gui").unwrap();
        fs::write(install.path().join("polled.exe"), b"MZold_cli").unwrap();

        // Seed valid-sized PE stubs in stage.
        let mut new_gui = vec![0u8; 120 * 1024];
        new_gui[0] = b'M';
        new_gui[1] = b'Z';
        new_gui[2] = 0x99;
        let mut new_cli = vec![0u8; 40 * 1024];
        new_cli[0] = b'M';
        new_cli[1] = b'Z';
        new_cli[2] = 0x77;
        fs::write(stage.path().join("polled-gui.exe"), &new_gui).unwrap();
        fs::write(stage.path().join("polled.exe"), &new_cli).unwrap();
        fs::write(stage.path().join("README.md"), b"# New").unwrap();

        let dst_gui = swap_extracted_binaries(install.path(), stage.path()).unwrap();
        assert_eq!(dst_gui, install.path().join("polled-gui.exe"));
        assert_eq!(fs::read(&dst_gui).unwrap()[2], 0x99);
        assert_eq!(
            fs::read(install.path().join("polled.exe")).unwrap()[2],
            0x77
        );
        assert!(install.path().join("polled-gui.exe.old").is_file());
        assert!(install.path().join("polled.exe.old").is_file());
        assert_eq!(
            fs::read_to_string(install.path().join("README.md")).unwrap(),
            "# New"
        );

        cleanup_stale_update_backups(install.path());
        assert!(!install.path().join("polled-gui.exe.old").exists());
        assert!(!install.path().join("polled.exe.old").exists());
    }
}
