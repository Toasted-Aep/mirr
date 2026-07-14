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

pub fn airplay_devices() -> Vec<Device> {
    AIRPLAY.lock().unwrap().values().cloned().collect()
}

pub fn raop_targets() -> Vec<Target> {
    RAOP.lock().unwrap().values().cloned().collect()
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
            if let ServiceEvent::ServiceResolved(info) = event {
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
        }
    });

    // keep the daemon alive for the whole process
    std::mem::forget(mdns);
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
