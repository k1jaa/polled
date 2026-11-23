# Polled

Open-source, lightweight pure-Rust Windows 10/11 utility for competitive mouse input hygiene and per-game peripheral profiles.

- **Small & fast:** 100% pure Rust (`eframe`/`egui` + `hidapi` + `windows-rs`) — no WebView2, no Electron, no Node.js runtime, monochrome Dark/Light UI, single-instance system tray app with optional minimized Windows startup (`HKCU\...\Run`, no admin needed).
- **No account, no telemetry, no cloud:** Fully local. Data lives in `%LOCALAPPDATA%\Polled\` (`baseline_snapshot.json`, `state.json`, `profiles.json`, `settings.json`).
- **Safe & reversible by design:** Before applying any Windows fix, Polled atomically saves original values to `baseline_snapshot.json` (first-write-wins). One click or `polled --undo-all` restores everything. Nothing touches device firmware.
- **No persistent admin rights:** Runs non-elevated. Only requests UAC elevation for the specific fix that needs HKLM/power-policy access, explaining why first.
- **Anti-cheat friendly:** No DLL injection, no hooks into game processes, no reading game memory. Per-game detection uses an out-of-context `SetWinEventHook` (`EVENT_SYSTEM_FOREGROUND`) + `PROCESS_QUERY_LIMITED_INFORMATION`. Works alongside Vanguard, FACEIT, EAC, and BattlEye.
- **License:** GNU GPL-3.0-or-later.

---

## Features

### Phase 1 — Input Settings Health Check

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

- **Baseline undo snapshot:** `%LOCALAPPDATA%\Polled\baseline_snapshot.json` records true original pre-Polled values before any mutation.
- **Update & regression alerts:** Re-scans on startup and when `CurrentBuildNumber.UBR` changes, notifying **only** if a setting got worse.

### Phase 2 — Per-Game Peripheral Profiles

- **Event-driven game detection:** Uses `SetWinEventHook` (`EVENT_SYSTEM_FOREGROUND`, `WINEVENT_OUTOFCONTEXT`) — zero polling loops, zero admin.
- **Focus debouncing:** Waits for the focused app to stay in the foreground (default `1500 ms`, configurable `500–5000 ms`) before switching, so alt-tab spam triggers zero device writes.
- **Per-mouse profile overrides & smart adaptation:** Each profile supports shared defaults plus optional per-mouse overrides (so Pulsar can run `8000 Hz / 0.7 mm LOD` while LAMZU runs `4000 Hz / 1.0 mm LOD`). If a shared profile requests an unsupported value on a different mouse, Polled adapts to the closest supported value and logs the adjustment instead of blocking the switch.
- **Versioned JSON profiles + export/import:** Stored in `%LOCALAPPDATA%\Polled\profiles.json` (`schema_version: 1`) with single-profile `polled.profile.v1` JSON export/import ready for shareable settings cards, plus a fallback `"Desktop (Default)"` profile.
- **Mandatory read-back verification + rollback:** After every mouse write, Polled reads settings back from the mouse. If they do not match the target, Polled automatically rolls back to the pre-write values and logs an error.
- **Dry-run simulator:** Built-in `Dry-Run Virtual Mouse` (both volatile RAM and flash-only modes) for testing profile switching without hardware.

---

## Flash-Wear Warning (Read Before Auto-Switching)

Some wireless gaming mice expose volatile RAM-only write commands, while others — including the **Pulsar X2 CrazyLight** — write `CMD_MEM_SET (0x07)` changes directly to persistent onboard memory (flash/NVRAM).

To protect your mouse's onboard flash from excessive erase/write cycles:
1. **Register-level diffing:** Polled reads the mouse first and writes **only** registers whose value actually changed (0 writes when settings already match).
2. **Focus debounce:** Quick alt-tabs cancel pending switches before any HID packet is sent.
3. **Sliding-window flash rate-limiter:** On flash-only devices (`supports_volatile_writes = false`), Polled allows normal Game ↔ Desktop bursts (up to 4 onboard flash writes per 60 s) so switching back to Desktop happens right after focus debounce, while throttling runaway loops/spam with automatic cooldown retry and a visible session flash-write counter.
4. **Recommendation:** Keep your desktop and game profiles sharing the same baseline settings where possible (only varying what you actually change per game) to minimize onboard flash writes.

---

## Supported Devices & Protocol Families

Polled organizes device backends by **protocol family** rather than single models, automatically detecting compatible mice across brands via their USB HID report descriptors and hot-plugging seamlessly when you swap mice. Every backend is labeled by verification status:

| Protocol Family / Device | VID:PID | Status | Volatile RAM Writes | Protocol Source & License |
| --- | --- | --- | :---: | --- |
| Dry-Run Virtual Mouse | *(simulated)* | Built-in simulator | Configurable | Internal test backend |
| **Compx / Nordic 17B (`0xFF02` / Report `0x08`)** — Pulsar X2 CrazyLight Mini Wireless (`cid=0x57, mid=0x01`) | `3710:5406` (8K) | **Tested on hardware** *(read, write, read-back & auto-switch)* | No (diffed + rate-limited) | [packerlschupfer/pulsar-mouse-linux](https://github.com/packerlschupfer/pulsar-mouse-linux) (MIT) |
| **Compx / Nordic 17B (`0xFF02` / Report `0x08`)** — Pulsar X2 CrazyLight (`mid=0x03`) & Wired | `3710:5406`, `3710:3414` | **Implemented from docs** | No (diffed + rate-limited) | [packerlschupfer/pulsar-mouse-linux](https://github.com/packerlschupfer/pulsar-mouse-linux) (MIT) |
| **Compx / Nordic 17B (`0xFF02` / Report `0x08`)** — LAMZU 4K Receiver | `3554:f510` | **Tested on hardware** *(read, write, read-back, hot-plug & auto-switch)* | No (diffed + rate-limited) | [LeadSun/lamzu-cfg](https://github.com/LeadSun/lamzu-cfg) (MIT/Apache-2.0), [anany1702/lamzu-ctl](https://github.com/anany1702/lamzu-ctl) (MIT) |
| **Compx / Nordic 17B (`0xFF02` / Report `0x08`)** — LAMZU 1K / Wired & Pulsar Nordic (`X2A`, `Xlite V3`, `X2V2`) | `3554:f50d`, `3554:f50f`, `3554:f507..f509` | **Implemented from docs** | No (diffed + rate-limited) | [LeadSun/lamzu-cfg](https://github.com/LeadSun/lamzu-cfg) (MIT/Apache-2.0), [anany1702/lamzu-ctl](https://github.com/anany1702/lamzu-ctl) (MIT) |
| **Compx / Nordic 17B (`0xFF02` / Report `0x08`)** — Generic Compx `0x3554:*` auto-detected mice | `3554:*` (`0xFF02` 17B) | **Implemented from docs** | No (diffed + rate-limited) | Shared Compx CX52850 protocol |
| **Logitech HID++ 2.0 (`0xFF00` / Report `0x11`)** — G Pro X Superlight 1/2, G305, G502, G703, G403, G903, MX *(wired & Lightspeed/Unifying/Bolt)* | `046d:*` (`0xFF00` 20B) | **Implemented from docs** | **Yes (Volatile RAM)** | [libratbag/libratbag](https://github.com/libratbag/libratbag) (MIT), [pwr-Solaar/Solaar](https://github.com/pwr-Solaar/Solaar) (GPL-2.0-or-later) |
| Vancer × Arbiter Studio Akitsu (`0xFFFF` 64B Feature) | `373e:000a` | Planned *(pending USBPcap capture for DPI/LOD/debounce)* | TBD | Not guessed |

> **Protocol policy:** Polled never guesses HID report bytes. Backends are implemented strictly from documented, license-compatible open-source protocols or real USBPcap/Wireshark captures.

### Submitting Data for an Unsupported Mouse

Run `--dump` (serial numbers are redacted automatically by default) and attach the JSON output to a GitHub issue:

```powershell
# Human-readable list of HID interfaces and usage pages
polled --dump

# Filter by Vendor ID (hex) and output JSON
polled --dump --vid 3710 --json
```

---

## CLI Usage

```powershell
# Phase 1: Health checks
polled --scan
polled --scan --json
polled --fix mouse_keys_shortcut
polled --undo mouse_keys_shortcut
polled --undo-all

# Phase 2: HID discovery & hardware verification (read-only)
polled --dump
polled --dump --vid 3710 --json
polled --detect
polled --detect --json
```

Exit codes:
- `0`: Success
- `1`: General error
- `2`: No action specified
- `5`: Administrator elevation required for the requested fix

---

## Building from Source (Windows 10/11)

Prerequisites:
- Rust stable toolchain (`rustup`)
- Microsoft Visual Studio C++ Build Tools

```powershell
# Format, lint & run all unit tests
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Run the GUI tray app directly
cargo run -p polled-gui

# Or build optimized release binaries (CLI + GUI tray app)
cargo build --release -p polled-cli -p polled-gui
```

Release binaries are written to `target/release/polled.exe` and `target/release/polled-gui.exe`. Keep both files in the same folder so the tray app can invoke `polled.exe` on demand when a single fix needs UAC elevation.

---

## Project Layout

```text
crates/
  polled-core/         Pure Rust core (Windows API traits, 11 checks, snapshots,
                       HID --dump, MouseDevice trait, SafeDeviceController,
                       Compx/Nordic & Logitech HID++ 2.0 backends, settings,
                       profiles, debouncer, watcher)
  polled-cli/          Standalone console binary (polled.exe) & on-demand UAC helper
  polled-gui/          Pure-Rust monochrome Dark/Light GUI & tray app (eframe/egui)
```

## License

Polled is free software licensed under the [GNU General Public License v3.0 or later](LICENSE).
