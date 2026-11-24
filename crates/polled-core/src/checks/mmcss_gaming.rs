// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const SYS_PROFILE_KEY: &str =
    r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile";
const GAMES_TASK_KEY: &str =
    r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile\Tasks\Games";

const SYS_RESPONSIVENESS: &str = "SystemResponsiveness";
const NET_THROTTLING: &str = "NetworkThrottlingIndex";
const GPU_PRIORITY: &str = "GPU Priority";
const PRIORITY: &str = "Priority";
const SCHED_CATEGORY: &str = "Scheduling Category";
const SFIO_PRIORITY: &str = "SFIO Priority";

const TARGET_RESPONSIVENESS: u32 = 10;
const TARGET_NET_THROTTLING_OFF: u32 = 0xFFFF_FFFF;
const TARGET_GPU_PRIORITY: u32 = 8;
const TARGET_PRIORITY: u32 = 6;

/// Optional reversible tweak optimizing Windows Multimedia Class Scheduler Service (MMCSS)
/// for low-latency gaming (`SystemResponsiveness=10`, `NetworkThrottlingIndex=Off`, Games `High`).
pub struct MmcssGamingCheck;

impl HealthCheck for MmcssGamingCheck {
    fn id(&self) -> &str {
        "mmcss_gaming"
    }

    fn name(&self) -> &str {
        "MMCSS Gaming Scheduling Priority"
    }

    fn is_optional_tweak(&self) -> bool {
        true
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Tuning MMCSS gaming priority writes under HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Multimedia\\SystemProfile.")
    }

    fn requires_reboot(&self) -> bool {
        true
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let resp = env.reg_get_dword(RegHive::Hklm, SYS_PROFILE_KEY, SYS_RESPONSIVENESS)?;
        let net = env.reg_get_dword(RegHive::Hklm, SYS_PROFILE_KEY, NET_THROTTLING)?;
        let gpu = env.reg_get_dword(RegHive::Hklm, GAMES_TASK_KEY, GPU_PRIORITY)?;
        let prio = env.reg_get_dword(RegHive::Hklm, GAMES_TASK_KEY, PRIORITY)?;
        let sched = env.reg_get_string(RegHive::Hklm, GAMES_TASK_KEY, SCHED_CATEGORY)?;

        let applied = resp == Some(TARGET_RESPONSIVENESS)
            && net == Some(TARGET_NET_THROTTLING_OFF)
            && gpu == Some(TARGET_GPU_PRIORITY)
            && prio == Some(TARGET_PRIORITY)
            && sched.as_deref() == Some("High");

        let net_label = match net {
            Some(0xFFFF_FFFF) => "Off".to_string(),
            Some(v) => v.to_string(),
            None => "default (10)".to_string(),
        };
        let current_value = format!(
            "Responsiveness {} · NetThrottle {net_label} · Games Prio {} / {}",
            resp.map_or_else(|| "default (20)".to_string(), |v| v.to_string()),
            prio.map_or_else(|| "default (2)".to_string(), |v| v.to_string()),
            sched.as_deref().unwrap_or("Medium")
        );

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Responsiveness 10 · NetThrottle Off · Games High (6)".to_string(),
            reason: "Lowers MMCSS background CPU reservation from 20% to the documented 10% floor, disables multimedia packet throttling, and elevates the Games task class. Optional tweak—fully reversible."
                .to_string(),
            severity: if applied { Severity::Ok } else { Severity::Info },
            can_fix: true,
            is_optional_tweak: true,
            requires_elevation: true,
            elevation_reason: self.elevation_reason().map(str::to_string),
            requires_relogin: false,
            requires_reboot: true,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        Ok(SavedValue::MmcssGames {
            system_responsiveness: env.reg_get_dword(
                RegHive::Hklm,
                SYS_PROFILE_KEY,
                SYS_RESPONSIVENESS,
            )?,
            network_throttling_index: env.reg_get_dword(
                RegHive::Hklm,
                SYS_PROFILE_KEY,
                NET_THROTTLING,
            )?,
            gpu_priority: env.reg_get_dword(RegHive::Hklm, GAMES_TASK_KEY, GPU_PRIORITY)?,
            priority: env.reg_get_dword(RegHive::Hklm, GAMES_TASK_KEY, PRIORITY)?,
            scheduling_category: env.reg_get_string(
                RegHive::Hklm,
                GAMES_TASK_KEY,
                SCHED_CATEGORY,
            )?,
            sfio_priority: env.reg_get_string(RegHive::Hklm, GAMES_TASK_KEY, SFIO_PRIORITY)?,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        env.reg_set_dword(
            RegHive::Hklm,
            SYS_PROFILE_KEY,
            SYS_RESPONSIVENESS,
            TARGET_RESPONSIVENESS,
        )?;
        env.reg_set_dword(
            RegHive::Hklm,
            SYS_PROFILE_KEY,
            NET_THROTTLING,
            TARGET_NET_THROTTLING_OFF,
        )?;
        env.reg_set_dword(
            RegHive::Hklm,
            GAMES_TASK_KEY,
            GPU_PRIORITY,
            TARGET_GPU_PRIORITY,
        )?;
        env.reg_set_dword(RegHive::Hklm, GAMES_TASK_KEY, PRIORITY, TARGET_PRIORITY)?;
        env.reg_set_string(RegHive::Hklm, GAMES_TASK_KEY, SCHED_CATEGORY, "High")?;
        env.reg_set_string(RegHive::Hklm, GAMES_TASK_KEY, SFIO_PRIORITY, "High")?;
        Ok(())
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        let SavedValue::MmcssGames {
            system_responsiveness,
            network_throttling_index,
            gpu_priority,
            priority,
            scheduling_category,
            sfio_priority,
        } = saved
        else {
            return Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::MmcssGames".to_string(),
            });
        };

        restore_dword(
            env,
            SYS_PROFILE_KEY,
            SYS_RESPONSIVENESS,
            *system_responsiveness,
        )?;
        restore_dword(
            env,
            SYS_PROFILE_KEY,
            NET_THROTTLING,
            *network_throttling_index,
        )?;
        restore_dword(env, GAMES_TASK_KEY, GPU_PRIORITY, *gpu_priority)?;
        restore_dword(env, GAMES_TASK_KEY, PRIORITY, *priority)?;
        restore_str(
            env,
            GAMES_TASK_KEY,
            SCHED_CATEGORY,
            scheduling_category.as_deref(),
        )?;
        restore_str(env, GAMES_TASK_KEY, SFIO_PRIORITY, sfio_priority.as_deref())?;
        Ok(())
    }
}

fn restore_dword(env: &dyn SystemEnv, subkey: &str, name: &str, val: Option<u32>) -> Result<()> {
    match val {
        Some(v) => env.reg_set_dword(RegHive::Hklm, subkey, name, v),
        None => env.reg_delete_value(RegHive::Hklm, subkey, name),
    }
}

fn restore_str(env: &dyn SystemEnv, subkey: &str, name: &str, val: Option<&str>) -> Result<()> {
    match val {
        Some(v) => env.reg_set_string(RegHive::Hklm, subkey, name, v),
        None => env.reg_delete_value(RegHive::Hklm, subkey, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn applies_mmcss_gaming_tweak_and_restores_exact_original_values() {
        let env = MockEnv::new();
        env.reg_set_dword(RegHive::Hklm, SYS_PROFILE_KEY, SYS_RESPONSIVENESS, 20)
            .unwrap();
        env.reg_set_string(RegHive::Hklm, GAMES_TASK_KEY, SCHED_CATEGORY, "Medium")
            .unwrap();

        let check = MmcssGamingCheck;
        assert!(check.is_optional_tweak());
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(
            env.reg_get_dword(RegHive::Hklm, SYS_PROFILE_KEY, SYS_RESPONSIVENESS)
                .unwrap(),
            Some(20)
        );
        assert_eq!(
            env.reg_get_dword(RegHive::Hklm, SYS_PROFILE_KEY, NET_THROTTLING)
                .unwrap(),
            None
        );
        assert_eq!(
            env.reg_get_string(RegHive::Hklm, GAMES_TASK_KEY, SCHED_CATEGORY)
                .unwrap()
                .as_deref(),
            Some("Medium")
        );
    }
}
