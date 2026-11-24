# System Modifications & Rollback Reference

> [!IMPORTANT]
> Polled is currently **experimental (`0.x`)** software. This document lists **every** Windows registry value, power policy setting, Win32 API call, local file, and USB HID register that Polled reads or writes so you can audit every change before applying it.

Every mutable fix, optional tweak, and user-defined custom tweak records its **exact original state** (including whether a registry value previously did not exist) into `%LOCALAPPDATA%\Polled\baseline_snapshot.json` **before** modifying Windows.

---

## 1. Built-In Health Checks & Optional Tweaks (25 Total)

### 1.1 Core Mouse & Cursor Input

| Check ID | Mode | Exact Windows Target | Value Written on Fix | Admin / Restart | Rollback Behavior |
| --- | --- | --- | --- | --- | --- |
| `enhance_pointer_precision` | Fixable | Win32 `SystemParametersInfoW(SPI_SETMOUSE)` + `HKCU\Control Panel\Mouse\MouseSpeed` | `threshold1=0, threshold2=0, speed=0` (`SPIF_UPDATEINIFILE \| SPIF_SENDCHANGE`) | No admin / Immediate | Calls `SPI_SETMOUSE` with exact captured `(threshold1, threshold2, speed)` |
| `pointer_speed` | Fixable | Win32 `SystemParametersInfoW(SPI_SETMOUSESPEED)` | `10` (6/11 notch, 1.0× scaling) | No admin / Immediate | Calls `SPI_SETMOUSESPEED` with captured speed (`1..=20`) |
| `custom_accel_curves` | Fixable | `HKCU\Control Panel\Mouse\SmoothMouseXCurve` & `SmoothMouseYCurve` (`REG_BINARY`, 40 B each) | Official Windows 10/11 default 40-byte X & Y curves | No admin / **Sign-out** | Restores base64-captured binary values (or deletes value if absent) |
| `cursor_sanity` | Fixable | `HKCU\Control Panel\Mouse\MouseTrails` & `SnapToDefaultButton` (`REG_SZ`) | `"0"` for both | No admin / Immediate | Restores captured strings (or deletes value if absent) |
| `mouse_data_queue_size` | Fixable | `HKLM\SYSTEM\CurrentControlSet\Services\mouclass\Parameters\MouseDataQueueSize` (`REG_DWORD`) | `100` (only offered when `< 32` is detected) | **UAC (Admin)** / **Reboot** | Restores captured `REG_DWORD` (or deletes value if absent) |

### 1.2 USB, PCIe & Power Delivery

| Check ID | Mode | Exact Windows Target | Value Written on Fix | Admin / Restart | Rollback Behavior |
| --- | --- | --- | --- | --- | --- |
| `usb_selective_suspend` | Fixable | `powrprof.dll` active scheme → Subgroup `2a737441-1930-4402-8d77-b2bebba308a3` → Setting `48e6b7a6-50f5-4782-a5d4-53bb8f07e226` | AC index = `0` (Disabled); DC/battery left untouched | **UAC (Admin)** / Immediate | Restores captured AC & DC power indices on saved scheme GUID |
| `usb_power_management` | **Report-Only** | Reads `HKLM\SYSTEM\CurrentControlSet\Enum\USB\VID_...` & `ROOT_HUB20/30` (`EnhancedPowerManagementEnabled`, `SelectiveSuspendEnabled`, `AllowIdleIrpInD3`, `WDF\IdleInWorkingState`) | *None (never modifies)* | No admin | N/A |
| `usb_xhci_msi` | **Report-Only** | Reads `HKLM\SYSTEM\CurrentControlSet\Enum\PCI\...\Device Parameters\Interrupt Management\MessageSignaledInterruptProperties\MSISupported` where `Service == USBXHCI` | *None (never modifies)* | No admin | N/A |
| `pcie_aspm` | Fixable | `powrprof.dll` active scheme → Subgroup `501a4d13-42af-4429-9fd1-a8218c268e20` → Setting `ee12f906-d277-404b-b6da-e5fa1a576df5` | AC index = `0` (Off); DC/battery left untouched | **UAC (Admin)** / Immediate | Restores captured AC & DC power indices on saved scheme GUID |
| `power_plan` | Fixable | `powrprof.dll` `PowerSetActiveScheme` | Switches active scheme to High Performance (`8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c`) / Ultimate Performance / Balanced (if on Power Saver) | No admin / Immediate | Calls `PowerSetActiveScheme` with original scheme GUID |
| `cpu_processor_state` | Fixable | `powrprof.dll` active scheme → Subgroup `54533251-82be-4824-96c1-47b60b740d00` → Setting `bc5038f7-23e0-4960-96da-33abaf5935ec` (`PROCTHROTTLEMAX`) | AC index = `100` (%); DC/battery left untouched | **UAC (Admin)** / Immediate | Restores captured AC & DC power indices on saved scheme GUID |

### 1.3 Timing, Display & Presentation

| Check ID | Mode | Exact Windows Target | Value Written on Fix | Admin / Restart | Rollback Behavior |
| --- | --- | --- | --- | --- | --- |
| `timer_resolution` | **Report-Only** | Temporarily calls `timeBeginPeriod(1)`, measures `Sleep(1)` wake jitter, then immediately calls `timeEndPeriod(1)` | *None (ephemeral measurement only)* | No admin | N/A |
| `monitor_refresh` | **Report-Only** | Reads active and supported display modes via Win32 `EnumDisplayDevicesW` / `EnumDisplaySettingsW` | *None (never modifies)* | No admin | N/A |
| `windowed_optimizations` | Fixable | `HKCU\Software\Microsoft\DirectX\UserGpuPreferences\DirectXUserGlobalSettings` (`REG_SZ`) | Inserts/updates `SwapEffectUpgradeEnable=1;` while preserving all other semicolon-delimited tokens | No admin / Immediate (new game launches) | Restores exact original `DirectXUserGlobalSettings` string (or deletes value if absent) |
| `game_mode` | Fixable | `HKCU\Software\Microsoft\GameBar\AutoGameModeEnabled` & `AllowAutoGameMode` (`REG_DWORD`) | Sets `AutoGameModeEnabled=1` (and `AllowAutoGameMode=1` if it was `0`) | No admin / Immediate | Restores both captured `REG_DWORD` values (or deletes if absent) |
| `game_bar_capture` | Fixable | `HKCU\Software\Microsoft\Windows\CurrentVersion\GameDVR\AppCaptureEnabled` & `HKCU\System\GameConfigStore\GameDVR_Enabled` (`REG_DWORD`) | `0` for both (does **not** touch Fullscreen Optimizations) | No admin / Immediate | Restores both captured `REG_DWORD` values (or deletes if absent) |
| `hags` | **Report-Only** | Reads `HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers\HwSchMode` | *None (never modifies)* | No admin | N/A |
| `vbs_memory_integrity` | **Report-Only** | Reads `HKLM\SYSTEM\CurrentControlSet\Control\DeviceGuard\EnableVirtualizationBasedSecurity` & `...\Scenarios\HypervisorEnforcedCodeIntegrity\Enabled` | *None (Polled never weakens OS security)* | No admin | N/A |

### 1.4 Optional Reversible Low-Latency Tweaks (Opt-In Only)

Optional tweaks are **never** applied by **Fix Safe Issues** and always show an explicit confirmation warning before applying.

| Check ID | Mode | Exact Windows Target | Value Written on Apply | Admin / Restart | Rollback Behavior |
| --- | --- | --- | --- | --- | --- |
| `mmcss_gaming` | Optional Tweak | `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile` (`SystemResponsiveness`, `NetworkThrottlingIndex`) & `...\Tasks\Games` (`GPU Priority`, `Priority`, `Scheduling Category`, `SFIO Priority`) | `SystemResponsiveness=10`, `NetworkThrottlingIndex=0xFFFFFFFF`, `GPU Priority=8`, `Priority=6`, `Scheduling Category="High"`, `SFIO Priority="High"` | **UAC (Admin)** / **Reboot** | Restores all 6 values to exact captured state (or deletes any previously absent value) |
| `win32_priority_separation` | Optional Tweak | `HKLM\SYSTEM\CurrentControlSet\Control\PriorityControl\Win32PrioritySeparation` (`REG_DWORD`) | `0x26` (38 decimal — Short, Variable, 3:1 High foreground boost) | **UAC (Admin)** / Immediate | Restores captured `REG_DWORD` (or deletes value if absent) |
| `global_timer_requests` | Optional Tweak | `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\kernel\GlobalTimerResolutionRequests` (`REG_DWORD`) | `1` | **UAC (Admin)** / **Reboot** | Restores captured `REG_DWORD` (or deletes value if absent) |

### 1.5 Accessibility & Background Vendor Suites

| Check ID | Mode | Exact Windows Target | Value Written on Fix | Admin / Restart | Rollback Behavior |
| --- | --- | --- | --- | --- | --- |
| `sticky_keys_shortcut` | Fixable | Win32 `SystemParametersInfoW(SPI_SETSTICKYKEYS)` | Clears `SKF_HOTKEYACTIVE` (`0x0004`) bit only; preserves all other flags | No admin / Immediate | Calls `SPI_SETSTICKYKEYS` with exact captured `dwFlags` |
| `filter_keys_shortcut` | Fixable | Win32 `SystemParametersInfoW(SPI_SETFILTERKEYS)` | Clears `FKF_HOTKEYACTIVE` (`0x0004`) bit only | No admin / Immediate | Calls `SPI_SETFILTERKEYS` with exact captured `dwFlags` |
| `mouse_keys_shortcut` | Fixable | Win32 `SystemParametersInfoW(SPI_SETMOUSEKEYS)` | Clears `MKF_HOTKEYACTIVE` (`0x0004`) bit only | No admin / Immediate | Calls `SPI_SETMOUSEKEYS` with exact captured `dwFlags` |
| `vendor_software` | **Report-Only** | Enumerates running processes via `sysinfo` for G Hub, Razer Synapse, iCUE, SteelSeries GG, Armoury Crate | *None (never terminates or uninstalls processes)* | No admin | N/A |

---

## 2. User-Defined Custom Tweaks

Custom tweaks added in the **Health Check → + Custom Tweak** modal are stored in `%LOCALAPPDATA%\Polled\custom_tweaks.json`.

- **Supported types:** `REG_DWORD` and `REG_SZ` under `HKCU` (no admin) or `HKLM` (on-demand UAC).
- **Curated quick-fill presets** (opt-in only, never pre-applied):
  - `Win32PrioritySeparation = 0x2A` (`HKLM\SYSTEM\CurrentControlSet\Control\PriorityControl`)
  - `PowerThrottlingOff = 1` (`HKLM\SYSTEM\CurrentControlSet\Control\Power\PowerThrottling`, reboot)
  - `MouseDataQueueSize = 64` (`HKLM\SYSTEM\CurrentControlSet\Services\mouclass\Parameters`, reboot)
  - `MouseHoverTime = "10"` (`HKCU\Control Panel\Mouse`, no admin)
- **Hard-blocked security/boot paths:** Polled refuses to save or apply custom tweaks targeting:
  - `SAM`, `SECURITY`, `BCD`
  - `SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon`
  - `SOFTWARE\Microsoft\Windows Defender`, `SOFTWARE\Policies\Microsoft\Windows Defender`
  - `SYSTEM\CurrentControlSet\Control\DeviceGuard`, `...\Control\Lsa`, `...\Control\SafeBoot`
- **Rollback:** Before applying any custom tweak, Polled captures the exact prior value (or `null` if absent) in `baseline_snapshot.json`. Clicking **Undo** restores the prior value or deletes the registry value if it did not previously exist. Polled blocks deleting a custom tweak definition until you undo it first.

---

## 3. Local Application Data & Windows Startup

All persistent files created by Polled live under **`%LOCALAPPDATA%\Polled\`**:

| File | Purpose |
| --- | --- |
| `baseline_snapshot.json` | Atomic pre-change backup of every Windows setting modified by Polled, used by **Undo** / **Undo All**. |
| `state.json` | Last scanned Windows build number and previous check severities for Windows Update regression alerts. |
| `profiles.json` | User-created per-game mouse profiles, per-mouse overrides, and focus debounce delay. |
| `settings.json` | UI theme (`dark` / `light`) and whether Auto-Switch starts enabled on launch. |
| `custom_tweaks.json` | User-defined custom reversible registry tweaks. |
| `exports/` | Optional user-triggered Input Lab PNG benchmark cards (`polled_lab_*.png`) and CSV raw interval exports (`polled_lab_*.csv`). |

**Optional Windows Startup Entry (OFF by default):**
- When you toggle **Startup: OFF → ON**, Polled writes:
  - Key: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
  - Value Name: `Polled` (`REG_SZ`)
  - Data: `"<path-to>\polled-gui.exe" --minimized`
- Turning **Startup: ON → OFF** deletes the `Polled` value cleanly. No scheduled tasks or Windows services are ever created.

---

## 4. Per-Game Mouse Profiles & USB HID Writes

When **Auto-Switch** is enabled or you click **Apply Now**, Polled can modify:

1. **Optional Windows Pointer Speed** (only if checked on the profile): calls `SystemParametersInfoW(SPI_SETMOUSESPEED)` with the chosen `1..=20` value.
2. **Optional Windows Power Plan** (only if checked on the profile): calls `powrprof.dll` `PowerSetActiveScheme` to switch between installed `Balanced`, `High Performance`, or `Ultimate Performance` power schemes (skips gracefully if a scheme is unavailable on Modern Standby laptops).
3. **Mouse Hardware Settings over USB HID** (only on matching protocol-family devices):
   - **CompX / Nordic 17-byte protocol** (`usage_page 0xFF02`, `usage 0x0002`, `Report ID 0x08`, `CMD_MEM_SET = 0x07`, Pulsar / LAMZU / VGN / Scyrox V8 / ATK & VXE R1 / G-Wolves XVI:0 / Hitscan Hyperlight):
     - Writes **only changed registers** among: Polling Rate (`0x0000`), Active DPI Stage X/Y (`0x000C + 4*stage`), Lift-Off Distance (`0x000A`), Debounce (`0x00A9`), Motion Sync (`0x00AB`), Angle Snapping (`0x00AF`), and Ripple Control (`0x00B1`). Reads battery via read-only `CMD_BATTERY = 0x04`. Never sends factory reset (`0x09`).
   - **CompX 64-byte Page-Command protocol** (`Report ID 0x00`, WLMouse / LAMZU Maya X & Inca / Attack Shark / Vancer / RAWM / CRDRAKO / LunaFury):
     - Writes active-profile DPI stage table (`page 0x01, cmd 0x01`), polling rate (`cmd 0x00`), LOD (`cmd 0x08`), debounce (`page 0x00, cmd 0x08`), Motion Sync (`0x09`), Angle Snapping (`0x04`), and Ripple Control (`0x0A`) to onboard profile storage. Reads battery via read-only `0x83`.
   - **Razer 90-byte Feature Report protocol** (`VID 0x1532`, `Report ID 0x00`, XOR checksum byte 88):
     - Writes DPI (`class 0x04, id 0x05`, or V4 multi-stage `0x04/0x06` on Viper V4 Pro), legacy (`0x00/0x05`) or extended (`0x00/0x40` + commit selector `0x01` where required) polling rate, and symmetric tracking distance (`0x0B/0x0B`, only on verified LOD-capable models). Reads battery via read-only `0x07/0x80` & `0x07/0x84`. Never writes button remaps or lighting.
   - **Endgame Gear K26/W26 protocol** (`VID 0x3367`, Feature Report `0xA0`/`0xA1`):
     - Updates enabled CPI stages, polling divider, LOD, Motion Sync, Angle Snapping, and Ripple Control via full config store (`0x11`) on wired 8K models or changed-only block writes (`0x14`/`0x15`) on wireless 4K models. Never issues factory reset (`0x13`).
   - **Finalmouse ULX / SLX protocol** (`VID 0x361D`, Output Report `0x04`/`0x01`):
     - Sends DPI (`cmd 16`), polling rate (`cmd 17`, ULX only), LOD (`cmd 21` / `cmd 25`), and Motion Sync (`cmd 18`). Reads battery via read-only telemetry (`cmd 5/37/38`).
   - **VAXEE Control Center v3.6 protocol** (`VID 0x3057`, Feature Report `0x0E` on `0xFF05:0x01`):
     - Updates active-stage DPI (`0x04`), polling rate (`0x07`), and LOD (`0x09`). Reads battery via read-only `0x0B` & `0x10`.
   - **Ninjutso NinjaForce protocol** (`VID 0x093A` Feature Report `0x06`/`0x03`, `VID 0x1915` Feature Report `0x05`/`0x04`):
     - Updates active-stage DPI, polling rate, LOD, and Motion Sync. Reads battery on current-gen models via read-only cmds `17/18`.
   - **MCHOSE V2 / Ultra protocol** (`VID 0x3837` / `0x5253`, Feature Reports `0x11`/`0x12` on `0xFF01:0x01`):
     - Updates DPI stages, polling rate (`0x42`), LOD, debounce, Motion Sync, Angle Snapping, and Ripple Control via read-modify-write config blob (`0x67`/`0x57`). Reads battery via read-only `0x06`.
   - **Incott protocol** (`VID 0x093A:522C/622C`, Feature Report `0x09` on `0xFF05`):
     - Updates polling rate (`0x01`), active-stage DPI (`0x02`), LOD / Ripple / Angle Snap / Motion Sync (`0x04`), and debounce (`0x05`).
   - **Keychron Launcher 8K protocol** (`VID 0x3434`, Output/Input Reports `0xB3`/`0xB4`/`0xB5`/`0xB6` on `0xFFC1:0x01`):
     - Updates active-stage DPI (`0x40`/`0x48`), polling rate (`0x41`/`0x4A`), LOD / Motion Sync / Angle Snap / Ripple (`0x42`), and debounce (`0x43`). Reads battery from read-only status report (`0x06`, byte 19).
   - **Logitech HID++ 2.0 protocol** (`usage_page 0xFF00`, `Report ID 0x11`):
     - Switches `0x8100` (Onboard Profiles) to Host Mode (`0x02`) when present, then updates DPI/LOD via `0x2201` / `0x2202` and report rate via `0x8060` / `0x8061`. Reads battery via read-only `0x1004`/`0x1000`/`0x1001`. **Volatile RAM only** — never writes or erases `0x8100` onboard flash sectors.
4. **Safety invariants (`SafeDeviceController`):**
   - Validates ranges/steps against device capabilities before sending any HID packet.
   - Reads current device settings first and performs **zero writes** if already matching.
   - Enforces a sliding-window write limit (**max 4 writes per 60 seconds**) on onboard-flash devices.
   - Performs mandatory **read-back verification** after every write; on any mismatch, automatically rolls back to the pre-write settings and reports an error.

---

## 5. Rollback & Manual Recovery

### Recommended: Restore via Polled
- **GUI:** Click **Undo** on any individual check card, or **Undo All** in the top bar (Polled will prompt for UAC only if an `HKLM` or power-policy setting is in your snapshot).
- **CLI:**
  ```powershell
  .\polled.exe --undo <check_id>
  .\polled.exe --undo-all
  ```

### Manual Recovery Without Polled
`%LOCALAPPDATA%\Polled\baseline_snapshot.json` is plain, human-readable JSON. Even if you delete Polled, you can inspect every original value directly in Notepad or PowerShell:

```powershell
Get-Content "$env:LOCALAPPDATA\Polled\baseline_snapshot.json"
```

- For `registry_dword` / `registry_string` entries where `"value": null`, deleting that value name in `regedit.exe` restores Windows to its default state.
- For `power_setting` entries, you can re-apply the saved AC/DC index using `powercfg /setacvalueindex <scheme_guid> <subgroup_guid> <setting_guid> <value>` followed by `powercfg /setactive <scheme_guid>`.
- **Before applying system-wide (`HKLM`), reboot-requiring, or custom tweaks**, we still recommend creating a **Windows System Restore Point** (`Create a restore point` in the Start Menu).

---

## 6. What Polled Never Does

- Never installs or loads a custom kernel driver (`.sys`).
- Never injects DLLs, hooks DirectX/Vulkan, reads game memory, or inspects anti-cheat processes.
- Never flashes or updates mouse firmware, and never issues HID factory-reset commands.
- Never disables Windows Defender, VBS, HVCI (Memory Integrity), Firewall, or Windows Update.
- Never makes outbound network connections or collects telemetry.
