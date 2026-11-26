// Polled - Windows input health check & per-game peripheral profiles
// Copyright (C) 2026 k1jaa
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pure-Rust Windows Audio (`MMDevice` + `WASAPI` + `IAudioEndpointVolume` + `IPolicyConfig`)
//! helpers for:
//! - Enumerating active playback & capture endpoints, mix formats, WASAPI periods, and APO flags
//! - Switching default playback / capture devices (user-mode COM, zero admin)
//! - Reading / setting endpoint volume (including per-game volume presets & mic gain lock)
//! - Running an on-demand silent 1-second WASAPI audio engine latency & glitch benchmark

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::time::{Duration, Instant};

use windows::core::{Interface, GUID, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, PROPERTYKEY, RPC_E_CHANGED_MODE, S_FALSE, S_OK,
};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, eConsole, eMultimedia, eRender, IAudioClient, IAudioClient3,
    IAudioRenderClient, IAudioSessionControl2, IAudioSessionManager2, IMMDevice,
    IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{
    CreateEventW, OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject,
    PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::checks::Severity;
use crate::error::{PolledError, Result};
use crate::win::{registry, AudioEndpointInfo, AudioFlow, AudioLatencyBenchmark, RegHive};

/// `PKEY_Device_FriendlyName` (`{a45c254e-df1c-4efd-8020-67d146a850e0}`, pid 14).
const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

/// `PKEY_AudioEndpoint_Disable_SysFx` in `FxProperties` registry (`1` = disabled, `0` = enabled).
const FX_DISABLE_SYSFX_KEY: &str = "{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5";
const FX_APO_CLSID_KEYS: &[&str] = &[
    "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},1",
    "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},2",
    "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},3",
    "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},5",
    "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},6",
];

/// Standard Windows Vista/7/8/10/11 user-mode COM PolicyConfigClient CLSID & IPolicyConfig IID.
const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
const IID_IPOLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);

/// RAII COM initializer that safely handles threads already initialized in STA mode.
struct ComGuard {
    should_uninit: bool,
}

impl ComGuard {
    fn acquire() -> Result<Self> {
        // SAFETY: Standard per-thread COM initialization.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == S_OK || hr == S_FALSE {
            Ok(Self {
                should_uninit: true,
            })
        } else if hr == RPC_E_CHANGED_MODE {
            // Thread already initialized COM (e.g. GUI STA thread); do not uninitialize it.
            Ok(Self {
                should_uninit: false,
            })
        } else {
            Err(PolledError::Win32 {
                context: "CoInitializeEx".to_string(),
                message: format!("HRESULT 0x{:08x}", hr.0 as u32),
            })
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.should_uninit {
            // SAFETY: Matches the successful `CoInitializeEx` on this thread.
            unsafe {
                CoUninitialize();
            }
        }
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: `self.0` is an owned Win32 event handle created by `CreateEventW`.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct AudioStopGuard<'a>(&'a IAudioClient);

impl Drop for AudioStopGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: Stopping a started WASAPI client on exit is always safe.
        unsafe {
            let _ = self.0.Stop();
        }
    }
}

fn to_wide_null(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn pwstr_to_string_and_free(pw: PWSTR) -> String {
    if pw.is_null() {
        return String::new();
    }
    // SAFETY: `pw` is a CoTaskMem-allocated null-terminated UTF-16 string returned by COM.
    let s = unsafe { pw.to_string().unwrap_or_default() };
    unsafe {
        CoTaskMemFree(Some(pw.0.cast()));
    }
    s
}

/// Extracts the trailing `{guid}` token from an MMDevice endpoint ID like `{0.0.0.00000000}.{guid}`.
pub fn extract_endpoint_guid(endpoint_id: &str) -> String {
    endpoint_id
        .rsplit('.')
        .next()
        .unwrap_or(endpoint_id)
        .to_string()
}

/// Detects Bluetooth Hands-Free (HFP/SCO) mono voice fallback from endpoint name or sample rate.
pub fn is_bluetooth_hands_free_endpoint(name: &str, sample_rate_hz: Option<u32>) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.contains("hands-free")
        || lower.contains("hands free")
        || lower.contains("ag audio")
        || lower.contains("hfp")
    {
        return true;
    }
    sample_rate_hz.is_some_and(|hz| hz > 0 && hz <= 16_000)
}

fn create_enumerator() -> Result<IMMDeviceEnumerator> {
    // SAFETY: Creating the standard Windows MMDeviceEnumerator COM object.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| {
        PolledError::Win32 {
            context: "CoCreateInstance(MMDeviceEnumerator)".to_string(),
            message: e.to_string(),
        }
    })
}

fn read_device_friendly_name(device: &IMMDevice) -> String {
    // SAFETY: Read-only property store query for `PKEY_Device_FriendlyName`.
    let Ok(store) = (unsafe { device.OpenPropertyStore(STGM_READ) }) else {
        return "(Unknown Audio Device)".to_string();
    };
    let Ok(prop) = (unsafe { store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME) }) else {
        return "(Unknown Audio Device)".to_string();
    };
    let text = prop.to_string();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        "(Unknown Audio Device)".to_string()
    } else {
        trimmed.to_string()
    }
}

fn inspect_fx_enhancements(flow: AudioFlow, guid: &str) -> Option<bool> {
    if !guid.starts_with('{') {
        return None;
    }
    let dir = match flow {
        AudioFlow::Render => "Render",
        AudioFlow::Capture => "Capture",
    };
    let fx_key = format!(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\{dir}\{guid}\FxProperties"
    );

    let disable_sysfx = registry::get_dword(RegHive::Hklm, &fx_key, FX_DISABLE_SYSFX_KEY)
        .ok()
        .flatten();
    if let Some(val) = disable_sysfx {
        return Some(val == 0);
    }

    // If `Disable_SysFx` isn't explicitly written yet, check whether any APO effect CLSID is registered.
    let has_apo = FX_APO_CLSID_KEYS.iter().any(|k| {
        registry::get_string(RegHive::Hklm, &fx_key, k)
            .ok()
            .flatten()
            .is_some_and(|s| !s.trim().is_empty())
    });
    if has_apo {
        Some(true)
    } else {
        None
    }
}

fn inspect_device(
    device: &IMMDevice,
    flow: AudioFlow,
    default_id: Option<&str>,
) -> AudioEndpointInfo {
    let id = unsafe { device.GetId() }
        .map(pwstr_to_string_and_free)
        .unwrap_or_default();
    let guid = extract_endpoint_guid(&id);
    let name = read_device_friendly_name(device);
    let is_default = default_id.is_some_and(|d| d.eq_ignore_ascii_case(&id));

    let (volume_percent, muted) =
        match unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) } {
            Ok(vol) => {
                let pct = unsafe { vol.GetMasterVolumeLevelScalar() }
                    .ok()
                    .map(|s| (s.clamp(0.0, 1.0) * 100.0).round() as u8);
                let is_muted = unsafe { vol.GetMute() }.ok().map(|b| b.as_bool());
                (pct, is_muted)
            }
            Err(_) => (None, None),
        };

    let mut sample_rate_hz = None;
    let mut channels = None;
    let mut bits_per_sample = None;
    let mut default_period_ms = None;
    let mut min_period_ms = None;

    if let Ok(client) = unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None) } {
        if let Ok(fmt_ptr) = unsafe { client.GetMixFormat() } {
            if !fmt_ptr.is_null() {
                // SAFETY: `fmt_ptr` is a valid `WAVEFORMATEX` returned by `GetMixFormat`.
                let fmt: WAVEFORMATEX = unsafe { *fmt_ptr };
                let sr = fmt.nSamplesPerSec;
                sample_rate_hz = Some(sr);
                channels = Some(fmt.nChannels);
                bits_per_sample = Some(fmt.wBitsPerSample);

                let mut def_100ns = 0i64;
                let mut min_100ns = 0i64;
                if unsafe { client.GetDevicePeriod(Some(&mut def_100ns), Some(&mut min_100ns)) }
                    .is_ok()
                {
                    if def_100ns > 0 {
                        default_period_ms = Some(def_100ns as f64 / 10_000.0);
                    }
                    if min_100ns > 0 {
                        min_period_ms = Some(min_100ns as f64 / 10_000.0);
                    }
                }

                // Prefer true shared-mode low-latency `IAudioClient3` engine period when supported.
                if let Ok(client3) = client.cast::<IAudioClient3>() {
                    let mut def_f = 0u32;
                    let mut fund_f = 0u32;
                    let mut min_f = 0u32;
                    let mut max_f = 0u32;
                    if unsafe {
                        client3.GetSharedModeEnginePeriod(
                            fmt_ptr,
                            &mut def_f,
                            &mut fund_f,
                            &mut min_f,
                            &mut max_f,
                        )
                    }
                    .is_ok()
                        && sr > 0
                    {
                        if def_f > 0 {
                            default_period_ms = Some(f64::from(def_f) * 1000.0 / f64::from(sr));
                        }
                        if min_f > 0 {
                            min_period_ms = Some(f64::from(min_f) * 1000.0 / f64::from(sr));
                        }
                    }
                }

                unsafe {
                    CoTaskMemFree(Some(fmt_ptr.cast()));
                }
            }
        }
    }

    let enhancements_enabled = inspect_fx_enhancements(flow, &guid);
    let is_bluetooth_hands_free = is_bluetooth_hands_free_endpoint(&name, sample_rate_hz);

    AudioEndpointInfo {
        id,
        guid,
        name,
        flow,
        is_default,
        volume_percent,
        muted,
        sample_rate_hz,
        channels,
        bits_per_sample,
        default_period_ms,
        min_period_ms,
        enhancements_enabled,
        is_bluetooth_hands_free,
    }
}

/// Enumerates all active Windows playback (`Render`) or recording (`Capture`) audio endpoints.
pub fn list_audio_endpoints(flow: AudioFlow) -> Result<Vec<AudioEndpointInfo>> {
    let _com = ComGuard::acquire()?;
    let enumerator = create_enumerator()?;
    let data_flow = match flow {
        AudioFlow::Render => eRender,
        AudioFlow::Capture => eCapture,
    };

    let default_id = unsafe { enumerator.GetDefaultAudioEndpoint(data_flow, eConsole) }
        .ok()
        .and_then(|d| unsafe { d.GetId() }.ok())
        .map(pwstr_to_string_and_free);

    let collection = unsafe { enumerator.EnumAudioEndpoints(data_flow, DEVICE_STATE_ACTIVE) }
        .map_err(|e| PolledError::Win32 {
            context: "IMMDeviceEnumerator::EnumAudioEndpoints".to_string(),
            message: e.to_string(),
        })?;
    let count = unsafe { collection.GetCount() }.unwrap_or(0);

    let mut out = Vec::with_capacity(count as usize);
    for idx in 0..count {
        if let Ok(device) = unsafe { collection.Item(idx) } {
            out.push(inspect_device(&device, flow, default_id.as_deref()));
        }
    }
    // Keep the default device first, then alphabetical for stable UI dropdowns.
    out.sort_by(|a, b| {
        b.is_default.cmp(&a.is_default).then_with(|| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        })
    });
    Ok(out)
}

/// Minimal ABI-exact vtable for Windows `IPolicyConfig` (`SetDefaultEndpoint` at slot 13).
#[repr(C)]
struct IPolicyConfigVtbl {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_mix_format: usize,
    get_device_format: usize,
    reset_device_format: usize,
    set_device_format: usize,
    get_processing_period: usize,
    set_processing_period: usize,
    get_share_mode: usize,
    set_share_mode: usize,
    get_property_value: usize,
    set_property_value: usize,
    set_default_endpoint: unsafe extern "system" fn(*mut c_void, PCWSTR, u32) -> HRESULT,
}

#[repr(C)]
struct RawPolicyConfig {
    vtbl: *const IPolicyConfigVtbl,
}

struct OwnedPolicyConfig(*mut RawPolicyConfig);

impl Drop for OwnedPolicyConfig {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Valid COM pointer acquired via `CoCreateInstance`; calling `IUnknown::Release`.
            unsafe {
                ((*(*self.0).vtbl).release)(self.0.cast());
            }
        }
    }
}

/// Switches the Windows default audio endpoint across Console, Multimedia, and Communications roles.
pub fn set_default_audio_endpoint(endpoint_id: &str) -> Result<()> {
    let _com = ComGuard::acquire()?;
    let id_w = to_wide_null(endpoint_id);

    // SAFETY: `CoCreateInstance` with standard `CLSID_PolicyConfigClient` and `IID_IPOLICY_CONFIG`.
    let raw_ptr: *mut c_void = unsafe {
        let mut out: *mut c_void = ptr::null_mut();
        let hr = windows::Win32::System::Com::CoCreateInstance(
            &CLSID_POLICY_CONFIG_CLIENT,
            None,
            CLSCTX_ALL,
        )
        .and_then(|unk: windows::core::IUnknown| {
            let qhr = unk.query(&IID_IPOLICY_CONFIG, &mut out);
            if qhr.is_ok() {
                Ok(())
            } else {
                Err(windows::core::Error::from(qhr))
            }
        });
        hr.map_err(|e| PolledError::Win32 {
            context: "CoCreateInstance(IPolicyConfig)".to_string(),
            message: e.to_string(),
        })?;
        out
    };
    if raw_ptr.is_null() {
        return Err(PolledError::Win32 {
            context: "IPolicyConfig".to_string(),
            message: "Windows returned a null IPolicyConfig interface".to_string(),
        });
    }
    let policy = OwnedPolicyConfig(raw_ptr.cast());

    for role in [
        eConsole.0 as u32,
        eMultimedia.0 as u32,
        eCommunications.0 as u32,
    ] {
        // SAFETY: `policy.0` is a valid `IPolicyConfig`; `id_w` is null-terminated UTF-16.
        let hr = unsafe {
            ((*(*policy.0).vtbl).set_default_endpoint)(policy.0.cast(), PCWSTR(id_w.as_ptr()), role)
        };
        if hr.is_err() {
            return Err(PolledError::Win32 {
                context: format!("IPolicyConfig::SetDefaultEndpoint(role={role})"),
                message: format!("HRESULT 0x{:08x}", hr.0 as u32),
            });
        }
    }
    Ok(())
}

/// Reads master volume (`0..=100`) on a specific endpoint ID (or the current default if `endpoint_id` is `None`).
pub fn get_audio_endpoint_volume(flow: AudioFlow, endpoint_id: Option<&str>) -> Result<u8> {
    let _com = ComGuard::acquire()?;
    let enumerator = create_enumerator()?;
    let device = resolve_device(&enumerator, flow, endpoint_id)?;
    let vol: IAudioEndpointVolume =
        unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|e| PolledError::Win32 {
            context: "Activate(IAudioEndpointVolume)".to_string(),
            message: e.to_string(),
        })?;
    let scalar = unsafe { vol.GetMasterVolumeLevelScalar() }.map_err(|e| PolledError::Win32 {
        context: "GetMasterVolumeLevelScalar".to_string(),
        message: e.to_string(),
    })?;
    Ok((scalar.clamp(0.0, 1.0) * 100.0).round() as u8)
}

/// Sets master volume (`0..=100`) on a specific endpoint ID (or the current default if `endpoint_id` is `None`).
pub fn set_audio_endpoint_volume(
    flow: AudioFlow,
    endpoint_id: Option<&str>,
    percent: u8,
) -> Result<()> {
    let _com = ComGuard::acquire()?;
    let enumerator = create_enumerator()?;
    let device = resolve_device(&enumerator, flow, endpoint_id)?;
    let vol: IAudioEndpointVolume =
        unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|e| PolledError::Win32 {
            context: "Activate(IAudioEndpointVolume)".to_string(),
            message: e.to_string(),
        })?;
    let scalar = f32::from(percent.min(100)) / 100.0;
    // SAFETY: Passing `ptr::null()` for event context GUID is documented by Microsoft.
    unsafe { vol.SetMasterVolumeLevelScalar(scalar, ptr::null()) }.map_err(|e| PolledError::Win32 {
        context: "SetMasterVolumeLevelScalar".to_string(),
        message: e.to_string(),
    })
}

/// Reads the mute state (`true` = muted) of an audio endpoint (or default endpoint when `None`).
pub fn get_audio_endpoint_mute(flow: AudioFlow, endpoint_id: Option<&str>) -> Result<bool> {
    let _com = ComGuard::acquire()?;
    let enumerator = create_enumerator()?;
    let device = resolve_device(&enumerator, flow, endpoint_id)?;
    let vol: IAudioEndpointVolume =
        unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|e| PolledError::Win32 {
            context: "Activate(IAudioEndpointVolume)".to_string(),
            message: e.to_string(),
        })?;
    let muted = unsafe { vol.GetMute() }.map_err(|e| PolledError::Win32 {
        context: "IAudioEndpointVolume::GetMute".to_string(),
        message: e.to_string(),
    })?;
    Ok(muted.as_bool())
}

/// Sets the mute state of an audio endpoint (or default endpoint when `None`).
pub fn set_audio_endpoint_mute(
    flow: AudioFlow,
    endpoint_id: Option<&str>,
    muted: bool,
) -> Result<()> {
    let _com = ComGuard::acquire()?;
    let enumerator = create_enumerator()?;
    let device = resolve_device(&enumerator, flow, endpoint_id)?;
    let vol: IAudioEndpointVolume =
        unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|e| PolledError::Win32 {
            context: "Activate(IAudioEndpointVolume)".to_string(),
            message: e.to_string(),
        })?;
    // SAFETY: Passing `ptr::null()` for event context GUID is documented by Microsoft.
    unsafe { vol.SetMute(muted, ptr::null()) }.map_err(|e| PolledError::Win32 {
        context: "IAudioEndpointVolume::SetMute".to_string(),
        message: e.to_string(),
    })
}

/// Toggles mute on an audio endpoint (or default endpoint when `None`), returning
/// `(friendly_name, is_now_muted)`.
pub fn toggle_audio_endpoint_mute(
    flow: AudioFlow,
    endpoint_id: Option<&str>,
) -> Result<(String, bool)> {
    let _com = ComGuard::acquire()?;
    let enumerator = create_enumerator()?;
    let device = resolve_device(&enumerator, flow, endpoint_id)?;
    let name = read_device_friendly_name(&device);
    let vol: IAudioEndpointVolume =
        unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|e| PolledError::Win32 {
            context: "Activate(IAudioEndpointVolume)".to_string(),
            message: e.to_string(),
        })?;
    let currently_muted = unsafe { vol.GetMute() }
        .map_err(|e| PolledError::Win32 {
            context: "IAudioEndpointVolume::GetMute".to_string(),
            message: e.to_string(),
        })?
        .as_bool();
    let next = !currently_muted;
    // SAFETY: Passing `ptr::null()` for event context GUID is documented by Microsoft.
    unsafe { vol.SetMute(next, ptr::null()) }.map_err(|e| PolledError::Win32 {
        context: "IAudioEndpointVolume::SetMute".to_string(),
        message: e.to_string(),
    })?;
    Ok((name, next))
}

fn resolve_device(
    enumerator: &IMMDeviceEnumerator,
    flow: AudioFlow,
    endpoint_id: Option<&str>,
) -> Result<IMMDevice> {
    if let Some(id) = endpoint_id {
        let id_w = to_wide_null(id);
        return unsafe { enumerator.GetDevice(PCWSTR(id_w.as_ptr())) }.map_err(|e| {
            PolledError::Win32 {
                context: format!("IMMDeviceEnumerator::GetDevice({id})"),
                message: e.to_string(),
            }
        });
    }
    let data_flow = match flow {
        AudioFlow::Render => eRender,
        AudioFlow::Capture => eCapture,
    };
    unsafe { enumerator.GetDefaultAudioEndpoint(data_flow, eConsole) }.map_err(|e| {
        PolledError::Win32 {
            context: "IMMDeviceEnumerator::GetDefaultAudioEndpoint".to_string(),
            message: e.to_string(),
        }
    })
}

/// Runs a strictly silent on-demand WASAPI shared-mode audio engine latency & wake-jitter benchmark.
pub fn measure_audio_latency_benchmark(duration_ms: u64) -> Result<AudioLatencyBenchmark> {
    let _com = ComGuard::acquire()?;
    let enumerator = create_enumerator()?;
    let device = resolve_device(&enumerator, AudioFlow::Render, None)?;
    let device_name = read_device_friendly_name(&device);

    let client: IAudioClient =
        unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|e| PolledError::Win32 {
            context: "Activate(IAudioClient)".to_string(),
            message: e.to_string(),
        })?;

    let fmt_ptr = unsafe { client.GetMixFormat() }.map_err(|e| PolledError::Win32 {
        context: "IAudioClient::GetMixFormat".to_string(),
        message: e.to_string(),
    })?;
    if fmt_ptr.is_null() {
        return Err(PolledError::Win32 {
            context: "IAudioClient::GetMixFormat".to_string(),
            message: "Returned null WAVEFORMATEX".to_string(),
        });
    }

    struct FormatGuard(*mut WAVEFORMATEX);
    impl Drop for FormatGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CoTaskMemFree(Some(self.0.cast()));
                }
            }
        }
    }
    let _fmt_guard = FormatGuard(fmt_ptr);
    let fmt = unsafe { *fmt_ptr };
    let sample_rate_hz = fmt.nSamplesPerSec.max(1);
    let channels = fmt.nChannels;

    let mut def_100ns = 0i64;
    let mut min_100ns = 0i64;
    let _ = unsafe { client.GetDevicePeriod(Some(&mut def_100ns), Some(&mut min_100ns)) };
    let mut default_period_ms = (def_100ns as f64 / 10_000.0).max(1.0);
    let mut min_period_ms = (min_100ns as f64 / 10_000.0).max(0.5);

    // Attempt true low-latency shared-mode init via `IAudioClient3::InitializeSharedAudioStream`.
    let mut initialized_low_latency = false;
    if let Ok(client3) = client.cast::<IAudioClient3>() {
        let mut def_f = 0u32;
        let mut fund_f = 0u32;
        let mut min_f = 0u32;
        let mut max_f = 0u32;
        if unsafe {
            client3.GetSharedModeEnginePeriod(
                fmt_ptr,
                &mut def_f,
                &mut fund_f,
                &mut min_f,
                &mut max_f,
            )
        }
        .is_ok()
        {
            if def_f > 0 {
                default_period_ms = f64::from(def_f) * 1000.0 / f64::from(sample_rate_hz);
            }
            if min_f > 0 {
                min_period_ms = f64::from(min_f) * 1000.0 / f64::from(sample_rate_hz);
                if unsafe {
                    client3.InitializeSharedAudioStream(
                        AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                        min_f,
                        fmt_ptr,
                        None,
                    )
                }
                .is_ok()
                {
                    initialized_low_latency = true;
                }
            }
        }
    }

    if !initialized_low_latency {
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                def_100ns.max(100_000),
                0,
                fmt_ptr,
                None,
            )
        }
        .map_err(|e| PolledError::Win32 {
            context: "IAudioClient::Initialize".to_string(),
            message: e.to_string(),
        })?;
    }

    let buffer_frames = unsafe { client.GetBufferSize() }.unwrap_or(480);
    let active_buffer_ms = f64::from(buffer_frames) * 1000.0 / f64::from(sample_rate_hz);

    let event_raw = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.map_err(|e| {
        PolledError::Win32 {
            context: "CreateEventW".to_string(),
            message: e.to_string(),
        }
    })?;
    let event = OwnedHandle(event_raw);

    unsafe { client.SetEventHandle(event.0) }.map_err(|e| PolledError::Win32 {
        context: "IAudioClient::SetEventHandle".to_string(),
        message: e.to_string(),
    })?;

    let render: IAudioRenderClient =
        unsafe { client.GetService() }.map_err(|e| PolledError::Win32 {
            context: "IAudioClient::GetService(IAudioRenderClient)".to_string(),
            message: e.to_string(),
        })?;

    // Pre-fill initial silent buffer so WASAPI starts cleanly with zero audible click/pop.
    if let Ok(_buf) = unsafe { render.GetBuffer(buffer_frames) } {
        let _ = unsafe { render.ReleaseBuffer(buffer_frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) };
    }

    unsafe { client.Start() }.map_err(|e| PolledError::Win32 {
        context: "IAudioClient::Start".to_string(),
        message: e.to_string(),
    })?;
    let _stop_guard = AudioStopGuard(&client);

    let clamped_ms = duration_ms.clamp(400, 3_000);
    let target = Duration::from_millis(clamped_ms);
    let bench_start = Instant::now();
    let mut last_wake: Option<Instant> = None;
    let mut samples_ms = Vec::with_capacity(400);

    while bench_start.elapsed() < target {
        let wait = unsafe { WaitForSingleObject(event.0, 100) };
        if wait.0 != 0 {
            continue;
        }
        let now = Instant::now();
        if let Some(prev) = last_wake {
            samples_ms.push(now.duration_since(prev).as_secs_f64() * 1000.0);
        }
        last_wake = Some(now);

        let padding = unsafe { client.GetCurrentPadding() }.unwrap_or(0);
        let avail = buffer_frames.saturating_sub(padding);
        if avail > 0 {
            if let Ok(_buf) = unsafe { render.GetBuffer(avail) } {
                let _ = unsafe { render.ReleaseBuffer(avail, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) };
            }
        }
    }
    let elapsed_ms = bench_start.elapsed().as_secs_f64() * 1000.0;

    let mut sorted = samples_ms.clone();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len().max(1);
    let min_ms = sorted.first().copied().unwrap_or(0.0);
    let max_ms = sorted.last().copied().unwrap_or(0.0);
    let median_ms = sorted[n / 2];
    let p95_ms = sorted[(((n as f64) * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(n - 1)];
    let p99_ms = sorted[(((n as f64) * 0.99).ceil() as usize)
        .saturating_sub(1)
        .min(n - 1)];

    let expected_period = if initialized_low_latency {
        min_period_ms
    } else {
        default_period_ms
    };
    let glitch_threshold = (expected_period * 1.75)
        .max(active_buffer_ms * 1.4)
        .max(12.0);
    let glitch_count = samples_ms.iter().filter(|&&v| v > glitch_threshold).count();

    let severity = if samples_ms.is_empty() {
        Severity::Info
    } else if glitch_count == 0 && p99_ms <= (expected_period * 1.35).max(12.0) {
        Severity::Ok
    } else if glitch_count <= 2 && p99_ms <= 22.0 {
        Severity::Warn
    } else {
        Severity::Bad
    };

    Ok(AudioLatencyBenchmark {
        device_name,
        sample_rate_hz,
        channels,
        default_period_ms,
        min_period_ms,
        active_buffer_ms,
        elapsed_ms,
        samples_ms,
        min_ms,
        median_ms,
        p95_ms,
        p99_ms,
        max_ms,
        glitch_count,
        severity,
    })
}

/// Captured Windows per-application audio mixer state so Polled can restore exact sliders on desktop/exit.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedAppAudioVolume {
    pub exe_name: String,
    pub pid: u32,
    pub original_volume: f32,
    pub original_muted: bool,
}

fn normalize_audio_app_name(raw: &str) -> String {
    let base = raw
        .trim()
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if base.is_empty() {
        return String::new();
    }
    if base.ends_with(".exe") {
        base
    } else {
        format!("{base}.exe")
    }
}

fn process_exe_basename(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    // SAFETY: Standard read-only `PROCESS_QUERY_LIMITED_INFORMATION` handle query (anti-cheat safe).
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let _guard = OwnedHandle(handle);
    let mut buf = vec![0u16; 512];
    let mut len = buf.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .ok()?;
    let full = String::from_utf16_lossy(&buf[..len as usize]);
    let base = full
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    (!base.is_empty()).then_some(base)
}

/// Lowers or mutes (`target_volume_pct == 0`) active Windows Volume Mixer sessions for matching
/// background app executables (e.g. `spotify.exe`, `chrome.exe`) on the default playback device,
/// returning their exact original mixer states so Polled can restore them cleanly on tab-out/exit.
pub fn duck_app_audio_sessions(
    target_exes: &[String],
    target_volume_pct: u8,
) -> Result<Vec<SavedAppAudioVolume>> {
    let targets: Vec<String> = target_exes
        .iter()
        .map(|s| normalize_audio_app_name(s))
        .filter(|s| !s.is_empty())
        .collect();
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let _com = ComGuard::acquire()?;
    let enumerator = create_enumerator()?;
    let device = resolve_device(&enumerator, AudioFlow::Render, None)?;
    let mgr: IAudioSessionManager2 =
        unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|e| PolledError::Win32 {
            context: "Activate(IAudioSessionManager2)".to_string(),
            message: e.to_string(),
        })?;
    let session_enum = unsafe { mgr.GetSessionEnumerator() }.map_err(|e| PolledError::Win32 {
        context: "IAudioSessionManager2::GetSessionEnumerator".to_string(),
        message: e.to_string(),
    })?;
    let count = unsafe { session_enum.GetCount() }.unwrap_or(0);
    let target_scalar = f32::from(target_volume_pct.min(100)) / 100.0;

    let mut saved = Vec::new();
    for idx in 0..count {
        let Ok(ctrl) = (unsafe { session_enum.GetSession(idx) }) else {
            continue;
        };
        let Ok(ctrl2) = ctrl.cast::<IAudioSessionControl2>() else {
            continue;
        };
        let Ok(pid) = (unsafe { ctrl2.GetProcessId() }) else {
            continue;
        };
        let Some(exe) = process_exe_basename(pid) else {
            continue;
        };
        if !targets.iter().any(|t| t == &exe) {
            continue;
        }
        let Ok(vol) = ctrl.cast::<ISimpleAudioVolume>() else {
            continue;
        };
        let Ok(orig_vol) = (unsafe { vol.GetMasterVolume() }) else {
            continue;
        };
        let Ok(orig_muted_bool) = (unsafe { vol.GetMute() }) else {
            continue;
        };
        let orig_muted = orig_muted_bool.as_bool();

        let applied = if target_volume_pct == 0 {
            unsafe { vol.SetMute(true, ptr::null()) }.is_ok()
        } else {
            unsafe { vol.SetMasterVolume(target_scalar, ptr::null()) }.is_ok()
        };
        if applied {
            saved.push(SavedAppAudioVolume {
                exe_name: exe,
                pid,
                original_volume: orig_vol.clamp(0.0, 1.0),
                original_muted: orig_muted,
            });
        }
    }
    Ok(saved)
}

/// Restores previously ducked/muted application audio sessions back to their exact captured
/// Windows Volume Mixer level and mute state.
pub fn restore_app_audio_sessions(saved: &[SavedAppAudioVolume]) -> Result<usize> {
    if saved.is_empty() {
        return Ok(0);
    }
    let _com = ComGuard::acquire()?;
    let enumerator = create_enumerator()?;
    let device = resolve_device(&enumerator, AudioFlow::Render, None)?;
    let mgr: IAudioSessionManager2 =
        unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|e| PolledError::Win32 {
            context: "Activate(IAudioSessionManager2)".to_string(),
            message: e.to_string(),
        })?;
    let session_enum = unsafe { mgr.GetSessionEnumerator() }.map_err(|e| PolledError::Win32 {
        context: "IAudioSessionManager2::GetSessionEnumerator".to_string(),
        message: e.to_string(),
    })?;
    let count = unsafe { session_enum.GetCount() }.unwrap_or(0);
    let mut restored = 0usize;

    for idx in 0..count {
        let Ok(ctrl) = (unsafe { session_enum.GetSession(idx) }) else {
            continue;
        };
        let Ok(ctrl2) = ctrl.cast::<IAudioSessionControl2>() else {
            continue;
        };
        let Ok(pid) = (unsafe { ctrl2.GetProcessId() }) else {
            continue;
        };
        let entry = saved.iter().find(|s| s.pid == pid).or_else(|| {
            let exe = process_exe_basename(pid)?;
            saved.iter().find(|s| s.exe_name == exe)
        });
        let Some(entry) = entry else {
            continue;
        };
        if let Ok(vol) = ctrl.cast::<ISimpleAudioVolume>() {
            let ok_v =
                unsafe { vol.SetMasterVolume(entry.original_volume.clamp(0.0, 1.0), ptr::null()) }
                    .is_ok();
            let ok_m = unsafe { vol.SetMute(entry.original_muted, ptr::null()) }.is_ok();
            if ok_v || ok_m {
                restored += 1;
            }
        }
    }
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_background_audio_app_names() {
        assert_eq!(normalize_audio_app_name("spotify"), "spotify.exe");
        assert_eq!(normalize_audio_app_name("  Chrome.EXE "), "chrome.exe");
        assert_eq!(
            normalize_audio_app_name(r"C:\Program Files\Discord\Discord.exe"),
            "discord.exe"
        );
        assert_eq!(normalize_audio_app_name("   "), "");
    }

    #[test]
    fn extracts_endpoint_guid_and_detects_bluetooth_hfp() {
        assert_eq!(
            extract_endpoint_guid("{0.0.0.00000000}.{12345678-abcd-ef00-1234-56789abcdef0}"),
            "{12345678-abcd-ef00-1234-56789abcdef0}"
        );
        assert!(is_bluetooth_hands_free_endpoint(
            "Headset (WH-1000XM5 Hands-Free AG Audio)",
            Some(16_000)
        ));
        assert!(is_bluetooth_hands_free_endpoint(
            "USB Headset",
            Some(16_000)
        ));
        assert!(!is_bluetooth_hands_free_endpoint(
            "Speakers (Topping DX3 Pro+)",
            Some(48_000)
        ));
    }
}
