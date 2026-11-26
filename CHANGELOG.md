# Changelog

All notable changes to **Polled** are documented in this file.
Format loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). While version numbers remain `0.x.y`, Polled is considered **experimental / early development**.

---

## Known Issues & Limitations (Current)

- **Unsigned Windows binaries:** Release executables (`polled.exe`, `polled-gui.exe`) are not yet signed with a commercial code-signing certificate, so Windows Defender SmartScreen may prompt on first run. Every release ships `SHA256SUMS.txt` and builds publicly via GitHub Actions.
- **Unverified mouse models (`Implemented from docs`):** Only **Pulsar X2 CrazyLight Mini Wireless** (`3710:5406`) and **LAMZU 4K Receiver** (`3554:f510`) have been verified on physical hardware in Polled. Razer, CompX 64B (WLMouse / LAMZU Maya X & Inca / Attack Shark / Vancer / RAWM), Endgame Gear, Finalmouse, VAXEE, Ninjutso, MCHOSE, Incott, Keychron, Logitech HID++ 2.0, and Wave 2 CompX 17B models (VGN, Scyrox V8, ATK/VXE R1, G-Wolves XVI:0, Hitscan Hyperlight) are implemented from open-source protocol documentation and prompt for confirmation before the first write in a session (see [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)).
- **Onboard flash writes:** Most wireless gaming mice persist settings to onboard memory rather than RAM; Polled mitigates flash wear via register diffing, focus debouncing, and a 4-writes / 60-second sliding-window cap.
- **No automatic background updater:** Polled includes a strictly on-demand **Check for Updates** button (Settings / `--check-update`) over Win32 WinHTTP that opens the GitHub Releases page when you choose; portable ZIPs are never downloaded or overwritten silently in the background.
- **Minimum window size:** The GUI enforces a minimum viewport of `740×540` px to avoid crowding Bench plots and Profile editors.
- **Version metadata in `v0.2.0`:** The `v0.2.0` tagged binaries reported `0.1.0` from `polled --version` because workspace `Cargo.toml` was not bumped prior to tagging `v0.2.0` (fixed in `v0.2.1`+).

---

## [Unreleased]

### Changed
- **Ultra-Light Pure-Win32 Background Tray Daemon & On-Demand GPU GUI (`daemon.rs`, `ipc.rs`, `main.rs`, `app.rs`, `tray.rs`, `single_instance.rs`, `profile_runtime.rs`, `hotkeys.rs`, `notifications.rs`):**
  - Split `polled-gui.exe` into a pure-Win32 zero-OpenGL background tray daemon (`polled-gui.exe` / `--minimized`) and an on-demand GPU UI child window (`polled-gui.exe --ui --ipc-pid <pid>`) synchronized over two local-only Win32 Named Pipes (`\\.\pipe\PolledIpcC2S_<pid>` and `PolledIpcS2C_<pid>`, `PIPE_REJECT_REMOTE_CLIENTS`, zero network sockets).
  - Because the tray daemon never initializes `eframe` / OpenGL, GPU drivers (`nvoglv64.dll` / AMD ICD) are never loaded while Polled sits in the tray, and closing `[X]` exits the `--ui` child process to return 100% of GPU memory back to Windows immediately (cutting idle tray RAM from ~77 MB down to ~3 MB private bytes / <1 MB trimmed working set).
  - Replaced the 50 ms tray polling loop with native event-driven `TrayIconEvent::set_event_handler` / `MenuEvent::set_event_handler` callbacks and replaced the 250 ms single-instance wait loop with kernel-blocking `WaitForMultipleObjects(..., INFINITE)` for true 0% idle CPU.
  - Added clean `WM_QUERYENDSESSION` / `WM_ENDSESSION` handling and deterministic runtime thread join on quit so temporary display overrides, ducked app mixer volumes, and quiet notifications always restore cleanly.
  - Tightened release profile (`opt-level = "s"`, `lto = true`, `strip = true`, `panic = "abort"`), shrinking `polled-gui.exe` from `9.19 MB` to `5.53 MB` and `polled.exe` from `2.96 MB` to `1.47 MB`.

---

## [0.13.0] - 2026-11-26

### Added
- **Multi-Brand Keyboard Architecture & 9 New Open-Source Keyboard Backends (`devices/keyboards/*`, `keyboard_ui.rs`, `profile_runtime.rs`, `profiles_ui.rs`, `tray.rs`, `diagnostics/export.rs`, `polled-cli`):**
  - Introduced a pluggable `KeyboardDevice` trait, `KeyboardStatus`, `KeyboardCapabilities`, and collision-safe ordered detection registry (`detect_all_keyboards()`) wrapping Wooting (`SupportTier::TestedOnHardware`) with zero regressions while opening Polled's Keyboard Hub, Profiles, Tray, Global Hotkey (`Cycle Keyboard Profile`), Rig Scorecard PNG v2 (`6. KEYBOARD`), and CLI (`polled --keyboards`) to 10 keyboard ecosystems.
  - Added verified open-source backends (`SupportTier::ImplementedFromDocs`, confirm-gated + read-back verified, zero destructive opcodes) independently implemented from `OpenMouse-Project/keyboard-protocol` (AGPL-3.0-or-later) and `chcbaram/baram-qmk-8k`:
    - **Wuque Studio / SparkLink HE** (`1ca6:1b10/1b0a/1b09`, `36b5:75d4/75d2` — BABAO60/80 HE, ZOOM TIGA HE): Config slots 1–4, polling rate (`250–8000 Hz`), Win/Mac mode, RT precision, sleep & shake optimization telemetry.
    - **Keychron Hall Effect / TMR** (`3434:*`, 160+ Q/K/V/J/C HE & Lemokey PIDs, `KC_HE 0xA9 v1–v5`): profile names & slot switching, global actuation & Rapid Trigger (`setTravel` + `saveProfile`), DKS & Snap Tap counts.
    - **SteelSeries Apex Pro TKL** (`1038:1628/1642/1630/1632`, `0xFFC0:0x0001`): firmware, 12.5 KB STM32 CRC-32 profile validation, live RAM hall actuation, Rapid Trigger & Rapid Tap (SOCD).
    - **Razer Keyboards** (`1532:*`, 42 standalone keyboard PIDs — Huntsman V3 Pro / TKL / Mini / 8KHz, BlackWidow V4, DeathStalker V2): strict keyboard PID catalog (excludes mice), firmware, backlight, polling rate (`125–8000 Hz`), Huntsman V3 Rapid Trigger.
    - **Arbiter Studio** (`1a81:*`, `371c:*` — Polar 65 / 65+ / 65 PRO / 75 PRO, Nimbus 65): firmware, actuation & RT travel table, PRO onboard profiles, SuperTap (SOCD).
    - **Logitech HID++ 2.0 Keyboards** (`046d:*` — PRO X TKL RAPID, PRO X 60, G915 X, G715, G515 wired/LIGHTSPEED): strict keyboard PID catalog + receiver pairing verification, firmware, unified battery (`0x1004`), extended polling rate (`0x8061`), backlight, and `0x1B08` analog switches (Rapid Trigger & Key Priority / Snap Tap).
    - **Kochi Hummingbird 68** (`1ca2:0901`, `0xFFA0:0x0001`): verified `KOCHI68` identity + Windows/macOS mode toggle.
    - **Geonworks Venom 8K** (`0483:5204/5205/5206/5207/520b` — VENOM-60MX/80MX/WISH45-8K): `baram-qmk-8k` `0xB0` firmware, matrix, USB SOF heartbeat & error counters, plus VIA channel 13 Gaming/Typing debounce (`5–40 ms`).
    - **Universal QMK / VIA** (`0xFF60:0x61`, unclaimed boards): read-only VIA protocol version, layer count, firmware/keycodes rev, board uptime, macro slots & buffer size.
- **Dedicated `Keyboard` Tab & `Profiles → Keyboard` Sub-Tab (`keyboard_ui.rs`, `app.rs`, `profiles_ui.rs`):**
  - Promoted Wooting keyboard management into its own top-level **Keyboard** workspace in the left rail (with live `P#` / `LIVE` badge and `RIG STATUS` summary) plus a dedicated **Keyboard** sub-tab inside the Profile Editor showing real firmware profile names instead of generic `Profile 1–4`.
- **Deep Open-Source-Verified Wooting ARM & Standard HID Protocol (`devices/wooting.rs`, `profile_runtime.rs`, `polled-cli`):**
  - Audited directly from `OpenMouse-Project/keyboard-protocol` (`AGPL-3.0-or-later`), `WootingKb/wooting-analog-sdk` (`MPL-2.0`), and `clemenscodes/wootswitch` (`MIT`): fixed ARM (`0xFF55`, report ID `0x01`, magic `0xD1 0xDA`, slot byte 4, runtime slot `data[2]`) vs Standard (`0x1337`, report ID `0x00`, magic `0xD0 0xDA`, slot byte 7, runtime slot `data[0]`) framing, added `WootDevInit (0x21)` + `100 ms` settle delays, quiet drain, and command-echo matching across all 13 Wooting base PIDs (including 60HE+, 60HE v2, 80HE, Two HE, UwU, One, Two).
  - Grabs firmware version (`0x01`), physical layout (`0x13`), real onboard custom profile names (`0x37`, up to 8 slots), active profile base actuation mm / Rapid Trigger / RT sensitivity / Tachyon / tick rate (`0x27`), per-key actuation overrides (`0x31`, supporting both Q12 Field 4 and live-verified `v2.13.2` 8-bit Field 2), per-key Rapid Trigger groups (`0x3D`), Advanced Keys / AKC (`0x34` — SOCD pairs & mode, Rappy Snappy, DKS, Mod Tap, Toggle Key), Global Device Settings (`0x33`), RGB Core brightness & effect layers (`0x32`), and hardware/flash stats (`0x10`/`0x04`/`0x0A`/`0x38`/`0x3A`).
  - Added an opt-in **Live Analog Key-Travel Visualizer (`0xFF53` V2 10-bit / `0xFF54`)** rendering smoothed real-time `0.00–4.00 mm` depth bars with live hardware actuation state, 1-second peak linger, and zero keystrokes stored (0% CPU when stopped, auto-stops on minimize-to-tray).
  - Added confirm-gated, field-preserving protobuf ARM writes for Base Analog (`OUT 0x11` verified via `0x27`), **Per-Key Actuation Overrides (`OUT 0x15` verified via `0x31`, supporting both Q12 and `v2.13.2` 8-bit formats)**, and Global Settings (`OUT 0x17` verified via `0x33`), plus upgraded CLI `polled --wooting` (text & `--json`) to dump the full telemetry snapshot.
- **Interactive Multi-Layout Wooting Visual Keymap, Click-to-Edit Per-Key Actuation, `A <-> D` Counter-Strafe Analyzer, & Tray/Hotkey Controls (`keyboard_ui.rs`, `tray.rs`, `hotkeys.rs`, `profile_runtime.rs`, `app.rs`):**
  - Added a responsive monochrome **Visual Keymap** supporting **60% ANSI, 60% ISO, and 80HE/TKL ANSI & ISO** layouts with live bottom-to-top analog fill, green hardware actuation highlights, per-key actuation mm overrides, **hover-linked SOCD / Rappy Snappy pair highlights**, and **click / multi-select per-key actuation editing** (plus 1-click `Select WASD` and `Reset to Base`).
  - Added a live **`A <-> D` Counter-Strafe Timing Analyzer** inside the Analog Visualizer timing raw unsmoothed `A`/`D` actuation edges to measure overlap (`-ms`) vs release gap (`+ms`) across the last 20 strafe reversals.
  - Added a **Wooting Keyboard** submenu to the Windows system tray displaying real onboard slot names (`● P2 · mainother`, etc.) plus an optional **Cycle Wooting Profile** global hotkey (`Ctrl+Alt+K` default).
- **Per-Core Interrupt & DPC Spike Monitor — Mini LatencyMon (`diagnostics/dpc_monitor.rs`, `diagnostics/analyzer.rs`, `input_lab_ui.rs`):**
  - Added a pure user-mode per-logical-core Interrupt & DPC latency monitor in **Bench → System & USB** querying `ntdll.dll` `NtQuerySystemInformation` (`SystemProcessorPerformanceInformation = 8` + `SystemInterruptInformation = 23`, 500 ms interval, zero kernel drivers, zero admin, 0% CPU when stopped) with automatic Core 0 IRQ starvation detection fed directly into Bench **Recommended Fixes**.
- **Low-Battery Polling Guard, Background App Mixer Ducking, Wake Retry, GPU Pref, & Quiet Notifications (`win/audio.rs`, `settings.rs`, `profiles/watcher.rs`, `profiles/mod.rs`, `profile_runtime.rs`, `profiles_ui.rs`, `app.rs`):**
  - Added an opt-in **Battery Polling Guard** in Settings that automatically steps wireless 2K/4K/8K mice down to `1000 Hz` once battery hits your low-battery threshold, restoring profile polling rate automatically when charging or recovered.
  - Added pure Core Audio (`IAudioSessionManager2` / `ISimpleAudioVolume`, zero admin) **Background App Mixer Ducking** per game profile (with 1-click `+ Spotify`, `+ Chrome`, `+ Edge`, `+ Firefox`, `+ Discord` chips) to lower or mute noisy desktop apps while gaming and restore their exact original mixer sliders on desktop/exit.
  - Automatically retries applying a profile for up to 90 seconds (polling every 2 seconds) when a wireless mouse is asleep at game launch, firing the switch & OSD pill the moment the mouse wakes.
  - Added optional per-game **Force High-Performance GPU** (`HKCU\Software\Microsoft\DirectX\UserGpuPreferences`, `GpuPreference=2;`) and reversible **Quiet Notifications** (`HKCU\Software\Microsoft\Windows\CurrentVersion\PushNotifications\ToastEnabled = 0`, auto-restored on desktop/exit).
- **Raw-Input Click Reaction Test, Persistent Bench History, & Rig Scorecard PNG v2 (`diagnostics/reaction.rs`, `diagnostics/history.rs`, `diagnostics/export.rs`, `diagnostics/raw_capture.rs`, `input_lab_ui.rs`):**
  - Added a 5/10-trial monochrome visual-flash reaction & switch consistency tester in **Bench → Aim & Switches** timing `RI_MOUSE_LEFT_BUTTON_DOWN` directly on the decoupled `WM_INPUT` `QueryPerformanceCounter` thread (median, best, mean, P95, jitter SD, false-start tracking, and per-trial bar plot).
  - Added **Persistent Bench History (`bench_history.json`)** so named polling/latency runs survive restarts and can be reloaded into **Baseline (A)** for Before/After A/B comparisons in one click.
  - Upgraded the shareable **All-in-One Rig Scorecard PNG** to **v2 (`1040×760`)**, adding Row 3 tiles for **Wooting Keyboard**, **Per-Core DPC / IRQ**, and **Raw-Input Reaction**.

---

## [0.12.0] - 2026-11-25

### Added
- **Reversible PCI GPU & HD Audio MSI Mode Fixer (`checks/pci_gpu_audio_msi.rs`, `checks/snapshot.rs`, `win/usb_msi.rs`):**
  - Upgraded `pci_gpu_audio_msi` from report-only to an opt-in reversible tweak (`is_optional_tweak = true`, `requires_elevation = true`, `requires_reboot = true`, Curated Opt-In Tweaks: 12 → 13).
  - Strictly targets PCI GPUs & High Definition Audio controllers explicitly reporting `MSISupported == 0` (never touches devices already on MSI or missing the capability), capturing every exact device registry path & original DWORD into `baseline_snapshot.json` (`SavedValue::PciMsiMode`) for 1-click Undo.
- **Scroll Wheel Encoder Bounce & Reverse-Scroll Tester (`diagnostics/raw_capture.rs`, `diagnostics/analyzer.rs`, `input_lab_ui.rs`):**
  - Captures raw hardware `RI_MOUSE_WHEEL` delta ticks on the decoupled `WM_INPUT` thread to detect single opposite-direction reverse-scroll glitches during steady sweeps (`<= 160 ms`) and mechanical sub-18 ms double-notch contact bounce.
  - Added a live **Scroll Wheel Encoder Tester** card with directional tick-strip visualization in **Bench → Aim & Switches**, integrated into **Recommended Fixes** and diagnostic text exports.
- **Passive Match Watchdog (`diagnostics/watchdog.rs`, `input_lab_ui.rs`):**
  - Strictly user-started, zero-game-hook background logger sampling Windows total + kernel/DPC CPU %, scheduler wake overshoot, local gateway ICMP RTT, and target internet IP RTT every 2 seconds for up to 60 minutes (1,800 rolling samples).
  - Automatically classifies and timestamps stutter events (**OS Scheduler Stall**, **Kernel/DPC Spike**, **CPU Saturation**, **Local LAN Spike / Router Loss**, **ISP Ping Spike / Packet Loss**) with a dual-series CPU/Ping sparkline and **Copy Log** export in **Bench → System & USB**.
- **Wooting Analog/HE Keyboard Per-Game Profile Switching (`devices/wooting.rs`, `profiles/mod.rs`, `profile_runtime.rs`, `profiles_ui.rs`):**
  - Implemented open-source verified Wooting HID profile switching (`VID 0x31E3` & legacy `0x03EB`, usage pages `0x1337` / `0xFF55`, 8-byte feature reports `[0x00, 0xD0, 0xDA, cmd, ...]`, `0x0B` query / `0x17` `ActivateProfile` / `0x26` `ReloadProfile`, read-back verified, volatile RAM only — zero flash wear).
  - Added optional `wooting_profile` (`Profile 1–4`) per game profile with auto-detection in the Profiles status bar and automatic switching on game focus / manual apply / hotkey cycle.
- **Pure-Win32 Monochrome On-Screen Switch Pill / OSD (`osd.rs`, `settings.rs`, `profile_runtime.rs`, `app.rs`):**
  - Added an opt-in (`osd_pill_enabled`, off by default) click-through, non-focus-stealing Win32 topmost layered pill (`WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED`, zero DirectX/Vulkan hooking, 0% CPU when hidden) that briefly displays profile, DPI, Hz, display, and Wooting switches with smooth fade-out and a **Test Pill** preview button in Settings.
- **Full CLI Parity (`polled-cli`):**
  - Added `--wooting`, `--list-profiles`, `--apply-profile <ID_OR_NAME>`, `--backup [PATH]`, `--restore <PATH>`, `--report-mouse [PATH]`, and `--check-update` (with `--json` support).
- **On-Demand Update Checker & WinGet / Release Packaging (`update.rs`, `app.rs`, `packaging/winget/*`, `scripts/release-checklist.ps1`):**
  - Added a strictly user-triggered GitHub Releases check over native Win32 `WinHTTP` (zero new crates, zero startup/background network calls) in **Settings → Updates** and CLI `--check-update`.
  - Added ready-to-submit WinGet portable manifests (`packaging/winget/k1jaa.Polled*.yaml`, schema `1.9.0`) and a PowerShell release verification & SHA-256 staging script (`scripts/release-checklist.ps1`).

---

## [0.11.0] - 2026-11-25

### Added
- **Per-Game Display Mode Switching (Stretched Resolution & Refresh Rate) + 10-Second Auto-Revert Safety Guard (`win/display.rs`, `profiles/mod.rs`, `profile_runtime.rs`, `profiles_ui.rs`):**
  - Added optional per-game **Display Mode** override (`display_device_name`, `display_width`, `display_height`, `display_refresh_hz`) to `GameProfile` and the **Profiles → Display, Audio & Power** editor tab, enumerating all 32-bit monitor modes with aspect ratio tags (`16:9`, `4:3`, `5:4`, `16:10`, `21:9`).
  - Uses Win32 `ChangeDisplaySettingsExW` pre-validated with `CDS_TEST` and applied strictly as a temporary session-only switch (`CDS_UPDATEREGISTRY` is **never** passed), automatically restoring native desktop resolution & Hz when tabbing out, switching profiles, or exiting Polled.
  - Added a **`Test (10s)`** button with both a live 10-second countdown confirmation modal and an independent 11-second runtime failsafe timer so unsupported or blank monitor modes always auto-revert cleanly.
- **Full Setup Backup & Restore (`backup.rs`, `app.rs`):**
  - Added versioned `polled.setup.v1` JSON export/import bundling `settings.json`, `profiles.json`, and `custom_tweaks.json` (while excluding host-specific `baseline_snapshot.json` and `state.json`, and validating all custom tweaks against protected security paths on import).
  - Accessible directly from **Settings → Export Setup Backup / Restore from Backup...** with one-click **Copy JSON**, **Save to Exports**, and live apply across theme, hotkeys, alerts, profiles, and custom tweaks.
- **GUI "Report Unsupported Mouse" Modal (`hid/dump.rs`, `profiles_ui.rs`):**
  - Added a **Report Mouse** button in the Profiles device bar that generates a ready-to-paste, serial-redacted GitHub Issue markdown report combining `dump_hid_devices()`, `list_usb_topology()`, detected devices, and Windows build version with **Copy Markdown** and **Save to Exports**.
- **GPU & Audio PCI MSI Interrupts Health Check (`pci_gpu_audio_msi`, Total Built-In Checks: 40 → 41):**
  - Added read-only audit of PCI Display adapters (NVIDIA / AMD / Intel GPUs via Display `ClassGUID` & graphics services) and PCI High Definition Audio controllers (`HDAudBus` / `HdAudio` / `MEDIA`) for `MSISupported = 1` vs legacy line-based IRQ sharing.
- **Verified Pro Onboard Mouse Controls (`devices/mod.rs`, `compx.rs`, `compx64.rs`, `mchose.rs`, `vaxee.rs`, `razer.rs`, `endgame_gear.rs`, `profiles_ui.rs`):**
  - **Onboard DPI Stage Slot (`dpi_stage`)**: switch active 1-based onboard DPI stage on CompX 17B (Pulsar X2 CrazyLight, LAMZU, Scyrox, VGN, VXE, G-Wolves, Hitscan), MCHOSE, and VAXEE mice.
  - **Lock DPI Button (`lock_dpi_stages`)**: syncs the profile's target DPI across all onboard DPI stages (diffed per-stage, verified via read-back) on CompX 17B, CompX 64B, MCHOSE, VAXEE, and Razer V4 mice so accidentally pressing the physical DPI button mid-game never shifts sensitivity.
  - **Glass Pad Mode (`glass_mode`)**: exposes verified hardware Glass / high-reflectivity surface sensor mode (`OFFSET_GLASS_MODE`) on supported Endgame Gear v2 / Purple Frost mice.

---

## [0.10.0] - 2026-11-24

### Added
- **Expanded Reversible Low-Latency Tweak Suite (Total Built-In Checks: 32 → 40; Curated Opt-In Tweaks: 3 → 12):**
  - `cpu_core_parking`: sets `CPMINCORES = 100%` on AC power via `powrprof.dll` (leaving DC/battery untouched) to eliminate CPU core-unpark wake stutters.
  - `power_throttling`: sets `PowerThrottlingOff = 1` to stop Windows EcoQoS from down-clocking background audio, voice, capture, and peripheral helpers onto slow cores.
  - `keyboard_repeat`: zero-admin immediate Win32 `SPI_SETKEYBOARDDELAY` / `SPI_SETKEYBOARDSPEED` tweak for shortest repeat delay (`250 ms`) and fastest repeat rate (`31/31`).
  - `fast_startup`: sets `HiberbootEnabled = 0` so Windows Shutdown performs a clean kernel boot instead of hibernating stale driver/DPC state (Sleep untouched).
  - `delivery_optimization`: sets `DODownloadMode = 0` (HTTP-only) to stop Windows Update from silently seeding P2P update uploads and causing mid-match ping spikes.
  - `network_nagle`: sets `TcpAckFrequency = 1` and `TCPNoDelay = 1` on the primary active network adapter for TCP-based games/MMOs.
  - `transparency_effects`: zero-admin immediate `EnableTransparency = 0` tweak disabling Mica/Acrylic multi-pass DWM backdrop blur.
  - `mpo_toggle`: official `OverlayTestMode = 5` troubleshooting override for mixed-Hz dual-monitor stutter / flicker, with clean delete-on-undo.
  - Upgraded `hags` (`HwSchMode`) from report-only to a reversible opt-in tweak backed by `baseline_snapshot.json`.

---

## [0.9.0] - 2026-11-24

### Added
- **Full Monochrome UI Revamp (`theme.rs`, `app.rs`, `profiles_ui.rs`, `input_lab_ui.rs`):**
  - Replaced the double-decker top bar and file-path footer with a sleek **left navigation rail** (`Health` / `Profiles` / `Bench`), live rig status readout, and bottom utility stack (`Startup`, `Settings`, `Dark/Light`), auto-collapsing cleanly on narrow windows.
  - **Calm Health Check Accordion:** Replaced 32 tall double-boxed cards with a unified, searchable, single-line scannable checklist — warnings and `Fix` buttons stand out immediately, and clicking any row smoothly expands full `Current → Recommended` details and explanation.
  - **Decluttered Profiles Tab:** Replaced the dense pill wall with a clean inline rig readout, split the profile editor into **3 tidy sub-tabs** (`Mouse`, `Audio & Power`, `General`), and made the **Activity Log** a collapsible 1-line strip at the bottom.
  - **Organized Bench Workspace:** Split Bench's 12 stacked cards into **4 focused sub-tabs** (`Polling`, `Aim & Switches`, `System & USB`, `Audio & Net`), replaced the 8-button export strip with one **`Export ▾`** menu, and added 4 crisp Hero KPI tiles.
- **Bench v2 Diagnostics (`diagnostics/raw_capture.rs`, `diagnostics/analyzer.rs`, `diagnostics/sensitivity.rs`, `diagnostics/export.rs`):**
  - **Real-Time CPU & OS Scheduler Stall Correlation:** Samples `GetSystemTimes` (total + kernel/DPC CPU %) and scheduler wake overshoots on the decoupled ~30 Hz analyzer thread (zero work on the `WM_INPUT` capture thread) to separate OS/CPU-linked dropouts from wireless/USB transport dropouts — integrated into Polling KPIs, System & USB breakdown, **Recommended Fixes**, and diagnostic reports.
  - **eDPI & `cm/360°` Sensitivity Converter:** Exact calculator for CS2/Source, Valorant, Apex Legends, Overwatch 2/CoD, Rainbow Six Siege, and Fortnite with 1-click DPI-step (`800 → 1600 / 3200`) and cross-game sensitivity conversion.
  - **All-in-One Rig Scorecard PNG (1040×620):** Export a single shareable monochrome bento card combining Windows Health Check, 4K/8K Polling + OS Stalls, USB PnP Topology, WASAPI Audio / OS Timer, and Dual-Hop Network Ping.

---

## [0.8.0] - 2026-11-24

### Added
- **Live Battery Tray Icon & Configurable Notifications (`icon.rs`, `tray.rs`, `notifications.rs`, `settings.rs`):**
  - Procedurally renders a live 32×32 monochrome wireless battery percentage + charge-meter icon directly into the Windows system tray (with semantic green while charging, amber at low battery, and red at critical `<= 5%`), paired with a rich live hover tooltip (`Polled · Mouse · Battery · Polling Hz · Profile`).
  - Added unified **Settings — Tray, Alerts & Global Hotkeys** modal in the top bar: toggle the live battery tray icon, configure low-battery toast alerts and threshold (`5–50%`, default `15%`), and opt into Windows toast notifications when game/desktop profiles switch.
- **Tray Mouse Quick Controls (`tray.rs`, `profile_runtime.rs`):**
  - Added a **Mouse Quick Controls** submenu to the system tray to switch **Polling Rate** (per device capabilities, up to `8000 Hz`) and **Quick DPI** (`400 / 800 / 1600 / 3200`) or trigger **Refresh Mouse & Battery** in one click — routed through `SafeDeviceController` with full validation, diff-before-write, flash-wear protection, and read-back verification.
- **Instant Event-Driven USB Hot-Plug Detection (`hotplug.rs`):**
  - Added a zero-CPU Win32 `WM_DEVICECHANGE` + `RegisterDeviceNotificationW` (`GUID_DEVINTERFACE_HID` & `GUID_DEVINTERFACE_USB_DEVICE`) watcher so plugging/unplugging mice, 4K/8K dongles, wired charging cables, or USB audio interfaces updates the UI and tray within ~300 ms (with a 5 s fallback poll).
- **Read from Mouse in Profile Editor (`profiles_ui.rs`, `profile_runtime.rs`):**
  - Added a one-click **Read from Mouse** button beside **Mouse Target** in the Profile Editor to pull live onboard DPI, polling rate, LOD, debounce, sensor toggles, and sleep timeout directly from your connected mouse into a profile draft.

---

## [0.7.0] - 2026-11-24

### Added
- **True Win32 PnP Device-Tree USB Topology (`win/usb_topology.rs`):**
  - Replaced registry `ParentIdPrefix` guessing with real Win32 `cfgmgr32.dll` PnP device-tree traversal (`CM_Get_Device_ID_ListW`, `CM_Locate_DevNodeW`, `CM_Get_Parent`, `CM_Get_Child`, `CM_Get_Sibling`) paired with `hidapi` product strings.
  - Accurately maps composite `usbccgp` 4K/8K wireless receivers and wired mice to their exact USB port, hub, PCI xHCI host controller, and shared high-bandwidth sibling devices — including vendor ASIO/USB audio interfaces (e.g. Focusrite USB Audio) detected via PnP `MEDIA` / `Camera` / `DiskDrive` classes.
- **Network Latency & Ping Stability Suite (`win/network.rs` + 3 Health Checks + Bench + CLI):**
  - **3 new read-only Network Health Checks (Total Built-In Checks: 29 → 32):**
    - `network_connection`: audits Ethernet vs Wi-Fi and negotiated link speed (flags Wi-Fi or `< 1000 Mbps` Ethernet).
    - `network_nic_power`: inspects active NIC driver flags (`*EEE`, `EEELinkAdvertisement`, `EnableGreenEthernet`, `GigaLite`, `PowerSavingMode`, `*SelectiveSuspend`, `*InterruptModeration`) without ever restarting your network adapter.
    - `network_dns`: audits configured IPv4 DNS resolvers (anycast Cloudflare/Google/Quad9 vs router/ISP DNS).
  - **Bench Dual-Hop Network Jitter & Packet-Loss Probe (5s):** on-demand pure Win32 `iphlpapi.dll` `IcmpSendEcho` benchmark (zero admin) that pings your **local router gateway** and **internet IP (`1.1.1.1` / `8.8.8.8` / `9.9.9.9` / custom IPv4)** simultaneously with sub-millisecond gateway timing, dual-series sparkline plot, automatic local-vs-ISP root-cause diagnosis, **Recommended Fixes** integration, and **Copy Report** export.
  - **CLI `--network` & `--ping <IPV4>`** (both supporting `--json`).

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
- **Actionable Input Lab Fix Advisor & Zero-Jitter Decoupled Capture (`raw_capture.rs`, `analyzer.rs`, `input_lab_ui.rs`):**
  - Decoupled the Win32 `WM_INPUT` timestamping thread from snapshot sorting/publishing via a ~50 ns double-buffered `Vec` swap (eliminating 33 ms `WM_TIMER` self-jitter and fake bunched/dropped packets at 4K/8K Hz).
  - Updated 4K/8K health grading to account for Windows' ~80 µs user-mode `WM_INPUT` scheduling floor while still flagging genuine RF dropouts (`> 1.85×`) and bunched bursts (`< 0.35×`).
  - Added an automatic **Recommended Fixes** card (also appended to **Copy Report**) whenever polling, click debounce, keyboard chatter, USB topology, OS timer, or WASAPI audio tests report `WARN`/`BAD` — cross-referencing live USB hub contention and active Health Check warnings with 1-click **Open Health Check** / **Open Profiles** shortcuts.
- **Hands-Free Timed Input Lab Capture & `Esc` Stop (`input_lab_ui.rs`):**
  - Default capture mode is now a clean **`3s -> 10s` timed run** (selectable `3s -> 5s`, `3s -> 10s`, `3s -> 15s`, or `Manual`): clicking **Start Capture** gives a 3-second prep countdown before recording starts and stops automatically when finished so moving/clicking Stop never pollutes your polling trace or click debounce counter.
  - Press **`Esc`** anytime while focused (or use the global `Ctrl+Alt+I` hotkey) to stop capture or cancel the countdown hands-free.
- **CLI `--audio` Inspection (`polled --audio` / `--audio --json`):**
  - Lists all active playback & capture endpoints with default status, volume/mute, format, WASAPI periods, APO enhancement state, and Bluetooth HFP status.

### Changed
- Renamed the **Input Lab** tab to **Bench** across the GUI, Global Hotkeys modal, clipboard reports, PNG/CSV exports (`polled_bench_*.png`/`.csv`), and docs to reflect its expanded scope (mouse, keyboard, USB topology, OS timer, and WASAPI audio benchmarking).

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
