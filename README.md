# Polled

Open-source, lightweight Windows 10/11 utility for competitive mouse input hygiene and per-game peripheral profiles.

- **Small & fast:** Native Rust core with a Tauri v2 + React tray app. Closing the window destroys the WebView2 instance so idle tray memory stays low.
- **No account, no telemetry, no cloud:** 100% local. Data lives in `%LOCALAPPDATA%\Polled\`.
- **Safe & reversible by design:** Before applying any fix, Polled atomically saves the original Windows values to `baseline_snapshot.json` (first-write-wins). One click or `polled --undo-all` restores everything.
- **No persistent admin rights:** Runs non-elevated. Only requests UAC elevation for the specific check that needs HKLM/power-policy access, explaining why first.
- **Anti-cheat friendly:** No DLL injection, no hooks into game processes, no reading game memory. Works alongside Vanguard, FACEIT, EAC, and BattlEye.
- **License:** GNU GPL-3.0-or-later.

---

## Features

### Phase 1 — Input Settings Health Check *(Implemented)*

| Check ID | Setting | Auto-Fix | Admin Needed |
| --- | --- | :---: | :---: |
| `enhance_pointer_precision` | Enhance pointer precision (`SPI_SETMOUSE` + `HKCU\Control Panel\Mouse`) | Yes | No |
| `pointer_speed` | Windows pointer speed (`MouseSensitivity = 10`, i.e. 6/11 1.0x) | Yes | No |
| `custom_accel_curves` | Custom `SmoothMouseXCurve` / `SmoothMouseYCurve` registry curves | Yes *(sign-out required)* | No |
| `usb_selective_suspend` | USB selective suspend in the active power plan (`powrprof.dll`) | Yes | Yes (on-demand UAC) |
| `power_plan` | Active Windows power plan (with Modern Standby S0ix awareness) | Yes | No |
| `game_mode` | Windows Game Mode (`HKCU\Software\Microsoft\GameBar`) | Yes | No |
| `hags` | Hardware-accelerated GPU scheduling (`HwSchMode`) | Report only | No |
| `sticky_keys_shortcut` | Sticky Keys shortcut (5× Shift) | Yes | No |
| `filter_keys_shortcut` | Filter Keys shortcut (hold Right Shift 8 s) | Yes | No |
| `mouse_keys_shortcut` | Mouse Keys shortcut (Left Alt + Left Shift + NumLock) | Yes | No |
| `vendor_software` | Running peripheral suites (G Hub, Synapse, iCUE, SteelSeries GG, Armoury Crate) CPU & RAM footprint | Report only | No |

Additional Phase 1 behavior:
- **Baseline undo snapshot:** `%LOCALAPPDATA%\Polled\baseline_snapshot.json` records true original pre-Polled values before any mutation.
- **Update & regression alerts:** Re-scans on startup and when the Windows build/UBR changes (`CurrentBuildNumber.UBR`), notifying **only** if a setting got worse.

### Phase 2 — Per-Game Peripheral Profiles *(Upcoming)*

- Out-of-context foreground window detection (`SetWinEventHook EVENT_SYSTEM_FOREGROUND`) with 1–2 s focus debounce.
- Versioned, exportable/importable JSON profiles (DPI, polling rate, LOD, debounce, optional Windows pointer speed) with automatic desktop fallback.
- Flash-wear protection (volatile RAM writes when supported; diff-before-write + rate limiting + warnings on flash-only devices) and mandatory read-back verification with automatic rollback.
- `--dump` HID interface & capability dumper for community device submissions.

---

## CLI Usage

```powershell
# Scan all 11 input health checks (human-readable)
polled --scan

# Machine-readable JSON scan output
polled --scan --json

# Fix a single check (saves baseline snapshot first)
polled --fix mouse_keys_shortcut

# Undo a single check from the saved snapshot
polled --undo mouse_keys_shortcut

# Restore every setting changed by Polled
polled --undo-all
```

Exit codes:
- `0`: Success
- `1`: General error
- `2`: No action specified
- `5`: Administrator elevation required for the requested fix

---

## Supported Devices *(Phase 2)*

Every hardware backend in Polled is explicitly labeled by verification status:

| Device | Support Status | Volatile RAM Writes | Notes |
| --- | --- | :---: | --- |
| Dry-Run Virtual Mouse | Built-in simulator *(Phase 2)* | Yes | Logs operations for hardware-free testing |
| Pulsar X2 Crazylight Mini | Planned — *Tested on hardware* *(Phase 2)* | TBD (pending `--dump` / capture) | First real hardware target |

> **Protocol policy:** Polled never guesses HID report bytes. Backends are implemented strictly from verified open-source protocol documentation (with source & license attribution) or real USBPcap/Wireshark captures.

---

## Building from Source (Windows 10/11)

Prerequisites:
- Rust stable toolchain (`rustup`)
- Node.js 20+ (for the React frontend)
- Microsoft Visual Studio C++ Build Tools & WebView2 Runtime (preinstalled on Windows 11)

```powershell
# 1. Build the React UI
cd apps/polled-desktop
npm ci
npm run build
cd ../..

# 2. Run all checks & unit tests
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# 3. Build release binaries (CLI + tray app)
cargo build --release -p polled-cli -p polled-desktop
```

Release binaries are written to `target/release/polled.exe` and `target/release/polled-desktop.exe`. Keep both files in the same folder so the tray app can invoke `polled.exe` on demand when a single fix needs UAC elevation.

---

## Project Layout

```text
crates/
  polled-core/         Pure Rust library (Windows API traits, 11 checks, snapshots, state)
  polled-cli/          Standalone console binary (polled.exe) & on-demand UAC helper
apps/
  polled-desktop/      Tauri v2 + React + TypeScript system tray app
```

## License

Polled is free software licensed under the [GNU General Public License v3.0 or later](LICENSE).
