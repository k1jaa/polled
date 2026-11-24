# Polled

> [!WARNING]
> **Experimental Software (`0.x` Early Development):** Polled is under active early development. It modifies Windows registry and power settings and sends raw USB HID configuration commands to supported mice. Although Polled snapshots your original Windows values before making changes and validates/verifies mouse writes, **use at your own risk** and consider creating a Windows System Restore Point before applying system-wide (`HKLM`) or custom tweaks. Provided **"AS IS"** without warranty under the [GNU Affero General Public License v3.0](LICENSE) (Sections 15–16).

A lightweight, pure-Rust Windows 10/11 tray utility and CLI that does three things:

1. **Input Health Check & Reversible Tweaks** — audits 26 Windows input/latency settings (pointer acceleration, pointer speed, custom acceleration curves, hardware cursor, mouse queue depth, USB selective suspend / hub power / xHCI MSI / **port & hub topology**, PCIe ASPM, CPU max state, power plan, timer wake jitter, monitor refresh rate, Win11 Flip-Model windowed optimizations, Game Mode, GameDVR, HAGS, VBS/HVCI, optional MMCSS / scheduler / global timer tweaks, accessibility shortcuts, and background vendor suites) plus user-defined Custom Tweaks, with one-click fixes and full rollback.
2. **Per-Game Mouse Profiles & Live Battery** — automatically switches DPI (**including independent X/Y DPI**), polling rate, lift-off distance, debounce, sensor toggles (Motion Sync, Angle Snapping, Ripple Control), **wireless idle sleep timeout**, Windows pointer speed, and optional Windows power plan when a game takes focus, then restores your desktop settings when you tab out. Includes a **Smart Game Picker** (installed Steam / Riot / Epic games & running apps), **tray quick-switch & profile lock**, optional **global hotkeys** (`RegisterHotKey`), and live wireless battery % with low-battery alerts (`<= 15%`).
3. **Input Lab** — live high-precision raw-input diagnostics up to 8,000 Hz (motion-saturated wireless dropout vs. slow-hand separation, 29-bin interval histogram, Velocity/IPS/G & 1:1 2D XY Trace, hands-free Measured DPI/CPI calibrator, mouse switch double-click bounce tester, **privacy-safe keyboard scan-rate & chatter tester**, **live USB port & hub topology inspector**, 1-second OS timer jitter benchmark, **Before/After A/B comparison**, plus one-click **PNG** / **A/B PNG** & **CSV** export).

- **Lightweight & local:** Pure Rust tray app (~6 MB, no WebView2/Electron, no account, no telemetry, no network access).
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

- **Health Check:** Open Polled to scan 25 Windows input settings. Click **Fix** on individual items (or **Fix Safe Issues** for non-admin, non-optional fixes), and **Undo** / **Undo All** anytime. Polled also re-checks after Windows Updates and notifies you only if a setting regressed.
- **Per-Game Profiles:**
  1. Switch to the **Profiles** tab — supported mice and live wireless battery percentages are detected automatically (including USB hot-plug).
  2. Click **+ New**, enter your game executable (e.g. `cs2.exe`, `valorant-win64-shipping.exe`), and configure DPI, polling rate, lift-off distance, debounce, sensor toggles (Motion Sync / Angle Snapping / Ripple Control), and optional Windows power plan.
  3. If you use multiple mice, pick a mouse under **Mouse Target** to save per-mouse overrides.
  4. Toggle **Auto-Switch: ON** and close `[X]` to keep Polled running quietly in the tray (hover the tray icon for active profile & battery %).
- **Input Lab:** Switch to the **Input Lab** tab and click **Start Capture** to test real polling consistency up to 8,000 Hz, measure true DPI/CPI, inspect velocity/XY tracking, check switch double-click bounce, run the 1-second timer jitter benchmark, or export shareable **PNG** benchmark cards and raw **CSV** intervals.

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

> **Note on onboard flash:** Most modern wireless mice persist profile changes to onboard flash rather than RAM. Polled protects against flash wear by skipping unchanged registers, debouncing alt-tabs, rate-limiting rapid bursts (max 4 writes / 60 s), and rolling back automatically if read-back verification fails.

Using an unsupported mouse or testing an unverified model? Run `.\polled.exe --dump` (serial numbers are automatically redacted) and `.\polled.exe --detect` in PowerShell, then open an issue with the output.

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
- [OpenMouse-Project/mouse-protocol](https://github.com/OpenMouse-Project/mouse-protocol) (AGPL-3.0-or-later)
- [openrazer/openrazer](https://github.com/openrazer/openrazer) (GPL-2.0-or-later)
- [libratbag/libratbag](https://github.com/libratbag/libratbag) (MIT) & [pwr-Solaar/Solaar](https://github.com/pwr-Solaar/Solaar) (GPL-2.0-or-later)
- [packerlschupfer/pulsar-mouse-linux](https://github.com/packerlschupfer/pulsar-mouse-linux) (MIT), [LeadSun/lamzu-cfg](https://github.com/LeadSun/lamzu-cfg) (MIT/Apache-2.0), & [anany1702/lamzu-ctl](https://github.com/anany1702/lamzu-ctl) (MIT)
