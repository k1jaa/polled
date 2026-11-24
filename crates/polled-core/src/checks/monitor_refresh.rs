// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, Severity};
use crate::error::Result;
use crate::win::SystemEnv;

/// Allows a 2 Hz tolerance for fractional refresh rates reported as integers
/// (for example 143 Hz vs 144 Hz or 239 Hz vs 240 Hz).
const HZ_TOLERANCE: u32 = 2;

/// Report-only check that verifies every active display is running at its maximum
/// supported refresh rate for its current resolution.
pub struct MonitorRefreshCheck;

impl HealthCheck for MonitorRefreshCheck {
    fn id(&self) -> &str {
        "monitor_refresh"
    }

    fn name(&self) -> &str {
        "Monitor Refresh Rate"
    }

    fn can_fix(&self) -> bool {
        false
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let displays = env.list_display_modes()?;
        if displays.is_empty() {
            return Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: "No active desktop displays reported".to_string(),
                recommended_value: "Maximum supported Hz per monitor".to_string(),
                reason: "Could not read display refresh rates from Windows display settings."
                    .to_string(),
                severity: Severity::Info,
                can_fix: false,

                is_optional_tweak: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,

                requires_reboot: false,
            });
        }

        let mut parts = Vec::with_capacity(displays.len());
        let mut any_primary_suboptimal = false;
        let mut any_secondary_suboptimal = false;

        for d in &displays {
            let short_name = d
                .device_name
                .strip_prefix(r"\\.\")
                .unwrap_or(&d.device_name);
            let tag = if d.is_primary { "Primary" } else { short_name };
            let below_max = d.max_supported_hz > d.current_hz.saturating_add(HZ_TOLERANCE);

            if below_max {
                if d.is_primary {
                    any_primary_suboptimal = true;
                } else {
                    any_secondary_suboptimal = true;
                }
                parts.push(format!(
                    "{tag}: {}x{} @ {} Hz (supports {} Hz)",
                    d.width, d.height, d.current_hz, d.max_supported_hz
                ));
            } else {
                parts.push(format!(
                    "{tag}: {}x{} @ {} Hz",
                    d.width, d.height, d.current_hz
                ));
            }
        }

        let severity = if any_primary_suboptimal {
            Severity::Bad
        } else if any_secondary_suboptimal {
            Severity::Warn
        } else {
            Severity::Ok
        };

        let reason = if any_primary_suboptimal || any_secondary_suboptimal {
            "A display is running below its maximum refresh rate at its current resolution (common after GPU driver or Windows updates). Raise it in Settings -> System -> Display -> Advanced display."
        } else {
            "Every connected monitor is running at its maximum supported refresh rate for its active resolution, minimizing visual motion latency."
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value: parts.join(" · "),
            recommended_value: "Maximum supported Hz per monitor".to_string(),
            reason: reason.to_string(),
            severity,
            can_fix: false,

            is_optional_tweak: false,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,

            requires_reboot: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;
    use crate::win::DisplayModeInfo;

    #[test]
    fn flags_primary_monitor_stuck_at_60hz_as_bad() {
        let env = MockEnv::new();
        env.set_displays(vec![DisplayModeInfo {
            device_name: r"\\.\DISPLAY1".to_string(),
            friendly_name: "Gaming Monitor".to_string(),
            is_primary: true,
            width: 2560,
            height: 1440,
            current_hz: 60,
            max_supported_hz: 240,
        }]);

        let report = MonitorRefreshCheck.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Bad);
        assert!(report.current_value.contains("60 Hz"));
        assert!(report.current_value.contains("240 Hz"));
    }

    #[test]
    fn allows_fractional_hz_tolerance_and_warns_on_secondary() {
        let env = MockEnv::new();
        env.set_displays(vec![
            DisplayModeInfo {
                device_name: r"\\.\DISPLAY1".to_string(),
                friendly_name: "Primary".to_string(),
                is_primary: true,
                width: 2560,
                height: 1440,
                current_hz: 239,
                max_supported_hz: 240,
            },
            DisplayModeInfo {
                device_name: r"\\.\DISPLAY2".to_string(),
                friendly_name: "Secondary".to_string(),
                is_primary: false,
                width: 1920,
                height: 1080,
                current_hz: 60,
                max_supported_hz: 144,
            },
        ]);

        let report = MonitorRefreshCheck.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Warn);
    }
}
