# Changelog

All notable changes to **Polled** are documented in this file.
Format loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). While version numbers remain `0.x.y`, Polled is considered **experimental / early development**.

---

## Known Issues & Limitations (Current)

- **Unsigned Windows binaries:** Release executables (`polled.exe`, `polled-gui.exe`) are not yet signed with a commercial code-signing certificate, so Windows Defender SmartScreen may prompt on first run. Every release ships `SHA256SUMS.txt` and builds publicly via GitHub Actions.
- **Unverified mouse models (`Implemented from docs`):** Only **Pulsar X2 CrazyLight Mini Wireless** (`3710:5406`) and **LAMZU 4K Receiver** (`3554:f510`) have been verified on physical hardware in Polled. Razer, CompX 64B (WLMouse / LAMZU Maya X & Inca / Attack Shark / Vancer / RAWM), Endgame Gear, Finalmouse, VAXEE, Ninjutso, MCHOSE, Incott, Keychron, Logitech HID++ 2.0, and Wave 2 CompX 17B models (VGN, Scyrox V8, ATK/VXE R1, G-Wolves XVI:0, Hitscan Hyperlight) are implemented from open-source protocol documentation and prompt for confirmation before the first write in a session (see [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)).
- **Onboard flash writes:** Most wireless gaming mice persist settings to onboard memory rather than RAM; Polled mitigates flash wear via register diffing, focus debouncing, and a 4-writes / 60-second sliding-window cap.
- **No built-in auto-updater:** Portable zip releases are updated manually.
- **Minimum window size:** The GUI enforces a minimum viewport of `680×540` px to avoid crowding Input Lab plots and Profile editors.
- **Version metadata in `v0.2.0`:** The `v0.2.0` tagged binaries reported `0.1.0` from `polled --version` because workspace `Cargo.toml` was not bumped prior to tagging `v0.2.0` (fixed in `v0.2.1`+).

---

## [0.6.0] - 2026-11-24

### Added
- **Windows Core Audio / WASAPI Backend (`win/audio.rs`):**
  - Pure-Rust Win32 COM (`MMDeviceEnumerator`, `IAudioEndpointVolume`, `IAudioClient` / `IAudioClient3`, and user-mode `IPolicyConfig`) for active playback/capture enumeration, default endpoint switching, scalar master volume control, APO `FxProperties` inspection, Bluetooth Hands-Free (HFP/SCO) detection, and silent WASAPI wake-jitter benchmarking with zero administrator rights.
- **Per-Game Audio Automation, Mic Gain Lock & Tray Audio Switcher:**
  - Extended `GameProfile` (`profiles/mod.rs`), `ProfileRuntimeHandle`, and the **Profiles** editor with optional per-game **Audio Output** device, **Output Volume (`0..=100%`)**, **Microphone** device, **Mic Volume (`0..=100%`)**, and **Lock Gain** (re-enforces microphone level every 2 seconds while active so games, Steam Voice, or Discord cannot hijack your mic slider).
  - Added a dynamic **Audio Output** submenu to the system tray for instant one-click speaker/headset switching, plus an optional **Cycle Audio Output** global hotkey (`Ctrl+Alt+A`, Win32 `RegisterHotKey`).
- **3 New Audio Health Checks (Total Built-In Checks: 26 → 29):**
  - `audio_comms_ducking` (**Fixable, HKCU, no admin**): detects Windows' default 80% communications volume ducking (`HKCU\Software\Microsoft\Multimedia\Audio\UserDuckingPreference`) and sets it to `3` ("Do nothing") with full `baseline_snapshot.json` undo.
  - `audio_enhancements` (**Report-Only**): inspects `MMDevices\Audio\Render\{guid}\FxProperties` to flag active Windows APO/DSP audio enhancements without ever tampering with `TrustedInstaller` ACLs.
  - `audio_format` (**Report-Only**): audits default playback & capture sample rates, channels, bit depths, and WASAPI scheduling periods — flagging Bluetooth Hands-Free (HFP/SCO `<= 16 kHz`) as `Bad`, extreme shared-mode resampling (`>= 192 kHz`) or high periods (`>= 15 ms`) as `Warn`, and playback/mic sample-rate mismatches (`44.1` vs `48 kHz`) as `Info`.
- **Silent WASAPI Audio Latency & Wake-Jitter Benchmark in Input Lab (`input_lab_ui.rs`):**
  - On-demand 1-second strictly silent (`AUDCLNT_BUFFERFLAGS_SILENT`) shared-mode WASAPI benchmark (`IAudioClient3` low-latency period when supported) plotting real audio callback wake intervals, driver min/default period, active buffer, p99/max latency, and DPC glitch counts (also appended to **Copy Report**).
- **CLI `--audio` Inspection (`polled --audio` / `--audio --json`):**
  - Lists all active playback & capture endpoints with default status, volume/mute, format, WASAPI periods, APO enhancement state, and Bluetooth HFP status.

---

## [0.5.0] - 2026-11-24

### Added
- **Smart Game Picker (`profiles/game_discovery.rs` + GUI modal):**
  - One-click `.exe` discovery from installed **Steam** libraries (`libraryfolders.vdf` + `appmanifest_*.acf`), **Riot Games** (`VALORANT`, `League of Legends`), **Epic Games Launcher** (`Manifests\*.item`), and currently running user apps (filtered free of Windows OS background services).
- **Richer System Tray Menu & Profile Lock:**
  - Added read-only active profile & live wireless battery % status header, dynamic **Switch Profile** submenu, and **Lock Current Profile** toggle (pauses automatic focus switches while holding the active profile) directly in the system tray and Profiles card.
- **Anti-Cheat-Safe Global Hotkeys (`hotkeys.rs` + `settings.rs`):**
  - Optional Win32 `RegisterHotKey` desktop shortcuts (zero keyboard hooks, off by default) for **Cycle Next Profile**, **Lock / Unlock Profile**, and **Start / Stop Input Lab Capture**, persisted in `settings.json` with a monochrome GUI configuration modal.
- **Independent X/Y DPI & Wireless Idle Sleep Timeout:**
  - Extended `MouseSettings`, `DeviceCapabilities`, `ProfilesUiState`, and verified drivers with independent vertical `dpi_y` (CompX 17B, CompX 64B, Razer Standard & V4, Keychron 8K XY, Dry-Run) and wireless `sleep_timeout_secs` (CompX 17B `0x00AD`, CompX 64B `0x87/0x07`, Razer `0x07/0x83 & 0x03`, MCHOSE `0x0A`, Incott `0x85/0x05 sub 0x03`, Keychron `0x0A`, Dry-Run).
- **USB Mouse Port & Hub Topology Inspector (Health Check #26 + Input Lab card):**
  - Read-only inspection of active Windows USB hubs and high-bandwidth sibling devices (`usbvideo` webcams/capture cards, `usbaudio`/`usbaudio2` DACs/mics, `USBSTOR`/`UASPStor` drives) that warns when a mouse or 4K/8K dongle sits behind an external/monitor hub or shares a hub with bandwidth-heavy peripherals.
- **Input Lab Before (A) vs After (B) Comparison & A/B PNG Export:**
  - Pin any Input Lab run as **Baseline (A)** with custom labels (e.g. `Front USB 4K` vs `Rear CPU USB 4K`), view live metric deltas (`Effective Hz`, `Sat ±15%`, `Median/p99`, `σ`, `Dropouts`, `Bunched`), and export a 960×560 side-by-side monochrome **A/B Diff PNG** card with paired histograms.
- **Privacy-Safe Keyboard Scan-Rate & Chatter Tester in Input Lab:**
  - Extends active Input Lab Raw Input capture (`RIM_TYPEKEYBOARD`) to measure keyboard scan-rate ceiling up to 8,000 Hz, fastest hold, and switch chatter (`< 25 ms`) using anonymous QPC transition timing only — zero characters or key names are ever recorded or exported.

---

## [0.4.0] - 2026-11-24

### Added
- **Wave 2 Esports Mouse Support** (`SupportTier::ImplementedFromDocs`, ported with full attribution from [`OpenMouse-Project/mouse-protocol`](https://github.com/OpenMouse-Project/mouse-protocol)):
  - **MCHOSE (`devices/mchose.rs`)**: A7 V2 Pro / Pro+ / Ultra / Ultra+, A7 Ultra, L7 Pro / Pro+ (`0x3837` / `0x5253`, bit-inverted Feature Reports `0x11`/`0x12`, explicitly excluding MagDock and V3 PIDs).
  - **Incott (`devices/incott.rs`)**: G23 / G23 V2 / G24 / Ghero / Zero 29 / Zero 39 (`0x093A:522C/622C`, Feature Report `0x09` with dual-`0xFF05` collection probing).
  - **Keychron (`devices/keychron.rs`)**: M1–M8, G3–G10, LM7 wired + Link / Ultra-Link / TurboLink receivers (`0x3434`, `0xFFC1` Launcher 8K protocol, excluding 8K Nordic PIDs).
  - **Expanded CompX 17B (`devices/compx.rs`)**: **VGN Dragonfly F2 Master+** (`3554:fb56/fb57`), **Scyrox V8** (`3554:f5f7`), **ATK / VXE R1 CompX** (`3554:f58a–f58f`), **G-Wolves XVI:0** (`33e4:*` HTX Ultra, HTS Plus/Pro, Lycan, Fenrir, HTX Mini, WARG), and **Hitscan Hyperlight** (`3770:0100/0200/0300`).
  - **Expanded Razer (`devices/razer.rs`)**: **Razer Viper V4 Pro** (`1532:00e5` wired / `00e6` wireless, 50,000 DPI, 8,000 Hz, V4 multi-stage DPI protocol).
- **Live Wireless Battery Telemetry & Low-Battery Alerts:**
  - Implemented read-only `MouseDevice::read_battery()` across CompX 17B (`0x04`), CompX 64B (`0x83`), Razer (`0x07/0x80` & `0x84`), Logitech HID++ 2.0 (`0x1004`/`0x1000`/`0x1001`), Finalmouse (`cmd 5/37/38`), VAXEE (`0x0B`/`0x10`), Ninjutso (`17/18`), MCHOSE (`0x06`), Keychron (`status[19]`), and `DryRunDevice`.
  - Surfaced live battery % + charging/voltage status in the **Profiles** status pills, **System Tray hover tooltip**, and **`polled --detect`** (human & JSON), polled every 60 seconds with a one-shot Windows notification + warning banner when discharging `<= 15%`.
- **Per-Game Sensor Controls:**
  - Added optional **Motion Sync**, **Angle Snapping**, and **Ripple Control** toggles to `MouseSettings`, `DeviceCapabilities`, `ProfilesUiState`, and supported drivers (CompX 17B, CompX 64B, Endgame Gear, Finalmouse motion sync, Ninjutso motion sync, MCHOSE, Incott, Keychron, DryRun).
- **Per-Game Windows Power Plan Switching:**
  - Added optional **Power Plan** preset (`Balanced` / `High Performance` / `Ultimate Performance`) to `GameProfile`, automatically applied on profile switch (gracefully skipping unavailable schemes on Modern Standby laptops).
- **Shareable Input Lab Reports (PNG + CSV Export):**
  - Added zero-dependency pure-Rust **960×560 monochrome PNG benchmark card renderer** (`render_report_png`) and **CSV raw interval exporter** (`format_intervals_csv`) in `polled-core::diagnostics::export`.
  - Added **Save PNG**, **Save CSV**, and **Exports** folder buttons to the Input Lab toolbar, saving files to `%LOCALAPPDATA%\Polled\exports\`.

### Fixed
- Fixed VAXEE VID typo (`2c2e` -> `3057`) in `docs/COMPATIBILITY.md`.

---

## [0.3.0] - 2026-11-24

### Added
- **Six native Rust gaming-mouse protocol drivers** (ported with full attribution from [`OpenMouse-Project/mouse-protocol`](https://github.com/OpenMouse-Project/mouse-protocol) & `openrazer`, protected by `SafeDeviceController` diffing, flash-wear rate limiting, read-back verification/rollback, and the GUI Unverified Hardware Warning):
  - **Razer (`devices/razer.rs`)**: 80+ Razer models over 90-byte HID feature reports (`0x1532`) — Viper V3 Pro / SE / HyperSpeed, Viper V2 Pro / Ultimate / 8KHz / Mini SE, DeathAdder V4 Pro / V3 Pro / V3 / V2 / Essential, Basilisk V3 Pro / V3 / X, Cobra Pro, Naga V2 Pro / Pro / X, Orochi V2, Mouse Dock Pro, and HyperPolling Wireless Dongle.
  - **CompX 64B Page-Command (`devices/compx64.rs`)**: **WLMouse** (Beast X / Pro / Max / Mini / Mini Pro / Miao / G, Sword X, Strider, Huan, Ying, 1K Receiver), **LAMZU Aurora / 8K Gen** (Maya X, Inca 8K, Paro Aurora, Aurora 1K/8K Receivers), **Attack Shark** (R5 Ultra, R6, R8), **Vancer** Akitsu Medium 3950, **RAWM** Leviathan V4 GT, **CRDRAKO** KO-ONE, and **LunaFury** LUNA33/TYPE33.
  - **Endgame Gear (`devices/endgame_gear.rs`)**: OP1 8K / v2 / Purple Frost, XM2 8K / v2, OP1w 4K / v2, XM2w 4K / v2 (`0x3367`, Feature Report `0xA0`/`0xA1`, wired config store & wireless 4K block writes).
  - **Finalmouse (`devices/finalmouse.rs`)**: UltralightX (ULX) and Starlight X (SLX) receivers (`0x361D`).
  - **VAXEE (`devices/vaxee.rs`)**: XE / V2 / XE-S, NP-01 / NP-01S V2 / V3 / Ergo, Outset AX, E1, and VAXEE x Ninjutso Sora (`0x3057`, Feature Report `0x0E`).
  - **Ninjutso (`devices/ninjutso.rs`)**: Sora V2 (legacy `0x1915`), Sora V3, and TEN / TEN AIR (`0x093A`).
- Expanded USB HID hot-plug fingerprinting (`hid_topology_fingerprint`) to also track Generic Desktop Mouse collections (`0x01:0x02`) so Razer mice hot-plug cleanly.

### Changed
- **Re-licensed Polled from `GPL-3.0-or-later` to `AGPL-3.0-or-later`** (`LICENSE`, `Cargo.toml`, all source SPDX headers, GUI footer/tooltip, and docs) for full license compatibility with `OpenMouse-Project/mouse-protocol`.

---

## [0.2.1] - 2026-11-24

### Added
- **Experimental labeling & transparency docs:**
  - Added prominent Experimental (`0.x`) / GPL-3.0 warranty notice to `README.md` and a `v0.2.1 · Experimental` badge + window title in `polled-gui`.
  - Added [docs/SYSTEM_MODIFICATIONS.md](docs/SYSTEM_MODIFICATIONS.md) auditing all 25 health checks/tweaks, custom tweaks blocklist, `%LOCALAPPDATA%\Polled\` files, startup key, USB HID registers, and manual rollback instructions.
  - Added [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) tracking tested vs. unverified mice, vendor-software conflicts, wireless sleep timeouts, and laptop caveats.
  - Added `CHANGELOG.md` with publicly disclosed known limitations.
- **Pre-operation safety warnings in the GUI:**
  - Confirmation modal before applying **Custom Tweaks**, **Optional Tweaks**, or **Sign-out / Reboot-requiring** Windows changes (showing exact `Current → Target` values, impact badges, System Restore Point recommendation for `HKLM`/reboot tweaks, and rollback guarantee).
  - Enhanced UAC elevation modal to clearly display exact change summary, impact flags, and rollback behavior in a single prompt.
  - First-write session warning dialog in the **Profiles** tab before writing settings to any unverified (`Implemented from docs`) hardware mouse via **Apply Now** or **Auto-Switch: ON**, plus an inline unverified-hardware banner.
- **Release packaging:** Bundled `CHANGELOG.md`, `docs/SYSTEM_MODIFICATIONS.md`, and `docs/COMPATIBILITY.md` into the portable Windows release zip.

### Fixed
- Synced workspace `Cargo.toml` version (`0.2.1`) so `polled --version` and GUI badges match the release tag.
- Updated `README.md` installation link so it points to the latest release archive instead of `v0.1.0`.

---

## [0.2.0] - 2026-11-24

### Added
- **Input Lab tab** (`polled-gui` + `polled-core::diagnostics`):
  - Dedicated kernel-blocking Win32 `WM_INPUT` + `QueryPerformanceCounter` capture thread supporting up to **8,000 Hz** with zero idle polling.
  - Motion-saturated 4K/8K wireless dropout detection (separating genuine radio drops from slow hand motion) and a **29-bin interval distribution histogram** with `1×`/`2×` harmonic markers and interactive tooltips.
  - **Measured DPI/CPI Calibrator** with hands-free `Space`/`Enter` start/stop, cm/inch distance conversion, deviation %, straightness %, and sensitivity match multiplier.
  - Smoothed **Velocity (IPS / m/s) & Acceleration (G)** plot (4 ms sliding window) and **1:1 2D XY Trace** plot.
  - **Click Debounce & Double-Click Bounce Tester** (`< 35 ms` bounce detection) and **1-second OS Timer Jitter Benchmark**.
- **Expanded Health Check from 11 to 25 built-in checks & reversible tweaks:**
  - Added PCIe ASPM, Game Bar Background Capture (`GameDVR`), Monitor Refresh Rate, Windows Timer Resolution & Wake Jitter, USB Mouse & Root Hub Power Saving, VBS / Memory Integrity (HVCI, report-only), Win11 Windowed Game Optimizations (Flip-Model), Hardware Cursor & Pointer Trails, `mouclass` Mouse Data Queue Size, USB xHCI MSI Interrupts (report-only), and CPU Maximum Processor State (AC).
  - Added 3 opt-in reversible latency tweaks: **MMCSS Gaming Scheduling Priority**, **Win32 Priority Separation (`0x26`)**, and **Global Timer Resolution Requests (Win11)**.
- **User Custom Tweaks Manager:** Create, save (`custom_tweaks.json`), apply, and cleanly undo custom `HKCU`/`HKLM` registry tweaks with 4 curated quick-fill presets and a security/boot registry path blocklist.
- **Latency Tips panel:** Collapsible hardware/USB/sensor/GPU/display latency checklist in Health Check.

### Fixed
- Removed duplicate test definition in `crates/polled-core/src/checks/runner.rs` that failed `cargo fmt --check` and test compilation.

---

## [0.1.1] - 2026-11-24

### Fixed
- Drove responsive layout from live viewport width (`ctx.screen_rect().width()`) across Health Check and Profiles tabs.
- Replaced `egui::Frame` inside `horizontal_wrapped` with pre-measured row-packed status pills (`render_status_pill_rows`), eliminating horizontal clipping of `FLASH WRITES` and `Copy Log` and enabling clean narrow-window stacking down to `680×540`.

---

## [0.1.0] - 2026-11-23

### Added
- Initial pure-Rust (`eframe`/`egui` + `hidapi` + `windows`) Windows 10/11 tray utility (`polled-gui.exe`) and CLI (`polled.exe`).
- 11 initial Windows input health checks with atomic `baseline_snapshot.json` undo, Windows Update regression detection, and on-demand UAC elevation.
- Per-Game Mouse Profiles with anti-cheat-safe `SetWinEventHook` foreground watcher, focus debouncer, per-mouse overrides, capability adaptation, read-back verification + rollback, flash-wear protection, USB hot-plug detection, and JSON export/import.
- Protocol-family hardware drivers for **Compx / Nordic 17B** (Pulsar X2 CrazyLight Mini Wireless & LAMZU 4K verified on hardware) and **Logitech HID++ 2.0** (volatile RAM, implemented from docs).
- Persisted Dark/Light monochrome theme, HKCU `--minimized` autostart, single-instance lock, and system tray controls.
