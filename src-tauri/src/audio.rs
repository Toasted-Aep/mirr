// Windows audio output device enumeration and default device change monitoring

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpatialCapabilities {
    pub is_spatial_supported: bool,
    pub is_dolby_atmos: bool,
    pub spatial_technology: String,
    pub active_format: String,
    pub max_channels: u32,
}

impl Default for SpatialCapabilities {
    fn default() -> Self {
        Self {
            is_spatial_supported: false,
            is_dolby_atmos: false,
            spatial_technology: "None".to_string(),
            active_format: String::new(),
            max_channels: 2,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    #[serde(default)]
    pub supports_spatial: bool,
    #[serde(default)]
    pub is_dolby_atmos: bool,
    #[serde(default)]
    pub spatial_technology: String,
}

#[cfg(windows)]
mod win_audio {
    use super::{AudioDevice, SpatialCapabilities};
    use windows::core::{PCWSTR, PWSTR, Result, GUID};
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

    #[repr(C)]
    pub struct ISpatialAudioClient_Vtbl {
        pub base__: windows::core::IUnknown_Vtbl,
        pub get_static_object_position_mask: unsafe extern "system" fn(*mut core::ffi::c_void, *mut u32) -> windows::core::HRESULT,
        pub get_native_static_object_type_mask: unsafe extern "system" fn(*mut core::ffi::c_void, *mut u32) -> windows::core::HRESULT,
        pub get_max_dynamic_object_count: unsafe extern "system" fn(*mut core::ffi::c_void, *mut u32) -> windows::core::HRESULT,
        pub get_supported_audio_object_format_enumerator: unsafe extern "system" fn(*mut core::ffi::c_void, *mut *mut core::ffi::c_void) -> windows::core::HRESULT,
        pub get_max_frame_count: unsafe extern "system" fn(*mut core::ffi::c_void, *const core::ffi::c_void, *mut u32) -> windows::core::HRESULT,
        pub is_audio_object_format_supported: unsafe extern "system" fn(*mut core::ffi::c_void, *const core::ffi::c_void) -> windows::core::HRESULT,
        pub is_spatial_audio_stream_available: unsafe extern "system" fn(*mut core::ffi::c_void, *const windows::core::GUID, *const core::ffi::c_void) -> windows::core::HRESULT,
        pub activate_spatial_audio_stream: unsafe extern "system" fn(*mut core::ffi::c_void, *const core::ffi::c_void, *const windows::core::GUID, *mut *mut core::ffi::c_void) -> windows::core::HRESULT,
    }

    #[repr(transparent)]
    #[derive(Clone, PartialEq, Eq)]
    pub struct ISpatialAudioClient(pub windows::core::IUnknown);

    unsafe impl windows::core::Interface for ISpatialAudioClient {
        type Vtable = ISpatialAudioClient_Vtbl;
        const IID: windows::core::GUID = windows::core::GUID::from_u128(0xbbf8e066_aaaa_49be_9a4d_fd2a858ea27f);
    }

    // PKEY_Device_FriendlyName: {a45c254e-df1c-4efd-8020-67d146a850e0}, 14
    const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
        pid: 14,
    };
    // PKEY_Device_DeviceDesc: {a45c254e-df1c-4efd-8020-67d146a850e0}, 2
    const PKEY_DEVICE_DEVICE_DESC: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
        pid: 2,
    };

    fn propvariant_to_string(pv: &PROPVARIANT) -> Option<String> {
        unsafe {
            // Check if VT_LPWSTR (31)
            let vt = pv.Anonymous.Anonymous.vt;
            if vt.0 == 31 {
                let pwsz = pv.Anonymous.Anonymous.Anonymous.pwszVal;
                if !pwsz.is_null() {
                    return pwsz.to_string().ok();
                }
            }
        }
        None
    }

    fn get_device_name(dev: &IMMDevice) -> Option<String> {
        unsafe {
            let store: IPropertyStore = dev.OpenPropertyStore(STGM_READ).ok()?;
            if let Ok(pv) = store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME) {
                if let Some(s) = propvariant_to_string(&pv) {
                    if !s.trim().is_empty() {
                        return Some(s);
                    }
                }
            }
            if let Ok(pv) = store.GetValue(&PKEY_DEVICE_DEVICE_DESC) {
                if let Some(s) = propvariant_to_string(&pv) {
                    if !s.trim().is_empty() {
                        return Some(s);
                    }
                }
            }
        }
        None
    }

    fn check_registry_for_dolby(guid_str: &str) -> (bool, String) {
        let clean_guid = if let Some(idx) = guid_str.rfind('{') {
            &guid_str[idx..]
        } else {
            guid_str
        };

        let subkeys = [
            format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\{clean_guid}\FxProperties"),
            format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\{clean_guid}\Properties"),
        ];

        for subkey in &subkeys {
            let wide: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
            let mut hkey: HKEY = HKEY::default();
            let res = unsafe {
                RegOpenKeyExW(
                    HKEY_LOCAL_MACHINE,
                    PCWSTR::from_raw(wide.as_ptr()),
                    None,
                    KEY_READ,
                    &mut hkey,
                )
            };
            if res.is_ok() && !hkey.is_invalid() {
                let mut index = 0u32;
                let mut val_name = [0u16; 512];
                let mut data = [0u8; 2048];

                loop {
                    let mut val_name_len = val_name.len() as u32;
                    let mut data_len = data.len() as u32;
                    let mut val_type = 0u32;

                    let enum_res = unsafe {
                        RegEnumValueW(
                            hkey,
                            index,
                            Some(PWSTR::from_raw(val_name.as_mut_ptr())),
                            &mut val_name_len,
                            None,
                            Some(&mut val_type),
                            Some(data.as_mut_ptr()),
                            Some(&mut data_len),
                        )
                    };

                    if enum_res.is_err() {
                        break;
                    }

                    if (val_type == 1 || val_type == 2 || val_type == 7) && data_len >= 2 {
                        let u16_slice = unsafe {
                            std::slice::from_raw_parts(
                                data.as_ptr() as *const u16,
                                (data_len as usize) / 2,
                            )
                        };
                        let text = String::from_utf16_lossy(u16_slice);
                        let upper = text.to_uppercase();
                        if upper.contains("DOLBY")
                            || upper.contains("ATMOS")
                            || upper.contains("DAX3")
                            || upper.contains("0EBD8605")
                            || upper.contains("25C1BC6B")
                            || upper.contains("4C81E564")
                            || upper.contains("1459AC38")
                            || upper.contains("A289735D")
                        {
                            unsafe { let _ = RegCloseKey(hkey); }
                            return (true, "Dolby Atmos (Hardware / DAX3)".to_string());
                        }
                    }
                    index += 1;
                }
                unsafe { let _ = RegCloseKey(hkey); }
            }
        }
        (false, String::new())
    }

    pub fn get_device_spatial_capabilities(dev: &IMMDevice, id_str: &str, name: &str) -> SpatialCapabilities {
        let mut caps = SpatialCapabilities::default();

        // 1. Check ISpatialAudioClient COM activation
        unsafe {
            let spatial_res: Result<ISpatialAudioClient> = dev.Activate(CLSCTX_ALL, None);
            if spatial_res.is_ok() {
                caps.is_spatial_supported = true;
                caps.spatial_technology = "Windows Spatial Audio".to_string();
            }
        }

        // 2. Query channel count from WASAPI mix format
        unsafe {
            if let Ok(client) = dev.Activate::<IAudioClient>(CLSCTX_ALL, None) {
                if let Ok(p_format) = client.GetMixFormat() {
                    if !p_format.is_null() {
                        caps.max_channels = (*p_format).nChannels as u32;
                        CoTaskMemFree(Some(p_format as *const _));
                    }
                }
            }
        }

        // 3. Check registry for Dolby Atmos APOs and services
        let (has_reg_dolby, reg_tech) = check_registry_for_dolby(id_str);
        if has_reg_dolby {
            caps.is_dolby_atmos = true;
            caps.is_spatial_supported = true;
            caps.spatial_technology = if !reg_tech.is_empty() {
                reg_tech
            } else {
                "Dolby Atmos".to_string()
            };
        }

        // 4. Check device friendly name for Dolby / Atmos keywords
        let lower_name = name.to_lowercase();
        if lower_name.contains("dolby") || lower_name.contains("atmos") {
            caps.is_dolby_atmos = true;
            caps.is_spatial_supported = true;
            caps.spatial_technology = "Dolby Atmos".to_string();
        }

        // 5. Check property store for Dolby strings
        unsafe {
            if let Ok(store) = dev.OpenPropertyStore(STGM_READ) {
                if let Ok(count) = store.GetCount() {
                    for i in 0..count {
                        let mut key = PROPERTYKEY::default();
                        if store.GetAt(i, &mut key).is_ok() {
                            if let Ok(val) = store.GetValue(&key) {
                                let vt = val.Anonymous.Anonymous.vt.0;
                                if vt == 31 {
                                    let pwsz = val.Anonymous.Anonymous.Anonymous.pwszVal;
                                    if !pwsz.is_null() {
                                        if let Ok(s) = pwsz.to_string() {
                                            let s_lower = s.to_lowercase();
                                            if s_lower.contains("dolby") || s_lower.contains("atmos") {
                                                caps.is_dolby_atmos = true;
                                                caps.is_spatial_supported = true;
                                                caps.spatial_technology = "Dolby Atmos".to_string();
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        caps
    }

    pub fn get_spatial_capabilities(device_id: Option<&str>) -> SpatialCapabilities {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: Result<IMMDeviceEnumerator> =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL);
            let Ok(enumerator) = enumerator else {
                return SpatialCapabilities::default();
            };

            let dev_res: Result<IMMDevice> = if let Some(id) = device_id {
                if id == "default" || id.is_empty() {
                    enumerator.GetDefaultAudioEndpoint(eRender, eConsole)
                } else if id.starts_with('{') {
                    use windows::core::HSTRING;
                    enumerator
                        .GetDevice(&HSTRING::from(id))
                        .or_else(|_| enumerator.GetDefaultAudioEndpoint(eRender, eConsole))
                } else {
                    // Match by friendly name or substring
                    let mut matched = None;
                    if let Ok(collection) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) {
                        if let Ok(count) = collection.GetCount() {
                            for i in 0..count {
                                if let Ok(d) = collection.Item(i) {
                                    let dev_name = get_device_name(&d).unwrap_or_default();
                                    if dev_name.eq_ignore_ascii_case(id)
                                        || dev_name.to_lowercase().contains(&id.to_lowercase())
                                        || id.to_lowercase().contains(&dev_name.to_lowercase())
                                    {
                                        matched = Some(d);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    match matched {
                        Some(d) => Ok(d),
                        None => enumerator.GetDefaultAudioEndpoint(eRender, eConsole),
                    }
                }
            } else {
                enumerator.GetDefaultAudioEndpoint(eRender, eConsole)
            };

            let Ok(dev) = dev_res else {
                return SpatialCapabilities::default();
            };

            let id_str = dev.GetId().ok().map(|s| s.to_string().unwrap_or_default()).unwrap_or_default();
            let name = get_device_name(&dev).unwrap_or_default();
            get_device_spatial_capabilities(&dev, &id_str, &name)
        }
    }

    pub fn list_devices() -> Vec<AudioDevice> {
        let mut result = Vec::new();
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: Result<IMMDeviceEnumerator> =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL);
            let Ok(enumerator) = enumerator else {
                return result;
            };

            let default_id: Option<String> = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .ok()
                .and_then(|dev| dev.GetId().ok())
                .map(|id| id.to_string().unwrap_or_default());

            if let Ok(collection) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) {
                if let Ok(count) = collection.GetCount() {
                    for i in 0..count {
                        if let Ok(dev) = collection.Item(i) {
                            if let Ok(id_pwstr) = dev.GetId() {
                                let id_str = id_pwstr.to_string().unwrap_or_default();
                                let is_def = default_id.as_deref() == Some(&id_str);
                                let name = get_device_name(&dev).unwrap_or_else(|| {
                                    format!("Audio Device {}", i + 1)
                                });
                                let caps = get_device_spatial_capabilities(&dev, &id_str, &name);
                                result.push(AudioDevice {
                                    id: id_str,
                                    name,
                                    is_default: is_def,
                                    supports_spatial: caps.is_spatial_supported,
                                    is_dolby_atmos: caps.is_dolby_atmos,
                                    spatial_technology: caps.spatial_technology,
                                });
                            }
                        }
                    }
                }
            }
        }
        result
    }

    pub fn get_default_device() -> Option<AudioDevice> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: Result<IMMDeviceEnumerator> =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL);
            let enumerator = enumerator.ok()?;
            let dev = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
            let id = dev.GetId().ok()?.to_string().unwrap_or_default();
            let name = get_device_name(&dev).unwrap_or_else(|| "Default Audio Device".to_string());
            let caps = get_device_spatial_capabilities(&dev, &id, &name);
            Some(AudioDevice {
                id,
                name,
                is_default: true,
                supports_spatial: caps.is_spatial_supported,
                is_dolby_atmos: caps.is_dolby_atmos,
                spatial_technology: caps.spatial_technology,
            })
        }
    }

    pub fn get_volume(device_id: Option<&str>) -> Result<f32> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: Result<IMMDeviceEnumerator> =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL);
            let enumerator = enumerator?;
            let dev: IMMDevice = if let Some(id) = device_id {
                if id == "default" || id.is_empty() {
                    enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?
                } else {
                    use windows::core::HSTRING;
                    enumerator
                        .GetDevice(&HSTRING::from(id))
                        .or_else(|_| enumerator.GetDefaultAudioEndpoint(eRender, eConsole))?
                }
            } else {
                enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?
            };

            use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
            let vol: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None)?;
            if let Ok(muted) = vol.GetMute() {
                if muted.as_bool() {
                    return Ok(0.0);
                }
            }
            let level = vol.GetMasterVolumeLevelScalar()?;
            Ok(level * 100.0)
        }
    }

    pub fn set_volume(pct: f32, device_id: Option<&str>) -> Result<()> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: Result<IMMDeviceEnumerator> =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL);
            let enumerator = enumerator?;
            let dev: IMMDevice = if let Some(id) = device_id {
                if id == "default" || id.is_empty() {
                    enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?
                } else {
                    use windows::core::HSTRING;
                    enumerator
                        .GetDevice(&HSTRING::from(id))
                        .or_else(|_| enumerator.GetDefaultAudioEndpoint(eRender, eConsole))?
                }
            } else {
                enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?
            };

            use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
            let vol: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None)?;
            let clamped = (pct / 100.0).clamp(0.0, 1.0);
            vol.SetMasterVolumeLevelScalar(clamped, std::ptr::null())?;
            // If setting volume > 0 and it was muted, unmute. If setting 0, mute.
            if clamped > 0.0 {
                if let Ok(muted) = vol.GetMute() {
                    if muted.as_bool() {
                        let _ = vol.SetMute(false, std::ptr::null());
                    }
                }
            } else {
                let _ = vol.SetMute(true, std::ptr::null());
            }
            Ok(())
        }
    }
}

pub fn get_volume(device_id: Option<&str>) -> Result<f32, String> {
    #[cfg(windows)]
    return win_audio::get_volume(device_id).map_err(|e| e.to_string());
    #[cfg(not(windows))]
    Ok(70.0)
}

pub fn set_volume(pct: f32, device_id: Option<&str>) -> Result<(), String> {
    #[cfg(windows)]
    return win_audio::set_volume(pct, device_id).map_err(|e| e.to_string());
    #[cfg(not(windows))]
    Ok(())
}

pub fn list_devices() -> Vec<AudioDevice> {
    #[cfg(windows)]
    return win_audio::list_devices();
    #[cfg(not(windows))]
    Vec::new()
}

pub fn get_default_device() -> Option<AudioDevice> {
    #[cfg(windows)]
    return win_audio::get_default_device();
    #[cfg(not(windows))]
    None
}

pub fn get_spatial_capabilities(device_id: Option<&str>) -> SpatialCapabilities {
    #[cfg(windows)]
    return win_audio::get_spatial_capabilities(device_id);
    #[cfg(not(windows))]
    SpatialCapabilities::default()
}

pub fn resolve_device_name(identifier: Option<&str>) -> Option<String> {
    let id_or_name = identifier?;
    if id_or_name.is_empty() || id_or_name == "default" {
        return get_default_device().map(|d| d.name);
    }
    let devs = list_devices();
    if let Some(d) = devs.iter().find(|d| d.id == id_or_name) {
        return Some(d.name.clone());
    }
    if let Some(d) = devs.iter().find(|d| d.name.eq_ignore_ascii_case(id_or_name)) {
        return Some(d.name.clone());
    }
    if let Some(d) = devs.iter().find(|d| {
        let n_low = d.name.to_lowercase();
        let target_low = id_or_name.to_lowercase();
        n_low.contains(&target_low) || target_low.contains(&n_low)
    }) {
        return Some(d.name.clone());
    }
    Some(id_or_name.to_string())
}

pub fn find_dolby_atmos_device() -> Option<AudioDevice> {
    let devs = list_devices();
    devs.into_iter().find(|d| d.is_dolby_atmos)
}

pub fn get_effective_audio_device(
    configured_device: &str,
    prefer_dolby_atmos: bool,
) -> (Option<AudioDevice>, SpatialCapabilities) {
    let devs = list_devices();
    let def_dev = devs.iter().find(|d| d.is_default).cloned();

    // 1. If user explicitly specified a non-default device, respect it
    if !configured_device.is_empty() && configured_device != "default" {
        if let Some(d) = devs.iter().find(|d| d.id == configured_device || d.name == configured_device).cloned() {
            let caps = get_spatial_capabilities(Some(&d.id));
            return (Some(d), caps);
        }
    }

    // 2. If prefer_dolby_atmos is enabled, utilize a Dolby Atmos device if available
    if prefer_dolby_atmos {
        if let Some(ref def) = def_dev {
            if def.is_dolby_atmos {
                let caps = get_spatial_capabilities(Some(&def.id));
                return (Some(def.clone()), caps);
            }
        }
        if let Some(atmos_dev) = find_dolby_atmos_device() {
            let caps = get_spatial_capabilities(Some(&atmos_dev.id));
            return (Some(atmos_dev), caps);
        }
    }

    // 3. Fall back to Windows default device
    if let Some(ref def) = def_dev {
        let caps = get_spatial_capabilities(Some(&def.id));
        return (Some(def.clone()), caps);
    }

    (None, get_spatial_capabilities(None))
}

static WATCHER_RUNNING: AtomicBool = AtomicBool::new(false);

/// Watcher thread that checks for Windows default audio device changes
/// and notifies the frontend or auto-switches playback if enabled.
pub fn ensure_watcher(app: AppHandle) {
    if WATCHER_RUNNING.swap(true, Ordering::SeqCst) {
        return; // already started
    }

    std::thread::spawn(move || {
        let mut last_default_id: Option<String> = None;
        let mut last_device_ids: Vec<String> = Vec::new();
        let mut last_volume: f32 = -1.0;
        let mut last_effective_id: Option<String> = None;
        let mut last_spatial: Option<SpatialCapabilities> = None;

        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if !WATCHER_RUNNING.load(Ordering::Relaxed) {
                break;
            }

            // Poll current volume to notify frontend if system volume changes
            if let Ok(vol) = get_volume(None) {
                if (vol - last_volume).abs() >= 0.5 {
                    last_volume = vol;
                    let _ = app.emit("audio://volume-changed", serde_json::json!({ "volume": vol }));
                }
            }

            let current_default = get_default_device();
            let current_devices = list_devices();
            let current_ids: Vec<String> = current_devices.iter().map(|d| d.id.clone()).collect();

            let devices_changed = current_ids != last_device_ids;
            if devices_changed {
                last_device_ids = current_ids;
            }

            let cfg = crate::engines::get_config();
            let (effective_device, current_spatial) = get_effective_audio_device(
                &cfg.audio_output_device,
                cfg.prefer_dolby_atmos,
            );
            let effective_id = effective_device.as_ref().map(|d| d.id.clone());

            let effective_changed = match &last_effective_id {
                None => {
                    last_effective_id = effective_id.clone();
                    false
                }
                Some(prev) => Some(prev) != effective_id.as_ref(),
            };

            let spatial_changed = match &last_spatial {
                None => {
                    last_spatial = Some(current_spatial.clone());
                    false
                }
                Some(prev) => prev != &current_spatial,
            };

            if effective_changed || spatial_changed {
                last_effective_id = effective_id.clone();
                last_spatial = Some(current_spatial.clone());
                println!(
                    "[audio] Effective endpoint changed: {:?} (Dolby Atmos={}, Tech={}, Channels={})",
                    effective_device.as_ref().map(|d| &d.name),
                    current_spatial.is_dolby_atmos,
                    current_spatial.spatial_technology,
                    current_spatial.max_channels
                );
                let _ = app.emit(
                    "audio://spatial-changed",
                    serde_json::json!({
                        "spatial": current_spatial,
                        "device": effective_device,
                    }),
                );
                if cfg.auto_switch_audio_output {
                    if crate::engines::is_airplay_receiver_running() {
                        let _ = crate::engines::restart_airplay_receiver(&app);
                    }
                    if crate::airplay2::is_running() {
                        crate::airplay2::stop(Some(&app));
                        let _ = crate::airplay2::start(app.clone());
                    }
                }
            }

            if let Some(ref def) = current_default {
                let default_changed = match &last_default_id {
                    None => {
                        last_default_id = Some(def.id.clone());
                        false
                    }
                    Some(last_id) => last_id != &def.id,
                };

                if default_changed {
                    last_default_id = Some(def.id.clone());
                    println!(
                        "[audio] Windows default audio output changed to: {} ({}) [Dolby Atmos: {}]",
                        def.name, def.id, def.is_dolby_atmos
                    );

                    let cfg = crate::engines::get_config();
                    if cfg.auto_switch_audio_output && !effective_changed && !spatial_changed {
                        println!(
                            "[audio] Auto-switch enabled: automatically switching playback to {}",
                            def.name
                        );
                        let _ = app.emit(
                            "audio://device-switched",
                            serde_json::json!({
                                "device": def,
                                "auto": true,
                            }),
                        );
                        if crate::engines::is_airplay_receiver_running() {
                            let _ = crate::engines::restart_airplay_receiver(&app);
                        }
                        if crate::airplay2::is_running() {
                            crate::airplay2::stop(Some(&app));
                            let _ = crate::airplay2::start(app.clone());
                        }
                    }

                    // Always notify frontend of default device change and full device list
                    let _ = app.emit(
                        "audio://default-changed",
                        serde_json::json!({
                            "device": def,
                            "devices": current_devices,
                            "autoSwitched": cfg.auto_switch_audio_output,
                            "spatial": current_spatial,
                            "effectiveDevice": effective_device,
                        }),
                    );
                } else if devices_changed {
                    // Devices plugged in / removed, notify frontend so dropdown is fresh
                    let _ = app.emit(
                        "audio://devices-changed",
                        serde_json::json!({
                            "devices": current_devices,
                            "spatial": current_spatial,
                            "effectiveDevice": effective_device,
                        }),
                    );
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_devices() {
        let devs = list_devices();
        println!("Found {} audio devices:", devs.len());
        for d in &devs {
            println!("  - {} (ID: {}, Default: {})", d.name, d.id, d.is_default);
        }
        assert!(!devs.is_empty(), "Should find at least 1 audio device on Windows");
        let def = get_default_device();
        println!("Default device: {:?}", def);
        assert!(def.is_some());

        let vol = get_volume(None);
        println!("Current system volume: {:?}", vol);
        assert!(vol.is_ok(), "Getting volume should succeed");
    }

    #[test]
    fn test_uxplay_with_device() {
        let devs = list_devices();
        if let Some(dev) = devs.first() {
            let mut cmd = std::process::Command::new(
                r"C:\Program Files (x86)\uxplay-windows\_internal\bin\uxplay.exe",
            );
            let sink_arg = format!("wasapisink device=\"{}\"", dev.id);
            cmd.args(["-as", &sink_arg, "-h"]);
            let output = cmd.output().expect("failed to run uxplay");
            println!("UxPlay -h with {}: status={:?}", sink_arg, output.status);
            assert!(output.status.success());
        }
    }

    #[test]
    fn test_spatial_capabilities_detection() {
        let devs = list_devices();
        println!("Checking spatial capabilities for {} devices:", devs.len());
        for d in &devs {
            println!(
                "  - {}: spatial={}, dolby_atmos={}, tech='{}'",
                d.name, d.supports_spatial, d.is_dolby_atmos, d.spatial_technology
            );
        }
        assert!(!devs.is_empty(), "Audio devices must be found");

        let default_spatial = get_spatial_capabilities(None);
        println!("Default spatial capabilities: {:?}", default_spatial);
        assert!(default_spatial.max_channels >= 1);

        // Check if laptop speakers are detected with Dolby Atmos
        if let Some(speakers) = devs.iter().find(|d| d.name.contains("Speakers") || d.name.contains("Realtek")) {
            println!("Testing Laptop Speakers specifically: {:?}", speakers);
            let speaker_caps = get_spatial_capabilities(Some(&speakers.id));
            println!("Speaker spatial capabilities: {:?}", speaker_caps);
            assert!(speaker_caps.is_dolby_atmos, "Laptop speakers with DAX3 should be detected as Dolby Atmos");
            assert!(speaker_caps.is_spatial_supported, "Spatial audio should be supported on speakers");
        }
    }

    #[test]
    fn test_spatial_capabilities_serde() {
        let caps = SpatialCapabilities {
            is_spatial_supported: true,
            is_dolby_atmos: true,
            spatial_technology: "Dolby Atmos (Hardware / DAX3)".to_string(),
            active_format: "DolbyAtmosForSpeakers".to_string(),
            max_channels: 8,
        };
        let json_str = serde_json::to_string(&caps).expect("Must serialize to json");
        assert!(json_str.contains("isDolbyAtmos"));
        assert!(json_str.contains("spatialTechnology"));
        let decoded: SpatialCapabilities = serde_json::from_str(&json_str).expect("Must deserialize");
        assert_eq!(caps, decoded);
    }

    #[test]
    fn test_device_name_resolution_and_effective_endpoint() {
        let devs = list_devices();
        assert!(!devs.is_empty());
        let first = &devs[0];

        // Resolving by ID should return the name
        let resolved = resolve_device_name(Some(&first.id));
        assert_eq!(resolved, Some(first.name.clone()));

        // Resolving by name should return the name
        let resolved_name = resolve_device_name(Some(&first.name));
        assert_eq!(resolved_name, Some(first.name.clone()));

        // Resolving "default" should return the default device name
        let def_name = resolve_device_name(Some("default"));
        assert!(def_name.is_some());

        // Test querying spatial capabilities by friendly name instead of ID
        let caps_by_name = get_spatial_capabilities(Some(&first.name));
        let caps_by_id = get_spatial_capabilities(Some(&first.id));
        assert_eq!(caps_by_name.is_dolby_atmos, caps_by_id.is_dolby_atmos);
        assert_eq!(caps_by_name.is_spatial_supported, caps_by_id.is_spatial_supported);

        // Test effective device with prefer_dolby_atmos = true
        let (eff_dev, eff_caps) = get_effective_audio_device("default", true);
        if devs.iter().any(|d| d.is_dolby_atmos) {
            assert!(eff_caps.is_dolby_atmos, "Should select Dolby Atmos endpoint when available");
            assert!(eff_dev.as_ref().map(|d| d.is_dolby_atmos).unwrap_or(false));
        }

        // Test effective device with prefer_dolby_atmos = false
        let (eff_dev_nopref, _) = get_effective_audio_device("default", false);
        let def_dev = get_default_device();
        assert_eq!(eff_dev_nopref.map(|d| d.id), def_dev.map(|d| d.id));
    }
}
