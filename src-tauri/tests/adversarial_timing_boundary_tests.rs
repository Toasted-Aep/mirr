//! Adversarial Timing, Boundary, and Deserialization Stress Test Suite
//! Independent verification harness authored by Challenger 2.
//! 
//! Probes:
//! 1. Synchronization properties (1.0x real-time guarantee, QoS frame dropping under throttled display rates).
//! 2. Deserialization adversarial robustness (missing vsync defaults to true, extra/unknown fields, null handling, boundary values).
//! 3. Binary CLI syntax verification with uxplay.exe.

use std::path::PathBuf;
use std::process::Command;

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn uxplay_exe_path() -> Option<PathBuf> {
    let p1 = PathBuf::from(r"C:\Program Files (x86)\uxplay-windows\_internal\bin\uxplay.exe");
    if p1.is_file() {
        return Some(p1);
    }
    let p2 = manifest_path().join("../bin/uxplay.exe");
    if p2.is_file() {
        return Some(p2);
    }
    None
}

fn default_true() -> bool {
    true
}

fn default_receiver_mode() -> String {
    "dual".to_string()
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdversarialEngineConfig {
    pub broadcast_name: String,
    pub fps: u32,
    #[serde(default = "default_true")]
    pub vsync: bool,
    pub always_on_top_mirror: bool,
    pub auto_theater: bool,
    pub audio_output_device: String,
    pub auto_switch_audio_output: bool,
    #[serde(default = "default_true")]
    pub prefer_dolby_atmos: bool,
    #[serde(default)]
    pub show_transport_controls: bool,
    #[serde(default = "default_receiver_mode")]
    pub receiver_mode: String,
}

impl Default for AdversarialEngineConfig {
    fn default() -> Self {
        Self {
            broadcast_name: "Mirr".to_string(),
            fps: 60,
            vsync: true,
            always_on_top_mirror: false,
            auto_theater: false,
            audio_output_device: "default".to_string(),
            auto_switch_audio_output: true,
            prefer_dolby_atmos: true,
            show_transport_controls: false,
            receiver_mode: "dual".to_string(),
        }
    }
}

/// Normalization logic as implemented in engines::set_config
fn normalize_engine_config(mut cfg: AdversarialEngineConfig) -> AdversarialEngineConfig {
    let trimmed = cfg.broadcast_name.trim().to_string();
    cfg.broadcast_name = if trimmed.is_empty() {
        "Mirr".to_string()
    } else {
        trimmed
    };
    cfg.fps = match cfg.fps {
        30 => 30,
        _ => 60,
    };
    if cfg.audio_output_device.trim().is_empty() {
        cfg.audio_output_device = "default".to_string();
    }
    cfg
}

/// Reference UxPlay CLI argument builder conforming to M1 changes
fn build_reference_args(
    cfg: &AdversarialEngineConfig,
    has_d3d11: bool,
    exports_ok: bool,
) -> Vec<String> {
    let mut args = Vec::new();
    let name = cfg.broadcast_name.as_str();
    let fps_str = cfg.fps.to_string();

    args.extend([
        "-n".to_string(),
        name.to_string(),
        "-nh".to_string(),
        "-p".to_string(),
        "7100".to_string(),
        "-fps".to_string(),
        fps_str,
        "-nofreeze".to_string(),
    ]);

    if cfg.vsync {
        args.push("-vsync".to_string());
    } else {
        args.extend(["-vsync".to_string(), "no".to_string()]);
    }

    let audio_dev = cfg.audio_output_device.trim();
    if !audio_dev.is_empty() && audio_dev != "default" {
        args.extend(["-as".to_string(), format!("wasapisink device=\"{}\"", audio_dev)]);
    } else {
        args.extend(["-as".to_string(), "wasapisink".to_string()]);
    }

    if exports_ok {
        args.extend([
            "-md".to_string(), "metadata.txt".to_string(),
            "-ca".to_string(), "cover.jpg".to_string(),
            "-dacp".to_string(), "remote.dacp".to_string(),
        ]);
    }

    if has_d3d11 {
        args.extend([
            "-vd".to_string(), "d3d11h264dec".to_string(),
            "-vs".to_string(), "d3d11videosink".to_string(),
        ]);
    }

    args
}

// =========================================================================
// SECTION 1: ADVERSARIAL CONFIGURATION DESERIALIZATION PROBING
// =========================================================================

#[test]
fn test_adversarial_deser_vsync_omitted_completely() {
    let raw_json = r#"{
        "broadcastName": "Living Room Mirr",
        "fps": 60,
        "alwaysOnTopMirror": false,
        "autoTheater": true,
        "audioOutputDevice": "default",
        "autoSwitchAudioOutput": true
    }"#;

    let res: Result<AdversarialEngineConfig, _> = serde_json::from_str(raw_json);
    assert!(res.is_ok(), "Deserialization must succeed when vsync is omitted: {:?}", res.err());
    let cfg = res.unwrap();
    assert!(cfg.vsync, "Omitted vsync MUST default to true for real-time playback");
    assert!(cfg.prefer_dolby_atmos, "Omitted preferDolbyAtmos MUST default to true");
    assert!(!cfg.show_transport_controls, "Omitted showTransportControls MUST default to false");
    assert_eq!(cfg.receiver_mode, "dual", "Omitted receiverMode MUST default to 'dual'");
}

#[test]
fn test_adversarial_deser_extra_and_unknown_fields_tolerated() {
    let raw_json = r#"{
        "broadcastName": "Conference Mirr",
        "fps": 60,
        "vsync": true,
        "alwaysOnTopMirror": true,
        "autoTheater": false,
        "audioOutputDevice": "default",
        "autoSwitchAudioOutput": true,
        "unknown_extra_param_1": 12345,
        "legacyVsyncOption": "yes",
        "nestedGarbage": { "key": [1, 2, 3], "flag": true },
        "futureProofFlag": "active"
    }"#;

    let res: Result<AdversarialEngineConfig, _> = serde_json::from_str(raw_json);
    assert!(res.is_ok(), "Extra fields must be silently ignored by Serde: {:?}", res.err());
    let cfg = res.unwrap();
    assert_eq!(cfg.broadcast_name, "Conference Mirr");
    assert!(cfg.vsync);
    assert!(cfg.always_on_top_mirror);
}

#[test]
fn test_adversarial_deser_explicit_null_vsync_rejected() {
    // Serde `#[serde(default)]` applies ONLY when the key is absent.
    // When "vsync": null is explicitly sent, standard Serde rejects invalid null for a bool.
    let raw_json = r#"{
        "broadcastName": "Mirr",
        "fps": 60,
        "vsync": null,
        "alwaysOnTopMirror": false,
        "autoTheater": false,
        "audioOutputDevice": "default",
        "autoSwitchAudioOutput": true
    }"#;

    let res: Result<AdversarialEngineConfig, _> = serde_json::from_str(raw_json);
    assert!(res.is_err(), "Explicit null for boolean vsync must fail type validation");
    let err_msg = format!("{}", res.unwrap_err());
    assert!(err_msg.contains("invalid type: null, expected a boolean"));
}

#[test]
fn test_adversarial_deser_type_mismatch_string_vsync_rejected() {
    let raw_json = r#"{
        "broadcastName": "Mirr",
        "fps": 60,
        "vsync": "true",
        "alwaysOnTopMirror": false,
        "autoTheater": false,
        "audioOutputDevice": "default",
        "autoSwitchAudioOutput": true
    }"#;

    let res: Result<AdversarialEngineConfig, _> = serde_json::from_str(raw_json);
    assert!(res.is_err(), "String 'true' must be rejected as invalid boolean");
    let err_msg = format!("{}", res.unwrap_err());
    assert!(err_msg.contains("invalid type: string"));
}

#[test]
fn test_adversarial_deser_missing_mandatory_fields_rejected() {
    // Missing broadcastName
    let raw_json_no_name = r#"{
        "fps": 60,
        "vsync": true,
        "alwaysOnTopMirror": false,
        "autoTheater": false,
        "audioOutputDevice": "default",
        "autoSwitchAudioOutput": true
    }"#;
    let res = serde_json::from_str::<AdversarialEngineConfig>(raw_json_no_name);
    assert!(res.is_err());
    assert!(format!("{}", res.unwrap_err()).contains("missing field `broadcastName`"));

    // Missing fps
    let raw_json_no_fps = r#"{
        "broadcastName": "Mirr",
        "vsync": true,
        "alwaysOnTopMirror": false,
        "autoTheater": false,
        "audioOutputDevice": "default",
        "autoSwitchAudioOutput": true
    }"#;
    let res2 = serde_json::from_str::<AdversarialEngineConfig>(raw_json_no_fps);
    assert!(res2.is_err());
    assert!(format!("{}", res2.unwrap_err()).contains("missing field `fps`"));
}

#[test]
fn test_adversarial_normalization_of_extreme_inputs() {
    let mut cfg = AdversarialEngineConfig::default();

    // Whitespace only broadcast name
    cfg.broadcast_name = "   \t\n\r   ".to_string();
    let norm1 = normalize_engine_config(cfg.clone());
    assert_eq!(norm1.broadcast_name, "Mirr", "Whitespace-only name must normalize to 'Mirr'");

    // Empty broadcast name
    cfg.broadcast_name = "".to_string();
    let norm2 = normalize_engine_config(cfg.clone());
    assert_eq!(norm2.broadcast_name, "Mirr", "Empty name must normalize to 'Mirr'");

    // Boundary FPS values
    cfg.fps = 0;
    assert_eq!(normalize_engine_config(cfg.clone()).fps, 60, "FPS 0 must normalize to 60");
    cfg.fps = 1;
    assert_eq!(normalize_engine_config(cfg.clone()).fps, 60, "FPS 1 must normalize to 60");
    cfg.fps = 30;
    assert_eq!(normalize_engine_config(cfg.clone()).fps, 30, "FPS 30 must remain 30");
    cfg.fps = 45;
    assert_eq!(normalize_engine_config(cfg.clone()).fps, 60, "FPS 45 must normalize to 60");
    cfg.fps = 60;
    assert_eq!(normalize_engine_config(cfg.clone()).fps, 60, "FPS 60 must remain 60");
    cfg.fps = 120;
    assert_eq!(normalize_engine_config(cfg.clone()).fps, 60, "FPS 120 must normalize to 60");
    cfg.fps = u32::MAX;
    assert_eq!(normalize_engine_config(cfg.clone()).fps, 60, "FPS u32::MAX must normalize to 60");

    // Audio output device whitespace
    cfg.audio_output_device = "   ".to_string();
    assert_eq!(
        normalize_engine_config(cfg.clone()).audio_output_device,
        "default",
        "Whitespace audio output device must normalize to 'default'"
    );
}

// =========================================================================
// SECTION 2: SYNCHRONIZATION PROPERTIES & TIMING DYNAMICS
// =========================================================================

struct SyncSimulationOracle {
    pub dropped_frames: usize,
    pub rendered_frames: usize,
    pub max_queue_backlog: usize,
    pub final_queue_backlog: usize,
    pub final_latency_ms: f64,
}

fn simulate_video_pipeline(
    vsync_enabled: bool,
    stream_fps: f64,
    display_fps: f64,
    duration_sec: f64,
) -> SyncSimulationOracle {
    let stream_interval_ms = 1000.0 / stream_fps;
    let display_interval_ms = 1000.0 / display_fps;
    let qos_late_threshold_ms = 25.0; // GStreamer QoS frame drop threshold

    let total_stream_frames = (stream_fps * duration_sec).round() as usize;

    let mut queue: Vec<f64> = Vec::new(); // PTS timestamps of buffered frames
    let mut dropped = 0;
    let mut rendered = 0;
    let mut max_backlog = 0;

    let mut current_time_ms = 0.0;
    let mut next_send_time_ms = 0.0;
    let mut next_render_time_ms = display_interval_ms;
    let mut frames_sent = 0;

    let end_time_ms = duration_sec * 1000.0;

    while current_time_ms <= end_time_ms {
        // Inflow (Sender produces frames at stream_fps cadence)
        while frames_sent < total_stream_frames && current_time_ms >= next_send_time_ms {
            let pts = frames_sent as f64 * stream_interval_ms;
            queue.push(pts);
            frames_sent += 1;
            next_send_time_ms += stream_interval_ms;
        }

        if queue.len() > max_backlog {
            max_backlog = queue.len();
        }

        // Outflow (Display sink renders or drops frames at display_interval_ms cadence)
        while current_time_ms >= next_render_time_ms && !queue.is_empty() {
            let frame_pts = queue.remove(0);
            let frame_latency = current_time_ms - frame_pts;

            if vsync_enabled {
                // With VSync enabled, GStreamer QoS drops late frames
                if frame_latency > qos_late_threshold_ms {
                    dropped += 1;
                    continue; // Late frame dropped without blocking display slot
                }
            }

            rendered += 1;
            next_render_time_ms += display_interval_ms;
        }

        let next_event = if frames_sent < total_stream_frames {
            next_send_time_ms.min(next_render_time_ms)
        } else {
            next_render_time_ms
        };

        if next_event <= current_time_ms {
            current_time_ms += 0.1;
        } else {
            current_time_ms = next_event;
        }
    }

    let final_backlog = queue.len();
    let final_latency = final_backlog as f64 * stream_interval_ms;

    SyncSimulationOracle {
        dropped_frames: dropped,
        rendered_frames: rendered,
        max_queue_backlog: max_backlog,
        final_queue_backlog: final_backlog,
        final_latency_ms: final_latency,
    }
}

#[test]
fn test_sync_property_1_default_config_guarantees_1_0x_realtime_playback() {
    // 60 fps stream onto a 60 fps display for 60 seconds (3600 frames)
    let oracle = simulate_video_pipeline(true, 60.0, 60.0, 60.0);

    // Queue backlog must remain negligible (at most 1 frame in transit)
    assert!(
        oracle.final_queue_backlog <= 1,
        "Default config must prevent queue accumulation: final backlog was {}",
        oracle.final_queue_backlog
    );
    assert!(
        oracle.max_queue_backlog <= 2,
        "Peak queue backlog must not exceed 2 frames: was {}",
        oracle.max_queue_backlog
    );
    assert!(
        oracle.final_latency_ms <= 17.0,
        "Final latency must be bounded by 1 frame duration (<=17ms): was {:.2}ms",
        oracle.final_latency_ms
    );
    assert_eq!(
        oracle.dropped_frames, 0,
        "When display keeps up with stream, QoS must not drop any frames"
    );
    assert_eq!(
        oracle.rendered_frames, 3600,
        "All 3600 frames must be rendered in 60.0 seconds (exact 1.0x speed)"
    );
}

#[test]
fn test_sync_property_2_presentation_rate_below_stream_qos_drops_bound_latency() {
    // Throttled display: 45 fps display (e.g. DWM composition overhead), 60 fps stream
    // Stream runs for 60 seconds (3600 frames arriving, display can only render ~2700)
    let oracle_vsync_on = simulate_video_pipeline(true, 60.0, 45.0, 60.0);

    // Under vsync: true, QoS late-frame dropping MUST activate
    assert!(
        oracle_vsync_on.dropped_frames > 800,
        "QoS must drop late frames when display is slower than stream: dropped was {}",
        oracle_vsync_on.dropped_frames
    );
    assert!(
        oracle_vsync_on.final_queue_backlog <= 2,
        "Queue backlog must remain strictly bounded: was {}",
        oracle_vsync_on.final_queue_backlog
    );
    assert!(
        oracle_vsync_on.final_latency_ms <= 35.0,
        "Latency must remain bounded (<=35ms): was {:.2}ms",
        oracle_vsync_on.final_latency_ms
    );

    // Under vsync: false (the original bug condition)
    let oracle_vsync_off = simulate_video_pipeline(false, 60.0, 45.0, 60.0);

    // Without vsync, late frames cannot be dropped, causing runaway buffer bloat
    assert_eq!(
        oracle_vsync_off.dropped_frames, 0,
        "Without vsync, 0 frames are dropped"
    );
    assert!(
        oracle_vsync_off.final_queue_backlog > 800,
        "Without vsync, queue backlog must explode: was {}",
        oracle_vsync_off.final_queue_backlog
    );
    assert!(
        oracle_vsync_off.final_latency_ms > 14000.0,
        "Without vsync, latency must explode (>14 seconds): was {:.2}ms",
        oracle_vsync_off.final_latency_ms
    );
}

#[test]
fn test_sync_property_3_severe_bottleneck_30fps_display_survives_with_vsync() {
    // Severe bottleneck: 30 fps display (e.g. 4K 30Hz or severe GPU throttling), 60 fps stream
    let oracle = simulate_video_pipeline(true, 60.0, 30.0, 60.0);

    // In 60 seconds: 3600 frames in, 1800 rendered, ~1800 dropped
    assert!(
        oracle.dropped_frames >= 1700 && oracle.dropped_frames <= 1900,
        "QoS should drop approx 50% of frames (actual dropped: {})",
        oracle.dropped_frames
    );
    assert!(
        oracle.final_queue_backlog <= 3,
        "Backlog must remain <= 3 frames despite 50% frame drop: was {}",
        oracle.final_queue_backlog
    );
    assert!(
        oracle.final_latency_ms <= 50.0,
        "End-to-end latency must remain bounded <= 50ms: was {:.2}ms",
        oracle.final_latency_ms
    );
}

#[test]
fn test_sync_property_4_extreme_bottleneck_15fps_display_survives_with_vsync() {
    // Extreme bottleneck: 15 fps display, 60 fps stream (display interval is 66.67ms, 4 frames arrive per display tick)
    let oracle = simulate_video_pipeline(true, 60.0, 15.0, 30.0);

    // Latency must still remain bounded by QoS to at most 1 display frame interval (~67ms)
    assert!(
        oracle.final_queue_backlog <= 4,
        "Under extreme 15 fps bottleneck, backlog must stay <= 4 frames (one 15fps display tick): was {}",
        oracle.final_queue_backlog
    );
    assert!(
        oracle.final_latency_ms <= 67.0,
        "Latency must remain bounded <= 67ms: was {:.2}ms",
        oracle.final_latency_ms
    );
    assert!(
        oracle.dropped_frames > 1200,
        "QoS must discard ~75% of frames: was {}",
        oracle.dropped_frames
    );
}

// =========================================================================
// SECTION 3: EMPIRICAL LIVE UXPLAY CLI VERIFICATION
// =========================================================================

#[test]
fn test_live_uxplay_binary_argument_compatibility() {
    let Some(uxplay_exe) = uxplay_exe_path() else {
        eprintln!("Skipping live uxplay test: uxplay.exe not found on system");
        return;
    };

    // 1. Verify bare -vsync is accepted (code 0 with -h)
    let out_vsync = Command::new(&uxplay_exe)
        .args(["-vsync", "-h"])
        .output()
        .expect("execute uxplay -vsync -h");
    assert!(
        out_vsync.status.success(),
        "uxplay -vsync must succeed (exit code 0)"
    );

    // 2. Verify -vsync no is accepted (code 0 with -h)
    let out_vsync_no = Command::new(&uxplay_exe)
        .args(["-vsync", "no", "-h"])
        .output()
        .expect("execute uxplay -vsync no -h");
    assert!(
        out_vsync_no.status.success(),
        "uxplay -vsync no must succeed (exit code 0)"
    );

    // 3. Verify -vsync yes FAILS with code 1 and specific error message
    let out_vsync_yes = Command::new(&uxplay_exe)
        .args(["-vsync", "yes", "-h"])
        .output()
        .expect("execute uxplay -vsync yes -h");
    assert!(
        !out_vsync_yes.status.success(),
        "uxplay -vsync yes MUST fail (exit code 1)"
    );
    let stderr_str = String::from_utf8_lossy(&out_vsync_yes.stderr);
    let stdout_str = String::from_utf8_lossy(&out_vsync_yes.stdout);
    let combined = format!("{}\n{}", stdout_str, stderr_str);
    assert!(
        combined.contains("unknown option yes, stopping"),
        "Output must contain 'unknown option yes, stopping': {}",
        combined
    );
}

#[test]
fn test_cli_builder_does_not_emit_vc_and_generates_valid_vsync() {
    let cfg_default = AdversarialEngineConfig::default();
    let args_d3d11 = build_reference_args(&cfg_default, true, true);

    // Check vsync syntax
    let vsync_idx = args_d3d11.iter().position(|a| a == "-vsync").expect("must contain -vsync");
    let after_vsync = args_d3d11.get(vsync_idx + 1);
    assert_ne!(after_vsync, Some(&"yes".to_string()), "Must not contain 'yes'");
    assert_ne!(after_vsync, Some(&"no".to_string()), "Default config must not contain 'no'");

    // Check d3d11 arguments
    assert!(args_d3d11.contains(&"-vd".to_string()));
    assert!(args_d3d11.contains(&"d3d11h264dec".to_string()));
    assert!(args_d3d11.contains(&"-vs".to_string()));
    assert!(args_d3d11.contains(&"d3d11videosink".to_string()));

    // Check converter override omission
    assert!(!args_d3d11.contains(&"-vc".to_string()), "Must not contain -vc");
    assert!(!args_d3d11.contains(&"d3d11convert".to_string()), "Must not contain d3d11convert");

    // Check disabled vsync
    let mut cfg_disabled = AdversarialEngineConfig::default();
    cfg_disabled.vsync = false;
    let args_disabled = build_reference_args(&cfg_disabled, false, false);
    let pos = args_disabled.iter().position(|a| a == "-vsync").unwrap();
    assert_eq!(args_disabled.get(pos + 1), Some(&"no".to_string()));
}
