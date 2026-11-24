// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Read-only Windows Network Adapter Audit & On-Demand Dual-Hop ICMP Jitter Probe.
//!
//! - Uses `iphlpapi.dll` `GetAdaptersAddresses` + read-only `HKLM\...\Class\{4d36e972-...}`
//!   registry inspection to detect active Ethernet/Wi-Fi adapters, link speeds, IPv4 gateways,
//!   DNS servers, and NIC power-saving / interrupt-moderation flags (zero admin).
//! - Runs an on-demand dual-hop ICMP ping probe (`IcmpCreateFile` / `IcmpSendEcho` /
//!   `IcmpCloseHandle`, zero admin) against your local gateway and an internet IP (default
//!   `1.1.1.1`) simultaneously to separate local router/Wi-Fi spikes from ISP routing jitter.

use std::mem::size_of;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS, HANDLE};
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho, GAA_FLAG_INCLUDE_GATEWAYS,
    ICMP_ECHO_REPLY, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211, IP_ADAPTER_ADDRESSES_LH,
    IP_SUCCESS,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

use crate::checks::Severity;
use crate::error::{PolledError, Result};
use crate::win::{
    registry, NetworkAdapterInfo, NetworkAdapterKind, NetworkJitterBenchmark, PingHopStats, RegHive,
};

const NIC_CLASS_ROOT: &str =
    r"SYSTEM\CurrentControlSet\Control\Class\{4d36e972-e325-11ce-bfc1-08002be10318}";
const TCPIP_INTERFACES_ROOT: &str =
    r"SYSTEM\CurrentControlSet\Services\Tcpip\Parameters\Interfaces";

/// RAII wrapper ensuring `IcmpCloseHandle` always runs on drop.
struct IcmpHandleGuard(HANDLE);

impl Drop for IcmpHandleGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: Handle came from `IcmpCreateFile` and is closed exactly once on drop.
            unsafe {
                let _ = IcmpCloseHandle(self.0);
            }
        }
    }
}

fn is_virtual_adapter_desc(desc: &str, friendly: &str) -> bool {
    let d = desc.to_ascii_lowercase();
    let f = friendly.to_ascii_lowercase();
    const VIRTUAL_MARKERS: &[&str] = &[
        "virtual",
        "vmware",
        "virtualbox",
        "hyper-v",
        "vethernet",
        "tap-windows",
        "wintun",
        "wireguard",
        "tailscale",
        "zerotier",
        "npcap",
        "bluetooth",
        "loopback",
        "km-test",
        "radmin",
        "hamachi",
        "openvpn",
    ];
    VIRTUAL_MARKERS
        .iter()
        .any(|m| d.contains(m) || f.contains(m))
}

/// Inspects a Windows NIC driver class key (`HKLM\...\Class\{4d36e972-...}\00xx`) for
/// known power-saving or packet-coalescing settings that add network latency spikes.
pub(crate) fn inspect_nic_latency_flags_from_values(values: &[(&str, &str)]) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();
    let mut has_eee = false;
    let mut has_green = false;
    let mut has_power_save = false;
    let mut has_int_mod = false;

    for &(key, val) in values {
        let v = val.trim();
        let enabled = !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("disabled");
        if !enabled {
            continue;
        }
        let k = key.to_ascii_lowercase();
        if k == "*eee" || k == "eee" || k == "eeelinkadvertisement" || k == "advancedeee" {
            has_eee = true;
        } else if k == "enablegreenethernet" || k == "gigalite" {
            has_green = true;
        } else if k == "powersavingmode"
            || k == "autopowersavemodeenabled"
            || k == "ultralowpowermode"
            || k == "*selectivesuspend"
            || k == "enablepme"
            || k == "nicpowersaving"
        {
            has_power_save = true;
        } else if k == "*interruptmoderation" {
            has_int_mod = true;
        }
    }

    if has_eee {
        flags.push("Energy-Efficient Ethernet (EEE)".to_string());
    }
    if has_green {
        flags.push("Green Ethernet / GigaLite".to_string());
    }
    if has_power_save {
        flags.push("NIC Power Saving".to_string());
    }
    if has_int_mod {
        flags.push("Interrupt Moderation".to_string());
    }
    flags
}

fn read_nic_latency_flags(adapter_guid: &str) -> Vec<String> {
    let Ok(subkeys) = registry::list_subkeys(RegHive::Hklm, NIC_CLASS_ROOT) else {
        return Vec::new();
    };
    const CANDIDATE_KEYS: &[&str] = &[
        "*EEE",
        "EEE",
        "EEELinkAdvertisement",
        "AdvancedEEE",
        "EnableGreenEthernet",
        "GigaLite",
        "PowerSavingMode",
        "AutoPowerSaveModeEnabled",
        "UltraLowPowerMode",
        "*SelectiveSuspend",
        "EnablePME",
        "*InterruptModeration",
    ];
    for sub in subkeys {
        if sub.len() != 4 || !sub.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let path = format!(r"{NIC_CLASS_ROOT}\{sub}");
        let Ok(Some(net_id)) = registry::get_string(RegHive::Hklm, &path, "NetCfgInstanceId")
        else {
            continue;
        };
        if !net_id.eq_ignore_ascii_case(adapter_guid) {
            continue;
        }

        let mut owned: Vec<(&'static str, String)> = Vec::new();
        for &val_name in CANDIDATE_KEYS {
            if let Ok(Some(s)) = registry::get_string(RegHive::Hklm, &path, val_name) {
                owned.push((val_name, s));
            } else if let Ok(Some(d)) = registry::get_dword(RegHive::Hklm, &path, val_name) {
                owned.push((val_name, d.to_string()));
            }
        }
        let borrowed: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
        return inspect_nic_latency_flags_from_values(&borrowed);
    }
    Vec::new()
}

fn read_nagle_disabled(adapter_guid: &str) -> bool {
    let path = format!(r"{TCPIP_INTERFACES_ROOT}\{adapter_guid}");
    let ack = registry::get_dword(RegHive::Hklm, &path, "TcpAckFrequency")
        .ok()
        .flatten();
    let nodelay = registry::get_dword(RegHive::Hklm, &path, "TCPNoDelay")
        .ok()
        .flatten();
    ack == Some(1) && nodelay == Some(1)
}

fn parse_registry_dns_list(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in raw.split(|c: char| c == ',' || c.is_whitespace()) {
        let t = token.trim();
        if let Ok(ip) = t.parse::<Ipv4Addr>() {
            if !ip.is_unspecified() {
                let s = ip.to_string();
                if !out.contains(&s) {
                    out.push(s);
                }
            }
        }
    }
    out
}

/// Fallback read-only IPv4 DNS server lookup from `Tcpip\Parameters\Interfaces\{guid}` when
/// `GetAdaptersAddresses` returns no DNS addresses (e.g. restricted tokens or transient DHCP state).
fn read_dns_servers_from_registry(adapter_guid: &str) -> Vec<String> {
    let path = format!(r"{TCPIP_INTERFACES_ROOT}\{adapter_guid}");
    for key in ["NameServer", "DhcpNameServer"] {
        if let Ok(Some(raw)) = registry::get_string(RegHive::Hklm, &path, key) {
            let parsed = parse_registry_dns_list(&raw);
            if !parsed.is_empty() {
                return parsed;
            }
        }
    }
    Vec::new()
}

unsafe fn extract_ipv4_from_sockaddr(
    addr: windows::Win32::Networking::WinSock::SOCKET_ADDRESS,
) -> Option<Ipv4Addr> {
    if addr.lpSockaddr.is_null() || (addr.iSockaddrLength as usize) < size_of::<SOCKADDR_IN>() {
        return None;
    }
    // SAFETY: Caller provides a valid `SOCKET_ADDRESS` from `GetAdaptersAddresses`.
    let family = (*addr.lpSockaddr).sa_family;
    if family != AF_INET {
        return None;
    }
    let sin = &*(addr.lpSockaddr as *const SOCKADDR_IN);
    let raw_addr = sin.sin_addr.S_un.S_addr;
    let ip = Ipv4Addr::from(raw_addr.to_ne_bytes());
    if ip.is_unspecified() {
        None
    } else {
        Some(ip)
    }
}

/// Lists active UP Windows Ethernet & Wi-Fi adapters with IPv4 gateways, DNS servers,
/// link speeds, and NIC driver latency flags (read-only, zero admin).
pub fn list_network_adapters() -> Result<Vec<NetworkAdapterInfo>> {
    let flags = GAA_FLAG_INCLUDE_GATEWAYS;
    let family = u32::from(AF_INET.0);
    let mut buf_len = 16_384u32;
    let mut buf = vec![0u8; buf_len as usize];

    for _ in 0..3 {
        // SAFETY: `buf` is valid writable memory of length `buf_len`.
        let status = unsafe {
            GetAdaptersAddresses(
                family,
                flags,
                None,
                Some(buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>()),
                &mut buf_len,
            )
        };
        if status == ERROR_SUCCESS.0 {
            break;
        }
        if status == ERROR_BUFFER_OVERFLOW.0 {
            buf.resize(buf_len as usize, 0);
            continue;
        }
        return Err(PolledError::Win32 {
            context: "GetAdaptersAddresses".to_string(),
            message: format!("Win32 error {status}"),
        });
    }

    struct Candidate {
        info: NetworkAdapterInfo,
        metric: u32,
        is_virtual: bool,
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut cur = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();

    while !cur.is_null() {
        // SAFETY: Linked list pointers stay valid inside `buf`.
        let adapter = unsafe { &*cur };
        cur = adapter.Next;

        if adapter.OperStatus != IfOperStatusUp {
            continue;
        }
        let kind = match adapter.IfType {
            IF_TYPE_ETHERNET_CSMACD => NetworkAdapterKind::Ethernet,
            IF_TYPE_IEEE80211 => NetworkAdapterKind::Wifi,
            _ => continue,
        };

        let name = unsafe { adapter.FriendlyName.to_string() }.unwrap_or_default();
        let description = unsafe { adapter.Description.to_string() }.unwrap_or_default();
        let adapter_guid = unsafe { adapter.AdapterName.to_string() }.unwrap_or_default();
        let is_virtual = is_virtual_adapter_desc(&description, &name);

        let mut ipv4_address = None;
        let mut uc = adapter.FirstUnicastAddress;
        while !uc.is_null() {
            let entry = unsafe { &*uc };
            if let Some(ip) = unsafe { extract_ipv4_from_sockaddr(entry.Address) } {
                ipv4_address = Some(ip.to_string());
                break;
            }
            uc = entry.Next;
        }

        let mut gateway_ipv4 = None;
        let mut gw = adapter.FirstGatewayAddress;
        while !gw.is_null() {
            let entry = unsafe { &*gw };
            if let Some(ip) = unsafe { extract_ipv4_from_sockaddr(entry.Address) } {
                gateway_ipv4 = Some(ip.to_string());
                break;
            }
            gw = entry.Next;
        }

        let mut dns_servers = Vec::new();
        let mut dns = adapter.FirstDnsServerAddress;
        while !dns.is_null() && dns_servers.len() < 4 {
            let entry = unsafe { &*dns };
            if let Some(ip) = unsafe { extract_ipv4_from_sockaddr(entry.Address) } {
                let s = ip.to_string();
                if !dns_servers.contains(&s) {
                    dns_servers.push(s);
                }
            }
            dns = entry.Next;
        }
        if dns_servers.is_empty() {
            dns_servers = read_dns_servers_from_registry(&adapter_guid);
        }

        let link_speed_mbps = if adapter.TransmitLinkSpeed == u64::MAX {
            0
        } else {
            adapter.TransmitLinkSpeed / 1_000_000
        };

        let latency_flags = if !is_virtual {
            read_nic_latency_flags(&adapter_guid)
        } else {
            Vec::new()
        };
        let nagle_disabled = read_nagle_disabled(&adapter_guid);

        candidates.push(Candidate {
            info: NetworkAdapterInfo {
                name,
                description,
                adapter_guid,
                kind,
                is_primary: false,
                link_speed_mbps,
                ipv4_address,
                gateway_ipv4,
                dns_servers,
                latency_flags,
                nagle_disabled,
            },
            metric: adapter.Ipv4Metric,
            is_virtual,
        });
    }

    // Prefer physical adapters unless only virtual adapters are up.
    let has_physical = candidates.iter().any(|c| !c.is_virtual);
    if has_physical {
        candidates.retain(|c| !c.is_virtual);
    }

    // Sort: adapter with gateway first, then lowest IPv4 metric, then Ethernet before Wi-Fi.
    candidates.sort_by_key(|c| {
        (
            c.info.gateway_ipv4.is_none(),
            c.metric,
            c.info.kind != NetworkAdapterKind::Ethernet,
        )
    });

    if let Some(first) = candidates.first_mut() {
        first.info.is_primary = true;
    }

    Ok(candidates.into_iter().map(|c| c.info).collect())
}

fn icmp_ping_once(handle: HANDLE, target: Ipv4Addr, timeout_ms: u32) -> Option<f64> {
    let payload: [u8; 32] = *b"PolledNetworkLatencyProbe0000001";
    let reply_size = size_of::<ICMP_ECHO_REPLY>() + payload.len() + 64;
    let mut reply_buf = vec![0u8; reply_size];
    let dest_addr = u32::from_ne_bytes(target.octets());

    let started = Instant::now();
    // SAFETY: Valid `IcmpCreateFile` handle, payload buffer, and reply buffer sized per MSDN.
    let replies = unsafe {
        IcmpSendEcho(
            handle,
            dest_addr,
            payload.as_ptr().cast(),
            payload.len() as u16,
            None,
            reply_buf.as_mut_ptr().cast(),
            reply_size as u32,
            timeout_ms,
        )
    };
    let wall_ms = started.elapsed().as_secs_f64() * 1_000.0;

    if replies == 0 {
        return None;
    }
    // SAFETY: `reply_buf` is >= `size_of::<ICMP_ECHO_REPLY>()` and `replies > 0`.
    let reply = unsafe { &*(reply_buf.as_ptr().cast::<ICMP_ECHO_REPLY>()) };
    if reply.Status != IP_SUCCESS {
        return None;
    }
    // Windows `RoundTripTime` is integer milliseconds (often `0` for sub-ms LAN gateways).
    // Use high-resolution wall time when `RoundTripTime == 0` or closely tracks wall time.
    let rtt_ms =
        if reply.RoundTripTime == 0 || (wall_ms - f64::from(reply.RoundTripTime)).abs() <= 2.0 {
            wall_ms
        } else {
            f64::from(reply.RoundTripTime)
        };
    Some(rtt_ms.max(0.05))
}

pub(crate) fn compute_hop_stats(
    label: &str,
    target_ip: &str,
    samples_ms: Vec<Option<f64>>,
    is_local_gateway: bool,
) -> PingHopStats {
    let sent = samples_ms.len();
    let valid: Vec<f64> = samples_ms.iter().copied().flatten().collect();
    let received = valid.len();
    let lost = sent.saturating_sub(received);
    let loss_pct = if sent > 0 {
        (lost as f64) * 100.0 / (sent as f64)
    } else {
        0.0
    };

    if valid.is_empty() {
        return PingHopStats {
            label: label.to_string(),
            target_ip: target_ip.to_string(),
            sent,
            received: 0,
            lost,
            loss_pct,
            samples_ms,
            min_ms: None,
            median_ms: None,
            p95_ms: None,
            max_ms: None,
            jitter_ms: None,
            severity: Severity::Bad,
        };
    }

    let mut sorted = valid.clone();
    sorted.sort_by(f64::total_cmp);
    let min_ms = sorted[0];
    let max_ms = sorted[sorted.len() - 1];
    let median_ms = sorted[sorted.len() / 2];
    let p95_idx = ((sorted.len() as f64 * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    let p95_ms = sorted[p95_idx];

    let jitter_ms = if valid.len() >= 2 {
        let sum_diff: f64 = valid.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
        Some(sum_diff / ((valid.len() - 1) as f64))
    } else {
        Some(0.0)
    };
    let jit = jitter_ms.unwrap_or(0.0);

    let severity = if is_local_gateway {
        if loss_pct >= 5.0 || jit >= 5.0 || p95_ms >= 15.0 {
            Severity::Bad
        } else if loss_pct > 0.0 || jit >= 2.0 || p95_ms >= 6.0 {
            Severity::Warn
        } else {
            Severity::Ok
        }
    } else if loss_pct >= 5.0 || jit >= 12.0 || (p95_ms - median_ms) >= 35.0 {
        Severity::Bad
    } else if loss_pct > 0.0 || jit >= 4.0 || (p95_ms - median_ms) >= 15.0 {
        Severity::Warn
    } else {
        Severity::Ok
    };

    PingHopStats {
        label: label.to_string(),
        target_ip: target_ip.to_string(),
        sent,
        received,
        lost,
        loss_pct,
        samples_ms,
        min_ms: Some(min_ms),
        median_ms: Some(median_ms),
        p95_ms: Some(p95_ms),
        max_ms: Some(max_ms),
        jitter_ms,
        severity,
    }
}

pub(crate) fn diagnose_dual_hop(
    adapter_kind: NetworkAdapterKind,
    gateway: Option<&PingHopStats>,
    internet: &PingHopStats,
) -> (Severity, String) {
    let gw_sev = gateway.map(|g| g.severity);

    if internet.received == 0 {
        return (
            Severity::Bad,
            format!(
                "100% packet loss reaching {} (target blocked ICMP or offline). Try 1.1.1.1 or 8.8.8.8.",
                internet.target_ip
            ),
        );
    }

    if let Some(gw) = gateway {
        if gw.severity >= Severity::Warn {
            let medium_hint = if adapter_kind == NetworkAdapterKind::Wifi {
                "You are on Wi-Fi — local wireless contention is causing spikes before traffic even leaves your home. Switch to wired Ethernet if possible."
            } else {
                "Local router hop is unstable — check your Ethernet cable, NIC power saving/EEE, router QoS/SQM, or heavy local LAN traffic."
            };
            let sev = gw.severity.max(internet.severity);
            return (
                sev,
                format!(
                    "Local LAN / Router jitter detected (gateway {}: median {:.2} ms, jitter {:.2} ms, {:.0}% loss). {medium_hint}",
                    gw.target_ip,
                    gw.median_ms.unwrap_or(0.0),
                    gw.jitter_ms.unwrap_or(0.0),
                    gw.loss_pct
                ),
            );
        }
    }

    if internet.severity >= Severity::Warn {
        let gw_note = gateway
            .map(|g| {
                format!(
                    "Local gateway ({}) is rock-solid ({:.2} ms median, 0% loss) — ",
                    g.target_ip,
                    g.median_ms.unwrap_or(0.0)
                )
            })
            .unwrap_or_default();
        return (
            internet.severity,
            format!(
                "{gw_note}jitter/loss occurs past your router on the ISP/upstream hop ({}: median {:.1} ms, p95 {:.1} ms, jitter {:.1} ms, {:.0}% loss). Check background uploads/downloads or enable SQM/QoS on your router.",
                internet.target_ip,
                internet.median_ms.unwrap_or(0.0),
                internet.p95_ms.unwrap_or(0.0),
                internet.jitter_ms.unwrap_or(0.0),
                internet.loss_pct
            ),
        );
    }

    let sev = gw_sev.map_or(internet.severity, |g| g.max(internet.severity));
    let gw_str = gateway
        .map(|g| {
            format!(
                "Gateway {:.2} ms (jitter {:.2} ms) · ",
                g.median_ms.unwrap_or(0.0),
                g.jitter_ms.unwrap_or(0.0)
            )
        })
        .unwrap_or_default();
    (
        sev,
        format!(
            "Clean & stable network path — {gw_str}Internet ({}) {:.1} ms median / {:.1} ms p95 (jitter {:.1} ms, 0% loss).",
            internet.target_ip,
            internet.median_ms.unwrap_or(0.0),
            internet.p95_ms.unwrap_or(0.0),
            internet.jitter_ms.unwrap_or(0.0)
        ),
    )
}

/// Runs an on-demand dual-hop ICMP ping jitter & packet-loss probe (local gateway + internet IP).
///
/// Uses pure Win32 `iphlpapi.dll` (`IcmpSendEcho`), requiring zero administrator rights.
pub fn measure_network_jitter_benchmark(
    target_ip: &str,
    count: usize,
    interval_ms: u64,
) -> Result<NetworkJitterBenchmark> {
    let trimmed = target_ip.trim();
    let dest_ip: Ipv4Addr = trimmed.parse().map_err(|_| PolledError::Win32 {
        context: "measure_network_jitter_benchmark".to_string(),
        message: format!(
            "Invalid IPv4 target '{trimmed}' (expected e.g. 1.1.1.1, 8.8.8.8, or a server IPv4)"
        ),
    })?;

    let adapters = list_network_adapters().unwrap_or_default();
    let primary = adapters.iter().find(|a| a.is_primary).or(adapters.first());
    let adapter_name = primary
        .map(|a| format!("{} ({})", a.name, a.description))
        .unwrap_or_else(|| "Active Network Adapter".to_string());
    let adapter_kind = primary.map_or(NetworkAdapterKind::Other, |a| a.kind);
    let gateway_ip: Option<Ipv4Addr> = primary
        .and_then(|a| a.gateway_ipv4.as_deref())
        .and_then(|s| s.parse::<Ipv4Addr>().ok());

    // SAFETY: Creating standard Win32 user-mode ICMP handle.
    let raw_handle = unsafe { IcmpCreateFile() }.map_err(|e| PolledError::Win32 {
        context: "IcmpCreateFile".to_string(),
        message: e.to_string(),
    })?;
    let handle = IcmpHandleGuard(raw_handle);

    let rounds = count.clamp(5, 60);
    let step = Duration::from_millis(interval_ms.clamp(100, 1000));
    let started = Instant::now();

    let mut gw_samples: Vec<Option<f64>> = Vec::with_capacity(rounds);
    let mut inet_samples: Vec<Option<f64>> = Vec::with_capacity(rounds);

    for i in 0..rounds {
        let round_start = Instant::now();
        if let Some(gw) = gateway_ip {
            gw_samples.push(icmp_ping_once(handle.0, gw, 500));
        }
        inet_samples.push(icmp_ping_once(handle.0, dest_ip, 750));

        if i + 1 < rounds {
            let spent = round_start.elapsed();
            if spent < step {
                std::thread::sleep(step - spent);
            }
        }
    }

    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let gateway_stats =
        gateway_ip.map(|gw| compute_hop_stats("Local Gateway", &gw.to_string(), gw_samples, true));
    let internet_stats =
        compute_hop_stats("Internet Target", &dest_ip.to_string(), inet_samples, false);
    let (severity, diagnosis) =
        diagnose_dual_hop(adapter_kind, gateway_stats.as_ref(), &internet_stats);

    Ok(NetworkJitterBenchmark {
        adapter_name,
        adapter_kind,
        elapsed_ms,
        gateway: gateway_stats,
        internet: internet_stats,
        diagnosis,
        severity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_nic_eee_power_saving_and_interrupt_moderation_flags() {
        let flags = inspect_nic_latency_flags_from_values(&[
            ("*EEE", "1"),
            ("EnableGreenEthernet", "1"),
            ("PowerSavingMode", "0"),
            ("*InterruptModeration", "1"),
        ]);
        assert!(flags.iter().any(|f| f.contains("EEE")));
        assert!(flags.iter().any(|f| f.contains("Green Ethernet")));
        assert!(flags.iter().any(|f| f.contains("Interrupt Moderation")));
        assert!(!flags.iter().any(|f| f.contains("NIC Power Saving")));

        let clean = inspect_nic_latency_flags_from_values(&[
            ("*EEE", "0"),
            ("EnableGreenEthernet", "0"),
            ("*InterruptModeration", "0"),
        ]);
        assert!(clean.is_empty());
    }

    #[test]
    fn separates_local_router_jitter_from_isp_upstream_jitter() {
        let clean_gw = compute_hop_stats(
            "Local Gateway",
            "192.168.1.1",
            vec![Some(0.4), Some(0.5), Some(0.4), Some(0.5), Some(0.4)],
            true,
        );
        assert_eq!(clean_gw.severity, Severity::Ok);

        let spikey_isp = compute_hop_stats(
            "Internet Target",
            "1.1.1.1",
            vec![Some(14.0), Some(45.0), Some(15.0), Some(52.0), Some(14.5)],
            false,
        );
        assert_eq!(spikey_isp.severity, Severity::Bad);

        let (sev, diag) =
            diagnose_dual_hop(NetworkAdapterKind::Ethernet, Some(&clean_gw), &spikey_isp);
        assert_eq!(sev, Severity::Bad);
        assert!(diag.contains("ISP/upstream"));

        let bad_wifi_gw = compute_hop_stats(
            "Local Gateway",
            "192.168.1.1",
            vec![Some(2.0), Some(22.0), Some(3.0), None, Some(18.0)],
            true,
        );
        let (sev2, diag2) =
            diagnose_dual_hop(NetworkAdapterKind::Wifi, Some(&bad_wifi_gw), &spikey_isp);
        assert_eq!(sev2, Severity::Bad);
        assert!(diag2.contains("Wi-Fi"));
    }

    #[test]
    fn parses_comma_and_space_delimited_registry_dns_lists() {
        assert_eq!(
            parse_registry_dns_list("1.1.1.1,1.0.0.1"),
            vec!["1.1.1.1".to_string(), "1.0.0.1".to_string()]
        );
        assert_eq!(
            parse_registry_dns_list("192.168.178.1  8.8.8.8 192.168.178.1 0.0.0.0"),
            vec!["192.168.178.1".to_string(), "8.8.8.8".to_string()]
        );
        assert!(parse_registry_dns_list("").is_empty());
    }
}
