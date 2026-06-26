# issue review request/finish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split `issue review` into `issue review request` (author asks for review) and `issue review finish` (reviewer announces the outcome over Slack), with a multi-target `--to` model (people + channels) and call-time `--arg` template overrides.

**Architecture:** `review` becomes a clap subcommand container (`ReviewCmd::{Request, Finish}`). `src/bin/issue/review.rs` becomes a `review/` module: `mod.rs` holds shared helpers (target resolution, reverse lookup, `--arg` parsing, per-recipient render + Slack fan-out), `request.rs` and `finish.rs` hold the two flows. Pure decision functions are unit-tested; orchestration shells out to `git`/`gh`/Slack exactly as the current `review.rs` does.

**Tech Stack:** Rust 2024, `clap` (derive), `anyhow`, `serde`/`serde_json`, minijinja via `devkit_common::template`, `devkit_common::{cmd, slack, secrets}`, `devkit_ports::config`.

**Reference:** spec at `docs/superpowers/specs/2026-06-26-issue-review-request-finish-design.md`.

---

## File structure

| File | Responsibility |
|---|---|
| `crates/devkit-ports/src/config.rs` (modify) | Rename `templates.slack` → `review_request`; add `review_finish`; consts + accessors + tests |
| `src/bin/issue/review.rs` (delete) | Replaced by the `review/` module |
| `src/bin/issue/review/mod.rs` (create) | Shared: `Target`, `resolve_target`, `target_from_person`, `person_by_login`, `is_human_login`, `parse_args`, `recipient_ctx`, `with_fields`, `base_ctx`, `render_review`, `deliver`, `guard_branch`, `action_for`, `PrAction`, `require_pr_title` |
| `src/bin/issue/review/request.rs` (create) | `request::run` + request-only helpers (`reviewer_logins`, `targets_from_logins`, `resolve_request_targets`) and gh structs |
| `src/bin/issue/review/finish.rs` (create) | `finish::run` + `resolve_pr`, `author_target` and gh structs |
| `src/bin/issue/main.rs` (modify) | `Cmd::Review` → container holding `ReviewCmd`; dispatch |
| `README.md` (modify) | Document the two subcommands |

The current `review.rs` test functions (`guard_rejects_base_branches`, `action_maps_pr_state`, `require_pr_title_rejects_empty`, the renamed default-template test) move into `review/mod.rs`. `resolve_reviewer` and `SlackIntent` are **deleted** (replaced by the target model). `reviewer_prefers_explicit_then_alias` is deleted with `resolve_reviewer`.

---

## Task 1: Rename `templates.slack` → `review_request`, add `review_finish`

**Files:**
- Modify: `crates/devkit-ports/src/config.rs:122` (const), `:137` (field), `:160-162` (accessor), `:762` (test)
- Modify: `src/bin/issue/review.rs:139,316` (call sites, kept compiling)

- [ ] **Step 1: Update the config test to the new names**

In `crates/devkit-ports/src/config.rs`, find the test asserting the default (around line 762):

```rust
        assert_eq!(t.slack(), DEFAULT_SLACK);
```

Replace with:

```rust
        assert_eq!(t.review_request(), DEFAULT_REVIEW_REQUEST);
        assert_eq!(t.review_finish(), DEFAULT_REVIEW_FINISH);
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit-ports --lib 2>&1 | head -30`
Expected: FAIL — compile error, `no method named review_request`, `cannot find value DEFAULT_REVIEW_REQUEST`.

- [ ] **Step 3: Rename the const and add the new one**

Replace `crates/devkit-ports/src/config.rs:122`:

```rust
pub const DEFAULT_SLACK: &str = "{{ input }} {{ pr_url }}";
```

with:

```rust
pub const DEFAULT_REVIEW_REQUEST: &str = "{{ input }} {{ pr_url }}";
pub const DEFAULT_REVIEW_FINISH: &str = "{{ input }} {{ pr_url }}";
```

- [ ] **Step 4: Rename the struct field and add the new one**

In the `Templates` struct, replace:

```rust
    pub slack: Option<String>,
```

with:

```rust
    pub review_request: Option<String>,
    pub review_finish: Option<String>,
```

- [ ] **Step 5: Rename the accessor and add the new one**

Replace the `slack` accessor:

```rust
    pub fn slack(&self) -> &str {
        self.slack.as_deref().unwrap_or(DEFAULT_SLACK)
    }
```

with:

```rust
    pub fn review_request(&self) -> &str {
        self.review_request.as_deref().unwrap_or(DEFAULT_REVIEW_REQUEST)
    }
    pub fn review_finish(&self) -> &str {
        self.review_finish.as_deref().unwrap_or(DEFAULT_REVIEW_FINISH)
    }
```

- [ ] **Step 6: Keep `review.rs` compiling**

In `src/bin/issue/review.rs`, line 316, replace `tmpls.slack(),` with `tmpls.review_request(),`.
At line 139 (the `default_slack_appends_url` test), replace `t.slack()` with `t.review_request()`.

- [ ] **Step 7: Run the gate**

Run: `cargo test -p devkit-ports --lib && cargo build -p devkit 2>&1 | tail -5`
Expected: PASS; `issue` binary builds.

- [ ] **Step 8: Commit**

```bash
git add crates/devkit-ports/src/config.rs src/bin/issue/review.rs
git commit -m "feat(config): rename slack template to review_request, add review_finish"
```

---

## Task 2: Move `review.rs` → `review/mod.rs` (verbatim)

Pure restructure: no behavior change. `main.rs` already has `mod review;`, which resolves to `review/mod.rs` once the file is gone.

**Files:**
- Delete: `src/bin/issue/review.rs`
- Create: `src/bin/issue/review/mod.rs`

- [ ] **Step 1: Move the file**

```bash
mkdir -p src/bin/issue/review
git mv src/bin/issue/review.rs src/bin/issue/review/mod.rs
```

- [ ] **Step 2: Run the gate**

Run: `cargo test -p devkit --bin issue 2>&1 | tail -15 && cargo clippy --bin issue -- -D warnings 2>&1 | tail -5`
Expected: PASS, zero warnings (identical code, new path).

- [ ] **Step 3: Commit**

```bash
git add -A src/bin/issue/review
git commit -m "refactor(issue): make review a module directory"
```

---

## Task 3: Shared helpers + rework `request` under `issue review request`

This task replaces the single-`--to`/`--reviewer` model with the multi-target model and wires the `ReviewCmd::Request` subcommand. All helpers it introduces are consumed here (no dead code).

**Files:**
- Modify: `src/bin/issue/review/mod.rs`
- Create: `src/bin/issue/review/request.rs`
- Modify: `src/bin/issue/main.rs:112-118` (Review variant), `:207-227` (dispatch)

- [ ] **Step 1: Write failing tests for the new shared helpers**

Replace the entire `#[cfg(test)] mod tests { … }` block in `src/bin/issue/review/mod.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use devkit_ports::config::Person;
    use std::collections::{BTreeMap, HashMap};

    fn person(slack: &str, gh: Option<&str>) -> Person {
        Person { slack: slack.into(), github: gh.map(String::from) }
    }
    fn people() -> HashMap<String, Person> {
        HashMap::from([
            ("lev".to_string(), person("U_LEV", Some("LevValle"))),
            ("igor".to_string(), person("U_IGOR", None)),
        ])
    }

    #[test]
    fn guard_rejects_base_branches() {
        assert!(guard_branch("staging").is_err());
        assert!(guard_branch("main").is_err());
        assert!(guard_branch("lev/eng-1-fix").is_ok());
    }

    #[test]
    fn action_maps_pr_state() {
        assert_eq!(action_for(None), PrAction::Create);
        assert_eq!(action_for(Some("OPEN")), PrAction::AddReviewer);
        assert!(matches!(action_for(Some("MERGED")), PrAction::Stop(_)));
        assert!(matches!(action_for(Some("CLOSED")), PrAction::Stop(_)));
    }

    #[test]
    fn require_pr_title_rejects_empty() {
        assert!(require_pr_title("  ").is_err());
        assert!(require_pr_title("Fix login").is_ok());
    }

    #[test]
    fn default_review_request_appends_url() {
        let t = devkit_ports::config::Templates::default();
        let ctx = serde_json::json!({"input": "please review", "pr_url": "https://gh/pr/1"});
        let out = devkit_common::template::render(t.review_request(), &ctx, &t.variables).unwrap();
        assert_eq!(out, "please review https://gh/pr/1");
    }

    #[test]
    fn resolve_target_classifies_channel_person_and_unknown() {
        let p = people();
        let chan = resolve_target("#eng", &p).unwrap();
        assert_eq!(chan.channel, "#eng");
        assert_eq!(chan.name, "#eng");
        assert!(chan.slack_id.is_none());
        assert!(chan.github.is_none());

        let lev = resolve_target("lev", &p).unwrap();
        assert_eq!(lev.channel, "U_LEV");
        assert_eq!(lev.name, "lev");
        assert_eq!(lev.slack_id.as_deref(), Some("U_LEV"));
        assert_eq!(lev.github.as_deref(), Some("LevValle"));

        assert!(resolve_target("nobody", &p).is_err());
    }

    #[test]
    fn person_by_login_is_case_insensitive() {
        let p = people();
        let (alias, _) = person_by_login("levvalle", &p).unwrap();
        assert_eq!(alias, "lev");
        assert!(person_by_login("ghost", &p).is_none());
    }

    #[test]
    fn is_human_login_excludes_bots() {
        assert!(is_human_login("LevValle"));
        assert!(!is_human_login("coderabbitai[bot]"));
    }

    #[test]
    fn parse_args_validates_against_allowlist() {
        let allowed = BTreeMap::from([("team".to_string(), "platform".to_string())]);
        let ok = parse_args(&["team=infra".to_string()], &allowed).unwrap();
        assert_eq!(ok.get("team").map(String::as_str), Some("infra"));
        assert!(parse_args(&["ghost=x".to_string()], &allowed).is_err());
        assert!(parse_args(&["noeq".to_string()], &allowed).is_err());
    }

    #[test]
    fn recipient_ctx_binds_name_and_slack_id() {
        let base = serde_json::json!({"pr_url": "u"});
        let person_t = Target {
            channel: "U_LEV".into(), name: "lev".into(),
            slack_id: Some("U_LEV".into()), github: Some("LevValle".into()),
        };
        let c = recipient_ctx(&base, &person_t);
        assert_eq!(c["name"], "lev");
        assert_eq!(c["slack_id"], "U_LEV");
        assert_eq!(c["pr_url"], "u");

        let chan_t = Target { channel: "#eng".into(), name: "#eng".into(), slack_id: None, github: None };
        let c = recipient_ctx(&base, &chan_t);
        assert_eq!(c["name"], "#eng");
        assert_eq!(c["slack_id"], "");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit --bin issue review 2>&1 | head -30`
Expected: FAIL — compile errors (`Target`, `resolve_target`, `person_by_login`, `is_human_login`, `parse_args`, `recipient_ctx` not found).

- [ ] **Step 3: Rewrite `review/mod.rs` with the shared helpers**

Replace the **non-test** portion of `src/bin/issue/review/mod.rs` (everything above the `#[cfg(test)]` block) with:

```rust
use anyhow::{Context, Result, bail};
use devkit_common::slack;
use devkit_ports::config::Person;
use std::collections::{BTreeMap, HashMap};

pub(crate) mod finish;
pub(crate) mod request;

/// What to do given an existing PR's state (or none) for the current branch.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PrAction {
    Create,
    AddReviewer,
    Stop(String),
}

/// A resolved Slack recipient: a person alias or a `#channel`.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    /// Conversation id/name passed to `chat.postMessage`.
    pub channel: String,
    /// Template `{{ name }}`: the people alias, or the channel string.
    pub name: String,
    /// Template `{{ slack_id }}`: the person's user id; `None` for a channel.
    pub slack_id: Option<String>,
    /// GitHub login, when this is a person with a `github` handle.
    pub github: Option<String>,
}

/// Branches we must never open a review against.
pub(crate) fn guard_branch(branch: &str) -> Result<()> {
    if branch == "staging" || branch == "main" || branch == "HEAD" {
        bail!("refusing to review from base branch `{branch}` — switch to a feature branch");
    }
    Ok(())
}

/// Map a detected PR state to the next action.
pub(crate) fn action_for(pr_state: Option<&str>) -> PrAction {
    match pr_state {
        None => PrAction::Create,
        Some("OPEN") => PrAction::AddReviewer,
        Some("MERGED") => PrAction::Stop("PR already merged — nothing to review".into()),
        Some("CLOSED") => PrAction::Stop("PR is closed — nothing to review".into()),
        Some(other) => PrAction::Stop(format!("unexpected PR state `{other}`")),
    }
}

/// Reject an empty rendered PR title on the create path.
pub(crate) fn require_pr_title(title: &str) -> Result<()> {
    if title.trim().is_empty() {
        bail!("--pr-title is required to create a PR");
    }
    Ok(())
}

/// Build a `Target` from a `[people]` alias and its entry.
pub(crate) fn target_from_person(alias: &str, p: &Person) -> Target {
    Target {
        channel: p.slack.clone(),
        name: alias.to_string(),
        slack_id: Some(p.slack.clone()),
        github: p.github.clone(),
    }
}

/// Classify one `--to` value. `#…` is a channel (Slack-only); anything else
/// must be a `[people]` alias.
pub(crate) fn resolve_target(value: &str, people: &HashMap<String, Person>) -> Result<Target> {
    if value.starts_with('#') {
        return Ok(Target {
            channel: value.to_string(),
            name: value.to_string(),
            slack_id: None,
            github: None,
        });
    }
    let p = people.get(value).with_context(|| {
        format!("unknown person alias `{value}` — add it under [people] in devkit.toml, or use `#channel`")
    })?;
    Ok(target_from_person(value, p))
}

/// Find the `[people]` alias + entry whose github login matches (case-insensitive).
pub(crate) fn person_by_login<'a>(
    login: &str,
    people: &'a HashMap<String, Person>,
) -> Option<(&'a String, &'a Person)> {
    people
        .iter()
        .find(|(_, p)| p.github.as_deref().is_some_and(|g| g.eq_ignore_ascii_case(login)))
}

/// A GitHub login is a human reviewer unless it is an app/bot (`name[bot]`).
pub(crate) fn is_human_login(login: &str) -> bool {
    !login.ends_with("[bot]")
}

/// Parse repeated `--arg key=value` pairs, validating each key against the
/// declared `[templates.variables]` allowlist.
pub(crate) fn parse_args(
    pairs: &[String],
    allowed: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for pair in pairs {
        let (k, v) = pair
            .split_once('=')
            .with_context(|| format!("--arg must be key=value, got `{pair}`"))?;
        if !allowed.contains_key(k) {
            bail!("--arg `{k}` is not declared in [templates.variables]");
        }
        out.insert(k.to_string(), v.to_string());
    }
    Ok(out)
}

/// Clone `base` and add extra fields for a single template render.
pub(crate) fn with_fields(
    base: &serde_json::Value,
    extra: &[(&str, serde_json::Value)],
) -> serde_json::Value {
    let mut m = base.as_object().cloned().unwrap_or_default();
    for (k, v) in extra {
        m.insert((*k).into(), v.clone());
    }
    serde_json::Value::Object(m)
}

/// Base context shared by every review template: branch + issue record fields.
pub(crate) fn base_ctx(
    record: Option<&crate::record::IssueRecord>,
    branch: &str,
) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("branch".into(), serde_json::json!(branch));
    if let Some(r) = record {
        m.insert("issue".into(), serde_json::json!(r.issue));
        m.insert("slug".into(), serde_json::json!(r.slug));
        m.insert("apps".into(), serde_json::json!(r.apps));
    }
    serde_json::Value::Object(m)
}

/// Per-recipient context: `name` + `slack_id` bound on top of `base`.
pub(crate) fn recipient_ctx(base: &serde_json::Value, t: &Target) -> serde_json::Value {
    with_fields(
        base,
        &[
            ("name", serde_json::json!(t.name)),
            ("slack_id", serde_json::json!(t.slack_id.clone().unwrap_or_default())),
        ],
    )
}

/// Render a review template, attaching the template key and, when the setup
/// record is absent, a hint that an `{{ issue }}`/`{{ slug }}` reference needs it.
pub(crate) fn render_review(
    tmpl: &str,
    key: &str,
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    missing_record_at: Option<&str>,
) -> Result<String> {
    let mut r = devkit_common::template::render(tmpl, ctx, vars)
        .with_context(|| format!("rendering `{key}` template"));
    if let Some(worktree) = missing_record_at {
        r = r.with_context(|| {
            format!("no .devkit/issue.toml found in {worktree} — was it created by `issue setup`?")
        });
    }
    r
}

/// Render `tmpl` once per target (binding `name`/`slack_id`) and Slack each.
/// With no `SLACK_TOKEN`, print the resolved intents instead.
pub(crate) fn deliver(
    tmpl: &str,
    key: &str,
    base: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    missing_at: Option<&str>,
    targets: &[Target],
) -> Result<()> {
    let token = devkit_common::secrets::resolve("SLACK_TOKEN");
    for t in targets {
        let ctx = recipient_ctx(base, t);
        let text = render_review(tmpl, key, &ctx, vars, missing_at)?;
        match &token {
            Some(tok) => {
                slack::post_message(tok, &t.channel, &text)?;
                println!("Sent to {} ({})", t.name, t.channel);
            }
            None => {
                let intent = serde_json::json!({ "to": t.name, "channel": t.channel, "text": text });
                println!("{}", serde_json::to_string_pretty(&intent)?);
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run the shared-helper tests**

Run: `cargo test -p devkit --bin issue review::tests 2>&1 | tail -20`
Expected: the 9 `review::tests::*` tests PASS. (The crate won't fully build yet — `request`/`finish` modules are declared but missing. That's the next steps; this command compiles the `review` test target only if cargo can build the bin. If it errors on missing modules, proceed to Step 5 and re-run after Step 7.)

- [ ] **Step 5: Create `request.rs` with the request flow + its helpers**

Create `src/bin/issue/review/request.rs`:

```rust
use anyhow::{Context, Result, bail};
use devkit_common::cmd::{capture, gh_json, git};
use devkit_ports::config::Person;
use serde::Deserialize;
use std::collections::HashMap;

use super::{
    PrAction, Target, action_for, base_ctx, deliver, guard_branch, is_human_login, parse_args,
    person_by_login, render_review, require_pr_title, resolve_target, target_from_person, with_fields,
};

pub struct Args {
    pub body: Option<String>,
    pub to: Vec<String>,
    pub base: Option<String>,
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
    pub no_push: bool,
    pub args: Vec<String>,
    pub dir: Option<String>,
    pub config: Option<String>,
}

#[derive(Deserialize)]
struct PrView {
    number: u64,
    state: String,
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewRequestsView {
    review_requests: Vec<ReviewRequest>,
}

#[derive(Deserialize)]
struct ReviewRequest {
    #[serde(default)]
    login: Option<String>,
}

/// GitHub logins among targets that can be requested as reviewers, plus warnings
/// for people that have no github handle. Channels are silently Slack-only.
pub(crate) fn reviewer_logins(targets: &[Target]) -> (Vec<String>, Vec<String>) {
    let mut logins = Vec::new();
    let mut warnings = Vec::new();
    for t in targets {
        match &t.github {
            Some(login) => logins.push(login.clone()),
            None if t.slack_id.is_some() => {
                warnings.push(format!("`{}` has no github handle; not added as reviewer", t.name));
            }
            None => {}
        }
    }
    (logins, warnings)
}

/// Build Slack targets from reviewer logins via reverse lookup. Unmatched logins
/// are skipped with a warning.
pub(crate) fn targets_from_logins(
    logins: &[String],
    people: &HashMap<String, Person>,
) -> (Vec<Target>, Vec<String>) {
    let mut targets = Vec::new();
    let mut warnings = Vec::new();
    for login in logins {
        match person_by_login(login, people) {
            Some((alias, p)) => targets.push(target_from_person(alias, p)),
            None => warnings.push(format!("reviewer `{login}` has no [people] alias; skipped")),
        }
    }
    (targets, warnings)
}

/// Notify-targets on the AddReviewer path: explicit `--to`, else the PR's
/// existing human reviewers (reverse-looked-up).
fn resolve_request_targets(
    explicit: &[Target],
    pr: u64,
    cwd: &str,
    people: &HashMap<String, Person>,
) -> Result<Vec<Target>> {
    if !explicit.is_empty() {
        return Ok(explicit.to_vec());
    }
    let view: ReviewRequestsView =
        gh_json(&["pr", "view", &pr.to_string(), "--json", "reviewRequests"], cwd)?;
    let logins: Vec<String> = view
        .review_requests
        .into_iter()
        .filter_map(|r| r.login)
        .filter(|l| is_human_login(l))
        .collect();
    if logins.is_empty() {
        bail!("no reviewers on the PR and no --to given");
    }
    let (targets, warnings) = targets_from_logins(&logins, people);
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    if targets.is_empty() {
        bail!("none of the PR's reviewers map to a [people] alias; pass --to");
    }
    Ok(targets)
}

pub fn run(args: Args) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded = devkit_ports::load::load(
        args.config.as_deref().map(std::path::Path::new),
        std::path::Path::new(&start),
    )?;
    let people = &loaded.config.people;
    let tmpls = &loaded.config.templates;

    let mut vars = tmpls.variables.clone();
    vars.extend(parse_args(&args.args, &tmpls.variables)?);

    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"], &start)?
        .trim()
        .to_string();
    guard_branch(&branch)?;

    if !args.no_push {
        git(&["push", "-u", "origin", &branch], &start)
            .context("git push failed (refusing to force-push)")?;
    }

    let explicit: Vec<Target> = args
        .to
        .iter()
        .map(|v| resolve_target(v, people))
        .collect::<Result<_>>()?;

    let toplevel = git(&["rev-parse", "--show-toplevel"], &start)?
        .trim()
        .to_string();
    let record = crate::record::read(std::path::Path::new(&toplevel));
    let missing_at = if record.is_none() { Some(toplevel.as_str()) } else { None };

    let base = base_ctx(record.as_ref(), &branch);
    let pr_title = render_review(
        tmpls.pr_title(),
        "pr_title",
        &with_fields(&base, &[("input", serde_json::json!(args.pr_title.clone().unwrap_or_default()))]),
        &vars,
        missing_at,
    )?;
    let pr_body = render_review(
        tmpls.pr_body(),
        "pr_body",
        &with_fields(
            &base,
            &[
                ("input", serde_json::json!(args.pr_body.clone().unwrap_or_default())),
                ("pr_title", serde_json::json!(pr_title)),
            ],
        ),
        &vars,
        missing_at,
    )?;

    let existing: Option<PrView> = gh_json::<Vec<PrView>>(
        &["pr", "list", "--head", &branch, "--state", "all", "--json", "number,state,url", "--limit", "1"],
        &start,
    )?
    .into_iter()
    .next();

    let (pr_url, targets) = match action_for(existing.as_ref().map(|p| p.state.as_str())) {
        PrAction::Stop(reason) => bail!("{reason}"),
        PrAction::AddReviewer => {
            let pr = existing.expect("AddReviewer implies an existing PR");
            let targets = resolve_request_targets(&explicit, pr.number, &start, people)?;
            let (logins, warnings) = reviewer_logins(&targets);
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            if !logins.is_empty() {
                capture(
                    "gh",
                    &["pr", "edit", &pr.number.to_string(), "--add-reviewer", &logins.join(",")],
                    Some(&start),
                )
                .context("gh pr edit --add-reviewer failed")?;
            }
            (pr.url, targets)
        }
        PrAction::Create => {
            require_pr_title(&pr_title)?;
            if explicit.is_empty() {
                bail!("no reviewers on the PR and no --to given");
            }
            let (logins, warnings) = reviewer_logins(&explicit);
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            let base_branch = args
                .base
                .clone()
                .unwrap_or_else(|| loaded.config.defaults.pr_base.clone());
            let joined = logins.join(",");
            let mut gh_args = vec![
                "pr", "create", "--base", &base_branch, "--title", &pr_title, "--body", &pr_body,
            ];
            if !logins.is_empty() {
                gh_args.push("--reviewer");
                gh_args.push(&joined);
            }
            let out = capture("gh", &gh_args, Some(&start)).context("gh pr create failed")?;
            let url = out
                .lines()
                .rev()
                .find(|l| l.contains("://"))
                .context("could not parse a PR URL from `gh pr create` output")?
                .trim()
                .to_string();
            (url, explicit)
        }
    };

    let base = with_fields(
        &base,
        &[
            ("pr_url", serde_json::json!(pr_url)),
            ("pr_title", serde_json::json!(pr_title)),
            ("input", serde_json::json!(args.body.clone().unwrap_or_default())),
        ],
    );
    deliver(tmpls.review_request(), "review_request", &base, &vars, missing_at, &targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chan(name: &str) -> Target {
        Target { channel: name.into(), name: name.into(), slack_id: None, github: None }
    }
    fn person(name: &str, gh: Option<&str>) -> Target {
        Target {
            channel: format!("U_{name}"),
            name: name.into(),
            slack_id: Some(format!("U_{name}")),
            github: gh.map(String::from),
        }
    }

    #[test]
    fn reviewer_logins_collects_handles_and_warns() {
        let targets = vec![person("lev", Some("LevValle")), person("igor", None), chan("#eng")];
        let (logins, warnings) = reviewer_logins(&targets);
        assert_eq!(logins, vec!["LevValle"]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("igor"));
    }

    #[test]
    fn targets_from_logins_reverse_looks_up_and_warns() {
        use devkit_ports::config::Person;
        use std::collections::HashMap;
        let people = HashMap::from([(
            "lev".to_string(),
            Person { slack: "U_LEV".into(), github: Some("LevValle".into()) },
        )]);
        let (targets, warnings) =
            targets_from_logins(&["levvalle".into(), "ghost".into()], &people);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name, "lev");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("ghost"));
    }
}
```

- [ ] **Step 6: Create an empty-for-now `finish.rs` so the module compiles**

Create `src/bin/issue/review/finish.rs`:

```rust
// The finish flow is implemented in Task 4.
```

Note: `mod.rs` declares `pub(crate) mod finish;` — an empty file is a valid module. Task 4 fills it in and wires the subcommand; nothing references `finish::run` until then, so there is no dead code.

- [ ] **Step 7: Rewire `main.rs` — `review` becomes a container with `request`**

In `src/bin/issue/main.rs`, replace the `Review { … }` variant (lines ~112-118 — the `body`/`to`/`reviewer`/`base`/`pr_title`/`pr_body`/`no_push` block) with:

```rust
    /// Request or finish a review.
    Review {
        #[command(subcommand)]
        cmd: ReviewCmd,
    },
```

Add this enum just below the `Cmd` enum (after its closing `}`):

```rust
#[derive(Subcommand)]
enum ReviewCmd {
    /// Push, open/reuse the PR, request review, and Slack the reviewers.
    Request {
        /// Slack body; fills the `review_request` template's `{{ input }}`.
        body: Option<String>,
        /// Recipient: a `[people]` alias or `#channel`. Repeatable.
        #[arg(long = "to")]
        to: Vec<String>,
        #[arg(long)]
        base: Option<String>,
        #[arg(long = "pr-title")]
        pr_title: Option<String>,
        #[arg(long = "pr-body")]
        pr_body: Option<String>,
        #[arg(long = "no-push")]
        no_push: bool,
        /// Override a declared template variable: `--arg key=value`. Repeatable.
        #[arg(long = "arg")]
        args: Vec<String>,
    },
}
```

Replace the `Some(Cmd::Review { … }) => review::run(review::ReviewArgs { … })` dispatch arm (lines ~207-227) with:

```rust
        Some(Cmd::Review { cmd }) => match cmd {
            ReviewCmd::Request {
                body,
                to,
                base,
                pr_title,
                pr_body,
                no_push,
                args,
            } => review::request::run(review::request::Args {
                body,
                to,
                base,
                pr_title,
                pr_body,
                no_push,
                args,
                dir: cli.dir,
                config: cli.config,
            }),
        },
```

- [ ] **Step 8: Run the gate**

Run: `cargo test -p devkit --bin issue 2>&1 | tail -20 && cargo clippy --bin issue -- -D warnings 2>&1 | tail -5`
Expected: all `review::tests::*` and `review::request::tests::*` PASS; zero warnings.

> If clippy flags `single-variant enum` on `ReviewCmd`, ignore — Task 4 adds the `Finish` variant. If it errors as `-D warnings`, add `#[allow(clippy::large_enum_variant)]` is NOT needed; the relevant lint is none for a one-variant subcommand enum. Proceed; Task 4 removes any doubt.

- [ ] **Step 9: Commit**

```bash
git add src/bin/issue/review src/bin/issue/main.rs
git commit -m "feat(issue): rework review request with multi-target --to and --arg"
```

---

## Task 4: Implement `issue review finish`

**Files:**
- Modify: `src/bin/issue/review/finish.rs`
- Modify: `src/bin/issue/main.rs` (`ReviewCmd::Finish` variant + dispatch)

- [ ] **Step 1: Write the finish unit tests**

Replace the contents of `src/bin/issue/review/finish.rs` with the test module first (implementation follows in Step 3). For now, put this at the **bottom** of the file you will write in Step 3; here is the test block to include:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use devkit_ports::config::Person;
    use std::collections::HashMap;

    #[test]
    fn resolve_pr_prefers_flag_then_branch() {
        assert_eq!(resolve_pr(Some(7), Some(9)).unwrap(), 9);
        assert_eq!(resolve_pr(Some(7), None).unwrap(), 7);
        assert_eq!(resolve_pr(None, Some(9)).unwrap(), 9);
        assert!(resolve_pr(None, None).is_err());
    }

    #[test]
    fn author_target_reverse_looks_up_or_errors() {
        let people = HashMap::from([(
            "lev".to_string(),
            Person { slack: "U_LEV".into(), github: Some("LevValle".into()) },
        )]);
        let t = author_target("levvalle", &people).unwrap();
        assert_eq!(t.name, "lev");
        assert_eq!(t.channel, "U_LEV");
        assert!(author_target("ghost", &people).is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p devkit --bin issue review::finish 2>&1 | head -20`
Expected: FAIL — `resolve_pr` / `author_target` not found.

- [ ] **Step 3: Write `finish.rs`**

Replace the whole `src/bin/issue/review/finish.rs` with (test module from Step 1 appended at the end):

```rust
use anyhow::{Context, Result};
use devkit_common::cmd::{gh_json, git};
use devkit_ports::config::Person;
use serde::Deserialize;
use std::collections::HashMap;

use super::{
    Target, deliver, parse_args, person_by_login, resolve_target, target_from_person, with_fields,
};

pub struct Args {
    pub body: Option<String>,
    pub to: Vec<String>,
    pub pr: Option<u64>,
    pub args: Vec<String>,
    pub dir: Option<String>,
    pub config: Option<String>,
}

#[derive(Deserialize)]
struct PrLite {
    number: u64,
}

#[derive(Deserialize)]
struct PrFull {
    url: String,
    title: String,
    author: Author,
}

#[derive(Deserialize)]
struct Author {
    #[serde(default)]
    login: Option<String>,
}

/// Choose the PR number: explicit `--pr` wins, else the worktree branch's PR.
pub(crate) fn resolve_pr(branch_pr: Option<u64>, pr_flag: Option<u64>) -> Result<u64> {
    pr_flag
        .or(branch_pr)
        .context("no PR for the current branch; pass --pr <number>")
}

/// Build the PR-author Slack target via reverse lookup.
pub(crate) fn author_target(login: &str, people: &HashMap<String, Person>) -> Result<Target> {
    person_by_login(login, people)
        .map(|(alias, p)| target_from_person(alias, p))
        .with_context(|| format!("PR author `{login}` has no [people] alias; pass --to"))
}

pub fn run(args: Args) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded = devkit_ports::load::load(
        args.config.as_deref().map(std::path::Path::new),
        std::path::Path::new(&start),
    )?;
    let people = &loaded.config.people;
    let tmpls = &loaded.config.templates;

    let mut vars = tmpls.variables.clone();
    vars.extend(parse_args(&args.args, &tmpls.variables)?);

    // PR from the current branch (best effort), unless --pr is given.
    let branch_pr = git(&["rev-parse", "--abbrev-ref", "HEAD"], &start)
        .ok()
        .and_then(|branch| {
            gh_json::<Vec<PrLite>>(
                &["pr", "list", "--head", branch.trim(), "--state", "all", "--json", "number", "--limit", "1"],
                &start,
            )
            .ok()
            .and_then(|v| v.into_iter().next())
            .map(|p| p.number)
        });
    let number = resolve_pr(branch_pr, args.pr)?;

    let view: PrFull = gh_json(
        &["pr", "view", &number.to_string(), "--json", "url,title,author"],
        &start,
    )?;
    let author_login = view.author.login.clone();

    let targets: Vec<Target> = if args.to.is_empty() {
        let login = author_login
            .clone()
            .context("PR has no author login; pass --to")?;
        vec![author_target(&login, people)?]
    } else {
        args.to
            .iter()
            .map(|v| resolve_target(v, people))
            .collect::<Result<_>>()?
    };

    let base = with_fields(
        &serde_json::json!({}),
        &[
            ("pr_url", serde_json::json!(view.url)),
            ("pr_title", serde_json::json!(view.title)),
            ("author", serde_json::json!(author_login.unwrap_or_default())),
            ("input", serde_json::json!(args.body.clone().unwrap_or_default())),
        ],
    );
    deliver(tmpls.review_finish(), "review_finish", &base, &vars, None, &targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_ports::config::Person;
    use std::collections::HashMap;

    #[test]
    fn resolve_pr_prefers_flag_then_branch() {
        assert_eq!(resolve_pr(Some(7), Some(9)).unwrap(), 9);
        assert_eq!(resolve_pr(Some(7), None).unwrap(), 7);
        assert_eq!(resolve_pr(None, Some(9)).unwrap(), 9);
        assert!(resolve_pr(None, None).is_err());
    }

    #[test]
    fn author_target_reverse_looks_up_or_errors() {
        let people = HashMap::from([(
            "lev".to_string(),
            Person { slack: "U_LEV".into(), github: Some("LevValle".into()) },
        )]);
        let t = author_target("levvalle", &people).unwrap();
        assert_eq!(t.name, "lev");
        assert_eq!(t.channel, "U_LEV");
        assert!(author_target("ghost", &people).is_err());
    }
}
```

- [ ] **Step 4: Add the `Finish` subcommand + dispatch in `main.rs`**

In `src/bin/issue/main.rs`, add a variant to `ReviewCmd` (after `Request { … }`):

```rust
    /// Announce over Slack that you finished reviewing; notify the author or --to.
    Finish {
        /// Slack body; fills the `review_finish` template's `{{ input }}`.
        body: Option<String>,
        /// Recipient: a `[people]` alias or `#channel`. Repeatable. Defaults to the PR author.
        #[arg(long = "to")]
        to: Vec<String>,
        /// PR number; required when not run inside the PR's worktree.
        #[arg(long)]
        pr: Option<u64>,
        /// Override a declared template variable: `--arg key=value`. Repeatable.
        #[arg(long = "arg")]
        args: Vec<String>,
    },
```

Add the dispatch arm inside `match cmd { … }` (after the `Request` arm):

```rust
            ReviewCmd::Finish { body, to, pr, args } => review::finish::run(review::finish::Args {
                body,
                to,
                pr,
                args,
                dir: cli.dir,
                config: cli.config,
            }),
```

- [ ] **Step 5: Run the gate**

Run: `cargo test -p devkit --bin issue 2>&1 | tail -20 && cargo clippy --bin issue -- -D warnings 2>&1 | tail -5`
Expected: all review tests PASS; zero warnings.

- [ ] **Step 6: Commit**

```bash
git add src/bin/issue/review/finish.rs src/bin/issue/main.rs
git commit -m "feat(issue): add review finish to Slack the PR author"
```

---

## Task 5: Docs + full workspace gate

**Files:**
- Modify: `src/bin/issue/main.rs` (the `#[command(about = …)]` text already says "review"; no change needed unless stale)
- Modify: `README.md` (review section)

- [ ] **Step 1: Update README**

Find the `issue review` section in `README.md` (search for "issue review"). Replace its synopsis/examples with the two subcommands:

````markdown
### `issue review request`

Push the branch, open or reuse the PR, request review on GitHub, and Slack the reviewers.

```sh
issue review request "ready for a look" --to igor
issue review request --to igor --to '#eng' --arg team=infra   # body optional; channel + people
issue review request                                          # re-ping the PR's existing reviewers
```

- `--to <alias|#channel>` (repeatable). People are added as GitHub reviewers (those with a `github` handle) and Slacked; `#channels` are Slack-only. Omit `--to` to re-request and Slack the PR's current human reviewers.
- `--base`, `--pr-title`, `--pr-body`, `--no-push` as before.
- `--arg key=value` (repeatable) overrides a variable declared in `[templates.variables]`.

### `issue review finish`

Announce over Slack that you finished reviewing. Posts nothing to GitHub.

```sh
issue review finish "LGTM, merging after CI"          # inside the PR's worktree → notifies the author
issue review finish --pr 1234 --to lev                # from anywhere, explicit PR + recipient
```

- Resolves the PR from the current branch, or `--pr <number>` when run outside a worktree.
- Defaults to notifying the PR author; `--to` overrides (repeatable, people or `#channels`).
- `--arg key=value` as above.

Templates: `review_request` and `review_finish` under `[templates]`. Per-recipient render fields: `name` (alias or channel), `slack_id` (user id, empty for channels), plus `pr_url`, `pr_title`, `input` (and `author` for finish).
````

If `README.md` has no `issue review` section, add the above under the `issue` CLI heading.

- [ ] **Step 2: Run the full workspace gate**

Run: `cargo fmt --all && cargo test --workspace 2>&1 | tail -20 && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10`
Expected: all tests green; zero clippy warnings; fmt clean.

- [ ] **Step 3: Manual smoke (no Slack token) — request intent printing**

Run (in any issue worktree, dry of side effects by using `--no-push` and an alias that exists in your `devkit.toml`):
`SLACK_TOKEN= issue review request "smoke" --to <your-alias> --no-push 2>&1 | tail -20`
Expected: with no token, a JSON intent is printed per target (`to`, `channel`, `text`) and the PR is created/reused. (Skip if you don't want to touch a real PR; the unit tests already cover the decision logic.)

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: document issue review request and finish"
```

---

## Self-review notes (already applied)

- **Spec coverage:** subcommand container (T3/T4), `--to` people+channels (T3 `resolve_target`), reverse lookup (T3 `person_by_login`, T4 `author_target`), request reviewer add + no-`--to` derivation + zero-reviewer error (T3), finish PR resolution + author default (T4), template rename + `review_finish` + per-recipient `name`/`slack_id` (T1, T3 `recipient_ctx`), `--arg` allowlist (T3 `parse_args`), module split (T2), docs (T5). All covered.
- **Type consistency:** `Target { channel, name, slack_id, github }` used identically across mod/request/finish; `deliver`/`recipient_ctx`/`with_fields`/`base_ctx`/`render_review` signatures match every caller; gh structs use `#[serde(rename_all = "camelCase")]` only where the JSON is camelCase (`reviewRequests`).
```
