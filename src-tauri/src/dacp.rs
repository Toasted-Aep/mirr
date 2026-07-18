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

/// Idempotent: spawn the .dacp file poller (500 ms) and the long-lived
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
pub fn on_log_line(_app: &AppHandle, line: &str) {
    let l = line.to_lowercase();
    if !(l.contains("accepted") && l.contains("client") && l.contains(" from ")) {
        return;
    }
    // Positions are stable: these lines are pure ASCII, so lowercase indexing
    // maps 1:1 onto the original-case slice.
    if let Some(start) = l.rfind(" from ").map(|i| i + " from ".len()) {
        let tail = &line[start..];
        // "<ip>[:port] ..." -> first token, then strip the :port.
        let ip = tail
            .split_whitespace()
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .trim();
        if !ip.is_empty() {
            *CLIENT_IP.lock().unwrap() = Some(ip.to_string());
        }
    }
}

/// Engine stopped — clear session/endpoint and emit unavailable.
pub fn on_engine_stop(app: &AppHandle) {
    *SESSION.lock().unwrap() = None;
    *RESOLVED.lock().unwrap() = None;
    *CLIENT_IP.lock().unwrap() = None;
    REFUSED.store(0, Ordering::SeqCst);
    // ENDPOINTS is deliberately NOT cleared: the browse lives for the whole
    // process and mdns-sd won't re-emit ServiceResolved for an entry it already
    // resolved, so a reconnecting phone must still be matchable next session.
    emit_state(app, false, false);
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
    std::thread::spawn(move || loop {
        poll_once(&app);
        std::thread::sleep(Duration::from_millis(500));
    });
}

fn poll_once(app: &AppHandle) {
    // read_to_string fails (missing file) => session gone.
    let Some(text) = std::fs::read_to_string(crate::engines::dacp_path()).ok() else {
        clear_session(app);
        return;
    };
    // A present-but-unusable file (only one line, or no Active-Remote) is
    // treated the same as absent: nothing to authenticate with.
    let Some(new_session) = parse_dacp(&text) else {
        clear_session(app);
        return;
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
    // FAIL CLOSED — deliberate, not an oversight. The Active-Remote token we
    // would send IS the entire authentication for controlling the user's phone,
    // and the `iTunes_Ctrl_<id>` instance name is multicast to the whole LAN, so
    // any on-LAN device can advertise a colliding record. A known client IP is
    // therefore required for EVERY match, including the DACP-ID one: without it
    // the ID match is trivially spoofable. UxPlay accepts the client connection
    // (where we capture the IP) before it ever writes the .dacp file, so this
    // costs no availability in practice — and if the log format ever changes,
    // losing transport controls beats leaking a capability token to the LAN.
    let client = client_ip?;

    // Primary: DACP-ID <-> iTunes_Ctrl_<id>, uppercase + leading-zero stripped,
    // pinned to the client IP.
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
    // Fallback: exactly one advertised instance at the connected client's IP.
    let mut hit = None;
    let mut count = 0usize;
    for ((_, ip), port) in endpoints {
        if ip == client {
            count += 1;
            hit = Some((ip.clone(), *port));
        }
    }
    if count == 1 {
        return hit;
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
                if let Some(fresh) = reread_token() {
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

/// Latest Active-Remote token for the stale-token retry. Read from SESSION, not
/// from disk: the 500 ms poller already parses the file into SESSION, and this
/// runs on the async runtime where a blocking read would stall the executor.
fn reread_token() -> Option<String> {
    SESSION
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.active_remote.clone())
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
        "GET /ctrl-int/1/{path_q} HTTP/1.1\r\nHost: {ip}:{port}\r\nActive-Remote: {token}\r\nConnection: close\r\n\r\n"
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
