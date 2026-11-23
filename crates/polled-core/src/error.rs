// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::PathBuf;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, PolledError>;

#[derive(Debug, Error)]
pub enum PolledError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Windows API error in {context}: {message}")]
    Win32 { context: String, message: String },

    #[error("Administrator privileges required for {operation}: {reason}")]
    ElevationRequired { operation: String, reason: String },

    #[error("Unknown health check id: '{0}'")]
    CheckNotFound(String),

    #[error("Health check '{0}' is report-only and does not support automatic fixes")]
    NotFixable(String),

    #[error("No saved undo snapshot exists at {0}")]
    SnapshotMissing(PathBuf),

    #[error("Invalid or mismatched snapshot value for check '{check_id}': {detail}")]
    InvalidSnapshot { check_id: String, detail: String },

    #[error("Invalid power scheme GUID: '{0}'")]
    InvalidGuid(String),

    #[error("Unsupported platform: Polled requires Windows 10 or Windows 11")]
    UnsupportedPlatform,
}
