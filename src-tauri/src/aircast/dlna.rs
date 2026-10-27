//! UPnP / DLNA MediaRenderer:1 implementation for casting video and music from phones to Mirr.

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use once_cell::sync::Lazy;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

static DLNA_UUID: Lazy<String> = Lazy::new(|| uuid::Uuid::new_v4().to_string());

pub fn dlna_uuid() -> &'static str {
    &*DLNA_UUID
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct DlnaMediaState {
    pub current_uri: String,
    pub title: String,
    pub artist: String,
    pub transport_state: String, // STOPPED | PLAYING | PAUSED_PLAYBACK
    pub volume: u32,
}

static MEDIA_STATE: Lazy<Mutex<DlnaMediaState>> = Lazy::new(|| {
    Mutex::new(DlnaMediaState {
        transport_state: "STOPPED".to_string(),
        volume: 100,
        ..Default::default()
    })
});

pub fn device_description_xml() -> String {
    let cfg = crate::engines::get_config();
    let name = cfg.broadcast_name;
    let uuid = dlna_uuid();
    let lan_ip = crate::aircast::dial::best_lan_ip();
    let port = crate::aircast::http_stream::http_port();

    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion>
    <major>1</major>
    <minor>0</minor>
  </specVersion>
  <device>
    <deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>
    <friendlyName>{name} (DLNA)</friendlyName>
    <manufacturer>Mirr Bridge</manufacturer>
    <modelName>Mirr MediaRenderer</modelName>
    <modelNumber>1.0</modelNumber>
    <UDN>uuid:{uuid}</UDN>
    <serviceList>
      <service>
        <serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:AVTransport</serviceId>
        <SCPDURL>/upnp/scpd/AVTransport.xml</SCPDURL>
        <controlURL>/upnp/control/AVTransport</controlURL>
        <eventSubURL>/upnp/event/AVTransport</eventSubURL>
      </service>
      <service>
        <serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:RenderingControl</serviceId>
        <SCPDURL>/upnp/scpd/RenderingControl.xml</SCPDURL>
        <controlURL>/upnp/control/RenderingControl</controlURL>
        <eventSubURL>/upnp/event/RenderingControl</eventSubURL>
      </service>
      <service>
        <serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:ConnectionManager</serviceId>
        <SCPDURL></SCPDURL>
        <controlURL>/upnp/control/ConnectionManager</controlURL>
        <eventSubURL></eventSubURL>
      </service>
    </serviceList>
  </device>
  <URLBase>http://{lan_ip}:{port}/</URLBase>
</root>"#
    )
}

pub fn scpd_av_transport() -> String {
    r#"<?xml version="1.0" encoding="utf-8"?>
<scpd xmlns="urn:schemas-upnp-org:service-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <actionList>
    <action><name>SetAVTransportURI</name></action>
    <action><name>Play</name></action>
    <action><name>Pause</name></action>
    <action><name>Stop</name></action>
    <action><name>GetTransportInfo</name></action>
    <action><name>GetPositionInfo</name></action>
  </actionList>
</scpd>"#.to_string()
}

pub fn scpd_rendering_control() -> String {
    r#"<?xml version="1.0" encoding="utf-8"?>
<scpd xmlns="urn:schemas-upnp-org:service-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <actionList>
    <action><name>SetVolume</name></action>
    <action><name>GetVolume</name></action>
    <action><name>SetMute</name></action>
    <action><name>GetMute</name></action>
  </actionList>
</scpd>"#.to_string()
}

pub fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let mut search_idx = 0;
    while let Some(pos) = xml[search_idx..].find('<') {
        let start = search_idx + pos + 1;
        if start >= xml.len() {
            break;
        }
        if xml[start..].starts_with('/') {
            if let Some(close_end) = xml[start..].find('>') {
                search_idx = start + close_end + 1;
                continue;
            } else {
                break;
            }
        }
        if let Some(tag_end_rel) = xml[start..].find('>') {
            let tag_content = &xml[start..start + tag_end_rel];
            let tag_name = tag_content.split_whitespace().next().unwrap_or("");
            let clean_name = tag_name.split(':').last().unwrap_or(tag_name);

            if clean_name.eq_ignore_ascii_case(tag) {
                let body_start = start + tag_end_rel + 1;
                if let Some(close_rel) = xml[body_start..].find("</") {
                    let val = &xml[body_start..body_start + close_rel];
                    let unescaped = val
                        .replace("&amp;", "&")
                        .replace("&lt;", "<")
                        .replace("&gt;", ">")
                        .replace("&quot;", "\"")
                        .replace("&#39;", "'")
                        .replace("&apos;", "'");
                    return Some(unescaped.trim().to_string());
                }
            }
            search_idx = start + tag_end_rel + 1;
        } else {
            break;
        }
    }
    None
}

pub async fn handle_av_transport(
    app: &Option<AppHandle>,
    headers: &HeaderMap,
    body: &str,
) -> Response {
    let action = headers
        .get("soapaction")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    if body.contains("SetAVTransportURI") || action.contains("#SetAVTransportURI") {
        let uri = extract_xml_tag(body, "CurrentURI").unwrap_or_default();
        let meta = extract_xml_tag(body, "CurrentURIMetaData").unwrap_or_default();
        let title = extract_xml_tag(&meta, "title")
            .or_else(|| extract_xml_tag(&meta, "dc:title"))
            .unwrap_or_else(|| "DLNA Cast".to_string());
        let artist = extract_xml_tag(&meta, "artist")
            .or_else(|| extract_xml_tag(&meta, "upnp:artist"))
            .or_else(|| extract_xml_tag(&meta, "creator"))
            .or_else(|| extract_xml_tag(&meta, "dc:creator"))
            .unwrap_or_default();

        {
            let mut state = MEDIA_STATE.lock().unwrap();
            state.current_uri = uri.clone();
            state.title = title.clone();
            state.artist = artist.clone();
            state.transport_state = "STOPPED".to_string();
        }

        if let Some(ref handle) = app {
            let _ = handle.emit(
                "dlna://media",
                serde_json::json!({
                    "state": "loaded",
                    "url": uri,
                    "title": title,
                    "artist": artist
                }),
            );
        }

        return soap_response("SetAVTransportURIResponse", "");
    }

    if body.contains("Play") {
        {
            let mut state = MEDIA_STATE.lock().unwrap();
            state.transport_state = "PLAYING".to_string();
        }

        if let Some(ref handle) = app {
            let state = MEDIA_STATE.lock().unwrap().clone();
            let _ = handle.emit(
                "dlna://media",
                serde_json::json!({
                    "state": "playing",
                    "url": state.current_uri,
                    "title": state.title,
                    "artist": state.artist
                }),
            );
        }

        return soap_response("PlayResponse", "");
    }

    if body.contains("Pause") {
        {
            let mut state = MEDIA_STATE.lock().unwrap();
            state.transport_state = "PAUSED_PLAYBACK".to_string();
        }

        if let Some(ref handle) = app {
            let _ = handle.emit("dlna://media", serde_json::json!({ "state": "paused" }));
        }

        return soap_response("PauseResponse", "");
    }

    if body.contains("Stop") {
        {
            let mut state = MEDIA_STATE.lock().unwrap();
            state.transport_state = "STOPPED".to_string();
        }

        if let Some(ref handle) = app {
            let _ = handle.emit("dlna://media", serde_json::json!({ "state": "stopped" }));
        }

        return soap_response("StopResponse", "");
    }

    if body.contains("GetTransportInfo") {
        let state = MEDIA_STATE.lock().unwrap().transport_state.clone();
        let inner = format!(
            "<CurrentTransportState>{state}</CurrentTransportState>\
             <CurrentTransportStatus>OK</CurrentTransportStatus>\
             <CurrentSpeed>1</CurrentSpeed>"
        );
        return soap_response("GetTransportInfoResponse", &inner);
    }

    if body.contains("GetPositionInfo") {
        let inner = "<Track>1</Track>\
                     <TrackDuration>00:00:00</TrackDuration>\
                     <TrackMetaData></TrackMetaData>\
                     <TrackURI></TrackURI>\
                     <RelTime>00:00:00</RelTime>\
                     <AbsTime>00:00:00</AbsTime>\
                     <RelCount>0</RelCount>\
                     <AbsCount>0</AbsCount>";
        return soap_response("GetPositionInfoResponse", inner);
    }

    StatusCode::OK.into_response()
}

pub async fn handle_rendering_control(
    app: &Option<AppHandle>,
    headers: &HeaderMap,
    body: &str,
) -> Response {
    let action = headers
        .get("soapaction")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    if body.contains("SetVolume") || action.contains("#SetVolume") {
        if let Some(vol_str) = extract_xml_tag(body, "DesiredVolume") {
            if let Ok(vol) = vol_str.parse::<u32>() {
                MEDIA_STATE.lock().unwrap().volume = vol;
                if let Some(ref handle) = app {
                    let _ = handle.emit("dlna://volume", serde_json::json!({ "volume": vol }));
                }
            }
        }
        return soap_response("SetVolumeResponse", "");
    }

    if body.contains("GetVolume") {
        let vol = MEDIA_STATE.lock().unwrap().volume;
        let inner = format!("<CurrentVolume>{vol}</CurrentVolume>");
        return soap_response("GetVolumeResponse", &inner);
    }

    if body.contains("GetMute") {
        return soap_response("GetMuteResponse", "<CurrentMute>0</CurrentMute>");
    }

    if body.contains("SetMute") {
        return soap_response("SetMuteResponse", "");
    }

    StatusCode::OK.into_response()
}

pub fn handle_connection_manager() -> Response {
    soap_response(
        "GetProtocolInfoResponse",
        "<Source></Source><Sink>http-get:*:audio/mpeg:*,http-get:*:audio/mp4:*,http-get:*:audio/wav:*,http-get:*:video/mp4:*,http-get:*:video/webm:*</Sink>",
    )
}

fn soap_response(action: &str, inner: &str) -> Response {
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:{action} xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">
      {inner}
    </u:{action}>
  </s:Body>
</s:Envelope>"#
    );

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/xml; charset=\"utf-8\"")],
        xml,
    )
        .into_response()
}
