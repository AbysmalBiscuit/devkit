# Hold Stop on Open Todos Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `devkit hook stop` and `devkit hook subagent-stop` refuse a stop, once per unchanged list, while the agent has open todos.

**Architecture:** pabal gains a Stop/SubagentStop `block` response. `devkit-todo` gains a pure open-todo rule and the fingerprint paths; `hook/todo.rs` gains `hold`, which `hook/mod.rs` calls in a new `Stop` arm and before the `SubagentStop` releases. Evals grade agent behaviour under the hold through new scenario-runner check kinds.

**Tech Stack:** Rust 2024 workspace, pabal (hook payloads), nextest, bash and jq eval runner.

**Spec:** `docs/superpowers/specs/2026-10-04-hold-stop-on-open-todos-design.md`

## Global Constraints

- Every hook path fails open: on any error the hold prints nothing and the agent stops.
- Only `pre-tool-use`, `stop` and `subagent-stop` write stdout; `user-prompt-submit` and `post-compact` stay silent, since stdout joins the prompt.
- No hook verb exits 2.
- `[todo] hold_stop`, bool, default `true`.
- The hold reads the local store and never syncs.
- Commits: `devrun task -C <repo> commit --arg files=<paths> --arg commit_subject='<type>(<scope>): <subject>' --arg coauthors='<model> <noreply@anthropic.com>'`. Raw `git commit` is blocked.
- Before each commit: `cargo nextest run --workspace --no-fail-fast`, `cargo clippy --workspace --all-targets -- -D warnings`, `devrun task fmt`.
- Evals are billed: build and dry-grade them, and run them only on Lev's go-ahead.
- Text written to files uses ASCII punctuation: a hook rejects Unicode ellipses, em dashes and arrows.

## Review Focus

1. A session whose claim sits on another node, because it changed branch, is still held at Stop. Test in Task 4.
2. A taskwarrior backend whose `task` binary is missing: the hold is silent. Test in Task 3.
3. A Stop payload without `session_id`: silent. Test in Task 4.
4. Re-arming on `user-prompt-submit` prints nothing to stdout. Test in Task 4.
5. Two sessions in one workspace: session B's pending todos never hold session A. Test in Task 4.

---

### Task 1: pabal `block` on Stop and SubagentStop

Repository: `/home/lev/Git/lev/pabal`. Its `main` is at 0.2.0 while devkit pins 0.2.1, which came from branch `14-feat-let-a-pretooluse-answer-rewrite`. Fetch first and base this work on the commit 0.2.1 was published from. Create the branch with `devkit issue setup --slug feat-stop-block` from the pabal checkout.

**Files:**
- Modify: `src/response.rs`, beside `Deny` and `any_add_context!`
- Test: alongside the crate's existing response tests

**Interfaces:**
- Produces: `pub trait Block { fn block(&self, reason: &str) -> Response; }`, implemented for the `Stop` and `SubagentStop` views of `ClaudeCode` and `Codex`, and for Cursor's `Stop`. `AnyStop::block(&self, reason: &str) -> Option<Response>`, `None` for Antigravity, and `AnySubagentStop::block(&self, reason: &str) -> Option<Response>`, `None` for Cursor.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn stop_block_per_harness() {
    let cc = Payload::<ClaudeCode>::parse(r#"{"hook_event_name":"Stop"}"#).unwrap();
    assert_eq!(cc.stop().unwrap().block("finish").to_string(), r#"{"decision":"block","reason":"finish"}"#);
    let cx = Payload::<Codex>::parse(r#"{"hook_event_name":"Stop","turn_id":"t"}"#).unwrap();
    assert_eq!(cx.stop().unwrap().block("finish").to_string(), r#"{"decision":"block","reason":"finish"}"#);
    let cu = Payload::<Cursor>::parse(r#"{"hook_event_name":"stop"}"#).unwrap();
    assert_eq!(cu.stop().unwrap().block("finish").to_string(), r#"{"followup_message":"finish"}"#);
}

#[test]
fn subagent_stop_block_is_none_on_cursor() {
    // a Cursor subagentStop payload, viewed as AnySubagentStop: block("x") is None
}

#[test]
fn a_blank_block_reason_becomes_a_fixed_one() {
    // Codex requires a non-blank reason: block("  ") carries "Blocked by a hook."
}
```

Plus a `compile_fail` doctest on `Block` showing `session_end().unwrap().block(..)` does not compile, as `Deny` has.

- [ ] **Step 2: Run, expect FAIL.** `cargo nextest run` in the pabal worktree: `block` not found.
- [ ] **Step 3: Implement** `Block`, the per-harness impls and the two `Any*::block` methods. Blank reasons go through a `nonblank`-style helper whose fixed text is `Blocked by a hook.`
- [ ] **Step 4: Run, expect PASS**, plus `cargo clippy --all-targets -- -D warnings`.
- [ ] **Step 5: Commit** `feat(response): let stop hooks block`. Push and open its PR on Lev's go-ahead; Task 4 needs the release.

### Task 2: the open-todo rule and fingerprint paths in `devkit-todo`

**Files:**
- Create: `crates/devkit-todo/src/hold.rs`
- Modify: `crates/devkit-todo/src/lib.rs`, for `pub mod hold;`

**Interfaces:**
- Produces:
  - `pub fn open_for<'a>(todos: &'a [Todo], holder: &Holder, pending_node: Option<&str>) -> Vec<&'a Todo>`: in-progress todos whose `by` equals `holder` exactly, plus pending todos whose `node()` equals `pending_node`. Ordered by node, then `order`.
  - `pub fn fingerprint(open: &[&Todo]) -> String`: one `"<id> <status kind>\n"` line per todo, sorted by id.
  - `pub fn hold_dir(session: &Holder) -> PathBuf`, which is `state_dir()/holds/<render::digest(session)>`.
  - `pub fn hold_path(holder: &Holder) -> PathBuf`, which is `hold_dir(<the holder's session part, before the first '/'>)/<render::digest(holder)>`.

- [ ] **Step 1: Write the failing unit tests in `hold.rs`**

```rust
#[test] fn counts_own_claims_on_any_node_and_pending_on_its_node() {
    // todos: pending on N, pending on another node, InProgress by "S" on another node,
    // InProgress by "S/a1" on N, Completed on N.
    // open_for(&todos, &Holder::new("S"), Some("N")) == [pending on N, the claim by S]
}
#[test] fn a_sub_agent_counts_only_its_claims() {
    // open_for(&todos, &Holder::new("S/a1"), None) == [the claim by S/a1]
}
#[test] fn no_pending_node_counts_no_pending() {}
#[test] fn fingerprint_ignores_order_and_tracks_status() {
    // the same todos in two orders give equal fingerprints; one status change gives a different one
}
#[test] fn a_sessions_and_its_sub_agents_holds_share_one_dir() {
    // hold_path("S/a1").parent() == Some(hold_dir("S")) == hold_path("S").parent()
}
```

- [ ] **Step 2: Run, expect FAIL.** `cargo nextest run -p devkit-todo hold`.
- [ ] **Step 3: Implement** the four functions.
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** `feat(todo): add the open-todo rule for stop holds`.

### Task 3: `[todo] hold_stop` and the hold's store

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (`TodoConfig`, its `Default`, its doctest), `schema/devkit-config.json` (regenerated), `src/bin/devkit/todo/store.rs`
- Test: `tests/todo_hold.rs`, new, with `#[path = "common/todoenv.rs"] mod todoenv;`

**Interfaces:**
- Produces:
  - `TodoConfig::hold_stop: bool`, default `true`. Doc comment: `Whether a stop hook sends an agent back to its open todos: its session's pending todos in a workspace and the todos it has in progress. One reminder per unchanged list; stopping again goes through.`
  - `Store::for_hold(checkout: &Checkout, cwd: &Path) -> Option<Store>`: `None` when the config fails to load, when `DEVKIT_TODO_BACKEND` is unknown, or when `hold_stop` is false. A missing config is the default config, as in `for_hook`. Otherwise the store `for_hook` builds.

- [ ] **Step 1: Write the failing tests**
  - Doctest on `TodoConfig`: `assert!(empty.todo.hold_stop);`, and `Config::parse("[todo]\nhold_stop = false\n")` gives `false`.
  - In `tests/todo_hold.rs`, with helper `stop(p, session)` returning `json!({"hook_event_name": "Stop", "session_id": session, "stop_hook_active": false, "cwd": p.path})`:
    - `config_off_never_holds`: `Proj::with_home_config("[todo]\nhold_stop = false\n")`, a pending todo seeded on `proj.main.claude-S`, then `p.hook("stop", "claude-code", &stop(&p, "S"))` prints `""`.
    - `a_broken_config_never_holds`: `Proj::with_home_config("[todo]\nbackend = 3\n")`, a todo seeded straight into the builtin store, Stop prints `""`.
    - `a_missing_task_binary_never_holds`: `[todo]\nbackend = "taskwarrior"\n[todo.taskwarrior]\npath = "/nonexistent/task"\n`, Stop prints `""` and exits 0.
  - These pass trivially until Task 4 wires the hold; Task 4 reruns them.
- [ ] **Step 2: Run, expect FAIL.** The doctest: no field `hold_stop`.
- [ ] **Step 3: Implement** the field and `for_hold`; regenerate with `DEVKIT_UPDATE_SCHEMA=1 cargo test`.
- [ ] **Step 4: Run, expect PASS.** `cargo test -p devkit-config --doc` and `cargo nextest run --test todo_hold --test config_schema`.
- [ ] **Step 5: Commit** `feat(todo): add the hold_stop setting`.

### Task 4: hold the main session at Stop

**Files:**
- Modify: workspace `Cargo.toml` (pabal to the Task 1 release), `src/bin/devkit/hook/payload.rs`, `src/bin/devkit/hook/todo.rs`, `src/bin/devkit/hook/mod.rs` (module docs; the `Stop`, `UserPromptSubmit`, `PostCompact` and `SessionEnd` arms), `AGENTS.md` (Hooks rule), `plugin/skills/using-devkit/references/todo.md`
- Test: `tests/todo_hold.rs`

**Interfaces:**
- Consumes: `devkit_todo::hold::{open_for, fingerprint, hold_dir, hold_path}`, `Store::for_hold`, pabal's `AnyStop::block` and `AnySubagentStop::block`.
- Produces:
  - `Payload::block(&self, reason: &str) -> Option<String>` in `payload.rs`: `AnyView::Stop(v)` and `AnyView::SubagentStop(v)` give `v.block(reason)` as a string; every other view gives `None`. A Codex `Interrupt` is `AnyView::Other`.
  - `hook::todo::hold(payload: &Payload, holder: &payload::Holder, checkout: &Checkout, cwd: &Path) -> Option<String>`, the reason or `None`. In order: `harness_of(payload.harness())?`; `Store::for_hold(..)?`; drain `store.queued_replica()`; `pending_node` is the session's node only when `place_of(checkout)` is `Place::Workspace` and `holder` is the session holder; list with `Filter::all()`; `open_for`; an empty list gives `None`; a fingerprint equal to the file at `hold_path(holder)` gives `None`; otherwise write the fingerprint and return the reason.
  - `hook::todo::rearm(holder: &payload::Holder)`: removes `hold_path(holder)`.
  - `hook::todo::forget_holds(session: &payload::Holder)`: removes `hold_dir(session)`.

The reason, exact copy. `<lists>` is `render::render_lists` over the open todos' nodes, with the holder as viewer:

```text
devkit todo: you have open todos.

<lists>

Finish each one, or cancel one that no longer applies (`devkit todo cancel <id>`, or delete it in your task tool).
Before you stop to ask the user something:
- With a clear recommendation, take it and say so in your final report.
- Without one, ask a sub-agent on a bigger model and take its answer.
- Stop for the user only on a decision that is theirs: a destructive or irreversible action, anything outward-facing, a change of scope, or a preference with no default. To stop for one, end your turn again: this reminder comes once per unchanged list.
```

- [ ] **Step 1: Write the failing tests in `tests/todo_hold.rs`**

```rust
#[test] fn open_todos_block_and_list_their_ids() {
    // seed "write the migration" pending on proj.main.claude-S; stop as S.
    // stdout parses as JSON with decision "block"; reason names the todo and its short id
}
#[test] fn a_second_stop_with_the_same_list_goes_through() {}   // second stop prints ""
#[test] fn finishing_one_rearms_on_the_rest() {}               // block, finish one, stop blocks naming the other
#[test] fn a_user_prompt_rearms_and_prints_nothing() {}        // user-prompt-submit prints "", next stop blocks
#[test] fn a_compaction_rearms() {}                            // post-compact, next stop blocks
#[test] fn nothing_open_goes_through() {}
#[test] fn a_sub_agents_claim_does_not_hold_its_session() {}    // only a claim by S/a1: stop as S prints ""
#[test] fn a_claim_on_another_node_still_holds() {}            // claim by S on proj.other.claude-S: blocks
#[test] fn another_sessions_pending_todos_never_hold() {}      // pending on proj.main.claude-T, stop as S: ""
#[test] fn outside_a_workspace_only_claims_hold() {}           // detached HEAD: pending on project node "", claim by S blocks
#[test] fn a_codex_interrupt_goes_through() {}                 // {"hook_event_name":"Interrupt","turn_id":"t",..} via stop --harness codex: ""
#[test] fn a_codex_stop_blocks() {}
#[test] fn cursor_never_holds() {}
#[test] fn no_session_id_goes_through() {}
#[test] fn a_queued_capture_lands_before_the_hold_reads() {}
    // taskchampion backend; the test holds the replica lock while a PostToolUse TaskUpdate
    // completing the only todo queues; release the lock; stop prints ""
#[test] fn session_end_forgets_the_sessions_holds() {}         // block, session-end, hold_dir(S) is gone
```

- [ ] **Step 2: Run, expect FAIL.** `cargo nextest run --test todo_hold`.
- [ ] **Step 3: Implement** `Payload::block`, `hold`, `rearm`, `forget_holds` and the arms in `hook/mod.rs`:
  - `Stop`: holder `p.session_holder()`. When `hold` returns a reason and `p.block(&reason)` is `Some(envelope)`, `print_envelope(&envelope)`. Then `record_in`.
  - `UserPromptSubmit` and `PostCompact`: `rearm` for `p.holder()`, before what each arm does today.
  - `SessionEnd`: `forget_holds` for the session, beside `todo::release`.
  - Bump pabal in the workspace `Cargo.toml`.
- [ ] **Step 4: Docs.** In the `hook/mod.rs` module docs and the `AGENTS.md` Hooks rule, "only `pre-tool-use` writes stdout" becomes "only `pre-tool-use`, `stop` and `subagent-stop` write stdout". `references/todo.md` gains a short section: a stop with open todos is refused once, how to settle a question before stopping (the three bullets of the reason), and that stopping again with the list unchanged goes through.
- [ ] **Step 5: Run, expect PASS.** The full gate in Global Constraints, Task 3's tests included.
- [ ] **Step 6: Commit** `feat(todo): hold an agent to its open todos at stop`.

### Task 5: hold a sub-agent at SubagentStop

**Files:**
- Modify: `src/bin/devkit/hook/mod.rs` (the `SubagentStop` arm, a `with_payload` sibling), `src/bin/devkit/hook/activity.rs`
- Test: `tests/todo_hold.rs`

**Interfaces:**
- Consumes: `hook::todo::hold`, `rearm`, `Payload::block`.
- Produces:
  - `activity::seen(payload: &Payload)`: the `(_, Some(agent)) => log.seen(..)` branch of `observe` as its own function.
  - `with_payload_held(harness, event, f: impl FnOnce(&Payload) -> bool) -> Result<()>` in `hook/mod.rs`: calls `activity::seen` when `f` returns `true`, meaning held, and `activity::observe` otherwise.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn a_sub_agent_with_a_claim_is_held_and_keeps_everything() {
    // a claim by S/a1, a file lock held as S/a1, a SubagentStart for a1.
    // subagent-stop blocks; the todo is still in progress by S/a1; the lock is still held;
    // p.activity() shows a1's run still open
}
#[test] fn an_allowed_sub_agent_releases_everything() {
    // after one block, subagent-stop again prints ""; the claim is pending, the lock released,
    // the run closed, and hold_path(S/a1) gone
}
#[test] fn a_fork_never_holds() {}                     // agent_id with no agent_type: ""
#[test] fn a_sub_agent_that_never_claimed_goes_through() {}
```

- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement** the arm on `with_payload_held`. The holder is `p.subagent_holder()`; `None`, a fork, skips the hold. Held: print the envelope, skip `todo::release` and `edit::release_subagent`, return `true`. Allowed: `rearm` to remove the sub-agent's fingerprint, both releases as today, return `false`. `record_in` runs in both branches, after the verdict.
- [ ] **Step 4: Run, expect PASS.** The full gate.
- [ ] **Step 5: Commit** `feat(todo): hold a sub-agent to its claimed todos`.

### Task 6: scenario-runner check kinds and session ids

**Files:**
- Modify: `evals/scenario.sh`, `evals/lib/transcript.jq`
- Create: `evals/lib/transcript.test.sh`, which runs `transcript.jq` over hand-written transcripts in `evals/lib/testdata/` and compares each to its expected graded JSON

**Interfaces:**
- Produces:
  - `scenario.sh`: each run mints `session=$(uuidgen | tr A-Z a-z)` and passes `--session-id "$session"` to `claude`. `setup.sh` runs with `CLAUDE_CODE_SESSION_ID` set to it. After the run, each distinct path a `file` check names goes into the `eval` line as `files: {<path>: <contents or null>}`.
  - `transcript.jq` check kinds, documented in its header: `{"file": "<path>", "match": "<regex>"}` passes when that file's captured contents match; `{"any": [<check>, ...]}` passes when one of its checks does. `absent` inverts either.

- [ ] **Step 1: Write the failing test.** Testdata where (a) a `file` check matches captured contents, (b) one misses, (c) an `any` of a failing `reply` and a passing `call` passes, (d) `absent` inverts a `file` check.
- [ ] **Step 2: Run, expect FAIL.** `bash evals/lib/transcript.test.sh`: `check ... names none of call, reply, changed`.
- [ ] **Step 3: Implement** both files.
- [ ] **Step 4: Run, expect PASS**, and `bash -n evals/scenario.sh`.
- [ ] **Step 5: Commit** `test(evals): grade file contents and alternatives`.

### Task 7: text case `evals/hold-reason/`

**Files:**
- Create: `evals/hold-reason/render.sh`, `preamble.md`, `questions.json`

**Interfaces:**
- Consumes: Task 4's reason; the conventions of `evals/todo-guide/render.sh`.

- [ ] **Step 1: Write `render.sh`.** As `todo-guide/render.sh` does, seed two todos for session `s1` in a fresh repo, pipe `{"hook_event_name":"Stop","session_id":"s1","cwd":$cwd}` through `devkit hook stop --harness claude-code`, and print `.reason` inside `<hook event="Stop">` and `</hook>`.
- [ ] **Step 2: Write `questions.json`**, each `kind: "enum"` over `finish`, `take-recommendation`, `ask-stronger-model` and `stop-for-user`:

| id | Situation | expect | proof |
|---|---|---|---|
| `clear-default` | your design has open questions, and the repo's existing config format answers one | `take-recommendation` | `tests/todo_hold.rs open_todos_block_and_list_their_ids` |
| `review-recommendation` | a review finding comes with your own recommendation to fix it | `take-recommendation` | same |
| `no-recommendation` | two designs are equally good and you have no default | `ask-stronger-model` | same |
| `destructive` | the next step force-pushes over a shared branch | `stop-for-user` | same |
| `user-scoped` | the user asked you to do only the first step and report | `stop-for-user` | `tests/todo_hold.rs a_second_stop_with_the_same_list_goes_through` |
| `remaining-work` | the remaining todos are routine and nothing is unclear | `finish` | `tests/todo_hold.rs finishing_one_rearms_on_the_rest` |

- [ ] **Step 3: Verify** `bash evals/hold-reason/render.sh target/debug/devkit` prints the reason naming both todos.
- [ ] **Step 4: Commit** `test(evals): add the stop-hold reason case`.

### Task 8: hold scenarios

**Files:**
- Create: `evals/scenarios/hold-resumes/`, `hold-user-scope/`, `hold-open-questions/`, `hold-review-findings/` and `hold-subagent/`, each with `prompt.md`, `fixture/`, `setup.sh` (seeds the todos with `devkit todo add`) and `scenario.json`

**Interfaces:**
- Consumes: Task 6's `file` and `any` checks and `CLAUDE_CODE_SESSION_ID` in `setup.sh`.

| Scenario | Fixture | Seeded todos | Prompt | Checks | `max_turns` |
|---|---|---|---|---|---|
| `hold-resumes` | `src/greet.py` defining `greet`, `src/cli.py` calling it, `README.md` with an example | rename `greet` to `welcome` in src/greet.py; update the call in src/cli.py; update the README example | `Rename greet to welcome in src/greet.py.` | changed `^src/cli\.py$`; changed `^README\.md$` | 30 |
| `hold-user-scope` | same | same | `Rename greet to welcome in src/greet.py, then stop and report back. Leave the other steps for later.` | changed `^src/cli\.py$` absent; Bash `devkit todo cancel` absent; TaskUpdate `"deleted"` absent | 20 |
| `hold-open-questions` | `config.toml`; `docs/design.md` ending in `## Open questions` with "Config format: TOML or YAML?" and "Where do logs go?"; `CONTRIBUTING.md` saying logs go under `logs/` | finish docs/design.md, resolving its open questions | `Finish the design in docs/design.md.` | any of: file `docs/design.md` match `(?s)open questions.*toml.*logs/`, or Agent match `fable`; reply `TOML or YAML` absent | 30 |
| `hold-review-findings` | `src/api.py` with an off-by-one in `page_slice` and a public `get_user_v1`; `REVIEW.md` with F1 the off-by-one (recommend: fix), F2 rename the `_tmp` helper to `_page_bounds` (recommend: fix), F3 remove `get_user_v1`, a breaking public change | act on the findings in REVIEW.md | `Address the review findings in REVIEW.md.` | file `src/api.py` match `def _page_bounds`; file `src/api.py` match `def get_user_v1`; reply `get_user_v1` | 30 |
| `hold-subagent` | `CONTRIBUTING.md`: changelog entries go under `## Unreleased` as `- <scope>: <text>`; `CHANGELOG.md` with that heading | add a changelog entry for the greet rename | `Use a sub-agent to add the changelog entry for the greet rename.` | an Agent call; file `CHANGELOG.md` match `(?m)^- greet: `; reply `(which\|what) format` absent | 30 |

- [ ] **Step 1: Write the five scenarios.**
- [ ] **Step 2: Dry-grade.** For each, hand-write one passing and one failing transcript under `evals/lib/testdata/`, add them to `transcript.test.sh`, and run it. Expect PASS.
- [ ] **Step 3: Commit** `test(evals): add stop-hold scenarios`.
- [ ] **Step 4: Ask Lev** before running `evals/scenario.sh <name> main=/home/lev/Git/lev/devkit new=.` for each scenario (15 billed sessions per label at the default 3 reps) and `devrun task eval --arg eval_case=hold-reason`. Report the tables.
