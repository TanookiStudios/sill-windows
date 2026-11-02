//! The door's TLS, as RemoteTLS.swift sets it up: TLS 1.3 only, a client certificate required,
//! no tickets and no resumption, and trust decided by the key alone. `sill/1` (a session) admits
//! paired keys only, refused inside the handshake (the device's first read then fails with
//! -9825, which it reads as "this PC removed me"); `sill-pair/1` (pairing) takes any P-256 key,
//! since the pairing proofs bind both keys.

use crate::identity::{certificate_fingerprint, certificate_point, Identity, Trust};
use rustls::client::danger::HandshakeSignatureValid;
use rustls::crypto::{ring as provider, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::server::NoServerSessionStorage;
use rustls::{CertificateError, DigitallySignedStruct, DistinguishedName, Error, ServerConfig, SignatureScheme};
use std::sync::Arc;

pub const SESSION_ALPN: &[u8] = b"sill/1";
pub const PAIRING_ALPN: &[u8] = b"sill-pair/1";

#[derive(Debug)]
struct PinVerifier {
    /// None: any P-256 key (pairing). Some: only the paired ones (a session).
    trust: Option<Trust>,
}

impl ClientCertVerifier for PinVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(&self, leaf: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: UnixTime) -> Result<ClientCertVerified, Error> {
        let fp = certificate_fingerprint(leaf).ok_or(Error::InvalidCertificate(CertificateError::BadEncoding))?;
        match &self.trust {
            // Sent as the certificate_unknown alert, which Apple's TLS reports as -9825, exactly what
            // the Mac's refusal gives (measured over Network.framework: bad_certificate gives -9808,
            // which a device reads as "another key answered", not "this PC removed me").
            Some(t) if !t.contains(&fp) => Err(Error::InvalidCertificate(CertificateError::Other(rustls::OtherError(
                std::sync::Arc::new(std::io::Error::other("this device isn't paired")),
            )))),
            _ => Ok(ClientCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::PeerIncompatible(rustls::PeerIncompatible::Tls12NotOffered))
    }

    /// The device proves it holds the key, checked against the key alone (as Apple's side does:
    /// no part of the certificate but its key is ever evaluated).
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        let point = certificate_point(c).ok_or(Error::InvalidCertificate(CertificateError::BadEncoding))?;
        if d.scheme != SignatureScheme::ECDSA_NISTP256_SHA256 {
            return Err(Error::InvalidCertificate(CertificateError::BadSignature));
        }
        ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_ASN1, point)
            .verify(m, d.signature())
            .map(|_| HandshakeSignatureValid::assertion())
            .map_err(|_| Error::InvalidCertificate(CertificateError::BadSignature))
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ECDSA_NISTP256_SHA256]
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }
}

impl std::fmt::Debug for Trust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Trust({} devices)", self.count())
    }
}

/// One server config per application protocol; the door picks by what the ClientHello offers.
pub fn server_config(identity: &Identity, alpn: &[u8], trust: Option<Trust>) -> Result<Arc<ServerConfig>, String> {
    let p = Arc::new(provider::default_provider());
    let verifier = Arc::new(PinVerifier { trust });
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.pkcs8.clone()));
    let mut config = ServerConfig::builder_with_provider(p as Arc<CryptoProvider>)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![CertificateDer::from(identity.certificate.clone())], key)
        .map_err(|e| e.to_string())?;
    config.alpn_protocols = vec![alpn.to_vec()];
    config.send_tls13_tickets = 0;
    config.session_storage = Arc::new(NoServerSessionStorage {});
    Ok(Arc::new(config))
}
