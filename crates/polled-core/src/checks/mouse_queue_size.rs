// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const MOUCLASS_SUBKEY: &str = r"SYSTEM\CurrentControlSet\Services\mouclass\Parameters";
const QUEUE_SIZE_VAL: &str = "MouseDataQueueSize";
const DEFAULT_QUEUE_SIZE: u32 = 100;

/// Inspects `mouclass\Parameters\MouseDataQueueSize` and flags dangerously small queue sizes
/// (< 32) that can silently drop packets on 4,000 Hz / 8,000 Hz mice during frame hitches.
pub struct MouseDataQueueSizeCheck;

impl HealthCheck for MouseDataQueueSizeCheck {
    fn id(&self) -> &str {
        "mouse_data_queue_size"
    }

    fn name(&self) -> &str {
        "Mouse Driver Queue Size (mouclass)"
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Updating MouseDataQueueSize writes to HKLM\\SYSTEM\\CurrentControlSet\\Services\\mouclass\\Parameters.")
    }

    fn requires_reboot(&self) -> bool {
        true
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let val = env.reg_get_dword(RegHive::Hklm, MOUCLASS_SUBKEY, QUEUE_SIZE_VAL)?;

        let (severity, current_value) = match val {
            None => (
                Severity::Ok,
                "Windows default (100 packets — safe for 1K–8K Hz)".to_string(),
            ),
            Some(v) if v < 32 => (
                Severity::Warn,
                format!("{v} packets (too small for 4K/8K polling — risk of dropped input)"),
            ),
            Some(v) if v > 250 => (
                Severity::Info,
                format!("{v} packets (larger than Windows default 100)"),
            ),
            Some(v) => (Severity::Ok, format!("{v} packets (0x{v:X})")),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "64–100 packets (Windows default 100)".to_string(),
            reason: "The mouclass ring buffer holds pending HID mouse packets until Windows drains them. Setting it below 32 (an outdated 1000 Hz myth) can drop packets on 4K/8K mice during brief frame hitches."
                .to_string(),
            severity,
            can_fix: true,
            is_optional_tweak: false,
            requires_elevation: self.requires_elevation(),
            elevation_reason: self.elevation_reason().map(str::to_string),
            requires_relogin: false,
            requires_reboot: true,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let value = env.reg_get_dword(RegHive::Hklm, MOUCLASS_SUBKEY, QUEUE_SIZE_VAL)?;
        Ok(SavedValue::RegistryDword {
            hive: RegHive::Hklm,
            subkey: MOUCLASS_SUBKEY.to_string(),
            name: QUEUE_SIZE_VAL.to_string(),
            value,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_dword(
            RegHive::Hklm,
            MOUCLASS_SUBKEY,
            QUEUE_SIZE_VAL,
            DEFAULT_QUEUE_SIZE,
        )
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
    fn warns_on_tiny_queue_size_restores_to_100_and_undoes() {
        let env = MockEnv::new();
        let check = MouseDataQueueSizeCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        env.reg_set_dword(RegHive::Hklm, MOUCLASS_SUBKEY, QUEUE_SIZE_VAL, 16)
            .unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Warn);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(RegHive::Hklm, MOUCLASS_SUBKEY, QUEUE_SIZE_VAL)
                .unwrap(),
            Some(16)
        );
    }
}
