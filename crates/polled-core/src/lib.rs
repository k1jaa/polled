// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod checks;
pub mod error;
pub mod hid;
pub mod paths;
pub mod win;

pub use checks::{
    CheckReport, HealthCheck, HealthEngine, Regression, SavedValue, ScanSummary, Severity,
    UndoSummary,
};
pub use error::{PolledError, Result};
pub use hid::{dump_hid_devices, HidDeviceDump, HidDumpReport};
pub use paths::AppPaths;
