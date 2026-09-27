# Issue Creation With Enforced Templates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Issues agents file come out of the project's `issue_title`/`issue_body` templates, through `devkit issue create` on GitHub or through a tracker MCP call the pre-tool-use hook gates on render receipts.

**Architecture:** `issue render` runs the existing `check_required` over two new templates, prints `{title, body}` and writes one empty receipt file per field under `<checkout>/.devkit/issue-receipts/<session>/`. A new MCP branch in `pre_tool_use`, driven by `[harness.issue_tools]` rules, allows an issue-writing MCP call only when every gated field has a receipt. `issue create` renders the same way and calls `gh issue create`.

**Tech Stack:** Rust 2024 workspace, clap, minijinja (via `devkit_common::template`), pabal (hook payloads), serde/toml/schemars, nextest.

**Spec:** `docs/superpowers/specs/2026-09-27-issue-create-design.md`

## Global Constraints

- Read `AGENTS.md` first; its Rules and Conventions bind every task.
- No hook verb exits 2; only `pre-tool-use` writes stdout; the MCP branch writes no harness log record.
- Repository-scoped `gh` goes through `cmd::gh_capture`; git through `devkit_common::git`.
- Help text is ASCII. Config key meaning lives in the field's doc comment; regenerate `schema/devkit-config.json` with `DEVKIT_UPDATE_SCHEMA=1 cargo test` after any config type change.
- Receipt filenames contain no `:`. Receipt path: `<checkout>/.devkit/issue-receipts/<session>/title-<hex>` and `body-<hex>`, `<hex>` = 64 lowercase hex chars of SHA-256 over the normalized text.
- A valid session id is non-empty and only ASCII letters, digits, `-`, `_`.
- Stale receipt sweep age: seven days.
- Commit through `devrun task commit --arg files=<comma-separated> --arg commit_subject='<type(scope): subject>' --arg coauthors='<Model> <noreply@anthropic.com>'`. Conventional Commits, subject at most 50 chars.
- Before each task's commit: `devrun task fmt`, then `cargo clippy --workspace --all-targets -- -D warnings`.

## Review Focus

1. A title or body field that is present but not a string (a number, an object) in an MCP call: a matched call denies, naming the field; it is never hashed as its JSON text.
2. Two harness session variables set to different ids (`CLAUDE_CODE_SESSION_ID` and `CODEX_SESSION_ID`): `issue render` writes receipts under both, and a hook payload carrying either id is allowed.
3. `issue render` with `--body` omitted: the body renders as the default template over an empty input, a receipt for that body is written, and a create call with no `description` is allowed.
4. Non-ASCII text (emoji, accented letters) in title and body: the rendered JSON round-trips into an allowed hook call byte for byte.
5. `.devkit` existing as a regular file in the checkout: `issue render` fails with an error naming the path, and a matched hook call denies.

---

### Task 1: `issue_title` and `issue_body` templates

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (`Templates` struct near line 1280, defaults near line 1068, accessors near line 1350)
- Modify: `crates/devkit-ports/src/templates.rs` (the `BuiltIn` list near line 84)
- Modify: `schema/devkit-config.json` (regenerated)

**Interfaces:**
- Produces: `devkit_config::DEFAULT_ISSUE_TITLE: &str = "{{ input }}"`, `devkit_config::DEFAULT_ISSUE_BODY: &str = "{{ input }}"`, `Templates::issue_title(&self) -> &str`, `Templates::issue_body(&self) -> &str`, fields `Templates.issue_title: Option<String>`, `Templates.issue_body: Option<String>`.

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-config/src/lib.rs` tests, next to the test asserting `t.pr_title() == DEFAULT_PR_TITLE` (near line 3159), add `issue_templates_default_to_the_input`: `Templates::default()` gives `issue_title() == "{{ input }}"` and `issue_body() == "{{ input }}"`, and `Config::parse("[templates]\nissue_body = \"x {{ input }}\"\n")` gives `issue_body() == "x {{ input }}"`.

In `crates/devkit-ports/src/templates.rs` tests, add `the_issue_templates_are_built_ins`: the catalog's names contain `issue_title` and `issue_body` exactly once each (same shape as the existing `pr_body` count assertion near line 251).

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --workspace -E 'test(issue_templates_default_to_the_input) | test(the_issue_templates_are_built_ins)'`
Expected: compile error, `no method named issue_title`.

- [ ] **Step 3: Add the fields, defaults, accessors and two `BuiltIn` entries**

Doc comments (they become schema descriptions):
- `issue_title`: "Title of an issue rendered by `issue render` or created by `issue create`. `{{ input }}` is the `--title` argument; the rendered title must not be empty."
- `issue_body`: "Body of an issue rendered by `issue render` or created by `issue create`. `{{ input }}` is the `--body` argument and `issue_title` is the rendered title. A `[templates.variables]` entry either template reads, marked `required`, must be passed as `--arg`."

`BuiltIn` descriptions: "Title of an issue `issue render` or `issue create` writes" and "Body of an issue `issue render` or `issue create` writes".

- [ ] **Step 4: Regenerate the schema and run the tests**

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test -p devkit-config`, then the Step 2 command.
Expected: PASS; `git diff schema/devkit-config.json` shows the two new properties.

- [ ] **Step 5: Commit** with subject `feat(config): add issue title and body templates`.

---

### Task 2: `[harness.issue_tools]` rules

**Files:**
- Modify: `crates/devkit-config/src/harness.rs` (new `IssueToolRule` after `CommandRule`; new `HarnessSection.issue_tools` after `commands`; `wildcard_matches` moved here as `pub fn`)
- Modify: `crates/devkit-config/src/lib.rs` (re-export `IssueToolRule` and `wildcard_matches` beside `CommandRule`)
- Modify: `crates/devkit-ports/src/guard/mod.rs:113` (delete the private `wildcard_matches`, call `devkit_config::wildcard_matches`)
- Modify: `crates/devkit-common/src/harness.rs` (`HarnessRules.issue_tools`, `merge_rules`)
- Modify: `schema/devkit-config.json` (regenerated)

`devkit-config` does not depend on `devkit-ports`, so the one `wildcard_matches` definition moves down into `devkit-config` and the guard's existing `wildcard_matching` test keeps covering it through the guard.

**Interfaces:**
- Produces:

```rust
pub fn wildcard_matches(pattern: &str, text: &str) -> bool;   // body moved unchanged

#[derive(Deserialize, Debug, Clone, PartialEq, schemars::JsonSchema)]
pub struct IssueToolRule {
    #[serde(default)] pub servers: Vec<String>,
    #[serde(default)] pub tools: Vec<String>,
    #[serde(default)] pub absent: Vec<String>,
    #[serde(default)] pub equals: BTreeMap<String, String>,
    pub title: String,
    pub body: String,
    #[serde(default)] pub body_patch: Vec<String>,
    #[serde(default = "enabled_default")] pub enabled: bool,
}
impl IssueToolRule {
    /// Tool in `tools` and server matching a `servers` pattern (both sides
    /// lowercased, `wildcard_matches`); empty `servers` matches any server,
    /// including `None`.
    pub fn matches(&self, server: Option<&str>, tool: &str) -> bool;
    /// Every `absent` key missing or null and every `equals` pair holding.
    pub fn is_create(&self, input: &serde_json::Value) -> bool;
}
```

`HarnessRules.issue_tools: BTreeMap<String, IssueToolRule>`.

- [ ] **Step 1: Write the failing tests**

Doctest on `IssueToolRule` parsing the two shipped entries verbatim from the spec's "Hook enforcement of MCP writes" section, asserting: `linear.matches(Some("claude.ai Linear"), "save_issue")`, `linear.matches(Some("claude_ai_Linear"), "save_issue")`, `!linear.matches(Some("claude.ai Linear"), "get_issue")`, `github.matches(Some("claude.ai GitHub"), "issue_write")`, `linear.is_create(&json!({"title": "t"}))`, `!linear.is_create(&json!({"id": "ENG-1"}))`, `linear.is_create(&json!({"id": null}))`, `github.is_create(&json!({"method": "create"}))`, `!github.is_create(&json!({"method": "update"}))`, `linear.body_patch == ["patch"]`, and that a rule with `tools = []` matches nothing.

In `crates/devkit-common/src/harness.rs` tests, add `issue_tools_merge_by_name_and_a_child_disables_one` (root layer declares `[harness.issue_tools.linear]` with all keys, child layer sets only `enabled = false`; the merged rule keeps `tools == ["save_issue"]` and has `enabled == false`) and `a_malformed_issue_tool_is_skipped_with_a_warning` (a rule missing `title` is absent from the map and one warning names `[harness.issue_tools.<name>]`).

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p devkit-config --doc IssueToolRule` and `cargo nextest run -p devkit-common -E 'test(issue_tool)'`
Expected: compile errors, `IssueToolRule` not found.

- [ ] **Step 3: Implement the type, the move, the `HarnessSection` field, and the merge**

`merge_rules` handles `issue_tools` with the same per-entry `try_into` loop `commands` uses, including the "expected a table" warning; serde's required `title`/`body` is the only completeness check. Field doc comments carry the meanings in the spec's `IssueToolRule` field list; the `HarnessSection.issue_tools` doc adds "Enforces on its own presence, independent of `enforce_commands`."

- [ ] **Step 4: Regenerate the schema and run the tests**

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test -p devkit-config`, the Step 2 commands, and `cargo nextest run -p devkit-ports -E 'test(wildcard)'`
Expected: all PASS.

- [ ] **Step 5: Commit** with subject `feat(harness): add issue_tools rules for MCP calls`.

---

### Task 3: Receipt store

**Files:**
- Create: `src/bin/devkit/issue/receipt.rs`
- Modify: `src/bin/devkit/issue/mod.rs` (`pub(crate) mod receipt;`)

**Interfaces:**
- Produces:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Field { Title, Body }          // file prefixes "title-" and "body-"
pub(crate) const STALE_AFTER: Duration;        // 7 days
pub(crate) fn normalize(text: &str) -> String;
pub(crate) fn hex(text: &str) -> String;       // digest of normalize(text) with the "sha256:" prefix stripped
pub(crate) fn valid_session(id: &str) -> bool;
pub(crate) fn sessions_from_env() -> Vec<String>;   // distinct non-empty HARNESS_SESSION_VARS values, table order
pub(crate) fn session_dir(checkout: &Path, session: &str) -> PathBuf; // <checkout>/.devkit/issue-receipts/<session>
pub(crate) fn write(checkout: &Path, session: &str, title: &str, body: &str) -> Result<()>;
pub(crate) fn has(checkout: &Path, session: &str, field: Field, text: &str) -> Result<bool>;
pub(crate) fn clear_session(checkout: &Path, session: &str) -> Result<()>;
pub(crate) fn sweep_stale(checkout: &Path, older_than: Duration) -> Result<()>;
```

`hex` wraps `devkit_common::harness_log::redact::digest`. `write` bails on an invalid session, creates the session dir, calls `devkit_common::gitignore::write_self_ignore` on `<checkout>/.devkit`, and creates both empty files. `has` errors on an invalid session. `clear_session` on an invalid session or a missing dir is `Ok(())` and deletes nothing. `sweep_stale` removes session dirs whose mtime is older than `older_than`; a missing receipts dir is `Ok(())`.

- [ ] **Step 1: Write the failing unit tests** (in `receipt.rs`, each using a `tempfile::tempdir()` as the checkout)

- `normalize_folds_line_endings_and_edge_whitespace`: `normalize("a  \r\nb\t\r\n\r\n") == "a\nb"`, `normalize("  x ") == "x"`.
- `hex_is_colon_free_lowercase_sha256`: `hex("x").len() == 64`, no `:`, all `[0-9a-f]`, `hex("a\r\n") == hex("a")`.
- `session_ids_are_path_safe`: valid `"f18b31aa-9b8c"`, `"a_B-9"`; invalid `""`, `".."`, `"../x"`, `"a/b"`, `"a\\b"`, `"a:b"`, `"é"`.
- `a_written_pair_is_found_field_by_field`: after `write(dir, "S", "T", "B")`, `has(.., "S", Title, "T")` and `has(.., "S", Body, "B ")` are true, `has(.., "S", Body, "T")` and `has(.., "S2", Title, "T")` false, and `<dir>/.devkit/.gitignore` exists.
- `write_refuses_an_invalid_session`: `write(dir, "../x", "T", "B")` errors and creates nothing under `dir`.
- `clear_session_removes_only_that_session`: after writes under `S1` and `S2`, `clear_session(dir, "S1")` leaves only `S2`; `clear_session(dir, "..")` is `Ok` and leaves `S2`.
- `sweep_drops_only_old_sessions`: set one session dir's mtime 8 days back (`filetime` if it is already a dev-dependency, else `std::fs::File::set_modified` on the opened dir); `sweep_stale(dir, STALE_AFTER)` removes it and keeps a fresh one.
- `a_devkit_file_fails_the_write` (Review Focus 5): with `<dir>/.devkit` a regular file, `write` errors and the message names `.devkit`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --workspace -E 'binary(devkit) & test(/receipt::/)'`
Expected: compile error, unresolved items in `receipt`.

- [ ] **Step 3: Implement the module per the Interfaces block.**

- [ ] **Step 4: Run the tests.** Same command. Expected: PASS.

- [ ] **Step 5: Commit** with subject `feat(issue): add the issue render receipt store`.

---

### Task 4: `devkit issue render`

**Files:**
- Create: `src/bin/devkit/issue/render.rs`
- Modify: `src/bin/devkit/issue/mod.rs` (`Cmd::Render`, dispatch in `run`)
- Test: `tests/issue_render.rs`

**Interfaces:**
- Consumes: Task 1 accessors; Task 3 `receipt::{write, sweep_stale, sessions_from_env, STALE_AFTER}`; `issue::review::{parse_args, render_review, with_fields}`; `devkit_common::required::{missing_args, ensure_supplied, Missing}`; `devkit_common::template::undeclared`; `devkit_common::caller::caller()`.
- Produces:

```rust
pub(crate) const ISSUE_CONTEXT_KEYS: &[&str] = &["input", "issue_title"];
#[derive(Debug, Serialize)]
pub(crate) struct Rendered { pub title: String, pub body: String }
/// The required args the two issue templates read that `given` does not supply.
pub(crate) fn missing(cfg: &Config, given: &BTreeMap<String, String>, caller: Caller) -> Result<Vec<Missing>>;
/// Refuse on `missing`, then render the title over {input} and the body over
/// {input, issue_title}; an empty rendered title bails "--title is required".
pub(crate) fn render(cfg: &Config, title: &str, body: &str, vars: &VarArgs, caller: Caller) -> Result<Rendered>;
pub(crate) struct RenderArgs { pub title: String, pub body: Option<String>, pub vars: VarArgs, pub dir: Option<String>, pub config: Option<String> }
pub(crate) fn run(args: RenderArgs) -> Result<()>;
```

`missing` is the body of `check_required` returning the list instead of refusing; `render` passes it to `ensure_supplied("issue render", ..)`.

`Cmd::Render { #[arg(long)] title: String, #[arg(long)] body: Option<String>, #[command(flatten)] vars: VarArgs }`, help: "Render an issue title and body from the issue templates, for a tracker MCP call."

`run` follows the spec's `issue render` steps in order: load config, render, `sessions_from_env()`; if empty, `eprintln!("no agent session: no receipt written")`; otherwise `checkout_root(start)?`, `sweep_stale(.., STALE_AFTER)` with its error ignored, then `write` per session with errors propagated. Last, print `serde_json::to_string(&rendered)`.

- [ ] **Step 1: Write the failing integration tests** in `tests/issue_render.rs`

Helper `render(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Output` spawns `CARGO_BIN_EXE_devkit issue render ...` in `dir` with `testenv::scrub_identity`, private `HOME`/`XDG_STATE_HOME`, `DEVKIT_SKIP_AUTOLINK=1`, `DEVKIT_CALLER=agent` unless `env` overrides it, then each `env` pair. Project fixture: a `git init` temp dir whose `devkit.toml` is the spec's `[templates]` example.

- `a_missing_required_arg_is_refused_by_name`: no `--arg acceptance`: exit non-zero, stderr contains `--arg acceptance=...` and `observable outcomes`.
- `render_prints_json_and_writes_both_receipts`: `CLAUDE_CODE_SESSION_ID=S1`, `--title T --body B --arg acceptance=A`: stdout JSON has `title == "T"` and a `body` starting with `B` and containing `## Acceptance criteria\nA`; `.devkit/issue-receipts/S1/` holds exactly `title-<64 hex>` and `body-<64 hex>`.
- `two_harness_ids_each_get_receipts` (Review Focus 2): `CLAUDE_CODE_SESSION_ID=S1` and `CODEX_SESSION_ID=S2`: both session dirs exist.
- `an_omitted_body_still_gets_a_receipt` (Review Focus 3): no `--body`: exit 0 and a `body-` file exists under `S1`.
- `no_session_renders_without_receipts_outside_a_checkout`: non-git temp dir, no session var, `DEVKIT_CALLER=human`: exit 0, stdout is the JSON, stderr contains `no receipt`, no `.devkit` created.
- `an_invalid_session_id_is_refused`: `CLAUDE_CODE_SESSION_ID=../x`: exit non-zero.
- `an_empty_title_is_refused`: `--title "  " --arg acceptance=A`: exit non-zero, stderr contains `--title is required`, no receipt written.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --workspace -E 'binary(issue_render)'`
Expected: FAIL, clap reports an unrecognized subcommand `render`.

- [ ] **Step 3: Implement `render.rs` and wire `Cmd::Render`.** Add the unit test `the_issue_context_keys_match_what_render_builds`, asserting the keys of the two contexts `render` builds equal `ISSUE_CONTEXT_KEYS`, the way `the_context_keys_match_what_each_surface_builds` does for PRs.

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run --workspace -E 'binary(issue_render) | test(the_issue_context_keys)'`
Expected: PASS.

- [ ] **Step 5: Commit** with subject `feat(issue): add issue render`.

---

### Task 5: `devkit issue create` on GitHub

**Files:**
- Create: `src/bin/devkit/issue/create.rs`
- Modify: `src/bin/devkit/issue/mod.rs` (`Cmd::Create`, dispatch)
- Modify: `examples/ghfake.rs` (`canned`: `issue create` answers from `issue_create.txt`, fallback `https://github.com/o/r/issues/42\n`)
- Modify: `tests/common/ghfake.rs` (`build` writes `issues_repo = "o/r"` beside `pr_repo`)
- Test: `tests/issue_create.rs`

**Interfaces:**
- Consumes: Task 4 `render::render`; `issue::tracker::select(config, start, None) -> (Resolved, Repos)`; `Repos::issues() -> Result<&Repo>`; `cmd::gh_capture(args, repo, cwd) -> Result<String>`.
- Produces: `pub(crate) struct CreateArgs { pub title: String, pub body: Option<String>, pub vars: VarArgs, pub dir: Option<String>, pub config: Option<String> }`, `pub(crate) fn run(args: CreateArgs) -> Result<()>`. `Cmd::Create` takes the same three flags as `Cmd::Render`; help: "Create a GitHub issue from the issue templates."

Order: select the tracker first. A kind other than `Github` bails with: "`issue create` writes GitHub issues only, and this project's tracker is <kind>. Render the issue with `devkit issue render` and create it through the tracker's MCP." Then `render` (no receipts), then `gh issue create --title <t> --body <b>` via `gh_capture` against `repos.issues()?`, then print the last output line containing `://`, trimmed.

- [ ] **Step 1: Write the failing integration tests** in `tests/issue_create.rs`

Use `ghfake::Fake::without_pr(extra)`, where `extra` closes `[defaults]` by opening `[tracker]\nkind = "github"` followed by the spec's `[templates]` example.

- `a_missing_required_arg_is_refused_before_gh_runs`: `fake.issue(&["create", "--title", "T"])` fails, stderr names `acceptance`, `fake.calls()` lacks `issue create`.
- `create_passes_the_rendered_text_to_gh`: with `--arg acceptance=A`: success, stdout contains `https://github.com/o/r/issues/42`, `fake.calls()` contains `issue create --title T --body` and `## Acceptance criteria`.
- `a_linear_tracker_is_pointed_at_render`: `kind = "linear"`: fails, stderr contains `devkit issue render`, `fake.calls()` lacks `issue create`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --workspace -E 'binary(issue_create)'`
Expected: FAIL, unrecognized subcommand `create`.

- [ ] **Step 3: Implement `create.rs`, the ghfake answer, and the fixture's `issues_repo`.**

- [ ] **Step 4: Run the new tests and every ghfake consumer**

Run: `cargo nextest run --workspace -E 'binary(issue_create) | binary(pr_create_render) | binary(pr_ready_gate) | binary(review_request_order) | binary(template_cmd) | binary(origin_remote)'`
Expected: PASS.

- [ ] **Step 5: Commit** with subject `feat(issue): add issue create for GitHub`.

---

### Task 6: MCP branch of `pre-tool-use`

**Files:**
- Create: `src/bin/devkit/hook/mcp.rs`
- Modify: `src/bin/devkit/hook/mod.rs` (`mod mcp;`, dispatch in `pre_tool_use`)
- Test: `tests/hook_mcp_issue.rs`

**Interfaces:**
- Consumes: Task 2 `HarnessRules.issue_tools`, `IssueToolRule::{matches, is_create}`; Task 3 `receipt::{has, valid_session, session_dir, Field}`; Task 4 `render::missing`; `harness::resolve_rules_in`; `record::payload_cwd`; `Checkout::at(&cwd).root()`; `Harness::deny(reason) -> String`; `shell::print_envelope`.
- Produces:

```rust
pub(super) fn guard(payload: &Payload) -> Result<()>;   // never errors; prints a deny envelope or nothing
#[derive(Debug, PartialEq)]
pub(super) enum Verdict { Allow, Deny(String) }
/// The decision for one matched rule. `receipt` answers whether `text` has a
/// receipt for the field in this session.
pub(super) fn decide(
    rule: &IssueToolRule, server: &str, tool: &str, input: &Value,
    receipt: &dyn Fn(Field, &str) -> Result<bool>,
) -> Result<Verdict>;
```

Dispatch in `pre_tool_use`: the edit check, then `Some(Tool::Mcp { .. }) => mcp::guard(&payload)`, else `shell::guard`. `guard` runs `respond` under `catch_unwind` with a `matched: OnceLock<()>` set once a rule matches; a panic with `matched` set prints `deny("devkit issue guard: internal failure while checking an issue write (fail-closed)")`, and without it prints nothing. `respond` resolves rules at `payload_cwd` (warnings ignored) and takes the first enabled rule by name that `matches`; none means `Allow`. After a match: a missing or invalid session id, or no checkout root, denies with a reason naming which; otherwise `decide`, and an `Err` from it denies with the error text.

`decide` follows the spec's create and update rules. A create checks both fields, missing or null counting as `""`. An update denies on any `body_patch` key, then checks only the fields present. A present field that is neither a string nor null denies with "`<key>` is not a string" (Review Focus 1). Reasons:
- no receipt: "This `<server>` `<tool>` call writes an issue `<key>` that `devkit issue render` did not produce in this session. Run `devkit issue render --title ... [--body ...]` and pass its `title` output unchanged as `<title key>` and its `body` output as `<body key>`." When `render::missing(cfg, &BTreeMap::new(), Caller::Agent)` loads and is non-empty, append "Pass:" with each `Missing::hint()` and a `required::description_line` per described arg.
- session dir exists but the text has no receipt: the first sentence becomes "This call's `<key>` differs from what `devkit issue render` produced in this session."
- `body_patch`: "This call edits the issue body in place with `<patch key>`. Render the whole new body with `devkit issue render` and pass it as `<body key>`."

- [ ] **Step 1: Write the failing unit tests for `decide`** (in `mcp.rs`, with the shipped `linear` and `github` rules parsed from TOML and a `receipt` closure over a `HashSet<(Field, String)>`)

- `a_create_needs_both_fields`: title receipted, body not: `Deny` mentioning `description`.
- `a_create_without_description_checks_the_empty_body` (Review Focus 3): `{"title": "T"}` with `(Title, "T")` and `(Body, "")` receipted: `Allow`.
- `an_update_touching_neither_field_is_allowed`: `{"id": "ENG-1", "state": "Done"}`, empty set: `Allow`.
- `an_update_rewriting_the_body_needs_its_receipt`: `{"id": "ENG-1", "description": "x"}`: `Deny`; with `(Body, "x")`: `Allow`.
- `a_patch_is_denied`: `{"id": "ENG-1", "patch": []}`: `Deny` mentioning `patch`.
- `a_non_string_field_is_denied` (Review Focus 1): `{"title": 5}`: `Deny` mentioning `title`.
- `github_update_without_fields_is_allowed`: `{"method": "update", "issue_number": 1, "state": "closed"}`: `Allow`.

- [ ] **Step 2: Write the failing integration tests** in `tests/hook_mcp_issue.rs`

Copy the `run_hook_as` shape from `tests/hook_edit_verdict.rs`; test binaries do not share it. Project fixture: `git init` plus a `devkit.toml` with the spec's `[templates]` example and both shipped `[harness.issue_tools]` entries. Helper `render_as(project, var, session, title, body) -> (String, String)` runs `devkit issue render` with `<var>=<session>`, `DEVKIT_CALLER=agent` and `--arg acceptance=A`, and returns the parsed title and body. Claude payload: `{"hook_event_name": "PreToolUse", "session_id": S, "cwd": <project>, "tool_name": "mcp__claude_ai_Linear__save_issue", "tool_input": {...}}`. A denial is stdout parsing as JSON with `hookSpecificOutput.permissionDecision == "deny"`; an allow is empty stdout. Every case also asserts exit code 0.

- `an_unrendered_create_is_denied_with_the_render_command`: reason contains `devkit issue render` and `--arg acceptance=...`.
- `a_rendered_create_is_allowed`
- `crlf_and_trailing_spaces_still_match`
- `a_changed_word_is_denied_as_differing`: reason contains `differs`.
- `another_sessions_receipt_does_not_count`
- `a_traversal_session_id_is_denied`: payload `session_id` is `"../x"`.
- `an_update_with_state_only_is_allowed`
- `a_patch_update_is_denied`
- `an_unmatched_mcp_tool_is_silent`: `mcp__mcpls__get_hover` gives empty stdout, which proves it never reached the shell guard's unusable-payload denial.
- `github_create_through_the_hosted_connector_is_gated`: `mcp__claude_ai_GitHub__issue_write` with `"mcp_server": {"name": "claude.ai GitHub"}`, `method: "create"`, no receipt: denied.
- `non_ascii_text_round_trips` (Review Focus 4): title `"Fix: café ☕"`, body `"naïve 🚀"`, rendered then sent: allowed.
- `a_devkit_file_denies_a_matched_call` (Review Focus 5): `.devkit` a regular file: denied.
- `a_second_harness_id_is_honoured` (Review Focus 2): rendered with both `CLAUDE_CODE_SESSION_ID=S1` and `CODEX_SESSION_ID=S2`; a Claude payload with session `S2` is allowed.
- `codex_payloads_are_gated_too`: `--harness codex`, payload `{"hook_event_name": "PreToolUse", "turn_id": "t1", "session_id": S, "cwd": ..., "tool_name": "mcp__linear__save_issue", "tool_input": {...}}`, rendered with `CODEX_SESSION_ID=S`: denied before the render, allowed after.

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo nextest run --workspace -E 'binary(hook_mcp_issue) | test(/mcp::tests/)'`
Expected: unit tests fail to compile (`decide` missing); `an_unmatched_mcp_tool_is_silent` fails on the shell guard's denial in stdout.

- [ ] **Step 4: Implement `mcp.rs` and the dispatch.** Update the `pre_tool_use` doc comment to name its three paths.

- [ ] **Step 5: Run the tests and the existing hook suites**

Run: `cargo nextest run --workspace -E 'binary(hook_mcp_issue) | test(/mcp::tests/) | binary(hook_edit_verdict) | binary(hook_exit_codes) | binary(harness_guard) | binary(harness_shell_writes)'`
Expected: PASS.

- [ ] **Step 6: Commit** with subject `feat(hook): gate issue writes through tracker MCPs`.

---

### Task 7: Clear receipts at session end

**Files:**
- Modify: `src/bin/devkit/hook/mod.rs` (the `HookEvent::SessionEnd` arm)
- Test: `tests/hook_mcp_issue.rs`

**Interfaces:**
- Consumes: Task 3 `receipt::clear_session`; `record::payload_cwd`; `Checkout::at(..).root()`.

- [ ] **Step 1: Write the failing tests** in `tests/hook_mcp_issue.rs`, driving `devkit hook session-end --harness claude-code` with `{"hook_event_name": "SessionEnd", "session_id": S, "cwd": <project>}`

- `session_end_clears_only_its_receipts`: after renders under `S1` and `S2`, session end for `S1` leaves only `.devkit/issue-receipts/S2`; exit 0, empty stdout.
- `session_end_ignores_a_traversal_id`: session id `".."`: every file under the project survives, exit 0.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --workspace -E 'binary(hook_mcp_issue) & test(session_end)'`
Expected: `session_end_clears_only_its_receipts` FAILS with `S1` still present.

- [ ] **Step 3: Call `receipt::clear_session` in the `SessionEnd` arm after `edit::release_session`, discarding its result.**

- [ ] **Step 4: Run the tests.** Same command. Expected: PASS.

- [ ] **Step 5: Commit** with subject `feat(hook): clear issue receipts at session end`.

---

### Task 8: Manifests, devkit's own config, docs

**Files:**
- Modify: `plugin/hooks/hooks.json` (the Claude `PreToolUse` matcher becomes `Edit|MultiEdit|Write|NotebookEdit|Bash|PowerShell|mcp__.*`)
- Modify: `plugin/hooks/hooks-codex.json` (the matcher becomes `apply_patch|Write|Edit|Bash|mcp__.*`)
- Modify: `tests/hook_manifests.rs`
- Modify: `devkit.toml` (append the two `[harness.issue_tools]` entries and `[harness.commands.gh-issue-create]` verbatim from the spec)
- Modify: `plugin/skills/using-devkit/references/issues.md`

- [ ] **Step 1: Write the failing manifest test**

In `tests/hook_manifests.rs`, `mcp_tools_reach_pre_tool_use`: for the Claude and Codex manifests, the single `PreToolUse` matcher has an alternative exactly equal to `mcp__.*` (`split('|')`), and still contains `Bash` and `Write`. Update the doc comment on `the_merged_pre_tool_use_block_keeps_a_matcher`, which currently lists MCP among the tools the matcher keeps out.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run --workspace -E 'binary(hook_manifests)'`
Expected: `mcp_tools_reach_pre_tool_use` FAILS.

- [ ] **Step 3: Update both manifests and `devkit.toml`.**

- [ ] **Step 4: Write the skill reference section** "Filing issues" in `references/issues.md`: for an MCP tracker, run `devkit issue render --title ... --body ... --arg ...` and pass the printed `title` and `body` unchanged to the tracker's MCP; on GitHub, `devkit issue create` does both; rewriting an existing issue's title or body also goes through `render`, and a `patch` edit is refused; a denial names the missing args. Config: `[templates] issue_title`/`issue_body` with `required` variables, `[harness.issue_tools]` (on by presence), and the `gh issue create` command rule, which needs `enforce_commands = true`. No hard numbers.

- [ ] **Step 5: Run the manifest test and load devkit's own config**

Run: `cargo nextest run --workspace -E 'binary(hook_manifests)'`, then `cargo build` and `target/debug/devkit doctor` from the repo root.
Expected: PASS; doctor prints no warning mentioning `issue_tools`.

- [ ] **Step 6: Commit** with subject `feat(plugin): route MCP calls to the issue guard`.

---

### Task 9: Full gate

- [ ] **Step 1: Run the full gate**

Run: `devrun task verify`
Expected: formatting, clippy, nextest and doctests all pass.

- [ ] **Step 2: Fix anything it reports in the task that owns the code, re-run until green, and commit each fix under that task's scope.**
