// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, Severity};
use crate::error::Result;
use crate::win::{NetworkAdapterKind, SystemEnv};

/// Report-only check auditing the active network link medium (wired Ethernet vs Wi-Fi)
/// and negotiated link speed.
pub struct NetworkConnectionCheck;

impl HealthCheck for NetworkConnectionCheck {
    fn id(&self) -> &str {
        "network_connection"
    }

    fn name(&self) -> &str {
        "Network Link (Ethernet vs Wi-Fi)"
    }

    fn can_fix(&self) -> bool {
        false
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let adapters = env.list_network_adapters()?;
        let Some(primary) = adapters.iter().find(|a| a.is_primary).or(adapters.first()) else {
            return Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: "No active Ethernet or Wi-Fi adapter detected".to_string(),
                recommended_value: "Wired Gigabit Ethernet (>= 1000 Mbps)".to_string(),
                reason: "Connect to a network adapter to inspect link type and negotiated speed."
                    .to_string(),
                severity: Severity::Info,
                can_fix: false,
                is_optional_tweak: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,
                requires_reboot: false,
            });
        };

        let gw_part = primary
            .gateway_ipv4
            .as_deref()
            .map(|g| format!(" · GW {g}"))
            .unwrap_or_default();
        let speed_part = if primary.link_speed_mbps > 0 {
            format!(" · {} Mbps", primary.link_speed_mbps)
        } else {
            String::new()
        };
        let current = format!(
            "{} ({} — {}){speed_part}{gw_part}",
            primary.kind.label(),
            primary.name,
            primary.description
        );

        let (severity, reason) = match primary.kind {
            NetworkAdapterKind::Wifi => (
                Severity::Warn,
                "Wi-Fi uses shared half-duplex radio airtime and retransmissions that cause 5–50 ms ping jitter spikes during competitive matches. Use a wired Ethernet cable directly to your router when possible, or test local gateway jitter in Bench."
                    .to_string(),
            ),
            NetworkAdapterKind::Ethernet
                if primary.link_speed_mbps > 0 && primary.link_speed_mbps < 1000 =>
            {
                (
                    Severity::Warn,
                    format!(
                        "Wired Ethernet negotiated at only {} Mbps (< 1000 Mbps). A damaged Cat5/Cat5e cable, loose wall jack, or 100 Mbps switch port can bottleneck downloads and add bufferbloat.",
                        primary.link_speed_mbps
                    ),
                )
            }
            NetworkAdapterKind::Ethernet => (
                Severity::Ok,
                "Full-duplex wired Gigabit+ Ethernet link active for minimal local network jitter."
                    .to_string(),
            ),
            NetworkAdapterKind::Other => (
                Severity::Info,
                "Active non-standard network interface detected; test gateway vs. internet stability in Bench."
                    .to_string(),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value: current,
            recommended_value: "Wired Gigabit Ethernet (>= 1000 Mbps)".to_string(),
            reason,
            severity,
            can_fix: false,
            is_optional_tweak: false,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,
            requires_reboot: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;
    use crate::win::NetworkAdapterInfo;

    fn mk_nic(kind: NetworkAdapterKind, speed: u64) -> NetworkAdapterInfo {
        NetworkAdapterInfo {
            name: if kind == NetworkAdapterKind::Wifi {
                "Wi-Fi".to_string()
            } else {
                "Ethernet".to_string()
            },
            description: "Test NIC".to_string(),
            adapter_guid: "{test}".to_string(),
            kind,
            is_primary: true,
            link_speed_mbps: speed,
            ipv4_address: Some("192.168.1.10".to_string()),
            gateway_ipv4: Some("192.168.1.1".to_string()),
            dns_servers: vec!["1.1.1.1".to_string()],
            latency_flags: Vec::new(),
            nagle_disabled: false,
        }
    }

    #[test]
    fn warns_on_wifi_and_100mbps_and_passes_gigabit_ethernet() {
        let env = MockEnv::new();
        env.set_network_adapters(vec![mk_nic(NetworkAdapterKind::Wifi, 866)]);
        assert_eq!(
            NetworkConnectionCheck.scan(&env).unwrap().severity,
            Severity::Warn
        );

        env.set_network_adapters(vec![mk_nic(NetworkAdapterKind::Ethernet, 100)]);
        assert_eq!(
            NetworkConnectionCheck.scan(&env).unwrap().severity,
            Severity::Warn
        );

        env.set_network_adapters(vec![mk_nic(NetworkAdapterKind::Ethernet, 2500)]);
        assert_eq!(
            NetworkConnectionCheck.scan(&env).unwrap().severity,
            Severity::Ok
        );
    }
}
