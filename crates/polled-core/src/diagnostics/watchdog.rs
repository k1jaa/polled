// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Strictly user-started Passive Match Watchdog (zero game hooks, zero injection, zero admin).
//!
//! Records lightweight 2-second telemetry snapshots while you play a match (rolling up to 60 min /
//! 1,800 samples):
//! - Windows total CPU % & kernel/DPC CPU % via `GetSystemTimes`
//! - Scheduler wake overshoot (ms) via `Instant::now()` drift
//! - Local router gateway ICMP RTT + target internet IP ICMP RTT via Win32 `IcmpSendEcho`
//!
//! Automatically detects and timestamps stutter / spike events so you can review after a match
//! whether a lag spike came from CPU/DPC saturation, OS scheduling stalls, local router/Wi-Fi, or
//! your ISP upstream hop.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::checks::Severity;

const WATCHDOG_INTERVAL_SECS: u64 = 2;
const WATCHDOG_MAX_SAMPLES: usize = 1_800; // 60 minutes at 2s cadence
const WATCHDOG_MAX_EVENTS: usize = 120;

/// Single 2-second telemetry sample recorded by the Passive Match Watchdog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchdogSample {
    pub elapsed_secs: u64,
    pub cpu_pct: f64,
    pub kernel_pct: f64,
    pub wake_overshoot_ms: f64,
    pub gateway_ms: Option<f64>,
    pub internet_ms: Option<f64>,
}

/// Detected stutter or latency spike event recorded during a Watchdog session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchdogEvent {
    pub wall_time: String,
    pub elapsed_secs: u64,
    pub severity: Severity,
    pub category: String,
    pub detail: String,
}

/// Immutable snapshot of a Passive Match Watchdog session rendered in Bench → System & USB.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WatchdogSnapshot {
    pub running: bool,
    pub target_ip: String,
    pub gateway_ip: Option<String>,
    pub elapsed_secs: u64,
    pub sample_count: u64,
    pub avg_cpu_pct: f64,
    pub peak_cpu_pct: f64,
    pub peak_kernel_pct: f64,
    pub peak_wake_overshoot_ms: f64,
    pub median_gateway_ms: Option<f64>,
    pub peak_gateway_ms: Option<f64>,
    pub median_internet_ms: Option<f64>,
    pub peak_internet_ms: Option<f64>,
    pub gateway_timeouts: u64,
    pub internet_timeouts: u64,
    pub samples: Vec<WatchdogSample>,
    pub events: Vec<WatchdogEvent>,
    pub diagnosis: String,
}

#[derive(Debug)]
struct WatchdogAccumulator {
    target_ip: String,
    gateway_ip: Option<String>,
    sample_count: u64,
    sum_cpu_pct: f64,
    peak_cpu_pct: f64,
    peak_kernel_pct: f64,
    peak_wake_overshoot_ms: f64,
    gateway_timeouts: u64,
    internet_timeouts: u64,
    samples: VecDeque<WatchdogSample>,
    events: VecDeque<WatchdogEvent>,
}

impl WatchdogAccumulator {
    fn new(target_ip: String, gateway_ip: Option<String>) -> Self {
        Self {
            target_ip,
            gateway_ip,
            sample_count: 0,
            sum_cpu_pct: 0.0,
            peak_cpu_pct: 0.0,
            peak_kernel_pct: 0.0,
            peak_wake_overshoot_ms: 0.0,
            gateway_timeouts: 0,
            internet_timeouts: 0,
            samples: VecDeque::with_capacity(256),
            events: VecDeque::with_capacity(32),
        }
    }

    fn record_sample(&mut self, sample: WatchdogSample, wall_time: &str) {
        self.sample_count += 1;
        self.sum_cpu_pct += sample.cpu_pct;
        if sample.cpu_pct > self.peak_cpu_pct {
            self.peak_cpu_pct = sample.cpu_pct;
        }
        if sample.kernel_pct > self.peak_kernel_pct {
            self.peak_kernel_pct = sample.kernel_pct;
        }
        if sample.wake_overshoot_ms > self.peak_wake_overshoot_ms {
            self.peak_wake_overshoot_ms = sample.wake_overshoot_ms;
        }

        // Compute running baseline medians before pushing this sample so spike checks compare
        // against normal steady-state latency.
        let baseline_inet = self.median_internet();
        let baseline_gw = self.median_gateway();

        if self.gateway_ip.is_some() && sample.gateway_ms.is_none() {
            self.gateway_timeouts += 1;
            self.push_event(WatchdogEvent {
                wall_time: wall_time.to_string(),
                elapsed_secs: sample.elapsed_secs,
                severity: Severity::Bad,
                category: "Router Loss".to_string(),
                detail: format!(
                    "Local gateway {} timed out (packet dropped before leaving home LAN).",
                    self.gateway_ip.as_deref().unwrap_or("gateway")
                ),
            });
        } else if let Some(gw_ms) = sample.gateway_ms {
            let gw_threshold = baseline_gw.map_or(15.0, |m| (m * 4.0).max(m + 12.0).max(15.0));
            if gw_ms >= gw_threshold {
                self.push_event(WatchdogEvent {
                    wall_time: wall_time.to_string(),
                    elapsed_secs: sample.elapsed_secs,
                    severity: if gw_ms >= 35.0 {
                        Severity::Bad
                    } else {
                        Severity::Warn
                    },
                    category: "Local LAN Spike".to_string(),
                    detail: format!(
                        "Router ping spiked to {gw_ms:.1} ms (normal {:.1} ms).",
                        baseline_gw.unwrap_or(1.0)
                    ),
                });
            }
        }

        if sample.internet_ms.is_none() {
            self.internet_timeouts += 1;
            // Only flag ISP loss if gateway didn't already drop on the same tick.
            let gw_dropped = self.gateway_ip.is_some() && sample.gateway_ms.is_none();
            if !gw_dropped {
                self.push_event(WatchdogEvent {
                    wall_time: wall_time.to_string(),
                    elapsed_secs: sample.elapsed_secs,
                    severity: Severity::Bad,
                    category: "ISP Packet Loss".to_string(),
                    detail: format!(
                        "Ping to {} timed out while local router stayed reachable.",
                        self.target_ip
                    ),
                });
            }
        } else if let Some(inet_ms) = sample.internet_ms {
            let inet_threshold = baseline_inet.map_or(65.0, |m| (m + 28.0).max(m * 2.0));
            let gw_ok = sample.gateway_ms.is_none_or(|g| g < 15.0);
            if inet_ms >= inet_threshold && gw_ok {
                self.push_event(WatchdogEvent {
                    wall_time: wall_time.to_string(),
                    elapsed_secs: sample.elapsed_secs,
                    severity: if inet_ms >= inet_threshold + 30.0 {
                        Severity::Bad
                    } else {
                        Severity::Warn
                    },
                    category: "ISP Ping Spike".to_string(),
                    detail: format!(
                        "Internet ping ({}) spiked to {inet_ms:.1} ms (baseline {:.1} ms; local router steady).",
                        self.target_ip,
                        baseline_inet.unwrap_or(inet_ms)
                    ),
                });
            }
        }

        if sample.wake_overshoot_ms >= 40.0 {
            self.push_event(WatchdogEvent {
                wall_time: wall_time.to_string(),
                elapsed_secs: sample.elapsed_secs,
                severity: if sample.wake_overshoot_ms >= 100.0 {
                    Severity::Bad
                } else {
                    Severity::Warn
                },
                category: "OS Scheduler Stall".to_string(),
                detail: format!(
                    "Thread wakeup delayed by +{:.1} ms (CPU {:.0}%, kernel {:.0}%).",
                    sample.wake_overshoot_ms, sample.cpu_pct, sample.kernel_pct
                ),
            });
        } else if sample.kernel_pct >= 35.0 {
            self.push_event(WatchdogEvent {
                wall_time: wall_time.to_string(),
                elapsed_secs: sample.elapsed_secs,
                severity: if sample.kernel_pct >= 55.0 {
                    Severity::Bad
                } else {
                    Severity::Warn
                },
                category: "Kernel / DPC Spike".to_string(),
                detail: format!(
                    "Kernel/driver CPU hit {:.0}% (total CPU {:.0}%).",
                    sample.kernel_pct, sample.cpu_pct
                ),
            });
        } else if sample.cpu_pct >= 94.0 {
            self.push_event(WatchdogEvent {
                wall_time: wall_time.to_string(),
                elapsed_secs: sample.elapsed_secs,
                severity: Severity::Warn,
                category: "CPU Saturation".to_string(),
                detail: format!(
                    "Total CPU reached {:.0}% (kernel {:.0}%).",
                    sample.cpu_pct, sample.kernel_pct
                ),
            });
        }

        if self.samples.len() >= WATCHDOG_MAX_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    fn push_event(&mut self, event: WatchdogEvent) {
        if self.events.len() >= WATCHDOG_MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn median_gateway(&self) -> Option<f64> {
        let mut vals: Vec<f64> = self.samples.iter().filter_map(|s| s.gateway_ms).collect();
        median_of(&mut vals)
    }

    fn median_internet(&self) -> Option<f64> {
        let mut vals: Vec<f64> = self.samples.iter().filter_map(|s| s.internet_ms).collect();
        median_of(&mut vals)
    }

    fn snapshot(&self, running: bool, elapsed_secs: u64) -> WatchdogSnapshot {
        let avg_cpu_pct = if self.sample_count > 0 {
            self.sum_cpu_pct / (self.sample_count as f64)
        } else {
            0.0
        };
        let median_gateway_ms = self.median_gateway();
        let peak_gateway_ms = self
            .samples
            .iter()
            .filter_map(|s| s.gateway_ms)
            .reduce(f64::max);
        let median_internet_ms = self.median_internet();
        let peak_internet_ms = self
            .samples
            .iter()
            .filter_map(|s| s.internet_ms)
            .reduce(f64::max);

        let diagnosis = if self.sample_count == 0 {
            "Click Start Watchdog before a match to passively log CPU/kernel load, OS stalls, and router vs ISP ping every 2s (zero game hooks)."
                .to_string()
        } else if self.events.is_empty() {
            format!(
                "Clean session ({} samples) — avg {avg_cpu_pct:.0}% CPU (peak {:.0}%), 0 spikes or packet drops detected.",
                self.sample_count, self.peak_cpu_pct
            )
        } else {
            let mut os_events = 0usize;
            let mut lan_events = 0usize;
            let mut isp_events = 0usize;
            for ev in &self.events {
                if ev.category.contains("Router") || ev.category.contains("LAN") {
                    lan_events += 1;
                } else if ev.category.contains("ISP") {
                    isp_events += 1;
                } else {
                    os_events += 1;
                }
            }
            format!(
                "Logged {} spike event(s): {} OS/CPU/DPC · {} Local Router/Wi-Fi · {} ISP/Upstream.",
                self.events.len(),
                os_events,
                lan_events,
                isp_events
            )
        };

        WatchdogSnapshot {
            running,
            target_ip: self.target_ip.clone(),
            gateway_ip: self.gateway_ip.clone(),
            elapsed_secs,
            sample_count: self.sample_count,
            avg_cpu_pct,
            peak_cpu_pct: self.peak_cpu_pct,
            peak_kernel_pct: self.peak_kernel_pct,
            peak_wake_overshoot_ms: self.peak_wake_overshoot_ms,
            median_gateway_ms,
            peak_gateway_ms,
            median_internet_ms,
            peak_internet_ms,
            gateway_timeouts: self.gateway_timeouts,
            internet_timeouts: self.internet_timeouts,
            samples: self.samples.iter().cloned().collect(),
            events: self.events.iter().cloned().collect(),
            diagnosis,
        }
    }
}

fn median_of(vals: &mut [f64]) -> Option<f64> {
    if vals.is_empty() {
        return None;
    }
    vals.sort_by(f64::total_cmp);
    Some(vals[vals.len() / 2])
}

#[cfg(windows)]
fn read_cpu_ticks() -> Option<(u64, u64, u64)> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetSystemTimes;

    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: Standard Win32 `GetSystemTimes` writes 64-bit tick counters into stack structs.
    unsafe {
        GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).ok()?;
    }
    let to_u64 = |ft: FILETIME| (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
    Some((to_u64(idle), to_u64(kernel), to_u64(user)))
}

#[cfg(not(windows))]
fn read_cpu_ticks() -> Option<(u64, u64, u64)> {
    None
}

/// Active user-started Passive Match Watchdog background session.
pub struct MatchWatchdogSession {
    snapshot: Arc<Mutex<WatchdogSnapshot>>,
    running: Arc<AtomicBool>,
    stop_signal: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}

impl MatchWatchdogSession {
    /// Starts a passive 2-second match telemetry logger until `stop()` or drop.
    pub fn start<F>(target_ip: String, on_update: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        let clean_ip = {
            let t = target_ip.trim();
            if t.is_empty() {
                "1.1.1.1".to_string()
            } else {
                t.to_string()
            }
        };

        #[cfg(windows)]
        let gateway_ip = crate::win::network::primary_gateway_ipv4();
        #[cfg(not(windows))]
        let gateway_ip: Option<String> = None;

        let initial = WatchdogSnapshot {
            running: true,
            target_ip: clean_ip.clone(),
            gateway_ip: gateway_ip.clone(),
            diagnosis: format!("Watchdog active — sampling CPU, scheduler & ping ({clean_ip}) every {WATCHDOG_INTERVAL_SECS}s..."),
            ..Default::default()
        };

        let snapshot = Arc::new(Mutex::new(initial));
        let running = Arc::new(AtomicBool::new(true));
        let stop_signal = Arc::new((Mutex::new(false), Condvar::new()));

        let snap_bg = Arc::clone(&snapshot);
        let run_bg = Arc::clone(&running);
        let sig_bg = Arc::clone(&stop_signal);

        let handle = std::thread::Builder::new()
            .name("polled-match-watchdog".to_string())
            .spawn(move || {
                let started = Instant::now();
                let mut acc = WatchdogAccumulator::new(clean_ip.clone(), gateway_ip.clone());
                let mut prev_ticks = read_cpu_ticks();
                let step = Duration::from_secs(WATCHDOG_INTERVAL_SECS);

                while run_bg.load(Ordering::SeqCst) {
                    let sleep_start = Instant::now();
                    if let Ok(guard) = sig_bg.0.lock() {
                        if !*guard {
                            let _ = sig_bg.1.wait_timeout(guard, step);
                        }
                    }
                    if !run_bg.load(Ordering::SeqCst) {
                        break;
                    }
                    let slept_ms = sleep_start.elapsed().as_secs_f64() * 1_000.0;
                    let wake_overshoot_ms =
                        (slept_ms - (WATCHDOG_INTERVAL_SECS as f64 * 1_000.0)).max(0.0);

                    let (mut cpu_pct, mut kernel_pct) = (0.0, 0.0);
                    if let Some((idle, kernel, user)) = read_cpu_ticks() {
                        if let Some((p_idle, p_kernel, p_user)) = prev_ticks {
                            let d_idle = idle.saturating_sub(p_idle);
                            let d_kernel = kernel.saturating_sub(p_kernel);
                            let d_user = user.saturating_sub(p_user);
                            let d_total = d_kernel.saturating_add(d_user);
                            if d_total > 0 {
                                let d_kernel_busy = d_kernel.saturating_sub(d_idle);
                                let d_busy = d_kernel_busy.saturating_add(d_user);
                                cpu_pct =
                                    ((d_busy as f64) * 100.0 / (d_total as f64)).clamp(0.0, 100.0);
                                kernel_pct = ((d_kernel_busy as f64) * 100.0 / (d_total as f64))
                                    .clamp(0.0, 100.0);
                            }
                        }
                        prev_ticks = Some((idle, kernel, user));
                    }

                    #[cfg(windows)]
                    let gateway_ms = gateway_ip
                        .as_deref()
                        .and_then(|gw| crate::win::network::ping_once_ipv4(gw, 400));
                    #[cfg(not(windows))]
                    let gateway_ms: Option<f64> = None;

                    #[cfg(windows)]
                    let internet_ms = crate::win::network::ping_once_ipv4(&clean_ip, 600);
                    #[cfg(not(windows))]
                    let internet_ms: Option<f64> = None;

                    let elapsed_secs = started.elapsed().as_secs();
                    let wall_time = Local::now().format("%H:%M:%S").to_string();
                    acc.record_sample(
                        WatchdogSample {
                            elapsed_secs,
                            cpu_pct,
                            kernel_pct,
                            wake_overshoot_ms,
                            gateway_ms,
                            internet_ms,
                        },
                        &wall_time,
                    );

                    let next_snap = acc.snapshot(true, elapsed_secs);
                    if let Ok(mut g) = snap_bg.lock() {
                        *g = next_snap;
                    }
                    (on_update)();
                }

                let final_snap = acc.snapshot(false, started.elapsed().as_secs());
                if let Ok(mut g) = snap_bg.lock() {
                    *g = final_snap;
                }
                (on_update)();
            })
            .ok();

        Self {
            snapshot,
            running,
            stop_signal,
            handle,
        }
    }

    pub fn snapshot(&self) -> WatchdogSnapshot {
        self.snapshot.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Ok(mut stop_flag) = self.stop_signal.0.lock() {
            *stop_flag = true;
            self.stop_signal.1.notify_all();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        if let Ok(mut g) = self.snapshot.lock() {
            g.running = false;
        }
    }
}

impl Drop for MatchWatchdogSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Formats a Passive Match Watchdog session into a shareable plain-text summary report.
pub fn format_watchdog_report(snap: &WatchdogSnapshot) -> String {
    let mins = snap.elapsed_secs / 60;
    let secs = snap.elapsed_secs % 60;
    let gw_line = match (snap.gateway_ip.as_deref(), snap.median_gateway_ms) {
        (Some(ip), Some(med)) => format!(
            "Local Gateway ({ip}): median {med:.1} ms | peak {:.1} ms | timeouts {}",
            snap.peak_gateway_ms.unwrap_or(med),
            snap.gateway_timeouts
        ),
        (Some(ip), None) => format!(
            "Local Gateway ({ip}): no replies ({} timeouts)",
            snap.gateway_timeouts
        ),
        _ => "Local Gateway: unavailable".to_string(),
    };
    let inet_line = match snap.median_internet_ms {
        Some(med) => format!(
            "Internet ({}): median {med:.1} ms | peak {:.1} ms | timeouts {}",
            snap.target_ip,
            snap.peak_internet_ms.unwrap_or(med),
            snap.internet_timeouts
        ),
        None => format!(
            "Internet ({}): no replies ({} timeouts)",
            snap.target_ip, snap.internet_timeouts
        ),
    };

    let mut lines = vec![
        "Polled Passive Match Watchdog Report".to_string(),
        format!("Duration:       {mins:02}m {secs:02}s ({} samples @ 2s)", snap.sample_count),
        format!(
            "CPU / Kernel:   avg {:.0}% CPU | peak {:.0}% CPU | peak {:.0}% kernel | max wake +{:.1} ms",
            snap.avg_cpu_pct, snap.peak_cpu_pct, snap.peak_kernel_pct, snap.peak_wake_overshoot_ms
        ),
        format!("Router Hop:     {gw_line}"),
        format!("Internet Hop:   {inet_line}"),
        format!("Diagnosis:      {}", snap.diagnosis),
    ];
    if !snap.events.is_empty() {
        lines.push(format!("Detected Spike Events ({}):", snap.events.len()));
        for ev in &snap.events {
            lines.push(format!(
                "  [{}] +{:02}:{:02} [{}] {}: {}",
                ev.wall_time,
                ev.elapsed_secs / 60,
                ev.elapsed_secs % 60,
                ev.severity.as_str().to_uppercase(),
                ev.category,
                ev.detail
            ));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_classifies_clean_session_vs_lan_isp_and_os_spikes() {
        let mut acc =
            WatchdogAccumulator::new("1.1.1.1".to_string(), Some("192.168.1.1".to_string()));
        for i in 0..5u64 {
            acc.record_sample(
                WatchdogSample {
                    elapsed_secs: i * 2,
                    cpu_pct: 32.0,
                    kernel_pct: 6.0,
                    wake_overshoot_ms: 1.2,
                    gateway_ms: Some(0.6),
                    internet_ms: Some(12.0),
                },
                "20:00:00",
            );
        }
        let clean = acc.snapshot(true, 8);
        assert_eq!(clean.events.len(), 0);
        assert!(clean.diagnosis.contains("Clean session"));

        // Inject ISP ping spike (local router stays clean).
        acc.record_sample(
            WatchdogSample {
                elapsed_secs: 10,
                cpu_pct: 35.0,
                kernel_pct: 7.0,
                wake_overshoot_ms: 1.5,
                gateway_ms: Some(0.7),
                internet_ms: Some(78.0),
            },
            "20:00:10",
        );
        // Inject OS scheduler stall + router drop.
        acc.record_sample(
            WatchdogSample {
                elapsed_secs: 12,
                cpu_pct: 88.0,
                kernel_pct: 42.0,
                wake_overshoot_ms: 65.0,
                gateway_ms: None,
                internet_ms: None,
            },
            "20:00:12",
        );

        let snap = acc.snapshot(false, 12);
        assert!(snap.events.iter().any(|e| e.category == "ISP Ping Spike"));
        assert!(snap.events.iter().any(|e| e.category == "Router Loss"));
        assert!(snap
            .events
            .iter()
            .any(|e| e.category == "OS Scheduler Stall"));
        let report = format_watchdog_report(&snap);
        assert!(report.contains("Polled Passive Match Watchdog Report"));
        assert!(report.contains("ISP Ping Spike"));
    }
}
