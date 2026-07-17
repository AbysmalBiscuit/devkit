# `issue prs` Linear PR-Link Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When `[linear] resolve_pr_links = true`, `issue prs` queries Linear for the issues linked to each PR and shows the union of the text-derived id and all Linear-linked ids in the ISSUE column.

**Architecture:** A new opt-in `[linear]` config section (devkit-ports); a batched `attachmentsForURL` GraphQL lookup in `devkit-common::linear` (URLs as GraphQL variables, 25 per request, fail-soft); `MinePrView`/`ReviewPrView` switch from `issue_id: String` to `issue_ids: Vec<String>`; `gather` gains a `resolve_pr_links: bool` param and merges Linear links by PR URL after `classify`. Both callers (CLI + MCP) read the flag from config and pass it down — `devkit-issue` never depends on `devkit-ports`.

**Tech Stack:** Rust (edition 2024), `ureq` (blocking HTTP), `serde`/`serde_json`, `toml`, `anyhow`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-07-17-prs-linear-links-design.md`

## Global Constraints

- Merge gate after every task: `cargo test --workspace` green and `cargo clippy --workspace --all-targets -- -D warnings` clean. Format with `cargo fmt --all` before committing.
- TDD: write the failing test, watch it fail for the right reason, then implement.
- Commits: Conventional Commits, subject ≤50 chars, imperative, lowercase after the colon. End every commit message with the trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`. Commits are GPG-signed via the user's pinentry — if signing fails with a timeout, STOP and ask the user to unlock GPG; never pass `--no-gpg-sign`.
- `devkit-issue` must not gain a dependency on `devkit-ports` (config is read by callers, passed as plain values).
- The Linear API key is resolved only via `devkit_common::secrets::resolve("LINEAR_API_KEY")` (env → `~/.config/devkit/secrets.toml`), never from `config.toml`.
- `issue prs` must never fail because of Linear: no key / network error / API error all degrade to text-derived ids only.
- URLs travel as GraphQL variables, never spliced into the query string.
- All work happens in a feature worktree (`../devkit-worktrees/`), never on a branch checked out in the primary clone.

---

### Task 0: Create the worktree

**Files:** none (git only)

- [ ] **Step 1: Create the worktree and enter it**

```bash
git -C /home/lev/Git/lev/devkit worktree add ../devkit-worktrees/prs-linear-links -b feat/prs-linear-links main
cd /home/lev/Git/lev/devkit/../devkit-worktrees/prs-linear-links
```

All subsequent tasks run inside `/home/lev/Git/lev/devkit-worktrees/prs-linear-links` (verify with `pwd` after `cd`; all file paths below are relative to the worktree root).

- [ ] **Step 2: Verify a clean baseline**

Run: `cargo test --workspace`
Expected: all tests pass (~327).

---

### Task 1: `[linear]` config section

**Files:**
- Modify: `crates/devkit-ports/src/config.rs` (struct `Config` at the top of the file; new struct after `DaemonConfig`'s `impl Default`, ~line 71; test in the `mod tests` block at the bottom, ~line 455)

**Interfaces:**
- Consumes: nothing new.
- Produces: `Config.linear: LinearConfig` with `pub resolve_pr_links: bool` (default `false`). Task 5 reads `l.config.linear.resolve_pr_links` from a `devkit_ports::load::load(...)` result.

- [ ] **Step 1: Write the failing test**

In `crates/devkit-ports/src/config.rs`, add inside `mod tests` (after the existing `parses_sample` test):

```rust
    #[test]
    fn linear_section_parses_and_defaults_off() {
        let c = Config::parse(&format!("{SAMPLE}\n[linear]\nresolve_pr_links = true\n")).unwrap();
        assert!(c.linear.resolve_pr_links);
        let bare = Config::parse(SAMPLE).unwrap();
        assert!(!bare.linear.resolve_pr_links);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit-ports linear_section_parses_and_defaults_off`
Expected: COMPILE ERROR — `no field 'linear' on type 'Config'`. (A compile failure is the RED state here; the field doesn't exist yet.)

- [ ] **Step 3: Implement `LinearConfig`**

In `crates/devkit-ports/src/config.rs`, add to the `Config` struct (after the `daemon` field):

```rust
    #[serde(default)]
    pub linear: LinearConfig,
```

Add the struct after `impl Default for DaemonConfig` (~line 71). All fields default falsy, so `derive(Default)` suffices — no manual `impl Default` like `DaemonConfig` needs:

```rust
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct LinearConfig {
    /// Query Linear for the issues linked to each PR in `issue prs` (one
    /// extra batched round trip per run). Off by default.
    pub resolve_pr_links: bool,
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p devkit-ports linear_section_parses_and_defaults_off`
Expected: PASS. The layered config merge in `load.rs` operates on toml values before the typed parse, so `[linear]` merges across global/project files with no further change — confirm nothing else broke:

Run: `cargo test -p devkit-ports`
Expected: PASS.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add crates/devkit-ports/src/config.rs
git commit -m "feat(config): add [linear] resolve_pr_links option

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Batched `attachmentsForURL` lookup in `devkit-common::linear`

**Files:**
- Modify: `crates/devkit-common/src/linear.rs` (new functions after `states`, ~line 223; tests in the existing `mod tests`)

**Interfaces:**
- Consumes: the private `fn send(body: serde_json::Value, key: &str, detail: &str) -> Result<serde_json::Value>` transport already in this file (~line 130). It accepts a full JSON body, so a `{query, variables}` payload needs no transport change.
- Produces (all `pub`, in `devkit_common::linear`):
  - `issues_for_prs_queries(urls: &[String]) -> Vec<(String, serde_json::Value, HashMap<String, String>)>` — per chunk of ≤25 URLs: (query, variables object, alias→url map). Empty input → empty vec.
  - `parse_issues_for_prs(resp: &serde_json::Value, aliases: &HashMap<String, String>) -> HashMap<String, Vec<String>>` — url → linked issue identifiers.
  - `issues_for_prs(urls: &[String], key: Option<&str>) -> HashMap<String, Vec<String>>` — fail-soft orchestrator; Task 4 calls this from `gather`.

- [ ] **Step 1: Write the failing tests**

Add inside `mod tests` in `crates/devkit-common/src/linear.rs`:

```rust
    #[test]
    fn issues_for_prs_queries_use_variables() {
        let urls = vec![
            "https://github.com/o/r/pull/1".to_string(),
            "https://github.com/o/r/pull/2".to_string(),
        ];
        let batches = issues_for_prs_queries(&urls);
        assert_eq!(batches.len(), 1);
        let (q, vars, aliases) = &batches[0];
        assert!(q.contains("a0: attachmentsForURL(url: $u0)"), "{q}");
        assert!(q.contains("$u1: String!"), "{q}");
        assert!(
            !q.contains("github.com"),
            "urls must ride in variables, not the query: {q}"
        );
        assert_eq!(vars["u1"], "https://github.com/o/r/pull/2");
        assert_eq!(aliases["a0"], "https://github.com/o/r/pull/1");
    }

    #[test]
    fn issues_for_prs_queries_chunk_at_25() {
        let urls: Vec<String> = (0..26)
            .map(|i| format!("https://github.com/o/r/pull/{i}"))
            .collect();
        let batches = issues_for_prs_queries(&urls);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[1].2.len(), 1, "second chunk carries the 26th url");
        assert!(issues_for_prs_queries(&[]).is_empty());
    }

    #[test]
    fn parse_issues_for_prs_collects_and_dedups() {
        let aliases = HashMap::from([
            ("a0".to_string(), "u0".to_string()),
            ("a1".to_string(), "u1".to_string()),
        ]);
        let resp = serde_json::json!({ "data": {
            "a0": { "nodes": [
                { "issue": { "identifier": "SWE-6" } },
                { "issue": null },
                { "issue": { "identifier": "SWE-7" } },
                { "issue": { "identifier": "SWE-6" } }
            ]},
            "a1": { "nodes": [ { "issue": null } ] }
        }});
        let got = parse_issues_for_prs(&resp, &aliases);
        assert_eq!(got["u0"], vec!["SWE-6", "SWE-7"]);
        assert!(!got.contains_key("u1"), "all-null attachments mean no links");
    }

    #[test]
    fn parse_issues_for_prs_ignores_unknown_aliases() {
        let resp = serde_json::json!({ "data": {
            "zz": { "nodes": [ { "issue": { "identifier": "X-1" } } ] }
        }});
        assert!(parse_issues_for_prs(&resp, &HashMap::new()).is_empty());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-common issues_for_prs`
Expected: COMPILE ERROR — `cannot find function issues_for_prs_queries` (and `parse_issues_for_prs`).

- [ ] **Step 3: Implement the three functions**

Add after the `states` function (~line 223) in `crates/devkit-common/src/linear.rs`:

```rust
/// GraphQL payloads resolving GitHub PR URLs to their linked Linear issues,
/// 25 URLs per request to stay under Linear's query-complexity budget. Each
/// entry is (query, variables, alias → url). Pure → testable. URLs ride in
/// GraphQL variables, never spliced into the query string.
pub fn issues_for_prs_queries(
    urls: &[String],
) -> Vec<(String, serde_json::Value, HashMap<String, String>)> {
    urls.chunks(25)
        .map(|chunk| {
            let mut decls = Vec::new();
            let mut parts = Vec::new();
            let mut vars = serde_json::Map::new();
            let mut aliases = HashMap::new();
            for (i, url) in chunk.iter().enumerate() {
                decls.push(format!("$u{i}: String!"));
                parts.push(format!(
                    "a{i}: attachmentsForURL(url: $u{i}) {{ nodes {{ issue {{ identifier }} }} }}"
                ));
                vars.insert(format!("u{i}"), serde_json::Value::String(url.clone()));
                aliases.insert(format!("a{i}"), url.clone());
            }
            let query = format!("query({}) {{ {} }}", decls.join(", "), parts.join(" "));
            (query, serde_json::Value::Object(vars), aliases)
        })
        .collect()
}

/// From one `issues_for_prs_queries` response: url → linked issue ids.
/// Attachments without an issue are skipped; ids are deduped per PR (an
/// issue can attach to the same PR more than once). URLs with no linked
/// issue get no entry.
pub fn parse_issues_for_prs(
    resp: &serde_json::Value,
    aliases: &HashMap<String, String>,
) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    let Some(data) = resp.get("data").and_then(|d| d.as_object()) else {
        return out;
    };
    for (alias, block) in data {
        let Some(url) = aliases.get(alias) else {
            continue;
        };
        let mut ids: Vec<String> = Vec::new();
        for node in block["nodes"].as_array().into_iter().flatten() {
            if let Some(id) = node["issue"]["identifier"].as_str()
                && !ids.iter().any(|have| have == id)
            {
                ids.push(id.to_string());
            }
        }
        if !ids.is_empty() {
            out.insert(url.clone(), ids);
        }
    }
    out
}

/// Linked Linear issues for each PR URL. Fail-soft like [`states`]: empty
/// map with no key or no URLs; on error, one stderr line and whatever
/// chunks resolved before it. A URL absent from the map has no known links.
pub fn issues_for_prs(urls: &[String], key: Option<&str>) -> HashMap<String, Vec<String>> {
    let Some(key) = key else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for (query, vars, aliases) in issues_for_prs_queries(urls) {
        match send(
            ureq::json!({ "query": query, "variables": vars }),
            key,
            "issues_for_prs",
        ) {
            Ok(resp) => out.extend(parse_issues_for_prs(&resp, &aliases)),
            Err(e) => {
                eprintln!("Linear PR-link lookup failed: {e}");
                break;
            }
        }
    }
    out
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit-common`
Expected: PASS (the four new tests plus all existing ones).

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add crates/devkit-common/src/linear.rs
git commit -m "feat(linear): batched pr-url to linked-issues lookup

attachmentsForURL maps a GitHub PR URL to the Linear issues it is
attached to. Aliased and chunked at 25 URLs per request; URLs travel
as GraphQL variables so no escaping hazard reaches the query string.
Fail-soft like states: no key or an API error degrades to an empty or
partial map.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: `issue_ids: Vec<String>` through the data model and renderer

**Files:**
- Modify: `crates/devkit-issue/src/prs.rs` — `issue_of` (~line 209), `classify` (~lines 567, 594), `MinePrView`/`ReviewPrView` (~lines 615–636), tests (~lines 728, 734, 1250–1271)
- Modify: `src/bin/issue/prs.rs` — `issue_cell` (~line 39), the two call sites (~lines 147, 236), tests (`mine_view` ~line 432, `review_view` ~line 451, `snapshot_round_trips` ~line 518, `next_snapshot_preserves_unrequested_sections` ~line 542, plus two new tests)

**Interfaces:**
- Consumes: `devkit_common::worktree::find_id` (unchanged).
- Produces: `MinePrView.issue_ids: Vec<String>` and `ReviewPrView.issue_ids: Vec<String>` (both `#[serde(default)]`; the `issue_id: String` fields are REMOVED — this also renames the field in the MCP `issue.prs` JSON output, a deliberate break per the spec). Private `fn issue_ids_of(head: &str, title: &str) -> Vec<String>` replaces `issue_of`. Task 4 mutates `issue_ids` via `merge_linked`. Renderer signature becomes `fn issue_cell(issue_ids: &[String], url_key: Option<&str>) -> String`.

No other file touches these structs (verified: only `src/bin/issue/prs.rs` and `crates/devkit-mcp/src/issue.rs` consume them, and the MCP handler only serializes the whole report — it compiles unchanged).

- [ ] **Step 1: Write the failing tests**

In `src/bin/issue/prs.rs`, add to `mod tests`:

```rust
    #[test]
    fn issue_cell_joins_multiple_ids() {
        assert_eq!(issue_cell(&[], None), ui::dim("-"));
        let cell = issue_cell(&["ENG-1".into(), "SWE-6".into()], None);
        assert!(cell.contains("ENG-1") && cell.contains("SWE-6"), "{cell}");
    }

    // A pre-issue_ids snapshot (rows carry `issue_id`) still whole-struct
    // parses; the missing `issue_ids` reads as empty for one run.
    #[test]
    fn old_issue_id_snapshot_reads_with_empty_ids() {
        let old = r#"{"mine":[{"number":1,"url":"u","issue_id":"ENG-1","review_state":"approved","check_state":"ok","action":"MERGE"}],"reviews":[],"diff":{}}"#;
        let snap: Snapshot = serde_json::from_str(old).unwrap();
        assert_eq!(snap.mine.len(), 1);
        assert!(snap.mine[0].issue_ids.is_empty());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin issue issue_cell_joins_multiple_ids`
Expected: COMPILE ERROR — `issue_cell` takes `&str`, and `MinePrView` has no field `issue_ids`.

- [ ] **Step 3: Change the data model in `crates/devkit-issue/src/prs.rs`**

Replace `issue_of` (~line 209) with:

```rust
/// The issue ids a PR addresses, uppercased — zero or one from text. Taken
/// from the branch (head) ref — the convention for our own PRs — falling
/// back to the PR title, where other people's PRs carry the id (e.g.
/// `feat: … [SWE-123]`). Linear-linked ids are merged in later by `gather`.
fn issue_ids_of(head: &str, title: &str) -> Vec<String> {
    devkit_common::worktree::find_id(head)
        .or_else(|| devkit_common::worktree::find_id(title))
        .map(|s| s.to_uppercase())
        .into_iter()
        .collect()
}
```

In `MinePrView` (~line 619), replace `pub issue_id: String,` with:

```rust
    #[serde(default)]
    pub issue_ids: Vec<String>,
```

In `ReviewPrView` (~lines 631–632), replace the two lines `#[serde(default)]` + `pub issue_id: String,` with the same:

```rust
    #[serde(default)]
    pub issue_ids: Vec<String>,
```

In `classify`, both construction sites (~lines 567 and 594): replace
`issue_id: issue_of(&pr.head_ref_name, &pr.title),` with
`issue_ids: issue_ids_of(&pr.head_ref_name, &pr.title),`.

- [ ] **Step 4: Update the devkit-issue tests**

In `parses_graphql_and_classifies` (~lines 728, 734):

```rust
        assert_eq!(report.mine[0].issue_ids, vec!["ENG-1"]);
```
```rust
        assert_eq!(report.reviews[0].issue_ids, vec!["SWE-2"]);
```

Replace the three `issue_of_*` tests (~lines 1249–1272) with:

```rust
    #[test]
    fn issue_ids_of_finds_swe() {
        assert_eq!(issue_ids_of("lev/swe-123-fix", ""), vec!["SWE-123"]);
        assert!(issue_ids_of("main", "").is_empty());
    }
    #[test]
    fn issue_ids_of_finds_non_swe_prefix() {
        assert_eq!(issue_ids_of("lev/eng-1234-fix", ""), vec!["ENG-1234"]);
        assert_eq!(issue_ids_of("feature/abc-9-thing", ""), vec!["ABC-9"]);
    }
    #[test]
    fn issue_ids_of_falls_back_to_title() {
        assert_eq!(
            issue_ids_of("igork/ff-b01-thing", "feat(api): flag-gate thing [SWE-10412]"),
            vec!["SWE-10412"]
        );
        assert_eq!(
            issue_ids_of("lev/eng-1-fix", "chore: touches SWE-999 too"),
            vec!["ENG-1"]
        );
        assert!(issue_ids_of("main", "no id anywhere").is_empty());
    }
```

- [ ] **Step 5: Update the renderer in `src/bin/issue/prs.rs`**

Replace `issue_cell` (~lines 39–51) with:

```rust
/// The ISSUE column cell: every id as a Linear link (plain text without a
/// workspace url key), space-joined; dim `-` when no id resolved.
fn issue_cell(issue_ids: &[String], url_key: Option<&str>) -> String {
    if issue_ids.is_empty() {
        return ui::dim("-");
    }
    let cells: Vec<String> = issue_ids
        .iter()
        .map(|id| {
            let linked = match url_key {
                Some(k) => ui::link(id, &format!("https://linear.app/{k}/issue/{id}")),
                None => id.to_string(),
            };
            ui::cyan(&linked)
        })
        .collect();
    cells.join(" ")
}
```

Update both call sites: in `mine_table_build` (~line 147) and `reviews_table_build` (~line 236), replace `issue_cell(&pr.issue_id, url_key),` with `issue_cell(&pr.issue_ids, url_key),`.

- [ ] **Step 6: Update the binary's test fixtures**

In `mod tests` of `src/bin/issue/prs.rs`:

- `mine_view` (~line 436): `issue_id: "-".into(),` → `issue_ids: vec![],`
- `review_view` (~line 455): `issue_id: "ENG-9".into(),` → `issue_ids: vec!["ENG-9".into()],`
- `snapshot_round_trips` (~line 523): `issue_id: "ENG-1".into(),` → `issue_ids: vec!["ENG-1".into()],`
- `next_snapshot_preserves_unrequested_sections` (~line 545): `issue_id: "ENG-7".into(),` → `issue_ids: vec!["ENG-7".into()],` and (~line 555): `issue_id: "ENG-9".into(),` → `issue_ids: vec!["ENG-9".into()],`

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p devkit-issue && cargo test --bin issue`
Expected: PASS, including `issue_cell_joins_multiple_ids`, `old_issue_id_snapshot_reads_with_empty_ids`, and `reviews_table_build_renders_issue_column` (unchanged — `ENG-9` still renders).

- [ ] **Step 8: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add crates/devkit-issue/src/prs.rs src/bin/issue/prs.rs
git commit -m "refactor(issue)!: prs views carry issue_ids as a list

A PR can be linked to several Linear issues; a single issue_id field
cannot show them. The text path still yields zero or one id; the empty
vec replaces the \"-\" sentinel, which moves into the renderer. Old
pr-status snapshots parse with an empty list for one run (serde
default). The MCP issue.prs JSON renames issue_id to issue_ids with no
legacy alias.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: `gather` resolves and merges Linear links

**Files:**
- Modify: `crates/devkit-issue/src/prs.rs` — imports (~line 5), new `merge_linked`/`apply_linked` after `issue_ids_of`, `gather` (~line 671), new tests
- Modify: `src/bin/issue/prs.rs` — `fetch_report` (~lines 289–316), its call in `run` (~line 401)
- Modify: `crates/devkit-mcp/src/issue.rs` — `prs_handler` (~line 85)

**Interfaces:**
- Consumes: `devkit_common::linear::issues_for_prs(urls: &[String], key: Option<&str>) -> HashMap<String, Vec<String>>` (Task 2); `devkit_common::secrets::resolve(name: &str) -> Option<String>`; `issue_ids: Vec<String>` on the views (Task 3).
- Produces: `pub fn gather(root: &str, mine: bool, reviews: bool, repo: Option<&str>, ignored_checks: &[String], resolve_pr_links: bool) -> Result<PrsReport>` — the new trailing param; both callers pass `false` in this task (Task 5 wires the config value). `fetch_report` in the binary gains the same trailing `resolve_pr_links: bool` param.

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-issue/src/prs.rs` `mod tests`:

```rust
    #[test]
    fn merge_linked_unions_and_dedups() {
        let mut ids = vec!["ENG-123".to_string()];
        merge_linked(&mut ids, &["eng-123".to_string(), "SWE-6".to_string()]);
        assert_eq!(ids, vec!["ENG-123", "SWE-6"]);
        let mut empty: Vec<String> = vec![];
        merge_linked(&mut empty, &["SWE-7".to_string()]);
        assert_eq!(empty, vec!["SWE-7"]);
        let mut untouched = vec!["ENG-1".to_string()];
        merge_linked(&mut untouched, &[]);
        assert_eq!(untouched, vec!["ENG-1"]);
    }

    #[test]
    fn apply_linked_hits_both_sections_by_url() {
        let mut report = PrsReport {
            mine: vec![MinePrView {
                number: 1,
                url: "u1".into(),
                issue_ids: vec!["ENG-1".into()],
                review_state: "-".into(),
                check_state: "ok".into(),
                action: "MERGE".into(),
            }],
            reviews: vec![ReviewPrView {
                number: 2,
                url: "u2".into(),
                issue_ids: vec![],
                author: "a".into(),
                my_vote: "-".into(),
                action: "REVIEW NEEDED".into(),
            }],
        };
        let linked = HashMap::from([
            ("u1".to_string(), vec!["ENG-1".to_string(), "SWE-6".to_string()]),
            ("u2".to_string(), vec!["SWE-7".to_string()]),
        ]);
        apply_linked(&mut report, &linked);
        assert_eq!(report.mine[0].issue_ids, vec!["ENG-1", "SWE-6"]);
        assert_eq!(report.reviews[0].issue_ids, vec!["SWE-7"]);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-issue merge_linked`
Expected: COMPILE ERROR — `cannot find function merge_linked` (and `apply_linked`, `HashMap`).

- [ ] **Step 3: Implement the merge and thread the flag through `gather`**

In `crates/devkit-issue/src/prs.rs`, extend the collections import (line 5):

```rust
use std::collections::{BTreeMap, HashMap};
```

Add after `issue_ids_of`:

```rust
/// Merge Linear-linked ids into the text-derived ids: union, text id first,
/// deduped case-insensitively (text ids are uppercased; Linear identifiers
/// are canonical uppercase).
fn merge_linked(ids: &mut Vec<String>, linked: &[String]) {
    for id in linked {
        if !ids.iter().any(|have| have.eq_ignore_ascii_case(id)) {
            ids.push(id.clone());
        }
    }
}

/// Union the Linear-linked issue ids (url → ids) into every view row.
fn apply_linked(report: &mut PrsReport, linked: &HashMap<String, Vec<String>>) {
    for pr in &mut report.mine {
        if let Some(ids) = linked.get(&pr.url) {
            merge_linked(&mut pr.issue_ids, ids);
        }
    }
    for pr in &mut report.reviews {
        if let Some(ids) = linked.get(&pr.url) {
            merge_linked(&mut pr.issue_ids, ids);
        }
    }
}
```

Replace `gather` (~line 671) with:

```rust
/// Fetch and classify the caller's PRs in a single GraphQL round-trip.
/// Neither flag set ⇒ both groups. Stateless: no diff cache is read or
/// written. With `resolve_pr_links`, one extra batched Linear round trip
/// (after the GitHub fetch — it needs the PR URLs) unions Linear-linked
/// issue ids into each row; fail-soft, so a missing LINEAR_API_KEY or a
/// Linear error leaves the text-derived ids as-is.
pub fn gather(
    root: &str,
    mine: bool,
    reviews: bool,
    repo: Option<&str>,
    ignored_checks: &[String],
    resolve_pr_links: bool,
) -> Result<PrsReport> {
    let want_mine = mine || !reviews;
    let want_reviews = reviews || !mine;
    let repo = match repo {
        Some(r) => r.to_string(),
        None => resolve_repo(None, root)?,
    };
    let query = build_query(&repo);
    let resp = fetch_graphql(&query, root)?;
    let mut report = classify(resp.data, want_mine, want_reviews, ignored_checks);
    if resolve_pr_links {
        let key = devkit_common::secrets::resolve("LINEAR_API_KEY");
        let urls: Vec<String> = report
            .mine
            .iter()
            .map(|pr| pr.url.clone())
            .chain(report.reviews.iter().map(|pr| pr.url.clone()))
            .collect();
        let linked = devkit_common::linear::issues_for_prs(&urls, key.as_deref());
        apply_linked(&mut report, &linked);
    }
    Ok(report)
}
```

- [ ] **Step 4: Update both callers (pass `false` for now)**

In `src/bin/issue/prs.rs`, `fetch_report` gains the trailing param and forwards it (the doc comment stays accurate: the Linear PR-link call runs inside `gather`, still concurrent with the `workspace_url_key` thread):

```rust
fn fetch_report(
    resolved: &str,
    mine: bool,
    reviews: bool,
    ignored_checks: &[String],
    resolve_pr_links: bool,
) -> Result<(Option<String>, devkit_issue::prs::PrsReport)> {
```

and inside its second spawn (~line 310):

```rust
                let _ = tx.send(Update::Fetched(devkit_issue::prs::gather(
                    ".",
                    mine,
                    reviews,
                    Some(resolved),
                    ignored_checks,
                    resolve_pr_links,
                )));
```

At the call site in `run` (~line 401):

```rust
    let fetched = fetch_report(&resolved, mine, reviews, &ignored_checks, false);
```

In `crates/devkit-mcp/src/issue.rs` (~line 85):

```rust
    let report = prs::gather(&root, a.mine, a.reviews, a.repo.as_deref(), &ignored_checks, false)?;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p devkit-issue && cargo test --bin issue && cargo test -p devkit-mcp`
Expected: PASS.

- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add crates/devkit-issue/src/prs.rs src/bin/issue/prs.rs crates/devkit-mcp/src/issue.rs
git commit -m "feat(issue): union linear-linked issue ids into prs rows

gather gains a resolve_pr_links flag: after classify, one batched
attachmentsForURL round trip maps each PR URL to its linked Linear
issues, unioned (deduped, text id first) into issue_ids across both
sections. Callers pass false until config wiring lands.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Read the config flag in both callers, document it

**Files:**
- Modify: `src/bin/issue/prs.rs` — `run` (~lines 356–360 and the `fetch_report` call)
- Modify: `crates/devkit-mcp/src/issue.rs` — `prs_handler` (~lines 78–87)
- Modify: `docs/configuration.md` — new `### [linear]` section before `### [people.<alias>]` (~line 251)

**Interfaces:**
- Consumes: `Config.linear.resolve_pr_links` (Task 1) via `devkit_ports::load::load(config_path, cwd) -> Result<Loaded>` (field `l.config`); `gather`/`fetch_report` trailing bool (Task 4).
- Produces: end-user behavior — `[linear] resolve_pr_links = true` in any layered config file activates the lookup for both `issue prs` and the MCP `issue.prs` action.

There is no unit test for the wiring itself (both call sites are thin config plumbing into already-tested functions; the load→field read has no seam that doesn't just restate the code). Verification is the manual end-to-end run in Step 3.

- [ ] **Step 1: Wire the flag in the CLI**

In `src/bin/issue/prs.rs::run`, replace (~lines 356–360):

```rust
    // Check-name globs to discount from the CHECK verdict. Absent or unreadable
    // config simply means no checks are ignored — triage still works repo-wide.
    let ignored_checks = devkit_ports::load::load(config.as_deref().map(Path::new), Path::new("."))
        .map(|l| l.config.defaults.ignored_checks)
        .unwrap_or_default();
```

with:

```rust
    // Check-name globs to discount from the CHECK verdict, and the Linear
    // PR-link opt-in. Absent or unreadable config means no checks are ignored
    // and no Linear lookup — triage still works repo-wide.
    let loaded = devkit_ports::load::load(config.as_deref().map(Path::new), Path::new(".")).ok();
    let ignored_checks = loaded
        .as_ref()
        .map(|l| l.config.defaults.ignored_checks.clone())
        .unwrap_or_default();
    let resolve_pr_links = loaded
        .as_ref()
        .is_some_and(|l| l.config.linear.resolve_pr_links);
```

and update the `fetch_report` call (~line 401):

```rust
    let fetched = fetch_report(&resolved, mine, reviews, &ignored_checks, resolve_pr_links);
```

- [ ] **Step 2: Wire the flag in the MCP handler**

In `crates/devkit-mcp/src/issue.rs::prs_handler`, replace (~lines 81–85):

```rust
    // Check-name globs to discount from the CHECK verdict; absent config ⇒ none.
    let ignored_checks = devkit_ports::load::load(None, std::path::Path::new(&root))
        .map(|l| l.config.defaults.ignored_checks)
        .unwrap_or_default();
    let report = prs::gather(&root, a.mine, a.reviews, a.repo.as_deref(), &ignored_checks)?;
```

with:

```rust
    // Check-name globs to discount from the CHECK verdict, plus the Linear
    // PR-link opt-in; absent config ⇒ neither.
    let loaded = devkit_ports::load::load(None, std::path::Path::new(&root)).ok();
    let ignored_checks = loaded
        .as_ref()
        .map(|l| l.config.defaults.ignored_checks.clone())
        .unwrap_or_default();
    let resolve_pr_links = loaded
        .as_ref()
        .is_some_and(|l| l.config.linear.resolve_pr_links);
    let report = prs::gather(
        &root,
        a.mine,
        a.reviews,
        a.repo.as_deref(),
        &ignored_checks,
        resolve_pr_links,
    )?;
```

- [ ] **Step 3: Build and verify end-to-end**

```bash
cargo test --workspace
cargo build --release --bin issue
```

Then run the manual check from the spec (a repo with Linear-linked PRs, e.g. the monorepo checkout; `LINEAR_API_KEY` must be set in the environment or `secrets.toml`). Do NOT test with a minimal `--config` file containing only `[linear]` — the typed parse requires `[defaults]`, so such a file fails to load and silently reads as flag-off. Toggle the flag in the user's real global config instead:

```bash
cd <monorepo-checkout>
<worktree>/target/release/issue prs --no-cache          # baseline: flag off (global config has no [linear] yet)
printf '\n[linear]\nresolve_pr_links = true\n' >> ~/.config/devkit/config.toml
<worktree>/target/release/issue prs --no-cache          # flag on
```

Expected: rows that showed `-` (or a lone id) in the baseline now show the Linear-linked id(s); a multi-linked PR shows all ids space-separated. With the flag off, output is byte-identical to before this feature.

Afterwards, ask the user whether to keep `[linear] resolve_pr_links = true` in their global config or revert it (`git`-less file — revert means deleting the three appended lines).

- [ ] **Step 4: Document `[linear]` in `docs/configuration.md`**

Insert before the `### [people.<alias>]` heading (~line 251):

```markdown
### `[linear]`

Opt-in Linear enrichment for `issue prs`.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `resolve_pr_links` | bool | `false` | When `true`, `issue prs` asks Linear which issues each open PR is linked to and shows the union of the text-derived id and every linked id in the ISSUE column, deduplicated (text id first). |

The lookup authenticates with `LINEAR_API_KEY` (environment or
`~/.config/devkit/secrets.toml`); **no token lives in this table**. It costs
one extra batched round trip per 25 PRs, after the GitHub fetch. Fail-soft:
with no key, or on any Linear error, the column falls back to the
text-derived id — `issue prs` never fails because of Linear. The MCP
`issue.prs` action honors the same flag.

```toml
[linear]
resolve_pr_links = true
```
```

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git add src/bin/issue/prs.rs crates/devkit-mcp/src/issue.rs docs/configuration.md
git commit -m "feat(issue): config-gate linear pr-link resolution

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Land on main

**Files:** none (git only)

- [ ] **Step 1: Fast-forward `main` from outside its worktree**

Per the repo's worktree discipline (primary clone stays on `main`, merges happen from outside):

```bash
cd /home/lev/Git/lev/devkit
git switch main
git merge --ff-only feat/prs-linear-links
cargo test --workspace
```

Expected: fast-forward succeeds, tests green on `main`.

- [ ] **Step 2: Remove the worktree — then STOP**

```bash
git worktree remove ../devkit-worktrees/prs-linear-links
git branch -d feat/prs-linear-links
```

Do NOT push. Report completion to the user and let them decide when to push.
