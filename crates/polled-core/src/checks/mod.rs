// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod accel_curves;
pub mod accessibility_shortcuts;
pub mod cpu_throttle;
pub mod cursor_sanity;
pub mod custom_tweaks;
pub mod enhance_pointer_precision;
pub mod game_bar_capture;
pub mod game_mode;
pub mod global_timer_requests;
pub mod hags;
pub mod mmcss_gaming;
pub mod monitor_refresh;
pub mod mouse_queue_size;
pub mod pcie_aspm;
pub mod pointer_speed;
pub mod power_plan;
pub mod priority_separation;
pub mod runner;
pub mod snapshot;
pub mod state;
pub mod timer_resolution;
pub mod usb_msi_mode;
pub mod usb_power_management;
pub mod usb_selective_suspend;
pub mod vbs_memory_integrity;
pub mod vendor_software;
pub mod windowed_optimizations;

use serde::{Deserialize, Serialize};

use crate::error::{PolledError, Result};
use crate::win::SystemEnv;

pub use custom_tweaks::{
    curated_custom_tweak_presets, CustomTweakCheck, CustomTweakDef, CustomTweakStore,
    CustomValueKind,
};
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

/// Serializable result produced by scanning a single health check or reversible tweak.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReport {
    pub id: String,
    pub name: String,
    pub current_value: String,
    pub recommended_value: String,
    pub reason: String,
    pub severity: Severity,
    pub can_fix: bool,
    /// Optional latency tweaks are opt-in (never auto-applied by "Fix Safe Issues") and always reversible.
    #[serde(default)]
    pub is_optional_tweak: bool,
    pub requires_elevation: bool,
    pub elevation_reason: Option<String>,
    pub requires_relogin: bool,
    #[serde(default)]
    pub requires_reboot: bool,
}

/// Every input health check and reversible tweak implements this trait.
///
/// `capture_state()` is intentionally separate from `apply_fix()` so `HealthEngine`
/// always writes and flushes the baseline undo snapshot to disk *before* modifying
/// any Windows setting.
pub trait HealthCheck: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;

    fn can_fix(&self) -> bool {
        true
    }

    fn is_optional_tweak(&self) -> bool {
        false
    }

    fn requires_elevation(&self) -> bool {
        false
    }

    fn elevation_reason(&self) -> Option<&str> {
        None
    }

    fn requires_relogin(&self) -> bool {
        false
    }

    fn requires_reboot(&self) -> bool {
        false
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport>;

    /// Reads the exact original system state needed to cleanly undo a fix.
    fn capture_state(&self, _env: &dyn SystemEnv) -> Result<SavedValue> {
        Err(PolledError::NotFixable(self.id().to_string()))
    }

    /// Applies the recommended fix or optional tweak.
    fn apply_fix(&self, _env: &dyn SystemEnv) -> Result<()> {
        Err(PolledError::NotFixable(self.id().to_string()))
    }

    /// Restores the exact value captured prior to fixing.
    fn undo(&self, _env: &dyn SystemEnv, _saved: &SavedValue) -> Result<()> {
        Err(PolledError::NotFixable(self.id().to_string()))
    }
}

/// Returns all 25 registered built-in input health checks and optional tweaks in display order.
pub fn default_checks() -> Vec<Box<dyn HealthCheck>> {
    vec![
        // --- Core Mouse & Cursor Input ---
        Box::new(enhance_pointer_precision::EnhancePointerPrecisionCheck),
        Box::new(pointer_speed::PointerSpeedCheck),
        Box::new(accel_curves::AccelCurvesCheck),
        Box::new(cursor_sanity::CursorSanityCheck),
        Box::new(mouse_queue_size::MouseDataQueueSizeCheck),
        // --- USB, PCIe & Power Delivery ---
        Box::new(usb_selective_suspend::UsbSelectiveSuspendCheck),
        Box::new(usb_power_management::UsbPowerManagementCheck),
        Box::new(usb_msi_mode::UsbXhciMsiCheck),
        Box::new(pcie_aspm::PcieAspmCheck),
        Box::new(power_plan::PowerPlanCheck),
        Box::new(cpu_throttle::CpuProcessorStateCheck),
        // --- Timing, Display & Presentation ---
        Box::new(timer_resolution::TimerResolutionCheck),
        Box::new(monitor_refresh::MonitorRefreshCheck),
        Box::new(windowed_optimizations::WindowedOptimizationsCheck),
        Box::new(game_mode::GameModeCheck),
        Box::new(game_bar_capture::GameBarCaptureCheck),
        Box::new(hags::HagsCheck),
        Box::new(vbs_memory_integrity::VbsMemoryIntegrityCheck),
        // --- Optional Reversible Low-Latency Tweaks (Opt-In) ---
        Box::new(mmcss_gaming::MmcssGamingCheck),
        Box::new(priority_separation::PrioritySeparationCheck),
        Box::new(global_timer_requests::GlobalTimerRequestsCheck),
        // --- Accessibility & Background Bloat ---
        Box::new(accessibility_shortcuts::StickyKeysShortcutCheck),
        Box::new(accessibility_shortcuts::FilterKeysShortcutCheck),
        Box::new(accessibility_shortcuts::MouseKeysShortcutCheck),
        Box::new(vendor_software::VendorSoftwareCheck),
    ]
}
