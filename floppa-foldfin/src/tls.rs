//! Atomic TLS identity replacement shared by H2 and H3 listeners.
//!
//! The host obtains certificates (ACME, an internal CA or explicit PEM files),
//! reads a complete pair and calls `replace_pem`. Existing connections survive;
//! a failed replacement leaves the previous identity in service.
use rustls::{
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use std::{
    io,
    sync::{Arc, RwLock},
};

pub struct TlsIdentity(RwLock<Arc<CertifiedKey>>);

impl std::fmt::Debug for TlsIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TlsIdentity([REDACTED])")
    }
}

fn parse(certificate: &[u8], private_key: &[u8]) -> io::Result<Arc<CertifiedKey>> {
    let certificates =
        rustls_pemfile::certs(&mut &certificate[..]).collect::<io::Result<Vec<_>>>()?;
    let key = rustls_pemfile::private_key(&mut &private_key[..])?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing TLS private key"))?;
    let certified =
        CertifiedKey::from_der(certificates, key, &rustls::crypto::ring::default_provider())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    certified
        .keys_match()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(Arc::new(certified))
}

impl TlsIdentity {
    pub fn from_pem(certificate: &[u8], private_key: &[u8]) -> io::Result<Self> {
        Ok(Self(RwLock::new(parse(certificate, private_key)?)))
    }

    /// Validate the entire certificate/key pair before publishing it to listeners.
    /// Does not establish CA trust, hostname or validity dates: issuance/deployment
    /// checks those, and clients continue enforcing all TLS verification.
    pub fn replace_pem(&self, certificate: &[u8], private_key: &[u8]) -> io::Result<()> {
        let replacement = parse(certificate, private_key)?;
        *self.0.write().unwrap() = replacement;
        Ok(())
    }

    pub fn server_config(self: &Arc<Self>) -> Arc<rustls::ServerConfig> {
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("ring supports default TLS versions")
        .with_no_client_auth()
        .with_cert_resolver(self.clone());
        config.alpn_protocols = vec![b"h2".to_vec()];
        config.max_early_data_size = 0;
        Arc::new(config)
    }
}

impl ResolvesServerCert for TlsIdentity {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.read().unwrap().clone())
    }
}
