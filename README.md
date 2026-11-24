# Polled

> [!WARNING]
> **Experimental Software (`0.x` Early Development):** Polled is under active early development. It modifies Windows registry and power settings and sends raw USB HID configuration commands to supported mice. Although Polled snapshots your original Windows values before making changes and validates/verifies mouse writes, **use at your own risk** and consider creating a Windows System Restore Point before applying system-wide (`HKLM`) or custom tweaks. Provided **"AS IS"** without warranty under the [GNU General Public License v3.0](LICENSE) (Sections 15–16).

A lightweight, pure-Rust Windows 10/11 tray utility and CLI that does three things:

1. **Input Health Check & Reversible Tweaks** — audits 25 Windows input/latency settings (pointer acceleration, pointer speed, custom acceleration curves, hardware cursor, mouse queue depth, USB selective suspend/MSI/hub power, PCIe ASPM, CPU max state, power plan, timer wake jitter, monitor refresh rate, Win11 Flip-Model windowed optimizations, Game Mode, GameDVR, HAGS, VBS/HVCI, optional MMCSS / scheduler / global timer tweaks, accessibility shortcuts, and background vendor suites) plus user-defined Custom Tweaks, with one-click fixes and full rollback.
2. **Per-Game Mouse Profiles** — automatically switches DPI, polling rate, lift-off distance, and debounce when a game takes focus, then restores your desktop settings when you tab out.
3. **Input Lab** — live high-precision raw-input diagnostics up to 8,000 Hz (motion-saturated wireless dropout vs. slow-hand separation, 29-bin interval histogram, Velocity/IPS/G & 1:1 2D XY Trace, hands-free Measured DPI/CPI calibrator, mouse switch double-click bounce tester, and an on-demand 1-second OS timer jitter benchmark).

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
  1. Switch to the **Profiles** tab — supported mice are detected automatically (including USB hot-plug).
  2. Click **+ New**, enter your game executable (e.g. `cs2.exe`, `valorant-win64-shipping.exe`), and pick your DPI, polling rate, lift-off distance, and debounce.
  3. If you use multiple mice, pick a mouse under **Mouse Target** to save per-mouse overrides.
  4. Toggle **Auto-Switch: ON** and close `[X]` to keep Polled running quietly in the tray.
- **Input Lab:** Switch to the **Input Lab** tab and click **Start Capture** to test real polling consistency up to 8,000 Hz, measure true DPI/CPI, inspect velocity/XY tracking, check switch double-click bounce, or run the 1-second timer jitter benchmark.

---

## Supported Mice

See [**docs/COMPATIBILITY.md**](docs/COMPATIBILITY.md) for the complete compatibility table, known issues, and per-device caveats.

| Mouse / Protocol Family | Verification Status | Memory Mode |
| --- | --- | --- |
| **Pulsar X2 CrazyLight Mini Wireless** (`3710:5406`) | **Tested on hardware** | Onboard flash *(diffed & rate-limited)* |
| **LAMZU 4K Receiver** (`3554:f510`) | **Tested on hardware** | Onboard flash *(diffed & rate-limited)* |
| **Other Compx / Nordic mice** (Pulsar X2 CrazyLight, X2A, Xlite V3, LAMZU 1K/Wired, generic `3554:*`) | Implemented from docs *(unverified)* | Onboard flash *(diffed & rate-limited)* |
| **Logitech HID++ 2.0** (G Pro X Superlight 1/2, G305, G502, G703, G403, G903, MX) | Implemented from docs *(unverified)* | Volatile RAM *(zero flash wear)* |

> **Note on onboard flash:** Mice using the Compx/Nordic protocol store profile changes in onboard flash rather than RAM. Polled protects against flash wear by skipping unchanged registers, debouncing alt-tabs, rate-limiting rapid bursts (max 4 writes / 60 s), and rolling back automatically if read-back verification fails.

Using an unsupported mouse or testing an unverified model? Run `.\polled.exe --dump` (serial numbers are automatically redacted) and `.\polled.exe --detect` in PowerShell, then open an issue with the output.

---

## Building from Source

Requires [Rust](https://rustup.rs/) and Visual Studio C++ Build Tools on Windows 10/11:

```powershell
cargo build --release -p polled-cli -p polled-gui
```

Binaries are output to `target\release\polled.exe` and `target\release\polled-gui.exe`.

---

## License

Licensed under the [GNU General Public License v3.0 or later](LICENSE). Provided **AS IS**, without warranty of any kind.
