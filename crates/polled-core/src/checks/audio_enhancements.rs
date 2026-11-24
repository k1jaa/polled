// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, Severity};
use crate::error::Result;
use crate::win::{AudioFlow, SystemEnv};

/// Report-only check inspecting whether Windows Audio Enhancements (APO / DSP effects)
/// are enabled on the default playback device.
///
/// Intentionally non-mutating: `HKLM\...\MMDevices\Audio\Render\{guid}\FxProperties` is
/// owned by `TrustedInstaller`, and Polled never hijacks Windows system registry ACLs.
pub struct AudioEnhancementsCheck;

impl HealthCheck for AudioEnhancementsCheck {
    fn id(&self) -> &str {
        "audio_enhancements"
    }

    fn name(&self) -> &str {
        "Audio Enhancements (APO / DSP)"
    }

    fn can_fix(&self) -> bool {
        false
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let endpoints = env.list_audio_endpoints(AudioFlow::Render)?;
        let Some(default_ep) = endpoints
            .iter()
            .find(|e| e.is_default)
            .or_else(|| endpoints.first())
        else {
            return Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: "No active playback device detected".to_string(),
                recommended_value: "Audio enhancements Off".to_string(),
                reason: "Connect or enable an audio playback device to audit APO processing."
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

        let (severity, current_value, reason) = match default_ep.enhancements_enabled {
            Some(true) => (
                Severity::Warn,
                format!("{} — Enabled", default_ep.name),
                "Windows Audio Processing Objects (APOs, virtual surround, loudness equalization) add software buffer latency and can smear directional footsteps. Disable manually via Settings -> System -> Sound -> [Device] -> Audio enhancements -> Off (Polled never tampers with TrustedInstaller ACLs)."
                    .to_string(),
            ),
            Some(false) => (
                Severity::Ok,
                format!("{} — Disabled (direct)", default_ep.name),
                "Audio enhancements / APO processing are disabled on the default playback device for minimal software DSP latency."
                    .to_string(),
            ),
            None => (
                Severity::Ok,
                format!("{} — No active APO override detected", default_ep.name),
                "No Windows APO enhancement flag was found on the default playback device."
                    .to_string(),
            ),
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value,
            recommended_value: "Audio enhancements Off".to_string(),
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
    use crate::win::AudioEndpointInfo;

    fn sample_ep(enhancements_enabled: Option<bool>) -> AudioEndpointInfo {
        AudioEndpointInfo {
            id: "{0.0.0.00000000}.{11111111-1111-1111-1111-111111111111}".to_string(),
            guid: "{11111111-1111-1111-1111-111111111111}".to_string(),
            name: "Headphones (USB DAC)".to_string(),
            flow: AudioFlow::Render,
            is_default: true,
            volume_percent: Some(60),
            muted: Some(false),
            sample_rate_hz: Some(48_000),
            channels: Some(2),
            bits_per_sample: Some(24),
            default_period_ms: Some(10.0),
            min_period_ms: Some(2.67),
            enhancements_enabled,
            is_bluetooth_hands_free: false,
        }
    }

    #[test]
    fn warns_when_apo_enhancements_enabled() {
        let env = MockEnv::new();
        env.set_audio_endpoints(vec![sample_ep(Some(true))]);
        let report = AudioEnhancementsCheck.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Warn);
        assert!(!report.can_fix);
        assert!(report.current_value.contains("Enabled"));
    }

    #[test]
    fn reports_ok_when_apo_enhancements_disabled() {
        let env = MockEnv::new();
        env.set_audio_endpoints(vec![sample_ep(Some(false))]);
        let report = AudioEnhancementsCheck.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Ok);
    }
}
