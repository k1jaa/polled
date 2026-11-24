// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const PRIORITY_SUBKEY: &str = r"SYSTEM\CurrentControlSet\Control\PriorityControl";
const PRIORITY_VAL: &str = "Win32PrioritySeparation";

/// Short quantum, Variable interval, 3:1 foreground boost (`0x26` = 38 decimal).
const TARGET_PRIORITY_SEP: u32 = 0x26;

/// Decodes documented `Win32PrioritySeparation` bitfields (bits 0..=1 boost, 2..=3 interval, 4..=5 length).
fn describe_priority_separation(val: u32) -> String {
    let boost = match val & 0b11 {
        0 => "No boost",
        1 => "Medium boost (2:1)",
        2 => "High boost (3:1)",
        _ => "Default boost",
    };
    let interval = match (val >> 2) & 0b11 {
        1 => "Variable",
        2 => "Fixed",
        _ => "Default interval",
    };
    let length = match (val >> 4) & 0b11 {
        1 => "Long",
        2 => "Short",
        _ => "Default length",
    };
    format!("0x{val:02X} ({val}) — {length}, {interval}, {boost}")
}

/// Optional reversible tweak for `Win32PrioritySeparation` (foreground thread scheduling quantum).
pub struct PrioritySeparationCheck;

impl HealthCheck for PrioritySeparationCheck {
    fn id(&self) -> &str {
        "win32_priority_separation"
    }

    fn name(&self) -> &str {
        "Win32 Priority Separation (Scheduler Quantum)"
    }

    fn is_optional_tweak(&self) -> bool {
        true
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Updating Win32PrioritySeparation writes to HKLM\\SYSTEM\\CurrentControlSet\\Control\\PriorityControl.")
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let val = env.reg_get_dword(RegHive::Hklm, PRIORITY_SUBKEY, PRIORITY_VAL)?;
        let current_value = match val {
            Some(v) => describe_priority_separation(v),
            None => "Not set (Windows desktop default: 0x02)".to_string(),
        };
        let is_low_latency_preset = matches!(val, Some(0x26 | 0x28 | 0x2A));

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "0x26 (Short, Variable, High boost) or 0x2A (Short, Fixed)"
                .to_string(),
            reason: "Controls Windows CPU time-slice (quantum) length and foreground priority boost. Short quantums (0x26 or 0x2A via Custom Tweaks) favor rapid foreground input responsiveness. Optional tweak—takes effect immediately and is fully reversible."
                .to_string(),
            severity: if is_low_latency_preset {
                Severity::Ok
            } else {
                Severity::Info
            },
            can_fix: true,
            is_optional_tweak: true,
            requires_elevation: true,
            elevation_reason: self.elevation_reason().map(str::to_string),
            requires_relogin: false,
            requires_reboot: false,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let value = env.reg_get_dword(RegHive::Hklm, PRIORITY_SUBKEY, PRIORITY_VAL)?;
        Ok(SavedValue::RegistryDword {
            hive: RegHive::Hklm,
            subkey: PRIORITY_SUBKEY.to_string(),
            name: PRIORITY_VAL.to_string(),
            value,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_dword(
            RegHive::Hklm,
            PRIORITY_SUBKEY,
            PRIORITY_VAL,
            TARGET_PRIORITY_SEP,
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
    fn applies_short_quantum_preset_and_restores_original() {
        let env = MockEnv::new();
        env.reg_set_dword(RegHive::Hklm, PRIORITY_SUBKEY, PRIORITY_VAL, 2)
            .unwrap();

        let check = PrioritySeparationCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        let after = check.scan(&env).unwrap();
        assert_eq!(after.severity, Severity::Ok);
        assert!(after.current_value.contains("0x26"));

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(RegHive::Hklm, PRIORITY_SUBKEY, PRIORITY_VAL)
                .unwrap(),
            Some(2)
        );
    }
}
