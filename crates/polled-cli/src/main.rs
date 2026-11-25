// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use polled_core::win::{SystemEnv, WindowsEnv};
use polled_core::{
    check_latest_github_release, detect_hardware_devices, detect_wooting_keyboards,
    dump_hid_devices, export_setup_bundle_json, format_network_benchmark_summary,
    format_unsupported_mouse_issue, parse_setup_bundle_json, restore_setup_bundle,
    run_network_jitter_benchmark, AppPaths, ApplyOutcome, AudioEndpointInfo, AudioFlow,
    CheckReport, HealthEngine, NetworkAdapterInfo, PolledError, ProfileStore, SafeDeviceController,
    Severity, WootingKeyboardInfo,
};

#[derive(Debug, Parser)]
#[command(
    name = "polled",
    version,
    about = "Open-source Windows input health check & per-game peripheral profile tool"
)]
struct Cli {
    /// Scan all Windows input health checks
    #[arg(long)]
    scan: bool,

    /// List connected HID devices, interfaces, and usage pages (for device support reports)
    #[arg(long)]
    dump: bool,

    /// Detect supported mice and read their current settings (read-only, zero writes)
    #[arg(long)]
    detect: bool,

    /// Detect connected Wooting keyboards and read their active onboard profile (read-only)
    #[arg(long)]
    wooting: bool,

    /// List all saved per-game profiles from profiles.json
    #[arg(long)]
    list_profiles: bool,

    /// Apply a saved profile by ID or name to connected mice, Wooting keyboards, power plan, and audio
    #[arg(long, value_name = "ID_OR_NAME")]
    apply_profile: Option<String>,

    /// Export full portable setup backup (polled.setup.v1) to PATH or stdout
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "-")]
    backup: Option<String>,

    /// Restore full portable setup backup (polled.setup.v1) from PATH
    #[arg(long, value_name = "PATH")]
    restore: Option<PathBuf>,

    /// Generate a serial-redacted unsupported mouse Markdown report to PATH or stdout
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "-")]
    report_mouse: Option<String>,

    /// Check GitHub Releases once for a newer version of Polled (strictly on-demand, WinHTTP)
    #[arg(long)]
    check_update: bool,

    /// List active Windows audio playback & capture endpoints, WASAPI formats, and APO status
    #[arg(long)]
    audio: bool,

    /// List active network adapters, link speeds, gateways, DNS, and NIC power-saving flags
    #[arg(long)]
    network: bool,

    /// Run a 5-second dual-hop ICMP ping jitter & packet-loss probe (gateway + target IPv4, e.g. 1.1.1.1)
    #[arg(long, value_name = "IPV4")]
    ping: Option<String>,

    /// Optional hex Vendor ID filter for --dump (e.g. 3710 or 0x3710 for Pulsar)
    #[arg(long, value_name = "HEX_VID")]
    vid: Option<String>,

    /// Include raw hardware serial numbers in --dump output (redacted by default)
    #[arg(long)]
    include_serials: bool,

    /// Output results as machine-readable JSON
    #[arg(long)]
    json: bool,

    /// Apply the recommended fix for a specific check ID (saves a baseline snapshot first)
    #[arg(long, value_name = "ID")]
    fix: Option<String>,

    /// Undo a single previously fixed check ID from the saved snapshot
    #[arg(long, value_name = "ID")]
    undo: Option<String>,

    /// Restore all original settings from the saved baseline snapshot
    #[arg(long)]
    undo_all: bool,

    /// Override data directory (defaults to %LOCALAPPDATA%\Polled)
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Do not update state.json during --scan (useful for read-only inspection)
    #[arg(long)]
    no_state_update: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            if let PolledError::ElevationRequired { operation, reason } = &err {
                eprintln!("Elevation required ({operation}): {reason}");
                eprintln!("Re-run this specific fix from an elevated prompt or click 'Fix [Admin]' in Polled.");
                return ExitCode::from(5);
            }
            eprintln!("Error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn parse_hex_vid(input: &str) -> polled_core::Result<u16> {
    let trimmed = input
        .trim()
        .strip_prefix("0x")
        .or_else(|| input.trim().strip_prefix("0X"))
        .unwrap_or(input.trim());
    u16::from_str_radix(trimmed, 16).map_err(|_| PolledError::Win32 {
        context: "--vid".to_string(),
        message: format!("Invalid hex Vendor ID '{input}' (expected e.g. 3710 or 0x3710)"),
    })
}

fn run(cli: Cli) -> polled_core::Result<ExitCode> {
    if cli.check_update {
        let res = check_latest_github_release()?;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&res)?);
        } else if res.update_available {
            println!(
                "Update available: v{} -> v{}",
                res.current_version, res.latest_version
            );
            println!("Release page: {}", res.release_url);
            if !res.notes_preview.is_empty() {
                println!();
                println!("{}", res.notes_preview);
            }
        } else {
            println!(
                "Polled v{} is up to date (latest release: v{}).",
                res.current_version, res.latest_version
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    if cli.wooting {
        let boards = detect_wooting_keyboards()?;
        let infos: Vec<WootingKeyboardInfo> = boards
            .iter()
            .map(|b| b.info(b.read_active_profile().ok()))
            .collect();
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&infos)?);
        } else if infos.is_empty() {
            println!("No supported Wooting keyboards detected.");
            println!("Close Wootility if open and run `polled --dump --vid 31e3` to inspect HID interfaces.");
        } else {
            for kb in &infos {
                let slot = kb
                    .active_profile
                    .map(WootingKeyboardInfo::profile_label)
                    .unwrap_or("Unknown");
                println!("{} ({}) — Active: {}", kb.name, kb.vid_pid, slot);
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(target_path) = cli.report_mouse {
        let dump = dump_hid_devices(false, None)?;
        let env = WindowsEnv;
        let topology = env.list_usb_topology().unwrap_or_default();
        let win_ver = env.windows_version().ok().map(|v| v.display_string());
        let detected: Vec<_> = detect_hardware_devices()
            .unwrap_or_default()
            .iter()
            .map(|d| d.info())
            .collect();
        let md = format_unsupported_mouse_issue(&dump, &topology, &detected, win_ver.as_deref());
        if target_path == "-" {
            println!("{md}");
        } else {
            let p = PathBuf::from(&target_path);
            fs::write(&p, &md).map_err(|source| PolledError::Io {
                path: p.clone(),
                source,
            })?;
            println!("Wrote serial-redacted mouse report to {}", p.display());
        }
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(target_ip) = cli.ping {
        let bench = run_network_jitter_benchmark(&target_ip, 20, 250)?;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&bench)?);
        } else {
            println!("{}", format_network_benchmark_summary(&bench));
        }
        return Ok(ExitCode::SUCCESS);
    }

    if cli.network {
        let env = WindowsEnv;
        let adapters = env.list_network_adapters()?;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&adapters)?);
        } else {
            println!("Active Network Adapters:");
            if adapters.is_empty() {
                println!("  (none)");
            } else {
                for nic in &adapters {
                    print_network_adapter(nic);
                }
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    if cli.audio {
        let env = WindowsEnv;
        let playback = env.list_audio_endpoints(AudioFlow::Render)?;
        let capture = env.list_audio_endpoints(AudioFlow::Capture)?;
        if cli.json {
            let out = serde_json::json!({
                "playback": playback,
                "capture": capture,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else {
            println!("Playback (Render) Endpoints:");
            if playback.is_empty() {
                println!("  (none)");
            } else {
                for ep in &playback {
                    print_audio_endpoint(ep);
                }
            }
            println!();
            println!("Capture (Microphone) Endpoints:");
            if capture.is_empty() {
                println!("  (none)");
            } else {
                for ep in &capture {
                    print_audio_endpoint(ep);
                }
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    if cli.detect {
        let mut devices = detect_hardware_devices()?;
        if devices.is_empty() {
            if cli.json {
                println!("[]");
            } else {
                println!("No supported hardware mice detected.");
                println!("Run `polled --dump` to inspect all connected HID interfaces.");
            }
            return Ok(ExitCode::SUCCESS);
        }

        if cli.json {
            let entries: Vec<_> = devices
                .iter_mut()
                .map(|d| {
                    let info = d.info();
                    let caps = d.capabilities();
                    let settings = d.read_settings().ok();
                    let battery = d.read_battery();
                    serde_json::json!({
                        "info": info,
                        "capabilities": caps,
                        "current_settings": settings,
                        "battery": battery,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&entries)?);
        } else {
            for d in &mut devices {
                let info = d.info();
                let caps = d.capabilities();
                println!(
                    "{} ({}) [{}]",
                    info.name,
                    info.vid_pid.as_deref().unwrap_or("n/a"),
                    info.support_tier.label()
                );
                println!("  Connection: {}", info.connection);
                println!(
                    "  Writes:     {}",
                    if caps.supports_volatile_writes {
                        "Volatile RAM (no flash wear)"
                    } else {
                        "Onboard flash (diffed & rate-limited)"
                    }
                );
                match d.read_settings() {
                    Ok(s) => println!("  Current:    {}", s.summary_string()),
                    Err(e) => println!("  Current:    Unavailable ({e})"),
                }
                if let Some(batt) = d.read_battery() {
                    println!("  Battery:    {}", batt.detail_label());
                }
                println!();
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    if cli.dump {
        let filter_vid = match &cli.vid {
            Some(v) => Some(parse_hex_vid(v)?),
            None => None,
        };
        let report = dump_hid_devices(cli.include_serials, filter_vid)?;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!(
                "Polled HID Interface Dump ({} interface{}, serials {})",
                report.device_count,
                if report.device_count == 1 { "" } else { "s" },
                if report.serials_redacted {
                    "redacted"
                } else {
                    "included"
                }
            );
            println!();
            for dev in &report.devices {
                let product = dev.product.as_deref().unwrap_or("(unnamed)");
                let mfg = dev.manufacturer.as_deref().unwrap_or("(unknown vendor)");
                let reports_str = dev
                    .reports
                    .as_ref()
                    .map(|r| r.compact_label())
                    .unwrap_or_else(|| "reports:unopened".to_string());
                println!(
                    "{}  if={:<2}  usage_page=0x{:04x} usage=0x{:04x}  {}  {} — {}",
                    dev.vid_pid,
                    dev.interface_number,
                    dev.usage_page,
                    dev.usage,
                    reports_str,
                    mfg,
                    product
                );
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    let paths = match &cli.data_dir {
        Some(dir) => AppPaths::from_root(dir),
        None => AppPaths::default_user_dir(),
    };

    if let Some(target_path) = cli.backup {
        let json = export_setup_bundle_json(&paths)?;
        if target_path == "-" {
            println!("{json}");
        } else {
            let p = PathBuf::from(&target_path);
            fs::write(&p, &json).map_err(|source| PolledError::Io {
                path: p.clone(),
                source,
            })?;
            println!(
                "Saved full setup backup (polled.setup.v1) to {}",
                p.display()
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(src_path) = cli.restore {
        let raw = fs::read_to_string(&src_path).map_err(|source| PolledError::Io {
            path: src_path.clone(),
            source,
        })?;
        let bundle = parse_setup_bundle_json(&raw)?;
        let restored = restore_setup_bundle(&paths, &bundle)?;
        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "restored": restored.summary_counts(),
                    "profiles_count": restored.profiles.profiles.len(),
                    "custom_tweaks_count": restored.custom_tweaks.tweaks.len(),
                }))?
            );
        } else {
            println!(
                "Restored setup backup from {} ({}).",
                src_path.display(),
                restored.summary_counts()
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    if cli.list_profiles {
        let store = ProfileStore::load_or_default(&paths.profiles_path())?;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&store)?);
        } else {
            println!("Saved Profiles ({}):", store.profiles.len());
            for prof in &store.profiles {
                let tag = if prof.is_desktop_default {
                    " [default]"
                } else {
                    ""
                };
                let exes = if prof.executables.is_empty() {
                    "(fallback)".to_string()
                } else {
                    prof.executables.join(", ")
                };
                let mut extras = Vec::new();
                if let Some(slot) = prof.wooting_profile {
                    extras.push(format!("Wooting P{}", slot + 1));
                }
                if let Some((_, w, h, hz)) = prof.display_mode_override() {
                    extras.push(format!("{w}x{h}@{hz}Hz"));
                }
                if let Some(plan) = prof.windows_power_plan {
                    extras.push(plan.label().to_string());
                }
                let extra_str = if extras.is_empty() {
                    String::new()
                } else {
                    format!(" | {}", extras.join(", "))
                };
                println!(
                    "  - {} ({}){} — {} | {}{}",
                    prof.name,
                    prof.id,
                    tag,
                    prof.mouse.summary_string(),
                    exes,
                    extra_str
                );
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(query) = cli.apply_profile {
        let store = ProfileStore::load_or_default(&paths.profiles_path())?;
        let needle = query.trim();
        let profile = store
            .profiles
            .iter()
            .find(|p| p.id.eq_ignore_ascii_case(needle) || p.name.eq_ignore_ascii_case(needle))
            .cloned()
            .ok_or_else(|| {
                PolledError::InvalidProfile(format!(
                    "No saved profile matches '{needle}'. Run `polled --list-profiles` to see available IDs."
                ))
            })?;

        let env = WindowsEnv;
        let mut actions: Vec<String> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();

        if let Some(speed) = profile.windows_pointer_speed {
            match env.set_mouse_speed(speed) {
                Ok(()) => actions.push(format!("Windows pointer speed -> {speed}/20")),
                Err(e) => warnings.push(format!("Windows pointer speed failed: {e}")),
            }
        }

        if let Some(preset) = profile.windows_power_plan {
            if let Ok(schemes) = env.list_power_schemes() {
                if let Some(target) = preset.resolve_scheme(&schemes) {
                    match env.set_active_power_scheme(&target.guid) {
                        Ok(()) => actions.push(format!("Power plan -> {}", target.name)),
                        Err(e) => warnings.push(format!("Power plan failed: {e}")),
                    }
                }
            }
        }

        if let Some(out_id) = profile.audio_output_device_id.as_deref() {
            match env.set_default_audio_endpoint(out_id) {
                Ok(()) => actions.push(format!(
                    "Audio output -> {}",
                    profile
                        .audio_output_device_name
                        .as_deref()
                        .unwrap_or(out_id)
                )),
                Err(e) => warnings.push(format!("Audio output switch failed: {e}")),
            }
        }
        if let Some(vol) = profile.audio_output_volume {
            match env.set_audio_endpoint_volume(
                AudioFlow::Render,
                profile.audio_output_device_id.as_deref(),
                vol,
            ) {
                Ok(()) => actions.push(format!("Output volume -> {vol}%")),
                Err(e) => warnings.push(format!("Output volume failed: {e}")),
            }
        }
        if let Some(in_id) = profile.audio_input_device_id.as_deref() {
            match env.set_default_audio_endpoint(in_id) {
                Ok(()) => actions.push(format!(
                    "Microphone -> {}",
                    profile.audio_input_device_name.as_deref().unwrap_or(in_id)
                )),
                Err(e) => warnings.push(format!("Microphone switch failed: {e}")),
            }
        }
        if let Some(vol) = profile.audio_input_volume {
            match env.set_audio_endpoint_volume(
                AudioFlow::Capture,
                profile.audio_input_device_id.as_deref(),
                vol,
            ) {
                Ok(()) => actions.push(format!("Microphone volume -> {vol}%")),
                Err(e) => warnings.push(format!("Microphone volume failed: {e}")),
            }
        }

        if let Some(slot) = profile.wooting_profile {
            match detect_wooting_keyboards() {
                Ok(boards) if boards.is_empty() => {
                    warnings.push("No connected Wooting keyboard detected.".to_string());
                }
                Ok(boards) => {
                    for kb in boards {
                        let info = kb.info(None);
                        match kb.switch_profile(slot) {
                            Ok(true) => actions.push(format!(
                                "{} -> {}",
                                info.name,
                                WootingKeyboardInfo::profile_label(slot)
                            )),
                            Ok(false) => actions.push(format!(
                                "{} already on {}",
                                info.name,
                                WootingKeyboardInfo::profile_label(slot)
                            )),
                            Err(e) => warnings.push(format!("{} failed: {e}", info.name)),
                        }
                    }
                }
                Err(e) => warnings.push(format!("Wooting detection failed: {e}")),
            }
        }

        match detect_hardware_devices() {
            Ok(devices) if devices.is_empty() => {
                warnings.push("No supported hardware mouse detected.".to_string());
            }
            Ok(devices) => {
                for dev in devices {
                    let info = dev.info();
                    let caps = dev.capabilities();
                    let (raw_desired, _) = profile.settings_for_device(&info.id);
                    let (adapted, _) = caps.adapt_settings(raw_desired);
                    let mut ctrl = SafeDeviceController::new(dev);
                    match ctrl.apply_profile_settings(&adapted) {
                        Ok(ApplyOutcome::Applied { mode, verified, .. }) => actions.push(format!(
                            "{} -> {} ({mode:?})",
                            info.name,
                            verified.summary_string()
                        )),
                        Ok(ApplyOutcome::SkippedUnchanged { current }) => actions.push(format!(
                            "{} already matches ({})",
                            info.name,
                            current.summary_string()
                        )),
                        Ok(ApplyOutcome::SkippedNoSettings) => {}
                        Err(e) => warnings.push(format!("{} failed: {e}", info.name)),
                    }
                }
            }
            Err(e) => warnings.push(format!("Mouse detection failed: {e}")),
        }

        if let Some((_, w, h, hz)) = profile.display_mode_override() {
            warnings.push(format!(
                "Temporary display switch ({w}x{h} @ {hz} Hz) is managed by the Polled GUI runtime so it safely auto-reverts on exit."
            ));
        }

        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "profile_id": profile.id,
                    "profile_name": profile.name,
                    "actions": actions,
                    "warnings": warnings,
                }))?
            );
        } else {
            println!("Applied profile '{}' ({}):", profile.name, profile.id);
            for a in &actions {
                println!("  [ OK ] {a}");
            }
            for w in &warnings {
                println!("  [WARN] {w}");
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    let env = WindowsEnv;
    let engine = HealthEngine::with_default_and_custom_checks(&paths);

    if let Some(id) = cli.fix {
        let report = engine.fix_check(&id, &env, &paths)?;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!("Fixed '{}' ({}):", report.id, report.name);
            print_report(&report);
            println!(
                "Baseline snapshot saved at: {}",
                paths.baseline_snapshot_path().display()
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(id) = cli.undo {
        let report = engine.undo_check(&id, &env, &paths)?;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!("Restored '{}' ({}):", report.id, report.name);
            print_report(&report);
        }
        return Ok(ExitCode::SUCCESS);
    }

    if cli.undo_all {
        let summary = engine.undo_all(&env, &paths)?;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&summary)?);
        } else {
            println!(
                "Restored {} setting(s): {}",
                summary.restored_ids.len(),
                if summary.restored_ids.is_empty() {
                    "(none)".to_string()
                } else {
                    summary.restored_ids.join(", ")
                }
            );
            for failure in &summary.failed {
                eprintln!("  Failed '{}': {}", failure.check_id, failure.error);
            }
        }
        return Ok(if summary.failed.is_empty() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    if cli.scan {
        let summary = engine.scan_all(&env, &paths, !cli.no_state_update)?;
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&summary)?);
        } else {
            println!(
                "Polled Input Health Check (Windows {})",
                summary.windows_version
            );
            if summary.windows_build_changed {
                if let Some(prev) = &summary.previous_windows_version {
                    println!(
                        "Note: Windows build changed since last scan ({prev} -> {}).",
                        summary.windows_version
                    );
                }
            }
            println!();
            for check in &summary.checks {
                print_report(check);
            }
            if !summary.regressions.is_empty() {
                println!("Regressions detected since previous scan:");
                for reg in &summary.regressions {
                    println!(
                        "  - {} ({}): {:?} -> {:?} ({})",
                        reg.name, reg.check_id, reg.previous, reg.current, reg.current_value
                    );
                }
            }
            if summary.has_undo_snapshot {
                println!(
                    "Undo snapshot available for: {}",
                    summary.undoable_check_ids.join(", ")
                );
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    eprintln!("No action specified. Run `polled --help` for usage (e.g. `polled --scan` or `polled --dump`).");
    Ok(ExitCode::from(2))
}

fn print_report(r: &CheckReport) {
    let badge = match r.severity {
        Severity::Ok => "[ OK ]",
        Severity::Info => "[INFO]",
        Severity::Warn => "[WARN]",
        Severity::Bad => "[BAD ]",
    };
    let action_hint = if !r.can_fix {
        " (report only)"
    } else if r.is_optional_tweak && r.severity != Severity::Ok {
        if r.requires_elevation {
            " (optional tweak, admin)"
        } else {
            " (optional tweak)"
        }
    } else if r.severity >= Severity::Warn && r.requires_elevation {
        " (fixable, admin)"
    } else if r.severity >= Severity::Warn {
        " (fixable)"
    } else {
        ""
    };
    println!("{} {} ({}){}", badge, r.name, r.id, action_hint);
    println!("       Current:     {}", r.current_value);
    println!("       Recommended: {}", r.recommended_value);
    println!("       Reason:      {}", r.reason);
    if r.requires_relogin && r.severity >= Severity::Warn {
        println!("       Note:        Requires sign-out/sign-in to take effect.");
    }
    if r.requires_reboot && r.can_fix {
        println!("       Note:        Requires reboot to take effect.");
    }
    println!();
}

fn print_audio_endpoint(ep: &AudioEndpointInfo) {
    let marker = if ep.is_default { "[*]" } else { "[ ]" };
    let hfp = if ep.is_bluetooth_hands_free {
        " [BT Hands-Free HFP]"
    } else {
        ""
    };
    println!("  {} {}{}", marker, ep.name, hfp);
    let vol = match (ep.volume_percent, ep.muted) {
        (Some(v), Some(true)) => format!("{v}% (MUTED)"),
        (Some(v), _) => format!("{v}%"),
        _ => "n/a".to_string(),
    };
    let fmt = match (ep.sample_rate_hz, ep.channels, ep.bits_per_sample) {
        (Some(hz), Some(ch), Some(bits)) => format!("{hz} Hz · {ch}ch · {bits}-bit"),
        (Some(hz), Some(ch), None) => format!("{hz} Hz · {ch}ch"),
        _ => "n/a".to_string(),
    };
    let period = match (ep.default_period_ms, ep.min_period_ms) {
        (Some(def), Some(min)) => format!("{def:.2} ms (min {min:.2} ms)"),
        (Some(def), None) => format!("{def:.2} ms"),
        _ => "n/a".to_string(),
    };
    let apo = match ep.enhancements_enabled {
        Some(true) => "Enabled",
        Some(false) => "Disabled",
        None => "None/Unknown",
    };
    println!("      Volume: {vol} | Format: {fmt} | Period: {period} | APO: {apo}");
}

fn print_network_adapter(nic: &NetworkAdapterInfo) {
    let marker = if nic.is_primary { "[*]" } else { "[ ]" };
    let speed = if nic.link_speed_mbps > 0 {
        format!("{} Mbps", nic.link_speed_mbps)
    } else {
        "n/a".to_string()
    };
    println!(
        "  {} {} — {} [{} · {}]",
        marker,
        nic.name,
        nic.description,
        nic.kind.label(),
        speed
    );
    let ip = nic.ipv4_address.as_deref().unwrap_or("n/a");
    let gw = nic.gateway_ipv4.as_deref().unwrap_or("n/a");
    let dns = if nic.dns_servers.is_empty() {
        "none".to_string()
    } else {
        nic.dns_servers.join(", ")
    };
    let flags = if nic.latency_flags.is_empty() {
        "Clean".to_string()
    } else {
        nic.latency_flags.join(", ")
    };
    println!("      IPv4: {ip} | Gateway: {gw} | DNS: {dns}");
    println!("      NIC Latency Flags: {flags}");
}
