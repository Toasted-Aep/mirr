//! DIAL (Discovery And Launch) protocol implementation.
//! Enables phones to discover Mirr (via SSDP on UDP 1900) and cast YouTube and web media.

use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use once_cell::sync::Lazy;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

use std::collections::HashSet;

static RUNNING: AtomicBool = AtomicBool::new(false);
static DIAL_UUID: Lazy<String> = Lazy::new(|| uuid::Uuid::new_v4().to_string());
static RUNNING_APPS: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

pub fn dial_uuid() -> &'static str {
    &*DIAL_UUID
}

/// Discovers best physical LAN IP, prioritizing 192.168.* and 10.* over WSL/Hyper-V virtual interfaces.
pub fn best_lan_ip() -> String {
    if let Ok(interfaces) = local_ip_address::list_afinet_netifas() {
        // Priority 1: 192.168.x.x
        for (name, ip) in &interfaces {
            if let std::net::IpAddr::V4(ipv4) = ip {
                if !ipv4.is_loopback() && ipv4.octets()[0] == 192 && ipv4.octets()[1] == 168 {
                    let n = name.to_lowercase();
                    if !n.contains("wsl") && !n.contains("virtual") && !n.contains("hyper-v") {
                        return ipv4.to_string();
                    }
                }
            }
        }
        // Priority 2: 10.x.x.x
        for (name, ip) in &interfaces {
            if let std::net::IpAddr::V4(ipv4) = ip {
                if !ipv4.is_loopback() && ipv4.octets()[0] == 10 {
                    let n = name.to_lowercase();
                    if !n.contains("wsl") && !n.contains("virtual") && !n.contains("hyper-v") {
                        return ipv4.to_string();
                    }
                }
            }
        }
        // Priority 3: any non-loopback, non-virtual IPv4
        for (name, ip) in &interfaces {
            if let std::net::IpAddr::V4(ipv4) = ip {
                if !ipv4.is_loopback() {
                    let n = name.to_lowercase();
                    if !n.contains("wsl") && !n.contains("virtual") && !n.contains("hyper-v") {
                        return ipv4.to_string();
                    }
                }
            }
        }
    }
    local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

pub fn device_description_xml() -> String {
    let cfg = crate::engines::get_config();
    let name = cfg.broadcast_name;
    let uuid = dial_uuid();
    let lan_ip = best_lan_ip();
    let port = crate::aircast::http_stream::http_port();

    format!(
        r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion>
    <major>1</major>
    <minor>0</minor>
  </specVersion>
  <device>
    <deviceType>urn:schemas-upnp-org:device:tvdevice:1</deviceType>
    <friendlyName>{name}</friendlyName>
    <manufacturer>Mirr Bridge</manufacturer>
    <modelName>Mirr Receiver</modelName>
    <modelNumber>1.0</modelNumber>
    <UDN>uuid:{uuid}</UDN>
    <serviceList>
      <service>
        <serviceType>urn:dial-multiscreen-org:service:dial:1</serviceType>
        <serviceId>urn:dial-multiscreen-org:serviceId:dial</serviceId>
        <controlURL>/apps</controlURL>
        <eventSubURL></eventSubURL>
        <SCPDURL></SCPDURL>
      </service>
    </serviceList>
  </device>
  <URLBase>http://{lan_ip}:{port}/</URLBase>
</root>"#
    )
}

pub fn app_status_xml(app_name: &str) -> String {
    let is_running = RUNNING_APPS.lock().unwrap().contains(app_name);
    let state = if is_running { "running" } else { "stopped" };
    let link = if is_running {
        r#"<link rel="run" href="run"/>"#
    } else {
        ""
    };

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<service xmlns="urn:dial-multiscreen-org:schemas:dial">
  <name>{app_name}</name>
  <options allowStop="true"/>
  <state>{state}</state>
  {link}
</service>"#
    )
}

pub fn handle_launch(app: &Option<AppHandle>, app_name: &str, body: &str) -> axum::response::Response {
    eprintln!("[dial] App launch request: {app_name} with body: {body}");
    RUNNING_APPS.lock().unwrap().insert(app_name.to_string());

    let mut video_id = None;
    let mut pairing_code = None;

    // Body can be urlencoded (e.g. "pairingCode=...&v=dQw4w9WgXcQ")
    for part in body.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            match k {
                "v" | "videoId" | "video_id" => video_id = Some(v.to_string()),
                "pairingCode" | "pairing_code" => pairing_code = Some(v.to_string()),
                _ => {}
            }
        }
    }

    if let Some(ref handle) = app {
        let _ = handle.emit(
            "dial://launch",
            serde_json::json!({
                "app": app_name,
                "raw": body,
                "videoId": video_id,
                "pairingCode": pairing_code,
            }),
        );
    }

    let lan_ip = best_lan_ip();
    let port = crate::aircast::http_stream::http_port();
    let location = format!("http://{lan_ip}:{port}/apps/{app_name}/run");

    (
        StatusCode::CREATED,
        [
            (header::LOCATION, location),
            (header::CONTENT_TYPE, "text/plain".to_string()),
        ],
        "OK",
    )
        .into_response()
}

pub fn handle_stop(app: &Option<AppHandle>, app_name: &str) {
    eprintln!("[dial] App stop request: {app_name}");
    RUNNING_APPS.lock().unwrap().remove(app_name);

    if let Some(ref handle) = app {
        let _ = handle.emit(
            "dial://stop",
            serde_json::json!({
                "app": app_name,
            }),
        );
    }
}

pub fn format_ssdp_response(lan_ip: &str, port: u16, st: &str, usn: &str, xml_path: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\n\
CACHE-CONTROL: max-age=1800\r\n\
EXT:\r\n\
LOCATION: http://{lan_ip}:{port}{xml_path}\r\n\
SERVER: Windows/11 UPnP/1.1 Mirr/1.0\r\n\
ST: {st}\r\n\
USN: {usn}\r\n\
BOOTID.UPNP.ORG: 1\r\n\
CONFIGID.UPNP.ORG: 1\r\n\
\r\n"
    )
}

/// Start SSDP multicast listener for DIAL and DLNA on UDP 239.255.255.250:1900.
pub fn start_ssdp_listener() {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }

    std::thread::Builder::new()
        .name("ssdp-dial".to_string())
        .spawn(move || {
            let socket = match socket2::Socket::new(
                socket2::Domain::IPV4,
                socket2::Type::DGRAM,
                Some(socket2::Protocol::UDP),
            ) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[ssdp-dial] Failed to create socket: {e}");
                    return;
                }
            };

            let _ = socket.set_reuse_address(true);

            let bind_addr: SocketAddr = "0.0.0.0:1900".parse().unwrap();
            if let Err(e) = socket.bind(&bind_addr.into()) {
                eprintln!("[ssdp-dial] Failed to bind 0.0.0.0:1900: {e}");
                return;
            }

            let multi_addr: std::net::Ipv4Addr = "239.255.255.250".parse().unwrap();
            let any_addr: std::net::Ipv4Addr = "0.0.0.0".parse().unwrap();
            if let Err(e) = socket.join_multicast_v4(&multi_addr, &any_addr) {
                eprintln!("[ssdp-dial] Failed to join multicast group 239.255.255.250: {e}");
            }

            let std_socket: std::net::UdpSocket = socket.into();
            let mut buf = [0u8; 2048];

            while RUNNING.load(Ordering::SeqCst) {
                match std_socket.recv_from(&mut buf) {
                    Ok((len, src)) => {
                        let msg = String::from_utf8_lossy(&buf[..len]);
                        if msg.starts_with("M-SEARCH") {
                            let is_dial = msg.contains("urn:dial-multiscreen-org:service:dial:1");
                            let is_dlna_dev = msg.contains("urn:schemas-upnp-org:device:MediaRenderer:1");
                            let is_dlna_avt = msg.contains("urn:schemas-upnp-org:service:AVTransport:1");
                            let is_dlna_rc = msg.contains("urn:schemas-upnp-org:service:RenderingControl:1");
                            let is_root = msg.contains("upnp:rootdevice");
                            let is_all = msg.contains("ssdp:all");

                            let lan_ip = best_lan_ip();
                            let port = crate::aircast::http_stream::http_port();
                            let d_uuid = dial_uuid();
                            let dlna_uuid = crate::aircast::dlna::dlna_uuid();

                            if is_dial || is_all {
                                let st = "urn:dial-multiscreen-org:service:dial:1";
                                let usn = format!("uuid:{d_uuid}::{st}");
                                let resp = format_ssdp_response(&lan_ip, port, st, &usn, "/dd.xml");
                                let _ = std_socket.send_to(resp.as_bytes(), src);
                            }

                            if is_dlna_dev || is_all {
                                let st = "urn:schemas-upnp-org:device:MediaRenderer:1";
                                let usn = format!("uuid:{dlna_uuid}::{st}");
                                let resp = format_ssdp_response(&lan_ip, port, st, &usn, "/upnp/dev.xml");
                                let _ = std_socket.send_to(resp.as_bytes(), src);
                            }

                            if is_dlna_avt || is_all {
                                let st = "urn:schemas-upnp-org:service:AVTransport:1";
                                let usn = format!("uuid:{dlna_uuid}::{st}");
                                let resp = format_ssdp_response(&lan_ip, port, st, &usn, "/upnp/dev.xml");
                                let _ = std_socket.send_to(resp.as_bytes(), src);
                            }

                            if is_dlna_rc || is_all {
                                let st = "urn:schemas-upnp-org:service:RenderingControl:1";
                                let usn = format!("uuid:{dlna_uuid}::{st}");
                                let resp = format_ssdp_response(&lan_ip, port, st, &usn, "/upnp/dev.xml");
                                let _ = std_socket.send_to(resp.as_bytes(), src);
                            }

                            if is_root || is_all {
                                let st = "upnp:rootdevice";
                                let usn = format!("uuid:{dlna_uuid}::{st}");
                                let resp = format_ssdp_response(&lan_ip, port, st, &usn, "/upnp/dev.xml");
                                let _ = std_socket.send_to(resp.as_bytes(), src);
                            }
                        }
                    }
                    Err(_) => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            }
        })
        .expect("failed to spawn dial ssdp thread");
}
