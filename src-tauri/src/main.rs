// Mirr — Windows 11 Apple wireless bridge
// AirPlay (receive) · AirPlay 2 (audio) · AirCast (send) · LocalSend (transfer)
#![windows_subsystem = "windows"]

mod aircast;
mod airplay2;
mod audio;
mod dacp;
mod discovery;
#[cfg(windows)]
mod embed;
mod engines;
mod localsend;
mod nowplaying;

use tauri::{Emitter, Manager};

#[tauri::command]
fn device_name() -> String {
    localsend::device_alias()
}

#[tauri::command]
fn list_airplay_devices() -> Vec<discovery::Device> {
    discovery::airplay_devices()
}

#[tauri::command]
fn list_airplay_targets() -> Vec<discovery::Target> {
    let mut targets = discovery::raop_targets();
    for ct in aircast::discovery::list_cast_targets() {
        targets.push(discovery::Target {
            name: ct.name,
            meta: ct.meta,
            on: aircast::cast_sender::is_casting()
                && aircast::cast_sender::current_target().as_deref() == Some(&ct.addr),
            vol: 100,
            addr: ct.addr,
        });
    }
    targets
}

#[tauri::command]
fn list_localsend_peers() -> Vec<localsend::Peer> {
    localsend::peers()
}

#[tauri::command]
async fn localsend_send(target_ip: String, port: u16, path: String) -> Result<(), String> {
    localsend::send_file(&target_ip, port, &path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn start_airplay_receiver(app: tauri::AppHandle) -> Result<(), String> {
    engines::start_airplay_receiver(&app).map_err(|e| e.to_string())
}

#[tauri::command]
fn stop_airplay_receiver(app: tauri::AppHandle) {
    engines::stop_airplay_receiver(&app);
}

#[tauri::command]
fn receiver_state() -> serde_json::Value {
    engines::receiver_state()
}

#[tauri::command]
fn network_diagnostics(app: tauri::AppHandle) -> serde_json::Value {
    engines::diagnostics(&app)
}

#[tauri::command]
fn get_settings() -> engines::EngineConfig {
    engines::get_config()
}

#[tauri::command]
fn list_audio_devices() -> Vec<audio::AudioDevice> {
    audio::list_devices()
}

#[tauri::command]
fn get_default_audio_device() -> Option<audio::AudioDevice> {
    audio::get_default_device()
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EffectiveAudioDeviceResponse {
    device: Option<audio::AudioDevice>,
    spatial: audio::SpatialCapabilities,
}

#[tauri::command]
fn get_spatial_audio_capabilities(device_id: Option<String>) -> audio::SpatialCapabilities {
    if let Some(ref id) = device_id {
        audio::get_spatial_capabilities(Some(id.as_str()))
    } else {
        let cfg = engines::get_config();
        let (_dev, spatial) = audio::get_effective_audio_device(&cfg.audio_output_device, cfg.prefer_dolby_atmos);
        spatial
    }
}

#[tauri::command]
fn get_effective_audio_device(
    device_id: Option<String>,
    prefer_dolby_atmos: Option<bool>,
) -> EffectiveAudioDeviceResponse {
    let cfg = engines::get_config();
    let configured = device_id.unwrap_or(cfg.audio_output_device);
    let prefer = prefer_dolby_atmos.unwrap_or(cfg.prefer_dolby_atmos);
    let (dev, spatial) = audio::get_effective_audio_device(&configured, prefer);
    EffectiveAudioDeviceResponse {
        device: dev,
        spatial,
    }
}

#[tauri::command]
fn update_settings(
    app: tauri::AppHandle,
    config: Option<engines::EngineConfig>,
    restart_receiver: Option<bool>,
    broadcast_name: Option<String>,
    fps: Option<u32>,
    vsync: Option<bool>,
    always_on_top_mirror: Option<bool>,
    auto_theater: Option<bool>,
    audio_output_device: Option<String>,
    auto_switch_audio_output: Option<bool>,
    prefer_dolby_atmos: Option<bool>,
    show_transport_controls: Option<bool>,
    receiver_mode: Option<String>,
) -> Result<engines::EngineConfig, String> {
    let cfg = if let Some(c) = config {
        c
    } else {
        let mut c = engines::get_config();
        if let Some(b) = broadcast_name { c.broadcast_name = b; }
        if let Some(f) = fps { c.fps = f; }
        if let Some(v) = vsync { c.vsync = v; }
        if let Some(a) = always_on_top_mirror { c.always_on_top_mirror = a; }
        if let Some(at) = auto_theater { c.auto_theater = at; }
        if let Some(ad) = audio_output_device { c.audio_output_device = ad; }
        if let Some(asw) = auto_switch_audio_output { c.auto_switch_audio_output = asw; }
        if let Some(pda) = prefer_dolby_atmos { c.prefer_dolby_atmos = pda; }
        if let Some(stc) = show_transport_controls { c.show_transport_controls = stc; }
        if let Some(rm) = receiver_mode { c.receiver_mode = rm; }
        c
    };
    let should_restart = restart_receiver.unwrap_or(false);
    let saved = engines::set_config(cfg);
    if saved.broadcast_name != "Mirr" {
        localsend::set_custom_alias(Some(saved.broadcast_name.clone()));
    } else {
        localsend::set_custom_alias(None);
    }
    if should_restart {
        if saved.receiver_mode != "airplay2" {
            engines::stop_airplay_receiver(&app);
            let _ = engines::start_airplay_receiver(&app);
        } else {
            engines::stop_airplay_receiver(&app);
        }

        if saved.receiver_mode != "airplay1" {
            engines::stop_airplay2_receiver(&app);
            let _ = engines::start_airplay2_receiver(&app);
        } else {
            engines::stop_airplay2_receiver(&app);
        }
    }
    Ok(saved)
}

#[tauri::command]
fn set_always_on_top(app: tauri::AppHandle, on_top: bool) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("main") {
        win.set_always_on_top(on_top).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn set_pip_mode(app: tauri::AppHandle, pip: bool, restore_always_on_top: bool) -> Result<(), String> {
    let Some(win) = app.get_webview_window("main") else {
        return Err("main window not found".into());
    };
    if pip {
        let _ = win.unmaximize();
        let _ = win.set_min_size(Some(tauri::Size::Logical(tauri::LogicalSize {
            width: 220.0,
            height: 200.0,
        })));
        let _ = win.set_size(tauri::Size::Logical(tauri::LogicalSize {
            width: 320.0,
            height: 320.0,
        }));
        let _ = win.set_always_on_top(true);
    } else {
        let _ = win.set_always_on_top(restore_always_on_top);
        let _ = win.set_min_size(Some(tauri::Size::Logical(tauri::LogicalSize {
            width: 920.0,
            height: 620.0,
        })));
        let _ = win.set_size(tauri::Size::Logical(tauri::LogicalSize {
            width: 1180.0,
            height: 760.0,
        }));
    }
    Ok(())
}

#[tauri::command]
fn reveal_in_explorer(path: Option<String>) -> Result<(), String> {
    let dir = localsend::downloads_dir();
    let _ = std::fs::create_dir_all(&dir);
    let canon_dir = dir.canonicalize().unwrap_or_else(|_| dir.clone());
    if let Some(p) = path.filter(|s| !s.is_empty()) {
        let target = std::path::PathBuf::from(p);
        if let Ok(canon_target) = target.canonicalize() {
            if canon_target.starts_with(&canon_dir) {
                std::process::Command::new("explorer.exe")
                    .arg(format!("/select,{}", canon_target.display()))
                    .spawn()
                    .map_err(|e| e.to_string())?;
                return Ok(());
            }
        }
    }
    std::process::Command::new("explorer.exe")
        .arg(&dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn nowplaying_state() -> serde_json::Value {
    nowplaying::state()
}

#[tauri::command]
fn dacp_state() -> serde_json::Value {
    dacp::state()
}

#[tauri::command]
async fn dacp_command(cmd: String) -> Result<u16, String> {
    dacp::command(cmd).await
}

#[tauri::command]
fn audio_get_volume() -> Result<f32, String> {
    let cfg = engines::get_config();
    let dev_id = if cfg.audio_output_device == "default" || cfg.audio_output_device.is_empty() {
        None
    } else {
        Some(cfg.audio_output_device.as_str())
    };
    audio::get_volume(dev_id)
}

#[tauri::command]
fn audio_set_volume(pct: f32) -> Result<(), String> {
    let cfg = engines::get_config();
    let dev_id = if cfg.audio_output_device == "default" || cfg.audio_output_device.is_empty() {
        None
    } else {
        Some(cfg.audio_output_device.as_str())
    };
    audio::set_volume(pct, dev_id)
}

#[tauri::command]
async fn dacp_set_volume(pct: f32) -> Result<u16, String> {
    // 1. Directly adjust Windows audio endpoint volume on the active/selected playback device
    let cfg = engines::get_config();
    let dev_id = if cfg.audio_output_device == "default" || cfg.audio_output_device.is_empty() {
        None
    } else {
        Some(cfg.audio_output_device.as_str())
    };
    let _ = audio::set_volume(pct, dev_id);

    // 2. Also send DACP volume command to the connected Apple device if DACP is active
    let _ = dacp::set_volume(pct).await;
    Ok(200)
}

#[tauri::command]
async fn dacp_play_pause() -> Result<u16, String> {
    dacp::command("playpause".into()).await
}

#[tauri::command]
async fn dacp_next() -> Result<u16, String> {
    dacp::command("nextitem".into()).await
}

#[tauri::command]
async fn dacp_prev() -> Result<u16, String> {
    dacp::command("previtem".into()).await
}

#[tauri::command]
fn start_aircast(app: tauri::AppHandle, target_ip: String) -> Result<(), String> {
    engines::start_aircast(&app, &target_ip).map_err(|e| e.to_string())
}

#[tauri::command]
fn stop_aircast() {
    engines::stop_aircast();
}

#[tauri::command]
fn aircast_state() -> serde_json::Value {
    aircast::aircast_state()
}

#[tauri::command]
fn list_cast_targets() -> Vec<aircast::discovery::CastTarget> {
    aircast::discovery::list_cast_targets()
}

#[tauri::command]
fn scrcpy_start(app: tauri::AppHandle, target: Option<String>) -> Result<(), String> {
    aircast::scrcpy::start_scrcpy(&app, target).map_err(|e| e.to_string())
}

#[tauri::command]
fn scrcpy_stop() {
    aircast::scrcpy::stop_scrcpy();
}

#[tauri::command]
fn scrcpy_status() -> bool {
    aircast::scrcpy::is_scrcpy_running()
}

#[tauri::command]
fn scrcpy_list_devices(app: tauri::AppHandle) -> Vec<aircast::scrcpy::AdbDevice> {
    aircast::scrcpy::list_adb_devices(Some(&app))
}

#[tauri::command]
fn scrcpy_connect_wireless(app: tauri::AppHandle, ip: String) -> Result<String, String> {
    aircast::scrcpy::connect_wireless_adb(Some(&app), &ip).map_err(|e| e.to_string())
}

#[tauri::command]
fn start_airplay2_receiver(app: tauri::AppHandle) -> Result<(), String> {
    engines::start_airplay2_receiver(&app).map_err(|e| e.to_string())
}

#[tauri::command]
fn stop_airplay2_receiver(app: tauri::AppHandle) {
    engines::stop_airplay2_receiver(&app);
}

#[tauri::command]
fn airplay2_state() -> serde_json::Value {
    engines::airplay2_state()
}

/// The frontend reports where the mirror video should be docked, in physical
/// pixels relative to the main window's client area. `zoom` is percent (100 = fit).
#[tauri::command]
fn set_video_rect(x: i32, y: i32, w: i32, h: i32, radius: i32, zoom: i32) {
    #[cfg(windows)]
    embed::set_target_rect(x, y, w, h, radius, zoom);
    #[cfg(not(windows))]
    let _ = (x, y, w, h, radius, zoom);
}

/// Hide/show the docked video when the user switches panes — the native child
/// window would otherwise paint over every view.
#[tauri::command]
fn set_video_visible(visible: bool) {
    #[cfg(windows)]
    embed::set_video_visible(visible);
    #[cfg(not(windows))]
    let _ = visible;
}

#[tauri::command]
fn mirror_active() -> bool {
    #[cfg(windows)]
    return embed::mirror_active();
    #[cfg(not(windows))]
    false
}

fn main() {
    #[cfg(windows)]
    unsafe {
        let _ = windows::Win32::System::Console::FreeConsole();
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Set AppHandle for live discovery event emission
            discovery::set_app_handle(app.handle().clone());
            // mDNS discovery of AirPlay screen receivers + RAOP audio targets
            if let Err(e) = discovery::start() {
                eprintln!("[discovery] {e}");
            }
            // LocalSend multicast discovery + HTTP receive server
            localsend::start(app.handle().clone());
            // AirCast: HTTP chunked stream server, Google Cast discovery, DIAL/SSDP responders
            aircast::init(&app.handle().clone());
            // Windows audio output device watcher & auto-switching
            audio::ensure_watcher(app.handle().clone());
            // Auto-start engines based on configured receiver_mode
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let cfg = engines::get_config();
                if cfg.receiver_mode != "airplay2" {
                    if let Err(e) = engines::start_airplay_receiver(&handle) {
                        let _ = handle.emit(
                            "airplay://status",
                            serde_json::json!({ "state": "error", "detail": e.to_string() }),
                        );
                    }
                }
            });
            // Auto-start the embedded AirPlay 2 receiver ("Mirr Audio" on port 7000)
            let handle_a2 = app.handle().clone();
            std::thread::spawn(move || {
                let cfg = engines::get_config();
                if cfg.receiver_mode != "airplay1" {
                    if let Err(e) = engines::start_airplay2_receiver(&handle_a2) {
                        let _ = handle_a2.emit(
                            "airplay2://status",
                            serde_json::json!({ "state": "error", "detail": e.to_string() }),
                        );
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|_window, event| match event {
            // kill the engines while our window still exists: the mirror video
            // is a cross-process child of it, and tearing the parent down first
            // can stall on the engine's window thread
            tauri::WindowEvent::CloseRequested { .. } => engines::shutdown_all(),
            tauri::WindowEvent::Destroyed => engines::shutdown_all(),
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            device_name,
            list_airplay_devices,
            list_airplay_targets,
            list_localsend_peers,
            localsend_send,
            start_airplay_receiver,
            stop_airplay_receiver,
            receiver_state,
            network_diagnostics,
            get_settings,
            update_settings,
            set_always_on_top,
            set_pip_mode,
            reveal_in_explorer,
            nowplaying_state,
            dacp_state,
            dacp_command,
            dacp_set_volume,
            dacp_play_pause,
            dacp_next,
            dacp_prev,
            start_aircast,
            stop_aircast,
            aircast_state,
            list_cast_targets,
            scrcpy_start,
            scrcpy_stop,
            scrcpy_status,
            scrcpy_list_devices,
            scrcpy_connect_wireless,
            start_airplay2_receiver,
            stop_airplay2_receiver,
            airplay2_state,
            set_video_rect,
            set_video_visible,
            mirror_active,
            list_audio_devices,
            get_default_audio_device,
            get_effective_audio_device,
            get_spatial_audio_capabilities,
            audio_get_volume,
            audio_set_volume,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Mirr");
}
