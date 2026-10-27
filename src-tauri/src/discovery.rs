//! mDNS / Bonjour discovery for Apple wireless services.
//!
//! Browses the local network for:
//!   * `_airplay._tcp`        — AirPlay screen / video receivers
//!   * `_raop._tcp`           — AirPlay audio (RAOP) targets we can CAST to
//!   * `_companion-link._tcp` — Continuity / Companion-Link (iPhones, iPads, Macs)
//!   * `_apple-mobdev2._tcp`  — Apple Mobile Device (Wi-Fi sync)
//!
//! When an Apple device connects to Mirr via UxPlay, active connection events
//! and IP/model logs immediately register the connected client into the device
//! list so it appears in "ON YOUR NETWORK" and the Ecosystem drawer.

use mdns_sd::{ServiceDaemon, ServiceEvent};
use once_cell::sync::{Lazy, OnceCell};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Clone, Serialize, Debug, PartialEq)]
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

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveClient {
    pub name: String,
    pub model: String,
    pub ip: String,
    pub device_id: String,
}

static AIRPLAY: Lazy<Mutex<HashMap<String, Device>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static RAOP: Lazy<Mutex<HashMap<String, Target>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static DAEMON: OnceCell<ServiceDaemon> = OnceCell::new();
static ACTIVE_CLIENT: Lazy<Mutex<Option<ActiveClient>>> = Lazy::new(|| Mutex::new(None));
static APP_HANDLE: OnceCell<tauri::AppHandle> = OnceCell::new();

/// The app's single mDNS daemon (also keeps it alive for the process).
/// dacp.rs browses `_dacp._tcp` on it; falls back to its own daemon if None.
pub fn shared_daemon() -> Option<ServiceDaemon> {
    DAEMON.get().cloned()
}

pub fn set_app_handle(app: tauri::AppHandle) {
    let _ = APP_HANDLE.set(app);
}

pub fn emit_devices() {
    if let Some(app) = APP_HANDLE.get() {
        use tauri::Emitter;
        let _ = app.emit("airplay://devices", airplay_devices());
    }
}

#[cfg(windows)]
pub fn active_engine_tcp_client_ip() -> Option<String> {
    use std::net::Ipv4Addr;
    let pid = crate::embed::engine_pid();
    if pid == 0 {
        return None;
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct MIB_TCPROW_OWNER_PID {
        dw_state: u32,
        dw_local_addr: u32,
        dw_local_port: u32,
        dw_remote_addr: u32,
        dw_remote_port: u32,
        dw_owning_pid: u32,
    }

    #[repr(C)]
    struct MIB_TCPTABLE_OWNER_PID {
        dw_num_entries: u32,
        table: [MIB_TCPROW_OWNER_PID; 1],
    }

    #[link(name = "iphlpapi")]
    extern "system" {
        fn GetExtendedTcpTable(
            pTcpTable: *mut std::ffi::c_void,
            pdwSize: *mut u32,
            bOrder: i32,
            ulAf: u32,
            TableClass: u32,
            Reserved: u32,
        ) -> u32;
    }

    const AF_INET: u32 = 2;
    const TCP_TABLE_OWNER_PID_ALL: u32 = 5;
    const MIB_TCP_STATE_ESTAB: u32 = 5;

    let mut size = 0u32;
    unsafe {
        let _ = GetExtendedTcpTable(
            std::ptr::null_mut(),
            &mut size,
            0,
            AF_INET,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        );
    }
    if size == 0 {
        return None;
    }

    let mut buffer: Vec<u8> = vec![0u8; size as usize];
    let ret = unsafe {
        GetExtendedTcpTable(
            buffer.as_mut_ptr() as *mut std::ffi::c_void,
            &mut size,
            0,
            AF_INET,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        )
    };
    if ret != 0 {
        return None;
    }

    let p_table = buffer.as_ptr() as *const MIB_TCPTABLE_OWNER_PID;
    let num_entries = unsafe { (*p_table).dw_num_entries as usize };
    let rows_ptr = unsafe { (*p_table).table.as_ptr() };

    for i in 0..num_entries {
        let row = unsafe { *rows_ptr.add(i) };
        if row.dw_owning_pid == pid && row.dw_state == MIB_TCP_STATE_ESTAB {
            let remote_ip = Ipv4Addr::from(row.dw_remote_addr.to_ne_bytes());
            if !remote_ip.is_loopback() && !remote_ip.is_unspecified() {
                return Some(remote_ip.to_string());
            }
        }
    }
    None
}

#[cfg(not(windows))]
pub fn active_engine_tcp_client_ip() -> Option<String> {
    None
}

/// Sorted by name, deliberately: these come out of a HashMap, whose iteration
/// order is arbitrary and can differ between two calls with identical contents.
/// The UI polls this every 5 s and skips re-rendering when the JSON is
/// unchanged, so an unstable order would rebuild the whole card grid on every
/// poll (and visibly shuffle the cards).
pub fn airplay_devices() -> Vec<Device> {
    let mut map = AIRPLAY.lock().unwrap().clone();

    // 1. Check if UxPlay has an active connected AirPlay client or if mirror/audio is active
    let is_mirroring = {
        #[cfg(windows)]
        {
            crate::embed::mirror_active()
        }
        #[cfg(not(windows))]
        {
            false
        }
    };
    let is_audio = crate::nowplaying::is_active();
    let tcp_client_ip = active_engine_tcp_client_ip();
    let dacp_ip = crate::dacp::client_ip();
    let active_client = ACTIVE_CLIENT.lock().unwrap().clone();
    let is_active_session = active_client.is_some() || is_mirroring || is_audio;

    if is_active_session {
        let best_ip = active_client
            .as_ref()
            .map(|c| c.ip.as_str())
            .filter(|s| !s.is_empty())
            .or(tcp_client_ip.as_deref())
            .or(dacp_ip.as_deref())
            .unwrap_or("")
            .to_string();

        let client_name = active_client
            .as_ref()
            .map(|c| c.name.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| {
                if !best_ip.is_empty() {
                    crate::localsend::peers()
                        .into_iter()
                        .find(|p| p.ip == best_ip)
                        .map(|p| p.alias)
                        .or_else(|| {
                            map.values()
                                .find(|d| d.addr == best_ip && !d.name.starts_with("Apple Device"))
                                .map(|d| d.name.clone())
                        })
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                if is_mirroring {
                    "Connected iPhone".to_string()
                } else {
                    "iPhone (AirPlay)".to_string()
                }
            });

        let raw_model = active_client
            .as_ref()
            .map(|c| c.model.clone())
            .filter(|m| !m.is_empty())
            .or_else(|| {
                if !best_ip.is_empty() {
                    crate::localsend::peers()
                        .into_iter()
                        .find(|p| p.ip == best_ip)
                        .map(|p| p.model)
                } else {
                    None
                }
            })
            .unwrap_or_default();

        let model_label = friendly_model(&raw_model);

        let meta = if is_mirroring {
            if model_label.is_empty() {
                "AirPlay · Mirroring Active".to_string()
            } else {
                format!("{model_label} · AirPlay Mirroring")
            }
        } else if is_audio {
            if model_label.is_empty() {
                "AirPlay · Audio Streaming".to_string()
            } else {
                format!("{model_label} · AirPlay Audio")
            }
        } else if model_label.is_empty() {
            "AirPlay · Connected".to_string()
        } else {
            format!("{model_label} · AirPlay Connected")
        };

        let kind = guess_kind(&client_name, &raw_model);

        if let Some(existing) = map.values_mut().find(|d| {
            (!best_ip.is_empty() && d.addr == best_ip) || d.name == client_name
        }) {
            existing.online = true;
            existing.meta = meta;
            existing.kind = kind;
            if !client_name.is_empty()
                && (existing.name.contains('-') || existing.name.starts_with("Apple Device"))
            {
                existing.name = client_name.clone();
            }
            if !best_ip.is_empty() {
                existing.addr = best_ip.clone();
            }
        } else {
            map.insert(
                client_name.clone(),
                Device {
                    name: client_name,
                    meta,
                    online: true,
                    kind,
                    addr: best_ip,
                },
            );
        }
    }

    // 2. Merge with LocalSend peers
    for peer in crate::localsend::peers() {
        let is_apple = peer.kind == "iphone"
            || peer.kind == "ipad"
            || peer.kind == "mac"
            || peer.alias.to_lowercase().contains("iphone")
            || peer.alias.to_lowercase().contains("ipad")
            || peer.alias.to_lowercase().contains("mac")
            || peer.alias.to_lowercase().contains("apple")
            || peer.model.to_lowercase().contains("iphone")
            || peer.model.to_lowercase().contains("ipad")
            || peer.model.to_lowercase().contains("mac")
            || peer.model.to_lowercase().contains("apple")
            || peer.model.to_lowercase().contains("ios");
        let kind = if is_apple {
            guess_kind(&peer.alias, &peer.model)
        } else {
            peer.kind.clone()
        };

        if let Some(existing) = map
            .values_mut()
            .find(|d| (!peer.ip.is_empty() && d.addr == peer.ip) || d.name == peer.alias)
        {
            if existing.name.contains('-') || existing.name.starts_with("Apple Device") {
                existing.name = peer.alias.clone();
            }
            if existing.meta.is_empty() || existing.meta == "Apple device" {
                existing.meta = if peer.model.is_empty() {
                    "LocalSend · Nearby".into()
                } else {
                    format!("{} · LocalSend", peer.model)
                };
            }
            existing.online = true;
        } else {
            map.insert(
                peer.alias.clone(),
                Device {
                    name: peer.alias.clone(),
                    meta: if peer.model.is_empty() {
                        "LocalSend · Nearby".into()
                    } else {
                        format!("{} · LocalSend", peer.model)
                    },
                    online: true,
                    kind,
                    addr: peer.ip,
                },
            );
        }
    }

    let v: Vec<Device> = map.into_values().collect();
    let mut deduped: Vec<Device> = Vec::new();
    for dev in v {
        if let Some(existing) = deduped.iter_mut().find(|d| {
            (!dev.addr.is_empty() && d.addr == dev.addr)
                || (!dev.name.is_empty() && d.name.eq_ignore_ascii_case(&dev.name))
        }) {
            if dev.online {
                existing.online = true;
            }
            if existing.kind == "generic" && dev.kind != "generic" {
                existing.kind = dev.kind.clone();
            }
            if existing.addr.is_empty() && !dev.addr.is_empty() {
                existing.addr = dev.addr.clone();
            }
            if dev.meta.len() > existing.meta.len() && !dev.meta.contains("Offline") {
                existing.meta = dev.meta.clone();
            }
            if (existing.name.contains('-') && existing.name.len() > 20)
                || existing.name.starts_with("Apple Device")
            {
                if !dev.name.starts_with("Apple Device") && !dev.name.is_empty() {
                    existing.name = dev.name.clone();
                }
            }
        } else {
            deduped.push(dev);
        }
    }
    deduped.sort_by(|a, b| a.name.cmp(&b.name));
    deduped
}

/// Sorted by name for the same reason as `airplay_devices`.
pub fn raop_targets() -> Vec<Target> {
    let mut v: Vec<Target> = RAOP.lock().unwrap().values().cloned().collect();
    v.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.addr.cmp(&b.addr)));
    v
}

fn is_self_broadcast(name: &str) -> bool {
    let cfg_name = crate::engines::get_config().broadcast_name;
    name.eq_ignore_ascii_case("Mirr")
        || name.eq_ignore_ascii_case("Mirr Audio")
        || name.eq_ignore_ascii_case("Mirr (Dolby Atmos)")
        || name.eq_ignore_ascii_case("Mirr Spatial Audio")
        || name.eq_ignore_ascii_case(&cfg_name)
        || name.eq_ignore_ascii_case(&format!("{cfg_name} Audio"))
        || name.eq_ignore_ascii_case(&format!("{cfg_name} (Dolby Atmos)"))
        || name.eq_ignore_ascii_case(&format!("{cfg_name} Spatial Audio"))
}

/// Begin browsing. Spawns background threads that live for the app's lifetime.
pub fn start() -> anyhow::Result<()> {
    let mdns = ServiceDaemon::new()?;
    let _ = DAEMON.set(mdns.clone());

    // 1. AirPlay screen / video receivers
    match mdns.browse("_airplay._tcp.local.") {
        Ok(ap_rx) => {
            std::thread::spawn(move || {
                while let Ok(event) = ap_rx.recv() {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let name = clean_name(info.get_fullname());
                            if is_self_broadcast(&name) {
                                continue;
                            }
                            let addr = pick_addr(&info);
                            let raw_model = info
                                .get_property_val_str("model")
                                .unwrap_or("Apple device");
                            let model = friendly_model(raw_model);
                            let dev = Device {
                                kind: guess_kind(&name, &model),
                                name: name.clone(),
                                meta: model,
                                online: true,
                                addr,
                            };
                            AIRPLAY.lock().unwrap().insert(name.clone(), dev);
                            emit_devices();
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            let raw = clean_name(&fullname);
                            let mut map = AIRPLAY.lock().unwrap();
                            let mut found = false;
                            if let Some(d) = map.get_mut(&raw) {
                                d.online = false;
                                found = true;
                            } else if let Some(d) = map.values_mut().find(|d| d.name == raw || d.addr == raw) {
                                d.online = false;
                                found = true;
                            }
                            drop(map);
                            if found {
                                emit_devices();
                            }
                        }
                        _ => {}
                    }
                }
            });
        }
        Err(e) => eprintln!("[discovery] failed to browse _airplay: {e}"),
    }

    // 2. Apple Continuity / Companion-Link (all iPhones, iPads, Macs advertise this)
    match mdns.browse("_companion-link._tcp.local.") {
        Ok(comp_rx) => {
            std::thread::spawn(move || {
                while let Ok(event) = comp_rx.recv() {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let fullname = info.get_fullname();
                            let raw_name = clean_name(fullname);
                            if is_self_broadcast(&raw_name) {
                                continue;
                            }
                            let host = info
                                .get_hostname()
                                .trim_end_matches('.')
                                .trim_end_matches(".local")
                                .replace('-', " ")
                                .trim()
                                .to_string();
                            let model_prop = info
                                .get_property_val_str("model")
                                .or_else(|| info.get_property_val_str("rpMd"))
                                .unwrap_or("")
                                .to_string();
                            let rphn = info
                                .get_property_val_str("rpHN")
                                .unwrap_or("")
                                .trim()
                                .to_string();
                            let is_uuid = raw_name.contains('-')
                                && raw_name.len() > 20
                                && raw_name.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
                            let name = if !rphn.is_empty() {
                                rphn
                            } else if is_uuid
                                && !host.is_empty()
                                && !host.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
                            {
                                host
                            } else if is_uuid {
                                if !model_prop.is_empty() {
                                    format!("Apple Device ({})", friendly_model(&model_prop))
                                } else {
                                    "Apple Device".to_string()
                                }
                            } else {
                                raw_name.clone()
                            };
                            let addr = pick_addr(&info);
                            let model = if !model_prop.is_empty() {
                                friendly_model(&model_prop)
                            } else {
                                "Apple device".to_string()
                            };
                            let kind = guess_kind(&name, &model);
                            let dev = Device {
                                kind,
                                name: name.clone(),
                                meta: model,
                                online: true,
                                addr,
                            };
                            AIRPLAY.lock().unwrap().insert(raw_name.clone(), dev);
                            emit_devices();
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            let raw = clean_name(&fullname);
                            let mut map = AIRPLAY.lock().unwrap();
                            let mut found = false;
                            if let Some(d) = map.get_mut(&raw) {
                                d.online = false;
                                found = true;
                            } else if let Some(d) = map.values_mut().find(|d| d.name == raw || d.addr == raw) {
                                d.online = false;
                                found = true;
                            }
                            drop(map);
                            if found {
                                emit_devices();
                            }
                        }
                        _ => {}
                    }
                }
            });
        }
        Err(e) => eprintln!("[discovery] failed to browse _companion-link: {e}"),
    }

    // 3. Apple Mobile Device (Wi-Fi sync: advertises user-friendly device name)
    match mdns.browse("_apple-mobdev2._tcp.local.") {
        Ok(mob_rx) => {
            std::thread::spawn(move || {
                while let Ok(event) = mob_rx.recv() {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let name = clean_name(info.get_fullname());
                            if is_self_broadcast(&name) {
                                continue;
                            }
                            let addr = pick_addr(&info);
                            let dev = Device {
                                kind: "iphone".into(),
                                name: name.clone(),
                                meta: "iPhone · Wi-Fi".into(),
                                online: true,
                                addr,
                            };
                            AIRPLAY.lock().unwrap().insert(name.clone(), dev);
                            emit_devices();
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            let raw = clean_name(&fullname);
                            let mut map = AIRPLAY.lock().unwrap();
                            let mut found = false;
                            if let Some(d) = map.get_mut(&raw) {
                                d.online = false;
                                found = true;
                            } else if let Some(d) = map.values_mut().find(|d| d.name == raw || d.addr == raw) {
                                d.online = false;
                                found = true;
                            }
                            drop(map);
                            if found {
                                emit_devices();
                            }
                        }
                        _ => {}
                    }
                }
            });
        }
        Err(e) => eprintln!("[discovery] failed to browse _apple-mobdev2: {e}"),
    }

    // 4. AirPlay audio (RAOP) targets we can cast to
    match mdns.browse("_raop._tcp.local.") {
        Ok(raop_rx) => {
            std::thread::spawn(move || {
                while let Ok(event) = raop_rx.recv() {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let raw = clean_name(info.get_fullname());
                            let addr = pick_addr(&info);
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
                            RAOP.lock().unwrap().remove(&clean_name(&fullname));
                        }
                        _ => {}
                    }
                }
            });
        }
        Err(e) => eprintln!("[discovery] failed to browse _raop: {e}"),
    }

    Ok(())
}

/// Called when an AirPlay connection or request is received from an Apple device.
/// Stores active client details and updates the device in the Ecosystem / On Your Network list.
pub fn note_client_connected(
    ip: Option<&str>,
    name: Option<&str>,
    model: Option<&str>,
    device_id: Option<&str>,
) {
    let mut active = ACTIVE_CLIENT.lock().unwrap();
    let mut cur = active.clone().unwrap_or(ActiveClient {
        name: String::new(),
        model: String::new(),
        ip: String::new(),
        device_id: String::new(),
    });

    if let Some(i) = ip {
        let i = i.trim();
        if !i.is_empty() {
            cur.ip = i.to_string();
        }
    }
    if let Some(n) = name {
        let n = n.trim();
        if !n.is_empty() {
            cur.name = n.to_string();
        }
    }
    if let Some(m) = model {
        let m = m.trim();
        if !m.is_empty() {
            cur.model = m.to_string();
        }
    }
    if let Some(d) = device_id {
        let d = d.trim();
        if !d.is_empty() {
            cur.device_id = d.to_string();
        }
    }

    // If IP is still empty, resolve from active TCP table or DACP or LocalSend peers
    if cur.ip.is_empty() {
        #[cfg(windows)]
        if let Some(tcp_ip) = active_engine_tcp_client_ip() {
            cur.ip = tcp_ip;
        }
        if cur.ip.is_empty() {
            if let Some(dacp_ip) = crate::dacp::client_ip() {
                cur.ip = dacp_ip;
            }
        }
        if cur.ip.is_empty() && !cur.name.is_empty() {
            if let Some(peer) = crate::localsend::peers()
                .into_iter()
                .find(|p| p.alias.eq_ignore_ascii_case(&cur.name))
            {
                cur.ip = peer.ip;
            }
        }
    }

    // If name is still empty, resolve from LocalSend peers by IP
    if cur.name.is_empty() && !cur.ip.is_empty() {
        if let Some(peer) = crate::localsend::peers().into_iter().find(|p| p.ip == cur.ip) {
            cur.name = peer.alias;
            if cur.model.is_empty() && !peer.model.is_empty() {
                cur.model = peer.model;
            }
        }
    }

    *active = Some(cur.clone());
    drop(active);

    let client_name = if !cur.name.is_empty() {
        cur.name.clone()
    } else {
        crate::localsend::peers()
            .into_iter()
            .find(|p| !cur.ip.is_empty() && p.ip == cur.ip)
            .map(|p| p.alias)
            .unwrap_or_else(|| "iPhone (AirPlay)".to_string())
    };

    let model_label = friendly_model(&cur.model);
    let meta = if model_label.is_empty() {
        "AirPlay · Connected".to_string()
    } else {
        format!("{model_label} · AirPlay Connected")
    };
    let kind = guess_kind(&client_name, &cur.model);

    let mut map = AIRPLAY.lock().unwrap();
    if let Some(existing) = map
        .values_mut()
        .find(|d| (!cur.ip.is_empty() && d.addr == cur.ip) || d.name == client_name)
    {
        existing.online = true;
        existing.meta = meta;
        existing.kind = kind;
        if !cur.name.is_empty() {
            existing.name = cur.name;
        }
        if !cur.ip.is_empty() {
            existing.addr = cur.ip;
        }
    } else {
        map.insert(
            client_name.clone(),
            Device {
                name: client_name,
                meta,
                online: true,
                kind,
                addr: cur.ip,
            },
        );
    }
    drop(map);
    emit_devices();
}

pub fn note_connected_client(ip: &str) {
    if !ip.trim().is_empty() {
        note_client_connected(Some(ip), None, None, None);
    }
}

pub fn note_client_disconnected() {
    let mut active = ACTIVE_CLIENT.lock().unwrap();
    if let Some(cur) = active.take() {
        let mut map = AIRPLAY.lock().unwrap();
        for dev in map.values_mut() {
            if (!cur.ip.is_empty() && dev.addr == cur.ip)
                || (!cur.name.is_empty() && dev.name == cur.name)
            {
                dev.online = false;
                let model_label = friendly_model(&cur.model);
                dev.meta = if model_label.is_empty() {
                    "AirPlay (Offline)".to_string()
                } else {
                    format!("{model_label} · AirPlay (Offline)")
                };
            }
        }
        drop(map);
        drop(active);
        emit_devices();
    }
}

pub fn note_disconnected_client(ip: &str) {
    if ip.trim().is_empty() {
        return;
    }
    let mut active = ACTIVE_CLIENT.lock().unwrap();
    if let Some(cur) = active.as_ref() {
        if cur.ip == ip {
            *active = None;
        }
    }
    let mut map = AIRPLAY.lock().unwrap();
    for dev in map.values_mut() {
        if dev.addr == ip {
            dev.online = false;
            dev.meta = "AirPlay (Offline)".to_string();
        }
    }
    drop(map);
    drop(active);
    emit_devices();
}

/// Parse IPv4/IPv6 address from UxPlay log lines (e.g. "Local: ... Remote: <IP>" or "from <IP>").
pub fn parse_ip_from_line(line: &str) -> Option<String> {
    let l = line.to_lowercase();
    if let Some(idx) = l.find("remote:") {
        let tail = line[idx + "remote:".len()..].trim_start();
        return extract_ip_token(tail);
    }
    if let Some(idx) = l.find("remote :") {
        let tail = line[idx + "remote :".len()..].trim_start();
        return extract_ip_token(tail);
    }
    if let Some(idx) = l.rfind(" from ") {
        let tail = line[idx + " from ".len()..].trim_start();
        if let Some(ip) = extract_ip_token(tail) {
            return Some(ip);
        }
    }
    None
}

fn extract_ip_token(s: &str) -> Option<String> {
    let raw = s.split_whitespace().next()?.trim_matches(|c: char| {
        c == '.' || c == ',' || c == ';' || c == '"' || c == '\'' || c == ')'
    });
    let candidate = if raw.starts_with('[') {
        raw.trim_matches('[').split(']').next()?.trim()
    } else if raw.contains(':') && raw.matches(':').count() == 1 {
        raw.split(':').next()?.trim()
    } else {
        raw
    };
    if candidate.is_empty() {
        return None;
    }
    if candidate.parse::<std::net::IpAddr>().is_ok() {
        Some(candidate.to_string())
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConnectionRequestInfo {
    pub name: String,
    pub model: Option<String>,
    pub device_id: Option<String>,
}

pub fn parse_connection_request(line: &str) -> Option<ConnectionRequestInfo> {
    let l = line.to_lowercase();
    let prefix = "connection request from ";
    let idx = l.find(prefix)?;
    let rest = line[idx + prefix.len()..].trim();

    let (name_model_part, device_id) = if let Some(d_idx) = rest.to_lowercase().find(" with deviceid")
    {
        let nm = rest[..d_idx].trim();
        let after_d = rest[d_idx + " with deviceid".len()..]
            .trim_start_matches(|c: char| c == ' ' || c == '=');
        let dev_id = after_d.split_whitespace().next().map(|s| s.trim().to_string());
        (nm, dev_id)
    } else {
        (rest, None)
    };

    let (name, model) = if let Some(open) = name_model_part.rfind('(') {
        if let Some(close) = name_model_part[open..].rfind(')') {
            let m = name_model_part[open + 1..open + close].trim();
            let n = name_model_part[..open].trim();
            if !n.is_empty() && !m.is_empty() {
                (n.to_string(), Some(m.to_string()))
            } else {
                (name_model_part.to_string(), None)
            }
        } else {
            (name_model_part.to_string(), None)
        }
    } else {
        (name_model_part.to_string(), None)
    };

    Some(ConnectionRequestInfo {
        name,
        model,
        device_id,
    })
}

pub fn on_log_line(app: &tauri::AppHandle, line: &str) {
    let l = line.to_lowercase();
    let mut updated = false;

    if let Some(ip) = parse_ip_from_line(line) {
        note_client_connected(Some(&ip), None, None, None);
        updated = true;
    }

    if l.contains("connection request from ") {
        if let Some(req) = parse_connection_request(line) {
            note_client_connected(
                None,
                Some(&req.name),
                req.model.as_deref(),
                req.device_id.as_deref(),
            );
            updated = true;
        }
    }

    if l.contains("lost connection with client")
        || l.contains("client http request post stop")
        || l.contains("stopped mirroring")
        || l.contains("connection reset")
        || l.contains("client silence")
    {
        note_client_disconnected();
        updated = true;
    }

    if updated {
        use tauri::Emitter;
        let _ = app.emit("airplay://devices", airplay_devices());
    }
}

pub fn friendly_model(model: &str) -> String {
    let m = model.trim();
    if m.is_empty() {
        return String::new();
    }
    match m {
        // iPhone 16
        "iPhone17,1" => "iPhone 16 Pro",
        "iPhone17,2" => "iPhone 16 Pro Max",
        "iPhone17,3" => "iPhone 16",
        "iPhone17,4" => "iPhone 16 Plus",
        // iPhone 15
        "iPhone15,4" => "iPhone 15",
        "iPhone15,5" => "iPhone 15 Plus",
        "iPhone16,1" => "iPhone 15 Pro",
        "iPhone16,2" => "iPhone 15 Pro Max",
        // iPhone 14
        "iPhone14,7" => "iPhone 14",
        "iPhone14,8" => "iPhone 14 Plus",
        "iPhone15,2" => "iPhone 14 Pro",
        "iPhone15,3" => "iPhone 14 Pro Max",
        // iPhone 13
        "iPhone14,4" => "iPhone 13 mini",
        "iPhone14,5" => "iPhone 13",
        "iPhone14,2" => "iPhone 13 Pro",
        "iPhone14,3" => "iPhone 13 Pro Max",
        // iPhone 12
        "iPhone13,1" => "iPhone 12 mini",
        "iPhone13,2" => "iPhone 12",
        "iPhone13,3" => "iPhone 12 Pro",
        "iPhone13,4" => "iPhone 12 Pro Max",
        // iPhone 11
        "iPhone12,1" => "iPhone 11",
        "iPhone12,3" => "iPhone 11 Pro",
        "iPhone12,5" => "iPhone 11 Pro Max",
        // iPhone SE
        "iPhone12,8" => "iPhone SE (2nd gen)",
        "iPhone14,6" => "iPhone SE (3rd gen)",
        // iPad
        "iPad13,1" | "iPad13,2" => "iPad Air (4th gen)",
        "iPad13,4" | "iPad13,5" | "iPad13,6" | "iPad13,7" => "iPad Pro 11-inch (3rd gen)",
        "iPad13,8" | "iPad13,9" | "iPad13,10" | "iPad13,11" => "iPad Pro 12.9-inch (5th gen)",
        "iPad14,1" | "iPad14,2" => "iPad mini (6th gen)",
        "iPad14,3" | "iPad14,4" => "iPad Pro 11-inch (4th gen)",
        "iPad14,5" | "iPad14,6" => "iPad Pro 12.9-inch (6th gen)",
        // HomePod & Apple TV
        "AudioAccessory1,1" | "AudioAccessory1,2" => "HomePod",
        "AudioAccessory5,1" => "HomePod mini",
        "AudioAccessory6,1" => "HomePod (2nd gen) · Spatial Audio",
        "AppleTV11,1" | "AppleTV14,1" => "Apple TV 4K · Dolby Atmos",
        "AppleTV6,2" | "AppleTV5,3" => "Apple TV HD / 4K",
        _ => {
            if m.starts_with("iPhone") {
                "iPhone"
            } else if m.starts_with("iPad") {
                "iPad"
            } else if m.starts_with("Mac") {
                "Mac"
            } else if m.starts_with("AudioAccessory") {
                "HomePod"
            } else if m.starts_with("AppleTV") {
                "Apple TV"
            } else {
                m
            }
        }
    }
    .to_string()
}

/// Prefer a deterministic IPv4 address from mdns-sd's unordered address set
/// (dual-stack Apple devices advertise both fe80:: link-local v6 and v4).
fn pick_addr(info: &mdns_sd::ServiceInfo) -> String {
    let mut v4: Vec<String> = info
        .get_addresses()
        .iter()
        .map(|a| a.to_string())
        .filter(|s| !s.contains(':'))
        .collect();
    v4.sort();
    if let Some(first) = v4.into_iter().next() {
        return first;
    }
    let mut all: Vec<String> = info.get_addresses().iter().map(|a| a.to_string()).collect();
    all.sort();
    all.into_iter().next().unwrap_or_default()
}

/// "Living Room._airplay._tcp.local." -> "Living Room"
fn clean_name(fullname: &str) -> String {
    let raw = fullname.split("._").next().unwrap_or(fullname);
    raw.replace("\\032", " ")
        .replace("\\ ", " ")
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
    } else if s.contains("homepod") || s.contains("audio") || s.contains("audioaccessory") {
        "speaker"
    } else {
        "generic"
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_friendly_model_newer_devices() {
        assert_eq!(friendly_model("iPhone17,1"), "iPhone 16 Pro");
        assert_eq!(friendly_model("iPhone17,2"), "iPhone 16 Pro Max");
        assert_eq!(friendly_model("iPhone17,3"), "iPhone 16");
        assert_eq!(friendly_model("iPhone16,1"), "iPhone 15 Pro");
        assert_eq!(friendly_model("iPhone15,4"), "iPhone 15");
        assert_eq!(friendly_model("iPad14,1"), "iPad mini (6th gen)");
    }

    #[test]
    fn test_parse_ip_remote_line() {
        assert_eq!(
            parse_ip_from_line("Local: 192.168.1.130. Remote: 192.168.1.136"),
            Some("192.168.1.136".to_string())
        );
        assert_eq!(
            parse_ip_from_line("Local : 192.168.1.130 Remote: 192.168.1.136:54321"),
            Some("192.168.1.136".to_string())
        );
        assert_eq!(
            parse_ip_from_line("Remote: [fe80::1234]:54321"),
            Some("fe80::1234".to_string())
        );
    }

    #[test]
    fn test_parse_ip_from_accepted() {
        assert_eq!(
            parse_ip_from_line("Accepted IPv4 client on socket 395 from 10.0.0.42"),
            Some("10.0.0.42".to_string())
        );
        assert_eq!(
            parse_ip_from_line("connection request from Nicholas's iPhone (iPhone15,4)"),
            None
        );
    }

    #[test]
    fn test_parse_connection_request() {
        let req = parse_connection_request(
            "connection request from Nicholas's iPhone (iPhone15,4) with deviceID = DA:0E:37:66:8B:4E",
        )
        .expect("should parse");
        assert_eq!(req.name, "Nicholas's iPhone");
        assert_eq!(req.model.as_deref(), Some("iPhone15,4"));
        assert_eq!(req.device_id.as_deref(), Some("DA:0E:37:66:8B:4E"));

        let req2 = parse_connection_request(
            "\"nohold\" feature: switch to new connection request from Sarah's iPad",
        )
        .expect("should parse");
        assert_eq!(req2.name, "Sarah's iPad");
        assert_eq!(req2.model, None);
    }

    #[test]
    fn test_friendly_model() {
        assert_eq!(friendly_model("iPhone15,4"), "iPhone 15");
        assert_eq!(friendly_model("iPhone14,2"), "iPhone 13 Pro");
        assert_eq!(friendly_model("iPad13,4"), "iPad Pro 11-inch (3rd gen)");
        assert_eq!(friendly_model("AudioAccessory6,1"), "HomePod (2nd gen) · Spatial Audio");
        assert_eq!(friendly_model("AppleTV11,1"), "Apple TV 4K · Dolby Atmos");
        assert_eq!(friendly_model("CustomDevice"), "CustomDevice");
    }

    #[test]
    fn test_is_self_broadcast() {
        assert!(is_self_broadcast("Mirr"));
        assert!(is_self_broadcast("Mirr (Dolby Atmos)"));
        assert!(is_self_broadcast("Mirr Spatial Audio"));
    }

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_note_client_connected() {
        let _guard = TEST_LOCK.lock().unwrap();
        AIRPLAY.lock().unwrap().clear();
        *ACTIVE_CLIENT.lock().unwrap() = None;

        note_client_connected(
            Some("192.168.1.50"),
            Some("Alex's iPhone"),
            Some("iPhone14,5"),
            Some("AA:BB:CC:DD:EE:FF"),
        );
        let devs = airplay_devices();
        let found = devs.iter().find(|d| d.name == "Alex's iPhone");
        assert!(found.is_some(), "Device should be present in airplay_devices");
        let d = found.unwrap();
        assert_eq!(d.addr, "192.168.1.50");
        assert!(d.online);
        assert_eq!(d.kind, "iphone");
        assert_eq!(d.meta, "iPhone 13 · AirPlay Connected");

        note_client_disconnected();
        let devs_after = airplay_devices();
        let found_after = devs_after.iter().find(|d| d.name == "Alex's iPhone");
        assert!(found_after.is_some());
        assert!(!found_after.unwrap().online);
        assert_eq!(found_after.unwrap().meta, "iPhone 13 · AirPlay (Offline)");
    }

    #[test]
    fn test_note_disconnected_client_by_ip() {
        let _guard = TEST_LOCK.lock().unwrap();
        AIRPLAY.lock().unwrap().clear();
        *ACTIVE_CLIENT.lock().unwrap() = None;

        note_client_connected(
            Some("192.168.1.99"),
            Some("Taylor's iPad"),
            Some("iPad13,4"),
            None,
        );
        let devs = airplay_devices();
        let found = devs.iter().find(|d| d.addr == "192.168.1.99");
        assert!(found.is_some());
        assert!(found.unwrap().online);

        note_disconnected_client("192.168.1.99");
        let devs_after = airplay_devices();
        let found_after = devs_after.iter().find(|d| d.addr == "192.168.1.99");
        assert!(found_after.is_some());
        assert!(!found_after.unwrap().online);
    }

    #[test]
    fn test_parse_ip_edge_cases() {
        assert_eq!(parse_ip_from_line(""), None);
        assert_eq!(parse_ip_from_line("random log line with no ip"), None);
        assert_eq!(parse_ip_from_line("Remote: not_an_ip"), None);
        assert_eq!(
            parse_ip_from_line("Remote: 10.1.2.3."),
            Some("10.1.2.3".to_string())
        );
        assert_eq!(
            parse_ip_from_line("Remote: 192.168.0.1; port 80"),
            Some("192.168.0.1".to_string())
        );
    }

    #[test]
    fn test_parse_connection_request_edge_cases() {
        assert_eq!(parse_connection_request(""), None);
        assert_eq!(parse_connection_request("some other line"), None);

        // Multiple parentheses in name
        let req = parse_connection_request(
            "connection request from Bob's iPhone (Work) (iPhone16,1) with deviceID = 00:11:22:33:44:55",
        )
        .expect("should parse");
        assert_eq!(req.name, "Bob's iPhone (Work)");
        assert_eq!(req.model.as_deref(), Some("iPhone16,1"));
        assert_eq!(req.device_id.as_deref(), Some("00:11:22:33:44:55"));

        // No model, just deviceID
        let req2 = parse_connection_request(
            "connection request from My Mac with deviceID = AA:BB:CC:DD:EE:FF",
        )
        .expect("should parse");
        assert_eq!(req2.name, "My Mac");
        assert_eq!(req2.model, None);
        assert_eq!(req2.device_id.as_deref(), Some("AA:BB:CC:DD:EE:FF"));
    }

    #[test]
    fn test_clean_name() {
        assert_eq!(clean_name("Living Room._airplay._tcp.local."), "Living Room");
        assert_eq!(clean_name("John\\032Doe's\\ iPhone._companion-link._tcp.local."), "John Doe's iPhone");
        assert_eq!(clean_name("My\\ Device._raop._tcp.local."), "My Device");
    }

    #[test]
    fn test_device_deduplication() {
        let _guard = TEST_LOCK.lock().unwrap();
        AIRPLAY.lock().unwrap().clear();
        *ACTIVE_CLIENT.lock().unwrap() = None;
        // Insert two entries for the same device under different keys (e.g. mDNS _airplay and _companion-link)
        {
            let mut map = AIRPLAY.lock().unwrap();
            map.insert("entry-uuid-1".to_string(), Device {
                name: "Living Room Apple TV".to_string(),
                meta: "Apple device".to_string(),
                online: true,
                kind: "generic".to_string(),
                addr: "192.168.1.188".to_string(),
            });
            map.insert("entry-airplay-1".to_string(), Device {
                name: "Living Room Apple TV".to_string(),
                meta: "AppleTV11,1 · AirPlay Connected".to_string(),
                online: true,
                kind: "appletv".to_string(),
                addr: "192.168.1.188".to_string(),
            });
        }
        let list = airplay_devices();
        let matches: Vec<_> = list.iter().filter(|d| d.addr == "192.168.1.188").collect();
        assert_eq!(matches.len(), 1, "Duplicate device records with same IP/name must be consolidated");
        assert_eq!(matches[0].name, "Living Room Apple TV");
        assert_eq!(matches[0].kind, "appletv");
    }
}
