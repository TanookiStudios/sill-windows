//! Windows capture and encode: DXGI Desktop Duplication of the main monitor, the GPU's video
//! processor for BGRA → NV12 (and any scaling), and the hardware HEVC encoder through Media
//! Foundation. Everything stays on the GPU until the encoded bytes come out.
//!
//! The loop, on one thread: take a new desktop image when there is one (Desktop Duplication only
//! delivers changes), answer the encoder's events (it asks for input and hands back output in its
//! own time: hardware encoders are asynchronous), and encode the latest image when it changed or a
//! device needs a keyframe, at most `fps` times a second. A still screen sends nothing until a
//! device asks for a keyframe; then the last image is encoded again as one.

use super::{access_unit, Settings};
use crate::input::Rect;
use crate::server::Hub;
use std::mem::ManuallyDrop;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::{Interface, VARIANT};
use windows::Win32::Foundation::{BOOL, HMODULE, RECT, TRUE};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

type Result<T> = std::result::Result<T, String>;

fn err<E: std::fmt::Display>(what: &'static str) -> impl Fn(E) -> String {
    move |e| format!("{what}: {e}")
}

pub fn start(hub: Arc<Hub>, settings: Settings, rect: Arc<Mutex<Rect>>) {
    std::thread::Builder::new()
        .name("video".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                    eprintln!("Media Foundation didn't start ({e}); no picture can be sent.");
                    return;
                }
            }
            let mut last_error = String::new();
            loop {
                if hub.client_count() == 0 {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                match Pipeline::new(&settings, &hub, &rect).and_then(|mut p| p.run(&hub)) {
                    Ok(()) => {}
                    Err(e) => {
                        if e != last_error {
                            eprintln!("Picture: {e}. Trying again…");
                            last_error = e;
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        })
        .expect("video thread");
}

struct Pipeline {
    /// Kept alive for the encoder and the video processor, which use it.
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    /// The latest desktop image (BGRA, desktop size).
    desktop: ID3D11Texture2D,
    video_context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    input_view: ID3D11VideoProcessorInputView,
    /// NV12 surfaces for the encoder, used in turn so one being encoded is never overwritten.
    surfaces: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next_surface: usize,
    encoder: Encoder,
    fps: u32,
}

impl Pipeline {
    fn new(settings: &Settings, hub: &Hub, rect: &Mutex<Rect>) -> Result<Pipeline> {
        unsafe {
            let (adapter, output, desc) = main_output()?;
            let r = desc.DesktopCoordinates;
            *rect.lock().unwrap() = Rect { left: r.left, top: r.top, width: r.right - r.left, height: r.bottom - r.top };

            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                &adapter, D3D_DRIVER_TYPE_UNKNOWN, HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]), D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut context),
            )
            .map_err(err("the graphics device didn't start"))?;
            let device = device.ok_or("no graphics device")?;
            let context = context.ok_or("no graphics context")?;
            // The encoder uses the device from its own threads.
            if let Ok(mt) = device.cast::<ID3D11Multithread>() {
                let _ = mt.SetMultithreadProtected(TRUE);
            }

            let output1: IDXGIOutput1 = output.cast().map_err(err("this Windows can't capture the screen"))?;
            let duplication = output1.DuplicateOutput(&device).map_err(err(
                "couldn't capture the screen (another app may be capturing it, or the lock screen or an administrator prompt is up)",
            ))?;
            let dd = duplication.GetDesc();
            let (dw, dh) = (dd.ModeDesc.Width, dd.ModeDesc.Height);

            // The picture: the desktop, scaled down so its longer side is at most max_side, even sizes.
            let scale = (settings.max_side as f64 / dw.max(dh) as f64).min(1.0);
            let width = ((dw as f64 * scale) as u32) & !1;
            let height = ((dh as f64 * scale) as u32) & !1;

            let desktop = texture(&device, dw, dh, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_RENDER_TARGET)?;
            let video_device: ID3D11VideoDevice = device.cast().map_err(err("no video processor"))?;
            let video_context: ID3D11VideoContext = context.cast().map_err(err("no video processor"))?;
            let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: settings.fps, Denominator: 1 },
                InputWidth: dw,
                InputHeight: dh,
                OutputFrameRate: DXGI_RATIONAL { Numerator: settings.fps, Denominator: 1 },
                OutputWidth: width,
                OutputHeight: height,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let processor_enum = video_device.CreateVideoProcessorEnumerator(&content).map_err(err("no video processor"))?;
            let processor = video_device.CreateVideoProcessor(&processor_enum, 0).map_err(err("no video processor"))?;
            // Full-range RGB in; BT.709 limited-range YCbCr out, which the encoder's stream says too.
            video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 });
            video_context.VideoProcessorSetOutputColorSpace(&processor, &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (1 << 4) });
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            let mut input_view = None;
            video_device
                .CreateVideoProcessorInputView(
                    &desktop, &processor_enum,
                    &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                        FourCC: 0,
                        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
                    },
                    Some(&mut input_view),
                )
                .map_err(err("video processor input"))?;
            let mut surfaces = Vec::new();
            for _ in 0..8 {
                let t = texture(&device, width, height, DXGI_FORMAT_NV12, D3D11_BIND_RENDER_TARGET)?;
                let mut view = None;
                video_device
                    .CreateVideoProcessorOutputView(
                        &t, &processor_enum,
                        &D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
                        },
                        Some(&mut view),
                    )
                    .map_err(err("video processor output"))?;
                surfaces.push((t, view.ok_or("video processor output")?));
            }

            let bitrate = hub.bitrate.load(Ordering::SeqCst) as u32;
            let encoder = Encoder::new(&device, &adapter, width, height, settings.fps, bitrate)?;
            *hub.stream.lock().unwrap() = Some(crate::protocol::RunningStream {
                width: width as i64,
                height: height as i64,
                fps: settings.fps as i64,
                mbps: (bitrate / 1_000_000) as i64,
                on_virtual_display: false,
            });
            println!("Streaming the screen: {dw}×{dh} as {width}×{height} at up to {} fps, HEVC on {}.", settings.fps, encoder.name);
            Ok(Pipeline {
                _device: device, context, duplication, desktop, video_context, processor,
                input_view: input_view.ok_or("video processor input")?, surfaces, next_surface: 0, encoder,
                fps: settings.fps,
            })
        }
    }

    /// Runs until nobody is connected (Ok) or the capture or encoder fails (Err: a new pipeline).
    fn run(&mut self, hub: &Hub) -> Result<()> {
        let interval = Duration::from_secs_f64(1.0 / self.fps as f64);
        let mut next_slot = Instant::now();
        let mut have_image = false;
        let mut dirty = false;
        let mut timestamp: i64 = 0;
        while hub.client_count() > 0 {
            // 1. A new desktop image, if there is one (waits up to 4 ms).
            unsafe {
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut resource: Option<IDXGIResource> = None;
                match self.duplication.AcquireNextFrame(4, &mut info, &mut resource) {
                    Ok(()) => {
                        if info.LastPresentTime != 0 || !have_image {
                            if let Some(r) = resource {
                                let t: ID3D11Texture2D = r.cast().map_err(err("desktop image"))?;
                                self.context.CopyResource(&self.desktop, &t);
                                have_image = true;
                                dirty = true;
                            }
                        }
                        let _ = self.duplication.ReleaseFrame();
                    }
                    Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
                    Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => return Err("the screen changed (resolution, lock screen or an administrator prompt)".into()),
                    Err(e) => return Err(format!("screen capture stopped: {e}")),
                }
            }
            // 2. What the encoder said meanwhile.
            unsafe { self.encoder.pump(hub)? };
            // 3. Encode, when there is something to send and the encoder takes input.
            if have_image && self.encoder.wants_input > 0 && Instant::now() >= next_slot {
                let key = hub.take_keyframe_request();
                if dirty || key {
                    self.encode(timestamp, key)?;
                    timestamp += 10_000_000 / self.fps as i64;
                    dirty = false;
                    next_slot = (next_slot + interval).max(Instant::now());
                }
            }
        }
        Ok(())
    }

    fn encode(&mut self, timestamp: i64, keyframe: bool) -> Result<()> {
        unsafe {
            let (surface, view) = &self.surfaces[self.next_surface];
            self.next_surface = (self.next_surface + 1) % self.surfaces.len();
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: TRUE,
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: ManuallyDrop::new(Some(self.input_view.clone())),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };
            let mut streams = [stream];
            let blt = self.video_context.VideoProcessorBlt(&self.processor, view, 0, &streams);
            ManuallyDrop::drop(&mut streams[0].pInputSurface);
            blt.map_err(err("color conversion"))?;
            self.encoder.encode(surface, timestamp, 10_000_000 / self.fps as i64, keyframe)
        }
    }
}

unsafe fn texture(device: &ID3D11Device, w: u32, h: u32, format: DXGI_FORMAT, bind: D3D11_BIND_FLAG) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut t = None;
    device.CreateTexture2D(&desc, None, Some(&mut t)).map_err(err("couldn't make a video surface"))?;
    t.ok_or_else(|| "couldn't make a video surface".to_string())
}

/// The monitor whose top-left corner is the desktop's origin: Windows' main display.
unsafe fn main_output() -> Result<(IDXGIAdapter1, IDXGIOutput, DXGI_OUTPUT_DESC)> {
    let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(err("DXGI didn't start"))?;
    let mut first = None;
    let mut a = 0;
    while let Ok(adapter) = factory.EnumAdapters1(a) {
        let mut o = 0;
        while let Ok(output) = adapter.EnumOutputs(o) {
            let desc = output.GetDesc().map_err(err("display description"))?;
            if desc.AttachedToDesktop.as_bool() {
                let RECT { left, top, .. } = desc.DesktopCoordinates;
                if left == 0 && top == 0 {
                    return Ok((adapter, output, desc));
                }
                if first.is_none() {
                    first = Some((adapter.clone(), output, desc));
                }
            }
            o += 1;
        }
        a += 1;
    }
    first.ok_or_else(|| "no display is connected".to_string())
}

// ---- The encoder --------------------------------------------------------------------------------

struct Encoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: Option<ICodecAPI>,
    /// Input requests not answered yet (the encoder's NeedInput events).
    wants_input: u32,
    provides_samples: bool,
    output_size: u32,
    /// The latest VPS, SPS and PPS, for a keyframe that comes without them.
    parameter_sets: Option<Vec<Vec<u8>>>,
    name: String,
    _manager: IMFDXGIDeviceManager,
}

const NEED_INPUT: u32 = 601; // METransformNeedInput
const HAVE_OUTPUT: u32 = 602; // METransformHaveOutput

impl Encoder {
    unsafe fn new(device: &ID3D11Device, adapter: &IDXGIAdapter1, width: u32, height: u32, fps: u32, bitrate: u32) -> Result<Encoder> {
        let mut token = 0u32;
        let mut manager = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(err("Media Foundation device manager"))?;
        let manager = manager.ok_or("Media Foundation device manager")?;
        manager.ResetDevice(device, token).map_err(err("Media Foundation device manager"))?;

        let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
        let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_HEVC };
        let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input), Some(&output), &mut list, &mut count,
        )
        .map_err(err("couldn't list the HEVC encoders"))?;
        let activates: Vec<IMFActivate> =
            if list.is_null() { vec![] } else { std::slice::from_raw_parts_mut(list, count as usize).iter_mut().filter_map(|a| a.take()).collect() };
        if !list.is_null() {
            CoTaskMemFree(Some(list as _));
        }
        if activates.is_empty() {
            return Err("this PC's graphics chip has no hardware HEVC (H.265) encoder, which Sill needs".into());
        }
        let luid = adapter.GetDesc1().map(|d| d.AdapterLuid).ok();
        let mut problems = Vec::new();
        // The encoder on the capturing graphics chip first (on a laptop with two, the other can't use its surfaces).
        let mut ordered: Vec<&IMFActivate> = activates.iter().collect();
        ordered.sort_by_key(|a| {
            let on_adapter = a.GetBlobSize(&MFT_ENUM_ADAPTER_LUID).is_ok() && {
                let mut b = [0u8; 8];
                a.GetBlob(&MFT_ENUM_ADAPTER_LUID, &mut b, None).is_ok()
                    && luid.map(|l| b == [l.LowPart.to_le_bytes(), (l.HighPart as u32).to_le_bytes()].concat()[..]).unwrap_or(false)
            };
            !on_adapter
        });
        for activate in ordered {
            let name = friendly_name(activate);
            match Self::configure(activate, &manager, width, height, fps, bitrate) {
                Ok((transform, events, codec, provides_samples, output_size)) => {
                    return Ok(Encoder {
                        transform, events, codec, wants_input: 0, provides_samples, output_size,
                        parameter_sets: None, name, _manager: manager,
                    })
                }
                Err(e) => problems.push(format!("{name}: {e}")),
            }
        }
        Err(format!("no HEVC encoder would start ({})", problems.join("; ")))
    }

    #[allow(clippy::type_complexity)]
    unsafe fn configure(
        activate: &IMFActivate, manager: &IMFDXGIDeviceManager, width: u32, height: u32, fps: u32, bitrate: u32,
    ) -> Result<(IMFTransform, IMFMediaEventGenerator, Option<ICodecAPI>, bool, u32)> {
        let transform: IMFTransform = activate.ActivateObject().map_err(err("activate"))?;
        let attributes = transform.GetAttributes().map_err(err("attributes"))?;
        // Hardware encoders are asynchronous: they ask for input and announce output as events.
        if attributes.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 0 {
            return Err("not an asynchronous (hardware) encoder".into());
        }
        attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).map_err(err("unlock"))?;
        let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
        transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize).map_err(err("Direct3D"))?;

        let size = ((width as u64) << 32) | height as u64;
        let rate = ((fps as u64) << 32) | 1;
        let out = MFCreateMediaType().map_err(err("media type"))?;
        out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(err("output type"))?;
        out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_HEVC).map_err(err("output type"))?;
        out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate).map_err(err("output type"))?;
        out.SetUINT64(&MF_MT_FRAME_SIZE, size).map_err(err("output type"))?;
        out.SetUINT64(&MF_MT_FRAME_RATE, rate).map_err(err("output type"))?;
        out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1).map_err(err("output type"))?;
        out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(err("output type"))?;
        out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH265VProfile_Main_420_8.0 as u32).map_err(err("output type"))?;
        color(&out);
        transform.SetOutputType(0, &out, 0).map_err(err("the encoder refused the HEVC format"))?;

        let inp = MFCreateMediaType().map_err(err("media type"))?;
        inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(err("input type"))?;
        inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).map_err(err("input type"))?;
        inp.SetUINT64(&MF_MT_FRAME_SIZE, size).map_err(err("input type"))?;
        inp.SetUINT64(&MF_MT_FRAME_RATE, rate).map_err(err("input type"))?;
        inp.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1).map_err(err("input type"))?;
        inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(err("input type"))?;
        color(&inp);
        transform.SetInputType(0, &inp, 0).map_err(err("the encoder refused the picture format"))?;

        // Real time: no B-frames, low latency, a steady rate, a keyframe only when asked (or every 4 s).
        let codec = transform.cast::<ICodecAPI>().ok();
        if let Some(c) = &codec {
            let _ = c.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
            let _ = c.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &VARIANT::from(0u32));
            let _ = c.SetValue(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(fps * 4));
            let _ = c.SetValue(&CODECAPI_AVEncCommonRateControlMode, &VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32));
            let _ = c.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &VARIANT::from(bitrate));
        }
        let info = transform.GetOutputStreamInfo(0).map_err(err("output stream"))?;
        let provides = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32) != 0;
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(err("start"))?;
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(err("start"))?;
        let events = transform.cast::<IMFMediaEventGenerator>().map_err(err("events"))?;
        Ok((transform, events, codec, provides, info.cbSize.max(1 << 20)))
    }

    /// Handles every event the encoder has queued, without waiting.
    unsafe fn pump(&mut self, hub: &Hub) -> Result<()> {
        let events = self.events.clone();
        loop {
            let event = match events.GetEvent(MF_EVENT_FLAG_NO_WAIT) {
                Ok(e) => e,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(e) => return Err(format!("the encoder stopped: {e}")),
            };
            match event.GetType().map_err(err("encoder event"))? {
                NEED_INPUT => self.wants_input += 1,
                HAVE_OUTPUT => self.output(hub)?,
                _ => {}
            }
        }
    }

    unsafe fn encode(&mut self, surface: &ID3D11Texture2D, time: i64, duration: i64, keyframe: bool) -> Result<()> {
        if keyframe {
            if let Some(c) = &self.codec {
                let _ = c.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32));
            }
        }
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, surface, 0, BOOL(0)).map_err(err("encoder input"))?;
        if let Ok(b2) = buffer.cast::<IMF2DBuffer>() {
            if let Ok(len) = b2.GetContiguousLength() {
                let _ = buffer.SetCurrentLength(len);
            }
        }
        let sample = MFCreateSample().map_err(err("encoder input"))?;
        sample.AddBuffer(&buffer).map_err(err("encoder input"))?;
        sample.SetSampleTime(time).map_err(err("encoder input"))?;
        sample.SetSampleDuration(duration).map_err(err("encoder input"))?;
        self.transform.ProcessInput(0, &sample, 0).map_err(err("the encoder refused a frame"))?;
        self.wants_input = self.wants_input.saturating_sub(1);
        Ok(())
    }

    /// Takes one encoded access unit and sends it to every device.
    unsafe fn output(&mut self, hub: &Hub) -> Result<()> {
        let sample = if self.provides_samples {
            None
        } else {
            let s = MFCreateSample().map_err(err("encoder output"))?;
            s.AddBuffer(&MFCreateMemoryBuffer(self.output_size).map_err(err("encoder output"))?).map_err(err("encoder output"))?;
            Some(s)
        };
        let mut buffers = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: ManuallyDrop::new(sample), dwStatus: 0, pEvents: ManuallyDrop::new(None) }];
        let mut status = 0u32;
        let r = self.transform.ProcessOutput(0, &mut buffers, &mut status);
        let sample = ManuallyDrop::take(&mut buffers[0].pSample);
        drop(ManuallyDrop::take(&mut buffers[0].pEvents));
        match r {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // The encoder settled its output format: take what it offers and go on.
                let t = self.transform.GetOutputAvailableType(0, 0).map_err(err("encoder format"))?;
                self.transform.SetOutputType(0, &t, 0).map_err(err("encoder format"))?;
                self.parameter_sets = None;
                return Ok(());
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
            Err(e) => return Err(format!("the encoder failed: {e}")),
        }
        let Some(sample) = sample else { return Ok(()) };
        let buffer = sample.ConvertToContiguousBuffer().map_err(err("encoder output"))?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut ptr, None, Some(&mut len)).map_err(err("encoder output"))?;
        let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
        let _ = buffer.Unlock();

        let au = access_unit(&bytes);
        if au.frame.is_empty() {
            return Ok(());
        }
        if let Some(ps) = au.parameter_sets {
            self.parameter_sets = Some(ps);
        } else if au.is_keyframe && self.parameter_sets.is_none() {
            self.parameter_sets = self.sequence_header();
        }
        let ps = if au.is_keyframe {
            match &self.parameter_sets {
                Some(sets) => Some(crate::protocol::encode_parameter_sets(4, sets)),
                None => return Err("the encoder's keyframe came without its parameter sets".into()),
            }
        } else {
            None
        };
        hub.broadcast_frame(&au.frame, au.is_keyframe, ps);
        Ok(())
    }

    /// VPS, SPS and PPS from the output type, for encoders that keep them out of the stream.
    unsafe fn sequence_header(&self) -> Option<Vec<Vec<u8>>> {
        let t = self.transform.GetOutputCurrentType(0).ok()?;
        let n = t.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()?;
        let mut blob = vec![0u8; n as usize];
        t.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None).ok()?;
        access_unit(&blob).parameter_sets
    }
}

/// BT.709, limited range: what the video processor writes and what the device should decode.
unsafe fn color(t: &IMFMediaType) {
    let _ = t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32);
    let _ = t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32);
    let _ = t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32);
    let _ = t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32);
}

unsafe fn friendly_name(a: &IMFActivate) -> String {
    let mut buf = [0u16; 256];
    let mut len = 0u32;
    if a.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, Some(&mut len)).is_ok() {
        return String::from_utf16_lossy(&buf[..len as usize]);
    }
    "the hardware encoder".into()
}
