// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod checks;
pub mod devices;
pub mod diagnostics;
pub mod error;
pub mod hid;
pub mod paths;
pub mod profiles;
pub mod settings;
pub mod win;

pub use checks::{
    curated_custom_tweak_presets, CheckReport, CustomTweakCheck, CustomTweakDef, CustomTweakStore,
    CustomValueKind, HealthCheck, HealthEngine, Regression, SavedValue, ScanSummary, Severity,
    UndoSummary,
};
pub use devices::{
    detect_hardware_devices, hid_topology_fingerprint, ApplyOutcome, BatteryStatus, Compx64Device,
    Compx64ProtocolDriver, CompxDevice, CompxProtocolDriver, DeviceCapabilities, DeviceDriver,
    DeviceInfo, DryRunDevice, EndgameGearOp1Device, EndgameGearOp1ProtocolDriver, FinalmouseDevice,
    FinalmouseProtocolDriver, Hidpp20Device, Hidpp20ProtocolDriver, IncottDevice,
    IncottProtocolDriver, KeychronDevice, KeychronProtocolDriver, LiftOffDistance, MchoseDevice,
    MchoseProtocolDriver, MouseDevice, MouseSettings, NinjutsoDevice, NinjutsoProtocolDriver,
    RazerDevice, RazerProtocolDriver, SafeDeviceController, SupportTier, VaxeeDevice,
    VaxeeProtocolDriver, WriteMode,
};
pub use diagnostics::{
    build_diagnostic_advice, compute_sensitivity, format_advice_summary,
    format_audio_benchmark_summary, format_diagnostic_report, format_intervals_csv,
    format_network_benchmark_summary, render_ab_comparison_png, render_report_png,
    render_rig_scorecard_png, run_audio_latency_benchmark, run_network_jitter_benchmark,
    run_timer_jitter_benchmark, ButtonBounceStats, ClickBounceAnalyzer, ClickBounceSnapshot,
    ClickEventRecord, DiagnosticAdvice, DiagnosticsSnapshot, DpiCalibrationResult, DpiCalibrator,
    DpiMeasurementSnapshot, HistogramBin, IntervalSample, KeyboardPollingAnalyzer,
    KeyboardPollingStats, MouseButtonKind, PinnedBenchmark, PollingAnalyzer, PollingStats,
    PollingTarget, RawInputCaptureSession, RigScorecardInput, SensitivityConversion,
    SensitivityGame, SystemLoadStats, TimerJitterBenchmark, TrackingStats, XyTracePoint,
    BOUNCE_THRESHOLD_MS, HISTOGRAM_BASE_BINS, KEY_CHATTER_THRESHOLD_MS, STANDARD_RATES_HZ,
};
pub use error::{PolledError, Result};
pub use hid::{dump_hid_devices, HidDeviceDump, HidDumpReport};
pub use paths::AppPaths;
pub use profiles::{
    discover_installed_games, discover_running_apps, export_profile_json, import_profile_json,
    match_profile, normalize_exe_name, DiscoveredGame, FocusDebouncer, FocusEvent,
    ForegroundWatcher, GameProfile, GameSource, PowerPlanPreset, ProfileCardExport, ProfileStore,
};
pub use settings::{
    format_autostart_command, is_autostart_enabled, set_autostart_enabled, AppSettings,
    HotkeyBinding, HotkeyConfig, ThemePreference, TrayAlertConfig,
};
pub use win::{
    AudioEndpointInfo, AudioFlow, AudioLatencyBenchmark, NetworkAdapterInfo, NetworkAdapterKind,
    NetworkJitterBenchmark, PingHopStats,
};
