# Polled

**A lightweight, open-source Windows 10/11 tray utility for competitive input health checks, per-game mouse & keyboard profiles, and 1K–8K hardware benchmarking.**

Pure Rust (~3 MB idle tray RAM with zero GPU/OpenGL memory while minimized, no Electron/WebView2, no account, no telemetry, zero background network calls, and **100% anti-cheat safe** — standard Win32 user-mode APIs only, no kernel drivers or game hooks).

> [!WARNING]
> **Experimental (`0.x`):** Polled modifies Windows settings and sends USB HID configuration commands to supported peripherals. Every Windows change is snapshotted first for one-click rollback, and unverified (`Implemented from docs`) devices require explicit confirmation before writing. Provided **"AS IS"** under [AGPL-3.0-or-later](LICENSE).

---

## What It Does

Polled is split into four clean workspaces:

- **Health** — Audits 41 Windows input, USB/MSI interrupt, audio, and network latency settings, plus 13 opt-in reversible low-latency tweaks (CPU core parking, power throttling, keyboard repeat, PCI GPU/Audio MSI mode, etc.) and custom tweaks. One-click **Undo** / **Undo All** restores your exact original values.
- **Profiles** — 1-click installed game discovery (Steam, Riot, Epic, Battle.net, Ubisoft, EA App) that automatically switches mouse settings (DPI, X/Y DPI, stage lock, polling rate, LOD, debounce, sensor toggles, sleep, low-battery polling guard), keyboard onboard slots and per-game presets (polling rate, actuation, Rapid Trigger, SOCD, debounce), temporary per-game display resolution/Hz, Windows power plan, default audio/mic devices (with pinned Headphones ↔ Speakers cycling & global mic-mute hotkey), mic gain lock, background app mixer ducking, and quiet notifications when a game launches.
- **Keyboard** — Dedicated multi-brand keyboard hub. Hardware-tested deep control for **Wooting** (interactive 60%/80HE/ISO keymap, live analog depth bars, `A <-> D` counter-strafe timing, per-key actuation, Rapid Trigger, SOCD/AKC, RAM-only profile switching) plus open-source support for **9 additional keyboard ecosystems**.
- **Bench** — Live 1K–8K mouse polling & dropout analyzer, OS scheduler & per-core DPC/IRQ spike monitor (user-mode Mini LatencyMon), raw-input click reaction test, DPI/CPI calibrator, click bounce & scroll encoder tester, passive match watchdog, USB hub topology, silent WASAPI audio latency, dual-hop ping jitter, persistent history, and shareable **Rig Scorecard PNG** exports.

---

## Install & Quick Start

1. Download `polled-v*-windows-x64.zip` (and `SHA256SUMS.txt`) from [**Releases**](https://github.com/k1jaa/polled/releases).
2. Extract anywhere (keep `polled-gui.exe` and `polled.exe` in the same folder) and run **`polled-gui.exe`**.
3. Run a **Health** scan, configure a game in **Profiles** (turn **Auto-Switch: ON**), and close `[X]` to free all GPU/UI memory while Polled's ~3 MB pure-Win32 tray daemon keeps running quietly in the background.

> **Uninstall:** Click **Undo All** in Health if you want to revert Windows tweaks, turn **Startup** off, right-click the tray icon → **Quit**, and delete the folder. Local data lives in `%LOCALAPPDATA%\Polled\`.

---

## Supported Hardware

Only devices marked **Tested on hardware** have been verified physically end-to-end; all others are **Implemented from docs** with confirm-gated, read-back-verified writes and zero destructive opcodes.

- **Mice — Tested on hardware:** Pulsar X2 CrazyLight Mini Wireless, LAMZU 4K Receiver.
- **Mice — Implemented from docs:** Pulsar (CrazyLight / Nordic), LAMZU (1K / Maya X / Inca / Paro), Razer (80+ models incl. Viper V4/V3/V2 Pro & DeathAdder V4/V3), Logitech HID++ 2.0 (Superlight 1/2, G305–G903), WLMouse (Beast X / Sword X / Strider), Endgame Gear (OP1/XM2 8K & 4K), Finalmouse (ULX / SLX), VAXEE, Ninjutso (Sora / TEN), MCHOSE, Incott, Keychron, VGN / ATK / VXE / Scyrox / G-Wolves / Hitscan / Attack Shark / Vancer / RAWM.
- **Keyboards — Tested on hardware:** Wooting (60HE, **60HE+**, 60HE v2, 80HE, Two HE, UwU, One, Two).
- **Keyboards — Implemented from docs:** Wuque Studio / SparkLink HE (BABAO60/80, ZOOM TIGA), Keychron Hall Effect / TMR (160+ Q/K/V/J/C HE & Lemokey models), SteelSeries Apex Pro TKL (2023 & Gen 3), Razer Keyboards (Huntsman V3 Pro, BlackWidow V4, DeathStalker V2), Arbiter Studio (Polar 65/75 PRO, Nimbus 65), Logitech HID++ 2.0 Keyboards (PRO X TKL RAPID, PRO X 60, G915 X, G715, G515), Kochi Hummingbird 68, Geonworks Venom 8K (60MX / 80MX / WISH45-8K), and Universal QMK / VIA (`0xFF60:0x61`).

See [**docs/COMPATIBILITY.md**](docs/COMPATIBILITY.md) for exact VID:PIDs, per-device capabilities, closed-firmware notes (e.g. Geonworks Venom HE RISC-V, Tekkusai Titan 75 HE), and vendor suite caveats.

**Want your mouse or keyboard added or verified?** Click **Report Mouse** or **Report Keyboard** inside the app (or run `.\polled.exe --dump`) — serial numbers are automatically redacted — and open a GitHub issue.

---

## CLI Usage

Every flag also supports `--json`:

```powershell
.\polled.exe --scan                 # Audit all 41 Windows input/audio/network checks
.\polled.exe --detect               # Read-only connected mouse detection & settings
.\polled.exe --keyboards            # Read-only multi-brand keyboard detection
.\polled.exe --wooting              # Detailed Wooting telemetry snapshot
.\polled.exe --apply-profile <ID>   # Apply a saved game profile from the terminal
.\polled.exe --ping 1.1.1.1         # 5-second dual-hop router vs. internet jitter probe
.\polled.exe --dump                 # Serial-redacted HID report for device requests
```

---

## Build from Source

Requires [Rust](https://rustup.rs/) and Visual Studio C++ Build Tools on Windows 10/11:

```powershell
cargo build --release -p polled-cli -p polled-gui
```

Outputs `target\release\polled.exe` and `target\release\polled-gui.exe`.

---

## Documentation & License

- [**System Modifications & Rollback Guide**](docs/SYSTEM_MODIFICATIONS.md) — exact registry keys, power GUIDs, Win32 APIs, and USB HID writes.
- [**Hardware Compatibility & Known Issues**](docs/COMPATIBILITY.md) — full mouse & keyboard matrices and troubleshooting.
- [**Changelog**](CHANGELOG.md) — release history and known limitations.

Licensed under [AGPL-3.0-or-later](LICENSE). Open-source protocol codecs and catalogs incorporate work from [OpenMouse-Project](https://github.com/OpenMouse-Project) (AGPL-3.0-or-later), [OpenRazer](https://github.com/openrazer/openrazer) (GPL-2.0-or-later), [libratbag](https://github.com/libratbag/libratbag) (MIT), [Solaar](https://github.com/pwr-Solaar/Solaar) (GPL-2.0-or-later), [WootingKb/wooting-rgb-sdk](https://github.com/WootingKb/wooting-rgb-sdk) (MPL-2.0), [clemenscodes/wootswitch](https://github.com/clemenscodes/wootswitch) (MIT), [chcbaram/baram-qmk-8k](https://github.com/chcbaram/baram-qmk-8k), and community CompX/LAMZU/Pulsar drivers.
