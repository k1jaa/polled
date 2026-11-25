// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{PolledError, Result};

/// Resolves per-user data paths under `%LOCALAPPDATA%\Polled` (no admin required).
#[derive(Debug, Clone)]
pub struct AppPaths {
    root: PathBuf,
}

impl AppPaths {
    /// Uses `%LOCALAPPDATA%\Polled` on Windows, falling back to a local `.polled` folder.
    pub fn default_user_dir() -> Self {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            root: base.join("Polled"),
        }
    }

    /// Creates an `AppPaths` rooted at a custom directory (useful for tests and portable mode).
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Ensures the application data directory exists on disk.
    pub fn ensure_dir(&self) -> Result<()> {
        fs::create_dir_all(&self.root).map_err(|source| PolledError::Io {
            path: self.root.clone(),
            source,
        })
    }

    /// Stores the pre-fix baseline values used by `--undo-all` / "Undo all".
    pub fn baseline_snapshot_path(&self) -> PathBuf {
        self.root.join("baseline_snapshot.json")
    }

    /// Stores the last seen Windows build number and previous scan severities for regression detection.
    pub fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    /// Stores Phase 2 per-game peripheral profiles.
    pub fn profiles_path(&self) -> PathBuf {
        self.root.join("profiles.json")
    }

    /// Stores GUI preferences (theme, startup auto-switch state).
    pub fn settings_path(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// Stores user-defined reversible registry tweaks.
    pub fn custom_tweaks_path(&self) -> PathBuf {
        self.root.join("custom_tweaks.json")
    }

    /// Directory where exported Input Lab PNG benchmark cards and CSV interval captures are saved.
    pub fn exports_dir(&self) -> PathBuf {
        self.root.join("exports")
    }

    /// Stores persistent Bench benchmark run history for cross-session Before/After A/B comparisons.
    pub fn bench_history_path(&self) -> PathBuf {
        self.root.join("bench_history.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn resolves_expected_file_names() {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::from_root(dir.path());
        paths.ensure_dir().unwrap();

        assert_eq!(
            paths.baseline_snapshot_path(),
            dir.path().join("baseline_snapshot.json")
        );
        assert_eq!(paths.state_path(), dir.path().join("state.json"));
        assert_eq!(paths.profiles_path(), dir.path().join("profiles.json"));
        assert_eq!(paths.settings_path(), dir.path().join("settings.json"));
    }
}
