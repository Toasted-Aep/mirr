//! DACP remote control.
//!
//! Watches UxPlay's -dacp export (`engines::dacp_path()`, CRLF line endings:
//! line 1 = DACP-ID hex, line 2 = Active-Remote token), resolves the sender's
//! `_dacp._tcp.local.` endpoint (instance `iTunes_Ctrl_<id>`, leading-zero-
//! stripped match, always pinned to the connected client's IP) and sends
//! allowlisted
//! `GET /ctrl-int/1/<cmd>` requests. Emits:
//!   dacp://state { available, discovering }

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent};
use once_cell::sync::{Lazy, OnceCell};
use serde_json::json;
use tauri::{AppHandle, Emitter};

// ----------------------------------------------------------------------------
// State
// ----------------------------------------------------------------------------

/// Parsed contents of the .dacp file for the current connection.
#[derive(Clone, PartialEq)]
struct DacpSession {
    /// Uppercase hex DACP-ID, or None when the client sent "(null)"/no ID.
    dacp_id: Option<String>,
    /// Active-Remote auth token (the entire authentication for the request).
    active_remote: String,
}

/// A resolved mDNS endpoint we can actually send commands to.
#[derive(Clone)]
struct Resolved {
    ip: String,
    port: u16,
    token: String,
}

/// One-shot HTTP outcome, distinguishing a dead endpoint (connect failed) from
/// an HTTP error so `send` can flip the remote unavailable after two refusals.
enum SendErr {
    Connect,
    Other(String),
}

static WATCHER_STARTED: AtomicBool = AtomicBool::new(false);
/// Consecutive connect failures against the resolved endpoint.
static REFUSED: AtomicU32 = AtomicU32::new(0);
static APP: OnceCell<AppHandle> = OnceCell::new();

/// Latest parsed .dacp session (token + id) for the current connection.
static SESSION: Lazy<Mutex<Option<DacpSession>>> = Lazy::new(|| Mutex::new(None));
/// The resolved endpoint for the current session (None while discovering).
static RESOLVED: Lazy<Mutex<Option<Resolved>>> = Lazy::new(|| Mutex::new(None));
/// Advertised `_dacp._tcp` instances, kept for the whole process lifetime:
/// (instance name `iTunes_Ctrl_<id>`, first IPv4) -> port.
/// Keyed by instance AND ip on purpose: the phone multicasts its instance name
/// to the whole LAN, so any host can advertise a colliding record. Keying by
/// name alone would let the impostor's ServiceResolved overwrite the real entry
/// (last one wins) and collect the Active-Remote token. Colliding advertisers
/// must coexist here so `match_endpoint` can pick the one at the client IP.
static ENDPOINTS: Lazy<Mutex<HashMap<(String, String), u16>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
/// Connected client's IPv4 (from UxPlay's "Accepted IPv4 client ... from <ip>").
static CLIENT_IP: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));
/// Last emitted (available, discovering) — emit only on change / snapshot source.
static LAST_STATE: Lazy<Mutex<(bool, bool)>> = Lazy::new(|| Mutex::new((false, false)));

/// Exact-match command allowlist. `cmd` is interpolated straight into the HTTP
/// request line, so anything outside this set is rejected BEFORE the request is
/// built (CRLF-smuggling guard).
const ALLOWED: &[&str] = &[
    "play",
    "pause",
    "playpause",
    "stop",
    "nextitem",
    "previtem",
    "beginff",
    "beginrew",
    "playresume",
    "mutetoggle",
    "volumeup",
    "volumedown",
    "shuffle_songs",
];

// ----------------------------------------------------------------------------
// Hooks (frozen signatures)
// ----------------------------------------------------------------------------

/// Idempotent: spawn the .dacp file poller (500 ms with a live session, 2 s
/// while the export file is absent) and the long-lived
/// `_dacp._tcp` browse. Called by `engines::start_airplay_receiver`.
pub fn ensure_watcher(app: AppHandle) {
    let _ = APP.set(app.clone());
    if WATCHER_STARTED.swap(true, Ordering::SeqCst) {
        return; // threads already running for this process
    }
    spawn_browse(app.clone());
    spawn_file_poller(app);
}

/// Fed every trimmed original-case line; captures the client IPv4 from
/// UxPlay's "Accepted IPv4 client ... from <ip>" line (discovery fallback).
pub fn on_log_line(app: &AppHandle, line: &str) {
    if let Some(ip) = crate::discovery::parse_ip_from_line(line) {
        let changed = {
            let mut cur = CLIENT_IP.lock().unwrap();
            if cur.as_deref() != Some(&ip) {
                *cur = Some(ip.clone());
                true
            } else {
                false
            }
        };
        crate::discovery::note_connected_client(&ip);
        if changed {
            *RESOLVED.lock().unwrap() = None;
        }
        try_resolve(app);
    }
}

/// Engine stopped — clear session/endpoint and emit unavailable.
pub fn on_engine_stop(app: &AppHandle) {
    *SESSION.lock().unwrap() = None;
    *RESOLVED.lock().unwrap() = None;
    if let Some(old_ip) = CLIENT_IP.lock().unwrap().take() {
        crate::discovery::note_disconnected_client(&old_ip);
    }
    REFUSED.store(0, Ordering::SeqCst);
    emit_state(app, false, false);
}

pub fn client_ip() -> Option<String> {
    CLIENT_IP.lock().unwrap().clone()
}

/// Snapshot: { available: bool, discovering: bool }.
pub fn state() -> serde_json::Value {
    let (available, discovering) = *LAST_STATE.lock().unwrap();
    json!({ "available": available, "discovering": discovering })
}

/// Send one transport command. EXACT-MATCH allowlist (CRLF-smuggling guard):
/// play pause playpause stop nextitem previtem beginff beginrew playresume
/// mutetoggle volumeup volumedown shuffle_songs. Returns the HTTP status.
pub async fn command(cmd: String) -> Result<u16, String> {
    if !ALLOWED.contains(&cmd.as_str()) {
        return Err(format!("command not allowed: {cmd}"));
    }
    send(cmd).await
}

/// Absolute volume 0..100 -> dmcp.device-volume dB (-144.0 mute,
/// else -30.0 + 30.0*pct/100, formatted {:.6}).
pub async fn set_volume(pct: f32) -> Result<u16, String> {
    // NaN/inf slip past `pct <= 0.0` (every NaN compare is false) and would put a
    // literal "NaN"/"inf" on the request line — reject before building anything.
    if !pct.is_finite() {
        return Err("invalid volume".into());
    }
    let db = if pct <= 0.0 {
        -144.0
    } else {
        -30.0 + 30.0 * (pct.clamp(0.0, 100.0) / 100.0)
    };
    // `db` is a formatted float — no external input reaches the request line.
    send(format!("setproperty?dmcp.device-volume={db:.6}")).await
}

// ----------------------------------------------------------------------------
// File poller
// ----------------------------------------------------------------------------

fn spawn_file_poller(app: AppHandle) {
    // Hoisted: dacp_path() re-reads LOCALAPPDATA and rebuilds the same PathBuf
    // on every tick, and the path is constant for the process lifetime.
    let path = crate::engines::dacp_path();
    std::thread::spawn(move || {
        // Cadence from state, NOT a constant. `engines` deletes the export files
        // on every stop and before every spawn, so the file's existence on the
        // last tick is a local engine/session liveness signal that costs nothing
        // extra (we just tried to open it) and needs no cross-module hook. Idle,
        // a constant 500 ms meant a failing CreateFile twice a second forever.
        // Don't flatten this back to one interval.
        loop {
            let live = poll_once(&app, &path);
            std::thread::sleep(Duration::from_millis(if live { 500 } else { 2000 }));
        }
    });
}

/// One tick. Returns true when the file held a usable session — the poller
/// picks its next sleep from that.
fn poll_once(app: &AppHandle, path: &Path) -> bool {
    // read_to_string fails (missing file) => session gone.
    let Some(text) = std::fs::read_to_string(path).ok() else {
        clear_session(app);
        return false;
    };
    // A present-but-unusable file (only one line, or no Active-Remote) has no
    // session to authenticate with, but is NOT the same as absent: it can be a
    // torn read of uxplay's two-line CRLF write caught mid-flight, which means
    // the engine is actively writing to it, i.e. very much alive. Stay on the
    // fast cadence so a torn read during session startup doesn't back off to
    // 2 s and delay the transport buttons.
    let Some(new_session) = parse_dacp(&text) else {
        clear_session(app);
        return true;
    };

    let changed = {
        let mut cur = SESSION.lock().unwrap();
        if cur.as_ref() != Some(&new_session) {
            *cur = Some(new_session);
            true
        } else {
            false
        }
    };
    if changed {
        // A new/different token invalidates any prior endpoint (the ephemeral
        // DACP port changes every session); re-enter the discovering state.
        *RESOLVED.lock().unwrap() = None;
        REFUSED.store(0, Ordering::SeqCst);
        emit_state(app, false, true);
    }
    try_resolve(app);
    true
}

/// Drop the current session/endpoint and mark the remote unavailable.
/// `emit_state` dedupes, so calling this every idle tick is cheap.
fn clear_session(app: &AppHandle) {
    *SESSION.lock().unwrap() = None;
    *RESOLVED.lock().unwrap() = None;
    emit_state(app, false, false);
}

/// Parse the two-line, CRLF-terminated .dacp file. Returns None when there is
/// no Active-Remote token (line 2) to authenticate with.
fn parse_dacp(text: &str) -> Option<DacpSession> {
    let mut lines = text.lines().map(str::trim);
    let line1 = lines.next().unwrap_or("");
    let line2 = lines.next().unwrap_or("");
    if line2.is_empty() {
        return None;
    }
    // "(null)"/non-hex line 1 => no usable ID; fall back to IP matching.
    let dacp_id = if !line1.is_empty()
        && !line1.eq_ignore_ascii_case("(null)")
        && line1.bytes().all(|b| b.is_ascii_hexdigit())
    {
        Some(line1.to_ascii_uppercase())
    } else {
        None
    };
    Some(DacpSession {
        dacp_id,
        active_remote: line2.to_string(),
    })
}

// ----------------------------------------------------------------------------
// Discovery (_dacp._tcp browse + endpoint matching)
// ----------------------------------------------------------------------------

fn spawn_browse(app: AppHandle) {
    // Reuse the app's daemon (proven to work on this NIC/firewall setup);
    // fall back to our own only if discovery hasn't started one.
    let Some(daemon) = crate::discovery::shared_daemon().or_else(|| ServiceDaemon::new().ok()) else {
        return;
    };
    let Ok(rx) = daemon.browse("_dacp._tcp.local.") else {
        return;
    };
    std::thread::spawn(move || {
        // Hold the fallback daemon for the thread's life; the shared one is
        // already kept alive by discovery.rs.
        let _daemon = daemon;
        while let Ok(event) = rx.recv() {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    let instance = instance_of(info.get_fullname());
                    if instance.is_empty() {
                        continue;
                    }
                    // First IPv4 address (client speaks v4); .to_string() works
                    // whether get_addresses yields IpAddr or Ipv4Addr.
                    let ip = info
                        .get_addresses()
                        .iter()
                        .map(|a| a.to_string())
                        .find(|s| !s.contains(':'));
                    if let Some(ip) = ip {
                        ENDPOINTS
                            .lock()
                            .unwrap()
                            .insert((instance, ip), info.get_port());
                        try_resolve(&app);
                    }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    // Removal only carries the fullname, so drop every host's
                    // entry for that instance (colliding advertisers included).
                    let instance = instance_of(&fullname);
                    ENDPOINTS.lock().unwrap().retain(|(i, _), _| i != &instance);
                }
                _ => {}
            }
        }
    });
}

/// "iTunes_Ctrl_ABC._dacp._tcp.local." -> "iTunes_Ctrl_ABC"
fn instance_of(fullname: &str) -> String {
    fullname.split("._dacp").next().unwrap_or("").to_string()
}

/// Recompute the endpoint from the current session + advertised instances.
/// Called on every file change, ServiceResolved, and poll tick (the ad can lag
/// the RTSP handshake by seconds). Emits available:true on first match.
fn try_resolve(app: &AppHandle) {
    let session = { SESSION.lock().unwrap().clone() };
    let Some(session) = session else { return };
    if RESOLVED.lock().unwrap().is_some() {
        return; // already matched for this session
    }
    let endpoints = { ENDPOINTS.lock().unwrap().clone() };
    let client_ip = { CLIENT_IP.lock().unwrap().clone() };

    if let Some((ip, port)) = match_endpoint(&session, &endpoints, client_ip.as_deref()) {
        *RESOLVED.lock().unwrap() = Some(Resolved {
            ip,
            port,
            token: session.active_remote.clone(),
        });
        REFUSED.store(0, Ordering::SeqCst);
        emit_state(app, true, false);
    }
}

fn match_endpoint(
    session: &DacpSession,
    endpoints: &HashMap<(String, String), u16>,
    client_ip: Option<&str>,
) -> Option<(String, u16)> {
    // 1. If client_ip is known, prefer matching at that specific IP:
    if let Some(client) = client_ip {
        if let Some(id) = &session.dacp_id {
            let want = strip_leading_zeros(id);
            for ((instance, ip), port) in endpoints {
                if ip != client {
                    continue;
                }
                if let Some(adv) = instance.strip_prefix("iTunes_Ctrl_") {
                    let adv = adv.to_ascii_uppercase();
                    if strip_leading_zeros(&adv) == want {
                        return Some((ip.clone(), *port));
                    }
                }
            }
        }
        // Fallback with client_ip: if any advertised instance matches the client's IP
        for ((_, ip), port) in endpoints {
            if ip == client {
                return Some((ip.clone(), *port));
            }
        }
    }

    // 2. Direct cryptographic / identifier match: DACP-ID <-> iTunes_Ctrl_<id>
    if let Some(id) = &session.dacp_id {
        let want = strip_leading_zeros(id);
        for ((instance, ip), port) in endpoints {
            if let Some(adv) = instance.strip_prefix("iTunes_Ctrl_") {
                let adv = adv.to_ascii_uppercase();
                if strip_leading_zeros(&adv) == want {
                    return Some((ip.clone(), *port));
                }
            }
        }
    }

    // 3. Fallback when there is at least one DACP endpoint advertised on the LAN
    if let Some(((_, ip), port)) = endpoints.iter().next() {
        return Some((ip.clone(), *port));
    }

    None
}

/// Strip leading '0's for the shairport-style compare; an all-zero id -> "0".
fn strip_leading_zeros(s: &str) -> &str {
    let t = s.trim_start_matches('0');
    if t.is_empty() {
        "0"
    } else {
        t
    }
}

// ----------------------------------------------------------------------------
// Send path
// ----------------------------------------------------------------------------

/// Send one already-vetted `/ctrl-int/1/<path_q>` against the resolved endpoint,
/// handling stale-token (403) retry and connect-refused -> unavailable.
async fn send(path_q: String) -> Result<u16, String> {
    let Some(r) = ({ RESOLVED.lock().unwrap().clone() }) else {
        return Err("remote not available".into());
    };

    match dacp_get(&r.ip, r.port, &r.token, &path_q).await {
        Ok(code) => {
            REFUSED.store(0, Ordering::SeqCst);
            // 403 = wrong/stale Active-Remote: re-read the file once, retry once.
            if code == 403 {
                if let Some(fresh) = reread_token(&r.token).await {
                    if fresh != r.token {
                        if let Some(cur) = RESOLVED.lock().unwrap().as_mut() {
                            cur.token = fresh.clone();
                        }
                        if let Ok(code2) = dacp_get(&r.ip, r.port, &fresh, &path_q).await {
                            return Ok(code2);
                        }
                    }
                }
            }
            Ok(code)
        }
        Err(SendErr::Connect) => {
            // Two consecutive dead-endpoint hits => session is gone (uxplay was
            // killed, ephemeral port closed). Drop the endpoint, go unavailable.
            let n = REFUSED.fetch_add(1, Ordering::SeqCst) + 1;
            if n >= 2 {
                *RESOLVED.lock().unwrap() = None;
                // Also evict the advertisement we just proved dead, otherwise the
                // next try_resolve() re-matches the same stale ephemeral port
                // forever (the DACP port changes every session).
                ENDPOINTS
                    .lock()
                    .unwrap()
                    .retain(|(_, ip), port| !(ip == &r.ip && *port == r.port));
                if let Some(app) = APP.get() {
                    emit_state(app, false, false);
                }
            }
            Err("connect failed".into())
        }
        Err(SendErr::Other(e)) => Err(e),
    }
}

/// Latest Active-Remote token for the stale-token retry. Checks SESSION first
/// (populated by the 500 ms poller), and if it still matches `old_token`,
/// performs a non-blocking async read of the .dacp file in case the token was
/// rewritten between poll ticks.
async fn reread_token(old_token: &str) -> Option<String> {
    if let Some(tok) = SESSION
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.active_remote.clone())
    {
        if tok != old_token {
            return Some(tok);
        }
    }
    let text = tokio::fs::read_to_string(crate::engines::dacp_path())
        .await
        .ok()?;
    let parsed = parse_dacp(&text)?;
    let fresh = parsed.active_remote.clone();
    *SESSION.lock().unwrap() = Some(parsed);
    Some(fresh)
}

/// One-shot raw HTTP/1.1 GET over a fresh TcpStream (the DACP mini-server is a
/// plain, unpaired HTTP endpoint — Active-Remote is the entire auth). Mirrors
/// localsend's hand-rolled client but GET-only; returns the status code.
async fn dacp_get(ip: &str, port: u16, token: &str, path_q: &str) -> Result<u16, SendErr> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = match tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect((ip, port)),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(_)) => return Err(SendErr::Connect), // refused / unreachable
        Err(_) => return Err(SendErr::Connect),     // connect timeout
    };

    let req = format!(
        "GET /ctrl-int/1/{path_q} HTTP/1.1\r\nHost: {ip}:{port}\r\nActive-Remote: {token}\r\nUser-Agent: Remote/1028\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| SendErr::Other(format!("write: {e}")))?;
    let _ = stream.flush().await;

    // Only the status line matters; drain and close.
    let mut resp = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut resp)).await;
    parse_status(&resp).ok_or_else(|| SendErr::Other("no status line".into()))
}

/// Parse the code out of "HTTP/1.1 204 No Content".
fn parse_status(resp: &[u8]) -> Option<u16> {
    let end = resp.iter().position(|&b| b == b'\n').unwrap_or(resp.len());
    let line = std::str::from_utf8(&resp[..end]).ok()?;
    line.split_whitespace().nth(1)?.parse::<u16>().ok()
}

// ----------------------------------------------------------------------------
// Emit helper
// ----------------------------------------------------------------------------

/// Emit dacp://state only when it actually changes; also feeds `state()`.
fn emit_state(app: &AppHandle, available: bool, discovering: bool) {
    {
        let mut last = LAST_STATE.lock().unwrap();
        if *last == (available, discovering) {
            return;
        }
        *last = (available, discovering);
    }
    let _ = app.emit(
        "dacp://state",
        json!({ "available": available, "discovering": discovering }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_commands() {
        assert!(ALLOWED.contains(&"playpause"));
        assert!(ALLOWED.contains(&"nextitem"));
        assert!(ALLOWED.contains(&"previtem"));
        assert!(ALLOWED.contains(&"play"));
        assert!(ALLOWED.contains(&"pause"));
        assert!(!ALLOWED.contains(&"reboot"));
        assert!(!ALLOWED.contains(&"playpause\r\nInjected: True"));
    }

    #[test]
    fn test_strip_leading_zeros() {
        assert_eq!(strip_leading_zeros("0001A2B"), "1A2B");
        assert_eq!(strip_leading_zeros("123"), "123");
        assert_eq!(strip_leading_zeros("0000"), "0");
        assert_eq!(strip_leading_zeros("0"), "0");
        assert_eq!(strip_leading_zeros(""), "0");
    }

    #[test]
    fn test_parse_dacp() {
        let valid = "1234567890ABCDEF\r\n42424242\r\n";
        let parsed = parse_dacp(valid).expect("should parse valid .dacp");
        assert_eq!(parsed.dacp_id.as_deref(), Some("1234567890ABCDEF"));
        assert_eq!(parsed.active_remote, "42424242");

        let null_id = "(null)\r\n99999\r\n";
        let parsed_null = parse_dacp(null_id).expect("should parse null id");
        assert_eq!(parsed_null.dacp_id, None);
        assert_eq!(parsed_null.active_remote, "99999");
    }

    #[test]
    fn test_parse_status() {
        assert_eq!(parse_status(b"HTTP/1.1 204 No Content\r\n"), Some(204));
        assert_eq!(parse_status(b"HTTP/1.1 200 OK\r\n"), Some(200));
        assert_eq!(parse_status(b"HTTP/1.1 403 Forbidden\r\n"), Some(403));
        assert_eq!(parse_status(b"garbage"), None);
    }
}
