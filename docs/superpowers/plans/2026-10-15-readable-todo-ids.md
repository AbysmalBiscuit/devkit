# Readable Todo Ids Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Todos show and accept a readable name (tracker key, or a word name derived from the uuid) chosen by `[todo] ids` and `--ids`.

**Architecture:** `devkit-todo` gains a `names` module: word names from the uuid's last 24 bits, a pure resolver over a listing, and a `Names` display table the renderers take. The CLI `Store` wrapper's `get` falls back to that resolver when the backend has no exact match, so every command that looks a todo up accepts names. The binary builds `Names` from the style (flag over config) and the caller's tracker, and passes it to every output that prints an id.

**Tech Stack:** Rust 2024, clap, serde/schemars, strum, nextest.

**Spec:** `docs/superpowers/specs/2026-10-15-readable-todo-ids-design.md`

## Global Constraints

- Work in a worktree per ticket (#385, #386, #387). The spec and this plan live on branch `readable-todo-ids-design` until a docs PR lands them on main; read them there with `git show origin/readable-todo-ids-design:<path>`.
- Base on `origin/main` at or after `d0e753da`. #373 has merged (`a14d187e`), so its `short_id` sites in `mod.rs` and `push.rs` are in scope. #374 (`todo promote`) is still in flight; rebase onto main after it merges and route any `short_id` sites it adds through `Names`.
- Styles: `name` (default), `short`, `full`. Config key `[todo] ids`; flag `--ids <style>`, global on `devkit todo`.
- Word names and hex prefixes exist only for uuid ids; a non-uuid id (builtin's `"65"`) shows whole under every style.
- `short` displays exactly today's `short_id` (8 hex). Input accepts any unique hex prefix of 4 or more characters.
- Word name space: two words from a 4096-word list, picked by the uuid's last 24 bits.
- An exact stored id always wins and nothing else is tried.
- Full ids stay in JSON output, the native map, the activity log and stored state. Mermaid node identifiers stay `t{short_id}`; only labels change.
- A config key's meaning is the doc comment on its field; regenerate `schema/devkit-config.json` with `DEVKIT_UPDATE_SCHEMA=1 cargo test`.
- Help text stays ASCII.
- The workspace builds and the gate passes at every commit: `cargo nextest run --workspace --no-fail-fast`, `cargo test --workspace --doc`, `cargo clippy --workspace --all-targets -- -D warnings`, `devrun task fmt`.
- Conventional Commits; comments only for a non-obvious why.

## Review Focus

1. Mixed-case input (`Brave-Otter`, `swe-123`) resolves; names are lowercase on display, keys as stored. Test in Task 3.
2. A quoted `'#65'` resolves the same as `65`. Test in Task 5.
3. A ticket todo from another repository in a global listing shows `owner/repo#N`, not `#N`. Test in Task 5.
4. A ticket todo re-described by hand, so its text no longer starts with the key, prints its text whole. Test in Task 4.
5. A name that matches nothing still fails with today's `no todo <input>` message. Test in Task 3.

---

### Task 1: Word names

**Files:**
- Create: `crates/devkit-todo/src/names.rs`, `crates/devkit-todo/src/words.txt`
- Modify: `crates/devkit-todo/src/lib.rs` (add `pub mod names;`)

**Interfaces:**
- Produces: `devkit_todo::names::word_name(id: &str) -> Option<String>`; `devkit_todo::names::WORDS`, 4096 `&'static str`.

- [ ] **Step 1: Build `words.txt`.** Source: the EFF large wordlist (`https://www.eff.org/files/2016/07/18/eff_large_wordlist.txt`, CC BY 3.0 US). Keep words matching `^[a-z]{3,7}$` and not matching `^[a-f]+$`, sorted, first 4096, one per line. The module's doc comment names the source and license.

- [ ] **Step 2: Write the failing tests** in `names.rs`:

```rust
#[test]
fn the_list_holds_4096_distinct_words_none_of_them_hex() {
    assert_eq!(WORDS.len(), 4096);
    assert_eq!(WORDS.iter().collect::<std::collections::HashSet<_>>().len(), 4096);
    assert!(WORDS.iter().all(|w| (3..=7).contains(&w.len()) && w.bytes().all(|b| b.is_ascii_lowercase())));
    assert!(WORDS.iter().all(|w| !w.bytes().all(|b| (b'a'..=b'f').contains(&b))));
}

#[test]
fn a_uuid_names_itself_from_its_last_24_bits() {
    let id = "96432cd6-082a-4d8c-a9cb-adef8823ff92";
    // last 24 bits 0x23ff92: high 12 bits 0x23f, low 12 bits 0xf92
    assert_eq!(word_name(id), Some(format!("{}-{}", WORDS[0x23f], WORDS[0xf92])));
    assert_eq!(word_name(&id.to_uppercase()), word_name(id));
}

#[test]
fn an_id_that_is_not_a_uuid_has_no_word_name() {
    assert_eq!(word_name("65"), None);
    assert_eq!(word_name("96432cd6"), None);
}
```

- [ ] **Step 3: Run** `cargo nextest run -p devkit-todo names`. Expected: FAIL, module missing.

- [ ] **Step 4: Implement** `WORDS` from `include_str!("words.txt")` and `word_name`: accept only the 36-character uuid form `short_id` recognises, parse it with `uuid::Uuid::try_parse`, take bytes 13..16 as a `u32`, and join `WORDS[(v >> 12) & 0xfff]` and `WORDS[v & 0xfff]` with `-`. Those bytes are random in v4 uuids and in the v5 uuids `ticket_todo_uuid` makes.

- [ ] **Step 5: Run** the same command. Expected: PASS.

- [ ] **Step 6: Commit** `feat(todo): derive word names from todo uuids`

### Task 2: `[todo] ids` config

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (`TodoConfig`, its doctest, `Default`), `crates/devkit-config/src/todo.rs` (`Input` and its conversions), `schema/devkit-config.json`

**Interfaces:**
- Produces: `devkit_config::IdStyle { Name, Short, Full }`, `Default` = `Name`, serde and strum spellings `name`/`short`/`full`, `strum::VariantNames` and `FromStr` as `HoldStop` has them; field `TodoConfig::ids: IdStyle`.

- [ ] **Step 1: Write the failing doctest lines** in `TodoConfig`'s doctest:

```rust
/// # assert_eq!(empty.todo.ids, IdStyle::Name);
/// # assert_eq!(Config::parse("[todo]\nids = \"short\"\n").unwrap().todo.ids, IdStyle::Short);
/// # assert_eq!(Config::parse("[todo]\nids = \"full\"\n").unwrap().todo.ids, IdStyle::Full);
/// # assert!(Config::parse("[todo]\nids = \"hex\"\n").is_err());
```

- [ ] **Step 2: Run** `cargo test -p devkit-config --doc TodoConfig`. Expected: FAIL.

- [ ] **Step 3: Implement** the enum and field. The field's doc comment is the user-facing definition: what each style shows for a ticket todo, another uuid todo and a non-uuid id (the spec's table, in prose), that every form is accepted as input whatever the style, that `--ids` overrides it per command, and that JSON output always carries full ids.

- [ ] **Step 4: Regenerate the schema** with `DEVKIT_UPDATE_SCHEMA=1 cargo test -p devkit-config`, then run `cargo test -p devkit-config`. Expected: PASS.

- [ ] **Step 5: Commit** `feat(config): add the todo ids display style`

### Task 3: The resolver

**Files:**
- Modify: `crates/devkit-todo/src/names.rs`, `src/bin/devkit/todo/tickets.rs` (drop its `names_issue` and import the moved one, moving its unit assertions too), `src/bin/devkit/todo/store.rs`, `src/bin/devkit/todo/mod.rs` (`Describe`, `Move`, `Add --parent`)
- Create: `src/bin/devkit/todo/names.rs`, `tests/todo_names.rs`

**Interfaces:**
- Consumes: `word_name` (Task 1).
- Produces:
  - `devkit_todo::names::names_issue(key: &str, issue: &str) -> bool`, moved verbatim from `tickets.rs`.
  - `devkit_todo::names::resolve(input: &str, todos: Vec<Todo>, tracker_key: Option<&dyn Fn(&str) -> Option<String>>) -> anyhow::Result<Option<Todo>>`. `tracker_key` is `None` when no tracker is in reach; otherwise it maps a GitHub short form to the full key, or to `None` when the tracker refuses it.
  - `pub(crate) fn tracker_key(cwd: &Path) -> Option<Box<dyn Fn(&str) -> Option<String>>>` in `src/bin/devkit/todo/names.rs`: selects the tracker with `tracker::select(None, &cwd.to_string_lossy(), None)`, returns `None` unless it is GitHub, and wraps `Tracker::ticket_key` with errors mapped to `None`.

`resolve` matches each todo, every status, as any of:
- **ticket key:** `ticket.key` equals `input` ignoring ASCII case; or the input is a GitHub short form (`#N`, `N`, or an `http` URL) and either `tracker_key` is `Some` and its result equals `ticket.key` ignoring case, or `tracker_key` is `None`, the ticket's tracker is `github` and `names_issue(&ticket.key, N)`.
- **word name:** `word_name(&todo.id)` equals `input` lowercased.
- **hex prefix:** `input` is 4 or more characters of hex and dashes, the id is a uuid, and it starts with `input` lowercased.

One distinct todo is `Some`, none is `None`, and more is the error `todo name {input} is ambiguous: {a}, {b}` naming each match by `short_id`, sorted.

- [ ] **Step 1: Write failing unit tests** in `names.rs`, building `Todo`s by hand: uuid ids from fixed strings, ticket todos with `TicketRef { tracker: "github", key: "o/r#65", .. }` and `TicketRef { tracker: "linear", key: "SWE-123", .. }`.
  - `a_full_key_resolves_ignoring_case`: `resolve("swe-123", ..)` and `resolve("O/R#65", ..)` return those todos.
  - `a_number_matches_every_github_ticket_without_a_tracker`: with `o/r#65` and `x/y#65` and `tracker_key: None`, `resolve("65", ..)` errs, and the message contains both `short_id`s.
  - `a_tracker_reads_the_short_form`: with the same two and a `tracker_key` mapping `"#65"` and `"65"` to `"o/r#65"`, `resolve("#65", ..)` returns the `o/r#65` todo.
  - `a_refused_short_form_is_not_a_key`: with a `tracker_key` returning `None`, `resolve("65", ..)` is `Ok(None)`.
  - `a_word_name_resolves_in_any_case`: `resolve(&word_name(id).unwrap().to_uppercase(), ..)` returns it.
  - `a_four_char_prefix_resolves_and_three_does_not`.
  - `a_match_across_forms_is_ambiguous`: a todo whose uuid starts with `1234` and a GitHub ticket `o/r#1234`; `resolve("1234", .., None)` errs naming both.
  - `a_cancelled_todo_whose_text_is_the_key_is_not_matched`: a cancelled todo with description `"#65"` and no `ticket`, beside the live `o/r#65` todo; `resolve("#65", ..)` returns the live one.

- [ ] **Step 2: Run** `cargo nextest run -p devkit-todo names`. Expected: FAIL.

- [ ] **Step 3: Implement** `resolve`, and move `names_issue`.

- [ ] **Step 4: Run** the same command. Expected: PASS.

- [ ] **Step 5: Write failing CLI tests** in `tests/todo_names.rs`. Use `todoenv::Proj`, the taskchampion backend through `DEVKIT_TODO_BACKEND=taskchampion` for uuid ids, and `tickets::Synced` / `two_issues()` for the GitHub tickets `o/r#65` and `o/r#66`.
  - `start_takes_a_word_name`: on taskchampion, add a todo, read its full id from the replica, and `todo start <word_name(full id)>` leaves it in progress.
  - `start_takes_a_four_char_prefix`, `describe_takes_a_word_name`, `move_parent_takes_a_word_name`, `add_parent_takes_a_word_name`: one per call site.
  - `start_takes_a_bare_ticket_number`: after `two_issues().sync(&[])`, `devkit todo start 65` succeeds and `todo_for("o/r#65")` is in progress.
  - `an_exact_builtin_id_wins_over_a_ticket_number`: add a plain todo (builtin id `1`), sync one GitHub issue numbered 1 (`issue(1, ..)` through `graphql`), and `todo start 1` starts the plain todo, not the ticket.
  - `an_unmatched_name_says_no_todo`: `todo start brave-nothing` fails with stderr containing `no todo brave-nothing`.

- [ ] **Step 6: Run** `cargo nextest run --test todo_names`. Expected: FAIL.

- [ ] **Step 7: Implement `Store::get`** in `store.rs`: the backend's `get` first, returning its match; otherwise `names::resolve(id, listing, tracker_key)` over `self.store.list(&Filter::all())`. Keep the listing in a `Mutex<Option<Vec<Todo>>>` field, filled on first use and cleared in `add` and `apply`, so a command lists the store at most once. Build `tracker_key` from `self.origin`'s path, and only when `id` is a GitHub short form; `origin: None` passes `None`.

- [ ] **Step 8: Route the raw-string sites** through `resolve_id`: `Describe { id }`, `Move { parent }` and `Add { parent }` in `mod.rs`, putting the resolved `todo.id` in the `Edit` or `NewTodo`.

- [ ] **Step 9: Run** `cargo nextest run --test todo_names`, then the full gate. Expected: PASS.

- [ ] **Step 10: Commit** `feat(todo): accept ticket keys and names as todo ids`

### Task 4: The display table and renderers

**Files:**
- Modify: `crates/devkit-todo/src/names.rs`, `crates/devkit-todo/src/ticket.rs`, `crates/devkit-todo/src/render.rs`, and the binary's callers of `render_plan`, `render_mermaid` and `Notes` (`src/bin/devkit/todo/mod.rs`, `src/bin/devkit/hook/todo.rs`)

**Interfaces:**
- Consumes: `word_name` (Task 1), `IdStyle` (Task 2).
- Produces:
  - `devkit_todo::ticket::key_prefix(tracker: &str, key: &str) -> String`: `"#65"` for GitHub, the key otherwise. `todo_text` calls it.
  - `devkit_todo::names::Names`:
    - `Names::new(style: IdStyle, todos: &[Todo], shown_key: impl Fn(&TicketRef) -> String) -> Names`, recording `shown_key(ticket)` as the name of each ticket todo in `todos`.
    - `Names::plain(style: IdStyle) -> Names`, holding no todos.
    - `fn id<'a>(&'a self, id: &'a str) -> Cow<'a, str>`. `Full`: the id. `Name` or `Short` for a recorded ticket todo: its shown key. Otherwise `Name` gives `word_name(id)` and `Short` gives `short_id(id)`, each falling back to the whole id when it is not a uuid.
    - `fn text<'a>(&self, todo: &'a Todo) -> &'a str`. When `id` shows the todo's key, its description with the leading `"{key_prefix(tracker, key)} "` removed if present; otherwise the description.
  - `render_plan(root, plan, work, names: &Names)`, `render_mermaid(plan, names: &Names)`, and `Notes { .., names: &'a Names }`, which `render_lists`, `section`, `line`, `work_line` and `blocking_note` read.

- [ ] **Step 1: Write failing unit tests** in `names.rs` and `render.rs`:
  - `name_style_shows_a_word_a_key_and_a_builtin_id`: `Names::new(IdStyle::Name, ..)` over a uuid todo, a ticket todo with shown key `"#65"`, and id `"7"`.
  - `short_and_full_styles`: the same todos under `Short` (8 hex, `#65`, `7`) and `Full` (stored ids).
  - `text_strips_the_key_the_description_starts_with`: `"#65 Issue 65"` gives `"Issue 65"`; `"Renamed by hand"` stays whole; under `Full`, `"#65 Issue 65"` stays whole.
  - `plain_names_match_todays_output`: `Names::plain(IdStyle::Short)` gives `short_id` for every id and leaves descriptions whole.
  - `plan_rows_show_names`: a `render_plan` row for a ticket todo reads `o #65  Issue 65`, and a uuid todo's row shows its word name.
  - `list_lines_show_names_in_parentheses`: the `render_lists` line is `- [ ] Issue 65 (#65)`.
  - `mermaid_keeps_hex_node_ids_and_labels_names`: node `t96432cd6["<word name>: text"]`.

- [ ] **Step 2: Run** `cargo nextest run -p devkit-todo`. Expected: FAIL.

- [ ] **Step 3: Implement** `key_prefix` and `Names`, and thread `&Names` through `render.rs`: every `crate::short_id` that prints for a reader becomes `names.id(..)`, and every printed description becomes `names.text(todo)`. Mermaid node identifiers keep `crate::short_id`. The binary's callers pass `&Names::plain(IdStyle::Short)` for now, which reproduces today's output.

- [ ] **Step 4: Run** the full gate. Expected: PASS, with no change to existing CLI tests.

- [ ] **Step 5: Commit** `feat(todo): render todos through a names table`

### Task 5: CLI style, outputs and agent text

**Files:**
- Modify: `src/bin/devkit/todo/mod.rs`, `src/bin/devkit/todo/names.rs`, `src/bin/devkit/todo/push.rs`, `src/bin/devkit/hook/todo.rs`, `src/bin/devkit/activity.rs`, `crates/devkit-todo/src/render.rs` (`guide`), `plugin/skills/using-devkit/references/todo.md`
- Test: `tests/todo_names.rs`, plus existing tests that assert 8-hex display on uuid backends: `tests/todo_taskchampion.rs` `the_env_selects_taskchampion_with_no_config`, and any others the gate turns up in `todo_postgres.rs`, `todo_supabase.rs`, `todo_replica.rs` or `todo_hooks.rs`

**Interfaces:**
- Consumes: `Names` (Task 4), `IdStyle` (Task 2), `tracker_key`'s tracker selection (Task 3).
- Produces:
  - `TodoCli { #[arg(long, global = true, value_parser = id_styles())] ids: Option<IdStyle>, .. }`, with `id_styles()` built like `hold_modes()`.
  - `pub(crate) fn names(style: IdStyle, todos: &[Todo], cwd: &Path) -> Names` in `src/bin/devkit/todo/names.rs`. Its `shown_key`: a GitHub key `owner/repo#N` shows `#N` when the cwd's tracker reads `#N` back to that key ignoring case, else the whole key; any other key shows whole. The tracker is selected once, and only when `todos` holds a GitHub ticket todo.

- [ ] **Step 1: Write failing CLI tests** in `tests/todo_names.rs`:
  - `ready_and_plan_show_word_names_by_default`: on taskchampion, `ready` and `plan` print the word name and not the 8-hex prefix.
  - `the_flag_overrides_the_config`: with `[todo] ids = "short"` in the home config, `ready` prints 8 hex, `ready --ids full` the full uuid, and `ready --ids name` the word name.
  - `builtin_ids_show_whole`: on builtin, `ready` prints `1`.
  - `ticket_rows_show_the_key_once`: after `two_issues().sync(&[])`, `plan` contains `#65  Issue 65` and not `#65 #65`, and `list` contains `Issue 65 (#65`.
  - `a_quoted_hash_key_resolves`: `todo start` with the single argv element `"#65"` starts it.
  - `another_repos_ticket_shows_its_full_key`: a GitHub ticket todo keyed `x/y#7`, added through `p.store().add(NewTodo { ticket: Some(..), .. })`, shows as `x/y#7` in `list --all` run in the `o/r` project.
  - `list_json_keeps_full_ids`: `list --json` `id` fields equal the stored ids.
  - `add_prints_the_name`: on taskchampion, `todo add x` prints the word name.

- [ ] **Step 2: Run** `cargo nextest run --test todo_names`. Expected: FAIL.

- [ ] **Step 3: Implement.** Resolve the style once in the todo entry point as `cli.ids.unwrap_or(config.todo.ids)`; `context` and the stop hook use `config.todo.ids`. Build `Names` with `names(..)` from the todos each output already holds (`Fence::todos()` for ready, plan, list and context) and replace the `Names::plain` placeholders from Task 4. Route every reader-facing `devkit_todo::short_id` in `mod.rs` through it: `add`'s printed id, `unblocked:`, the cycle refusal, the blocked-start refusal, and the status-change and push summaries. Do the same for the messages in `push.rs` and the hold reason in `hook/todo.rs`. `devkit activity`'s held-todo table uses `Names::plain(config.todo.ids)`, since it holds no todos.

- [ ] **Step 4: Run** `cargo nextest run --workspace --no-fail-fast`. For a test failing only because it asserts the old 8-hex display on a uuid backend: when the test is about the printed id, assert the word name; otherwise pass `--ids short`. Any other failure is a bug in this task. Re-run until PASS.

- [ ] **Step 5: Agent text.** In `render::guide`, replace `- Ids are in parentheses.` with a line saying ids are in parentheses, and that a GitHub ticket's `#N` is typed quoted (`'#65'`) or as its full key, because `#` starts a shell comment and a bare number loses to an equal builtin id. In `references/todo.md`, state the same rule and add one short paragraph on `--ids` and `[todo] ids` that points at `devkit schema` for the styles. Update guide-text assertions in `tests/todo_context.rs` if any.

- [ ] **Step 6: Gate**: nextest, doctests, clippy, fmt. Expected: all PASS. Optional and billed: `devrun task eval --arg eval_case=brief` checks agents still read the guide correctly.

- [ ] **Step 7: Commit** `feat(todo): show readable todo names in listings`
