//! AirCast module: Outbound audio casting (Google Cast & AirPlay), inbound media (DIAL & DLNA), and Android screen mirroring (Scrcpy).

pub mod airconnect;
pub mod cast_sender;
pub mod dial;
pub mod discovery;
pub mod dlna;
pub mod http_stream;
pub mod loopback;
pub mod scrcpy;

#[cfg(test)]
mod tests;

use serde_json::json;
use tauri::AppHandle;

/// Initialize all AirCast background services: HTTP chunked stream server,
/// Google Cast mDNS discovery, and DIAL/SSDP responders.
pub fn init(app: &AppHandle) {
    http_stream::ensure_http_server(Some(app.clone()));
    discovery::start_cast_discovery(Some(app.clone()));
    dial::start_ssdp_listener();
}

/// Outbound casting to Google Cast or AirPlay speaker.
pub fn start_aircast(app: &AppHandle, target_ip: &str) -> anyhow::Result<()> {
    // 1. Try Google Cast pure-Rust sender first (on port 8009)
    if let Ok(()) = cast_sender::start_cast(target_ip, Some(app)) {
        return Ok(());
    }

    // 2. If Cast fails or target is AirPlay RAOP, try AirConnect sidecar bridge
    if let Ok(()) = airconnect::start_airconnect(app, "aircast.exe", &["-x", target_ip]) {
        return Ok(());
    }

    if let Ok(()) = airconnect::start_airconnect(app, "airupnp.exe", &["-x", target_ip]) {
        return Ok(());
    }

    Err(anyhow::anyhow!("Failed to connect to cast target {target_ip}"))
}

pub fn stop_aircast() {
    cast_sender::stop_cast();
    airconnect::stop_airconnect();
}

pub fn aircast_state() -> serde_json::Value {
    json!({
        "casting": cast_sender::is_casting(),
        "target": cast_sender::current_target(),
        "loopbackRunning": loopback::is_running(),
        "httpPort": http_stream::http_port(),
        "scrcpyRunning": scrcpy::is_scrcpy_running(),
    })
}
