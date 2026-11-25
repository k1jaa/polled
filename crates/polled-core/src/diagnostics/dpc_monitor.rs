// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pure user-mode per-core Windows Interrupt & DPC Latency Monitor (Mini LatencyMon).
//!
//! Uses documented Windows user-mode `ntdll.dll` `NtQuerySystemInformation`:
//! - `SystemProcessorPerformanceInformation (8)` for per-logical-core idle, kernel, user, DPC,
//!   and hardware interrupt times plus interrupt counts
//! - `SystemInterruptInformation (23)` for per-logical-core DPC counts/rates
//!
//! Zero kernel drivers, zero admin privileges, zero game hooks, 0% CPU when stopped.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::checks::Severity;

const DPC_POLL_INTERVAL_MS: u64 = 500;

/// Raw cumulative per-logical-core counter sample from Windows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RawCorePerfSample {
    pub idle_time: i64,
    pub kernel_time: i64,
    pub user_time: i64,
    pub dpc_time: i64,
    pub interrupt_time: i64,
    pub interrupt_count: u32,
    pub dpc_count: Option<u32>,
}

/// Computed per-logical-core CPU, Kernel, DPC, and Hardware Interrupt percentages.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CoreDpcMetrics {
    pub core_index: usize,
    pub cpu_pct: f64,
    pub kernel_pct: f64,
    pub dpc_pct: f64,
    pub interrupt_pct: f64,
    pub dpc_irq_pct: f64,
    pub peak_dpc_irq_pct: f64,
    pub interrupts_per_sec: f64,
    pub dpcs_per_sec: Option<f64>,
}

/// Live immutable snapshot rendered in Bench -> System & USB.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DpcMonitorSnapshot {
    pub running: bool,
    pub sample_count: u64,
    pub core_count: usize,
    pub cores: Vec<CoreDpcMetrics>,
    pub hottest_core: Option<usize>,
    pub hottest_dpc_irq_pct: f64,
    pub peak_dpc_irq_pct: f64,
    pub core0_dpc_irq_pct: f64,
    pub other_cores_avg_dpc_irq_pct: f64,
    pub core0_starved: bool,
    pub severity: Severity,
    pub diagnosis: String,
    pub error: Option<String>,
}

impl Default for DpcMonitorSnapshot {
    fn default() -> Self {
        Self {
            running: false,
            sample_count: 0,
            core_count: 0,
            cores: Vec::new(),
            hottest_core: None,
            hottest_dpc_irq_pct: 0.0,
            peak_dpc_irq_pct: 0.0,
            core0_dpc_irq_pct: 0.0,
            other_cores_avg_dpc_irq_pct: 0.0,
            core0_starved: false,
            severity: Severity::Info,
            diagnosis: String::new(),
            error: None,
        }
    }
}

/// Computes per-core DPC/IRQ percentages and Core 0 starvation diagnosis between two snapshots.
pub fn compute_dpc_snapshot(
    prev: &[RawCorePerfSample],
    curr: &[RawCorePerfSample],
    elapsed_secs: f64,
    prior_peaks: &mut Vec<f64>,
    sample_count: u64,
    running: bool,
) -> DpcMonitorSnapshot {
    let n = prev.len().min(curr.len());
    if prior_peaks.len() < n {
        prior_peaks.resize(n, 0.0);
    }
    let dt = elapsed_secs.max(0.05);
    let mut cores = Vec::with_capacity(n);

    for i in 0..n {
        let p = prev[i];
        let c = curr[i];

        let d_idle = (c.idle_time.saturating_sub(p.idle_time)).max(0) as f64;
        // On Windows, KernelTime includes IdleTime + DpcTime + InterruptTime.
        let d_kernel_incl_idle = (c.kernel_time.saturating_sub(p.kernel_time)).max(0) as f64;
        let d_kernel_active = (d_kernel_incl_idle - d_idle).max(0.0);
        let d_user = (c.user_time.saturating_sub(p.user_time)).max(0) as f64;
        let d_total = (d_kernel_incl_idle + d_user).max(1.0);

        let d_dpc = (c.dpc_time.saturating_sub(p.dpc_time)).max(0) as f64;
        let d_irq = (c.interrupt_time.saturating_sub(p.interrupt_time)).max(0) as f64;

        let cpu_pct = (((d_kernel_active + d_user) / d_total) * 100.0).clamp(0.0, 100.0);
        let kernel_pct = ((d_kernel_active / d_total) * 100.0).clamp(0.0, 100.0);
        let dpc_pct = ((d_dpc / d_total) * 100.0).clamp(0.0, 100.0);
        let interrupt_pct = ((d_irq / d_total) * 100.0).clamp(0.0, 100.0);
        let dpc_irq_pct = (dpc_pct + interrupt_pct).min(100.0);

        if dpc_irq_pct > prior_peaks[i] {
            prior_peaks[i] = dpc_irq_pct;
        }

        let d_irq_cnt = c.interrupt_count.wrapping_sub(p.interrupt_count) as f64;
        let interrupts_per_sec = d_irq_cnt / dt;
        let dpcs_per_sec = match (p.dpc_count, c.dpc_count) {
            (Some(pc), Some(cc)) => Some((cc.wrapping_sub(pc) as f64) / dt),
            _ => None,
        };

        cores.push(CoreDpcMetrics {
            core_index: i,
            cpu_pct,
            kernel_pct,
            dpc_pct,
            interrupt_pct,
            dpc_irq_pct,
            peak_dpc_irq_pct: prior_peaks[i],
            interrupts_per_sec,
            dpcs_per_sec,
        });
    }

    let hottest = cores
        .iter()
        .max_by(|a, b| a.dpc_irq_pct.total_cmp(&b.dpc_irq_pct));
    let hottest_core = hottest.map(|c| c.core_index);
    let hottest_dpc_irq_pct = hottest.map_or(0.0, |c| c.dpc_irq_pct);
    let peak_dpc_irq_pct = prior_peaks.iter().copied().fold(0.0_f64, f64::max);

    let core0_dpc_irq_pct = cores.first().map_or(0.0, |c| c.dpc_irq_pct);
    let other_cores_avg_dpc_irq_pct = if cores.len() > 1 {
        let sum: f64 = cores[1..].iter().map(|c| c.dpc_irq_pct).sum();
        sum / ((cores.len() - 1) as f64)
    } else {
        0.0
    };

    let core0_starved = cores.len() > 1
        && ((core0_dpc_irq_pct >= 10.0 && other_cores_avg_dpc_irq_pct <= 2.5)
            || (core0_dpc_irq_pct >= 5.0
                && core0_dpc_irq_pct >= other_cores_avg_dpc_irq_pct * 3.5));

    let (severity, diagnosis) = if n == 0 {
        (
            Severity::Info,
            "Click Start Monitor to inspect live per-core Interrupt & DPC load.".to_string(),
        )
    } else if hottest_dpc_irq_pct >= 20.0 || (core0_starved && core0_dpc_irq_pct >= 12.0) {
        (
            Severity::Bad,
            format!(
                "Heavy Interrupt/DPC spike on Core {} ({hottest_dpc_irq_pct:.1}% DPC+IRQ; peak {peak_dpc_irq_pct:.1}%). Check PCI MSI mode (Health Check), move 4K/8K dongles off shared USB hubs, or audit GPU/NIC/audio drivers.",
                hottest_core.unwrap_or(0)
            ),
        )
    } else if core0_starved {
        (
            Severity::Warn,
            format!(
                "Core 0 IRQ/DPC imbalance: Core 0 at {core0_dpc_irq_pct:.1}% DPC+IRQ vs {other_cores_avg_dpc_irq_pct:.1}% avg across other cores. Ensure PCI GPU/Audio MSI mode is enabled and avoid sharing Core 0 with heavy USB audio/capture gear."
            ),
        )
    } else if hottest_dpc_irq_pct >= 8.0 || peak_dpc_irq_pct >= 15.0 {
        (
            Severity::Warn,
            format!(
                "Elevated DPC/IRQ load on Core {} ({hottest_dpc_irq_pct:.1}% now · {peak_dpc_irq_pct:.1}% session peak). Monitor while moving your 4K/8K mouse or during game audio playback.",
                hottest_core.unwrap_or(0)
            ),
        )
    } else {
        (
            Severity::Ok,
            format!(
                "Clean per-core Interrupt/DPC balance across {n} logical cores (hottest Core {}: {hottest_dpc_irq_pct:.1}% · session peak {peak_dpc_irq_pct:.1}%).",
                hottest_core.unwrap_or(0)
            ),
        )
    };

    DpcMonitorSnapshot {
        running,
        sample_count,
        core_count: n,
        cores,
        hottest_core,
        hottest_dpc_irq_pct,
        peak_dpc_irq_pct,
        core0_dpc_irq_pct,
        other_cores_avg_dpc_irq_pct,
        core0_starved,
        severity,
        diagnosis,
        error: None,
    }
}

/// Live background per-core Interrupt & DPC latency monitor (0% CPU when stopped).
pub struct DpcMonitorSession {
    snapshot: Arc<Mutex<DpcMonitorSnapshot>>,
    running: Arc<AtomicBool>,
    stop_signal: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}

impl DpcMonitorSession {
    pub fn start<F>(on_update: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        let snapshot = Arc::new(Mutex::new(DpcMonitorSnapshot {
            running: true,
            severity: Severity::Info,
            diagnosis: "Sampling baseline per-core Interrupt & DPC counters...".to_string(),
            ..Default::default()
        }));
        let running = Arc::new(AtomicBool::new(true));
        let stop_signal = Arc::new((Mutex::new(false), Condvar::new()));

        let snap_bg = Arc::clone(&snapshot);
        let run_bg = Arc::clone(&running);
        let sig_bg = Arc::clone(&stop_signal);

        let handle = std::thread::Builder::new()
            .name("polled-dpc-monitor".to_string())
            .spawn(move || {
                let mut prev = match query_raw_core_perf() {
                    Ok(v) if !v.is_empty() => v,
                    Ok(_) => {
                        if let Ok(mut g) = snap_bg.lock() {
                            g.running = false;
                            g.error = Some("Windows returned 0 logical CPU entries.".to_string());
                        }
                        run_bg.store(false, Ordering::SeqCst);
                        (on_update)();
                        return;
                    }
                    Err(err) => {
                        if let Ok(mut g) = snap_bg.lock() {
                            g.running = false;
                            g.error = Some(err);
                        }
                        run_bg.store(false, Ordering::SeqCst);
                        (on_update)();
                        return;
                    }
                };
                let mut prev_time = Instant::now();
                let mut peaks: Vec<f64> = vec![0.0; prev.len()];
                let mut sample_count = 0u64;

                while run_bg.load(Ordering::SeqCst) {
                    {
                        let (lock, cvar) = &*sig_bg;
                        if let Ok(guard) = lock.lock() {
                            if !*guard {
                                let _ = cvar.wait_timeout(
                                    guard,
                                    Duration::from_millis(DPC_POLL_INTERVAL_MS),
                                );
                            }
                        }
                    }
                    if !run_bg.load(Ordering::SeqCst) {
                        break;
                    }
                    let now = Instant::now();
                    let elapsed = now.saturating_duration_since(prev_time).as_secs_f64();
                    match query_raw_core_perf() {
                        Ok(curr) if !curr.is_empty() => {
                            sample_count += 1;
                            let next = compute_dpc_snapshot(
                                &prev,
                                &curr,
                                elapsed,
                                &mut peaks,
                                sample_count,
                                true,
                            );
                            prev = curr;
                            prev_time = now;
                            if let Ok(mut g) = snap_bg.lock() {
                                *g = next;
                            }
                            (on_update)();
                        }
                        Ok(_) => {}
                        Err(err) => {
                            if let Ok(mut g) = snap_bg.lock() {
                                g.running = false;
                                g.error = Some(err);
                            }
                            run_bg.store(false, Ordering::SeqCst);
                            (on_update)();
                            break;
                        }
                    }
                }
                if let Ok(mut g) = snap_bg.lock() {
                    g.running = false;
                }
            })
            .ok();

        Self {
            snapshot,
            running,
            stop_signal,
            handle,
        }
    }

    pub fn snapshot(&self) -> DpcMonitorSnapshot {
        self.snapshot.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        let (lock, cvar) = &*self.stop_signal;
        if let Ok(mut stopped) = lock.lock() {
            *stopped = true;
            cvar.notify_all();
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        if let Ok(mut g) = self.snapshot.lock() {
            g.running = false;
        }
    }
}

impl Drop for DpcMonitorSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
fn query_raw_core_perf() -> Result<Vec<RawCorePerfSample>, String> {
    use std::ffi::c_void;
    use std::mem::size_of;

    use windows::core::s;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct SystemProcessorPerformanceInformation {
        idle_time: i64,
        kernel_time: i64,
        user_time: i64,
        dpc_time: i64,
        interrupt_time: i64,
        interrupt_count: u32,
        _pad: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct SystemInterruptInformation {
        context_switches: u32,
        dpc_count: u32,
        dpc_rate: u32,
        time_increment: u32,
        dpc_bypass_count: u32,
        apc_bypass_count: u32,
    }

    type NtQuerySystemInformationFn = unsafe extern "system" fn(
        system_information_class: u32,
        system_information: *mut c_void,
        system_information_length: u32,
        return_length: *mut u32,
    ) -> i32;

    const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: u32 = 8;
    const SYSTEM_INTERRUPT_INFORMATION: u32 = 23;
    const MAX_LOGICAL_CORES: usize = 256;

    assert_eq!(size_of::<SystemProcessorPerformanceInformation>(), 48);
    assert_eq!(size_of::<SystemInterruptInformation>(), 24);

    // SAFETY: `ntdll.dll` is permanently loaded in every Win32 process.
    let ntdll = unsafe { GetModuleHandleA(s!("ntdll.dll")) }
        .map_err(|e| format!("Failed to locate ntdll.dll: {e}"))?;
    let proc = unsafe { GetProcAddress(ntdll, s!("NtQuerySystemInformation")) }
        .ok_or_else(|| "NtQuerySystemInformation export not found".to_string())?;
    // SAFETY: Matches official NT prototype in `winternl.h`.
    let nt_query: NtQuerySystemInformationFn = unsafe { std::mem::transmute(proc) };

    let mut perf_buf = vec![SystemProcessorPerformanceInformation::default(); MAX_LOGICAL_CORES];
    let mut ret_len = 0u32;
    let status = unsafe {
        nt_query(
            SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION,
            perf_buf.as_mut_ptr().cast::<c_void>(),
            (perf_buf.len() * size_of::<SystemProcessorPerformanceInformation>()) as u32,
            &mut ret_len,
        )
    };
    if status < 0 {
        return Err(format!(
            "NtQuerySystemInformation(ProcessorPerformance) failed: 0x{:08X}",
            status as u32
        ));
    }
    let count = (ret_len as usize) / size_of::<SystemProcessorPerformanceInformation>();
    if count == 0 {
        return Ok(Vec::new());
    }

    // Best-effort secondary query for per-core DPC counts (`SystemInterruptInformation = 23`).
    let mut irq_buf = vec![SystemInterruptInformation::default(); count];
    let mut irq_ret_len = 0u32;
    let irq_ok = unsafe {
        nt_query(
            SYSTEM_INTERRUPT_INFORMATION,
            irq_buf.as_mut_ptr().cast::<c_void>(),
            (irq_buf.len() * size_of::<SystemInterruptInformation>()) as u32,
            &mut irq_ret_len,
        )
    } >= 0
        && (irq_ret_len as usize) >= count * size_of::<SystemInterruptInformation>();

    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let p = perf_buf[i];
        out.push(RawCorePerfSample {
            idle_time: p.idle_time,
            kernel_time: p.kernel_time,
            user_time: p.user_time,
            dpc_time: p.dpc_time,
            interrupt_time: p.interrupt_time,
            interrupt_count: p.interrupt_count,
            dpc_count: if irq_ok {
                Some(irq_buf[i].dpc_count)
            } else {
                None
            },
        });
    }
    Ok(out)
}

#[cfg(not(windows))]
fn query_raw_core_perf() -> Result<Vec<RawCorePerfSample>, String> {
    Err("Per-core Interrupt/DPC monitoring is only available on Windows.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_clean_vs_core0_starved_dpc_profiles() {
        // 4 cores over 0.5s (5,000,000 * 100ns ticks per core):
        // Core 0: 15% DPC + 3% IRQ = 18% DPC+IRQ -> Core 0 starvation!
        // Cores 1..3: 0.5% DPC + 0.2% IRQ = 0.7%
        let prev = vec![
            RawCorePerfSample {
                dpc_count: Some(0),
                ..RawCorePerfSample::default()
            };
            4
        ];
        let curr = vec![
            RawCorePerfSample {
                idle_time: 3_000_000,
                kernel_time: 4_500_000,
                user_time: 500_000,
                dpc_time: 750_000,
                interrupt_time: 150_000,
                interrupt_count: 8_000,
                dpc_count: Some(4_000),
            },
            RawCorePerfSample {
                idle_time: 4_500_000,
                kernel_time: 4_700_000,
                user_time: 300_000,
                dpc_time: 25_000,
                interrupt_time: 10_000,
                interrupt_count: 500,
                dpc_count: Some(250),
            },
            RawCorePerfSample {
                idle_time: 4_600_000,
                kernel_time: 4_800_000,
                user_time: 200_000,
                dpc_time: 20_000,
                interrupt_time: 10_000,
                interrupt_count: 450,
                dpc_count: Some(200),
            },
            RawCorePerfSample {
                idle_time: 4_600_000,
                kernel_time: 4_800_000,
                user_time: 200_000,
                dpc_time: 20_000,
                interrupt_time: 10_000,
                interrupt_count: 450,
                dpc_count: Some(200),
            },
        ];

        let mut peaks = Vec::new();
        let snap = compute_dpc_snapshot(&prev, &curr, 0.5, &mut peaks, 1, true);
        assert_eq!(snap.core_count, 4);
        assert_eq!(snap.hottest_core, Some(0));
        assert!((snap.core0_dpc_irq_pct - 18.0).abs() < 0.1);
        assert!(snap.core0_starved);
        assert_eq!(snap.severity, Severity::Bad);
        assert_eq!(snap.cores[0].interrupts_per_sec, 16_000.0);
        assert_eq!(snap.cores[0].dpcs_per_sec, Some(8_000.0));

        // Clean balanced snapshot across all 4 cores (< 1% DPC+IRQ)
        let clean_curr = vec![
            RawCorePerfSample {
                idle_time: 4_500_000,
                kernel_time: 4_600_000,
                user_time: 400_000,
                dpc_time: 20_000,
                interrupt_time: 15_000,
                interrupt_count: 400,
                dpc_count: Some(150),
            };
            4
        ];
        let mut clean_peaks = Vec::new();
        let clean_snap = compute_dpc_snapshot(&prev, &clean_curr, 0.5, &mut clean_peaks, 1, true);
        assert!(!clean_snap.core0_starved);
        assert_eq!(clean_snap.severity, Severity::Ok);
    }

    #[cfg(windows)]
    #[test]
    fn windows_nt_query_returns_live_logical_cores() {
        let cores = query_raw_core_perf().expect("NtQuerySystemInformation should succeed");
        assert!(!cores.is_empty());
        assert!(cores.len() <= 256);
    }
}
