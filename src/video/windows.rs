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
                    crate::warn!("Media Foundation didn't start ({e}); no picture can be sent.");
                    return;
                }
            }
            let mut last_error = String::new();
            loop {
                if !hub.wanted() {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                match Pipeline::new(&settings, &hub, &rect).and_then(|mut p| p.run(&hub, &rect)) {
                    Ok(()) => {}
                    Err(e) => {
                        *hub.video_error.lock().unwrap() = Some(e.clone());
                        if e != last_error {
                            crate::warn!("Picture: {e}. Trying again…");
                            last_error = e;
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        })
        .expect("video thread");
}

pub fn check_capture() {
    match unsafe { capture_one_image() } {
        Ok(text) => crate::say!("Screen capture works: {text}."),
        Err(e) => crate::warn!("Screen capture FAILED: {e}."),
    }
}

/// One desktop image from a plain Direct3D device, with none of the video parts.
unsafe fn capture_one_image() -> Result<String> {
    let (adapter, output, _) = find_output(None)?;
    let mut device = None;
    let mut context = None;
    D3D11CreateDevice(
        &adapter, D3D_DRIVER_TYPE_UNKNOWN, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        None, D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut context),
    )
    .map_err(err("the graphics device didn't start"))?;
    let (device, context) = (device.ok_or("no graphics device")?, context.ok_or("no graphics context")?);
    let output1: IDXGIOutput1 = output.cast().map_err(err("this Windows can't capture the screen"))?;
    let duplication = output1.DuplicateOutput(&device).map_err(err("the capture wouldn't start"))?;
    let dd = duplication.GetDesc();
    let copy = texture(&device, dd.ModeDesc.Width, dd.ModeDesc.Height, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_RENDER_TARGET)?;
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match duplication.AcquireNextFrame(100, &mut info, &mut resource) {
            Ok(()) => {
                if let Some(r) = resource {
                    let t: ID3D11Texture2D = r.cast().map_err(err("desktop image"))?;
                    context.CopyResource(&copy, &t);
                }
                let _ = duplication.ReleaseFrame();
                let name = adapter.GetDesc1().map(|d| String::from_utf16_lossy(&d.Description).trim_end_matches('\0').to_string()).unwrap_or_default();
                return Ok(format!("{}×{} from {name}", dd.ModeDesc.Width, dd.ModeDesc.Height));
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
            Err(e) => return Err(format!("the capture stopped: {e}")),
        }
    }
    Err("no image came in 3 seconds".into())
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
    /// The streamed window and where it was, when one streams.
    window: Option<(u32, RECT)>,
    /// The captured monitor on the desktop.
    monitor: RECT,
}

/// The part of `w` on the monitor.
fn clip(w: RECT, m: RECT) -> RECT {
    RECT { left: w.left.max(m.left), top: w.top.max(m.top), right: w.right.min(m.right), bottom: w.bottom.min(m.bottom) }
}

/// A rectangle on the desktop in the captured image's own pixels.
fn offset(c: RECT, m: RECT) -> RECT {
    RECT { left: c.left - m.left, top: c.top - m.top, right: c.right - m.left, bottom: c.bottom - m.top }
}

impl Pipeline {
    fn new(settings: &Settings, hub: &Hub, rect: &Mutex<Rect>) -> Result<Pipeline> {
        unsafe {
            // A window streams from whichever monitor holds its middle.
            let source = hub.source.lock().unwrap().clone();
            let wanted_bounds = match source {
                crate::protocol::StreamSource::Window { id } => crate::catalog::platform::bounds(id),
                _ => None,
            };
            let (adapter, output, desc) = find_output(wanted_bounds.map(|b| ((b.left + b.right) / 2, (b.top + b.bottom) / 2)))?;
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
            // The encoder uses the device from its own threads: Direct3D must lock around every call.
            let mt: ID3D11Multithread = context.cast().map_err(err("Direct3D multithread protection"))?;
            let _ = mt.SetMultithreadProtected(TRUE);

            let output1: IDXGIOutput1 = output.cast().map_err(err("this Windows can't capture the screen"))?;
            let duplication = output1.DuplicateOutput(&device).map_err(err(
                "couldn't capture the screen (another app may be capturing it, or the lock screen or an administrator prompt is up)",
            ))?;
            let dd = duplication.GetDesc();
            let (dw, dh) = (dd.ModeDesc.Width, dd.ModeDesc.Height);
            // A display turned to portrait delivers its image as the panel scans it out: turn it back.
            let turn = match dd.Rotation {
                DXGI_MODE_ROTATION_ROTATE90 => Some(D3D11_VIDEO_PROCESSOR_ROTATION_270),
                DXGI_MODE_ROTATION_ROTATE180 => Some(D3D11_VIDEO_PROCESSOR_ROTATION_180),
                DXGI_MODE_ROTATION_ROTATE270 => Some(D3D11_VIDEO_PROCESSOR_ROTATION_90),
                _ => None,
            };
            let (mut uw, mut uh) = if matches!(dd.Rotation, DXGI_MODE_ROTATION_ROTATE90 | DXGI_MODE_ROTATION_ROTATE270) { (dh, dw) } else { (dw, dh) };
            // A window streams as its part of the screen (not on a turned display, or one mostly off this monitor).
            let mut window = None;
            if let (crate::protocol::StreamSource::Window { id }, None) = (source, turn) {
                if let Some(c) = wanted_bounds.map(|b| clip(b, r)).filter(|c| c.right - c.left >= 64 && c.bottom - c.top >= 64) {
                    uw = (c.right - c.left) as u32;
                    uh = (c.bottom - c.top) as u32;
                    *rect.lock().unwrap() = Rect { left: c.left, top: c.top, width: c.right - c.left, height: c.bottom - c.top };
                    window = Some((id, c));
                }
            }

            // The picture: the desktop, scaled down so its longer side is at most max_side, even sizes.
            let wish = *hub.settings.lock().unwrap();
            let fps = wish.max_fps as u32;
            let longest = if wish.capture_scale >= 2.0 { settings.max_side } else { settings.max_side.min(uw.max(uh)) / 2 };
            let scale = (longest as f64 / uw.max(uh) as f64).min(1.0);
            // Encoders turn down tiny pictures, so a small window is scaled up a little.
            let width = (((uw as f64 * scale) as u32) & !1).max(256);
            let height = (((uh as f64 * scale) as u32) & !1).max(256);

            let desktop = texture(&device, dw, dh, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_RENDER_TARGET)?;
            let video_device: ID3D11VideoDevice = device.cast().map_err(err("no video processor"))?;
            let video_context: ID3D11VideoContext = context.cast().map_err(err("no video processor"))?;
            let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: fps, Denominator: 1 },
                InputWidth: dw,
                InputHeight: dh,
                OutputFrameRate: DXGI_RATIONAL { Numerator: fps, Denominator: 1 },
                OutputWidth: width,
                OutputHeight: height,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let processor_enum = video_device.CreateVideoProcessorEnumerator(&content).map_err(err("no video processor"))?;
            let processor = video_device.CreateVideoProcessor(&processor_enum, 0).map_err(err("no video processor"))?;
            // Without a destination rectangle a stream writes nothing at all (Microsoft's default is an
            // empty one), and the encoder would happily encode blank surfaces.
            let whole = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&whole));
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&whole));
            video_context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            if let Some((_, c)) = window {
                video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&offset(c, r)));
            }
            // Full-range RGB in; BT.709 limited-range YCbCr out. The stream itself leaves its colors
            // unspecified, as Chromium's does (setting them on the encoder has crashed AMD drivers),
            // which a decoder reads as BT.709 at HD sizes and up.
            video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 });
            video_context.VideoProcessorSetOutputColorSpace(&processor, &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (1 << 4) });
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            if let Some(turn) = turn {
                video_context.VideoProcessorSetStreamRotation(&processor, 0, true, turn);
            }
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
                // Some drivers want to be told a surface feeds the encoder; older ones don't know the flag.
                let to_encoder = D3D11_BIND_FLAG(D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_VIDEO_ENCODER.0);
                let t = texture(&device, width, height, DXGI_FORMAT_NV12, to_encoder)
                    .or_else(|_| texture(&device, width, height, DXGI_FORMAT_NV12, D3D11_BIND_RENDER_TARGET))?;
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

            let bitrate = (wish.bitrate as u64 * fps as u64 / 60) as u32;
            let encoder = Encoder::new(&device, &adapter, width, height, fps, bitrate)?;
            *hub.stream.lock().unwrap() = Some(crate::protocol::RunningStream {
                width: width as i64,
                height: height as i64,
                fps: fps as i64,
                mbps: (bitrate / 1_000_000) as i64,
                on_virtual_display: false,
            });
            hub.stream_changed.store(true, Ordering::SeqCst);
            crate::say!("Streaming {}: {uw}×{uh} as {width}×{height} at up to {fps} fps, HEVC on {}.", if window.is_some() { "a window" } else { "the screen" }, encoder.name);
            Ok(Pipeline {
                _device: device, context, duplication, desktop, video_context, processor,
                input_view: input_view.ok_or("video processor input")?, surfaces, next_surface: 0, encoder,
                fps,
                window,
                monitor: r,
            })
        }
    }

    /// Runs until nobody is connected (Ok) or the capture or encoder fails (Err: a new pipeline).
    fn run(&mut self, hub: &Hub, rect: &Mutex<Rect>) -> Result<()> {
        let interval = Duration::from_secs_f64(1.0 / self.fps as f64);
        let mut next_slot = Instant::now();
        let mut have_image = false;
        let mut dirty = false;
        let mut timestamp: i64 = 0;
        let mut waiting_since: Option<Instant> = None;
        // A keyframe asked of the encoder, and how many had come out by then.
        let mut key_asked: Option<(Instant, u64)> = None;
        while hub.wanted() {
            if hub.restart_picture.swap(false, Ordering::SeqCst) {
                return Ok(()); // new settings: the next pipeline takes them
            }
            // A streamed window: follow it as it moves; a new size means a new encoder, and a
            // window that's gone or minimized goes back to what the catalog says.
            if let Some((id, was)) = self.window {
                let now = crate::catalog::platform::bounds(id).map(|b| clip(b, self.monitor));
                match now {
                    Some(c) if c == was => {}
                    Some(c) if c.right - c.left == was.right - was.left && c.bottom - c.top == was.bottom - was.top => unsafe {
                        self.video_context.VideoProcessorSetStreamSourceRect(&self.processor, 0, true, Some(&offset(c, self.monitor)));
                        *rect.lock().unwrap() = Rect { left: c.left, top: c.top, width: c.right - c.left, height: c.bottom - c.top };
                        self.window = Some((id, c));
                        dirty = true;
                    },
                    _ => return Ok(()),
                }
            }
            // 1. A new desktop image, if there is one (waits up to 4 ms).
            unsafe {
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut resource: Option<IDXGIResource> = None;
                match self.duplication.AcquireNextFrame(4, &mut info, &mut resource) {
                    Ok(()) => {
                        if info.PointerShapeBufferSize > 0 {
                            pointer_shape(&self.duplication, &info, hub);
                        }
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
            // An encoder that stops asking for input while a frame waits has failed (a driver reset
            // can do that without a word): start again rather than freeze the picture.
            if have_image && (dirty || hub.keyframe_pending()) && self.encoder.wants_input == 0 {
                if waiting_since.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(2) {
                    return Err("the encoder stopped taking frames".into());
                }
            } else {
                waiting_since = None;
            }
            if have_image && self.encoder.wants_input > 0 && Instant::now() >= next_slot {
                let key = hub.take_keyframe_request();
                if dirty || key {
                    if self.encode(timestamp, key)? {
                        timestamp += 10_000_000 / self.fps as i64;
                        dirty = false;
                        if key {
                            key_asked = Some((Instant::now(), hub.keyframes.load(Ordering::SeqCst)));
                        }
                    } else if key {
                        hub.request_keyframe(); // refused this time: the frame goes in again
                    }
                    next_slot = (next_slot + interval).max(Instant::now());
                }
            }
            // Hardware encoders can drop a frame without a word, and on a still screen nothing else
            // would ever ask again: a device would stay blank.
            if let Some((at, count)) = key_asked {
                if hub.keyframes.load(Ordering::SeqCst) > count {
                    key_asked = None;
                } else if at.elapsed() > Duration::from_millis(500) {
                    hub.request_keyframe();
                    key_asked = None;
                }
            }
        }
        Ok(())
    }

    /// Whether the encoder took the frame.
    fn encode(&mut self, timestamp: i64, keyframe: bool) -> Result<bool> {
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
            self.context.Flush(); // the encoder reads the surface from another thread
            self.encoder.encode(surface, timestamp, 10_000_000 / self.fps as i64, keyframe)
        }
    }
}

/// Hands the pointer's new shape to the hub, in points as a device draws it.
unsafe fn pointer_shape(duplication: &IDXGIOutputDuplication, info: &DXGI_OUTDUPL_FRAME_INFO, hub: &Hub) {
    use crate::cursor::{blob, png, to_rgba, Shape};
    let mut buffer = vec![0u8; info.PointerShapeBufferSize as usize];
    let mut needed = 0u32;
    let mut shape = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
    if duplication.GetFramePointerShape(buffer.len() as u32, buffer.as_mut_ptr() as *mut _, &mut needed, &mut shape).is_err() {
        return;
    }
    let kind = match DXGI_OUTDUPL_POINTER_SHAPE_TYPE(shape.Type as i32) {
        DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME => Shape::Monochrome,
        DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR => Shape::Color,
        _ => Shape::MaskedColor,
    };
    let Some((rgba, height)) = to_rgba(kind, shape.Width as usize, shape.Height as usize, shape.Pitch as usize, &buffer) else { return };
    let Some(image) = png(shape.Width, height as u32, &rgba) else { return };
    let points = 96.0 / windows::Win32::UI::HiDpi::GetDpiForSystem().max(96) as f32;
    hub.set_cursor(blob(
        shape.HotSpot.x as f32 * points, shape.HotSpot.y as f32 * points, shape.Width as f32 * points, height as f32 * points, &image,
    ));
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

/// The monitor holding `point`, else the one whose top-left corner is the desktop's origin:
/// Windows' main display.
unsafe fn find_output(point: Option<(i32, i32)>) -> Result<(IDXGIAdapter1, IDXGIOutput, DXGI_OUTPUT_DESC)> {
    let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(err("DXGI didn't start"))?;
    let mut first = None;
    let mut a = 0;
    while let Ok(adapter) = factory.EnumAdapters1(a) {
        let mut o = 0;
        while let Ok(output) = adapter.EnumOutputs(o) {
            let desc = output.GetDesc().map_err(err("display description"))?;
            if desc.AttachedToDesktop.as_bool() {
                let d = desc.DesktopCoordinates;
                if let Some((x, y)) = point {
                    if x >= d.left && x < d.right && y >= d.top && y < d.bottom {
                        return Ok((adapter, output, desc));
                    }
                }
                let main = d.left == 0 && d.top == 0;
                if main || first.is_none() {
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
    activate: IMFActivate,
    _manager: IMFDXGIDeviceManager,
}

impl Drop for Encoder {
    /// Asynchronous encoders must be shut down before release, or the hardware session can outlive
    /// them (and consumer GPUs allow only a few at once).
    fn drop(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Ok(s) = self.transform.cast::<IMFShutdown>() {
                let _ = s.Shutdown();
            }
            let _ = self.activate.ShutdownObject();
        }
    }
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
        let activates = list_encoders(adapter, &input, &output);
        if activates.is_empty() {
            return Err("this PC's graphics chip has no hardware HEVC (H.265) encoder, which Sill needs".into());
        }
        let mut problems = Vec::new();
        for activate in &activates {
            let name = friendly_name(activate);
            match Self::configure(activate, &manager, width, height, fps, bitrate) {
                Ok((transform, events, codec, provides_samples, output_size)) => {
                    return Ok(Encoder {
                        transform, events, codec, wants_input: 0, provides_samples, output_size,
                        parameter_sets: None, name, activate: activate.clone(), _manager: manager,
                    })
                }
                Err(e) => {
                    let _ = activate.ShutdownObject();
                    problems.push(format!("{name}: {e}"));
                }
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

        // Real time: no B-frames, low latency, a steady rate, a keyframe only when asked (or every 4
        // s). Set before the media types, which some encoders read them at. One that refuses a
        // setting still works, at its own default, which the log says.
        let codec = transform.cast::<ICodecAPI>().ok();
        let apply = || -> Vec<&'static str> {
            let Some(c) = &codec else { return vec![] };
            [
                ("constant bitrate", c.SetValue(&CODECAPI_AVEncCommonRateControlMode, &VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32))),
                ("bitrate", c.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &VARIANT::from(bitrate))),
                ("no B-frames", c.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &VARIANT::from(0u32))),
                ("keyframe interval", c.SetValue(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(fps * 4))),
                ("low latency", c.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true))),
            ]
            .into_iter()
            .filter_map(|(what, r)| r.is_err().then_some(what))
            .collect()
        };
        apply();
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
        transform.SetOutputType(0, &out, 0).map_err(err("the encoder refused the HEVC format"))?;

        let inp = MFCreateMediaType().map_err(err("media type"))?;
        inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(err("input type"))?;
        inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).map_err(err("input type"))?;
        inp.SetUINT64(&MF_MT_FRAME_SIZE, size).map_err(err("input type"))?;
        inp.SetUINT64(&MF_MT_FRAME_RATE, rate).map_err(err("input type"))?;
        inp.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1).map_err(err("input type"))?;
        inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(err("input type"))?;
        if let Err(first) = transform.SetInputType(0, &inp, 0) {
            // Some encoders take only a type they handed out themselves.
            let mut taken = false;
            for i in 0..64 {
                let Ok(t) = transform.GetInputAvailableType(0, i) else { break };
                if t.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFVideoFormat_NV12) {
                    t.SetUINT64(&MF_MT_FRAME_SIZE, size).map_err(err("input type"))?;
                    t.SetUINT64(&MF_MT_FRAME_RATE, rate).map_err(err("input type"))?;
                    taken = transform.SetInputType(0, &t, 0).is_ok();
                    break;
                }
            }
            if !taken {
                return Err(format!("the encoder refused the picture format: {first}"));
            }
        }
        // Again, for encoders that only take these once both types are set; what they still
        // refuse, they keep at their own default, and the log says so.
        let refused = apply();
        if !refused.is_empty() {
            crate::say!("({} kept its own settings for: {}.)", friendly_name(activate), refused.join(", "));
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
            if let Ok(status) = event.GetStatus() {
                status.ok().map_err(err("the encoder reported a failure"))?;
            }
            match event.GetType().map_err(err("encoder event"))? {
                t if t == MEError.0 as u32 => return Err("the encoder reported an error".into()),
                NEED_INPUT => self.wants_input += 1,
                HAVE_OUTPUT => self.output(hub)?,
                _ => {}
            }
        }
    }

    /// Whether it took the frame: an encoder may turn input down even after asking for it.
    unsafe fn encode(&mut self, surface: &ID3D11Texture2D, time: i64, duration: i64, keyframe: bool) -> Result<bool> {
        if keyframe {
            if let Some(c) = &self.codec {
                let _ = c.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32));
            }
        }
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, surface, 0, BOOL(0)).map_err(err("encoder input"))?;
        buffer.SetCurrentLength(buffer.GetMaxLength().map_err(err("encoder input"))?).map_err(err("encoder input"))?;
        let sample = MFCreateSample().map_err(err("encoder input"))?;
        sample.AddBuffer(&buffer).map_err(err("encoder input"))?;
        sample.SetSampleTime(time).map_err(err("encoder input"))?;
        sample.SetSampleDuration(duration).map_err(err("encoder input"))?;
        match self.transform.ProcessInput(0, &sample, 0) {
            Ok(()) => {
                self.wants_input = self.wants_input.saturating_sub(1);
                Ok(true)
            }
            Err(e) if e.code() == MF_E_NOTACCEPTING => Ok(false),
            Err(e) => Err(format!("the encoder refused a frame: {e}")),
        }
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
                let info = self.transform.GetOutputStreamInfo(0).map_err(err("output stream"))?;
                self.provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32) != 0;
                self.output_size = info.cbSize.max(1 << 20);
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

/// The hardware HEVC encoders of this graphics chip (on a laptop with two, the other can't use its
/// surfaces). Falls back to every encoder where Windows can't filter by chip.
unsafe fn list_encoders(adapter: &IDXGIAdapter1, input: &MFT_REGISTER_TYPE_INFO, output: &MFT_REGISTER_TYPE_INFO) -> Vec<IMFActivate> {
    let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;
    let collect = |list: *mut Option<IMFActivate>, count: u32| -> Vec<IMFActivate> {
        if list.is_null() {
            return vec![];
        }
        let v = std::slice::from_raw_parts_mut(list, count as usize).iter_mut().filter_map(|a| a.take()).collect();
        CoTaskMemFree(Some(list as _));
        v
    };
    let mut attributes = None;
    if let (Ok(desc), Ok(())) = (adapter.GetDesc1(), MFCreateAttributes(&mut attributes, 1)) {
        if let Some(a) = attributes {
            let luid = [desc.AdapterLuid.LowPart.to_le_bytes(), (desc.AdapterLuid.HighPart as u32).to_le_bytes()].concat();
            let mut list = std::ptr::null_mut();
            let mut count = 0u32;
            if a.SetBlob(&MFT_ENUM_ADAPTER_LUID, &luid).is_ok()
                && MFTEnum2(MFT_CATEGORY_VIDEO_ENCODER, flags, Some(input), Some(output), &a, &mut list, &mut count).is_ok()
            {
                let v = collect(list, count);
                if !v.is_empty() {
                    return v;
                }
            }
        }
    }
    let mut list = std::ptr::null_mut();
    let mut count = 0u32;
    match MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, flags, Some(input), Some(output), &mut list, &mut count) {
        Ok(()) => collect(list, count),
        Err(_) => vec![],
    }
}

unsafe fn friendly_name(a: &IMFActivate) -> String {
    let mut buf = [0u16; 256];
    let mut len = 0u32;
    if a.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, Some(&mut len)).is_ok() {
        return String::from_utf16_lossy(&buf[..len as usize]);
    }
    "the hardware encoder".into()
}
