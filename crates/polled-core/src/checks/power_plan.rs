// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{PowerSchemeInfo, SystemEnv};

pub const GUID_POWER_SAVER: &str = "a1841308-3541-4fab-bc81-f71556f20b4a";
pub const GUID_BALANCED: &str = "381b4222-f694-41f0-9685-ff5bb260df2e";
pub const GUID_HIGH_PERFORMANCE: &str = "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c";
pub const GUID_ULTIMATE_PERFORMANCE: &str = "e9a42b02-d5df-448d-aa00-03f14749eb61";

/// Checks the active Windows power plan.
///
/// - Flags **Power Saver** as `Bad` (aggressive CPU downclocking and USB/PCIe latency).
/// - Treats **High Performance** / **Ultimate Performance** as `Ok`.
/// - Treats **Balanced** as `Info` when High/Ultimate Performance is available, or `Ok`
///   on Modern Standby (S0ix) devices where Windows exposes only Balanced.
pub struct PowerPlanCheck;

fn is_high_perf_plan(scheme: &PowerSchemeInfo) -> bool {
    if scheme.guid.eq_ignore_ascii_case(GUID_HIGH_PERFORMANCE)
        || scheme.guid.eq_ignore_ascii_case(GUID_ULTIMATE_PERFORMANCE)
    {
        return true;
    }
    let lower = scheme.name.to_ascii_lowercase();
    lower.contains("high performance")
        || lower.contains("ultimate performance")
        || lower.contains("bitsum highest")
}

fn is_power_saver_plan(scheme: &PowerSchemeInfo) -> bool {
    scheme.guid.eq_ignore_ascii_case(GUID_POWER_SAVER)
        || scheme.name.to_ascii_lowercase().contains("power saver")
}

fn pick_recommended_scheme(schemes: &[PowerSchemeInfo]) -> Option<&PowerSchemeInfo> {
    schemes
        .iter()
        .find(|s| s.guid.eq_ignore_ascii_case(GUID_HIGH_PERFORMANCE))
        .or_else(|| {
            schemes
                .iter()
                .find(|s| s.guid.eq_ignore_ascii_case(GUID_ULTIMATE_PERFORMANCE))
        })
        .or_else(|| schemes.iter().find(|s| is_high_perf_plan(s)))
        .or_else(|| {
            schemes
                .iter()
                .find(|s| s.guid.eq_ignore_ascii_case(GUID_BALANCED))
        })
}

impl HealthCheck for PowerPlanCheck {
    fn id(&self) -> &str {
        "power_plan"
    }

    fn name(&self) -> &str {
        "Active Power Plan"
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let active = env.get_active_power_scheme()?;
        let schemes = env.list_power_schemes()?;
        let has_high_perf = schemes.iter().any(is_high_perf_plan);

        let (severity, current_value, recommended_value, reason, can_fix) = if is_power_saver_plan(
            &active,
        ) {
            let target_name = pick_recommended_scheme(&schemes)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "High Performance / Balanced".to_string());
            (
                    Severity::Bad,
                    format!("{} ({})", active.name, active.guid),
                    target_name,
                    "Power Saver aggressively throttles CPU clocks and increases USB/DPC input latency."
                        .to_string(),
                    pick_recommended_scheme(&schemes).is_some(),
                )
        } else if is_high_perf_plan(&active) {
            (
                Severity::Ok,
                format!("{} ({})", active.name, active.guid),
                "High Performance (or Balanced on modern CPUs)".to_string(),
                "Active power plan avoids aggressive power throttling.".to_string(),
                true,
            )
        } else if has_high_perf {
            (
                    Severity::Info,
                    format!("{} ({})", active.name, active.guid),
                    "High Performance (Balanced is also recommended on Ryzen X3D / Intel hybrid CPUs)"
                        .to_string(),
                    "Balanced is fine on modern hybrid/X3D CPUs, but High Performance can reduce core wake latency on some systems."
                        .to_string(),
                    true,
                )
        } else {
            (
                    Severity::Ok,
                    format!("{} (Modern Standby — only plan available)", active.name),
                    "Balanced (Windows Modern Standby default)".to_string(),
                    "Your system uses Modern Standby (S0ix), where Balanced is the standard active power plan."
                        .to_string(),
                    false,
                )
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value,
            reason,
            severity,
            can_fix,

            is_optional_tweak: false,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,

            requires_reboot: false,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let active = env.get_active_power_scheme()?;
        Ok(SavedValue::PowerSchemeGuid(active.guid))
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        let schemes = env.list_power_schemes()?;
        let target = pick_recommended_scheme(&schemes).ok_or_else(|| PolledError::Win32 {
            context: "PowerPlanCheck::apply_fix".to_string(),
            message: "No suitable target power plan found on this system".to_string(),
        })?;
        env.set_active_power_scheme(&target.guid)
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        match saved {
            SavedValue::PowerSchemeGuid(guid) => env.set_active_power_scheme(guid),
            _ => Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::PowerSchemeGuid".to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn flags_power_saver_as_bad_switches_to_high_perf_and_undoes() {
        let env = MockEnv::new();
        env.set_power_schemes(
            GUID_POWER_SAVER,
            vec![
                PowerSchemeInfo {
                    guid: GUID_POWER_SAVER.to_string(),
                    name: "Power saver".to_string(),
                },
                PowerSchemeInfo {
                    guid: GUID_BALANCED.to_string(),
                    name: "Balanced".to_string(),
                },
                PowerSchemeInfo {
                    guid: GUID_HIGH_PERFORMANCE.to_string(),
                    name: "High performance".to_string(),
                },
            ],
        );

        let check = PowerPlanCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Bad);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(
            env.get_active_power_scheme().unwrap().guid,
            GUID_HIGH_PERFORMANCE
        );
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.get_active_power_scheme().unwrap().guid,
            GUID_POWER_SAVER
        );
    }

    #[test]
    fn balanced_only_modern_standby_reports_ok() {
        let env = MockEnv::new();
        env.set_power_schemes(
            GUID_BALANCED,
            vec![PowerSchemeInfo {
                guid: GUID_BALANCED.to_string(),
                name: "Balanced".to_string(),
            }],
        );

        let check = PowerPlanCheck;
        let report = check.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Ok);
        assert!(!report.can_fix);
    }
}
