// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::profiles::{GameProfile, ProfileStore};

/// Normalizes a full executable path or filename into a lowercase `.exe` basename.
///
/// Example: `C:\Riot Games\VALORANT\VALORANT-Win64-Shipping.exe` -> `valorant-win64-shipping.exe`
pub fn normalize_exe_name(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('"');
    let basename = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed).trim();
    basename.to_ascii_lowercase()
}

/// Resolves the matching profile for the current foreground executable, falling back
/// to the default desktop profile when no game profile matches.
pub fn match_profile<'a>(
    store: &'a ProfileStore,
    foreground_exe: Option<&str>,
) -> Option<&'a GameProfile> {
    if let Some(exe) = foreground_exe {
        let target = normalize_exe_name(exe);
        if !target.is_empty() {
            if let Some(found) = store.profiles.iter().find(|p| {
                !p.is_desktop_default
                    && p.executables
                        .iter()
                        .any(|candidate| normalize_exe_name(candidate) == target)
            }) {
                return Some(found);
            }
        }
    }

    store
        .profiles
        .iter()
        .find(|p| p.is_desktop_default)
        .or_else(|| store.profiles.iter().find(|p| p.id == "desktop"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::MouseSettings;
    use std::collections::BTreeMap;

    fn sample_store() -> ProfileStore {
        let mut store = ProfileStore::default();
        store.upsert_profile(GameProfile {
            id: "val".to_string(),
            name: "VALORANT".to_string(),
            is_desktop_default: false,
            executables: vec!["VALORANT-Win64-Shipping.exe".to_string()],
            mouse: MouseSettings {
                dpi: Some(1600),
                ..Default::default()
            },
            device_overrides: BTreeMap::new(),
            windows_pointer_speed: None,
            author: None,
            notes: None,
        });
        store.upsert_profile(GameProfile {
            id: "tac_fps".to_string(),
            name: "Source FPS".to_string(),
            is_desktop_default: false,
            executables: vec!["cs2.exe".to_string(), "hl2.exe".to_string()],
            mouse: MouseSettings {
                dpi: Some(800),
                ..Default::default()
            },
            device_overrides: BTreeMap::new(),
            windows_pointer_speed: None,
            author: None,
            notes: None,
        });
        store
    }

    #[test]
    fn matches_case_insensitively_and_strips_paths() {
        let store = sample_store();
        let matched = match_profile(
            &store,
            Some(r"C:\Riot Games\VALORANT\live\ShooterGame\Binaries\Win64\valorant-win64-shipping.exe"),
        )
        .unwrap();
        assert_eq!(matched.id, "val");

        let matched2 = match_profile(&store, Some("CS2.EXE")).unwrap();
        assert_eq!(matched2.id, "tac_fps");
    }

    #[test]
    fn falls_back_to_desktop_for_unmatched_or_missing_exe() {
        let store = sample_store();
        assert_eq!(
            match_profile(&store, Some("chrome.exe")).unwrap().id,
            "desktop"
        );
        assert_eq!(match_profile(&store, None).unwrap().id, "desktop");
    }
}
