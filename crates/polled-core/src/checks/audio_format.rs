// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, Severity};
use crate::error::Result;
use crate::win::{AudioEndpointInfo, AudioFlow, SystemEnv};

/// Report-only check inspecting default playback & capture WASAPI mix format,
/// engine period latency, Bluetooth Hands-Free (HFP/SCO) degradation, and sample-rate mismatches.
pub struct AudioFormatCheck;

impl HealthCheck for AudioFormatCheck {
    fn id(&self) -> &str {
        "audio_format"
    }

    fn name(&self) -> &str {
        "Audio Format & WASAPI Latency"
    }

    fn can_fix(&self) -> bool {
        false
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let render_eps = env.list_audio_endpoints(AudioFlow::Render)?;
        let capture_eps = env.list_audio_endpoints(AudioFlow::Capture)?;

        let Some(render) = render_eps
            .iter()
            .find(|e| e.is_default)
            .or_else(|| render_eps.first())
        else {
            return Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: "No active playback device detected".to_string(),
                recommended_value: "48 kHz · 24-bit · <= 10 ms period (wired / 2.4 GHz)"
                    .to_string(),
                reason: "Connect or enable an audio playback device to inspect its WASAPI format."
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

        let capture = capture_eps
            .iter()
            .find(|e| e.is_default)
            .or_else(|| capture_eps.first());

        let summary = format_endpoint_summary(render);

        if render.is_bluetooth_hands_free {
            return Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: format!("{summary} [Bluetooth Hands-Free HFP]"),
                recommended_value: "48 kHz Stereo (wired, USB DAC, or 2.4 GHz dongle)".to_string(),
                reason: "Your default playback device is running in Bluetooth Hands-Free (HFP/SCO) telephony mode (<= 16 kHz mono, 100-250 ms audio latency). Switch output to Stereo / wired / 2.4 GHz or disable the headset Hands-Free mic."
                    .to_string(),
                severity: Severity::Bad,
                can_fix: false,
                is_optional_tweak: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,
                requires_reboot: false,
            });
        }

        if render.sample_rate_hz.is_some_and(|r| r >= 192_000) {
            return Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: summary,
                recommended_value: "48 kHz (24-bit) shared mode".to_string(),
                reason: "Running Windows shared-mode audio at 192 kHz+ forces constant CPU resampling of 48 kHz game audio with extra mixer overhead and zero competitive benefit. Set Format to 24 bit, 48000 Hz in Windows Sound settings."
                    .to_string(),
                severity: Severity::Warn,
                can_fix: false,
                is_optional_tweak: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,
                requires_reboot: false,
            });
        }

        if render.default_period_ms.is_some_and(|p| p >= 15.0) {
            return Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: summary,
                recommended_value: "48 kHz · <= 10.0 ms default WASAPI period".to_string(),
                reason: "Your default playback driver reports a high WASAPI scheduling period (>= 15 ms), increasing audio latency. Update your audio/USB DAC driver or switch away from high-buffer virtual/Bluetooth sinks."
                    .to_string(),
                severity: Severity::Warn,
                can_fix: false,
                is_optional_tweak: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,
                requires_reboot: false,
            });
        }

        if let Some(cap) = capture {
            if cap.is_bluetooth_hands_free {
                return Ok(CheckReport {
                    id: self.id().to_string(),
                    name: self.name().to_string(),
                    current_value: format!(
                        "{summary} | Mic: {} [Bluetooth Hands-Free HFP]",
                        cap.name
                    ),
                    recommended_value: "Dedicated USB/XLR or 2.4 GHz microphone (48 kHz)"
                        .to_string(),
                    reason: "Your default microphone is a Bluetooth Hands-Free AG Audio endpoint. Joining Discord or in-game voice can force Bluetooth headphones into low-quality HFP telephony mode."
                        .to_string(),
                    severity: Severity::Warn,
                    can_fix: false,
                    is_optional_tweak: false,
                    requires_elevation: false,
                    elevation_reason: None,
                    requires_relogin: false,
                    requires_reboot: false,
                });
            }

            if let (Some(out_hz), Some(in_hz)) = (render.sample_rate_hz, cap.sample_rate_hz) {
                if out_hz != in_hz {
                    return Ok(CheckReport {
                        id: self.id().to_string(),
                        name: self.name().to_string(),
                        current_value: format!(
                            "{summary} | Mic: {} ({})",
                            cap.name,
                            format_khz(in_hz)
                        ),
                        recommended_value: "Match playback & mic sample rates (48 kHz)".to_string(),
                        reason: "Playback and microphone endpoints are running at different sample rates. Setting both to 48000 Hz eliminates unnecessary shared-mode resampling in games, Discord, and OBS."
                            .to_string(),
                        severity: Severity::Info,
                        can_fix: false,
                        is_optional_tweak: false,
                        requires_elevation: false,
                        elevation_reason: None,
                        requires_relogin: false,
                        requires_reboot: false,
                    });
                }
            }
        }

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value: summary,
            recommended_value: "48 kHz · <= 10.0 ms WASAPI period".to_string(),
            reason: "Default playback format and WASAPI engine period are well-suited for low-latency gaming audio."
                .to_string(),
            severity: Severity::Ok,
            can_fix: false,
            is_optional_tweak: false,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,
            requires_reboot: false,
        })
    }
}

fn format_khz(hz: u32) -> String {
    if hz.is_multiple_of(1000) {
        format!("{} kHz", hz / 1000)
    } else {
        format!("{:.1} kHz", hz as f64 / 1000.0)
    }
}

fn format_endpoint_summary(ep: &AudioEndpointInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(hz) = ep.sample_rate_hz {
        parts.push(format_khz(hz));
    }
    if let Some(ch) = ep.channels {
        parts.push(format!("{ch}ch"));
    }
    if let Some(bits) = ep.bits_per_sample {
        parts.push(format!("{bits}-bit"));
    }
    match (ep.default_period_ms, ep.min_period_ms) {
        (Some(def), Some(min)) => parts.push(format!("{def:.1} ms (min {min:.1} ms)")),
        (Some(def), None) => parts.push(format!("{def:.1} ms")),
        _ => {}
    }

    if parts.is_empty() {
        ep.name.clone()
    } else {
        format!("{}: {}", ep.name, parts.join(" · "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    fn mk_ep(
        name: &str,
        flow: AudioFlow,
        sample_rate_hz: u32,
        default_period_ms: f64,
        is_hfp: bool,
    ) -> AudioEndpointInfo {
        AudioEndpointInfo {
            id: format!("id-{name}"),
            guid: "{11111111-1111-1111-1111-111111111111}".to_string(),
            name: name.to_string(),
            flow,
            is_default: true,
            volume_percent: Some(70),
            muted: Some(false),
            sample_rate_hz: Some(sample_rate_hz),
            channels: Some(if is_hfp { 1 } else { 2 }),
            bits_per_sample: Some(24),
            default_period_ms: Some(default_period_ms),
            min_period_ms: Some(2.67),
            enhancements_enabled: Some(false),
            is_bluetooth_hands_free: is_hfp,
        }
    }

    #[test]
    fn flags_bluetooth_hfp_render_as_bad() {
        let env = MockEnv::new();
        env.set_audio_endpoints(vec![mk_ep(
            "Headset (AirPods Hands-Free AG Audio)",
            AudioFlow::Render,
            16_000,
            10.0,
            true,
        )]);
        let report = AudioFormatCheck.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Bad);
    }

    #[test]
    fn warns_on_192khz_and_high_period() {
        let env = MockEnv::new();
        env.set_audio_endpoints(vec![mk_ep(
            "USB DAC",
            AudioFlow::Render,
            192_000,
            10.0,
            false,
        )]);
        assert_eq!(
            AudioFormatCheck.scan(&env).unwrap().severity,
            Severity::Warn
        );

        env.set_audio_endpoints(vec![mk_ep(
            "Virtual Sink",
            AudioFlow::Render,
            48_000,
            20.0,
            false,
        )]);
        assert_eq!(
            AudioFormatCheck.scan(&env).unwrap().severity,
            Severity::Warn
        );
    }

    #[test]
    fn reports_info_on_sample_rate_mismatch_and_ok_when_matched() {
        let env = MockEnv::new();
        env.set_audio_endpoints(vec![
            mk_ep("DAC", AudioFlow::Render, 48_000, 10.0, false),
            mk_ep("Mic", AudioFlow::Capture, 44_100, 10.0, false),
        ]);
        assert_eq!(
            AudioFormatCheck.scan(&env).unwrap().severity,
            Severity::Info
        );

        env.set_audio_endpoints(vec![
            mk_ep("DAC", AudioFlow::Render, 48_000, 10.0, false),
            mk_ep("Mic", AudioFlow::Capture, 48_000, 10.0, false),
        ]);
        assert_eq!(AudioFormatCheck.scan(&env).unwrap().severity, Severity::Ok);
    }
}
