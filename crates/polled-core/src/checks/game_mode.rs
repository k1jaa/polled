// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const GAMEBAR_SUBKEY: &str = r"Software\Microsoft\GameBar";
const AUTO_GAME_MODE_ENABLED: &str = "AutoGameModeEnabled";
const ALLOW_AUTO_GAME_MODE: &str = "AllowAutoGameMode";

/// Checks Windows Game Mode (`HKCU\Software\Microsoft\GameBar`).
pub struct GameModeCheck;

impl HealthCheck for GameModeCheck {
    fn id(&self) -> &'static str {
        "game_mode"
    }

    fn name(&self) -> &'static str {
        "Windows Game Mode"
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let enabled = env.reg_get_dword(RegHive::Hkcu, GAMEBAR_SUBKEY, AUTO_GAME_MODE_ENABLED)?;
        let allowed = env.reg_get_dword(RegHive::Hkcu, GAMEBAR_SUBKEY, ALLOW_AUTO_GAME_MODE)?;

        // On Windows 10 1903+ and Windows 11, Game Mode defaults to enabled when absent
        // unless either AutoGameModeEnabled or AllowAutoGameMode is explicitly set to 0.
        let is_disabled = enabled == Some(0) || allowed == Some(0);
        let (severity, current_value) = if is_disabled {
            (Severity::Warn, "Disabled".to_string())
        } else if enabled == Some(1) {
            (Severity::Ok, "Enabled".to_string())
        } else {
            (Severity::Ok, "Enabled (Windows default)".to_string())
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Enabled".to_string(),
            reason: "Game Mode suppresses Windows Update restarts and background notifications while a game is running."
                .to_string(),
            severity,
            can_fix: true,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        Ok(SavedValue::GameMode {
            auto_game_mode_enabled: env.reg_get_dword(
                RegHive::Hkcu,
                GAMEBAR_SUBKEY,
                AUTO_GAME_MODE_ENABLED,
            )?,
            allow_auto_game_mode: env.reg_get_dword(
                RegHive::Hkcu,
                GAMEBAR_SUBKEY,
                ALLOW_AUTO_GAME_MODE,
            )?,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_dword(RegHive::Hkcu, GAMEBAR_SUBKEY, AUTO_GAME_MODE_ENABLED, 1)?;
        if env.reg_get_dword(RegHive::Hkcu, GAMEBAR_SUBKEY, ALLOW_AUTO_GAME_MODE)? == Some(0) {
            env.reg_set_dword(RegHive::Hkcu, GAMEBAR_SUBKEY, ALLOW_AUTO_GAME_MODE, 1)?;
        }
        Ok(())
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        let SavedValue::GameMode {
            auto_game_mode_enabled,
            allow_auto_game_mode,
        } = saved
        else {
            return Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::GameMode".to_string(),
            });
        };

        restore_opt_dword(env, AUTO_GAME_MODE_ENABLED, *auto_game_mode_enabled)?;
        restore_opt_dword(env, ALLOW_AUTO_GAME_MODE, *allow_auto_game_mode)?;
        Ok(())
    }
}

fn restore_opt_dword(env: &dyn SystemEnv, name: &str, value: Option<u32>) -> Result<()> {
    match value {
        Some(v) => env.reg_set_dword(RegHive::Hkcu, GAMEBAR_SUBKEY, name, v),
        None => env.reg_delete_value(RegHive::Hkcu, GAMEBAR_SUBKEY, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn absent_defaults_to_ok_and_explicit_zero_is_fixed_and_undone() {
        let env = MockEnv::new();
        let check = GameModeCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        env.reg_set_dword(RegHive::Hkcu, GAMEBAR_SUBKEY, AUTO_GAME_MODE_ENABLED, 0)
            .unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Warn);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(RegHive::Hkcu, GAMEBAR_SUBKEY, AUTO_GAME_MODE_ENABLED)
                .unwrap(),
            Some(0)
        );
        assert_eq!(
            env.reg_get_dword(RegHive::Hkcu, GAMEBAR_SUBKEY, ALLOW_AUTO_GAME_MODE)
                .unwrap(),
            None
        );
    }
}
