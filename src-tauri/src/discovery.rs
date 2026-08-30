//! mDNS / Bonjour discovery for Apple wireless services.
//!
//! Browses the local network for:
//!   * `_airplay._tcp`  — AirPlay screen / video receivers (devices we can mirror FROM,
//!                        and a sanity check on what's advertising).
//!   * `_raop._tcp`     — AirPlay audio (RAOP) targets we can CAST to (HomePod,
//!                        AirPort Express, third-party AirPlay speakers).
//!
//! NOTE: *Advertising* this PC as an AirPlay receiver (so an iPhone shows "Mirr"
//! in its Screen-Mirroring sheet) is performed by the AirPlay engine sidecar
//! (UxPlay), which owns the full AirPlay handshake + TXT records. This module is
//! the read side: it tells the UI what's out there.

use mdns_sd::{ServiceDaemon, ServiceEvent};
use once_cell::sync::Lazy;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Clone, Serialize, Debug)]
pub struct Device {
    pub name: String,
    pub meta: String,
    pub online: bool,
    #[serde(rename = "type")]
    pub kind: String,
    pub addr: String,
}

#[derive(Clone, Serialize, Debug)]
pub struct Target {
    pub name: String,
    pub meta: String,
    pub on: bool,
    pub vol: u8,
    pub addr: String,
}

static AIRPLAY: Lazy<Mutex<HashMap<String, Device>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static RAOP: Lazy<Mutex<HashMap<String, Target>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static DAEMON: once_cell::sync::OnceCell<ServiceDaemon> = once_cell::sync::OnceCell::new();

/// The app's single mDNS daemon (also keeps it alive for the process).
/// dacp.rs browses `_dacp._tcp` on it; falls back to its own daemon if None.
pub fn shared_daemon() -> Option<ServiceDaemon> {
    DAEMON.get().cloned()
}

/// Sorted by name, deliberately: these come out of a HashMap, whose iteration
/// order is arbitrary and can differ between two calls with identical contents.
/// The UI polls this every 5 s and skips re-rendering when the JSON is
/// unchanged, so an unstable order would rebuild the whole card grid on every
/// poll (and visibly shuffle the cards).
pub fn airplay_devices() -> Vec<Device> {
    let mut v: Vec<Device> = AIRPLAY.lock().unwrap().values().cloned().collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

/// Sorted by name for the same reason as `airplay_devices`.
pub fn raop_targets() -> Vec<Target> {
    let mut v: Vec<Target> = RAOP.lock().unwrap().values().cloned().collect();
    // RAOP is keyed by "MAC@name", not the prettified name, so two speakers
    // sharing a friendly name would otherwise tie non-deterministically.
    v.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.addr.cmp(&b.addr)));
    v
}

/// Begin browsing. Spawns background threads that live for the app's lifetime.
pub fn start() -> anyhow::Result<()> {
    let mdns = ServiceDaemon::new()?;

    // --- AirPlay screen / video receivers ---
    let ap_rx = mdns.browse("_airplay._tcp.local.")?;
    std::thread::spawn(move || {
        while let Ok(event) = ap_rx.recv() {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    let name = clean_name(info.get_fullname());
                    if name == "Mirr" {
                        continue; // our own receiver advertising via UxPlay
                    }
                    let addr = info
                        .get_addresses()
                        .iter()
                        .next()
                        .map(|a| a.to_string())
                        .unwrap_or_default();
                    let model = info
                        .get_property_val_str("model")
                        .unwrap_or("Apple device")
                        .to_string();
                    let dev = Device {
                        kind: guess_kind(&name, &model),
                        name: name.clone(),
                        meta: model,
                        online: true,
                        addr,
                    };
                    AIRPLAY.lock().unwrap().insert(name, dev);
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    let name = clean_name(&fullname);
                    if let Some(d) = AIRPLAY.lock().unwrap().get_mut(&name) {
                        d.online = false;
                    }
                }
                _ => {}
            }
        }
    });

    // --- AirPlay audio (RAOP) targets we can cast to ---
    let raop_rx = mdns.browse("_raop._tcp.local.")?;
    std::thread::spawn(move || {
        while let Ok(event) = raop_rx.recv() {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    let raw = clean_name(info.get_fullname());
                    let addr = info
                        .get_addresses()
                        .iter()
                        .next()
                        .map(|a| a.to_string())
                        .unwrap_or_default();
                    let target = Target {
                        name: pretty_raop(&raw),
                        meta: "AirPlay audio · RAOP".into(),
                        on: false,
                        vol: 50,
                        addr,
                    };
                    RAOP.lock().unwrap().insert(raw, target);
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    // Mirrors the AirPlay arm above. Without this the map only
                    // ever grew: on a laptop that roams networks, speakers from
                    // the office stayed listed as castable all session, and
                    // `list_airplay_targets` (polled every 8 s) cloned the whole
                    // accumulated set each time. `Target` has no reachability
                    // flag to flip the way `Device.online` does — `on` means
                    // "currently casting to it" — so drop the entry outright.
                    RAOP.lock().unwrap().remove(&clean_name(&fullname));
                }
                _ => {}
            }
        }
    });

    // keep the daemon alive for the whole process
    let _ = DAEMON.set(mdns);
    Ok(())
}

/// "Living Room._airplay._tcp.local." -> "Living Room"
fn clean_name(fullname: &str) -> String {
    fullname
        .split("._")
        .next()
        .unwrap_or(fullname)
        .replace('\\', "")
        .trim()
        .to_string()
}

/// RAOP names are often "AABBCCDDEEFF@Speaker Name" — keep the friendly part.
fn pretty_raop(raw: &str) -> String {
    match raw.split_once('@') {
        Some((_, name)) => name.to_string(),
        None => raw.to_string(),
    }
}

fn guess_kind(name: &str, model: &str) -> String {
    let s = format!("{name} {model}").to_lowercase();
    if s.contains("iphone") {
        "iphone"
    } else if s.contains("ipad") {
        "ipad"
    } else if s.contains("mac") {
        "mac"
    } else if s.contains("appletv") || s.contains("apple tv") {
        "tv"
    } else if s.contains("homepod") || s.contains("audio") {
        "speaker"
    } else {
        "generic"
    }
    .to_string()
}
