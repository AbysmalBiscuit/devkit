//! The `supabase` rule source against a fake Data API that speaks the
//! `repo_rules_api` contract in the rules design spec: what it asks for, in
//! which order, and how it reads each answer.

#[path = "common/fakeapi.rs"]
mod fakeapi;

use std::{path::Path, sync::Arc, time::Duration};

use devkit_rules::{
    edit::Fields, model::RuleIndex, remote::RemoteRules, source::RuleSource,
    supabase::SupabaseSource, vocab::Severity,
};
use devkit_supabase::{Api, Auth, fakehttp::FakeServer};
use fakeapi::{conflict, page, payloads, put, stats};
use serde_json::{Value, json};

const REPO: &str = "0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10";
const INDEX: &str = include_str!("fixtures/index.json");

fn index() -> RuleIndex {
    serde_json::from_str(INDEX).unwrap()
}

fn source(server: &FakeServer) -> SupabaseSource {
    let api = Api::new(
        &server.url(),
        "repo_rules_api",
        Auth::None,
        Duration::from_secs(5),
        "rules API",
    )
    .unwrap();
    SupabaseSource::new(Arc::new(api), Some(REPO), Path::new("/srv/acme"))
}

/// The bodies of the calls to `function`, in order.
fn calls(server: &FakeServer, function: &str) -> Vec<Value> {
    server
        .requests()
        .iter()
        .filter(|r| r.path_and_query == format!("/rest/v1/rpc/{function}"))
        .map(|r| r.json())
        .collect()
}

fn ids(index: &RuleIndex) -> Vec<String> {
    index.rules.iter().map(|r| r.id.clone()).collect()
}

fn titled(title: &str) -> Fields {
    Fields {
        title: Some(title.to_string()),
        ..Fields::default()
    }
}

#[test]
fn pull_pages_through_every_rule() {
    let rules = payloads(&index());
    let key = |n: usize| rules[n]["rule_key"].as_str().unwrap().to_string();
    let (k1, k3) = (key(1), key(3));
    let server = FakeServer::start(vec![
        (200, page(4, &rules[0..2], Some((1, &k1)))),
        (200, page(4, &rules[2..4], Some((3, &k3)))),
        (200, page(4, &rules[4..], None)),
    ]);
    let (revision, pulled) = source(&server).with_page_size(2).pull().unwrap();
    assert_eq!(revision, 4);
    assert_eq!(ids(&pulled), ids(&index()));
    assert_eq!(pulled.repo, "/srv/acme");
    let rule = &pulled.rules[0];
    let fixture = &index().rules[0];
    assert_eq!(rule.title, fixture.title);
    assert_eq!(rule.severity_raw, fixture.severity_raw);
    assert_eq!(rule.scope_raw, fixture.scope_raw);
    assert_eq!(rule.tasks, fixture.tasks);
    assert_eq!(rule.languages, fixture.languages);
    assert_eq!(rule.source_file, fixture.source_file);
    let files: Vec<&str> = pulled.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(files, ["AGENTS.md", "crates/foo/AGENTS.md"]);

    let bodies = calls(&server, "query_rules");
    assert_eq!(bodies[0], json!({"p_repo_id": REPO, "p_limit": 2}));
    assert_eq!(
        bodies[1],
        json!({
            "p_repo_id": REPO,
            "p_limit": 2,
            "p_after_position": 1,
            "p_after_rule_key": k1,
            "p_expected_revision": 4,
        })
    );
}

#[test]
fn topics_come_from_the_extra_fields() {
    let mut rules = payloads(&index());
    rules[0]["topics"] = json!(["security"]);
    rules[1].as_object_mut().unwrap().remove("topics");
    let server = FakeServer::start(vec![(200, page(1, &rules, None))]);
    let (_, pulled) = source(&server).pull().unwrap();
    assert_eq!(pulled.rules[0].topics, ["security"]);
    assert!(pulled.rules[1].topics.is_empty());
}

#[test]
fn revision_reads_stats_rules() {
    let server = FakeServer::start(vec![(200, stats(5))]);
    assert_eq!(source(&server).revision().unwrap(), 5);
    assert_eq!(calls(&server, "stats_rules"), [json!({"p_repo_id": REPO})]);
}

#[test]
fn unrecognised_stats_shape_is_refused() {
    let server = FakeServer::start(vec![(200, json!({"ok": true}))]);
    let err = source(&server).revision().unwrap_err();
    assert!(
        format!("{err:#}").contains("unrecognised stats_rules shape"),
        "{err:#}"
    );
}

#[test]
fn unknown_repository_is_not_found() {
    let server = FakeServer::start(vec![(200, json!({"error": "not_found"}))]);
    let err = source(&server).revision().unwrap_err();
    let message = format!("{err:#}");
    assert!(message.contains(REPO), "{message}");
    assert!(message.contains("repository_members"), "{message}");
}

#[test]
fn conflict_mid_pull_restarts() {
    let rules = payloads(&index());
    let k1 = rules[1]["rule_key"].as_str().unwrap().to_string();
    let server = FakeServer::start(vec![
        (200, page(7, &rules[0..2], Some((1, &k1)))),
        (200, conflict(8)),
        (200, page(8, &rules, None)),
    ]);
    let (revision, pulled) = source(&server).with_page_size(2).pull().unwrap();
    assert_eq!(revision, 8);
    assert_eq!(ids(&pulled), ids(&index()));
    let bodies = calls(&server, "query_rules");
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[2], json!({"p_repo_id": REPO, "p_limit": 2}));
}

#[test]
fn three_conflicts_fail_the_pull() {
    let rules = payloads(&index());
    let k1 = rules[1]["rule_key"].as_str().unwrap().to_string();
    let mut answers = Vec::new();
    for revision in 1..=3 {
        answers.push((200, page(revision, &rules[0..2], Some((1, &k1)))));
        answers.push((200, conflict(revision + 1)));
    }
    let server = FakeServer::start(answers);
    let err = source(&server).with_page_size(2).pull().unwrap_err();
    assert!(
        err.to_string()
            .contains("rules changed during the pull 3 times; try again"),
        "{err:#}"
    );
    assert_eq!(calls(&server, "query_rules").len(), 6);
}

#[test]
fn add_sends_null_key_and_eight_fields() {
    let rules = payloads(&index());
    let server = FakeServer::start(vec![
        (200, page(3, &rules, None)),
        (200, put(4, "00000000-0000-4000-8000-0000000000aa")),
    ]);
    let fields = Fields {
        title: Some("Log with tracing".to_string()),
        severity: Some(Severity::Must),
        ..Fields::default()
    };
    let id = source(&server).add("/srv/acme", fields).unwrap();
    let digest = ring::digest::digest(&ring::digest::SHA256, b"<manual>:Log with tracing");
    let hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(id, hex[..12]);
    let body = &calls(&server, "put_rule")[0];
    assert_eq!(body["p_repo_id"], REPO);
    assert_eq!(body["p_rule_key"], Value::Null);
    assert_eq!(body["p_expected_revision"], 3);
    let payload = body["p_payload"].as_object().unwrap();
    let mut keys: Vec<&str> = payload.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, [
        "category",
        "description",
        "directory",
        "languages",
        "scope",
        "severity",
        "tasks",
        "title"
    ]);
    assert_eq!(payload["title"], "Log with tracing");
    assert_eq!(payload["severity"], "must");
}

#[test]
fn add_refuses_an_id_a_live_rule_has() {
    let mut rules = payloads(&index());
    let digest = ring::digest::digest(&ring::digest::SHA256, b"<manual>:Taken");
    let hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    rules[0]["id"] = json!(hex[..12]);
    let server = FakeServer::start(vec![(200, page(3, &rules, None))]);
    let err = source(&server)
        .add("/srv/acme", titled("Taken"))
        .unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err:#}");
    assert!(calls(&server, "put_rule").is_empty());
}

#[test]
fn edit_merges_flags_over_the_pulled_rule() {
    let rules = payloads(&index());
    let target = rules[1].clone();
    let id = target["id"].as_str().unwrap();
    let server = FakeServer::start(vec![
        (200, page(3, &rules, None)),
        (200, put(4, target["rule_key"].as_str().unwrap())),
    ]);
    source(&server).edit(id, titled("Renamed")).unwrap();
    let body = &calls(&server, "put_rule")[0];
    assert_eq!(body["p_rule_key"], target["rule_key"]);
    assert_eq!(body["p_expected_revision"], 3);
    let mut expected = json!({});
    for field in [
        "description",
        "category",
        "scope",
        "severity",
        "directory",
        "tasks",
        "languages",
    ] {
        expected[field] = target[field].clone();
    }
    expected["title"] = json!("Renamed");
    assert_eq!(body["p_payload"], expected);
}

#[test]
fn remove_sends_rule_key_and_revision() {
    let rules = payloads(&index());
    let target = rules[2].clone();
    let server = FakeServer::start(vec![
        (200, page(3, &rules, None)),
        (
            200,
            json!({"revision": 4, "generation": "00000000-0000-4000-8000-000000000001"}),
        ),
    ]);
    source(&server)
        .remove(target["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(calls(&server, "remove_rule"), [json!({
        "p_repo_id": REPO,
        "p_rule_key": target["rule_key"],
        "p_expected_revision": 3,
    })]);
}

#[test]
fn edit_conflict_pulls_and_retries_once() {
    let rules = payloads(&index());
    let target = rules[0].clone();
    let key = target["rule_key"].as_str().unwrap();
    let server = FakeServer::start(vec![
        (200, page(3, &rules, None)),
        (200, conflict(4)),
        (200, page(4, &rules, None)),
        (200, put(5, key)),
    ]);
    source(&server)
        .edit(target["id"].as_str().unwrap(), titled("Renamed"))
        .unwrap();
    let puts = calls(&server, "put_rule");
    assert_eq!(puts.len(), 2);
    assert_eq!(puts[0]["p_expected_revision"], 3);
    assert_eq!(puts[1]["p_expected_revision"], 4);
}

#[test]
fn a_second_conflict_fails_the_edit() {
    let rules = payloads(&index());
    let target = rules[0].clone();
    let server = FakeServer::start(vec![
        (200, page(3, &rules, None)),
        (200, conflict(4)),
        (200, page(4, &rules, None)),
        (200, conflict(5)),
    ]);
    let err = source(&server)
        .edit(target["id"].as_str().unwrap(), titled("Renamed"))
        .unwrap_err();
    assert!(err.to_string().contains("conflict"), "{err:#}");
}

#[test]
fn reader_cannot_edit() {
    let rules = payloads(&index());
    let server = FakeServer::start(vec![
        (200, page(3, &rules, None)),
        (
            403,
            json!({"code": "42501", "message": "repository edit forbidden"}),
        ),
    ]);
    let err = source(&server)
        .edit(rules[0]["id"].as_str().unwrap(), titled("Renamed"))
        .unwrap_err();
    let message = format!("{err:#}");
    assert!(message.contains("cannot edit repository"), "{message}");
    assert!(message.contains(REPO), "{message}");
}

#[test]
fn an_invalid_value_passes_the_server_message_through() {
    let rules = payloads(&index());
    let server = FakeServer::start(vec![
        (200, page(3, &rules, None)),
        (
            400,
            json!({"code": "22023", "message": "full editable payload required"}),
        ),
    ]);
    let err = source(&server)
        .edit(rules[0]["id"].as_str().unwrap(), titled("Renamed"))
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("full editable payload required"),
        "{err:#}"
    );
}

#[test]
fn topics_are_refused() {
    let server = FakeServer::start(Vec::new());
    let fields = Fields {
        topics: Some(vec!["security".to_string()]),
        ..Fields::default()
    };
    let err = source(&server).edit("r-root-must", fields).unwrap_err();
    assert!(
        err.to_string()
            .contains("topics change through repo-rules-agent"),
        "{err:#}"
    );
    let fields = Fields {
        title: Some("T".to_string()),
        topics: Some(vec!["security".to_string()]),
        ..Fields::default()
    };
    source(&server).add("/srv/acme", fields).unwrap_err();
    assert!(server.requests().is_empty());
}

#[test]
fn ambiguous_id_is_refused() {
    let mut rules = payloads(&index());
    rules[1]["id"] = rules[0]["id"].clone();
    let server = FakeServer::start(vec![(200, page(3, &rules, None))]);
    let err = source(&server)
        .edit(rules[0]["id"].as_str().unwrap(), titled("Renamed"))
        .unwrap_err();
    let message = err.to_string();
    for rule in &rules[0..2] {
        let key = rule["rule_key"].as_str().unwrap();
        assert!(message.contains(key), "{message}");
    }
    assert!(calls(&server, "put_rule").is_empty());
}

#[test]
fn an_unknown_id_is_refused() {
    let rules = payloads(&index());
    let server = FakeServer::start(vec![(200, page(3, &rules, None))]);
    let err = source(&server).remove("nope").unwrap_err();
    assert!(err.to_string().contains("no rule nope"), "{err:#}");
}

#[test]
fn location_names_the_api_and_repository() {
    let server = FakeServer::start(Vec::new());
    let source = source(&server);
    assert_eq!(source.kind(), "supabase");
    assert!(source.location().contains(&server.url()));
    assert!(source.location().contains(REPO));
}
