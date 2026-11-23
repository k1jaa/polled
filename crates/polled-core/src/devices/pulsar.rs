// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

//! Pulsar X2 CrazyLight (Wireless & Wired) HID protocol backend.
//!
//! # Protocol Attribution & License
//! Every USB VID/PID, HID report ID, frame checksum, register address, and DPI encoding formula
//! in this module is taken directly from the hardware-verified documentation and drivers in:
//! - `packerlschupfer/pulsar-mouse-linux` (<https://github.com/packerlschupfer/pulsar-mouse-linux>)
//!   - `docs/protocol-x2-crazylight.md`
//!   - `src/pulsar_mouse/drivers/x2_crazylight.py`
//!   - `src/pulsar_mouse/drivers/x2_crazylight_wired.py`
//!   - `src/pulsar_mouse/drivers/nordic.py`
//!   - Licensed under the **MIT License**, Copyright (c) 2026 Packerlschupfer (compatible with GPL-3.0).
//!
//! No HID bytes are guessed. Destructive commands (such as `0x09` factory reset) are never issued.
//!
//! # Flash-Wear Protection
//! `CMD_MEM_SET (0x07)` writes to the active profile's persistent onboard storage. Because no
//! volatile RAM-only write command is documented for this firmware, [`DeviceCapabilities::supports_volatile_writes`]
//! is set to `false`:
//! - [`crate::devices::SafeDeviceController`] diffs before writing and enforces a write cooldown.
//! - [`PulsarX2CrazyLightDevice::apply_settings`] further diffs register-by-register so unchanged
//!   settings generate zero `CMD_MEM_SET` writes.

use hidapi::{HidApi, HidDevice};

use crate::devices::{
    DeviceCapabilities, DeviceDriver, DeviceInfo, LiftOffDistance, MouseDevice, MouseSettings,
    SupportTier, WriteMode,
};
use crate::error::{PolledError, Result};
use crate::hid::parse_hid_report_descriptor;

// --- USB & HID Transport Constants (MIT: packerlschupfer/pulsar-mouse-linux) ---
pub const PULSAR_VID: u16 = 0x3710;
pub const X2_CRAZYLIGHT_WIRELESS_PID: u16 = 0x5406;
pub const X2_CRAZYLIGHT_WIRED_PID: u16 = 0x3414;

/// Config interface index carrying 17-byte Report ID `0x08` command packets.
pub const CONFIG_INTERFACE_NUM: i32 = 1;
pub const REPORT_ID: u8 = 0x08;
pub const REPORT_LEN: usize = 17;

// Identity pairs answered by `CMD_HANDSHAKE (0x01)` on the shared CrazyLight dongle (`3710:5406`):
// - (0x57, 0x01): Pulsar X2 CrazyLight Mini (confirmed via live `polled --detect`)
// - (0x57, 0x03): Pulsar X2 CrazyLight (documented in packerlschupfer/pulsar-mouse-linux)
// - (0x57, 0x68): Pulsar X3 LHD CrazyLight Medium (non-X2 model, skipped here)
pub const X2_CRAZYLIGHT_CID: u8 = 0x57;
pub const X2_CRAZYLIGHT_MINI_MID: u8 = 0x01;
pub const X2_CRAZYLIGHT_MID: u8 = 0x03;
const X3_LHD_CRAZYLIGHT_MID: u8 = 0x68;

fn resolve_crazylight_model_name(cid: u8, mid: u8, connection: ConnectionKind) -> Option<String> {
    let suffix = match connection {
        ConnectionKind::WirelessDongle => "Wireless",
        ConnectionKind::WiredUsb => "Wired",
    };
    match (cid, mid) {
        (X2_CRAZYLIGHT_CID, X2_CRAZYLIGHT_MINI_MID) => {
            Some(format!("Pulsar X2 CrazyLight Mini {suffix}"))
        }
        (X2_CRAZYLIGHT_CID, X2_CRAZYLIGHT_MID) => Some(format!("Pulsar X2 CrazyLight {suffix}")),
        (X2_CRAZYLIGHT_CID, X3_LHD_CRAZYLIGHT_MID) => None,
        _ => Some(format!(
            "Pulsar CrazyLight {suffix} (cid=0x{cid:02x}, mid=0x{mid:02x})"
        )),
    }
}

// --- Command IDs ---
const CMD_HANDSHAKE: u8 = 0x01;
const CMD_MEM_SET: u8 = 0x07;
const CMD_MEM_GET: u8 = 0x08;
const CMD_EVENT: u8 = 0x0A;

// --- Active-Profile Register Addresses ---
const ADDR_POLLING_RATE: u16 = 0x0000;
const ADDR_ACTIVE_DPI_STAGE: u16 = 0x0004;
const ADDR_LOD_MM: u16 = 0x000A;
const ADDR_DPI_BASE: u16 = 0x000C;
const DPI_STAGE_STRIDE: u16 = 4;
const MAX_DPI_STAGES: u8 = 6;
const ADDR_DEBOUNCE: u16 = 0x00A9;

const READ_TIMEOUT_MS: i32 = 1000;
const MAX_SKIPPED_REPORTS: usize = 8;

/// Computes the Pulsar Nordic complement checksum: `(0x55 - sum(bytes)) & 0xFF`.
pub fn checksum(bytes: &[u8]) -> u8 {
    let sum = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    0x55u8.wrapping_sub(sum)
}

/// Builds a 17-byte Report ID `0x08` command frame with its trailing byte-16 checksum.
pub fn build_packet(cmd: u8, addr: u16, len: u8, payload: &[u8]) -> [u8; REPORT_LEN] {
    let mut pkt = [0u8; REPORT_LEN];
    pkt[0] = REPORT_ID;
    pkt[1] = cmd;
    pkt[2] = 0x00;
    pkt[3] = (addr >> 8) as u8;
    pkt[4] = (addr & 0xFF) as u8;
    pkt[5] = len;
    let copy_len = payload.len().min(10);
    pkt[6..6 + copy_len].copy_from_slice(&payload[..copy_len]);
    pkt[16] = checksum(&pkt[..16]);
    pkt
}

pub fn verify_packet_checksum(pkt: &[u8]) -> bool {
    pkt.len() >= REPORT_LEN && pkt[0] == REPORT_ID && checksum(&pkt[..16]) == pkt[16]
}

/// Encodes polling rate Hz into the X2 CrazyLight register code (`0x0000`).
pub fn encode_polling_rate(hz: u16) -> Option<u8> {
    match hz {
        1000 => Some(0x01),
        500 => Some(0x02),
        250 => Some(0x04),
        125 => Some(0x08),
        2000 => Some(0x10),
        4000 => Some(0x20),
        8000 => Some(0x40),
        _ => None,
    }
}

/// Decodes the X2 CrazyLight polling rate register byte (`0x0000`) into Hz.
pub fn decode_polling_rate(code: u8) -> Option<u16> {
    match code {
        0x01 => Some(1000),
        0x02 => Some(500),
        0x04 => Some(250),
        0x08 => Some(125),
        0x10 => Some(2000),
        0x20 => Some(4000),
        0x40 => Some(8000),
        _ => None,
    }
}

/// Encodes lift-off distance into the X2 CrazyLight register code (`0x000A`).
/// Note: `0.7 mm` is `0x03` (added after `1 mm` = `0x01` and `2 mm` = `0x02`).
pub fn encode_lod(lod: LiftOffDistance) -> u8 {
    match lod {
        LiftOffDistance::Mm10 => 0x01,
        LiftOffDistance::Mm20 => 0x02,
        LiftOffDistance::Mm07 => 0x03,
    }
}

/// Decodes the X2 CrazyLight lift-off distance register byte (`0x000A`).
pub fn decode_lod(code: u8) -> Option<LiftOffDistance> {
    match code {
        0x01 => Some(LiftOffDistance::Mm10),
        0x02 => Some(LiftOffDistance::Mm20),
        0x03 => Some(LiftOffDistance::Mm07),
        _ => None,
    }
}

/// Self-inverse 2-bit rotation of a 4-bit nibble used in Pulsar X2 CrazyLight DPI records.
fn rot2_nibble(n: u8) -> u8 {
    ((n << 2) | (n >> 2)) & 0x0F
}

/// Encodes DPI into the 3 data bytes `[x_lo, y_lo, high]` of an X2 CrazyLight DPI stage record.
///
/// Granularity modes from `docs/protocol-x2-crazylight.md`:
/// - `mode 0`: step 10, base 1 (`10..=10240`)
/// - `mode 2`: step 50, base 201 (`10250..=25600`)
/// - `mode 3`: step 100, base 201 (`25700..=32000`)
pub fn encode_dpi_raw(dpi: u16) -> Option<[u8; 3]> {
    if !(10..=32000).contains(&dpi) {
        return None;
    }

    let pick = |val: u32| -> (u8, u32, u32) {
        if val <= 10240 {
            (0, 10, 1)
        } else if val <= 25600 {
            (2, 50, 201)
        } else {
            (3, 100, 201)
        }
    };

    let (mut mode, mut step, mut base) = pick(u32::from(dpi));
    let snapped = ((u32::from(dpi) + step / 2) / step) * step;
    if snapped != u32::from(dpi) {
        (mode, step, base) = pick(snapped);
    }

    let units = snapped / step;
    if units < base {
        return None;
    }
    let index = units - base;
    if index > 1023 {
        return None;
    }

    let page = (index / 256) as u8;
    let low = (index % 256) as u8;
    let nib = rot2_nibble((mode << 2) | page);
    let high = (nib << 4) | nib;
    Some([low, low, high])
}

/// Decodes the 3 data bytes `[x_lo, y_lo, high]` of an X2 CrazyLight DPI stage record.
pub fn decode_dpi_raw(raw: [u8; 3]) -> Option<u16> {
    let high = rot2_nibble(raw[2] & 0x0F);
    let mode = high >> 2;
    let page = u32::from(high & 0x03);
    let (step, base) = match mode {
        0 => (10u32, 1u32),
        2 => (50u32, 201u32),
        3 => (100u32, 201u32),
        _ => return None,
    };
    let dpi = (u32::from(raw[0]) + base + 256 * page) * step;
    u16::try_from(dpi).ok()
}

/// Connection mode for the Pulsar X2 CrazyLight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionKind {
    WirelessDongle,
    WiredUsb,
}

/// Hardware backend for the Pulsar X2 CrazyLight (wireless 8K dongle and wired USB-C).
pub struct PulsarX2CrazyLightDevice {
    hid: HidDevice,
    connection: ConnectionKind,
    vid_pid: String,
    model_name: String,
}

impl PulsarX2CrazyLightDevice {
    fn dev_err(&self, message: impl Into<String>) -> PolledError {
        PolledError::Device {
            device: self.model_name.clone(),
            message: message.into(),
        }
    }

    fn send_command(
        &mut self,
        cmd: u8,
        addr: u16,
        len: u8,
        payload: &[u8],
    ) -> Result<[u8; REPORT_LEN]> {
        let pkt = build_packet(cmd, addr, len, payload);
        self.hid
            .write(&pkt)
            .map_err(|e| self.dev_err(format!("HID write failed for cmd 0x{cmd:02x}: {e}")))?;

        let mut buf = [0u8; 64];
        for _ in 0..MAX_SKIPPED_REPORTS {
            let n = self
                .hid
                .read_timeout(&mut buf, READ_TIMEOUT_MS)
                .map_err(|e| self.dev_err(format!("HID read failed for cmd 0x{cmd:02x}: {e}")))?;
            if n == 0 {
                return Err(self.dev_err(format!(
                    "Timed out waiting for response to cmd 0x{cmd:02x} (is the mouse awake?)"
                )));
            }
            if n < REPORT_LEN || !verify_packet_checksum(&buf[..REPORT_LEN]) {
                continue;
            }
            // Skip unsolicited asynchronous events (DPI button / profile switch).
            if buf[1] == CMD_EVENT {
                continue;
            }
            if buf[1] != cmd {
                continue;
            }
            // Memory reads echo addr_hi, addr_lo, len in bytes 3..6.
            if cmd == CMD_MEM_GET && buf[3..6] != pkt[3..6] {
                continue;
            }
            let mut reply = [0u8; REPORT_LEN];
            reply.copy_from_slice(&buf[..REPORT_LEN]);
            return Ok(reply);
        }

        Err(self.dev_err(format!(
            "No matching reply for cmd 0x{cmd:02x} after {MAX_SKIPPED_REPORTS} packets"
        )))
    }

    fn mem_read(&mut self, addr: u16, len: u8) -> Result<Vec<u8>> {
        let n = len.min(10);
        let reply = self.send_command(CMD_MEM_GET, addr, n, &[])?;
        Ok(reply[6..6 + usize::from(n)].to_vec())
    }

    fn mem_write(&mut self, addr: u16, data: &[u8]) -> Result<()> {
        if data.is_empty() || data.len() > 10 {
            return Err(self.dev_err(format!(
                "Invalid register write length {} at 0x{addr:04x}",
                data.len()
            )));
        }
        let reply = self.send_command(CMD_MEM_SET, addr, data.len() as u8, data)?;
        if reply[2] != 0x00 {
            return Err(self.dev_err(format!(
                "Device rejected write at 0x{addr:04x} (status 0x{:02x})",
                reply[2]
            )));
        }
        Ok(())
    }

    fn write_byte_with_checksum(&mut self, addr: u16, value: u8) -> Result<()> {
        self.mem_write(addr, &[value, checksum(&[value])])
    }

    /// Queries `(cid, mid)` via `CMD_HANDSHAKE (0x01)` to confirm the mouse behind the shared
    /// `3710:5406` CrazyLight dongle is actually an X2 CrazyLight `(0x57, 0x03)`.
    pub fn query_model_ids(&mut self) -> Result<(u8, u8)> {
        // Non-zero 4-byte nonce (byte 5 = 0x08 per docs/protocol-x2-crazylight.md & nordic.py).
        let nonce = [0x50u8, 0x4F, 0x4C, 0x44];
        let reply = self.send_command(CMD_HANDSHAKE, 0, 0x08, &nonce)?;
        if reply[2] != 0x00 || reply[10] == 0 || reply[11] == 0 {
            return Err(self.dev_err(format!(
                "Handshake did not identify model (status=0x{:02x}, cid=0x{:02x}, mid=0x{:02x})",
                reply[2], reply[10], reply[11]
            )));
        }
        Ok((reply[10], reply[11]))
    }

    fn read_stored_polling_rate(&mut self) -> Result<u16> {
        let bytes = self.mem_read(ADDR_POLLING_RATE, 2)?;
        decode_polling_rate(bytes[0]).ok_or_else(|| {
            self.dev_err(format!("Unrecognized polling rate code 0x{:02x}", bytes[0]))
        })
    }

    fn read_active_dpi_stage(&mut self) -> Result<u8> {
        let bytes = self.mem_read(ADDR_ACTIVE_DPI_STAGE, 2)?;
        Ok(bytes[0].min(MAX_DPI_STAGES - 1))
    }
}

impl MouseDevice for PulsarX2CrazyLightDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: match self.connection {
                ConnectionKind::WirelessDongle => "pulsar_x2_crazylight_wireless".to_string(),
                ConnectionKind::WiredUsb => "pulsar_x2_crazylight_wired".to_string(),
            },
            name: self.model_name.clone(),
            vendor: "Pulsar".to_string(),
            vid_pid: Some(self.vid_pid.clone()),
            connection: match self.connection {
                ConnectionKind::WirelessDongle => "Wireless 2.4 GHz (8K Dongle)".to_string(),
                ConnectionKind::WiredUsb => "Wired USB-C (1K max)".to_string(),
            },
            // Flip to TestedOnHardware once confirmed on physical X2 CrazyLight Mini hardware.
            support_tier: SupportTier::ImplementedFromDocs,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let polling_rates_hz = match self.connection {
            ConnectionKind::WirelessDongle => vec![125, 250, 500, 1000, 2000, 4000, 8000],
            ConnectionKind::WiredUsb => vec![125, 250, 500, 1000],
        };

        DeviceCapabilities {
            dpi_min: 10,
            dpi_max: 32000,
            dpi_step: 10,
            polling_rates_hz,
            lod_options: vec![
                LiftOffDistance::Mm07,
                LiftOffDistance::Mm10,
                LiftOffDistance::Mm20,
            ],
            debounce_min_ms: 0,
            debounce_max_ms: 20,
            supports_volatile_writes: false,
            flash_write_warning: Some(
                "Pulsar X2 CrazyLight writes settings to onboard profile storage (no volatile RAM-only write command). Polled diffs every register and rate-limits writes to protect onboard flash."
                    .to_string(),
            ),
        }
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        let stored_hz = self.read_stored_polling_rate()?;
        let effective_hz = match self.connection {
            ConnectionKind::WirelessDongle => stored_hz,
            // Wired mode runs at max 1000 Hz even if the shared profile stores 2K/4K/8K.
            ConnectionKind::WiredUsb => stored_hz.min(1000),
        };

        let active_stage = self.read_active_dpi_stage()?;
        let stage_addr = ADDR_DPI_BASE + u16::from(active_stage) * DPI_STAGE_STRIDE;
        let dpi_bytes = self.mem_read(stage_addr, 4)?;
        let dpi = decode_dpi_raw([dpi_bytes[0], dpi_bytes[1], dpi_bytes[2]]).ok_or_else(|| {
            self.dev_err(format!(
                "Failed to decode DPI stage bytes {:02x?}",
                &dpi_bytes[..3]
            ))
        })?;

        let lod_bytes = self.mem_read(ADDR_LOD_MM, 2)?;
        let lod = decode_lod(lod_bytes[0])
            .ok_or_else(|| self.dev_err(format!("Unrecognized LOD code 0x{:02x}", lod_bytes[0])))?;

        let debounce_bytes = self.mem_read(ADDR_DEBOUNCE, 2)?;
        let debounce_ms = debounce_bytes[0];

        Ok(MouseSettings {
            dpi: Some(dpi),
            polling_rate_hz: Some(effective_hz),
            lod: Some(lod),
            debounce_ms: Some(debounce_ms),
        })
    }

    fn apply_settings(&mut self, settings: &MouseSettings, _mode: WriteMode) -> Result<()> {
        // Read current state first so we only write registers that actually changed.
        let current = self.read_settings()?;

        if let Some(target_hz) = settings.polling_rate_hz {
            if current.polling_rate_hz != Some(target_hz) {
                // Match x2_crazylight_wired.py safety rule: when wired at 1 kHz, don't overwrite
                // a stored 2K/4K/8K dongle polling rate with 1000 Hz.
                let skip_wired_cap = self.connection == ConnectionKind::WiredUsb
                    && target_hz == 1000
                    && self.read_stored_polling_rate()? > 1000;

                if !skip_wired_cap {
                    let code = encode_polling_rate(target_hz).ok_or_else(|| {
                        PolledError::UnsupportedSetting {
                            device: self.model_name.clone(),
                            detail: format!("Unsupported polling rate {target_hz} Hz"),
                        }
                    })?;
                    self.write_byte_with_checksum(ADDR_POLLING_RATE, code)?;
                }
            }
        }

        if let Some(target_dpi) = settings.dpi {
            if current.dpi != Some(target_dpi) {
                let raw =
                    encode_dpi_raw(target_dpi).ok_or_else(|| PolledError::UnsupportedSetting {
                        device: self.model_name.clone(),
                        detail: format!("DPI {target_dpi} cannot be encoded for X2 CrazyLight"),
                    })?;
                let active_stage = self.read_active_dpi_stage()?;
                let stage_addr = ADDR_DPI_BASE + u16::from(active_stage) * DPI_STAGE_STRIDE;
                self.mem_write(stage_addr, &[raw[0], raw[1], raw[2], checksum(&raw)])?;
            }
        }

        if let Some(target_lod) = settings.lod {
            if current.lod != Some(target_lod) {
                self.write_byte_with_checksum(ADDR_LOD_MM, encode_lod(target_lod))?;
            }
        }

        if let Some(target_db) = settings.debounce_ms {
            if current.debounce_ms != Some(target_db) {
                self.write_byte_with_checksum(ADDR_DEBOUNCE, target_db)?;
            }
        }

        Ok(())
    }
}

/// Discovery driver for Pulsar X2 CrazyLight (wireless dongle `3710:5406` & wired `3710:3414`).
pub struct PulsarX2CrazyLightDriver;

impl DeviceDriver for PulsarX2CrazyLightDriver {
    fn driver_name(&self) -> &'static str {
        "Pulsar X2 CrazyLight"
    }

    fn detect(&self, api: &HidApi) -> Result<Vec<Box<dyn MouseDevice>>> {
        let mut found: Vec<Box<dyn MouseDevice>> = Vec::new();

        for dev in api.device_list() {
            if dev.vendor_id() != PULSAR_VID {
                continue;
            }
            let connection = match dev.product_id() {
                X2_CRAZYLIGHT_WIRELESS_PID => ConnectionKind::WirelessDongle,
                X2_CRAZYLIGHT_WIRED_PID => ConnectionKind::WiredUsb,
                _ => continue,
            };

            // On Windows, Interface 1 is split into multiple vendor Top-Level Collections
            // (e.g. 0xff02..0xff06). Inspect the collection's HID Report Descriptor and select
            // ONLY the collection that actually declares Output Report ID 0x08 (17 bytes).
            if dev.interface_number() != CONFIG_INTERFACE_NUM || dev.usage_page() < 0xFF00 {
                continue;
            }

            let Ok(hid) = dev.open_device(api) else {
                continue;
            };

            let mut desc_buf = [0u8; 4096];
            let has_config_report = hid
                .get_report_descriptor(&mut desc_buf)
                .ok()
                .map(|len| parse_hid_report_descriptor(&desc_buf[..len]))
                .is_some_and(|summary| {
                    summary.output_reports.contains_key(&REPORT_ID)
                        && summary.input_reports.contains_key(&REPORT_ID)
                });
            if !has_config_report {
                continue;
            }

            let vid_pid = format!("{:04x}:{:04x}", dev.vendor_id(), dev.product_id());
            let mut candidate = PulsarX2CrazyLightDevice {
                hid,
                connection,
                vid_pid,
                model_name: match connection {
                    ConnectionKind::WirelessDongle => "Pulsar X2 CrazyLight Wireless".to_string(),
                    ConnectionKind::WiredUsb => "Pulsar X2 CrazyLight Wired".to_string(),
                },
            };

            // Query (cid, mid) via CMD_HANDSHAKE (0x01) to distinguish X2 CrazyLight Mini (0x57, 0x01)
            // and X2 CrazyLight (0x57, 0x03) from other CrazyLight models on the shared 3710:5406 dongle.
            if let Ok((cid, mid)) = candidate.query_model_ids() {
                match resolve_crazylight_model_name(cid, mid, connection) {
                    Some(name) => candidate.model_name = name,
                    None => continue,
                }
            }

            found.push(Box::new(candidate));
        }

        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies ALL 14 hardware-captured (bytes -> DPI) pairs from
    /// `packerlschupfer/pulsar-mouse-linux/docs/protocol-x2-crazylight.md`.
    #[test]
    fn encodes_and_decodes_all_14_hardware_captured_dpi_values() {
        let cases: &[([u8; 3], u16)] = &[
            ([0x27, 0x27, 0x00], 400),
            ([0x4F, 0x4F, 0x00], 800),
            ([0x9F, 0x9F, 0x00], 1600),
            ([0xEF, 0xEF, 0x00], 2400),
            ([0x3F, 0x3F, 0x44], 3200),
            ([0x7F, 0x7F, 0x88], 6400),
            ([0x1F, 0x1F, 0xCC], 8000),
            ([0xE7, 0xE7, 0xCC], 10000),
            ([0x37, 0x37, 0x22], 12800),
            ([0x77, 0x77, 0x22], 16000),
            ([0xC7, 0xC7, 0x22], 20000),
            ([0x17, 0x17, 0x66], 24000),
            ([0x37, 0x37, 0x66], 25600),
            ([0x77, 0x77, 0x33], 32000),
        ];

        for &(expected_raw, dpi) in cases {
            assert_eq!(decode_dpi_raw(expected_raw), Some(dpi), "decode {dpi}");
            assert_eq!(encode_dpi_raw(dpi), Some(expected_raw), "encode {dpi}");
        }
    }

    #[test]
    fn verifies_packet_framing_and_checksum() {
        // Write polling rate 4000 Hz (0x20, checksum = 0x55 - 0x20 = 0x35) to 0x0000.
        let pkt = build_packet(
            CMD_MEM_SET,
            ADDR_POLLING_RATE,
            2,
            &[0x20, checksum(&[0x20])],
        );
        assert_eq!(pkt[0], REPORT_ID);
        assert_eq!(pkt[1], CMD_MEM_SET);
        assert_eq!(pkt[3], 0x00);
        assert_eq!(pkt[4], 0x00);
        assert_eq!(pkt[5], 0x02);
        assert_eq!(pkt[6], 0x20);
        assert_eq!(pkt[7], 0x35);
        assert!(verify_packet_checksum(&pkt));

        let mut corrupted = pkt;
        corrupted[6] ^= 0x01;
        assert!(!verify_packet_checksum(&corrupted));
    }

    #[test]
    fn roundtrips_polling_rates_and_lod_codes() {
        for hz in [125, 250, 500, 1000, 2000, 4000, 8000] {
            let code = encode_polling_rate(hz).unwrap();
            assert_eq!(decode_polling_rate(code), Some(hz));
        }

        assert_eq!(encode_lod(LiftOffDistance::Mm10), 0x01);
        assert_eq!(encode_lod(LiftOffDistance::Mm20), 0x02);
        assert_eq!(encode_lod(LiftOffDistance::Mm07), 0x03);
        for lod in LiftOffDistance::ALL {
            assert_eq!(decode_lod(encode_lod(lod)), Some(lod));
        }
    }

    #[test]
    fn resolves_crazylight_model_identities() {
        assert_eq!(
            resolve_crazylight_model_name(0x57, 0x01, ConnectionKind::WirelessDongle).as_deref(),
            Some("Pulsar X2 CrazyLight Mini Wireless")
        );
        assert_eq!(
            resolve_crazylight_model_name(0x57, 0x03, ConnectionKind::WiredUsb).as_deref(),
            Some("Pulsar X2 CrazyLight Wired")
        );
        assert_eq!(
            resolve_crazylight_model_name(0x57, 0x68, ConnectionKind::WirelessDongle),
            None
        );
    }
}
