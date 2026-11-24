// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{PowerValueIndex, SystemEnv};

/// Documented Microsoft Windows power subgroup GUID for USB settings (`GUID_USB_SUBGROUP`).
pub const GUID_USB_SUBGROUP: &str = "2a737441-1930-4402-8d77-b2bebba308a3";
/// Documented Microsoft Windows power setting GUID for USB selective suspend.
pub const GUID_USB_SELECTIVE_SUSPEND: &str = "48e6b7a6-50f5-4782-a5d4-53bb8f07e226";

const SUSPENDED_DISABLED: u32 = 0;
const SUSPENDED_ENABLED: u32 = 1;

/// Checks USB selective suspend in the active Windows power plan via `powrprof.dll`.
pub struct UsbSelectiveSuspendCheck;

impl HealthCheck for UsbSelectiveSuspendCheck {
    fn id(&self) -> &str {
        "usb_selective_suspend"
    }

    fn name(&self) -> &str {
        "USB Selective Suspend"
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Updating USB selective suspend writes to the active Windows power policy under HKLM\\SYSTEM\\CurrentControlSet\\Control\\Power.")
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let scheme = env.get_active_power_scheme()?;
        let val = env.get_power_setting_index(
            &scheme.guid,
            GUID_USB_SUBGROUP,
            GUID_USB_SELECTIVE_SUSPEND,
        )?;

        let (severity, current_value) = match (val.ac, val.dc) {
            (Some(SUSPENDED_ENABLED), Some(SUSPENDED_ENABLED)) => (
                Severity::Warn,
                format!("Enabled on AC and DC (plan: {})", scheme.name),
            ),
            (Some(SUSPENDED_ENABLED), _) => (
                Severity::Warn,
                format!("Enabled on AC (plan: {})", scheme.name),
            ),
            (Some(SUSPENDED_DISABLED), Some(SUSPENDED_ENABLED)) => (
                Severity::Info,
                format!(
                    "Disabled on AC, enabled on battery/DC (plan: {})",
                    scheme.name
                ),
            ),
            (Some(SUSPENDED_DISABLED), _) => (
                Severity::Ok,
                format!("Disabled on AC (plan: {})", scheme.name),
            ),
            (None, Some(SUSPENDED_ENABLED)) => (
                Severity::Warn,
                format!("Enabled on DC; AC not set (plan: {})", scheme.name),
            ),
            (None, Some(SUSPENDED_DISABLED)) => (
                Severity::Ok,
                format!("Disabled on DC; AC not set (plan: {})", scheme.name),
            ),
            (None, None) => (
                Severity::Info,
                format!("Not exposed in active plan ({})", scheme.name),
            ),
            (ac, dc) => (
                Severity::Info,
                format!("Unknown value AC={ac:?}, DC={dc:?} (plan: {})", scheme.name),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Disabled on AC".to_string(),
            reason: "USB selective suspend can power down idle USB ports and wireless mouse receivers, causing micro-freezes or wake latency."
                .to_string(),
            severity,
            can_fix: true,

            is_optional_tweak: false,
            requires_elevation: self.requires_elevation(),
            elevation_reason: self.elevation_reason().map(str::to_string),
            requires_relogin: false,

            requires_reboot: false,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let scheme = env.get_active_power_scheme()?;
        let val = env.get_power_setting_index(
            &scheme.guid,
            GUID_USB_SUBGROUP,
            GUID_USB_SELECTIVE_SUSPEND,
        )?;
        Ok(SavedValue::PowerSetting {
            scheme_guid: scheme.guid,
            ac: val.ac,
            dc: val.dc,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        let scheme = env.get_active_power_scheme()?;
        // Disable on AC while leaving battery (DC) unchanged to preserve laptop battery life.
        env.set_power_setting_index(
            &scheme.guid,
            GUID_USB_SUBGROUP,
            GUID_USB_SELECTIVE_SUSPEND,
            PowerValueIndex {
                ac: Some(SUSPENDED_DISABLED),
                dc: None,
            },
        )
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        let SavedValue::PowerSetting {
            scheme_guid,
            ac,
            dc,
        } = saved
        else {
            return Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::PowerSetting".to_string(),
            });
        };

        env.set_power_setting_index(
            scheme_guid,
            GUID_USB_SUBGROUP,
            GUID_USB_SELECTIVE_SUSPEND,
            PowerValueIndex { ac: *ac, dc: *dc },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn detects_enabled_usb_suspend_fixes_ac_and_undoes() {
        let env = MockEnv::new();
        let active = env.get_active_power_scheme().unwrap();
        env.set_power_setting_index(
            &active.guid,
            GUID_USB_SUBGROUP,
            GUID_USB_SELECTIVE_SUSPEND,
            PowerValueIndex {
                ac: Some(1),
                dc: Some(1),
            },
        )
        .unwrap();

        let check = UsbSelectiveSuspendCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Warn);
        assert!(check.requires_elevation());

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();

        // AC is now disabled while DC remains untouched -> Info.
        let after = check.scan(&env).unwrap();
        assert_eq!(after.severity, Severity::Info);
        let idx = env
            .get_power_setting_index(&active.guid, GUID_USB_SUBGROUP, GUID_USB_SELECTIVE_SUSPEND)
            .unwrap();
        assert_eq!(idx.ac, Some(0));
        assert_eq!(idx.dc, Some(1));

        check.undo(&env, &saved).unwrap();
        let restored = env
            .get_power_setting_index(&active.guid, GUID_USB_SUBGROUP, GUID_USB_SELECTIVE_SUSPEND)
            .unwrap();
        assert_eq!(restored.ac, Some(1));
        assert_eq!(restored.dc, Some(1));
    }
}
