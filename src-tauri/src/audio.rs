// Windows audio output device enumeration and default device change monitoring

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[cfg(windows)]
mod win_audio {
    use super::AudioDevice;
    use windows::core::{Result, GUID};
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Com::*;
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

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
                                result.push(AudioDevice {
                                    id: id_str,
                                    name,
                                    is_default: is_def,
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
            Some(AudioDevice {
                id,
                name,
                is_default: true,
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

        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));

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
                        "[audio] Windows default audio output changed to: {} ({})",
                        def.name, def.id
                    );

                    let cfg = crate::engines::get_config();
                    if cfg.auto_switch_audio_output {
                        println!(
                            "[audio] Auto-switch enabled: automatically switching playback to {}",
                            def.name
                        );
                        // Emit event that sound output automatically switched
                        let _ = app.emit(
                            "audio://device-switched",
                            serde_json::json!({
                                "device": def,
                                "auto": true,
                            }),
                        );
                        // If receiver is running, restart it with new sound output
                        if crate::engines::is_airplay_receiver_running() {
                            let _ = crate::engines::restart_airplay_receiver(&app);
                        }
                    }

                    // Always notify frontend of default device change and full device list
                    let _ = app.emit(
                        "audio://default-changed",
                        serde_json::json!({
                            "device": def,
                            "devices": current_devices,
                            "autoSwitched": cfg.auto_switch_audio_output,
                        }),
                    );
                } else if devices_changed {
                    // Devices plugged in / removed, notify frontend so dropdown is fresh
                    let _ = app.emit(
                        "audio://devices-changed",
                        serde_json::json!({
                            "devices": current_devices,
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
}
