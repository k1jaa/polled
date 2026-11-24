// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Read-only Windows USB Port & Hub Topology inspector for 4K/8K gaming mice.
//!
//! Walks the live Windows Plug-and-Play device tree via `cfgmgr32.dll`
//! (`CM_Get_Device_ID_ListW`, `CM_Locate_DevNodeW`, `CM_Get_Parent`, `CM_Get_Child`,
//! `CM_Get_Sibling`, `CM_Get_DevNode_Registry_PropertyW`) paired with `hidapi` product strings.
//!
//! Maps 100% of wired mice and composite 2.4 GHz 4K/8K dongles (including `usbccgp` multi-interface
//! receivers and vendor ASIO/USB audio DACs like Focusrite) to their exact physical port, hub,
//! PCI xHCI host controller, and shared high-bandwidth sibling devices — zero admin, zero drivers.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Child, CM_Get_DevNode_PropertyW, CM_Get_DevNode_Registry_PropertyW, CM_Get_Device_IDW,
    CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW, CM_Get_Parent, CM_Get_Sibling,
    CM_Locate_DevNodeW, CM_DRP_CLASS, CM_DRP_DEVICEDESC, CM_DRP_FRIENDLYNAME, CM_DRP_HARDWAREID,
    CM_DRP_LOCATION_INFORMATION, CM_DRP_SERVICE, CM_GETIDLIST_FILTER_ENUMERATOR,
    CM_GETIDLIST_FILTER_PRESENT, CM_LOCATE_DEVNODE_NORMAL, CR_SUCCESS,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_BusReportedDeviceDesc, DEVPROPTYPE, DEVPROP_TYPE_STRING,
};

use crate::error::Result;
use crate::win::{UsbMouseTopology, UsbSharedDevice, UsbSharedDeviceKind};

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

fn parse_vid_pid_from_instance(inst: &str) -> Option<(u16, u16)> {
    let upper = inst.to_ascii_uppercase();
    let vid_pos = upper.find("VID_")?;
    let pid_pos = upper.find("PID_")?;
    let vid = u16::from_str_radix(upper.get(vid_pos + 4..vid_pos + 8)?, 16).ok()?;
    let pid = u16::from_str_radix(upper.get(pid_pos + 4..pid_pos + 8)?, 16).ok()?;
    Some((vid, pid))
}

fn clean_desc(raw: &str) -> String {
    raw.rsplit(';')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(raw)
        .to_string()
}

fn is_generic_usb_name(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    lower.is_empty()
        || lower == "usb composite device"
        || lower == "usb input device"
        || lower == "hid keyboard device"
        || lower == "hid-tastatur"
        || lower == "usb-verbundgerät"
        || lower == "usb-eingabegerät"
        || lower.starts_with("hid-compliant ")
        || lower.starts_with("hid-konforme")
        || lower.starts_with("hid-konformer")
}

fn cm_get_bus_reported_desc(devinst: u32) -> Option<String> {
    let mut prop_type = DEVPROPTYPE(0);
    let mut byte_len = 0u32;
    // SAFETY: Querying required byte size for `DEVPKEY_Device_BusReportedDeviceDesc`.
    let _ = unsafe {
        CM_Get_DevNode_PropertyW(
            devinst,
            &DEVPKEY_Device_BusReportedDeviceDesc,
            &mut prop_type,
            None,
            &mut byte_len,
            0,
        )
    };
    if !(2..=65_536).contains(&byte_len) {
        return None;
    }
    let mut buf = vec![0u16; (byte_len as usize).div_ceil(2)];
    // SAFETY: `buf` has at least `byte_len` bytes of writable storage.
    let cr = unsafe {
        CM_Get_DevNode_PropertyW(
            devinst,
            &DEVPKEY_Device_BusReportedDeviceDesc,
            &mut prop_type,
            Some(buf.as_mut_ptr().cast()),
            &mut byte_len,
            0,
        )
    };
    if cr != CR_SUCCESS || prop_type != DEVPROP_TYPE_STRING {
        return None;
    }
    let u16_len = (byte_len as usize / 2).min(buf.len());
    let nul_pos = buf[..u16_len]
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(u16_len);
    let s = String::from_utf16_lossy(&buf[..nul_pos]).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn to_wide_null(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn cm_get_device_id(devinst: u32) -> Option<String> {
    let mut buf = [0u16; 512];
    // SAFETY: `buf` is valid writable UTF-16 storage for `CM_Get_Device_IDW`.
    let cr = unsafe { CM_Get_Device_IDW(devinst, &mut buf, 0) };
    if cr != CR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn cm_get_string_prop(devinst: u32, prop: u32) -> Option<String> {
    let mut byte_len = 0u32;
    // SAFETY: Querying required buffer size with `buffer = None`.
    let _ =
        unsafe { CM_Get_DevNode_Registry_PropertyW(devinst, prop, None, None, &mut byte_len, 0) };
    if !(2..=65_536).contains(&byte_len) {
        return None;
    }
    let mut buf = vec![0u16; (byte_len as usize).div_ceil(2)];
    // SAFETY: `buf` has at least `byte_len` bytes of writable storage.
    let cr = unsafe {
        CM_Get_DevNode_Registry_PropertyW(
            devinst,
            prop,
            None,
            Some(buf.as_mut_ptr().cast()),
            &mut byte_len,
            0,
        )
    };
    if cr != CR_SUCCESS {
        return None;
    }
    let u16_len = (byte_len as usize / 2).min(buf.len());
    let nul_pos = buf[..u16_len]
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(u16_len);
    let s = String::from_utf16_lossy(&buf[..nul_pos]).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn cm_get_multi_sz_prop(devinst: u32, prop: u32) -> Vec<String> {
    let mut byte_len = 0u32;
    // SAFETY: Querying required buffer size.
    let _ =
        unsafe { CM_Get_DevNode_Registry_PropertyW(devinst, prop, None, None, &mut byte_len, 0) };
    if !(2..=65_536).contains(&byte_len) {
        return Vec::new();
    }
    let mut buf = vec![0u16; (byte_len as usize).div_ceil(2)];
    // SAFETY: `buf` has at least `byte_len` bytes of writable storage.
    let cr = unsafe {
        CM_Get_DevNode_Registry_PropertyW(
            devinst,
            prop,
            None,
            Some(buf.as_mut_ptr().cast()),
            &mut byte_len,
            0,
        )
    };
    if cr != CR_SUCCESS {
        return Vec::new();
    }
    let u16_len = (byte_len as usize / 2).min(buf.len());
    buf[..u16_len]
        .split(|&c| c == 0)
        .filter(|slice| !slice.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

fn cm_children(devinst: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut child = 0u32;
    // SAFETY: Walking live PnP children via standard `cfgmgr32.dll` calls.
    if unsafe { CM_Get_Child(&mut child, devinst, 0) } != CR_SUCCESS {
        return out;
    }
    out.push(child);
    let mut current = child;
    for _ in 0..256 {
        let mut sibling = 0u32;
        if unsafe { CM_Get_Sibling(&mut sibling, current, 0) } != CR_SUCCESS {
            break;
        }
        out.push(sibling);
        current = sibling;
    }
    out
}

fn cm_present_usb_device_ids() -> Vec<String> {
    let filter_w = to_wide_null("USB");
    let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
    let mut len = 0u32;
    // SAFETY: Standard 2-step `CM_Get_Device_ID_List_SizeW` + `CM_Get_Device_ID_ListW`.
    if unsafe { CM_Get_Device_ID_List_SizeW(&mut len, PCWSTR(filter_w.as_ptr()), flags) }
        != CR_SUCCESS
        || len <= 1
    {
        return Vec::new();
    }
    let mut buf = vec![0u16; len as usize];
    if unsafe { CM_Get_Device_ID_ListW(PCWSTR(filter_w.as_ptr()), &mut buf, flags) } != CR_SUCCESS {
        return Vec::new();
    }
    buf.split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

fn node_best_name(devinst: u32) -> Option<String> {
    cm_get_string_prop(devinst, CM_DRP_FRIENDLYNAME)
        .or_else(|| cm_get_string_prop(devinst, CM_DRP_DEVICEDESC))
        .map(|s| clean_desc(&s))
}

#[derive(Debug, Default)]
struct SubtreeClassification {
    is_mouse: bool,
    is_bluetooth: bool,
    heavy_kind: Option<UsbSharedDeviceKind>,
    best_heavy_name: Option<String>,
    best_any_name: Option<String>,
}

fn classify_usb_subtree(devinst: u32, depth: usize, out: &mut SubtreeClassification) {
    if depth > 6 {
        return;
    }
    let service = cm_get_string_prop(devinst, CM_DRP_SERVICE)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let class = cm_get_string_prop(devinst, CM_DRP_CLASS)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let name = node_best_name(devinst);

    if let Some(ref n) = name {
        if !is_generic_usb_name(n) && out.best_any_name.is_none() {
            out.best_any_name = Some(n.clone());
        }
    }

    if class == "bluetooth" || service == "bthusb" || service == "ibtusb" {
        out.is_bluetooth = true;
    }

    if service == "mouhid" || class == "mouse" {
        out.is_mouse = true;
    } else {
        for hw_id in cm_get_multi_sz_prop(devinst, CM_DRP_HARDWAREID) {
            if hw_id.eq_ignore_ascii_case("HID_DEVICE_SYSTEM_MOUSE") {
                out.is_mouse = true;
                break;
            }
        }
    }

    let this_heavy = if service == "usbvideo" || class == "camera" || class == "image" {
        Some(UsbSharedDeviceKind::CameraOrCapture)
    } else if service == "usbaudio"
        || service == "usbaudio2"
        || class == "media"
        || class == "audioendpoint"
    {
        Some(UsbSharedDeviceKind::UsbAudio)
    } else if service == "usbstor" || service == "uaspstor" || class == "diskdrive" {
        Some(UsbSharedDeviceKind::UsbStorage)
    } else {
        None
    };

    if let Some(kind) = this_heavy {
        if out.heavy_kind.is_none() {
            out.heavy_kind = Some(kind);
        }
        if let Some(ref n) = name {
            if !is_generic_usb_name(n) && out.best_heavy_name.is_none() {
                out.best_heavy_name = Some(n.clone());
            }
        }
    }

    for child in cm_children(devinst) {
        classify_usb_subtree(child, depth + 1, out);
    }
}

#[derive(Debug, Clone)]
struct FoundMouseOnHub {
    devinst: u32,
    name: String,
    vid_pid: String,
    port_number: Option<u32>,
    hub_number: Option<u32>,
    hub_name: String,
    behind_external_hub: bool,
}

#[derive(Debug, Clone)]
struct FoundHeavyOnHub {
    devinst: u32,
    device: UsbSharedDevice,
}

fn walk_usb_hub(
    hub_devinst: u32,
    hub_label: &str,
    controller_short: Option<&str>,
    behind_external_hub: bool,
    depth: usize,
    hidapi_mice: &BTreeMap<(u16, u16), String>,
    all_mice_out: &mut Vec<UsbMouseTopology>,
) -> Vec<FoundHeavyOnHub> {
    if depth > 6 {
        return Vec::new();
    }

    let mut mice_here: Vec<FoundMouseOnHub> = Vec::new();
    let mut heavy_here: Vec<FoundHeavyOnHub> = Vec::new();

    for child in cm_children(hub_devinst) {
        let Some(child_id) = cm_get_device_id(child) else {
            continue;
        };
        if !child_id.to_ascii_uppercase().starts_with(r"USB\") {
            continue;
        }

        let service = cm_get_string_prop(child, CM_DRP_SERVICE)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let child_desc = node_best_name(child).unwrap_or_else(|| child_id.clone());

        if service == "usbhub3" || service == "usbhub" {
            let ext_label = match controller_short {
                Some(ctrl) => format!("{child_desc} ({ctrl})"),
                None => child_desc.clone(),
            };
            let downstream_heavy = walk_usb_hub(
                child,
                &ext_label,
                controller_short,
                true,
                depth + 1,
                hidapi_mice,
                all_mice_out,
            );
            // Downstream isochronous devices behind an external hub also consume upstream root hub bandwidth.
            heavy_here.extend(downstream_heavy);
            continue;
        }

        let mut cls = SubtreeClassification::default();
        classify_usb_subtree(child, 0, &mut cls);

        let vid_pid_pair = parse_vid_pid_from_instance(&child_id);
        let is_hidapi_mouse = vid_pid_pair.is_some_and(|pair| hidapi_mice.contains_key(&pair));
        let is_mouse = cls.is_mouse || is_hidapi_mouse;

        let location = cm_get_string_prop(child, CM_DRP_LOCATION_INFORMATION);
        let (port_number, hub_number) = location
            .as_deref()
            .and_then(parse_usb_port_hub)
            .map(|(p, h)| (Some(p), Some(h)))
            .unwrap_or((None, None));

        let child_bus_name = cm_get_bus_reported_desc(child).filter(|s| !is_generic_usb_name(s));

        if is_mouse {
            let vid_pid = vid_pid_pair
                .map(|(v, p)| format!("{v:04x}:{p:04x}"))
                .unwrap_or_else(|| "usb".to_string());
            let mouse_name = child_bus_name
                .clone()
                .or_else(|| vid_pid_pair.and_then(|pair| hidapi_mice.get(&pair).cloned()))
                .or_else(|| cls.best_any_name.clone())
                .unwrap_or_else(|| {
                    if is_generic_usb_name(&child_desc) {
                        format!("USB Mouse ({vid_pid})")
                    } else {
                        child_desc.clone()
                    }
                });

            mice_here.push(FoundMouseOnHub {
                devinst: child,
                name: mouse_name,
                vid_pid,
                port_number,
                hub_number,
                hub_name: hub_label.to_string(),
                behind_external_hub,
            });
        }

        if !cls.is_bluetooth {
            if let Some(kind) = cls.heavy_kind {
                let heavy_name = child_bus_name
                    .or(cls.best_heavy_name)
                    .or(cls.best_any_name)
                    .unwrap_or(child_desc);
                heavy_here.push(FoundHeavyOnHub {
                    devinst: child,
                    device: UsbSharedDevice {
                        name: heavy_name,
                        kind,
                    },
                });
            }
        }
    }

    for mouse in mice_here {
        let mut seen_names: BTreeSet<String> = BTreeSet::new();
        let mut shared_heavy_devices = Vec::new();
        for h in &heavy_here {
            if h.devinst == mouse.devinst {
                continue;
            }
            if seen_names.insert(h.device.name.to_ascii_lowercase()) {
                shared_heavy_devices.push(h.device.clone());
            }
        }
        all_mice_out.push(UsbMouseTopology {
            name: mouse.name,
            vid_pid: mouse.vid_pid,
            port_number: mouse.port_number,
            hub_number: mouse.hub_number,
            hub_name: mouse.hub_name,
            behind_external_hub: mouse.behind_external_hub,
            shared_heavy_devices,
        });
    }

    heavy_here
}

fn shorten_controller_name(raw: &str) -> String {
    raw.replace(" - 1.10 (Microsoft)", "")
        .replace(" - 1.20 (Microsoft)", "")
        .replace(" (Microsoft)", "")
        .trim()
        .to_string()
}

/// Inspects active USB hub placement, PCI xHCI controller, and high-bandwidth sibling devices
/// for all connected USB HID mice using live Win32 `cfgmgr32.dll` PnP device-tree traversal.
pub fn list_usb_topology() -> Result<Vec<UsbMouseTopology>> {
    // 1. Collect friendly product names from hidapi for all active HID mouse interfaces.
    let mut hidapi_mice: BTreeMap<(u16, u16), String> = BTreeMap::new();
    if let Ok(api) = hidapi::HidApi::new() {
        for dev in api.device_list() {
            if dev.usage_page() == 0x01 && dev.usage() == 0x02 && dev.vendor_id() != 0 {
                let pair = (dev.vendor_id(), dev.product_id());
                if let Some(prod) = dev
                    .product_string()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    hidapi_mice.entry(pair).or_insert_with(|| prod.to_string());
                }
            }
        }
    }

    // 2. Locate all present USB Root Hubs and walk their live PnP subtrees.
    let mut results: Vec<UsbMouseTopology> = Vec::new();
    let mut seen_root_hubs: BTreeSet<String> = BTreeSet::new();
    let mut root_idx = 0u32;

    for id in cm_present_usb_device_ids() {
        let upper = id.to_ascii_uppercase();
        if !upper.starts_with(r"USB\ROOT_HUB") {
            continue;
        }
        if !seen_root_hubs.insert(upper) {
            continue;
        }

        let id_w = to_wide_null(&id);
        let mut root_devinst = 0u32;
        // SAFETY: Locating a present PnP device node returned by `CM_Get_Device_ID_ListW`.
        if unsafe {
            CM_Locate_DevNodeW(
                &mut root_devinst,
                PCWSTR(id_w.as_ptr()),
                CM_LOCATE_DEVNODE_NORMAL,
            )
        } != CR_SUCCESS
        {
            continue;
        }

        root_idx += 1;
        let mut parent_ctrl = 0u32;
        let controller_short =
            if unsafe { CM_Get_Parent(&mut parent_ctrl, root_devinst, 0) } == CR_SUCCESS {
                node_best_name(parent_ctrl).map(|s| shorten_controller_name(&s))
            } else {
                None
            };

        let root_desc =
            node_best_name(root_devinst).unwrap_or_else(|| format!("USB Root Hub #{root_idx}"));
        let root_label = match controller_short.as_deref() {
            Some(ctrl) => format!("{root_desc} · {ctrl}"),
            None => root_desc,
        };

        walk_usb_hub(
            root_devinst,
            &root_label,
            controller_short.as_deref(),
            false,
            0,
            &hidapi_mice,
            &mut results,
        );
    }

    Ok(results)
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

    #[test]
    fn recognizes_generic_usb_placeholder_names() {
        assert!(is_generic_usb_name("USB Composite Device"));
        assert!(is_generic_usb_name("USB-Verbundgerät"));
        assert!(is_generic_usb_name("HID-compliant consumer control device"));
        assert!(is_generic_usb_name("HID-compliant system controller"));
        assert!(!is_generic_usb_name("Focusrite USB Audio"));
        assert!(!is_generic_usb_name("Pulsar 8K Dongle"));
    }
}
