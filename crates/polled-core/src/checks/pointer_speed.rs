// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::SystemEnv;

const RECOMMENDED_SPEED: u32 = 10;

/// Checks Windows pointer speed (`MouseSensitivity`), recommending 10/20 (6/11, 1.0x scaling).
pub struct PointerSpeedCheck;

fn format_speed(speed: u32) -> String {
    let classic_notch = match speed {
        1 => Some("1/11"),
        2 => Some("2/11"),
        4 => Some("3/11"),
        6 => Some("4/11"),
        8 => Some("5/11"),
        10 => Some("6/11 — 1.0x"),
        12 => Some("7/11"),
        14 => Some("8/11"),
        16 => Some("9/11"),
        18 => Some("10/11"),
        20 => Some("11/11"),
        _ => None,
    };
    match classic_notch {
        Some(notch) => format!("{speed}/20 ({notch})"),
        None => format!("{speed}/20"),
    }
}

impl HealthCheck for PointerSpeedCheck {
    fn id(&self) -> &'static str {
        "pointer_speed"
    }

    fn name(&self) -> &'static str {
        "Windows Pointer Speed"
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let speed = env.get_mouse_speed()?;
        let severity = if speed == RECOMMENDED_SPEED {
            Severity::Ok
        } else {
            Severity::Warn
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value: format_speed(speed),
            recommended_value: format_speed(RECOMMENDED_SPEED),
            reason: "Values other than 6/11 (10/20) drop or duplicate cursor counts instead of applying clean 1:1 scaling."
                .to_string(),
            severity,
            can_fix: true,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        Ok(SavedValue::MouseSpeed(env.get_mouse_speed()?))
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.set_mouse_speed(RECOMMENDED_SPEED)
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        match saved {
            SavedValue::MouseSpeed(speed) => env.set_mouse_speed(*speed),
            _ => Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::MouseSpeed".to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn flags_non_6_11_speed_and_restores_on_undo() {
        let env = MockEnv::new();
        env.set_mouse_speed(14).unwrap();

        let check = PointerSpeedCheck;
        let report = check.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Warn);
        assert!(report.current_value.contains("8/11"));

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);
        assert_eq!(env.get_mouse_speed().unwrap(), 10);

        check.undo(&env, &saved).unwrap();
        assert_eq!(env.get_mouse_speed().unwrap(), 14);
    }
}
