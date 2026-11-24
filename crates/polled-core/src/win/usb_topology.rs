// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Read-only Windows USB Port & Hub Topology inspector for 4K/8K gaming mice.
//!
//! Uses only read-only Windows Registry queries (`HKLM\SYSTEM\CurrentControlSet\Services\*\Enum`
//! for currently active PnP device instances + `HKLM\SYSTEM\CurrentControlSet\Enum\USB\*`
//! `LocationInformation` & `ParentIdPrefix`) and `hidapi` — zero admin, zero kernel drivers.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::Result;
use crate::win::{registry, RegHive, UsbMouseTopology, UsbSharedDevice, UsbSharedDeviceKind};

const USB_ENUM_ROOT: &str = r"SYSTEM\CurrentControlSet\Enum";

/// Parses Windows `LocationInformation` strings like `"Port_#0004.Hub_#0002"` -> `Some((4, 2))`.
pub fn parse_usb_port_hub(location: &str) -> Option<(u32, u32)> {
    let mut port = None;
    let mut hub = None;
    for part in location.split('.') {
        let trimmed = part.trim();
        if let Some(rest) = trimmed
            .strip_prefix("Port_#")
            .or_else(|| trimmed.strip_prefix("port_#"))
        {
            port = rest.parse::<u32>().ok();
        } else if let Some(rest) = trimmed
            .strip_prefix("Hub_#")
            .or_else(|| trimmed.strip_prefix("hub_#"))
        {
            hub = rest.parse::<u32>().ok();
        }
    }
    Some((port?, hub?))
}

fn clean_desc(raw: &str) -> String {
    raw.rsplit(';')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(raw)
        .to_string()
}

/// Reads currently active PnP instance paths from `HKLM\SYSTEM\CurrentControlSet\Services\<svc>\Enum`.
fn active_service_instances(service_name: &str) -> Vec<String> {
    let key = format!(r"SYSTEM\CurrentControlSet\Services\{service_name}\Enum");
    let count = registry::get_dword(RegHive::Hklm, &key, "Count")
        .ok()
        .flatten()
        .unwrap_or(0)
        .min(512);
    let mut out = Vec::with_capacity(count as usize);
    for idx in 0..count {
        if let Ok(Some(inst)) = registry::get_string(RegHive::Hklm, &key, &idx.to_string()) {
            if !inst.trim().is_empty() {
                out.push(inst);
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
struct ActiveUsbNode {
    instance_id: String,
    name: String,
    port: Option<u32>,
    hub: Option<u32>,
    parent_id_prefix: Option<String>,
    is_root_hub: bool,
}

fn parse_vid_pid_from_instance(inst: &str) -> Option<(u16, u16)> {
    let upper = inst.to_ascii_uppercase();
    let vid_pos = upper.find("VID_")?;
    let pid_pos = upper.find("PID_")?;
    let vid = u16::from_str_radix(upper.get(vid_pos + 4..vid_pos + 8)?, 16).ok()?;
    let pid = u16::from_str_radix(upper.get(pid_pos + 4..pid_pos + 8)?, 16).ok()?;
    Some((vid, pid))
}

fn read_usb_node(instance_id: &str) -> Option<ActiveUsbNode> {
    if !instance_id.to_ascii_uppercase().starts_with(r"USB\") {
        return None;
    }
    let key = format!(r"{USB_ENUM_ROOT}\{instance_id}");
    let location = registry::get_string(RegHive::Hklm, &key, "LocationInformation")
        .ok()
        .flatten();
    let (port, hub) = location
        .as_deref()
        .and_then(parse_usb_port_hub)
        .map(|(p, h)| (Some(p), Some(h)))
        .unwrap_or((None, None));
    let name = registry::get_string(RegHive::Hklm, &key, "FriendlyName")
        .ok()
        .flatten()
        .or_else(|| {
            registry::get_string(RegHive::Hklm, &key, "DeviceDesc")
                .ok()
                .flatten()
        })
        .map(|s| clean_desc(&s))
        .unwrap_or_else(|| instance_id.to_string());
    let parent_id_prefix = registry::get_string(RegHive::Hklm, &key, "ParentIdPrefix")
        .ok()
        .flatten();
    let upper = instance_id.to_ascii_uppercase();
    let is_root_hub = upper.contains("ROOT_HUB");

    Some(ActiveUsbNode {
        instance_id: instance_id.to_string(),
        name,
        port,
        hub,
        parent_id_prefix,
        is_root_hub,
    })
}

/// Inspects active USB hub placement and high-bandwidth sibling devices for all connected HID mice.
pub fn list_usb_topology() -> Result<Vec<UsbMouseTopology>> {
    // 1. Collect connected HID mouse VID:PIDs and display names from hidapi.
    let mut connected_mice: BTreeMap<(u16, u16), String> = BTreeMap::new();
    if let Ok(api) = hidapi::HidApi::new() {
        for dev in api.device_list() {
            if dev.usage_page() == 0x01 && dev.usage() == 0x02 && dev.vendor_id() != 0 {
                let pair = (dev.vendor_id(), dev.product_id());
                let label = dev
                    .product_string()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("USB Mouse {:04X}:{:04X}", pair.0, pair.1));
                connected_mice.entry(pair).or_insert(label);
            }
        }
    }
    if connected_mice.is_empty() {
        return Ok(Vec::new());
    }

    // 2. Collect currently active USB hubs (both root hubs and external/intermediate hubs).
    let mut active_hubs: Vec<ActiveUsbNode> = Vec::new();
    let mut seen_hub_ids: BTreeSet<String> = BTreeSet::new();
    for svc in ["USBHUB3", "usbhub"] {
        for inst in active_service_instances(svc) {
            if seen_hub_ids.insert(inst.to_ascii_uppercase()) {
                if let Some(node) = read_usb_node(&inst) {
                    active_hubs.push(node);
                }
            }
        }
    }

    // Build lookup of external (non-root) hub ParentIdPrefix -> hub display name.
    let external_hubs: Vec<(&str, &str)> = active_hubs
        .iter()
        .filter(|h| !h.is_root_hub)
        .filter_map(|h| Some((h.parent_id_prefix.as_deref()?, h.name.as_str())))
        .collect();
    let root_hub_count = active_hubs.iter().filter(|h| h.is_root_hub).count() as u32;

    // 3. Collect active high-bandwidth sibling USB devices grouped by `Hub_#`.
    let mut heavy_by_hub: BTreeMap<u32, Vec<UsbSharedDevice>> = BTreeMap::new();
    let heavy_services: &[(&str, UsbSharedDeviceKind)] = &[
        ("usbvideo", UsbSharedDeviceKind::CameraOrCapture),
        ("usbaudio", UsbSharedDeviceKind::UsbAudio),
        ("usbaudio2", UsbSharedDeviceKind::UsbAudio),
        ("USBSTOR", UsbSharedDeviceKind::UsbStorage),
        ("UASPStor", UsbSharedDeviceKind::UsbStorage),
    ];
    let mut seen_heavy: BTreeSet<(u32, String)> = BTreeSet::new();
    for &(svc, kind) in heavy_services {
        for inst in active_service_instances(svc) {
            // For MI_XX composite interfaces, inspect the parent USB device too if LocationInformation is absent.
            let node = read_usb_node(&inst).or_else(|| {
                parse_vid_pid_from_instance(&inst)
                    .and_then(|(v, p)| find_active_parent_usb_node(v, p))
            });
            if let Some(n) = node {
                if let Some(hub_num) = n.hub {
                    if seen_heavy.insert((hub_num, n.name.to_ascii_lowercase())) {
                        heavy_by_hub
                            .entry(hub_num)
                            .or_default()
                            .push(UsbSharedDevice { name: n.name, kind });
                    }
                }
            }
        }
    }

    // 4. Match active mouse USB parent instances from `usbccgp` and `hidusb`.
    let mut active_mouse_nodes: BTreeMap<(u16, u16), ActiveUsbNode> = BTreeMap::new();
    for svc in ["usbccgp", "hidusb"] {
        for inst in active_service_instances(svc) {
            let Some(pair) = parse_vid_pid_from_instance(&inst) else {
                continue;
            };
            if !connected_mice.contains_key(&pair) {
                continue;
            }
            // Prefer non-MI parent device node (which carries LocationInformation & Hub_#).
            if inst.to_ascii_uppercase().contains("&MI_") {
                continue;
            }
            if let Some(node) = read_usb_node(&inst) {
                if node.hub.is_some() || !active_mouse_nodes.contains_key(&pair) {
                    active_mouse_nodes.insert(pair, node);
                }
            }
        }
    }

    // 5. Build topology entries for each connected mouse.
    let mut results = Vec::new();
    for ((vid, pid), label) in connected_mice {
        let node = active_mouse_nodes
            .remove(&(vid, pid))
            .or_else(|| find_active_parent_usb_node(vid, pid));

        let (port_number, hub_number, behind_external_hub, hub_name) = match &node {
            Some(n) => {
                // Exact parent match: child instance ID starts with external hub's `ParentIdPrefix`.
                let inst_tail = n.instance_id.rsplit('\\').next().unwrap_or("");
                let matched_ext = external_hubs.iter().find(|(prefix, _)| {
                    !prefix.is_empty()
                        && inst_tail
                            .to_ascii_lowercase()
                            .starts_with(&prefix.to_ascii_lowercase())
                });
                if let Some((_, ext_name)) = matched_ext {
                    (n.port, n.hub, true, ext_name.to_string())
                } else if let Some(h) = n.hub {
                    // Fallback: if hub index exceeds root hub count and external hubs exist, note possible downstream hub.
                    let maybe_ext =
                        root_hub_count > 0 && h > root_hub_count && !external_hubs.is_empty();
                    let hname = if maybe_ext {
                        format!("External USB Hub (Hub #{h})")
                    } else {
                        format!("USB Root Hub #{h}")
                    };
                    (n.port, n.hub, maybe_ext, hname)
                } else {
                    (n.port, None, false, "Unknown USB Hub".to_string())
                }
            }
            None => (None, None, false, "Unknown USB Hub".to_string()),
        };

        let shared_heavy_devices = hub_number
            .and_then(|h| heavy_by_hub.get(&h))
            .cloned()
            .unwrap_or_default();

        results.push(UsbMouseTopology {
            name: label,
            vid_pid: format!("{vid:04x}:{pid:04x}"),
            port_number,
            hub_number,
            hub_name,
            behind_external_hub,
            shared_heavy_devices,
        });
    }

    Ok(results)
}

fn find_active_parent_usb_node(vid: u16, pid: u16) -> Option<ActiveUsbNode> {
    let key_prefix = format!(r"SYSTEM\CurrentControlSet\Enum\USB\VID_{vid:04X}&PID_{pid:04X}");
    let instances = registry::list_subkeys(RegHive::Hklm, &key_prefix).ok()?;
    for inst in instances {
        let full = format!(r"USB\VID_{vid:04X}&PID_{pid:04X}\{inst}");
        if let Some(node) = read_usb_node(&full) {
            if node.hub.is_some() {
                return Some(node);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_windows_port_and_hub_location_strings() {
        assert_eq!(parse_usb_port_hub("Port_#0004.Hub_#0002"), Some((4, 2)));
        assert_eq!(parse_usb_port_hub("Port_#0011.Hub_#0001"), Some((11, 1)));
        assert_eq!(parse_usb_port_hub("0000.0014.0000"), None);
    }

    #[test]
    fn extracts_vid_pid_from_pnp_instance_paths() {
        assert_eq!(
            parse_vid_pid_from_instance(r"USB\VID_3710&PID_5406\6&1234abcd&0&4"),
            Some((0x3710, 0x5406))
        );
        assert_eq!(
            parse_vid_pid_from_instance(r"USB\ROOT_HUB30\4&123456&0&0"),
            None
        );
    }
}
