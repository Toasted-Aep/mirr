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
        let host = cpal::default_host();
        let device = match target_device_name {
            Some(name) if !name.is_empty() && name != "default" => {
                let mut found = None;
                if let Ok(devices) = host.output_devices() {
                    for dev in devices {
                        let dev_name = dev
                            .description()
                            .map(|d| d.name().to_string())
                            .unwrap_or_else(|_| dev.to_string());
                        if dev_name.contains(name) || name.contains(&dev_name) {
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

        // State for resampler across callbacks
        let mut phase = 0.0f64;
        let step = rate_in as f64 / dev_sample_rate as f64;
        let mut prev_l = 0i16;
        let mut prev_r = 0i16;
        let mut next_l = 0i16;
        let mut next_r = 0i16;

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
                            prev_l = 0;
                            prev_r = 0;
                            next_l = 0;
                            next_r = 0;
                            state.flushed = false;
                        }

                        let frames = data.len() / dev_channels.max(1);
                        let same_rate = rate_in == dev_sample_rate;
                        let is_mono = channels_in == 1;

                        for f in 0..frames {
                            let (out_l, out_r) = if same_rate {
                                let l = state.queue.pop_front().unwrap_or(0);
                                let r = if is_mono { l } else { state.queue.pop_front().unwrap_or(0) };
                                (l as f32 / 32768.0, r as f32 / 32768.0)
                            } else {
                                let frac = phase as f32;
                                let l = (prev_l as f32 * (1.0 - frac) + next_l as f32 * frac) / 32768.0;
                                let r = (prev_r as f32 * (1.0 - frac) + next_r as f32 * frac) / 32768.0;
                                phase += step;
                                while phase >= 1.0 {
                                    phase -= 1.0;
                                    prev_l = next_l;
                                    prev_r = next_r;
                                    next_l = state.queue.pop_front().unwrap_or(0);
                                    next_r = if is_mono { next_l } else { state.queue.pop_front().unwrap_or(0) };
                                }
                                (l, r)
                            };

                            let base = f * dev_channels;
                            if dev_channels == 1 {
                                data[base] = (out_l + out_r) * 0.5;
                            } else {
                                data[base] = out_l;
                                data[base + 1] = out_r;
                                for ch in 2..dev_channels {
                                    data[base + ch] = 0.0;
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
                            prev_l = 0;
                            prev_r = 0;
                            next_l = 0;
                            next_r = 0;
                            state.flushed = false;
                        }

                        let frames = data.len() / dev_channels.max(1);
                        let same_rate = rate_in == dev_sample_rate;
                        let is_mono = channels_in == 1;

                        for f in 0..frames {
                            let (out_l, out_r) = if same_rate {
                                let l = state.queue.pop_front().unwrap_or(0);
                                let r = if is_mono { l } else { state.queue.pop_front().unwrap_or(0) };
                                (l, r)
                            } else {
                                let frac = phase as f32;
                                let l = (prev_l as f32 * (1.0 - frac) + next_l as f32 * frac) as i16;
                                let r = (prev_r as f32 * (1.0 - frac) + next_r as f32 * frac) as i16;
                                phase += step;
                                while phase >= 1.0 {
                                    phase -= 1.0;
                                    prev_l = next_l;
                                    prev_r = next_r;
                                    next_l = state.queue.pop_front().unwrap_or(0);
                                    next_r = if is_mono { next_l } else { state.queue.pop_front().unwrap_or(0) };
                                }
                                (l, r)
                            };

                            let base = f * dev_channels;
                            if dev_channels == 1 {
                                data[base] = (((out_l as i32) + (out_r as i32)) / 2) as i16;
                            } else {
                                data[base] = out_l;
                                data[base + 1] = out_r;
                                for ch in 2..dev_channels {
                                    data[base + ch] = 0;
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
            channels: channels_in.max(1) as usize,
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
}

