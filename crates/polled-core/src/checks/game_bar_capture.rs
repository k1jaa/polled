// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const GAMEDVR_APP_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\GameDVR";
const APP_CAPTURE_ENABLED: &str = "AppCaptureEnabled";

const GAMECONFIG_SUBKEY: &str = r"System\GameConfigStore";
const GAMEDVR_ENABLED: &str = "GameDVR_Enabled";

/// Checks Xbox Game Bar background capture (`GameDVR`) in HKCU without touching
/// modern Windows 11 Fullscreen Optimizations.
pub struct GameBarCaptureCheck;

impl HealthCheck for GameBarCaptureCheck {
    fn id(&self) -> &'static str {
        "game_bar_capture"
    }

    fn name(&self) -> &'static str {
        "Game Bar Background Capture (GameDVR)"
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let app_capture =
            env.reg_get_dword(RegHive::Hkcu, GAMEDVR_APP_SUBKEY, APP_CAPTURE_ENABLED)?;
        let gamedvr = env.reg_get_dword(RegHive::Hkcu, GAMECONFIG_SUBKEY, GAMEDVR_ENABLED)?;

        let capture_on = app_capture == Some(1);
        let dvr_on = gamedvr == Some(1);

        let (severity, current_value) = match (capture_on, dvr_on) {
            (true, true) => (
                Severity::Warn,
                "Enabled (AppCaptureEnabled = 1, GameDVR_Enabled = 1)".to_string(),
            ),
            (true, false) => (
                Severity::Warn,
                "Background capture enabled (AppCaptureEnabled = 1)".to_string(),
            ),
            (false, true) => (
                Severity::Warn,
                "GameDVR enabled (GameDVR_Enabled = 1)".to_string(),
            ),
            (false, false) => {
                if app_capture == Some(0) || gamedvr == Some(0) {
                    (Severity::Ok, "Disabled".to_string())
                } else {
                    (Severity::Ok, "Disabled / not configured".to_string())
                }
            }
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Disabled".to_string(),
            reason: "Background GameDVR recording hooks DXGI frame presentation continuously, which can add input latency and frame-pacing jitter in competitive games."
                .to_string(),
            severity,
            can_fix: true,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        Ok(SavedValue::GameDvr {
            app_capture_enabled: env.reg_get_dword(
                RegHive::Hkcu,
                GAMEDVR_APP_SUBKEY,
                APP_CAPTURE_ENABLED,
            )?,
            game_dvr_enabled: env.reg_get_dword(
                RegHive::Hkcu,
                GAMECONFIG_SUBKEY,
                GAMEDVR_ENABLED,
            )?,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_dword(RegHive::Hkcu, GAMEDVR_APP_SUBKEY, APP_CAPTURE_ENABLED, 0)?;
        env.reg_set_dword(RegHive::Hkcu, GAMECONFIG_SUBKEY, GAMEDVR_ENABLED, 0)?;
        Ok(())
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        let SavedValue::GameDvr {
            app_capture_enabled,
            game_dvr_enabled,
        } = saved
        else {
            return Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::GameDvr".to_string(),
            });
        };

        restore_opt_dword(
            env,
            GAMEDVR_APP_SUBKEY,
            APP_CAPTURE_ENABLED,
            *app_capture_enabled,
        )?;
        restore_opt_dword(env, GAMECONFIG_SUBKEY, GAMEDVR_ENABLED, *game_dvr_enabled)?;
        Ok(())
    }
}

fn restore_opt_dword(
    env: &dyn SystemEnv,
    subkey: &str,
    name: &str,
    value: Option<u32>,
) -> Result<()> {
    match value {
        Some(v) => env.reg_set_dword(RegHive::Hkcu, subkey, name, v),
        None => env.reg_delete_value(RegHive::Hkcu, subkey, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn detects_enabled_gamedvr_fixes_and_undoes() {
        let env = MockEnv::new();
        let check = GameBarCaptureCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        env.reg_set_dword(RegHive::Hkcu, GAMEDVR_APP_SUBKEY, APP_CAPTURE_ENABLED, 1)
            .unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Warn);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);
        assert_eq!(
            env.reg_get_dword(RegHive::Hkcu, GAMEDVR_APP_SUBKEY, APP_CAPTURE_ENABLED)
                .unwrap(),
            Some(0)
        );
        assert_eq!(
            env.reg_get_dword(RegHive::Hkcu, GAMECONFIG_SUBKEY, GAMEDVR_ENABLED)
                .unwrap(),
            Some(0)
        );

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(RegHive::Hkcu, GAMEDVR_APP_SUBKEY, APP_CAPTURE_ENABLED)
                .unwrap(),
            Some(1)
        );
        assert_eq!(
            env.reg_get_dword(RegHive::Hkcu, GAMECONFIG_SUBKEY, GAMEDVR_ENABLED)
                .unwrap(),
            None
        );
    }
}
