// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::devices::{
    DeviceCapabilities, DeviceInfo, LiftOffDistance, MouseDevice, MouseSettings, SupportTier,
    WriteMode,
};
use crate::error::Result;

/// Logging-only virtual mouse backend for hardware-free testing of profile switching,
/// flash-wear guards, and read-back verification/rollback.
pub struct DryRunDevice {
    name: String,
    supports_volatile_writes: bool,
    current: MouseSettings,
    simulate_next_mismatch: bool,
    write_count: Arc<AtomicU64>,
    log: Arc<Mutex<Vec<String>>>,
}

impl DryRunDevice {
    pub fn new_volatile() -> Self {
        Self::new("Dry-Run Virtual Mouse (Volatile RAM)", true)
    }

    pub fn new_flash_only() -> Self {
        Self::new("Dry-Run Virtual Mouse (Flash-Only)", false)
    }

    fn new(name: &str, supports_volatile_writes: bool) -> Self {
        Self {
            name: name.to_string(),
            supports_volatile_writes,
            current: MouseSettings {
                dpi: Some(800),
                polling_rate_hz: Some(1000),
                lod: Some(LiftOffDistance::Mm10),
                debounce_ms: Some(4),
            },
            simulate_next_mismatch: false,
            write_count: Arc::new(AtomicU64::new(0)),
            log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Forces the next write to silently apply a wrong DPI so `SafeDeviceController` rollback
    /// can be tested deterministically.
    pub fn with_simulated_mismatch(mut self, enabled: bool) -> Self {
        self.simulate_next_mismatch = enabled;
        self
    }

    pub fn write_count_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.write_count)
    }

    pub fn log_handle(&self) -> Arc<Mutex<Vec<String>>> {
        Arc::clone(&self.log)
    }

    fn record_log(&self, entry: String) {
        if let Ok(mut log) = self.log.lock() {
            log.push(entry);
        }
    }
}

impl MouseDevice for DryRunDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: if self.supports_volatile_writes {
                "dry_run_volatile".to_string()
            } else {
                "dry_run_flash".to_string()
            },
            name: self.name.clone(),
            vendor: "Polled".to_string(),
            vid_pid: None,
            connection: "Simulated".to_string(),
            support_tier: SupportTier::Simulator,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            dpi_min: 50,
            dpi_max: 32000,
            dpi_step: 50,
            polling_rates_hz: vec![125, 250, 500, 1000, 2000, 4000, 8000],
            lod_options: LiftOffDistance::ALL.to_vec(),
            supports_debounce: true,
            debounce_min_ms: 0,
            debounce_max_ms: 20,
            supports_volatile_writes: self.supports_volatile_writes,
            flash_write_warning: if self.supports_volatile_writes {
                None
            } else {
                Some(
                    "Simulated flash-only device: writes are diffed and rate-limited to prevent onboard flash wear."
                        .to_string(),
                )
            },
        }
    }

    fn read_settings(&mut self) -> Result<MouseSettings> {
        self.record_log(format!(
            "[dry-run] READ -> {}",
            self.current.summary_string()
        ));
        Ok(self.current.clone())
    }

    fn apply_settings(&mut self, settings: &MouseSettings, mode: WriteMode) -> Result<()> {
        self.write_count.fetch_add(1, Ordering::SeqCst);
        let mut next = self.current.merged_with(settings);

        if self.simulate_next_mismatch {
            // Corrupt only the first write, then allow the subsequent rollback write to succeed.
            self.simulate_next_mismatch = false;
            next.dpi = Some(next.dpi.unwrap_or(800).saturating_add(50));
            self.record_log(format!(
                "[dry-run] WRITE ({mode:?}) [simulated mismatch] -> {}",
                next.summary_string()
            ));
        } else {
            self.record_log(format!(
                "[dry-run] WRITE ({mode:?}) -> {}",
                next.summary_string()
            ));
        }

        self.current = next;
        Ok(())
    }
}
