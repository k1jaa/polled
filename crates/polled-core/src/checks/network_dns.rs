// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, Severity};
use crate::error::Result;
use crate::win::SystemEnv;

/// Report-only check inspecting configured IPv4 DNS resolvers on the primary network adapter.
pub struct NetworkDnsCheck;

fn known_fast_dns_label(ip: &str) -> Option<&'static str> {
    match ip {
        "1.1.1.1" | "1.0.0.1" => Some("Cloudflare"),
        "8.8.8.8" | "8.8.4.4" => Some("Google"),
        "9.9.9.9" | "149.112.112.112" => Some("Quad9"),
        "208.67.222.222" | "208.67.220.220" => Some("OpenDNS"),
        _ => None,
    }
}

impl HealthCheck for NetworkDnsCheck {
    fn id(&self) -> &str {
        "network_dns"
    }

    fn name(&self) -> &str {
        "DNS Resolver Configuration"
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
                current_value: "No active network adapter detected".to_string(),
                recommended_value: "Fast low-latency DNS (e.g. 1.1.1.1 / 8.8.8.8)".to_string(),
                reason: "Connect to a network adapter to inspect configured DNS servers."
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

        if primary.dns_servers.is_empty() {
            return Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: format!("{} — No IPv4 DNS servers configured", primary.name),
                recommended_value: "1.1.1.1 / 1.0.0.1 or 8.8.8.8 / 8.8.4.4".to_string(),
                reason: "No IPv4 DNS servers were reported on the primary adapter.".to_string(),
                severity: Severity::Warn,
                can_fix: false,
                is_optional_tweak: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,
                requires_reboot: false,
            });
        }

        let fast_provider = primary
            .dns_servers
            .iter()
            .find_map(|ip| known_fast_dns_label(ip));
        let dns_list = primary.dns_servers.join(", ");

        if let Some(provider) = fast_provider {
            Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: format!("{provider} ({dns_list})"),
                recommended_value: "Fast public anycast DNS (1.1.1.1 / 8.8.8.8)".to_string(),
                reason: "Primary adapter uses a fast anycast DNS resolver for quick game-launcher, CDN, and matchmaking lookups."
                    .to_string(),
                severity: Severity::Ok,
                can_fix: false,
                is_optional_tweak: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,
                requires_reboot: false,
            })
        } else {
            Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: format!("Router / ISP DNS ({dns_list})"),
                recommended_value: "Optional: Cloudflare (1.1.1.1) or Google (8.8.8.8)".to_string(),
                reason: "DNS does not affect in-game UDP ping once connected to a server, though switching from slow ISP/router DNS to 1.1.1.1 or 8.8.8.8 can speed up game launcher, CDN, and server-list resolution."
                    .to_string(),
                severity: Severity::Info,
                can_fix: false,
                is_optional_tweak: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,
                requires_reboot: false,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;
    use crate::win::{NetworkAdapterInfo, NetworkAdapterKind};

    fn nic_with_dns(dns: Vec<&str>) -> NetworkAdapterInfo {
        NetworkAdapterInfo {
            name: "Ethernet".to_string(),
            description: "Realtek 2.5GbE".to_string(),
            adapter_guid: "{test}".to_string(),
            kind: NetworkAdapterKind::Ethernet,
            is_primary: true,
            link_speed_mbps: 1000,
            ipv4_address: Some("192.168.1.5".to_string()),
            gateway_ipv4: Some("192.168.1.1".to_string()),
            dns_servers: dns.into_iter().map(str::to_string).collect(),
            latency_flags: Vec::new(),
            nagle_disabled: false,
        }
    }

    #[test]
    fn distinguishes_anycast_dns_router_dns_and_missing_dns() {
        let env = MockEnv::new();
        env.set_network_adapters(vec![nic_with_dns(vec!["1.1.1.1", "1.0.0.1"])]);
        assert_eq!(NetworkDnsCheck.scan(&env).unwrap().severity, Severity::Ok);

        env.set_network_adapters(vec![nic_with_dns(vec!["192.168.1.1"])]);
        assert_eq!(NetworkDnsCheck.scan(&env).unwrap().severity, Severity::Info);

        env.set_network_adapters(vec![nic_with_dns(Vec::new())]);
        assert_eq!(NetworkDnsCheck.scan(&env).unwrap().severity, Severity::Warn);
    }
}
