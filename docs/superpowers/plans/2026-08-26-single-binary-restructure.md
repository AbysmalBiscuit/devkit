# Single-binary restructure implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `devkit` and `devkitd` instead of eight executables, with the old command names preserved as hardlinks that dispatch on `argv[0]`.

**Architecture:** Each merged CLI's `Cli` struct becomes a variant of `devkit`'s top-level `Subcommand`, with its sources moved from `src/bin/<tool>/` to `src/bin/devkit/<tool>/`. At startup `devkit` reads the file stem of `argv[0]`; a stem in the shim table selects that tool's `clap::Command`, renamed to the shim name and parsed as a root command, so `issue status` and `devkit issue status` reach the same code with the same help and error text. `devkitd` is untouched.

**Tech Stack:** Rust 2024, clap 4 (derive), clap_complete 4, anyhow, fd-lock, tempfile.

**Spec:** `docs/superpowers/specs/2026-08-26-single-binary-restructure-design.md`

## Global constraints

- Edition 2024. The workspace already sets this; do not add per-crate `edition` keys.
- `cargo clippy --workspace --all-targets -- -D warnings` must pass. Zero-warning policy.
- `cargo test --workspace` is the merge gate and must be green at the end of every task.
- `cargo fmt --all` before every commit, on the stable toolchain.
- Conventional Commits for every commit message.
- Tests that spawn or reap processes poll for the expected state. Never sleep a fixed interval; a loaded Windows CI runner is slower than any constant you pick.
- Test scratch comes from `tempfile`. Never build a path by hand from `std::env::temp_dir()`. Bind the `TempDir` guard for as long as the path is used.
- `devkitd` keeps its own `[[bin]]` target and its `required-features = ["daemon"]` gate. No task in this plan modifies `src/bin/devkitd/`.
- `install_panic_hook` takes `&'static str`, so every name reaching it comes from a `const`, never from a runtime `String`.
- Never call `Cli::parse()` in a merged tool's code path. The shim path parses through `get_matches_from` and the direct path through the top-level `Cli`.

---

### Task 1: Shim table and argv[0] resolution

**Files:**
- Create: `src/bin/devkit/shim.rs`
- Modify: `src/bin/devkit/main.rs` (add `mod shim;`)

**Interfaces:**
- Consumes: nothing.
- Produces: `shim::Shim { name: &'static str, subcommand: &'static str }`, `shim::SHIMS: &[Shim]`, `shim::resolve(argv0: &str) -> Option<&'static Shim>`.

- [ ] **Step 1: Write the failing test**

Append to `src/bin/devkit/shim.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_a_bare_shim_name() {
        assert_eq!(resolve("portm").map(|s| s.subcommand), Some("ports"));
    }

    #[test]
    fn resolves_a_full_path_with_windows_extension() {
        assert_eq!(
            resolve(r"C:\Users\Lev\.cargo\bin\issue.exe").map(|s| s.subcommand),
            Some("issue")
        );
    }

    #[test]
    fn resolves_a_unix_path() {
        assert_eq!(
            resolve("/home/lev/.cargo/bin/devrun").map(|s| s.subcommand),
            Some("run")
        );
    }

    /// A hyphenated shim must not be mistaken for its prefix.
    #[test]
    fn devkit_mcp_is_its_own_shim() {
        assert_eq!(resolve("devkit-mcp").map(|s| s.subcommand), Some("mcp"));
    }

    /// The tool's own name, and anything unknown, fall through to `devkit`
    /// parsing rather than erroring.
    #[test]
    fn unknown_and_own_name_do_not_resolve() {
        assert!(resolve("devkit").is_none());
        assert!(resolve("devkit.exe").is_none());
        assert!(resolve("some-other-tool").is_none());
        assert!(resolve("").is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin devkit shim::`
Expected: FAIL to compile, `cannot find function resolve in this scope`.

- [ ] **Step 3: Write minimal implementation**

Prepend to `src/bin/devkit/shim.rs`:

```rust
//! The old command names, and the `devkit` subcommand each one selects.
//!
//! Dispatch reads `argv[0]` rather than `current_exe()`: the two disagree by
//! link type. `current_exe()` resolves a symlink to its target and reports
//! `devkit` for every shim, while `argv[0]` carries the name the caller
//! actually typed under both hardlinks and symlinks.

use std::path::Path;

pub struct Shim {
    /// The executable name on PATH.
    pub name: &'static str,
    /// The `devkit` subcommand it selects.
    pub subcommand: &'static str,
}

pub const SHIMS: &[Shim] = &[
    Shim { name: "issue", subcommand: "issue" },
    Shim { name: "devrun", subcommand: "run" },
    Shim { name: "portm", subcommand: "ports" },
    Shim { name: "lockm", subcommand: "locks" },
    Shim { name: "docm", subcommand: "docs" },
    Shim { name: "devkit-mcp", subcommand: "mcp" },
];

/// The shim `argv0` names, if any. Accepts a bare name or a full path, with or
/// without a `.exe` extension.
pub fn resolve(argv0: &str) -> Option<&'static Shim> {
    let stem = Path::new(argv0).file_stem()?.to_str()?;
    SHIMS.iter().find(|s| s.name == stem)
}
```

Add `mod shim;` to `src/bin/devkit/main.rs` beside the existing `mod auth;`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin devkit shim::`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add src/bin/devkit/shim.rs src/bin/devkit/main.rs
git commit -m "feat(devkit): add the shim name table and argv0 resolution"
```

---

### Task 2: Migrate `portm` to `devkit ports`, with shim dispatch

This is the first migration and it establishes the pattern every later one follows. `portm` is the smallest CLI that has an `Option<Cmd>` default, so it exercises the bare-shim path too.

**Files:**
- Create: `src/bin/devkit/ports.rs` (moved from `src/bin/portm.rs`)
- Create: `tests/common/shimtest.rs`
- Delete: `src/bin/portm.rs`
- Modify: `src/bin/devkit/main.rs`
- Modify: `tests/completions.rs`, `tests/cli_version.rs`, `tests/cli_ergonomics.rs`

**Interfaces:**
- Consumes: `shim::resolve`, `shim::SHIMS` from Task 1.
- Produces:
  - `ports::PortsCli` (the former `portm` `Cli`, now `#[derive(clap::Args)]`)
  - `ports::run(cli: PortsCli) -> anyhow::Result<()>`
  - `dispatch_shim(shim: &shim::Shim, args: Vec<std::ffi::OsString>) -> anyhow::Result<()>` in `main.rs`
  - `shimtest::linked(name: &str) -> (tempfile::TempDir, std::path::PathBuf)` — hardlinks the built `devkit` under `name` in a fresh temp dir and returns the guard plus the link path. The guard must be bound by the caller for as long as the path is used.

- [ ] **Step 1: Write the failing test**

Create `tests/common/shimtest.rs`:

```rust
//! Run the built `devkit` under a shim name, the way an installed hardlink does.

use std::path::PathBuf;

/// Hardlink the built `devkit` as `name` inside a fresh temp dir. Returns the
/// guard and the link path; bind the guard for as long as the path is used, or
/// the directory is gone before the test runs anything.
pub fn linked(name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let link = dir.path().join(exe);
    std::fs::hard_link(env!("CARGO_BIN_EXE_devkit"), &link)
        .unwrap_or_else(|e| panic!("hardlink devkit as {name}: {e}"));
    (dir, link)
}
```

Create `tests/shim_dispatch.rs`:

```rust
mod common;

use common::shimtest;
use std::process::Command;

#[test]
fn portm_shim_parses_portm_arguments() {
    let (_dir, link) = shimtest::linked("portm");
    let out = Command::new(&link)
        .args(["--help"])
        .output()
        .expect("spawn portm shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "portm --help exited non-zero: {text}");
    assert!(
        text.contains("Port registry"),
        "shim should show portm's own about text: {text}"
    );
    assert!(
        !text.contains("Configure and diagnose"),
        "shim must not show devkit's about text: {text}"
    );
}

/// `portm` with no subcommand shows status, which needs no project to exit 0
/// against an empty registry.
#[test]
fn portm_shim_defaults_to_status() {
    let (_dir, link) = shimtest::linked("portm");
    let state = tempfile::tempdir().expect("state dir");
    let out = Command::new(&link)
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("spawn bare portm shim");
    assert!(
        out.status.success(),
        "bare portm should run status: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn portm_shim_reports_the_package_version() {
    let (_dir, link) = shimtest::linked("portm");
    let out = Command::new(&link)
        .arg("--version")
        .output()
        .expect("spawn portm --version");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "portm --version should print {}, got: {text}",
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn devkit_ports_reaches_the_same_command() {
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(["ports", "--help"])
        .output()
        .expect("spawn devkit ports --help");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("Port registry"),
        "devkit ports should show portm's about text: {text}"
    );
}
```

Add `pub mod shimtest;` to `tests/common/mod.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test shim_dispatch`
Expected: FAIL. `portm_shim_parses_portm_arguments` fails because the hardlinked binary still prints devkit's own help; `devkit_ports_reaches_the_same_command` fails because there is no `ports` subcommand.

- [ ] **Step 3: Write minimal implementation**

Move the file and adapt it:

```bash
git mv src/bin/portm.rs src/bin/devkit/ports.rs
```

In `src/bin/devkit/ports.rs`:
- Change `#[derive(Parser)] #[command(version, about = "Port registry for local dev servers")] struct Cli` to `#[derive(clap::Args)] #[command(about = "Port registry for local dev servers")] pub struct PortsCli`. Drop `version` from the attribute; the shim path sets it explicitly and the `devkit` path inherits from the root.
- Make the `dir` and `cmd` fields `pub`.
- Rename `fn main() -> Result<()>` to `pub fn run(cli: PortsCli) -> Result<()>`, delete its first three lines (`install_panic_hook`, `migrate_legacy_state`, `Cli::parse()`), and take `cli` as the parameter instead.
- Change the completions arm to take the name from the caller:

```rust
Cmd::Completions { shell } => {
    crate::emit_completions(shell, "ports", "portm");
}
```

In `src/bin/devkit/main.rs`:

```rust
mod ports;
mod shim;

use clap::FromArgMatches;
use std::ffi::OsString;

/// Build a tool's `Command` as a root command under `shim_name`. Subcommands do
/// not inherit `version` from the root, so set it explicitly or `--version`
/// through a shim reports nothing.
fn shim_command(subcommand: &str, shim_name: &'static str) -> clap::Command {
    Cli::command()
        .find_subcommand(subcommand)
        .unwrap_or_else(|| panic!("no `{subcommand}` subcommand"))
        .clone()
        .name(shim_name)
        .bin_name(shim_name)
        .version(env!("CARGO_PKG_VERSION"))
}

/// Emit a completion script for a tool under the name it is invoked as: the
/// shim name when installed as one, `devkit <sub>` otherwise.
fn emit_completions(shell: Shell, subcommand: &str, shim_name: &'static str) {
    let mut cmd = shim_command(subcommand, shim_name);
    clap_complete::generate(shell, &mut cmd, shim_name, &mut std::io::stdout());
}

fn dispatch_shim(s: &'static shim::Shim, args: Vec<OsString>) -> Result<()> {
    let matches = shim_command(s.subcommand, s.name).get_matches_from(args);
    match s.subcommand {
        "ports" => ports::run(ports::PortsCli::from_arg_matches(&matches)?),
        other => unreachable!("shim `{}` selects unknown subcommand `{other}`", s.name),
    }
}
```

Add the variant to `Cmd`:

```rust
    /// Port registry for local dev servers (also installed as `portm`).
    Ports(ports::PortsCli),
```

and the arm to the existing `match`:

```rust
        Cmd::Ports(c) => ports::run(c),
```

Replace `fn main`'s opening so it branches before parsing:

```rust
fn main() -> Result<()> {
    let args: Vec<OsString> = std::env::args_os().collect();
    let argv0 = args.first().map(|a| a.to_string_lossy().into_owned());
    if let Some(s) = argv0.as_deref().and_then(shim::resolve) {
        devkit_common::report::install_panic_hook(s.name);
        devkit_common::paths::migrate_legacy_state();
        return dispatch_shim(s, args);
    }
    devkit_common::report::install_panic_hook("devkit");
    devkit_common::paths::migrate_legacy_state();
    let cli = Cli::parse();
    // ... existing match on cli.cmd
}
```

Remove the `[[bin]]` entry for `portm` if one exists in `Cargo.toml`. It is an implicit target from `src/bin/portm.rs`, so deleting the file is enough.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test shim_dispatch`
Expected: PASS, 4 tests.

- [ ] **Step 5: Repoint the existing binary-path tests**

In `tests/completions.rs`, `tests/cli_version.rs`, and `tests/cli_ergonomics.rs`, `env!("CARGO_BIN_EXE_portm")` no longer compiles. Replace each `portm` case with a `shimtest::linked("portm")` call, binding the guard:

```rust
#[test]
fn portm_emits_completions() {
    let (_dir, link) = shimtest::linked("portm");
    completions_contain_name("portm", link.to_str().expect("utf-8 link path"));
}
```

In `tests/cli_ergonomics.rs`, `run()` matches on a `bin` name to pick a `CARGO_BIN_EXE_*`. Change the `"portm"` arm to resolve through a link. Because `linked` returns a guard the caller must hold, give `run` a `&Path` exe parameter rather than a name, and let each test create its own link.

- [ ] **Step 6: Run the full gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(devkit): merge portm into devkit ports behind an argv0 shim"
```

---

### Task 3: Migrate `lockm` to `devkit locks`

**Files:**
- Create: `src/bin/devkit/locks.rs` (moved from `src/bin/lockm.rs`)
- Delete: `src/bin/lockm.rs`
- Modify: `src/bin/devkit/main.rs`, `tests/shim_dispatch.rs`, `tests/completions.rs`, `tests/cli_version.rs`, `tests/cli_ergonomics.rs`, `tests/locks.rs`

**Interfaces:**
- Consumes: `shim_command`, `emit_completions`, `dispatch_shim`, `shimtest::linked`.
- Produces: `locks::LocksCli`, `locks::run(cli: LocksCli) -> anyhow::Result<()>`.

`lockm` is the one merged tool on a hot path: `hooks/hooks.json` runs `lockm hook pretooluse` on every `Edit`, `Write`, and `NotebookEdit`.

- [ ] **Step 1: Write the failing test**

Append to `tests/shim_dispatch.rs`:

```rust
#[test]
fn lockm_shim_parses_lockm_arguments() {
    let (_dir, link) = shimtest::linked("lockm");
    let out = Command::new(&link)
        .arg("--help")
        .output()
        .expect("spawn lockm shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "lockm --help exited non-zero: {text}");
    assert!(
        text.contains("acquire"),
        "shim should list lockm's own subcommands: {text}"
    );
}

/// The PreToolUse hook runs this on every edit; it must keep working through a
/// shim and must not require a terminal.
#[test]
fn lockm_shim_runs_the_pretooluse_hook() {
    let (_dir, link) = shimtest::linked("lockm");
    let state = tempfile::tempdir().expect("state dir");
    let out = Command::new(&link)
        .args(["hook", "pretooluse"])
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("spawn lockm hook pretooluse");
    assert!(
        out.status.success(),
        "lockm hook pretooluse should exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test shim_dispatch lockm_`
Expected: FAIL. The hardlinked binary prints devkit's help, which does not contain `acquire`.

- [ ] **Step 3: Write minimal implementation**

```bash
git mv src/bin/lockm.rs src/bin/devkit/locks.rs
```

In `src/bin/devkit/locks.rs`: rename `Cli` to `pub struct LocksCli`, change `#[derive(Parser)]` to `#[derive(clap::Args)]`, drop `version` from the `#[command(...)]` attribute, make its fields `pub`, rename `fn main` to `pub fn run(cli: LocksCli) -> Result<()>` with the three preamble lines removed, and change the completions arm to `crate::emit_completions(shell, "locks", "lockm");`.

In `src/bin/devkit/main.rs`: add `mod locks;`, add the `Cmd` variant

```rust
    /// Advisory file locks across sessions (also installed as `lockm`).
    Locks(locks::LocksCli),
```

the top-level arm `Cmd::Locks(c) => locks::run(c),`, and the `dispatch_shim` arm

```rust
        "locks" => locks::run(locks::LocksCli::from_arg_matches(&matches)?),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test shim_dispatch lockm_`
Expected: PASS, 2 tests.

- [ ] **Step 5: Repoint the existing binary-path tests**

`tests/locks.rs`, `tests/completions.rs`, `tests/cli_version.rs`, and `tests/cli_ergonomics.rs` each reference `CARGO_BIN_EXE_lockm`. Replace with `shimtest::linked("lockm")`, binding the guard for the life of the path.

- [ ] **Step 6: Run the full gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(devkit): merge lockm into devkit locks"
```

---

### Task 4: Migrate `docm` to `devkit docs`

**Files:**
- Create: `src/bin/devkit/docs.rs` (moved from `src/bin/docm.rs`)
- Delete: `src/bin/docm.rs`
- Modify: `src/bin/devkit/main.rs`, `tests/shim_dispatch.rs`, `tests/docm_cli.rs`, `tests/docm_reporting.rs`, `tests/completions.rs`, `tests/cli_version.rs`

**Interfaces:**
- Consumes: `shim_command`, `emit_completions`, `dispatch_shim`, `shimtest::linked`.
- Produces: `docs::DocsCli`, `docs::run(cli: DocsCli) -> anyhow::Result<()>`.

`docm`'s `main` calls `install_panic_hook` but not `migrate_legacy_state`, and it guards a setup step behind `if !matches!(cli.cmd, Cmd::Completions { .. })`. Keep that guard inside `run`.

- [ ] **Step 1: Write the failing test**

Append to `tests/shim_dispatch.rs`:

```rust
#[test]
fn docm_shim_parses_docm_arguments() {
    let (_dir, link) = shimtest::linked("docm");
    let out = Command::new(&link)
        .arg("--help")
        .output()
        .expect("spawn docm shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "docm --help exited non-zero: {text}");
    assert!(
        text.contains("prune"),
        "shim should list docm's own subcommands: {text}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test shim_dispatch docm_`
Expected: FAIL, devkit's help does not contain `prune`.

- [ ] **Step 3: Write minimal implementation**

```bash
git mv src/bin/docm.rs src/bin/devkit/docs.rs
```

In `src/bin/devkit/docs.rs`: rename `Cli` to `pub struct DocsCli`, `#[derive(Parser)]` to `#[derive(clap::Args)]`, drop `version` from `#[command(...)]`, make fields `pub`, rename `fn main` to `pub fn run(cli: DocsCli) -> Result<()>` dropping the `install_panic_hook` and `Cli::parse()` lines, keep the `if !matches!(cli.cmd, Cmd::Completions { .. })` guard, and change the completions arm to `crate::emit_completions(shell, "docs", "docm");`.

In `src/bin/devkit/main.rs`: add `mod docs;`, the variant

```rust
    /// Version-correct library checkouts (also installed as `docm`).
    Docs(docs::DocsCli),
```

the top-level arm `Cmd::Docs(c) => docs::run(c),`, and the `dispatch_shim` arm

```rust
        "docs" => docs::run(docs::DocsCli::from_arg_matches(&matches)?),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test shim_dispatch docm_`
Expected: PASS.

- [ ] **Step 5: Repoint the existing binary-path tests**

`tests/docm_cli.rs` and `tests/docm_reporting.rs` are the two largest consumers of `CARGO_BIN_EXE_docm`. Add a module-level helper in each that creates one link per test and returns the guard, rather than a single shared link, so no test depends on another's temp dir surviving.

- [ ] **Step 6: Run the full gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(devkit): merge docm into devkit docs"
```

---

### Task 5: Migrate `devrun` to `devkit run`

**Files:**
- Create: `src/bin/devkit/run/` from `src/bin/devrun/` (`main.rs` becomes `mod.rs`, `baseline.rs` and `config.rs` move unchanged)
- Delete: `src/bin/devrun/`
- Modify: `src/bin/devkit/main.rs`, `tests/shim_dispatch.rs`, `tests/devrun_down_gate.rs`, `tests/down_ports.rs`, `tests/task_cmd.rs`, `tests/lifecycle.rs`, `tests/supervision.rs`, `tests/app_url.rs`, `tests/completions.rs`, `tests/cli_version.rs`, `tests/cli_ergonomics.rs`

**Interfaces:**
- Consumes: `shim_command`, `emit_completions`, `dispatch_shim`, `shimtest::linked`.
- Produces: `run::RunCli`, `run::run(cli: RunCli) -> anyhow::Result<()>`.

`devrun` carries two TTY gates that must keep behaving identically through a shim: cross-worktree `down` (`cmd_down`) refuses without an interactive terminal, and `reap` refuses with no bypass at all. Dispatch changes nothing about how stdin is inherited, but the tests below prove it.

- [ ] **Step 1: Write the failing test**

Append to `tests/shim_dispatch.rs`:

```rust
#[test]
fn devrun_shim_parses_devrun_arguments() {
    let (_dir, link) = shimtest::linked("devrun");
    let out = Command::new(&link)
        .arg("--help")
        .output()
        .expect("spawn devrun shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "devrun --help exited non-zero: {text}");
    assert!(
        text.contains("supervise") || text.contains("baseline"),
        "shim should list devrun's own subcommands: {text}"
    );
}

/// `reap` is TTY-gated with no bypass. A shim inherits the same non-terminal
/// stdin a hook or agent has, so it must still refuse.
#[test]
fn devrun_shim_still_refuses_reap_without_a_terminal() {
    let (_dir, link) = shimtest::linked("devrun");
    let state = tempfile::tempdir().expect("state dir");
    let out = Command::new(&link)
        .arg("reap")
        .stdin(std::process::Stdio::null())
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("spawn devrun reap");
    assert!(
        !out.status.success(),
        "reap through a shim must refuse without a TTY"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test shim_dispatch devrun_`
Expected: FAIL on the help assertion.

- [ ] **Step 3: Write minimal implementation**

```bash
git mv src/bin/devrun src/bin/devkit/run
git mv src/bin/devkit/run/main.rs src/bin/devkit/run/mod.rs
```

In `src/bin/devkit/run/mod.rs`: rename `Cli` to `pub struct RunCli`, `#[derive(Parser)]` to `#[derive(clap::Args)]`, drop `version` from `#[command(...)]`, make fields `pub`, rename `fn main` to `pub fn run(cli: RunCli) -> Result<()>` with the preamble removed, and change the completions arm to `crate::emit_completions(shell, "run", "devrun");`.

The `mod baseline;` and `mod config;` declarations at the top of the file stay as they are; both files moved with the directory.

In `src/bin/devkit/main.rs`: add `mod run;`, the variant

```rust
    /// Supervised dev servers and canned project tasks (also installed as `devrun`).
    Run(run::RunCli),
```

the top-level arm `Cmd::Run(c) => run::run(c),`, and the `dispatch_shim` arm

```rust
        "run" => run::run(run::RunCli::from_arg_matches(&matches)?),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test shim_dispatch devrun_`
Expected: PASS, 2 tests.

- [ ] **Step 5: Repoint the existing binary-path tests**

Seven test files reference `CARGO_BIN_EXE_devrun`. Several spawn long-lived servers, so each must hold its `TempDir` guard until after the server is stopped. Do not hoist the link into a `static` or `OnceLock`: the guard's `Drop` is what removes the directory, and a `static` never drops.

- [ ] **Step 6: Run the full gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green. `tests/supervision.rs` is Unix-only and spawns real children; give it the full run rather than a filter.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(devkit): merge devrun into devkit run"
```

---

### Task 6: Migrate `issue` to `devkit issue`

**Files:**
- Create: `src/bin/devkit/issue/` from `src/bin/issue/` (`main.rs` becomes `mod.rs`, all sixteen other files move unchanged)
- Delete: `src/bin/issue/`
- Modify: `src/bin/devkit/main.rs`, `tests/shim_dispatch.rs`, `tests/cli_ergonomics.rs`, `tests/completions.rs`, `tests/cli_version.rs`, `tests/brief_pins.rs`

**Interfaces:**
- Consumes: `shim_command`, `emit_completions`, `dispatch_shim`, `shimtest::linked`.
- Produces: `issue::IssueCli`, `issue::run(cli: IssueCli) -> anyhow::Result<()>`.

`issue` declares `cmd: Option<Cmd>` and falls through to `status::run` when it is `None` (`src/bin/issue/main.rs:410`). That default is the behavior most users hit, so it gets its own test.

This task collides with the separate `feat/issue-sync-includes` branch, which adds a subcommand to the same `Cmd` enum. Rebase onto it before starting this task, not after; the merged file layout would otherwise have to be re-applied by hand.

- [ ] **Step 1: Write the failing test**

Append to `tests/shim_dispatch.rs`:

```rust
#[test]
fn issue_shim_parses_issue_arguments() {
    let (_dir, link) = shimtest::linked("issue");
    let out = Command::new(&link)
        .arg("--help")
        .output()
        .expect("spawn issue shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "issue --help exited non-zero: {text}");
    assert!(
        text.contains("checkout-pr"),
        "shim should list issue's own subcommands: {text}"
    );
}

/// Bare `issue` runs `status`. Outside a devkit project that reports nothing
/// and exits 0, which is what makes it a safe default.
#[test]
fn issue_shim_defaults_to_status() {
    let (_dir, link) = shimtest::linked("issue");
    let state = tempfile::tempdir().expect("state dir");
    let project = tempfile::tempdir().expect("project dir");
    let out = Command::new(&link)
        .current_dir(project.path())
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("spawn bare issue shim");
    assert!(
        out.status.success(),
        "bare issue should run status: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test shim_dispatch issue_`
Expected: FAIL on the help assertion.

- [ ] **Step 3: Write minimal implementation**

```bash
git mv src/bin/issue src/bin/devkit/issue
git mv src/bin/devkit/issue/main.rs src/bin/devkit/issue/mod.rs
```

In `src/bin/devkit/issue/mod.rs`: rename `Cli` to `pub struct IssueCli`, `#[derive(Parser)]` to `#[derive(clap::Args)]`, drop `version` from `#[command(...)]`, make fields `pub`, rename `fn main` to `pub fn run(cli: IssueCli) -> Result<()>` with the preamble removed, and change the completions arm to `crate::emit_completions(shell, "issue", "issue");`.

Every `crate::` path inside the moved subtree now resolves to the `devkit` bin root rather than the old `issue` bin root. The submodules refer to each other as `crate::setup::…`, `crate::slug::…`, and so on (for example `src/bin/issue/checkout.rs:423` calls `crate::setup::backfill_includes`). Rewrite those to `crate::issue::…`:

```bash
rg -l 'crate::' src/bin/devkit/issue/ | xargs sed -i 's/\bcrate::/crate::issue::/g'
sed -i 's/crate::issue::issue::/crate::issue::/g' src/bin/devkit/issue/*.rs src/bin/devkit/issue/*/*.rs
```

Then read the diff before committing: the second `sed` exists to undo double-prefixing from any pre-existing `crate::issue::` path, and a blind pass over generated text is exactly where a silent breakage hides.

In `src/bin/devkit/main.rs`: add `mod issue;`, the variant

```rust
    /// Issue worktrees, PR triage, and dashboards (also installed as `issue`).
    Issue(issue::IssueCli),
```

the top-level arm `Cmd::Issue(c) => issue::run(c),`, and the `dispatch_shim` arm

```rust
        "issue" => issue::run(issue::IssueCli::from_arg_matches(&matches)?),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test shim_dispatch issue_`
Expected: PASS, 2 tests.

- [ ] **Step 5: Repoint the existing binary-path tests**

`tests/cli_ergonomics.rs` exercises `issue` most heavily. Its `run()` helper already takes a `bin` name; give it a `&Path` instead, as in Task 2.

- [ ] **Step 6: Run the full gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(devkit): merge issue into devkit issue"
```

---

### Task 7: Migrate `devkit-mcp` to `devkit mcp`

**Files:**
- Create: `src/bin/devkit/mcp.rs`
- Delete: `src/bin/devkit-mcp/main.rs`
- Modify: `src/bin/devkit/main.rs`, `tests/shim_dispatch.rs`, `tests/mcp.rs`, `tests/cli_version.rs`

**Interfaces:**
- Consumes: `dispatch_shim`, `shimtest::linked`.
- Produces: `mcp::McpCli` (a unit `Args` struct with no fields), `mcp::run(cli: McpCli) -> anyhow::Result<()>`.

`devkit-mcp` does not use clap today: it hand-checks `--version` from `args().nth(1)` and otherwise reads a JSON-RPC stream on stdin. Giving it an empty `Args` struct puts `--version` and `--help` on the standard path and changes nothing else. `.mcp.json` keeps invoking `devkit-mcp`, which is why the shim stays.

- [ ] **Step 1: Write the failing test**

Append to `tests/shim_dispatch.rs`:

```rust
#[test]
fn devkit_mcp_shim_reports_the_package_version() {
    let (_dir, link) = shimtest::linked("devkit-mcp");
    let out = Command::new(&link)
        .arg("--version")
        .output()
        .expect("spawn devkit-mcp --version");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "devkit-mcp --version should print {}, got: {text}",
        env!("CARGO_PKG_VERSION")
    );
}

/// The server answers a JSON-RPC request on stdin when run through the shim,
/// which is how `.mcp.json` starts it.
#[test]
fn devkit_mcp_shim_serves_a_request() {
    use std::io::Write;
    let (_dir, link) = shimtest::linked("devkit-mcp");
    let state = tempfile::tempdir().expect("state dir");
    let mut child = Command::new(&link)
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn devkit-mcp shim");
    let req = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let mut stdin = child.stdin.take().expect("stdin");
    stdin.write_all(req).expect("write request");
    stdin.write_all(b"\n").expect("write newline");
    drop(stdin);
    let out = child.wait_with_output().expect("wait for mcp shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("\"id\":1"),
        "shim should answer the request: {text}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test shim_dispatch devkit_mcp_`
Expected: FAIL. `--version` prints devkit's version string under the `devkit` name, and the JSON-RPC test gets clap's "unexpected argument" path or an empty stdout.

- [ ] **Step 3: Write minimal implementation**

Create `src/bin/devkit/mcp.rs`:

```rust
//! The stdio MCP server. `.mcp.json` starts it as `devkit-mcp`, so the shim
//! name stays even though the code now lives in `devkit`.

use anyhow::Result;
use std::io::{BufReader, Write};

#[derive(clap::Args)]
#[command(about = "Serve the devkit MCP tools over stdio")]
pub struct McpCli {}

pub fn run(_cli: McpCli) -> Result<()> {
    let ctx = devkit_mcp::ServerCtx {
        default_holder: devkit_mcp::mint_holder(),
    };
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    devkit_mcp::run(&mut reader, &mut writer, &ctx)?;
    writer.flush()?;
    Ok(())
}
```

```bash
git rm -r src/bin/devkit-mcp
```

In `src/bin/devkit/main.rs`: add `mod mcp;`, the variant

```rust
    /// Serve the devkit MCP tools over stdio (also installed as `devkit-mcp`).
    Mcp(mcp::McpCli),
```

the top-level arm `Cmd::Mcp(c) => mcp::run(c),`, and the `dispatch_shim` arm

```rust
        "mcp" => mcp::run(mcp::McpCli::from_arg_matches(&matches)?),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test shim_dispatch devkit_mcp_`
Expected: PASS, 2 tests.

- [ ] **Step 5: Repoint `tests/mcp.rs`**

Replace `CARGO_BIN_EXE_devkit-mcp` with `shimtest::linked("devkit-mcp")`. Note that `env!` cannot name a hyphenated variable directly, so the existing file already works around this; remove that workaround with the link.

- [ ] **Step 6: Run the full gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green. At this point `cargo build --release` produces exactly `devkit` and `devkitd`.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(devkit): merge devkit-mcp into devkit mcp"
```

---

### Task 8: `devkit install-links`

> The shipped implementation diverges from the code below. This section's
> `is_devkit_binary` accepted any program that printed its own name first,
> which is what clap does by default, so it would have deleted an unrelated
> binary sitting at one of these names. The replacement anchors the version
> line to the shim being judged, requires a non-empty subcommand set to appear
> in `--help`, and adds a marker probe. `Outcome::SkippedForeign` carries the
> reason. Read `src/bin/devkit/links.rs` for what exists; the reasoning is in
> the ledger under Task 8.

**Files:**
- Create: `src/bin/devkit/links.rs`
- Create: `tests/install_links.rs`
- Modify: `src/bin/devkit/main.rs`

**Interfaces:**
- Consumes: `shim::SHIMS`, `shim::Shim`.
- Produces:
  - `links::Outcome { Created, Replaced, AlreadyLinked, SkippedForeign, Failed(String) }`
  - `links::link_all(exe: &Path, dir: &Path, force: bool) -> Vec<(&'static str, Outcome)>`
  - `links::is_devkit_binary(path: &Path) -> bool`
  - `links::same_file(a: &Path, b: &Path) -> bool`
  - `links::InstallLinksArgs { force: bool }` and `links::run(args: InstallLinksArgs) -> anyhow::Result<()>`

- [ ] **Step 1: Write the failing test**

Create `tests/install_links.rs`:

```rust
mod common;

use common::shimtest;
use std::process::Command;

/// Run `install-links` against a throwaway directory holding a copy of the
/// built binary, so the test never touches the real CARGO_HOME.
fn staged() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let exe = dir.path().join(if cfg!(windows) { "devkit.exe" } else { "devkit" });
    std::fs::copy(env!("CARGO_BIN_EXE_devkit"), &exe).expect("stage devkit");
    (dir, exe)
}

fn shim_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    dir.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    })
}

#[test]
fn creates_every_shim() {
    let (dir, exe) = staged();
    let out = Command::new(&exe)
        .arg("install-links")
        .output()
        .expect("spawn install-links");
    assert!(
        out.status.success(),
        "install-links failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for name in ["issue", "devrun", "portm", "lockm", "docm", "devkit-mcp"] {
        assert!(
            shim_path(dir.path(), name).exists(),
            "install-links did not create {name}"
        );
    }
}

/// The upgrade path: a real devkit binary already sits at a shim name.
#[test]
fn replaces_an_existing_devkit_binary() {
    let (dir, exe) = staged();
    let stale = shim_path(dir.path(), "portm");
    std::fs::copy(env!("CARGO_BIN_EXE_devkit"), &stale).expect("stage a stale portm");
    let out = Command::new(&exe)
        .arg("install-links")
        .output()
        .expect("spawn install-links");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("portm"), "should report portm: {text}");
    assert!(
        shimtest::same_inode(&exe, &stale),
        "portm should now be a hardlink to devkit"
    );
}

/// A name held by something else is never destroyed.
#[test]
fn leaves_a_foreign_binary_alone() {
    let (dir, exe) = staged();
    let foreign = shim_path(dir.path(), "issue");
    std::fs::write(&foreign, b"#!/bin/sh\necho not devkit\n").expect("write foreign issue");
    let out = Command::new(&exe)
        .arg("install-links")
        .output()
        .expect("spawn install-links");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&foreign).expect("foreign still readable"),
        b"#!/bin/sh\necho not devkit\n",
        "a foreign file at a shim name must not be replaced"
    );
    assert!(text.contains("skipped"), "should report the skip: {text}");
}

#[test]
fn force_takes_over_a_foreign_binary() {
    let (dir, exe) = staged();
    let foreign = shim_path(dir.path(), "issue");
    std::fs::write(&foreign, b"#!/bin/sh\necho not devkit\n").expect("write foreign issue");
    let out = Command::new(&exe)
        .args(["install-links", "--force"])
        .output()
        .expect("spawn install-links --force");
    assert!(out.status.success());
    assert!(
        shimtest::same_inode(&exe, &foreign),
        "--force should claim the name"
    );
}
```

Add to `tests/common/shimtest.rs`:

```rust
/// Whether two paths name the same file on disk. A hardlink shares an inode
/// with its target on Unix and a file index on Windows.
pub fn same_inode(a: &std::path::Path, b: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
            return false;
        };
        ma.dev() == mb.dev() && ma.ino() == mb.ino()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
            return false;
        };
        ma.file_size() == mb.file_size() && ma.creation_time() == mb.creation_time()
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test install_links`
Expected: FAIL, clap reports `unrecognized subcommand 'install-links'` on all four tests.

- [ ] **Step 3: Write minimal implementation**

Create `src/bin/devkit/links.rs`:

```rust
//! Create the old command names as hardlinks beside the running executable.
//!
//! A hardlink is a second name for the same inode: no disk cost, no exec-time
//! indirection, and no symlink privilege requirement on Windows. It also keeps
//! `argv[0]` reporting the name the caller typed, which is what dispatch reads.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::shim::SHIMS;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Created,
    Replaced,
    AlreadyLinked,
    SkippedForeign,
    Failed(String),
}

#[derive(clap::Args)]
pub struct InstallLinksArgs {
    /// Claim a shim name even when the file there is not a devkit binary.
    #[arg(long)]
    pub force: bool,
}

/// Whether two paths name the same file, so an existing correct hardlink is
/// left alone instead of being deleted and recreated. On Windows that matters
/// beyond tidiness: deleting a running executable fails.
pub fn same_file(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ma.dev() == mb.dev() && ma.ino() == mb.ino()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ma.file_size() == mb.file_size() && ma.creation_time() == mb.creation_time()
    }
}

/// Whether the file at `path` is a devkit binary, asked by running it. A file
/// that will not execute, or whose output does not name the package, is
/// treated as foreign and left alone.
pub fn is_devkit_binary(path: &Path) -> bool {
    let Ok(out) = std::process::Command::new(path).arg("--version").output() else {
        return false;
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out.status.success() && text.contains(env!("CARGO_PKG_VERSION"))
        || out.status.success() && text.starts_with(&shim_prefixes())
}

fn shim_prefixes() -> String {
    // An older devkit binary prints `<name> <version>`; the name alone is the
    // stable half across versions.
    String::from("devkit")
}

fn shim_file_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Link every shim name in `dir` at `exe`. Returns one outcome per shim, in
/// `SHIMS` order, so the caller renders and exits on the whole set.
pub fn link_all(exe: &Path, dir: &Path, force: bool) -> Vec<(&'static str, Outcome)> {
    SHIMS
        .iter()
        .map(|s| (s.name, link_one(exe, &dir.join(shim_file_name(s.name)), force)))
        .collect()
}

fn link_one(exe: &Path, dest: &Path, force: bool) -> Outcome {
    if dest.exists() {
        if same_file(exe, dest) {
            return Outcome::AlreadyLinked;
        }
        if !force && !is_devkit_binary(dest) {
            return Outcome::SkippedForeign;
        }
        if let Err(e) = std::fs::remove_file(dest) {
            return Outcome::Failed(format!("removing {}: {e}", dest.display()));
        }
        return match std::fs::hard_link(exe, dest) {
            Ok(()) => Outcome::Replaced,
            Err(e) => Outcome::Failed(format!("linking {}: {e}", dest.display())),
        };
    }
    match std::fs::hard_link(exe, dest) {
        Ok(()) => Outcome::Created,
        Err(e) => Outcome::Failed(format!("linking {}: {e}", dest.display())),
    }
}

pub fn run(args: InstallLinksArgs) -> Result<()> {
    let exe = std::env::current_exe().context("resolving the running executable")?;
    let dir = exe
        .parent()
        .context("the running executable has no parent directory")?;
    let results = link_all(&exe, dir, args.force);
    let mut failed = 0;
    for (name, outcome) in &results {
        match outcome {
            Outcome::Created => println!("created   {name}"),
            Outcome::Replaced => println!("replaced  {name}"),
            Outcome::AlreadyLinked => println!("current   {name}"),
            Outcome::SkippedForeign => {
                println!("skipped   {name} (not a devkit binary; --force to claim it)");
            }
            Outcome::Failed(e) => {
                failed += 1;
                eprintln!("failed    {name}: {e}");
            }
        }
    }
    anyhow::ensure!(failed == 0, "{failed} link(s) could not be created");
    Ok(())
}
```

In `src/bin/devkit/main.rs`: add `mod links;`, the variant

```rust
    /// Install the old command names (`issue`, `devrun`, …) as hardlinks
    /// beside this executable.
    InstallLinks(links::InstallLinksArgs),
```

and the arm `Cmd::InstallLinks(a) => links::run(a),`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test install_links`
Expected: PASS, 4 tests.

- [ ] **Step 5: Simplify `is_devkit_binary`**

The draft above has a redundant second branch through `shim_prefixes`. Replace the whole function body with the single check that matters and delete `shim_prefixes`:

```rust
pub fn is_devkit_binary(path: &Path) -> bool {
    let Ok(out) = std::process::Command::new(path).arg("--version").output() else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // Any devkit binary, current or older, prints a name from the shim set or
    // `devkit` itself, followed by a version.
    SHIMS
        .iter()
        .map(|s| s.name)
        .chain(std::iter::once("devkit"))
        .any(|n| text.starts_with(n))
}
```

- [ ] **Step 6: Run the full gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(devkit): add install-links to create the shim hardlinks"
```

---

### Task 9: Automatic linking behind a version stamp

**Files:**
- Modify: `src/bin/devkit/links.rs`, `src/bin/devkit/main.rs`
- Modify: `tests/install_links.rs`

**Interfaces:**
- Consumes: `links::link_all`, `devkit_common::paths::state_dir`.
- Produces: `links::ensure_current(exe: &Path) -> ()`.

Every invocation of every shim takes this path, including `lockm hook pretooluse` on every editor write. The stamp comparison must be one small read when nothing has changed.

- [ ] **Step 1: Write the failing test**

Append to `tests/install_links.rs`:

```rust
/// A first run links without being asked.
#[test]
fn first_run_links_automatically() {
    let (dir, exe) = staged();
    let state = tempfile::tempdir().expect("state dir");
    let out = Command::new(&exe)
        .arg("doctor")
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("spawn devkit doctor");
    let _ = out;
    assert!(
        shim_path(dir.path(), "portm").exists(),
        "a first run should have created the shims"
    );
}

/// A foreign name is not claimed by the automatic path, whatever the stamp says.
#[test]
fn automatic_linking_never_claims_a_foreign_name() {
    let (dir, exe) = staged();
    let state = tempfile::tempdir().expect("state dir");
    let foreign = shim_path(dir.path(), "issue");
    std::fs::write(&foreign, b"#!/bin/sh\necho not devkit\n").expect("write foreign issue");
    Command::new(&exe)
        .arg("doctor")
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("spawn devkit doctor");
    assert_eq!(
        std::fs::read(&foreign).expect("foreign still readable"),
        b"#!/bin/sh\necho not devkit\n",
        "the automatic path must never claim a foreign name"
    );
}

/// The second run does no filesystem work: the stamp already matches.
#[test]
fn second_run_leaves_the_stamp_alone() {
    let (_dir, exe) = staged();
    let state = tempfile::tempdir().expect("state dir");
    for _ in 0..2 {
        Command::new(&exe)
            .arg("doctor")
            .env("HOME", state.path())
            .env("XDG_STATE_HOME", state.path())
            .output()
            .expect("spawn devkit doctor");
    }
    let stamp = state.path().join("devkit/links-version");
    assert_eq!(
        std::fs::read_to_string(&stamp).expect("stamp written").trim(),
        env!("CARGO_PKG_VERSION")
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test install_links first_run_links_automatically`
Expected: FAIL, `portm` does not exist because nothing links without `install-links`.

- [ ] **Step 3: Write minimal implementation**

Append to `src/bin/devkit/links.rs`:

```rust
/// Link the shim names when the stamp does not match this version.
///
/// Runs before dispatch on every invocation, so the match case must stay one
/// small read: `lockm hook pretooluse` takes this path on every editor write.
/// Every failure is a warning; linking never blocks the command the user asked
/// for.
pub fn ensure_current(exe: &Path) {
    let state = devkit_common::paths::state_dir();
    let stamp = state.join("links-version");
    if std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == env!("CARGO_PKG_VERSION")) {
        return;
    }
    let Some(dir) = exe.parent() else { return };
    if std::fs::create_dir_all(&state).is_err() {
        return;
    }
    let Ok(gate) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(state.join("links.lock"))
    else {
        return;
    };
    let mut gate = fd_lock::RwLock::new(gate);
    let Ok(_held) = gate.try_write() else {
        // Another process is linking right now; it will write the stamp.
        return;
    };
    for (name, outcome) in link_all(exe, dir, false) {
        match outcome {
            Outcome::Failed(e) => eprintln!("devkit: could not link {name}: {e}"),
            Outcome::SkippedForeign(why) => {
                eprintln!("devkit: {name} on PATH is not a devkit binary ({why}); left alone");
            }
            Outcome::Created | Outcome::Replaced | Outcome::AlreadyLinked => {}
        }
    }
    let _ = std::fs::write(&stamp, format!("{}\n", env!("CARGO_PKG_VERSION")));
}
```

In `src/bin/devkit/main.rs`, call it from `main` after the panic hook and before dispatch, on both branches:

```rust
fn main() -> Result<()> {
    let args: Vec<OsString> = std::env::args_os().collect();
    let argv0 = args.first().map(|a| a.to_string_lossy().into_owned());
    let shim = argv0.as_deref().and_then(shim::resolve);
    devkit_common::report::install_panic_hook(shim.map_or("devkit", |s| s.name));
    devkit_common::paths::migrate_legacy_state();
    if let Ok(exe) = std::env::current_exe() {
        links::ensure_current(&exe);
    }
    match shim {
        Some(s) => dispatch_shim(s, args),
        None => {
            let cli = Cli::parse();
            // ... existing match on cli.cmd
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test install_links`
Expected: PASS, 7 tests.

- [ ] **Step 5: Measure the hot path**

The `lockm hook pretooluse` path now reads a stamp file per editor write. Confirm the cost is nil before shipping it:

```bash
cargo build --release
hyperfine --warmup 20 'target/release/devkit locks hook pretooluse'
```

Expected: within noise of the same command with `ensure_current` commented out. If it is not, move the stamp check behind an env guard the hook sets and record why in the spec's risks section.

- [ ] **Step 6: Run the full gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(devkit): link shim names automatically on a version change"
```

---

### Task 10: `devkit doctor` reports the shims

**Files:**
- Modify: `src/bin/devkit/doctor.rs`
- Create test in: `tests/install_links.rs`

**Interfaces:**
- Consumes: `shim::SHIMS`, `links::same_file`.
- Produces: a `Row` per shim in doctor's existing table.

`doctor.rs` already has `enum Check { Ok, Warn, Invalid, Unreachable, Unset }` and `struct Row { key, source, check }`. A skipped foreign name is the one case a person must see, since the automatic path stays quiet about it after the first run.

- [ ] **Step 1: Write the failing test**

Append to `tests/install_links.rs`:

```rust
#[test]
fn doctor_reports_a_foreign_shim_name() {
    let (dir, exe) = staged();
    let state = tempfile::tempdir().expect("state dir");
    let foreign = shim_path(dir.path(), "issue");
    std::fs::write(&foreign, b"#!/bin/sh\necho not devkit\n").expect("write foreign issue");
    let out = Command::new(&exe)
        .arg("doctor")
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .output()
        .expect("spawn devkit doctor");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // `issue` alone is not evidence: doctor's tracker row already prints
    // "issue state gates stay closed". Assert on the shim row's own wording.
    assert!(
        text.contains("is not this devkit"),
        "doctor should say the `issue` name is held by something else: {text}"
    );
    assert!(
        text.contains("run: devkit install-links"),
        "doctor should say how to claim the missing shims: {text}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "an unclaimed shim is a warning, not a doctor failure: {text}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test install_links doctor_reports_a_foreign_shim_name`
Expected: FAIL, doctor's output has no shim section.

- [ ] **Step 3: Write minimal implementation**

Add to `src/bin/devkit/doctor.rs`, and call it where the other row builders are called:

```rust
/// One row per shim name: linked, missing, or held by something else. The
/// automatic linker warns once and then stays quiet, so this is where a name it
/// could not claim stays visible.
fn shim_rows() -> Vec<Row> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    crate::shim::SHIMS
        .iter()
        .map(|s| {
            let path = dir.join(if cfg!(windows) {
                format!("{}.exe", s.name)
            } else {
                s.name.to_string()
            });
            let check = if !path.exists() {
                Check::Unset("run: devkit install-links")
            } else if crate::links::same_file(&exe, &path) {
                Check::Ok(format!("linked to {}", exe.display()))
            } else {
                Check::Warn(format!(
                    "{} is not this devkit; run: devkit install-links --force",
                    path.display()
                ))
            };
            Row {
                key: s.name,
                // Doctor's non-credential rows already use `Unset`; a shim has
                // no env or secrets.toml origin to report.
                source: Source::Unset,
                check,
            }
        })
        .collect()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test install_links doctor_reports_a_foreign_shim_name`
Expected: PASS.

- [ ] **Step 5: Run the full gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green. `doctor.rs` has its own `mod tests`; check that `worst_exit` still returns 0 when the only non-`Ok` rows are shim warnings, since a missing shim is not a credential failure.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(devkit): report shim link state in doctor"
```

---

### Task 11: Help lists the shim spellings

**Files:**
- Modify: `src/bin/devkit/main.rs`
- Modify: `tests/cli_ergonomics.rs`

**Interfaces:**
- Consumes: `shim::SHIMS`.
- Produces: nothing later tasks use.

`devkit --help` now lists the merged subcommands on its own, which is the discovery fix. What it still does not say is that `issue status` works as a bare command.

- [ ] **Step 1: Write the failing test**

Append to `tests/cli_ergonomics.rs`:

```rust
/// `devkit` is the one name an agent can guess from a `devkit.toml`. Its help
/// has to name the shim spellings, because a subcommand list alone never says
/// that `issue status` also works as a bare command. Asserting on the whole
/// mapping line, not the bare name, is what keeps this from passing on the
/// subcommand list alone — `issue`, `docs`, and `ports` already appear there.
#[test]
fn devkit_help_names_every_shim() {
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .arg("--help")
        .output()
        .expect("spawn devkit --help");
    let text = String::from_utf8(out.stdout).expect("utf-8 help");
    for line in [
        "issue       = devkit issue",
        "devrun      = devkit run",
        "portm       = devkit ports",
        "lockm       = devkit locks",
        "docm        = devkit docs",
        "devkit-mcp  = devkit mcp",
    ] {
        assert!(
            text.contains(line),
            "`devkit --help` never maps the shim: {line}\n{text}"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test cli_ergonomics devkit_help_names_every_shim`
Expected: FAIL on `devrun`. The subcommand list names `run`, `ports`, `locks`, and `docs`, not the shim spellings.

- [ ] **Step 3: Write minimal implementation**

In `src/bin/devkit/main.rs`:

```rust
const SHIM_HELP: &str = "\
Also installed under their own names:
  issue       = devkit issue
  devrun      = devkit run
  portm       = devkit ports
  lockm       = devkit locks
  docm        = devkit docs
  devkit-mcp  = devkit mcp

Run `devkit install-links` if any of them are missing.";
```

and add `after_help = SHIM_HELP` to the existing `#[command(...)]` attribute on `Cli`. Use `after_help` rather than `after_long_help` so `-h` shows it too; an agent is as likely to type the short form.

`SHIM_HELP` restates the `SHIMS` table, so guard the drift where both are in scope. Add to `main.rs`'s unit tests:

```rust
#[test]
fn shim_help_names_every_shim() {
    for s in crate::shim::SHIMS {
        assert!(
            SHIM_HELP.contains(s.name),
            "SHIM_HELP never names the `{}` shim",
            s.name
        );
    }
}
```

An integration test cannot see `SHIMS`, which is why the table-driven check lives here and the rendered-output check lives in `tests/cli_ergonomics.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test cli_ergonomics devkit_help_names_every_shim`
Expected: PASS.

- [ ] **Step 5: Run the full gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(devkit): name the shim spellings in devkit --help"
```

---

### Task 12: Hooks, packaging, and docs

**Files:**
- Modify: `hooks/bootstrap-binaries`
- Modify: `README.md`
- Modify: `AGENTS.md`
- Modify: `docs/configuration.md` if it names the binaries
- Verify: `dist-workspace.toml`, `.mcp.json`, `hooks/hooks.json`

**Interfaces:**
- Consumes: everything above.
- Produces: nothing.

- [ ] **Step 1: Update the bootstrap hook's presence check**

In `hooks/bootstrap-binaries`, replace

```bash
for bin in devkit lockm devkit-mcp; do
```

with

```bash
for bin in devkit; do
```

The hook does not link. The first `devkit brief` of the session takes `ensure_current` on its own, which covers both installer runs and source builds. Leave every `exit 0` path as it is; a session must still start with no network.

- [ ] **Step 2: Confirm the unchanged files really are unchanged**

Run: `rg -n 'lockm|devkit-mcp|devrun' hooks/hooks.json .mcp.json`
Expected: `hooks.json` still calls `lockm hook …` and `.mcp.json` still calls `devkit-mcp`. Both keep working through their shims, so neither file changes. If either one has been edited, revert it.

Run: `rg -n 'bin|targets' dist-workspace.toml`
Expected: no per-binary list. cargo-dist takes the package's targets, which are now `devkit` and `devkitd`, so the file needs no edit. If a binary list has been added, remove the stale names.

- [ ] **Step 3: Update AGENTS.md**

The Layout table lists a row per binary and the Conventions section states that "the operational verbs (`portm`, `devrun`, `issue`, `lockm`) stay in their own binaries". Both are now wrong.

Rewrite the table so `src/bin/devkit/` describes the merged tool with its subcommands, and `src/bin/devkitd/` stays its own row. Replace the convention sentence with what now holds: every user-facing verb is a `devkit` subcommand, reachable under its old name through a hardlink created by `devkit install-links`, and `devkitd` stays a separate binary because `devkitd_bin()` finds it as a sibling file and `install-service` writes its path into a systemd unit.

Keep it free of hard numbers, per the repo's persistent-writing rule: "the CLIs install together", not "six CLIs install together".

- [ ] **Step 4: Update README.md**

The install section tells a reader to run `cargo install --path .` and get several binaries. Say instead that it installs `devkit` and `devkitd`, and that the old names appear on first run, with `devkit install-links` as the manual fallback. Document `--force` and what it overrides.

- [ ] **Step 2b: Update the PowerShell bootstrap hook and its test**

`hooks/bootstrap-binaries.ps1:38` carries the same presence list as the shell hook. Replace

```powershell
$missing = @('devkit', 'lockm', 'devkit-mcp') | Where-Object { -not (Get-Command $_ -ErrorAction SilentlyContinue) }
```

with

```powershell
$missing = @('devkit') | Where-Object { -not (Get-Command $_ -ErrorAction SilentlyContinue) }
```

`tests/hooks/bootstrap-binaries.test.sh` encodes the list twice: the loop at line 54 (`for b in devkit lockm devkit-mcp; do`) and the comment at lines 81-82 describing what the `binaries` fixture puts on PATH. Both must agree with the hooks or the test fails. Change the loop to `for b in devkit; do` and rewrite the comment to name only `devkit`.

Run: `bash tests/hooks/bootstrap-binaries.test.sh`
Expected: PASS.

- [ ] **Step 2c: Confirm the shipped bin targets and the second hooks file**

Run: `rg -n -A3 '\[\[bin\]\]' Cargo.toml && ls src/bin/`
Expected: one explicit `[[bin]]` for `devkitd`, and `src/bin/` holding only `devkit/` and `devkitd/`. Cargo auto-discovers `src/bin/devkit/main.rs` as the package binary. A surviving old bin directory or `[[bin]]` entry is stale and still ships.

Run: `rg -n 'lockm|devkit-mcp|devrun|docm' hooks/hooks-codex.json`
Expected: `lockm hook …` calls, unchanged. This file is the Codex twin of `hooks/hooks.json` and works through the same shims, so it needs no edit either.

- [ ] **Step 4b: Document the shim mechanics in README.md**

Three things a reader now needs and cannot learn anywhere else:

- `devkit install-links` creates the hardlinks. `--force` claims a name held by something that is not a devkit binary; without it, such a name is reported as skipped and left alone.
- The links refresh on their own. Any `devkit` invocation relinks when the binary's version, directory, or modification time stops matching the recorded stamp, so an upgrade needs no manual step.
- `DEVKIT_SKIP_AUTOLINK`, set to any value, turns that automatic pass off. It is the opt-out for a packager who manages the links themselves and for anything that must not write the state directory.

- [ ] **Step 4c: Update the agent-facing skill docs**

`skills/using-devkit/SKILL.md`, `skills/using-devkit/cli-reference.md`, and `skills/docs/SKILL.md` teach agents the command surface, and they name the old binaries throughout. Those spellings still work, so this is an addition, not a rewrite: state once, near the top of each file that names a binary, that every command also spells as a `devkit` subcommand, with one example (`docm list` and `devkit docs list` are the same command). Leave the existing examples alone. An agent that reads only `devkit --help` has to be able to find these commands; an agent carrying the old spellings must not be told they are gone.

`docs/configuration.md` gets the same treatment, only where it names a binary as the thing to run.

- [ ] **Step 5: Run the full gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

Then confirm the shipped set by hand:

```bash
cargo build --release && ls target/release/ | rg -v '\.(d|rlib)$' | rg -x 'devkit|devkitd|issue|portm|lockm|devrun|docm|devkit-mcp'
```

Expected: `devkit` and `devkitd`, nothing else. Stale files from earlier builds may linger; `cargo clean` first if the list disagrees.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "docs: describe the single-binary layout and shim links"
```

---

## Notes for the executor

- **Rebase before Task 6.** `feat/issue-sync-includes` adds a subcommand to the same `Cmd` enum that Task 6 moves. Rebasing after the move means re-applying that work against a relocated file by hand.
- **The `sed` pass in Task 6 Step 3 is the riskiest edit in this plan.** It rewrites every `crate::` path in sixteen files. Read the diff. A wrong path is a compile error, which is the good case; a path that happens to resolve to a different module is not.
- **Never hoist a `shimtest::linked` guard into a `static` or `OnceLock`.** The `TempDir`'s `Drop` is what removes the directory, and a `static` never drops. Each test creates its own link and holds its own guard.
- **The `-D warnings` gate will fire on moved code.** Files that were bin roots become modules, so items that were reachable from `main` may now be unused. Delete what is genuinely dead rather than adding `#[allow(dead_code)]`.
