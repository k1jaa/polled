// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const KERNEL_SUBKEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\kernel";
const GLOBAL_TIMER_VAL: &str = "GlobalTimerResolutionRequests";

/// Optional reversible tweak for Windows 11 22H2+ `GlobalTimerResolutionRequests = 1`,
/// restoring global 1 ms timer propagation across processes when a game requests high timer resolution.
pub struct GlobalTimerRequestsCheck;

impl HealthCheck for GlobalTimerRequestsCheck {
    fn id(&self) -> &str {
        "global_timer_requests"
    }

    fn name(&self) -> &str {
        "Global Timer Resolution Requests (Win11)"
    }

    fn is_optional_tweak(&self) -> bool {
        true
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Setting GlobalTimerResolutionRequests writes to HKLM\\SYSTEM\\CurrentControlSet\\Control\\Session Manager\\kernel.")
    }

    fn requires_reboot(&self) -> bool {
        true
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let val = env.reg_get_dword(RegHive::Hklm, KERNEL_SUBKEY, GLOBAL_TIMER_VAL)?;
        let (severity, current_value) = match val {
            Some(1) => (
                Severity::Ok,
                "Enabled (GlobalTimerResolutionRequests = 1)".to_string(),
            ),
            Some(0) => (
                Severity::Info,
                "Disabled (per-process Win11 default: 0)".to_string(),
            ),
            None => (
                Severity::Info,
                "Not set (per-process Win11 22H2+ default)".to_string(),
            ),
            Some(other) => (
                Severity::Info,
                format!("Custom (GlobalTimerResolutionRequests = {other})"),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Enabled (1) — optional low-latency tweak".to_string(),
            reason: "Since Windows 11 22H2, high-resolution timer requests apply only to the requesting process unless GlobalTimerResolutionRequests=1 is set. Enabling it restores consistent Win10-style timer scheduling across game helper threads. Optional tweak—requires reboot and is fully reversible."
                .to_string(),
            severity,
            can_fix: true,
            is_optional_tweak: true,
            requires_elevation: true,
            elevation_reason: self.elevation_reason().map(str::to_string),
            requires_relogin: false,
            requires_reboot: true,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let value = env.reg_get_dword(RegHive::Hklm, KERNEL_SUBKEY, GLOBAL_TIMER_VAL)?;
        Ok(SavedValue::RegistryDword {
            hive: RegHive::Hklm,
            subkey: KERNEL_SUBKEY.to_string(),
            name: GLOBAL_TIMER_VAL.to_string(),
            value,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_dword(RegHive::Hklm, KERNEL_SUBKEY, GLOBAL_TIMER_VAL, 1)
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        let SavedValue::RegistryDword {
            hive,
            subkey,
            name,
            value,
        } = saved
        else {
            return Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::RegistryDword".to_string(),
            });
        };
        match value {
            Some(v) => env.reg_set_dword(*hive, subkey, name, *v),
            None => env.reg_delete_value(*hive, subkey, name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn enables_global_timer_requests_and_deletes_on_undo_when_absent() {
        let env = MockEnv::new();
        let check = GlobalTimerRequestsCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(RegHive::Hklm, KERNEL_SUBKEY, GLOBAL_TIMER_VAL)
                .unwrap(),
            None
        );
    }
}
