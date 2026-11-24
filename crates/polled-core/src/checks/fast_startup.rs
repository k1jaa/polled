// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const POWER_SESSION_SUBKEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Power";
const HIBERBOOT_VAL: &str = "HiberbootEnabled";

/// Optional reversible tweak setting `HiberbootEnabled = 0` so Windows "Shut down" performs
/// a clean kernel boot instead of hibernating stale driver/DPC state across weeks of uptime.
pub struct FastStartupCheck;

impl HealthCheck for FastStartupCheck {
    fn id(&self) -> &str {
        "fast_startup"
    }

    fn name(&self) -> &str {
        "Disable Windows Fast Startup"
    }

    fn is_optional_tweak(&self) -> bool {
        true
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Changing Fast Startup writes HiberbootEnabled under HKLM\\SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Power.")
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let val = env.reg_get_dword(RegHive::Hklm, POWER_SESSION_SUBKEY, HIBERBOOT_VAL)?;
        let (severity, current_value) = match val {
            Some(0) => (
                Severity::Ok,
                "Disabled (HiberbootEnabled = 0 — clean kernel boot on Shutdown)".to_string(),
            ),
            Some(1) => (
                Severity::Info,
                "Enabled (HiberbootEnabled = 1 — hybrid kernel hibernation)".to_string(),
            ),
            None => (
                Severity::Info,
                "Windows default (Fast Startup enabled when hibernation is on)".to_string(),
            ),
            Some(other) => (
                Severity::Info,
                format!("Custom (HiberbootEnabled = {other})"),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Disabled (0) — clean kernel/driver state on Shutdown".to_string(),
            reason: "Windows Fast Startup hibernates the kernel session on Shut down, carrying stale USB/audio/GPU driver state and accumulated uptime across days or weeks (and preventing kernel tweaks from applying on Shutdown). On NVMe SSDs, disabling it adds ~1–2 s to boot while keeping Sleep untouched."
                .to_string(),
            severity,
            can_fix: true,
            is_optional_tweak: true,
            requires_elevation: true,
            elevation_reason: self.elevation_reason().map(str::to_string),
            requires_relogin: false,
            requires_reboot: false,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let value = env.reg_get_dword(RegHive::Hklm, POWER_SESSION_SUBKEY, HIBERBOOT_VAL)?;
        Ok(SavedValue::RegistryDword {
            hive: RegHive::Hklm,
            subkey: POWER_SESSION_SUBKEY.to_string(),
            name: HIBERBOOT_VAL.to_string(),
            value,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_dword(RegHive::Hklm, POWER_SESSION_SUBKEY, HIBERBOOT_VAL, 0)
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
    fn disables_fast_startup_and_restores_prior_value_on_undo() {
        let env = MockEnv::new();
        env.reg_set_dword(RegHive::Hklm, POWER_SESSION_SUBKEY, HIBERBOOT_VAL, 1)
            .unwrap();

        let check = FastStartupCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(RegHive::Hklm, POWER_SESSION_SUBKEY, HIBERBOOT_VAL)
                .unwrap(),
            Some(1)
        );
    }
}
