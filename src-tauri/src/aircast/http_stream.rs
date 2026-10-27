//! Axum HTTP streaming server for outbound Cast audio and inbound DIAL/DLNA media.

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Router,
};
use once_cell::sync::OnceCell;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

static HTTP_PORT: AtomicU16 = AtomicU16::new(7070);
static SERVER_STARTED: OnceCell<()> = OnceCell::new();

pub fn http_port() -> u16 {
    HTTP_PORT.load(Ordering::SeqCst)
}

pub fn stream_url(host: &str) -> String {
    format!("http://{}:{}/aircast/stream.wav", host, http_port())
}

#[derive(Clone)]
pub struct AppState {
    pub app_handle: Option<tauri::AppHandle>,
}

/// Start the Axum HTTP server if not already running.
pub fn ensure_http_server(app_handle: Option<tauri::AppHandle>) {
    if SERVER_STARTED.set(()).is_err() {
        return;
    }

    let state = AppState { app_handle };

    tokio::spawn(async move {
        let app = Router::new()
            // Outbound Audio chunked streaming
            .route("/aircast/stream.wav", get(audio_stream_handler))
            .route("/stream.wav", get(audio_stream_handler))
            // Phase 2: DIAL protocol endpoints (supports YouTube and any DIAL app dynamically)
            .route("/dd.xml", get(dial_dd_handler))
            .route("/apps/:app_name", get(dial_app_get).post(dial_app_post))
            .route("/apps/:app_name/run", delete(dial_app_delete))
            // Phase 2: UPnP / DLNA endpoints
            .route("/upnp/dev.xml", get(dlna_dev_handler))
            .route("/upnp/scpd/AVTransport.xml", get(dlna_scpd_avt))
            .route("/upnp/scpd/RenderingControl.xml", get(dlna_scpd_rc))
            .route("/upnp/control/AVTransport", post(dlna_control_avt))
            .route("/upnp/control/RenderingControl", post(dlna_control_rc))
            .route("/upnp/control/ConnectionManager", post(dlna_control_cm))
            .with_state(state);

        // Try binding port 7070, then fallback up to 7080
        for port in 7070..=7085 {
            let addr = SocketAddr::from(([0, 0, 0, 0], port));
            match tokio::net::TcpListener::bind(addr).await {
                Ok(listener) => {
                    HTTP_PORT.store(port, Ordering::SeqCst);
                    eprintln!("[aircast-http] Axum server running on http://0.0.0.0:{port}");
                    let _ = axum::serve(
                        listener,
                        app.into_make_service_with_connect_info::<SocketAddr>(),
                    )
                    .await;
                    break;
                }
                Err(_) => continue,
            }
        }
    });
}

/// Chunked WAV audio stream handler.
/// Emits 44-byte WAV header first, followed by live WASAPI loopback chunks.
async fn audio_stream_handler() -> Response {
    let rx = crate::aircast::loopback::subscribe();
    let current_sr = crate::aircast::loopback::sample_rate();

    let header_bytes = Bytes::copy_from_slice(&crate::aircast::loopback::generate_wav_header(current_sr, 2));
    let initial = tokio_stream::once(Ok::<Bytes, std::io::Error>(header_bytes));

    let rx_stream = BroadcastStream::new(rx).filter_map(|res| match res {
        Ok(chunk) => Some(Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&chunk))),
        Err(_) => None, // missed chunks due to lag, skip gracefully
    });

    let stream = initial.chain(rx_stream);
    let body = Body::from_stream(stream);

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "audio/wav".parse().unwrap());
    headers.insert(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate".parse().unwrap());
    headers.insert(header::CONNECTION, "keep-alive".parse().unwrap());
    headers.insert(header::ACCEPT_RANGES, "none".parse().unwrap());
    headers.insert(
        header::HeaderName::from_static("icy-name"),
        "Mirr PC Audio".parse().unwrap(),
    );

    (headers, body).into_response()
}

// -------------------------------------------------------------
// Phase 2: DIAL handlers
// -------------------------------------------------------------
async fn dial_dd_handler() -> Response {
    let xml = crate::aircast::dial::device_description_xml();
    (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

async fn dial_app_get(axum::extract::Path(app_name): axum::extract::Path<String>) -> Response {
    let xml = crate::aircast::dial::app_status_xml(&app_name);
    (
        [(header::CONTENT_TYPE, "text/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

async fn dial_app_post(
    State(state): State<AppState>,
    axum::extract::Path(app_name): axum::extract::Path<String>,
    body: String,
) -> Response {
    crate::aircast::dial::handle_launch(&state.app_handle, &app_name, &body)
}

async fn dial_app_delete(
    State(state): State<AppState>,
    axum::extract::Path(app_name): axum::extract::Path<String>,
) -> Response {
    crate::aircast::dial::handle_stop(&state.app_handle, &app_name);
    StatusCode::OK.into_response()
}

// -------------------------------------------------------------
// Phase 2: UPnP / DLNA handlers
// -------------------------------------------------------------
async fn dlna_dev_handler() -> Response {
    let xml = crate::aircast::dlna::device_description_xml();
    (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

async fn dlna_scpd_avt() -> Response {
    let xml = crate::aircast::dlna::scpd_av_transport();
    (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

async fn dlna_scpd_rc() -> Response {
    let xml = crate::aircast::dlna::scpd_rendering_control();
    (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

async fn dlna_control_avt(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    crate::aircast::dlna::handle_av_transport(&state.app_handle, &headers, &body).await
}

async fn dlna_control_rc(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    crate::aircast::dlna::handle_rendering_control(&state.app_handle, &headers, &body).await
}

async fn dlna_control_cm(_headers: HeaderMap, _body: String) -> Response {
    crate::aircast::dlna::handle_connection_manager()
}
