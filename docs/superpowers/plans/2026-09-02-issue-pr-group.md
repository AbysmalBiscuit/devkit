# `issue pr` command group implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the PR lifecycle into an `issue pr` command group and make draft PR state visible everywhere devkit reads a PR.

**Architecture:** Draft state is plumbed bottom-up first (`PrBrief` → `PrStatus` → render), so every later task can read it. The CLI surface then grows `issue pr` with `status` and `checkout` as pure moves, gains `create` and `ready`, and only the final task changes `issue review request`'s contract. Each task compiles and passes the full gate on its own.

**Tech Stack:** Rust 2024, clap derive, anyhow, serde/serde_json, schemars, `gh` CLI, GitHub GraphQL + REST.

**Spec:** `docs/superpowers/specs/2026-09-02-issue-pr-group-design.md`

## Global constraints

- The merge gate is `cargo nextest run --workspace --no-fail-fast`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all`. All three must pass before every commit.
- Commits follow Conventional Commits: `type(scope): description`, imperative, lowercase after the colon, no trailing period, subject at most 50 characters.
- Every commit ends with the trailer `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.
- Feature work happens in a worktree under `../devkit-worktrees/`, never by checking out a branch in the primary clone.
- Test scratch directories come from `tempfile::tempdir()`, never a hand-built path under `std::env::temp_dir()`.
- `StateKind` and `Role` style: match enums exhaustively, no `_ =>` catch-all arms.
- Repository-scoped `gh` calls go through `cmd::gh_json_in` / `cmd::gh_capture`, which append `--repo`. Never shell out to `gh` directly.
- Comments state a non-obvious *why*. No change-relative phrasing (`this PR`, `now we`, `used to`), no issue references, no TDD narration.

---

## Task 1: Carry `is_draft` on `PrBrief`

**Files:**
- Modify: `crates/devkit-common/src/github.rs` (`PrBrief` at 548, `parse_brief` at 626, `prs_by_number_query` at 712, `head_query` at 824, `parse_pr_node` at 835)
- Modify: `crates/devkit-issue/src/status.rs` (`heads_query` at 316)
- Modify: `src/bin/devkit/issue/review/request.rs` (`PrFlat` at 34, `existing_pr` at 122)

**Interfaces:**
- Consumes: nothing.
- Produces: `github::PrBrief { number: u64, state: String, url: String, head_ref_name: String, head_ref_oid: String, head_repo_owner: Option<String>, is_draft: bool }`. Every construction site sets `is_draft`. A payload missing the key yields `false`.

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-common/src/github.rs`, inside `mod tests`:

```rust
    #[test]
    fn parse_pr_node_reads_is_draft() {
        let n = json!({
            "number": 7, "state": "OPEN", "url": "u7",
            "headRefName": "feat/x", "headRefOid": "abc123",
            "isDraft": true
        });
        assert!(parse_pr_node(&n).unwrap().is_draft);
    }

    #[test]
    fn a_node_without_is_draft_is_not_a_draft() {
        let n = json!({
            "number": 7, "state": "OPEN", "url": "u7",
            "headRefName": "feat/x", "headRefOid": "abc123"
        });
        assert!(!parse_pr_node(&n).unwrap().is_draft);
    }

    #[test]
    fn parse_brief_reads_the_rest_draft_key() {
        let v = json!({
            "number": 42, "state": "open",
            "html_url": "https://github.com/a/b/pull/42",
            "head": { "ref": "you/eng-1-foo" },
            "draft": true
        });
        assert!(parse_brief(&v).unwrap().is_draft);
    }

    #[test]
    fn every_pr_query_selects_is_draft() {
        assert!(prs_by_number_query(&targets()).contains("isDraft"));
        assert!(head_query("o/r", "feat/x").contains("isDraft"));
    }
```

In `crates/devkit-issue/src/status.rs`, inside `mod tests`:

```rust
    #[test]
    fn heads_query_selects_is_draft() {
        let q = heads_query("o/r", &["feat/a".into()]);
        assert!(q.contains("isDraft"), "heads_query must select isDraft: {q}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p devkit-common -p devkit-issue -E 'test(is_draft) or test(draft_key)'`

Expected: FAIL. The `parse_*` tests fail to compile with "struct `PrBrief` has no field named `is_draft`"; the query tests fail the assertion.

- [ ] **Step 3: Add the field and read it**

In `crates/devkit-common/src/github.rs`, add to `PrBrief` after `head_repo_owner`:

```rust
    /// Whether the PR is a draft. GitHub reports a draft's `state` as `OPEN`,
    /// so this is the only thing separating "still being written" from "waiting
    /// on a reviewer".
    pub is_draft: bool,
```

In `parse_pr_node`, add the field. A missing key is `false` rather than a parse failure, matching how `headRefOid` is already treated:

```rust
        is_draft: n["isDraft"].as_bool().unwrap_or(false),
```

In `parse_brief`, read the REST spelling:

```rust
        is_draft: v.get("draft").and_then(|d| d.as_bool()).unwrap_or(false),
```

In `prs_by_number_query`, extend `fields`:

```rust
    let fields = "number state url headRefName headRefOid isDraft \
                  headRepositoryOwner { login }";
```

In `head_query`, extend the node selection:

```rust
               nodes {{ number state url headRefName headRefOid isDraft
                        headRepositoryOwner {{ login }} }}
```

In `crates/devkit-issue/src/status.rs`, extend `heads_query`'s `fields`:

```rust
    let fields = "totalCount nodes { number state url headRefName headRefOid isDraft \
                  headRepositoryOwner { login } }";
```

- [ ] **Step 4: Fix the remaining construction sites the compiler names**

`src/bin/devkit/issue/review/request.rs` builds a `PrBrief` from `gh pr list`, the path taken whenever `github::token()` is `None`. Add the field to `PrFlat`:

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrFlat {
    number: u64,
    state: String,
    url: String,
    head_ref_name: String,
    head_ref_oid: String,
    #[serde(default)]
    is_draft: bool,
}
```

In `existing_pr`, request the field and pass it through:

```rust
            "number,state,url,headRefName,headRefOid,isDraft",
```

```rust
    Ok(v.into_iter().next().map(|p| github::PrBrief {
        number: p.number,
        state: p.state,
        url: p.url,
        head_ref_name: p.head_ref_name,
        head_ref_oid: p.head_ref_oid,
        head_repo_owner: None,
        is_draft: p.is_draft,
    }))
```

Run `cargo build --workspace` and set `is_draft` on every other site the compiler reports. `src/bin/devkit/issue/review/finish.rs:314` (`brief`, `brief_at`) builds `PrBrief` literals in tests, as do the fixtures in `github.rs`.

- [ ] **Step 5: Run the full gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-fail-fast
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git diff --stat        # review every file the compiler forced
git add -u
git commit -m "$(cat <<'EOF'
feat(github): carry is_draft on PrBrief

GitHub reports a draft's state as OPEN, so nothing below `issue prs`
could tell a draft from a PR waiting on a reviewer.

Select isDraft in every query that builds a PrBrief, including the
`gh pr list` fallback taken when no token resolves. A payload missing
the key parses as not-draft, the way headRefOid is already treated.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Tag `PrStatus::Unique` with `is_draft`

**Files:**
- Modify: `crates/devkit-issue/src/status.rs` (`PrStatus` at 15, `pr_status_of` at 204)
- Modify: `src/bin/devkit/issue/info_cache.rs` (`CachedPr` at 9)
- Modify: `src/bin/devkit/issue/info.rs` (`apply_cached_pr` at 312, cache write at 114)

**Interfaces:**
- Consumes: `PrBrief::is_draft` from Task 1.
- Produces: `PrStatus::Unique { number: u64, state: String, url: String, is_draft: bool }`, serialized inside the `pr` field of `IssueWorktree`. `state_label()` still returns `"OPEN"` for a draft. `info_cache::CachedPr { number, state, url, is_draft }` with `is_draft` defaulting when absent.

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-issue/src/status.rs`, inside `mod tests`:

```rust
    #[test]
    fn pr_status_of_carries_the_draft_flag() {
        let pr = github::PrBrief {
            number: 7,
            state: "OPEN".into(),
            url: "u7".into(),
            head_ref_name: "feat/x".into(),
            head_ref_oid: "abc123".into(),
            head_repo_owner: None,
            is_draft: true,
        };
        let status = pr_status_of(&github::HeadLookup::Unique(pr));
        assert_eq!(
            status,
            PrStatus::Unique {
                number: 7,
                state: "OPEN".into(),
                url: "u7".into(),
                is_draft: true,
            }
        );
    }

    #[test]
    fn a_draft_still_labels_as_open_for_serialized_consumers() {
        let status = PrStatus::Unique {
            number: 7,
            state: "OPEN".into(),
            url: "u7".into(),
            is_draft: true,
        };
        assert_eq!(status.state_label(), "OPEN");
    }
```

In `src/bin/devkit/issue/info_cache.rs`, inside `mod tests`:

```rust
    #[test]
    fn a_cache_without_is_draft_reads_as_not_draft() {
        let wt = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(wt.path().join(".devkit")).unwrap();
        std::fs::write(
            wt.path().join(".devkit").join("pr.json"),
            br#"{"number":1,"state":"OPEN","url":"u"}"#,
        )
        .unwrap();
        let got = read(wt.path()).expect("a cache predating is_draft still reads");
        assert!(!got.is_draft);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p devkit-issue -E 'test(draft)'` and `cargo nextest run --bin devkit -E 'test(is_draft)'`

Expected: FAIL to compile with "variant `PrStatus::Unique` has no field named `is_draft`" and "struct `CachedPr` has no field named `is_draft`".

- [ ] **Step 3: Add the field to `PrStatus::Unique`**

In `crates/devkit-issue/src/status.rs`:

```rust
    Unique {
        number: u64,
        state: String,
        url: String,
        /// A draft's `state` is `OPEN`, so `state_label` cannot express this and
        /// deliberately does not try: consumers read `pr_state` and a changed
        /// string there breaks them.
        is_draft: bool,
    },
```

In `pr_status_of`:

```rust
        github::HeadLookup::Unique(pr) => PrStatus::Unique {
            number: pr.number,
            state: pr.state.clone(),
            url: pr.url.clone(),
            is_draft: pr.is_draft,
        },
```

Leave `state_label()` alone. Its doc comment already records that the string is a consumer contract.

- [ ] **Step 4: Add the field to the cache**

In `src/bin/devkit/issue/info_cache.rs`:

```rust
pub struct CachedPr {
    pub number: u64,
    pub state: String,
    pub url: String,
    /// Defaulted because `read` treats a parse failure as a cache miss, so a
    /// required field would silently invalidate every cache written earlier.
    #[serde(default)]
    pub is_draft: bool,
}
```

In `src/bin/devkit/issue/info.rs`, `apply_cached_pr`:

```rust
    row.pr = PrStatus::Unique {
        number: pr.number,
        state: pr.state,
        url: pr.url,
        is_draft: pr.is_draft,
    };
```

And the cache write near line 114:

```rust
        if let PrStatus::Unique {
            number,
            state,
            url,
            is_draft,
        } = &row.pr
        {
            let _ = crate::issue::info_cache::write(
                Path::new(&row.worktree),
                &crate::issue::info_cache::CachedPr {
                    number: *number,
                    state: state.clone(),
                    url: url.clone(),
                    is_draft: *is_draft,
                },
            );
        }
```

Run `cargo build --workspace` and fix every remaining construction site the compiler names: `src/bin/devkit/issue/select.rs:71`, `src/bin/devkit/issue/status.rs:358`, and `src/bin/devkit/issue/triage.rs:145,193` each build `PrStatus::Unique` literals.

- [ ] **Step 5: Run the full gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-fail-fast
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git diff --stat        # review every file the compiler forced
git add -u
git commit -m "$(cat <<'EOF'
feat(issue): tag PrStatus::Unique with is_draft

Carries the flag from PrBrief into the report row, so the triage
render and MCP consumers can see it. state_label keeps returning OPEN
for a draft: its string is a consumer contract and a draft's state
genuinely is OPEN.

CachedPr defaults the field, since info_cache::read treats a parse
failure as a miss and every cache written earlier lacks the key.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Render drafts in triage and the reviewer queue

**Files:**
- Modify: `src/bin/devkit/issue/triage.rs` (`pr_label` at 5, `pr_cell` at 50)
- Modify: `crates/devkit-issue/src/prs.rs` (`reviewer_state` at 557)
- Modify: `docs/agents.md`

**Interfaces:**
- Consumes: `PrStatus::Unique { is_draft }` from Task 2, `PrNode::is_draft` (already present at `prs.rs:205`).
- Produces: nothing later tasks depend on. `pr_label` renders `DRAFT #n`; `reviewer_state` returns the action `"draft"`.

**Note:** `pr_label` and `pr_cell` match `Unique { .. }` with a rest pattern, so the compiler will not flag them. They are found by grep, not by rustc.

- [ ] **Step 1: Write the failing tests**

`src/bin/devkit/issue/triage.rs:123` already has a `mod tests` carrying a
`row_with(pr: PrStatus) -> IssueWorktree` helper. Add these two tests inside it,
without redeclaring the module or the helper:

```rust
    #[test]
    fn a_draft_labels_as_draft() {
        let row = row_with(PrStatus::Unique {
            number: 7,
            state: "OPEN".into(),
            url: "u7".into(),
            is_draft: true,
        });
        assert_eq!(pr_label(&row), "DRAFT #7");
    }

    #[test]
    fn a_ready_pr_keeps_its_state_label() {
        let row = row_with(PrStatus::Unique {
            number: 7,
            state: "OPEN".into(),
            url: "u7".into(),
            is_draft: false,
        });
        assert_eq!(pr_label(&row), "OPEN #7");
    }
```

In `crates/devkit-issue/src/prs.rs`, inside `mod tests`, add a case beside the
existing `reviewer_state` tests. That module does not import the `json!` macro,
so qualify it, and build the node through the existing `node` helper at
`prs.rs:958`:

```rust
    #[test]
    fn a_draft_is_not_review_needed() {
        let pr = node(serde_json::json!({
            "number": 1, "url": "u", "headRefName": "h", "isDraft": true,
            "reviewDecision": null, "mergeable": "MERGEABLE",
            "author": { "login": "someone" },
            "reviewRequests": { "nodes": [
                { "requestedReviewer": { "login": "me" } }
            ] }
        }));
        let (_, action) = reviewer_state(&pr, "me");
        assert_eq!(action, "draft");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --workspace -E 'test(draft)'`

Expected: FAIL. `pr_label` returns `"OPEN #7"` where `"DRAFT #7"` is expected; `reviewer_state` returns `"REVIEW NEEDED"`.

- [ ] **Step 3: Render the draft label**

In `src/bin/devkit/issue/triage.rs`:

```rust
fn pr_label(row: &IssueWorktree) -> String {
    match &row.pr {
        PrStatus::None => "no PR".into(),
        PrStatus::Unique {
            number,
            state,
            is_draft,
            ..
        } => {
            let word = if *is_draft { "DRAFT" } else { state };
            format!("{word} #{number}")
        }
        PrStatus::Ambiguous { candidates } => format!("ambiguous ({})", candidates.len()),
        PrStatus::Unknown { .. } => "unknown".into(),
    }
}
```

- [ ] **Step 4: Colour a draft as passive**

In `pr_cell`, check the flag before matching `state_label()`, since a draft's label is `OPEN` and would otherwise take the yellow arm:

```rust
pub(crate) fn pr_cell(row: &IssueWorktree) -> String {
    let label = pr_label(row);
    let drafting = matches!(row.pr, PrStatus::Unique { is_draft: true, .. });
    let colored = if drafting {
        ui::dim(&label)
    } else {
        match row.pr.state_label() {
            "MERGED" => ui::green(&label),
            "OPEN" => ui::yellow(&label),
            "CLOSED" => ui::red(&label),
            _ => ui::dim(&label), // NO_PR | AMBIGUOUS | UNKNOWN
        }
    };
    match row.pr.url() {
        Some(u) => ui::link(&colored, u),
        None => colored,
    }
}
```

- [ ] **Step 5: Keep a draft out of the reviewer's queue**

In `crates/devkit-issue/src/prs.rs`, at the top of `reviewer_state`:

```rust
fn reviewer_state(pr: &PrNode, me: &str) -> (String, String) {
    let vote = my_vote(pr, me);
    let vote_label = match vote {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "changes",
        "COMMENTED" => "commented",
        _ => "-",
    }
    .to_string();
    // A draft is the author's to finish, so it is passive for a reviewer even
    // while a review request sits on it.
    if pr.is_draft {
        return (vote_label, "draft".into());
    }
    let requested = pr
        .review_requests
        .nodes
        .iter()
        .filter_map(|r| r.requested_reviewer.as_ref())
        .any(|rr| rr.login == me);
    let action = if requested {
        "REVIEW NEEDED"
    } else {
        match vote {
            "APPROVED" => "done (approved)",
            "CHANGES_REQUESTED" => "awaiting author fixes",
            "COMMENTED" => "commented; decide",
            _ => "REVIEW NEEDED",
        }
    }
    .to_string();
    (vote_label, action)
}
```

`paint_action` (`src/bin/devkit/issue/prs.rs:13`) already falls through to `ui::dim` for an unrecognised verb, so `"draft"` renders dim with no renderer change.

- [ ] **Step 6: Document the new action value**

In `docs/agents.md`, note that a `reviews` row's ACTION can now read `draft`, a
value MCP consumers reading that table have not seen before.

- [ ] **Step 7: Run the full gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-fail-fast
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add -u
git commit -m "$(cat <<'EOF'
feat(issue): render drafts in triage and the reviewer queue

The PR column showed a draft as OPEN in yellow, indistinguishable
from one waiting on a reviewer, and a draft carrying a review request
sat in that reviewer's queue as REVIEW NEEDED.

Label a draft DRAFT #n and dim it, and return the passive `draft`
action for a reviewer, matching what the author's own section already
reports.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Add `defaults.pr_create_state`

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (`Defaults` at 291, its `Default` impl, `require_pr_reviewer` doc at 321)
- Modify: `schema/devkit-config.json` (regenerated)
- Modify: `docs/configuration.md`

**Interfaces:**
- Consumes: nothing.
- Produces: `devkit_config::PrCreateState` with variants `Draft` and `Ready`, `Display` rendering `"draft"` / `"ready"`, and `Defaults::pr_create_state: PrCreateState` defaulting to `Draft`.

**Note:** `devkit-config` has no clap dependency (its deps are anyhow, schemars, serde, toml), so this enum derives no `ValueEnum`. The CLI surface is `--draft` / `--ready`, added in Task 6.

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-config/src/lib.rs`, inside `mod tests`:

```rust
    #[test]
    fn pr_create_state_defaults_to_draft() {
        let c = Config::parse(SAMPLE).unwrap();
        assert_eq!(c.defaults.pr_create_state, PrCreateState::Draft);
    }

    #[test]
    fn pr_create_state_parses_ready() {
        let src = r#"
[defaults]
worktree_root = "/w"
branch_prefix = "you/"
baseline_ref = "origin/main"
baseline_path = "/b"
pr_create_state = "ready"
"#;
        let c = Config::parse(src).unwrap();
        assert_eq!(c.defaults.pr_create_state, PrCreateState::Ready);
        assert_eq!(c.defaults.pr_create_state.to_string(), "ready");
    }

    #[test]
    fn an_unknown_pr_create_state_is_an_error() {
        let src = r#"
[defaults]
worktree_root = "/w"
branch_prefix = "you/"
baseline_ref = "origin/main"
baseline_path = "/b"
pr_create_state = "wip"
"#;
        assert!(Config::parse(src).is_err());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p devkit-config -E 'test(pr_create_state)'`

Expected: FAIL to compile with "cannot find type `PrCreateState` in this scope".

- [ ] **Step 3: Add the enum and the key**

In `crates/devkit-config/src/lib.rs`, beside the other config types:

```rust
/// The state a PR is opened in by `issue pr create` when neither `--draft` nor
/// `--ready` is passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PrCreateState {
    /// Opened as a draft. Reviewers are notified only once it is marked ready.
    #[default]
    Draft,
    /// Opened ready for review.
    Ready,
}

impl std::fmt::Display for PrCreateState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Draft => f.write_str("draft"),
            Self::Ready => f.write_str("ready"),
        }
    }
}
```

Add the key to `Defaults`, after `pr_base`:

```rust
    /// State `issue pr create` opens a PR in when neither `--draft` nor
    /// `--ready` is given. Draft by default, so a new PR never lands in
    /// anyone's review queue until it is marked ready.
    #[serde(default)]
    pub pr_create_state: PrCreateState,
```

Update the `require_pr_reviewer` doc comment, which reaches `schema/devkit-config.json` verbatim:

```rust
    /// Refuse any run that would leave a PR ready for review with no human
    /// GitHub reviewer: `issue pr create --ready`, `issue pr ready`, and the
    /// draft-to-ready flip in `issue review request`. Off by default.
    #[serde(default)]
    pub require_pr_reviewer: bool,
```

Add `pr_create_state: PrCreateState::default()` to the `Defaults` `Default` impl.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p devkit-config -E 'test(pr_create_state)'`

Expected: PASS.

- [ ] **Step 5: Regenerate the committed schema**

```bash
DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema
git diff --stat schema/devkit-config.json
```

Expected: `schema/devkit-config.json` gains `pr_create_state` and carries the new `require_pr_reviewer` description.

- [ ] **Step 6: Document the key**

In `docs/configuration.md`, in the `[defaults]` table, add a row for `pr_create_state` describing the two values and the draft default, and update the `require_pr_reviewer` row to name the ready transition rather than PR creation.

- [ ] **Step 7: Run the full gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-fail-fast
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/devkit-config/src/lib.rs schema/devkit-config.json docs/configuration.md
git commit -m "$(cat <<'EOF'
feat(config): add defaults.pr_create_state

Names the state `issue pr create` opens a PR in without an explicit
flag. Draft by default: a new PR should not reach a review queue
before its author says so.

A two-value enum rather than a bool, because `pr_draft = false` is a
double negative for "open it ready". No ValueEnum derive, since
devkit-config carries no clap dependency and the CLI spells this as
--draft / --ready.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Add the `pr` group with `status` and `checkout`

**Files:**
- Modify: `src/bin/devkit/issue/mod.rs` (`Cmd` enum at 61, dispatch in `run` at 320)
- Modify: `src/bin/devkit/main.rs:115` (the `issue` about line)
- Modify: `tests/shim_dispatch.rs:228`
- Modify: `docs/commands.md`, `docs/agents.md`, `AGENTS.md`, `skills/using-devkit/references/issues.md`

**Interfaces:**
- Consumes: nothing.
- Produces: `Cmd::Pr { cmd: Option<PrCmd> }` and `enum PrCmd { Status { selector, json, cache_only }, Checkout { target, worktree_path, setup, apps } }`. Task 6 adds `PrCmd::Create`; Task 7 adds `PrCmd::Ready`.

**Note:** This task moves commands without changing behavior. `issue info` and `issue checkout-pr` stay reachable as hidden variants.

- [ ] **Step 1: Write the failing test**

In `tests/shim_dispatch.rs`, replace the assertion at line 228:

```rust
    assert!(
        text.contains("Pull-request lifecycle"),
        "shim should list issue's own subcommands: {text}"
    );
```

`text.contains("pr")` would be a vacuous assertion: `prs` and `Prepare` both
satisfy it. Match the group's description instead.

Add a test asserting the old names still parse but no longer advertise
themselves. This file has no `issue_cmd` helper; every test spawns the shim the
way `issue_shim_parses_issue_arguments` does at `shim_dispatch.rs:219`:

```rust
#[test]
fn hidden_aliases_stay_reachable_but_unlisted() {
    let (_dir, link) = shimtest::linked("issue");
    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .arg("--help")
        .output()
        .expect("spawn issue shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !text.contains("checkout-pr"),
        "checkout-pr should be hidden from help: {text}"
    );

    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .args(["info", "--help"])
        .output()
        .expect("spawn issue shim");
    assert!(
        out.status.success(),
        "issue info must still parse: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run --test shim_dispatch`

Expected: FAIL. `checkout-pr` is still listed in help.

- [ ] **Step 3: Add the `pr` group**

In `src/bin/devkit/issue/mod.rs`, add to `Cmd`:

```rust
    /// Pull-request lifecycle for this worktree.
    Pr {
        #[command(subcommand)]
        cmd: Option<PrCmd>,
    },
```

And the group's own enum, mirroring `IssueCli.cmd: Option<Cmd>` so bare `issue pr` means status:

```rust
#[derive(Subcommand)]
pub(crate) enum PrCmd {
    /// Show one worktree's PR + issue id (current worktree, or a SELECTOR).
    Status {
        /// Issue id, branch, worktree basename, or path. Defaults to cwd.
        selector: Option<String>,
        /// Emit the worktree as one JSON object instead of a table.
        #[arg(long)]
        json: bool,
        /// Skip the network: take the PR number from the worktree's cache and
        /// leave the issue state blank.
        #[arg(long = "cache-only")]
        cache_only: bool,
    },
    /// Check out an existing PR (by number, issue id, or URL) into a new worktree.
    Checkout {
        /// `#3340` | `3340` | `PREFIX-3340` | github PR URL | tracker issue URL.
        target: String,
        /// Worktree path; defaults to the config-resolved placement.
        worktree_path: Option<String>,
        /// Also write each app's prep files and run its setup commands.
        #[arg(long)]
        setup: bool,
        /// Apps to bootstrap under --setup. Omit for a worktree with no per-app
        /// setup.
        #[arg(long, value_delimiter = ',')]
        apps: Vec<String>,
    },
}
```

- [ ] **Step 4: Hide the old variants and dispatch both spellings**

Mark the existing `Cmd::Info` and `Cmd::CheckoutPr` variants hidden, following the pattern at `src/bin/devkit/locks.rs:85`:

```rust
    /// Show one worktree's PR + issue id (current worktree, or a SELECTOR).
    #[command(hide = true)]
    Info {
```

```rust
    /// Check out an existing PR (by number, issue id, or URL) into a new worktree.
    #[command(hide = true)]
    CheckoutPr {
```

In `run`, route both spellings into the same functions. Normalise the group first so there is one call site per action:

```rust
        Some(Cmd::Pr { cmd }) => {
            let cmd = cmd.unwrap_or(PrCmd::Status {
                selector: None,
                json: false,
                cache_only: false,
            });
            match cmd {
                PrCmd::Status {
                    selector,
                    json,
                    cache_only,
                } => info::run(
                    &start(&cli.dir),
                    selector.as_deref(),
                    json,
                    cache_only,
                    cli.config.as_deref(),
                ),
                PrCmd::Checkout {
                    target,
                    worktree_path,
                    setup,
                    apps,
                } => checkout::run(checkout::CheckoutArgs {
                    target,
                    worktree_path,
                    setup,
                    apps,
                    dir: cli.dir,
                    config: cli.config,
                }),
            }
        }
```

Leave the existing `Cmd::Info` and `Cmd::CheckoutPr` arms untouched: they already call the same functions.

The `issue` about line is printed by `issue --help`, so hiding the variants is
not enough on its own. In `src/bin/devkit/main.rs:115`:

```rust
    /// Issue lifecycle: setup, pr, status, end, sync-includes, prs, dashboard, review.
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo nextest run --test shim_dispatch`

Expected: PASS.

- [ ] **Step 6: Update the docs**

The hidden aliases are absent from the docs as well as from `--help`: the docs
teach one name per job.

- `docs/commands.md`: rename the `checkout-pr` section to `pr checkout`, split `info` out of the `status, info, end, sync-includes` heading into an `issue pr status` entry, and update the cross-references at `docs/commands.md:159,167`.
- `docs/agents.md`: add `issue pr` to the line naming the CLI-only `issue` verbs.
- `AGENTS.md:37`: update the `src/bin/devkit/` row's list of `issue` subcommands.
- `skills/using-devkit/references/issues.md`: replace `issue checkout-pr` with `issue pr checkout` and the `issue info --json` recipe with `issue pr status --json`.
- `docs/configuration.md:124,357,367,419,423,615`, `src/bin/devkit/schema.rs:116`, and the comments at `record.rs:20`, `slug.rs:1`, `setup.rs:97`, `review/finish.rs:215`, `checkout.rs:524`: same rename.
- `crates/devkit-config/src/lib.rs:164,523,529` name `checkout-pr` in doc comments that reach the committed schema. Rename them and regenerate with `DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema`.

- [ ] **Step 7: Run the full gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-fail-fast
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git diff --stat
git add -u
git commit -m "$(cat <<'EOF'
feat(issue): add the pr command group

`info` and `checkout-pr` are names from before there was anywhere
better to put them. Group them as `pr status` and `pr checkout`, with
bare `issue pr` meaning status the way bare `issue` means status.

Both old spellings stay reachable as hidden variants, so scripts and
muscle memory keep working without advertising two names for one job.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Add `issue pr create`

**Files:**
- Create: `src/bin/devkit/issue/pr/mod.rs`, `src/bin/devkit/issue/pr/create.rs`
- Modify: `src/bin/devkit/issue/mod.rs` (add `PrCmd::Create`, declare `mod pr`)
- Modify: `src/bin/devkit/issue/review/request.rs` (call the shared create path)

**Interfaces:**
- Consumes: `PrCreateState` from Task 4; `PrStatus`/`PrBrief` draft flags from Tasks 1–2.
- Produces two entry points, because Task 8 needs the resolution half without the create half:
  - `pr::create::resolve_existing(&Existing) -> Result<Found>` where `Found { pr: Option<github::PrBrief>, locator: Option<github::PrLocator>, repo: github::Repo }`. Pushes, then resolves through explicit `--pr`, the record, and branch discovery.
  - `pr::create::ensure(Ensure<'_>) -> Result<Resolved>` where `Resolved { url: String, locator: github::PrLocator, pr: github::PrBrief, created: bool }`. Calls `resolve_existing`, then creates or reuses.
  - `pr::create::run(pr::create::Args) -> Result<()>`, the subcommand.
  - Moved from `review/request.rs` and re-exported: `parse_pr_flag`, `acting_repo`, `verify_created`, `record_with_pr`, `existing_pr`, `PrFlat`.

- [ ] **Step 1: Write the failing tests**

Create `src/bin/devkit/issue/pr/create.rs` with a test module covering the pure decisions:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_flag_takes_the_configured_state() {
        assert_eq!(
            wanted_state(false, false, PrCreateState::Draft),
            PrCreateState::Draft
        );
        assert_eq!(
            wanted_state(false, false, PrCreateState::Ready),
            PrCreateState::Ready
        );
    }

    #[test]
    fn an_explicit_flag_beats_the_config() {
        assert_eq!(
            wanted_state(true, false, PrCreateState::Ready),
            PrCreateState::Draft
        );
        assert_eq!(
            wanted_state(false, true, PrCreateState::Draft),
            PrCreateState::Ready
        );
    }

    #[test]
    fn reuse_reports_a_state_flag_it_did_not_apply() {
        let note = reuse_note(123, /* pr_is_draft */ false, Some(PrCreateState::Draft));
        let note = note.expect("a contradicted flag is reported");
        assert!(note.contains("#123"), "names the PR: {note}");
        assert!(note.contains("gh pr ready --undo"), "names the way out: {note}");
    }

    #[test]
    fn reuse_says_nothing_when_the_state_already_matches() {
        assert!(reuse_note(123, false, Some(PrCreateState::Ready)).is_none());
        assert!(reuse_note(123, true, Some(PrCreateState::Draft)).is_none());
        assert!(reuse_note(123, true, None).is_none());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --bin devkit -E 'test(wanted_state) or test(reuse)'`

Expected: FAIL to compile: the module does not exist.

- [ ] **Step 3: Write the pure decisions**

In `src/bin/devkit/issue/pr/create.rs`:

```rust
use devkit_config::PrCreateState;

/// The state a create should use: an explicit flag, else the configured
/// default. Clap makes the two flags mutually exclusive, so both being set is
/// unreachable.
fn wanted_state(draft: bool, ready: bool, configured: PrCreateState) -> PrCreateState {
    match (draft, ready) {
        (true, _) => PrCreateState::Draft,
        (_, true) => PrCreateState::Ready,
        (false, false) => configured,
    }
}

/// What to print when a run reused a PR whose draft state contradicts an
/// explicit flag. `create` never flips an existing PR, so saying nothing would
/// leave the user believing the flag applied.
fn reuse_note(number: u64, pr_is_draft: bool, asked: Option<PrCreateState>) -> Option<String> {
    let asked = asked?;
    let matches = match asked {
        PrCreateState::Draft => pr_is_draft,
        PrCreateState::Ready => !pr_is_draft,
    };
    if matches {
        return None;
    }
    let (is, flag, way_back) = if pr_is_draft {
        ("a draft", "--ready", "issue pr ready")
    } else {
        ("ready for review", "--draft", "gh pr ready --undo")
    };
    Some(format!(
        "PR #{number} already exists and is {is}.\n\
         {flag} was ignored. To move it: {way_back}"
    ))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --bin devkit -E 'test(wanted_state) or test(reuse)'`

Expected: PASS.

- [ ] **Step 5: Split the create-or-reuse path out of `review request`**

Extract in two pieces, not one. Task 8 needs to resolve a PR *without* creating
one, so the push-and-resolve half stands alone:

```rust
/// Inputs to the push-and-resolve half.
pub(crate) struct Existing<'a> {
    pub start: &'a str,
    pub branch: &'a str,
    pub repos: &'a github::Repos,
    pub record: Option<&'a devkit_common::record::IssueRecord>,
    pub explicit_pr: Option<github::PrLocator>,
    pub no_push: bool,
    pub steps: &'a devkit_common::progress::Steps,
}

/// What resolution found. `pr` is `None` when this branch has no PR.
pub(crate) struct Found {
    pub pr: Option<github::PrBrief>,
    pub locator: Option<github::PrLocator>,
    pub repo: github::Repo,
}

/// Push the branch, then resolve this run's PR: explicit `--pr`, then the
/// record, then branch discovery. The repository a locator names wins over
/// `pr_repo`, so the fetch that the head-oid gate validates and the edit it
/// protects read the same repository.
pub(crate) fn resolve_existing(args: &Existing<'_>) -> Result<Found>;

/// The PR this run acts on, created or reused.
pub(crate) struct Resolved {
    pub url: String,
    pub locator: github::PrLocator,
    pub pr: github::PrBrief,
    pub created: bool,
}

pub(crate) struct Ensure<'a> {
    pub existing: Existing<'a>,
    pub head: &'a str,
    pub state: PrCreateState,
    /// The explicit `--draft` / `--ready`, or `None` when neither was passed.
    /// `reuse_note` reports only a flag the user actually typed.
    pub asked: Option<PrCreateState>,
    pub base: String,
    pub pr_title: String,
    pub pr_body: String,
    pub reviewers: Vec<String>,
    pub require_reviewer: bool,
    pub steps: &'a devkit_common::progress::Steps,
}

pub(crate) fn ensure(args: Ensure<'_>) -> Result<Resolved>;
```

Move into `pr::create` from `review/request.rs`: `PrFlat`, `existing_pr`,
`parse_pr_flag`, `acting_repo`, `fetch_context`, `verify_created`, and
`record_with_pr`. `review request` imports them from `pr::create` rather than
owning them. Mark each `pub(crate)`.

`ensure` calls `resolve_existing`, then branches on `action_for`:

- `Create`: render nothing extra, pass `--draft` to `gh pr create` when `state` is `PrCreateState::Draft`, run `verify_created`, write the record.
- `AddReviewer`: run `assert_belongs`, apply `--to` through `gh pr edit --add-reviewer`, print `reuse_note(number, pr.is_draft, args.asked)` when it returns `Some`, write the record. Never change draft state.
- `Stop(reason)`: bail with the reason.

```rust
            if matches!(state, PrCreateState::Draft) {
                gh_args.push("--draft");
            }
```

`require_reviewer` is threaded but unused until Task 7 adds the gate.

`review::request::run` calls `ensure` with `state: PrCreateState::Ready` and
`asked: None`, which is what it does today, so its behavior is unchanged by this
task.

- [ ] **Step 6: Wire up the subcommand**

Add to `PrCmd` in `src/bin/devkit/issue/mod.rs`:

```rust
    /// Push the branch and open (or reuse) this branch's PR.
    Create {
        /// Open as a draft, whatever `defaults.pr_create_state` says.
        #[arg(long)]
        draft: bool,
        /// Open ready for review, whatever `defaults.pr_create_state` says.
        #[arg(long, conflicts_with = "draft")]
        ready: bool,
        /// Reviewer: a `[people]` alias. Repeatable. Adds GitHub reviewers and
        /// sends no Slack.
        #[arg(long = "to")]
        to: Vec<String>,
        /// PR base branch, instead of the configured baseline ref.
        #[arg(long)]
        base: Option<String>,
        /// PR title, instead of the one the template renders.
        #[arg(long = "pr-title")]
        pr_title: Option<String>,
        /// PR body, instead of the one the template renders.
        #[arg(long = "pr-body")]
        pr_body: Option<String>,
        /// Open or update the PR without pushing the branch first.
        #[arg(long = "no-push")]
        no_push: bool,
        /// Use this PR for this run: a GitHub PR URL or a bare number (meaning
        /// `pr_repo`). Replaces a wrong recorded binding.
        #[arg(long)]
        pr: Option<String>,
    },
```

Add `mod pr;` beside the other module declarations, and the dispatch arm. `Args`
mirrors the variant's fields plus the two globals every `issue` subcommand
carries:

```rust
pub struct Args {
    pub draft: bool,
    pub ready: bool,
    pub to: Vec<String>,
    pub base: Option<String>,
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
    pub no_push: bool,
    pub pr: Option<String>,
    pub args: Vec<String>,
    pub dir: Option<String>,
    pub config: Option<String>,
}
```

`--arg key=value` comes across with the rest: the `pr_title` and `pr_body`
templates read `[templates.variables]` overrides through it, so leaving it behind
would make those templates un-overridable. Add the flag to the `PrCmd::Create`
variant alongside the others, spelled as it is on `ReviewCmd::Request`.

`run` derives `asked` from the flags the user typed, not from the resolved state:

```rust
    let asked = if args.draft {
        Some(PrCreateState::Draft)
    } else if args.ready {
        Some(PrCreateState::Ready)
    } else {
        None
    };
```

Deriving it from `wanted_state` instead would print "--draft was ignored" on
every reuse under the default config.

- [ ] **Step 7: Run the full gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-fail-fast
```

Expected: PASS. `issue review request` still creates PRs through the shared path, so its existing tests keep passing.

- [ ] **Step 8: Commit**

```bash
git add src/bin/devkit/issue/pr/ src/bin/devkit/issue/mod.rs src/bin/devkit/issue/review/request.rs
git commit -m "$(cat <<'EOF'
feat(issue): add pr create

Opening a PR lived inside `issue review request`, a command named
after telling humans about one. Give it its own verb, defaulting to a
draft.

The create-or-reuse path moves into a shared module both commands
call, so this commit changes no behavior for `review request`. The
reuse arm never flips draft state: a state flag that contradicts the
PR found is reported rather than silently dropped.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Add `issue pr ready` and the reviewer gate

**Files:**
- Create: `src/bin/devkit/issue/pr/ready.rs`
- Modify: `src/bin/devkit/issue/pr/mod.rs` (declare `ready`, add the gate helper)
- Modify: `src/bin/devkit/issue/mod.rs` (add `PrCmd::Ready`)
- Modify: `crates/devkit-common/src/github.rs` (add `submitted_reviewers`)
- Modify: `src/bin/devkit/issue/pr/create.rs` (gate the create-ready path)
- Modify: `src/bin/devkit/issue/review/mod.rs` (delete `require_reviewer` and its test)

**Interfaces:**
- Consumes: `pr::create::{Resolved, parse_pr_flag, acting_repo}` from Task 6; `github::requested_reviewers`; `review::is_human_login`.
- Produces: `github::submitted_reviewers(slug: &str, n: u64) -> Result<Vec<String>>`; `pr::ready::run(pr::ready::Args { .. }) -> Result<()>`; `pr::require_reviewer_for_ready(existing: &[String], added: &[String], required: bool) -> Result<()>`, the gate Task 8 calls before flipping a draft.

- [ ] **Step 1: Write the failing tests**

In `src/bin/devkit/issue/pr/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_is_off_unless_configured() {
        assert!(require_reviewer_for_ready(&[], &[], false).is_ok());
    }

    #[test]
    fn a_ready_pr_needs_a_human_reviewer() {
        let err = require_reviewer_for_ready(&[], &[], true).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("--to"), "names the way to satisfy it: {msg}");
    }

    #[test]
    fn an_existing_reviewer_satisfies_the_gate() {
        assert!(require_reviewer_for_ready(&["igoracc".into()], &[], true).is_ok());
    }

    #[test]
    fn a_reviewer_added_this_run_satisfies_the_gate() {
        assert!(require_reviewer_for_ready(&[], &["igoracc".into()], true).is_ok());
    }

    #[test]
    fn a_bot_is_not_a_reviewer() {
        assert!(require_reviewer_for_ready(&["dependabot[bot]".into()], &[], true).is_err());
    }
}
```

In `crates/devkit-common/src/github.rs`, inside `mod tests`:

```rust
    #[test]
    fn parse_submitted_reviewers_dedupes_and_skips_empty_logins() {
        let v = json!([
            { "user": { "login": "igoracc" }, "state": "COMMENTED" },
            { "user": { "login": "igoracc" }, "state": "APPROVED" },
            { "user": null, "state": "APPROVED" }
        ]);
        assert_eq!(parse_submitted_reviewers(&v), vec!["igoracc".to_string()]);
    }
```

In `src/bin/devkit/issue/pr/ready.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_already_ready_pr_needs_no_call() {
        assert!(!needs_flip(/* is_draft */ false));
        assert!(needs_flip(true));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --bin devkit -E 'test(gate) or test(reviewer) or test(needs_flip)'`

Expected: FAIL to compile: `require_reviewer_for_ready` and `needs_flip` do not exist.

- [ ] **Step 3: Fetch the reviewers who already reviewed**

`requested_reviewers` (`github.rs:881`) returns only pending requests, and GitHub
drops a login from that list the moment they submit a review. A PR that has
already been looked at would otherwise count zero reviewers. Add the companion
fetch in `crates/devkit-common/src/github.rs`, beside `requested_reviewers`:

```rust
fn parse_submitted_reviewers(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in v.as_array().into_iter().flatten() {
        let Some(login) = r.get("user").and_then(|u| u.get("login")).and_then(|l| l.as_str())
        else {
            continue;
        };
        if !out.iter().any(|s| s == login) {
            out.push(login.to_string());
        }
    }
    out
}

/// Logins that have submitted a review on PR `n`. GitHub removes a reviewer
/// from `requested_reviewers` once they review, so a caller asking "is anyone
/// reviewing this" needs both lists.
pub fn submitted_reviewers(slug: &str, n: u64) -> Result<Vec<String>> {
    let v = rest_get(&format!("/repos/{slug}/pulls/{n}/reviews"))?;
    Ok(parse_submitted_reviewers(&v))
}
```

`rest_get` resolves a bearer token through `bearer()` (`github.rs:86`) and errors
without one, so this call cannot be the only path. Wrap it in `pr/mod.rs` with a
`gh pr view <n> --json reviews` fallback through `cmd::gh_json_in`, mirroring
`requested_reviewer_logins` (`review/request.rs:97`), which already does exactly
this for the pending list. A machine authenticated only through `gh auth login`
must still be able to run `pr ready`.

- [ ] **Step 4: Write the gate**

In `src/bin/devkit/issue/pr/mod.rs`:

```rust
use anyhow::{Result, bail};

/// Refuse a run that would leave a PR ready for review with no human reviewer,
/// when `defaults.require_pr_reviewer` is set.
///
/// A login that already submitted a review counts: GitHub drops a reviewer from
/// `reviewRequests` the moment they review, so counting pending requests alone
/// would refuse a PR that has already been looked at.
pub(crate) fn require_reviewer_for_ready(
    existing: &[String],
    added: &[String],
    required: bool,
) -> Result<()> {
    if !required {
        return Ok(());
    }
    let any_human = existing
        .iter()
        .chain(added)
        .any(|l| crate::issue::review::is_human_login(l));
    if !any_human {
        bail!(
            "refusing to mark this PR ready with no human reviewer \
             (defaults.require_pr_reviewer is set) — pass --to, or add a \
             reviewer on GitHub"
        );
    }
    Ok(())
}
```

Make `is_human_login` visible to this module by widening it from `pub(crate)` within `review` if needed, keeping it defined once.

- [ ] **Step 5: Write the command**

In `src/bin/devkit/issue/pr/ready.rs`:

```rust
/// A ready PR needs no call: `gh pr ready` on one is a no-op, but skipping it
/// keeps the run silent and offline-safe.
fn needs_flip(is_draft: bool) -> bool {
    is_draft
}
```

`run` performs, in order:

1. Load config, resolve `Repos`, read the record, resolve the branch and guard it with `review::guard_branch`.
2. Push the branch unless `--no-push`, the same call `review request` makes at `request.rs:265`. Without it, any commit made since `pr create` fails the head-oid gate with an error whose stated diagnosis is wrong.
3. Resolve the PR through `pr::create::parse_pr_flag` / the record / branch discovery, and the repository through `pr::create::acting_repo`.
4. `review::finish::assert_belongs(&pr, &head)?`.
5. Resolve `--to` through `review::resolve_target` into logins, and apply them: when the list is non-empty, run `gh pr edit <n> --add-reviewer <logins>` through `cmd::gh_capture`. The flag's help says it adds GitHub reviewers, so a run that only counted them for the gate would be lying.
6. When `require_pr_reviewer` is set, fetch both reviewer lists (`requested_reviewers` and `submitted_reviewers`, each behind the `gh` fallback), concatenate, and call `require_reviewer_for_ready`. Skip both fetches when the key is off: they are two network round trips that decide nothing.
7. If `needs_flip(pr.is_draft)`, run `gh pr ready <n>` through `cmd::gh_capture`, which appends `--repo`. Otherwise print that the PR is already ready and exit zero.
8. Write the resolved locator into `.devkit/issue.toml` through `pr::create::record_with_pr`.
9. Print the PR URL.

Steps 1 through 4 are `pr::create::resolve_existing` plus `assert_belongs`: call
it with `Existing { start, branch, repos, record, explicit_pr, no_push, steps }`
rather than reimplementing the resolution.

- [ ] **Step 6: Wire up the subcommand**

Add to `PrCmd`:

```rust
    /// Mark this branch's PR ready for review.
    Ready {
        /// Reviewer: a `[people]` alias. Repeatable. Adds GitHub reviewers and
        /// sends no Slack.
        #[arg(long = "to")]
        to: Vec<String>,
        /// Mark ready without pushing the branch first.
        #[arg(long = "no-push")]
        no_push: bool,
        /// Use this PR for this run: a GitHub PR URL or a bare number.
        #[arg(long)]
        pr: Option<String>,
    },
```

Add the dispatch arm calling `pr::ready::run`:

```rust
pub struct Args {
    pub to: Vec<String>,
    pub no_push: bool,
    pub pr: Option<String>,
    pub dir: Option<String>,
    pub config: Option<String>,
}
```

- [ ] **Step 7: Gate `pr create --ready` too**

In `pr::create::ensure`'s create arm, call `require_reviewer_for_ready(&[], &reviewers, required)` before `gh pr create` when `state` is `PrCreateState::Ready`. A draft create is not gated: an unreviewed draft is not a violation.

Delete `review::require_reviewer` (`review/mod.rs:60`), its call inside
`pr::create::ensure`'s create arm (moved there by Task 6), and the test
`require_reviewer_only_gates_when_configured` at `review/mod.rs:278`, which
exercises the deleted function and would otherwise fail to compile. It checked
Slack targets rather than GitHub reviewers and only ran when creating.

- [ ] **Step 8: Run the full gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-fail-fast
```

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/devkit-common/src/github.rs src/bin/devkit/issue/pr/ src/bin/devkit/issue/mod.rs src/bin/devkit/issue/review/
git commit -m "$(cat <<'EOF'
feat(issue): add pr ready and move the reviewer gate

`pr ready` pushes, checks the PR carries this worktree's commits, and
marks it ready. The push is not optional: without it any commit made
since `pr create` fails the head-oid gate with an error whose stated
diagnosis is wrong.

require_pr_reviewer moves onto the ready transition and counts human
GitHub reviewers rather than Slack recipients, so a #channel no longer
satisfies it and the AddReviewer path no longer skips it. A login that
already submitted a review counts, since GitHub drops those from
reviewRequests.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Require an existing PR in `issue review request`

**Files:**
- Modify: `src/bin/devkit/issue/review/request.rs`
- Modify: `src/bin/devkit/issue/review/mod.rs` (`action_for` doc)
- Modify: `src/bin/devkit/issue/mod.rs` (`ReviewCmd::Request` flags)
- Modify: `docs/commands.md`, `skills/using-devkit/references/issues.md`

**Interfaces:**
- Consumes: `pr::create::{resolve_existing, Existing, Found, record_with_pr}` from Task 6; `pr::require_reviewer_for_ready` from Task 7. Not `ensure`: this command must never create a PR.
- Produces: nothing later tasks depend on. This is the only breaking commit.

- [ ] **Step 1: Write the failing tests**

In `src/bin/devkit/issue/review/request.rs`, inside `mod tests`:

```rust
    #[test]
    fn no_pr_names_the_command_that_opens_one() {
        let err = require_existing_pr(None).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("issue pr create"),
            "names the way forward: {msg}"
        );
    }

    #[test]
    fn an_open_pr_is_accepted() {
        assert!(require_existing_pr(Some("OPEN")).is_ok());
    }

    #[test]
    fn a_merged_pr_is_refused_without_naming_pr_create() {
        let msg = format!("{}", require_existing_pr(Some("MERGED")).unwrap_err());
        assert!(msg.contains("merged"), "says why: {msg}");
        assert!(!msg.contains("issue pr create"), "creating is not the fix: {msg}");
    }

    #[test]
    fn notifying_flips_a_draft_but_no_notify_does_not() {
        assert!(should_flip(/* is_draft */ true, /* no_notify */ false));
        assert!(!should_flip(true, true));
        assert!(!should_flip(false, false));
    }

    #[test]
    fn a_record_without_a_pr_gets_one_written() {
        let rec = devkit_common::record::IssueRecord {
            issue: "ENG-1".into(),
            slug: "fix-login".into(),
            apps: Vec::new(),
            summary: None,
            pr: None,
        };
        let loc = github::PrLocator {
            repo: Some("o/r".into()),
            number: 7,
        };
        let healed = record_with_pr(Some(&rec), loc.clone()).expect("a record to heal");
        assert_eq!(healed.pr, Some(loc));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --bin devkit -E 'test(require_existing_pr) or test(flip)'`

Expected: FAIL to compile: neither function exists.

- [ ] **Step 3: Write the decisions**

In `src/bin/devkit/issue/review/request.rs`:

```rust
/// Requesting a review acts on a PR that exists. Creating one is `issue pr
/// create`'s job, so a missing PR is an error that names it rather than a
/// silent create.
fn require_existing_pr(pr_state: Option<&str>) -> Result<()> {
    match super::action_for(pr_state) {
        PrAction::Create => bail!(
            "no PR for this branch — run `issue pr create` first, \
             or pass --pr <URL|number>"
        ),
        PrAction::AddReviewer => Ok(()),
        PrAction::Stop(reason) => bail!("{reason}"),
    }
}

/// Asking a human to look at a draft is incoherent, so a notifying run promotes
/// it. `--no-notify` tells nobody, and promoting a PR to ready tells everybody.
fn should_flip(is_draft: bool, no_notify: bool) -> bool {
    is_draft && !no_notify
}
```

- [ ] **Step 4: Resolve without creating**

Task 6 moved the create-or-reuse path into `pr::create::ensure`. Swap
`review::request::run`'s call to `ensure` for the resolution half alone:

```rust
    let found = crate::issue::pr::create::resolve_existing(&crate::issue::pr::create::Existing {
        start: &start,
        branch: &branch,
        repos: &repos,
        record: record.as_ref(),
        explicit_pr: explicit_loc,
        no_push: args.no_push,
        steps: &steps,
    })?;
    require_existing_pr(found.pr.as_ref().map(|p| p.state.as_str()))?;
    let pr = found.pr.expect("require_existing_pr rejects a missing PR");
    let repo = found.repo;
    let loc = found.locator.expect("a resolved PR carries a locator");
    super::finish::assert_belongs(&pr, &head)?;
```

`pr` is a `github::PrBrief`, so `pr.number`, `pr.url` and `pr.is_draft` are all
in hand for the steps that follow. Write the record from `loc` through
`pr::create::record_with_pr`, which is the record heal: a PR found by branch
discovery gets bound to a `setup` worktree whose record still carries
`pr: None`.

Remove `--base`, `--pr-title`, `--pr-body` from `ReviewCmd::Request` in
`src/bin/devkit/issue/mod.rs` and from `request::Args`, along with the
`pr_title`/`pr_body` template renders. Delete `review::require_pr_title`
(`review/mod.rs:52`) and the test `require_pr_title_rejects_empty` at
`review/mod.rs:264`, which exercises it: `pr create` now owns the only path that
needs a title.

Rewrite the two doc comments that describe the old behavior: the
`ReviewCmd::Request` summary at `issue/mod.rs:246` ("Push, open/reuse the PR…")
and `--no-notify`'s at `issue/mod.rs:265`.

`action_for` keeps its single `None => Create` mapping; only its doc comment
changes, to note that `pr create` acts on `Create` while `review request`
refuses it.

- [ ] **Step 5: Flip the draft, gated**

After `assert_belongs` and after reviewers are added, and before any Slack goes out:

```rust
    if should_flip(pr.is_draft, args.no_notify) {
        let mut existing = requested_reviewer_logins(pr.number, &start, &repo)?;
        existing.extend(github::submitted_reviewers(&repo.slug, pr.number)?);
        crate::issue::pr::require_reviewer_for_ready(
            &existing,
            &logins,
            loaded.config.defaults.require_pr_reviewer,
        )?;
        steps
            .during_result("Marking ready for review…", || {
                gh_capture(&["pr", "ready", &pr.number.to_string()], &repo, &start)
            })
            .context("gh pr ready failed")?;
    }
```

The gate runs before the flip so a refusal leaves the PR a draft.

- [ ] **Step 6: Take the Slack title from the PR**

Replace the locally rendered `pr_title` in the notify context with the fetched title. `PrBrief` has no title field, so use the call `review finish` already makes:

```rust
    let full = steps
        .during_result("Fetching PR title…", || {
            super::finish::fetch_pr_full(&repo, pr.number, &start)
        })
        .context("fetching the PR's title")?;
```

and pass `full.title` as the `pr_title` field of `notify_ctx`.

Use `finish::fetch_pr_full` (`review/finish.rs:110`), widening it to
`pub(crate)`, rather than `github::pr_full` directly. `pr_full` goes through
`rest_get`, which errors without a token; `fetch_pr_full` already carries the
`gh` fallback for exactly this read. Match its real parameter list when calling
it.

- [ ] **Step 7: Update the docs**

- `docs/commands.md`: rewrite the `review request` section. It no longer opens PRs, no longer takes `--base`/`--pr-title`/`--pr-body`, marks a draft ready when it notifies, and errors naming `issue pr create` when no PR exists. Move the `require_pr_reviewer` paragraph to the `issue pr` section and describe the three ready paths.
- `skills/using-devkit/references/issues.md`: the shipping recipe becomes `issue pr create` then `issue review request --to <alias>`.

- [ ] **Step 8: Run the full gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-fail-fast
```

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add src/bin/devkit/issue/ docs/commands.md skills/using-devkit/
git commit -m "$(cat <<'EOF'
feat(issue)!: require an existing PR in review request

Requesting a review now acts only on a PR that exists; opening one is
`issue pr create`. A missing PR errors naming that command instead of
silently creating one, and --base, --pr-title and --pr-body are gone
along with the create path.

A notifying run marks a draft ready first, since asking a human to
review a draft is incoherent. --no-notify leaves draft state alone.
The Slack title comes from the PR rather than a rendered template, so
a re-request no longer depends on a template for a PR it is not
creating.

BREAKING CHANGE: `issue review request` no longer opens PRs. Use
`issue pr create` first. The flags --base, --pr-title and --pr-body
moved to that command.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Verification

After Task 8, walk the surface end to end in a scratch worktree:

```bash
issue pr --help          # lists status, create, ready, checkout
issue pr status          # the row `issue info` used to print
issue info               # same output, absent from `issue --help`
issue pr create          # opens a draft, prints the URL
issue pr status          # PR column reads DRAFT #n, dimmed
issue pr ready           # flips to ready, idempotent on a second run
issue review request --to <alias>   # adds reviewers and Slacks
```

Then confirm the gate holds: set `require_pr_reviewer = true`, run `issue pr create --ready` with no `--to`, and check it refuses.
