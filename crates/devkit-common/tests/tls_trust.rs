use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::PathBuf,
    sync::Arc,
};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tls")
        .join(name)
}

/// An HTTPS server on 127.0.0.1 whose certificate chains to the throwaway
/// `fixtures/tls/ca.pem`, a CA no platform store or bundled list contains.
/// Answers every request with `200 ok`; returns the port.
fn serve() -> u16 {
    let chain = CertificateDer::pem_file_iter(fixture("server.pem"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_file(fixture("server.key")).unwrap();
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for tcp in listener.incoming().flatten() {
            let conn = rustls::ServerConnection::new(config.clone()).unwrap();
            let mut tls = rustls::StreamOwned::new(conn, tcp);
            let mut reader = BufReader::new(&mut tls);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let _ = tls
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });
    port
}

/// One sequential test: `SSL_CERT_FILE` is process-global and
/// `http::agent` reads it once, on first use.
#[test]
fn ssl_cert_file_ca_is_trusted_and_an_untrusted_one_is_named() {
    let ca = fixture("ca.pem");
    unsafe { std::env::set_var("SSL_CERT_FILE", &ca) };
    let url = format!("https://127.0.0.1:{}/", serve());

    let body = devkit_common::http::agent()
        .get(&url)
        .call()
        .map_err(devkit_common::http::explain)
        .expect("a CA named by SSL_CERT_FILE must be trusted")
        .into_string()
        .unwrap();
    assert_eq!(body, "ok");

    // The bundled roots alone reject the same server, as a proxy's CA is
    // rejected by a client that never read the platform store.
    let bundled_only = ureq::agent().get(&url).call().unwrap_err();
    let err = devkit_common::http::explain(bundled_only);
    let msg = format!("{err:#}");
    assert!(msg.contains("not trusted"), "{msg}");
    assert!(
        msg.contains(&format!("SSL_CERT_FILE={}", ca.display())),
        "{msg}"
    );
    assert!(err.is::<devkit_common::http::UntrustedCertificate>());
    assert!(
        matches!(
            err.downcast_ref::<ureq::Error>(),
            Some(ureq::Error::Transport(_))
        ),
        "the ureq error must stay downcastable"
    );
}
