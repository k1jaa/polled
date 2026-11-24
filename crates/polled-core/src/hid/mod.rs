// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod dump;

pub use dump::{
    dump_hid_devices, parse_hid_report_descriptor, HidDeviceDump, HidDumpReport, HidReportSummary,
};
