// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod backup;
pub mod checks;
pub mod devices;
pub mod diagnostics;
pub mod error;
pub mod hid;
pub mod paths;
pub mod profiles;
pub mod settings;
pub mod update;
pub mod win;

pub use backup::{
    export_setup_bundle, export_setup_bundle_json, parse_setup_bundle_json, restore_setup_bundle,
    SetupBackupBundle, SETUP_BACKUP_FORMAT, SETUP_BACKUP_SCHEMA_VERSION,
};

pub use checks::{
    curated_custom_tweak_presets, CheckReport, CustomTweakCheck, CustomTweakDef, CustomTweakStore,
    CustomValueKind, HealthCheck, HealthEngine, Regression, SavedValue, ScanSummary, Severity,
    UndoSummary,
};
pub use devices::{
    detect_hardware_devices, detect_wooting_keyboards, hid_topology_fingerprint, ApplyOutcome,
    BatteryStatus, Compx64Device, Compx64ProtocolDriver, CompxDevice, CompxProtocolDriver,
    DeviceCapabilities, DeviceDriver, DeviceInfo, DryRunDevice, EndgameGearOp1Device,
    EndgameGearOp1ProtocolDriver, FinalmouseDevice, FinalmouseProtocolDriver, Hidpp20Device,
    Hidpp20ProtocolDriver, IncottDevice, IncottProtocolDriver, KeychronDevice,
    KeychronProtocolDriver, LiftOffDistance, MchoseDevice, MchoseProtocolDriver, MouseDevice,
    MouseSettings, NinjutsoDevice, NinjutsoProtocolDriver, RazerDevice, RazerProtocolDriver,
    SafeDeviceController, SupportTier, VaxeeDevice, VaxeeProtocolDriver, WootingAkcSummary,
    WootingAnalogBasePatch, WootingAnalogKeyReading, WootingAnalogProfile, WootingAnalogStream,
    WootingDevice, WootingDiagnostics, WootingGlobalSettings, WootingGlobalSettingsPatch,
    WootingKeyActuationOverride, WootingKeyboardInfo, WootingProfileSlot, WootingProtocol,
    WootingRapidTriggerGroupInfo, WootingRgbSummary, WriteMode, WOOTING_MAX_PROFILE_INDEX,
};
pub use diagnostics::{
    build_diagnostic_advice, compute_dpc_snapshot, compute_reaction_summary, compute_sensitivity,
    current_qpc_ns, format_advice_summary, format_audio_benchmark_summary,
    format_diagnostic_report, format_intervals_csv, format_network_benchmark_summary,
    format_watchdog_report, render_ab_comparison_png, render_report_png, render_rig_scorecard_png,
    run_audio_latency_benchmark, run_network_jitter_benchmark, run_timer_jitter_benchmark,
    ButtonBounceStats, ClickBounceAnalyzer, ClickBounceSnapshot, ClickEventRecord, CoreDpcMetrics,
    DiagnosticAdvice, DiagnosticsSnapshot, DpcMonitorSession, DpcMonitorSnapshot,
    DpiCalibrationResult, DpiCalibrator, DpiMeasurementSnapshot, HistogramBin, IntervalSample,
    KeyboardPollingAnalyzer, KeyboardPollingStats, MatchWatchdogSession, MouseButtonKind,
    PinnedBenchmark, PollingAnalyzer, PollingStats, PollingTarget, RawCorePerfSample,
    RawInputCaptureSession, ReactionTestSummary, RigScorecardInput, ScrollEncoderAnalyzer,
    ScrollEncoderSnapshot, SensitivityConversion, SensitivityGame, SystemLoadStats,
    TimerJitterBenchmark, TrackingStats, WatchdogEvent, WatchdogSample, WatchdogSnapshot,
    XyTracePoint, BOUNCE_THRESHOLD_MS, HISTOGRAM_BASE_BINS, KEY_CHATTER_THRESHOLD_MS,
    SCROLL_DOUBLE_NOTCH_MS, SCROLL_REVERSE_GLITCH_WINDOW_MS, STANDARD_RATES_HZ,
};
pub use error::{PolledError, Result};
pub use hid::{dump_hid_devices, format_unsupported_mouse_issue, HidDeviceDump, HidDumpReport};
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
pub use update::{
    check_latest_github_release, is_newer_version, parse_github_release_json, UpdateCheckResult,
    GITHUB_RELEASES_WEB_URL,
};
pub use win::{
    AudioEndpointInfo, AudioFlow, AudioLatencyBenchmark, DisplayModeInfo, DisplayResolutionMode,
    NetworkAdapterInfo, NetworkAdapterKind, NetworkJitterBenchmark, PciMsiCategory, PciMsiDevice,
    PingHopStats,
};
