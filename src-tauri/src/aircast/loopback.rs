//! WASAPI loopback audio capture with silence injection for outbound streaming.
//!
//! Windows WASAPI loopback capture (`AUDCLNT_STREAMFLAGS_LOOPBACK`) stops
//! providing packets when no audio is currently playing on the system.
//! To prevent HTTP stream stalls and keep Google Cast / DLNA players from
//! aborting with MEDIA_TIMEOUT or LOAD_FAILED, silence frames (0-filled PCM)
//! are automatically injected if no packets arrive within 40ms.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// Global broadcast channel for loopback PCM chunks.
static RUNNING: AtomicBool = AtomicBool::new(false);
static ACTIVE_SAMPLE_RATE: AtomicU32 = AtomicU32::new(48000);
static SENDER: once_cell::sync::Lazy<broadcast::Sender<Arc<Vec<u8>>>> =
    once_cell::sync::Lazy::new(|| {
        let (tx, _rx) = broadcast::channel(128);
        tx
    });

/// Get current active sample rate captured from WASAPI loopback (defaults to 48000).
pub fn sample_rate() -> u32 {
    ACTIVE_SAMPLE_RATE.load(Ordering::SeqCst)
}

/// 44-byte standard RIFF WAV header for 16-bit stereo PCM.
/// `data_len` is set to 0x7FFF_0000 for infinite chunked streaming.
pub fn generate_wav_header(sample_rate: u32, channels: u16) -> [u8; 44] {
    let mut header = [0u8; 44];
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * (channels as u32) * (bits_per_sample as u32 / 8);
    let block_align = channels * (bits_per_sample / 8);
    let total_data_len: u32 = 0x7FFF_0000;
    let riff_chunk_size: u32 = total_data_len + 36;

    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&riff_chunk_size.to_le_bytes());
    header[8..12].copy_from_slice(b"WAVE");
    header[12..16].copy_from_slice(b"fmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes()); // Subchunk1Size (16 for PCM)
    header[20..22].copy_from_slice(&1u16.to_le_bytes()); // AudioFormat (1 = PCM)
    header[22..24].copy_from_slice(&channels.to_le_bytes());
    header[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    header[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    header[32..34].copy_from_slice(&block_align.to_le_bytes());
    header[34..36].copy_from_slice(&bits_per_sample.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&total_data_len.to_le_bytes());

    header
}

/// Subscribe to the live audio stream.
pub fn subscribe() -> broadcast::Receiver<Arc<Vec<u8>>> {
    ensure_capture_running();
    SENDER.subscribe()
}

pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

/// Downmix multi-channel IEEE float frame to 16-bit stereo PCM.
/// For surround formats (5.1/7.1), dialogue in the center channel is folded into L and R.
pub fn downmix_float_to_i16_stereo(frame: &[f32], channels: usize) -> (i16, i16) {
    if channels == 1 {
        let val = (frame[0].clamp(-1.0, 1.0) * 32767.0) as i16;
        (val, val)
    } else if channels == 2 {
        let l = (frame[0].clamp(-1.0, 1.0) * 32767.0) as i16;
        let r = (frame[1].clamp(-1.0, 1.0) * 32767.0) as i16;
        (l, r)
    } else {
        let left = frame[0];
        let right = frame[1];
        let center = if channels > 2 { frame[2] } else { 0.0 };
        let surround_l = if channels > 4 { frame[4] } else { 0.0 };
        let surround_r = if channels > 5 { frame[5] } else { 0.0 };
        let mix_l = left + 0.7071 * center + 0.7071 * surround_l;
        let mix_r = right + 0.7071 * center + 0.7071 * surround_r;
        let l = (mix_l.clamp(-1.0, 1.0) * 32767.0) as i16;
        let r = (mix_r.clamp(-1.0, 1.0) * 32767.0) as i16;
        (l, r)
    }
}

/// Downmix multi-channel 16-bit PCM frame to 16-bit stereo PCM.
pub fn downmix_i16_to_i16_stereo(frame: &[i16], channels: usize) -> (i16, i16) {
    if channels == 1 {
        (frame[0], frame[0])
    } else if channels == 2 {
        (frame[0], frame[1])
    } else {
        let left = frame[0] as f32;
        let right = frame[1] as f32;
        let center = if channels > 2 { frame[2] as f32 } else { 0.0 };
        let surround_l = if channels > 4 { frame[4] as f32 } else { 0.0 };
        let surround_r = if channels > 5 { frame[5] as f32 } else { 0.0 };
        let mix_l = (left + 0.7071 * center + 0.7071 * surround_l).clamp(-32768.0, 32767.0) as i16;
        let mix_r = (right + 0.7071 * center + 0.7071 * surround_r).clamp(-32768.0, 32767.0) as i16;
        (mix_l, mix_r)
    }
}

pub fn ensure_capture_running() {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }

    std::thread::Builder::new()
        .name("aircast-loopback".to_string())
        .spawn(move || {
            #[cfg(windows)]
            {
                while RUNNING.load(Ordering::SeqCst) {
                    if let Err(e) = run_wasapi_loopback() {
                        eprintln!("[aircast-loopback] WASAPI capture loop exited ({e}), holding stream with silence and re-probing...");
                        run_silence_for_duration(Duration::from_millis(1000));
                    }
                }
            }
            #[cfg(not(windows))]
            {
                run_fallback_silence_loop();
            }
        })
        .expect("failed to spawn aircast loopback thread");
}

#[allow(dead_code)]
pub fn stop_capture() {
    RUNNING.store(false, Ordering::SeqCst);
}

/// Run silence generator for a specific duration (used during device switching).
fn run_silence_for_duration(duration: Duration) {
    let sample_rate = sample_rate();
    let chunk_duration_ms = 40u64;
    let samples_per_chunk = (sample_rate as u64 * chunk_duration_ms / 1000) as usize;
    let bytes_per_chunk = samples_per_chunk * 4; // stereo 16-bit
    let silence = Arc::new(vec![0u8; bytes_per_chunk]);

    let start = Instant::now();
    while RUNNING.load(Ordering::SeqCst) && start.elapsed() < duration {
        let _ = SENDER.send(silence.clone());
        std::thread::sleep(Duration::from_millis(chunk_duration_ms));
    }
}

/// Continuous fallback silence loop used on non-Windows or when no audio hardware is present.
#[allow(dead_code)]
fn run_fallback_silence_loop() {
    let sample_rate = sample_rate();
    let channels = 2u16;
    let chunk_duration_ms = 40u64;
    let samples_per_chunk = (sample_rate as u64 * chunk_duration_ms / 1000) as usize;
    let bytes_per_chunk = samples_per_chunk * (channels as usize) * 2;
    let silence = Arc::new(vec![0u8; bytes_per_chunk]);

    while RUNNING.load(Ordering::SeqCst) {
        let _ = SENDER.send(silence.clone());
        std::thread::sleep(Duration::from_millis(chunk_duration_ms));
    }
}

#[cfg(windows)]
fn run_wasapi_loopback() -> anyhow::Result<()> {
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| anyhow::anyhow!("CoCreateInstance MMDeviceEnumerator failed: {e}"))?;

        let device: IMMDevice = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| anyhow::anyhow!("GetDefaultAudioEndpoint failed: {e}"))?;

        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| anyhow::anyhow!("Activate IAudioClient failed: {e}"))?;

        let format_ptr = client
            .GetMixFormat()
            .map_err(|e| anyhow::anyhow!("GetMixFormat failed: {e}"))?;
        let format = &*format_ptr;

        let sample_rate = format.nSamplesPerSec;
        let channels = format.nChannels;
        let bits_per_sample = format.wBitsPerSample;

        ACTIVE_SAMPLE_RATE.store(sample_rate, Ordering::SeqCst);

        const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: windows::core::GUID =
            windows::core::GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

        // Is it IEEE Float (tag 3 or extensible float)?
        let is_float = if format.wFormatTag == 3 {
            true
        } else if format.wFormatTag == 0xFFFE {
            let ext = &*(format_ptr as *const WAVEFORMATEXTENSIBLE);
            let sub_format = std::ptr::addr_of!(ext.SubFormat).read_unaligned();
            sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        } else {
            false
        };

        // 100ns units -> 200ms buffer
        let buffer_duration = 2_000_000i64;
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                buffer_duration,
                0,
                format_ptr,
                None,
            )
            .map_err(|e| anyhow::anyhow!("IAudioClient.Initialize loopback failed: {e}"))?;

        let capture_client: IAudioCaptureClient = client
            .GetService()
            .map_err(|e| anyhow::anyhow!("GetService IAudioCaptureClient failed: {e}"))?;

        client
            .Start()
            .map_err(|e| anyhow::anyhow!("IAudioClient.Start failed: {e}"))?;

        // Periodic 20ms silence chunk to inject when loopback produces 0 packets
        let silence_frames = (sample_rate as usize * 20) / 1000;
        let silence_bytes = silence_frames * 2 * 2; // stereo 16-bit
        let silence_chunk = Arc::new(vec![0u8; silence_bytes]);

        let mut last_packet_time = Instant::now();

        while RUNNING.load(Ordering::SeqCst) {
            let packet_size = match capture_client.GetNextPacketSize() {
                Ok(sz) => sz,
                Err(e) => {
                    eprintln!("[aircast-loopback] GetNextPacketSize error: {e}");
                    let _ = client.Stop();
                    CoTaskMemFree(Some(format_ptr as *const _));
                    CoUninitialize();
                    return Err(anyhow::anyhow!("Audio device invalidated: {e}"));
                }
            };

            if packet_size == 0 {
                // If silence for more than 40ms, inject silence frame to keep HTTP stream alive!
                if last_packet_time.elapsed() >= Duration::from_millis(40) {
                    let _ = SENDER.send(silence_chunk.clone());
                    last_packet_time = Instant::now();
                }
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }

            // Read all available packets
            let mut current_packet_size = packet_size;
            while current_packet_size > 0 {
                let mut data_ptr = std::ptr::null_mut();
                let mut num_frames = 0u32;
                let mut flags = 0u32;

                if capture_client
                    .GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None)
                    .is_ok()
                {
                    if num_frames > 0 && !data_ptr.is_null() {
                        let is_silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
                        let mut pcm_stereo_16 = Vec::with_capacity((num_frames as usize) * 4);

                        if is_silent {
                            pcm_stereo_16.resize((num_frames as usize) * 4, 0);
                        } else if is_float && bits_per_sample == 32 {
                            let float_slice = std::slice::from_raw_parts(
                                data_ptr as *const f32,
                                (num_frames as usize) * (channels as usize),
                            );
                            for frame in float_slice.chunks_exact(channels as usize) {
                                let (left, right) = downmix_float_to_i16_stereo(frame, channels as usize);
                                pcm_stereo_16.extend_from_slice(&left.to_le_bytes());
                                pcm_stereo_16.extend_from_slice(&right.to_le_bytes());
                            }
                        } else if bits_per_sample == 16 {
                            let i16_slice = std::slice::from_raw_parts(
                                data_ptr as *const i16,
                                (num_frames as usize) * (channels as usize),
                            );
                            for frame in i16_slice.chunks_exact(channels as usize) {
                                let (left, right) = downmix_i16_to_i16_stereo(frame, channels as usize);
                                pcm_stereo_16.extend_from_slice(&left.to_le_bytes());
                                pcm_stereo_16.extend_from_slice(&right.to_le_bytes());
                            }
                        }

                        if !pcm_stereo_16.is_empty() {
                            let _ = SENDER.send(Arc::new(pcm_stereo_16));
                            last_packet_time = Instant::now();
                        }
                    }

                    let _ = capture_client.ReleaseBuffer(num_frames);
                }

                current_packet_size = capture_client.GetNextPacketSize().unwrap_or(0);
            }

            std::thread::sleep(Duration::from_millis(5));
        }

        let _ = client.Stop();
        CoTaskMemFree(Some(format_ptr as *const _));
        CoUninitialize();
    }

    Ok(())
}
