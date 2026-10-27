//! mDNS discovery for Google Cast devices (`_googlecast._tcp.local.`).

use mdns_sd::{ServiceDaemon, ServiceEvent};
use once_cell::sync::Lazy;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

#[derive(Clone, Serialize, Debug, PartialEq)]
pub struct CastTarget {
    pub name: String,
    pub meta: String,
    pub addr: String,
    pub port: u16,
    pub model: String,
    pub is_cast: bool,
}

static CAST_TARGETS: Lazy<Mutex<HashMap<String, CastTarget>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub fn list_cast_targets() -> Vec<CastTarget> {
    let mut v: Vec<CastTarget> = CAST_TARGETS.lock().unwrap().values().cloned().collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

pub fn start_cast_discovery(app_handle: Option<AppHandle>) {
    std::thread::Builder::new()
        .name("googlecast-discovery".to_string())
        .spawn(move || {
            let daemon = match crate::discovery::shared_daemon() {
                Some(d) => d,
                None => match ServiceDaemon::new() {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("[cast-discovery] Failed to create mDNS daemon: {e}");
                        return;
                    }
                },
            };

            let receiver = match daemon.browse("_googlecast._tcp.local.") {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[cast-discovery] Failed to browse _googlecast._tcp: {e}");
                    return;
                }
            };

            while let Ok(event) = receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let fn_prop = info.get_property_val_str("fn");
                        let md_prop = info.get_property_val_str("md");

                        let name = fn_prop
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| info.get_fullname().trim_end_matches("._googlecast._tcp.local.").to_string());

                        let model = md_prop
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "Google Cast Device".to_string());

                        let addr = info
                            .get_addresses_v4()
                            .into_iter()
                            .next()
                            .map(|ip| ip.to_string())
                            .unwrap_or_else(|| "127.0.0.1".to_string());

                        let port = info.get_port();
                        let meta = format!("{model} · Google Cast");

                        let target = CastTarget {
                            name: name.clone(),
                            meta,
                            addr,
                            port,
                            model,
                            is_cast: true,
                        };

                        {
                            let mut lock = CAST_TARGETS.lock().unwrap();
                            lock.insert(name, target);
                        }

                        if let Some(ref app) = app_handle {
                            let _ = app.emit("cast://targets", list_cast_targets());
                        }
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        let mut lock = CAST_TARGETS.lock().unwrap();
                        lock.retain(|_, v| !fullname.starts_with(&v.name));
                        if let Some(ref app) = app_handle {
                            let _ = app.emit("cast://targets", list_cast_targets());
                        }
                    }
                    _ => {}
                }
            }
        })
        .expect("failed to spawn googlecast discovery thread");
}
