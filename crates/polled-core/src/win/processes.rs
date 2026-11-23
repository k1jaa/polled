// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::thread;

use sysinfo::{CpuRefreshKind, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::error::Result;
use crate::win::ProcessSample;

/// Samples running processes with Task-Manager-equivalent CPU% and resident memory.
///
/// Uses `sysinfo` which queries OS-level process tables rather than opening handles to
/// game processes or reading process memory (safe alongside Vanguard, FACEIT, EAC, BattlEye).
pub fn sample_processes() -> Result<Vec<ProcessSample>> {
    let mut sys = System::new();
    sys.refresh_cpu_list(CpuRefreshKind::nothing());
    let cpu_count = sys.cpus().len().max(1) as f32;

    let refresh_kind = ProcessRefreshKind::nothing().with_cpu().with_memory();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);
    // CPU usage requires two time points separated by at least MINIMUM_CPU_UPDATE_INTERVAL.
    thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);

    let samples = sys
        .processes()
        .iter()
        .map(|(pid, proc_info)| ProcessSample {
            pid: pid.as_u32(),
            name: proc_info.name().to_string_lossy().into_owned(),
            cpu_percent: (proc_info.cpu_usage() / cpu_count).clamp(0.0, 100.0),
            memory_bytes: proc_info.memory(),
        })
        .collect();

    Ok(samples)
}
