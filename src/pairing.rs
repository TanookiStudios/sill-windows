//! Pairing, as Sources/StreamProtocol/Pairing.swift and RemoteIdentity.swift define it: the
//! 12-digit code with its Damm check digit, the proofs both sides exchange, the Mac ID, the
//! Bonjour recognition tag and the sill://pair link; and the pairing window's rules
//! (Sources/SillHost/PairingWindow.swift): 5 minutes, 5 wrong proofs, spaced out.
//! tests/conformance.rs checks each against vectors computed by the Swift code.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::{digest, hmac, pbkdf2, rand::SecureRandom};
use std::num::NonZeroU32;

const LABEL: &str = "sill-pair-v1";

const DAMM: [[u8; 10]; 10] = [
    [0, 3, 1, 7, 5, 9, 8, 6, 4, 2], [7, 0, 9, 2, 1, 5, 4, 8, 6, 3], [4, 2, 0, 6, 8, 7, 1, 3, 5, 9],
    [1, 7, 5, 0, 9, 8, 3, 4, 2, 6], [6, 1, 2, 3, 0, 4, 5, 9, 7, 8], [3, 6, 7, 4, 2, 0, 9, 5, 8, 1],
    [5, 8, 6, 9, 7, 2, 0, 1, 3, 4], [8, 9, 4, 5, 3, 6, 2, 0, 1, 7], [9, 4, 3, 8, 6, 1, 7, 2, 0, 5],
    [2, 5, 8, 1, 4, 3, 6, 7, 9, 0],
];

pub fn check_digit(digits: &[u8]) -> u8 {
    digits.iter().fold(0, |i, &d| DAMM[i as usize][d as usize])
}

/// 11 uniformly random digits (rejection sampling), then the check digit.
pub fn generate_code(rng: &dyn SecureRandom) -> String {
    let mut digits = Vec::new();
    while digits.len() < 11 {
        let mut b = [0u8; 16];
        rng.fill(&mut b).expect("system random source");
        digits.extend(b.iter().filter(|&&x| x < 250).map(|x| x % 10));
        digits.truncate(11);
    }
    digits.push(check_digit(&digits));
    digits.iter().map(|d| char::from(b'0' + d)).collect()
}

/// "4829 1355 7208".
pub fn grouped(code: &str) -> String {
    code.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(" ")
}

pub fn b64u(d: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(d)
}

/// Canonical unpadded base64url only, as Base64URL.decode.
pub fn b64u_decode(s: &str) -> Option<Vec<u8>> {
    let d = URL_SAFE_NO_PAD.decode(s).ok()?;
    (b64u(&d) == s).then_some(d)
}

/// The DER prefix of every P-256 SubjectPublicKeyInfo; the 65-byte point follows.
pub const P256_SPKI_PREFIX: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d,
    0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];

pub fn spki_der(point: &[u8]) -> Vec<u8> {
    [&P256_SPKI_PREFIX[..], point].concat()
}

/// SHA-256 of the SPKI: the pin.
pub fn fingerprint(point: &[u8]) -> [u8; 32] {
    digest::digest(&digest::SHA256, &spki_der(point)).as_ref().try_into().unwrap()
}

/// Crockford base32 of the fingerprint's first 10 bytes: 16 characters.
pub fn mac_id(fp: &[u8]) -> String {
    const A: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let (mut out, mut buf, mut bits) = (String::new(), 0u32, 0);
    for &b in &fp[..10] {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(A[((buf >> bits) & 31) as usize] as char);
        }
        buf &= (1 << bits) - 1;
    }
    out
}

/// The typed path's key: PBKDF2-HMAC-SHA256(code, "sill-pair-v1" ‖ fp_mac, 600,000, 32 bytes).
pub fn code_key(code: &str, fp_mac: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let salt = [LABEL.as_bytes(), fp_mac].concat();
    pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA256, NonZeroU32::new(600_000).unwrap(), &salt, code.as_bytes(), &mut out);
    out
}

fn proof_message(who: &str, first: &[u8], second: &[u8]) -> Vec<u8> {
    [format!("{LABEL} {who}").as_bytes(), &[0], first, second].concat()
}

/// proof_D = HMAC(key, "sill-pair-v1 device" ‖ 0 ‖ fp_device ‖ fp_mac), checked in constant time.
pub fn device_proof_valid(key: &[u8], proof: &[u8], fp_device: &[u8], fp_mac: &[u8]) -> bool {
    hmac::verify(&hmac::Key::new(hmac::HMAC_SHA256, key), &proof_message("device", fp_device, fp_mac), proof).is_ok()
}

/// proof_M = HMAC(key, "sill-pair-v1 mac" ‖ 0 ‖ fp_mac ‖ fp_device).
pub fn mac_proof(key: &[u8], fp_mac: &[u8], fp_device: &[u8]) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), &proof_message("mac", fp_mac, fp_device)).as_ref().to_vec()
}

/// The TXT record's `r`: base64url of nonce(6) ‖ HMAC(recognitionKey, "sill-tag-v1" ‖ nonce)[..6].
pub fn recognition_tag(recognition_key: &[u8], nonce: [u8; 6]) -> String {
    let m = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, recognition_key), &[b"sill-tag-v1".as_slice(), &nonce].concat());
    b64u(&[&nonce[..], &m.as_ref()[..6]].concat())
}

/// `sill://pair?v=1&m=…&k=…&s=…&n=…&p=…&a=…`, values percent-encoded down to RFC 3986's
/// unreserved characters and ":". IPv6 addresses go in brackets, as ParsedAddress.text writes them.
pub fn pair_link(fp_mac: &[u8], secret: &[u8], name: &str, port: u16, addresses: &[String]) -> String {
    fn escape(s: &str) -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b':' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect()
    }
    let mut items = vec![
        ("v", "1".to_string()),
        ("m", mac_id(fp_mac)),
        ("k", b64u(fp_mac)),
        ("s", b64u(secret)),
        ("n", name.to_string()),
        ("p", port.to_string()),
    ];
    for a in addresses.iter().take(5) {
        items.push(("a", if a.contains(':') { format!("[{a}]") } else { a.clone() }));
    }
    "sill://pair?".to_string() + &items.iter().map(|(k, v)| format!("{k}={}", escape(v))).collect::<Vec<_>>().join("&")
}

// ---- The pairing window ---------------------------------------------------------------------

pub const LIFETIME: f64 = 300.0;
pub const MAX_FAILURES: u32 = 5;
const SOURCE_SPACING: f64 = 5.0;
const SPACING: [f64; 4] = [1.0, 2.0, 4.0, 8.0];

pub struct Window {
    pub secret: [u8; 16],
    pub code: String,
    /// K from the code: about a tenth of a second of PBKDF2, derived once per window.
    pub code_key: Option<[u8; 32]>,
    pub expires_at: f64,
    failures: u32,
    next_allowed_at: f64,
    last_by_source: std::collections::HashMap<String, f64>,
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// Send proof_M; the window is used up.
    Accept(Vec<u8>),
    Reject { tries_left: i64 },
    Busy { retry_after: f64 },
    /// "expired", "stopped" or "closed".
    Closed(&'static str),
}

impl Window {
    pub fn new(secret: [u8; 16], code: String, now: f64) -> Window {
        Window {
            secret, code, code_key: None, expires_at: now + LIFETIME, failures: 0, next_allowed_at: 0.0,
            last_by_source: Default::default(),
        }
    }

    /// Used, stopped or expired: time for a new code.
    pub fn is_done(&self, now: f64) -> bool {
        now >= self.expires_at || self.failures >= MAX_FAILURES
    }

    /// One proof, judged with the two keys of the TLS session it came on.
    pub fn judge(&mut self, method: &str, proof: &[u8], fp_device: &[u8], fp_mac: &[u8], source: &str, now: f64) -> Verdict {
        if now >= self.expires_at {
            return Verdict::Closed("expired");
        }
        if self.failures >= MAX_FAILURES {
            return Verdict::Closed("closed");
        }
        if now < self.next_allowed_at {
            return Verdict::Busy { retry_after: self.next_allowed_at - now };
        }
        if let Some(&last) = self.last_by_source.get(source) {
            if now - last < SOURCE_SPACING {
                return Verdict::Busy { retry_after: SOURCE_SPACING - (now - last) };
            }
        }
        let key: Vec<u8> = match method {
            "qr" => self.secret.to_vec(),
            "code" => match self.code_key {
                Some(k) => k.to_vec(),
                None => return Verdict::Busy { retry_after: 0.5 },
            },
            _ => return Verdict::Closed("closed"),
        };
        self.last_by_source.insert(source.to_string(), now);
        if device_proof_valid(&key, proof, fp_device, fp_mac) {
            self.failures = MAX_FAILURES; // single use
            return Verdict::Accept(mac_proof(&key, fp_mac, fp_device));
        }
        self.failures += 1;
        if self.failures >= MAX_FAILURES {
            return Verdict::Closed("stopped");
        }
        self.next_allowed_at = now + SPACING[(self.failures - 1) as usize];
        Verdict::Reject { tries_left: (MAX_FAILURES - self.failures) as i64 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_rules() {
        let secret = [7u8; 16];
        let (dev, mac) = ([1u8; 32], [2u8; 32]);
        let good = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &secret), &proof_message("device", &dev, &mac)).as_ref().to_vec();
        let mut w = Window::new(secret, "000000000000".into(), 0.0);
        assert_eq!(w.judge("qr", &[0; 32], &dev, &mac, "a", 1.0), Verdict::Reject { tries_left: 4 });
        assert!(matches!(w.judge("qr", &good, &dev, &mac, "b", 1.5), Verdict::Busy { .. }), "spaced after a wrong proof");
        assert!(matches!(w.judge("qr", &good, &dev, &mac, "a", 3.0), Verdict::Busy { .. }), "same source within 5 s");
        assert!(matches!(w.judge("code", &good, &dev, &mac, "c", 3.0), Verdict::Busy { .. }), "code key not ready");
        assert_eq!(w.judge("qr", &good, &dev, &mac, "b", 3.0), Verdict::Accept(mac_proof(&secret, &mac, &dev)));
        assert_eq!(w.judge("qr", &good, &dev, &mac, "d", 4.0), Verdict::Closed("closed"), "single use");
        let mut w = Window::new(secret, "000000000000".into(), 0.0);
        assert_eq!(w.judge("qr", &good, &dev, &mac, "a", 301.0), Verdict::Closed("expired"));
        let mut w = Window::new(secret, "000000000000".into(), 0.0);
        let mut t = 0.0;
        for left in (1..=4).rev() {
            t += 20.0;
            assert_eq!(w.judge("qr", &[0; 32], &dev, &mac, "a", t), Verdict::Reject { tries_left: left });
        }
        assert_eq!(w.judge("qr", &[0; 32], &dev, &mac, "a", t + 20.0), Verdict::Closed("stopped"));
        assert_eq!(w.judge("qr", &good, &dev, &mac, "a", t + 40.0), Verdict::Closed("closed"), "nothing after a stop");
    }

    #[test]
    fn generated_codes_check() {
        let rng = ring::rand::SystemRandom::new();
        for _ in 0..100 {
            let c = generate_code(&rng);
            assert_eq!(c.len(), 12);
            assert_eq!(check_digit(&c.bytes().map(|b| b - b'0').collect::<Vec<_>>()), 0);
        }
        assert_eq!(grouped("482913557208"), "4829 1355 7208");
    }
}
