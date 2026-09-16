# Required Args Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A project can declare that an `--arg` must be supplied, optionally only by agents or only by humans, on every devkit command that accepts `--arg`.

**Architecture:** `devkit-config` gains the config shape (`Required`, `VariableDecl`) and two accessors over `Templates::variables`. `devkit-common` gains the behaviour: a `Caller` enum, a pure `decide_caller` with an IO wrapper, and a pure `missing_args` resolver every surface calls. The four `--arg` commands resolve `Caller` once at entry and pass it down. No surface re-derives required-ness.

**Tech Stack:** Rust edition 2024, serde + toml, schemars (JSON Schema), minijinja via `devkit_common::template`, anyhow, cargo-nextest.

**Spec:** `docs/superpowers/specs/2026-09-16-required-args-design.md`

## Global Constraints

- The merge gate is all three of: `cargo nextest run --workspace --no-fail-fast`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --doc`. Run all three before every commit.
- Formatting requires nightly: `cargo +nightly fmt --all`. Stable rustfmt silently ignores `rustfmt.toml`'s unstable options and reformats to defaults. Never run stable `cargo fmt` here.
- Three tests fail in a root container for reasons unrelated to this work: `devkit-common git::tests::a_path_that_cannot_be_resolved_is_unknown_rather_than_different`, `devkit-common git::tests::a_double_failure_is_unknown_unless_both_paths_are_merely_absent`, `devkit::install_links a_failed_pass_is_partial_and_retries_once_the_cooldown_elapses`. Each chmods a directory to `0o000` and expects the read to fail, which does not happen as root. They pass in CI. Treat only *new* failures as yours.
- `schema/devkit-config.json` is generated, never hand-edited. Regenerate with `DEVKIT_UPDATE_SCHEMA=1 cargo test --workspace schema`.
- Conventional Commits, imperative and lowercase after the colon. Commit author identity comes from the VM environment; add `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.
- Test scratch comes from `tempfile`. Never build a path by hand from `std::env::temp_dir()`.
- `anyhow` everywhere, with `.context()` chains.
- No `_ =>` catch-all arms over `Required` or `Caller`. Match exhaustively.
- Branch `claude/eager-hawking-2kkefo`, force-push allowed (PR #83 squash-merges).

## Baseline

The branch is reset to `main` plus the spec commit. There is no prior implementation to revert; an earlier attempt was discarded by force-push.

## File Structure

| File | Responsibility |
|---|---|
| `crates/devkit-config/src/lib.rs` | `Required`, `VariableDecl`, `Templates::defaults()`/`declared()`, and the load-time rejection of `never` without a default. Shape only, no behaviour. |
| `crates/devkit-common/src/caller.rs` (new) | `Caller`, `HARNESS_SESSION_VARS`, `decide_caller` (pure), `parse_caller_override` (pure), `caller()` (does the env and TTY reads). |
| `crates/devkit-common/src/required.rs` (new) | `Missing`, `binds`, `declared_required`, `is_required`, `missing_args`. All pure. The single door. |
| `crates/devkit-locks/src/ident.rs` | Loses its own `HARNESS_SESSION_VARS`, re-exports the one in `devkit-common`. |
| `crates/devkit-ports/src/task.rs` | `TaskArg::required` computed from the shared resolver; `check_args` delegates its requirement half. |
| `src/bin/devkit/run/mod.rs` | Resolves `Caller` for `devrun task` and the task listing. |
| `src/bin/devkit/issue/{pr/create.rs,review/request.rs,review/finish.rs}` | Each resolves `Caller`, computes its template's read set, calls `missing_args`. |
| `crates/devkit-ports/src/guard/mod.rs` | Usage hint built with `Caller::Agent`. |
| `docs/configuration.md`, `docs/commands.md` | User-facing reference. |

---

### Task 1: Config shape and the two accessors

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (`Templates::variables` at ~line 907)
- Modify (mechanical, `.variables` → `.defaults()`): `src/bin/devkit/baseline/mod.rs`, `src/bin/devkit/issue/end.rs`, `src/bin/devkit/issue/summary.rs`, `src/bin/devkit/issue/checkout.rs`, `src/bin/devkit/issue/setup.rs`, `src/bin/devkit/issue/review/{mod.rs,request.rs,finish.rs}`, `src/bin/devkit/issue/pr/create.rs`, `src/bin/devkit/run/mod.rs`, `crates/devkit-mcp/src/devrun.rs`, `crates/devkit-ports/src/{task.rs,guard/mod.rs}`
- Modify: `schema/devkit-config.json` (regenerate)

**Interfaces:**
- Produces: `Required` (`Never`/`Humans`/`Agents`/`Always`, serde lowercase, `Never` is `Default`); `VariableDecl` with `default_value() -> Option<&str>` and `required() -> Required`; `Templates::defaults() -> BTreeMap<String, String>`; `Templates::declared() -> BTreeSet<String>`.

- [ ] **Step 1: Write the failing tests**

In the `tests` module of `crates/devkit-config/src/lib.rs`:

```rust
#[test]
fn variable_declarations_parse_in_all_three_forms() {
    let s = "[defaults]\nworktree_root='w'\nbranch_prefix='x/'\nbaseline_ref='m'\n\
             [templates.variables]\n\
             team = 'platform'\n\
             msg = { default = 'wip', required = 'agents' }\n\
             ticket = { required = 'always' }\n";
    let c = Config::parse(s).unwrap();
    let v = &c.templates.variables;
    assert_eq!(v["team"].default_value(), Some("platform"));
    assert_eq!(v["team"].required(), Required::Never);
    assert_eq!(v["msg"].default_value(), Some("wip"));
    assert_eq!(v["msg"].required(), Required::Agents);
    assert_eq!(v["ticket"].default_value(), None);
    assert_eq!(v["ticket"].required(), Required::Always);

    // A plain string must still round-trip as a bare string.
    let out = toml::to_string(&c).unwrap();
    assert!(out.contains("team = \"platform\""), "{out}");
    let c2 = Config::parse(&out).unwrap();
    assert_eq!(c2.templates.variables["msg"].required(), Required::Agents);
    assert_eq!(c2.templates.variables["ticket"].default_value(), None);
}

#[test]
fn defaults_omits_valueless_entries_and_declared_keeps_them() {
    let s = "[defaults]\nworktree_root='w'\nbranch_prefix='x/'\nbaseline_ref='m'\n\
             [templates.variables]\n\
             team = 'platform'\n\
             ticket = { required = 'always' }\n";
    let c = Config::parse(s).unwrap();
    let d = c.templates.defaults();
    assert_eq!(d.get("team").map(String::as_str), Some("platform"));
    assert!(!d.contains_key("ticket"), "a valueless entry must not reach the render context");
    let declared = c.templates.declared();
    assert!(declared.contains("team") && declared.contains("ticket"));
}

#[test]
fn a_misspelled_key_in_a_variable_table_is_rejected() {
    let s = "[defaults]\nworktree_root='w'\nbranch_prefix='x/'\nbaseline_ref='m'\n\
             [templates.variables]\n\
             msg = { deafult = 'wip', required = 'agents' }\n";
    assert!(Config::parse(s).is_err(), "deny_unknown_fields must catch the typo");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-config --lib variable_declarations_parse_in_all_three_forms`
Expected: FAIL to compile, "cannot find type `Required`".

- [ ] **Step 3: Add the types**

In `crates/devkit-config/src/lib.rs`, above `pub struct Templates`:

```rust
/// Who must supply an `--arg` even though a default exists. `Never` is the
/// absence of a marking, not a licence to omit: the derived rule (an arg with
/// no default is required) is a floor this enum sits on top of and cannot
/// lower. `Never` is therefore meaningful only as a task-level override of a
/// variable-level marking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Required {
    #[default]
    Never,
    Humans,
    Agents,
    Always,
}

/// One `[templates.variables]` entry: a bare string, or a table carrying a
/// `default` and a `required` marking.
///
/// `deny_unknown_fields` for the reason `RunArg` documents: under untagged
/// matching a misspelled key beside a well-formed pair deserializes silently,
/// and the author sees no diagnostic while their guard does nothing.
///
/// ```
/// # use devkit_config::{Config, Required};
/// # let cfg = Config::parse(r#"
/// [templates.variables]
/// team = "platform"                              # a constant, never required
/// msg = { default = "wip", required = "agents" } # defaulted, but agents must pass it
/// ticket = { required = "always" }               # declared, no default, always required
/// # "#).unwrap();
/// # let v = &cfg.templates.variables;
/// # assert_eq!(v["team"].required(), Required::Never);
/// # assert_eq!(v["msg"].default_value(), Some("wip"));
/// # assert_eq!(v["ticket"].default_value(), None);
/// ```
///
/// `default` is the fallback used when nothing else supplies the name, which
/// is what these entries have always been: `template::render` merges them
/// underneath the context, and `--arg` overwrites them. Note that minijinja's
/// own `default` filter is a different thing and does not affect whether an
/// arg is required.
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum VariableDecl {
    Value(String),
    Table {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
        #[serde(default)]
        required: Required,
    },
}

impl VariableDecl {
    /// The fallback value, absent for a declaration that only marks a name.
    pub fn default_value(&self) -> Option<&str> {
        match self {
            VariableDecl::Value(v) => Some(v),
            VariableDecl::Table { default, .. } => default.as_deref(),
        }
    }

    /// The marking. A bare string carries none.
    pub fn required(&self) -> Required {
        match self {
            VariableDecl::Value(_) => Required::Never,
            VariableDecl::Table { required, .. } => *required,
        }
    }
}

impl From<&str> for VariableDecl {
    fn from(v: &str) -> Self {
        VariableDecl::Value(v.to_string())
    }
}
```

- [ ] **Step 4: Change the field and add the accessors**

Change the field on `Templates`:

```rust
    /// Constants available to every template above. A context field of the same
    /// name wins, and `--arg key=value` overrides either. An entry may instead
    /// be a table carrying a `default` and a `required` marking.
    #[serde(default)]
    pub variables: std::collections::BTreeMap<String, VariableDecl>,
```

Add to `impl Templates`:

```rust
    /// Name to value, for building a render context. A declaration with no
    /// `default` is omitted, so a template reading it hits strict-undefined
    /// until a caller supplies the name.
    pub fn defaults(&self) -> std::collections::BTreeMap<String, String> {
        self.variables
            .iter()
            .filter_map(|(k, d)| d.default_value().map(|v| (k.clone(), v.to_string())))
            .collect()
    }

    /// Every declared name, valueless ones included. This is the `--arg`
    /// allowlist: without it a name declared only to be marked required would
    /// be unpassable.
    pub fn declared(&self) -> std::collections::BTreeSet<String> {
        self.variables.keys().cloned().collect()
    }
```

- [ ] **Step 5: Update every consumer**

Each site that passes the map to a renderer takes `.defaults()` instead. The two `parse_args` allowlist sites take `.declared()` (Task 6 changes `parse_args`'s signature; for now pass `&cfg.templates.defaults()` so the tree compiles).

`reject_reserved_variables` in the same file keys off names only, so it needs no change beyond compiling.

One existing test asserts the old value type and must be updated, not deleted:
`an_ordinary_template_variable_is_still_accepted` does
`assert_eq!(cfg.templates.variables["region"], "eu")`. It becomes
`assert_eq!(cfg.templates.variables["region"].default_value(), Some("eu"))`.
Grep the whole workspace for `variables[` and `variables.get(` to catch its
siblings.

Run `cargo build --workspace --all-targets` and fix each error in turn. Expect
roughly 47 references across 14 files; all are `.variables` → `.defaults()`
except the allowlist pair and the assertions above. Use `--all-targets` so test
code breaks now rather than at the gate.

- [ ] **Step 6: Run the gate**

```bash
cargo +nightly fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-fail-fast
cargo test --workspace --doc
```
Expected: PASS, except the three known root-container failures.

- [ ] **Step 7: Regenerate the schema**

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test --workspace schema`
Then `cargo nextest run --workspace schema` to confirm it no longer diffs.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(config): let a template variable carry a required marking"
```

---

### Task 2: Reject `never` without a default at config load

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (beside `reject_reserved_variables`, ~line 1453)

**Interfaces:**
- Consumes: `Required`, `VariableDecl::default_value` from Task 1.
- Produces: `reject_never_without_default(cfg: &Config, origin: &HashMap<String, PathBuf>) -> Result<()>`, called from the same place `reject_reserved_variables` is.

- [ ] **Step 1: Write the failing test**

The validation runs inside `resolve_with_home`, not `Config::parse`, so these
mirror `a_template_variable_colliding_with_a_context_key_is_rejected` exactly:

```rust
#[test]
fn never_without_a_default_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("devkit.toml"),
        "[config]\nroot = true\n[templates.variables]\nticket = { required = 'never' }\n",
    )
    .unwrap();
    let err = resolve_with_home(None, dir.path(), None, None, None, None).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("ticket"), "the error names the variable: {msg}");
    assert!(msg.contains("never"), "the error names the marking: {msg}");
}

#[test]
fn never_with_a_default_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("devkit.toml"),
        "[config]\nroot = true\n\
         [templates.variables]\nmsg = { default = 'wip', required = 'never' }\n",
    )
    .unwrap();
    let (cfg, _) = resolve_with_home(None, dir.path(), None, None, None, None).unwrap();
    assert_eq!(cfg.templates.variables["msg"].default_value(), Some("wip"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-config --lib never_without_a_default_is_rejected`
Expected: FAIL, the config parses cleanly.

- [ ] **Step 3: Implement**

```rust
/// Refuse `required = "never"` on a variable with no default. The derived rule
/// already requires such a name, `never` cannot lower that floor, and
/// honouring it would replace a named error with minijinja's strict-undefined
/// chain, which never mentions the arg.
fn reject_never_without_default(cfg: &Config, origin: &HashMap<String, PathBuf>) -> Result<()> {
    for (name, decl) in &cfg.templates.variables {
        if decl.required() != Required::Never || decl.default_value().is_some() {
            continue;
        }
        if matches!(decl, VariableDecl::Value(_)) {
            continue;
        }
        let key = format!("templates.variables.{name}");
        let declared = origin
            .get(&key)
            .map(|p| format!(" (declared in {})", p.display()))
            .unwrap_or_default();
        anyhow::bail!(
            "`[templates.variables] {name}`{declared} sets `required = \"never\"` \
             with no `default`. An arg with nothing to fall back on is required \
             either way; give it a `default` or drop the marking."
        );
    }
    Ok(())
}
```

Call it next to `reject_reserved_variables(&cfg, &origin)?`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit-config --lib never`
Expected: PASS.

- [ ] **Step 5: Run the gate and commit**

```bash
cargo +nightly fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace --no-fail-fast
git add -A
git commit -m "feat(config): reject a never marking with no default"
```

---

### Task 3: Caller identity

**Files:**
- Create: `crates/devkit-common/src/caller.rs`
- Modify: `crates/devkit-common/src/lib.rs` (add `pub mod caller;`, alphabetically before `cmd`)
- Modify: `crates/devkit-locks/src/ident.rs` (drop its own `HARNESS_SESSION_VARS`, use the shared one)

**Interfaces:**
- Produces: `Caller` (`Human`/`Agent`, `Copy`, `PartialEq`); `HARNESS_SESSION_VARS: [&str; 2]`; `decide_caller(harness_session: bool, is_tty: bool) -> Caller`; `parse_caller_override(val: Option<&str>) -> Option<Caller>`; `caller() -> Caller`.

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-common/src/caller.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_harness_session_means_agent_whatever_the_terminal_says() {
        assert_eq!(decide_caller(true, true), Caller::Agent);
        assert_eq!(decide_caller(true, false), Caller::Agent);
    }

    #[test]
    fn without_a_harness_session_the_terminal_decides() {
        assert_eq!(decide_caller(false, true), Caller::Human);
        assert_eq!(decide_caller(false, false), Caller::Agent);
    }

    #[test]
    fn the_override_parses_both_names_and_ignores_anything_else() {
        assert_eq!(parse_caller_override(Some("agent")), Some(Caller::Agent));
        assert_eq!(parse_caller_override(Some(" HUMAN ")), Some(Caller::Human));
        assert_eq!(parse_caller_override(Some("")), None);
        assert_eq!(parse_caller_override(Some("yes")), None);
        assert_eq!(parse_caller_override(None), None);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-common --lib caller`
Expected: FAIL to compile, module does not exist.

- [ ] **Step 3: Implement**

```rust
//! Who is running this command. A coding agent and a person get different
//! answers from the `required` markings in `[templates.variables]`, and this
//! is the single door that decides which.

use std::io::IsTerminal;

/// Environment variables carrying a coding-agent harness's own session id, one
/// per harness. `CODEX_THREAD_ID` is excluded deliberately: it holds the same
/// value as `CODEX_SESSION_ID`, so listing it would manufacture a false
/// ambiguity for `devkit-locks`, which reads this same list.
pub const HARNESS_SESSION_VARS: [&str; 2] = ["CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    Human,
    Agent,
}

/// The classification, with the reads done by the caller so this stays pure.
///
/// A harness session id is positive evidence of a coding agent, so it leads: a
/// pipe, a cron job and `devrun task foo < /dev/null` are not agents. The
/// terminal check stays as a backstop because the list above covers Claude
/// Code and Codex, while Cursor is recognised from a hook payload field rather
/// than the environment.
///
/// The classification leans toward `Agent`. An agent misread as human means a
/// marking never fires and the default is taken silently, which is the defect
/// this exists to close. A human misread as agent is asked for an arg they
/// expected to be defaulted: recoverable, and never silent.
pub fn decide_caller(harness_session: bool, is_tty: bool) -> Caller {
    if harness_session || !is_tty {
        Caller::Agent
    } else {
        Caller::Human
    }
}

/// `DEVKIT_CALLER` as an explicit answer. Blank or unrecognised is no opinion,
/// the same tri-state shape `harness::parse_env_override` uses.
pub fn parse_caller_override(val: Option<&str>) -> Option<Caller> {
    match val.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("agent") => Some(Caller::Agent),
        Some("human") => Some(Caller::Human),
        _ => None,
    }
}

/// Resolve once per command at the CLI edge and pass the result down.
pub fn caller() -> Caller {
    if let Some(c) = parse_caller_override(std::env::var("DEVKIT_CALLER").ok().as_deref()) {
        return c;
    }
    let harness = HARNESS_SESSION_VARS
        .iter()
        .any(|v| std::env::var(v).is_ok_and(|s| !s.is_empty()));
    decide_caller(harness, std::io::stdin().is_terminal())
}
```

- [ ] **Step 4: Point devkit-locks at the shared list**

In `crates/devkit-locks/src/ident.rs`, delete the local `HARNESS_SESSION_VARS` and add:

```rust
pub use devkit_common::caller::HARNESS_SESSION_VARS;
```

`harness_candidates` keeps working unchanged. Confirm `cargo nextest run -p devkit-locks` still passes, including its identity tests.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo nextest run -p devkit-common -p devkit-locks`
Expected: PASS.

- [ ] **Step 6: Run the gate and commit**

```bash
cargo +nightly fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace --no-fail-fast
git add -A
git commit -m "feat(common): classify the caller as agent or human"
```

---

### Task 4: The shared resolver

**Files:**
- Create: `crates/devkit-common/src/required.rs`
- Modify: `crates/devkit-common/src/lib.rs` (add `pub mod required;`)

**Interfaces:**
- Consumes: `Caller` (Task 3); `Required`, `VariableDecl`, `Templates::defaults` (Task 1); `TaskConfig::required_args` (added here).
- Produces: `Missing { name: String, reason: Required }`; `binds(Required, Caller) -> bool`; `declared_required(&Config, Option<&str>, &str) -> Required`; `is_required(&Config, Option<&str>, &str, Caller) -> bool`; `missing_args(&Config, Option<&str>, &BTreeSet<String>, &BTreeMap<String, String>, Caller) -> Vec<Missing>`; `Missing::hint(&self) -> String`.

This task also adds `TaskConfig::required_args` to `crates/devkit-config/src/lib.rs`, because the resolver cannot be written without it.

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-common/src/required.rs`:

```rust
#[cfg(test)]
mod tests {
    use devkit_config::Config;

    use super::*;

    /// `msg` is defaulted, `ticket` is declared with no default, `plain` is a
    /// bare constant. The `commit` task reads all three.
    fn cfg(task_marking: &str) -> Config {
        let s = format!(
            "[defaults]\nworktree_root='w'\nbranch_prefix='x/'\nbaseline_ref='m'\n\
             [templates.variables]\n\
             plain = 'p'\n\
             msg = {{ default = 'wip', required = 'agents' }}\n\
             ticket = {{ required = 'always' }}\n\
             [tasks.commit]\n\
             run = ['git', 'commit', '-m', '{{{{ msg }}}}']\n\
             {task_marking}\n"
        );
        Config::parse(&s).unwrap()
    }

    fn names(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_marking_binds_only_the_caller_it_names() {
        let c = cfg("");
        assert!(is_required(&c, Some("commit"), "msg", Caller::Agent));
        assert!(!is_required(&c, Some("commit"), "msg", Caller::Human));
    }

    #[test]
    fn an_arg_with_no_default_is_required_of_everyone() {
        let c = cfg("");
        for caller in [Caller::Agent, Caller::Human] {
            assert!(is_required(&c, Some("commit"), "ticket", caller));
            assert!(is_required(&c, Some("commit"), "undeclared", caller));
        }
    }

    #[test]
    fn an_unmarked_defaulted_arg_is_required_of_nobody() {
        let c = cfg("");
        for caller in [Caller::Agent, Caller::Human] {
            assert!(!is_required(&c, Some("commit"), "plain", caller));
        }
    }

    #[test]
    fn a_task_marking_beats_the_variable_marking() {
        let c = cfg("required_args = { msg = 'never' }");
        assert!(!is_required(&c, Some("commit"), "msg", Caller::Agent));
        // ... but only for that task.
        assert!(is_required(&c, None, "msg", Caller::Agent));
    }

    #[test]
    fn a_task_marking_cannot_lower_the_derived_floor() {
        let c = cfg("required_args = { ticket = 'never' }");
        assert!(
            is_required(&c, Some("commit"), "ticket", Caller::Human),
            "never cannot relax an arg with no default"
        );
    }

    #[test]
    fn missing_args_reports_only_unsupplied_required_reads() {
        let c = cfg("");
        let given = BTreeMap::from([("ticket".to_string(), "T-1".to_string())]);
        let out = missing_args(
            &c,
            Some("commit"),
            &names(&["plain", "msg", "ticket"]),
            &given,
            Caller::Agent,
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].name, "msg");
        assert_eq!(out[0].reason, Required::Agents);
        assert_eq!(out[0].hint(), "--arg msg=... (required for agents)");
    }

    #[test]
    fn a_derived_requirement_hints_without_an_audience() {
        let c = cfg("");
        let out = missing_args(
            &c,
            Some("commit"),
            &names(&["ticket"]),
            &BTreeMap::new(),
            Caller::Human,
        );
        assert_eq!(out[0].hint(), "--arg ticket=...");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-common --lib required`
Expected: FAIL to compile, module does not exist.

- [ ] **Step 3: Add `required_args` to `TaskConfig`**

In `crates/devkit-config/src/lib.rs`, on `TaskConfig`, after `require_live`:

```rust
    /// Args this task requires beyond what `[templates.variables]` markings
    /// say, as name to marking. A task entry beats a variable entry, including
    /// relaxing one to `never`. Neither can lower the derived floor: an arg
    /// with no default is required whatever this says. Each name must be an
    /// arg the task reads.
    #[serde(default)]
    pub required_args: std::collections::BTreeMap<String, Required>,
```

Widen the sequence-shape check in `crates/devkit-ports/src/task.rs` so a sequence may set it, updating the message to name all three allowed keys. Regenerate the schema.

- [ ] **Step 4: Implement the resolver**

```rust
//! Whether a caller must supply a given `--arg`. The single door: every
//! `--arg` surface asks here, and nothing re-derives the answer. An earlier
//! attempt at this feature re-derived it inline for the task listing, and the
//! listing then disagreed with the check that refused the run.

use std::collections::{BTreeMap, BTreeSet};

use devkit_config::{Config, Required};

pub use crate::caller::Caller;

/// A required arg the caller did not supply. `reason` is the marking that
/// bound it, or `Never` when the derived rule did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    pub name: String,
    pub reason: Required,
}

impl Missing {
    /// The fragment an error joins. A caller-specific requirement names its
    /// audience, so an agent's report explains why the same command succeeds
    /// for the human reading it.
    pub fn hint(&self) -> String {
        let name = &self.name;
        match self.reason {
            Required::Never | Required::Always => format!("--arg {name}=..."),
            Required::Agents => format!("--arg {name}=... (required for agents)"),
            Required::Humans => format!("--arg {name}=... (required for humans)"),
        }
    }
}

/// Whether a marking binds this caller.
pub fn binds(r: Required, caller: Caller) -> bool {
    match r {
        Required::Never => false,
        Required::Always => true,
        Required::Agents => caller == Caller::Agent,
        Required::Humans => caller == Caller::Human,
    }
}

/// The marking in force: the task's entry, else the variable's, else none.
pub fn declared_required(cfg: &Config, task: Option<&str>, name: &str) -> Required {
    if let Some(t) = task.and_then(|t| cfg.tasks.get(t))
        && let Some(r) = t.required_args.get(name)
    {
        return *r;
    }
    cfg.templates
        .variables
        .get(name)
        .map(|d| d.required())
        .unwrap_or_default()
}

/// The derived rule ORed with the marking. The derived rule is a floor, so a
/// marking can only add a requirement, never remove one.
pub fn is_required(cfg: &Config, task: Option<&str>, name: &str, caller: Caller) -> bool {
    let has_default = cfg
        .templates
        .variables
        .get(name)
        .and_then(|d| d.default_value())
        .is_some();
    !has_default || binds(declared_required(cfg, task, name), caller)
}

/// The required names this run's templates read and the caller did not supply.
/// `reads` is what the surface will actually render; a name no template reads
/// is never asked for.
pub fn missing_args(
    cfg: &Config,
    task: Option<&str>,
    reads: &BTreeSet<String>,
    given: &BTreeMap<String, String>,
    caller: Caller,
) -> Vec<Missing> {
    reads
        .iter()
        .filter(|n| !given.contains_key(n.as_str()))
        .filter(|n| is_required(cfg, task, n, caller))
        .map(|n| Missing {
            name: n.clone(),
            reason: declared_required(cfg, task, n),
        })
        .collect()
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo nextest run -p devkit-common required`
Expected: PASS, all seven.

- [ ] **Step 6: Run the gate and commit**

```bash
DEVKIT_UPDATE_SCHEMA=1 cargo test --workspace schema
cargo +nightly fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace --no-fail-fast
git add -A
git commit -m "feat(common): resolve required args through one door"
```

---

### Task 5: Wire `devrun task`

**Files:**
- Modify: `crates/devkit-ports/src/task.rs` (`TaskArg`, `list`, `check_args`, `resolve`, `resolve_step`)
- Modify: `src/bin/devkit/run/mod.rs` (`cmd_task` and the listing path)
- Modify: `crates/devkit-mcp/src/devrun.rs` if it calls `task::list` (grep first)
- Modify: `src/bin/devkit/brief.rs` (calls `task::args_text`)
- Test: `tests/task_cmd.rs`

**Interfaces:**
- Consumes: `missing_args`, `Missing`, `Caller`, `caller()`.
- Produces: `task::list(cfg, caller)`, `task::required_args(cfg, name, caller) -> Result<BTreeSet<String>>`, `task::resolve(..., caller)`, `task::resolve_step(..., caller)`.

`Caller` threads through `resolve`/`resolve_step` rather than being read inside them, so the existing unit tests stay free of TTY and env.

- [ ] **Step 1: Write the failing integration tests**

Add to the `setup()` fixture in `tests/task_cmd.rs`, inside the existing `devkit.toml`:

```toml
[tasks.pinned-commit]
description = "commit that will not take the default scope"
run = ["git", "--msg={{ scope }}: {{ msg }}", "version"]
required_args = { scope = "agents" }

[tasks.typo-required]
run = ["git", "--msg={{ msg }}", "version"]
required_args = { nope = "always" }
```

The fixture already declares `[templates.variables] scope = "devkit"`, so `scope` is defaulted and would be optional without the marking.

Then:

```rust
#[test]
fn an_agents_marking_binds_a_run_with_no_terminal() {
    let dir = setup();
    // The test harness has no TTY, so the run classifies as an agent.
    let out = run_in(dir.path(), &["task", "pinned-commit", "--arg", "msg=fix", "--dry-run"]);
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--arg scope=..."), "{stderr}");
    assert!(stderr.contains("required for agents"), "the audience is named: {stderr}");
}

#[test]
fn the_same_run_as_a_human_takes_the_default() {
    let dir = setup();
    let state = dir.path().join("state");
    let out = devkit_run()
        .args(["task", "pinned-commit", "--arg", "msg=fix", "--dry-run"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("LOCALAPPDATA", &state)
        .env("USERPROFILE", dir.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_CALLER", "human")
        .output()
        .expect("run devkit run");
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("argv: git --msg=devkit: fix version"),
        "{out:?}"
    );
}

#[test]
fn the_listing_is_caller_relative() {
    let dir = setup();
    let agent = run_in(dir.path(), &["task"]);
    let stdout = String::from_utf8_lossy(&agent.stdout);
    let row = stdout
        .lines()
        .find(|l| l.contains("will not take the default scope"))
        .unwrap_or_else(|| panic!("row missing: {stdout}"));
    assert!(row.contains("scope") && !row.contains("[scope]"), "agent sees it required: {row}");
}

#[test]
fn a_required_args_name_the_task_never_reads_is_invalid() {
    let dir = setup();
    let out = run_in(dir.path(), &["task", "typo-required", "--arg", "msg=x"]);
    assert!(!out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("nope"), "{out:?}");

    let listing = run_in(dir.path(), &["task"]);
    let stdout = String::from_utf8_lossy(&listing.stdout);
    let row = stdout
        .lines()
        .find(|l| l.contains("typo-required"))
        .unwrap_or_else(|| panic!("row missing: {stdout}"));
    assert!(row.contains("invalid"), "{row}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --test task_cmd an_agents_marking_binds_a_run_with_no_terminal`
Expected: FAIL, the run succeeds because the marking is ignored.

- [ ] **Step 3: Thread `Caller` and delegate the check**

In `crates/devkit-ports/src/task.rs`:

```rust
/// The args task `name` cannot run without, for this caller. Errors when
/// `required_args` names something the task never reads: an entry that
/// silently guards nothing leaves the author believing it does.
pub fn required_args(cfg: &Config, name: &str, caller: Caller) -> Result<BTreeSet<String>> {
    let all = args(cfg, name)?;
    for n in declared_required_names(cfg, name)? {
        ensure!(
            all.contains(&n),
            "task `{name}` lists `{n}` in required_args but reads no such arg"
        );
    }
    Ok(all
        .into_iter()
        .filter(|n| devkit_common::required::is_required(cfg, Some(name), n, caller))
        .collect())
}

/// Names the task's own `required_args` declares, plus those of every command
/// task its steps name. A sub-task's marking holds however it is reached.
fn declared_required_names(cfg: &Config, name: &str) -> Result<BTreeSet<String>> {
    let t = cfg
        .tasks
        .get(name)
        .ok_or_else(|| anyhow!("unknown task `{name}` (run `devrun task` to list)"))?;
    let mut names: BTreeSet<String> = t.required_args.keys().cloned().collect();
    for step in &t.steps {
        if let Step::Task(r) = step
            && let Some(sub) = cfg.tasks.get(r)
        {
            names.extend(sub.required_args.keys().cloned());
        }
    }
    Ok(names)
}
```

Replace the requirement half of `check_args` (keep the "reads no variable" half exactly as it is):

```rust
    let reads = args(cfg, name)?;
    let missing = devkit_common::required::missing_args(cfg, Some(name), &reads, given, caller);
    ensure!(
        missing.is_empty(),
        "task `{name}` needs {}",
        missing.iter().map(Missing::hint).collect::<Vec<_>>().join(" ")
    );
```

`check_args`, `resolve` and `resolve_step` each take `caller: Caller` and pass it along.

- [ ] **Step 4: Make the listing caller-relative**

```rust
pub fn list(cfg: &Config, caller: Caller) -> Vec<TaskRow> {
```

and inside the map, replacing the inline derivation:

```rust
            // Both, not just `args`: a `required_args` naming something the
            // task never reads is a typo the listing has to show, the same way
            // it shows a template that will not compile.
            let resolved = match (args(cfg, name), required_args(cfg, name, caller)) {
                (Ok(all), Ok(required)) => Some((all, required)),
                _ => None,
            };
            TaskRow {
                name: name.clone(),
                kind: match (!t.run.is_empty(), !t.steps.is_empty(), resolved.is_some()) {
                    (true, false, true) => "command",
                    (false, true, true) => "sequence",
                    _ => "invalid",
                },
                app: t.app.clone().unwrap_or_else(|| "-".into()),
                args: resolved
                    .map(|(all, required)| {
                        all.into_iter()
                            .map(|name| TaskArg {
                                required: required.contains(&name),
                                name,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                description: t.description.clone().unwrap_or_default(),
            }
```

Update `TaskArg::required`'s doc comment to say it is this caller's answer.

- [ ] **Step 5: Resolve `Caller` at the CLI edge**

In `src/bin/devkit/run/mod.rs`, in `cmd_task` and wherever `task::list` is called, add `let caller = devkit_common::caller::caller();` and pass it. Do the same in `src/bin/devkit/brief.rs` and `crates/devkit-mcp/src/devrun.rs` if either calls `list`; MCP passes `Caller::Agent` outright, never `caller()`, because its stdin is a harness pipe.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo nextest run --test task_cmd`
Expected: PASS, all of them including the pre-existing ones.

- [ ] **Step 7: Run the gate and commit**

```bash
cargo +nightly fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace --no-fail-fast
git add -A
git commit -m "feat(run): enforce required args on devrun task"
```

---

### Task 6: Wire the three `issue` surfaces

**Files:**
- Modify: `src/bin/devkit/issue/review/mod.rs` (`parse_args`, plus a new `check_required`)
- Modify: `src/bin/devkit/issue/pr/create.rs:204`, `src/bin/devkit/issue/review/request.rs:100`, `src/bin/devkit/issue/review/finish.rs:182`
- Test: `tests/pr_create_render.rs` (existing, `gh`-backed via `tests/common/ghfake.rs`)

**Interfaces:**
- Consumes: `missing_args`, `Missing`, `caller()`, `Templates::declared`.
- Produces: `check_required(surface: &str, cfg: &Config, templates: &[&str], context_keys: &[&str], given: &BTreeMap<String, String>, caller: Caller) -> Result<()>`. It reads `cfg.templates` itself, so it takes no separate `Templates`.

A name a template reads that is neither declared nor a context key is *not* reported here. `parse_args` would reject the `--arg` that fixes it, so asking for it would be unanswerable. Those stay the strict-undefined render error they are today. Intersecting `reads` with `declared()` is what achieves that.

- [ ] **Step 1: Write the failing test**

In `tests/pr_create_render.rs`, following the existing fixture pattern in that file (it already sets up a repo, a `devkit.toml` and `ghfake` on PATH):

```rust
#[test]
fn issue_pr_refuses_a_required_arg_the_agent_did_not_pass() {
    let dir = setup();
    std::fs::write(
        dir.path().join("devkit.toml"),
        r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"

[templates]
pr_title = "{{ ticket }}: {{ input }}"

[templates.variables]
ticket = { default = "NONE", required = "agents" }
"#,
    )
    .unwrap();

    let out = run_issue(dir.path(), &["pr", "--pr-title", "add a thing"]);
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--arg ticket=..."), "{stderr}");
    assert!(stderr.contains("required for agents"), "{stderr}");
}

#[test]
fn a_name_only_another_surfaces_template_reads_does_not_bind() {
    let dir = setup();
    std::fs::write(
        dir.path().join("devkit.toml"),
        r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"

[templates]
review_finish = "{{ ticket }} {{ pr_url }}"

[templates.variables]
ticket = { default = "NONE", required = "agents" }
"#,
    )
    .unwrap();

    // `pr_title`/`pr_body` default to `{{ input }}` and read no `ticket`,
    // so the marking is irrelevant to this command.
    let out = run_issue(dir.path(), &["pr", "--pr-title", "add a thing"]);
    assert!(out.status.success(), "{out:?}");
}
```

If `run_issue` does not already exist in that file, add it mirroring `run_in` from `tests/task_cmd.rs`, invoking `CARGO_BIN_EXE_devkit` with a leading `issue` argument and the same isolated env.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --test pr_create_render issue_pr_refuses_a_required_arg`
Expected: FAIL, the command succeeds and renders `NONE`.

- [ ] **Step 3: Implement the shared check**

In `src/bin/devkit/issue/review/mod.rs`:

```rust
/// Refuse a required `--arg` this run's templates read and the caller did not
/// supply. `templates` are the ones this command can render, taken statically:
/// `issue pr` builds title and body in closures `ensure` may not call, and
/// gating on that would move the error after the push.
///
/// `reads` is intersected with the declared names because `parse_args` rejects
/// an `--arg` for anything else, so an undeclared name could not be supplied
/// even if it were asked for. Those stay the strict-undefined render error.
pub(crate) fn check_required(
    surface: &str,
    cfg: &Config,
    templates: &[&str],
    context_keys: &[&str],
    given: &BTreeMap<String, String>,
    caller: Caller,
) -> Result<()> {
    let declared = cfg.templates.declared();
    let mut reads = devkit_common::template::undeclared(templates)?;
    reads.retain(|n| declared.contains(n) && !context_keys.contains(&n.as_str()));
    let missing = devkit_common::required::missing_args(cfg, None, &reads, given, caller);
    anyhow::ensure!(
        missing.is_empty(),
        "{surface} needs {}",
        missing
            .iter()
            .map(devkit_common::required::Missing::hint)
            .collect::<Vec<_>>()
            .join(" ")
    );
    Ok(())
}
```

Change `parse_args`'s allowlist parameter from `&BTreeMap<String, String>` to `&BTreeSet<String>` and pass `tmpls.declared()`, so a variable declared only to be marked required is passable. Update its existing test accordingly.

- [ ] **Step 4: Call it from each surface**

In `src/bin/devkit/issue/pr/create.rs`, replacing lines 204-205:

```rust
    let caller = devkit_common::caller::caller();
    let mut vars = tmpls.defaults();
    let given = parse_args(&args.args, &tmpls.declared())?;
    check_required(
        "issue pr",
        &loaded.config,
        &[tmpls.pr_title(), tmpls.pr_body()],
        &["input", "pr_title", "issue", "slug", "branch", "apps", "prefix"],
        &given,
        caller,
    )?;
    vars.extend(given);
```

In `request.rs` at line 100, the same with `surface = "issue review request"`, `templates = &[tmpls.review_request()]`, and context keys `["input", "pr_url", "pr_title", "name", "slack_id", "issue", "slug", "branch", "apps", "prefix"]`.

In `finish.rs` at line 182, the same with `surface = "issue review finish"`, `templates = &[tmpls.review_finish()]`, and the `request` keys plus `"author"`.

Confirm each context-key list against `base_ctx` and the `with_fields` calls in that file before committing; a key missing from the list produces a spurious "needs --arg" for something devkit supplies.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo nextest run --test pr_create_render`
Expected: PASS.

- [ ] **Step 6: Run the gate and commit**

```bash
cargo +nightly fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace --no-fail-fast
git add -A
git commit -m "feat(issue): enforce required args on the pr and review surfaces"
```

---

### Task 7: Guard hint and documentation

**Files:**
- Modify: `crates/devkit-ports/src/guard/mod.rs` (`project_hit`, ~line 277)
- Modify: `docs/configuration.md` (the Tasks section and the `[templates]` section)
- Modify: `docs/commands.md` (the `devrun task` bullet)
- Modify: `AGENTS.md` (the `devkit-common` row, to name the two new modules)

**Interfaces:**
- Consumes: `task::required_args(cfg, name, caller)`.

- [ ] **Step 1: Write the failing test**

In the `tests` module of `crates/devkit-ports/src/guard/mod.rs`, mirroring the existing `devrun task stage --arg files=<files>` assertion:

```rust
#[test]
fn the_redirect_names_what_an_agent_must_pass() {
    // The guard always speaks to an agent, so an `agents` marking shows up in
    // the hint even though a human running the same task would not be asked.
    let p = project_with(
        "[templates.variables]\n\
         scope = { default = 'devkit', required = 'agents' }\n\
         [tasks.commit]\n\
         run = ['git', 'commit', '-m', '{{ scope }}: {{ msg }}']\n\
         guard = true\n",
    );
    let d = decide_for(&p, &["git", "commit", "-m", "x"]);
    assert!(
        reason(&d).contains("--arg scope=<scope>"),
        "the hint must name the agent-required arg: {}",
        reason(&d)
    );
}
```

Use whatever fixture helpers that module already has; `project_with`/`decide_for`/`reason` are placeholders for the existing ones, so read the neighbouring tests first and match them.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit-ports --lib the_redirect_names_what_an_agent_must_pass`
Expected: FAIL to compile, `required_args` now takes three arguments.

- [ ] **Step 3: Implement**

In `project_hit`:

```rust
        // The guard only ever fires because a coding agent acted, and its
        // stdin is a harness pipe, so the TTY says nothing. Name the set the
        // agent will actually be asked for.
        let usage: String = crate::task::required_args(&p.config, &name, Caller::Agent)
            .unwrap_or_default()
            .iter()
            .map(|a| format!(" --arg {a}=<{a}>"))
            .collect();
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run -p devkit-ports guard`
Expected: PASS.

- [ ] **Step 5: Document**

In `docs/configuration.md`, add a bullet to the Tasks section before the `require_live` bullet:

> - `required_args = { name = "always" | "agents" | "humans" | "never" }` requires an arg whatever `[templates.variables]` supplies. Declaring a default is otherwise the only way to make an arg optional, and it makes the arg omittable by every caller at once: an agent that drops `--arg msg=...` runs on the project constant, and nothing in the run says an arg went unsupplied. A task entry beats a variable entry of the same name, `never` included, so one task can opt out of a project-wide marking. Neither can lower the derived floor: an arg with no default is required whatever the marking says, and `required = "never"` on such an arg is a config error. A pinned name must be an arg the task reads, or the task is invalid and says so in the listing. A command task's markings carry to a sequence whose step names it.

In the `[templates]` section, document the table form of a variable entry, and state the contrast explicitly:

> An entry may be a bare string or a table: `msg = { default = "wip", required = "agents" }`. `default` is the fallback used when nothing else supplies the name. Note that this is a different thing from minijinja's `default` filter: the config key decides whether an arg is required, the filter does not, and `{{ msg | default("wip") }}` in a template still leaves `msg` required if nothing declares it.

Document `DEVKIT_CALLER` in the Environment section: `agent` or `human`, overriding the harness-variable and terminal detection.

In `docs/commands.md`, extend the `devrun task` bullet to say the args listing is relative to whoever runs it, and that `DEVKIT_CALLER=agent devrun task` shows an agent's view.

- [ ] **Step 6: Run the gate and commit**

```bash
cargo +nightly fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace --no-fail-fast && cargo test --workspace --doc
git add -A
git commit -m "docs: document required args and caller detection"
```

- [ ] **Step 7: Push and update the PR**

```bash
git push -u origin claude/eager-hawking-2kkefo
```

PR #83 already exists for this branch. Rewrite its title and body to match the delivered design: the old ones describe the discarded task-only version.

---

## Self-review notes

Spec coverage checked section by section. Config shape is Task 1, the `never` rejection is Task 2, caller identity is Task 3, the resolution rule and `required_args` are Task 4, per-surface enforcement is Tasks 5 and 6, errors and listings are Tasks 5 and 6, the guard is Task 7, crate placement is fixed by the File Structure table, migration and the doc contrast are Task 7.

Two things deliberately deferred to implementation rather than pinned here: the exact context-key list for each `issue` surface, which Task 6 Step 4 says to verify against `base_ctx` because getting it wrong produces a spurious error, and the guard module's fixture helper names, which Task 7 Step 1 says to read from neighbouring tests. Both are cases where writing a guess into the plan would be worse than naming the file to read.
