// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::checks::{CheckReport, HealthCheck, Severity};
use crate::error::Result;
use crate::win::{ProcessSample, SystemEnv};

struct VendorSuiteDef {
    suite_name: &'static str,
    /// Lowercase process image names belonging to this peripheral suite.
    executables: &'static [&'static str],
}

const VENDOR_SUITES: &[VendorSuiteDef] = &[
    VendorSuiteDef {
        suite_name: "Logitech G Hub",
        executables: &[
            "lghub.exe",
            "lghub_agent.exe",
            "lghub_updater.exe",
            "lghub_system_tray.exe",
            "logi_lamparray_service.exe",
        ],
    },
    VendorSuiteDef {
        suite_name: "Razer Synapse",
        executables: &[
            "razer synapse 3.exe",
            "razer synapse.exe",
            "razercentralservice.exe",
            "razerappengine.exe",
            "gamemanagerservice.exe",
            "gamemanagerservice3.exe",
            "rzsdkservice.exe",
            "rzsdkserver.exe",
        ],
    },
    VendorSuiteDef {
        suite_name: "Corsair iCUE",
        executables: &[
            "icue.exe",
            "corsair.service.exe",
            "corsair.service.cpuidremote64.exe",
            "corsair.service.displayadapter.exe",
            "cue.exe",
        ],
    },
    VendorSuiteDef {
        suite_name: "SteelSeries GG",
        executables: &[
            "steelseriesgg.exe",
            "steelseriesggclient.exe",
            "steelseriesengine.exe",
            "steelseriessonar.exe",
            "steelseriescapturesvc.exe",
        ],
    },
    VendorSuiteDef {
        suite_name: "ASUS Armoury Crate",
        executables: &[
            "armourycrate.exe",
            "armourycrate.usersessionhelper.exe",
            "armourycrate.service.exe",
            "armourysocketserver.exe",
            "lightingservice.exe",
            "asuscertservice.exe",
            "rogliveservice.exe",
        ],
    },
];

/// Aggregated resource footprint for a single detected peripheral vendor suite.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectedVendorSuite {
    pub suite_name: &'static str,
    pub process_count: usize,
    pub cpu_percent: f32,
    pub memory_mb: f64,
}

/// Reports running vendor peripheral software and its combined CPU & RAM usage (report-only).
pub struct VendorSoftwareCheck;

pub fn summarize_vendor_suites(processes: &[ProcessSample]) -> Vec<DetectedVendorSuite> {
    let mut detected = Vec::new();

    for suite in VENDOR_SUITES {
        let mut count = 0usize;
        let mut cpu = 0.0f32;
        let mut mem_bytes = 0u64;

        for proc_sample in processes {
            let lower = proc_sample.name.to_ascii_lowercase();
            if suite.executables.iter().any(|&exe| exe == lower) {
                count += 1;
                cpu += proc_sample.cpu_percent;
                mem_bytes = mem_bytes.saturating_add(proc_sample.memory_bytes);
            }
        }

        if count > 0 {
            detected.push(DetectedVendorSuite {
                suite_name: suite.suite_name,
                process_count: count,
                cpu_percent: cpu,
                memory_mb: mem_bytes as f64 / (1024.0 * 1024.0),
            });
        }
    }

    // Show heaviest suites first.
    detected.sort_by(|a, b| b.memory_mb.total_cmp(&a.memory_mb));
    detected
}

impl HealthCheck for VendorSoftwareCheck {
    fn id(&self) -> &'static str {
        "vendor_software"
    }

    fn name(&self) -> &'static str {
        "Vendor Peripheral Software"
    }

    fn can_fix(&self) -> bool {
        false
    }

    fn scan(&self, env: &dyn SystemEnv) -> Result<CheckReport> {
        let processes = env.list_processes()?;
        let detected = summarize_vendor_suites(&processes);

        if detected.is_empty() {
            return Ok(CheckReport {
                id: self.id().to_string(),
                name: self.name().to_string(),
                current_value: "None running".to_string(),
                recommended_value: "Save mouse settings to onboard memory and close heavy vendor suites when not needed"
                    .to_string(),
                reason: "Heavy peripheral suites (G Hub, Synapse, iCUE, GG, Armoury Crate) can consume background CPU/RAM and cause DPC latency spikes."
                    .to_string(),
                severity: Severity::Ok,
                can_fix: false,
                requires_elevation: false,
                elevation_reason: None,
                requires_relogin: false,
            });
        }

        let total_cpu: f32 = detected.iter().map(|s| s.cpu_percent).sum();
        let total_mb: f64 = detected.iter().map(|s| s.memory_mb).sum();
        let parts: Vec<String> = detected
            .iter()
            .map(|s| {
                format!(
                    "{} ({} proc{}, {:.1}% CPU, {:.0} MB RAM)",
                    s.suite_name,
                    s.process_count,
                    if s.process_count == 1 { "" } else { "s" },
                    s.cpu_percent,
                    s.memory_mb
                )
            })
            .collect();

        let severity = if total_mb >= 300.0 || total_cpu >= 2.0 {
            Severity::Warn
        } else {
            Severity::Info
        };

        Ok(CheckReport {
            id: self.id().to_string(),
            name: self.name().to_string(),
            current_value: format!(
                "{} — Total: {:.1}% CPU, {:.0} MB RAM",
                parts.join("; "),
                total_cpu,
                total_mb
            ),
            recommended_value: "Report only (consider onboard profiles if footprint is high)"
                .to_string(),
            reason: "Background peripheral suites can consume significant RAM/CPU or wake the CPU during gameplay."
                .to_string(),
            severity,
            can_fix: false,
            requires_elevation: false,
            elevation_reason: None,
            requires_relogin: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::mock::MockEnv;

    const MB: u64 = 1024 * 1024;

    #[test]
    fn reports_ok_when_no_vendor_suites_running() {
        let env = MockEnv::new();
        env.set_processes(vec![ProcessSample {
            pid: 100,
            name: "explorer.exe".to_string(),
            cpu_percent: 0.2,
            memory_bytes: 80 * MB,
        }]);

        let check = VendorSoftwareCheck;
        assert!(!check.can_fix());
        let report = check.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Ok);
        assert_eq!(report.current_value, "None running");
    }

    #[test]
    fn aggregates_cpu_and_ram_per_vendor_suite() {
        let env = MockEnv::new();
        env.set_processes(vec![
            ProcessSample {
                pid: 1,
                name: "LGHUB.exe".to_string(),
                cpu_percent: 1.2,
                memory_bytes: 220 * MB,
            },
            ProcessSample {
                pid: 2,
                name: "lghub_agent.exe".to_string(),
                cpu_percent: 0.4,
                memory_bytes: 110 * MB,
            },
            ProcessSample {
                pid: 3,
                name: "iCUE.exe".to_string(),
                cpu_percent: 0.9,
                memory_bytes: 400 * MB,
            },
        ]);

        let check = VendorSoftwareCheck;
        let report = check.scan(&env).unwrap();
        assert_eq!(report.severity, Severity::Warn);
        assert!(report.current_value.contains("Corsair iCUE"));
        assert!(report.current_value.contains("Logitech G Hub (2 procs"));
        assert!(report.current_value.contains("730 MB RAM"));
    }
}
