// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Adapter wrapping [`WootingDevice`] into the generic [`KeyboardDevice`] trait while
//! preserving 100% of Wooting's rich telemetry, keymap, analog stream, and tuning behavior.

use crate::devices::keyboards::{
    KeyboardBrand, KeyboardCapabilities, KeyboardDevice, KeyboardProfileSlot, KeyboardStatus,
};
use crate::devices::{
    SupportTier, WootingAnalogBasePatch, WootingDevice, WootingKeyboardInfo, WootingProtocol,
};
use crate::error::Result;

pub struct WootingKeyboardAdapter {
    inner: WootingDevice,
}

impl WootingKeyboardAdapter {
    pub fn new(inner: WootingDevice) -> Self {
        Self { inner }
    }

    /// Converts a [`WootingKeyboardInfo`] snapshot into the unified [`KeyboardStatus`].
    pub fn status_from_wooting(info: WootingKeyboardInfo) -> KeyboardStatus {
        let is_arm = info.protocol == WootingProtocol::Arm;
        let profiles = info
            .profiles
            .iter()
            .map(|p| KeyboardProfileSlot {
                index: p.index,
                name: p.name.clone(),
                is_active: p.is_active,
            })
            .collect();

        let actuation_mm = info.analog_profile.as_ref().and_then(|a| a.actuation_mm);
        let rapid_trigger_enabled = info
            .analog_profile
            .as_ref()
            .and_then(|a| a.rapid_trigger_enabled);
        let rapid_trigger_mm = info
            .analog_profile
            .as_ref()
            .and_then(|a| a.rapid_trigger_sensitivity_mm);
        let tachyon_enabled = info.analog_profile.as_ref().and_then(|a| a.tachyon_enabled);

        let capabilities = KeyboardCapabilities {
            supports_profile_switch: true,
            profile_switch_volatile: true,
            supports_base_actuation_write: is_arm,
            supports_per_key_actuation_write: is_arm,
            supports_rapid_trigger_toggle: is_arm,
            supports_polling_rate_write: false,
            supported_polling_rates_hz: Vec::new(),
            supports_system_mode_toggle: false,
            supports_socd_toggle: false,
            socd_label: Some("Rappy Snappy / SOCD".to_string()),
            supports_live_analog_stream: true,
            supports_global_settings_write: is_arm,
            supports_debounce_tuning: false,
        };

        KeyboardStatus {
            id: info.id.clone(),
            brand: KeyboardBrand::Wooting,
            name: info.name.clone(),
            vid_pid: info.vid_pid.clone(),
            support_tier: SupportTier::TestedOnHardware,
            protocol_label: info.protocol.label().to_string(),
            chip_type: info.chip_type.clone(),
            switch_tech: info.switch_tech.clone(),
            firmware_version: info.firmware_version.clone(),
            layout: info.layout.clone(),
            active_profile: info.active_profile,
            profiles,
            actuation_mm,
            actuation_range_mm: Some((0.1, 4.0)),
            rapid_trigger_enabled,
            rapid_trigger_mm,
            polling_rate_hz: None,
            battery: None,
            system_mode: None,
            socd_enabled: None,
            tachyon_enabled,
            debounce_mode: None,
            debounce_ms: None,
            extra_telemetry: Vec::new(),
            status_note: None,
            capabilities,
            wooting: Some(info),
        }
    }
}

impl KeyboardDevice for WootingKeyboardAdapter {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn brand(&self) -> KeyboardBrand {
        KeyboardBrand::Wooting
    }

    fn support_tier(&self) -> SupportTier {
        SupportTier::TestedOnHardware
    }

    fn read_status(&self) -> KeyboardStatus {
        Self::status_from_wooting(self.inner.read_full_status())
    }

    fn switch_profile(&self, slot: u8) -> Result<bool> {
        self.inner.switch_profile(slot)
    }

    fn set_base_actuation(
        &self,
        actuation_mm: f32,
        rt_enabled: Option<bool>,
        rt_mm: Option<f32>,
    ) -> Result<()> {
        self.inner.write_analog_defaults(&WootingAnalogBasePatch {
            actuation_mm: Some(actuation_mm),
            rapid_trigger_enabled: rt_enabled,
            rapid_trigger_sensitivity_mm: rt_mm,
        })?;
        Ok(())
    }
}
