// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use polled_core::win::WindowsEnv;
use polled_core::{
    detect_hardware_devices, dump_hid_devices, AppPaths, CheckReport, HealthEngine, PolledError,
    Severity,
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

    /// Optional hex Vendor ID filter for --dump (e.g. 3710 or 0x3710 for Pulsar)
    #[arg(long, value_name = "HEX_VID")]
    vid: Option<String>,

    /// Include raw hardware serial numbers in --dump output (redacted by default)
    #[arg(long)]
    include_serials: bool,

    /// Output results as machine-readable JSON (use with --scan, --dump, --fix, or --undo-all)
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
                    serde_json::json!({
                        "info": info,
                        "capabilities": caps,
                        "current_settings": settings,
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
