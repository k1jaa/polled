// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::checks::snapshot::write_json_atomic;
use crate::checks::{CheckReport, Severity};
use crate::error::{PolledError, Result};

pub const STATE_SCHEMA_VERSION: u32 = 1;

/// Persisted scan history used to detect Windows updates and alert *only* on regressions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolledState {
    pub schema_version: u32,
    pub last_windows_version: String,
    pub last_scanned_at: DateTime<Utc>,
    pub last_severities: BTreeMap<String, Severity>,
}

impl PolledState {
    pub fn load(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(path).map_err(|source| PolledError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let state: Self = serde_json::from_str(&raw)?;
        Ok(Some(state))
    }

    pub fn save_atomic(&self, path: &Path) -> Result<()> {
        write_json_atomic(path, self)
    }

    pub fn from_reports(windows_version: impl Into<String>, reports: &[CheckReport]) -> Self {
        let last_severities = reports.iter().map(|r| (r.id.clone(), r.severity)).collect();
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            last_windows_version: windows_version.into(),
            last_scanned_at: Utc::now(),
            last_severities,
        }
    }
}

/// Describes a single health check whose status got worse since the previous scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Regression {
    pub check_id: String,
    pub name: String,
    pub previous: Severity,
    pub current: Severity,
    pub current_value: String,
    pub reason: String,
}

/// Complete result returned by `HealthEngine::scan_all`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanSummary {
    pub scanned_at: DateTime<Utc>,
    pub windows_version: String,
    pub windows_build_changed: bool,
    pub previous_windows_version: Option<String>,
    pub has_undo_snapshot: bool,
    pub undoable_check_ids: Vec<String>,
    pub checks: Vec<CheckReport>,
    pub regressions: Vec<Regression>,
}

/// Computes checks that strictly worsened into `Warn` or `Bad` since `previous_state`.
pub fn detect_regressions(
    previous_state: Option<&PolledState>,
    current_reports: &[CheckReport],
) -> Vec<Regression> {
    let Some(prev) = previous_state else {
        return Vec::new();
    };

    current_reports
        .iter()
        .filter_map(|report| {
            let old = *prev.last_severities.get(&report.id)?;
            if report.severity > old && report.severity >= Severity::Warn {
                Some(Regression {
                    check_id: report.id.clone(),
                    name: report.name.clone(),
                    previous: old,
                    current: report.severity,
                    current_value: report.current_value.clone(),
                    reason: report.reason.clone(),
                })
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(id: &str, severity: Severity) -> CheckReport {
        CheckReport {
            id: id.to_string(),
            name: id.to_string(),
            current_value: "val".to_string(),
            recommended_value: "rec".to_string(),
            reason: "reason".to_string(),
            severity,
            can_fix: true,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,
        }
    }

    #[test]
    fn detects_only_worsening_regressions() {
        let prev = PolledState::from_reports(
            "26200.1000",
            &[
                report("epp", Severity::Ok),
                report("speed", Severity::Warn),
                report("usb", Severity::Bad),
                report("hags", Severity::Ok),
            ],
        );

        let current = vec![
            report("epp", Severity::Bad),   // Ok -> Bad (regression!)
            report("speed", Severity::Bad), // Warn -> Bad (regression!)
            report("usb", Severity::Ok),    // Bad -> Ok (improved, not a regression)
            report("hags", Severity::Info), // Ok -> Info (informational, not a regression)
        ];

        let regressions = detect_regressions(Some(&prev), &current);
        assert_eq!(regressions.len(), 2);
        assert_eq!(regressions[0].check_id, "epp");
        assert_eq!(regressions[1].check_id, "speed");
    }

    #[test]
    fn first_scan_reports_no_regressions() {
        let current = vec![report("epp", Severity::Bad)];
        assert!(detect_regressions(None, &current).is_empty());
    }
}
