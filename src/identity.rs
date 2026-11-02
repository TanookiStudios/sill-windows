//! This PC's identity (one P-256 key, kept for good: it is what paired devices pin), the
//! certificate made from it at every launch (RemoteIdentity.swift's SelfSignedCertificate, byte
//! for byte in shape), the recognition key behind the Bonjour tag, and the paired devices.
//!
//! Everything lives in one folder: `%LOCALAPPDATA%\Sill` on Windows, or the folder `SILL_HOME`
//! names (tests point it at a throwaway folder, so no test can touch a real identity). On Windows
//! the private key and recognition key are sealed with DPAPI, so only this Windows user can read them.

use crate::pairing::{b64u, b64u_decode, fingerprint};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("SILL_HOME") {
        return PathBuf::from(h);
    }
    #[cfg(windows)]
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(l).join("Sill");
    }
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".sill-windows")
}

#[derive(Serialize, Deserialize)]
struct Stored {
    pkcs8: String,
    #[serde(rename = "recognitionKey")]
    recognition_key: String,
}

pub struct Identity {
    pub pkcs8: Vec<u8>,
    pub fingerprint: [u8; 32],
    pub recognition_key: [u8; 32],
    pub certificate: Vec<u8>,
}

impl Identity {
    /// The stored identity, or a new one saved for next time. An identity file that exists but
    /// cannot be read is an error, never silently replaced: every paired device pins this key.
    pub fn load_or_create() -> Result<Identity, String> {
        let rng = SystemRandom::new();
        let path = home().join("identity.bin");
        let stored: Stored = if path.exists() {
            let sealed = std::fs::read(&path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
            let plain = unseal(&sealed).ok_or_else(|| format!("Couldn't unlock {} (was it copied from another PC or user?)", path.display()))?;
            serde_json::from_slice(&plain).map_err(|e| format!("{} is damaged: {e}", path.display()))?
        } else {
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).map_err(|_| "Couldn't make a key")?;
            let mut rk = [0u8; 32];
            rng.fill(&mut rk).map_err(|_| "No random source")?;
            let s = Stored { pkcs8: b64u(pkcs8.as_ref()), recognition_key: b64u(&rk) };
            std::fs::create_dir_all(home()).map_err(|e| format!("Couldn't create {}: {e}", home().display()))?;
            let sealed = seal(&serde_json::to_vec(&s).unwrap()).ok_or("Couldn't seal the new identity")?;
            std::fs::write(&path, sealed).map_err(|e| format!("Couldn't save {}: {e}", path.display()))?;
            s
        };
        let pkcs8 = b64u_decode(&stored.pkcs8).ok_or("Identity key is damaged")?;
        let recognition_key: [u8; 32] =
            b64u_decode(&stored.recognition_key).and_then(|v| v.try_into().ok()).ok_or("Recognition key is damaged")?;
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8, &rng).map_err(|_| "Identity key is damaged")?;
        let point = pair.public_key().as_ref().to_vec();
        let certificate = self_signed_certificate(&pair, &rng)?;
        Ok(Identity { fingerprint: fingerprint(&point), pkcs8, recognition_key, certificate })
    }
}

/// Just enough DER for one certificate.
fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let n = content.len();
    let mut d = vec![tag];
    if n < 0x80 {
        d.push(n as u8);
    } else {
        let bytes: Vec<u8> = n.to_be_bytes().into_iter().skip_while(|&b| b == 0).collect();
        d.push(0x80 | bytes.len() as u8);
        d.extend(bytes);
    }
    d.extend_from_slice(content);
    d
}
fn seq(parts: &[&[u8]]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

/// Version 3, no extensions, a random positive serial, issuer = subject = a CN of 16 random hex
/// characters (it names nothing), valid 2026-01-01 to 9999-12-31, signed ECDSA-SHA256. Only the
/// key matters to a pin; the device never evaluates names, dates or chains.
pub fn self_signed_certificate(pair: &EcdsaKeyPair, rng: &SystemRandom) -> Result<Vec<u8>, String> {
    let (mut cn, mut serial) = ([0u8; 8], [0u8; 16]);
    rng.fill(&mut cn).map_err(|_| "No random source")?;
    rng.fill(&mut serial).map_err(|_| "No random source")?;
    serial[0] = (serial[0] & 0x7f) | 0x01;
    let hex: String = cn.iter().map(|b| format!("{b:02x}")).collect();
    let oid_cn = tlv(0x06, &[0x55, 0x04, 0x03]);
    let name = seq(&[&tlv(0x31, &seq(&[&oid_cn, &tlv(0x0c, hex.as_bytes())]))]);
    let algorithm = seq(&[&tlv(0x06, &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02])]);
    let validity = seq(&[&tlv(0x17, b"260101000000Z"), &tlv(0x18, b"99991231235959Z")]);
    let spki = crate::pairing::spki_der(pair.public_key().as_ref());
    let tbs = seq(&[&tlv(0xa0, &tlv(0x02, &[2])), &tlv(0x02, &serial), &algorithm, &name, &validity, &name, &spki]);
    let sig = pair.sign(rng, &tbs).map_err(|_| "Couldn't sign the certificate")?;
    Ok(seq(&[&tbs, &algorithm, &tlv(0x03, &[&[0u8][..], sig.as_ref()].concat())]))
}

/// The SPKI fingerprint of a certificate's key when it is P-256, else None.
pub fn certificate_fingerprint(der: &[u8]) -> Option<[u8; 32]> {
    certificate_point(der).map(|p| fingerprint(&p))
}

/// A certificate's P-256 public key (65 bytes, 04 ‖ X ‖ Y), else None. Walks the DER only as far
/// as the key: Certificate ▸ tbsCertificate ▸ its 6th or 7th element (after the optional [0]
/// version), each length checked against what is left. Nothing else in it matters to a pin.
pub fn certificate_point(der: &[u8]) -> Option<Vec<u8>> {
    fn element(d: &[u8]) -> Option<(u8, &[u8], &[u8])> {
        let (&tag, rest) = d.split_first()?;
        let (&first, rest) = rest.split_first()?;
        let (len, rest) = if first < 0x80 {
            (first as usize, rest)
        } else {
            let n = (first & 0x7f) as usize;
            if n == 0 || n > 4 || rest.len() < n {
                return None;
            }
            (rest[..n].iter().fold(0usize, |a, &b| (a << 8) | b as usize), &rest[n..])
        };
        if rest.len() < len {
            return None;
        }
        Some((tag, &rest[..len], &rest[len..]))
    }
    let (tag, cert, _) = element(der)?;
    if tag != 0x30 {
        return None;
    }
    let (tag, mut tbs, _) = element(cert)?;
    if tag != 0x30 {
        return None;
    }
    let mut index = 0;
    loop {
        let start = tbs;
        let (tag, _, rest) = element(tbs)?;
        if index == 0 && tag == 0xa0 {
            tbs = rest;
            continue;
        }
        if index == 5 {
            let spki = &start[..start.len() - rest.len()];
            let point = spki.strip_prefix(&crate::pairing::P256_SPKI_PREFIX[..])?;
            return (point.len() == 65 && point[0] == 4).then(|| point.to_vec());
        }
        index += 1;
        tbs = rest;
    }
}

// ---- Paired devices ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub fingerprint: String,
    pub name: String,
    pub model: Option<String>,
    #[serde(rename = "pairedAt")]
    pub paired_at: f64,
}

#[derive(Clone)]
pub struct Trust(Arc<RwLock<Vec<Device>>>);

impl Trust {
    fn path() -> PathBuf {
        home().join("devices.json")
    }

    /// A damaged list is an error, never an empty one: emptying it would unpair every device.
    pub fn load() -> Result<Trust, String> {
        let path = Self::path();
        let list = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{} is damaged: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("Couldn't read {}: {e}", path.display())),
        };
        Ok(Trust(Arc::new(RwLock::new(list))))
    }

    pub fn contains(&self, fp: &[u8]) -> bool {
        let fp = b64u(fp);
        self.0.read().unwrap().iter().any(|d| d.fingerprint == fp)
    }

    pub fn name(&self, fp: &[u8]) -> Option<String> {
        let fp = b64u(fp);
        self.0.read().unwrap().iter().find(|d| d.fingerprint == fp).map(|d| d.name.clone())
    }

    pub fn count(&self) -> usize {
        self.0.read().unwrap().len()
    }

    /// Adds (or renames) a device and saves the list at once.
    pub fn add(&self, fp: &[u8], name: String, model: Option<String>) -> Result<(), String> {
        let mut list = self.0.write().unwrap();
        let fp = b64u(fp);
        list.retain(|d| d.fingerprint != fp);
        list.push(Device { fingerprint: fp, name, model, paired_at: crate::protocol::now() });
        let path = Self::path();
        std::fs::create_dir_all(home()).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&*list).unwrap()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    /// Forgets every paired device.
    pub fn clear(&self) -> Result<(), String> {
        self.0.write().unwrap().clear();
        match std::fs::remove_file(Self::path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }
}

// ---- Sealing ----------------------------------------------------------------------------------

#[cfg(windows)]
fn seal(plain: &[u8]) -> Option<Vec<u8>> {
    dpapi(plain, true)
}
#[cfg(windows)]
fn unseal(sealed: &[u8]) -> Option<Vec<u8>> {
    dpapi(sealed, false)
}

#[cfg(windows)]
fn dpapi(data: &[u8], protect: bool) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN};
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        let r = if protect {
            CryptProtectData(&input, windows::core::w!("Sill identity"), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
        } else {
            CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
        };
        r.ok()?;
        let v = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(output.pbData as _));
        Some(v)
    }
}

// shortcut: outside Windows (development and tests only) the identity is stored unsealed, in a
// folder only this user can read; the shipped build is Windows-only and always seals.
#[cfg(not(windows))]
fn seal(plain: &[u8]) -> Option<Vec<u8>> {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(home(), std::fs::Permissions::from_mode(0o700));
    Some(plain.to_vec())
}
#[cfg(not(windows))]
fn unseal(sealed: &[u8]) -> Option<Vec<u8>> {
    Some(sealed.to_vec())
}
