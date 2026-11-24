// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, Severity};
use crate::error::Result;
use crate::win::SystemEnv;

/// Report-only check auditing active NIC driver power-saving (EEE, Green Ethernet, Power Saving)
/// and Interrupt Moderation flags under `HKLM\...\Class\{4d36e972-...}`.
///
/// Intentionally non-mutating so Polled never restarts or disconnects your active network adapter.
pub struct NetworkNicPowerCheck;

impl HealthCheck for NetworkNicPowerCheck {
    fn id(&self) -> &str {
        "network_nic_power"
    }

    fn name(&self) -> &str {
        "NIC Power Saving & Interrupt Moderation"
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
                current_value: "No active physical network adapter detected".to_string(),
                recommended_value: "EEE, Green Ethernet & NIC Power Saving disabled".to_string(),
                reason: "Connect to a network adapter to inspect NIC driver latency properties."
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

        if primary.latency_flags.is_empty() {
            return Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: format!("{} — Clean (no EEE/power-saving flags)", primary.description),
                recommended_value: "EEE, Green Ethernet & NIC Power Saving disabled".to_string(),
                reason: "No active Energy-Efficient Ethernet, Green Ethernet, or interrupt-moderation flags were found on your primary NIC driver."
                    .to_string(),
                severity: Severity::Ok,
                can_fix: false,
                is_optional_tweak: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,
                requires_reboot: false,
            });
        }

        let has_power_saving = primary
            .latency_flags
            .iter()
            .any(|f| !f.eq_ignore_ascii_case("Interrupt Moderation"));

        let (severity, reason) = if has_power_saving {
            (
                Severity::Warn,
                format!(
                    "NIC power-saving features (EEE / Green Ethernet / Power Saving) can put your Ethernet PHY into low-power sleep between packets, adding 1–5 ms wake jitter or brief disconnects. Disable manually in Device Manager -> Network adapters -> {} -> Properties -> Advanced (Polled never restarts your active NIC automatically).",
                    primary.description
                ),
            )
        } else {
            (
                Severity::Info,
                format!(
                    "Interrupt Moderation coalesces small incoming packets to save CPU at gigabit speeds, adding ~0.2–0.5 ms of latency. For pure competitive gaming, you can optionally disable it in Device Manager -> Network adapters -> {} -> Properties -> Advanced.",
                    primary.description
                ),
            )
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value: format!(
                "{} — Active: {}",
                primary.description,
                primary.latency_flags.join(", ")
            ),
            recommended_value: "EEE, Green Ethernet & Power Saving Off".to_string(),
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
    use crate::win::{NetworkAdapterInfo, NetworkAdapterKind};

    fn nic_with_flags(flags: Vec<&str>) -> NetworkAdapterInfo {
        NetworkAdapterInfo {
            name: "Ethernet".to_string(),
            description: "Intel I225-V".to_string(),
            adapter_guid: "{test}".to_string(),
            kind: NetworkAdapterKind::Ethernet,
            is_primary: true,
            link_speed_mbps: 1000,
            ipv4_address: Some("192.168.1.5".to_string()),
            gateway_ipv4: Some("192.168.1.1".to_string()),
            dns_servers: vec!["1.1.1.1".to_string()],
            latency_flags: flags.into_iter().map(str::to_string).collect(),
            nagle_disabled: false,
        }
    }

    #[test]
    fn classifies_eee_as_warn_moderation_as_info_and_clean_as_ok() {
        let env = MockEnv::new();
        env.set_network_adapters(vec![nic_with_flags(vec![
            "Energy-Efficient Ethernet (EEE)",
            "Interrupt Moderation",
        ])]);
        assert_eq!(
            NetworkNicPowerCheck.scan(&env).unwrap().severity,
            Severity::Warn
        );

        env.set_network_adapters(vec![nic_with_flags(vec!["Interrupt Moderation"])]);
        assert_eq!(
            NetworkNicPowerCheck.scan(&env).unwrap().severity,
            Severity::Info
        );

        env.set_network_adapters(vec![nic_with_flags(Vec::new())]);
        assert_eq!(
            NetworkNicPowerCheck.scan(&env).unwrap().severity,
            Severity::Ok
        );
    }
}
