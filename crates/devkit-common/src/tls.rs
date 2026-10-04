//! What a devkit TLS client trusts: the Mozilla roots bundled into the binary,
//! the machine's own store, and any CA file a config names. Certificate
//! verification is always on; nothing turns it off.

use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, pem::PemObject},
};

/// The certificates a client trusts beyond the default roots. It names no
/// client library, so any of devkit's TLS clients can take it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trust {
    /// A PEM file of CA certificates to trust as well.
    pub ca_file: Option<PathBuf>,
}

impl Trust {
    /// A rustls client config that verifies servers against the default
    /// roots and `ca_file`. A CA file that cannot be read, or holds no
    /// certificate, is an error naming it.
    pub fn client_config(&self) -> Result<ClientConfig> {
        let mut roots = roots();
        if let Some(path) = &self.ca_file {
            for cert in ca_file(path)? {
                roots
                    .add(cert)
                    .with_context(|| format!("trusting a certificate in {}", path.display()))?;
            }
        }
        Ok(config(roots))
    }
}

fn ca_file(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let certs = CertificateDer::pem_file_iter(path)
        .and_then(|certs| certs.collect::<Result<Vec<_>, _>>())
        .with_context(|| format!("reading the CA file {}", path.display()))?;
    anyhow::ensure!(
        !certs.is_empty(),
        "the CA file {} holds no certificate",
        path.display()
    );
    Ok(certs)
}

/// The bundled Mozilla roots plus the machine's store. A store certificate
/// that does not parse is skipped.
pub fn roots() -> RootCertStore {
    let mut roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let native = rustls_native_certs::load_native_certs();
    for e in &native.errors {
        tracing::debug!("loading {}: {e}", store());
    }
    roots.add_parsable_certificates(native.certs);
    roots
}

/// A client config that verifies servers against `roots`.
pub fn config(roots: RootCertStore) -> ClientConfig {
    ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS12, &rustls::version::TLS13])
        .expect("ring supports TLS 1.2 and 1.3")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// The store `rustls_native_certs::load_native_certs` reads, described the
/// way a user would go fix it.
pub fn store() -> String {
    let named: Vec<String> = ["SSL_CERT_FILE", "SSL_CERT_DIR"]
        .into_iter()
        .filter_map(|k| env::var_os(k).map(|v| format!("{k}={}", v.to_string_lossy())))
        .collect();
    if named.is_empty() {
        "the platform certificate store".to_string()
    } else {
        named.join(", ")
    }
}
