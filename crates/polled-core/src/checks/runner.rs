// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::checks::custom_tweaks::CustomTweakStore;
use crate::checks::snapshot::BaselineSnapshot;
use crate::checks::state::{detect_regressions, PolledState, ScanSummary};
use crate::checks::{default_checks, CheckReport, HealthCheck, Severity};
use crate::error::{PolledError, Result};
use crate::paths::AppPaths;
use crate::win::SystemEnv;

/// Summary of an `--undo-all` operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoSummary {
    pub restored_ids: Vec<String>,
    pub failed: Vec<UndoFailure>,
    pub remaining_in_snapshot: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoFailure {
    pub check_id: String,
    pub error: String,
    pub requires_elevation: bool,
}

/// Orchestrates scanning, snapshot-before-fix, single/full undo, and regression tracking.
pub struct HealthEngine {
    checks: Vec<Box<dyn HealthCheck>>,
}

impl HealthEngine {
    pub fn with_default_checks() -> Self {
        Self {
            checks: default_checks(),
        }
    }

    /// Loads all built-in checks plus any user-defined reversible tweaks from `custom_tweaks.json`.
    pub fn with_default_and_custom_checks(paths: &AppPaths) -> Self {
        let mut checks = default_checks();
        let store = CustomTweakStore::load_or_default(&paths.custom_tweaks_path());
        checks.extend(store.into_checks());
        Self { checks }
    }

    pub fn with_checks(checks: Vec<Box<dyn HealthCheck>>) -> Self {
        Self { checks }
    }

    pub fn checks(&self) -> &[Box<dyn HealthCheck>] {
        &self.checks
    }

    fn find_check(&self, check_id: &str) -> Result<&dyn HealthCheck> {
        self.checks
            .iter()
            .find(|c| c.id() == check_id)
            .map(|c| c.as_ref())
            .ok_or_else(|| PolledError::CheckNotFound(check_id.to_string()))
    }

    /// Runs every registered health check and compares against the prior persisted scan.
    ///
    /// When `persist_state` is `true`, updates `state.json` with the latest Windows build
    /// and severities so future startup/update scans only notify if something got worse.
    pub fn scan_all(
        &self,
        env: &dyn SystemEnv,
        paths: &AppPaths,
        persist_state: bool,
    ) -> Result<ScanSummary> {
        let win_ver = env.windows_version()?.display_string();
        let prev_state = PolledState::load(&paths.state_path())?;
        let snapshot = BaselineSnapshot::load(&paths.baseline_snapshot_path())?;

        let mut reports = Vec::with_capacity(self.checks.len());
        for check in &self.checks {
            let report = match check.scan(env) {
                Ok(report) => report,
                Err(err) => CheckReport {
                    id: check.id().to_string(),
                    name: check.name().to_string(),
                    current_value: format!("Unavailable ({err})"),
                    recommended_value: "N/A".to_string(),
                    reason: "This setting could not be queried under current permissions."
                        .to_string(),
                    severity: Severity::Info,
                    can_fix: false,
                    is_optional_tweak: check.is_optional_tweak(),
                    requires_elevation: matches!(err, PolledError::ElevationRequired { .. }),
                    elevation_reason: check.elevation_reason().map(str::to_string),
                    requires_relogin: false,
                    requires_reboot: check.requires_reboot(),
                },
            };
            reports.push(report);
        }

        let regressions = detect_regressions(prev_state.as_ref(), &reports);
        let (windows_build_changed, previous_windows_version) = match &prev_state {
            Some(prev) => (
                prev.last_windows_version != win_ver,
                Some(prev.last_windows_version.clone()),
            ),
            None => (false, None),
        };

        if persist_state {
            let new_state = PolledState::from_reports(&win_ver, &reports);
            new_state.save_atomic(&paths.state_path())?;
        }

        let undoable_check_ids: Vec<String> = snapshot
            .as_ref()
            .map(|s| s.entries.keys().cloned().collect())
            .unwrap_or_default();

        Ok(ScanSummary {
            scanned_at: Utc::now(),
            windows_version: win_ver,
            windows_build_changed,
            previous_windows_version,
            has_undo_snapshot: !undoable_check_ids.is_empty(),
            undoable_check_ids,
            checks: reports,
            regressions,
        })
    }

    /// Saves a pre-fix JSON snapshot entry (first-write-wins) and applies the fix for `check_id`.
    pub fn fix_check(
        &self,
        check_id: &str,
        env: &dyn SystemEnv,
        paths: &AppPaths,
    ) -> Result<CheckReport> {
        let check = self.find_check(check_id)?;
        if !check.can_fix() {
            return Err(PolledError::NotFixable(check_id.to_string()));
        }

        // 1. Capture original value BEFORE mutating any Windows setting.
        let original = check.capture_state(env)?;
        let snapshot_path = paths.baseline_snapshot_path();
        let win_ver = env.windows_version()?.display_string();

        let mut snapshot = BaselineSnapshot::load(&snapshot_path)?
            .unwrap_or_else(|| BaselineSnapshot::new(win_ver));
        let inserted_new = snapshot.record_if_absent(check_id, original);

        // 2. Atomically persist snapshot to disk BEFORE calling apply_fix().
        if inserted_new {
            snapshot.save_atomic(&snapshot_path)?;
        }

        // 3. Apply the fix. If it fails (e.g. ElevationRequired), roll back the newly inserted
        //    snapshot entry so we do not claim an unapplied change can be undone.
        if let Err(err) = check.apply_fix(env) {
            if inserted_new {
                snapshot.entries.remove(check_id);
                if snapshot.is_empty() {
                    let _ = fs::remove_file(&snapshot_path);
                } else {
                    let _ = snapshot.save_atomic(&snapshot_path);
                }
            }
            return Err(err);
        }

        // 4. Re-scan the check and update state.json if present so fixing doesn't look stale.
        let updated = check.scan(env)?;
        if let Some(mut state) = PolledState::load(&paths.state_path())? {
            state
                .last_severities
                .insert(updated.id.clone(), updated.severity);
            state.last_scanned_at = Utc::now();
            let _ = state.save_atomic(&paths.state_path());
        }

        Ok(updated)
    }

    /// Fixes all currently unhealthy (`Warn` or `Bad`) core checks that do not require elevation.
    /// Optional latency tweaks are intentionally excluded so the user opts into each explicitly.
    pub fn fix_all_safe(&self, env: &dyn SystemEnv, paths: &AppPaths) -> Result<Vec<CheckReport>> {
        let mut fixed = Vec::new();
        for check in &self.checks {
            if !check.can_fix() || check.requires_elevation() || check.is_optional_tweak() {
                continue;
            }
            let report = check.scan(env)?;
            if report.severity >= Severity::Warn {
                fixed.push(self.fix_check(check.id(), env, paths)?);
            }
        }
        Ok(fixed)
    }

    /// Restores a single check from the saved baseline snapshot.
    pub fn undo_check(
        &self,
        check_id: &str,
        env: &dyn SystemEnv,
        paths: &AppPaths,
    ) -> Result<CheckReport> {
        let check = self.find_check(check_id)?;
        let snapshot_path = paths.baseline_snapshot_path();
        let mut snapshot = BaselineSnapshot::load(&snapshot_path)?
            .ok_or_else(|| PolledError::SnapshotMissing(snapshot_path.clone()))?;

        let entry = snapshot
            .entries
            .get(check_id)
            .ok_or_else(|| PolledError::InvalidSnapshot {
                check_id: check_id.to_string(),
                detail: "Check was not modified in the current undo snapshot".to_string(),
            })?
            .clone();

        check.undo(env, &entry.original)?;

        snapshot.entries.remove(check_id);
        if snapshot.is_empty() {
            let _ = fs::remove_file(&snapshot_path);
        } else {
            snapshot.save_atomic(&snapshot_path)?;
        }

        let updated = check.scan(env)?;
        if let Some(mut state) = PolledState::load(&paths.state_path())? {
            state
                .last_severities
                .insert(updated.id.clone(), updated.severity);
            state.last_scanned_at = Utc::now();
            let _ = state.save_atomic(&paths.state_path());
        }

        Ok(updated)
    }

    /// Restores every entry in the saved baseline snapshot.
    ///
    /// Entries that succeed are removed; any entry that fails (for instance because it needs
    /// elevation) remains in the snapshot file so no backup data is ever lost.
    pub fn undo_all(&self, env: &dyn SystemEnv, paths: &AppPaths) -> Result<UndoSummary> {
        let snapshot_path = paths.baseline_snapshot_path();
        let mut snapshot = BaselineSnapshot::load(&snapshot_path)?
            .ok_or_else(|| PolledError::SnapshotMissing(snapshot_path.clone()))?;

        let entries: Vec<_> = snapshot.entries.values().cloned().collect();
        let mut restored_ids = Vec::new();
        let mut failed = Vec::new();

        for entry in entries {
            let check = match self.find_check(&entry.check_id) {
                Ok(c) => c,
                Err(err) => {
                    failed.push(UndoFailure {
                        check_id: entry.check_id,
                        error: err.to_string(),
                        requires_elevation: false,
                    });
                    continue;
                }
            };

            match check.undo(env, &entry.original) {
                Ok(()) => {
                    snapshot.entries.remove(&entry.check_id);
                    restored_ids.push(entry.check_id);
                }
                Err(err) => {
                    let requires_elevation = matches!(err, PolledError::ElevationRequired { .. });
                    failed.push(UndoFailure {
                        check_id: entry.check_id,
                        error: err.to_string(),
                        requires_elevation,
                    });
                }
            }
        }

        let remaining = snapshot.entries.len();
        if snapshot.is_empty() {
            let _ = fs::remove_file(&snapshot_path);
        } else {
            snapshot.save_atomic(&snapshot_path)?;
        }

        // Refresh persisted severities so the user's intentional undo isn't flagged as a
        // surprise regression on the next startup scan.
        if paths.state_path().exists() {
            let _ = self.scan_all(env, paths, true);
        }

        Ok(UndoSummary {
            restored_ids,
            failed,
            remaining_in_snapshot: remaining,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::SavedValue;
    use crate::win::mock::MockEnv;
    use tempfile::TempDir;

    struct DummySpeedCheck;

    impl HealthCheck for DummySpeedCheck {
        fn id(&self) -> &str {
            "dummy_speed"
        }

        fn name(&self) -> &str {
            "Dummy Pointer Speed"
        }

        fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
            let speed = env.get_mouse_speed()?;
            Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: format!("{speed}/20"),
                recommended_value: "10/20".to_string(),
                reason: "Keeps 1:1 mouse scaling".to_string(),
                severity: if speed == 10 {
                    Severity::Ok
                } else {
                    Severity::Warn
                },
                can_fix: true,
                is_optional_tweak: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,
                requires_reboot: false,
            })
        }

        fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
            Ok(SavedValue::MouseSpeed(env.get_mouse_speed()?))
        }

        fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
            env.set_mouse_speed(10)
        }

        fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
            match saved {
                SavedValue::MouseSpeed(v) => env.set_mouse_speed(*v),
                _ => Err(PolledError::InvalidSnapshot {
                    check_id: self.id().to_string(),
                    detail: "Expected MouseSpeed".to_string(),
                }),
            }
        }
    }

    #[test]
    fn fix_saves_snapshot_before_mutating_and_undo_all_restores_original() {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::from_root(dir.path());
        let env = MockEnv::new();
        env.set_mouse_speed(16).unwrap();

        let engine = HealthEngine::with_checks(vec![Box::new(DummySpeedCheck)]);

        // Initial scan finds Warn
        let scan1 = engine.scan_all(&env, &paths, true).unwrap();
        assert_eq!(scan1.checks[0].severity, Severity::Warn);
        assert!(!scan1.has_undo_snapshot);

        // Fix persists snapshot and sets speed to 10
        let fixed = engine.fix_check("dummy_speed", &env, &paths).unwrap();
        assert_eq!(fixed.severity, Severity::Ok);
        assert_eq!(env.get_mouse_speed().unwrap(), 10);
        assert!(paths.baseline_snapshot_path().exists());

        // Undo all restores original 16 and cleans up snapshot file
        let undo = engine.undo_all(&env, &paths).unwrap();
        assert_eq!(undo.restored_ids, vec!["dummy_speed"]);
        assert!(undo.failed.is_empty());
        assert_eq!(undo.remaining_in_snapshot, 0);
        assert_eq!(env.get_mouse_speed().unwrap(), 16);
        assert!(!paths.baseline_snapshot_path().exists());
    }

    #[test]
    fn detects_windows_update_and_setting_regression() {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::from_root(dir.path());
        let env = MockEnv::new();
        env.set_windows_version("26200", 1000);
        env.set_mouse_speed(10).unwrap();

        let engine = HealthEngine::with_checks(vec![Box::new(DummySpeedCheck)]);
        let scan1 = engine.scan_all(&env, &paths, true).unwrap();
        assert!(!scan1.windows_build_changed);
        assert!(scan1.regressions.is_empty());

        // Simulate Windows Update resetting pointer speed
        env.set_windows_version("26200", 2000);
        env.set_mouse_speed(14).unwrap();

        let scan2 = engine.scan_all(&env, &paths, true).unwrap();
        assert!(scan2.windows_build_changed);
        assert_eq!(
            scan2.previous_windows_version.as_deref(),
            Some("26200.1000")
        );
        assert_eq!(scan2.regressions.len(), 1);
        assert_eq!(scan2.regressions[0].check_id, "dummy_speed");
    }

    #[test]
    fn default_engine_registers_all_25_checks_and_roundtrips_safe_fixes() {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::from_root(dir.path());
        let env = MockEnv::new();
        env.set_mouse_speed(14).unwrap();
        env.reg_set_dword(
            crate::win::RegHive::Hkcu,
            r"Software\Microsoft\Windows\CurrentVersion\GameDVR",
            "AppCaptureEnabled",
            1,
        )
        .unwrap();

        let engine = HealthEngine::with_default_checks();
        assert_eq!(engine.checks().len(), 25);

        let initial = engine.scan_all(&env, &paths, true).unwrap();
        assert_eq!(initial.checks.len(), 25);

        // `fix_all_safe` fixes safe core issues (pointer_speed, game_bar_capture, windowed_optimizations)
        // and intentionally skips optional tweaks (mmcss_gaming, win32_priority_separation, etc.).
        let fixed = engine.fix_all_safe(&env, &paths).unwrap();
        assert!(fixed.iter().any(|r| r.id == "pointer_speed"));
        assert!(fixed.iter().any(|r| r.id == "game_bar_capture"));
        assert!(fixed.iter().any(|r| r.id == "windowed_optimizations"));
        assert!(!fixed.iter().any(|r| r.is_optional_tweak));
        assert_eq!(env.get_mouse_speed().unwrap(), 10);

        let undo = engine.undo_all(&env, &paths).unwrap();
        assert!(undo.failed.is_empty());
        assert_eq!(env.get_mouse_speed().unwrap(), 14);
        assert_eq!(
            env.reg_get_dword(
                crate::win::RegHive::Hkcu,
                r"Software\Microsoft\Windows\CurrentVersion\GameDVR",
                "AppCaptureEnabled"
            )
            .unwrap(),
            Some(1)
        );
    }

    #[test]
    fn loads_applies_and_reverts_user_custom_tweaks_from_disk() {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::from_root(dir.path());
        let env = MockEnv::new();

        let mut store = CustomTweakStore::default();
        store
            .upsert(crate::checks::CustomTweakDef {
                id: "custom_hover_fast".to_string(),
                name: "Fast Hover".to_string(),
                hive: crate::win::RegHive::Hkcu,
                subkey: r"Control Panel\Mouse".to_string(),
                value_name: "MouseHoverTime".to_string(),
                target: crate::checks::CustomValueKind::String("10".to_string()),
                requires_reboot: false,
                notes: "Snappy hover".to_string(),
            })
            .unwrap();
        store.save_atomic(&paths.custom_tweaks_path()).unwrap();

        let engine = HealthEngine::with_default_and_custom_checks(&paths);
        assert_eq!(engine.checks().len(), 26);

        let fixed = engine.fix_check("custom_hover_fast", &env, &paths).unwrap();
        assert_eq!(fixed.severity, Severity::Ok);
        assert_eq!(
            env.reg_get_string(
                crate::win::RegHive::Hkcu,
                r"Control Panel\Mouse",
                "MouseHoverTime"
            )
            .unwrap()
            .as_deref(),
            Some("10")
        );

        let undone = engine
            .undo_check("custom_hover_fast", &env, &paths)
            .unwrap();
        assert_eq!(undone.severity, Severity::Info);
        assert_eq!(
            env.reg_get_string(
                crate::win::RegHive::Hkcu,
                r"Control Panel\Mouse",
                "MouseHoverTime"
            )
            .unwrap(),
            None
        );
    }
}
