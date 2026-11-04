//! Protocol conformance: every message this host sends, and every message it reads, against the
//! bytes Sill's own Swift code (Sources/StreamProtocol) produces. The fixtures come from
//! `conformance/` (`swift run SillConformance dump tests/fixtures/swift`); `SWIFT_FIXTURES` points at
//! a fresh dump instead. This test also writes what the Rust side produces to tests/fixtures/rust,
//! which `swift run SillConformance check tests/fixtures/rust` decodes with Sill's own decoders.

use serde_json::Value;
use sill_windows::cursor;
use sill_windows::identity::{self_signed_certificate, certificate_fingerprint};
use sill_windows::pairing::*;
use sill_windows::protocol::{self as p, kind, Header, InputEvent, PointerAction, ScrollPhase};
use std::collections::HashMap;
use std::path::PathBuf;

const TS: f64 = 1234.5;

fn dir() -> PathBuf {
    std::env::var_os("SWIFT_FIXTURES").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/swift"))
}

fn swift(name: &str) -> Vec<u8> {
    std::fs::read(dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e} (run the Swift dump first)"))
}

fn split(m: &[u8]) -> (Header, &[u8]) {
    let h = Header::parse(m[..14].try_into().unwrap());
    assert_eq!(h.payload_len, m.len() - 14, "length field");
    (h, &m[14..])
}

/// JSON compared by meaning: keys in any order (Swift's JSONEncoder has none), numbers by value
/// (Swift writes 1.0 as 1).
fn same_json(a: &[u8], b: &[u8]) -> bool {
    fn norm(v: Value) -> Value {
        match v {
            Value::Number(n) => serde_json::json!(n.as_f64().unwrap()),
            Value::Array(a) => Value::Array(a.into_iter().map(norm).collect()),
            Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, norm(v))).collect()),
            v => v,
        }
    }
    norm(serde_json::from_slice(a).unwrap()) == norm(serde_json::from_slice(b).unwrap())
}

/// A host message: the header byte for byte (kind, timestamp, keyframe flag), the JSON by meaning.
fn assert_json_message(name: &str, ours: &[u8]) {
    let theirs = swift(name);
    let ((h1, p1), (h2, p2)) = (split(ours), split(&theirs));
    assert_eq!((h1.kind, h1.timestamp.to_bits(), h1.is_keyframe), (h2.kind, h2.timestamp.to_bits(), h2.is_keyframe), "{name}: header");
    assert!(same_json(p1, p2), "{name}:\n ours   {}\n theirs {}", String::from_utf8_lossy(p1), String::from_utf8_lossy(p2));
}

fn json(kind: u8, v: &impl serde::Serialize) -> Vec<u8> {
    p::message(kind, TS, false, &serde_json::to_vec(v).unwrap())
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn vectors() -> HashMap<String, String> {
    serde_json::from_slice(&swift("vectors.json")).unwrap()
}

fn window_list() -> p::WindowList {
    p::WindowList {
        mac_name: "Test PC".into(), windows: vec![], active: p::StreamSource::Desktop {},
        launch_id: Some("L1".into()), host_version: Some("0.1.0".into()), protocol: Some(1),
        gestures: None, clipboard: None, key_repeat: None,
    }
}

fn full_list() -> p::WindowList {
    p::WindowList {
        windows: vec![p::WindowInfo { id: 77, title: "Doc".into(), app_name: "Word".into(), bundle_id: "C:\\w.exe".into(), width: 800, height: 600 }],
        active: p::StreamSource::Window { id: 77 },
        gestures: Some(1), clipboard: Some(1), key_repeat: Some(1),
        ..window_list()
    }
}

fn apps() -> Vec<p::AppInfo> {
    vec![p::AppInfo { name: "Calc".into(), bundle_id: "C:\\c.lnk".into() }]
}

fn settings() -> p::HostSettingsState {
    p::HostSettingsState {
        settings: p::StreamSettings { max_fps: 60, bitrate: 15_000_000, capture_scale: 1.0, prioritize_speed: true, virtual_display: false, send_audio: None },
        persistent: false, virtual_display_available: false, virtual_display_note: Some("Not on Windows.".into()),
        software_encoder: false,
        stream: Some(p::RunningStream { width: 1920, height: 1080, fps: 60, mbps: 15, on_virtual_display: false }),
        answering: Some(3), audio_note: None, session_paired: Some(true),
    }
}

fn ok_result(v: &HashMap<String, String>) -> p::PairResult {
    let (fp_mac, fp_dev) = (unhex(&v["fpMac"]), unhex(&v["fpDevice"]));
    p::PairResult {
        ok: true,
        proof: Some(b64u(&mac_proof(&unhex(&v["secret"]), &fp_mac, &fp_dev))),
        mac_id: Some(mac_id(&fp_mac)),
        name: Some("Test PC".into()),
        recognition_key: Some(b64u(&unhex(&v["recognitionKey"]))),
        ..Default::default()
    }
}

#[test]
fn host_messages_match_swift() {
    let v = vectors();
    let ps = p::encode_parameter_sets(4, &[
        unhex("40010c01ffff016000000300b0000003000003005dac09"),
        unhex("420101016000000300b0000003000003005da00280802d16"),
        unhex("4401c172b46240"),
    ]);
    assert_eq!(p::message(kind::PARAMETER_SETS, TS, true, &ps), swift("ps.bin"), "parameter sets, byte for byte");
    assert_eq!(p::message(kind::FRAME, TS, true, &unhex("000000032601af00000002d001")), swift("frame.bin"), "frame");
    assert_eq!(p::message(kind::TICK, TS, false, &[]), swift("tick.bin"), "tick");
    // A pong is the ping's header timestamp and payload, unchanged.
    let (ph, pp) = { let m = swift("ping.bin"); let (h, p) = split(&m); (h, p.to_vec()) };
    assert_eq!(p::message(kind::PONG, ph.timestamp, false, &pp), swift("pong.bin"), "pong");
    assert_json_message("windowlist.bin", &json(kind::WINDOW_LIST, &window_list()));
    assert_json_message("windowlist_full.bin", &json(kind::WINDOW_LIST, &full_list()));
    assert_eq!(p::message(kind::THUMBNAIL, TS, false, &p::encode_thumbnail(77, &[0xFF, 0xD8, 1])), swift("thumbnail.bin"), "thumbnail");
    assert_eq!(p::message(kind::APP_ICON, TS, false, &p::encode_icon("C:\\w.exe", &[0x89, 0x50, 1])), swift("icon.bin"), "icon");
    assert_json_message("applist.bin", &json(kind::APP_LIST, &apps()));
    assert_eq!(p::message(kind::CURSOR_SHAPE, TS, false, &cursor::blob(1.5, 2.0, 32.0, 32.0, &[0x89, 0x50, 1])), swift("cursor.bin"), "cursor shape");
    assert_json_message("pointer.bin", &json(kind::MAC_POINTER, &p::MacPointer { x: Some(0.25), y: Some(0.5), inside: true, seen: 12 }));
    assert_json_message("pointer_off.bin", &json(kind::MAC_POINTER, &p::MacPointer { x: None, y: None, inside: false, seen: 12 }));
    assert_json_message("clipboard_out.bin", &json(kind::CLIPBOARD, &p::Clipboard { text: Some("Hé".into()), stop: None }));
    assert_json_message("hostsettings.bin", &json(kind::HOST_SETTINGS, &settings()));
    assert_json_message("pairresult_ok.bin", &json(kind::PAIR_RESULT, &ok_result(&v)));
    assert_json_message("pairresult_shown.bin", &json(kind::PAIR_RESULT, &p::PairResult::refused("shown")));
    assert_json_message("pairresult_code.bin", &json(kind::PAIR_RESULT, &p::PairResult { tries_left: Some(3), ..p::PairResult::refused("code") }));
    assert_json_message("pairresult_busy.bin", &json(kind::PAIR_RESULT, &p::PairResult { retry_after: Some(1.5), ..p::PairResult::refused("busy") }));
    assert_json_message("goodbye_quit.bin", &json(kind::GOODBYE, &p::Goodbye { reason: "quit".into() }));
}

fn decode<T: serde::de::DeserializeOwned>(name: &str, want_kind: u8) -> T {
    let m = swift(name);
    let (h, payload) = split(&m);
    assert_eq!(h.kind, want_kind, "{name}: kind");
    serde_json::from_slice(payload).unwrap_or_else(|e| panic!("{name}: {e}: {}", String::from_utf8_lossy(payload)))
}

#[test]
fn device_messages_decode() {
    use InputEvent::*;
    let cases: [(&str, InputEvent); 10] = [
        ("in_move.bin", Pointer { action: PointerAction::Move, x: 0.25, y: 0.75 }),
        ("in_leftdown.bin", Pointer { action: PointerAction::LeftDown, x: 0.5, y: 0.5 }),
        ("in_leftup.bin", Pointer { action: PointerAction::LeftUp, x: 0.5, y: 0.5 }),
        ("in_rightdown.bin", Pointer { action: PointerAction::RightDown, x: 0.1, y: 0.2 }),
        ("in_rightup.bin", Pointer { action: PointerAction::RightUp, x: 0.1, y: 0.2 }),
        ("in_scroll.bin", Scroll { x: 0.5, y: 0.5, dx: -0.01, dy: 0.05 }),
        ("in_scrollgesture.bin", ScrollGesture { phase: ScrollPhase::Began, x: 0.5, y: 0.5 }),
        ("in_momentum.bin", ScrollGesture { phase: ScrollPhase::MomentumEnded, x: 0.5, y: 0.5 }),
        ("in_text.bin", Text { text: "Hé\n".into() }),
        ("in_key.bin", Key { hid_usage: 6, down: true, modifiers: 1 << 20 }),
    ];
    for (name, want) in cases {
        assert_eq!(decode::<InputEvent>(name, kind::INPUT), want, "{name}");
    }
    assert_eq!(decode::<p::TrackpadGesture>("gesture.bin", kind::GESTURE).gesture, "swipeUp");
    use sill_windows::clipboard::{from_device, Ask};
    assert_eq!(from_device(split(&swift("clipboard_sub.bin")).1), Ask::Subscribe);
    assert_eq!(from_device(split(&swift("clipboard_text.bin")).1), Ask::Set("Hé".into()));
    assert_eq!(from_device(split(&swift("clipboard_stop.bin")).1), Ask::Stop);
    assert_eq!(decode::<p::LaunchApp>("launch_app.bin", kind::LAUNCH_APP).bundle_id, "C:\\apps\\calc.lnk");
    let c: p::WindowCommand = decode("window_command.bin", kind::WINDOW_COMMAND);
    assert_eq!((c.id, c.action.as_str()), (77, "fullScreen"));
    let v: p::Viewport = decode("viewport_scaled.bin", kind::VIEWPORT);
    assert_eq!((v.width, v.height, v.scale), (1180.0, 820.0, Some(1.25)));
    assert_eq!(decode::<p::Viewport>("viewport.bin", kind::VIEWPORT).scale, None);
    let hello: p::Hello = decode("hello.bin", kind::HELLO);
    assert_eq!((hello.app_version.as_deref(), hello.device.as_deref()), (Some("1.0"), Some("iPad (iPad14,1)")));
    assert_eq!(decode::<p::StreamSource>("select_desktop.bin", kind::SELECT_SOURCE), p::StreamSource::Desktop {});
    assert_eq!(decode::<p::StreamSource>("select_none.bin", kind::SELECT_SOURCE), p::StreamSource::None {});
    assert_eq!(decode::<p::StreamSource>("select_window.bin", kind::SELECT_SOURCE), p::StreamSource::Window { id: 77 });
    assert_eq!(decode::<p::HostSettingsChange>("change_settings.bin", kind::CHANGE_SETTINGS).token, Some(7));
    let ask: p::PairRequest = decode("pair_ask.bin", kind::PAIR_REQUEST);
    assert_eq!((ask.v, ask.method.as_str(), ask.proof.as_str(), ask.name.as_str()), (1, "ask", "", "iPad"));
    let qr: p::PairRequest = decode("pair_qr.bin", kind::PAIR_REQUEST);
    let v = vectors();
    let proof = b64u_decode(&qr.proof).expect("canonical base64url");
    assert!(device_proof_valid(&unhex(&v["secret"]), &proof, &unhex(&v["fpDevice"]), &unhex(&v["fpMac"])), "the device's QR proof checks here");
    let (h, payload) = { let m = swift("keyframe_request.bin"); let (h, p) = split(&m); (h, p.len()) };
    assert_eq!((h.kind, payload), (kind::KEYFRAME_REQUEST, 0));
}

#[test]
fn pairing_vectors_match_swift() {
    let v = vectors();
    let (mac_point, dev_point) = (unhex(&v["macPoint"]), unhex(&v["devicePoint"]));
    assert_eq!(spki_der(&mac_point), unhex(&v["macSPKI"]), "SPKI");
    assert_eq!(fingerprint(&mac_point).to_vec(), unhex(&v["fpMac"]), "fingerprint");
    assert_eq!(fingerprint(&dev_point).to_vec(), unhex(&v["fpDevice"]));
    assert_eq!(b64u(&unhex(&v["fpMac"])), v["fpMacB64U"], "base64url");
    assert_eq!(mac_id(&unhex(&v["fpMac"])), v["macID"], "Mac ID");
    let (fp_mac, fp_dev) = (unhex(&v["fpMac"]), unhex(&v["fpDevice"]));
    let secret = unhex(&v["secret"]);
    assert!(device_proof_valid(&secret, &unhex(&v["qrDeviceProof"]), &fp_dev, &fp_mac), "QR proof_D");
    assert!(!device_proof_valid(&secret, &unhex(&v["qrDeviceProof"]), &fp_mac, &fp_dev), "a proof for swapped keys fails");
    assert_eq!(mac_proof(&secret, &fp_mac, &fp_dev), unhex(&v["qrMacProof"]), "QR proof_M");
    let code = &v["code"];
    assert_eq!(check_digit(&code.bytes().map(|b| b - b'0').collect::<Vec<_>>()), 0, "Damm");
    let k = code_key(code, &fp_mac);
    assert_eq!(k.to_vec(), unhex(&v["codeKey"]), "PBKDF2 code key");
    assert!(device_proof_valid(&k, &unhex(&v["codeDeviceProof"]), &fp_dev, &fp_mac), "code proof_D");
    assert_eq!(mac_proof(&k, &fp_mac, &fp_dev), unhex(&v["codeMacProof"]), "code proof_M");
    let nonce: [u8; 6] = unhex(&v["nonce"]).try_into().unwrap();
    assert_eq!(recognition_tag(&unhex(&v["recognitionKey"]), nonce), v["tag"], "recognition tag");
    let link = pair_link(&fp_mac, &secret, "Maddie's PC & Co", 7455, &["192.168.1.20".into(), "fd7a:115c:a1e0::1234".into()]);
    assert_eq!(link, v["linkURL"], "pairing link");
    assert_eq!(certificate_fingerprint(&unhex(&v["certDER"])).map(|f| f.to_vec()), Some(fp_mac), "the fingerprint of Swift's certificate");
    assert_eq!(b64u_decode("abc="), None, "padding refused");
    assert_eq!(b64u_decode("ab+/"), None, "standard alphabet refused");
}

/// What the Rust side makes, for `SillConformance check` to decode with Sill's own code.
#[test]
fn write_rust_fixtures() {
    let v = vectors();
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust");
    std::fs::create_dir_all(&out).unwrap();
    let w = |name: &str, d: Vec<u8>| std::fs::write(out.join(name), d).unwrap();
    let ps = p::encode_parameter_sets(4, &[vec![0x40, 1], vec![0x42, 1], vec![0x44, 1]]);
    w("ps.bin", p::message(kind::PARAMETER_SETS, TS, true, &ps));
    w("frame.bin", p::message(kind::FRAME, TS, true, &[0, 0, 0, 3, 0x26, 1, 0xaf]));
    w("tick.bin", p::message(kind::TICK, TS, false, &[]));
    w("pong.bin", p::message(kind::PONG, TS, false, &99.25f64.to_bits().to_be_bytes()));
    w("windowlist.bin", json(kind::WINDOW_LIST, &window_list()));
    w("hostsettings.bin", json(kind::HOST_SETTINGS, &settings()));
    w("goodbye.bin", json(kind::GOODBYE, &p::Goodbye { reason: "quit".into() }));
    w("pairresult_shown.bin", json(kind::PAIR_RESULT, &p::PairResult::refused("shown")));
    w("pairresult_code.bin", json(kind::PAIR_RESULT, &p::PairResult { tries_left: Some(3), ..p::PairResult::refused("code") }));
    w("pairresult_ok.bin", json(kind::PAIR_RESULT, &ok_result(&v)));
    w("windowlist_full.bin", json(kind::WINDOW_LIST, &full_list()));
    w("thumbnail.bin", p::message(kind::THUMBNAIL, TS, false, &p::encode_thumbnail(77, &[0xFF, 0xD8, 1])));
    w("icon.bin", p::message(kind::APP_ICON, TS, false, &p::encode_icon("C:\\w.exe", &[0x89, 0x50, 1])));
    w("applist.bin", json(kind::APP_LIST, &apps()));
    w("cursor.bin", p::message(kind::CURSOR_SHAPE, TS, false, &cursor::blob(1.5, 2.0, 32.0, 32.0, &[0x89, 0x50, 1])));
    w("pointer.bin", json(kind::MAC_POINTER, &p::MacPointer { x: Some(0.25), y: Some(0.5), inside: true, seen: 12 }));
    w("pointer_off.bin", json(kind::MAC_POINTER, &p::MacPointer { x: None, y: None, inside: false, seen: 12 }));
    let mut eld = sill_windows::audio::Eld::new().unwrap();
    let pcm: Vec<i16> = (0..sill_windows::audio::FRAMES * 2).map(|i| ((i as f32 / 40.0).sin() * 8000.0) as i16).collect();
    let packet = (0..4).filter_map(|_| eld.encode(&pcm)).last().unwrap();
    // A second of a 440 Hz tone, for a one-off decode with Apple's decoder.
    let mut tone = sill_windows::audio::Eld::new().unwrap();
    let mut packets = Vec::new();
    for block in 0..100usize {
        let pcm: Vec<i16> = (0..sill_windows::audio::FRAMES)
            .flat_map(|i| {
                let t = (block * sill_windows::audio::FRAMES + i) as f64 / 48_000.0;
                let v = ((t * 440.0 * std::f64::consts::TAU).sin() * 9830.0) as i16;
                [v, v]
            })
            .collect();
        if let Some(p) = tone.encode(&pcm) {
            packets.extend_from_slice(&(p.len() as u16).to_be_bytes());
            packets.extend_from_slice(&p);
        }
    }
    w("audio_tone.packets", packets);
    w("audio_format.bin", sill_windows::audio::format_message(7, &eld.cookie, eld.priming));
    w("audio_packet.bin", sill_windows::audio::packet_message(7, 9, true, &packet, TS));
    w("clipboard_out.bin", json(kind::CLIPBOARD, &p::Clipboard { text: Some("Hé".into()), stop: None }));
    // Signed as the host signs it, through the real identity code.
    let home = std::env::temp_dir().join(format!("sill-conformance-{}", std::process::id()));
    std::env::set_var("SILL_HOME", &home);
    let id = sill_windows::identity::Identity::load_or_create().unwrap();
    let info = p::MacInfo {
        v: 1, mac_id: mac_id(&id.fingerprint), name: "Test PC".into(), issued_at: 1234.5, remote_access: true, remote_port: 7455, internet: false,
        addresses: vec![p::MacAddress { host: "100.101.102.103".into(), kind: "vpn".into(), via: "Tailscale".into() }],
    };
    let bytes = serde_json::to_vec(&info).unwrap();
    use base64::Engine;
    let b = base64::engine::general_purpose::STANDARD;
    w("macinfo.bin", json(kind::MAC_INFO, &p::SignedMacInfo { info: b.encode(&bytes), key: b.encode(&id.point), sig: b.encode(id.sign(&bytes).unwrap()) }));
    let _ = std::fs::remove_dir_all(&home);
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::EcdsaKeyPair::generate_pkcs8(&ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
    let pair = ring::signature::EcdsaKeyPair::from_pkcs8(&ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
    let cert = self_signed_certificate(&pair, &rng).unwrap();
    let hex = |d: &[u8]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let fp_mac = unhex(&v["fpMac"]);
    let vectors = serde_json::json!({
        "linkURL": pair_link(&fp_mac, &unhex(&v["secret"]), "Maddie's PC & Co", 7455, &["192.168.1.20".into(), "fd7a:115c:a1e0::1234".into()]),
        "tag": recognition_tag(&unhex(&v["recognitionKey"]), [9, 8, 7, 6, 5, 4]),
        "certDER": hex(&cert),
        "certFingerprint": hex(&certificate_fingerprint(&cert).unwrap()),
    });
    w("vectors.json", serde_json::to_vec_pretty(&vectors).unwrap());
}
