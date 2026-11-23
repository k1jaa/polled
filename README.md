# Polled

A small, fast Windows 10/11 tray utility that does two things:

1. **Input Health Check** — finds Windows settings that hurt mouse input (pointer acceleration, pointer speed, USB selective suspend, accessibility shortcuts, power plan, Game Mode, etc.) and fixes them with one click and a full undo.
2. **Per-Game Mouse Profiles** — automatically switches DPI, polling rate, lift-off distance, and debounce when a game takes focus, then restores your desktop settings when you tab out.

- **Lightweight & local:** Pure Rust tray app (~6 MB, no WebView2/Electron, no account, no telemetry, no cloud).
- **Safe & reversible:** Original Windows settings are saved before any change. Click **Undo All** anytime to restore everything. Never touches device firmware.
- **No persistent admin:** Runs normally without admin rights and only asks for UAC on the specific fix that needs it (like USB selective suspend).
- **Anti-cheat safe:** Only watches which window is in the foreground — no DLL injection, no game hooks, no memory reading. Safe alongside Vanguard, FACEIT, EAC, and BattlEye.

---

## Installation

1. Download `polled-v0.1.0-windows-x64.zip` from the [**Releases**](https://github.com/k1jaa/polled/releases) page.
2. Extract the zip to any folder you like (for example `C:\Tools\Polled\`).
   - Keep `polled-gui.exe` and `polled.exe` in the same folder.
3. Double-click **`polled-gui.exe`** to open Polled.
4. *(Optional)* Click **Startup: OFF → ON** in the top-right bar to start Polled minimized in the system tray when you sign in to Windows.

> **Uninstall:** Turn **Startup** off (if enabled), right-click the tray icon → **Quit**, and delete the folder. All local settings and undo snapshots live in `%LOCALAPPDATA%\Polled\`.

---

## Quick Start

- **Health Check:** Open Polled to scan your Windows input settings. Click **Fix** on individual items (or **Fix Safe Issues**), and **Undo** / **Undo All** anytime. Polled also re-checks after Windows Updates and notifies you only if something got worse.
- **Per-Game Profiles:**
  1. Switch to the **Profiles** tab — supported mice are detected automatically (including USB hot-plug).
  2. Click **+ New**, enter your game executable (e.g. `cs2.exe`, `valorant-win64-shipping.exe`), and pick your DPI, polling rate, lift-off distance, and debounce.
  3. If you use multiple mice, pick a mouse under **Mouse Target** to save per-mouse overrides.
  4. Toggle **Auto-Switch: ON** and close `[X]` to keep Polled running quietly in the tray.

---

## Supported Mice

| Mouse / Protocol Family | Status | Memory Mode |
| --- | --- | --- |
| **Pulsar X2 CrazyLight Mini Wireless** (`3710:5406`) | **Tested on hardware** | Onboard flash *(diffed & rate-limited)* |
| **LAMZU 4K Receiver** (`3554:f510`) | **Tested on hardware** | Onboard flash *(diffed & rate-limited)* |
| **Other Compx / Nordic mice** (Pulsar X2 CrazyLight, X2A, Xlite V3, LAMZU 1K/Wired, generic `3554:*`) | Implemented from docs | Onboard flash *(diffed & rate-limited)* |
| **Logitech HID++ 2.0** (G Pro X Superlight 1/2, G305, G502, G703, G403, G903, MX) | Implemented from docs | Volatile RAM *(zero flash wear)* |

> **Note on onboard flash:** Mice using the Compx/Nordic protocol store changes in onboard flash rather than RAM. Polled protects against flash wear by skipping unchanged registers, debouncing alt-tabs, and rate-limiting rapid bursts.

Using an unsupported mouse? Run `.\polled.exe --dump` in PowerShell (serial numbers are automatically redacted) and open an issue with the output.

---

## Building from Source

Requires [Rust](https://rustup.rs/) and Visual Studio C++ Build Tools on Windows 10/11:

```powershell
cargo build --release -p polled-cli -p polled-gui
```

Binaries are output to `target\release\polled.exe` and `target\release\polled-gui.exe`.

---

## License

Licensed under the [GNU General Public License v3.0 or later](LICENSE).
