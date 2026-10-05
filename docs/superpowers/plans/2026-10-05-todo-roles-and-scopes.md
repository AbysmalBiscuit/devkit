# Todo roles and scopes implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** File bare todo nodes with configurable scopes and caller roles, preserving claim authority and excluding personal projects from scans.

**Architecture:** Config owns the template grammar and validates the scope and role trees. The todo crate fills templates, resolves persistent roles, and constructs visibility filters. CLI and hooks consume this shared resolution; stores retain their existing claim contract.

**Tech Stack:** Rust, serde, schemars, taskchampion, tempfile, nextest, pabal.

**Spec:** `docs/superpowers/specs/2026-10-05-todo-roles-and-scopes-design.md`.

## Global constraints

- The approved spec supersedes issue #240's acceptance text and Stop-and-ask.
- Do not move existing `devkit.*` todos or change alacritree.
- Claim authority remains `Holder::covers`; role parent relationships grant none.
- #241 owns taskwarrior removal and TASKDATA/taskrc data directory discovery. Integrate its schema move after main receives it.
- Keep all test replicas and visual proof isolated from personal todo data and daily-driver build channels.
- Templates use `{repo}`, `{branch}`, `{harness}`, `{session}`, `{agent}` only; every value is sanitized.
- Role records survive session end and each write prunes entries older than 30 days.

## Review focus

- Overridden default scopes remain connected to the root; partially configured maps retain built-ins.
- Literal suffixes and harness segments read back without accepting malformed or empty identities.
- Personal tasks that share a repository prefix remain excluded unless they match a valid scope template.
- A role deleted during a resumed session warns once and falls back without losing claims or writes.
- Concurrent native writes preserve the role/nudge record under its file lock.

---

### Task 1: Parse and validate workflow config

**Files:** create `crates/devkit-config/src/todo.rs`; modify `crates/devkit-config/src/lib.rs`; test doctests and `crates/devkit-config/tests/todo_layout.rs`; update `schema/devkit-config.json` when the integrated schema settles.

**Interfaces:** `ScopeConfig { node: String, parent: Option<String> }`, `RoleConfig { scope: String, parent: Option<String>, agent_types: Vec<String>, hold_pending: Option<bool> }`; `Template::parse(&str) -> Result<Template>`, `Template::read(&str) -> Option<BTreeMap<String, String>>`, `Template::fill(&BTreeMap<String, String>) -> Option<String>`, `Template::contains(&str) -> bool`, `Template::anchored() -> bool`. `TodoConfig` exposes default-extended `scopes` and `roles`; `PostgresConfig::root` defaults to `devkit`.

- [ ] Write `defaults_and_custom_entries_share_one_tree` through `Config::parse`, asserting built-in scopes/roles plus a custom manager and worker.
- [ ] Run `cargo nextest run --manifest-path <checkout>/Cargo.toml -p devkit-config --test todo_layout`; expect RED because scopes are unknown.
- [ ] Implement config types, grammar parser, tree/role validation and default map extension. Reject `[todo] project` with `[todo.scopes]` replacement text; remove its field while keeping #241-owned fields until integration.
- [ ] Add parse/doctest slices for unknown/repeated placeholders, multiple open placeholders per segment, unanchorable templates, malformed braces/empty segments, missing parents, cycles, multiple roots, root placeholders, missing role/scope and duplicate agent types. Confirm RED then GREEN for each behavior slice.
- [ ] Pin keywise layering and default role overrides through real config resolution; run config tests and doctests, then the required workspace gates before the logical commit.

### Task 2: Shared layout, fence and caller role state

**Files:** create `crates/devkit-todo/src/layout.rs`, `crates/devkit-todo/src/roles.rs`; modify `crates/devkit-todo/src/node.rs`, `lib.rs`, `Cargo.toml`; test public interfaces in these modules.

**Interfaces:** `Facts::new(&Place, Option<&SessionRef>, &Holder) -> Facts`; `Layout::new(&TodoConfig) -> Layout`; `Layout::fill(&str, &Facts) -> Result<String>`; `Layout::visible(&Facts, &str) -> Filter`; `Layout::is_devkit(&str, &BTreeSet<String>) -> bool`; `Layout::fence(Vec<Todo>) -> Vec<Todo>`. `Roles::at(PathBuf)` resolves, records and nudges exact holders; `ResolvedRole` supplies name, node and `hold_pending`.

- [ ] Write fill/read/visibility public tests, asserting `r.main.codex-S.a1`, walk-up to workspace for terminal callers, and sanitation of `r.x`, `feat/a`, `S.x`, `a/1`.
- [ ] Run `cargo nextest run --manifest-path <checkout>/Cargo.toml -p devkit-todo`; expect RED for missing layout.
- [ ] Implement shared layout with an additional `NodeMatch` for same-session descendants using template read-back. Preserve exact/subtree matching and holder authority.
- [ ] Add fence slice asserting anchored `r`, `r.main`, session/agent nodes and `global` count, but `ideas`, malformed nodes and legacy prefixed projects do not. Confirm RED then GREEN.
- [ ] Add role-state slices for exact holder precedence, agent-types, built-ins, resume, stale-role warning/fallback, pruning at 30 days, nudge once and concurrent writes; implement locked JSON writes using common store helpers. Run todo tests and required gates before commit.

### Task 3: Bare taskchampion storage and CLI roles/scopes

**Files:** modify `crates/devkit-todo-taskchampion/src/store.rs`, `lib.rs`; `src/bin/devkit/todo/mod.rs`, `store.rs`, `queue.rs`; `tests/todo_taskchampion.rs`, `tests/todo_cli.rs`, `tests/common/todoenv.rs`; update affected root assumptions in backend tests.

**Interfaces:** taskchampion writes `Todo::node()` directly to project and drops `with_root`/`root`; queued capture/release contain no root. `todo role [name]` records only actor; add/list gain `--scope`, conflicting with raw filters. `Store` exposes resolved todo config so CLI and hooks share layout.

- [ ] Through temporary taskchampion CLI entry points, add `session_todos_use_bare_nodes` asserting `proj.main.codex-S`; run `cargo nextest run --manifest-path <checkout>/Cargo.toml --test todo_taskchampion`; expect RED for rooted project.
- [ ] Remove taskchampion prefix mapping and queue roots, preserve builtin/Postgres nodes and point Postgres tenancy at `postgres.root`. Leave #241's directory discovery and schema move untouched.
- [ ] Add CLI slices for manager selection, worker `--scope workspace`, role errors listing names and refusal without session; confirm RED then GREEN through real CLI.
- [ ] Resolve caller layout/role once; implement add/scope/list/context routing. Scan fences apply only to `--all`/`--subtree`; default listing includes own ancestors and same-session descendant nodes; `--scope` lists its filled exact node.
- [ ] Pin personal-task exclusion, anchored repo inclusion, sibling-session exclusion and raw-node preservation through CLI. Run backend contract tests and CLI tests, then required gates before commit.

### Task 4: Role-aware hooks, native mirrors and stop hold

**Files:** modify `src/bin/devkit/hook/todo.rs`, `mod.rs`, `shell.rs`, `writes.rs`; `src/bin/devkit/todo/mod.rs`; `tests/todo_hooks.rs`, `todo_hold.rs`, `todo_context.rs`.

**Interfaces:** subagent-start records agent-types roles for its exact subagent holder without stdout. Pre-tool-use context combines role suggestions with existing guard answers. Native capture uses shared layout. Hold passes resolved pending-node choice to existing `hold::open_for`.

- [ ] Write payload test `subagent_start_records_configured_role_without_stdout`; run `cargo nextest run --manifest-path <checkout>/Cargo.toml --test todo_hooks`; expect RED for missing role record.
- [ ] Wire role assignment at spawn and native routing. Add role-less subagent payload slices for TaskCreate, TodoWrite and shell `devkit todo add`; first write nudges with suggested child roles, second does not. Confirm RED then GREEN.
- [ ] Add context slices asserting role/node and visibility through payloads, including changes to a recorded role's scope.
- [ ] Add stop/subagent-stop slices for own claims anywhere, worker pending at agent scope, manager workspace defaults and explicit pending override. Preserve built-in main/subagent behavior and one-reminder semantics. Confirm RED then GREEN.
- [ ] Run hook/context/hold suites and required gates before commit.

### Task 5: Reference, agent scenario and visual evidence

**Files:** modify `plugin/skills/using-devkit/references/todo.md`; create `evals/scenarios/todo-roles/{prompt.md,setup.sh,scenario.json}` and any isolated fixture required; create unstaged `.devkit/proof/handoff.md`, screenshot and captured commands.

**Interfaces:** scenario grades a role-less worker choosing a suggested manager child role and filing its todo at its agent node. Proof is numbered against the approved spec requirements supplied by the manager.

- [ ] Update reference to explain scopes, roles, fence, stop defaults and Postgres root; keep command/schema facts at their owning clap/doc-comment locations.
- [ ] Add scenario with manager and two worker roles. Validate check grammar with existing transcript tests, then run `EVAL_REPS=1 devrun task eval-scenario --arg scenario=todo-roles`. Expected: every required behavior check passes; retain actual billed result.
- [ ] Use this build and an isolated taskchampion replica to add a session todo, open isolated alacritree pointing to that replica and capture genuine task-tab screenshot. Expected: the todo appears under its repo. If binaries or capture are unavailable, report the concrete blocker while completing other work.
- [ ] Record `devkit schema` output for scopes/roles and evidence per acceptance item in `.devkit/proof/handoff.md`; commit scenario/reference changes after required gates.

### Task 6: Integrate #241 and final verification

**Files:** dependency-sensitive files from Task 1 and Task 3; generated schema; affected reference text and tests.

**Interfaces:** #241's taskchampion schema and data_dir resolution replace taskwarrior-dependent imports; #240 retains only its bare-node changes.

- [ ] Notify manager of ready integration seams; manager rebases after #241 lands. Resolve resulting conflicts against both approved scopes and run real temporary taskchampion CLI tests.
- [ ] Regenerate schema with `DEVKIT_UPDATE_SCHEMA=1` on the schema test and verify scopes/roles and Postgres root.
- [ ] Run `devrun task fmt`, `devrun task test`, `devrun task test-doc`, `devrun task lint`, then `devrun task fmt-check`; expected: all pass. Report skipped external Postgres cases honestly.
- [ ] Commit final integration edits selectively, with evidence in the execution ledger and proof handoff. Manager performs independent verification and review and requests PR draft.

## Unresolved questions

- None about behavior. #241 must land before final integration. Visual proof depends on an available alacritree binary and isolated display capture; agent eval depends on logged-in Claude.
