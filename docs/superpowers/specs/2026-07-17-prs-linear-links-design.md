# `issue prs`: resolve PR→issue links from Linear

## Problem

The ISSUE column of `issue prs` is populated by text extraction only:
`issue_of` (`crates/devkit-issue/src/prs.rs`) takes the first `TEAM-NUMBER`
token from the branch name, falling back to the PR title. Many PRs carry the
id in neither place — the link lives only in Linear, created by the GitHub
integration or by hand. Those PRs render `-` even though Linear knows exactly
which issue(s) they belong to. A PR can also be linked to *several* issues;
the text path can only ever produce one.

Verified live against the Linear API: `attachmentsForURL(url: $u)` maps a
GitHub PR URL to its linked issues (`nodes { issue { identifier } }`),
accepts a `variables` payload, and batches via aliases — one round trip for
all PRs. 9 of 12 sampled monorepo PRs resolved ids this way.

## Design

### Config: `[linear] resolve_pr_links` (opt-in)

New section in `crates/devkit-ports/src/config.rs`, following the
`DaemonConfig` pattern (all-bool default is derivable, so no manual
`impl Default`):

```rust
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct LinearConfig {
    /// Query Linear for issues linked to each PR (one extra batched
    /// round trip per `issue prs` run). Off by default.
    pub resolve_pr_links: bool,
}
```

wired as `#[serde(default)] pub linear: LinearConfig` on `Config`. Usage:

```toml
[linear]
resolve_pr_links = true
```

Default off ⇒ zero behavior change and no extra network call.

### Linear API (`crates/devkit-common/src/linear.rs`)

Three additions, mirroring the existing query/parse/orchestrate split
(`build_query` / `fetch` / `states`):

- `issues_for_prs_queries(urls: &[String]) -> Vec<(String, serde_json::Value, HashMap<String, String>)>`
  — pure, testable. Chunks the URLs (25 per request, staying under Linear's
  query-complexity budget) and builds per chunk
  `query($u0: String!, …) { a0: attachmentsForURL(url: $u0) { nodes { issue { identifier } } } … }`
  plus the `variables` object and the alias→url map; empty input yields an
  empty vec. URLs travel as GraphQL variables, never spliced into the query
  string — sidesteps the escaping/injection hazard `parse_id` guards against
  for ids.
- `parse_issues_for_prs(resp, aliases) -> HashMap<String, Vec<String>>` —
  pure. Per alias, collects `nodes[].issue.identifier`, skipping attachments
  whose `issue` is null and deduplicating within a PR (an issue can carry two
  attachments to the same PR).
- `issues_for_prs(urls: &[String], key: Option<&str>) -> HashMap<String, Vec<String>>`
  — orchestrator, fail-soft like `states`: empty map when the key is absent
  or `urls` is empty; on error, log one `eprintln!` line and return what
  resolved so far. Sends each chunk payload and merges the results into one
  map.

A PR URL absent from the map (not linked in Linear, or its chunk failed)
simply contributes no ids — the text-derived id stands alone, which is
today's behavior.

### Data layer (`crates/devkit-issue/src/prs.rs`)

`issue_id: String` becomes `issue_ids: Vec<String>` on both `MinePrView` and
`ReviewPrView`, with `#[serde(default)]`. Empty vec replaces the `"-"`
sentinel. `classify` keeps using `issue_of` and produces a 0-or-1-element
vec; it does no Linear work.

`gather` gains a flag:

```rust
pub fn gather(root, mine, reviews, repo, ignored_checks, resolve_pr_links: bool) -> Result<PrsReport>
```

When the flag is set, after `classify`, `gather` resolves the key via
`devkit_common::secrets::resolve("LINEAR_API_KEY")`, collects the PR URLs
from both view vecs, calls `linear::issues_for_prs` (one batched call for
mine + reviews together), and applies the merge via a pure helper:

```rust
fn merge_linked(ids: &mut Vec<String>, linked: &[String])
```

**Merge rule — union, deduplicated:** the text-derived id stays first, the
Linear-linked ids follow in response order, duplicates removed
case-insensitively (text ids are already uppercased; Linear identifiers are
canonical). A PR with branch id `ENG-123` and Linear links `ENG-123`,
`SWE-6` renders `ENG-123 SWE-6`. Information is never dropped: a text id
Linear doesn't know about is kept, and Linear links the text path missed are
added.

The Linear call runs inside `gather`, sequentially after the GitHub fetch —
it needs the PR URLs, so unlike `workspace_url_key` it cannot overlap the
GitHub round trip. Cost when enabled: one extra HTTPS round trip per 25 PRs.
`fetch_report`'s thread structure in the binary is unchanged (the
`workspace_url_key` thread still runs concurrently with all of `gather`).

`devkit-issue` stays decoupled from `devkit-ports`: the flag is read from
config by each caller and passed down as a bool, exactly like
`ignored_checks`.

### Callers

Both `gather` call sites load config already; each additionally reads
`linear.resolve_pr_links` from the same `load()` result:

- `src/bin/issue/prs.rs::run` — one `load()`, extract `ignored_checks` and
  the flag together.
- `crates/devkit-mcp/src/issue.rs::prs_handler` — same. The MCP `issue.prs`
  JSON output changes shape: `issue_id: "ENG-1"` → `issue_ids: ["ENG-1"]`
  (empty array for none). Deliberate break; the action's consumers are
  agents reading fresh output, and one field name change beats carrying a
  redundant legacy field.

### Rendering (`src/bin/issue/prs.rs`)

`issue_cell(issue_ids: &[String], url_key)` replaces the single-id version:
empty → dim `-`; otherwise each id becomes a cyan OSC-8 link to
`https://linear.app/{url_key}/issue/{id}` (plain cyan text without a url
key), space-joined into the one ISSUE cell. Both `mine_table_build` and
`reviews_table_build` pass `&pr.issue_ids`. The ISSUE column takes no part
in `diff_cell`, so no diff handling changes.

### Snapshot cache

The views are persisted in `~/.cache/devkit/pr-status/<repo>.json`. Old
snapshots have `issue_id` (ignored as an unknown field) and lack
`issue_ids` (rescued by `#[serde(default)]` to empty), so the whole-struct
parse still succeeds — the stale pre-fetch render shows `-` in ISSUE for
one run, then the fresh fetch repopulates it. No migration needed.

### Docs

`docs/configuration.md` gains a `### [linear]` section modeled on
`### [harness]`: a `| Key | Type | Default | Meaning |` row for
`resolve_pr_links` plus prose noting the key comes from
`LINEAR_API_KEY` (env or `secrets.toml`), the union/dedup merge, and the
one-extra-round-trip cost.

## Error handling

- Flag off (default): no Linear call, behavior identical to today.
- Flag on, no `LINEAR_API_KEY`: `issues_for_prs` returns empty; text ids
  render as today. No warning — same silent degradation as `states`.
- Flag on, network/API error: one stderr line, partial or empty map,
  command still succeeds.
- `issue prs` never fails because of Linear.

## Out of scope

- `issue status` / `issue dashboard` — their ids come from worktree branch
  names, which always carry the id by construction.
- Reverse lookup (issue → PRs); `issue_pr` already covers that direction.
- Caching Linear link results between runs.
- A CLI flag override (`--resolve-pr-links`); config-only until someone
  needs per-run control.

## Tests (TDD, failing first)

`crates/devkit-common/src/linear.rs`:

- `issues_for_prs_queries`: aliases one variable per URL, URLs land in
  `variables` (not the query string), empty input → empty vec, 26 URLs →
  two payloads.
- `parse_issues_for_prs`: multi-issue PR collects all identifiers; null
  `issue` attachments skipped; duplicate identifiers within a PR deduped;
  alias missing from response → no entry.

`crates/devkit-issue/src/prs.rs`:

- `merge_linked`: text id + disjoint links → union in order; overlapping →
  deduped; empty text side → links only; empty links → text unchanged.
- `classify` tests updated: `issue_id == "ENG-1"` → `issue_ids ==
  vec!["ENG-1"]`, the `-` case → empty vec.

`src/bin/issue/prs.rs`:

- `issue_cell`: empty → dim `-`; two ids → two links, space-joined.
- snapshot compat: a JSON snapshot with the old `issue_id` field parses
  (whole-struct) with empty `issue_ids`.

Manual verification: `issue prs` against the monorepo with
`resolve_pr_links = true`, confirming previously `-` rows resolve and a
multi-issue PR shows all ids.

## Resolved decisions

1. Merge rule: union of text-derived and Linear-linked ids, deduplicated,
   text id first.
2. Config key: `[linear] resolve_pr_links`, boolean, default `false`.
3. MCP `issue.prs` output: `issue_id` renamed to `issue_ids` (array), no
   legacy alias.
