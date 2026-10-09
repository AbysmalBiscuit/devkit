//! The Data API client against a scripted loopback server: what each request
//! carries, and how refusals and silence read.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use devkit_supabase::{Api, Auth, Refused, fakehttp::FakeServer, is_unreachable};
use serde_json::json;

fn api(server: &FakeServer, auth: Auth, label: &'static str) -> Api {
    Api::new(
        &server.url(),
        "repo_rules_api",
        auth,
        Duration::from_secs(5),
        label,
    )
    .unwrap()
}

#[test]
fn key_goes_as_apikey_and_bearer() {
    let server = FakeServer::start(vec![(200, json!({}))]);
    api(&server, Auth::Key("k".into()), "rules API")
        .call("f", &json!({}))
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path_and_query, "/rest/v1/rpc/f");
    assert_eq!(request.header("apikey"), Some("k"));
    assert_eq!(request.header("authorization"), Some("Bearer k"));
    assert_eq!(request.header("content-profile"), Some("repo_rules_api"));
}

#[test]
fn no_auth_sends_no_credentials() {
    let server = FakeServer::start(vec![(200, json!({}))]);
    api(&server, Auth::None, "rules API")
        .call("f", &json!({}))
        .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.header("apikey"), None);
    assert_eq!(request.header("authorization"), None);
}

#[test]
fn select_reads_the_schema_and_total() {
    let server = FakeServer::start(vec![(200, json!([{"id": 1}]))]);
    let rows: Vec<serde_json::Value> = api(&server, Auth::None, "todo API")
        .select("todos", &[("select", "id".into())])
        .unwrap();
    assert_eq!(rows, vec![json!({"id": 1})]);
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.path_and_query, "/rest/v1/todos?select=id");
    assert_eq!(request.header("accept-profile"), Some("repo_rules_api"));
}

#[test]
fn refusal_carries_code_message_details() {
    let server = FakeServer::start(vec![(
        400,
        json!({"code": "42501", "message": "m", "details": "d"}),
    )]);
    let err = api(&server, Auth::None, "todo API")
        .call("f", &json!({}))
        .unwrap_err();
    let refused = err.downcast_ref::<Refused>().expect("a Refused");
    assert_eq!(refused.status.as_u16(), 400);
    assert_eq!(refused.code.as_deref(), Some("42501"));
    assert_eq!(refused.message, "m");
    assert_eq!(refused.details.as_deref(), Some("d"));
    assert!(err.to_string().starts_with("todo API "), "{err}");
    assert!(!is_unreachable(&err));
}

#[test]
fn a_401_runs_on_rejected() {
    let server = FakeServer::start(vec![(401, json!({}))]);
    let api = api(&server, Auth::Key("k".into()), "rules API");
    let rejected = Arc::new(AtomicU32::new(0));
    let counter = rejected.clone();
    api.on_rejected(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    api.call("f", &json!({})).unwrap_err();
    assert_eq!(rejected.load(Ordering::SeqCst), 1);
}

#[test]
fn one_unanswered_request_fails_the_rest_at_once() {
    let api = Api::new(
        "http://127.0.0.1:1",
        "devkit",
        Auth::None,
        Duration::from_secs(5),
        "todo API",
    )
    .unwrap();
    let first = api.call("f", &json!({})).unwrap_err();
    assert!(is_unreachable(&first), "{first:#}");
    let started = Instant::now();
    let second = api.call("f", &json!({})).unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(is_unreachable(&second), "{second:#}");
}

#[test]
fn a_url_that_does_not_parse_names_the_label() {
    let err = Api::new(
        "not a url",
        "devkit",
        Auth::None,
        Duration::from_secs(1),
        "rules API",
    )
    .unwrap_err();
    assert!(err.to_string().contains("rules API URL"), "{err}");
}

#[test]
fn identity_names_the_project_without_credentials() {
    let api = Api::new(
        "https://abc.supabase.co/",
        "repo_rules_api",
        Auth::None,
        Duration::from_secs(5),
        "rules API",
    )
    .unwrap();
    assert_eq!(api.identity(), "https://abc.supabase.co");
}

#[test]
fn a_url_with_credentials_is_refused_without_repeating_it() {
    for url in [
        "https://user:secret@abc.supabase.co",
        "https://secret@abc.supabase.co",
        "ftp://user:secret@abc.supabase.co",
        "https://abc.supabase.co/?apikey=secret",
        "https://abc.supabase.co?apikey=secret",
        "https://abc.supabase.co/#secret",
    ] {
        let err = Api::new(
            url,
            "repo_rules_api",
            Auth::None,
            Duration::from_secs(5),
            "rules API",
        )
        .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("rules API URL"), "{message}");
        assert!(!message.contains("secret"), "{message}");
    }
}
