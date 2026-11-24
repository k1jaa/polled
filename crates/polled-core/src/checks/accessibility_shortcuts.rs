// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{AccessibilityKind, SystemEnv};

/// Shared Windows accessibility bitmask values:
/// - `0x0001`: feature currently active (`SKF_STICKYKEYSON` / `FKF_FILTERKEYSON` / `MKF_MOUSEKEYSON`)
/// - `0x0004`: keyboard hotkey shortcut active (`*_HOTKEYACTIVE`)
const FEATURE_ON_BIT: u32 = 0x0001;
const HOTKEY_ACTIVE_BIT: u32 = 0x0004;

/// Checks whether the Sticky Keys shortcut (pressing Shift 5 times) is enabled.
pub struct StickyKeysShortcutCheck;

/// Checks whether the Filter Keys shortcut (holding Right Shift for 8 seconds) is enabled.
pub struct FilterKeysShortcutCheck;

/// Checks whether the Mouse Keys shortcut (Left Alt + Left Shift + NumLock) is enabled.
pub struct MouseKeysShortcutCheck;

fn scan_accessibility_shortcut(
    env: &dyn SystemEnv,
    kind: AccessibilityKind,
    id: &str,
    name: &str,
    shortcut_desc: &str,
    reason: &str,
) -> Result<CheckReport> {
    let flags = env.get_accessibility_flags(kind)?;
    let hotkey_on = (flags & HOTKEY_ACTIVE_BIT) != 0;
    let feature_on = (flags & FEATURE_ON_BIT) != 0;

    let (severity, current_value) = if hotkey_on && feature_on {
        (
            Severity::Warn,
            format!("Shortcut enabled ({shortcut_desc}); feature also active"),
        )
    } else if hotkey_on {
        (
            Severity::Warn,
            format!("Shortcut enabled ({shortcut_desc})"),
        )
    } else if feature_on {
        (
            Severity::Info,
            "Shortcut disabled (feature manually active)".to_string(),
        )
    } else {
        (Severity::Ok, "Shortcut disabled".to_string())
    };

    Ok(CheckReport {
        id: id.to_string(),
        name: name.to_string(),
        current_value,
        recommended_value: "Shortcut disabled".to_string(),
        reason: reason.to_string(),
        severity,
        can_fix: true,

        is_optional_tweak: false,
        requires_elevation: false,
        elevation_reason: None,
        requires_relogin: false,

        requires_reboot: false,
    })
}

fn disable_hotkey_bit(env: &dyn SystemEnv, kind: AccessibilityKind) -> Result<()> {
    let flags = env.get_accessibility_flags(kind)?;
    env.set_accessibility_flags(kind, flags & !HOTKEY_ACTIVE_BIT)
}

fn undo_accessibility_flags(
    env: &dyn SystemEnv,
    expected_kind: AccessibilityKind,
    check_id: &str,
    saved: &SavedValue,
) -> Result<()> {
    match saved {
        SavedValue::AccessibilityFlags { kind, flags } if *kind == expected_kind => {
            env.set_accessibility_flags(expected_kind, *flags)
        }
        _ => Err(PolledError::InvalidSnapshot {
            check_id: check_id.to_string(),
            detail: format!("Expected SavedValue::AccessibilityFlags for {expected_kind:?}"),
        }),
    }
}

impl HealthCheck for StickyKeysShortcutCheck {
    fn id(&self) -> &str {
        "sticky_keys_shortcut"
    }

    fn name(&self) -> &str {
        "Sticky Keys Shortcut"
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        scan_accessibility_shortcut(
            env,
            AccessibilityKind::StickyKeys,
            self.id(),
            self.name(),
            "5x Shift",
            "Tapping Shift repeatedly in-game can trigger the Sticky Keys dialog and steal window focus.",
        )
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        Ok(SavedValue::AccessibilityFlags {
            kind: AccessibilityKind::StickyKeys,
            flags: env.get_accessibility_flags(AccessibilityKind::StickyKeys)?,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        disable_hotkey_bit(env, AccessibilityKind::StickyKeys)
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        undo_accessibility_flags(env, AccessibilityKind::StickyKeys, self.id(), saved)
    }
}

impl HealthCheck for FilterKeysShortcutCheck {
    fn id(&self) -> &str {
        "filter_keys_shortcut"
    }

    fn name(&self) -> &str {
        "Filter Keys Shortcut"
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        scan_accessibility_shortcut(
            env,
            AccessibilityKind::FilterKeys,
            self.id(),
            self.name(),
            "Hold Right Shift 8s",
            "Holding Right Shift can trigger Filter Keys and introduce input delay or dropped keystrokes.",
        )
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        Ok(SavedValue::AccessibilityFlags {
            kind: AccessibilityKind::FilterKeys,
            flags: env.get_accessibility_flags(AccessibilityKind::FilterKeys)?,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        disable_hotkey_bit(env, AccessibilityKind::FilterKeys)
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        undo_accessibility_flags(env, AccessibilityKind::FilterKeys, self.id(), saved)
    }
}

impl HealthCheck for MouseKeysShortcutCheck {
    fn id(&self) -> &str {
        "mouse_keys_shortcut"
    }

    fn name(&self) -> &str {
        "Mouse Keys Shortcut"
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        scan_accessibility_shortcut(
            env,
            AccessibilityKind::MouseKeys,
            self.id(),
            self.name(),
            "Left Alt + Left Shift + NumLock",
            "Accidentally pressing Left Alt + Shift + NumLock can hijack numpad binds and pop up a dialog.",
        )
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        Ok(SavedValue::AccessibilityFlags {
            kind: AccessibilityKind::MouseKeys,
            flags: env.get_accessibility_flags(AccessibilityKind::MouseKeys)?,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        disable_hotkey_bit(env, AccessibilityKind::MouseKeys)
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        undo_accessibility_flags(env, AccessibilityKind::MouseKeys, self.id(), saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn disables_only_hotkey_bit_and_restores_exact_flags_for_all_three() {
        let env = MockEnv::new();
        // Set hotkey bit (0x0004) plus unrelated user flags (0x0010, 0x0080).
        let sticky_orig = 0x01FE;
        let filter_orig = 0x007E;
        let mouse_orig = 0x003E;
        env.set_accessibility_flags(AccessibilityKind::StickyKeys, sticky_orig)
            .unwrap();
        env.set_accessibility_flags(AccessibilityKind::FilterKeys, filter_orig)
            .unwrap();
        env.set_accessibility_flags(AccessibilityKind::MouseKeys, mouse_orig)
            .unwrap();

        let sticky = StickyKeysShortcutCheck;
        let filter = FilterKeysShortcutCheck;
        let mouse = MouseKeysShortcutCheck;

        for (check, kind, orig) in [
            (
                &sticky as &dyn HealthCheck,
                AccessibilityKind::StickyKeys,
                sticky_orig,
            ),
            (
                &filter as &dyn HealthCheck,
                AccessibilityKind::FilterKeys,
                filter_orig,
            ),
            (
                &mouse as &dyn HealthCheck,
                AccessibilityKind::MouseKeys,
                mouse_orig,
            ),
        ] {
            assert_eq!(check.scan(&env).unwrap().severity, Severity::Warn);
            let saved = check.capture_state(&env).unwrap();

            check.apply_fix(&env).unwrap();
            assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);
            // All other flag bits remain preserved.
            assert_eq!(
                env.get_accessibility_flags(kind).unwrap(),
                orig & !HOTKEY_ACTIVE_BIT
            );

            check.undo(&env, &saved).unwrap();
            assert_eq!(env.get_accessibility_flags(kind).unwrap(), orig);
        }
    }
}
