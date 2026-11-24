# Hardware Compatibility & Known Issues

> [!WARNING]
> Polled is currently **experimental (`0.x`)**. Only mice marked **Tested on hardware** have been physically verified end-to-end. Models marked **Implemented from docs** follow documented open-source protocols and prompt for explicit confirmation before the first write in a session.

---

## 1. Operating System Support

| OS | Status | Notes |
| --- | --- | --- |
| **Windows 11 (22H2 / 23H2 / 24H2+)** | Supported (Primary) | All 25 health checks, Input Lab (up to 8,000 Hz), and Per-Game Profiles supported. |
| **Windows 10 (1903+)** | Supported | `windowed_optimizations` and `global_timer_requests` are Windows 11-specific features and are safe to leave untouched on Windows 10. |
| **Windows 7 / 8 / 8.1** | Unsupported | Not tested; Win32 / power / Raw Input behavior is unsupported. |
| **Linux / macOS** | Unsupported | Polled targets Windows APIs directly. |

---

## 2. Mouse Support Matrix

| Device / Protocol Family | USB VID:PID | Status | Memory Mode | Configurable Settings |
| --- | --- | --- | --- | --- |
| **Pulsar X2 CrazyLight Mini Wireless** (`cid=0x57, mid=0x01`) | `3710:5406` | **Tested on hardware** | Onboard flash *(diffed & rate-limited)* | DPI `10–32000` (10/50/100 steps), Polling `125–8000 Hz`, LOD `0.7/1.0/2.0 mm`, Debounce `0–20 ms` |
| **LAMZU 4K Wireless Receiver** | `3554:f510` | **Tested on hardware** | Onboard flash *(diffed & rate-limited)* | DPI `50–26000` (50 step), Polling `125–4000 Hz`, LOD `1.0/2.0 mm`, Debounce `0–15 ms` |
| **Pulsar X2 CrazyLight (Standard) Wireless** (`cid=0x57, mid=0x03`) | `3710:5406` | Implemented from docs *(unverified)* | Onboard flash | Same as CrazyLight Mini |
| **Pulsar X2 CrazyLight Wired** | `3710:3414` | Implemented from docs *(unverified)* | Onboard flash | Same as wireless, capped to `1000 Hz` when wired |
| **Pulsar Nordic Series** (X2A, Xlite V3, etc.) | `3554:f507`, `f508`, `f509` | Implemented from docs *(unverified)* | Onboard flash | DPI `50–26000` (50 step), Polling `125–1000 Hz`, LOD `1.0/2.0 mm`, Debounce `0–30 ms` |
| **LAMZU 1K Wireless / Wired** | `3554:f50d`, `f50f` | Implemented from docs *(unverified)* | Onboard flash | DPI `50–26000` (50 step), Polling `125–1000 Hz`, LOD `1.0/2.0 mm`, Debounce `0–15 ms` |
| **Generic Compx CX52850 Mice** (`0xFF02` Report `0x08` 17B) | `3554:*` | Implemented from docs *(unverified)* | Onboard flash | Auto-detected capabilities; DPI `50–26000` (50 step), Polling `1K/4K/8K` |
| **Logitech HID++ 2.0 Mice** (G Pro X Superlight 1/2, G305, G502, G703, G403, G903, MX) | `046d:*` | Implemented from docs *(unverified)* | **Volatile RAM** *(zero flash wear)* | DPI (`0x2201`/`0x2202`), Polling (`0x8060`/`0x8061`), LOD (`0x2202` if supported); Debounce not exposed by HID++ |

> **Intentionally skipped:** `Pulsar X3 LHD CrazyLight Medium` (`3710:5406`, `cid=0x57, mid=0x68`) is explicitly skipped during detection until its register layout is verified.

---

## 3. Known Hardware & Software Compatibility Issues

### Vendor Peripheral Suites Conflicting Over HID Handles
- **Affected:** Pulsar Fusion / Bibimbap, LAMZU Aurora, Logitech G HUB / Onboard Memory Manager, Razer Synapse, Corsair iCUE, SteelSeries GG.
- **Symptom:** Profile reads time out, read-back verification fails, or mouse settings revert seconds after Polled switches them.
- **Workaround:** Fully quit vendor suites from the system tray (or switch Logitech mice to onboard/closed G HUB) when using Polled's **Auto-Switch**. Polled's Health Check flags active heavy vendor suites automatically.

### Wireless Mouse Deep Sleep / Timeout
- **Affected:** All 2.4 GHz wireless mice (Pulsar, LAMZU, Logitech).
- **Symptom:** The Profiles tab displays `asleep / move mouse` or a profile switch logs `Timed out waiting for response (move mouse to wake it)`.
- **Cause:** Wireless receivers cannot query or write mouse registers while the mouse MCU is in deep sleep. Moving or clicking the mouse wakes the link immediately.

### Onboard Flash Wear & Burst Rate Limiting (Compx / Nordic Mice)
- **Affected:** All Pulsar and LAMZU mice using the Compx/Nordic protocol (`3710:*`, `3554:*`).
- **Behavior:** These mice have no documented volatile RAM-only write command; every changed setting writes to onboard flash. Polled minimizes wear by skipping identical values, debouncing alt-tabs (default `1500 ms`), and capping writes to **4 writes per 60 seconds**. If you alt-tab rapidly between games with different mouse settings, the 5th write queues an automatic retry once the 60-second window clears.

### Unverified `Implemented from docs` Firmware Quirks
- **Affected:** Logitech HID++ 2.0 mice and Compx/Nordic models other than Pulsar X2 CrazyLight Mini Wireless and LAMZU 4K.
- **Risk:** Minor firmware revisions can change DPI step encoding, LOD codes, or report-rate bitmasks.
- **Mitigation:**
  1. Run `.\polled.exe --detect` first (100% read-only) to verify that Polled accurately reads your current DPI, polling rate, LOD, and debounce.
  2. When testing a write for the first time, Polled shows a warning dialog and automatically rolls back if read-back verification detects a mismatch.

### Laptops & Modern Standby (S0ix) Power Plans
- **Behavior:** Windows Modern Standby laptops typically expose only the **Balanced** power plan (High Performance is hidden by the OS). Polled detects this and marks `power_plan` as `OK` rather than trying to force an unavailable scheme.
- **Battery impact:** Disabling **USB Selective Suspend**, **PCIe ASPM**, or fixing **CPU Maximum Processor State** only modifies **AC (plugged-in)** power indices; **DC (battery)** values are left untouched.

### 4,000 Hz / 8,000 Hz USB Port & Host Controller Quirks
- **Symptom:** Input Lab shows high dropout counts or bunched packets (`< 0.5× nominal`) at 4K/8K Hz.
- **Common causes:** Plugging a 4K/8K dongle into an unpowered hub, monitor USB pass-through, or front-panel USB port; 2.4 GHz Wi-Fi router interference; or PCIe ASPM / USB Selective Suspend enabled. Plug directly into a rear motherboard CPU-lane USB port and keep the dongle extender within ~20 cm of the mousepad.

---

## 4. Reporting Compatibility Results or Requesting a Mouse

Help move devices from `Implemented from docs` to `Tested on hardware` — or request a new mouse family — by opening a GitHub Issue with:

1. Mouse model, connection type (wired / 1K / 4K / 8K dongle), and firmware version (if known).
2. Output of the read-only HID dumper (serial numbers are redacted by default):
   ```powershell
   .\polled.exe --dump
   .\polled.exe --detect
   ```
3. If testing profile switching, click **Copy Log** in the **Profiles → Activity Log** panel and attach the log.
