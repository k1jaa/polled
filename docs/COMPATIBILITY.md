# Hardware Compatibility & Known Issues

> [!WARNING]
> Polled is currently **experimental (`0.x`)**. Only mice marked **Tested on hardware** have been physically verified end-to-end. Models marked **Implemented from docs** follow documented open-source protocols and prompt for explicit confirmation before the first write in a session.

---

## 1. Operating System Support

| OS | Status | Notes |
| --- | --- | --- |
| **Windows 11 (22H2 / 23H2 / 24H2+)** | Supported (Primary) | All 41 health checks (including 13 opt-in reversible tweaks), Bench (up to 8,000 Hz, scroll wheel encoder tester, passive match watchdog, WASAPI audio & dual-hop ICMP ping benchmarks), Per-Game Profiles (mouse, Wooting keyboard, temporary display mode, power & audio), and optional click-through Win32 OSD switch pill supported. |
| **Windows 10 (1903+)** | Supported | `windowed_optimizations` and `global_timer_requests` are Windows 11-specific features and are safe to leave untouched on Windows 10. |
| **Windows 7 / 8 / 8.1** | Unsupported | Not tested; Win32 / power / Raw Input behavior is unsupported. |
| **Linux / macOS** | Unsupported | Polled targets Windows APIs directly. |

---

## 2. Mouse Support Matrix

| Device / Protocol Family | USB VID:PID | Status | Memory Mode | Configurable Settings |
| --- | --- | --- | --- | --- |
| **Pulsar X2 CrazyLight Mini Wireless** (`cid=0x57, mid=0x01`) | `3710:5406` | **Tested on hardware** | Onboard flash *(diffed & rate-limited)* | DPI `10–32000` (10/50/100 steps), DPI Stage / Lock DPI Button, Polling `125–8000 Hz`, LOD `0.7/1.0/2.0 mm`, Debounce `0–20 ms`, Motion Sync / Angle Snap / Ripple, Sleep, Live Battery % |
| **LAMZU 4K Wireless Receiver** | `3554:f510` | **Tested on hardware** | Onboard flash *(diffed & rate-limited)* | DPI `50–26000` (50 step), DPI Stage / Lock DPI Button, Polling `125–4000 Hz`, LOD `1.0/2.0 mm`, Debounce `0–15 ms`, Motion Sync / Angle Snap / Ripple, Sleep, Live Battery % |
| **Pulsar X2 CrazyLight (Standard) Wireless** (`cid=0x57, mid=0x03`) | `3710:5406` | Implemented from docs *(unverified)* | Onboard flash | Same as CrazyLight Mini |
| **Pulsar X2 CrazyLight Wired** | `3710:3414` | Implemented from docs *(unverified)* | Onboard flash | Same as wireless, capped to `1000 Hz` when wired |
| **Pulsar Fusion 2 / Sonix 64B** (8K/1K Wireless Dongles, Feinmann F01 8K, X3 / X3 LHD, Xlite v4, X2A v3, X2A / X2H / X2 / Xlite Wired) | `3710:5403`, `5402`, `5404`, `3409`, `3410`, `3401`, `3404`, `1401–1404` | Implemented from docs *(unverified)* | Onboard flash | DPI `50/100–32000` (10 step on XS-1, 50/100 step on wired PAW3395), X/Y DPI, DPI Stage / Lock, Polling `125–8000 Hz` (8K dongles) / `125–1000 Hz` (1K/wired), LOD `0.7/1.0/2.0 mm` (XS-1) / `1.0/2.0 mm`, Debounce `0–15 ms`, Motion Sync / Angle Snap / Ripple, Sleep, Live Battery % *(queries paired mouse on 1K/8K dongles)* |
| **Pulsar Nordic Series** (X2A, Xlite V3, etc.) | `3554:f507`, `f508`, `f509` | Implemented from docs *(unverified)* | Onboard flash | DPI `50–26000` (50 step), Polling `125–1000 Hz`, LOD `1.0/2.0 mm`, Debounce `0–30 ms`, Sensor toggles, Live Battery % |
| **LAMZU 1K Wireless / Wired** | `3554:f50d`, `f50f` | Implemented from docs *(unverified)* | Onboard flash | DPI `50–26000` (50 step), Polling `125–1000 Hz`, LOD `1.0/2.0 mm`, Debounce `0–15 ms`, Sensor toggles, Live Battery % |
| **VGN Dragonfly F2 Master+ / Scyrox V8 / ATK & VXE R1 CompX** | `3554:fb56`, `fb57`, `f5f7`, `f58a–f58f` | Implemented from docs *(unverified)* | Onboard flash | DPI up to `26000–30000` (50 step), Polling up to `8000 Hz` (wireless), LOD `0.7/1.0/2.0 mm`, Debounce, Sensor toggles, Live Battery % |
| **G-Wolves XVI:0** (HTX Ultra, HTS Plus/Pro, Lycan, Fenrir, HTX Mini, WARG) | `33e4:*` (`0xFF02`) | Implemented from docs *(unverified)* | Onboard flash | DPI `50–30000` (50 step), Polling up to `8000 Hz`, LOD `0.7/1.0/2.0 mm`, Debounce `0–15 ms`, Sensor toggles, Live Battery % |
| **Hitscan Hyperlight** (Wired, 1K, 8K Receiver) | `3770:0100`, `0200`, `0300` | Implemented from docs *(unverified)* | Onboard flash | DPI `50–26000` (50 step), Polling `125–8000 Hz` (model-dependent), LOD `1.0/2.0 mm`, Debounce `0–15 ms`, Sensor toggles, Live Battery % |
| **Generic Compx CX52850 Mice** (`0xFF02` Report `0x08` 17B) | `3554:*` *(excl. `f515`/`f517`)* | Implemented from docs *(unverified)* | Onboard flash | Auto-detected capabilities; DPI `50–26000` (50 step), Polling `1K/4K/8K`, Sensor toggles, Live Battery % |
| **Logitech HID++ 2.0 Mice** (G Pro X Superlight 1/2, G305, G502, G703, G403, G903, MX) | `046d:*` | Implemented from docs *(unverified)* | **Volatile RAM** *(zero flash wear)* | DPI (`0x2201`/`0x2202`), Polling (`0x8060`/`0x8061`), LOD (`0x2202` if supported), Live Battery % (`0x1004`/`0x1000`/`0x1001`) |
| **Razer 90B Mice & Dongles** (**Viper V4 Pro**, Viper V2/V3 Pro, DeathAdder V3/V4 Pro, Basilisk V3, Cobra, Naga, Orochi V2, HyperPolling Dongle, 80+ models) | `1532:*` *(incl. `00e5`/`00e6`)* | Implemented from docs *(unverified)* | Onboard / V2 volatile | DPI (up to `50000` on Viper V4 Pro), Polling (`125–8000 Hz`), Live Battery % (`0x07/0x80` & `0x84`) |
| **WLMouse CompX 64B** (Beast X 8K / Mini / Max / Pro, Strider, Sword X, Ying, Miao, Huan) | `36a7:*` *(excl. `a887`)* | Implemented from docs *(unverified)* | Onboard flash | DPI `50–30000` (50 step), Polling `125–8000 Hz`, LOD `0.7/1.0/2.0 mm`, Debounce `0–15 ms`, Sensor toggles, Live Battery % |
| **LAMZU Aurora/8K & ODM CompX 64B** (Maya X, Inca 8K, Paro Aurora, Attack Shark R5 Ultra/R6/R8, Vancer Akitsu, RAWM Leviathan V4 GT, CRDRAKO KO-ONE, LunaFury) | `373e:*`, `37b0:*` | Implemented from docs *(unverified)* | Onboard flash | DPI `50–30000` (50 step), Polling `125–8000 Hz`, LOD `0.7/1.0/2.0 mm`, Debounce `0–15 ms`, Sensor toggles, Live Battery % |
| **Endgame Gear K26 / W26** (OP1 8K / v2 / Purple Frost, XM2 8K / v2, OP1w 4K / v2, XM2w 4K / v2) | `3367:1954–1976` | Implemented from docs *(unverified)* | Onboard flash *(changed-block diffed on 4K wireless)* | DPI (up to `26000`/`30000`, 10/50 step), Polling `1000–8000 Hz` (wired) / `1000–4000 Hz` (wireless), LOD `0.7–2.0 mm`, Motion Sync / Angle Snap / Ripple, **Glass Pad Mode** *(v2 / Purple Frost)* |
| **Finalmouse UltralightX / Starlight X** (ULX / SLX wireless dongles) | `361d:0100`, `0101` | Implemented from docs *(unverified)* | Onboard flash | DPI `400–6400` (200 step), Polling `500–8000 Hz`, LOD `1.0/2.0 mm`, Motion Sync, Live Battery % |
| **VAXEE Control Center v3.6** (XE / V2 / XE-S, NP-01 / NP-01S V2 / V3 / Ergo, Outset AX, E1, Sora — 18 PIDs) | `3057:0005`, `1001–1015`, `2001–2002` | Implemented from docs *(unverified)* | Onboard flash | DPI (active stage + preset), Polling `500–4000 Hz`, LOD `1.0/2.0 mm`, Debounce `0–8 ms`, Live Battery % |
| **Ninjutso NinjaForce** (Sora V2 legacy, Sora V3, TEN / TEN AIR) | `1915:520c`, `093a:eb11–eb1c` | Implemented from docs *(unverified)* | Onboard flash | DPI `50–30000`, Polling `125–8000 Hz` (model-dependent), LOD `1.0/2.0 mm`, Debounce `0–15 ms`, Motion Sync, Live Battery % *(current-gen)* |
| **MCHOSE V2 / Ultra** (A7 V2 Pro / Pro+ / Ultra / Ultra+, A7 Ultra, L7 Pro / Pro+) | `3837:*`, `5253:1020/1011` | Implemented from docs *(unverified)* | Onboard flash | DPI `50–30000/42000` (50 step), Polling `125–8000 Hz`, LOD `0.7/1.0/2.0 mm`, Debounce `0–20 ms`, Motion Sync / Angle Snap / Ripple, Live Battery % |
| **Incott** (G23 / G23 V2 / G24 / Ghero / Zero 29 / Zero 39) | `093a:522c`, `622c` | Implemented from docs *(unverified)* | Onboard flash | DPI `50–32000/45000` (50 step), Polling `125–8000 Hz` (wireless) / `1000 Hz` (wired), LOD `0.7/1.0/2.0 mm`, Debounce `0–30 ms`, Motion Sync / Angle Snap / Ripple |
| **Keychron Launcher 8K** (M1–M8, G3–G10, LM7 wired + Link / Ultra-Link / TurboLink receivers) | `3434:*` (`0xFFC1`) | Implemented from docs *(unverified)* | Onboard flash | DPI up to `26000/30000` (50 step), Polling `125–8000 Hz`, LOD `0.7/1.0/2.0 mm`, Debounce `0–20 ms`, Motion Sync / Angle Snap / Ripple, Live Battery % |

---

## 2b. Keyboard Support Matrix

| Brand / Protocol Family | USB VID:PID & Interface | Status | Memory Mode | Configurable & Readable Features |
| --- | --- | --- | --- | --- |
| **Wooting** (60HE, **60HE+**, 60HE v2, 80HE, Two HE, UwU, One / Two — 13 base PIDs) | `31e3:*`, `03eb:ff01/ff02` (`0xFF55` / `0x1337` + `0xFF53`/`0xFF54`) | **Tested on hardware (60HE+ `31e3:1322`, fw `v2.13.2`)** | **Volatile RAM** profile switch + confirm-gated ARM flash writes | Real profile names (`0x37`), RAM slot switching, interactive 60%/80HE/ISO keymap, live analog depth stream & `A <-> D` counter-strafe, base & per-key actuation (`OUT 0x11`/`0x15`), Global Settings (`OUT 0x17`), AKC/SOCD, RGB Core & flash diagnostics |
| **Wuque Studio / SparkLink HE** (BABAO60 HE, BABAO80 HE, ZOOM TIGA HE) | `1ca6:1b10/1b0a/1b09`, `36b5:75d4/75d2` (`0xFFB0:0x01` / `0xFF80:0x01`) | Implemented from docs *(unverified)* | Onboard config *(confirm-gated + read-back)* | Protocol/app/PCB info, switch tech, Config slots 1–4, polling rate (`250–8000 Hz`), Win/Mac mode, RT min precision, sleep timer & shake optimization |
| **Keychron Hall Effect / TMR** (160+ Q/K/V/J/C HE & Lemokey models) | `3434:*` cataloged HE PIDs (`0xFF60:0x61`, `KC_HE 0xA9 v1–v5`) | Implemented from docs *(unverified)* | Onboard profile buffer *(confirm-gated + read-back)* | Profile names & slot switch (`0x11`), global actuation & Rapid Trigger (`0x14` + `0x1F`), travel step unit, DKS & Snap Tap slot counts |
| **SteelSeries Apex Pro TKL** (2023 wired `1628`, Gen 3 `1642`, Wireless `1630`/`1632`) | `1038:1628/1642/1630/1632` (`0xFFC0:0x0001`) | Implemented from docs *(unverified)* | **Live RAM** hall frames *(confirm-gated)* | Firmware, 12.5 KB STM32 CRC-32 profile decode, global actuation (`0x38 0x61`), Rapid Trigger release mode (`0x38 0x62`), Rapid Tap SOCD (`0x38 0x66`) |
| **Razer Keyboards** (42 standalone models: Huntsman V3 Pro / TKL / Mini / 8KHz, BlackWidow V4, DeathStalker V2, etc.) | `1532:*` strict keyboard PID catalog (`0x0001:0x0002`, 90B feature) | Implemented from docs *(unverified)* | Onboard / RAM *(confirm-gated + read-back)* | Firmware, backlight brightness, polling rate (`125–8000 Hz`), Huntsman V3 firmware Rapid Trigger (`02:1A`/`02:9A`). Strictly excludes mice/laptops. |
| **Arbiter Studio** (Polar 65 / 65+ / 65 PRO / 75 PRO, Nimbus 65) | `1a81:2072/2077/208e`, `371c:a001/a003/a005/a006/a009` (`0xFF01`/`0xFF02`) | Implemented from docs *(unverified)* | Onboard memory *(confirm-gated + read-back)* | Firmware, full travel table read/write (`0.1–3.8 mm` actuation / `0.1–2.4 mm` RT), PRO profile slots 1–3, SuperTap (SOCD) |
| **Logitech HID++ 2.0 Keyboards** (PRO X TKL RAPID, PRO X 60, G915 X, G715, G515 wired/LIGHTSPEED/BT) | `046d:*` strict keyboard PID catalog + paired LIGHTSPEED/Bolt/Unifying receiver check (`0xFF00`/`0xFF43`) | Implemented from docs *(unverified)* | Volatile RAM / HID++ *(confirm-gated + read-back)* | Firmware, unified battery % (`0x1004`), extended polling rate (`0x8061`), backlight (`0x8040`), `0x1B08` analog switches (Rapid Trigger & Key Priority / Snap Tap) |
| **Kochi Hummingbird 68** | `1ca2:0901` (`0xFFA0:0x0001`) | Implemented from docs *(unverified)* | Onboard *(confirm-gated + read-back)* | Verified `KOCHI68` identity check + Windows/macOS host mode toggle |
| **Geonworks Venom 8K** (VENOM-60MX 6.25U/7U, VENOM-80MX 6.25U/7U, WISH45-8K) | `0483:5204/5205/5206/5207/520b` (`0xFF60:0x61`, `0xB0` + VIA) | Implemented from docs *(unverified)* | EEPROM debounce *(confirm-gated + read-back)* | `baram-qmk-8k` firmware version, matrix size, USB SOF heartbeat & re-enum/stall counters, plus VIA channel 13 Gaming/Typing debounce (`5–40 ms`) |
| **Universal QMK / VIA** (any unclaimed QMK/VIA custom keyboard) | Any unclaimed VID:PID (`0xFF60:0x61`) | Implemented from docs *(read-only)* | Read-only | VIA protocol version, layer count, firmware/keycodes rev, board uptime, macro slot count & buffer size (zero destructive opcodes) |

> **Not yet supported (closed OEM firmware without public specs):**
> - **Geonworks Venom 60 HE / TKL HE** (`venom.how`, RISC-V32 firmware) and **Tekkusai × Luminkey Titan 75 HE** (`tekkus.ai`) use closed proprietary WebHID firmware with no public open-source protocol specification yet. Polled never guesses undocumented HID write bytes; owners can click **Report Keyboard** in the **Keyboard** tab (or run `polled --dump`) to contribute a serial-redacted HID report.
> - **Destructive commands never encoded:** Across every keyboard backend, factory reset, bootloader jump, firmware flash, and EEPROM clear opcodes are strictly omitted.

> **Intentionally skipped:**
> - `Pulsar X3 LHD CrazyLight Medium` (`3710:5406`, `cid=0x57, mid=0x68`) is explicitly skipped during detection until its register layout is verified.
> - `WLMouse Beast X 4K` (`36a7:a887`) uses a different protocol from CompX 64B and is explicitly excluded.
> - `Scyrox V8S` / 3955 flash-6912 PIDs (`3554:f515`, `3554:f517`), `MCHOSE MagDock` (`3837:1012`), `MCHOSE A7 V3` (`3837:4026–4038`), and `Keychron 8K Nordic` (`3434:d077`, `2801`, `d05b`, `d078`) are explicitly excluded so incompatible frames are never sent to them.
> - `Glorious Model O 2 / I 2` is not included because its PixArt `0x03` HID interface is write-only and cannot satisfy Polled's mandatory read-back verification invariant.
> - All vendor DFU / firmware-update bootloader PIDs are ignored; Polled never flashes mouse firmware or triggers factory resets.

---

## 3. Known Hardware & Software Compatibility Issues

### Vendor Peripheral Suites Conflicting Over HID Handles
- **Affected:** Pulsar Fusion / Bibimbap, LAMZU Aurora, Logitech G HUB / Onboard Memory Manager, Razer Synapse, Endgame Gear configuration tool, Finalmouse XPANEL, VAXEE Control Center, Ninjutso NinjaForce, WLMouse Hub, MCHOSE M-HUB, Incott / Keychron Launcher, VGN / ATK V-HUB, G-Wolves web tool, Corsair iCUE, SteelSeries GG.
- **Symptom:** Profile reads time out, read-back verification fails, or mouse settings revert seconds after Polled switches them.
- **Workaround:** Fully quit vendor suites from the system tray (or close web configurator tabs like XPANEL / NinjaForce / LAMZU Aurora / Keychron Launcher) when using Polled's **Auto-Switch**. Polled's Health Check flags active heavy vendor suites automatically.

### Wireless Mouse Deep Sleep / Timeout
- **Affected:** All 2.4 GHz wireless mice (Pulsar, LAMZU, VGN, Scyrox, ATK/VXE, G-Wolves, Hitscan, Logitech, Razer, WLMouse, Endgame Gear 4K, Finalmouse, VAXEE, Ninjutso, MCHOSE, Incott, Keychron).
- **Symptom:** The Profiles tab displays `asleep / move mouse` or a profile switch logs `Timed out waiting for response (move mouse to wake it)`.
- **Cause:** Wireless receivers cannot query or write mouse registers while the mouse MCU is in deep sleep. Moving or clicking the mouse wakes the link immediately.

### Onboard Flash Wear & Burst Rate Limiting
- **Affected:** Mice whose protocol commits changes to onboard flash/EEPROM (Pulsar/LAMZU/VGN/Scyrox/G-Wolves/Hitscan CompX 17B, CompX 64B, Endgame Gear, Finalmouse, VAXEE, Ninjutso, MCHOSE, Incott, Keychron, and Razer `VARSTORE` commands).
- **Behavior:** These protocols commit changed values to onboard flash. Polled minimizes wear by skipping identical values, debouncing alt-tabs (default `1500 ms`), and capping writes to **4 writes per 60 seconds**. If you alt-tab rapidly between games with different mouse settings, the 5th write queues an automatic retry once the 60-second window clears.

### Unverified `Implemented from docs` Firmware Quirks
- **Affected:** All protocol families except Pulsar X2 CrazyLight Mini Wireless (`3710:5406`) and LAMZU 4K Receiver (`3554:f510`), including Logitech HID++ 2.0, Razer 90B, CompX 64B, Endgame Gear, Finalmouse, VAXEE, Ninjutso, MCHOSE, Incott, Keychron, and Wave 2 CompX 17B models.
- **Risk:** Minor firmware revisions can change DPI step encoding, LOD codes, transaction IDs, or report-rate bitmasks.
- **Mitigation:**
  1. Run `.\polled.exe --detect` first (100% read-only) to verify that Polled accurately reads your current DPI, polling rate, LOD, and debounce.
  2. When testing a write for the first time, Polled shows a warning dialog and automatically rolls back if read-back verification detects a mismatch.

### Laptops & Modern Standby (S0ix) Power Plans
- **Behavior:** Windows Modern Standby laptops typically expose only the **Balanced** power plan (High Performance is hidden by the OS). Polled detects this and marks `power_plan` as `OK` rather than trying to force an unavailable scheme.
- **Battery impact:** Disabling **USB Selective Suspend**, **PCIe ASPM**, or fixing **CPU Maximum Processor State** only modifies **AC (plugged-in)** power indices; **DC (battery)** values are left untouched.

### 4,000 Hz / 8,000 Hz USB Port & Host Controller Quirks
- **Symptom:** Bench shows high dropout counts or bunched packets (`< 0.35× nominal`) at 4K/8K Hz.
- **Common causes:** Plugging a 4K/8K dongle into an unpowered hub, monitor USB pass-through, or front-panel USB port; 2.4 GHz Wi-Fi router interference; or PCIe ASPM / USB Selective Suspend enabled. Plug directly into a rear motherboard CPU-lane USB port and keep the dongle extender within ~20 cm of the mousepad.

---

## 4. Reporting Compatibility Results or Requesting a Mouse

Help move devices from `Implemented from docs` to `Tested on hardware` — or request a new mouse family — by opening a GitHub Issue with:

1. Mouse model, connection type (wired / 1K / 4K / 8K dongle), and firmware version (if known).
2. Click **Report Mouse** in **Profiles** (copies a serial-redacted HID + USB topology markdown bundle), or run the CLI read-only dumper:
   ```powershell
   .\polled.exe --dump
   .\polled.exe --detect
   ```
3. If testing profile switching, click **Copy Log** in the **Profiles → Activity Log** panel and attach the log.
