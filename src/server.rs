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
}

/// Where frames meet the connected devices.
#[derive(Default)]
pub struct Hub {
    clients: Mutex<HashMap<u64, Client>>,
    last_parameter_sets: Mutex<Option<Arc<Vec<u8>>>>,
    keyframe_wanted: AtomicBool,
    next_id: AtomicU64,
    pub stream: Mutex<Option<p::RunningStream>>,
    pub bitrate: AtomicU64,
}

impl Hub {
    pub fn client_count(&self) -> usize {
        self.clients.lock().unwrap().len()
    }

    pub fn request_keyframe(&self) {
        self.keyframe_wanted.store(true, Ordering::SeqCst);
    }

    /// The encoder asks before each frame: should this one be a keyframe?
    pub fn take_keyframe_request(&self) -> bool {
        self.keyframe_wanted.swap(false, Ordering::SeqCst)
    }

    fn register(&self, tx: mpsc::UnboundedSender<Out>, in_flight: Arc<AtomicUsize>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.clients.lock().unwrap().insert(id, Client { tx, in_flight, needs_keyframe: true });
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

    pub fn send_all(&self, data: Vec<u8>) {
        let data = Arc::new(data);
        for c in self.clients.lock().unwrap().values() {
            let _ = c.tx.send(Out { data: data.clone(), frame: false });
        }
    }

    /// One encoded access unit (length-prefixed NALs), with the parameter sets when it is a keyframe.
    pub fn broadcast_frame(&self, data: &[u8], is_keyframe: bool, parameter_sets: Option<Vec<u8>>) {
        let now = p::now();
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
    pub input: mpsc::UnboundedSender<p::InputEvent>,
    /// Always open; main puts up a new one once it is used, stopped or expired.
    pub pairing: Arc<Mutex<pairing::Window>>,
    session_tls: Arc<rustls::ServerConfig>,
    pairing_tls: Arc<rustls::ServerConfig>,
}

impl Host {
    pub fn new(
        identity: Identity, trust: Trust, name: String, launch_id: String, hub: Arc<Hub>,
        input: mpsc::UnboundedSender<p::InputEvent>, pairing: Arc<Mutex<pairing::Window>>,
    ) -> Result<Host, String> {
        let session_tls = tls::server_config(&identity, tls::SESSION_ALPN, Some(trust.clone()))?;
        let pairing_tls = tls::server_config(&identity, tls::PAIRING_ALPN, None)?;
        Ok(Host { identity, trust, name, launch_id, hub, input, pairing, session_tls, pairing_tls })
    }

    pub fn window_list(&self) -> Vec<u8> {
        p::json_message(kind::WINDOW_LIST, &p::WindowList {
            mac_name: self.name.clone(),
            windows: vec![],
            active: p::StreamSource::Desktop {},
            launch_id: Some(self.launch_id.clone()),
            host_version: Some(env!("CARGO_PKG_VERSION").into()),
            protocol: Some(1),
        })
    }

    pub fn settings_state(&self, answering: Option<i64>) -> p::HostSettingsState {
        p::HostSettingsState {
            settings: p::StreamSettings {
                max_fps: 60,
                bitrate: self.hub.bitrate.load(Ordering::SeqCst) as i64,
                capture_scale: 1.0,
                prioritize_speed: true,
                virtual_display: false,
            },
            persistent: false,
            virtual_display_available: false,
            virtual_display_note: Some("Not available on Windows.".into()),
            software_encoder: false,
            stream: self.hub.stream.lock().unwrap().clone(),
            answering,
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
        Err(e) => eprintln!("(IPv6 listener not started: {e})"),
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
                        eprintln!("{addr}: {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("accept: {e}");
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
            println!("{who} at {source} asks to pair: the code is on this screen.");
            p::PairResult::refused("shown")
        }
        "qr" | "code" => {
            let Some(proof) = b64u_decode(&request.proof) else { return p::PairResult::refused("code") };
            let verdict = host.pairing.lock().unwrap().judge(&request.method, &proof, fp_device, &host.identity.fingerprint, source, p::now());
            match verdict {
                Verdict::Accept(proof_m) => {
                    let model = request.model.as_deref().map(|m| p::safe_label(m, 64));
                    if let Err(e) = host.trust.add(fp_device, if who.is_empty() { "Device".into() } else { who.clone() }, model) {
                        eprintln!("Couldn't save the paired device: {e}");
                        return p::PairResult::refused("closed");
                    }
                    println!("Paired with {who} ({}).", if request.method == "qr" { "QR code" } else { "typed code" });
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
                    println!("{who} at {source} used a wrong code ({tries_left} tries left).");
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
    let id = host.hub.register(tx, in_flight.clone());
    println!("{name} connected from {source}.");
    let mut writing = tokio::spawn(write_loop(writer, rx, in_flight));
    let mut last_keyframe_request = 0.0;
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
        match h.kind {
            kind::PING => host.hub.send(id, p::message(kind::PONG, h.timestamp, false, &payload)),
            kind::INPUT => match serde_json::from_slice::<p::InputEvent>(&payload) {
                Ok(e) => {
                    let _ = host.input.send(e);
                }
                Err(_) => eprintln!("{name}: an input message didn't decode: {}", String::from_utf8_lossy(&payload)),
            },
            kind::HELLO => {
                if let Ok(hello) = serde_json::from_slice::<p::Hello>(&payload) {
                    println!(
                        "{name} is {} with Sill {}.",
                        p::safe_label(hello.device.as_deref().unwrap_or("a device"), 64),
                        p::safe_label(hello.app_version.as_deref().unwrap_or("?"), 16)
                    );
                }
            }
            // Only the Desktop streams here: every pick is answered with the list that says so.
            kind::SELECT_SOURCE => host.hub.send(id, host.window_list()),
            kind::CHANGE_SETTINGS => {
                // shortcut: settings are fixed in v0.1; the answer carries the unchanged state, which the
                // device shows as the PC's choice. Apply maxFPS/bitrate here when the encoder can restart.
                let token = serde_json::from_slice::<p::HostSettingsChange>(&payload).ok().and_then(|c| c.token);
                host.hub.send(id, p::json_message(kind::HOST_SETTINGS, &host.settings_state(token)));
            }
            kind::KEYFRAME_REQUEST => {
                let now = p::now();
                if now - last_keyframe_request >= 1.0 {
                    last_keyframe_request = now;
                    host.hub.request_keyframe();
                }
            }
            _ => {} // viewport, stats, menus, clipboard…: not in v0.1; unknown kinds are skipped
        }
    };
    host.hub.unregister(id);
    writing.abort();
    println!("{name} left ({reason}).");
    Ok(())
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
