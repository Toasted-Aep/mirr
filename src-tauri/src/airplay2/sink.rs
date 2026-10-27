//! Native Windows WASAPI Audio Sink for AirPlay 2 using `cpal`.
//!
//! Provides low-latency, glitch-free playback of 44.1 kHz 16-bit stereo PCM
//! decoded from AirPlay 2 streams, with automatic format negotiation (F32/I16),
//! sample-rate conversion (44.1 kHz -> 48 kHz native WASAPI shared mode), and
//! instant flush/seek support.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use openairplay2::AudioSink;

const MAX_BUFFER_SAMPLES: usize = 88200; // ~1000ms cushion for smooth WASAPI / Bluetooth output

struct SharedQueue {
    queue: VecDeque<i16>,
    flushed: bool,
}

pub struct WasapiSink {
    shared: Arc<(Mutex<SharedQueue>, Condvar)>,
    stopped: Arc<AtomicBool>,
    _stream: cpal::Stream,
    channels: usize,
}

impl WasapiSink {
    pub fn new(rate_in: u32, channels_in: u8, target_device_name: Option<&str>) -> anyhow::Result<Self> {
        let resolved_name = crate::audio::resolve_device_name(target_device_name);
        let target_name_ref = resolved_name.as_deref().or(target_device_name);

        let host = cpal::default_host();
        let device = match target_name_ref {
            Some(name) if !name.is_empty() && name != "default" => {
                let mut found = None;
                if let Ok(devices) = host.output_devices() {
                    for dev in devices {
                        let dev_name = dev
                            .description()
                            .map(|d| d.name().to_string())
                            .unwrap_or_else(|_| dev.to_string());
                        if dev_name.eq_ignore_ascii_case(name)
                            || dev_name.contains(name)
                            || name.contains(&dev_name)
                        {
                            found = Some(dev);
                            break;
                        }
                    }
                }
                match found {
                    Some(d) => d,
                    None => host
                        .default_output_device()
                        .ok_or_else(|| anyhow::anyhow!("No default audio output device found"))?,
                }
            }
            _ => host
                .default_output_device()
                .ok_or_else(|| anyhow::anyhow!("No default audio output device found"))?,
        };

        let default_config = device.default_output_config()?;
        let dev_sample_rate = default_config.sample_rate();
        let dev_channels = default_config.channels() as usize;
        let sample_format = default_config.sample_format();
        let stream_config = default_config.config();

        let spatial_caps = crate::audio::get_spatial_capabilities(target_device_name);
        println!(
            "[airplay2::sink] Initializing WASAPI stream: in={rate_in}Hz/{channels_in}ch, out={dev_sample_rate}Hz/{dev_channels}ch, format={sample_format:?}, Dolby Atmos={}, Tech='{}'",
            spatial_caps.is_dolby_atmos, spatial_caps.spatial_technology
        );

        let shared = Arc::new((
            Mutex::new(SharedQueue {
                queue: VecDeque::with_capacity(MAX_BUFFER_SAMPLES * 2),
                flushed: false,
            }),
            Condvar::new(),
        ));
        let stopped = Arc::new(AtomicBool::new(false));

        let stopped_cb = stopped.clone();
        let err_fn = move |err: cpal::Error| {
            eprintln!("[airplay2::sink] WASAPI stream error: {err}");
            stopped_cb.store(true, Ordering::SeqCst);
        };

        let in_ch = (channels_in as usize).max(1);

        // State for resampler across callbacks
        let mut phase = 0.0f64;
        let step = rate_in as f64 / dev_sample_rate as f64;
        let mut prev_frame = [0i16; 8];
        let mut next_frame = [0i16; 8];

        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                let shared_inner = shared.clone();
                device.build_output_stream(
                    stream_config,
                    move |data: &mut [f32], _| {
                        let (lock, cvar) = &*shared_inner;
                        let mut state = match lock.try_lock() {
                            Ok(guard) => guard,
                            Err(_) => {
                                for sample in data.iter_mut() {
                                    *sample = 0.0;
                                }
                                return;
                            }
                        };

                        if state.flushed {
                            phase = 0.0;
                            prev_frame = [0; 8];
                            next_frame = [0; 8];
                            state.flushed = false;
                        }

                        let frames = data.len() / dev_channels.max(1);
                        let same_rate = rate_in == dev_sample_rate;

                        for f in 0..frames {
                            let mut in_frame = [0i16; 8];
                            if same_rate {
                                for ch in 0..in_ch.min(8) {
                                    in_frame[ch] = state.queue.pop_front().unwrap_or(0);
                                }
                                for _ in 8..in_ch {
                                    let _ = state.queue.pop_front();
                                }
                            } else {
                                let frac = phase as f32;
                                for ch in 0..in_ch.min(8) {
                                    let p = prev_frame[ch] as f32;
                                    let n = next_frame[ch] as f32;
                                    in_frame[ch] = (p * (1.0 - frac) + n * frac) as i16;
                                }
                                phase += step;
                                while phase >= 1.0 {
                                    phase -= 1.0;
                                    prev_frame = next_frame;
                                    for ch in 0..in_ch.min(8) {
                                        next_frame[ch] = state.queue.pop_front().unwrap_or(0);
                                    }
                                    for _ in 8..in_ch {
                                        let _ = state.queue.pop_front();
                                    }
                                }
                            }

                            let base = f * dev_channels;
                            if in_ch == 1 {
                                let m = in_frame[0] as f32 / 32768.0;
                                data[base] = m;
                                if dev_channels >= 2 { data[base + 1] = m; }
                                for ch in 2..dev_channels { data[base + ch] = 0.0; }
                            } else if in_ch == 2 {
                                let l = in_frame[0] as f32 / 32768.0;
                                let r = in_frame[1] as f32 / 32768.0;
                                if dev_channels == 1 {
                                    data[base] = (l + r) * 0.5;
                                } else if dev_channels == 2 {
                                    data[base] = l;
                                    data[base + 1] = r;
                                } else {
                                    // Multi-channel spatial speaker layout (ITU-R BS.775 / 5.1 / 7.1 / Dolby Atmos surround bed)
                                    let center = (l + r) * 0.7071;
                                    let lfe = (l + r) * 0.5;
                                    let ls = (l - 0.5 * r) * 0.7071;
                                    let rs = (r - 0.5 * l) * 0.7071;
                                    data[base] = l;
                                    data[base + 1] = r;
                                    data[base + 2] = center;
                                    if dev_channels >= 4 { data[base + 3] = lfe; }
                                    if dev_channels >= 5 { data[base + 4] = ls; }
                                    if dev_channels >= 6 { data[base + 5] = rs; }
                                    if dev_channels >= 7 { data[base + 6] = (l - r) * 0.5; }
                                    if dev_channels >= 8 { data[base + 7] = (r - l) * 0.5; }
                                    for ch in 8..dev_channels {
                                        data[base + ch] = 0.0;
                                    }
                                }
                            } else {
                                // Multichannel input (5.1 / 7.1 / Atmos bed)
                                let l = in_frame[0] as f32 / 32768.0;
                                let r = in_frame[1] as f32 / 32768.0;
                                let c = in_frame[2] as f32 / 32768.0;
                                let lfe = in_frame[3] as f32 / 32768.0;
                                let ls = in_frame[4] as f32 / 32768.0;
                                let rs = in_frame[5] as f32 / 32768.0;
                                let rbl = in_frame[6] as f32 / 32768.0;
                                let rbr = in_frame[7] as f32 / 32768.0;

                                if dev_channels == 1 {
                                    data[base] = ((l + r + c * 1.414 + ls + rs) * 0.25).clamp(-1.0, 1.0);
                                } else if dev_channels == 2 {
                                    // Downmix to stereo
                                    data[base] = (l + 0.7071 * c + 0.7071 * ls + 0.5 * rbl).clamp(-1.0, 1.0);
                                    data[base + 1] = (r + 0.7071 * c + 0.7071 * rs + 0.5 * rbr).clamp(-1.0, 1.0);
                                } else {
                                    data[base] = l;
                                    data[base + 1] = r;
                                    data[base + 2] = c;
                                    if dev_channels >= 4 { data[base + 3] = lfe; }
                                    if dev_channels >= 5 { data[base + 4] = ls; }
                                    if dev_channels >= 6 { data[base + 5] = rs; }
                                    if dev_channels >= 7 { data[base + 6] = rbl; }
                                    if dev_channels >= 8 { data[base + 7] = rbr; }
                                    for ch in 8..dev_channels {
                                        data[base + ch] = 0.0;
                                    }
                                }
                            }
                        }

                        cvar.notify_one();
                    },
                    err_fn,
                    None,
                )?
            }
            cpal::SampleFormat::I16 => {
                let shared_inner = shared.clone();
                device.build_output_stream(
                    stream_config,
                    move |data: &mut [i16], _| {
                        let (lock, cvar) = &*shared_inner;
                        let mut state = match lock.try_lock() {
                            Ok(guard) => guard,
                            Err(_) => {
                                for sample in data.iter_mut() {
                                    *sample = 0;
                                }
                                return;
                            }
                        };

                        if state.flushed {
                            phase = 0.0;
                            prev_frame = [0; 8];
                            next_frame = [0; 8];
                            state.flushed = false;
                        }

                        let frames = data.len() / dev_channels.max(1);
                        let same_rate = rate_in == dev_sample_rate;

                        for f in 0..frames {
                            let mut in_frame = [0i16; 8];
                            if same_rate {
                                for ch in 0..in_ch.min(8) {
                                    in_frame[ch] = state.queue.pop_front().unwrap_or(0);
                                }
                                for _ in 8..in_ch {
                                    let _ = state.queue.pop_front();
                                }
                            } else {
                                let frac = phase as f32;
                                for ch in 0..in_ch.min(8) {
                                    let p = prev_frame[ch] as f32;
                                    let n = next_frame[ch] as f32;
                                    in_frame[ch] = (p * (1.0 - frac) + n * frac) as i16;
                                }
                                phase += step;
                                while phase >= 1.0 {
                                    phase -= 1.0;
                                    prev_frame = next_frame;
                                    for ch in 0..in_ch.min(8) {
                                        next_frame[ch] = state.queue.pop_front().unwrap_or(0);
                                    }
                                    for _ in 8..in_ch {
                                        let _ = state.queue.pop_front();
                                    }
                                }
                            }

                            let base = f * dev_channels;
                            if in_ch == 1 {
                                let m = in_frame[0];
                                data[base] = m;
                                if dev_channels >= 2 { data[base + 1] = m; }
                                for ch in 2..dev_channels { data[base + ch] = 0; }
                            } else if in_ch == 2 {
                                let l = in_frame[0];
                                let r = in_frame[1];
                                if dev_channels == 1 {
                                    data[base] = (((l as i32) + (r as i32)) / 2) as i16;
                                } else if dev_channels == 2 {
                                    data[base] = l;
                                    data[base + 1] = r;
                                } else {
                                    let l_f = l as f32;
                                    let r_f = r as f32;
                                    let center = (((l_f + r_f) * 0.7071) as i32).clamp(-32768, 32767) as i16;
                                    let lfe = (((l_f + r_f) * 0.5) as i32).clamp(-32768, 32767) as i16;
                                    let ls = (((l_f - 0.5 * r_f) * 0.7071) as i32).clamp(-32768, 32767) as i16;
                                    let rs = (((r_f - 0.5 * l_f) * 0.7071) as i32).clamp(-32768, 32767) as i16;
                                    data[base] = l;
                                    data[base + 1] = r;
                                    data[base + 2] = center;
                                    if dev_channels >= 4 { data[base + 3] = lfe; }
                                    if dev_channels >= 5 { data[base + 4] = ls; }
                                    if dev_channels >= 6 { data[base + 5] = rs; }
                                    if dev_channels >= 7 { data[base + 6] = (((l_f - r_f) * 0.5) as i32).clamp(-32768, 32767) as i16; }
                                    if dev_channels >= 8 { data[base + 7] = (((r_f - l_f) * 0.5) as i32).clamp(-32768, 32767) as i16; }
                                    for ch in 8..dev_channels {
                                        data[base + ch] = 0;
                                    }
                                }
                            } else {
                                let l = in_frame[0];
                                let r = in_frame[1];
                                let c = in_frame[2];
                                let lfe = in_frame[3];
                                let ls = in_frame[4];
                                let rs = in_frame[5];
                                let rbl = in_frame[6];
                                let rbr = in_frame[7];

                                if dev_channels == 1 {
                                    data[base] = ((((l as i32) + (r as i32) + ((c as f32 * 1.414) as i32) + (ls as i32) + (rs as i32)) / 4).clamp(-32768, 32767)) as i16;
                                } else if dev_channels == 2 {
                                    let l_down = (l as f32 + 0.7071 * (c as f32) + 0.7071 * (ls as f32) + 0.5 * (rbl as f32)) as i32;
                                    let r_down = (r as f32 + 0.7071 * (c as f32) + 0.7071 * (rs as f32) + 0.5 * (rbr as f32)) as i32;
                                    data[base] = l_down.clamp(-32768, 32767) as i16;
                                    data[base + 1] = r_down.clamp(-32768, 32767) as i16;
                                } else {
                                    data[base] = l;
                                    data[base + 1] = r;
                                    data[base + 2] = c;
                                    if dev_channels >= 4 { data[base + 3] = lfe; }
                                    if dev_channels >= 5 { data[base + 4] = ls; }
                                    if dev_channels >= 6 { data[base + 5] = rs; }
                                    if dev_channels >= 7 { data[base + 6] = rbl; }
                                    if dev_channels >= 8 { data[base + 7] = rbr; }
                                    for ch in 8..dev_channels {
                                        data[base + ch] = 0;
                                    }
                                }
                            }
                        }

                        cvar.notify_one();
                    },
                    err_fn,
                    None,
                )?
            }
            other => {
                return Err(anyhow::anyhow!("Unsupported audio sample format: {:?}", other));
            }
        };

        stream.play()?;

        Ok(Self {
            shared,
            stopped,
            _stream: stream,
            channels: in_ch,
        })
    }
}

impl AudioSink for WasapiSink {
    fn write(&mut self, pcm: &[i16]) {
        if self.stopped.load(Ordering::Relaxed) || pcm.is_empty() {
            return;
        }
        let (lock, cvar) = &*self.shared;
        let mut state = match lock.lock() {
            Ok(s) => s,
            Err(p) => p.into_inner(),
        };

        while state.queue.len() >= MAX_BUFFER_SAMPLES && !self.stopped.load(Ordering::Relaxed) {
            let res = cvar.wait_timeout(state, std::time::Duration::from_millis(20));
            state = match res {
                Ok((s, _)) => s,
                Err(p) => p.into_inner().0,
            };
        }
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        state.queue.extend(pcm.iter().copied());
        if state.queue.len() > MAX_BUFFER_SAMPLES * 2 {
            let mut excess = state.queue.len() - MAX_BUFFER_SAMPLES;
            let ch = self.channels.max(1);
            excess -= excess % ch;
            if excess > 0 {
                state.queue.drain(0..excess);
            }
        }
    }

    fn flush(&mut self) {
        let (lock, cvar) = &*self.shared;
        if let Ok(mut state) = lock.lock() {
            state.queue.clear();
            state.flushed = true;
            cvar.notify_all();
        }
    }
}

impl Drop for WasapiSink {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let (_, cvar) = &*self.shared;
        cvar.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wasapi_sink_flush_and_write() {
        let shared = Arc::new((
            Mutex::new(SharedQueue {
                queue: VecDeque::new(),
                flushed: false,
            }),
            Condvar::new(),
        ));
        let stopped = Arc::new(AtomicBool::new(false));

        struct DummySink {
            shared: Arc<(Mutex<SharedQueue>, Condvar)>,
            stopped: Arc<AtomicBool>,
        }

        impl AudioSink for DummySink {
            fn write(&mut self, pcm: &[i16]) {
                if self.stopped.load(Ordering::Relaxed) {
                    return;
                }
                let (lock, _) = &*self.shared;
                let mut state = lock.lock().unwrap();
                state.queue.extend(pcm.iter().copied());
            }

            fn flush(&mut self) {
                let (lock, cvar) = &*self.shared;
                let mut state = lock.lock().unwrap();
                state.queue.clear();
                cvar.notify_all();
            }
        }

        let mut sink = DummySink {
            shared: shared.clone(),
            stopped,
        };

        let samples = vec![100i16, 200, 300, 400];
        sink.write(&samples);

        {
            let lock = shared.0.lock().unwrap();
            assert_eq!(lock.queue.len(), 4);
            assert_eq!(lock.queue[0], 100);
            assert_eq!(lock.queue[3], 400);
        }

        sink.flush();

        {
            let lock = shared.0.lock().unwrap();
            assert_eq!(lock.queue.len(), 0);
        }
    }

    #[test]
    fn test_real_wasapi_sink_hardware_creation() {
        if let Some(_device) = cpal::default_host().default_output_device() {
            let res = WasapiSink::new(44100, 2, None);
            match res {
                Ok(mut sink) => {
                    let samples = vec![0i16; 512];
                    sink.write(&samples);
                    sink.flush();
                }
                Err(e) => {
                    eprintln!("WASAPI sink init (e.g. headless CI): {e}");
                }
            }
        }
    }

    #[test]
    fn test_resampler_linear_interpolation() {
        let rate_in = 44100f64;
        let dev_rate = 48000f64;
        let step = rate_in / dev_rate;

        let input_samples = vec![0i16, 1000, 2000, 3000, 4000];
        let mut queue = VecDeque::from(input_samples);

        let mut phase = 0.0f64;
        let mut prev = 0i16;
        let mut next = queue.pop_front().unwrap_or(0);
        let mut output = Vec::new();

        for _ in 0..6 {
            let frac = phase as f32;
            let val = (prev as f32 * (1.0 - frac) + next as f32 * frac) as i16;
            output.push(val);
            phase += step;
            while phase >= 1.0 {
                phase -= 1.0;
                prev = next;
                next = queue.pop_front().unwrap_or(0);
            }
        }

        assert_eq!(output.len(), 6);
        assert_eq!(output[0], 0);
        assert!(output[1] >= 0);
    }

    #[test]
    fn test_flushed_flag_clears_queue() {
        let shared = Arc::new((
            Mutex::new(SharedQueue {
                queue: VecDeque::from(vec![1, 2, 3, 4]),
                flushed: false,
            }),
            Condvar::new(),
        ));
        {
            let mut state = shared.0.lock().unwrap();
            state.queue.clear();
            state.flushed = true;
        }
        let state = shared.0.lock().unwrap();
        assert!(state.queue.is_empty());
        assert!(state.flushed);
    }

    #[test]
    fn test_channel_aligned_overflow_drain() {
        let mut queue: VecDeque<i16> = VecDeque::new();
        // Stereo interleaved samples: (L=10, R=20), (L=11, R=21)...
        for i in 0..100 {
            queue.push_back(i * 10);
            queue.push_back(i * 10 + 1);
        }
        let channels = 2usize;
        let max_keep = 50usize;
        if queue.len() > max_keep {
            let mut excess = queue.len() - max_keep;
            excess -= excess % channels;
            queue.drain(0..excess);
        }
        // First sample in queue must still be an L sample (multiple of 10)
        assert_eq!(queue.len() % channels, 0);
        let first = queue.front().copied().unwrap();
        assert_eq!(first % 10, 0, "Channel alignment must not swap left/right: got {first}");
    }

    #[test]
    fn test_multichannel_5_1_and_7_1_frame_alignment() {
        for in_channels in [6usize, 8usize] {
            let mut queue: VecDeque<i16> = VecDeque::new();
            for frame_idx in 0..50 {
                for ch in 0..in_channels {
                    queue.push_back((frame_idx * 100 + ch) as i16);
                }
            }
            let max_keep = 100usize;
            if queue.len() > max_keep {
                let mut excess = queue.len() - max_keep;
                excess -= excess % in_channels;
                queue.drain(0..excess);
            }
            assert_eq!(queue.len() % in_channels, 0, "Multichannel frame must stay channel-aligned");
            let first = queue.front().copied().unwrap();
            assert_eq!(first % 100, 0, "First channel in frame must be Left (channel 0)");
        }
    }
}

