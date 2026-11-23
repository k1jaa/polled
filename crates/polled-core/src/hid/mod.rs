// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod dump;

pub use dump::{dump_hid_devices, HidDeviceDump, HidDumpReport};
