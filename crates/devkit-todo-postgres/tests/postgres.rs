//! The Postgres store against a real database: `DEVKIT_TEST_POSTGRES_URL`
//! names one reached directly, and `DEVKIT_TEST_POOLER_URL` the same database
//! behind a transaction-mode pooler that keeps no prepared statement.
//! `testdb/up.sh` starts both. A test whose variable is unset returns early.

use std::{
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use devkit_todo_postgres::{Database, PostgresStore};

const DIRECT: &str = "DEVKIT_TEST_POSTGRES_URL";
const POOLER: &str = "DEVKIT_TEST_POOLER_URL";

fn url(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|url| !url.trim().is_empty())
}

/// A root no other test shares, so each store starts empty.
fn fresh_root() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "test-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn store(var: &str) -> Option<((), PostgresStore)> {
    let db = Database::new(&url(var)?, Duration::from_secs(10)).unwrap();
    Some(((), PostgresStore::new(db, fresh_root())))
}

mod direct {
    devkit_todo::contract_tests!(
        skip_unless || super::store(super::DIRECT),
        activity = |s: &devkit_todo_postgres::PostgresStore| s.activity()
    );
}

mod pooled {
    devkit_todo::contract_tests!(
        skip_unless || super::store(super::POOLER),
        activity = |s: &devkit_todo_postgres::PostgresStore| s.activity()
    );
}

/// The pooled run proves something only if the pooler keeps no prepared
/// statement between transactions, as Supabase's transaction mode does.
#[test]
fn the_pooler_keeps_no_prepared_statement() {
    let Some(url) = url(POOLER) else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(connection);
        let statement = client.prepare("SELECT 1").await.unwrap();
        let err = client.query(&statement, &[]).await.unwrap_err();
        let err = err.as_db_error().map(|e| e.message().to_string());
        assert!(
            err.as_deref().is_some_and(|e| e.contains("does not exist")),
            "{err:?}"
        );
    });
}
