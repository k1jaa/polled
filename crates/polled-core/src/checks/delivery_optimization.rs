// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const DO_POLICY_SUBKEY: &str = r"SOFTWARE\Policies\Microsoft\Windows\DeliveryOptimization";
const DO_CONFIG_SUBKEY: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\DeliveryOptimization\Config";
const DO_MODE_VAL: &str = "DODownloadMode";

/// Optional reversible tweak restricting Windows Delivery Optimization to HTTP-only (`0`),
/// preventing background P2P update uploads from causing mid-match ping spikes / bufferbloat.
pub struct DeliveryOptimizationCheck;

fn mode_label(mode: u32) -> &'static str {
    match mode {
        0 => "HTTP only (P2P off)",
        1 => "LAN P2P seeding",
        2 => "LAN/Group P2P seeding",
        3 => "Internet + LAN P2P seeding",
        99 => "Simple HTTP only",
        100 => "Bypass",
        _ => "Custom",
    }
}

impl HealthCheck for DeliveryOptimizationCheck {
    fn id(&self) -> &str {
        "delivery_optimization"
    }

    fn name(&self) -> &str {
        "Disable Windows Update P2P Seeding"
    }

    fn is_optional_tweak(&self) -> bool {
        true
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Setting DODownloadMode writes to HKLM\\SOFTWARE\\Policies\\Microsoft\\Windows\\DeliveryOptimization.")
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let policy = env.reg_get_dword(RegHive::Hklm, DO_POLICY_SUBKEY, DO_MODE_VAL)?;
        let config = env.reg_get_dword(RegHive::Hklm, DO_CONFIG_SUBKEY, DO_MODE_VAL)?;
        let effective = policy.or(config);

        let (severity, current_value) = match effective {
            Some(0 | 99 | 100) => (
                Severity::Ok,
                format!("P2P Off (DODownloadMode = {})", effective.unwrap_or(0)),
            ),
            Some(m) => (
                Severity::Info,
                format!("{} (DODownloadMode = {m})", mode_label(m)),
            ),
            None => (
                Severity::Info,
                "Windows default (LAN P2P update sharing allowed)".to_string(),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "HTTP only (DODownloadMode = 0) — zero background P2P uploads"
                .to_string(),
            reason: "Stops Windows Delivery Optimization from silently seeding Windows Update chunks to other PCs on your LAN or the internet, which saturates home upload bandwidth and triggers sudden ping spikes. Windows Update itself continues working normally over official Microsoft HTTP servers."
                .to_string(),
            severity,
            can_fix: true,
            is_optional_tweak: true,
            requires_elevation: true,
            elevation_reason: self.elevation_reason().map(str::to_string),
            requires_relogin: false,
            requires_reboot: false,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let value = env.reg_get_dword(RegHive::Hklm, DO_POLICY_SUBKEY, DO_MODE_VAL)?;
        Ok(SavedValue::RegistryDword {
            hive: RegHive::Hklm,
            subkey: DO_POLICY_SUBKEY.to_string(),
            name: DO_MODE_VAL.to_string(),
            value,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_dword(RegHive::Hklm, DO_POLICY_SUBKEY, DO_MODE_VAL, 0)
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
    fn disables_p2p_delivery_optimization_and_restores_absent_on_undo() {
        let env = MockEnv::new();
        let check = DeliveryOptimizationCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(RegHive::Hklm, DO_POLICY_SUBKEY, DO_MODE_VAL)
                .unwrap(),
            None
        );
    }
}
