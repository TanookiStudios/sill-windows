//! The PC's sound on the device (kind 43): what Windows plays, captured with WASAPI loopback, encoded
//! as AAC-ELD (48 kHz stereo, 480 frames a packet, 128 kbps) and sent as Audio.swift lays it out.
//! Only while Send Audio is on and a device's hello lists "aac-eld". Windows has no AAC-ELD encoder
//! of its own, so this uses Fraunhofer's FDK AAC library. Windows delivers nothing while nothing
//! plays; a gap like that ends a segment, and the next sound starts a new one.

use crate::protocol::{self as p, kind};
use crate::server::Hub;
use fdk_aac_sys as fdk;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const FRAMES: usize = 480;
const RATE: u32 = 48_000;
const BITRATE: u32 = 128_000;
/// A silence this long in what Windows delivers ends a segment.
const GAP: Duration = Duration::from_millis(50);

/// One AAC-ELD encoder.
pub struct Eld {
    h: fdk::HANDLE_AACENCODER,
    pub cookie: Vec<u8>,
    pub priming: u32,
    out: Vec<u8>,
}

// The handle is used from one thread at a time.
unsafe impl Send for Eld {}

impl Eld {
    pub fn new() -> Result<Eld, String> {
        unsafe {
            let mut h: fdk::HANDLE_AACENCODER = std::ptr::null_mut();
            if fdk::aacEncOpen(&mut h, 0, 2) != fdk::AACENC_ERROR_AACENC_OK {
                return Err("the AAC encoder didn't open".into());
            }
            let mut e = Eld { h, cookie: Vec::new(), priming: 0, out: vec![0; 2048] };
            for (param, value) in [
                (fdk::AACENC_PARAM_AACENC_AOT, 39), // AAC-ELD
                (fdk::AACENC_PARAM_AACENC_SBR_MODE, 0),
                (fdk::AACENC_PARAM_AACENC_SAMPLERATE, RATE),
                (fdk::AACENC_PARAM_AACENC_CHANNELMODE, 2),
                (fdk::AACENC_PARAM_AACENC_CHANNELORDER, 1),
                (fdk::AACENC_PARAM_AACENC_BITRATE, BITRATE),
                (fdk::AACENC_PARAM_AACENC_GRANULE_LENGTH, FRAMES as u32),
                (fdk::AACENC_PARAM_AACENC_TRANSMUX, 0), // raw packets
                (fdk::AACENC_PARAM_AACENC_AFTERBURNER, 1),
            ] {
                if fdk::aacEncoder_SetParam(h, param, value) != fdk::AACENC_ERROR_AACENC_OK {
                    return Err(format!("the AAC encoder refused setting {param}"));
                }
            }
            if fdk::aacEncEncode(h, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null_mut()) != fdk::AACENC_ERROR_AACENC_OK {
                return Err("the AAC encoder didn't start".into());
            }
            let mut info: fdk::AACENC_InfoStruct = std::mem::zeroed();
            if fdk::aacEncInfo(h, &mut info) != fdk::AACENC_ERROR_AACENC_OK {
                return Err("the AAC encoder gave no details".into());
            }
            e.cookie = info.confBuf[..info.confSize as usize].to_vec();
            e.priming = info.nDelay;
            Ok(e)
        }
    }

    /// One block of `FRAMES` stereo frames. None until the encoder has a whole packet.
    pub fn encode(&mut self, pcm: &[i16]) -> Option<Vec<u8>> {
        unsafe {
            let mut in_ptr = pcm.as_ptr() as *mut std::os::raw::c_void;
            let (mut in_id, mut in_size, mut in_el) = (fdk::AACENC_BufferIdentifier_IN_AUDIO_DATA as i32, (pcm.len() * 2) as i32, 2i32);
            let in_desc = fdk::AACENC_BufDesc { numBufs: 1, bufs: &mut in_ptr, bufferIdentifiers: &mut in_id, bufSizes: &mut in_size, bufElSizes: &mut in_el };
            let mut out_ptr = self.out.as_mut_ptr() as *mut std::os::raw::c_void;
            let (mut out_id, mut out_size, mut out_el) = (fdk::AACENC_BufferIdentifier_OUT_BITSTREAM_DATA as i32, self.out.len() as i32, 1i32);
            let out_desc = fdk::AACENC_BufDesc { numBufs: 1, bufs: &mut out_ptr, bufferIdentifiers: &mut out_id, bufSizes: &mut out_size, bufElSizes: &mut out_el };
            let args = fdk::AACENC_InArgs { numInSamples: pcm.len() as i32, numAncBytes: 0 };
            let mut out: fdk::AACENC_OutArgs = std::mem::zeroed();
            if fdk::aacEncEncode(self.h, &in_desc, &out_desc, &args, &mut out) != fdk::AACENC_ERROR_AACENC_OK || out.numOutBytes <= 0 {
                return None;
            }
            Some(self.out[..out.numOutBytes as usize].to_vec())
        }
    }
}

impl Drop for Eld {
    fn drop(&mut self) {
        unsafe {
            fdk::aacEncClose(&mut self.h);
        }
    }
}

/// The magic cookie as Apple's decoder takes it: the AudioSpecificConfig inside an MPEG-4 elementary
/// stream descriptor, laid out as Apple's own AAC-ELD encoder writes it (a bare config is refused).
pub fn apple_cookie(config: &[u8]) -> Vec<u8> {
    fn descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
        [&[tag, 0x80, 0x80, 0x80, body.len() as u8][..], body].concat()
    }
    let decoder_info = descriptor(5, config);
    // MPEG-4 audio, audio stream, a 6 KB buffer, 128 kbps at most and on average.
    let decoder_config = descriptor(4, &[&[0x40, 0x14, 0x00, 0x18, 0x00, 0x00, 0x01, 0xf4, 0x00, 0x00, 0x01, 0xf4, 0x00][..], &decoder_info].concat());
    let sync = descriptor(6, &[2]);
    descriptor(3, &[&[0, 0, 0][..], &decoder_config, &sync].concat())
}

/// Kind 43, type 1: how to decode what follows.
pub fn format_message(epoch: u16, cookie: &[u8], priming: u32) -> Vec<u8> {
    use base64::Engine;
    let f = serde_json::json!({
        "codec": "aac-eld", "sampleRate": RATE, "channels": 2, "framesPerPacket": FRAMES, "primingFrames": priming,
        "bitrate": BITRATE, "epoch": epoch, "cookie": base64::engine::general_purpose::STANDARD.encode(apple_cookie(cookie)), "source": "desktop",
    });
    let mut d = vec![1u8];
    d.extend_from_slice(&serde_json::to_vec(&f).unwrap_or_default());
    p::message(kind::AUDIO, p::now(), false, &d)
}

/// Kind 43, type 2: one packet. `time` is the wall clock of its first decoded sample.
pub fn packet_message(epoch: u16, seq: u32, segment_start: bool, packet: &[u8], time: f64) -> Vec<u8> {
    let mut d = Vec::with_capacity(11 + packet.len());
    d.push(2);
    d.extend_from_slice(&epoch.to_be_bytes());
    d.extend_from_slice(&seq.to_be_bytes());
    d.push(segment_start as u8);
    d.push(1);
    d.extend_from_slice(&(packet.len() as u16).to_be_bytes());
    d.extend_from_slice(packet);
    p::message(kind::AUDIO, time, false, &d)
}

/// Interleaved floats to the encoder's 16-bit blocks.
fn to_i16(samples: &[f32]) -> Vec<i16> {
    samples.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect()
}

/// Runs while a device wants sound.
pub fn start(hub: Arc<Hub>) {
    std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            let mut epoch = 0u16;
            loop {
                std::thread::sleep(Duration::from_millis(200));
                if !hub.audio_wanted() {
                    // Off: no note about sound that nobody asked for.
                    if hub.audio_note.lock().unwrap().take().is_some() {
                        hub.stream_changed.store(true, Ordering::SeqCst);
                    }
                    continue;
                }
                epoch = epoch.wrapping_add(1);
                let result = run(&hub, epoch);
                *hub.audio_note.lock().unwrap() = result.as_ref().err().cloned();
                hub.stream_changed.store(true, Ordering::SeqCst);
                if let Err(e) = result {
                    crate::warn!("Sound: {e}.");
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
        })
        .expect("audio thread");
}

fn run(hub: &Hub, epoch: u16) -> Result<(), String> {
    let mut capture = platform::Capture::open()?;
    let mut encoder = Eld::new()?;
    let mut format = Arc::new(format_message(epoch, &encoder.cookie, encoder.priming));
    let (mut pending, mut seq, mut segment_start) = (Vec::<f32>::new(), 0u32, true);
    let mut last_data = Instant::now();
    crate::say!("Sending sound to devices.");
    let mut looked = Instant::now();
    while hub.audio_wanted() {
        // Someone picked another output in Windows: follow it.
        if looked.elapsed() > Duration::from_secs(3) {
            looked = Instant::now();
            if capture.output_changed() {
                return Ok(());
            }
        }
        let chunk = capture.read()?;
        if chunk.is_empty() {
            if last_data.elapsed() > GAP && !segment_start {
                // Nothing playing: what's short of a block is dropped, and the next sound starts clean.
                pending.clear();
                encoder = Eld::new()?;
                format = Arc::new(format_message(epoch, &encoder.cookie, encoder.priming));
                segment_start = true;
            }
            std::thread::sleep(Duration::from_millis(3));
            continue;
        }
        last_data = Instant::now();
        pending.extend_from_slice(&chunk);
        while pending.len() >= FRAMES * 2 {
            let block: Vec<f32> = pending.drain(..FRAMES * 2).collect();
            if let Some(packet) = encoder.encode(&to_i16(&block)) {
                let late = pending.len() as f64 / 2.0 / RATE as f64 + (FRAMES as f64 + encoder.priming as f64) / RATE as f64;
                hub.send_audio(&format, epoch, packet_message(epoch, seq, segment_start, &packet, p::now() - late));
                seq = seq.wrapping_add(1);
                segment_start = false;
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
mod platform {
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;

    pub struct Capture {
        client: IAudioClient,
        capture: IAudioCaptureClient,
        devices: IMMDeviceEnumerator,
        output: String,
        channels: usize,
        bits: u16,
        rate: f64,
        /// Where the next output frame falls in the input, for a mix that isn't 48 kHz.
        phase: f64,
        last: [f32; 2],
    }

    unsafe impl Send for Capture {}

    impl Capture {
        pub fn open() -> Result<Capture, String> {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let e = |what: &'static str| move |err: windows::core::Error| format!("{what}: {err}");
                let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(e("the sound system didn't answer"))?;
                let device = devices.GetDefaultAudioEndpoint(eRender, eConsole).map_err(e("this PC has no sound output"))?;
                let output = output_id(&device);
                let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(e("the sound output wouldn't open"))?;
                let format = client.GetMixFormat().map_err(e("the sound format is unknown"))?;
                let (channels, rate, bits) = ((*format).nChannels as usize, (*format).nSamplesPerSec as f64, (*format).wBitsPerSample);
                let started = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 200_000, 0, format, None);
                CoTaskMemFree(Some(format as *const _ as _));
                started.map_err(e("sound capture wouldn't start"))?;
                let capture: IAudioCaptureClient = client.GetService().map_err(e("sound capture wouldn't start"))?;
                client.Start().map_err(e("sound capture wouldn't start"))?;
                Ok(Capture { client, capture, devices, output, channels, bits, rate, phase: 0.0, last: [0.0; 2] })
            }
        }

        pub fn output_changed(&self) -> bool {
            unsafe { self.devices.GetDefaultAudioEndpoint(eRender, eConsole).map(|d| output_id(&d) != self.output).unwrap_or(false) }
        }

        /// What's waiting, as interleaved stereo at 48 kHz; empty when nothing is.
        pub fn read(&mut self) -> Result<Vec<f32>, String> {
            let mut out = Vec::new();
            unsafe {
                loop {
                    let waiting = self.capture.GetNextPacketSize().map_err(|e| format!("sound capture stopped: {e}"))?;
                    if waiting == 0 {
                        return Ok(out);
                    }
                    let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                    self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None).map_err(|e| format!("sound capture stopped: {e}"))?;
                    let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                    let step = self.rate / super::RATE as f64;
                    for i in 0..frames as usize {
                        let frame = if silent {
                            [0.0, 0.0]
                        } else {
                            let at = |c: usize| {
                                let c = c.min(self.channels.saturating_sub(1));
                                if self.bits == 32 {
                                    *(data as *const f32).add(i * self.channels + c)
                                } else {
                                    *(data as *const i16).add(i * self.channels + c) as f32 / 32768.0
                                }
                            };
                            [at(0), at(1)]
                        };
                        // Straight-line resampling: plain, and close enough for a 44.1 kHz mix.
                        while self.phase <= 1.0 {
                            let t = self.phase as f32;
                            out.push(self.last[0] + (frame[0] - self.last[0]) * t);
                            out.push(self.last[1] + (frame[1] - self.last[1]) * t);
                            self.phase += step;
                        }
                        self.phase -= 1.0;
                        self.last = frame;
                    }
                    self.capture.ReleaseBuffer(frames).map_err(|e| format!("sound capture stopped: {e}"))?;
                }
            }
        }
    }

    unsafe fn output_id(device: &IMMDevice) -> String {
        match device.GetId() {
            Ok(id) => {
                let text = id.to_string().unwrap_or_default();
                CoTaskMemFree(Some(id.0 as _));
                text
            }
            Err(_) => String::new(),
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            unsafe {
                let _ = self.client.Stop();
            }
        }
    }
}

/// Outside Windows (tests): with `SILL_TEST_TONE`, a 440 Hz tone with a pause every 2 seconds, on time.
#[cfg(not(windows))]
mod platform {
    pub struct Capture {
        started: std::time::Instant,
        sent: u64,
    }

    impl Capture {
        pub fn output_changed(&self) -> bool {
            false
        }

        pub fn open() -> Result<Capture, String> {
            if std::env::var_os("SILL_TEST_TONE").is_none() {
                return Err("no sound source on this system".into());
            }
            Ok(Capture { started: std::time::Instant::now(), sent: 0 })
        }

        pub fn read(&mut self) -> Result<Vec<f32>, String> {
            let due = (self.started.elapsed().as_secs_f64() * 48_000.0) as u64;
            let mut out = Vec::new();
            while self.sent < due {
                let t = self.sent as f64 / 48_000.0;
                self.sent += 1;
                if t % 2.0 > 1.8 {
                    continue;
                }
                let s = (t * 440.0 * std::f64::consts::TAU).sin() as f32 * 0.3;
                out.extend_from_slice(&[s, s]);
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_aac_eld() {
        let mut e = Eld::new().unwrap();
        assert!(!e.cookie.is_empty(), "an AudioSpecificConfig");
        // AOT 39 is the escape 31 then 7: its first bits are 11111 000111.
        assert_eq!((e.cookie[0] >> 3, ((e.cookie[0] as u16 & 7) << 3) | (e.cookie[1] as u16 >> 5)), (31, 7), "AAC-ELD: {:02x?}", e.cookie);
        let block: Vec<i16> = (0..FRAMES * 2).map(|i| ((i as f32 / 40.0).sin() * 8000.0) as i16).collect();
        let packets: Vec<Vec<u8>> = (0..20).filter_map(|_| e.encode(&block)).collect();
        assert!(packets.len() >= 18, "{} packets", packets.len());
        let average = packets.iter().map(|p| p.len()).sum::<usize>() / packets.len();
        assert!((100..260).contains(&average), "about 160 bytes at 128 kbps: {average}");
    }

    #[test]
    fn cookie_is_apples() {
        // Written by Apple's AudioConverter for 48 kHz stereo AAC-ELD at 480 frames and 128 kbps.
        let apple = "0380808024000000048080801640140018000001f4000001f4000580808004f8e65000068080800102";
        let mine: String = apple_cookie(&[0xf8, 0xe6, 0x50, 0x00]).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(mine, apple);
        assert_eq!(Eld::new().unwrap().cookie, [0xf8, 0xe6, 0x50, 0x00], "the same config as Apple's encoder");
    }

    #[test]
    fn packet_layout() {
        let m = packet_message(7, 9, true, &[1, 2, 3], 5.0);
        assert_eq!(&m[14..], &[2, 0, 7, 0, 0, 0, 9, 1, 1, 0, 3, 1, 2, 3]);
    }
}
