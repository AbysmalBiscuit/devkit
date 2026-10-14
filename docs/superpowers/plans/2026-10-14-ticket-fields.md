# Ticket Fields Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Repositories declare ticket fields per tracker in `devkit.toml`; `ticket create` and `ticket edit` write them on GitHub and Linear, a pre-tool-use check holds tracker MCP writes to the same levels, and `ticket fields` / `ticket fields discover` show and generate the declarations.

**Architecture:** Declarations are config types in `devkit-config` with load-time validation. One pure evaluator in `devkit-common` (levels, conditions, value validation, messages) serves every path: the CLI, the MCP hook and `ticket fields`. Tracker writes are query builders and parsers tested over recorded responses, plus a networked writer per tracker, following `github_status.rs` / `linear_status.rs`.

**Tech Stack:** Rust 2024, serde/schemars, clap, minijinja, `gh` CLI and GitHub GraphQL, Linear GraphQL over `reqwest`, `toml_edit`.

**Spec:** `docs/superpowers/specs/2026-10-14-ticket-fields-design.md`

## Global Constraints

- No `hook` verb exits 2; only `pre-tool-use`, `stop` and `subagent-stop` write stdout; logging never changes a verdict.
- A hook verdict needs no network, and the hook never resolves or detects a tracker.
- Once an `[harness.issue_tools]` rule matches, any failure denies (fail closed).
- A field key's meaning is the doc comment on its config type, with a `devkit.toml` doctest; regenerate `schema/devkit-config.json` with `DEVKIT_UPDATE_SCHEMA=1 cargo test`.
- Agent-facing behavior goes in `plugin/skills/using-devkit/references/ticket.md`; `docs/` restates nothing.
- Help text stays ASCII.
- Repository-scoped `gh` calls go through `cmd::gh_json_in` / `cmd::gh_capture`; tokens resolve through `devkit_common::secrets`.
- TDD: each test is seen failing for the right reason before the code that passes it.
- Gate before every commit: `cargo nextest run --workspace --no-fail-fast`, `cargo test --workspace --doc`, `cargo clippy --workspace --all-targets -- -D warnings`, `devrun task fmt`.
- Commit with `devkit commit --files <paths> --subject '<type(scope): subject>' --coauthor '<model> <noreply@anthropic.com>'`.

## Review Focus

1. List values with stray whitespace or repeats (`--arg labels=" a , b,a"`): trimmed and deduplicated before validation and write, and read-back compares sets. Test in Task 2.
2. Enum names typed in another case (`--arg priority=p1`): matched case-insensitively on every path, written by id. Test in Task 2.
3. A `[people]` alias that exists but lacks the tracker's identity key: refused naming the alias and the key, before any write. Test in Task 2.
4. A GitHub create whose project add fails for token scope: URL printed, failed fields named, the scope remedy from `github_status::scope_error`, exit non-zero, no second create. Test in Task 5.
5. An MCP value of the wrong JSON type (a number where an enum name is expected, an array for a scalar): refused, never coerced. Test in Task 8.

## Delivery

Three PRs, each green on its own:

- **PR A, GitHub end to end:** Tasks 1 to 5.
- **PR B, Linear:** Task 6 and Task 7.
- **PR C, MCP check and discovery:** Tasks 8 to 10.

---

### Task 1: Field declarations in config

**Files:**
- Create: `crates/devkit-config/src/ticket_fields.rs`
- Modify: `crates/devkit-config/src/lib.rs` (`TicketConfig` gains `fields`; `Person` gains `linear`, `slack` becomes `Option<String>`; `ISSUE_CONTEXT_KEYS` moves here; a `reject_invalid_fields(&cfg, &origin)` call beside `reject_reserved_variables` in the layered load)
- Modify: `src/bin/devkit/issue/render.rs:21` (re-export `devkit_config::ISSUE_CONTEXT_KEYS`), every reader of `Person.slack` (`rg -n '\.slack\b' src crates`)
- Modify: `schema/devkit-config.json` (regenerated)
- Test: unit tests in `ticket_fields.rs`, load tests in `crates/devkit-config/src/lib.rs` tests

**Interfaces:**
- Produces, in `devkit_config` (re-exported from the crate root):
  - `pub struct TicketFieldsConfig { pub github: BTreeMap<String, FieldDecl>, pub linear: BTreeMap<String, FieldDecl> }` with `pub fn for_tracker(&self, kind: TrackerKind) -> &BTreeMap<String, FieldDecl>` (empty map for `None`).
  - `pub struct FieldDecl { pub r#type: Option<FieldType>, pub list: bool, pub r#where: Option<Place>, pub values: Vec<EnumValue>, pub level: FieldLevel, pub when: Vec<WhenEntry>, pub default: Option<String>, pub description: Option<String>, pub allow_none: Option<bool> }` with `pub fn allows_none(&self) -> bool` (`allow_none.unwrap_or(true)`, always false for `Place::Team`) and `pub fn max_level(&self) -> FieldLevel` (highest of `level` and every `when` level).
  - `pub enum FieldType { String, Number, Date, Enum, User, Issue }`, serialized lowercase.
  - `pub enum FieldLevel { None, Info, Warning, Error }`, lowercase, `Ord` in that order, `impl From<Severity> for FieldLevel`.
  - `pub enum Place` with GitHub places `Labels, Assignees, Milestone, IssueType, Parent, BlockedBy, ProjectMembership, ProjectField { id: String, name: String }`, Linear places `Team, Labels, Assignee, Priority, Estimate, Cycle, Project, ProjectMilestone, DueDate, Parent, BlockedBy`, and `State`, which parses only so validation can refuse it with a pointer to `[ticket.events]`. A unit place is written as a string (`where = "milestone"`), `ProjectField` as `{ project_field = "<id>", name = "<field name>" }`. `pub fn valid_for(&self, kind: TrackerKind) -> bool`.
  - `pub struct EnumValue { pub name: String, pub id: String, pub teams: Vec<String> }`.
  - `pub struct WhenEntry { pub r#if: BTreeMap<String, Cond>, pub level: FieldLevel }`; `pub enum Cond { One(String), Any(Vec<String>) }` (untagged). The key `caller` tests `"agent"` / `"human"`; any other key names a field; `"*"` means set, `""` unset.
  - `pub const ISSUE_CONTEXT_KEYS: &[&str] = &["input", "ticket_title", "issue_title"];`
  - `Person { pub slack: Option<String>, pub github: Option<String>, pub linear: Option<String> }`.

- [ ] **Step 1: Write the failing tests** in `ticket_fields.rs`:
  - `a_declaration_parses_every_shape`: the spec's two illustration tables (with `where = { project_field = "PVTSSF_x", name = "Priority" }`) parse; `values[1].name == "P1"`, `when[0].level == FieldLevel::Warning`, `default == Some("P2")`.
  - `levels_order_none_below_error`: `FieldLevel::None < Info < Warning < Error`, and `FieldLevel::from(Severity::Warning) == FieldLevel::Warning`.
  - `max_level_takes_the_highest_when_entry`: `level = "none"` with a `when` at `error` gives `Error`.
  - `team_never_allows_none`: `allow_none = true` on a `Place::Team` field still gives `allows_none() == false`.

  And load tests in `lib.rs` (through the layered load the CLI uses), each asserting the error text:
  - `a_status_place_is_refused_pointing_at_ticket_events`: Linear `where = "state"`, and GitHub `project_field` whose `name` equals `[github] status_field` (`Status`, case-insensitively). Both errors contain `[ticket.events]`.
  - `an_enum_that_can_reach_error_needs_values`: `type = "enum"`, `level = "info"`, `when = [{ if = { caller = "agent" }, level = "error" }]`, no values. Error names the field.
  - `a_merged_declaration_without_type_names_the_layer_of_its_level`: home layer sets only `[ticket.fields.github.priority] level = "error"`, the repository layer nothing. Error contains the home layer's path.
  - `two_fields_with_one_place_are_refused`, `a_condition_on_an_undeclared_field_is_refused`, `a_place_from_the_other_tracker_is_refused` (`where = "cycle"` under `github`).
  - `a_field_named_like_a_template_variable_or_context_key_is_refused`: one case with `[templates.variables] priority`, one with a field named `issue_title`, one named `caller`.
  - `values_replace_whole_across_layers`: home declares three values, repository two; the merged declaration has the repository's two.
  - `a_person_may_omit_slack`: `[people.lev] github = "AbysmalBiscuit"` parses.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run -p devkit-config ticket_fields a_status_place a_merged_declaration values_replace a_person_may_omit`
Expected: compile errors for the missing types, then failures on each assertion.

- [ ] **Step 3: Implement the types and `fn reject_invalid_fields(cfg: &Config, origin: &HashMap<String, PathBuf>) -> Result<()>` in `crates/devkit-config/src/lib.rs`.** Origin keys are dotted (`ticket.fields.github.priority.level`), as `reject_reserved_variables` reads them. Each type's doc comment carries the `devkit.toml` example as a doctest. Update `Person` readers to handle `slack: None` the way they handle a missing alias today.

- [ ] **Step 4: Run the tests and the schema update**

Run: `cargo nextest run -p devkit-config && DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema && cargo test -p devkit-config --doc`
Expected: PASS; `schema/devkit-config.json` shows the new keys.

- [ ] **Step 5: Commit** `feat(config): declare ticket fields per tracker`

---

### Task 2: The field evaluator

**Files:**
- Create: `crates/devkit-common/src/fields.rs` (register in `crates/devkit-common/src/lib.rs`)
- Test: unit tests in the same file

**Interfaces:**
- Consumes: Task 1 types; `devkit_common::caller::Caller`; `devkit_config::Person`.
- Produces:
  - `pub enum Surface { Create, Edit, McpCreate, McpUpdate }`
  - `pub struct Given { pub values: BTreeMap<String, String>, pub nones: BTreeSet<String> }` (raw `--arg` text, or MCP values rendered to text by Task 8)
  - `pub enum Value { Text(String), Number(f64), Date(String), Enum { name: String, id: String }, User { alias: String, id: String }, Issue(String) }`, and `pub struct Resolved { pub fields: BTreeMap<String, Vec<Value>>, pub nones: BTreeSet<String> }` (a scalar is a one-element vec).
  - `pub struct Current(pub BTreeMap<String, Vec<String>>)`: a ticket's present field values by name, for `Surface::Edit`.
  - `pub fn level_of(decls: &BTreeMap<String, FieldDecl>, name: &str, seen: &BTreeMap<String, Vec<String>>, caller: Caller) -> FieldLevel`: the first matching `when` entry, else `level`.
  - `pub fn resolve(decls, kind: TrackerKind, given: &Given, people: &HashMap<String, Person>, team: Option<&str>) -> Result<Resolved, Vec<Problem>>`: validates every given value; never stops at the first problem.
  - `pub fn check(decls, path: Path, given: &Given, resolved: &Resolved, current: Option<&Current>, caller: Caller) -> Report`
  - `pub struct Report { pub refused: Vec<Problem>, pub warned: Vec<Problem> }`, `pub enum Problem { Missing { field: String }, Invalid { field: String, value: String, reason: String }, Clear { field: String }, NoneNotAllowed { field: String } }`
  - `pub fn message(decls, problems: &[Problem]) -> String`: one block naming each field, its description, its valid values (enum names, or `[people]` aliases with the needed key), `--arg <field>=...`, and `--none <field>` when the field allows none, using `required::description_line`.
  - `pub fn listing(decls, caller: Caller, given: &Given) -> Vec<FieldView>` with `pub struct FieldView { pub name: String, pub r#type: FieldType, pub list: bool, pub level: FieldLevel, pub when: Vec<WhenEntry>, pub values: Vec<String>, pub default: Option<String>, pub description: Option<String>, pub allows_none: bool }` (`Serialize`), omitting a field only when `max_level() == None`.

- [ ] **Step 1: Write the failing tests:**
  - `missing_error_field_is_refused_on_create_and_defaults_count_only_on_create`: `priority` at `error` with `default = "P2"`: `check(Create)` with nothing given refuses nothing; `check(McpCreate)` refuses `Missing { field: "priority" }`, and `message` contains `P2`.
  - `warning_is_warned_not_refused`.
  - `edit_never_reports_missing_and_never_applies_defaults`: `check(Edit)` with only `labels` given returns no `Missing` and `resolve` adds no `priority`.
  - `a_clear_of_an_error_field_is_refused_unless_it_allows_none`: with `nones = {"milestone"}` on `Edit`, `allow_none = false` gives `NoneNotAllowed`; the default gives no problem.
  - `edit_conditions_see_current_overlaid_with_given`: `milestone` is `error` when `project = "*"`; `Current` has `project = ["devkit"]`; clearing `milestone` with `allow_none = false` is refused.
  - `mcp_update_judges_a_clear_at_the_highest_possible_level`: same declaration, `McpUpdate`, `project` absent from the call: the clear is refused.
  - `an_explicit_none_satisfies_error_on_create`.
  - `invalid_values_are_refused_at_every_level`: an unknown enum value at `none` gives `Invalid`.
  - Review Focus 1 `list_values_are_trimmed_and_deduplicated`: `" a , b,a"` resolves to `[a, b]`.
  - Review Focus 2 `enum_names_match_case_insensitively`: `p1` resolves to `Enum { name: "P1", id: "47fc9ee4" }`.
  - Review Focus 3 `an_alias_without_the_tracker_identity_names_the_key`: `[people.lev]` with only `github`, on Linear, gives `Invalid` whose `reason` contains `linear`.
  - `a_team_scoped_value_from_another_team_is_refused`: `EnumValue { teams: ["ENG"] }` with `team = Some("OPS")`.
  - `an_empty_arg_points_at_none`: `values = {"milestone": ""}` gives `Invalid` whose `reason` contains `--none milestone`.
  - `listing_hides_only_fields_that_can_never_rise_above_none`: the spec's milestone example (`none`, `warning` when project set) is listed, at `None` without `--arg project=...` and at `Warning` with it.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run -p devkit-common fields::`
Expected: compile errors, then each assertion failing.

- [ ] **Step 3: Implement `fields.rs`.** `number` parses as `f64`; `date` as ISO `YYYY-MM-DD`; `issue` is accepted as text here and checked against the tracker's `issue_ref` by the writer before any write. Enum match is ASCII case-insensitive on `name`, exact on `id`.

- [ ] **Step 4: Run them and see them pass**

Run: `cargo nextest run -p devkit-common fields::`
Expected: PASS

- [ ] **Step 5: Commit** `feat(ticket): evaluate ticket field levels and values`

---

### Task 3: `--arg`, `--none` and templates

**Files:**
- Modify: `src/bin/devkit/template.rs` (a `FieldArgs { #[arg(long = "none", value_name = "FIELD")] pub none: Vec<String> }`, flattened into `TicketCmd::Create` and `TicketCmd::Edit` in `src/bin/devkit/issue/mod.rs`)
- Modify: `src/bin/devkit/issue/render.rs` (`render` takes the declared fields: the allowed `--arg` set is `templates.declared()` plus field names; field values reach the templates; a field no one gave renders `""`)
- Modify: `src/bin/devkit/issue/review/mod.rs` (`missing_required` leaves field names out of `reads`)
- Test: `tests/issue_create.rs`

**Interfaces:**
- Consumes: Task 1 `TicketFieldsConfig::for_tracker`; Task 2 `Given`.
- Produces: `pub(crate) fn given(vars: &VarArgs, fields: &FieldArgs, decls: &BTreeMap<String, FieldDecl>) -> Result<Given>` in `render.rs`, refusing a `--none` naming an undeclared field; `render::render` gains a `decls: &BTreeMap<String, FieldDecl>` parameter and keeps its other parameters.

- [ ] **Step 1: Write the failing tests** in `tests/issue_create.rs` (config with `[tracker] kind = "github"` and a `priority` field at `warning`, `ticket_body` containing `Priority: {{ priority }}`):
  - `a_field_the_body_reads_is_not_required_by_the_template`: `ticket render --title T` succeeds and prints `Priority: ` (empty).
  - `a_given_field_reaches_the_body`: `--arg priority=P1` renders `Priority: P1`.
  - `an_undeclared_arg_is_still_refused`: `--arg nope=1` fails naming `nope`.
  - `none_on_an_undeclared_field_is_refused`.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run --test issue_create field`
Expected: FAIL (`priority` refused as undeclared, or as a required template arg).

- [ ] **Step 3: Implement** the parameter threading and the `missing_required` exclusion.

- [ ] **Step 4: Run them and see them pass**

Run: `cargo nextest run --test issue_create`
Expected: PASS, existing tests included.

- [ ] **Step 5: Commit** `feat(ticket): pass field values through --arg and --none`

---

### Task 4: `ticket fields`

**Files:**
- Create: `src/bin/devkit/issue/fields.rs`
- Modify: `src/bin/devkit/issue/mod.rs` (`TicketCmd::Fields { #[arg(long)] json: bool, #[command(flatten)] vars: VarArgs }`; Task 9 adds its `discover` subcommand)
- Modify: `plugin/skills/using-devkit/references/ticket.md` (declaring fields, levels, `--none`, `ticket fields`)
- Test: `tests/ticket_fields.rs`

**Interfaces:**
- Consumes: Task 2 `listing`, `FieldView`.
- Produces: `pub(crate) fn run(args: FieldsArgs) -> Result<()>`; the tracker kind comes from `[tracker] kind`, else from `tracker::select` as other `ticket` verbs do.

- [ ] **Step 1: Write the failing tests:**
  - `lists_fields_with_their_level_for_this_caller`: with `DEVKIT_CALLER=agent` and estimate at `info` with `when = [{ if = { caller = "agent" }, level = "error" }]`, the text shows `estimate` with `error`; with `DEVKIT_CALLER=human`, `info`.
  - `arg_values_evaluate_conditions`: milestone shows `none` alone and `warning` with `--arg project=devkit`.
  - `json_is_the_field_views`: `--json` parses as an array whose items have `name`, `level`, `values`, `allows_none`.
  - `makes_no_tracker_request`: under `ghfake`, `fake.calls()` is empty after the run.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run --test ticket_fields`
Expected: FAIL (`unrecognized subcommand 'fields'`).

- [ ] **Step 3: Implement** the command and the reference section. Help text ASCII.

- [ ] **Step 4: Run them and see them pass**

Run: `cargo nextest run --test ticket_fields && cargo nextest run --test completions`
Expected: PASS

- [ ] **Step 5: Commit** `feat(ticket): add ticket fields`

---

### Task 5: Writing fields on GitHub

**Files:**
- Create: `crates/devkit-common/src/tracker/github_fields.rs` (query builders, parsers, `GithubFieldWriter`)
- Modify: `src/bin/devkit/issue/create.rs`, `src/bin/devkit/issue/edit.rs`, `src/bin/devkit/issue/mod.rs` (`Edit.body` becomes `Option<String>`)
- Modify: `plugin/skills/using-devkit/references/ticket.md` (create and edit with fields, partial failure, retries)
- Test: unit tests over recorded responses in `github_fields.rs` (fixtures under `crates/devkit-common/src/tracker/fixtures/`), e2e in `tests/issue_create.rs` with `ghfake` (`serve_graphql`, `serve_mutation`, `create_fails`, `fail_graphql`)

**Interfaces:**
- Consumes: Task 2 `Resolved`, `Current`, `check`, `message`; `github_status::{add_item_mutation, set_field_mutation, scope_error, check}`; `github::Api::graphql_or_gh`.
- Produces:
  - `pub struct GithubFieldWriter` with `pub fn new(repo: Repo, project: Option<ProjectRef>) -> Self`, `pub fn read(&self, number: u64, decls) -> Result<Current>`, `pub fn apply(&self, number: u64, decls, resolved: &Resolved) -> Vec<(String, anyhow::Error)>` (fields that did not take), `pub fn verify(&self, number: u64, decls, resolved: &Resolved) -> Vec<String>` (fields whose read-back differs).
  - `gh issue create` carries `--label`, `--assignee`, `--milestone` for those places; everything else goes through `apply` after the create.
- The create order is the spec's steps 1 to 5. Linear is still refused by `github_only` until Task 7.

- [ ] **Step 1: Write the failing tests:**
  - Unit: `read_query_reads_labels_assignees_milestone_type_parent_blockers_and_the_project_item`, `parse_read_maps_project_field_options_back_to_value_ids`, `blocked_by_and_parent_mutations_carry_node_ids`.
  - e2e: `create_writes_every_field_and_reads_it_back`: labels, assignee and milestone appear on the `issue create` call, and the project add and the field set are in the mutation log; stdout has the URL.
  - e2e: `a_missing_error_field_refuses_before_gh_runs`: no `issue create` in `fake.calls()`.
  - e2e Review Focus 4 `a_failed_project_add_prints_the_url_and_the_field`: `fail_graphql("INSUFFICIENT_SCOPES ...")` after the create: exit non-zero, stdout has the URL, stderr names `priority` and the scope remedy, and only one `issue create` was called.
  - e2e: `an_ambiguous_create_failure_says_to_search_before_retrying`: `create_fails("timeout")`: stderr says the issue may exist and to search for the title.
  - e2e: `a_read_back_mismatch_names_the_field`: the mutation succeeds and the read returns another option.
  - e2e: `a_rejected_id_says_to_rerun_discover`: the field mutation fails because the option id no longer exists; stderr names the field and value and says to run `ticket fields discover`, and nothing is retried.
  - e2e: `field_only_edit_leaves_title_and_body`: `ticket edit 7 --arg priority=P1`: no `issue edit --body` call.
  - e2e: `edit_lists_replace`: `--arg labels=a` on an issue with `a,b` removes `b`.
  - e2e: `edit_none_clears_and_refuses_when_not_allowed`.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run -p devkit-common github_fields && cargo nextest run --test issue_create`
Expected: FAIL

- [ ] **Step 3: Implement** `github_fields.rs`, then the create and edit flows. Blocked-by and parent use GitHub's `addBlockedBy` and `addSubIssue` mutations, and issue type uses `updateIssueIssueType`. Confirm their current input names against GitHub's GraphQL schema with `gh api graphql` introspection before recording fixtures.

- [ ] **Step 4: Run them and see them pass, then run the full gate**

Run: the four Global Constraints gate commands
Expected: all green

- [ ] **Step 5: Commit** `feat(ticket): write declared fields on GitHub`, then open PR A.

---

### Task 6: A Linear test seam and Linear field writes

**Files:**
- Modify: `crates/devkit-common/src/tracker/linear.rs` (`send` posts to `DEVKIT_LINEAR_URL` when set, else `https://api.linear.app/graphql`; doc comment says it exists for tests)
- Create: `crates/devkit-common/src/tracker/linear_fields.rs`
- Create: `tests/common/linearfake.rs`: a `TcpListener` thread, like `tests/common/syncserver.rs`, that answers a POST by the first registered substring of its query and records every body
- Test: unit tests over recorded responses in `linear_fields.rs`

**Interfaces:**
- Consumes: Task 2 `Resolved`, `Current`; `linear::post_graphql`.
- Produces:
  - `pub fn create_id(team_id: &str, title: &str, body: &str) -> Uuid`: a v5 uuid over the three, so a retry of the same create reuses the id.
  - `pub struct LinearFieldWriter` with `pub fn new(key: String) -> Self`, `pub fn create(&self, title: &str, body: &str, decls, resolved: &Resolved) -> Result<Created>` (`pub struct Created { pub identifier: String, pub url: String, pub uuid: String }`), `pub fn read(&self, id: &str, decls) -> Result<Current>`, `pub fn apply(&self, uuid: &str, decls, resolved) -> Vec<(String, anyhow::Error)>`, `pub fn verify(&self, id: &str, decls, resolved) -> Vec<String>`.
  - `create` passes `id: create_id(..)` in `IssueCreateInput`. When Linear answers that the id already exists, it reads that issue and returns it as `Created` with a note that it already existed.

- [ ] **Step 1: Write the failing tests:**
  - `create_mutation_carries_the_client_id_and_every_create_input`
  - `create_id_is_stable_for_the_same_team_title_and_body`
  - `blocked_by_becomes_issue_relation_create_of_type_blocks`
  - `parse_read_maps_ids_back_to_declared_names`
  - `an_existing_id_is_read_back_not_duplicated` (recorded "already exists" error, then a read)

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run -p devkit-common linear_fields`
Expected: FAIL

- [ ] **Step 3: Implement.** Confirm that `IssueCreateInput.id` is still accepted, and the exact duplicate-id error text, against Linear's schema with a `LINEAR_API_KEY` introspection query before recording the fixture. If Linear no longer accepts a client id, stop and report back, because the spec's duplicate guard depends on it.

- [ ] **Step 4: Run them and see them pass**

Run: `cargo nextest run -p devkit-common linear_fields`
Expected: PASS

- [ ] **Step 5: Commit** `feat(tracker): write ticket fields on Linear`

---

### Task 7: `ticket create` and `ticket edit` on Linear

**Files:**
- Modify: `src/bin/devkit/issue/create.rs`, `src/bin/devkit/issue/edit.rs`, `src/bin/devkit/issue/mod.rs` (replace `github_only` for these two verbs with a dispatch on tracker kind; help text drops "GitHub")
- Modify: `plugin/skills/using-devkit/references/ticket.md`
- Test: `tests/issue_create_linear.rs` using `linearfake` and `DEVKIT_LINEAR_URL`

**Interfaces:**
- Consumes: Task 6 `LinearFieldWriter`, `create_id`; Task 2 `resolve` with `team` taken from the resolved `team` field.

- [ ] **Step 1: Write the failing tests:**
  - `create_on_linear_writes_fields_and_prints_the_identifier`
  - `create_without_a_team_field_names_the_missing_declaration`
  - `a_label_from_another_team_is_refused_before_any_request` (the fake records no bodies)
  - `a_retried_create_returns_the_first_ticket` (the fake answers "already exists")
  - `edit_on_linear_reads_first_and_writes_only_named_fields`
  - Update `a_linear_tracker_is_pointed_at_render` in `tests/issue_create.rs`: Linear create now succeeds, so the test is replaced by `a_none_tracker_is_refused`.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run --test issue_create_linear`
Expected: FAIL (the `ticket create` Linear refusal)

- [ ] **Step 3: Implement** the dispatch.

- [ ] **Step 4: Run the full gate**

Expected: all green.

- [ ] **Step 5: Commit** `feat(ticket): create and edit Linear tickets`, then open PR B.

---

### Task 8: The MCP field check

**Files:**
- Modify: `crates/devkit-config/src/harness.rs` (`RenderedToolRule` gains `pub tracker: Option<TrackerKind>` and `pub fields: BTreeMap<String, String>`, field name to input key; doctest extended)
- Modify: `src/bin/devkit/hook/mcp.rs` (`Verdict` gains `AllowWith(String)`; after the receipt check, `respond` runs the field check; `guard` prints `payload.pre_tool_use_context(..)` for `AllowWith` when it returns `Some`)
- Modify: `plugin/skills/using-devkit/references/ticket.md`
- Test: unit tests in `mcp.rs`; e2e in `tests/hook_mcp_issue.rs`

**Interfaces:**
- Consumes: Task 2 `check`, `message`, `Surface::{McpCreate, McpUpdate}`; Task 1 config.
- Produces: `pub(super) fn field_verdict(rule: &RenderedToolRule, input: &Value, cfg: &Config, caller: Caller) -> Verdict` in `mcp.rs`, pure.
- When it runs: `rule.tracker` is `Some(kind)`, `cfg.ticket.fields.for_tracker(kind)` is non-empty, `cfg.tracker.kind` is unset or equal, and for GitHub `cfg.github.issues_repo` is unset or equals the input's `owner`/`repo`. Otherwise `Verdict::Allow` from this check. The config comes from `devkit_common::config::resolve_in(checkout, None, cwd)`, and a load error denies with it.
- MCP values become `Given`: a JSON string is the value; an array of strings is a list; `null` under a mapped key puts the field in `nones`; any other JSON type is `Invalid`.

- [ ] **Step 1: Write the failing tests:**
  - Unit: `create_missing_an_error_field_denies_naming_it_and_the_ticket_create_flag`
  - Unit: `a_field_with_no_mapped_key_is_always_missing_on_create` (GitHub project field on `issue_write`)
  - Unit: `warning_allows_with_context`
  - Unit: `a_call_to_another_repo_gets_no_field_check`
  - Unit: `a_rule_without_tracker_gets_no_field_check`
  - Unit: `null_is_explicit_none`
  - Unit: `a_linear_label_valid_in_any_team_is_accepted`
  - Unit Review Focus 5: `a_value_of_the_wrong_json_type_is_refused`
  - e2e: `a_save_issue_without_priority_is_denied_after_a_valid_render` (render receipt present, field missing)
  - e2e: `a_broken_config_denies_once_a_rule_matched`

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run mcp:: && cargo nextest run --test hook_mcp_issue`
Expected: FAIL

- [ ] **Step 3: Implement.** The receipt check stays first and unchanged.

- [ ] **Step 4: Run the full gate**, including `cargo nextest run --test hook_exit_codes`

Expected: all green

- [ ] **Step 5: Commit** `feat(hook): check ticket fields on tracker MCP writes`

---

### Task 9: `ticket fields discover` on GitHub

**Files:**
- Create: `src/bin/devkit/issue/fields_discover.rs` (shared merge and prompt) and `crates/devkit-common/src/tracker/github_discover.rs` (query and parser)
- Modify: `src/bin/devkit/issue/mod.rs` (`TicketCmd::Fields` gains `#[command(subcommand)] cmd: Option<FieldsCmd>`, with `FieldsCmd::Discover { #[arg(short = 'y', long)] yes: bool, #[arg(long)] team: Vec<String> }`)
- Modify: `Cargo.toml` (root package gains `toml_edit` from the workspace)
- Modify: `plugin/skills/using-devkit/references/ticket.md`
- Test: unit tests for the merge; e2e in `tests/ticket_fields.rs` with `ghfake.serve_graphql`

**Interfaces:**
- Produces:
  - `pub struct Discovered { pub fields: BTreeMap<String, DiscoveredField>, pub unmatched_users: Vec<String> }`, `pub struct DiscoveredField { pub r#type: FieldType, pub list: bool, pub r#where: Place, pub values: Vec<EnumValue> }`.
  - `pub(crate) fn merge(doc: &mut toml_edit::DocumentMut, kind: TrackerKind, merged: &TicketFieldsConfig, found: &Discovered) -> Vec<Change>` where `pub enum Change { Added(String), Refreshed(String), ValueDropped { field: String, value: String } }`.
- Rules (from the spec): refresh `type`/`where`/`values` for any field any layer declares, writing only those keys; add an undeclared field with `level = "info"`; never write `level`, `when`, `default` or `description` for a declared field; skip `[github] status_field`; edit the document in place.
- Prompt: when `caller() == Human` and `--yes` is absent, print a unified diff of the file and read `y` from a terminal. With no terminal and no `--yes`, refuse naming `--yes`. An agent writes without asking.
- Field names: built-ins use the place name (`labels`, `assignees`, `milestone`, `type`, `parent`, `blocked_by`, `project`); a project field uses its name lowercased with non-alphanumerics as `_` (`Priority` becomes `priority`).

- [ ] **Step 1: Write the failing tests:**
  - Unit: `merge_keeps_comments_and_person_set_keys`: a document with a comment and `level = "error"` keeps both after a refresh.
  - Unit: `merge_adds_undeclared_fields_at_info_only`: a field whose `level` comes from the home layer gets no `level` written.
  - Unit: `merge_drops_and_reports_values_gone_from_the_tracker`.
  - Unit: `the_status_field_is_never_added`.
  - e2e: `an_agent_writes_without_asking` (`DEVKIT_CALLER=agent`).
  - e2e: `a_human_without_a_terminal_must_pass_yes`.
  - e2e: `unmatched_users_are_reported_not_written`.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run fields_discover && cargo nextest run --test ticket_fields`
Expected: FAIL

- [ ] **Step 3: Implement.**

- [ ] **Step 4: Run them and see them pass**

Expected: PASS

- [ ] **Step 5: Commit** `feat(ticket): discover GitHub ticket fields`

---

### Task 10: `ticket fields discover` on Linear

**Files:**
- Create: `crates/devkit-common/src/tracker/linear_discover.rs`
- Modify: `src/bin/devkit/issue/fields_discover.rs` (Linear branch)
- Test: unit tests over a recorded response; e2e in `tests/ticket_fields.rs` with `linearfake`

**Interfaces:**
- Consumes: Task 9 `Discovered`, `merge`; Task 6 seam.
- Reads the viewer's teams (or those `--team` names) with their labels, cycles, projects, project milestones, priority scale and estimate settings, plus workspace users. Each team-scoped value records `teams`. A `team` field is added from the teams read.

- [ ] **Step 1: Write the failing tests:** `team_scoped_labels_record_their_teams`, `workspace_labels_have_no_teams`, `team_option_limits_the_teams_read`, `the_workflow_state_is_never_added`.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run linear_discover && cargo nextest run --test ticket_fields linear`
Expected: FAIL

- [ ] **Step 3: Implement.**

- [ ] **Step 4: Run the full gate**

Expected: all green

- [ ] **Step 5: Commit** `feat(ticket): discover Linear ticket fields`, then open PR C.

---

## Unresolved questions

1. **Linear client ids:** the duplicate guard depends on `IssueCreateInput.id` still accepting a client-supplied uuid. Task 6 verifies this first and stops if it fails.
2. **Deterministic id:** the plan derives the Linear id from team, title and body, so a retry of the same create collides with the first. The cost is that two deliberate tickets with identical title and body in one team also collide. I made this call so that a retry in a new process is still caught; the spec only says the id is generated before creating.
