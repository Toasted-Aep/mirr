//! LocalSend v2 protocol — a real, cross-platform, MIT-licensed AirDrop alternative.
//!
//! Two halves:
//!   1. DISCOVERY over UDP multicast group 224.0.0.167:53317. Each device periodically
//!      multicasts a small JSON "announcement"; peers reply (here, via the HTTP
//!      /register endpoint) so both sides learn about each other.
//!   2. AN HTTP API on :53317 implementing the v2 endpoints used to receive files:
//!         GET  /api/localsend/v2/info
//!         POST /api/localsend/v2/register
//!         POST /api/localsend/v2/prepare-upload
//!         POST /api/localsend/v2/upload?sessionId=&fileId=&token=
//!      plus a `send_file` client for the outgoing direction.
//!
//! Reference: https://github.com/localsend/localsend/blob/main/documentation/PROTOCOL.md
//!
//! This implementation speaks plain HTTP (protocol = "http"). Production should add
//! the self-signed-TLS ("https") variant for parity with the mobile apps.

use axum::{
    body::Bytes,
    extract::Query,
    routing::{get, post},
    Json, Router,
};
use once_cell::sync::{Lazy, OnceCell};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
const PORT: u16 = 53317;
const PROTOCOL_VERSION: &str = "2.1";

static APP: OnceCell<AppHandle> = OnceCell::new();
static PEERS: Lazy<Mutex<HashMap<String, Peer>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static SESSIONS: Lazy<Mutex<HashMap<String, Session>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static FINGERPRINT: Lazy<String> = Lazy::new(generate_fingerprint);

/// What the UI sees.
#[derive(Clone, Serialize, Debug)]
pub struct Peer {
    pub alias: String,
    pub model: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub ip: String,
    pub port: u16,
    pub fingerprint: String,
}

/// Wire format of a LocalSend announcement / device descriptor.
#[derive(Clone, Serialize, Deserialize, Debug)]
struct DeviceInfo {
    alias: String,
    version: String,
    #[serde(rename = "deviceModel")]
    device_model: Option<String>,
    #[serde(rename = "deviceType")]
    device_type: Option<String>,
    fingerprint: String,
    port: u16,
    protocol: String,
    #[serde(default)]
    download: bool,
    #[serde(default)]
    announce: bool,
}

struct Session {
    dir: PathBuf,
    /// fileId -> (token, fileName)
    files: HashMap<String, (String, String)>,
}

pub fn device_alias() -> String {
    hostname().unwrap_or_else(|| "Mirr".to_string())
}

pub fn peers() -> Vec<Peer> {
    PEERS.lock().unwrap().values().cloned().collect()
}

/// Boot the LocalSend subsystem: store the app handle, then run the multicast
/// announcer/listener and the HTTP receive server on Tauri's async runtime.
pub fn start(app: AppHandle) {
    let _ = APP.set(app);
    tauri::async_runtime::spawn(async {
        if let Err(e) = run_http_server().await {
            eprintln!("[localsend] http server error: {e}");
        }
    });
    tauri::async_runtime::spawn(async {
        if let Err(e) = run_multicast().await {
            eprintln!("[localsend] multicast error: {e}");
        }
    });
}

fn self_info() -> DeviceInfo {
    DeviceInfo {
        alias: device_alias(),
        version: PROTOCOL_VERSION.to_string(),
        device_model: Some("Windows".to_string()),
        device_type: Some("desktop".to_string()),
        fingerprint: FINGERPRINT.clone(),
        port: PORT,
        protocol: "http".to_string(),
        download: false,
        announce: false,
    }
}

// ----------------------------------------------------------------------------
// Discovery (UDP multicast)
// ----------------------------------------------------------------------------

async fn run_multicast() -> anyhow::Result<()> {
    use socket2::{Domain, Protocol, Socket, Type};

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    let bind: SocketAddr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), PORT);
    socket.bind(&bind.into())?;
    socket.join_multicast_v4(&MULTICAST_ADDR, &Ipv4Addr::UNSPECIFIED)?;
    socket.set_multicast_loop_v4(false)?;
    socket.set_nonblocking(true)?;

    let std_sock: std::net::UdpSocket = socket.into();
    let udp = tokio::net::UdpSocket::from_std(std_sock)?;

    // periodic announcer
    let _announcer = {
        let udp = udp.clone_into_arc();
        tokio::spawn(async move {
            let target = SocketAddr::new(MULTICAST_ADDR.into(), PORT);
            loop {
                let mut info = self_info();
                info.announce = true;
                if let Ok(payload) = serde_json::to_vec(&info) {
                    let _ = udp.send_to(&payload, target).await;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        })
    };

    // listener
    let mut buf = vec![0u8; 8192];
    loop {
        let (n, from) = match udp.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
        if let Ok(info) = serde_json::from_slice::<DeviceInfo>(&buf[..n]) {
            if info.fingerprint == *FINGERPRINT {
                continue; // ourselves
            }
            let ip = from.ip().to_string();
            register_peer(&info, &ip);

            // If they announced, reply directly so they learn about us too.
            if info.announce {
                let ip2 = ip.clone();
                let port = info.port;
                tokio::spawn(async move {
                    let _ = reply_register(&ip2, port).await;
                });
            }
        }
    }
    #[allow(unreachable_code)]
    {
        _announcer.abort();
        Ok(())
    }
}

fn register_peer(info: &DeviceInfo, ip: &str) {
    let peer = Peer {
        alias: info.alias.clone(),
        model: info.device_model.clone().unwrap_or_default(),
        kind: info.device_type.clone().unwrap_or_else(|| "generic".into()),
        ip: ip.to_string(),
        port: info.port,
        fingerprint: info.fingerprint.clone(),
    };
    PEERS.lock().unwrap().insert(info.fingerprint.clone(), peer);
    emit_peers();
}

fn emit_peers() {
    if let Some(app) = APP.get() {
        let _ = app.emit("localsend://peers", peers());
    }
}

/// Tell a freshly-discovered peer about us via their /register endpoint.
async fn reply_register(ip: &str, port: u16) -> anyhow::Result<()> {
    let url = format!("http://{ip}:{port}/api/localsend/v2/register");
    let body = serde_json::to_vec(&self_info())?;
    // minimal client using std + tokio; reqwest would be the production choice
    http_post(&url, "application/json", &body).await?;
    Ok(())
}

// ----------------------------------------------------------------------------
// HTTP receive server
// ----------------------------------------------------------------------------

async fn run_http_server() -> anyhow::Result<()> {
    let app = Router::new()
        .route("/api/localsend/v2/info", get(info_handler))
        .route("/api/localsend/v2/register", post(register_handler))
        .route("/api/localsend/v2/prepare-upload", post(prepare_handler))
        .route("/api/localsend/v2/upload", post(upload_handler));

    let addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), PORT);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn info_handler() -> Json<serde_json::Value> {
    Json(serde_json::to_value(self_info()).unwrap())
}

async fn register_handler(Json(info): Json<DeviceInfo>) -> Json<serde_json::Value> {
    // peer learned of us and is registering; record them (ip unknown here, kept blank)
    register_peer(&info, "");
    Json(serde_json::to_value(self_info()).unwrap())
}

#[derive(Deserialize)]
struct PrepareRequest {
    #[allow(dead_code)]
    info: serde_json::Value,
    files: HashMap<String, FileMeta>,
}

#[derive(Deserialize)]
struct FileMeta {
    id: String,
    #[serde(rename = "fileName")]
    file_name: String,
}

async fn prepare_handler(Json(req): Json<PrepareRequest>) -> Json<serde_json::Value> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let dir = downloads_dir();
    let _ = std::fs::create_dir_all(&dir);

    let mut tokens = serde_json::Map::new();
    let mut files = HashMap::new();
    for (fid, meta) in req.files {
        let token = uuid::Uuid::new_v4().to_string();
        tokens.insert(fid.clone(), json!(token));
        files.insert(meta.id.clone(), (token, meta.file_name.clone()));
    }
    SESSIONS
        .lock()
        .unwrap()
        .insert(session_id.clone(), Session { dir, files });

    Json(json!({ "sessionId": session_id, "files": tokens }))
}

#[derive(Deserialize)]
struct UploadParams {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "fileId")]
    file_id: String,
    token: String,
}

async fn upload_handler(
    Query(p): Query<UploadParams>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let path = {
        let sessions = SESSIONS.lock().unwrap();
        let session = sessions
            .get(&p.session_id)
            .ok_or(axum::http::StatusCode::FORBIDDEN)?;
        let (token, name) = session
            .files
            .get(&p.file_id)
            .ok_or(axum::http::StatusCode::FORBIDDEN)?;
        if *token != p.token {
            return Err(axum::http::StatusCode::FORBIDDEN);
        }
        session.dir.join(sanitize(name))
    };

    std::fs::write(&path, &body).map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    if let Some(app) = APP.get() {
        let _ = app.emit("localsend://received", json!({ "path": path.to_string_lossy() }));
    }
    Ok(Json(json!({ "ok": true })))
}

// ----------------------------------------------------------------------------
// Outgoing: send a file to a peer
// ----------------------------------------------------------------------------

pub async fn send_file(ip: &str, port: u16, path: &str) -> anyhow::Result<()> {
    let pbuf = PathBuf::from(path);
    let file_name = pbuf
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let bytes = std::fs::read(&pbuf)?;

    // 1) prepare-upload
    let prepare = json!({
        "info": serde_json::to_value(self_info())?,
        "files": {
            "file1": {
                "id": "file1",
                "fileName": file_name,
                "size": bytes.len(),
                "fileType": "application/octet-stream"
            }
        }
    });
    let url = format!("http://{ip}:{port}/api/localsend/v2/prepare-upload");
    let resp = http_post(&url, "application/json", &serde_json::to_vec(&prepare)?).await?;
    let parsed: serde_json::Value = serde_json::from_slice(&resp)?;
    let session_id = parsed["sessionId"].as_str().unwrap_or_default().to_string();
    let token = parsed["files"]["file1"].as_str().unwrap_or_default().to_string();

    // 2) upload
    let url = format!(
        "http://{ip}:{port}/api/localsend/v2/upload?sessionId={session_id}&fileId=file1&token={token}"
    );
    http_post(&url, "application/octet-stream", &bytes).await?;
    Ok(())
}

// ----------------------------------------------------------------------------
// helpers
// ----------------------------------------------------------------------------

fn downloads_dir() -> PathBuf {
    dirs_downloads().join("Mirr")
}

fn dirs_downloads() -> PathBuf {
    if let Some(up) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(up).join("Downloads");
    }
    std::env::temp_dir()
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect()
}

fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
}

fn generate_fingerprint() -> String {
    use rand::Rng;
    use sha2::{Digest, Sha256};
    let salt: [u8; 16] = rand::thread_rng().gen();
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(device_alias().as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Tiny HTTP POST client (keeps the dependency surface small).
/// Production: swap for `reqwest` to gain HTTPS + connection reuse.
async fn http_post(url: &str, content_type: &str, body: &[u8]) -> anyhow::Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (host, port, path) = parse_url(url)?;
    let mut stream = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: {content_type}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
        len = body.len()
    );
    stream.write_all(req.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;

    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).await?;
    // strip headers, return body
    if let Some(pos) = find_subslice(&resp, b"\r\n\r\n") {
        Ok(resp[pos + 4..].to_vec())
    } else {
        Ok(resp)
    }
}

fn parse_url(url: &str) -> anyhow::Result<(String, u16, String)> {
    let rest = url.strip_prefix("http://").ok_or_else(|| anyhow::anyhow!("only http:// supported"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(PORT)),
        None => (authority.to_string(), PORT),
    };
    Ok((host, port, path.to_string()))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

// small ext-trait so the announcer can share the socket
trait CloneIntoArc {
    fn clone_into_arc(&self) -> std::sync::Arc<tokio::net::UdpSocket>;
}
impl CloneIntoArc for tokio::net::UdpSocket {
    fn clone_into_arc(&self) -> std::sync::Arc<tokio::net::UdpSocket> {
        // The listener owns the socket; for sending we open a second ephemeral socket.
        // Returning a fresh sender keeps ownership simple.
        let sender = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("bind sender");
        sender.set_multicast_loop_v4(false).ok();
        sender.set_nonblocking(true).ok();
        std::sync::Arc::new(tokio::net::UdpSocket::from_std(sender).expect("tokio sender"))
    }
}
