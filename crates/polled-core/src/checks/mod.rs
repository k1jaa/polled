// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod accel_curves;
pub mod accessibility_shortcuts;
pub mod enhance_pointer_precision;
pub mod game_mode;
pub mod hags;
pub mod pointer_speed;
pub mod power_plan;
pub mod runner;
pub mod snapshot;
pub mod state;
pub mod usb_selective_suspend;
pub mod vendor_software;

use serde::{Deserialize, Serialize};

use crate::error::{PolledError, Result};
use crate::win::SystemEnv;

pub use runner::{HealthEngine, UndoSummary};
pub use snapshot::{BaselineSnapshot, SavedValue, SnapshotEntry};
pub use state::{PolledState, Regression, ScanSummary};

/// Health severity ordered from best (`Ok`) to worst (`Bad`).
///
/// Deriving `PartialOrd` + `Ord` on an enum uses declaration order, so
/// `new_severity > old_severity` directly tells us whether a check got worse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Ok,
    Info,
    Warn,
    Bad,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Ok => "ok",
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Bad => "bad",
        }
    }
}

/// Serializable result produced by scanning a single health check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReport {
    pub id: String,
    pub name: String,
    pub current_value: String,
    pub recommended_value: String,
    pub reason: String,
    pub severity: Severity,
    pub can_fix: bool,
    pub requires_elevation: bool,
    pub elevation_reason: Option<String>,
    pub requires_relogin: bool,
}

/// Every Phase 1 input health check implements this trait.
///
/// `capture_state()` is intentionally separate from `apply_fix()` so `HealthEngine`
/// always writes and flushes the baseline undo snapshot to disk *before* modifying
/// any Windows setting.
pub trait HealthCheck: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;

    fn can_fix(&self) -> bool {
        true
    }

    fn requires_elevation(&self) -> bool {
        false
    }

    fn elevation_reason(&self) -> Option<&'static str> {
        None
    }

    fn requires_relogin(&self) -> bool {
        false
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport>;

    /// Reads the exact original system state needed to cleanly undo a fix.
    fn capture_state(&self, _env: &dyn SystemEnv) -> Result<SavedValue> {
        Err(PolledError::NotFixable(self.id().to_string()))
    }

    /// Applies the recommended fix.
    fn apply_fix(&self, _env: &dyn SystemEnv) -> Result<()> {
        Err(PolledError::NotFixable(self.id().to_string()))
    }

    /// Restores the exact value captured prior to fixing.
    fn undo(&self, _env: &dyn SystemEnv, _saved: &SavedValue) -> Result<()> {
        Err(PolledError::NotFixable(self.id().to_string()))
    }
}

/// Returns all 11 registered Phase 1 health checks in display order.
pub fn default_checks() -> Vec<Box<dyn HealthCheck>> {
    vec![
        Box::new(enhance_pointer_precision::EnhancePointerPrecisionCheck),
        Box::new(pointer_speed::PointerSpeedCheck),
        Box::new(accel_curves::AccelCurvesCheck),
        Box::new(usb_selective_suspend::UsbSelectiveSuspendCheck),
        Box::new(power_plan::PowerPlanCheck),
        Box::new(game_mode::GameModeCheck),
        Box::new(hags::HagsCheck),
        Box::new(accessibility_shortcuts::StickyKeysShortcutCheck),
        Box::new(accessibility_shortcuts::FilterKeysShortcutCheck),
        Box::new(accessibility_shortcuts::MouseKeysShortcutCheck),
        Box::new(vendor_software::VendorSoftwareCheck),
    ]
}
