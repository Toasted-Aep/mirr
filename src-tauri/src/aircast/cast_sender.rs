//! Google Cast client implementation for outbound audio streaming using rust_cast.

use once_cell::sync::Lazy;
use rust_cast::channels::media::{Media, Metadata, MusicTrackMediaMetadata, StreamType};
use rust_cast::channels::receiver::{CastDeviceApp, Volume};
use rust_cast::CastDevice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

use std::sync::Arc;

static ACTIVE_SESSION: Lazy<Mutex<Option<ActiveCastSession>>> =
    Lazy::new(|| Mutex::new(None));
static CASTING: AtomicBool = AtomicBool::new(false);

struct ActiveCastSession {
    target_ip: String,
    session_id: String,
    device: Arc<Mutex<Option<CastDevice<'static>>>>,
    stop_tx: tokio::sync::oneshot::Sender<()>,
}

pub fn is_casting() -> bool {
    CASTING.load(Ordering::SeqCst)
}

pub fn current_target() -> Option<String> {
    ACTIVE_SESSION
        .lock()
        .ok()?
        .as_ref()
        .map(|s| s.target_ip.clone())
}

/// Start casting outbound PC audio to a Google Cast receiver (e.g. Chromecast, Nest Audio).
pub fn start_cast(target_ip: &str, app: Option<&AppHandle>) -> anyhow::Result<()> {
    // 1. Stop any current session first to avoid resource or port contention
    stop_cast();

    // 2. Ensure WASAPI loopback capture and HTTP server are active
    crate::aircast::loopback::ensure_capture_running();
    crate::aircast::http_stream::ensure_http_server(app.cloned());

    let target_str = target_ip.to_string();
    let app_handle = app.cloned();

    // Determine our LAN IP to format the stream URL
    let lan_ip = crate::aircast::dial::best_lan_ip();
    let stream_url = crate::aircast::http_stream::stream_url(&lan_ip);

    eprintln!("[aircast-sender] Connecting to Google Cast at {target_str}:8009 with stream {stream_url}");

    // Fast probe (1500ms) to ensure port 8009 is reachable before starting TLS
    let probe_addr: std::net::SocketAddr = format!("{target_str}:8009")
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid target address {target_str}: {e}"))?;
    let _probe = std::net::TcpStream::connect_timeout(&probe_addr, Duration::from_millis(1500))
        .map_err(|e| anyhow::anyhow!("Google Cast port 8009 unreachable on {target_str}: {e}"))?;
    drop(_probe);

    // Connect to Google Cast device without certificate verification (Cast devices use self-signed root certs)
    let device = CastDevice::connect_without_host_verification(target_str.clone(), 8009)
        .map_err(|e| anyhow::anyhow!("Failed to connect to Google Cast device at {target_str}: {e}"))?;

    // Connect to default receiver-0
    device
        .connection
        .connect("receiver-0")
        .map_err(|e| anyhow::anyhow!("Failed to connect to Cast receiver: {e}"))?;

    // Launch the Default Media Receiver (CC1AD845)
    let app_info = device
        .receiver
        .launch_app(&CastDeviceApp::DefaultMediaReceiver)
        .map_err(|e| anyhow::anyhow!("Failed to launch Default Media Receiver: {e}"))?;

    let transport_id = app_info.transport_id.clone();
    let session_id = app_info.session_id.clone();

    // Connect to the launched media app transport
    device
        .connection
        .connect(&transport_id)
        .map_err(|e| anyhow::anyhow!("Failed to connect to Cast app transport: {e}"))?;

    // Prepare media load request pointing to our live chunked WAV stream
    let media = Media {
        content_id: stream_url.clone(),
        stream_type: StreamType::Live,
        content_type: "audio/wav".to_string(),
        metadata: Some(Metadata::MusicTrack(MusicTrackMediaMetadata {
            title: Some("Mirr PC Audio".to_string()),
            artist: Some("Mirr Wireless Bridge".to_string()),
            album_name: Some("Live Windows Audio".to_string()),
            ..Default::default()
        })),
        duration: None,
    };

    device
        .media
        .load(&transport_id, &session_id, &media)
        .map_err(|e| anyhow::anyhow!("Failed to load audio stream on Cast device: {e}"))?;

    CASTING.store(true, Ordering::SeqCst);

    if let Some(ref handle) = app_handle {
        let _ = handle.emit(
            "aircast://status",
            serde_json::json!({
                "state": "streaming",
                "target": target_str,
                "url": stream_url
            }),
        );
    }

    let dev_arc = Arc::new(Mutex::new(Some(device)));
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

    let ping_dev = Arc::clone(&dev_arc);
    let target_ping = target_str.clone();
    let app_ping = app_handle.clone();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(4));
        loop {
            tokio::select! {
                _ = &mut stop_rx => {
                    eprintln!("[aircast-sender] Heartbeat received stop signal for {target_ping}");
                    break;
                }
                _ = interval.tick() => {
                    let ping_ok = {
                        let lock = ping_dev.lock().unwrap();
                        if let Some(ref dev) = *lock {
                            dev.heartbeat.ping().is_ok()
                        } else {
                            false
                        }
                    };
                    if !ping_ok {
                        eprintln!("[aircast-sender] Ping failed on active connection for {target_ping}");
                        CASTING.store(false, Ordering::SeqCst);
                        if let Some(ref handle) = app_ping {
                            let _ = handle.emit(
                                "aircast://status",
                                serde_json::json!({
                                    "state": "disconnected",
                                    "target": target_ping
                                }),
                            );
                        }
                        break;
                    }
                }
            }
        }
    });

    if let Ok(mut lock) = ACTIVE_SESSION.lock() {
        *lock = Some(ActiveCastSession {
            target_ip: target_str,
            session_id,
            device: dev_arc,
            stop_tx,
        });
    }

    Ok(())
}

/// Stop the currently active Google Cast audio session.
pub fn stop_cast() {
    let session = {
        if let Ok(mut lock) = ACTIVE_SESSION.lock() {
            lock.take()
        } else {
            None
        }
    };

    if let Some(sess) = session {
        let _ = sess.stop_tx.send(());
        if let Ok(mut dev_lock) = sess.device.lock() {
            if let Some(dev) = dev_lock.take() {
                let _ = dev.connection.connect("receiver-0");
                let _ = dev.receiver.stop_app(&sess.session_id);
            }
        }
    }

    CASTING.store(false, Ordering::SeqCst);
}

/// Adjust volume on active Google Cast device (0.0 to 1.0).
#[allow(dead_code)]
pub fn set_volume(level: f32) -> anyhow::Result<()> {
    let session = ACTIVE_SESSION.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(ref sess) = *session {
        if let Ok(dev_lock) = sess.device.lock() {
            if let Some(ref dev) = *dev_lock {
                let vol = Volume::from(level.clamp(0.0, 1.0));
                let _ = dev.receiver.set_volume(vol);
            }
        }
    }
    Ok(())
}
