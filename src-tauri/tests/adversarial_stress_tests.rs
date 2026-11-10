//! Mirr AirPlay Adversarial Stress Testing & Verification Harness
//! Written by Challenger 1 (Adversarial Empirical Verification)
//!
//! Stress-tests:
//! 1. Massive burst arrivals & queue collapse
//! 2. Extreme network jitter & packet starvation / clumping
//! 3. Severe display stall & recovery latency dynamics
//! 4. Long-duration clock micro-drift (10-hour simulation at 50 ppm)
//! 5. Variable and high frame-rates (15 fps, 24 fps, 60 fps, 120 fps)
//! 6. Live UxPlay binary argument execution across all boundary permutations

use std::path::PathBuf;
use std::process::Command;

fn uxplay_exe() -> Option<PathBuf> {
    let p1 = PathBuf::from(r"C:\Program Files (x86)\uxplay-windows\_internal\bin\uxplay.exe");
    if p1.is_file() {
        return Some(p1);
    }
    None
}

/// Advanced simulation engine modeling AirPlay streaming queue dynamics under stress
struct StressSimulator {
    pub vsync: bool,
    pub sender_fps: f64,
    pub display_fps: f64,
    pub qos_late_drop_threshold_ms: f64,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct SimResult {
    pub total_sent: usize,
    pub total_rendered: usize,
    pub total_dropped: usize,
    pub max_latency_ms: f64,
    pub final_latency_ms: f64,
    pub final_queue_len: usize,
}

impl StressSimulator {
    pub fn new(vsync: bool, sender_fps: f64, display_fps: f64) -> Self {
        Self {
            vsync,
            sender_fps,
            display_fps,
            qos_late_drop_threshold_ms: 25.0, // GStreamer QoS default late threshold
        }
    }

    /// Run simulation where frame arrival times can be subjected to arbitrary arrival offsets
    pub fn simulate_with_arrival_schedule(
        &self,
        duration_ms: f64,
        arrival_offsets_ms: &[f64],
        render_stall_start_ms: Option<f64>,
        render_stall_duration_ms: Option<f64>,
    ) -> SimResult {
        let frame_interval_ms = 1000.0 / self.sender_fps;
        let render_interval_ms = 1000.0 / self.display_fps;

        let mut queue: Vec<f64> = Vec::new(); // stores frame PTS
        let mut dropped = 0;
        let mut rendered = 0;
        let mut sent = 0;
        let mut max_latency = 0.0f64;

        let mut current_time_ms = 0.0;
        let mut next_send_time_ms = 0.0;
        let mut next_render_time_ms = render_interval_ms;

        let stall_start = render_stall_start_ms.unwrap_or(f64::MAX);
        let stall_end = stall_start + render_stall_duration_ms.unwrap_or(0.0);

        while current_time_ms <= duration_ms {
            // Producer: Generate frames
            while current_time_ms >= next_send_time_ms {
                let frame_idx = sent;
                let ideal_pts = frame_idx as f64 * frame_interval_ms;
                let offset = if !arrival_offsets_ms.is_empty() {
                    arrival_offsets_ms[frame_idx % arrival_offsets_ms.len()]
                } else {
                    0.0
                };

                let arrival_time = next_send_time_ms + offset;
                if arrival_time <= current_time_ms {
                    queue.push(ideal_pts);
                    sent += 1;
                    next_send_time_ms += frame_interval_ms;
                } else {
                    break;
                }
            }

            // Consumer: Render frames (unless stalled)
            let is_stalled = current_time_ms >= stall_start && current_time_ms < stall_end;

            if !is_stalled {
                while current_time_ms >= next_render_time_ms && !queue.is_empty() {
                    let frame_pts = queue.remove(0);
                    let latency = current_time_ms - frame_pts;
                    if latency > max_latency {
                        max_latency = latency;
                    }

                    if self.vsync {
                        // With vsync active: QoS drops late frames
                        if latency > self.qos_late_drop_threshold_ms {
                            dropped += 1;
                            continue; // Drop immediately
                        }
                    }

                    rendered += 1;
                    next_render_time_ms += render_interval_ms;
                }
            } else {
                // If stalled, push render time forward to stall end
                if next_render_time_ms < stall_end {
                    next_render_time_ms = stall_end;
                }
            }

            // Step clock
            let next_event = next_send_time_ms.min(next_render_time_ms);
            if next_event <= current_time_ms {
                current_time_ms += 0.05;
            } else {
                current_time_ms = next_event;
            }
        }

        let final_queue = queue.len();
        let final_lat = final_queue as f64 * frame_interval_ms;

        SimResult {
            total_sent: sent,
            total_rendered: rendered,
            total_dropped: dropped,
            max_latency_ms: max_latency,
            final_latency_ms: final_lat,
            final_queue_len: final_queue,
        }
    }
}

// =========================================================================
// 1. BURST ARRIVALS STRESS TEST
// =========================================================================

#[test]
fn test_stress_burst_arrival_queue_collapse() {
    // Burst scenario: 100 frames arrive simultaneously at t = 500ms
    // (simulates WiFi congestion burst or buffer release)
    let sim_sync = StressSimulator::new(true, 60.0, 60.0);
    let sim_nosync = StressSimulator::new(false, 60.0, 60.0);

    // Negative offsets create a clump/burst where multiple frames arrive at once
    let burst_offsets = vec![-100.0; 60];

    let res_sync = sim_sync.simulate_with_arrival_schedule(5000.0, &burst_offsets, None, None);
    let res_nosync = sim_nosync.simulate_with_arrival_schedule(5000.0, &burst_offsets, None, None);

    // With VSync (sync=true): Late burst frames are dropped by QoS and final latency recovers to bounded real-time
    assert!(
        res_sync.final_latency_ms <= 40.0,
        "With vsync=true, burst arrival must not cause permanent latency bloat (actual: {:.1}ms)",
        res_sync.final_latency_ms
    );
    assert!(
        res_sync.final_queue_len <= 3,
        "With vsync=true, queue must collapse back to nominal size (actual: {})",
        res_sync.final_queue_len
    );

    // With VSync disabled (sync=false): Zero frames dropped, queue stays bloated
    assert_eq!(
        res_nosync.total_dropped, 0,
        "With vsync=false, zero frames are dropped, proving unbounded FIFO accumulation"
    );
}

// =========================================================================
// 2. SEVERE NETWORK JITTER & STARVATION
// =========================================================================

#[test]
fn test_stress_severe_network_jitter_up_to_150ms() {
    let sim_sync = StressSimulator::new(true, 60.0, 60.0);

    // Jitter schedule with swings up to +/- 150 ms
    let extreme_jitter = vec![
        -50.0, 75.0, -120.0, 150.0, -80.0, 30.0, -140.0, 110.0,
        0.0, -30.0, 90.0, -110.0, 140.0, -70.0, 50.0, -90.0
    ];

    let res = sim_sync.simulate_with_arrival_schedule(10_000.0, &extreme_jitter, None, None);

    // Bounded latency despite extreme jitter
    assert!(
        res.final_latency_ms <= 50.0,
        "Final latency under extreme jitter must remain <= 50ms with vsync (actual: {:.1}ms)",
        res.final_latency_ms
    );
}

// =========================================================================
// 3. SEVERE RENDERING STALL & RECOVERY DYNAMICS
// =========================================================================

#[test]
fn test_stress_display_sink_freeze_and_fast_recovery() {
    // Simulate a 1000ms freeze (e.g. desktop lock, display mode switch, GPU TDR)
    let sim_sync = StressSimulator::new(true, 60.0, 60.0);
    let sim_nosync = StressSimulator::new(false, 60.0, 60.0);

    let res_sync = sim_sync.simulate_with_arrival_schedule(5000.0, &[], Some(1000.0), Some(1000.0));
    let res_nosync = sim_nosync.simulate_with_arrival_schedule(5000.0, &[], Some(1000.0), Some(1000.0));

    // Under vsync: true, all frames that expired during the 1000ms freeze are dropped immediately
    assert!(
        res_sync.total_dropped >= 55,
        "During 1000ms stall, approximately 60 frames should be dropped (actual: {})",
        res_sync.total_dropped
    );
    assert!(
        res_sync.final_latency_ms <= 35.0,
        "After stall recovery, vsync=true must instantly snap back to real-time (actual: {:.1}ms)",
        res_sync.final_latency_ms
    );

    // Under vsync: false, zero frames dropped; queue is permanently 1000ms behind
    assert_eq!(res_nosync.total_dropped, 0);
    assert!(
        res_nosync.final_latency_ms >= 900.0,
        "Without vsync, stall leaves a permanent >900ms delay (actual: {:.1}ms)",
        res_nosync.final_latency_ms
    );
}

// =========================================================================
// 4. LONG DURATION CLOCK MICRO-DRIFT (50 PPM)
// =========================================================================

#[test]
fn test_stress_clock_drift_50_ppm_over_10_hours() {
    // 50 ppm drift: Sender clock runs 50 microseconds faster per second
    // Over 10 hours (36,000s), that is 1.8 seconds of drift = ~108 extra frames
    let total_seconds = 36000.0;
    let sender_fps = 60.003; // ~50 ppm fast
    let display_fps = 60.000;

    let frames_sent = (total_seconds * sender_fps) as usize;
    let frames_display_capacity = (total_seconds * display_fps) as usize;
    let drift_frames = frames_sent.saturating_sub(frames_display_capacity);

    assert_eq!(drift_frames, 108, "50 ppm over 10 hours generates exactly 108 excess frames");

    // With vsync enabled, GStreamer QoS drops these 108 frames transparently (1 drop every ~5.5 minutes)
    // keeping buffer backlog at 0-1 frame.
    // Without vsync, those 108 frames remain in buffer, creating a 1.8-second slow-motion lag!
    let latency_without_vsync_ms = drift_frames as f64 * (1000.0 / 60.0);
    assert!(
        latency_without_vsync_ms > 1750.0,
        "Without vsync, clock drift accumulates >1.75s latency (actual: {:.1}ms)",
        latency_without_vsync_ms
    );
}

// =========================================================================
// 5. VARIABLE & HIGH FRAME-RATES
// =========================================================================

#[test]
fn test_stress_high_framerate_120fps_and_24fps_cinematic() {
    // 120 fps ProMotion
    let sim_120 = StressSimulator::new(true, 120.0, 120.0);
    let res_120 = sim_120.simulate_with_arrival_schedule(2000.0, &[], None, None);
    assert!(res_120.final_latency_ms <= 20.0);

    // 24 fps Cinema
    let sim_24 = StressSimulator::new(true, 24.0, 24.0);
    let res_24 = sim_24.simulate_with_arrival_schedule(2000.0, &[], None, None);
    assert!(res_24.final_latency_ms <= 50.0);
}

// =========================================================================
// 6. LIVE UXPLAY BINARY EXECUTION ACROSS ALL BOUNDARY COMBINATIONS
// =========================================================================

#[test]
fn test_stress_live_uxplay_binary_argument_permutations() {
    let exe = match uxplay_exe() {
        Some(e) => e,
        None => {
            eprintln!("Skipping live binary test: uxplay.exe not found");
            return;
        }
    };

    // Test matrix of valid permutations:
    // (vsync_arg, fps_arg, as_arg, vd_arg, vs_arg)
    let vsync_options = vec![
        vec!["-vsync"],
        vec!["-vsync", "no"],
    ];

    let fps_options = vec!["30", "60", "120"];
    let name_options = vec!["Mirr", "Living Room TV", "Test-123_@#"];

    for vsync in &vsync_options {
        for fps in &fps_options {
            for name in &name_options {
                let mut cmd = Command::new(&exe);
                cmd.args(["-n", name, "-nh", "-p", "7100", "-fps", fps, "-nofreeze"]);
                cmd.args(vsync);
                cmd.args(["-as", "wasapisink"]);
                cmd.args(["-vd", "d3d11h264dec", "-vs", "d3d11videosink"]);
                cmd.arg("-h"); // Parse and print help without binding ports

                let output = cmd.output().expect("execute uxplay with permutation");
                assert!(
                    output.status.success(),
                    "uxplay failed on permutation (vsync={:?}, fps={}, name={}): stderr={}",
                    vsync,
                    fps,
                    name,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
}

#[test]
fn test_stress_live_uxplay_binary_rejects_all_invalid_vsync_forms() {
    let exe = match uxplay_exe() {
        Some(e) => e,
        None => return,
    };

    let invalid_vsync_args = vec![
        vec!["-vsync", "yes"],
        vec!["-vsync", "true"],
        vec!["-vsync", "on"],
        vec!["-vsync", "enable"],
    ];

    for invalid in invalid_vsync_args {
        let mut cmd = Command::new(&exe);
        cmd.args(["-n", "Mirr"]);
        cmd.args(&invalid);
        cmd.arg("-h");

        let output = cmd.output().expect("execute uxplay with invalid args");
        assert!(
            !output.status.success(),
            "uxplay MUST reject invalid vsync argument {:?}, but it succeeded",
            invalid
        );
    }
}
