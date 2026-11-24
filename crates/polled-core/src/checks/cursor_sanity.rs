// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const MOUSE_SUBKEY: &str = r"Control Panel\Mouse";
const MOUSE_TRAILS: &str = "MouseTrails";
const SNAP_TO_DEFAULT: &str = "SnapToDefaultButton";

/// Verifies pointer trails (which force software cursor compositing) and Snap-To are disabled.
pub struct CursorSanityCheck;

impl HealthCheck for CursorSanityCheck {
    fn id(&self) -> &str {
        "cursor_sanity"
    }

    fn name(&self) -> &str {
        "Hardware Cursor & Pointer Trails"
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let trails = env.reg_get_string(RegHive::Hkcu, MOUSE_SUBKEY, MOUSE_TRAILS)?;
        let snap = env.reg_get_string(RegHive::Hkcu, MOUSE_SUBKEY, SNAP_TO_DEFAULT)?;

        let trails_on = trails
            .as_deref()
            .and_then(|s| s.trim().parse::<i32>().ok())
            .is_some_and(|v| v > 1);
        let snap_on = snap.as_deref().map(str::trim) == Some("1");

        let (severity, current_value) = match (trails_on, snap_on) {
            (true, true) => (
                Severity::Bad,
                "Pointer trails enabled (software cursor) · Snap-To enabled".to_string(),
            ),
            (true, false) => (
                Severity::Bad,
                "Pointer trails enabled (forces software cursor compositing)".to_string(),
            ),
            (false, true) => (
                Severity::Warn,
                "Hardware cursor OK · Snap-To default button enabled".to_string(),
            ),
            (false, false) => (
                Severity::Ok,
                "Hardware cursor active (Trails off, Snap-To off)".to_string(),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Pointer trails off, Snap-To off".to_string(),
            reason: "Enabling Mouse Pointer Trails forces Windows off the zero-latency GPU hardware cursor plane onto software compositing; Snap-To warps the cursor on dialogs."
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
        Ok(SavedValue::CursorSanity {
            mouse_trails: env.reg_get_string(RegHive::Hkcu, MOUSE_SUBKEY, MOUSE_TRAILS)?,
            snap_to_default: env.reg_get_string(RegHive::Hkcu, MOUSE_SUBKEY, SNAP_TO_DEFAULT)?,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_string(RegHive::Hkcu, MOUSE_SUBKEY, MOUSE_TRAILS, "0")?;
        env.reg_set_string(RegHive::Hkcu, MOUSE_SUBKEY, SNAP_TO_DEFAULT, "0")?;
        Ok(())
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        let SavedValue::CursorSanity {
            mouse_trails,
            snap_to_default,
        } = saved
        else {
            return Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::CursorSanity".to_string(),
            });
        };
        restore_opt_str(env, MOUSE_TRAILS, mouse_trails.as_deref())?;
        restore_opt_str(env, SNAP_TO_DEFAULT, snap_to_default.as_deref())?;
        Ok(())
    }
}

fn restore_opt_str(env: &dyn SystemEnv, name: &str, value: Option<&str>) -> Result<()> {
    match value {
        Some(v) => env.reg_set_string(RegHive::Hkcu, MOUSE_SUBKEY, name, v),
        None => env.reg_delete_value(RegHive::Hkcu, MOUSE_SUBKEY, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn flags_pointer_trails_fixes_and_restores() {
        let env = MockEnv::new();
        let check = CursorSanityCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        env.reg_set_string(RegHive::Hkcu, MOUSE_SUBKEY, MOUSE_TRAILS, "7")
            .unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Bad);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_string(RegHive::Hkcu, MOUSE_SUBKEY, MOUSE_TRAILS)
                .unwrap()
                .as_deref(),
            Some("7")
        );
    }
}
