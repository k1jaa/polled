// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const AUDIO_SUBKEY: &str = r"Software\Microsoft\Multimedia\Audio";
const USER_DUCKING_PREFERENCE: &str = "UserDuckingPreference";

/// Checks Windows Communications audio ducking (`HKCU\Software\Microsoft\Multimedia\Audio`).
///
/// Windows defaults to reducing all other application volume by 80% whenever it detects
/// communications activity (Discord, in-game voice chat, Steam Voice). Setting
/// `UserDuckingPreference = 3` selects "Do nothing" without requiring administrator rights.
pub struct AudioCommsDuckingCheck;

impl HealthCheck for AudioCommsDuckingCheck {
    fn id(&self) -> &str {
        "audio_comms_ducking"
    }

    fn name(&self) -> &str {
        "Windows Communications Volume Ducking"
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let val = env.reg_get_dword(RegHive::Hkcu, AUDIO_SUBKEY, USER_DUCKING_PREFERENCE)?;
        let (severity, current_value) = match val {
            Some(3) => (Severity::Ok, "Do nothing (disabled)".to_string()),
            Some(0) => (Severity::Warn, "Mute all other sounds (0)".to_string()),
            Some(1) => (Severity::Warn, "Reduce volume by 80% (1)".to_string()),
            Some(2) => (Severity::Warn, "Reduce volume by 50% (2)".to_string()),
            None => (
                Severity::Warn,
                "Windows default (Reduce volume by 80%)".to_string(),
            ),
            Some(other) => (Severity::Warn, format!("Custom value ({other})")),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Do nothing (3)".to_string(),
            reason: "Windows automatically lowers game audio volume by up to 80% when voice apps or in-game comms trigger a communications stream. Setting UserDuckingPreference=3 keeps game volume consistent."
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
        let value = env.reg_get_dword(RegHive::Hkcu, AUDIO_SUBKEY, USER_DUCKING_PREFERENCE)?;
        Ok(SavedValue::RegistryDword {
            hive: RegHive::Hkcu,
            subkey: AUDIO_SUBKEY.to_string(),
            name: USER_DUCKING_PREFERENCE.to_string(),
            value,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_dword(RegHive::Hkcu, AUDIO_SUBKEY, USER_DUCKING_PREFERENCE, 3)
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
    fn absent_warns_and_fix_undo_roundtrips_cleanly() {
        let env = MockEnv::new();
        let check = AudioCommsDuckingCheck;

        assert_eq!(check.scan(&env).unwrap().severity, Severity::Warn);
        let saved = check.capture_state(&env).unwrap();

        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);
        assert_eq!(
            env.reg_get_dword(RegHive::Hkcu, AUDIO_SUBKEY, USER_DUCKING_PREFERENCE)
                .unwrap(),
            Some(3)
        );

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(RegHive::Hkcu, AUDIO_SUBKEY, USER_DUCKING_PREFERENCE)
                .unwrap(),
            None
        );
    }

    #[test]
    fn explicit_ducking_value_restores_exact_prior_value() {
        let env = MockEnv::new();
        let check = AudioCommsDuckingCheck;
        env.reg_set_dword(RegHive::Hkcu, AUDIO_SUBKEY, USER_DUCKING_PREFERENCE, 2)
            .unwrap();

        let report = check.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Warn);
        assert!(report.current_value.contains("50%"));

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(RegHive::Hkcu, AUDIO_SUBKEY, USER_DUCKING_PREFERENCE)
                .unwrap(),
            Some(2)
        );
    }
}
