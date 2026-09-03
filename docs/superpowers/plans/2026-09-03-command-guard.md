# Command guard implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `devkit harness shell`, a pre-execution hook that denies a shell command when devkit already has a wired-up path for it and hands the agent the replacement command.

**Architecture:** A lenient per-key `[harness]` probe answers the gate and the user rules without a full config load. When neither settles it, the resolved config supplies tasks, apps, and the app catalog. A purpose-built shell lexer splits the command into segments; each segment is normalized (assignments, process wrappers, runner prefixes stripped) and tested against five sources in order. Every failure path exits 0.

**Tech Stack:** Rust 2024, `anyhow`, `serde`/`toml`, `clap`, `frizbee` (new), `tempfile` for test scratch.

**Spec:** `docs/superpowers/specs/2026-09-03-command-guard-design.md`

## Global constraints

- `cargo nextest run --workspace --no-fail-fast` and `cargo clippy --workspace --all-targets -- -D warnings` must both pass at every commit. Format with `cargo fmt --all`.
- Markdown in this repo is not hand-wrapped. Write each paragraph as one line.
- Test scratch comes from `tempfile`. Never build a path by hand from `std::env::temp_dir()`. A helper returning a path derived from a `TempDir` must hand back the guard too.
- No `_ => ` catch-all arms over `Role` or `StateKind`.
- The guard never mutates. It allocates no port, writes no registry row, takes no lock, and never calls `task::resolve` / `task::resolve_step` / `task::resolve_command`.
- The guard never exits non-zero and never panics out. Any failure warns on stderr and exits 0.
- `devkit-config` is a leaf crate with no internal library dependencies. Nothing added to it may import `devkit-common`.
- Comments are timeless: no PR/issue references, no `now we` / `used to`, no RED/GREEN narration.

---

### Task 1: Move the harness plumbing into the shared crates

Pure relocation. No behaviour changes. The suite must be green before and after with the same assertions.

**Files:**
- Create: `crates/devkit-config/src/harness.rs`
- Create: `crates/devkit-common/src/harness.rs`
- Modify: `crates/devkit-config/src/lib.rs` (add `pub mod harness;`, make `merge_layers` public)
- Modify: `crates/devkit-common/src/lib.rs` (add `pub mod harness;`)
- Modify: `crates/devkit-locks/src/hook.rs` (delete the moved items, re-export)
- Modify: `src/bin/devkit/schema.rs:47-49` (new path for `HarnessSection`)

**Interfaces:**
- Consumes: nothing.
- Produces: `devkit_config::harness::HarnessSection { enforce_writes: bool }`; `devkit_config::merge_layers(&[(PathBuf, toml::Table)]) -> (toml::Table, HashMap<String, PathBuf>, HashMap<String, Vec<Shadow>>)` now `pub`; `devkit_common::harness::{deny_json, parse_env_override, global_config_path, resolve_enforcement, enforcement_enabled}`, where `enforcement_enabled(cwd: &Path, flag: &str, env_var: &str) -> bool`.

- [ ] **Step 1: Write the failing test for the generalized gate**

Add to `crates/devkit-common/src/harness.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_beats_both_file_sources() {
        assert!(!resolve_enforcement(Some(false), || true, || true));
        assert!(resolve_enforcement(Some(true), || false, || false));
    }

    #[test]
    fn either_file_source_enables() {
        assert!(resolve_enforcement(None, || true, || false));
        assert!(resolve_enforcement(None, || false, || true));
        assert!(!resolve_enforcement(None, || false, || false));
    }

    #[test]
    fn env_override_parses_both_spellings() {
        assert_eq!(parse_env_override(Some(" ON ")), Some(true));
        assert_eq!(parse_env_override(Some("0")), Some(false));
        assert_eq!(parse_env_override(Some("maybe")), None);
        assert_eq!(parse_env_override(None), None);
    }
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo nextest run -p devkit-common harness`
Expected: FAIL — the module does not exist.

- [ ] **Step 3: Create `crates/devkit-config/src/harness.rs`**

```rust
//! The `[harness]` table: the coding-agent enforcement opt-ins.

use serde::Deserialize;

/// The `[harness]` table of a checkout's `devkit.toml`.
///
/// This is the shape `devkit schema` renders. Nothing at runtime deserializes
/// the table through it: the probe reads each key independently so one bad key
/// cannot take the others down with it.
#[derive(Deserialize, Default, Debug, Clone, PartialEq, schemars::JsonSchema)]
pub struct HarnessSection {
    /// Refuse writes to paths this checkout has not claimed with `lockm`.
    #[serde(default)]
    pub enforce_writes: bool,
}
```

- [ ] **Step 4: Export it and make `merge_layers` public**

In `crates/devkit-config/src/lib.rs`, add `pub mod harness;` beside the other module declarations and re-export the type: `pub use harness::HarnessSection;`.

Change the visibility of `merge_layers` from `pub(crate)` to `pub`, and extend its doc comment with the second caller:

```rust
/// Deep-merge parsed layers given lowest→highest precedence. Tables merge key by
/// key; every non-table value (scalar or array) is replaced wholesale by a higher
/// layer. Records, per leaf dotted-path, the highest layer that set it.
///
/// Public because the `[harness]` probe merges that one table across the same
/// layer files without resolving the whole config, and must not carry a second
/// copy of these semantics.
pub fn merge_layers(
```

- [ ] **Step 5: Create `crates/devkit-common/src/harness.rs`**

Move `deny_json`, `parse_env_override`, `global_config_path`, `resolve_enforcement`, `harness_enabled` and `enforcement_enabled` out of `crates/devkit-locks/src/hook.rs` verbatim, then generalize the two entry points over which flag they read. `harness_flag_in` moves too and keeps its current body for now; Task 2 replaces it.

```rust
//! Coding-agent harness glue shared by every devkit hook: the deny envelope,
//! and the per-checkout activation gate over the `[harness]` table.

use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The Claude Code / Codex `PreToolUse` deny envelope. `reason` reaches the agent.
pub fn deny_json(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason
        }
    })
}

#[derive(Deserialize, Default)]
struct Probe {
    #[serde(default)]
    harness: devkit_config::HarnessSection,
}

fn harness_flag_in(body: &str, flag: &str) -> bool {
    let Ok(p) = toml::from_str::<Probe>(body) else {
        return false;
    };
    match flag {
        "enforce_writes" => p.harness.enforce_writes,
        _ => false,
    }
}

/// Parse an enforcement env override into an explicit on/off, or `None` when
/// unset/blank/unrecognized. Case- and whitespace-insensitive.
pub fn parse_env_override(val: Option<&str>) -> Option<bool> {
    match val.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "1" | "true" | "yes" | "on") => Some(true),
        Some(v) if matches!(v.as_str(), "0" | "false" | "no" | "off") => Some(false),
        _ => None,
    }
}

/// The global devkit config file: `$DEVKIT_CONFIG`, else `~/.config/devkit/config.toml`.
pub fn global_config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DEVKIT_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/devkit/config.toml"))
}

/// Combine the enforcement opt-in sources. The env override is an explicit
/// on/off master switch; without it, enforcement is on when either a project
/// layer or the global config opts in. `checkout` and `global` are thunks
/// because each does filesystem and, for `checkout`, git work that an explicit
/// override must not pay for.
pub fn resolve_enforcement(
    env: Option<bool>,
    checkout: impl FnOnce() -> bool,
    global: impl FnOnce() -> bool,
) -> bool {
    match env {
        Some(v) => v,
        None => checkout() || global(),
    }
}

/// True iff any project layer applying at `cwd` sets `[harness] <flag>`.
/// Combined with `any` rather than by precedence: enforcement ratchets on, and
/// only the env override turns it off, so one layer opting in must win even if
/// a closer layer leaves the flag unset.
fn harness_enabled(cwd: &Path, flag: &str) -> bool {
    let main = crate::git::main_checkout(cwd).ok().flatten();
    let Ok(layers) = devkit_config::project_layers(cwd, main.as_deref()) else {
        return false;
    };
    layers.iter().any(|layer| {
        std::fs::read_to_string(&layer.path)
            .map(|b| harness_flag_in(&b, flag))
            .unwrap_or(false)
    })
}

fn global_harness_enabled(flag: &str) -> bool {
    global_config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|b| harness_flag_in(&b, flag))
        .unwrap_or(false)
}

/// Whether the named `[harness]` flag is active for an action originating at
/// `cwd`, across the env override, the project layers, and the global config.
/// Takes the working directory rather than a pre-resolved checkout root, so a
/// declaration in a directory between the root and the action is part of the
/// answer.
pub fn enforcement_enabled(cwd: &Path, flag: &str, env_var: &str) -> bool {
    resolve_enforcement(
        parse_env_override(std::env::var(env_var).ok().as_deref()),
        || harness_enabled(cwd, flag),
        || global_harness_enabled(flag),
    )
}
```

Add `pub mod harness;` to `crates/devkit-common/src/lib.rs`.

- [ ] **Step 6: Point `devkit-locks` at the moved items**

In `crates/devkit-locks/src/hook.rs`, delete the moved functions and `HarnessSection`, then add at the top:

```rust
pub use devkit_common::harness::deny_json;
pub use devkit_config::HarnessSection;

/// Whether write enforcement is active for a write originating at `cwd`.
pub fn enforcement_enabled(cwd: &Path) -> bool {
    devkit_common::harness::enforcement_enabled(cwd, "enforce_writes", "DEVKIT_ENFORCE_WRITES")
}
```

Six tests in `hook.rs` call functions this step moved and will not compile where they are. Move all of them into the `tests` module of `crates/devkit-common/src/harness.rs`, verbatim apart from the `enforcement_enabled` call sites, which gain the two new arguments (`"enforce_writes"`, `"DEVKIT_ENFORCE_WRITES"`):

- `resolve_enforcement_*` and `parse_env_override_*` (pure, no fixtures)
- `harness_enabled_reads_flag` (line 389)
- `harness_flag_in_reads_section_leniently` (line 428)
- `harness_declared_in_a_nested_directory_is_honored` (line 443)
- `harness_is_inherited_from_the_main_checkout` (line 469)

The last three build git fixtures through `devkit_common::git::Git::fixture`, which is available in `devkit-common`'s own tests, so they need no new dev-dependency.

Then prune what the move orphaned in `hook.rs`: `use serde::Deserialize;` and `PathBuf` become unused, and an unused import is a `-D warnings` failure on the lib target, not just a lint.

Callers outside the crate are already covered: `src/bin/devkit/locks.rs:140`, `:165` and `:182` use `hook::deny_json` and `hook::enforcement_enabled`, which the re-exports above preserve, and `schema.rs` is step 7.

- [ ] **Step 7: Update the schema root**

In `src/bin/devkit/schema.rs`, change the `harness` field's type from `devkit_locks::hook::HarnessSection` to `devkit_config::HarnessSection`.

- [ ] **Step 8: Run the full gate**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS, with the same test count as before minus the two relocated unit tests.

Run: `cargo test --workspace --doc && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Confirm the schema did not drift**

Run: `cargo nextest run -p devkit config_schema`
Expected: PASS. The JSON is unchanged: the struct moved, its shape did not.

- [ ] **Step 10: Commit**

```bash
git add crates/devkit-config/src/harness.rs crates/devkit-common/src/harness.rs \
        crates/devkit-config/src/lib.rs crates/devkit-common/src/lib.rs \
        crates/devkit-locks/src/hook.rs src/bin/devkit/schema.rs
git commit -m "refactor(harness): share the gate and deny envelope"
```

---

### Task 2: Replace the struct probe with a per-key probe

The whole `[harness]` table currently deserializes as one struct, so any parse failure reads as "off". Once rules live in that table, a mistyped rule would silently disable `enforce_writes`. This task removes that coupling before any rule exists.

**Files:**
- Modify: `crates/devkit-common/src/harness.rs`
- Test: `crates/devkit-common/src/harness.rs` (unit), `tests/lock_harness_race.rs` stays untouched

**Interfaces:**
- Consumes: Task 1's `devkit_common::harness`.
- Produces: `pub fn harness_flag_in(body: &str, flag: &str) -> bool` reading one key from a `toml::Table` without deserializing its siblings.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/devkit-common/src/harness.rs`:

```rust
#[test]
fn a_malformed_sibling_key_does_not_disable_the_flag() {
    let body = r#"
[harness]
enforce_writes = true

[harness.commands.bun-only]
programs = "node"
"#;
    assert!(harness_flag_in(body, "enforce_writes"));
}

#[test]
fn a_wrong_typed_flag_reads_as_off() {
    assert!(!harness_flag_in("[harness]\nenforce_writes = \"yes\"\n", "enforce_writes"));
}

#[test]
fn a_syntax_error_reads_as_off() {
    assert!(!harness_flag_in("[[[", "enforce_writes"));
}

#[test]
fn an_absent_table_reads_as_off() {
    assert!(!harness_flag_in("[defaults]\napps_dir = \"apps\"\n", "enforce_writes"));
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo nextest run -p devkit-common harness::tests::a_malformed_sibling`
Expected: FAIL — `programs = "node"` fails the struct deserialization, so the flag reads false.

- [ ] **Step 3: Rewrite the probe against `toml::Table`**

Replace `harness_flag_in` and delete the `Probe` struct:

```rust
/// Read one `[harness]` flag from a config body.
///
/// Parses to a `toml::Table` and reads the single key, so a malformed or
/// unrelated sibling key cannot change this answer. A body that is not valid
/// TOML, a missing table, and a key of the wrong type all read as off.
pub fn harness_flag_in(body: &str, flag: &str) -> bool {
    toml::from_str::<toml::Table>(body)
        .ok()
        .and_then(|t| t.get("harness")?.get(flag)?.as_bool())
        .unwrap_or(false)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p devkit-common harness`
Expected: PASS, all four new tests plus the three from Task 1.

- [ ] **Step 5: Run the write-lock end-to-end suite unchanged**

Run: `cargo nextest run -p devkit locks lock_harness_race`
Expected: PASS. The gate's observable behaviour is identical for every well-formed config.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-common/src/harness.rs
git commit -m "fix(harness): read each gate flag independently

The whole [harness] table deserialized as one struct, so any parse
failure read as off. With rules about to live in that table, a mistyped
rule would have disabled write enforcement in the same file."
```

---

### Task 3: The rule table, and merging it across layers

**Files:**
- Modify: `crates/devkit-config/src/harness.rs`
- Modify: `crates/devkit-common/src/harness.rs`
- Test: both, as unit tests

**Interfaces:**
- Consumes: Task 2's probe, `devkit_config::merge_layers`.
- Produces: `devkit_config::harness::CommandRule { programs: Vec<String>, args: Vec<String>, reason: String }` and `devkit_config::harness::AppMatch { fuzzy: bool, max_typos: u16, min_score: u16 }`; `HarnessSection` gains `enforce_commands: bool`, `commands: BTreeMap<String, CommandRule>` and `app_match: AppMatch`; `devkit_common::harness::HarnessRules { commands: BTreeMap<String, CommandRule>, app_match: AppMatch }` with `resolve_rules(cwd: &Path) -> (HarnessRules, Vec<String>)` returning the merged tables and the warnings for what was skipped; `devkit_common::harness::commands_enabled(cwd: &Path) -> bool`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/devkit-common/src/harness.rs` tests:

```rust
use std::path::PathBuf;

fn layer(name: &str, body: &str) -> (PathBuf, toml::Table) {
    (PathBuf::from(name), toml::from_str(body).expect("layer parses"))
}

#[test]
fn a_child_layer_adds_a_rule_and_keeps_the_parents() {
    let (h, warns) = merge_rules(&[
        layer("root", "[harness.commands.bun-only]\nprograms = [\"node\"]\nreason = \"use bun\"\n"),
        layer("child", "[harness.commands.no-curl]\nprograms = [\"curl\"]\nreason = \"use ureq\"\n"),
    ]);
    assert_eq!(h.commands.len(), 2);
    assert!(warns.is_empty());
    assert_eq!(h.commands["bun-only"].reason, "use bun");
    assert_eq!(h.commands["no-curl"].programs, vec!["curl"]);
}

#[test]
fn a_same_named_child_rule_overrides_only_the_keys_it_sets() {
    let (h, _) = merge_rules(&[
        layer("root", "[harness.commands.bun-only]\nprograms = [\"node\"]\nreason = \"use bun\"\n"),
        layer("child", "[harness.commands.bun-only]\nprograms = []\n"),
    ]);
    assert!(h.commands["bun-only"].programs.is_empty());
    assert_eq!(h.commands["bun-only"].reason, "use bun");
}

#[test]
fn a_rule_with_no_programs_after_merging_is_skipped_with_a_warning() {
    let (h, warns) = merge_rules(&[layer("root", "[harness.commands.oops]\nreason = \"hi\"\n")]);
    assert!(h.commands.is_empty());
    assert_eq!(warns.len(), 1);
    assert!(warns[0].contains("oops"), "warning names the rule: {}", warns[0]);
}

#[test]
fn a_malformed_rule_is_skipped_and_its_siblings_survive() {
    let (h, warns) = merge_rules(&[layer(
        "root",
        "[harness.commands.bad]\nprograms = \"node\"\n\
         [harness.commands.good]\nprograms = [\"curl\"]\nreason = \"use ureq\"\n",
    )]);
    assert_eq!(h.commands.len(), 1);
    assert!(h.commands.contains_key("good"));
    assert_eq!(warns.len(), 1);
    assert!(warns[0].contains("bad"));
}

#[test]
fn an_absent_app_match_is_the_default() {
    let (h, warns) = merge_rules(&[layer("root", "[harness]\nenforce_commands = true\n")]);
    assert!(warns.is_empty());
    assert_eq!(h.app_match, devkit_config::AppMatch::default());
}

#[test]
fn app_match_merges_key_by_key_across_layers() {
    let (h, warns) = merge_rules(&[
        layer("root", "[harness.app_match]\nmax_typos = 2\nmin_score = 40\n"),
        layer("child", "[harness.app_match]\nmin_score = 80\n"),
    ]);
    assert!(warns.is_empty());
    assert_eq!(h.app_match.max_typos, 2, "inherited from the parent layer");
    assert_eq!(h.app_match.min_score, 80, "the child's own value wins");
    assert!(h.app_match.fuzzy, "a key neither layer sets keeps its default");
}

#[test]
fn a_malformed_app_match_falls_back_to_the_defaults_with_a_warning() {
    let (h, warns) = merge_rules(&[layer(
        "root",
        "[harness.app_match]\nmax_typos = \"lots\"\n\
         [harness.commands.good]\nprograms = [\"curl\"]\nreason = \"use ureq\"\n",
    )]);
    assert_eq!(h.app_match, devkit_config::AppMatch::default());
    assert!(h.commands.contains_key("good"), "a bad app_match spares its siblings");
    assert_eq!(warns.len(), 1);
    assert!(warns[0].contains("app_match"), "warning names the table: {}", warns[0]);
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo nextest run -p devkit-common harness::tests::a_child_layer`
Expected: FAIL — `merge_rules` is not defined.

- [ ] **Step 3: Add the rule type to `devkit-config`**

In `crates/devkit-config/src/harness.rs`:

```rust
use std::collections::BTreeMap;

/// One `[harness.commands.<name>]` entry: a set of programs whose invocation
/// the guard refuses, and the correction it offers instead.
///
/// Deliberately not a regex. Splitting the command into segments, stripping
/// runner prefixes, and anchoring the command word are devkit's job; a rule
/// that had to restate them would get them wrong.
#[derive(Deserialize, Default, Debug, Clone, PartialEq, schemars::JsonSchema)]
pub struct CommandRule {
    /// Program names this rule refuses, matched against the segment's command
    /// word by basename. An empty list matches nothing, which is how a child
    /// layer exempts a subtree from a rule its parent declared.
    #[serde(default)]
    pub programs: Vec<String>,
    /// Arguments that must appear, in order, at the head of the typed
    /// arguments for the rule to fire. Empty matches any arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Shown to the agent verbatim when the rule denies. Name the replacement
    /// command; the agent retries from this text.
    #[serde(default)]
    pub reason: String,
}

/// `[harness.app_match]`: how the guard turns a hint into an app name.
///
/// Exact-name, exact-path and path-under-path matching are unconditional and
/// take no configuration. This table tunes only the fuzzy rung that runs when
/// none of those resolve, which is the one place the guard guesses.
///
/// `#[serde(default)]` sits on the container so a layer naming one key inherits
/// the other two rather than zeroing them.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq, schemars::JsonSchema)]
#[serde(default)]
pub struct AppMatch {
    /// Run the fuzzy matcher at all. `false` stops after exact and path
    /// matching, so an unrecognised hint names no app and the guard falls back
    /// to `devkit config apps`.
    pub fuzzy: bool,
    /// Substitutions, insertions and deletions the matcher forgives.
    ///
    /// One is what separates `lab-tools` from an app declared `lab_tools`.
    /// `frizbee::Config::default()` allows zero, which filters exactly that
    /// case, so this default is devkit's rather than the library's. Raising it
    /// buys confidently wrong app names.
    pub max_typos: u16,
    /// Below this score no app is named. Pointing an agent at another app's
    /// server is worse than naming none.
    pub min_score: u16,
}

impl Default for AppMatch {
    fn default() -> Self {
        Self {
            fuzzy: true,
            max_typos: 1,
            min_score: 60,
        }
    }
}

/// The `[harness]` table of a checkout's `devkit.toml`.
///
/// This is the shape `devkit schema` renders. Nothing at runtime deserializes
/// the table through it: the probe reads each key independently so one bad key
/// cannot take the others down with it.
#[derive(Deserialize, Default, Debug, Clone, PartialEq, schemars::JsonSchema)]
pub struct HarnessSection {
    /// Refuse writes to paths this checkout has not claimed with `lockm`.
    #[serde(default)]
    pub enforce_writes: bool,
    /// Refuse shell commands devkit already has a wired-up path for.
    #[serde(default)]
    pub enforce_commands: bool,
    /// Extra refusals beyond the ones devkit derives from `[apps]` and
    /// `[tasks]`. Merged across config layers like every other table.
    #[serde(default)]
    pub commands: BTreeMap<String, CommandRule>,
    /// How the guard resolves a guarded command to one of `[apps]`.
    #[serde(default)]
    pub app_match: AppMatch,
}
```

Re-export `CommandRule` and `AppMatch` from `lib.rs` beside `HarnessSection`.

- [ ] **Step 4: Add the merge and the resolvers to `devkit-common`**

```rust
use devkit_config::{AppMatch, CommandRule};
use std::collections::BTreeMap;

/// The merged `[harness]` tables the command guard reads. The two enforcement
/// flags are not here: they ratchet on with `any` across layers rather than
/// merging by precedence, and `enforcement_enabled` already owns that.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct HarnessRules {
    pub commands: BTreeMap<String, CommandRule>,
    pub app_match: AppMatch,
}

/// Merge the `[harness]` tables of parsed layers, lowest precedence first, and
/// deserialize each merged rule.
///
/// Routed through `devkit_config::merge_layers` rather than a hand-rolled
/// merge, so these inherit exactly the way every other config table does: a
/// child adds names, and a same-named rule overrides only the keys it sets.
/// Returns what survived alongside a warning per piece that did not.
pub fn merge_rules(layers: &[(PathBuf, toml::Table)]) -> (HarnessRules, Vec<String>) {
    let projected: Vec<_> = layers
        .iter()
        .map(|(p, t)| {
            let harness = t
                .get("harness")
                .and_then(toml::Value::as_table)
                .cloned()
                .unwrap_or_default();
            (p.clone(), harness)
        })
        .collect();
    let (merged, _, _) = devkit_config::merge_layers(&projected);
    let mut warnings = Vec::new();

    // Each key is deserialized on its own, so one that will not parse costs
    // only itself. A bad `app_match` degrades to the defaults rather than
    // failing the command: the guard is fail-open throughout.
    let app_match = match merged.get("app_match") {
        None => AppMatch::default(),
        Some(v) => v.clone().try_into::<AppMatch>().unwrap_or_else(|e| {
            warnings.push(format!("ignoring `[harness.app_match]`: {e}"));
            AppMatch::default()
        }),
    };

    let mut commands = BTreeMap::new();
    if let Some(table) = merged.get("commands").and_then(toml::Value::as_table) {
        for (name, value) in table {
            match value.clone().try_into::<CommandRule>() {
                Ok(rule) if rule.programs.is_empty() && !value_names_programs(value) => {
                    warnings.push(format!("skipping `[harness.commands.{name}]`: no `programs`"));
                }
                Ok(rule) => {
                    commands.insert(name.clone(), rule);
                }
                Err(e) => warnings.push(format!("skipping `[harness.commands.{name}]`: {e}")),
            }
        }
    }
    (HarnessRules { commands, app_match }, warnings)
}

/// Whether a merged rule table set `programs` at all. An explicit empty list is
/// a deliberate exemption and is kept; an absent key is an incomplete rule and
/// is skipped.
fn value_names_programs(v: &toml::Value) -> bool {
    v.as_table().is_some_and(|t| t.contains_key("programs"))
}
```

Then the two public entry points:

```rust
/// The merged `[harness]` command-guard tables applying at `cwd`, lowest
/// precedence first: the global config, then every project layer.
///
/// Warnings are returned rather than printed. This runs inside the shared gate,
/// which `lockm hook pretooluse` also calls, and a rule warning printed here
/// would fire on every `Edit` as well as every `Bash`.
pub fn resolve_rules(cwd: &Path) -> (HarnessRules, Vec<String>) {
    let mut layers: Vec<(PathBuf, toml::Table)> = Vec::new();
    if let Some(p) = global_config_path()
        && let Ok(body) = std::fs::read_to_string(&p)
        && let Ok(t) = toml::from_str::<toml::Table>(&body)
    {
        layers.push((p, t));
    }
    let main = crate::git::main_checkout(cwd).ok().flatten();
    if let Ok(project) = devkit_config::project_layers(cwd, main.as_deref()) {
        for layer in project {
            if let Ok(body) = std::fs::read_to_string(&layer.path)
                && let Ok(t) = toml::from_str::<toml::Table>(&body)
            {
                layers.push((layer.path, t));
            }
        }
    }
    merge_rules(&layers)
}

/// Whether the command guard is active for a command originating at `cwd`.
pub fn commands_enabled(cwd: &Path) -> bool {
    enforcement_enabled(cwd, "enforce_commands", "DEVKIT_ENFORCE_COMMANDS")
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p devkit-common harness`
Expected: PASS, all seven new tests.

- [ ] **Step 6: Regenerate the committed schema**

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test -p devkit config_schema`
Then: `cargo nextest run -p devkit config_schema`
Expected: PASS, with `schema/devkit-config.json` now describing `enforce_commands`, `commands` and `app_match`.

- [ ] **Step 7: Commit**

```bash
git add crates/devkit-config/src/harness.rs crates/devkit-config/src/lib.rs \
        crates/devkit-common/src/harness.rs schema/devkit-config.json
git commit -m "feat(harness): add [harness.commands] rules and [harness.app_match]"
```

---

### Task 4: The shell lexer

**Files:**
- Create: `crates/devkit-ports/src/guard/mod.rs`
- Create: `crates/devkit-ports/src/guard/lex.rs`
- Modify: `crates/devkit-ports/src/lib.rs` (add `pub mod guard;`)

**Interfaces:**
- Consumes: nothing.
- Produces: `devkit_ports::guard::lex::segments(command: &str) -> Vec<Vec<String>>` — one word vector per command position a shell would execute. Quoted text, redirection targets, and heredoc bodies never become segments.

- [ ] **Step 1: Write the failing tests**

Create `crates/devkit-ports/src/guard/lex.rs` with only its test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn heads(cmd: &str) -> Vec<String> {
        segments(cmd)
            .into_iter()
            .filter_map(|s| s.first().cloned())
            .collect()
    }

    #[test]
    fn a_plain_command_is_one_segment() {
        assert_eq!(segments("vite dev"), vec![vec!["vite", "dev"]]);
    }

    #[test]
    fn operators_start_a_new_segment() {
        assert_eq!(heads("foo && next dev"), vec!["foo", "next"]);
        assert_eq!(heads("cd x; uvicorn app"), vec!["cd", "uvicorn"]);
        assert_eq!(heads("a | b"), vec!["a", "b"]);
        assert_eq!(heads("(cd x && vite)"), vec!["cd", "vite"]);
        assert_eq!(heads("echo $(vite dev)"), vec!["echo", "vite"]);
    }

    #[test]
    fn a_quoted_operator_does_not_split() {
        assert_eq!(heads(r#"git commit -m "fix: crash under uvicorn; retry""#), vec!["git"]);
        assert_eq!(heads(r#"gh pr create --body "x && next dev""#), vec!["gh"]);
    }

    #[test]
    fn a_double_dash_does_not_split() {
        assert_eq!(heads("cargo run -- next dev"), vec!["cargo"]);
        assert_eq!(heads("rg -- vite"), vec!["rg"]);
    }

    #[test]
    fn a_redirection_ampersand_does_not_split() {
        assert_eq!(heads("bun run dev > /tmp/x.log 2>&1 &"), vec!["bun"]);
    }

    #[test]
    fn a_descriptor_duplication_leaves_no_stray_argument() {
        // `1` from `2>&1` must not survive as an argument to the command.
        assert_eq!(segments("vite dev 2>&1"), vec![vec!["vite", "dev"]]);
    }

    #[test]
    fn a_redirection_target_is_not_a_command_word() {
        assert_eq!(heads("cat notes.md > vite"), vec!["cat"]);
    }

    #[test]
    fn a_heredoc_body_is_inert() {
        let cmd = "cat > notes.md <<EOF\nnext dev\nuvicorn app\nEOF\nls";
        assert_eq!(heads(cmd), vec!["cat", "ls"]);
    }

    #[test]
    fn a_quoted_heredoc_delimiter_is_honoured() {
        let cmd = "cat <<'EOF'\nvite dev\nEOF";
        assert_eq!(heads(cmd), vec!["cat"]);
    }

    #[test]
    fn a_dash_heredoc_allows_an_indented_terminator() {
        let cmd = "cat <<-EOF\nvite dev\n\tEOF\nls";
        assert_eq!(heads(cmd), vec!["cat", "ls"]);
    }

    #[test]
    fn an_unterminated_quote_yields_no_segments() {
        assert!(segments("echo \"unterminated").is_empty());
    }
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo nextest run -p devkit-ports guard::lex`
Expected: FAIL to compile — `segments` is not defined.

- [ ] **Step 3: Implement the lexer**

Prepend to `crates/devkit-ports/src/guard/lex.rs`:

```rust
//! Split a shell command string into the word vectors a shell would execute.
//!
//! A word splitter is not enough here. `shell-words` and `shlex` strip quotes
//! and return a flat vector with no operator positions, so there is nothing
//! left to split segments on, and `2>&1` would have its `&` read as a
//! separator. This is a single pass that tracks quote state and emits words,
//! operators and redirections separately.

/// Characters that end a segment when unquoted.
const BREAKS: [char; 6] = ['|', '&', ';', '(', ')', '\n'];

#[derive(PartialEq)]
enum Tok {
    Word(String),
    Break,
    Redirect,
    /// `<<`, `<<-`, `<<'X'`: the next word names the terminator line.
    Heredoc,
}

/// Every command position in `command`, as word vectors. Quoted strings are one
/// opaque word, redirection targets are dropped, and heredoc bodies are skipped
/// entirely — an agent writing `cat > notes.md <<EOF` with "next dev" in the
/// body is writing a file, not launching a server.
///
/// An unterminated quote yields no segments: the string cannot be read as a
/// command, and guessing risks a denial on text nobody will execute.
pub fn segments(command: &str) -> Vec<Vec<String>> {
    let Some(toks) = tokenize(command) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut iter = toks.into_iter().peekable();
    while let Some(tok) = iter.next() {
        match tok {
            Tok::Break => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            Tok::Redirect | Tok::Heredoc => {
                iter.next();
            }
            Tok::Word(w) => current.push(w),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Scan into words, breaks and redirections. `None` on an unterminated quote.
fn tokenize(command: &str) -> Option<Vec<Tok>> {
    let mut toks = Vec::new();
    let mut word = String::new();
    let mut has_word = false;
    let mut chars = command.chars().peekable();
    let mut pending_heredocs: Vec<(String, bool)> = Vec::new();

    macro_rules! flush {
        () => {
            if has_word {
                toks.push(Tok::Word(std::mem::take(&mut word)));
                has_word = false;
            }
        };
    }

    while let Some(c) = chars.next() {
        if c == '\n' && !pending_heredocs.is_empty() {
            flush!();
            toks.push(Tok::Break);
            skip_heredoc_bodies(&mut chars, &mut pending_heredocs);
            continue;
        }
        match c {
            '\\' => {
                let escaped = chars.next()?;
                word.push(escaped);
                has_word = true;
            }
            '\'' => {
                has_word = true;
                loop {
                    let n = chars.next()?;
                    if n == '\'' {
                        break;
                    }
                    word.push(n);
                }
            }
            '"' => {
                has_word = true;
                loop {
                    let n = chars.next()?;
                    if n == '"' {
                        break;
                    }
                    if n == '\\' {
                        word.push(chars.next()?);
                        continue;
                    }
                    word.push(n);
                }
            }
            '$' if chars.peek() == Some(&'(') => {
                chars.next();
                flush!();
                toks.push(Tok::Break);
            }
            '<' => {
                flush!();
                if chars.peek() == Some(&'<') {
                    chars.next();
                    let dash = chars.peek() == Some(&'-');
                    if dash {
                        chars.next();
                    }
                    let delim = read_delimiter(&mut chars)?;
                    pending_heredocs.push((delim, dash));
                    toks.push(Tok::Heredoc);
                    toks.push(Tok::Word(String::new()));
                } else {
                    toks.push(Tok::Redirect);
                }
            }
            '>' => {
                // `2>`, `&>` and `>>` are all redirections; the digit or `&`
                // already sits in `word` and is not a command.
                word.clear();
                has_word = false;
                if chars.peek() == Some(&'>') {
                    chars.next();
                }
                // `2>&1` duplicates a descriptor. The `&` belongs to the
                // redirection, not to a job-control break, so consume the whole
                // target here rather than letting `&` split the segment.
                if chars.peek() == Some(&'&') {
                    chars.next();
                    while chars.peek().is_some_and(|c| c.is_ascii_digit() || *c == '-') {
                        chars.next();
                    }
                    continue;
                }
                toks.push(Tok::Redirect);
            }
            c if BREAKS.contains(&c) => {
                flush!();
                if (c == '|' || c == '&') && chars.peek() == Some(&c) {
                    chars.next();
                }
                toks.push(Tok::Break);
            }
            c if c.is_whitespace() => flush!(),
            c => {
                word.push(c);
                has_word = true;
            }
        }
    }
    flush!();
    let _ = line_start;
    Some(toks)
}

/// Read a heredoc terminator, honouring `<<'EOF'` and `<<"EOF"` quoting.
fn read_delimiter(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<String> {
    while chars.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
        chars.next();
    }
    let quote = match chars.peek() {
        Some('\'') => Some('\''),
        Some('"') => Some('"'),
        _ => None,
    };
    if quote.is_some() {
        chars.next();
    }
    let mut delim = String::new();
    while let Some(&c) = chars.peek() {
        match quote {
            Some(q) if c == q => {
                chars.next();
                break;
            }
            None if c.is_whitespace() || BREAKS.contains(&c) => break,
            _ => {
                delim.push(c);
                chars.next();
            }
        }
    }
    (!delim.is_empty()).then_some(delim)
}

/// Consume every queued heredoc body, up to and including its terminator line.
fn skip_heredoc_bodies(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    pending: &mut Vec<(String, bool)>,
) {
    for (delim, dash) in pending.drain(..) {
        loop {
            let mut line = String::new();
            let mut saw_any = false;
            for c in chars.by_ref() {
                saw_any = true;
                if c == '\n' {
                    break;
                }
                line.push(c);
            }
            let candidate = if dash { line.trim_start() } else { line.as_str() };
            if candidate == delim || !saw_any {
                break;
            }
        }
    }
}
```

Create `crates/devkit-ports/src/guard/mod.rs` with `pub mod lex;` and add `pub mod guard;` to `crates/devkit-ports/src/lib.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p devkit-ports guard::lex`
Expected: PASS, all eleven tests.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/guard/ crates/devkit-ports/src/lib.rs
git commit -m "feat(guard): lex a shell command into segments

Quoted operators, a bare --, redirections and heredoc bodies each
produced a false command word under the regex scan this replaces."
```

---

### Task 5: Segment normalization

**Files:**
- Create: `crates/devkit-ports/src/guard/norm.rs`
- Modify: `crates/devkit-ports/src/guard/mod.rs`

**Interfaces:**
- Consumes: Task 4's `segments`.
- Produces: `guard::norm::{Normalized, Doppler, normalize}`, where `normalize(words: &[String]) -> Option<Normalized>` and `Normalized { argv: Vec<String>, doppler: Option<Doppler> }`, `Doppler { config: Option<String>, project: Option<String> }`. Also `guard::norm::basename(prog: &str) -> &str`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn argv(cmd: &str) -> Vec<String> {
        let seg = crate::guard::lex::segments(cmd).remove(0);
        normalize(&seg).expect("a segment normalizes").argv
    }

    #[test]
    fn leading_assignments_are_stripped() {
        assert_eq!(argv("FOO=1 BAR=2 vite"), vec!["vite"]);
    }

    #[test]
    fn env_is_stripped_with_its_assignments() {
        assert_eq!(argv("env FOO=1 vite"), vec!["vite"]);
    }

    #[test]
    fn process_wrappers_are_stripped() {
        assert_eq!(argv("nohup bun run dev"), vec!["dev"]);
        assert_eq!(argv("timeout 30 vite"), vec!["vite"]);
        assert_eq!(argv("exec setsid vite dev"), vec!["vite", "dev"]);
    }

    #[test]
    fn runner_prefixes_are_stripped() {
        assert_eq!(argv("bunx vite"), vec!["vite"]);
        assert_eq!(argv("bun run vite dev"), vec!["vite", "dev"]);
        assert_eq!(argv("pnpm exec vite"), vec!["vite"]);
        assert_eq!(argv("uv run uvicorn app"), vec!["uvicorn", "app"]);
    }

    #[test]
    fn a_doppler_wrapper_is_stripped_and_recorded() {
        let seg = crate::guard::lex::segments("doppler run -c dev -- bun test").remove(0);
        let n = normalize(&seg).unwrap();
        assert_eq!(n.argv, vec!["bun", "test"]);
        assert_eq!(n.doppler.unwrap().config.as_deref(), Some("dev"));
    }

    #[test]
    fn doppler_config_spellings_normalize_together() {
        let short = crate::guard::lex::segments("doppler run -c dev -- x").remove(0);
        let long = crate::guard::lex::segments("doppler run --config dev -- x").remove(0);
        assert_eq!(
            normalize(&short).unwrap().doppler,
            normalize(&long).unwrap().doppler
        );
    }

    #[test]
    fn a_doppler_run_without_a_separator_is_left_alone() {
        assert_eq!(argv("doppler run -c dev"), vec!["doppler", "run", "-c", "dev"]);
    }

    #[test]
    fn the_command_word_compares_by_basename() {
        assert_eq!(basename("./node_modules/.bin/vite"), "vite");
        assert_eq!(basename("vite"), "vite");
    }

    #[test]
    fn a_segment_of_only_assignments_normalizes_to_nothing() {
        let seg = crate::guard::lex::segments("FOO=1").remove(0);
        assert!(normalize(&seg).is_none());
    }
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo nextest run -p devkit-ports guard::norm`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

```rust
//! Reduce a lexed segment to the argv a shell would actually exec, plus the
//! doppler wrapper that reduction removed.

/// Wrappers that exec their remaining argv unchanged.
const PROCESS_WRAPPERS: [&str; 4] = ["nohup", "setsid", "exec", "time"];

/// Runner prefixes, longest first so `bun run` is tried before `bun`.
const RUNNERS: [&[&str]; 6] = [
    &["bun", "run"],
    &["pnpm", "exec"],
    &["uv", "run"],
    &["bunx"],
    &["npx"],
    &["uvx"],
];

/// A doppler wrapper's identity, normalized so `-c dev` and `--config dev`
/// compare equal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Doppler {
    pub config: Option<String>,
    pub project: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Normalized {
    /// The program and its arguments, with every wrapper removed.
    pub argv: Vec<String>,
    /// The doppler wrapper that was removed, if any.
    pub doppler: Option<Doppler>,
}

/// The last path component of a program word. A launch through
/// `./node_modules/.bin/vite` is a `vite` launch; `run::assert_not_prd` already
/// matches `doppler` this way.
pub fn basename(prog: &str) -> &str {
    prog.rsplit(['/', '\\']).next().unwrap_or(prog)
}

/// Strip assignments and wrappers from a lexed segment. `None` when nothing
/// executable remains.
pub fn normalize(words: &[String]) -> Option<Normalized> {
    let mut argv: Vec<String> = words.to_vec();
    let mut doppler = None;

    loop {
        strip_assignments(&mut argv);
        let before = argv.len();
        strip_process_wrappers(&mut argv);
        if let Some(d) = strip_doppler(&mut argv) {
            doppler = Some(d);
        }
        strip_runner(&mut argv);
        if argv.len() == before {
            break;
        }
    }
    (!argv.is_empty()).then_some(Normalized { argv, doppler })
}

fn strip_assignments(argv: &mut Vec<String>) {
    if argv.first().is_some_and(|w| basename(w) == "env") {
        argv.remove(0);
    }
    while argv
        .first()
        .is_some_and(|w| w.contains('=') && !w.starts_with('=') && !w.starts_with('-'))
    {
        argv.remove(0);
    }
}

fn strip_process_wrappers(argv: &mut Vec<String>) {
    while let Some(first) = argv.first().map(|w| basename(w).to_string()) {
        if PROCESS_WRAPPERS.contains(&first.as_str()) {
            argv.remove(0);
        } else if first == "timeout" && argv.len() > 1 {
            argv.drain(..2);
        } else {
            break;
        }
    }
}

fn strip_runner(argv: &mut Vec<String>) {
    for runner in RUNNERS {
        let matches = argv.len() > runner.len()
            && argv
                .iter()
                .zip(runner)
                .enumerate()
                .all(|(i, (w, r))| if i == 0 { basename(w) == *r } else { w == r });
        if matches {
            argv.drain(..runner.len());
            return;
        }
    }
}

/// Remove a `doppler run … --` wrapper and report its `(config, project)`. A
/// `doppler run` with no `--` separator is left in place: it names no inner
/// command, so there is nothing to unwrap.
fn strip_doppler(argv: &mut Vec<String>) -> Option<Doppler> {
    if argv.len() < 2 || basename(&argv[0]) != "doppler" || argv[1] != "run" {
        return None;
    }
    let sep = argv.iter().position(|w| w == "--")?;
    let mut d = Doppler::default();
    let mut i = 2;
    while i < sep {
        let (flag, value) = (argv[i].as_str(), argv.get(i + 1).cloned());
        match flag {
            "-c" | "--config" => d.config = value,
            "-p" | "--project" => d.project = value,
            _ => {}
        }
        i += if flag.starts_with('-') { 2 } else { 1 };
    }
    argv.drain(..=sep);
    Some(d)
}
```

Add `pub mod norm;` to `crates/devkit-ports/src/guard/mod.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p devkit-ports guard::norm`
Expected: PASS, all nine tests.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/guard/
git commit -m "feat(guard): normalize a segment to its real argv"
```

---

### Task 6: Signature reduction and matching

**Files:**
- Create: `crates/devkit-ports/src/guard/sig.rs`
- Modify: `crates/devkit-ports/src/guard/mod.rs`

**Interfaces:**
- Consumes: Task 5's `basename`.
- Produces: `guard::sig::{signature, matches}`, where `signature(config_argv: &[String]) -> Option<Vec<String>>` and `matches(sig: &[String], typed: &[String]) -> bool`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn v(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    fn sig(words: &[&str]) -> Option<Vec<String>> {
        signature(&v(words))
    }

    #[test]
    fn a_trailing_template_is_dropped() {
        assert_eq!(sig(&["nitro", "dev", "--port", "{{ port }}"]), Some(v(&["nitro", "dev"])));
    }

    #[test]
    fn truncation_stops_at_the_first_flag() {
        assert_eq!(sig(&["uvicorn", "app:app", "--reload"]), Some(v(&["uvicorn", "app:app"])));
    }

    #[test]
    fn a_bare_positional_after_the_cut_rejects_the_signature() {
        // `["docker", "compose"]` is a prefix of `docker compose down`, so
        // matching on it would deny every sibling verb.
        assert_eq!(sig(&["docker", "compose", "-p", "{{ p }}", "up"]), None);
    }

    #[test]
    fn a_generic_interpreter_alone_rejects_the_signature() {
        assert_eq!(sig(&["python", "-m", "{{ module }}"]), None);
        assert_eq!(sig(&["node", "--enable-source-maps", "{{ entry }}"]), None);
    }

    #[test]
    fn a_one_token_signature_survives_when_the_token_is_specific() {
        assert_eq!(sig(&["dev"]), Some(v(&["dev"])));
        // An app's `bun run dev -- --port {{ port }}`, after runner stripping.
        assert_eq!(sig(&["dev", "--", "--port", "{{ port }}"]), Some(v(&["dev"])));
    }

    #[test]
    fn a_catalog_program_still_reduces_and_is_ranked_later() {
        // The catalog, not this signature, decides whether `vite build` is a
        // server. Reduction only has to avoid panicking on it.
        assert_eq!(sig(&["vite", "--port", "{{ port }}"]), Some(v(&["vite"])));
    }

    #[test]
    fn a_port_lookup_counts_as_a_template() {
        assert_eq!(sig(&["curl", "ports['api']"]), None);
    }

    #[test]
    fn a_typed_command_matches_a_prefix_signature() {
        assert!(matches(&v(&["nitro", "dev"]), &v(&["nitro", "dev"])));
        assert!(matches(&v(&["nitro", "dev"]), &v(&["nitro", "dev", "--port", "3000"])));
        assert!(!matches(&v(&["nitro", "dev"]), &v(&["nitro", "build"])));
        assert!(!matches(&v(&["nitro", "dev"]), &v(&["nitro"])));
    }

    #[test]
    fn the_command_word_matches_by_basename() {
        assert!(matches(&v(&["vite"]), &v(&["./node_modules/.bin/vite"])));
    }
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo nextest run -p devkit-ports guard::sig`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

```rust
//! Reduce a config argv to the fixed prefix a human would retype, and test a
//! typed command against it.

use super::norm::basename;

/// Programs whose every invocation looks alike from the outside. A one-token
/// signature naming one of these would match anything they run.
const GENERIC: [&str; 11] = [
    "python", "python3", "node", "bun", "deno", "docker", "cargo", "go", "uv", "sh", "bash",
];

/// The part of a config argv that a typed command can be expected to reproduce:
/// the command word plus its leading positionals.
///
/// Truncation stops at the first minijinja template *or* the first flag,
/// whichever comes first. Stopping at the template alone assumes it sits last,
/// and it usually does not.
///
/// Two rejections keep that from over-firing. A bare positional surviving after
/// the cut means the launch carries a verb the signature does not, so matching
/// the prefix would deny every sibling verb: `["docker", "compose"]` out of
/// `docker compose -p x up` would deny `docker compose down`. And a lone
/// generic interpreter matches everything it runs, so `["python"]` out of
/// `python -m {{ module }}` would deny `python -m pytest`.
pub fn signature(config_argv: &[String]) -> Option<Vec<String>> {
    let cut = config_argv
        .iter()
        .position(|w| is_template(w) || w.starts_with('-'))
        .unwrap_or(config_argv.len());
    let sig = &config_argv[..cut];
    if sig.is_empty() {
        return None;
    }
    let bare_after = config_argv[cut..]
        .iter()
        .any(|w| !w.starts_with('-') && !is_template(w));
    if bare_after {
        return None;
    }
    if sig.len() == 1 && GENERIC.contains(&basename(&sig[0])) {
        return None;
    }
    Some(sig.to_vec())
}

/// Whether a word carries minijinja that renders to something the typed command
/// cannot be expected to reproduce.
fn is_template(word: &str) -> bool {
    word.contains("{{") || word.contains("{%") || word.contains("ports[")
}

/// Whether `typed` starts with `sig`. The command word compares by basename;
/// every later word compares exactly.
pub fn matches(sig: &[String], typed: &[String]) -> bool {
    if sig.is_empty() || typed.len() < sig.len() {
        return false;
    }
    basename(&typed[0]) == basename(&sig[0]) && sig[1..] == typed[1..sig.len()]
}
```

Add `pub mod sig;` to `crates/devkit-ports/src/guard/mod.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p devkit-ports guard::sig`
Expected: PASS, all seven tests.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/guard/
git commit -m "feat(guard): reduce a config argv to a match signature"
```

---

### Task 7: The built-in dev-server catalog

**Files:**
- Create: `crates/devkit-ports/src/guard/catalog.rs`
- Modify: `crates/devkit-ports/src/guard/mod.rs`

**Interfaces:**
- Consumes: Task 5's `basename`.
- Produces: `guard::catalog::{is_known_program, is_dev_server}`, where both take `argv: &[String]` / `prog: &str` and return `bool`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn server(cmd: &str) -> bool {
        let argv: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
        is_dev_server(&argv)
    }

    #[test]
    fn dev_subcommand_frameworks() {
        assert!(server("next dev"));
        assert!(server("nitro dev"));
        assert!(server("wrangler dev"));
        assert!(server("mintlify dev"));
        assert!(!server("next build"));
        assert!(!server("next"));
    }

    #[test]
    fn uvicorn_is_a_server_bare() {
        assert!(server("uvicorn app:app"));
        assert!(server("uvicorn"));
    }

    #[test]
    fn flask_needs_its_run_verb_anywhere() {
        assert!(server("flask run"));
        assert!(server("flask --app x run"));
        assert!(!server("flask shell"));
    }

    #[test]
    fn vite_serves_on_three_verbs_and_no_others() {
        assert!(server("vite"));
        assert!(server("vite dev"));
        assert!(server("vite serve"));
        assert!(!server("vite build"));
        assert!(!server("vite preview"));
        assert!(!server("vite optimize"));
        assert!(!server("vite --version"));
        assert!(!server("vite -h"));
        // A non-info flag leaves the verdict to the verb.
        assert!(server("vite --port 3000"));
        assert!(!server("vite build --minify"));
    }

    #[test]
    fn a_catalog_program_is_recognised_by_basename() {
        assert!(is_known_program("./node_modules/.bin/vite"));
        assert!(!is_known_program("cargo"));
    }
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo nextest run -p devkit-ports guard::catalog`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

```rust
//! Ecosystem knowledge: which invocations of which programs start a long-lived
//! dev server. Extended by pull request, never by config — a project adding a
//! refusal of its own writes `[harness.commands.<name>]`.

use super::norm::basename;

/// Frameworks whose server is the `dev` subcommand and nothing else.
const DEV_SUBCOMMAND: [&str; 4] = ["next", "nitro", "wrangler", "mintlify"];

/// Vite verbs that do not start a server. Everything else, including no verb
/// at all, does.
const VITE_NON_SERVER: [&str; 3] = ["build", "preview", "optimize"];

/// Flags that make a verbless invocation print and exit rather than serve.
/// Any other flag leaves the verdict to the verb, so `vite --port 3000` is a
/// server.
const INFO_FLAGS: [&str; 4] = ["--version", "-v", "--help", "-h"];

/// Whether the guard has an opinion about this program at all.
pub fn is_known_program(prog: &str) -> bool {
    let p = basename(prog);
    DEV_SUBCOMMAND.contains(&p) || matches!(p, "uvicorn" | "flask" | "vite")
}

/// Whether this argv starts a dev server.
pub fn is_dev_server(argv: &[String]) -> bool {
    let Some(prog) = argv.first().map(|p| basename(p)) else {
        return false;
    };
    let rest: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    let first_verb = rest.iter().find(|a| !a.starts_with('-'));

    if DEV_SUBCOMMAND.contains(&prog) {
        return first_verb == Some(&"dev");
    }
    match prog {
        "uvicorn" => true,
        "flask" => rest.contains(&"run"),
        "vite" => {
            if rest.iter().any(|a| INFO_FLAGS.contains(a)) {
                return false;
            }
            match first_verb {
                None => true,
                Some(v) => !VITE_NON_SERVER.contains(v),
            }
        }
        _ => false,
    }
}
```

Add `pub mod catalog;` to `crates/devkit-ports/src/guard/mod.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p devkit-ports guard::catalog`
Expected: PASS, all five tests.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/guard/
git commit -m "feat(guard): add the built-in dev-server catalog"
```

---

### Task 8: The task-deny predicate and the `guard` field

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (add `guard` to `TaskConfig`)
- Create: `crates/devkit-ports/src/guard/tasks.rs`
- Modify: `crates/devkit-ports/src/guard/mod.rs`

**Interfaces:**
- Consumes: Task 5's `Doppler`, `devkit_common::template::referenced_ports`.
- Produces: `TaskConfig.guard: Option<bool>`; `guard::tasks::redirect_worth_it(task: &TaskConfig, vars: &BTreeMap<String, String>, typed: Option<&Doppler>, config: Option<&Doppler>) -> bool`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use devkit_config::TaskConfig;
    use std::collections::BTreeMap;

    fn task(run: &[&str]) -> TaskConfig {
        TaskConfig {
            run: run.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn worth(t: &TaskConfig) -> bool {
        redirect_worth_it(t, &BTreeMap::new(), None, None)
    }

    #[test]
    fn a_bare_run_is_not_worth_redirecting() {
        assert!(!worth(&task(&["bun", "test"])));
    }

    #[test]
    fn an_app_scoped_task_is_worth_redirecting() {
        let mut t = task(&["bun", "test"]);
        t.app = Some("web".into());
        assert!(worth(&t));
    }

    #[test]
    fn a_task_with_env_is_worth_redirecting() {
        let mut t = task(&["bun", "test"]);
        t.env.insert("CI".into(), "1".into());
        assert!(worth(&t));
    }

    #[test]
    fn a_port_template_is_worth_redirecting() {
        assert!(worth(&task(&["curl", "http://localhost:{{ port }}"])));
        assert!(worth(&task(&["curl", "{{ ports['api'] }}"])));
    }

    #[test]
    fn a_differing_doppler_config_is_worth_redirecting() {
        let typed = Doppler { config: Some("prd".into()), project: None };
        let cfg = Doppler { config: Some("dev".into()), project: None };
        assert!(redirect_worth_it(&task(&["bun", "test"]), &BTreeMap::new(), Some(&typed), Some(&cfg)));
    }

    #[test]
    fn an_identical_doppler_config_is_not() {
        let d = Doppler { config: Some("dev".into()), project: None };
        assert!(!redirect_worth_it(&task(&["bun", "test"]), &BTreeMap::new(), Some(&d), Some(&d)));
    }

    #[test]
    fn a_missing_wrapper_on_one_side_is_a_difference() {
        let d = Doppler { config: Some("dev".into()), project: None };
        assert!(redirect_worth_it(&task(&["bun", "test"]), &BTreeMap::new(), None, Some(&d)));
    }

    #[test]
    fn the_guard_field_overrides_both_ways() {
        let mut bare = task(&["bun", "test"]);
        bare.guard = Some(true);
        assert!(worth(&bare));

        let mut scoped = task(&["bun", "test"]);
        scoped.app = Some("web".into());
        scoped.guard = Some(false);
        assert!(!worth(&scoped));
    }
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo nextest run -p devkit-ports guard::tasks`
Expected: FAIL to compile — `guard` is not a field on `TaskConfig`.

- [ ] **Step 3: Add the `guard` field**

In `crates/devkit-config/src/lib.rs`, append to `TaskConfig`:

```rust
    /// Force the command guard's decision for this task instead of deriving it.
    /// `true` always redirects a matching command to `devrun task <name>`;
    /// `false` never does. Absent derives from whether devkit changes the
    /// process at all.
    #[serde(default)]
    pub guard: Option<bool>,
```

- [ ] **Step 4: Implement the predicate**

```rust
//! Whether redirecting a typed command to `devrun task <name>` changes anything.

use super::norm::Doppler;
use devkit_config::TaskConfig;
use std::collections::BTreeMap;

/// Whether `devrun task <name>` launches a different process than the argv the
/// agent typed.
///
/// True when the task sets `app` (a different cwd plus that app's `static_env`),
/// sets `env`, references a port the registry has to supply, or carries a
/// different doppler wrapper than the typed command. A task with none of those
/// resolves to the identical process in the same directory, and blocking it
/// buys nothing.
///
/// `steps` is deliberately not among them: `run` and `steps` are mutually
/// exclusive, so a sequence task has no `run` for a typed command to match.
/// Nor is `require_live`, which `resolve_command` already requires to be
/// referenced through `ports[...]`, so the port test has fired first.
pub fn redirect_worth_it(
    task: &TaskConfig,
    vars: &BTreeMap<String, String>,
    typed: Option<&Doppler>,
    config: Option<&Doppler>,
) -> bool {
    if let Some(forced) = task.guard {
        return forced;
    }
    task.app.is_some()
        || !task.env.is_empty()
        || typed != config
        || references_a_port(task, vars)
}

/// Whether the task's `run` or `env` reads a port. Rendered against a recording
/// context rather than scanned for `{{`, so `ports["web"]` and a
/// `{% if port %}` guard both count and neither touches the registry.
fn references_a_port(task: &TaskConfig, vars: &BTreeMap<String, String>) -> bool {
    let mut templates: Vec<&str> = task.run.iter().map(String::as_str).collect();
    templates.extend(task.env.values().map(String::as_str));
    devkit_common::template::referenced_ports(&templates, vars)
        .map(|r| r.own_port || !r.apps.is_empty())
        .unwrap_or(false)
}
```

Add `pub mod tasks;` to `crates/devkit-ports/src/guard/mod.rs`.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p devkit-ports guard::tasks`
Expected: PASS, all eight tests.

- [ ] **Step 6: Regenerate the schema and run the gate**

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test -p devkit config_schema`
Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/devkit-config/src/lib.rs crates/devkit-ports/src/guard/ schema/devkit-config.json
git commit -m "feat(guard): derive whether a task redirect is worth it"
```

---

### Task 9: App-name resolution

**Files:**
- Modify: `Cargo.toml` (workspace dependency on `frizbee`)
- Modify: `crates/devkit-ports/Cargo.toml`
- Create: `crates/devkit-ports/src/guard/appname.rs`
- Modify: `crates/devkit-ports/src/guard/mod.rs`

**Interfaces:**
- Consumes: `devkit_ports::apps::App`, Task 3's `devkit_config::AppMatch`.
- Produces: `guard::appname::{hint, resolve}`, where `hint(argv: &[String], cwd_rel: Option<&str>) -> Option<String>` and `resolve(hint: Option<&str>, candidates: &[&App], cfg: &AppMatch) -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::App;
    use devkit_config::AppMatch;

    fn app(name: &str, path: &str) -> App {
        App {
            name: name.into(),
            base_port: 3000,
            path: path.into(),
            launch: Vec::new(),
            url: None,
            url_env: None,
            provides_url: false,
            static_env: Default::default(),
            prep_files: Vec::new(),
            setup: Vec::new(),
        }
    }

    fn words(cmd: &str) -> Vec<String> {
        cmd.split_whitespace().map(str::to_string).collect()
    }

    /// Two apps, so `resolve` cannot short-circuit on a lone candidate.
    fn pair() -> [App; 2] {
        [app("lab_tools", "apps/lab_tools"), app("web", "apps/web")]
    }

    #[test]
    fn a_workspace_path_in_the_command_is_the_hint() {
        assert_eq!(hint(&words("vite --root apps/web"), None).as_deref(), Some("web"));
        assert_eq!(hint(&words("vite packages/ui/src"), None).as_deref(), Some("ui"));
    }

    #[test]
    fn a_filter_flag_is_the_hint_when_no_path_is_present() {
        assert_eq!(hint(&words("bun run --filter admin dev"), None).as_deref(), Some("admin"));
        assert_eq!(hint(&words("vite -C tools"), None).as_deref(), Some("tools"));
    }

    #[test]
    fn the_cwd_is_the_last_resort() {
        assert_eq!(hint(&words("vite"), Some("apps/web")).as_deref(), Some("apps/web"));
        assert_eq!(hint(&words("vite"), None), None);
    }

    #[test]
    fn an_exact_name_wins() {
        let apps = [app("web", "apps/web"), app("website", "apps/website")];
        let refs: Vec<&App> = apps.iter().collect();
        assert_eq!(resolve(Some("web"), &refs, &AppMatch::default()).as_deref(), Some("web"));
    }

    #[test]
    fn a_path_hint_resolves_to_the_owning_app() {
        let apps = [app("web", "apps/web"), app("admin", "apps/admin")];
        let refs: Vec<&App> = apps.iter().collect();
        assert_eq!(resolve(Some("apps/web"), &refs, &AppMatch::default()).as_deref(), Some("web"));
    }

    #[test]
    fn a_cwd_inside_an_app_resolves_to_it() {
        let apps = [app("web", "apps/web"), app("admin", "apps/admin")];
        let refs: Vec<&App> = apps.iter().collect();
        assert_eq!(
            resolve(Some("apps/web/src/routes"), &refs, &AppMatch::default()).as_deref(),
            Some("web")
        );
    }

    #[test]
    fn a_single_candidate_is_named_without_a_hint() {
        let apps = [app("web", "apps/web")];
        let refs: Vec<&App> = apps.iter().collect();
        assert_eq!(resolve(None, &refs, &AppMatch::default()).as_deref(), Some("web"));
    }

    #[test]
    fn a_near_miss_is_rescued_by_fuzzy_matching() {
        let apps = pair();
        let refs: Vec<&App> = apps.iter().collect();
        assert_eq!(
            resolve(Some("lab-tools"), &refs, &AppMatch::default()).as_deref(),
            Some("lab_tools")
        );
    }

    #[test]
    fn an_unrelated_hint_names_no_app() {
        let apps = [app("web", "apps/web"), app("admin", "apps/admin")];
        let refs: Vec<&App> = apps.iter().collect();
        assert_eq!(resolve(Some("zzzzzzzz"), &refs, &AppMatch::default()), None);
        assert_eq!(resolve(None, &refs, &AppMatch::default()), None);
    }

    #[test]
    fn fuzzy_false_stops_after_exact_and_path_matching() {
        let apps = pair();
        let refs: Vec<&App> = apps.iter().collect();
        let strict = AppMatch { fuzzy: false, ..Default::default() };
        assert_eq!(resolve(Some("lab-tools"), &refs, &strict), None);
        assert_eq!(resolve(Some("apps/web"), &refs, &strict).as_deref(), Some("web"));
    }

    #[test]
    fn a_raised_min_score_rejects_what_the_default_accepts() {
        let apps = pair();
        let refs: Vec<&App> = apps.iter().collect();
        let picky = AppMatch { min_score: u16::MAX, ..Default::default() };
        assert_eq!(resolve(Some("lab-tools"), &refs, &picky), None);
    }

    /// Pins the reason devkit does not inherit `frizbee::Config::default()`:
    /// a substitution is one typo, and a zero budget filters it.
    #[test]
    fn a_zero_typo_budget_filters_the_case_the_default_rescues() {
        let apps = pair();
        let refs: Vec<&App> = apps.iter().collect();
        let zero = AppMatch { max_typos: 0, ..Default::default() };
        assert_eq!(resolve(Some("lab-tools"), &refs, &zero), None);
    }
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo nextest run -p devkit-ports guard::appname`
Expected: FAIL to compile.

- [ ] **Step 3: Add the dependency**

In the root `Cargo.toml` `[workspace.dependencies]`, add `frizbee = "0.13"`. In `crates/devkit-ports/Cargo.toml` `[dependencies]`, add `frizbee.workspace = true`.

Nothing here reads a constant of its own: the thresholds are `devkit_config::AppMatch`, defaulted in Task 3 and overridable through `[harness.app_match]`.

Verified against the v0.13.0 source (commit `d608923`), so build to that API and not to recollection:

- There is no free `frizbee::match_list` and no `Options` type. The entry point is `Matcher::new(pattern: impl Into<Pattern>, config: &Config)` (`src/matcher/mod.rs:118`) then `matcher.match_list<S: AsRef<str>>(&[S]) -> Vec<Match>` (`:247`).
- `Match`'s fields are public: `score: u16`, `index: u32`, `exact: bool` (`src/lib.rs:154-165`). The index field is `index`, not `index_in_haystack`.
- `match_list` already returns descending score (`SortStrategy::ScoreThenIndexAsc` is the default, `src/lib.rs:326-331`), so do not sort it again.
- **`Config::default()` sets `max_typos: Some(0)`** (`src/lib.rs:276`). `lab-tools` against `lab_tools` is one substitution, which is one typo, so the default configuration filters out the exact case this code exists for. It must allow at least one.
- Portability is fine: `src/smith_waterman/backend/mod.rs:23-34` gates NEON on `aarch64` and a scalar backend on everything else, alongside SSE/AVX/AVX512 for x86_64. The crate's only dependency is an optional serde. MSRV is 1.89.0 and the edition is 2024.

- [ ] **Step 4: Implement**

```rust
//! Work out which app a dev-server command belongs to.
//!
//! The catalog knows a command starts a server without knowing whose it is, and
//! several apps can share one `launch`. Both problems resolve the same way.

use crate::apps::App;
use devkit_config::AppMatch;

/// Flags whose value names a workspace member.
const DIR_FLAGS: [&str; 5] = ["--filter", "-F", "--dir", "-C", "--cwd"];

/// The best guess at which app a segment refers to, before it is matched
/// against the catalog: a workspace path in the command, else a directory
/// flag's value, else the hook's cwd relative to the project root.
pub fn hint(argv: &[String], cwd_rel: Option<&str>) -> Option<String> {
    if let Some(name) = argv.iter().find_map(|w| workspace_member(w)) {
        return Some(name);
    }
    let mut it = argv.iter();
    while let Some(w) = it.next() {
        if let Some((flag, inline)) = w.split_once('=')
            && DIR_FLAGS.contains(&flag)
        {
            return Some(inline.to_string());
        }
        if DIR_FLAGS.contains(&w.as_str())
            && let Some(v) = it.next()
        {
            return Some(v.clone());
        }
    }
    cwd_rel.map(str::to_string)
}

/// The member name in an `apps/<name>` or `packages/<name>` path.
fn workspace_member(word: &str) -> Option<String> {
    let norm = word.replace('\\', "/");
    let mut parts = norm.split('/').peekable();
    while let Some(p) = parts.next() {
        if (p == "apps" || p == "packages")
            && let Some(name) = parts.peek()
            && !name.is_empty()
        {
            return Some((*name).to_string());
        }
    }
    None
}

/// Which of `candidates` the hint names.
///
/// A single candidate needs no hint: the caller already narrowed to it, and
/// asking a fuzzy matcher to confirm a set of one only adds a way to fail.
/// Otherwise an exact name or path wins, then a hint that is a path *under* an
/// app's path (a cwd of `apps/web/src` names `web`), then frizbee rescues a
/// near-miss such as `lab-tools` against an app declared `lab_tools`.
///
/// The fuzzy rung is the only guesswork here, and `cfg` is what a project tunes
/// it with. `frizbee::Config::default()` allows zero typos, which filters the
/// `lab-tools` case outright, so `cfg.max_typos` is always passed rather than
/// left to the library.
pub fn resolve(hint: Option<&str>, candidates: &[&App], cfg: &AppMatch) -> Option<String> {
    match candidates {
        [] => return None,
        [only] => return Some(only.name.clone()),
        _ => {}
    }
    let needle = hint?.trim_matches('/');

    for app in candidates {
        if app.name == needle || app.path.trim_matches('/') == needle {
            return Some(app.name.clone());
        }
    }
    for app in candidates {
        let path = app.path.trim_matches('/');
        if !path.is_empty() && needle.starts_with(&format!("{path}/")) {
            return Some(app.name.clone());
        }
    }

    if !cfg.fuzzy {
        return None;
    }
    let haystack: Vec<&str> = candidates.iter().map(|a| a.name.as_str()).collect();
    let config = frizbee::Config::default().max_typos(Some(cfg.max_typos));
    let mut matcher = frizbee::Matcher::new(needle, &config);
    // `match_list` already returns descending score.
    matcher
        .match_list(&haystack)
        .first()
        .filter(|m| m.score >= cfg.min_score)
        .map(|m| haystack[m.index as usize].to_string())
}
```

Add `pub mod appname;` to `crates/devkit-ports/src/guard/mod.rs`.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p devkit-ports guard::appname`
Expected: PASS, all twelve tests.

`AppMatch::default().min_score` is calibrated arithmetic, not a measurement, and Task 3 is where it lives. Calibrated against frizbee's scoring constants (`src/const.rs`): 12 per matched character, a 12-point prefix bonus, 4 per character matching case, 6 for a substitution. If `a_near_miss_is_rescued_by_fuzzy_matching` fails, print the score frizbee actually returns for `lab-tools` against `lab_tools` and set the default below it; if `an_unrelated_hint_names_no_app` fails, print the score for `zzzzzzzz` and set it above. Both cases are in the tests, so the calibration is one run, not guesswork. Do not widen the `max_typos` default to fix a threshold problem: more typo tolerance buys wrong answers, which is the failure this code is built to avoid. A project that needs a wider budget sets one in its own `[harness.app_match]`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/devkit-ports/Cargo.toml crates/devkit-ports/src/guard/
git commit -m "feat(guard): resolve a command to the app it belongs to"
```

---

### Task 10: The decision engine

Wires sources 1 through 5 together. This is the first task that reads the resolved config.

**Files:**
- Modify: `crates/devkit-ports/src/guard/mod.rs`
- Test: `crates/devkit-ports/src/guard/mod.rs` (unit, over an injected config)

**Interfaces:**
- Consumes: every module from Tasks 4 through 9, `devkit_config::{Config, AppMatch}`, `crate::apps::App`.
- Produces: `guard::{Decision, decide_with}`, where `Decision` is `Allow` or `Deny { reason: String }` and `decide_with(command: &str, rules: &BTreeMap<String, CommandRule>, project: Option<&Project>) -> Decision`. `Project { config: Config, catalog: HashMap<String, App>, cwd_rel: Option<String>, app_match: AppMatch }` is everything the guard needs once it has decided to look past the rules, absent when only rules were loaded.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use devkit_config::{AppConfig, AppMatch, CommandRule, TaskConfig};

    fn rules(programs: &[&str], reason: &str) -> BTreeMap<String, CommandRule> {
        BTreeMap::from([(
            "test-rule".to_string(),
            CommandRule {
                programs: programs.iter().map(|s| s.to_string()).collect(),
                args: Vec::new(),
                reason: reason.into(),
            },
        )])
    }

    fn project(build: impl FnOnce(&mut Config)) -> Project {
        let mut config = Config::default();
        build(&mut config);
        let catalog = config
            .apps
            .iter()
            .map(|(name, a)| {
                (
                    name.clone(),
                    App {
                        name: name.clone(),
                        base_port: a.base_port,
                        path: a.path.clone().unwrap_or_else(|| format!("apps/{name}")),
                        launch: a.launch.clone(),
                        url: None,
                        url_env: None,
                        provides_url: false,
                        static_env: Default::default(),
                        prep_files: Vec::new(),
                        setup: Vec::new(),
                    },
                )
            })
            .collect();
        Project { config, catalog, cwd_rel: None, app_match: AppMatch::default() }
    }

    fn denies(d: &Decision) -> bool {
        matches!(d, Decision::Deny { .. })
    }

    fn reason(d: &Decision) -> &str {
        match d {
            Decision::Deny { reason } => reason,
            Decision::Allow => panic!("expected a denial"),
        }
    }

    #[test]
    fn a_devkit_shim_is_never_gated() {
        let r = rules(&["node"], "use bun");
        assert!(!denies(&decide_with("devrun up web", &r, None)));
        assert!(!denies(&decide_with("devkit config apps", &r, None)));
    }

    #[test]
    fn a_user_rule_denies_with_its_reason() {
        let r = rules(&["node"], "This workspace is bun-only.");
        let d = decide_with("node server.js", &r, None);
        assert_eq!(reason(&d), "This workspace is bun-only.");
    }

    #[test]
    fn a_user_rule_with_args_needs_them() {
        let mut r = rules(&["docker-compose"], "use docker compose");
        r.get_mut("test-rule").unwrap().args = vec!["up".into()];
        assert!(denies(&decide_with("docker-compose up -d", &r, None)));
        assert!(!denies(&decide_with("docker-compose logs", &r, None)));
    }

    #[test]
    fn an_empty_programs_rule_matches_nothing() {
        let r = rules(&[], "unreachable");
        assert!(!denies(&decide_with("node server.js", &r, None)));
    }

    #[test]
    fn a_task_redirect_names_the_task() {
        let p = project(|c| {
            c.tasks.insert(
                "check".into(),
                TaskConfig {
                    run: vec!["bun".into(), "test".into()],
                    app: Some("web".into()),
                    ..Default::default()
                },
            );
        });
        let d = decide_with("bun test", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devrun task check"), "{}", reason(&d));
    }

    #[test]
    fn a_task_with_its_own_runner_prefix_still_matches() {
        let p = project(|c| {
            c.tasks.insert(
                "lint".into(),
                TaskConfig {
                    run: vec!["bun".into(), "run".into(), "lint".into()],
                    app: Some("web".into()),
                    ..Default::default()
                },
            );
        });
        let d = decide_with("bun run lint", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devrun task lint"), "{}", reason(&d));
    }

    #[test]
    fn a_differing_doppler_config_denies_and_a_matching_one_does_not() {
        let p = project(|c| {
            c.tasks.insert(
                "check".into(),
                TaskConfig {
                    run: ["doppler", "run", "-c", "dev", "--", "bun", "test"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    ..Default::default()
                },
            );
        });
        let denied = decide_with("doppler run -c prd -- bun test", &BTreeMap::new(), Some(&p));
        assert!(reason(&denied).contains("devrun task check"), "{}", reason(&denied));
        assert!(!denies(&decide_with(
            "doppler run -c dev -- bun test",
            &BTreeMap::new(),
            Some(&p)
        )));
    }

    #[test]
    fn an_identical_task_is_allowed_through() {
        let p = project(|c| {
            c.tasks.insert(
                "check".into(),
                TaskConfig { run: vec!["bun".into(), "test".into()], ..Default::default() },
            );
        });
        assert!(!denies(&decide_with("bun test", &BTreeMap::new(), Some(&p))));
    }

    #[test]
    fn the_longest_matching_task_wins() {
        let p = project(|c| {
            for (name, run) in [
                ("short", vec!["bun", "test"]),
                ("long", vec!["bun", "test", "unit"]),
            ] {
                c.tasks.insert(
                    name.into(),
                    TaskConfig {
                        run: run.iter().map(|s| s.to_string()).collect(),
                        app: Some("web".into()),
                        ..Default::default()
                    },
                );
            }
        });
        for _ in 0..20 {
            let d = decide_with("bun test unit", &BTreeMap::new(), Some(&p));
            assert!(reason(&d).contains("devrun task long"), "{}", reason(&d));
        }
    }

    #[test]
    fn an_app_launch_redirects_to_devrun_up() {
        let p = project(|c| {
            c.apps.insert(
                "web".into(),
                AppConfig {
                    base_port: 3000,
                    launch: vec!["nitro".into(), "dev".into(), "--port".into(), "{{ port }}".into()],
                    ..Default::default()
                },
            );
        });
        let d = decide_with("nitro dev", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devrun up web"), "{}", reason(&d));
    }

    #[test]
    fn the_catalog_outranks_a_launch_prefix() {
        let p = project(|c| {
            c.apps.insert(
                "web".into(),
                AppConfig {
                    base_port: 3000,
                    launch: vec!["vite".into(), "--port".into(), "{{ port }}".into()],
                    ..Default::default()
                },
            );
        });
        assert!(!denies(&decide_with("vite build", &BTreeMap::new(), Some(&p))));
        assert!(denies(&decide_with("vite", &BTreeMap::new(), Some(&p))));
    }

    #[test]
    fn a_catalog_hit_with_no_app_names_the_listing_command() {
        let p = project(|_| {});
        let d = decide_with("uvicorn app:app", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devkit config apps"), "{}", reason(&d));
    }

    #[test]
    fn a_quoted_mention_is_not_a_launch() {
        let p = project(|_| {});
        let cmd = r#"git commit -m "fix: crash under uvicorn; retry""#;
        assert!(!denies(&decide_with(cmd, &BTreeMap::new(), Some(&p))));
    }

    #[test]
    fn a_heredoc_body_is_not_a_launch() {
        let p = project(|_| {});
        let cmd = "cat > notes.md <<EOF\nuvicorn app:app\nEOF";
        assert!(!denies(&decide_with(cmd, &BTreeMap::new(), Some(&p))));
    }
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo nextest run -p devkit-ports guard::tests`
Expected: FAIL to compile.

- [ ] **Step 3: Implement the engine**

Replace the body of `crates/devkit-ports/src/guard/mod.rs`:

```rust
//! Decide whether a shell command has a devkit equivalent worth redirecting to.

pub mod appname;
pub mod catalog;
pub mod lex;
pub mod norm;
pub mod sig;
pub mod tasks;

use crate::apps::App;
use devkit_config::{AppMatch, CommandRule, Config};
use norm::{Normalized, basename};
use std::collections::{BTreeMap, HashMap};

/// Binaries devkit ships. Never gate the tools being promoted.
const SHIMS: [&str; 5] = ["devkit", "devrun", "lockm", "portm", "docm"];

/// The resolved half of the answer, absent when only the `[harness]` probe ran.
pub struct Project {
    pub config: Config,
    pub catalog: HashMap<String, App>,
    /// The hook's cwd relative to the project root, used as the last-resort
    /// app-name hint.
    pub cwd_rel: Option<String>,
    /// `[harness.app_match]`. It rides here rather than on `Config` because
    /// `Config` has no `harness` field, and because app naming is the only
    /// thing that reads it.
    pub app_match: AppMatch,
}

#[derive(Debug)]
pub enum Decision {
    Allow,
    Deny { reason: String },
}

/// Decide over already-loaded inputs. The IO-free half of the guard, so every
/// matching rule is unit-testable without a project on disk.
///
/// Segments are evaluated in order and the first denial decides the whole
/// command.
pub fn decide_with(
    command: &str,
    rules: &BTreeMap<String, CommandRule>,
    project: Option<&Project>,
) -> Decision {
    for words in lex::segments(command) {
        let Some(n) = norm::normalize(&words) else {
            continue;
        };
        let prog = basename(&n.argv[0]).to_string();
        if SHIMS.contains(&prog.as_str()) {
            continue;
        }
        if let Some(reason) = rule_hit(&n, &prog, rules) {
            return Decision::Deny { reason };
        }
        if let Some(p) = project
            && let Some(reason) = project_hit(&n, &prog, p)
        {
            return Decision::Deny { reason };
        }
    }
    Decision::Allow
}

/// Source 2: a `[harness.commands.*]` rule. An empty `programs` matches
/// nothing, which is how a child layer exempts a subtree.
fn rule_hit(
    n: &Normalized,
    prog: &str,
    rules: &BTreeMap<String, CommandRule>,
) -> Option<String> {
    rules.values().find_map(|rule| {
        let named = rule.programs.iter().any(|p| basename(p) == prog);
        let args_match = n.argv.len() > rule.args.len()
            && n.argv[1..=rule.args.len()] == rule.args[..];
        (named && (rule.args.is_empty() || args_match)).then(|| rule.reason.clone())
    })
}

/// Sources 3 through 5, in order.
fn project_hit(n: &Normalized, prog: &str, p: &Project) -> Option<String> {
    if let Some(name) = best_task(n, p) {
        return Some(format!(
            "`{}` is the `{name}` task. Run `devrun task {name}` so it gets its app directory, \
             layered env and allocated ports.",
            n.argv.join(" ")
        ));
    }

    let hits = matching_apps(n, p);

    // The catalog outranks an app's `launch` prefix: it knows which verbs start
    // a server, and a launch signature does not. The launch match still supplies
    // the app name, so the candidate set narrows to it when there was one.
    if catalog::is_known_program(prog) {
        return catalog::is_dev_server(&n.argv).then(|| {
            if !hits.is_empty() {
                return up_message(&n.argv, &hits);
            }
            let candidates: Vec<&App> = p.catalog.values().collect();
            let hint = appname::hint(&n.argv, p.cwd_rel.as_deref());
            match appname::resolve(hint.as_deref(), &candidates, &p.app_match) {
                Some(app) => up_message(&n.argv, &[app]),
                None => format!(
                    "`{}` starts a dev server. Start it with `devrun up <app>` so its port is \
                     registered and `devrun down`/`logs`/`status` can see it. \
                     `devkit config apps` lists the apps.",
                    n.argv.join(" ")
                ),
            }
        });
    }

    (!hits.is_empty()).then(|| up_message(&n.argv, &hits))
}

/// The longest-signature task this segment matches, if redirecting to it would
/// change the process. Longest wins so a `HashMap`'s iteration order cannot
/// make the message name a different task from one call to the next.
fn best_task(n: &Normalized, p: &Project) -> Option<String> {
    let mut hits: Vec<(usize, &String)> = p
        .config
        .tasks
        .iter()
        .filter_map(|(name, task)| {
            // Both sides normalize, or a task's own runner prefix and doppler
            // wrapper make it unmatchable against the stripped typed side.
            let cfg = norm::normalize(&task.run)?;
            let s = sig::signature(&cfg.argv)?;
            if !sig::matches(&s, &n.argv) {
                return None;
            }
            tasks::redirect_worth_it(
                task,
                &p.config.templates.variables,
                n.doppler.as_ref(),
                cfg.doppler.as_ref(),
            )
            .then_some((s.len(), name))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    hits.first().map(|(_, name)| (*name).clone())
}

/// Every app whose `launch` signature this segment matches, longest first.
fn matching_apps(n: &Normalized, p: &Project) -> Vec<String> {
    let mut hits: Vec<(usize, &str)> = p
        .catalog
        .values()
        .filter_map(|app| {
            let launch = norm::normalize(&app.launch)?;
            let s = sig::signature(&launch.argv)?;
            sig::matches(&s, &n.argv).then_some((s.len(), app.name.as_str()))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    let Some(&(best, _)) = hits.first() else {
        return Vec::new();
    };
    let tied: Vec<&App> = hits
        .iter()
        .take_while(|(len, _)| *len == best)
        .filter_map(|(_, name)| p.catalog.get(*name))
        .collect();
    if tied.len() > 1
        && let Some(app) = appname::resolve(
            appname::hint(&n.argv, p.cwd_rel.as_deref()).as_deref(),
            &tied,
            &p.app_match,
        )
    {
        return vec![app];
    }
    tied.into_iter().map(|a| a.name.clone()).collect()
}

fn up_message(argv: &[String], apps: &[String]) -> String {
    let ups = apps
        .iter()
        .map(|a| format!("`devrun up {a}`"))
        .collect::<Vec<_>>()
        .join(" or ");
    format!(
        "`{}` is how devkit launches this app. Run {ups} instead, so the port is allocated from \
         the registry and `devrun down`/`logs`/`status` can see the server.",
        argv.join(" ")
    )
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p devkit-ports guard`
Expected: PASS, all twelve engine tests plus every module test from Tasks 4 through 9.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/guard/mod.rs
git commit -m "feat(guard): decide a command against rules, tasks and apps"
```

---

### Task 11: The payload parser and the two deny envelopes

**Files:**
- Modify: `crates/devkit-common/src/harness.rs`

**Interfaces:**
- Consumes: Task 1's `deny_json`.
- Produces: `devkit_common::harness::{Harness, ShellPayload, parse_shell_payload, deny_shell_json}`, where `parse_shell_payload(p: &Value) -> Option<ShellPayload>` and `deny_shell_json(harness: Harness, reason: &str) -> Value`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_claude_code_payload_carries_its_command_under_tool_input() {
    let p = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": "vite dev" },
        "cwd": "/repo"
    });
    let parsed = parse_shell_payload(&p).expect("a Bash payload parses");
    assert_eq!(parsed.harness, Harness::ClaudeCode);
    assert_eq!(parsed.command, "vite dev");
    assert_eq!(parsed.cwd.unwrap(), std::path::Path::new("/repo"));
}

#[test]
fn a_cursor_payload_carries_its_command_at_the_top_level() {
    let p = serde_json::json!({ "command": "vite dev", "cwd": "/repo" });
    let parsed = parse_shell_payload(&p).expect("a Cursor payload parses");
    assert_eq!(parsed.harness, Harness::Cursor);
    assert_eq!(parsed.command, "vite dev");
}

#[test]
fn a_cursor_generic_tool_payload_is_still_cursor() {
    // Cursor's generic preToolUse also carries tool_input, so the presence of
    // that key cannot be the discriminator.
    let p = serde_json::json!({ "tool_name": "Shell", "tool_input": { "command": "vite" } });
    assert_eq!(parse_shell_payload(&p).unwrap().harness, Harness::Cursor);
}

#[test]
fn a_non_bash_tool_is_not_a_shell_payload() {
    let p = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Edit",
        "tool_input": { "file_path": "/repo/a.rs" }
    });
    assert!(parse_shell_payload(&p).is_none());
}

#[test]
fn a_payload_with_no_command_is_not_a_shell_payload() {
    assert!(parse_shell_payload(&serde_json::json!({ "cwd": "/repo" })).is_none());
}

#[test]
fn each_harness_gets_its_own_deny_envelope() {
    let cc = deny_shell_json(Harness::ClaudeCode, "use devrun");
    assert_eq!(cc["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(cc["hookSpecificOutput"]["permissionDecisionReason"], "use devrun");

    let cur = deny_shell_json(Harness::Cursor, "use devrun");
    assert_eq!(cur["permission"], "deny");
    assert_eq!(cur["agent_message"], "use devrun");
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo nextest run -p devkit-common harness::tests::a_claude_code_payload`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

```rust
/// Which harness sent a payload, and therefore which envelope answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    /// Claude Code and Codex, which share `PreToolUse` and its envelope.
    ClaudeCode,
    Cursor,
}

/// A pre-execution shell payload, normalized across the harnesses.
#[derive(Debug, Clone)]
pub struct ShellPayload {
    pub harness: Harness,
    pub command: String,
    pub cwd: Option<PathBuf>,
}

/// Read a pre-execution shell payload. `None` when the event is not about a
/// shell command, which is not a failure: harnesses send events this hook does
/// not model.
///
/// The harness is told apart by `hook_event_name`, which Claude Code and Codex
/// send and Cursor does not. The presence of `tool_input` cannot be the
/// discriminator: Cursor's generic `preToolUse` carries that key too, so
/// testing it would answer a Cursor session in Claude Code's envelope.
pub fn parse_shell_payload(p: &Value) -> Option<ShellPayload> {
    let harness = match p.get("hook_event_name").and_then(Value::as_str) {
        Some(_) => Harness::ClaudeCode,
        None => Harness::Cursor,
    };
    if harness == Harness::ClaudeCode
        && p.get("tool_name").and_then(Value::as_str) != Some("Bash")
    {
        return None;
    }
    let command = p
        .get("command")
        .and_then(Value::as_str)
        .or_else(|| p.get("tool_input")?.get("command")?.as_str())
        .filter(|s| !s.trim().is_empty())?
        .to_string();
    let cwd = p
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    Some(ShellPayload { harness, command, cwd })
}

/// The deny envelope this harness reads. Cursor's reason goes in
/// `agent_message`: `user_message` is shown to the human, and only the agent
/// message reaches the agent, which is the whole point of handing back a
/// command it can retry.
pub fn deny_shell_json(harness: Harness, reason: &str) -> Value {
    match harness {
        Harness::ClaudeCode => deny_json(reason),
        Harness::Cursor => json!({
            "permission": "deny",
            "agent_message": reason,
            "continue": true
        }),
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p devkit-common harness`
Expected: PASS, all six new tests.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-common/src/harness.rs
git commit -m "feat(harness): parse and answer both shell payload shapes"
```

---

### Task 12: The `devkit harness shell` subcommand

**Files:**
- Create: `src/bin/devkit/harness.rs`
- Modify: `src/bin/devkit/main.rs` (add the `Harness` variant and its dispatch arm)

**Interfaces:**
- Consumes: Tasks 3, 10 and 11.
- Produces: the `devkit harness shell` CLI path. No library interface.

- [ ] **Step 1: Write the failing test**

Create `tests/harness_guard.rs`:

```rust
//! End-to-end coverage of `devkit harness shell`: the gate, the deny envelopes,
//! and the fail-open contract. Each test runs in a private temp project with a
//! private HOME so it never reads the developer's real global config.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn project(config: &str) -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(p.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::write(p.path().join("devkit.toml"), config).unwrap();
    p
}

fn run_hook(project: &Path, home: &Path, payload: &str) -> Output {
    let exe = Path::new(env!("CARGO_BIN_EXE_devkit"));
    let mut child = Command::new(exe)
        .args(["harness", "shell"])
        .current_dir(project)
        .env("HOME", home)
        .env("XDG_STATE_HOME", home)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_ENFORCE_COMMANDS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn devkit harness shell");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().expect("hook output")
}

fn claude_payload(command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command }
    })
    .to_string()
}

fn denied(out: &Output) -> bool {
    assert_eq!(
        out.status.code(),
        Some(0),
        "the guard must always exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.trim().is_empty() {
        return false;
    }
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    v["hookSpecificOutput"]["permissionDecision"] == "deny"
        || v["permission"] == "deny"
}

const GUARDED: &str = r#"
[harness]
enforce_commands = true

[harness.commands.bun-only]
programs = ["node"]
reason = "This workspace is bun-only."
"#;

/// `Defaults` has four required fields with no serde default, so any config
/// that declares `[apps]` or `[tasks]` must carry the table or `resolve` fails
/// and the whole project half of the guard is silently skipped.
const DEFAULTS: &str = r#"
[defaults]
worktree_root = "../wt"
branch_prefix = "x/"
baseline_ref = "origin/main"
baseline_path = "../baseline"
"#;

#[test]
fn a_user_rule_denies_through_the_binary() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let out = run_hook(proj.path(), home.path(), &claude_payload("node server.js"));
    assert!(denied(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("bun-only"), "reason reaches the agent: {stdout}");
}

#[test]
fn the_gate_off_allows_everything() {
    let home = tempfile::tempdir().unwrap();
    let proj = project("[defaults]\napps_dir = \"apps\"\n");
    let out = run_hook(proj.path(), home.path(), &claude_payload("node server.js"));
    assert!(!denied(&out));
}

#[test]
fn a_cursor_payload_gets_the_cursor_envelope() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let payload = serde_json::json!({ "command": "node server.js" }).to_string();
    let out = run_hook(proj.path(), home.path(), &payload);
    assert!(denied(&out));
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(v["permission"], "deny");
    assert!(v["agent_message"].is_string());
}

#[test]
fn an_unparseable_config_fails_open() {
    // The gate must be forced on: an unparseable layer makes `project_layers`
    // fail, so the gate would read false on its own and this would prove only
    // that the gate-off path works.
    let home = tempfile::tempdir().unwrap();
    let proj = project("[[[ not toml");
    let exe = Path::new(env!("CARGO_BIN_EXE_devkit"));
    let mut child = Command::new(exe)
        .args(["harness", "shell"])
        .current_dir(proj.path())
        .env("HOME", home.path())
        .env("DEVKIT_ENFORCE_COMMANDS", "1")
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(claude_payload("node server.js").as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(!denied(&out));
}

#[test]
fn the_env_override_disables_the_guard() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let exe = Path::new(env!("CARGO_BIN_EXE_devkit"));
    let mut child = Command::new(exe)
        .args(["harness", "shell"])
        .current_dir(proj.path())
        .env("HOME", home.path())
        .env("DEVKIT_ENFORCE_COMMANDS", "0")
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(claude_payload("node server.js").as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(!denied(&out));
}

#[test]
fn a_garbage_payload_exits_zero_and_says_nothing() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let out = run_hook(proj.path(), home.path(), "not json at all");
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
}

#[test]
fn a_task_denies_through_the_binary_and_writes_no_registry_row() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(&format!(
        "{GUARDED}{DEFAULTS}\n[tasks.check]\nrun = [\"bun\", \"test\"]\napp = \"web\"\n\
         [apps.web]\nbase_port = 3000\npath = \"apps/web\"\nlaunch = [\"vite\"]\n"
    ));
    std::fs::create_dir_all(proj.path().join("apps/web")).unwrap();

    // Assert the denial first. Without it the no-registry-row assertion passes
    // against a config that never resolved, which is what a missing [defaults]
    // silently produced.
    let out = run_hook(proj.path(), home.path(), &claude_payload("bun test"));
    assert!(denied(&out), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("devrun task check"));

    let ports = home.path().join("devkit/ports.json");
    assert!(!ports.exists(), "the guard allocated a port: {}", ports.display());
}

#[test]
fn the_cwd_names_the_app_for_a_catalog_hit() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(&format!(
        "{GUARDED}{DEFAULTS}\n[apps.web]\nbase_port = 3000\npath = \"apps/web\"\n\
         launch = [\"vite\", \"--port\", \"{{{{ port }}}}\"]\n"
    ));
    let app_dir = proj.path().join("apps/web");
    std::fs::create_dir_all(&app_dir).unwrap();
    let out = run_hook(&app_dir, home.path(), &claude_payload("vite"));
    assert!(denied(&out), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("devrun up web"));
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo nextest run -p devkit --test harness_guard`
Expected: FAIL — `devkit harness shell` is not a subcommand.

- [ ] **Step 3: Implement the subcommand**

Create `src/bin/devkit/harness.rs`:

```rust
//! `devkit harness shell`: the pre-execution command guard.
//!
//! Reads a harness's hook payload on stdin and either emits that harness's deny
//! envelope or says nothing. Every failure path exits 0 — a missed nudge costs
//! nothing, while a false denial would block legitimate work on every command.

use anyhow::Result;
use clap::{Args, Subcommand};
use devkit_common::harness::{self, Harness, ShellPayload};
use devkit_ports::guard::{self, Decision, Project};
use std::io::Read;

#[derive(Args)]
pub struct HarnessCli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Guard a shell command about to run, reading the payload on stdin.
    Shell,
}

pub fn run(cli: HarnessCli) -> Result<()> {
    match cli.cmd {
        Cmd::Shell => {
            guard_shell();
            Ok(())
        }
    }
}

/// Never returns an error and never panics out: a guard that fails a tool call
/// is worse than a guard that misses one.
fn guard_shell() {
    let outcome = std::panic::catch_unwind(|| {
        let mut buf = String::new();
        if std::io::stdin().read_to_string(&mut buf).is_err() {
            return None;
        }
        let payload: serde_json::Value = serde_json::from_str(&buf).ok()?;
        let ShellPayload {
            harness: which,
            command,
            cwd,
        } = harness::parse_shell_payload(&payload)?;

        let cwd = cwd.or_else(|| std::env::current_dir().ok())?;
        if !harness::commands_enabled(&cwd) {
            return None;
        }

        let (rules, warnings) = harness::resolve_rules(&cwd);
        for w in warnings {
            eprintln!("devkit: {w}");
        }
        let project = load_project(&cwd, rules.app_match.clone());
        match guard::decide_with(&command, &rules.commands, project.as_ref()) {
            Decision::Allow => None,
            Decision::Deny { reason } => Some(harness::deny_shell_json(which, &reason)),
        }
    });

    match outcome {
        Ok(Some(envelope)) => println!("{envelope}"),
        Ok(None) => {}
        Err(_) => eprintln!("devkit: command guard panicked; allowing the command"),
    }
}

/// The resolved config and app catalog, or `None` when this is not a devkit
/// project or the config will not load. Read through `load_quiet`: an
/// unresolvable app is not this command's business to report.
fn load_project(cwd: &std::path::Path, app_match: devkit_config::AppMatch) -> Option<Project> {
    let loaded = devkit_ports::load::load_quiet(None, cwd).ok()?;
    // `checkout_root`, not `main_checkout`: the latter is `None` when this *is*
    // the primary clone, and in a linked worktree it names a directory the cwd
    // is never under, so the relative path would never resolve anywhere.
    let cwd_rel = devkit_common::git::checkout_root(cwd)
        .ok()
        .and_then(|r| cwd.strip_prefix(r).ok().map(|p| p.to_path_buf()))
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .filter(|s| !s.is_empty());
    Some(Project {
        config: loaded.config,
        catalog: loaded.catalog,
        cwd_rel,
        app_match,
    })
}
```

In `src/bin/devkit/main.rs`, add `mod harness;`, then the variant beside `Mcp`:

```rust
    /// Coding-agent harness hooks.
    #[command(display_name = "devkit harness")]
    Harness(harness::HarnessCli),
```

and the dispatch arm beside the others: `Cmd::Harness(c) => harness::run(c),`.

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p devkit --test harness_guard`
Expected: PASS, all seven tests.

- [ ] **Step 5: Run the whole gate**

Run: `cargo nextest run --workspace --no-fail-fast`
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS. `tests/cli_ergonomics.rs` and `tests/completions.rs` cover the command tree; if either asserts a fixed subcommand list, add `harness` to it.

- [ ] **Step 6: Commit**

```bash
git add src/bin/devkit/harness.rs src/bin/devkit/main.rs tests/harness_guard.rs
git commit -m "feat(harness): add the devkit harness shell command guard"
```

---

### Task 13: Register the hook, and document it

**Files:**
- Modify: `hooks/hooks.json`, `hooks/hooks-codex.json`, `hooks/hooks-cursor.json`
- Modify: `docs/configuration.md`, `docs/commands.md`, `AGENTS.md`

**Interfaces:**
- Consumes: Task 12's subcommand.
- Produces: nothing further.

- [ ] **Step 1: Register on Claude Code and Codex**

In `hooks/hooks.json`, add a second `PreToolUse` entry beside the write-tool one:

```json
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "devkit harness shell",
            "timeout": 10
          }
        ]
      }
```

Add the identical entry to `hooks/hooks-codex.json`'s `PreToolUse` array. Codex's unified exec matches as `Bash`, so it needs no second matcher.

- [ ] **Step 2: Register on Cursor**

`hooks/hooks-cursor.json` has no pre-execution hook today. Add the event:

```json
    "beforeShellExecution": [
      {
        "type": "command",
        "command": "devkit harness shell",
        "timeout": 10
      }
    ]
```

- [ ] **Step 3: Verify the manifests parse**

Run: `python3 -c "import json,sys; [json.load(open(p)) for p in sys.argv[1:]]" hooks/hooks.json hooks/hooks-codex.json hooks/hooks-cursor.json`
Expected: no output, exit 0.

- [ ] **Step 4: Document the config**

In `docs/configuration.md`, extend the `[harness]` section's table with the two new keys and add a `[harness.commands.<name>]` subsection. Keep every paragraph on one line.

The `enforce_commands` row:

```markdown
| `enforce_commands` | bool | `false` | When `true`, the devkit plugin's pre-execution hook refuses shell commands devkit already has a wired-up path for, naming the replacement. Resolves through the same three sources as `enforce_writes`, with `DEVKIT_ENFORCE_COMMANDS` as the override. |
```

Then, after the existing "What enforcement gates" paragraph, add:

```markdown
**What the command guard gates.** `Bash` (Claude Code and Codex) and `beforeShellExecution` (Cursor). A command is refused when it is a devkit shim's job, matches a `[harness.commands.*]` rule, retypes a `[tasks]` entry that devkit would run differently, launches an app the way its `[apps] launch` does, or starts a dev server devkit knows by name. Everything else runs. Unlike write enforcement, the guard fails **open**: a config it cannot read, a rule it cannot parse, or an internal error warns on stderr and lets the command through. `DEVKIT_ENFORCE_COMMANDS=0` turns it off for a session.

**Rules.** `[harness.commands.<name>]` takes `programs` (the program names to refuse, matched by basename), an optional `args` prefix, and the `reason` shown to the agent. Rules merge across config layers like every other table: a child layer adds names, and redefining a rule by the same name overrides only the keys it sets. An empty `programs` matches nothing, which is how a subtree opts out of a rule its parent declared.

**Naming the app.** When the guard refuses a dev-server command it names the app to run instead, resolving the app from a workspace path in the command, a `--filter`/`--dir`/`-C` value, or the shell's directory. Exact names and paths resolve first; a fuzzy match then rescues near-misses such as `lab-tools` against an app declared `lab_tools`. `[harness.app_match]` tunes that last step: `fuzzy` (default `true`) turns it off, `max_typos` (default `1`) is how many character differences it forgives, and `min_score` (default `60`) is the score below which no app is named and the message falls back to `devkit config apps`. Raising `max_typos` buys wrong app names; a project that would rather see the listing than a guess sets `fuzzy = false`.
```

Update the `Bash` gap sentence so it scopes to writes: shell-level *writes* remain uncovered by `lockm`, which is still true. Add `devkit` beside `lockm` in the "Activation requires `lockm` on `PATH`" paragraph.

- [ ] **Step 5: Document the command**

In `docs/commands.md`, add a `harness` section describing `devkit harness shell` as a hook entry point that reads a payload on stdin, never mutates, and always exits 0.

- [ ] **Step 6: Update `AGENTS.md`**

Add `harness` to the list of `devkit` subcommands in the Layout table's `src/bin/devkit/` row, and add one invariant under **Invariants (do not break)**:

```markdown
- **The command guard fails open and never mutates.** `devkit harness shell` allocates no port, writes no registry row, takes no lock, and never calls `task::resolve`. Any failure — a config that will not load, a malformed rule, a panic — warns on stderr and exits 0. This is the deliberate opposite of `enforce_writes`, which fails closed: a missed write lock corrupts another session's work, while a false command denial would block legitimate work on every shell call.
```

- [ ] **Step 7: Run the full gate**

Run: `cargo fmt --all && cargo nextest run --workspace --no-fail-fast && cargo test --workspace --doc && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add hooks/ docs/configuration.md docs/commands.md AGENTS.md
git commit -m "docs(harness): register and document the command guard"
```

---

## Verification

After Task 13, confirm the whole feature end to end in a scratch project:

```bash
cd "$(mktemp -d)" && git init -q -b main
printf '[harness]\nenforce_commands = true\n\n[harness.commands.bun-only]\nprograms = ["node"]\nreason = "This workspace is bun-only."\n' > devkit.toml
echo '{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"node server.js"}}' | devkit harness shell
```

Expected: a JSON deny envelope naming the bun-only reason, exit 0.

```bash
echo '{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git commit -m \"fix: uvicorn; retry\""}}' | devkit harness shell
```

Expected: no output, exit 0.
