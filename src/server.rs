//! The door (one TCP port, TLS for everything) and the sessions behind it.
//!
//! - `sill-pair/1`: exactly one kind 19 within 10 s, one kind 20 back, then closed.
//!   "ask" is answered "shown" (this host's pairing window is always open, as SillHost --pairing's),
//!   "qr" and "code" are judged by the window, "cancel" and any later `v` get "closed".
//! - `sill/1` (paired keys only; the handshake refuses any other): the catalog (kind 2, then
//!   16), then the picture (kind 0 before each keyframe, kind 1 frames), pongs for pings, ticks
//!   while nothing else goes out, and the device's input handed to the injector.
//!
//! Fan-out follows StreamServer.swift's home door: a device that joins, or falls more than two
//! frames behind, waits for a keyframe (and asks the encoder for one); deltas are never sent to a
//! device that missed one, since every later delta refers to it.

use crate::identity::{certificate_fingerprint, Identity, Trust};
use crate::pairing::{self, b64u, b64u_decode, Verdict};
use crate::protocol::{self as p, kind, Header, HEADER_LEN};
use crate::tls;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_rustls::LazyConfigAcceptor;

const TICK: Duration = Duration::from_millis(30);
/// A device that takes nothing for this long is gone (the Mac's home door: 4 s).
const DRAIN_LIMIT: Duration = Duration::from_secs(4);
/// A device pings four times a second; this much silence means it is gone.
const SILENCE_LIMIT: Duration = Duration::from_secs(15);
const MAX_FRAMES_IN_FLIGHT: usize = 2;

struct Out {
    data: Arc<Vec<u8>>,
    frame: bool,
}

struct Client {
    tx: mpsc::UnboundedSender<Out>,
    in_flight: Arc<AtomicUsize>,
    needs_keyframe: bool,
    /// It asked for what's copied on this PC (kind 32).
    clipboard: bool,
    /// How many input messages were read from it, and when it last moved the pointer.
    inputs: u64,
    moved_at: f64,
    /// Its hello lists the codec this host makes.
    audio: bool,
    /// The sound epoch whose format it has.
    audio_epoch: Option<u16>,
}

/// What a device may change, and only to Sill's own choices. Kept in `settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub max_fps: i64,
    pub bitrate: i64,
    /// 2: the screen's own detail (up to --max-size). 1: half that, for a slow link.
    pub capture_scale: f64,
    #[serde(default)]
    pub send_audio: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { max_fps: 60, bitrate: 15_000_000, capture_scale: 2.0, send_audio: false }
    }
}

impl Settings {
    fn path() -> std::path::PathBuf {
        crate::identity::home().join("settings.json")
    }

    /// Anything unreadable or out of range falls back to the defaults, field by field.
    pub fn load() -> Settings {
        let d = Settings::default();
        let Ok(s) = std::fs::read(Self::path()).map_err(|_| ()).and_then(|b| serde_json::from_slice::<Settings>(&b).map_err(|_| ())) else { return d };
        d.with(Some(s.max_fps), Some(s.bitrate), Some(s.capture_scale), Some(s.send_audio))
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(crate::identity::home());
        let _ = std::fs::write(Self::path(), serde_json::to_vec_pretty(self).unwrap_or_default());
    }

    /// `self` with what a device asked for, where it's one of Sill's choices (SettingsChoices).
    pub fn with(mut self, max_fps: Option<i64>, bitrate: Option<i64>, capture_scale: Option<f64>, send_audio: Option<bool>) -> Settings {
        const BITRATES: [i64; 7] = [4_000_000, 8_000_000, 15_000_000, 25_000_000, 40_000_000, 80_000_000, 150_000_000];
        if let Some(v) = max_fps.filter(|v| [60, 120].contains(v)) {
            self.max_fps = v;
        }
        if let Some(v) = bitrate.filter(|v| BITRATES.contains(v)) {
            self.bitrate = v;
        }
        if let Some(v) = capture_scale.filter(|v| *v == 1.0 || *v == 2.0) {
            self.capture_scale = v;
        }
        if let Some(v) = send_audio {
            self.send_audio = v;
        }
        self
    }
}

/// Where frames meet the connected devices.
#[derive(Default)]
pub struct Hub {
    clients: Mutex<HashMap<u64, Client>>,
    last_parameter_sets: Mutex<Option<Arc<Vec<u8>>>>,
    keyframe_wanted: AtomicBool,
    next_id: AtomicU64,
    pub stream: Mutex<Option<p::RunningStream>>,
    /// The settings a device can change (`Settings`), read when the picture starts.
    pub settings: Mutex<Settings>,
    /// The settings changed: the running picture stops and starts again with them.
    pub restart_picture: AtomicBool,
    /// The running stream changed: the devices hear of it at the next look.
    pub stream_changed: AtomicBool,
    /// Keeps the picture running with nobody connected, for the startup check.
    pub checking: AtomicBool,
    /// What the check reads: frames and keyframes encoded, and the last thing that went wrong.
    pub frames: AtomicU64,
    pub keyframes: AtomicU64,
    pub video_error: Mutex<Option<String>>,
    /// What streams: the whole screen unless a device picks a window.
    pub source: Mutex<p::StreamSource>,
    pub windows: Mutex<Vec<p::WindowInfo>>,
    pub icons: Mutex<Vec<Arc<Vec<u8>>>>,
    /// Each window's latest thumbnail, for a device that joins later.
    pub thumbnails: Mutex<HashMap<u32, Arc<Vec<u8>>>>,
    pub app_list: Mutex<Option<Arc<Vec<u8>>>>,
    /// The only paths a device may launch: the ones this PC listed.
    pub app_paths: Mutex<Vec<String>>,
    pub viewport: Mutex<Option<p::Viewport>>,
    /// Why no sound comes although Send Audio is on.
    pub audio_note: Mutex<Option<String>>,
    cursor: Mutex<Option<Arc<Vec<u8>>>>,
    last_pointer: Mutex<Option<Option<(f64, f64)>>>,
}

impl Hub {
    pub fn client_count(&self) -> usize {
        self.clients.lock().unwrap().len()
    }

    /// Whether the picture should run: a device is connected, or the startup check is on.
    pub fn wanted(&self) -> bool {
        self.checking.load(Ordering::SeqCst) || (self.client_count() > 0 && !matches!(*self.source.lock().unwrap(), p::StreamSource::None {}))
    }

    /// Whether the source changed. The picture restarts on it; the first frame is a keyframe.
    pub fn set_source(&self, source: p::StreamSource) -> bool {
        let mut s = self.source.lock().unwrap();
        if *s == source {
            return false;
        }
        *s = source;
        self.restart_picture.store(true, Ordering::SeqCst);
        self.request_keyframe();
        true
    }

    /// The pointer's shape, to every device now and to each one that joins.
    pub fn set_cursor(&self, blob: Vec<u8>) {
        let m = Arc::new(p::message(kind::CURSOR_SHAPE, p::now(), false, &blob));
        let mut c = self.cursor.lock().unwrap();
        if c.as_ref().is_some_and(|old| old[HEADER_LEN..] == m[HEADER_LEN..]) {
            return;
        }
        *c = Some(m.clone());
        for client in self.clients.lock().unwrap().values() {
            let _ = client.tx.send(Out { data: m.clone(), frame: false });
        }
    }

    fn note_input(&self, id: u64, moves_pointer: bool) {
        if let Some(c) = self.clients.lock().unwrap().get_mut(&id) {
            c.inputs += 1;
            if moves_pointer {
                c.moved_at = p::now();
            }
        }
    }

    /// Where the PC's pointer is now (None: off the stream): when it changed, to every device
    /// that isn't the one moving it. `seen` lets a device drop a report older than its own input.
    pub fn pointer_sample(&self, place: Option<(f64, f64)>) {
        let mut last = self.last_pointer.lock().unwrap();
        if *last == Some(place) {
            return;
        }
        *last = Some(place);
        let now = p::now();
        for c in self.clients.lock().unwrap().values() {
            if now - c.moved_at < 0.5 {
                continue;
            }
            let m = p::MacPointer { x: place.map(|v| v.0), y: place.map(|v| v.1), inside: place.is_some(), seen: c.inputs };
            let _ = c.tx.send(Out { data: Arc::new(p::json_message(kind::MAC_POINTER, &m)), frame: false });
        }
    }

    pub fn request_keyframe(&self) {
        self.keyframe_wanted.store(true, Ordering::SeqCst);
    }

    pub fn keyframe_pending(&self) -> bool {
        self.keyframe_wanted.load(Ordering::SeqCst)
    }

    /// The encoder asks before each frame: should this one be a keyframe?
    pub fn take_keyframe_request(&self) -> bool {
        self.keyframe_wanted.swap(false, Ordering::SeqCst)
    }

    fn register(&self, tx: mpsc::UnboundedSender<Out>, in_flight: Arc<AtomicUsize>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.clients.lock().unwrap().insert(id, Client { tx, in_flight, needs_keyframe: true, clipboard: false, inputs: 0, moved_at: 0.0, audio: false, audio_epoch: None });
        self.request_keyframe();
        id
    }

    fn unregister(&self, id: u64) {
        self.clients.lock().unwrap().remove(&id);
    }

    fn send(&self, id: u64, data: Vec<u8>) {
        if let Some(c) = self.clients.lock().unwrap().get(&id) {
            let _ = c.tx.send(Out { data: Arc::new(data), frame: false });
        }
    }

    /// Send Audio is on and a device plays this host's codec.
    pub fn audio_wanted(&self) -> bool {
        self.settings.lock().unwrap().send_audio && self.clients.lock().unwrap().values().any(|c| c.audio)
    }

    /// A sound packet to every device that plays it, each one's format first. Never counted as a
    /// frame: sound doesn't make the picture drop.
    pub fn send_audio(&self, format: &Arc<Vec<u8>>, epoch: u16, packet: Vec<u8>) {
        let packet = Arc::new(packet);
        for c in self.clients.lock().unwrap().values_mut() {
            if c.audio {
                if c.audio_epoch != Some(epoch) {
                    c.audio_epoch = Some(epoch);
                    let _ = c.tx.send(Out { data: format.clone(), frame: false });
                }
                let _ = c.tx.send(Out { data: packet.clone(), frame: false });
            }
        }
    }

    fn set_clipboard_subscribed(&self, id: u64, on: bool) {
        if let Some(c) = self.clients.lock().unwrap().get_mut(&id) {
            c.clipboard = on;
        }
    }

    pub fn has_clipboard_subscribers(&self) -> bool {
        self.clients.lock().unwrap().values().any(|c| c.clipboard)
    }

    /// Text to every subscriber but the device it came from.
    pub fn send_clipboard(&self, text: &str, except: Option<u64>) {
        let m = p::json_message(kind::CLIPBOARD, &p::Clipboard { text: Some(text.to_string()), stop: None });
        if m.len() - HEADER_LEN > p::MAX_CLIENT_PAYLOAD {
            return; // JSON can be far longer than its text: too long to send
        }
        let m = Arc::new(m);
        for (id, c) in self.clients.lock().unwrap().iter() {
            if c.clipboard && Some(*id) != except {
                let _ = c.tx.send(Out { data: m.clone(), frame: false });
            }
        }
    }

    pub fn send_all(&self, data: Vec<u8>) {
        let data = Arc::new(data);
        for c in self.clients.lock().unwrap().values() {
            let _ = c.tx.send(Out { data: data.clone(), frame: false });
        }
    }

    /// One encoded access unit (length-prefixed NALs), with the parameter sets when it is a keyframe.
    pub fn broadcast_frame(&self, data: &[u8], is_keyframe: bool, parameter_sets: Option<Vec<u8>>) {
        let now = p::now();
        self.frames.fetch_add(1, Ordering::SeqCst);
        if is_keyframe {
            self.keyframes.fetch_add(1, Ordering::SeqCst);
        }
        let mut ps_sent = false;
        if let Some(ps) = parameter_sets {
            let m = Arc::new(p::message(kind::PARAMETER_SETS, now, true, &ps));
            *self.last_parameter_sets.lock().unwrap() = Some(m.clone());
            for c in self.clients.lock().unwrap().values() {
                let _ = c.tx.send(Out { data: m.clone(), frame: false });
            }
            ps_sent = true;
        }
        let frame = Arc::new(p::message(kind::FRAME, now, is_keyframe, data));
        let ps = self.last_parameter_sets.lock().unwrap().clone();
        let mut want_key = false;
        for c in self.clients.lock().unwrap().values_mut() {
            if c.needs_keyframe {
                let (true, Some(ps)) = (is_keyframe, &ps) else { continue };
                if !ps_sent {
                    let _ = c.tx.send(Out { data: ps.clone(), frame: false });
                }
                c.needs_keyframe = false;
            } else if !is_keyframe && c.in_flight.load(Ordering::SeqCst) > MAX_FRAMES_IN_FLIGHT {
                c.needs_keyframe = true;
                want_key = true;
                continue;
            }
            c.in_flight.fetch_add(1, Ordering::SeqCst);
            let _ = c.tx.send(Out { data: frame.clone(), frame: true });
        }
        if want_key {
            self.request_keyframe();
        }
    }
}

/// What a session needs from the rest of the host.
pub struct Host {
    pub identity: Identity,
    pub trust: Trust,
    pub name: String,
    pub launch_id: String,
    pub hub: Arc<Hub>,
    pub input: std::sync::mpsc::Sender<crate::input::Command>,
    /// Always open; main puts up a new one once it is used, stopped or expired.
    pub pairing: Arc<Mutex<pairing::Window>>,
    pub port: u16,
    session_tls: Arc<rustls::ServerConfig>,
    pairing_tls: Arc<rustls::ServerConfig>,
}

impl Host {
    pub fn new(
        identity: Identity, trust: Trust, name: String, launch_id: String, hub: Arc<Hub>,
        input: std::sync::mpsc::Sender<crate::input::Command>, pairing: Arc<Mutex<pairing::Window>>, port: u16,
    ) -> Result<Host, String> {
        let session_tls = tls::server_config(&identity, tls::SESSION_ALPN, Some(trust.clone()))?;
        let pairing_tls = tls::server_config(&identity, tls::PAIRING_ALPN, None)?;
        Ok(Host { identity, trust, name, launch_id, hub, input, pairing, port, session_tls, pairing_tls })
    }

    pub fn window_list(&self) -> Vec<u8> {
        p::json_message(kind::WINDOW_LIST, &p::WindowList {
            mac_name: self.name.clone(),
            windows: self.hub.windows.lock().unwrap().clone(),
            active: self.hub.source.lock().unwrap().clone(),
            launch_id: Some(self.launch_id.clone()),
            // Sill's test scripts refuse a host that names a version (a real host would act on their
            // input); the Mac test host leaves it out, as SillHost does.
            host_version: std::env::var_os("SILL_TEST_NO_VERSION").is_none().then(|| env!("CARGO_PKG_VERSION").into()),
            protocol: Some(1),
            gestures: Some(1),
            clipboard: Some(1),
            key_repeat: Some(1),
        })
    }

    /// Kind 18: who this PC is and how to reach it from afar, signed with its key. A device saves
    /// it for this PC only when the key of the session it came on signed it.
    pub fn mac_info(&self) -> Option<Vec<u8>> {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        let info = p::MacInfo {
            v: 1,
            mac_id: pairing::mac_id(&self.identity.fingerprint),
            name: self.name.clone(),
            issued_at: p::now(),
            remote_access: true,
            remote_port: self.port as i64,
            internet: false,
            addresses: crate::net::addresses(),
        };
        let bytes = serde_json::to_vec(&info).ok()?;
        let sig = self.identity.sign(&bytes)?;
        Some(p::json_message(kind::MAC_INFO, &p::SignedMacInfo { info: STANDARD.encode(&bytes), key: STANDARD.encode(&self.identity.point), sig: STANDARD.encode(sig) }))
    }

    pub fn settings_state(&self, answering: Option<i64>) -> p::HostSettingsState {
        let s = *self.hub.settings.lock().unwrap();
        p::HostSettingsState {
            settings: p::StreamSettings {
                max_fps: s.max_fps,
                bitrate: s.bitrate,
                capture_scale: s.capture_scale,
                prioritize_speed: true,
                virtual_display: false,
                send_audio: Some(s.send_audio),
            },
            persistent: true,
            virtual_display_available: false,
            virtual_display_note: Some("Not available on Windows.".into()),
            software_encoder: false,
            stream: self.hub.stream.lock().unwrap().clone(),
            answering,
            audio_note: self.hub.audio_note.lock().unwrap().clone(),
            session_paired: Some(true),
        }
    }
}

/// Listens on every IPv4 and IPv6 address at `port`. Returns once both listeners are up (or
/// the IPv6 one is refused because the IPv4 one already covers it, as on macOS).
pub async fn listen(host: Arc<Host>, port: u16) -> Result<(), String> {
    let v4 = TcpListener::bind(("0.0.0.0", port)).await.map_err(|e| format!("Couldn't listen on port {port}: {e}"))?;
    tokio::spawn(accept_loop(host.clone(), v4));
    match TcpListener::bind(("::", port)).await {
        Ok(v6) => {
            tokio::spawn(accept_loop(host, v6));
        }
        Err(e) => crate::warn!("(IPv6 listener not started: {e})"),
    }
    Ok(())
}

async fn accept_loop(host: Arc<Host>, listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                let host = host.clone();
                tokio::spawn(async move {
                    if let Err(e) = connection(host, stream, addr.ip().to_string()).await {
                        crate::warn!("{addr}: {e}");
                    }
                });
            }
            Err(e) => {
                crate::warn!("accept: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn connection(host: Arc<Host>, stream: TcpStream, source: String) -> Result<(), String> {
    let _ = stream.set_nodelay(true);
    let handshake = async {
        let start = LazyConfigAcceptor::new(rustls::server::Acceptor::default(), stream).await.map_err(|e| e.to_string())?;
        let offered: Vec<Vec<u8>> = start.client_hello().alpn().map(|i| i.map(|a| a.to_vec()).collect()).unwrap_or_default();
        let config = if offered.iter().any(|a| a == tls::SESSION_ALPN) {
            host.session_tls.clone()
        } else if offered.iter().any(|a| a == tls::PAIRING_ALPN) {
            host.pairing_tls.clone()
        } else {
            return Err("offered no Sill protocol; closed".to_string());
        };
        start.into_stream(config).await.map_err(|e| e.to_string())
    };
    let tls = match timeout(Duration::from_secs(10), handshake).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return Err(format!("handshake refused: {e}")),
        Err(_) => return Err("handshake took over 10 s; closed".into()),
    };
    let (_, conn) = tls.get_ref();
    let alpn = conn.alpn_protocol().map(|a| a.to_vec()).unwrap_or_default();
    let fp = conn.peer_certificates().and_then(|c| c.first()).and_then(|c| certificate_fingerprint(c)).ok_or("no P-256 client key")?;
    if alpn == tls::PAIRING_ALPN {
        pair_connection(host, tls, fp, source).await
    } else {
        session(host, tls, fp, source).await
    }
}

async fn read_message<R: AsyncRead + Unpin>(r: &mut R, limit: usize) -> Result<(Header, Vec<u8>), String> {
    let mut h = [0u8; HEADER_LEN];
    r.read_exact(&mut h).await.map_err(|_| "closed".to_string())?;
    let header = Header::parse(&h);
    if header.payload_len > limit {
        return Err(format!("announced a {}-byte message (the limit is {limit}); closed", header.payload_len));
    }
    let mut payload = vec![0u8; header.payload_len];
    r.read_exact(&mut payload).await.map_err(|_| "closed".to_string())?;
    Ok((header, payload))
}

// ---- Pairing ------------------------------------------------------------------------------------

async fn pair_connection<S: AsyncRead + AsyncWrite + Unpin>(host: Arc<Host>, mut s: S, fp_device: [u8; 32], source: String) -> Result<(), String> {
    let (h, payload) = match timeout(Duration::from_secs(10), read_message(&mut s, p::MAX_PAIRING_PAYLOAD)).await {
        Ok(Err(e)) if e == "closed" => return Ok(()), // the device went away before asking: nothing to say
        Ok(r) => r?,
        Err(_) => return Err("pairing connection sent nothing for 10 s; closed".into()),
    };
    if h.kind != kind::PAIR_REQUEST || payload.is_empty() {
        return Err(format!("pairing connection sent kind {}; closed", h.kind));
    }
    let request: p::PairRequest = serde_json::from_slice(&payload).map_err(|_| "pair request didn't decode; closed")?;
    let result = judge(&host, &request, &fp_device, &source);
    let _ = timeout(Duration::from_secs(2), async {
        let _ = s.write_all(&p::json_message(kind::PAIR_RESULT, &result)).await;
        let _ = s.flush().await;
        let _ = s.shutdown().await;
    })
    .await;
    Ok(())
}

fn judge(host: &Host, request: &p::PairRequest, fp_device: &[u8; 32], source: &str) -> p::PairResult {
    let who = p::safe_label(&request.name, 64);
    if request.v != 1 {
        return p::PairResult::refused("closed");
    }
    match request.method.as_str() {
        "ask" => {
            crate::say!("{who} at {source} asks to pair: the code is on this screen.");
            crate::pair_window::bring_up();
            p::PairResult::refused("shown")
        }
        "qr" | "code" => {
            let Some(proof) = b64u_decode(&request.proof) else { return p::PairResult::refused("code") };
            let verdict = host.pairing.lock().unwrap().judge(&request.method, &proof, fp_device, &host.identity.fingerprint, source, p::now());
            match verdict {
                Verdict::Accept(proof_m) => {
                    let model = request.model.as_deref().map(|m| p::safe_label(m, 64));
                    if let Err(e) = host.trust.add(fp_device, if who.is_empty() { "Device".into() } else { who.clone() }, model) {
                        crate::warn!("Couldn't save the paired device: {e}");
                        return p::PairResult::refused("closed");
                    }
                    crate::say!("Paired with {who} ({}).", if request.method == "qr" { "QR code" } else { "typed code" });
                    crate::pair_window::hide();
                    p::PairResult {
                        ok: true,
                        proof: Some(b64u(&proof_m)),
                        mac_id: Some(pairing::mac_id(&host.identity.fingerprint)),
                        name: Some(host.name.clone()),
                        recognition_key: Some(b64u(&host.identity.recognition_key)),
                        ..Default::default()
                    }
                }
                Verdict::Reject { tries_left } => {
                    crate::say!("{who} at {source} used a wrong code ({tries_left} tries left).");
                    p::PairResult { tries_left: Some(tries_left), ..p::PairResult::refused("code") }
                }
                Verdict::Busy { retry_after } => p::PairResult { retry_after: Some(retry_after), ..p::PairResult::refused("busy") },
                Verdict::Closed(reason) => p::PairResult::refused(reason),
            }
        }
        _ => p::PairResult::refused("closed"),
    }
}

// ---- Sessions -----------------------------------------------------------------------------------

async fn session<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(host: Arc<Host>, s: S, fp: [u8; 32], source: String) -> Result<(), String> {
    let name = host.trust.name(&fp).unwrap_or_else(|| "A device".into());
    let (mut reader, writer) = tokio::io::split(s);
    let (tx, rx) = mpsc::unbounded_channel();
    let in_flight = Arc::new(AtomicUsize::new(0));
    // The catalog goes first, before any frame: the window list, then the settings.
    let _ = tx.send(Out { data: Arc::new(host.window_list()), frame: false });
    let _ = tx.send(Out { data: Arc::new(p::json_message(kind::HOST_SETTINGS, &host.settings_state(None))), frame: false });
    if let Some(info) = host.mac_info() {
        let _ = tx.send(Out { data: Arc::new(info), frame: false });
    }
    for icon in host.hub.icons.lock().unwrap().iter() {
        let _ = tx.send(Out { data: icon.clone(), frame: false });
    }
    for thumbnail in host.hub.thumbnails.lock().unwrap().values() {
        let _ = tx.send(Out { data: thumbnail.clone(), frame: false });
    }
    if let Some(apps) = host.hub.app_list.lock().unwrap().clone() {
        let _ = tx.send(Out { data: apps, frame: false });
    }
    if let Some(cursor) = host.hub.cursor.lock().unwrap().clone() {
        let _ = tx.send(Out { data: cursor, frame: false });
    }
    let id = host.hub.register(tx, in_flight.clone());
    crate::say!("{name} connected from {source}.");
    let mut writing = tokio::spawn(write_loop(writer, rx, in_flight));
    let mut last_keyframe_request = 0.0;
    let mut stats_seen = 0u64;
    let mut clipboard_arrivals = Vec::new();
    let reason = loop {
        let read = tokio::select! {
            r = timeout(SILENCE_LIMIT, read_message(&mut reader, p::MAX_CLIENT_PAYLOAD)) => r,
            _ = &mut writing => break "it stopped taking data".to_string(),
        };
        let (h, payload) = match read {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => break e,
            Err(_) => break "silent for 15 s".to_string(),
        };
        let _ = host.input.send(crate::input::Command::Heard);
        match h.kind {
            kind::PING => host.hub.send(id, p::message(kind::PONG, h.timestamp, false, &payload)),
            kind::INPUT => match serde_json::from_slice::<p::InputEvent>(&payload) {
                Ok(e) => {
                    host.hub.note_input(id, matches!(e, p::InputEvent::Pointer { .. } | p::InputEvent::Scroll { .. }));
                    let _ = host.input.send(crate::input::Command::Event(e));
                }
                Err(_) => crate::warn!("{name}: an input message didn't decode: {}", String::from_utf8_lossy(&payload)),
            },
            kind::HELLO => {
                if let Ok(hello) = serde_json::from_slice::<p::Hello>(&payload) {
                    if hello.audio.as_ref().is_some_and(|a| a.iter().any(|c| c == "aac-eld")) {
                        if let Some(c) = host.hub.clients.lock().unwrap().get_mut(&id) {
                            c.audio = true;
                        }
                    }
                    crate::say!(
                        "{name} is {} with Sill {}.",
                        p::safe_label(hello.device.as_deref().unwrap_or("a device"), 64),
                        p::safe_label(hello.app_version.as_deref().unwrap_or("?"), 16)
                    );
                }
            }
            kind::SELECT_SOURCE => {
                let Ok(source) = serde_json::from_slice::<p::StreamSource>(&payload) else { continue };
                // A window that isn't listed (gone a moment ago) keeps what streams now.
                let known = match source {
                    p::StreamSource::Window { id } => host.hub.windows.lock().unwrap().iter().any(|w| w.id == id),
                    _ => true,
                };
                if known {
                    if let p::StreamSource::Window { id } = source {
                        crate::catalog::platform::focus(id);
                        fit_to_viewport(&host, id);
                    }
                    if host.hub.set_source(source) {
                        host.hub.send_all(host.window_list());
                        continue;
                    }
                }
                host.hub.send(id, host.window_list());
            }
            kind::VIEWPORT => {
                let Ok(v) = serde_json::from_slice::<p::Viewport>(&payload) else { continue };
                let sides = 1.0..=16384.0;
                if !sides.contains(&v.width) || !sides.contains(&v.height) || v.scale.is_some_and(|s| !(0.25..=4.0).contains(&s)) {
                    crate::warn!("{name}'s panel size was refused.");
                    continue;
                }
                *host.hub.viewport.lock().unwrap() = Some(v);
                let source = host.hub.source.lock().unwrap().clone();
                if let p::StreamSource::Window { id } = source {
                    fit_to_viewport(&host, id);
                }
            }
            kind::WINDOW_COMMAND => {
                let Ok(c) = serde_json::from_slice::<p::WindowCommand>(&payload) else { continue };
                if host.hub.windows.lock().unwrap().iter().any(|w| w.id == c.id) {
                    crate::catalog::platform::command(c.id, &c.action);
                }
            }
            kind::LAUNCH_APP => {
                let Ok(a) = serde_json::from_slice::<p::LaunchApp>(&payload) else { continue };
                // Only what this PC listed: a device's message never names a program to run.
                if host.hub.app_paths.lock().unwrap().contains(&a.bundle_id) && !crate::catalog::platform::launch(&a.bundle_id) {
                    crate::warn!("Windows couldn't open that app.");
                }
            }
            kind::CHANGE_SETTINGS => {
                // Every decodable change gets one answer, carrying its token; a value outside Sill's
                // choices is left as it was, and the answer shows that. Other devices hear of a change.
                let Ok(c) = serde_json::from_slice::<p::HostSettingsChange>(&payload) else { continue };
                let changed = {
                    let mut s = host.hub.settings.lock().unwrap();
                    let new = s.with(c.max_fps, c.bitrate, c.capture_scale, c.send_audio);
                    let changed = new != *s;
                    let picture = Settings { send_audio: s.send_audio, ..new } != *s;
                    *s = new;
                    (changed, picture)
                };
                if changed.0 {
                    host.hub.settings.lock().unwrap().save();
                    crate::say!("{name} changed the settings.");
                }
                if changed.1 {
                    host.hub.restart_picture.store(true, Ordering::SeqCst);
                }
                host.hub.send(id, p::json_message(kind::HOST_SETTINGS, &host.settings_state(c.token)));
                if changed.0 {
                    host.hub.send_all(p::json_message(kind::HOST_SETTINGS, &host.settings_state(None)));
                }
            }
            kind::GESTURE => {
                if let Ok(g) = serde_json::from_slice::<p::TrackpadGesture>(&payload) {
                    let _ = host.input.send(crate::input::Command::Gesture(g.gesture));
                }
            }
            kind::CLIPBOARD => {
                use crate::clipboard::{admits, from_device, write, Ask};
                match from_device(&payload) {
                    Ask::Stop => host.hub.set_clipboard_subscribed(id, false),
                    Ask::Refuse => {}
                    ask if admits(&mut clipboard_arrivals, p::now()) => {
                        host.hub.set_clipboard_subscribed(id, true);
                        if let Ask::Set(text) = ask {
                            if write(&text) {
                                host.hub.send_clipboard(&text, Some(id));
                            } else {
                                crate::warn!("Couldn't put {name}'s text on the clipboard: another program has it open.");
                            }
                        }
                    }
                    _ => {} // past the rate: nothing taken
                }
            }
            kind::KEYFRAME_REQUEST => {
                let now = p::now();
                if now - last_keyframe_request >= 1.0 {
                    last_keyframe_request = now;
                    host.hub.request_keyframe();
                }
            }
            kind::CLIENT_STATS => {
                // What the device really shows: the one line that proves the picture arrives.
                stats_seen += 1;
                if stats_seen % 10 == 1 {
                    if let Ok(s) = serde_json::from_slice::<p::ClientStats>(&payload) {
                        crate::say!("{name} shows {} frames a second, {} ms behind this PC, network round trip {} ms.", s.fps, s.frame_age_ms, s.rtt_ms);
                    }
                }
            }
            _ => {} // viewport, menus, clipboard…: not in v0.1; unknown kinds are skipped
        }
    };
    host.hub.unregister(id);
    let _ = host.input.send(crate::input::Command::Left);
    writing.abort();
    crate::say!("{name} left ({reason}).");
    Ok(())
}

/// A device's text scale for a window: the window takes the panel's size at that scale.
fn fit_to_viewport(host: &Host, id: u32) {
    let viewport = host.hub.viewport.lock().unwrap().clone();
    if let Some(p::Viewport { width, height, scale: Some(s) }) = viewport {
        crate::catalog::platform::fit(id, width / s, height / s);
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(mut w: W, mut rx: mpsc::UnboundedReceiver<Out>, in_flight: Arc<AtomicUsize>) {
    let tick = Arc::new(p::message(kind::TICK, 0.0, false, &[]));
    loop {
        let out = match timeout(TICK, rx.recv()).await {
            Ok(Some(o)) => o,
            Ok(None) => return,
            // Nothing went out for 30 ms: a tick keeps the device's Wi-Fi radio awake.
            Err(_) => Out { data: tick.clone(), frame: false },
        };
        match timeout(DRAIN_LIMIT, w.write_all(&out.data)).await {
            Ok(Ok(())) => {}
            _ => return, // the device stopped taking data: its reader ends the session
        }
        if out.frame {
            in_flight.fetch_sub(1, Ordering::SeqCst);
        }
        if rx.is_empty() && w.flush().await.is_err() {
            return;
        }
    }
}

/// Every session gets goodbye "quit" (the device then shows that this PC quit, rather than a
/// network failure), with a moment for it to go out.
pub async fn goodbye_all(hub: &Hub) {
    hub.send_all(p::json_message(kind::GOODBYE, &p::Goodbye { reason: "quit".into() }));
    tokio::time::sleep(Duration::from_millis(300)).await;
}
