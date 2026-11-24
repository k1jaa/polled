// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{MouseAccelParams, RegHive, SystemEnv};

const MOUSE_SUBKEY: &str = r"Control Panel\Mouse";

/// Checks Windows "Enhance pointer precision" (mouse acceleration) via live SPI and HKCU registry.
pub struct EnhancePointerPrecisionCheck;

impl HealthCheck for EnhancePointerPrecisionCheck {
    fn id(&self) -> &str {
        "enhance_pointer_precision"
    }

    fn name(&self) -> &str {
        "Enhance Pointer Precision"
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let live = env.get_mouse_accel()?;
        let reg_speed = env.reg_get_string(RegHive::Hkcu, MOUSE_SUBKEY, "MouseSpeed")?;

        let enabled_live = live.speed != 0;
        let enabled_reg = reg_speed.as_deref().is_some_and(|v| v != "0");
        let thresholds_nonzero = live.threshold1 != 0 || live.threshold2 != 0;

        let (severity, current_value) = if enabled_live {
            (
                Severity::Bad,
                format!(
                    "Enabled (MouseSpeed={}, Threshold1={}, Threshold2={})",
                    live.speed, live.threshold1, live.threshold2
                ),
            )
        } else if enabled_reg {
            (
                Severity::Bad,
                format!(
                    "Registry mismatch (live speed=0, HKCU MouseSpeed={})",
                    reg_speed.unwrap_or_default()
                ),
            )
        } else if thresholds_nonzero {
            (
                Severity::Warn,
                format!(
                    "Disabled with non-zero thresholds (MouseSpeed=0, T1={}, T2={})",
                    live.threshold1, live.threshold2
                ),
            )
        } else {
            (
                Severity::Ok,
                "Disabled (MouseSpeed=0, Threshold1=0, Threshold2=0)".to_string(),
            )
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Disabled (0, 0, 0)".to_string(),
            reason: "Windows pointer acceleration applies non-linear speed scaling, breaking muscle memory in games that do not force Raw Input."
                .to_string(),
            severity,
            can_fix: true,

            is_optional_tweak: false,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,

            requires_reboot: false,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        Ok(SavedValue::MouseAccel(env.get_mouse_accel()?))
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.set_mouse_accel(MouseAccelParams {
            threshold1: 0,
            threshold2: 0,
            speed: 0,
        })
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        match saved {
            SavedValue::MouseAccel(params) => env.set_mouse_accel(*params),
            _ => Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::MouseAccel".to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn detects_enabled_epp_fixes_and_undoes() {
        let env = MockEnv::new();
        let original = MouseAccelParams {
            threshold1: 6,
            threshold2: 10,
            speed: 1,
        };
        env.set_mouse_accel(original).unwrap();

        let check = EnhancePointerPrecisionCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Bad);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);
        assert_eq!(
            env.reg_get_string(RegHive::Hkcu, MOUSE_SUBKEY, "MouseSpeed")
                .unwrap()
                .as_deref(),
            Some("0")
        );

        check.undo(&env, &saved).unwrap();
        assert_eq!(env.get_mouse_accel().unwrap(), original);
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Bad);
    }

    #[test]
    fn flags_nonzero_thresholds_when_speed_zero() {
        let env = MockEnv::new();
        env.set_mouse_accel(MouseAccelParams {
            threshold1: 6,
            threshold2: 10,
            speed: 0,
        })
        .unwrap();

        let check = EnhancePointerPrecisionCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Warn);
    }
}
