//! The Postgres connection devkit's database-backed stores share: how a URL
//! becomes a connection config, how a database is named without its
//! credentials, how a client connects, over TLS unless the URL says
//! otherwise, and the [`Database`] runner that connects on first use and
//! bounds each operation by a wait. What a store does with the connection
//! and how it reads its own refusals stay with the store.

mod database;

use anyhow::{Result, anyhow};
pub use database::{Database, is_unreachable};
use devkit_common::tls::Trust;
use tokio_postgres::{
    Client, Config, NoTls,
    config::{Host, SslMode},
};
use tokio_postgres_rustls::MakeRustlsConnect;

/// The connection config `url` names, a `postgres://` URL or libpq's
/// `key=value` form. TLS is required unless the URL says `sslmode=disable`.
pub fn config(url: &str) -> Result<Config, tokio_postgres::Error> {
    let mut config = url.parse::<Config>()?;
    // tokio-postgres's default, `prefer`, falls back to plaintext when the
    // server, or anyone between, declines TLS.
    if config.get_ssl_mode() == SslMode::Prefer {
        config.ssl_mode(SslMode::Require);
    }
    Ok(config)
}

/// `host:port/dbname`, without the user or password.
pub fn target(config: &Config) -> String {
    let host = match config.get_hosts().first() {
        Some(Host::Tcp(host)) => host.clone(),
        #[cfg(unix)]
        Some(Host::Unix(path)) => path.display().to_string(),
        None => "localhost".to_string(),
    };
    let port = config.get_ports().first().copied().unwrap_or(5432);
    let db = config
        .get_dbname()
        .or(config.get_user())
        .unwrap_or("postgres");
    format!("{host}:{port}/{db}")
}

/// The TLS setup that verifies a server against the default roots and
/// `trust`, built on a blocking thread so a caller's timeout covers reading
/// the CA file and the machine's roots.
pub async fn tls(trust: &Trust) -> Result<MakeRustlsConnect> {
    let trust = trust.clone();
    let config = tokio::task::spawn_blocking(move || trust.client_config())
        .await
        .map_err(|e| anyhow!("{e}"))??;
    Ok(MakeRustlsConnect::new(config))
}

/// A client connected to `config`'s database over `tls`, or in plaintext
/// when `tls` is `None`, its connection driven on the current runtime.
pub async fn connect(
    config: &Config,
    tls: Option<MakeRustlsConnect>,
) -> Result<Client, tokio_postgres::Error> {
    match tls {
        Some(tls) => {
            let (client, connection) = config.connect(tls).await?;
            tokio::spawn(connection);
            Ok(client)
        }
        None => {
            let (client, connection) = config.connect(NoTls).await?;
            tokio::spawn(connection);
            Ok(client)
        }
    }
}
