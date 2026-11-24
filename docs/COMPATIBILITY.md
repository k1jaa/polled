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
| **Razer 90B Mice & Dongles** (Viper V2/V3 Pro, DeathAdder V3/V4 Pro, Basilisk V3, Cobra, Naga, Orochi V2, HyperPolling Dongle, 80+ models) | `1532:*` | Implemented from docs *(unverified)* | Onboard / V2 volatile | DPI (per-model max up to `45000`), Polling (`125–8000 Hz` incl. two-step extended polling on Viper V3 Pro / DA V3 Wired / HyperPolling Dongle) |
| **WLMouse CompX 64B** (Beast X 8K / Mini / Max / Pro, Strider, Sword X, Ying, Miao, Huan) | `36a7:*` *(excl. `a887`)* | Implemented from docs *(unverified)* | Onboard flash | DPI `50–30000` (50 step), Polling `125–8000 Hz`, LOD `0.7/1.0/2.0 mm`, Debounce `0–15 ms` |
| **LAMZU Aurora/8K & ODM CompX 64B** (Maya X, Inca 8K, Paro Aurora, Attack Shark R5 Ultra/R6/R8, Vancer Akitsu, RAWM Leviathan V4 GT, CRDRAKO KO-ONE, LunaFury) | `373e:*`, `37b0:*` | Implemented from docs *(unverified)* | Onboard flash | DPI `50–30000` (50 step), Polling `125–8000 Hz`, LOD `0.7/1.0/2.0 mm`, Debounce `0–15 ms` |
| **Endgame Gear K26 / W26** (OP1 8K / v2 / Purple Frost, XM2 8K / v2, OP1w 4K / v2, XM2w 4K / v2) | `3367:1954–1976` | Implemented from docs *(unverified)* | Onboard flash *(changed-block diffed on 4K wireless)* | DPI (up to `26000`/`30000`, 10/50 step), Polling `1000–8000 Hz` (wired) / `1000–4000 Hz` (wireless), LOD `0.7–2.0 mm` |
| **Finalmouse UltralightX / Starlight X** (ULX / SLX wireless dongles) | `361d:0100`, `0101` | Implemented from docs *(unverified)* | Onboard flash | DPI `400–6400` (200 step), Polling `500–8000 Hz`, LOD `1.0/2.0 mm` |
| **VAXEE Control Center v3.6** (XE / V2 / XE-S, NP-01 / NP-01S V2 / V3 / Ergo, Outset AX, E1, Sora — 18 PIDs) | `2c2e:000b–001e` | Implemented from docs *(unverified)* | Onboard flash | DPI (active stage + preset), Polling `500–4000 Hz`, LOD `1.0/2.0 mm`, Debounce `0–8 ms` |
| **Ninjutso NinjaForce** (Sora V2 legacy, Sora V3, TEN / TEN AIR) | `1915:520c`, `093a:eb11–eb1c` | Implemented from docs *(unverified)* | Onboard flash | DPI `50–30000`, Polling `125–8000 Hz` (model-dependent), LOD `1.0/2.0 mm`, Debounce `0–15 ms` |

> **Intentionally skipped:**
> - `Pulsar X3 LHD CrazyLight Medium` (`3710:5406`, `cid=0x57, mid=0x68`) is explicitly skipped during detection until its register layout is verified.
> - `WLMouse Beast X 4K` (`36a7:a887`) uses a different protocol from CompX 64B and is explicitly excluded so Polled never sends incompatible 64B frames to it.
> - All vendor DFU / firmware-update bootloader PIDs are ignored; Polled never flashes mouse firmware or triggers factory resets.

---

## 3. Known Hardware & Software Compatibility Issues

### Vendor Peripheral Suites Conflicting Over HID Handles
- **Affected:** Pulsar Fusion / Bibimbap, LAMZU Aurora, Logitech G HUB / Onboard Memory Manager, Razer Synapse, Endgame Gear configuration tool, Finalmouse XPANEL, VAXEE Control Center, Ninjutso NinjaForce, WLMouse Hub, Corsair iCUE, SteelSeries GG.
- **Symptom:** Profile reads time out, read-back verification fails, or mouse settings revert seconds after Polled switches them.
- **Workaround:** Fully quit vendor suites from the system tray (or close web configurator tabs like XPANEL / NinjaForce / LAMZU Aurora) when using Polled's **Auto-Switch**. Polled's Health Check flags active heavy vendor suites automatically.

### Wireless Mouse Deep Sleep / Timeout
- **Affected:** All 2.4 GHz wireless mice (Pulsar, LAMZU, Logitech, Razer, WLMouse, Endgame Gear 4K, Finalmouse, VAXEE, Ninjutso).
- **Symptom:** The Profiles tab displays `asleep / move mouse` or a profile switch logs `Timed out waiting for response (move mouse to wake it)`.
- **Cause:** Wireless receivers cannot query or write mouse registers while the mouse MCU is in deep sleep. Moving or clicking the mouse wakes the link immediately.

### Onboard Flash Wear & Burst Rate Limiting
- **Affected:** Mice whose protocol commits changes to onboard flash/EEPROM (Pulsar/LAMZU Compx, CompX 64B, Endgame Gear, Finalmouse, VAXEE, Ninjutso, and Razer `VARSTORE` commands).
- **Behavior:** These protocols commit changed values to onboard flash. Polled minimizes wear by skipping identical values, debouncing alt-tabs (default `1500 ms`), and capping writes to **4 writes per 60 seconds**. If you alt-tab rapidly between games with different mouse settings, the 5th write queues an automatic retry once the 60-second window clears.

### Unverified `Implemented from docs` Firmware Quirks
- **Affected:** All protocol families except Pulsar X2 CrazyLight Mini Wireless (`3710:5406`) and LAMZU 4K Receiver (`3554:f510`), including Logitech HID++ 2.0, Razer 90B, CompX 64B, Endgame Gear, Finalmouse, VAXEE, and Ninjutso.
- **Risk:** Minor firmware revisions can change DPI step encoding, LOD codes, transaction IDs, or report-rate bitmasks.
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
