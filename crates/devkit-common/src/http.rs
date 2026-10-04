//! The one HTTP client every devkit network call goes through.
//!
//! TLS trusts the Mozilla roots bundled into the binary plus the machine's own
//! store, which is where an intercepting proxy's CA, a corporate CA, or a
//! GitHub Enterprise instance's internal CA gets installed. `SSL_CERT_FILE`
//! and `SSL_CERT_DIR`, when set, name that store in place of the platform
//! one. Certificate verification is always on; nothing turns it off.
//! `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY` are read once, when
//! the client is built.

use std::{env, sync::OnceLock, time::Duration};

pub use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder, Response};

/// One pooled client for the whole process, so repeated calls reuse the
/// TCP/TLS connection, and the thread a blocking client runs on starts once.
pub fn client() -> &'static Client {
    static C: OnceLock<Client> = OnceLock::new();
    C.get_or_init(|| {
        Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .use_preconfigured_tls(tls_config())
            .build()
            .expect("a client with a preconfigured rustls config builds")
    })
}

/// Sends `req`. A non-2xx status is an error, as a transport failure is, and
/// both go through [`explain`]; [`status`] reads the status back.
pub fn send(req: RequestBuilder) -> anyhow::Result<Response> {
    req.send()
        .and_then(Response::error_for_status)
        .map_err(explain)
}

/// The HTTP status a [`send`] error carries, `None` when no response came.
pub fn status(e: &anyhow::Error) -> Option<StatusCode> {
    e.downcast_ref::<reqwest::Error>()?.status()
}

/// Whether `e` is a request that never got a response: the host did not
/// resolve, refused, hung up before answering, or timed out.
pub fn is_unreachable(e: &anyhow::Error) -> bool {
    e.downcast_ref::<reqwest::Error>()
        .is_some_and(|e| e.is_request() || e.is_timeout())
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
/// `reqwest::Error` stays reachable through `downcast_ref`.
pub fn explain(e: reqwest::Error) -> anyhow::Error {
    let Some(cert) = certificate_error(&e) else {
        return e.into();
    };
    let context = UntrustedCertificate {
        reason: cert.to_string(),
        store: trust_store(),
    };
    anyhow::Error::new(e).context(context)
}

/// rustls reports a rejected certificate inside nested `io::Error`s, whose
/// `source()` skips the wrapped error, so each one is unwrapped by hand.
fn certificate_error(e: &reqwest::Error) -> Option<&rustls::CertificateError> {
    let mut link = std::error::Error::source(e);
    while let Some(err) = link {
        if let Some(rustls::Error::InvalidCertificate(cert)) = err.downcast_ref() {
            return Some(cert);
        }
        link = match err.downcast_ref::<std::io::Error>() {
            Some(io) => io.get_ref().map(|inner| inner as _),
            None => err.source(),
        };
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

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
    };

    use super::*;

    /// A loopback server that reads each request and hangs up without
    /// answering; returns its URL.
    fn hang_up() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for tcp in listener.incoming().flatten() {
                let mut reader = BufReader::new(&tcp);
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
            }
        });
        format!("http://127.0.0.1:{port}/")
    }

    #[test]
    fn a_connection_closed_before_any_answer_is_unreachable() {
        let err = send(client().get(hang_up())).unwrap_err();
        assert!(is_unreachable(&err), "{err:#}");
    }
}
