//! The shared connection runner, without a server: what it reports when
//! nothing answers, and how it saves later callers the wait.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use devkit_common::tls::Trust;
use devkit_postgres::{Database, is_unreachable};

const UNREACHABLE: &str = "postgres://u:secret@127.0.0.1:1/db?sslmode=disable";

#[test]
fn unusable_fails_every_run_with_its_reason() {
    let db = Database::unusable("X is not set");
    for _ in 0..2 {
        let err = db.run(async |_| Ok(())).unwrap_err();
        assert!(format!("{err:#}").contains("X is not set"), "{err:#}");
        assert!(!is_unreachable(&err), "{err:#}");
    }
}

#[test]
fn unreachable_server_calls_on_connect_failure_and_names_the_label() {
    let db = Database::new(
        UNREACHABLE,
        Duration::from_secs(2),
        &Trust::default(),
        "rules database",
    )
    .unwrap();
    let failures = Arc::new(AtomicU32::new(0));
    let counter = failures.clone();
    db.on_connect_failure(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let err = db.run(async |_| Ok(())).unwrap_err();
    let message = format!("{err:#}");
    assert!(
        message.contains("rules database 127.0.0.1:1/db"),
        "{message}"
    );
    assert!(!message.contains("secret"), "{message}");
    assert_eq!(failures.load(Ordering::SeqCst), 1);
    assert!(is_unreachable(&err), "{message}");
}

#[test]
fn a_failure_fails_the_next_run_at_once() {
    let db = Database::new(
        UNREACHABLE,
        Duration::from_secs(2),
        &Trust::default(),
        "rules database",
    )
    .unwrap();
    let first = db.run(async |_| Ok(())).unwrap_err();
    let started = Instant::now();
    let second = db.run(async |_| Ok(())).unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(100));
    assert_eq!(format!("{first:#}"), format!("{second:#}"));
    assert!(is_unreachable(&second), "{second:#}");
}

#[test]
fn deadline_in_the_past_gives_up_at_once() {
    let db = Database::new(
        UNREACHABLE,
        Duration::from_secs(30),
        &Trust::default(),
        "todo database",
    )
    .unwrap();
    db.finish_by(Instant::now());
    let started = Instant::now();
    db.run(async |_| Ok(())).unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(100));
}

#[test]
fn a_url_that_does_not_parse_names_the_label() {
    let err = Database::new(
        "postgres://u:secret@[::1",
        Duration::from_secs(1),
        &Trust::default(),
        "rules database",
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "the rules database URL does not parse");
}
