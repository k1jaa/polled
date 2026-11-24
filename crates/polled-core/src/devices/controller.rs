// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::devices::{
    BatteryStatus, DeviceCapabilities, DeviceInfo, MouseDevice, MouseSettings, WriteMode,
};
use crate::error::{PolledError, Result};

/// Default sliding window for flash-wear protection on mice without volatile RAM writes.
pub const DEFAULT_FLASH_WINDOW: Duration = Duration::from_secs(60);

/// Default maximum onboard flash writes allowed inside [`DEFAULT_FLASH_WINDOW`].
///
/// Allowing a small burst (4 writes / 60 s) lets normal Game <-> Desktop switches apply right
/// after focus debounce (~1.5 s) with zero artificial wait, while still blocking runaway loops
/// or rapid alt-tab hammering from wearing out onboard flash.
pub const DEFAULT_MAX_FLASH_WRITES_PER_WINDOW: usize = 4;

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
///    enforces a sliding-window write budget and counts session flash writes.
/// 4. **Read-back verification + rollback**: Reads settings back immediately after writing; if
///    they do not match the target, automatically rolls back to the pre-write settings and errors.
pub struct SafeDeviceController {
    device: Box<dyn MouseDevice>,
    flash_window: Duration,
    max_flash_writes_per_window: usize,
    recent_flash_writes: VecDeque<Instant>,
    flash_writes_this_session: u64,
}

impl SafeDeviceController {
    pub fn new(device: Box<dyn MouseDevice>) -> Self {
        Self {
            device,
            flash_window: DEFAULT_FLASH_WINDOW,
            max_flash_writes_per_window: DEFAULT_MAX_FLASH_WRITES_PER_WINDOW,
            recent_flash_writes: VecDeque::new(),
            flash_writes_this_session: 0,
        }
    }

    pub fn with_flash_window(mut self, window: Duration, max_writes: usize) -> Self {
        self.flash_window = window;
        self.max_flash_writes_per_window = max_writes.max(1);
        self
    }

    pub fn with_flash_cooldown(self, cooldown: Duration) -> Self {
        self.with_flash_window(cooldown, 1)
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

    pub fn read_battery(&mut self) -> Option<BatteryStatus> {
        self.device.read_battery()
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

        // 3. Pick write mode and enforce sliding-window flash protection when volatile writes are unavailable.
        let mode = if caps.supports_volatile_writes {
            WriteMode::VolatileRam
        } else {
            while let Some(&oldest) = self.recent_flash_writes.front() {
                if now.saturating_duration_since(oldest) >= self.flash_window {
                    self.recent_flash_writes.pop_front();
                } else {
                    break;
                }
            }
            if self.recent_flash_writes.len() >= self.max_flash_writes_per_window {
                let oldest = *self
                    .recent_flash_writes
                    .front()
                    .expect("non-empty flash window");
                let remaining = self
                    .flash_window
                    .saturating_sub(now.saturating_duration_since(oldest));
                let wait_secs =
                    (remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0)).max(1);
                return Err(PolledError::FlashRateLimited {
                    device: info.name,
                    wait_secs,
                });
            }
            WriteMode::OnboardFlash
        };

        // 4. Write desired settings.
        self.device.apply_settings(desired, mode)?;
        if mode == WriteMode::OnboardFlash {
            self.recent_flash_writes.push_back(now);
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
            motion_sync: Some(true),
            angle_snapping: Some(false),
            ripple_control: Some(false),
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
    fn allows_small_flash_burst_then_throttles_until_window_expires() {
        let dry = DryRunDevice::new_flash_only();
        let mut ctrl = SafeDeviceController::new(Box::new(dry));
        let t0 = Instant::now();

        let game = MouseSettings {
            dpi: Some(400),
            ..Default::default()
        };
        let desktop = MouseSettings {
            dpi: Some(1600),
            ..Default::default()
        };

        // 4 rapid Game <-> Desktop writes within 12s all succeed without artificial delay.
        for i in 0..DEFAULT_MAX_FLASH_WRITES_PER_WINDOW {
            let target = if i % 2 == 0 { &game } else { &desktop };
            let res = ctrl
                .apply_profile_settings_at(target, t0 + Duration::from_secs((i * 3) as u64))
                .unwrap();
            assert!(matches!(res, ApplyOutcome::Applied { .. }));
        }

        // 5th write inside the 60s window is blocked by flash-wear protection.
        let err = ctrl
            .apply_profile_settings_at(&game, t0 + Duration::from_secs(20))
            .unwrap_err();
        assert!(matches!(
            err,
            PolledError::FlashRateLimited { wait_secs: 40, .. }
        ));

        // Once the oldest write ages out past 60s, the next write succeeds.
        let res = ctrl
            .apply_profile_settings_at(&game, t0 + Duration::from_secs(61))
            .unwrap();
        assert!(matches!(
            res,
            ApplyOutcome::Applied {
                flash_writes_this_session: 5,
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
