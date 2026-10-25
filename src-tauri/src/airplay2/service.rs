//! AirPlay 2 Receiver Service Supervisor for Mirr.
//!
//! Owns the Tokio worker hosting the embedded `openairplay2` receiver:
//! - Configures the receiver with `.advertise(false)` to bypass Avahi.
//! - Publishes `_airplay._tcp` on port 7000 using `mdns-sd` with the TXT records.
//! - Bridges session events (`Metadata`, `Artwork`, `Progress`, `Volume`, `Paused`, `SessionStarted`, `SessionEnded`)
//!   to Tauri events, Now Playing, system volume, and the frontend.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use base64::Engine;
use mdns_sd::ServiceInfo;
use openairplay2::{Event, Receiver};
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::airplay2::sink::WasapiSink;

static RUNNING: AtomicBool = AtomicBool::new(false);
static SERVICE_INFO: Mutex<Option<ServiceInfo>> = Mutex::new(None);
static SHUTDOWN_TX: Mutex<Option<tokio::sync::oneshot::Sender<()>>> = Mutex::new(None);
static DEDICATED_DAEMON: Mutex<Option<mdns_sd::ServiceDaemon>> = Mutex::new(None);

pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

pub fn stop(app: Option<&AppHandle>) {
    if !RUNNING.swap(false, Ordering::SeqCst) {
        return;
    }

    if let Some(tx) = SHUTDOWN_TX.lock().unwrap().take() {
        let _ = tx.send(());
    }

    let dedicated = DEDICATED_DAEMON.lock().unwrap().take();
    let daemon = crate::discovery::shared_daemon().or(dedicated);
    if let Some(daemon) = daemon {
        if let Some(info) = SERVICE_INFO.lock().unwrap().take() {
            let _ = daemon.unregister(&info.get_fullname());
        }
    }

    if let Some(app) = app {
        let _ = app.emit("airplay2://status", json!({ "state": "stopped", "detail": "" }));
        let _ = app.emit("airplay2://session", json!({ "active": false }));
        crate::nowplaying::airplay2_session_ended(app);
        crate::discovery::note_client_disconnected();
        crate::discovery::emit_devices();
    }
}

pub fn start(app: AppHandle) -> anyhow::Result<()> {
    if RUNNING.load(Ordering::SeqCst) {
        return Ok(()); // already running
    }

    let cfg = crate::engines::get_config();
    let broadcast_name = if cfg.broadcast_name.trim().is_empty() {
        "Mirr".to_string()
    } else {
        cfg.broadcast_name.trim().to_string()
    };
    let a2_name = format!("{broadcast_name} Audio");

    let identity_dir = crate::engines::nowplaying_dir();
    let _ = std::fs::create_dir_all(&identity_dir);
    let identity_path = identity_dir.join("airplay2-identity");

    let receiver = Receiver::builder()
        .name(&a2_name)
        .port(7000)
        .identity_path(&identity_path)
        .advertise(false)
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build AirPlay 2 receiver: {e}"))?;

    // Prepare mDNS announcement using mdns-sd
    let txt_records = receiver.txt_records();
    let mut props = HashMap::new();
    for rec in txt_records {
        if let Some((k, v)) = rec.split_once('=') {
            props.insert(k.to_string(), v.to_string());
        }
    }

    let lan_ip = local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string());

    let hostname = crate::localsend::device_alias()
        .replace(' ', "-")
        .replace('.', "-");
    let host_domain = format!("{hostname}.local.");

    let service_type = "_airplay._tcp.local.";
    let s_info = ServiceInfo::new(
        service_type,
        &a2_name,
        &host_domain,
        lan_ip.as_str(),
        7000,
        Some(props),
    )
    .map_err(|e| anyhow::anyhow!("Failed to construct ServiceInfo for mDNS: {e}"))?
    .enable_addr_auto();

    let daemon = crate::discovery::shared_daemon().or_else(|| {
        let d = mdns_sd::ServiceDaemon::new().ok()?;
        *DEDICATED_DAEMON.lock().unwrap() = Some(d.clone());
        Some(d)
    });
    if let Some(daemon) = daemon {
        if let Err(e) = daemon.register(s_info.clone()) {
            eprintln!("[airplay2::service] mDNS register failed: {e}");
        } else {
            *SERVICE_INFO.lock().unwrap() = Some(s_info);
        }
    }

    RUNNING.store(true, Ordering::SeqCst);

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    *SHUTDOWN_TX.lock().unwrap() = Some(shutdown_tx);

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();

    let app_clone = app.clone();

    // On Windows, Tokio's TcpListener::bind(::) defaults to IPV6_V6ONLY=1,
    // preventing IPv4 Wi-Fi senders from connecting. By temporarily holding [::]:7000,
    // openairplay2's bind fallback automatically binds directly to 0.0.0.0:7000.
    #[cfg(windows)]
    let v6_blocker = std::net::TcpListener::bind("[::]:7000").ok();

    // Spawn the receiver loop on Tokio runtime
    tauri::async_runtime::spawn(async move {
        #[cfg(windows)]
        if let Some(blocker) = v6_blocker {
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                drop(blocker);
            });
        }

        let _ = app_clone.emit("airplay2://status", json!({ "state": "ready", "name": a2_name }));

        // Event processing task
        let app_events = app_clone.clone();
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    Event::SessionStarted { rate, channels, peer } => {
                        let clean_peer = match peer {
                            std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map(std::net::IpAddr::V4).unwrap_or(peer),
                            std::net::IpAddr::V4(_) => peer,
                        };
                        let peer_str = clean_peer.to_string();

                        let _ = app_events.emit(
                            "airplay2://status",
                            json!({ "state": "streaming", "peer": peer_str }),
                        );
                        let _ = app_events.emit(
                            "airplay2://session",
                            json!({
                                "active": true,
                                "peer": peer_str,
                                "rate": rate,
                                "channels": channels
                            }),
                        );
                        crate::discovery::note_client_connected(Some(&peer_str), None, None, None);
                        crate::nowplaying::airplay2_session_started(&app_events);
                    }
                    Event::Metadata { title, artist, album } => {
                        let _ = app_events.emit(
                            "airplay2://metadata",
                            json!({
                                "title": title,
                                "artist": artist,
                                "album": album
                            }),
                        );
                        crate::nowplaying::airplay2_metadata(
                            &app_events,
                            title.as_deref(),
                            artist.as_deref(),
                            album.as_deref(),
                        );
                    }
                    Event::Artwork { content_type, data } => {
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
                        let data_url = if data.is_empty() {
                            None
                        } else {
                            Some(format!("data:{content_type};base64,{b64}"))
                        };
                        let _ = app_events.emit(
                            "airplay2://artwork",
                            json!({
                                "contentType": content_type,
                                "dataUrl": data_url
                            }),
                        );
                        crate::nowplaying::airplay2_artwork(&app_events, data_url);
                    }
                    Event::Progress { elapsed, duration } => {
                        let elapsed_secs = elapsed.as_secs_f64();
                        let duration_secs = duration.as_secs_f64();
                        let pct = if duration_secs > 0.0 {
                            (elapsed_secs / duration_secs).min(1.0) as f32
                        } else {
                            0.0
                        };
                        let _ = app_events.emit(
                            "airplay2://progress",
                            json!({
                                "elapsed": elapsed.as_secs(),
                                "duration": duration.as_secs(),
                                "elapsedSecs": elapsed_secs,
                                "durationSecs": duration_secs,
                                "percent": pct
                            }),
                        );
                        crate::nowplaying::airplay2_progress(
                            &app_events,
                            elapsed.as_secs(),
                            duration.as_secs(),
                        );
                    }
                    Event::Volume { db } => {
                        // Map dB [-30, 0] to pct [0.0, 1.0]
                        let pct = if db <= -144.0 || db <= -30.0 {
                            0.0
                        } else {
                            ((db + 30.0) / 30.0).clamp(0.0, 1.0)
                        };
                        let _ = app_events.emit(
                            "airplay2://volume",
                            json!({ "db": db, "percent": pct }),
                        );
                        let _ = crate::audio::set_volume(pct * 100.0, None);
                    }
                    Event::Paused(paused) => {
                        let _ = app_events.emit(
                            "airplay2://paused",
                            json!({ "paused": paused }),
                        );
                        crate::nowplaying::airplay2_paused(&app_events, paused);
                    }
                    Event::Flushed => {
                        let _ = app_events.emit("airplay2://flushed", json!({}));
                    }
                    Event::SessionEnded => {
                        let _ = app_events.emit(
                            "airplay2://session",
                            json!({ "active": false }),
                        );
                        let _ = app_events.emit(
                            "airplay2://status",
                            json!({ "state": "ready" }),
                        );
                        crate::discovery::note_client_disconnected();
                        crate::discovery::emit_devices();
                        crate::nowplaying::airplay2_session_ended(&app_events);
                    }
                    _ => {}
                }
            }
        });

        // Run the receiver until shutdown signal
        let sink_factory = move |rate: u32, channels: u8| -> Box<dyn openairplay2::AudioSink> {
            let cur_cfg = crate::engines::get_config();
            let audio_device_name = cur_cfg.audio_output_device;
            let dev = if audio_device_name.is_empty() || audio_device_name == "default" {
                None
            } else {
                Some(audio_device_name.as_str())
            };
            match WasapiSink::new(rate, channels, dev) {
                Ok(sink) => Box::new(sink),
                Err(e) => {
                    eprintln!("[airplay2] Failed to create WasapiSink: {e}");
                    struct NoopSink;
                    impl openairplay2::AudioSink for NoopSink {
                        fn write(&mut self, _pcm: &[i16]) {}
                        fn flush(&mut self) {}
                    }
                    Box::new(NoopSink)
                }
            }
        };

        tokio::select! {
            res = receiver.run(sink_factory, event_tx) => {
                if let Err(e) = res {
                    eprintln!("[airplay2] Receiver error: {e}");
                    let _ = app_clone.emit("airplay2://status", json!({ "state": "error", "detail": e.to_string() }));
                }
            }
            _ = &mut shutdown_rx => {
                eprintln!("[airplay2] Receiver shut down gracefully");
            }
        }

        // Cleanup mDNS and session state upon termination
        let dedicated = DEDICATED_DAEMON.lock().unwrap().take();
        let daemon = crate::discovery::shared_daemon().or(dedicated);
        if let Some(daemon) = daemon {
            if let Some(info) = SERVICE_INFO.lock().unwrap().take() {
                let _ = daemon.unregister(&info.get_fullname());
            }
        }
        let _ = SHUTDOWN_TX.lock().unwrap().take();
        RUNNING.store(false, Ordering::SeqCst);
        let _ = app_clone.emit("airplay2://session", json!({ "active": false }));
        crate::discovery::note_client_disconnected();
        crate::discovery::emit_devices();
        crate::nowplaying::airplay2_session_ended(&app_clone);
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_airplay2_receiver_build_and_txt_records() {
        let temp_dir = std::env::temp_dir().join(format!("mirr-test-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let id_path = temp_dir.join("id");

        let receiver = Receiver::builder()
            .name("Mirr Audio Test")
            .port(7000)
            .identity_path(&id_path)
            .advertise(false)
            .build()
            .expect("Receiver build should succeed");

        assert_eq!(receiver.config().name, "Mirr Audio Test");
        assert_eq!(receiver.config().port, 7000);

        let records = receiver.txt_records();
        let map: HashMap<String, String> = records
            .into_iter()
            .filter_map(|r| r.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
            .collect();

        assert!(map.contains_key("deviceid"), "must contain deviceid");
        assert!(map.contains_key("features"), "must contain features");
        assert!(map.contains_key("flags"), "must contain flags");
        assert!(map.contains_key("model"), "must contain model");
        assert!(map.contains_key("pi"), "must contain pi");
        assert!(map.contains_key("pk"), "must contain pk");
        assert_eq!(map.get("vv").map(|s| s.as_str()), Some("2"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_db_to_pct_conversion() {
        fn db_to_pct(db: f32) -> f32 {
            if db <= -144.0 || db <= -30.0 {
                0.0
            } else {
                ((db + 30.0) / 30.0).clamp(0.0, 1.0)
            }
        }

        assert_eq!(db_to_pct(0.0), 1.0);
        assert_eq!(db_to_pct(-30.0), 0.0);
        assert_eq!(db_to_pct(-144.0), 0.0);
        assert!((db_to_pct(-15.0) - 0.5).abs() < 1e-4);
    }

    #[tokio::test]
    async fn test_airplay2_receiver_http_info_end_to_end() {
        let temp_dir = std::env::temp_dir().join(format!("mirr-info-test-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let id_path = temp_dir.join("id");

        let receiver = Receiver::builder()
            .name("Mirr Audio Test Info")
            .port(7009)
            .identity_path(&id_path)
            .advertise(false)
            .build()
            .expect("Receiver build should succeed");

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();

        struct NoopSink;
        impl openairplay2::AudioSink for NoopSink {
            fn write(&mut self, _pcm: &[i16]) {}
            fn flush(&mut self) {}
        }

        // Block IPv6 bind so openairplay2 falls back to IPv4 (0.0.0.0:7009)
        let v6_blocker = std::net::TcpListener::bind("[::]:7009").expect("bind v6 blocker");

        let task = tokio::spawn(async move {
            tokio::select! {
                res = receiver.run(|_, _| Box::new(NoopSink), event_tx) => {
                    eprintln!("[test] receiver run exited: {res:?}");
                }
                _ = shutdown_rx => {}
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        // Drop blocker now that receiver bound IPv4
        drop(v6_blocker);

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect("127.0.0.1:7009").await.expect("connect directly to receiver port 7009 over IPv4");
        stream.write_all(b"GET /info HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").await.expect("write request");

        let mut buf = vec![0u8; 2048];
        let n = stream.read(&mut buf).await.expect("read response");
        let resp = String::from_utf8_lossy(&buf[..n]);

        assert!(resp.starts_with("HTTP/1.1 200 OK"), "Expected 200 OK, got: {resp}");
        assert!(resp.contains("application/x-apple-binary-plist"), "Expected binary plist content type");

        let _ = shutdown_tx.send(());
        let _ = task.await;
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}


