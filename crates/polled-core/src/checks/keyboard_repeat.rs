// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{KeyboardRepeatParams, SystemEnv};

/// Optional zero-admin user tweak setting shortest keyboard repeat delay (`0` ≈ 250 ms)
/// and fastest repeat rate (`31` ≈ 30 chars/sec) via Win32 `SystemParametersInfoW`.
pub struct KeyboardRepeatCheck;

fn delay_ms_label(delay: u32) -> &'static str {
    match delay {
        0 => "250 ms (shortest)",
        1 => "500 ms",
        2 => "750 ms",
        _ => "1000 ms (longest)",
    }
}

impl HealthCheck for KeyboardRepeatCheck {
    fn id(&self) -> &str {
        "keyboard_repeat"
    }

    fn name(&self) -> &str {
        "Fast Keyboard Repeat Rate & Delay"
    }

    fn is_optional_tweak(&self) -> bool {
        true
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let cur = env.get_keyboard_repeat()?;
        let optimal = cur.delay == 0 && cur.speed == 31;
        let severity = if optimal {
            Severity::Ok
        } else {
            Severity::Info
        };
        let current_value = format!(
            "Delay {} ({}) · Speed {}/31",
            cur.delay,
            delay_ms_label(cur.delay),
            cur.speed
        );

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Delay 0 (250 ms) · Speed 31/31 (fastest) — optional user tweak"
                .to_string(),
            reason: "Sets the shortest Windows key-repeat delay and fastest repeat rate via SPI_SETKEYBOARDDELAY / SPI_SETKEYBOARDSPEED for snappier menu navigation, chat, and UI input. Does not affect raw in-game WASD hold timing. No admin required; takes effect immediately."
                .to_string(),
            severity,
            can_fix: true,
            is_optional_tweak: true,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,
            requires_reboot: false,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        Ok(SavedValue::KeyboardRepeat(env.get_keyboard_repeat()?))
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.set_keyboard_repeat(KeyboardRepeatParams {
            delay: 0,
            speed: 31,
        })
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        let SavedValue::KeyboardRepeat(orig) = saved else {
            return Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::KeyboardRepeat".to_string(),
            });
        };
        env.set_keyboard_repeat(*orig)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn sets_fastest_keyboard_repeat_and_restores_original_on_undo() {
        let env = MockEnv::new();
        env.set_keyboard_repeat(KeyboardRepeatParams {
            delay: 2,
            speed: 20,
        })
        .unwrap();

        let check = KeyboardRepeatCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);
        assert_eq!(
            env.get_keyboard_repeat().unwrap(),
            KeyboardRepeatParams {
                delay: 0,
                speed: 31
            }
        );

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.get_keyboard_repeat().unwrap(),
            KeyboardRepeatParams {
                delay: 2,
                speed: 20
            }
        );
    }
}
