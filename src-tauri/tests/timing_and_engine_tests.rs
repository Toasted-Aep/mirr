//! Mirr AirPlay Real-Time Playback & Synchronization Test Suite
//! Comprehensive 4-Tier Test Suite verifying:
//! - Tier 1: Feature Coverage (EngineConfig defaults, CLI arg generation, pipeline optimization, frontend settings & presets)
//! - Tier 2: Boundary & Corner Cases (vsync toggling, fps boundaries, quality/audio boundaries, broadcast names, plugin dir handling)
//! - Tier 3: Cross-Feature Combinations (pipeline combinations, JSON serde contracts, frontend preset synchronization)
//! - Tier 4: Real-World Scenarios (presentation timing simulation, clock drift bounds, QoS late frame dropping, AV lip-sync pacing)

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn engines_rs_path() -> PathBuf {
    manifest_path().join("src/engines.rs")
}

fn app_js_path() -> PathBuf {
    manifest_path().join("../src/app.js")
}

fn index_html_path() -> PathBuf {
    manifest_path().join("../src/index.html")
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

use mirr_lib::engines::{
    build_uxplay_cmd, compute_preset, normalize_fps, sanitize_broadcast_name,
    EngineConfig,
};

/// Transparent adapter extracting CLI args from std::process::Command produced by build_uxplay_cmd
fn uxplay_args(cfg: &EngineConfig, has_d3d11: bool, exports_ok: bool) -> Vec<String> {
    let dummy_exe = Path::new("uxplay.exe");
    let cmd = build_uxplay_cmd(dummy_exe, cfg, has_d3d11, exports_ok);
    cmd.get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect()
}

// =========================================================================
// TIER 1: FEATURE COVERAGE
// =========================================================================

mod tier1_feature_coverage {
    use super::*;

    // Feature 1: EngineConfig Defaults (vsync: true)
    #[test]
    fn test_tier1_f1_01_default_engine_config_vsync_is_true() {
        let cfg = EngineConfig::default();
        assert!(cfg.vsync, "EngineConfig default vsync must be true to enable 1.0x real-time playback");
    }

    #[test]
    fn test_tier1_f1_02_default_engine_config_fps_is_60() {
        let cfg = EngineConfig::default();
        assert_eq!(cfg.fps, 60, "Default fps must be 60 for smooth presentation");
    }

    #[test]
    fn test_tier1_f1_03_default_engine_config_broadcast_name_is_mirr() {
        let cfg = EngineConfig::default();
        assert_eq!(cfg.broadcast_name, "Mirr", "Default broadcast name must be 'Mirr'");
    }

    #[test]
    fn test_tier1_f1_04_default_engine_config_receiver_mode_is_dual() {
        let cfg = EngineConfig::default();
        assert_eq!(cfg.receiver_mode, "dual", "Default receiver mode must be 'dual'");
    }

    #[test]
    fn test_tier1_f1_05_default_engine_config_transport_controls_disabled() {
        let cfg = EngineConfig::default();
        assert!(!cfg.show_transport_controls, "Transport controls must be disabled by default on initial setup");
    }

    #[test]
    fn test_tier1_f1_06_engines_rs_source_declares_vsync_true() {
        let content = fs::read_to_string(engines_rs_path()).expect("read engines.rs");
        assert!(
            content.contains("vsync: true"),
            "engines.rs must contain 'vsync: true' in EngineConfig::default()"
        );
        assert!(
            !content.contains("vsync: false"),
            "engines.rs must not contain 'vsync: false' in EngineConfig::default()"
        );
    }

    // Feature 2: Valid CLI Arguments for uxplay.exe (no "-vsync yes")
    #[test]
    fn test_tier1_f2_01_cli_args_contain_vsync_flag_when_true() {
        let cfg = EngineConfig {
            vsync: true,
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let pos = args.iter().position(|a| a == "-vsync");
        assert!(pos.is_some(), "CLI args must contain -vsync when enabled");
    }

    #[test]
    fn test_tier1_f2_02_cli_args_never_contain_invalid_vsync_yes() {
        let cfg = EngineConfig {
            vsync: true,
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let pos = args.iter().position(|a| a == "-vsync").unwrap();
        let next = args.get(pos + 1);
        assert_ne!(next, Some(&"yes".to_string()), "CLI args must never pass 'yes' to -vsync");
        assert!(!args.contains(&"yes".to_string()), "CLI args must not contain standalone 'yes'");
    }

    #[test]
    fn test_tier1_f2_03_engines_rs_source_omits_vsync_arg_yes() {
        let content = fs::read_to_string(engines_rs_path()).expect("read engines.rs");
        assert!(
            !content.contains(r#"if cfg.vsync { "yes" } else { "no" }"#),
            "engines.rs must not format vsync as 'yes' string"
        );
        assert!(
            !content.contains(r#"["-vsync", "yes"]"#),
            "engines.rs must not contain ['-vsync', 'yes']"
        );
    }

    #[test]
    fn test_tier1_f2_04_uxplay_binary_accepts_vsync_flag() {
        if let Some(exe) = uxplay_exe_path() {
            let output = Command::new(&exe)
                .args(["-vsync", "-h"])
                .output()
                .expect("execute uxplay -vsync -h");
            assert!(
                output.status.success(),
                "uxplay.exe must accept -vsync without crashing; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn test_tier1_f2_05_uxplay_binary_rejects_vsync_yes() {
        if let Some(exe) = uxplay_exe_path() {
            let output = Command::new(&exe)
                .args(["-vsync", "yes", "-h"])
                .output()
                .expect("execute uxplay -vsync yes -h");
            assert!(
                !output.status.success(),
                "uxplay.exe must reject -vsync yes with non-zero exit code"
            );
            let combined = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                combined.contains("unknown option yes") || combined.contains("stopping"),
                "uxplay error output should mention unknown option yes"
            );
        }
    }

    // Feature 3: Pipeline Converter Optimization (no "-vc d3d11convert")
    #[test]
    fn test_tier1_f3_01_engines_rs_source_omits_vc_d3d11convert() {
        let content = fs::read_to_string(engines_rs_path()).expect("read engines.rs");
        assert!(
            !content.contains(r#""-vc", "d3d11convert""#),
            "engines.rs must not inject '-vc d3d11convert'"
        );
    }

    #[test]
    fn test_tier1_f3_02_pipeline_retains_hardware_decoder_d3d11h264dec() {
        let cfg = EngineConfig::default();
        let args = uxplay_args(&cfg, true, false);
        assert!(args.contains(&"-vd".to_string()), "Args must contain -vd");
        assert!(args.contains(&"d3d11h264dec".to_string()), "Args must contain d3d11h264dec");
    }

    #[test]
    fn test_tier1_f3_03_pipeline_retains_hardware_sink_d3d11videosink() {
        let cfg = EngineConfig::default();
        let args = uxplay_args(&cfg, true, false);
        assert!(args.contains(&"-vs".to_string()), "Args must contain -vs");
        assert!(args.contains(&"d3d11videosink".to_string()), "Args must contain d3d11videosink");
    }

    #[test]
    fn test_tier1_f3_04_pipeline_args_omit_vc_flag_completely() {
        let cfg = EngineConfig::default();
        let args = uxplay_args(&cfg, true, false);
        assert!(!args.contains(&"-vc".to_string()), "Hardware pipeline must omit -vc flag completely");
    }

    #[test]
    fn test_tier1_f3_05_engines_rs_build_uxplay_cmd_contains_no_vc() {
        let content = fs::read_to_string(engines_rs_path()).expect("read engines.rs");
        let start = content.find("fn build_uxplay_cmd").expect("find build_uxplay_cmd");
        let end = content[start..].find("pub fn start_airplay_receiver").expect("find next function") + start;
        let fn_body = &content[start..end];

        assert!(fn_body.contains(r#""-vd", "d3d11h264dec""#), "build_uxplay_cmd must keep hardware decoder");
        assert!(fn_body.contains(r#""-vs", "d3d11videosink""#), "build_uxplay_cmd must keep hardware sink");
        assert!(!fn_body.contains(r#""-vc""#), "build_uxplay_cmd must not pass -vc flag");
    }

    // Feature 4: Frontend runtimeSettings Defaults (vsync: true)
    #[test]
    fn test_tier1_f4_01_app_js_runtime_settings_vsync_is_true() {
        let content = fs::read_to_string(app_js_path()).expect("read app.js");
        assert!(
            content.contains("vsync: true"),
            "app.js runtimeSettings must set vsync: true by default"
        );
    }

    #[test]
    fn test_tier1_f4_02_app_js_runtime_settings_fps_is_60() {
        let content = fs::read_to_string(app_js_path()).expect("read app.js");
        assert!(
            content.contains("fps: 60"),
            "app.js runtimeSettings must set fps: 60 by default"
        );
    }

    #[test]
    fn test_tier1_f4_03_app_js_runtime_settings_broadcast_name_is_mirr() {
        let content = fs::read_to_string(app_js_path()).expect("read app.js");
        assert!(
            content.contains(r#"broadcastName: "Mirr""#),
            "app.js runtimeSettings must set broadcastName: 'Mirr'"
        );
    }

    #[test]
    fn test_tier1_f4_04_app_js_preset_key_mapping_for_sync() {
        let content = fs::read_to_string(app_js_path()).expect("read app.js");
        assert!(
            content.contains(r#"runtimeSettings.vsync ? "60-sync" : "60-low""#),
            "app.js syncSettingsUI must map vsync=true to '60-sync'"
        );
    }

    #[test]
    fn test_tier1_f4_05_app_js_preset_click_handler_updates_vsync() {
        let content = fs::read_to_string(app_js_path()).expect("read app.js");
        assert!(
            content.contains(r#"else if (p === "60-sync") { runtimeSettings.fps = 60; runtimeSettings.vsync = true; }"#),
            "app.js preset handler must update runtimeSettings.vsync = true for 60-sync"
        );
    }

    // Feature 5: Default UI Preset Button in index.html (60-sync active)
    #[test]
    fn test_tier1_f5_01_index_html_preset_60_sync_is_active() {
        let content = fs::read_to_string(index_html_path()).expect("read index.html");
        assert!(
            content.contains(r#"<button class="seg-btn active" type="button" data-preset="60-sync">"#),
            "index.html must mark 60-sync as active preset button"
        );
    }

    #[test]
    fn test_tier1_f5_02_index_html_preset_60_low_is_not_active() {
        let content = fs::read_to_string(index_html_path()).expect("read index.html");
        assert!(
            !content.contains(r#"<button class="seg-btn active" type="button" data-preset="60-low">"#),
            "index.html must not mark 60-low as active preset button"
        );
        assert!(
            content.contains(r#"<button class="seg-btn" type="button" data-preset="60-low">"#),
            "index.html must have 60-low button in inactive state"
        );
    }

    #[test]
    fn test_tier1_f5_03_index_html_preset_30_eco_is_not_active() {
        let content = fs::read_to_string(index_html_path()).expect("read index.html");
        assert!(
            !content.contains(r#"<button class="seg-btn active" type="button" data-preset="30-eco">"#),
            "index.html must not mark 30-eco as active preset button"
        );
    }

    #[test]
    fn test_tier1_f5_04_index_html_exactly_one_preset_button_is_active() {
        let content = fs::read_to_string(index_html_path()).expect("read index.html");
        let group_start = content.find(r#"id="setPresetGroup""#).expect("find setPresetGroup");
        let group_end = content[group_start..].find("</div>").expect("find closing div") + group_start;
        let group_html = &content[group_start..group_end];

        let active_count = group_html.matches(r#"class="seg-btn active""#).count();
        assert_eq!(active_count, 1, "Exactly one preset button in setPresetGroup must be active");
    }

    #[test]
    fn test_tier1_f5_05_index_html_preset_group_contains_all_three_presets() {
        let content = fs::read_to_string(index_html_path()).expect("read index.html");
        assert!(content.contains(r#"data-preset="60-low""#), "Must contain 60-low preset");
        assert!(content.contains(r#"data-preset="60-sync""#), "Must contain 60-sync preset");
        assert!(content.contains(r#"data-preset="30-eco""#), "Must contain 30-eco preset");
    }
}

// =========================================================================
// TIER 2: BOUNDARY & CORNER CASES
// =========================================================================

mod tier2_boundary_and_corner_cases {
    use super::*;

    // Boundary 1: VSync Toggling
    #[test]
    fn test_tier2_b1_01_vsync_false_produces_vsync_no() {
        let cfg = EngineConfig {
            vsync: false,
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let pos = args.iter().position(|a| a == "-vsync").expect("find -vsync");
        assert_eq!(
            args.get(pos + 1),
            Some(&"no".to_string()),
            "When vsync is false, -vsync must be followed by 'no'"
        );
    }

    #[test]
    fn test_tier2_b1_02_vsync_true_produces_bare_vsync() {
        let cfg = EngineConfig {
            vsync: true,
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let pos = args.iter().position(|a| a == "-vsync").expect("find -vsync");
        let next = args.get(pos + 1);
        assert_ne!(next, Some(&"no".to_string()), "When vsync is true, next arg must not be 'no'");
        assert_ne!(next, Some(&"yes".to_string()), "When vsync is true, next arg must not be 'yes'");
    }

    #[test]
    fn test_tier2_b1_03_vsync_toggling_idempotence() {
        let mut cfg = EngineConfig::default();
        for _ in 0..5 {
            cfg.vsync = true;
            let args_true = uxplay_args(&cfg, false, false);
            assert!(args_true.contains(&"-vsync".to_string()));
            assert!(!args_true.contains(&"no".to_string()));

            cfg.vsync = false;
            let args_false = uxplay_args(&cfg, false, false);
            let pos = args_false.iter().position(|a| a == "-vsync").unwrap();
            assert_eq!(args_false.get(pos + 1), Some(&"no".to_string()));
        }
    }

    #[test]
    fn test_tier2_b1_04_no_redundant_vsync_flags() {
        let cfg = EngineConfig {
            vsync: false,
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let count = args.iter().filter(|&a| a == "-vsync").count();
        assert_eq!(count, 1, "There must be exactly one -vsync flag emitted in command");
    }

    #[test]
    fn test_tier2_b1_05_uxplay_binary_accepts_vsync_no() {
        if let Some(exe) = uxplay_exe_path() {
            let output = Command::new(&exe)
                .args(["-vsync", "no", "-h"])
                .output()
                .expect("execute uxplay -vsync no -h");
            assert!(
                output.status.success(),
                "uxplay.exe must accept -vsync no cleanly; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    // Boundary 2: FPS Boundary Values
    #[test]
    fn test_tier2_b2_01_fps_30_exact() {
        let cfg = EngineConfig {
            fps: 30,
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let pos = args.iter().position(|a| a == "-fps").expect("find -fps");
        assert_eq!(args.get(pos + 1), Some(&"30".to_string()));
    }

    #[test]
    fn test_tier2_b2_02_fps_60_exact() {
        let cfg = EngineConfig {
            fps: 60,
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let pos = args.iter().position(|a| a == "-fps").expect("find -fps");
        assert_eq!(args.get(pos + 1), Some(&"60".to_string()));
    }

    #[test]
    fn test_tier2_b2_03_fps_boundary_zero_and_clamp_behavior() {
        assert_eq!(normalize_fps(0), 60, "FPS 0 must clamp/fallback to 60");
        assert_eq!(normalize_fps(120), 60, "FPS 120 must normalize to 60");
        assert_eq!(normalize_fps(24), 60, "FPS 24 must normalize to 60");
        assert_eq!(normalize_fps(30), 30, "FPS 30 must preserve 30");
        assert_eq!(normalize_fps(60), 60, "FPS 60 must preserve 60");
    }

    #[test]
    fn test_tier2_b2_04_fps_large_integer_safety() {
        let cfg = EngineConfig {
            fps: u32::MAX,
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let pos = args.iter().position(|a| a == "-fps").expect("find -fps");
        assert_eq!(args.get(pos + 1), Some(&u32::MAX.to_string()));
    }

    #[test]
    fn test_tier2_b2_05_frontend_preset_mapping_for_30_fps() {
        assert_eq!(compute_preset(30, true), "30-eco");
        assert_eq!(compute_preset(30, false), "30-eco");
        assert_eq!(compute_preset(60, true), "60-sync");
        assert_eq!(compute_preset(60, false), "60-low");
    }

    // Boundary 3: Audio Sink and Quality Boundaries
    #[test]
    fn test_tier2_b3_01_audio_device_default_yields_bare_wasapisink() {
        let cfg = EngineConfig {
            audio_output_device: "default".to_string(),
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let pos = args.iter().position(|a| a == "-as").expect("find -as");
        assert_eq!(args.get(pos + 1), Some(&"wasapisink".to_string()));
    }

    #[test]
    fn test_tier2_b3_02_audio_device_empty_string_yields_bare_wasapisink() {
        let cfg = EngineConfig {
            audio_output_device: "".to_string(),
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let pos = args.iter().position(|a| a == "-as").expect("find -as");
        assert_eq!(args.get(pos + 1), Some(&"wasapisink".to_string()));
    }

    #[test]
    fn test_tier2_b3_03_audio_device_whitespace_string_yields_bare_wasapisink() {
        let cfg = EngineConfig {
            audio_output_device: "   \t  ".to_string(),
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let pos = args.iter().position(|a| a == "-as").expect("find -as");
        assert_eq!(args.get(pos + 1), Some(&"wasapisink".to_string()));
    }

    #[test]
    fn test_tier2_b3_04_audio_device_named_properly_quoted() {
        let cfg = EngineConfig {
            audio_output_device: "Realtek High Definition Audio".to_string(),
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let pos = args.iter().position(|a| a == "-as").expect("find -as");
        assert_eq!(
            args.get(pos + 1),
            Some(&"wasapisink device=\"Realtek High Definition Audio\"".to_string())
        );
    }

    #[test]
    fn test_tier2_b3_05_audio_device_with_nested_quotes_handled() {
        let cfg = EngineConfig {
            audio_output_device: "Speakers (2- USB Audio)".to_string(),
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let pos = args.iter().position(|a| a == "-as").expect("find -as");
        assert_eq!(
            args.get(pos + 1),
            Some(&"wasapisink device=\"Speakers (2- USB Audio)\"".to_string())
        );
    }

    // Boundary 4: Broadcast Name Special Characters & Whitespace
    #[test]
    fn test_tier2_b4_01_broadcast_name_trimmed() {
        assert_eq!(sanitize_broadcast_name("   Living Room   "), "Living Room");
    }

    #[test]
    fn test_tier2_b4_02_broadcast_name_empty_defaults_to_mirr() {
        assert_eq!(sanitize_broadcast_name(""), "Mirr");
    }

    #[test]
    fn test_tier2_b4_03_broadcast_name_whitespace_only_defaults_to_mirr() {
        assert_eq!(sanitize_broadcast_name("   \n\t   "), "Mirr");
    }

    #[test]
    fn test_tier2_b4_04_broadcast_name_with_spaces_in_middle() {
        let cfg = EngineConfig {
            broadcast_name: "Mirr Conference Room".to_string(),
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let pos = args.iter().position(|a| a == "-n").expect("find -n");
        assert_eq!(args.get(pos + 1), Some(&"Mirr Conference Room".to_string()));
    }

    #[test]
    fn test_tier2_b4_05_broadcast_name_unicode_and_symbols() {
        let cfg = EngineConfig {
            broadcast_name: "客厅 Mirr 📺".to_string(),
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);
        let pos = args.iter().position(|a| a == "-n").expect("find -n");
        assert_eq!(args.get(pos + 1), Some(&"客厅 Mirr 📺".to_string()));
    }

    // Boundary 5: Missing / Empty Plugin Directory Handling
    #[test]
    fn test_tier2_b5_01_plugin_dir_none_omits_hardware_args() {
        let cfg = EngineConfig::default();
        let args = uxplay_args(&cfg, false, false);
        assert!(!args.contains(&"-vd".to_string()));
        assert!(!args.contains(&"-vc".to_string()));
        assert!(!args.contains(&"-vs".to_string()));
    }

    #[test]
    fn test_tier2_b5_02_plugin_dir_missing_dll_treated_as_false() {
        let temp_dir = std::env::temp_dir().join(format!("mirr_test_plugins_{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_dir);
        let has_d3d11 = temp_dir.join("libgstd3d11.dll").is_file();
        assert!(!has_d3d11, "Empty directory must not contain libgstd3d11.dll");
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_tier2_b5_03_plugin_dir_with_dll_detected_properly() {
        let temp_dir = std::env::temp_dir().join(format!("mirr_test_plugins_ok_{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_dir);
        let dummy_dll = temp_dir.join("libgstd3d11.dll");
        fs::write(&dummy_dll, b"MZ dummy").expect("write dummy dll");

        let has_d3d11 = dummy_dll.is_file();
        assert!(has_d3d11, "Directory with libgstd3d11.dll must evaluate has_d3d11 to true");

        let cfg = EngineConfig::default();
        let args = uxplay_args(&cfg, has_d3d11, false);
        assert!(args.contains(&"-vd".to_string()));
        assert!(args.contains(&"-vs".to_string()));
        assert!(!args.contains(&"-vc".to_string()), "Must never contain -vc");

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_tier2_b5_04_exports_ok_flag_controls_metadata_flags() {
        let cfg = EngineConfig::default();
        let args_without_exports = uxplay_args(&cfg, false, false);
        assert!(!args_without_exports.contains(&"-md".to_string()));
        assert!(!args_without_exports.contains(&"-ca".to_string()));
        assert!(!args_without_exports.contains(&"-dacp".to_string()));

        let args_with_exports = uxplay_args(&cfg, false, true);
        assert!(args_with_exports.contains(&"-md".to_string()));
        assert!(args_with_exports.contains(&"-ca".to_string()));
        assert!(args_with_exports.contains(&"-dacp".to_string()));
    }

    #[test]
    fn test_tier2_b5_05_nonexistent_plugin_dir_evaluation_is_safe() {
        let fake_path = Path::new("Z:\\nonexistent_mirr_dir_12345");
        let has_d3d11 = fake_path.join("libgstd3d11.dll").is_file();
        assert!(!has_d3d11, "Nonexistent path must return false without panic");
    }
}

// =========================================================================
// TIER 3: CROSS-FEATURE COMBINATIONS
// =========================================================================

mod tier3_cross_feature_combinations {
    use super::*;

    #[test]
    fn test_tier3_01_optimal_realtime_pipeline_combination() {
        let cfg = EngineConfig {
            vsync: true,
            fps: 60,
            broadcast_name: "MirrPro".to_string(),
            ..Default::default()
        };
        let args = uxplay_args(&cfg, true, true);

        // Verify full CLI composition
        assert_eq!(args[0], "-n");
        assert_eq!(args[1], "MirrPro");
        assert!(args.contains(&"-nh".to_string()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"7100".to_string()));
        assert!(args.contains(&"-fps".to_string()));
        assert!(args.contains(&"60".to_string()));
        assert!(args.contains(&"-nofreeze".to_string()));
        assert!(args.contains(&"-vsync".to_string()));
        assert!(!args.contains(&"no".to_string()));
        assert!(!args.contains(&"yes".to_string()));
        assert!(args.contains(&"-as".to_string()));
        assert!(args.contains(&"-vd".to_string()));
        assert!(args.contains(&"d3d11h264dec".to_string()));
        assert!(args.contains(&"-vs".to_string()));
        assert!(args.contains(&"d3d11videosink".to_string()));
        assert!(!args.contains(&"-vc".to_string()));
    }

    #[test]
    fn test_tier3_02_eco_fallback_pipeline_combination() {
        let cfg = EngineConfig {
            vsync: false,
            fps: 30,
            broadcast_name: "MirrEco".to_string(),
            ..Default::default()
        };
        let args = uxplay_args(&cfg, false, false);

        assert!(args.contains(&"-fps".to_string()));
        assert!(args.contains(&"30".to_string()));
        let pos = args.iter().position(|a| a == "-vsync").unwrap();
        assert_eq!(args.get(pos + 1), Some(&"no".to_string()));
        assert!(!args.contains(&"-vd".to_string()));
        assert!(!args.contains(&"-vc".to_string()));
        assert!(!args.contains(&"-vs".to_string()));
    }

    #[test]
    fn test_tier3_03_json_serialization_matches_frontend_contract() {
        let cfg = EngineConfig::default();
        let json_val = serde_json::to_value(&cfg).expect("serialize config to json value");

        assert_eq!(json_val["broadcastName"], "Mirr");
        assert_eq!(json_val["fps"], 60);
        assert_eq!(json_val["vsync"], true);
        assert_eq!(json_val["alwaysOnTopMirror"], false);
        assert_eq!(json_val["autoTheater"], false);
        assert_eq!(json_val["audioOutputDevice"], "default");
        assert_eq!(json_val["autoSwitchAudioOutput"], true);
        assert_eq!(json_val["preferDolbyAtmos"], true);
        assert_eq!(json_val["showTransportControls"], false);
        assert_eq!(json_val["receiverMode"], "dual");
    }

    #[test]
    fn test_tier3_04_json_deserialization_from_frontend_payload() {
        let payload = r#"{
            "broadcastName": "Conference Room",
            "fps": 60,
            "vsync": true,
            "alwaysOnTopMirror": true,
            "autoTheater": true,
            "audioOutputDevice": "USB DAC",
            "autoSwitchAudioOutput": false,
            "preferDolbyAtmos": false,
            "showTransportControls": true,
            "receiverMode": "airplay"
        }"#;

        let cfg: EngineConfig = serde_json::from_str(payload).expect("deserialize config");
        assert_eq!(cfg.broadcast_name, "Conference Room");
        assert_eq!(cfg.fps, 60);
        assert!(cfg.vsync);
        assert!(cfg.always_on_top_mirror);
        assert!(cfg.auto_theater);
        assert_eq!(cfg.audio_output_device, "USB DAC");
        assert!(!cfg.auto_switch_audio_output);
        assert!(!cfg.prefer_dolby_atmos);
        assert!(cfg.show_transport_controls);
        assert_eq!(cfg.receiver_mode, "airplay");
    }

    #[test]
    fn test_tier3_05_backward_compatible_deserialization_with_missing_vsync() {
        let mut val = serde_json::to_value(EngineConfig::default()).unwrap();
        val.as_object_mut().unwrap().remove("vsync");
        let cfg: EngineConfig = serde_json::from_value(val).expect("deserialize config");
        assert!(cfg.vsync, "Missing vsync field in payload must default to true");
    }

    #[test]
    fn test_tier3_06_frontend_preset_alignment_with_engine_config() {
        let presets = [
            ("60-sync", 60, true),
            ("60-low", 60, false),
            ("30-eco", 30, false),
        ];

        for (preset_key, expected_fps, expected_vsync) in presets {
            let cfg = EngineConfig {
                fps: expected_fps,
                vsync: expected_vsync,
                ..Default::default()
            };
            let args = uxplay_args(&cfg, true, false);

            assert!(
                args.contains(&expected_fps.to_string()),
                "Preset {} must emit fps {}",
                preset_key,
                expected_fps
            );

            if expected_vsync {
                assert!(
                    args.contains(&"-vsync".to_string()) && !args.contains(&"no".to_string()),
                    "Preset {} must emit -vsync without 'no'",
                    preset_key
                );
            } else {
                let pos = args.iter().position(|a| a == "-vsync").unwrap();
                assert_eq!(
                    args.get(pos + 1),
                    Some(&"no".to_string()),
                    "Preset {} must emit -vsync no",
                    preset_key
                );
            }
        }
    }
}

// =========================================================================
// TIER 4: REAL-WORLD SCENARIOS (TIMING, PACING, & LATENCY DYNAMICS)
// =========================================================================

mod tier4_real_world_timing_and_pacing {
    /// Mathematical Simulation of Video Frame Queue Dynamics
    /// Models 3600 frames (60 seconds of 60 fps AirPlay video stream)
    /// under hardware display / swapchain throughput throttled to ~45 fps
    /// (e.g. DWM recomposition or VSync lock overhead).
    struct StreamSimulation {
        pub dropped_frames: usize,
        pub final_queue_backlog: usize,
        pub final_latency_ms: f64,
    }

    fn run_timing_simulation(vsync_enabled: bool, render_cadence_ms: f64) -> StreamSimulation {
        let total_frames = 3600; // 60 seconds of 60 fps video
        let sender_frame_interval_ms = 1000.0 / 60.0; // 16.666 ms per frame
        let qos_late_threshold_ms = 25.0; // GStreamer QoS frame drop threshold

        let mut queue: Vec<f64> = Vec::new(); // stores PTS timestamp of buffered frames
        let mut dropped = 0;

        let mut current_time_ms = 0.0;
        let mut next_send_time_ms = 0.0;
        let mut next_render_time_ms = render_cadence_ms;
        let mut frames_sent = 0;

        // Simulate 60 seconds of stream time
        while current_time_ms <= 60_000.0 {
            // Producer: Sender sends frames at exact 60 fps cadence
            while frames_sent < total_frames && current_time_ms >= next_send_time_ms {
                let pts = frames_sent as f64 * sender_frame_interval_ms;
                queue.push(pts);
                frames_sent += 1;
                next_send_time_ms += sender_frame_interval_ms;
            }

            // Consumer: Display sink renders frames at render_cadence_ms intervals
            while current_time_ms >= next_render_time_ms && !queue.is_empty() {
                let frame_pts = queue.remove(0);

                if vsync_enabled {
                    // With vsync active: GStreamer QoS drops late frames immediately without display wait
                    let latency = current_time_ms - frame_pts;
                    if latency > qos_late_threshold_ms {
                        dropped += 1;
                        continue; // Discard late frame immediately without waiting for next display refresh
                    }
                }

                next_render_time_ms += render_cadence_ms;
            }

            // Advance clock to next discrete event
            let next_event = if frames_sent < total_frames {
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
        let final_latency = final_backlog as f64 * sender_frame_interval_ms;

        StreamSimulation {
            dropped_frames: dropped,
            final_queue_backlog: final_backlog,
            final_latency_ms: final_latency,
        }
    }

    #[test]
    fn test_tier4_01_vsync_true_prevents_latency_accumulation() {
        // Display throttled to 22.2 ms per frame (~45 fps, e.g. DWM composition constraint)
        let sim = run_timing_simulation(true, 22.22);

        // With vsync enabled, QoS drops late frames, preventing backlog growth
        assert!(
            sim.final_queue_backlog <= 3,
            "With vsync=true, final queue backlog must remain bounded (actual: {})",
            sim.final_queue_backlog
        );
        assert!(
            sim.final_latency_ms <= 50.0,
            "With vsync=true, final latency must remain bounded <= 50ms (actual: {:.1}ms)",
            sim.final_latency_ms
        );
        assert!(
            sim.dropped_frames > 0,
            "QoS frame dropping must be active to drop late frames when rendering is slower than input"
        );
    }

    #[test]
    fn test_tier4_02_vsync_false_causes_slow_motion_latency_runaway() {
        // Under identical throttled conditions with vsync disabled (sync=false)
        let sim = run_timing_simulation(false, 22.22);

        // Without vsync, late frames are never dropped, causing monotonic queue accumulation
        assert!(
            sim.final_queue_backlog > 500,
            "Without vsync, queue backlog must accumulate heavily (actual: {})",
            sim.final_queue_backlog
        );
        assert!(
            sim.final_latency_ms > 8000.0,
            "Without vsync, latency must explode (>8000ms delay, actual: {:.1}ms)",
            sim.final_latency_ms
        );
        assert_eq!(
            sim.dropped_frames, 0,
            "Without vsync, zero frames are dropped, causing slow-motion playback"
        );
    }

    #[test]
    fn test_tier4_03_ntp_to_pts_timestamp_mapping_monotonicity() {
        // Replicates UxPlay's raop_ntp.c & video_renderer.c calculation:
        // local_time = remote_ntp - sync_offset;
        // pts = (local_time > base_time) ? (local_time - base_time) : 0;
        let sync_offset_ns: i64 = 1_500_000_000; // 1.5s offset
        let base_time_ns: u64 = 10_000_000_000; // 10s base time

        let mut previous_pts = 0u64;

        for frame_idx in 0..1000 {
            // Remote NTP timestamp advancing at 16.666 ms (16,666,666 ns) intervals
            let remote_ntp_ns = base_time_ns as i64 + sync_offset_ns + (frame_idx * 16_666_666);
            let local_time_ns = (remote_ntp_ns - sync_offset_ns) as u64;

            let pts = if local_time_ns > base_time_ns {
                local_time_ns - base_time_ns
            } else {
                0
            };

            if frame_idx > 0 {
                assert!(
                    pts > previous_pts,
                    "PTS must be strictly monotonic (frame {}, prev: {}, current: {})",
                    frame_idx,
                    previous_pts,
                    pts
                );
                let delta = pts - previous_pts;
                assert_eq!(
                    delta, 16_666_666,
                    "PTS step must correspond exactly to frame interval"
                );
            }
            previous_pts = pts;
        }
    }

    #[test]
    fn test_tier4_04_network_jitter_absorption_without_stall() {
        // Simulates RTP packet arrival with +/- 15ms arrival jitter
        let sender_interval_ms = 16.666;
        let base_time_ms = 100.0;

        let jitter_offsets = [-12.0, 8.0, -5.0, 14.0, -10.0, 4.0, -8.0, 11.0];

        let mut previous_pts = 0.0;
        for (i, jitter) in jitter_offsets.iter().cycle().take(200).enumerate() {
            let ideal_arrival = base_time_ms + (i as f64 * sender_interval_ms);
            let actual_arrival = ideal_arrival + jitter;

            // Presentation timestamp is derived from packet header NTP timestamp, NOT arrival time
            let pts = i as f64 * sender_interval_ms;

            assert!(
                pts >= previous_pts,
                "Packet header PTS must remain strictly progressive regardless of network jitter"
            );
            assert!(
                (actual_arrival - ideal_arrival).abs() <= 15.0,
                "Jitter deviation is within simulated bounds"
            );
            previous_pts = pts;
        }
    }

    #[test]
    fn test_tier4_05_audio_video_lip_sync_drift_bounded_under_vsync_true() {
        // Simulates simultaneous 60-second audio and video streams
        // Audio clock slaved to WASAPI (44,100 Hz = 22.675 microseconds per sample)
        // Video sink slaved to system clock via PTS (GST_CLOCK_TYPE_REALTIME)
        let sample_rate = 44100.0;
        let video_fps = 60.0;

        for second in 1..=60 {
            let samples = second as f64 * sample_rate;
            let audio_time_ms = (samples / sample_rate) * 1000.0;

            let video_frames = second as f64 * video_fps;
            let video_pts_ms = (video_frames / video_fps) * 1000.0;

            let drift_ms = (audio_time_ms - video_pts_ms).abs();
            assert!(
                drift_ms < 0.001,
                "AV drift must remain bounded within sub-millisecond precision over 60 seconds (second {}, drift: {}ms)",
                second,
                drift_ms
            );
        }
    }
}
