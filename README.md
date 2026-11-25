# Polled

> [!WARNING]
> **Experimental Software (`0.x` Early Development):** Polled is under active early development. It modifies Windows registry and power settings and sends raw USB HID configuration commands to supported mice. Although Polled snapshots your original Windows values before making changes and validates/verifies mouse writes, **use at your own risk** and consider creating a Windows System Restore Point before applying system-wide (`HKLM`) or custom tweaks. Provided **"AS IS"** without warranty under the [GNU Affero General Public License v3.0](LICENSE) (Sections 15–16).

A lightweight, pure-Rust Windows 10/11 tray utility and CLI organized into four focused workspaces (**Health**, **Profiles**, **Keyboard**, **Bench**):

1. **Input, Audio & Network Health Check + 13 Reversible Low-Latency Tweaks** — audits **41** Windows input, audio, network & latency settings (pointer acceleration, pointer speed, custom acceleration curves, hardware cursor, mouse queue depth, USB selective suspend / hub power / xHCI MSI / **reversible PCI GPU & HD Audio MSI interrupt fixer** / **true PnP `cfgmgr32.dll` USB port & hub topology**, PCIe ASPM, CPU max state, power plan, **Windows communications volume ducking**, **APO audio enhancements**, **WASAPI format / period / Bluetooth HFP detection**, **Ethernet vs Wi-Fi & link speed**, **NIC power saving / EEE / Interrupt Moderation**, **DNS resolver configuration**, timer wake jitter, monitor refresh rate, Win11 Flip-Model windowed optimizations, Game Mode, GameDVR, VBS/HVCI, accessibility shortcuts, and background vendor suites) plus **13 curated opt-in reversible tweaks** (fast keyboard repeat rate/delay, DWM transparency off, CPU Core Parking off on AC, Power Throttling off, Fast Startup off, Windows Update P2P seeding off, primary NIC Nagle's algorithm off, HAGS toggle, MPO troubleshooting toggle, MMCSS gaming priority, `Win32PrioritySeparation`, `GlobalTimerResolutionRequests`, and **PCI GPU & HD Audio MSI mode**) and user-defined Custom Tweaks — all backed by pre-change snapshots and one-click rollback.
2. **Per-Game Mouse, Keyboard, Display & Audio Profiles + Monochrome OSD & Live Tray Controls** — automatically switches DPI (**including independent X/Y DPI, onboard DPI Stage Slot, and Lock DPI Button across all stages**), polling rate, lift-off distance, debounce, sensor toggles (Motion Sync, Angle Snapping, Ripple Control, **Endgame Gear Glass Pad Mode**), **wireless idle sleep timeout**, **wireless mouse wake-up auto-retry** (automatically re-applies a game profile the moment a sleeping wireless mouse wakes), **opt-in Low-Battery Polling Guard** (automatically steps 2K/4K/8K wireless mice to `1000 Hz` at low battery and restores on charge), **Wooting Analog/HE Keyboard onboard profiles (with real firmware slot names, volatile RAM switch, zero flash wear)** on a dedicated **Profiles → Keyboard** sub-tab, **temporary per-game Display Resolution & Refresh Rate overrides** (e.g. `1280×960 @ 240 Hz` stretched with zero registry writes and a **10-second auto-revert test guard**, automatically restoring native desktop mode on tab-out/exit), **per-game High-Performance GPU preference** (`HKCU\...\UserGpuPreferences`) and **reversible Quiet Notifications** (`ToastEnabled = 0`, auto-restored on desktop/exit), Windows pointer speed, optional Windows power plan, **per-game default Playback & Microphone devices, volumes, and 2-second Mic Gain Lock**, and **Core Audio Background App Mixer Ducking** (lowers or mutes apps like Spotify/Chrome in-game and restores their exact sliders on desktop/exit). Includes an optional **click-through monochrome Win32 On-Screen Switch Pill (OSD, zero game hooks)**, a **Smart Game Picker** (installed Steam / Riot / Epic games & running apps), **Read from Mouse** live snapshot into the profile editor, one-click **Report Mouse** (serial-redacted HID + USB topology issue generator), **Full Setup Backup & Restore (`polled.setup.v1`)**, **dynamic live battery % tray icon** with configurable low-battery & profile-switch notifications, **tray quick-switch for Profiles, Mouse DPI / Polling Rate, Wooting Keyboard Profiles, and Audio Output** + **Lock Profile**, instant **`WM_DEVICECHANGE` USB hot-plug detection**, optional **global hotkeys** (`RegisterHotKey`, including **Cycle Wooting Profile**), and **strictly on-demand GitHub Releases update checker** (zero background network calls).
3. **Dedicated Multi-Brand Keyboard Hub (10 Brands + Universal QMK / VIA)** — standalone **Keyboard** tab in the left rail supporting **Wooting** (hardware-verified on 60HE+ across 13 models: firmware, layout, real profile names `0x37`, actuation/RT/Tachyon `0x27`, interactive multi-layout 60%/80HE/ISO visual keymap with click-to-edit per-key actuation `OUT 0x15` and hover-linked SOCD/AKC pairs, live `0xFF53`/`0xFF54` analog visualizer + `A <-> D` counter-strafe analyzer, Global Settings `OUT 0x17`, RGB core, and flash health), plus **Wuque Studio / SparkLink HE** (`BABAO60/80 HE`, `ZOOM TIGA HE`), **Keychron Hall Effect / TMR** (160+ Q/K/V/J/C HE & Lemokey models via `KC_HE v1–v5`), **SteelSeries Apex Pro TKL** (2023 / Gen 3 live hall actuation, Rapid Trigger & Rapid Tap), **Razer Keyboards** (42 standalone Huntsman V3 Pro / BlackWidow V4 models — strict keyboard PID filter, polling rate & V3 Rapid Trigger), **Arbiter Studio** (Polar 65 / 65+ / 65 PRO / 75 PRO / Nimbus 65 — travel, profiles & SuperTap), **Logitech HID++ 2.0 Keyboards** (`PRO X TKL RAPID` `0x1B08` analog switches, G915 X, G715, G515 wired/LIGHTSPEED), **Kochi Hummingbird 68**, **Geonworks Venom 8K** (`VENOM-60MX/80MX/WISH45-8K` via `baram-qmk-8k` `0xB0` + runtime debounce tuning), and **Universal QMK / VIA (`0xFF60:0x61`)**. Non-Wooting brands are tagged **Implemented from docs** with confirm-gated, read-back verified writes and zero destructive commands.
4. **Bench** — organized into 4 clean workspaces (**Polling**, **Aim & Switches**, **System & USB**, **Audio & Net**) with live high-precision diagnostics up to 8,000 Hz (timed `3s -> 10s` capture & hands-free `Esc` stop, decoupled zero-jitter Raw Input capture, **real-time Windows CPU & scheduler stall correlation** separating wireless/USB vs OS dropouts, **pure user-mode Per-Core Interrupt & DPC Monitor (`ntdll.dll` `NtQuerySystemInformation` — Mini LatencyMon with Core 0 starvation detection, zero kernel drivers)**, motion-saturated dropout vs. slow-hand separation, 29-bin interval histogram, Velocity/IPS/G & 1:1 2D XY Trace, automatic **Recommended Fixes** advisor, **Raw-Input Click Reaction & Switch Consistency Test** timed directly on the `WM_INPUT` QPC thread, **eDPI & `cm/360°` sensitivity converter**, hands-free Measured DPI/CPI calibrator, mouse switch double-click bounce tester, **Scroll Wheel Encoder Tester** detecting reverse-scroll glitches & double-notch contact bounce, **privacy-safe keyboard scan-rate & chatter tester**, **user-started Passive Match Watchdog** logging 2-second CPU/kernel/DPC, scheduler stalls, and local-router vs. ISP ping for up to 60 minutes with zero game hooks, **live PnP USB port, hub & xHCI controller topology inspector**, 1-second OS timer jitter benchmark, **1-second strictly silent WASAPI audio latency benchmark**, **5-second dual-hop local-gateway vs. internet ICMP ping jitter probe**, **Persistent Bench History (`bench_history.json`)** with one-click **Before/After A/B baseline reload**, plus one-click **All-in-One Rig Scorecard PNG v2 (`1040×760`)**, **Bench PNG**, **A/B PNG**, and **CSV** export).

- **Lightweight & local:** Pure Rust tray app (~6 MB, no WebView2/Electron, no account, no telemetry, zero background network calls — update checks & ping probes only run when you explicitly click them).
- **Transparent & reversible:** Every Windows change is documented in [docs/SYSTEM_MODIFICATIONS.md](docs/SYSTEM_MODIFICATIONS.md) and snapshotted to `%LOCALAPPDATA%\Polled\baseline_snapshot.json` before applying. Click **Undo** / **Undo All** anytime to restore original values. Never flashes device firmware.
- **No persistent admin:** Runs normally without administrator rights and only requests UAC on demand for the specific fix that needs it (via `polled.exe`).
- **Anti-cheat safe:** Uses standard read-only Win32 `SetWinEventHook` (`WINEVENT_OUTOFCONTEXT`) and Raw Input (`WM_INPUT`) — no custom kernel drivers, no DLL injection, no game hooks, no memory reading.

---

## Documentation & Transparency

- [**System Modifications & Rollback Guide**](docs/SYSTEM_MODIFICATIONS.md) — exact registry keys, `powrprof.dll` power GUIDs, Win32 APIs, local files, USB HID registers, and manual recovery steps.
- [**Hardware Compatibility & Known Issues**](docs/COMPATIBILITY.md) — verified vs. unverified (`Implemented from docs`) mice, vendor software conflicts, laptop caveats, and how to submit device reports.
- [**Changelog & Known Limitations**](CHANGELOG.md) — version history and publicly disclosed caveats/defects.

---

## Installation

1. Download the latest `polled-v*-windows-x64.zip` and `SHA256SUMS.txt` from the [**Releases**](https://github.com/k1jaa/polled/releases) page.
   - *(Recommended)* Verify the archive hash in PowerShell: `Get-FileHash .\polled-v*-windows-x64.zip -Algorithm SHA256`.
   - *(Note)* Releases are currently unsigned open-source binaries, so Windows SmartScreen may show a warning on first launch. You can inspect the source and build locally with Cargo if preferred.
2. Extract the zip to any folder you like (for example `C:\Tools\Polled\`).
   - Keep `polled-gui.exe` and `polled.exe` in the same folder.
3. Double-click **`polled-gui.exe`** to open Polled.
4. *(Optional)* Click **Startup: OFF → ON** in the top-right bar to start Polled minimized in the system tray when you sign in to Windows (`HKCU\...\Run`, no admin needed).

> **Uninstall:** Click **Undo All** first if you want to revert any Windows tweaks, turn **Startup** off (if enabled), right-click the tray icon → **Quit**, and delete the folder. All local settings and undo snapshots live in `%LOCALAPPDATA%\Polled\`.

---

## Quick Start

- **Health Check & Tweaks:** Open Polled to scan 41 Windows input, audio, network & latency settings. Click **Fix** on individual items (or **Fix Safe** for non-admin, non-optional fixes), explore the **Tweaks** filter for 13 documented opt-in low-latency tweaks (including reversible PCI GPU & HD Audio MSI mode), and click **Undo** / **Undo All** anytime. Polled also re-checks after Windows Updates and notifies you only if a setting regressed.
- **Per-Game Profiles:**
  1. Switch to the **Profiles** tab — supported mice, connected Wooting keyboards, live wireless battery percentages, displays, and active Windows audio devices are detected automatically (including USB hot-plug).
  2. Click **+ New**, pick your game from **Pick Game...** (or enter `.exe` names like `cs2.exe`), and configure DPI, DPI stage / **Lock DPI Button**, polling rate, lift-off distance, debounce, sensor toggles, idle sleep, optional **Wooting Keyboard Profile (1–4)**, optional **Display Resolution & Refresh Rate** (with a safe 10s auto-revert **Test** button), Windows power plan, and optional **Audio Output / Microphone / Volumes / Mic Gain Lock**.
  3. If you use multiple mice, pick a mouse under **Mouse Target** to save per-mouse overrides.
  4. Toggle **Auto-Switch: ON** and close `[X]` to keep Polled running quietly in the tray (right-click tray to switch profiles or audio outputs instantly, or hover for active profile & battery %). Enable the optional monochrome **OSD Switch Pill**, back up/restore via **Settings → Setup Backup**, or manually check GitHub Releases via **Settings → Check for Updates**.
- **Bench & CLI:** Switch to **Bench** and click **Start Capture** (default `3s -> 10s` auto-stop, or `Esc` anytime) to test 1K–8K polling consistency, DPI accuracy, click bounce, **scroll wheel encoder glitches**, keyboard scan-rate/chatter, **Passive Match Watchdog**, PnP USB hub topology, OS timer jitter, silent WASAPI audio engine latency, and 5-second dual-hop network ping jitter — with automatic **Recommended Fixes**, **PNG**, **A/B PNG**, and **CSV** export. From PowerShell, run `.\polled.exe --scan`, `--detect`, `--keyboards`, `--wooting`, `--list-profiles`, `--apply-profile <ID>`, `--backup`, `--restore <PATH>`, `--report-mouse`, `--audio`, `--network`, `--ping 1.1.1.1`, `--check-update`, or `--dump` (all support `--json`).

---

## Supported Mice

See [**docs/COMPATIBILITY.md**](docs/COMPATIBILITY.md) for the complete compatibility matrix, known issues, and per-device caveats.

| Mouse / Protocol Family | Verification Status | Memory Mode |
| --- | --- | --- |
| **Pulsar X2 CrazyLight Mini Wireless** (`3710:5406`) | **Tested on hardware** | Onboard flash *(diffed & rate-limited)* |
| **LAMZU 4K Receiver** (`3554:f510`) | **Tested on hardware** | Onboard flash *(diffed & rate-limited)* |
| **CompX 17B / Nordic Mice** (Pulsar X2 CrazyLight/X2A/Xlite V3, LAMZU 1K/Wired, **VGN Dragonfly F2 Master+**, **Scyrox V8**, **ATK / VXE R1 CompX**, **G-Wolves XVI:0** HTX Ultra/HTS Plus/Fenrir/Lycan/WARG, **Hitscan Hyperlight**, generic `3554:*`) | Implemented from docs *(unverified)* | Onboard flash *(diffed & rate-limited)* |
| **Razer 90B Protocol (`1532:*`, 80+ models)** (**Viper V4 Pro**, Viper V3 Pro / SE / HyperSpeed, Viper V2 Pro / Ultimate / 8KHz, DeathAdder V4 Pro / V3 Pro / V3 / V2 / Essential, Basilisk V3 Pro / V3, Cobra Pro, Naga V2 Pro, Orochi V2, Mouse Dock Pro) | Implemented from docs *(unverified)* | VARSTORE *(diffed & rate-limited)* / NOSTORE RAM on V2 |
| **CompX 64B Page Protocol** (**WLMouse** Beast X / Pro / Max / Mini / Miao / Sword X / Strider, **LAMZU** Maya X / Inca 8K / Paro Aurora, **Attack Shark** R5 Ultra / R6 / R8, **Vancer** Akitsu, **RAWM** Leviathan V4 GT, **CRDRAKO**, **LunaFury**) | Implemented from docs *(unverified)* | Onboard flash *(diffed & rate-limited)* |
| **Endgame Gear (`3367:*`)** (OP1 8K / v2 / Purple Frost, XM2 8K / v2, OP1w 4K / v2, XM2w 4K / v2) | Implemented from docs *(unverified)* | Onboard flash *(diffed & rate-limited)* |
| **Finalmouse (`361d:*`)** (UltralightX & Starlight X dongles) | Implemented from docs *(unverified)* | Onboard memory *(diffed & rate-limited)* |
| **VAXEE (`3057:*`)** (XE / V2 / XE-S, NP-01 / NP-01S V2 / V3 / Ergo, Outset AX, E1, Sora) | Implemented from docs *(unverified)* | Onboard memory *(diffed & rate-limited)* |
| **Ninjutso (`093a:*`, `1915:*`)** (Sora V2, Sora V3, TEN / TEN AIR) | Implemented from docs *(unverified)* | Onboard memory *(diffed & rate-limited)* |
| **MCHOSE (`3837:*`, `5253:*`)** (A7 V2 Pro / Pro+ / Ultra / Ultra+, A7 Ultra, L7 Pro / Pro+) | Implemented from docs *(unverified)* | Onboard flash *(diffed & rate-limited)* |
| **Incott (`093a:522c/622c`)** (G23 / G23 V2 / G24 / Ghero / Zero 29 / Zero 39) | Implemented from docs *(unverified)* | Onboard flash *(diffed & rate-limited)* |
| **Keychron (`3434:*`, `0xFFC1`)** (M1–M8, G3–G10, LM7 wired + Link/Ultra-Link/TurboLink receivers) | Implemented from docs *(unverified)* | Onboard flash *(diffed & rate-limited)* |
| **Logitech HID++ 2.0 (`046d:*`)** (G Pro X Superlight 1/2, G305, G502, G703, G403, G903, MX) | Implemented from docs *(unverified)* | Volatile RAM *(zero flash wear)* |

## Supported Keyboards

| Keyboard / Protocol Family | Verification Status | Capabilities |
| --- | --- | --- |
| **Wooting** (`31e3:*`, legacy `03eb:ff01/ff02` — 60HE, **60HE+**, 60HE v2, 80HE, Two HE, UwU, One/Two) | **Tested on hardware (60HE+)** | RAM profile switch, interactive 60%/80HE/ISO keymap, live `0xFF53`/`0xFF54` analog visualizer + `A <-> D` counter-strafe, base & per-key actuation (`OUT 0x11`/`0x15`), Global Settings (`OUT 0x17`), AKC/SOCD & flash diagnostics |
| **Wuque Studio / SparkLink HE** (`1ca6:*`, `36b5:*` — BABAO60 HE, BABAO80 HE, ZOOM TIGA HE) | Implemented from docs *(unverified)* | Config slots 1–4, polling rate (`250–8000 Hz`), Win/Mac mode, RT precision, sleep & shake optimization telemetry |
| **Keychron Hall Effect / TMR** (`3434:*`, `KC_HE v1–v5` — 160+ Q/K/V/J/C HE & Lemokey models) | Implemented from docs *(unverified)* | Onboard profile names & switching, global actuation & Rapid Trigger (`setTravel` + `saveProfile`), DKS / Snap Tap slot counts |
| **SteelSeries Apex Pro TKL** (`1038:1628/1642/1630/1632` — 2023 & Gen 3) | Implemented from docs *(unverified)* | Firmware, CRC-32 profile telemetry, live RAM hall actuation, Rapid Trigger & Rapid Tap (SOCD) |
| **Razer Keyboards** (`1532:*`, 42 standalone models — Huntsman V3 Pro / TKL / Mini / 8KHz, BlackWidow V4, DeathStalker V2) | Implemented from docs *(unverified)* | Firmware, backlight, polling rate (`125–8000 Hz`), Huntsman V3 firmware Rapid Trigger *(strict keyboard PID filter)* |
| **Arbiter Studio** (`1a81:*`, `371c:*` — Polar 65 / 65+ / 65 PRO / 75 PRO, Nimbus 65) | Implemented from docs *(unverified)* | Firmware, actuation & RT travel table, onboard profiles (PRO models), SuperTap (SOCD) |
| **Logitech HID++ 2.0 Keyboards** (`046d:*` — PRO X TKL RAPID, PRO X 60, G915 X, G715, G515 wired/LIGHTSPEED) | Implemented from docs *(unverified)* | Firmware, battery %, extended polling rate (`0x8061`), brightness, `0x1B08` Rapid Trigger & Key Priority (SOCD) |
| **Kochi Hummingbird 68** (`1ca2:0901`) | Implemented from docs *(unverified)* | Verified `KOCHI68` identity + Windows/macOS host mode toggle |
| **Geonworks Venom 8K** (`0483:5204/5205/5206/5207/520b` — VENOM-60MX, VENOM-80MX, WISH45-8K) | Implemented from docs *(unverified)* | `baram-qmk-8k` `0xB0` firmware, matrix, USB SOF link health + VIA runtime Gaming/Typing debounce (`5–40 ms`) |
| **Universal QMK / VIA** (`0xFF60:0x61`, any unclaimed keyboard) | Implemented from docs *(unverified)* | Read-only VIA protocol version, layer count, firmware/keycodes rev, board uptime, macro slots & buffer size |

> **Closed-firmware HE boards (e.g. Geonworks Venom HE RISC-V `venom.how` & Tekkusai × Luminkey Titan 75 HE):** No public open-source HID specification exists yet, and Polled never guesses undocumented write opcodes. Use **Report Keyboard** in the Keyboard tab (or `polled --dump`) to share a serial-redacted HID report.

> **Note on onboard flash:** Most modern wireless mice persist profile changes to onboard flash rather than RAM. Polled protects against flash wear by skipping unchanged registers, debouncing alt-tabs, rate-limiting rapid bursts (max 4 writes / 60 s), and rolling back automatically if read-back verification fails.

Using an unsupported mouse or testing an unverified model? Click **Report Mouse** in the **Profiles** tab (or run `.\polled.exe --dump` and `.\polled.exe --detect` in PowerShell — serial numbers are automatically redacted) and open an issue with the output.

---

## Building from Source

Requires [Rust](https://rustup.rs/) and Visual Studio C++ Build Tools on Windows 10/11:

```powershell
cargo build --release -p polled-cli -p polled-gui
```

Binaries are output to `target\release\polled.exe` and `target\release\polled-gui.exe`.

---

## License & Attribution

Licensed under the [GNU Affero General Public License v3.0 or later](LICENSE). Provided **AS IS**, without warranty of any kind.

Protocol codecs and hardware catalogs incorporate work from open-source projects (attributed in each driver source file):
- [OpenMouse-Project/mouse-protocol](https://github.com/OpenMouse-Project/mouse-protocol) & [OpenMouse-Project/keyboard-protocol](https://github.com/OpenMouse-Project/keyboard-protocol) (AGPL-3.0-or-later)
- [openrazer/openrazer](https://github.com/openrazer/openrazer) (GPL-2.0-or-later)
- [libratbag/libratbag](https://github.com/libratbag/libratbag) (MIT) & [pwr-Solaar/Solaar](https://github.com/pwr-Solaar/Solaar) (GPL-2.0-or-later)
- [packerlschupfer/pulsar-mouse-linux](https://github.com/packerlschupfer/pulsar-mouse-linux) (MIT), [LeadSun/lamzu-cfg](https://github.com/LeadSun/lamzu-cfg) (MIT/Apache-2.0), & [anany1702/lamzu-ctl](https://github.com/anany1702/lamzu-ctl) (MIT)
- [WootingKb/wooting-rgb-sdk](https://github.com/WootingKb/wooting-rgb-sdk) (MPL-2.0), [clemenscodes/wootswitch](https://github.com/clemenscodes/wootswitch) (MIT), & [chcbaram/baram-qmk-8k](https://github.com/chcbaram/baram-qmk-8k) (open source)
