// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::devices::{DeviceCapabilities, DeviceInfo, MouseDevice, MouseSettings, WriteMode};
use crate::error::{PolledError, Result};

/// Default minimum spacing between onboard flash writes on mice that lack volatile RAM writes.
pub const DEFAULT_FLASH_COOLDOWN: Duration = Duration::from_secs(15);

/// Result of attempting to apply a profile's settings through `SafeDeviceController`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApplyOutcome {
    /// Profile specified no mouse settings to change.
    SkippedNoSettings,
    /// Hardware already matched the desired settings (zero writes performed).
    SkippedUnchanged { current: MouseSettings },
    /// Settings were written and verified via read-back.
    Applied {
        mode: WriteMode,
        verified: MouseSettings,
        flash_writes_this_session: u64,
    },
}

/// Safety wrapper around any `MouseDevice` enforcing four hardware invariants:
///
/// 1. **Validation**: Rejects unsupported DPI/polling/LOD/debounce before touching HID.
/// 2. **Diff-before-write**: Reads current settings and skips writing if already identical.
/// 3. **Flash-wear protection**: Uses `VolatileRam` whenever supported; on flash-only hardware,
///    enforces a cooldown rate-limiter and counts session flash writes.
/// 4. **Read-back verification + rollback**: Reads settings back immediately after writing; if
///    they do not match the target, automatically rolls back to the pre-write settings and errors.
pub struct SafeDeviceController {
    device: Box<dyn MouseDevice>,
    flash_cooldown: Duration,
    last_flash_write: Option<Instant>,
    flash_writes_this_session: u64,
}

impl SafeDeviceController {
    pub fn new(device: Box<dyn MouseDevice>) -> Self {
        Self {
            device,
            flash_cooldown: DEFAULT_FLASH_COOLDOWN,
            last_flash_write: None,
            flash_writes_this_session: 0,
        }
    }

    pub fn with_flash_cooldown(mut self, cooldown: Duration) -> Self {
        self.flash_cooldown = cooldown;
        self
    }

    pub fn info(&self) -> DeviceInfo {
        self.device.info()
    }

    pub fn capabilities(&self) -> DeviceCapabilities {
        self.device.capabilities()
    }

    pub fn flash_writes_this_session(&self) -> u64 {
        self.flash_writes_this_session
    }

    pub fn read_settings(&mut self) -> Result<MouseSettings> {
        self.device.read_settings()
    }

    pub fn apply_profile_settings(&mut self, desired: &MouseSettings) -> Result<ApplyOutcome> {
        self.apply_profile_settings_at(desired, Instant::now())
    }

    /// Deterministic clock version of `apply_profile_settings` for unit tests.
    pub fn apply_profile_settings_at(
        &mut self,
        desired: &MouseSettings,
        now: Instant,
    ) -> Result<ApplyOutcome> {
        if desired.is_empty() {
            return Ok(ApplyOutcome::SkippedNoSettings);
        }

        let info = self.device.info();
        let caps = self.device.capabilities();

        // 1. Validate bounds against device capabilities.
        caps.validate(&info.name, desired)?;

        // 2. Read current state and skip if nothing actually changed.
        let before = self.device.read_settings()?;
        if before.matches_desired(desired) {
            return Ok(ApplyOutcome::SkippedUnchanged { current: before });
        }

        // 3. Pick write mode and enforce flash rate-limiting when volatile writes are unavailable.
        let mode = if caps.supports_volatile_writes {
            WriteMode::VolatileRam
        } else {
            if let Some(last) = self.last_flash_write {
                let elapsed = now.saturating_duration_since(last);
                if elapsed < self.flash_cooldown {
                    let wait_secs = (self.flash_cooldown - elapsed).as_secs().max(1);
                    return Err(PolledError::FlashRateLimited {
                        device: info.name,
                        wait_secs,
                    });
                }
            }
            WriteMode::OnboardFlash
        };

        // 4. Write desired settings.
        self.device.apply_settings(desired, mode)?;
        if mode == WriteMode::OnboardFlash {
            self.last_flash_write = Some(now);
            self.flash_writes_this_session += 1;
        }

        // 5. Mandatory read-back verification.
        let after = self.device.read_settings()?;
        if !after.matches_desired(desired) {
            let rollback_status = match self.device.apply_settings(&before, mode) {
                Ok(()) => "succeeded".to_string(),
                Err(err) => format!("failed ({err})"),
            };
            return Err(PolledError::VerificationFailed {
                device: info.name,
                expected: desired.summary_string(),
                actual: after.summary_string(),
                rollback_status,
            });
        }

        Ok(ApplyOutcome::Applied {
            mode,
            verified: after,
            flash_writes_this_session: self.flash_writes_this_session,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::{DryRunDevice, LiftOffDistance};

    #[test]
    fn skips_write_when_settings_already_match() {
        let dry = DryRunDevice::new_volatile();
        let write_counter = dry.write_count_handle();
        let mut ctrl = SafeDeviceController::new(Box::new(dry));

        let initial = ctrl.read_settings().unwrap();
        let outcome = ctrl.apply_profile_settings(&initial).unwrap();
        assert!(matches!(outcome, ApplyOutcome::SkippedUnchanged { .. }));
        assert_eq!(write_counter.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn uses_volatile_mode_when_supported_without_flash_counter() {
        let dry = DryRunDevice::new_volatile();
        let mut ctrl = SafeDeviceController::new(Box::new(dry));

        let desired = MouseSettings {
            dpi: Some(1600),
            polling_rate_hz: Some(4000),
            lod: Some(LiftOffDistance::Mm07),
            debounce_ms: Some(2),
        };
        let outcome = ctrl.apply_profile_settings(&desired).unwrap();
        assert_eq!(
            outcome,
            ApplyOutcome::Applied {
                mode: WriteMode::VolatileRam,
                verified: desired,
                flash_writes_this_session: 0,
            }
        );
    }

    #[test]
    fn rate_limits_flash_only_devices_and_allows_after_cooldown() {
        let dry = DryRunDevice::new_flash_only();
        let mut ctrl =
            SafeDeviceController::new(Box::new(dry)).with_flash_cooldown(Duration::from_secs(10));
        let t0 = Instant::now();

        let p1 = MouseSettings {
            dpi: Some(1600),
            ..Default::default()
        };
        let p2 = MouseSettings {
            dpi: Some(3200),
            ..Default::default()
        };

        // First flash write succeeds.
        let r1 = ctrl.apply_profile_settings_at(&p1, t0).unwrap();
        assert!(matches!(
            r1,
            ApplyOutcome::Applied {
                mode: WriteMode::OnboardFlash,
                flash_writes_this_session: 1,
                ..
            }
        ));

        // Second flash write 3s later is blocked.
        let err = ctrl
            .apply_profile_settings_at(&p2, t0 + Duration::from_secs(3))
            .unwrap_err();
        assert!(matches!(err, PolledError::FlashRateLimited { .. }));
        assert_eq!(ctrl.read_settings().unwrap().dpi, Some(1600));

        // After 10s cooldown, write succeeds.
        let r2 = ctrl
            .apply_profile_settings_at(&p2, t0 + Duration::from_secs(11))
            .unwrap();
        assert!(matches!(
            r2,
            ApplyOutcome::Applied {
                mode: WriteMode::OnboardFlash,
                flash_writes_this_session: 2,
                ..
            }
        ));
    }

    #[test]
    fn rolls_back_automatically_on_readback_mismatch() {
        let dry = DryRunDevice::new_volatile().with_simulated_mismatch(true);
        let mut ctrl = SafeDeviceController::new(Box::new(dry));
        let original = ctrl.read_settings().unwrap();

        let desired = MouseSettings {
            dpi: Some(3200),
            ..Default::default()
        };
        let err = ctrl.apply_profile_settings(&desired).unwrap_err();
        assert!(matches!(err, PolledError::VerificationFailed { .. }));

        // Hardware must have been rolled back to its original pre-write settings.
        assert_eq!(ctrl.read_settings().unwrap(), original);
    }

    #[test]
    fn rejects_out_of_range_settings_before_writing() {
        let dry = DryRunDevice::new_volatile();
        let write_counter = dry.write_count_handle();
        let mut ctrl = SafeDeviceController::new(Box::new(dry));

        let bad = MouseSettings {
            polling_rate_hz: Some(9999),
            ..Default::default()
        };
        let err = ctrl.apply_profile_settings(&bad).unwrap_err();
        assert!(matches!(err, PolledError::UnsupportedSetting { .. }));
        assert_eq!(write_counter.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
