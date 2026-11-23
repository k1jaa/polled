// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod error;
pub mod paths;
pub mod win;

pub use error::{PolledError, Result};
pub use paths::AppPaths;
