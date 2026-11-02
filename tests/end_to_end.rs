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
