// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const MOUSE_SUBKEY: &str = r"Control Panel\Mouse";
const X_CURVE_NAME: &str = "SmoothMouseXCurve";
const Y_CURVE_NAME: &str = "SmoothMouseYCurve";

/// Official Windows 10/11 default 40-byte `SmoothMouseXCurve` registry binary value.
pub const DEFAULT_SMOOTH_MOUSE_X_CURVE: [u8; 40] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x15, 0x6E, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x40, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x29, 0xDC, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Official Windows 10/11 default 40-byte `SmoothMouseYCurve` registry binary value.
pub const DEFAULT_SMOOTH_MOUSE_Y_CURVE: [u8; 40] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0xFD, 0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x24, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0xFC, 0x12, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0xC0, 0xBB, 0x01, 0x00, 0x00, 0x00, 0x00,
];

/// Checks whether `SmoothMouseXCurve` or `SmoothMouseYCurve` was modified from Windows defaults.
pub struct AccelCurvesCheck;

fn is_default_or_absent(bytes: Option<&[u8]>, expected: &[u8; 40]) -> bool {
    match bytes {
        None => true,
        Some(actual) => actual == expected.as_slice(),
    }
}

impl HealthCheck for AccelCurvesCheck {
    fn id(&self) -> &'static str {
        "custom_accel_curves"
    }

    fn name(&self) -> &'static str {
        "Custom Acceleration Curves"
    }

    fn requires_relogin(&self) -> bool {
        true
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let x = env.reg_get_binary(RegHive::Hkcu, MOUSE_SUBKEY, X_CURVE_NAME)?;
        let y = env.reg_get_binary(RegHive::Hkcu, MOUSE_SUBKEY, Y_CURVE_NAME)?;

        let x_ok = is_default_or_absent(x.as_deref(), &DEFAULT_SMOOTH_MOUSE_X_CURVE);
        let y_ok = is_default_or_absent(y.as_deref(), &DEFAULT_SMOOTH_MOUSE_Y_CURVE);

        let (severity, current_value) = match (x_ok, y_ok) {
            (true, true) => (Severity::Ok, "Windows default curves".to_string()),
            (false, false) => (
                Severity::Warn,
                "Modified (both SmoothMouseXCurve and SmoothMouseYCurve differ from default)"
                    .to_string(),
            ),
            (false, true) => (
                Severity::Warn,
                "Modified (SmoothMouseXCurve differs from default)".to_string(),
            ),
            (true, false) => (
                Severity::Warn,
                "Modified (SmoothMouseYCurve differs from default)".to_string(),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Windows default curves".to_string(),
            reason: "Third-party registry acceleration curves alter pointer response if Enhance Pointer Precision is ever re-enabled by Windows or a legacy game."
                .to_string(),
            severity,
            can_fix: true,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: true,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let x = env.reg_get_binary(RegHive::Hkcu, MOUSE_SUBKEY, X_CURVE_NAME)?;
        let y = env.reg_get_binary(RegHive::Hkcu, MOUSE_SUBKEY, Y_CURVE_NAME)?;

        Ok(SavedValue::AccelCurves {
            x_curve_b64: x.map(|b| BASE64.encode(b)),
            y_curve_b64: y.map(|b| BASE64.encode(b)),
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_binary(
            RegHive::Hkcu,
            MOUSE_SUBKEY,
            X_CURVE_NAME,
            &DEFAULT_SMOOTH_MOUSE_X_CURVE,
        )?;
        env.reg_set_binary(
            RegHive::Hkcu,
            MOUSE_SUBKEY,
            Y_CURVE_NAME,
            &DEFAULT_SMOOTH_MOUSE_Y_CURVE,
        )?;
        Ok(())
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        let SavedValue::AccelCurves {
            x_curve_b64,
            y_curve_b64,
        } = saved
        else {
            return Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::AccelCurves".to_string(),
            });
        };

        restore_curve(env, X_CURVE_NAME, x_curve_b64.as_deref(), self.id())?;
        restore_curve(env, Y_CURVE_NAME, y_curve_b64.as_deref(), self.id())?;
        Ok(())
    }
}

fn restore_curve(
    env: &dyn SystemEnv,
    value_name: &str,
    encoded: Option<&str>,
    check_id: &str,
) -> Result<()> {
    match encoded {
        Some(b64) => {
            let bytes = BASE64
                .decode(b64)
                .map_err(|e| PolledError::InvalidSnapshot {
                    check_id: check_id.to_string(),
                    detail: format!("Invalid base64 for {value_name}: {e}"),
                })?;
            env.reg_set_binary(RegHive::Hkcu, MOUSE_SUBKEY, value_name, &bytes)
        }
        None => env.reg_delete_value(RegHive::Hkcu, MOUSE_SUBKEY, value_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn absent_and_default_curves_report_ok() {
        let env = MockEnv::new();
        let check = AccelCurvesCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        env.reg_set_binary(
            RegHive::Hkcu,
            MOUSE_SUBKEY,
            X_CURVE_NAME,
            &DEFAULT_SMOOTH_MOUSE_X_CURVE,
        )
        .unwrap();
        env.reg_set_binary(
            RegHive::Hkcu,
            MOUSE_SUBKEY,
            Y_CURVE_NAME,
            &DEFAULT_SMOOTH_MOUSE_Y_CURVE,
        )
        .unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);
    }

    #[test]
    fn detects_custom_curve_fixes_and_restores_exact_bytes() {
        let env = MockEnv::new();
        let custom_x = vec![0xAA; 40];
        let custom_y = vec![0xBB; 40];
        env.reg_set_binary(RegHive::Hkcu, MOUSE_SUBKEY, X_CURVE_NAME, &custom_x)
            .unwrap();
        env.reg_set_binary(RegHive::Hkcu, MOUSE_SUBKEY, Y_CURVE_NAME, &custom_y)
            .unwrap();

        let check = AccelCurvesCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Warn);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_binary(RegHive::Hkcu, MOUSE_SUBKEY, X_CURVE_NAME)
                .unwrap(),
            Some(custom_x)
        );
        assert_eq!(
            env.reg_get_binary(RegHive::Hkcu, MOUSE_SUBKEY, Y_CURVE_NAME)
                .unwrap(),
            Some(custom_y)
        );
    }
}
