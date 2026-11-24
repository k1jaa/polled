// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, SavedValue, Severity};
use crate::error::{PolledError, Result};
use crate::win::{RegHive, SystemEnv};

const TCPIP_INTERFACES_ROOT: &str =
    r"SYSTEM\CurrentControlSet\Services\Tcpip\Parameters\Interfaces";
const TCP_ACK_FREQ: &str = "TcpAckFrequency";
const TCP_NO_DELAY: &str = "TCPNoDelay";

/// Optional reversible tweak disabling Nagle's TCP packet coalescing (`TcpAckFrequency = 1`
/// and `TCPNoDelay = 1`) on the primary active network adapter.
pub struct NetworkNagleCheck;

fn interface_subkey(guid: &str) -> String {
    format!(r"{TCPIP_INTERFACES_ROOT}\{guid}")
}

impl HealthCheck for NetworkNagleCheck {
    fn id(&self) -> &str {
        "network_nagle"
    }

    fn name(&self) -> &str {
        "Disable Nagle's Algorithm (Primary NIC)"
    }

    fn is_optional_tweak(&self) -> bool {
        true
    }

    fn requires_elevation(&self) -> bool {
        true
    }

    fn elevation_reason(&self) -> Option<&str> {
        Some("Setting TcpAckFrequency and TCPNoDelay writes to HKLM\\SYSTEM\\CurrentControlSet\\Services\\Tcpip\\Parameters\\Interfaces.")
    }

    fn requires_reboot(&self) -> bool {
        true
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let adapters = env.list_network_adapters()?;
        let Some(primary) = adapters.iter().find(|a| a.is_primary).or(adapters.first()) else {
            return Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: "No active network adapter detected".to_string(),
                recommended_value: "TcpAckFrequency = 1, TCPNoDelay = 1 (optional TCP tweak)"
                    .to_string(),
                reason: "Connect to an active Ethernet or Wi-Fi adapter to inspect or configure TCP Nagle settings."
                    .to_string(),
                severity: Severity::Info,
                can_fix: false,
                is_optional_tweak: true,
                requires_elevation: true,
                elevation_reason: self.elevation_reason().map(str::to_string),
                requires_relogin: false,
                requires_reboot: true,
            });
        };

        let subkey = interface_subkey(&primary.adapter_guid);
        let ack = env.reg_get_dword(RegHive::Hklm, &subkey, TCP_ACK_FREQ)?;
        let nodelay = env.reg_get_dword(RegHive::Hklm, &subkey, TCP_NO_DELAY)?;
        let disabled = ack == Some(1) && nodelay == Some(1);

        let (severity, current_value) = if disabled {
            (
                Severity::Ok,
                format!(
                    "{} — Nagle Off (TcpAckFrequency=1, TCPNoDelay=1)",
                    primary.name
                ),
            )
        } else {
            (
                Severity::Info,
                format!(
                    "{} — Windows default (TCP ACK batching enabled)",
                    primary.name
                ),
            )
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Optional: Nagle Off for TCP games / MMOs (does not change UDP)"
                .to_string(),
            reason: "Sends small TCP packets and ACKs immediately instead of waiting to coalesce them. Helps latency in TCP-based games (e.g. MMOs, ARPGs, game lobbies), though competitive UDP shooters (CS2, Valorant, Apex) already bypass Nagle. Requires reboot or reconnect; fully reversible."
                .to_string(),
            severity,
            can_fix: true,
            is_optional_tweak: true,
            requires_elevation: true,
            elevation_reason: self.elevation_reason().map(str::to_string),
            requires_relogin: false,
            requires_reboot: true,
        })
    }

    fn capture_state(&self, env: &dyn SystemEnv) -> Result<SavedValue> {
        let adapters = env.list_network_adapters()?;
        let primary = adapters
            .iter()
            .find(|a| a.is_primary)
            .or(adapters.first())
            .ok_or_else(|| {
                PolledError::NotFixable("No active network adapter detected".to_string())
            })?;
        let subkey = interface_subkey(&primary.adapter_guid);
        let tcp_ack_frequency = env.reg_get_dword(RegHive::Hklm, &subkey, TCP_ACK_FREQ)?;
        let tcp_no_delay = env.reg_get_dword(RegHive::Hklm, &subkey, TCP_NO_DELAY)?;
        Ok(SavedValue::NetworkNagle {
            interface_guid: primary.adapter_guid.clone(),
            tcp_ack_frequency,
            tcp_no_delay,
        })
    }

    fn apply_fix(&self, env: &dyn SystemEnv) -> Result<()> {
        let adapters = env.list_network_adapters()?;
        let primary = adapters
            .iter()
            .find(|a| a.is_primary)
            .or(adapters.first())
            .ok_or_else(|| {
                PolledError::NotFixable("No active network adapter detected".to_string())
            })?;
        let subkey = interface_subkey(&primary.adapter_guid);
        env.reg_set_dword(RegHive::Hklm, &subkey, TCP_ACK_FREQ, 1)?;
        env.reg_set_dword(RegHive::Hklm, &subkey, TCP_NO_DELAY, 1)
    }

    fn undo(&self, env: &dyn SystemEnv, saved: &SavedValue) -> Result<()> {
        let SavedValue::NetworkNagle {
            interface_guid,
            tcp_ack_frequency,
            tcp_no_delay,
        } = saved
        else {
            return Err(PolledError::InvalidSnapshot {
                check_id: self.id().to_string(),
                detail: "Expected SavedValue::NetworkNagle".to_string(),
            });
        };
        let subkey = interface_subkey(interface_guid);
        match tcp_ack_frequency {
            Some(v) => env.reg_set_dword(RegHive::Hklm, &subkey, TCP_ACK_FREQ, *v)?,
            None => env.reg_delete_value(RegHive::Hklm, &subkey, TCP_ACK_FREQ)?,
        }
        match tcp_no_delay {
            Some(v) => env.reg_set_dword(RegHive::Hklm, &subkey, TCP_NO_DELAY, *v)?,
            None => env.reg_delete_value(RegHive::Hklm, &subkey, TCP_NO_DELAY)?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    #[test]
    fn disables_nagle_on_primary_nic_and_restores_absent_values_on_undo() {
        let env = MockEnv::new();
        let check = NetworkNagleCheck;
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);

        let saved = check.capture_state(&env).unwrap();
        check.apply_fix(&env).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Ok);

        check.undo(&env, &saved).unwrap();
        assert_eq!(check.scan(&env).unwrap().severity, Severity::Info);
        let subkey = interface_subkey("{mock-nic-1}");
        assert_eq!(
            env.reg_get_dword(RegHive::Hklm, &subkey, TCP_ACK_FREQ)
                .unwrap(),
            None
        );
        assert_eq!(
            env.reg_get_dword(RegHive::Hklm, &subkey, TCP_NO_DELAY)
                .unwrap(),
            None
        );
    }
}
