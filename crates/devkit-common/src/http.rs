//! The one HTTP client every devkit network call goes through.
//!
//! TLS trusts the Mozilla roots bundled into the binary plus the machine's own
//! store, which is where an intercepting proxy's CA, a corporate CA, or a
//! GitHub Enterprise instance's internal CA gets installed. `SSL_CERT_FILE`
//! and `SSL_CERT_DIR`, when set, name that store in place of the platform
//! one. Certificate verification is always on; nothing turns it off.

use std::{
    env,
    sync::{Arc, OnceLock},
    time::Duration,
};

/// One pooled agent for the whole process so repeated calls reuse the TCP/TLS
/// connection instead of dialing afresh each time.
pub fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(30))
            .tls_config(Arc::new(tls_config()))
            .build()
    })
}

fn tls_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let native = rustls_native_certs::load_native_certs();
    for e in &native.errors {
        tracing::debug!("loading {}: {e}", trust_store());
    }
    roots.add_parsable_certificates(native.certs);
    rustls::ClientConfig::builder_with_provider(rustls::crypto::ring::default_provider().into())
        .with_protocol_versions(&[&rustls::version::TLS12, &rustls::version::TLS13])
        .expect("ring supports TLS 1.2 and 1.3")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// A server certificate that no trusted root vouches for, typically an
/// intercepting proxy whose CA is missing from the store devkit read.
#[derive(Debug)]
pub struct UntrustedCertificate {
    reason: String,
    store: String,
}

impl std::fmt::Display for UntrustedCertificate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TLS certificate not trusted ({}); devkit checked the bundled Mozilla roots and {}",
            self.reason, self.store
        )
    }
}

/// Wrap a request error for `?`. A rejected server certificate gains an
/// [`UntrustedCertificate`] context naming the trust store devkit checked, so
/// it reads as a trust problem rather than a bad token or a down host. The
/// `ureq::Error` stays reachable through `downcast_ref`.
pub fn explain(e: ureq::Error) -> anyhow::Error {
    let Some(cert) = certificate_error(&e) else {
        return e.into();
    };
    let context = UntrustedCertificate {
        reason: cert.to_string(),
        store: trust_store(),
    };
    anyhow::Error::new(e).context(context)
}

/// rustls reports a rejected certificate inside an `io::Error`, whose
/// `source()` skips the wrapped error, so each link is unwrapped by hand.
fn certificate_error(e: &ureq::Error) -> Option<&rustls::CertificateError> {
    let mut link = std::error::Error::source(e);
    while let Some(err) = link {
        let tls = err.downcast_ref::<rustls::Error>().or_else(|| {
            err.downcast_ref::<std::io::Error>()
                .and_then(|io| io.get_ref())
                .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        });
        if let Some(rustls::Error::InvalidCertificate(cert)) = tls {
            return Some(cert);
        }
        link = err.source();
    }
    None
}

/// The store `rustls_native_certs::load_native_certs` reads, described the
/// way a user would go fix it.
fn trust_store() -> String {
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
