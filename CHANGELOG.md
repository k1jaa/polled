# Changelog

All notable changes to **Polled** are documented in this file.
Format loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). While version numbers remain `0.x.y`, Polled is considered **experimental / early development**.

---

## Known Issues & Limitations (Current)

- **Unsigned Windows binaries:** Release executables (`polled.exe`, `polled-gui.exe`) are not yet signed with a commercial code-signing certificate, so Windows Defender SmartScreen may prompt on first run. Every release ships `SHA256SUMS.txt` and builds publicly via GitHub Actions.
- **Unverified mouse models (`Implemented from docs`):** Only **Pulsar X2 CrazyLight Mini Wireless** (`3710:5406`) and **LAMZU 4K Receiver** (`3554:f510`) have been verified on physical hardware. Logitech HID++ 2.0 and other Compx/Nordic models are implemented from open-source protocol documentation and may exhibit firmware-specific quirks (see [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)).
- **Onboard flash writes on Compx/Nordic mice:** Compx/Nordic firmware exposes no volatile RAM-only write command; Polled mitigates flash wear via register diffing, focus debouncing, and a 4-writes / 60-second sliding-window cap.
- **No built-in auto-updater:** Portable zip releases are updated manually.
- **Minimum window size:** The GUI enforces a minimum viewport of `680×540` px to avoid crowding Input Lab plots and Profile editors.
- **Version metadata in `v0.2.0`:** The `v0.2.0` tagged binaries reported `0.1.0` from `polled --version` because workspace `Cargo.toml` was not bumped prior to tagging `v0.2.0` (fixed in `v0.2.1`).

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
