// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Read-only Smart Game Picker discovery:
//! - Active user applications (filtered free of Windows system noise)
//! - Installed Steam games (`libraryfolders.vdf` + `appmanifest_*.acf`)
//! - Installed Riot Games (`VALORANT`, `League of Legends`)
//! - Installed Epic Games (`EpicGamesLauncher\Data\Manifests\*.item`)
//!
//! 100% local, zero admin required, zero network calls.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, RefreshKind, System};

use crate::profiles::normalize_exe_name;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameSource {
    Running,
    Steam,
    Riot,
    Epic,
}

impl GameSource {
    pub fn label(self) -> &'static str {
        match self {
            GameSource::Running => "Running",
            GameSource::Steam => "Steam",
            GameSource::Riot => "Riot",
            GameSource::Epic => "Epic",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredGame {
    pub display_name: String,
    pub exe_name: String,
    pub source: GameSource,
}

const IGNORED_RUNNING_EXES: &[&str] = &[
    "aggregatorhost.exe",
    "applicationframehost.exe",
    "audiodg.exe",
    "backgroundtaskhost.exe",
    "conhost.exe",
    "csrss.exe",
    "ctfmon.exe",
    "dashost.exe",
    "dllhost.exe",
    "dwm.exe",
    "explorer.exe",
    "fontdrvhost.exe",
    "idle",
    "lockapp.exe",
    "lsass.exe",
    "memory compression",
    "mousocoreworker.exe",
    "msmpeng.exe",
    "nisSrv.exe",
    "openconsole.exe",
    "phoneexperiencehost.exe",
    "polled-gui.exe",
    "polled.exe",
    "registry",
    "runtimebroker.exe",
    "searchapp.exe",
    "searchhost.exe",
    "searchindexer.exe",
    "securityhealthservice.exe",
    "securityhealthsystray.exe",
    "services.exe",
    "shellexperiencehost.exe",
    "shellhost.exe",
    "sihost.exe",
    "smartscreen.exe",
    "smss.exe",
    "spoolsv.exe",
    "startmenuexperiencehost.exe",
    "svchost.exe",
    "system",
    "systemsettings.exe",
    "taskhostw.exe",
    "textinputhost.exe",
    "tiworker.exe",
    "trustedinstaller.exe",
    "werfault.exe",
    "widgetservice.exe",
    "widgets.exe",
    "wininit.exe",
    "winlogon.exe",
    "wmiprvse.exe",
    "wudfhost.exe",
];

const IGNORED_INSTALL_EXE_SUBSTRINGS: &[&str] = &[
    "unins",
    "uninstall",
    "crash",
    "reporter",
    "vc_redist",
    "vcredist",
    "dxsetup",
    "directx",
    "dotnet",
    "easyanticheat",
    "battleye",
    "beservice",
    "redist",
    "prereq",
    "ue4prereq",
    "ueprereq",
    "setup",
    "helper",
    "updater",
    "cleanup",
    "touchup",
    "webhelper",
    "cefprocess",
    "node.exe",
    "python",
];

/// Discovers currently running user processes (filtered free of Windows OS background services).
pub fn discover_running_apps() -> Vec<DiscoveredGame> {
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    );
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing(),
    );

    let mut by_exe: BTreeMap<String, String> = BTreeMap::new();
    for proc_info in sys.processes().values() {
        let raw_name = proc_info.name().to_string_lossy();
        let exe = normalize_exe_name(&raw_name);
        if exe.is_empty() || !exe.ends_with(".exe") {
            continue;
        }
        if IGNORED_RUNNING_EXES
            .iter()
            .any(|ignored| ignored.eq_ignore_ascii_case(&exe))
        {
            continue;
        }
        let stem = exe.trim_end_matches(".exe");
        let label = format_friendly_exe_label(stem);
        by_exe.entry(exe).or_insert(label);
    }

    by_exe
        .into_iter()
        .map(|(exe_name, display_name)| DiscoveredGame {
            display_name,
            exe_name,
            source: GameSource::Running,
        })
        .collect()
}

/// Discovers installed games across Steam, Riot Games, and Epic Games Launcher (read-only).
pub fn discover_installed_games() -> Vec<DiscoveredGame> {
    let mut results = Vec::new();
    let mut seen_exes: BTreeSet<String> = BTreeSet::new();

    for game in discover_steam_games()
        .into_iter()
        .chain(discover_riot_games())
        .chain(discover_epic_games())
    {
        if seen_exes.insert(game.exe_name.clone()) {
            results.push(game);
        }
    }

    results.sort_by(|a, b| {
        a.display_name
            .to_ascii_lowercase()
            .cmp(&b.display_name.to_ascii_lowercase())
    });
    results
}

fn format_friendly_exe_label(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    for (idx, ch) in stem.chars().enumerate() {
        if idx == 0 {
            for up in ch.to_uppercase() {
                out.push(up);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn discover_steam_games() -> Vec<DiscoveredGame> {
    let mut roots: Vec<PathBuf> = Vec::new();

    #[cfg(windows)]
    {
        if let Ok(Some(path_str)) = crate::win::registry::get_string(
            crate::win::RegHive::Hkcu,
            r"Software\Valve\Steam",
            "SteamPath",
        ) {
            let p = PathBuf::from(path_str.replace('/', "\\"));
            if p.is_dir() {
                roots.push(p);
            }
        }
    }

    for fallback in [
        r"C:\Program Files (x86)\Steam",
        r"C:\Program Files\Steam",
        r"D:\Steam",
        r"D:\SteamLibrary",
    ] {
        let p = PathBuf::from(fallback);
        if p.is_dir() && !roots.iter().any(|r| r.eq_ignore_ascii_case(&p)) {
            roots.push(p);
        }
    }

    let mut libraries: Vec<PathBuf> = Vec::new();
    for root in &roots {
        if !libraries.iter().any(|l| l.eq_ignore_ascii_case(root)) {
            libraries.push(root.clone());
        }
        let vdf = root.join("steamapps").join("libraryfolders.vdf");
        if let Ok(content) = fs::read_to_string(&vdf) {
            for lib in parse_steam_library_folders(&content) {
                if lib.is_dir() && !libraries.iter().any(|l| l.eq_ignore_ascii_case(&lib)) {
                    libraries.push(lib);
                }
            }
        }
    }

    let mut games = Vec::new();
    for lib in libraries {
        let steamapps = lib.join("steamapps");
        let Ok(entries) = fs::read_dir(&steamapps) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if !fname.starts_with("appmanifest_") || !fname.ends_with(".acf") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Some((name, installdir)) = parse_steam_acf(&text) else {
                continue;
            };
            let lower_name = name.to_ascii_lowercase();
            if lower_name.contains("steamworks common redistributables")
                || lower_name.contains("proton")
                || lower_name.contains("steam linux runtime")
                || lower_name.contains("dedicated server")
            {
                continue;
            }
            let game_dir = steamapps.join("common").join(installdir);
            if let Some(exe_name) = find_primary_game_exe(&game_dir) {
                games.push(DiscoveredGame {
                    display_name: name,
                    exe_name,
                    source: GameSource::Steam,
                });
            }
        }
    }

    games
}

fn discover_riot_games() -> Vec<DiscoveredGame> {
    let mut games = Vec::new();
    let mut riot_roots: Vec<PathBuf> = vec![PathBuf::from(r"C:\Riot Games")];

    let installs_json = PathBuf::from(r"C:\ProgramData\Riot Games\RiotClientInstalls.json");
    if let Ok(raw) = fs::read_to_string(&installs_json) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(obj) = val.as_object() {
                for v in obj.values() {
                    if let Some(s) = v.as_str() {
                        if let Some(parent) = Path::new(s).parent().and_then(Path::parent) {
                            let p = parent.to_path_buf();
                            if p.is_dir() && !riot_roots.iter().any(|r| r.eq_ignore_ascii_case(&p))
                            {
                                riot_roots.push(p);
                            }
                        }
                    }
                }
            }
        }
    }

    for root in riot_roots {
        let val_exe =
            root.join(r"VALORANT\live\ShooterGame\Binaries\Win64\VALORANT-Win64-Shipping.exe");
        if val_exe.is_file() {
            games.push(DiscoveredGame {
                display_name: "VALORANT".to_string(),
                exe_name: "valorant-win64-shipping.exe".to_string(),
                source: GameSource::Riot,
            });
        }
        let lol_exe = root.join(r"League of Legends\Game\League of Legends.exe");
        if lol_exe.is_file() {
            games.push(DiscoveredGame {
                display_name: "League of Legends".to_string(),
                exe_name: "league of legends.exe".to_string(),
                source: GameSource::Riot,
            });
        }
    }
    games
}

fn discover_epic_games() -> Vec<DiscoveredGame> {
    let manifests = PathBuf::from(r"C:\ProgramData\Epic\EpicGamesLauncher\Data\Manifests");
    let Ok(entries) = fs::read_dir(&manifests) else {
        return Vec::new();
    };
    let mut games = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("item") {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        if let Some(game) = parse_epic_item_json(&raw) {
            games.push(game);
        }
    }
    games
}

/// Parses `"path" "..."` entries out of Steam's `libraryfolders.vdf`.
pub fn parse_steam_library_folders(vdf: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in vdf.lines() {
        if let Some((key, val)) = parse_vdf_kv_line(line) {
            if key.eq_ignore_ascii_case("path") && !val.is_empty() {
                let unescaped = val.replace("\\\\", "\\");
                out.push(PathBuf::from(unescaped));
            }
        }
    }
    out
}

/// Parses `"name"` and `"installdir"` from a Steam `appmanifest_*.acf` file.
pub fn parse_steam_acf(acf: &str) -> Option<(String, String)> {
    let mut name = None;
    let mut installdir = None;
    for line in acf.lines() {
        if let Some((key, val)) = parse_vdf_kv_line(line) {
            if key.eq_ignore_ascii_case("name") {
                name = Some(val);
            } else if key.eq_ignore_ascii_case("installdir") {
                installdir = Some(val);
            }
        }
    }
    Some((name?, installdir?))
}

/// Parses an Epic Games Launcher `.item` manifest JSON into a `DiscoveredGame`.
pub fn parse_epic_item_json(raw: &str) -> Option<DiscoveredGame> {
    let val: serde_json::Value = serde_json::from_str(raw).ok()?;
    let display_name = val.get("DisplayName")?.as_str()?.trim().to_string();
    let launch_exe = val.get("LaunchExecutable")?.as_str()?.trim();
    if display_name.is_empty() || launch_exe.is_empty() {
        return None;
    }

    // Prefer Unreal Win64-Shipping game binary if installed directory is readable and launch exe is a launcher stub.
    let mut exe_name = normalize_exe_name(launch_exe);
    if exe_name.contains("launcher") {
        if let Some(loc) = val.get("InstallLocation").and_then(|v| v.as_str()) {
            if let Some(primary) = find_primary_game_exe(Path::new(loc)) {
                exe_name = primary;
            }
        }
    }
    if !exe_name.ends_with(".exe") {
        return None;
    }

    Some(DiscoveredGame {
        display_name,
        exe_name,
        source: GameSource::Epic,
    })
}

fn parse_vdf_kv_line(line: &str) -> Option<(String, String)> {
    let mut parts = Vec::with_capacity(2);
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            let mut token = String::new();
            while let Some(inner) = chars.next() {
                if inner == '\\' {
                    if let Some(next_ch) = chars.next() {
                        token.push('\\');
                        token.push(next_ch);
                    }
                } else if inner == '"' {
                    break;
                } else {
                    token.push(inner);
                }
            }
            parts.push(token);
            if parts.len() == 2 {
                break;
            }
        }
    }
    if parts.len() == 2 {
        Some((parts.remove(0), parts.remove(0)))
    } else {
        None
    }
}

/// Finds the most likely main game `.exe` inside a game's install directory (bounded shallow scan).
pub fn find_primary_game_exe(game_dir: &Path) -> Option<String> {
    if !game_dir.is_dir() {
        return None;
    }

    let mut candidates: Vec<(i32, u64, String)> = Vec::new();
    scan_exe_dir(game_dir, 0, &mut candidates);

    // Prefer Unreal Shipping binaries, then largest non-launcher binary.
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    candidates.into_iter().next().map(|(_, _, exe)| exe)
}

fn scan_exe_dir(dir: &Path, depth: usize, out: &mut Vec<(i32, u64, String)>) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_file() {
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            if !lower.ends_with(".exe") {
                continue;
            }
            if IGNORED_INSTALL_EXE_SUBSTRINGS
                .iter()
                .any(|bad| lower.contains(bad))
            {
                continue;
            }
            let mut score = 10i32;
            if lower.contains("win64-shipping") {
                score += 50;
            }
            if lower.contains("launcher") {
                score -= 25;
            }
            if lower.contains("eac") || lower.contains("anticheat") {
                score -= 30;
            }
            out.push((score, meta.len(), lower));
        } else if meta.is_dir() && depth < 3 {
            let Some(sub_name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let lower_sub = sub_name.to_ascii_lowercase();
            // Only recurse into common game binary directories to stay fast (< 50 ms).
            let interesting = matches!(
                lower_sub.as_str(),
                "game" | "bin" | "binaries" | "win64" | "x64" | "retail" | "shipping"
            ) || (depth == 0
                && !matches!(
                    lower_sub.as_str(),
                    "engine"
                        | "_commonredist"
                        | "redist"
                        | "directx"
                        | "vcredist"
                        | "content"
                        | "assets"
                        | "data"
                        | "movies"
                        | "sound"
                        | "audio"
                        | "textures"
                        | "maps"
                        | "localization"
                ));
            if interesting {
                scan_exe_dir(&path, depth + 1, out);
            }
        }
    }
}

trait PathEqIgnoreCase {
    fn eq_ignore_ascii_case(&self, other: &Path) -> bool;
}

impl PathEqIgnoreCase for Path {
    fn eq_ignore_ascii_case(&self, other: &Path) -> bool {
        self.to_string_lossy()
            .eq_ignore_ascii_case(&other.to_string_lossy())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_steam_vdf_acf_and_prefers_shipping_binary() {
        let vdf = r#"
"libraryfolders"
{
    "0"
    {
        "path"      "C:\\Program Files (x86)\\Steam"
    }
    "1"
    {
        "path"      "D:\\SteamLibrary"
    }
}
"#;
        let libs = parse_steam_library_folders(vdf);
        assert_eq!(libs.len(), 2);
        assert_eq!(libs[1], PathBuf::from(r"D:\SteamLibrary"));

        let acf = r#"
"AppState"
{
    "appid"         "730"
    "name"          "Counter-Strike 2"
    "installdir"    "Counter-Strike Global Offensive"
}
"#;
        let (name, dir) = parse_steam_acf(acf).unwrap();
        assert_eq!(name, "Counter-Strike 2");
        assert_eq!(dir, "Counter-Strike Global Offensive");

        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("game").join("bin").join("win64");
        fs::create_dir_all(&bin).unwrap();
        fs::write(tmp.path().join("unins000.exe"), [0u8; 2048]).unwrap();
        fs::write(tmp.path().join("GameLauncher.exe"), [0u8; 4096]).unwrap();
        fs::write(bin.join("cs2.exe"), [0u8; 8192]).unwrap();
        assert_eq!(
            find_primary_game_exe(tmp.path()).as_deref(),
            Some("cs2.exe")
        );
    }

    #[test]
    fn parses_epic_item_manifest_json() {
        let raw = r#"{
            "DisplayName": "Rocket League",
            "LaunchExecutable": "Binaries/Win64/RocketLeague.exe",
            "InstallLocation": "Z:\\NonExistent"
        }"#;
        let game = parse_epic_item_json(raw).unwrap();
        assert_eq!(game.display_name, "Rocket League");
        assert_eq!(game.exe_name, "rocketleague.exe");
        assert_eq!(game.source, GameSource::Epic);
    }
}
