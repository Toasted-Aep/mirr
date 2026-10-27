//! Unit and integration tests for AirCast, DIAL, DLNA, and Scrcpy.

#[cfg(test)]
mod tests {
    use crate::aircast::dial;
    use crate::aircast::dlna;
    use crate::aircast::http_stream;
    use crate::aircast::loopback;
    use crate::aircast::scrcpy;

    #[test]
    fn test_cast_device_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<rust_cast::CastDevice<'static>>();
    }

    #[test]
    fn test_wav_header_format() {
        let sample_rate = 48000;
        let channels = 2;
        let header = loopback::generate_wav_header(sample_rate, channels);
        assert_eq!(&header[0..4], b"RIFF");
        assert_eq!(&header[8..12], b"WAVE");
        assert_eq!(&header[12..16], b"fmt ");

        // PCM format tag = 1
        let format_tag = u16::from_le_bytes([header[20], header[21]]);
        assert_eq!(format_tag, 1);

        // Channels = 2
        let ch = u16::from_le_bytes([header[22], header[23]]);
        assert_eq!(ch, channels);

        // Sample rate = 48000
        let sr = u32::from_le_bytes([header[24], header[25], header[26], header[27]]);
        assert_eq!(sr, sample_rate);

        // Byte rate = 48000 * 2 * 2 = 192000
        let byte_rate = u32::from_le_bytes([header[28], header[29], header[30], header[31]]);
        assert_eq!(byte_rate, 192000);

        // Block align = 2 * 2 = 4
        let align = u16::from_le_bytes([header[32], header[33]]);
        assert_eq!(align, 4);

        // Bits per sample = 16
        let bits = u16::from_le_bytes([header[34], header[35]]);
        assert_eq!(bits, 16);

        // Data chunk
        assert_eq!(&header[36..40], b"data");
        let data_size = u32::from_le_bytes([header[40], header[41], header[42], header[43]]);
        assert_eq!(data_size, 0x7FFF_0000);
    }

    #[test]
    fn test_wav_header_sample_rates() {
        for &sr in &[44100u32, 48000u32, 96000u32] {
            let header = loopback::generate_wav_header(sr, 2);
            let parsed_sr = u32::from_le_bytes([header[24], header[25], header[26], header[27]]);
            assert_eq!(parsed_sr, sr);
            let byte_rate = u32::from_le_bytes([header[28], header[29], header[30], header[31]]);
            assert_eq!(byte_rate, sr * 4);
        }
    }

    #[test]
    fn test_downmix_multi_channel_dialogue() {
        // 5.1 surround frame: L=0.0, R=0.0, Center=1.0 (pure dialogue), LFE=0.0, Ls=0.0, Rs=0.0
        let frame_5_1 = [0.0f32, 0.0f32, 1.0f32, 0.0f32, 0.0f32, 0.0f32];
        let (left, right) = loopback::downmix_float_to_i16_stereo(&frame_5_1, 6);
        // Center dialogue must be present in both channels
        assert!(left > 20000, "Left channel should contain dialogue from center");
        assert_eq!(left, right, "Center channel dialogue must be equal in left and right");

        // Mono frame: duplicate to both channels
        let frame_mono = [0.5f32];
        let (m_l, m_r) = loopback::downmix_float_to_i16_stereo(&frame_mono, 1);
        assert_eq!(m_l, m_r);
        assert_eq!(m_l, (0.5 * 32767.0) as i16);
    }

    #[test]
    fn test_stream_url_format() {
        let host = "192.168.1.100";
        let url = http_stream::stream_url(host);
        assert!(url.starts_with("http://192.168.1.100:"));
        assert!(url.ends_with("/aircast/stream.wav"));
    }

    #[test]
    fn test_dial_device_description_xml() {
        let xml = dial::device_description_xml();
        assert!(xml.contains("urn:dial-multiscreen-org:service:dial:1"));
        assert!(xml.contains("<deviceType>urn:schemas-upnp-org:device:tvdevice:1</deviceType>"));
        assert!(xml.contains("<modelName>Mirr Receiver</modelName>"));
        assert!(xml.contains("<controlURL>/apps</controlURL>"));
    }

    #[test]
    fn test_dial_app_status_stopped_and_running() {
        let stopped_xml = dial::app_status_xml("GenericApp");
        assert!(stopped_xml.contains("<name>GenericApp</name>"));
        assert!(stopped_xml.contains("<state>stopped</state>"));
    }

    #[test]
    fn test_dial_handle_launch_parsing() {
        let body = "pairingCode=ABC-123-XYZ&v=dQw4w9WgXcQ";
        let resp = dial::handle_launch(&None, "YouTube", body);
        assert_eq!(resp.status(), axum::http::StatusCode::CREATED);
        let loc = resp.headers().get("location").unwrap().to_str().unwrap();
        assert!(loc.ends_with("/apps/YouTube/run"));

        // Now app status should report running
        let running_xml = dial::app_status_xml("YouTube");
        assert!(running_xml.contains("<state>running</state>"));

        // Clean up
        dial::handle_stop(&None, "YouTube");
        let after_stop = dial::app_status_xml("YouTube");
        assert!(after_stop.contains("<state>stopped</state>"));
    }

    #[test]
    fn test_dial_param_variations() {
        let body_video_id = "video_id=test123";
        let resp1 = dial::handle_launch(&None, "Vimeo", body_video_id);
        assert_eq!(resp1.status(), axum::http::StatusCode::CREATED);
        dial::handle_stop(&None, "Vimeo");

        let body_empty = "";
        let resp2 = dial::handle_launch(&None, "Twitch", body_empty);
        assert_eq!(resp2.status(), axum::http::StatusCode::CREATED);
        dial::handle_stop(&None, "Twitch");
    }

    #[test]
    fn test_dlna_device_description_xml() {
        let xml = dlna::device_description_xml();
        assert!(xml.contains("urn:schemas-upnp-org:device:MediaRenderer:1"));
        assert!(xml.contains("urn:schemas-upnp-org:service:AVTransport:1"));
        assert!(xml.contains("urn:schemas-upnp-org:service:RenderingControl:1"));
        assert!(xml.contains("urn:schemas-upnp-org:service:ConnectionManager:1"));
    }

    #[test]
    fn test_dlna_xml_extraction() {
        let xml_with_attrs = r#"<CurrentURI id="1" xmlns="urn:upnp">http://192.168.1.50/stream.mp4</CurrentURI>"#;
        let uri = dlna::extract_xml_tag(xml_with_attrs, "CurrentURI");
        assert_eq!(uri.as_deref(), Some("http://192.168.1.50/stream.mp4"));

        let xml_with_prefix = r#"<dc:title>Test &amp; Song</dc:title>"#;
        let title = dlna::extract_xml_tag(xml_with_prefix, "title");
        assert_eq!(title.as_deref(), Some("Test & Song"));
    }

    #[test]
    fn test_ssdp_response_formatting() {
        let lan_ip = "192.168.1.200";
        let port = 7070;
        let st = "urn:schemas-upnp-org:device:MediaRenderer:1";
        let usn = "uuid:1234::urn:schemas-upnp-org:device:MediaRenderer:1";
        let resp = dial::format_ssdp_response(lan_ip, port, st, usn, "/upnp/dev.xml");

        assert!(resp.contains("HTTP/1.1 200 OK"));
        assert!(resp.contains("LOCATION: http://192.168.1.200:7070/upnp/dev.xml"));
        assert!(resp.contains("ST: urn:schemas-upnp-org:device:MediaRenderer:1"));
        assert!(resp.contains("USN: uuid:1234::urn:schemas-upnp-org:device:MediaRenderer:1"));
    }

    #[tokio::test]
    async fn test_dlna_av_transport_actions() {
        let set_uri_body = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <u:SetAVTransportURI xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">
      <CurrentURI>http://192.168.1.50:8080/sample.mp4</CurrentURI>
      <CurrentURIMetaData>&lt;DIDL-Lite&gt;&lt;dc:title&gt;Holiday Video&lt;/dc:title&gt;&lt;upnp:artist&gt;John Doe&lt;/upnp:artist&gt;&lt;/DIDL-Lite&gt;</CurrentURIMetaData>
    </u:SetAVTransportURI>
  </s:Body>
</s:Envelope>"#;

        let headers = axum::http::HeaderMap::new();
        let resp = dlna::handle_av_transport(&None, &headers, set_uri_body).await;
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        // Play action
        let play_body = "<u:Play xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"/>";
        let resp_play = dlna::handle_av_transport(&None, &headers, play_body).await;
        assert_eq!(resp_play.status(), axum::http::StatusCode::OK);

        // Transport Info should report PLAYING
        let info_body = "<u:GetTransportInfo/>";
        let resp_info = dlna::handle_av_transport(&None, &headers, info_body).await;
        assert_eq!(resp_info.status(), axum::http::StatusCode::OK);

        // Stop action
        let stop_body = "<u:Stop/>";
        let resp_stop = dlna::handle_av_transport(&None, &headers, stop_body).await;
        assert_eq!(resp_stop.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn test_dlna_rendering_control_volume() {
        let headers = axum::http::HeaderMap::new();
        let set_vol_body = "<u:SetVolume><DesiredVolume>75</DesiredVolume></u:SetVolume>";
        let resp = dlna::handle_rendering_control(&None, &headers, set_vol_body).await;
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        let get_vol_body = "<u:GetVolume/>";
        let resp_get = dlna::handle_rendering_control(&None, &headers, get_vol_body).await;
        assert_eq!(resp_get.status(), axum::http::StatusCode::OK);
    }

    #[test]
    fn test_scrcpy_status_when_idle() {
        assert!(!scrcpy::is_scrcpy_running());
    }

    #[test]
    fn test_aircast_state_fields() {
        let state = crate::aircast::aircast_state();
        assert!(state.get("casting").is_some());
        assert!(state.get("loopbackRunning").is_some());
        assert!(state.get("httpPort").is_some());
        assert!(state.get("scrcpyRunning").is_some());
    }

    #[test]
    fn test_best_lan_ip_is_valid() {
        let ip = dial::best_lan_ip();
        assert!(!ip.is_empty());
        assert!(ip.parse::<std::net::IpAddr>().is_ok());
    }
}
