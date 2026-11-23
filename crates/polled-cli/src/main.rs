// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use polled_core::win::WindowsEnv;
use polled_core::{AppPaths, CheckReport, HealthEngine, PolledError, Severity};

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

    /// Output results as machine-readable JSON (use with --scan, --fix, or --undo-all)
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
                eprintln!("Re-run this specific fix from an elevated prompt or click 'Fix (Admin)' in Polled.");
                return ExitCode::from(5);
            }
            eprintln!("Error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> polled_core::Result<ExitCode> {
    let paths = match &cli.data_dir {
        Some(dir) => AppPaths::from_root(dir),
        None => AppPaths::default_user_dir(),
    };
    let env = WindowsEnv;
    let engine = HealthEngine::with_default_checks();

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

    eprintln!("No action specified. Run `polled --help` for usage (e.g. `polled --scan`).");
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
    println!();
}
