# Issue Status Events Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** devkit moves an issue's tracker status (a GitHub Projects v2 single-select field, or a Linear workflow state) on `issue setup`, on the first agent session in the issue's worktree, and on `issue pr create`, as `[issue.events]` configures.

**Architecture:** Config types in `devkit-config`; a locked read-modify-write and two new fields on the worktree's `IssueRecord`; a `StatusWriter` trait with GitHub and Linear implementations in `devkit-common::tracker::status`, separate from the read-only `Tracker`; one CLI verb, `devkit issue event`, that every trigger ends in; inline calls from `issue setup` and `issue pr create`; and a SessionStart hook that claims `start` locally and spawns the verb detached.

**Tech Stack:** Rust 2024 workspace, serde/schemars config, `fd-lock`, `ambassador`, GitHub GraphQL through `devkit_common::github::Api`, Linear GraphQL through `tracker::linear`.

**Spec:** `docs/superpowers/specs/2026-10-04-issue-status-events-design.md`

## Global Constraints

- Gates before every commit: `cargo nextest run --workspace --no-fail-fast`, `cargo test --workspace --doc`, `cargo clippy --workspace --all-targets -- -D warnings`, `devrun task fmt`.
- Commit with `devrun task commit --arg files=<comma-separated paths> --arg commit_subject='<type(scope): subject>' --arg coauthors='<model> <noreply@anthropic.com>'`. Conventional Commits.
- No `hook` verb exits 2; only `pre-tool-use` writes stdout; the SessionStart hook does no network IO and never changes a verdict.
- GitHub status calls go through `github::Api` (`Api::new(&repo.host)`), with the `gh api graphql --hostname <host>` fallback `GithubTracker::details` takes when `api.token()` is `None`. No `cmd::gh_json_in`.
- The `Tracker` trait stays read-only (`crates/devkit-common/src/tracker/mod.rs:123`).
- A key's meaning lives in its doc comment (it becomes the schema). Regenerate `schema/devkit-config.json` with `DEVKIT_UPDATE_SCHEMA=1 cargo test`. `docs/` and skill references restate no key.
- Help text stays ASCII.
- Events are empty by default: with no `[issue.events]`, nothing reads or writes a tracker and nothing spawns.
- Status names compare case-insensitively after trimming.
- Tests that spawn processes poll for the expected state instead of sleeping. Test scratch comes from `tempfile`.

## Review Focus

1. An issue in several Projects v2 projects: the writer reads and writes only the item in the configured project. Test in Task 4.
2. A corrupt or unreadable `.devkit/issue.toml` in the session's worktree: the hook claims nothing, spawns nothing and stays silent. Test in Task 8.
3. Status names that differ only in case or surrounding whitespace ("In progress" vs "In Progress "): they match, in `from`, in the already-at-target check, and in option lookup. Tests in Tasks 3 and 4.
4. `[github] project` set while the resolved tracker is Linear: the Linear writer is used and the GitHub keys are ignored. Test in Task 6.
5. A Linear id typed lowercase (`eng-123`): it resolves like `ENG-123`. Test in Task 5.

---

### Task 1: Config types

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (`Config` at :21-90; `GithubConfig` at :660-690)
- Modify: `schema/devkit-config.json` (regenerated)
- Test: unit tests in `crates/devkit-config/src/lib.rs`; doctest on `IssueConfig`

**Interfaces:**
- Produces:
  - `pub enum IssueEvent { Setup, Start, PrOpen }`: serde `snake_case` (`setup`, `start`, `pr_open`), `Copy`, `Eq`, `Hash`, `JsonSchema`, `Display` printing the same spelling.
  - `pub struct IssueConfig { pub events: IssueEventsConfig }`: `deny_unknown_fields`, `Default`.
  - `pub struct IssueEventsConfig { pub setup: Option<EventTransition>, pub start: Option<EventTransition>, pub pr_open: Option<EventTransition> }`: `deny_unknown_fields`, `Default`, with `pub fn get(&self, e: IssueEvent) -> Option<&EventTransition>` and `pub fn any(&self) -> bool`.
  - `pub struct EventTransition { pub from: Vec<String>, pub to: String }`: `from` defaults to `vec!["*".into()]`; `deny_unknown_fields`.
  - `pub struct ProjectRef { pub owner: Option<String>, pub number: u64 }`: deserializes from an integer `3` or a string `"owner/7"`; serializes back to the same form.
  - `Config.issue: IssueConfig` (`#[serde(default)]`); `GithubConfig.project: Option<ProjectRef>`, `GithubConfig.status_field: Option<String>`, and `pub fn status_field(&self) -> &str` returning `"Status"` when unset.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn issue_events_parse_and_default_from_to_any() {
    let cfg = Config::parse("[issue.events.start]\nto = \"In progress\"\n").unwrap();
    let t = cfg.issue.events.get(IssueEvent::Start).unwrap();
    assert_eq!((t.from.clone(), t.to.as_str()), (vec!["*".to_string()], "In progress"));
    assert!(cfg.issue.events.get(IssueEvent::Setup).is_none());
    assert!(!Config::parse("").unwrap().issue.events.any());
}

#[test]
fn an_unknown_event_name_fails_to_parse() {
    assert!(Config::parse("[issue.events.started]\nto = \"x\"\n").is_err());
}

#[test]
fn github_project_takes_a_number_or_owner_slash_number() {
    let n = Config::parse("[github]\nproject = 3\n").unwrap();
    assert_eq!(n.github.project, Some(ProjectRef { owner: None, number: 3 }));
    let o = Config::parse("[github]\nproject = \"some-org/7\"\n").unwrap();
    assert_eq!(o.github.project, Some(ProjectRef { owner: Some("some-org".into()), number: 7 }));
    assert!(Config::parse("[github]\nproject = \"7\"\n").is_err());
    assert_eq!(n.github.status_field(), "Status");
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run -p devkit-config issue_events github_project unknown_event`
Expected: FAIL to compile (`IssueEvent`, `issue`, `ProjectRef` not found).

- [ ] **Step 3: Add the types and fields**

Doc comments carry the spec's wording: `from` matches `*`, the empty string for no status, case-insensitively; an issue already at `to` is left alone; `start`'s `from` should list the start states, since `["*"]` lets a second worktree pull an issue back from review; event mappings belong in the repository's `devkit.toml`. The doc comment on `project` says a bare number is owned by `issues_repo`'s owner. `GithubConfig`'s struct doc widens to cover the project keys. Put the spec's config example in a doctest on `IssueConfig`. Update the `GithubConfig { .. }` literals in `crates/devkit-common/src/tracker/mod.rs` tests.

- [ ] **Step 4: Regenerate the schema and run the tests**

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test -p devkit-config && cargo nextest run -p devkit-config && cargo test -p devkit-config --doc`
Expected: PASS; `schema/devkit-config.json` changed.

- [ ] **Step 5: Commit** `feat(config): add issue status events` with `crates/devkit-config/src/lib.rs,schema/devkit-config.json,crates/devkit-common/src/tracker/mod.rs`.

### Task 2: Issue record: events, origin, locked update

**Files:**
- Modify: `crates/devkit-common/src/record.rs`
- Modify (move to `update`): `src/bin/devkit/issue/pr/create.rs:238-241`, `src/bin/devkit/issue/pr/ready.rs:50-80`, `src/bin/devkit/issue/review/request.rs:141-206`, `src/bin/devkit/baseline/mod.rs:1265-1288` (`write_pin`)
- Modify (struct literals): every `IssueRecord {` site (`rg -n "IssueRecord \{"`)
- Test: unit tests in `crates/devkit-common/src/record.rs`

**Interfaces:**
- Consumes: `devkit_config::IssueEvent` (Task 1).
- Produces:
  - `IssueRecord.events: Option<Vec<IssueEvent>>`: `None` means a record written before the field existed.
  - `IssueRecord.origin: Option<RecordOrigin>`, with `pub enum RecordOrigin { Setup, Checkout }` (serde `snake_case`). Both fields are `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - `#[derive(Default)]` on `IssueRecord`, so literals end in `..Default::default()`.
  - `pub fn update<T>(worktree: &Path, f: impl FnOnce(&mut Option<IssueRecord>) -> T) -> Result<T>` reads under an exclusive `fd_lock::RwLock` on `<worktree>/.devkit/issue.lock`, runs `f`, and writes through `write` when the record is `Some` afterwards.
  - `pub fn claim(worktree: &Path, event: IssueEvent) -> Result<bool>` uses `update` to add `event` to `events` (turning `None` into `Some`) and returns `true` only when this call added it. An absent record returns `Ok(false)`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_record_without_events_reads_as_legacy() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
    std::fs::write(path(dir.path()), "issue = \"65\"\nslug = \"x\"\napps = []\n").unwrap();
    let rec = read(dir.path()).unwrap();
    assert_eq!((rec.events, rec.origin), (None, None));
}

#[test]
fn claim_adds_an_event_once() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &IssueRecord { issue: "65".into(), ..Default::default() }).unwrap();
    assert!(claim(dir.path(), IssueEvent::Start).unwrap());
    assert!(!claim(dir.path(), IssueEvent::Start).unwrap());
    assert_eq!(read(dir.path()).unwrap().events, Some(vec![IssueEvent::Start]));
}

#[test]
fn concurrent_claims_have_one_winner_and_keep_other_fields() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &IssueRecord { issue: "65".into(), ..Default::default() }).unwrap();
    let wins: usize = std::thread::scope(|s| {
        let hs: Vec<_> = (0..8).map(|_| s.spawn(|| claim(dir.path(), IssueEvent::Start).unwrap())).collect();
        let pin = s.spawn(|| update(dir.path(), |r| {
            r.as_mut().unwrap().baseline = Some(BaselinePin { sha: "abc".into(), path: "/b".into() });
        }).unwrap());
        pin.join().unwrap();
        hs.into_iter().map(|h| h.join().unwrap() as usize).sum()
    });
    assert_eq!(wins, 1);
    let rec = read(dir.path()).unwrap();
    assert_eq!(rec.events, Some(vec![IssueEvent::Start]));
    assert_eq!(rec.baseline.unwrap().sha, "abc");
}

#[test]
fn claiming_without_a_record_claims_nothing() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!claim(dir.path(), IssueEvent::Start).unwrap());
    assert!(read(dir.path()).is_none());
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run -p devkit-common record::`
Expected: FAIL to compile (`events`, `claim`, `update` not found).

- [ ] **Step 3: Add the fields, `update` and `claim`; move the four updaters onto `update`; add `..Default::default()` to every literal**

`write_pin` keeps its behaviour of synthesizing a record when none exists: inside `update`, a `None` becomes `Some(synthesized)`. `pr create`'s `record_with_pr` call becomes one `update` that sets `pr`. Task 7 adds the `pr_open` claim to that same closure.

- [ ] **Step 4: Run the workspace tests**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit** `feat(record): lock issue record updates and track fired events`.

### Task 3: Transition rule and the writer trait

**Files:**
- Create: `crates/devkit-common/src/tracker/status.rs` (add `pub mod status;` to `tracker/mod.rs`)
- Test: unit tests in that file

**Interfaces:**
- Consumes: `EventTransition` (Task 1).
- Produces:
  - `pub fn target<'a>(t: &'a EventTransition, current: Option<&str>) -> Option<&'a str>` returns `Some(t.to)` when the issue should move, and `None` when it is already at `to` or `current` matches no `from` entry. `current` `None` matches `""` and `"*"`. Matching is case-insensitive after trim.
  - `#[ambassador::delegatable_trait] pub trait StatusWriter { fn status(&self, id: &str) -> Result<Option<String>>; fn set_status(&self, id: &str, to: &str) -> Result<()>; }`
  - `#[derive(ambassador::Delegate)] #[delegate(StatusWriter)] pub enum Writer { Github(github_status::GithubWriter), Linear(linear_status::LinearWriter) }`. The variants' types come from Tasks 4 and 5; until then this task declares the enum with only the trait and `target`, and Task 4 adds the enum.
  - `pub fn find_name<'a>(names: impl IntoIterator<Item = &'a str>, wanted: &str) -> Option<&'a str>`: the shared case-insensitive, trimmed lookup Tasks 4 and 5 use for options and states.

- [ ] **Step 1: Write the failing tests**

```rust
fn t(from: &[&str], to: &str) -> EventTransition {
    EventTransition { from: from.iter().map(|s| s.to_string()).collect(), to: to.into() }
}

#[test]
fn target_applies_from_and_skips_when_already_there() {
    assert_eq!(target(&t(&["*"], "In progress"), Some("Todo")), Some("In progress"));
    assert_eq!(target(&t(&["Todo"], "In progress"), Some("In review")), None);
    assert_eq!(target(&t(&["*"], "In progress"), Some(" in PROGRESS ")), None);
    assert_eq!(target(&t(&["todo "], "In progress"), Some("Todo")), Some("In progress"));
}

#[test]
fn no_status_matches_the_empty_string_and_star_only() {
    assert_eq!(target(&t(&["", "Todo"], "In progress"), None), Some("In progress"));
    assert_eq!(target(&t(&["*"], "In progress"), None), Some("In progress"));
    assert_eq!(target(&t(&["Todo"], "In progress"), None), None);
}

#[test]
fn find_name_is_case_and_space_insensitive() {
    assert_eq!(find_name(["Todo", "In Progress"], " in progress"), Some("In Progress"));
    assert_eq!(find_name(["Todo"], "Done"), None);
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run -p devkit-common tracker::status`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `target`, `find_name` and the trait**

- [ ] **Step 4: Run them and see them pass**

Run: `cargo nextest run -p devkit-common tracker::status`
Expected: PASS.

- [ ] **Step 5: Commit** `feat(tracker): add the status transition rule`.

### Task 4: GitHub writer

**Files:**
- Create: `crates/devkit-common/src/tracker/github_status.rs`
- Modify: `crates/devkit-common/src/github.rs` (expose the unfiltered response: make `graphql_request` `pub fn graphql_value`)
- Modify: `crates/devkit-common/src/tracker/status.rs` (add `Writer`)
- Create: fixtures under `crates/devkit-common/src/tracker/fixtures/` (`github_status_item.json`, `github_status_no_item.json`, `github_status_two_projects.json`, `github_status_not_single_select.json`, `github_status_insufficient_scopes.json`)
- Test: unit tests in `github_status.rs`

**Interfaces:**
- Consumes: `ProjectRef`, `GithubConfig::status_field` (Task 1); `StatusWriter`, `find_name` (Task 3); `Api` (`github.rs:93`, `:128`).
- Produces:
  - `pub fn status_query(slug: &str, issue: u64, project: &ProjectRef, field: &str) -> String`: the spec's query. When `project.owner` is `Some`, it reads `repositoryOwner(login:)` in place of `repository.owner`.
  - `pub struct StatusRead { pub issue_id: String, pub project_id: String, pub item_id: Option<String>, pub field_id: String, pub current: Option<String>, pub options: Vec<(String, String)> }` (option id, name).
  - `pub fn parse_status(resp: &Value, project_number: u64, field: &str) -> Result<StatusRead>`: picks the item whose `project.number` equals `project_number`. Errors: no project, which names `[github] project`; a field missing or not single-select, which names `[github] status_field`; `INSUFFICIENT_SCOPES`, via `scope_error`.
  - `pub fn scope_error(resp: &Value, source: TokenSource) -> Option<anyhow::Error>`: for an `errors[].type == "INSUFFICIENT_SCOPES"`, returns "token lacks the `project` scope" plus `gh auth refresh -s project` for `TokenSource::Gh`, or "reissue GH_TOKEN with `project`" (naming the variable) for `TokenSource::Env(var)`.
  - `pub fn add_item_mutation(project_id: &str, content_id: &str) -> String`, `pub fn parse_added_item(resp: &Value) -> Result<String>`, `pub fn set_field_mutation(project_id: &str, item_id: &str, field_id: &str, option_id: &str) -> String`.
  - `pub struct GithubWriter { repo: Repo, api: Api, project: ProjectRef, field: String }` with `pub fn new(repo: Repo, project: ProjectRef, field: String) -> Self`, implementing `StatusWriter`. An unknown `to` errors with "no option `<to>` in `<field>`; options: a, b, c". `set_status` adds the item first when `item_id` is `None`.

- [ ] **Step 1: Write the failing tests** over the fixtures

```rust
#[test]
fn reads_the_configured_projects_item() {
    let r = parse_status(&fixture("github_status_two_projects.json"), 3, "Status").unwrap();
    assert_eq!((r.item_id.as_deref(), r.current.as_deref()), (Some("PVTI_3"), Some("Todo")));
}

#[test]
fn an_issue_outside_the_project_has_no_item_and_no_status() {
    let r = parse_status(&fixture("github_status_no_item.json"), 3, "Status").unwrap();
    assert_eq!((r.item_id, r.current), (None, None));
}

#[test]
fn a_field_that_is_not_single_select_names_the_key() {
    let e = parse_status(&fixture("github_status_not_single_select.json"), 3, "Status").unwrap_err();
    assert!(e.to_string().contains("[github] status_field"));
}

#[test]
fn insufficient_scopes_names_the_remedy_for_each_token_source() {
    let v = fixture("github_status_insufficient_scopes.json");
    assert!(scope_error(&v, TokenSource::Gh).unwrap().to_string().contains("gh auth refresh -s project"));
    assert!(scope_error(&v, TokenSource::Env("GH_TOKEN")).unwrap().to_string().contains("GH_TOKEN"));
}

#[test]
fn an_owner_slash_number_project_queries_that_owner() {
    let q = status_query("me/repo", 65, &ProjectRef { owner: Some("org".into()), number: 7 }, "Status");
    assert!(q.contains("repositoryOwner(login: \"org\")") && q.contains("projectV2(number: 7)"));
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run -p devkit-common tracker::github_status`
Expected: FAIL to compile.

- [ ] **Step 3: Implement the builders, the parsers and `GithubWriter`; add `Writer` to `status.rs`**

String values in queries go through the JSON-string quoting `tracker/github.rs` already uses for owner and name. The network path mirrors `GithubTracker::details`: `api.graphql_value` when `api.token()` is `Some`, else `gh_json(["api","graphql","--hostname",host,"-f",query])`.

- [ ] **Step 4: Run them and see them pass**

Run: `cargo nextest run -p devkit-common tracker::`
Expected: PASS.

- [ ] **Step 5: Commit** `feat(tracker): write github project status`.

### Task 5: Linear writer

**Files:**
- Create: `crates/devkit-common/src/tracker/linear_status.rs`
- Modify: `crates/devkit-common/src/tracker/linear.rs` (make `parse_id` and `send` `pub(crate)`)
- Create: fixture `crates/devkit-common/src/tracker/fixtures/linear_status.json`
- Test: unit tests in `linear_status.rs`

**Interfaces:**
- Consumes: `StatusWriter`, `find_name` (Task 3); `linear::parse_id`, `linear::send`.
- Produces:
  - `pub fn status_query(id: &str) -> Result<String>` returns `issues(filter: {team: {key: {eq}}, number: {eq}}) { nodes { id state { name } team { states { nodes { id name } } } } }`. An id `parse_id` rejects errors naming it.
  - `pub struct LinearStatus { pub issue_id: String, pub current: String, pub states: Vec<(String, String)> }` and `pub fn parse_status(resp: &Value, id: &str) -> Result<LinearStatus>`.
  - `pub fn set_state_mutation(issue_id: &str, state_id: &str) -> String` (`issueUpdate(id:, input: {stateId:})`).
  - `pub struct LinearWriter { key: String }` with `pub fn new(key: String) -> Self`, implementing `StatusWriter`. An unknown `to` errors listing the team's states.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn reads_the_issue_its_state_and_its_teams_states() {
    let s = parse_status(&fixture("linear_status.json"), "ENG-123").unwrap();
    assert_eq!(s.current, "Todo");
    assert!(s.states.iter().any(|(_, n)| n == "In Progress"));
}

#[test]
fn a_lowercase_id_queries_the_uppercase_team() {
    assert!(status_query("eng-123").unwrap().contains("\"ENG\""));
}

#[test]
fn a_malformed_id_is_an_error_naming_it() {
    assert!(status_query("not-an-id-x").unwrap_err().to_string().contains("not-an-id-x"));
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run -p devkit-common tracker::linear_status`
Expected: FAIL to compile.

- [ ] **Step 3: Implement; add the `Linear` variant to `Writer`**

- [ ] **Step 4: Run them and see them pass**

Run: `cargo nextest run -p devkit-common tracker::`
Expected: PASS.

- [ ] **Step 5: Commit** `feat(tracker): write linear issue state`.

### Task 6: `devkit issue event`

**Files:**
- Create: `src/bin/devkit/issue/event.rs`
- Modify: `src/bin/devkit/issue/mod.rs` (new `Event` subcommand beside `Status` at :168)
- Modify: `crates/devkit-common/src/tracker/status.rs` (writer selection)
- Test: `tests/issue_event.rs` (driven through `tests/common/ghfake.rs`)

**Interfaces:**
- Consumes: Tasks 1-5; `tracker::select` / `tracker::resolve` for the resolved kind; `record::read`.
- Produces:
  - `pub fn writer_for(kind: TrackerKind, cfg: &devkit_config::Config, repos: &forge::Repos) -> Result<Writer>`: Linear when `kind` is Linear (needs `LINEAR_API_KEY` through `secrets::resolve`), GitHub when `kind` is Github and `[github] project` is set. Otherwise an error naming the missing key (`[github] project`, or `[tracker] kind` for `none`).
  - CLI `devkit issue event <setup|start|pr_open> [ISSUE]`: a clap `ValueEnum` in the binary mapped onto `IssueEvent`, with `#[value(name = "pr_open")]` so the spelling matches the config. Help: "Move the issue's tracker status as [issue.events] configures this event".
  - `pub(crate) enum Outcome { Moved { from: Option<String>, to: String }, AlreadyThere, NotInFrom(Option<String>), NotConfigured }` and `pub(crate) fn fire(dir: &Path, event: IssueEvent, issue: Option<&str>) -> Result<Outcome>`. It resolves the issue from the record at `dir`'s checkout root when `issue` is `None`, and errors when neither names a tracker id. It reads the status, applies `target` and writes. It never touches `events`: claiming belongs to the triggers.
  - The command prints one line per outcome to stderr ("moved 65: Todo -> In progress", "65 is already In progress", "65 is In review, not in [issue.events.start] from", "[issue.events.start] is not configured") and exits 0 on all four. A write error exits non-zero.

- [ ] **Step 1: Write the failing tests** in `tests/issue_event.rs`

```rust
#[test]
fn an_unconfigured_event_does_nothing_and_says_so() {
    let gh = GhFake::without_pr("");
    gh.record_issue("65");
    let out = gh.issue(&["event", "start"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("[issue.events.start] is not configured"));
    assert!(!gh.calls().contains("graphql"));
}

#[test]
fn an_issue_already_at_the_target_is_not_written() {
    let gh = GhFake::without_pr("[issue.events.start]\nto = \"In progress\"\n[github]\nproject = 3\n");
    gh.record_issue("65");
    gh.serve_graphql(include_str!("fixtures/issue_event/at_target.json"));
    let out = gh.issue(&["event", "start"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("already In progress"));
    assert_eq!(gh.calls().matches("graphql").count(), 1);
}

#[test]
fn github_without_a_project_is_an_error_naming_the_key() {
    let gh = GhFake::without_pr("[issue.events.start]\nto = \"In progress\"\n");
    gh.record_issue("65");
    let out = gh.issue(&["event", "start"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("[github] project"));
}
```

Add `serve_graphql(&self, body: &str)` to `tests/common/ghfake.rs` if no equivalent exists (it writes `graphql.json`). Add the Review Focus test for item 4 as a unit test of `writer_for` in `status.rs`: kind Linear with `[github] project` set returns `Writer::Linear`.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run --test issue_event && cargo nextest run -p devkit-common tracker::status`
Expected: FAIL (unknown subcommand `event`).

- [ ] **Step 3: Implement the subcommand, `fire` and `writer_for`**

- [ ] **Step 4: Run them and see them pass; confirm `issue status` is unchanged**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit** `feat(issue): add issue event to move tracker status`.

### Task 7: Inline triggers in `issue setup` and `issue pr create`, and record origin

**Files:**
- Modify: `src/bin/devkit/issue/setup.rs` (record write at :593; `run_after_worktree_create` call at :644)
- Modify: `src/bin/devkit/issue/checkout.rs:405-418` (`origin = Checkout`)
- Modify: `src/bin/devkit/issue/pr/create.rs` (after `ensure` at :238)
- Test: `tests/issue_event.rs`

**Interfaces:**
- Consumes: `fire`, `Outcome` (Task 6); `record::update`, `RecordOrigin` (Task 2).
- Produces:
  - `issue setup` writes `origin = Setup`, and also `events = Some(vec![Setup])` when `[issue.events.setup]` is configured, otherwise `Some(vec![])`. As its last step, after `run_after_worktree_create`, it calls `fire(worktree, Setup, Some(issue))` when configured. An `Err` prints `warning: issue event setup: <error>` to stderr, and setup still exits 0 with its JSON last on stdout.
  - `issue pr checkout` writes `origin = Checkout` and `events = Some(vec![])`.
  - `issue pr create`: the `update` that records the PR also claims `PrOpen` when configured. It then calls `fire(.., PrOpen, ..)` when that claim was new, whether `ensure` created the PR or reused it, and an `Err` warns.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn setup_fires_its_event_and_warns_without_failing() {
    let gh = GhFake::without_pr("[issue.events.setup]\nto = \"Todo\"\n[github]\nproject = 3\n");
    gh.serve_graphql("{\"errors\":[{\"type\":\"INSUFFICIENT_SCOPES\",\"message\":\"x\"}]}");
    let out = gh.issue(&["setup", "65"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("warning: issue event setup"));
    let rec = read_record_of_setup_worktree(&gh, &out);
    assert_eq!((rec.origin, rec.events), (Some(RecordOrigin::Setup), Some(vec![IssueEvent::Setup])));
}

#[test]
fn pr_create_fires_pr_open_when_it_reuses_a_pr() {
    // A branch whose PR already exists; `pr_open` is claimed once and its read query runs.
}

#[test]
fn checkout_records_its_origin_and_fires_nothing() {
    // `issue pr checkout <n>` with every event configured: origin = Checkout, events = Some([]), no graphql call.
}
```

The last two follow the existing `issue pr create` and `pr checkout` tests' setup in `tests/` (`rg -l "pr\", \"create\"" tests`). `read_record_of_setup_worktree` parses the `worktree` key from the command's JSON stdout and calls `record::read`.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run --test issue_event`
Expected: FAIL.

- [ ] **Step 3: Implement the three call sites**

- [ ] **Step 4: Run the workspace tests**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit** `feat(issue): fire status events on setup and pr create`.

### Task 8: SessionStart hook

**Files:**
- Create: `src/bin/devkit/hook/issue_event.rs`
- Modify: `src/bin/devkit/hook/mod.rs` (a `HookEvent::SessionStart` arm in `run`, before the record-only fallback)
- Test: unit tests in `hook/issue_event.rs`; `tests/hook_issue_event.rs`

**Interfaces:**
- Consumes: `record::read_state`, `record::claim`, `RecordOrigin`, `IssueId::tracker` (`worktree.rs:77`), `devkit_common::config::resolve`, `sys::spawn_background` (`sys/mod.rs:100`).
- Produces:
  - `fn qualifies(rec: &IssueRecord) -> bool`: `origin` is `Setup` or `None`; `rec.issue.parse::<IssueId>()` gives `tracker()` `Some`; and not `events.is_none() && pr.is_some()`.
  - `pub fn on_session_start(checkout: &Checkout)`: returns at once unless the record reads `Ok`, `qualifies` holds, the project config has `[issue.events.start]`, and the resolved tracker kind is not `None`. Otherwise it calls `record::claim(root, Start)`, and on `Ok(true)` spawns `current_exe() issue event start --dir <root>` through `spawn_background`. It ignores every error, writes nothing to stdout, and does no network IO.
  - The `SessionStart` arm: `on_session_start(&checkout)`, then `record_in(..)`.
  - `spawn_background` sends the child's output to null, so `issue event` takes a hidden `--log-file <path>` that appends each outcome or error line, with an RFC 3339 timestamp, to that file. The hook passes `<root>/.devkit/issue-event.log`; that file is how a person sees why a background run failed, and `issues.md` (Task 9) names it.
- Extra end-to-end test: `a_failed_background_run_is_logged`. The fake answers `INSUFFICIENT_SCOPES`; poll until `.devkit/issue-event.log` contains "lacks the `project` scope".

- [ ] **Step 1: Write the failing tests**

Unit tests for `qualifies`:

```rust
#[test]
fn only_setup_worktrees_with_a_tracker_id_qualify() {
    let base = IssueRecord { issue: "65".into(), origin: Some(RecordOrigin::Setup), events: Some(vec![]), ..Default::default() };
    assert!(qualifies(&base));
    assert!(qualifies(&IssueRecord { origin: None, ..base.clone() }));
    assert!(!qualifies(&IssueRecord { origin: Some(RecordOrigin::Checkout), ..base.clone() }));
    assert!(!qualifies(&IssueRecord { issue: "UNKNOWN".into(), ..base.clone() }));
    assert!(!qualifies(&IssueRecord { issue: String::new(), ..base.clone() }));
    assert!(!qualifies(&IssueRecord { events: None, pr: Some(locator()), ..base.clone() }));
}
```

End-to-end tests in `tests/hook_issue_event.rs`, sending a SessionStart payload whose `cwd` is a set-up worktree, with the ghfake environment so the spawned child talks to the fake:
- `first_session_claims_start_and_runs_the_event`: afterwards the record's `events` contains `start`, and polling `gh.calls()` sees one `graphql` call.
- `second_session_claims_nothing`: the record is unchanged and the hook's stdout is empty.
- `nothing_is_claimed_without_start_configured`, `nothing_is_claimed_in_a_checkout_worktree`, `nothing_is_claimed_with_tracker_none`.
- `a_corrupt_record_is_ignored_silently`: write `not toml` to `.devkit/issue.toml`. The hook exits 0 with empty stdout and stderr, and the file is unchanged. (Review Focus 2.)

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run --test hook_issue_event && cargo nextest run hook::issue_event`
Expected: FAIL.

- [ ] **Step 3: Implement `qualifies`, `on_session_start` and the arm**

- [ ] **Step 4: Run them, plus the hook contract suites**

Run: `cargo nextest run --test hook_issue_event --test hook_exit_codes --test hook_manifests && cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit** `feat(hook): fire the start status event on first session`.

### Task 9: `devkit doctor` row and the issues reference

**Files:**
- Modify: `src/bin/devkit/doctor.rs` (an `issue_events_row(start: &Path) -> Option<Row>`, gathered beside `tasks_row` at :236)
- Modify: `plugin/skills/using-devkit/references/issues.md`
- Test: `tests/doctor_issue_events.rs`

**Interfaces:**
- Consumes: `writer_for` (Task 6), `parse_status` and `scope_error` (Task 4), `IssueEventsConfig::any` (Task 1).
- Produces: `issue_events_row` returns `None` when no event is configured. Otherwise it returns a row listing `setup: * -> Todo`-style transitions. On GitHub it runs the read query once and fails the row with the error naming the key: project missing, field not single-select, any `to` or `from` name (other than `*` and `""`) missing from the options, or `INSUFFICIENT_SCOPES`. With no writer, the row reports the missing key.

- [ ] **Step 1: Write the failing tests**
  - `no_events_no_row`: `devkit doctor --json` has no `issue events` row.
  - `a_configured_project_passes`: the fixture has every name. The row is ok and lists each transition.
  - `a_missing_option_fails_naming_it`: `to = "Shipping"` makes the row fail with "no option `Shipping`".
  - `insufficient_scopes_fails_with_the_remedy`.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run --test doctor_issue_events`
Expected: FAIL.

- [ ] **Step 3: Implement the row; write the reference section**

The `issues.md` section covers four things: when each event fires and in which worktrees (setup worktrees only for `start`; `pr checkout` never), firing once per worktree, rerunning with `devkit issue event`, and the `project` token scope with both remedies. It points at `devkit schema` for the keys.

- [ ] **Step 4: Run every gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo test --workspace --doc && cargo clippy --workspace --all-targets -- -D warnings && devrun task fmt`
Expected: PASS, no diff from fmt.

- [ ] **Step 5: Commit** `feat(doctor): check issue status events`.

## Unresolved questions

- None blocking. The GitHub Enterprise Server minimum for Projects v2, and whether Linear's `issueUpdate` accepts `ENG-123` directly, stay unverified. The design does not depend on either: the doctor row reports a GHES without Projects v2, and the writer sends Linear's UUID.
