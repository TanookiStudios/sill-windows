//! The real program, started as a person would start it, and a device's side of the protocol
//! against it: the ask, pairing with the typed code, a pinned session (catalog, pong, picture),
//! an unpaired key refused, and the same PC (same key, same paired device) after a restart.
//! Runs on Windows in CI, so the door, TLS, DPAPI and the Bonjour registration run there for real.
//! `SILL_E2E_NO_VIDEO=1` skips the picture check where no screen can be captured (CI runners have
//! no GPU and no desktop); everywhere else a missing keyframe fails the test.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sill_windows::identity::{certificate_fingerprint, self_signed_certificate};
use sill_windows::pairing::*;
use sill_windows::protocol::{self as p, kind, Header};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// Accepts any server key and remembers it; the test pins by comparing afterwards.
#[derive(Debug, Default)]
struct Remember(Mutex<Option<[u8; 32]>>);

impl ServerCertVerifier for Remember {
    fn verify_server_cert(&self, c: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        *self.0.lock().unwrap() = certificate_fingerprint(c);
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ECDSA_NISTP256_SHA256]
    }
}

struct Device {
    pkcs8: Vec<u8>,
    cert: Vec<u8>,
    fp: [u8; 32],
}

impl Device {
    fn new() -> Device {
        let rng = ring::rand::SystemRandom::new();
        let alg = &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING;
        let pkcs8 = ring::signature::EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap().as_ref().to_vec();
        let pair = ring::signature::EcdsaKeyPair::from_pkcs8(alg, &pkcs8, &rng).unwrap();
        let cert = self_signed_certificate(&pair, &rng).unwrap();
        let fp = certificate_fingerprint(&cert).unwrap();
        Device { pkcs8, cert, fp }
    }

    /// TLS 1.3 with one ALPN, as a device dials. Returns the stream and the PC's key, or the error.
    async fn dial(&self, port: u16, alpn: &[u8]) -> Result<(tokio_rustls::client::TlsStream<TcpStream>, [u8; 32]), String> {
        let remember = Arc::new(Remember::default());
        let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(remember.clone())
            .with_client_auth_cert(vec![CertificateDer::from(self.cert.clone())], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.pkcs8.clone())))
            .unwrap();
        config.alpn_protocols = vec![alpn.to_vec()];
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.map_err(|e| e.to_string())?;
        let tls = TlsConnector::from(Arc::new(config)).connect(ServerName::try_from("sill").unwrap(), tcp).await.map_err(|e| e.to_string())?;
        let fp = remember.0.lock().unwrap().ok_or("no server key")?;
        Ok((tls, fp))
    }
}

async fn read<S: tokio::io::AsyncRead + Unpin>(s: &mut S) -> Result<(Header, Vec<u8>), String> {
    let mut h = [0u8; 14];
    tokio::time::timeout(Duration::from_secs(10), s.read_exact(&mut h)).await.map_err(|_| "timed out")?.map_err(|e| e.to_string())?;
    let h = Header::parse(&h);
    let mut payload = vec![0; h.payload_len];
    s.read_exact(&mut payload).await.map_err(|e| e.to_string())?;
    Ok((h, payload))
}

async fn pair_request<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(s: &mut S, method: &str, proof: &[u8]) -> p::PairResult {
    let req = serde_json::json!({"v": 1, "method": method, "proof": b64u(proof), "name": "E2E", "model": "Test"});
    s.write_all(&p::message(kind::PAIR_REQUEST, p::now(), false, &serde_json::to_vec(&req).unwrap())).await.unwrap();
    let (h, payload) = read(s).await.expect("a kind 20");
    assert_eq!(h.kind, kind::PAIR_RESULT);
    serde_json::from_slice(&payload).unwrap()
}

struct Host {
    child: Child,
    id: String,
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts SillHost and waits for its "PC ID" line and its first code.
fn start(home: &Path, port: u16, code: &Path) -> Host {
    let _ = std::fs::remove_file(code);
    let mut child = Command::new(env!("CARGO_BIN_EXE_SillHost"))
        .args(["--port", &port.to_string(), "--name", "E2E PC"])
        .env("SILL_HOME", home)
        .env("SILL_TEST_CODE_FILE", code)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("SillHost starts");
    let out = BufReader::new(child.stdout.take().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in out.lines().map_while(Result::ok) {
            println!("host: {line}");
            let _ = tx.send(line);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut id = None;
    while Instant::now() < deadline && (id.is_none() || !code.exists()) {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(100)) {
            if let Some(rest) = line.split("(PC ID ").nth(1) {
                id = Some(rest.trim_end_matches(").").to_string());
            }
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    Host { child, id: id.expect("the host printed its PC ID") }
}

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("sill-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_then_stream_then_restart() {
    let dir = scratch();
    let (home, code_file, port) = (dir.join("home"), dir.join("code.txt"), 17_000 + (std::process::id() % 1000) as u16);
    let host = start(&home, port, &code_file);
    let device = Device::new();

    // 1. The ask: answered "shown".
    let (mut s, pc_key) = device.dial(port, b"sill-pair/1").await.expect("pairing connection");
    assert_eq!(mac_id(&pc_key), host.id, "the key the door presents is the PC ID's");
    assert_eq!(pair_request(&mut s, "ask", &[]).await, p::PairResult::refused("shown"));

    // 2. The typed code, a wrong one first.
    let code = std::fs::read_to_string(&code_file).unwrap();
    let key = code_key(&code, &pc_key);
    let (mut s, _) = device.dial(port, b"sill-pair/1").await.unwrap();
    let wrong = pair_request(&mut s, "code", &[0u8; 32]).await;
    assert_eq!((wrong.reason.as_deref(), wrong.tries_left), (Some("code"), Some(4)), "a wrong proof costs a try");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let proof_d = {
        let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key);
        ring::hmac::sign(&k, &[b"sill-pair-v1 device\0".as_slice(), &device.fp, &pc_key].concat()).as_ref().to_vec()
    };
    let (mut s, _) = device.dial(port, b"sill-pair/1").await.unwrap();
    let mut r = pair_request(&mut s, "code", &proof_d).await;
    for _ in 0..20 {
        if r.reason.as_deref() != Some("busy") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let (mut s, _) = device.dial(port, b"sill-pair/1").await.unwrap();
        r = pair_request(&mut s, "code", &proof_d).await;
    }
    assert!(r.ok, "paired: {r:?}");
    assert_eq!(b64u_decode(r.proof.as_deref().unwrap()).unwrap(), mac_proof(&key, &pc_key, &device.fp), "proof_M");
    assert_eq!(r.mac_id.as_deref(), Some(host.id.as_str()));
    assert_eq!(b64u_decode(r.recognition_key.as_deref().unwrap()).map(|k| k.len()), Some(32));

    // 3. The session.
    session(&device, port, pc_key).await;

    // 4. A stranger's key: refused inside the handshake or on its first read.
    let stranger = Device::new();
    if let Ok((mut s, _)) = stranger.dial(port, b"sill/1").await {
        let _ = s.write_all(&p::message(kind::HELLO, p::now(), false, b"{}")).await;
        assert!(read(&mut s).await.is_err(), "an unpaired key gets nothing");
    }

    // 5. After a restart: the same key, and the paired device still gets in.
    drop(host);
    let again = start(&home, port, &code_file);
    let (_, key_after) = device.dial(port, b"sill-pair/1").await.unwrap();
    assert_eq!(key_after, pc_key, "the identity survives a restart");
    assert_eq!(again.id, mac_id(&pc_key));
    session(&device, port, pc_key).await;
}

async fn session(device: &Device, port: u16, pc_key: [u8; 32]) {
    let (mut s, key) = device.dial(port, b"sill/1").await.expect("a pinned session");
    assert_eq!(key, pc_key, "pinned");
    s.write_all(&p::message(kind::HELLO, p::now(), false, br#"{"appVersion":"1.0","device":"E2E"}"#)).await.unwrap();
    s.write_all(&p::message(kind::SELECT_SOURCE, p::now(), false, br#"{"desktop":{}}"#)).await.unwrap();
    let ping = 777.5f64.to_bits().to_be_bytes();
    s.write_all(&p::message(kind::PING, 42.0, false, &ping)).await.unwrap();
    let mut kinds = Vec::new();
    let (mut pong, mut ps, mut keyframe) = (false, false, false);
    let want_video = std::env::var_os("SILL_E2E_NO_VIDEO").is_none();
    let deadline = Instant::now() + Duration::from_secs(if want_video { 10 } else { 3 });
    while Instant::now() < deadline && !(pong && (!want_video || keyframe)) {
        let Ok(Ok((h, payload))) = tokio::time::timeout(Duration::from_secs(1), read(&mut s)).await else { continue };
        if kinds.len() < 3 && h.kind != kind::TICK && h.kind != kind::PONG {
            kinds.push(h.kind);
        }
        match h.kind {
            kind::PONG => pong = payload == ping && h.timestamp == 42.0,
            kind::PARAMETER_SETS => ps = true,
            kind::FRAME => keyframe |= ps && h.is_keyframe,
            _ => {}
        }
    }
    assert_eq!(&kinds[..2], &[kind::WINDOW_LIST, kind::HOST_SETTINGS], "the catalog comes first: {kinds:?}");
    assert!(pong, "the ping is echoed");
    if want_video {
        assert!(keyframe, "parameter sets, then a keyframe");
    }
}

/// Real clicks, wheel turns and typing on a real Windows desktop, read back from a real window. Only
/// where `SILL_E2E_INPUT=1` says a desktop is there to use (it moves the mouse and types).
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn input_lands_on_windows() {
    if std::env::var_os("SILL_E2E_INPUT").is_none() {
        return;
    }
    let dir = scratch();
    let (home, code_file, port) = (dir.join("home"), dir.join("code.txt"), 18_000 + (std::process::id() % 1000) as u16);
    let _host = start(&home, port, &code_file);
    let device = Device::new();
    let (_, pc_key) = device.dial(port, b"sill-pair/1").await.unwrap();
    let code = std::fs::read_to_string(&code_file).unwrap();
    let key = code_key(&code, &pc_key);
    let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key);
    let proof = ring::hmac::sign(&k, &[b"sill-pair-v1 device\0".as_slice(), &device.fp, &pc_key].concat()).as_ref().to_vec();
    let mut paired = false;
    for _ in 0..20 {
        let (mut s, _) = device.dial(port, b"sill-pair/1").await.unwrap();
        if pair_request(&mut s, "code", &proof).await.ok {
            paired = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(paired, "paired");

    let probe = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/windows_probe.ps1");
    let mut window = Command::new("powershell")
        .args(["-NoProfile", "-STA", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&probe)
        .arg("-Dir")
        .arg(&dir)
        .spawn()
        .unwrap();
    let place = dir.join("where.json");
    let end = Instant::now() + Duration::from_secs(30);
    while !place.exists() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    let w: serde_json::Value = serde_json::from_slice(&std::fs::read(&place).expect("the probe window opened")).unwrap();
    let n = |k: &str| w[k].as_f64().unwrap();
    let (sw, sh) = (n("screenW"), n("screenH"));
    println!("probe window: {w}");

    let (mut s, _) = device.dial(port, b"sill/1").await.unwrap();
    let send = |json: serde_json::Value| p::message(kind::INPUT, p::now(), false, &serde_json::to_vec(&json).unwrap());
    let pointer = |action: &str, x: f64, y: f64| send(serde_json::json!({"pointer": {"_0": action, "x": x / sw, "y": y / sh}}));
    let key_event = |usage: u16, down: bool, modifiers: u64| send(serde_json::json!({"key": {"hidUsage": usage, "down": down, "modifiers": modifiers}}));
    let text = |t: &str| send(serde_json::json!({"text": {"_0": t}}));
    let mut go = Vec::new();
    // Left and right click in the panel, and the wheel over it.
    let (px, py) = (n("panelX"), n("panelY"));
    go.push(pointer("move", px, py));
    go.push(pointer("leftDown", px, py));
    go.push(pointer("leftUp", px, py));
    go.push(pointer("rightDown", px, py));
    go.push(pointer("rightUp", px, py));
    go.push(send(serde_json::json!({"scroll": {"x": px / sw, "y": py / sh, "dx": 0.0, "dy": 100.0 / sh}})));
    // Click the text box, type, select all with Command-A, and type over it.
    let (tx, ty) = (n("textX"), n("textY"));
    go.push(pointer("move", tx, ty));
    go.push(pointer("leftDown", tx, ty));
    go.push(pointer("leftUp", tx, ty));
    go.push(text("abc"));
    go.push(key_event(0x04, true, 1 << 20));
    go.push(key_event(0x04, false, 0));
    go.push(text("Hé 😀 x"));
    go.push(text("\u{8}"));
    go.push(key_event(0x50, true, 0)); // left arrow, then Shift-less typing lands before the last character
    go.push(key_event(0x50, false, 0));
    go.push(key_event(0x07, true, 1 << 17)); // Shift-D
    go.push(key_event(0x07, false, 0));
    for m in go {
        s.write_all(&m).await.unwrap();
        s.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let events = std::fs::read_to_string(dir.join("events.txt")).unwrap_or_default();
    let _ = window.kill();
    let lines: Vec<&str> = events.trim_start_matches('\u{feff}').lines().collect();
    let last_text = lines.iter().rev().find_map(|l| l.strip_prefix("text ")).expect("something was typed");
    use base64::Engine;
    let typed = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(last_text).unwrap()).unwrap();
    println!("the window saw: {lines:?}\nfinal text: {typed:?}");
    assert!(lines.iter().any(|l| l.starts_with("down Left")), "left click");
    assert!(lines.iter().any(|l| l.starts_with("down Right")), "right click");
    assert!(lines.iter().any(|l| l.starts_with("wheel ") && l[6..].parse::<i32>().unwrap() > 0), "wheel forward");
    assert_eq!(typed, "Hé 😀D ", "text, Backspace, Command-A as Ctrl-A, an arrow and a Shift key");
}

/// The rest of a real desktop: the window list, thumbnails, icons, apps, the clipboard both ways, a
/// held key repeating, the PC's own pointer, and window commands, read back from Windows itself.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn catalog_and_clipboard_on_windows() {
    if std::env::var_os("SILL_E2E_INPUT").is_none() {
        return;
    }
    use std::collections::HashMap;
    let dir = scratch();
    let (home, code_file, port) = (dir.join("home"), dir.join("code.txt"), 19_000 + (std::process::id() % 1000) as u16);
    let _host = start(&home, port, &code_file);
    let device = Device::new();
    let (_, pc_key) = device.dial(port, b"sill-pair/1").await.unwrap();
    let key = code_key(&std::fs::read_to_string(&code_file).unwrap(), &pc_key);
    let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key);
    let proof = ring::hmac::sign(&k, &[b"sill-pair-v1 device\0".as_slice(), &device.fp, &pc_key].concat()).as_ref().to_vec();
    let mut paired = false;
    for _ in 0..20 {
        let (mut s, _) = device.dial(port, b"sill-pair/1").await.unwrap();
        if pair_request(&mut s, "code", &proof).await.ok {
            paired = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(paired, "paired");

    let probe = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/windows_probe.ps1");
    let mut window = Command::new("powershell")
        .args(["-NoProfile", "-STA", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&probe).arg("-Dir").arg(&dir)
        .spawn().unwrap();
    let place = dir.join("where.json");
    let end = Instant::now() + Duration::from_secs(30);
    while !place.exists() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    let w: serde_json::Value = serde_json::from_slice(&std::fs::read(&place).expect("the probe window opened")).unwrap();
    let n = |k: &str| w[k].as_f64().unwrap();
    let (sw, sh) = (n("screenW"), n("screenH"));

    let (mut s, _) = device.dial(port, b"sill/1").await.unwrap();
    let mut seen: HashMap<u8, Vec<Vec<u8>>> = HashMap::new();
    async fn gather<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(s: &mut S, seconds: f64, seen: &mut HashMap<u8, Vec<Vec<u8>>>) {
        let end = Instant::now() + Duration::from_secs_f64(seconds);
        while Instant::now() < end {
            // Pings four times a second, as a device, so held keys go on repeating.
            s.write_all(&p::message(kind::PING, p::now(), false, &[0; 8])).await.unwrap();
            s.flush().await.unwrap();
            let until = Instant::now() + Duration::from_millis(100);
            while let Ok(Ok((h, payload))) = tokio::time::timeout(until.saturating_duration_since(Instant::now()), read(s)).await {
                if h.kind != kind::TICK && h.kind != kind::PONG {
                    seen.entry(h.kind).or_default().push(payload);
                }
            }
        }
    }
    gather(&mut s, 6.0, &mut seen).await;

    // The list, with this window in it.
    let lists = seen.get(&kind::WINDOW_LIST).expect("a window list");
    let list: serde_json::Value = serde_json::from_slice(lists.last().unwrap()).unwrap();
    println!("windows: {}", list["windows"]);
    let me = list["windows"].as_array().unwrap().iter().find(|w| w["title"] == "Sill input probe").expect("the probe window is listed");
    let id = me["id"].as_u64().unwrap() as u32;
    assert!(me["bundleID"].as_str().unwrap().to_lowercase().ends_with("powershell.exe"), "{me}");
    assert_eq!((list["gestures"].as_i64(), list["clipboard"].as_i64(), list["keyRepeat"].as_i64()), (Some(1), Some(1), Some(1)));
    let thumbs = seen.get(&kind::THUMBNAIL).expect("thumbnails");
    let mine = thumbs.iter().find(|t| t[..4] == id.to_be_bytes()).expect("a thumbnail of the probe window");
    assert_eq!(&mine[4..6], &[0xFF, 0xD8], "a JPEG");
    let icons = seen.get(&kind::APP_ICON).expect("icons");
    assert!(icons.iter().any(|i| { let n = u16::from_be_bytes([i[0], i[1]]) as usize; i[2 + n..].starts_with(&[0x89, b'P', b'N', b'G']) }), "a PNG icon");
    let apps: serde_json::Value = serde_json::from_slice(&seen[&kind::APP_LIST][0]).unwrap();
    println!("{} apps listed, {} icons, {} thumbnails", apps.as_array().unwrap().len(), icons.len(), thumbs.len());
    assert!(seen.contains_key(&kind::MAC_INFO), "kind 18");

    // The clipboard, a device's text landing on Windows.
    let powershell = |script: &str| Command::new("powershell").args(["-NoProfile", "-STA", "-Command", script]).output().unwrap();
    s.write_all(&p::message(kind::CLIPBOARD, p::now(), false, br#"{"text":"from the device \u00e9"}"#)).await.unwrap();
    s.flush().await.unwrap();
    gather(&mut s, 0.6, &mut seen).await;
    let text = String::from_utf8_lossy(&powershell("[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-Clipboard").stdout).trim().to_string();
    assert_eq!(text, "from the device é", "the device's text is on Windows' clipboard");
    gather(&mut s, 0.6, &mut seen).await;
    assert!(!seen.contains_key(&kind::CLIPBOARD), "a device's own text isn't sent back to it");
    // And a copy on Windows reaching the device.
    powershell("Set-Clipboard -Value 'copied on the PC'");
    gather(&mut s, 1.2, &mut seen).await;
    let copied: serde_json::Value = serde_json::from_slice(&seen.get(&kind::CLIPBOARD).expect("the copy came")[0]).unwrap();
    assert_eq!(copied["text"], "copied on the PC");

    // A held key repeats, at Windows' own rate, and stops at its up.
    let pointer = |action: &str, x: f64, y: f64| p::message(kind::INPUT, p::now(), false, &serde_json::to_vec(&serde_json::json!({"pointer": {"_0": action, "x": x / sw, "y": y / sh}})).unwrap());
    let key_event = |down: bool| p::message(kind::INPUT, p::now(), false, &serde_json::to_vec(&serde_json::json!({"key": {"hidUsage": 0x1D, "down": down, "modifiers": 0}})).unwrap());
    let (tx, ty) = (n("textX"), n("textY"));
    for m in [pointer("leftDown", tx, ty), pointer("leftUp", tx, ty), key_event(true)] {
        s.write_all(&m).await.unwrap();
        s.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    gather(&mut s, 1.5, &mut seen).await;
    s.write_all(&key_event(false)).await.unwrap();
    s.flush().await.unwrap();
    gather(&mut s, 0.3, &mut seen).await;
    let events = std::fs::read_to_string(dir.join("events.txt")).unwrap_or_default();
    let count = |ev: &str| ev.trim_start_matches('\u{feff}').lines().filter(|l| l.starts_with("text ")).count();
    let held = count(&events);
    println!("the held key typed {held} letters");
    assert!(held >= 4, "a key held 1.5 s repeats: {held}");
    gather(&mut s, 0.7, &mut seen).await;
    assert_eq!(count(&std::fs::read_to_string(dir.join("events.txt")).unwrap()), held, "it stopped at its up");

    // The PC's own pointer, moved by something other than this device.
    tokio::time::sleep(Duration::from_millis(700)).await;
    seen.remove(&kind::MAC_POINTER);
    powershell("Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.Cursor]::Position = New-Object System.Drawing.Point(300,200)");
    gather(&mut s, 0.8, &mut seen).await;
    let at: serde_json::Value = serde_json::from_slice(seen.get(&kind::MAC_POINTER).expect("kind 26").last().unwrap()).unwrap();
    println!("pointer report: {at}");
    assert!(at["inside"] == true && (at["x"].as_f64().unwrap() - 300.0 / sw).abs() < 0.01, "{at}");

    // Picking the window: the list says so to everyone, and it's in front.
    let pick = serde_json::to_vec(&serde_json::json!({"window": {"_0": id}})).unwrap();
    s.write_all(&p::message(kind::SELECT_SOURCE, p::now(), false, &pick)).await.unwrap();
    s.flush().await.unwrap();
    seen.remove(&kind::WINDOW_LIST);
    gather(&mut s, 0.8, &mut seen).await;
    let list: serde_json::Value = serde_json::from_slice(seen[&kind::WINDOW_LIST].last().unwrap()).unwrap();
    assert_eq!(list["active"]["window"]["_0"].as_u64(), Some(id as u64), "{}", list["active"]);

    // Window commands, as the bar's long-press menu sends them.
    for action in ["fullScreen", "minimize", "close"] {
        let c = serde_json::to_vec(&serde_json::json!({"id": id, "action": action})).unwrap();
        s.write_all(&p::message(kind::WINDOW_COMMAND, p::now(), false, &c)).await.unwrap();
        s.flush().await.unwrap();
        gather(&mut s, 0.8, &mut seen).await;
    }
    let events = std::fs::read_to_string(dir.join("events.txt")).unwrap_or_default();
    println!("window saw: {:?}", events.lines().filter(|l| !l.starts_with("text ")).collect::<Vec<_>>());
    assert!(events.contains("state Maximized"), "full screen");
    assert!(events.contains("state Minimized"), "minimize");
    assert!(events.contains("closed"), "close");
    // It closed: the next list drops it and the stream goes back to the Desktop.
    seen.remove(&kind::WINDOW_LIST);
    gather(&mut s, 2.5, &mut seen).await;
    let list: serde_json::Value = serde_json::from_slice(seen.get(&kind::WINDOW_LIST).expect("a new list").last().unwrap()).unwrap();
    assert!(list["active"].get("desktop").is_some(), "back to the Desktop: {}", list["active"]);
    let _ = window.kill();
}
