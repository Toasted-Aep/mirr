// Mirr — Windows 11 Apple wireless bridge
// AirPlay (receive) · AirPlay 2 (audio) · AirCast (send) · LocalSend (transfer)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod discovery;
#[cfg(windows)]
mod embed;
mod engines;
mod localsend;

use tauri::Emitter;

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
    discovery::raop_targets()
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
fn start_aircast(app: tauri::AppHandle, target_ip: String) -> Result<(), String> {
    engines::start_aircast(&app, &target_ip).map_err(|e| e.to_string())
}

#[tauri::command]
fn start_airplay2_receiver(app: tauri::AppHandle) -> Result<(), String> {
    engines::start_airplay2_receiver(&app).map_err(|e| e.to_string())
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
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // mDNS discovery of AirPlay screen receivers + RAOP audio targets
            if let Err(e) = discovery::start() {
                eprintln!("[discovery] {e}");
            }
            // LocalSend multicast discovery + HTTP receive server
            localsend::start(app.handle().clone());
            // Auto-start the AirPlay receiver so the PC is discoverable
            // the moment the app opens — no launcher scripts needed.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                if let Err(e) = engines::start_airplay_receiver(&handle) {
                    let _ = handle.emit(
                        "airplay://status",
                        serde_json::json!({ "state": "error", "detail": e.to_string() }),
                    );
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
            start_aircast,
            start_airplay2_receiver,
            set_video_rect,
            set_video_visible,
            mirror_active,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Mirr");
}
