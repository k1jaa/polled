// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod checks;
pub mod devices;
pub mod error;
pub mod hid;
pub mod paths;
pub mod profiles;
pub mod settings;
pub mod win;

pub use checks::{
    CheckReport, HealthCheck, HealthEngine, Regression, SavedValue, ScanSummary, Severity,
    UndoSummary,
};
pub use devices::{
    detect_hardware_devices, hid_topology_fingerprint, ApplyOutcome, CompxDevice,
    CompxProtocolDriver, DeviceCapabilities, DeviceDriver, DeviceInfo, DryRunDevice, Hidpp20Device,
    Hidpp20ProtocolDriver, LiftOffDistance, MouseDevice, MouseSettings, SafeDeviceController,
    SupportTier, WriteMode,
};
pub use error::{PolledError, Result};
pub use hid::{dump_hid_devices, HidDeviceDump, HidDumpReport};
pub use paths::AppPaths;
pub use profiles::{
    export_profile_json, import_profile_json, match_profile, normalize_exe_name, FocusDebouncer,
    FocusEvent, ForegroundWatcher, GameProfile, ProfileCardExport, ProfileStore,
};
pub use settings::{
    format_autostart_command, is_autostart_enabled, set_autostart_enabled, AppSettings,
    ThemePreference,
};
