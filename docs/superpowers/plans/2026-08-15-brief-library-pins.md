# Library Pins in the Brief — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every devkit session state, from the filesystem alone, which version of each registered library this checkout actually pins — as a table in `devkit brief` and behind `docm list --project` — without ever asserting a version that `docm` resolution would not serve.

**Architecture:** One resolution path. `devkit-docs::importers` gains an `inspect` API that reports *declaration evidence* alongside the existing `Result<Selection>`; a new `devkit-docs::pins` module turns the merged docs manifest into a `Vec<Pin>` using only manifest and lockfile reads (no clone, no fetch, no cache lock); one renderer turns that into a table with byte budgets. Two callers use it — `docm list --project` and `devkit brief` — and the brief's emission is gated by a new `[brief]` config section so shipped hooks can be unconditional.

**Tech Stack:** Rust 2024, `anyhow`, `serde`/`serde_json`/`serde_yaml_ng`/`toml`, `comfy-table` via `devkit_common::ui::table`, `clap`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-15-brief-library-pins-design.md` — read it alongside this plan. Adversarial-review transcript: `docs/superpowers/specs/2026-08-15-brief-library-pins-review-log.md`.

## Global Constraints

- **Never state a version resolution would not serve.** No second lockfile parser. `crates/devkit-docs/src/lockfiles.rs` is for prune liveness only — never call it from this feature.
- **`pins` performs filesystem reads only**: manifests and lockfiles. No git, no network, no cache reads, no `fd-lock`, no process spawn.
- **No output changes to existing commands.** `docm list` without `--project`, `docm info`, `docm path`, `docm sync` render exactly as before. `importers::select`'s signature and every error string it produces are unchanged.
- **Every failure in the pins path is contained.** A broken docs manifest omits the pins section; it never suppresses the brief's apps/tasks/servers sections, and never makes `devkit brief` exit non-zero.
- **`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all` must be green before every commit.** CI runs all three on ubuntu, macos, and windows.
- **Tests that observe process or filesystem state poll for it**; never `sleep` a fixed interval (loaded Windows runners).
- Conventional Commits, imperative subject, ≤50 chars, lowercase after the colon, no trailing period, no emoji.
- Work in a worktree: `git worktree add ../devkit-worktrees/brief-pins -b feat/brief-pins main`. The primary clone stays on `main`.
- Byte budgets, fixed: **200 bytes per table cell**, **4096 bytes per rendered section**. Truncation is whole rows plus a visible marker row, never a clipped row.
- `--project --json` carries full untruncated values. Truncation is a property of context-injection rendering, not of the data.

---

## File Structure

**Created:**

| Path | Responsibility |
|---|---|
| `crates/devkit-docs/src/pins.rs` | The `Pin`/`Outcome`/`Dropped` model, `pins()` (manifest → outcomes), the relevance filter, the shared table renderer, and the JSON envelope builder. One module because these change together: a new `Outcome` variant needs a row shape and a JSON shape in the same edit. |
| `crates/devkit-docs/tests/importers_goldens.rs` | Exact-text capture of `importers::select` across a fixture matrix — the safety net for Task 6. |
| `crates/devkit-docs/tests/goldens/importers.txt` | The recorded goldens (checked in). |
| `crates/devkit-docs/tests/pins.rs` | Integration tests for `pins()` and the renderer. |
| `hooks/brief` | Bash hook shim: presence check + flag pass-through + stdin forwarding. |
| `hooks/brief.ps1` | PowerShell twin for a Windows host with no Git-for-Windows bash. |
| `tests/brief_pins.rs` | End-to-end tests driving the `devkit` binary: config gating, docs-only project, emission modes, watermark. |

**Modified:**

| Path | Change |
|---|---|
| `crates/devkit-docs/src/importers.rs` | `Undeclared` typed error, `Selection.lockfile`, `Evidence`/`Inspection`/`inspect` (Task 2); `Selector` + lazy memoized parsing (Task 6). |
| `crates/devkit-docs/src/lib.rs` | `pub mod pins;` |
| `src/bin/docm.rs` | `List { json, project }`, `cmd_list` project branch. |
| `crates/devkit-ports/src/config.rs` | `BriefConfig` on `Config`. |
| `crates/devkit-ports/src/registry.rs` | `listening_view` + `status_table_with`; `status_table` delegates. |
| `src/bin/devkit/brief.rs` | Config gate, pins section, `render` restructure, emission modes, `BriefSnapshot`, watermark. |
| `src/bin/devkit/main.rs` | `Brief { pins_only, if_changed }`. |
| `hooks/hooks.json` | SessionStart via `run-hook.cmd brief`; new PostCompact and CwdChanged entries. |
| `skills/docs/SKILL.md` | `allowed-tools`, inline block, reworded step 1, new second-version rule. |
| `README.md`, `docs/configuration.md` | `docm list --project` and `[brief]` documentation. |

---

# Milestone A — the content, through the CLI

## Task 1: Behavior goldens for `importers::select`

Recorded against the **current** implementation, before anything changes. After a refactor there is nothing left to compare against, and comparing `Selector::new(..).select(pkg)` to `select(start, eco, pkg)` afterwards proves only that the wrapper forwards.

The existing `crates/devkit-docs/tests/importers.rs` (25 tests) stays the primary semantic gate — it already covers per-manager validation ordering in depth. What it does **not** cover is exact error text: it asserts with `contains`. The goldens add byte-exact `{}`, `{:#}`, and `{:?}` capture, which is what Task 2's `Undeclared` change and Task 6's error-replay caching can silently break.

**Files:**
- Create: `crates/devkit-docs/tests/importers_goldens.rs`
- Create: `crates/devkit-docs/tests/goldens/importers.txt` (generated in Step 3, then committed)

**Interfaces:**
- Consumes: `devkit_docs::importers::select`, `devkit_docs::manifest::Ecosystem` (both as they exist today).
- Produces: nothing other tasks call. Task 2 and Task 6 must leave `cargo test -p devkit-docs --test importers_goldens` green without re-recording.

- [ ] **Step 1: Write the fixture matrix and the record/compare harness**

Create `crates/devkit-docs/tests/importers_goldens.rs`:

```rust
//! Byte-exact capture of what `importers::select` returns today, across a
//! fixture matrix. Re-record with `DOCM_GOLDEN_RECORD=1`; a diff without a
//! deliberate re-record is a behavior change.

use devkit_docs::importers;
use devkit_docs::manifest::Ecosystem;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
mod common;

const GOLDEN: &str = "tests/goldens/importers.txt";

/// One fixture tree and the probes recorded against it. `start` is relative to
/// the fixture root; `""` means the root itself.
struct Case {
    name: &'static str,
    build: fn(&Path),
    probes: &'static [(Ecosystem, &'static str, &'static str)],
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn pnpm_monorepo(root: &Path) {
    write(
        &root.join("pnpm-lock.yaml"),
        r#"lockfileVersion: '9.0'
importers:
  .:
    dependencies: {}
  apps/web:
    dependencies:
      kysely:
        specifier: ^0.28.0
        version: 0.28.17
  apps/api:
    dependencies:
      kysely:
        specifier: ^0.27.0
        version: 0.27.3
packages:
  kysely@0.28.17: {}
  kysely@0.27.3: {}
  transitive@3.2.1: {}
"#,
    );
    write(
        &root.join("package.json"),
        r#"{"name":"root","packageManager":"pnpm@9.0.0"}"#,
    );
    write(&root.join("apps/web/package.json"), r#"{"name":"@app/web"}"#);
    write(&root.join("apps/api/package.json"), r#"{"name":"@app/api"}"#);
}

fn bun_workspace(root: &Path) {
    write(
        &root.join("bun.lock"),
        r#"{
  "lockfileVersion": 1,
  "workspaces": {
    "": { "name": "root" },
    "apps/api": { "name": "@app/api", "dependencies": { "h3": "^1.15.5" } }
  },
  "packages": {
    "h3": ["h3@1.15.11", "", {}, "sha512-a"],
    "transitive": ["transitive@3.2.1", "", {}, "sha512-d"]
  }
}"#,
    );
    write(&root.join("package.json"), r#"{"name":"root"}"#);
    write(
        &root.join("apps/api/package.json"),
        r#"{"name":"@app/api","dependencies":{"h3":"^1.15.5"}}"#,
    );
}

fn npm_nested(root: &Path) {
    write(
        &root.join("package-lock.json"),
        r#"{
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "root" },
    "apps/web": { "name": "@app/web", "dependencies": { "h3": "^1.0.0" } },
    "apps/web/node_modules/h3": { "version": "1.15.11", "resolved": "https://registry.npmjs.org/h3/-/h3-1.15.11.tgz" },
    "node_modules/h3": { "version": "1.0.0", "resolved": "https://registry.npmjs.org/h3/-/h3-1.0.0.tgz" }
  }
}"#,
    );
    write(&root.join("package.json"), r#"{"name":"root"}"#);
    write(
        &root.join("apps/web/package.json"),
        r#"{"name":"@app/web","dependencies":{"h3":"^1.0.0"}}"#,
    );
}

fn cargo_workspace(root: &Path) {
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/app\"]\nresolver = \"2\"\n",
    );
    write(
        &root.join("crates/app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde = \"1.0.200\"\n",
    );
    write(
        &root.join("Cargo.lock"),
        r#"version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = ["serde"]

[[package]]
name = "serde"
version = "1.0.200"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aa"
"#,
    );
}

fn uv_project(root: &Path) {
    write(
        &root.join("pyproject.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"httpx\"]\n",
    );
    write(
        &root.join("uv.lock"),
        r#"version = 1

[[package]]
name = "app"
version = "0.1.0"
source = { editable = "." }
dependencies = [{ name = "httpx" }]

[[package]]
name = "httpx"
version = "0.27.2"
source = { registry = "https://pypi.org/simple" }
"#,
    );
}

/// Two JS lockfiles and no `packageManager` — the ambiguity arm, whose error
/// enumerates each lockfile's outcome in order.
fn ambiguous_js(root: &Path) {
    write(
        &root.join("bun.lock"),
        r#"{"lockfileVersion":1,"workspaces":{"":{"name":"root","dependencies":{"h3":"^1"}}},"packages":{"h3":["h3@1.15.11","",{},"sha512-a"]}}"#,
    );
    write(
        &root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      h3:\n        specifier: ^1\n        version: 1.0.0\npackages:\n  h3@1.0.0: {}\n",
    );
    write(
        &root.join("package.json"),
        r#"{"name":"root","dependencies":{"h3":"^1"}}"#,
    );
}

const CASES: &[Case] = &[
    Case {
        name: "pnpm-monorepo",
        build: pnpm_monorepo,
        probes: &[
            (Ecosystem::Js, "apps/web", "kysely"),
            (Ecosystem::Js, "apps/api", "kysely"),
            // Transitive-only: the `undeclared` diagnostic, verbatim.
            (Ecosystem::Js, "apps/web", "transitive"),
            // Absent entirely.
            (Ecosystem::Js, "apps/web", "nope"),
        ],
    },
    Case {
        name: "bun-workspace",
        build: bun_workspace,
        probes: &[
            (Ecosystem::Js, "apps/api", "h3"),
            (Ecosystem::Js, "apps/api", "transitive"),
        ],
    },
    Case {
        name: "npm-nested",
        build: npm_nested,
        probes: &[(Ecosystem::Js, "apps/web", "h3")],
    },
    Case {
        name: "cargo-workspace",
        build: cargo_workspace,
        probes: &[
            (Ecosystem::Rust, "crates/app", "serde"),
            (Ecosystem::Rust, "crates/app", "tokio"),
        ],
    },
    Case {
        name: "uv-project",
        build: uv_project,
        probes: &[
            (Ecosystem::Python, "", "httpx"),
            (Ecosystem::Python, "", "requests"),
        ],
    },
    Case {
        name: "ambiguous-js",
        build: ambiguous_js,
        probes: &[(Ecosystem::Js, "", "h3")],
    },
];

/// Absolute temp paths appear inside error text and in `Selection.workspace`;
/// replace them so a recording is portable across machines and runs.
fn scrub(text: &str, root: &Path) -> String {
    let root = root.to_string_lossy().replace('\\', "/");
    text.replace('\\', "/")
        .replace(&root, "<ROOT>")
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn record() -> String {
    let mut out = String::new();
    for case in CASES {
        let root = common::unique_tmp(&format!("golden-{}", case.name));
        (case.build)(&root);
        for (ecosystem, start, package) in case.probes {
            let dir = if start.is_empty() {
                root.clone()
            } else {
                root.join(start)
            };
            out.push_str(&format!(
                "## {}/{}/{package}\n",
                case.name,
                if start.is_empty() { "." } else { start }
            ));
            match importers::select(&dir, *ecosystem, package) {
                Ok(selection) => {
                    out.push_str(&format!(
                        "ok version={}\nok workspace={}\nok source={}\n",
                        scrub(&selection.version, &root),
                        scrub(&selection.workspace.to_string_lossy(), &root),
                        scrub(&selection.source, &root),
                    ));
                }
                Err(error) => {
                    out.push_str(&format!(
                        "err display={}\nerr alternate={}\nerr debug={}\n",
                        scrub(&format!("{error}"), &root),
                        scrub(&format!("{error:#}"), &root),
                        scrub(&format!("{error:?}"), &root),
                    ));
                }
            }
            out.push('\n');
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    out
}

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN)
}

#[test]
fn importer_selection_matches_the_recorded_goldens() {
    let actual = record();
    let path = golden_path();
    if std::env::var_os("DOCM_GOLDEN_RECORD").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{} is missing; record it with DOCM_GOLDEN_RECORD=1",
            path.display()
        )
    });
    assert_eq!(
        expected, actual,
        "importer behavior changed; if deliberate, re-record with DOCM_GOLDEN_RECORD=1"
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p devkit-docs --test importers_goldens`
Expected: FAIL — `crates/devkit-docs/tests/goldens/importers.txt is missing; record it with DOCM_GOLDEN_RECORD=1`

- [ ] **Step 3: Record the goldens**

Run: `DOCM_GOLDEN_RECORD=1 cargo test -p devkit-docs --test importers_goldens`
Then read `crates/devkit-docs/tests/goldens/importers.txt` and check by eye that each block is what the current implementation should say: the two `kysely` probes differ (`0.28.17` vs `0.27.3`), `transitive` produces the three-line `does not declare ... (it is transitive)` diagnostic, `nope` produces the same shape with `versions present in the lockfile: none`, and the `ambiguous-js` block enumerates `bun.lock` before `pnpm-lock.yaml`.

If a block records an unexpected error (e.g. a fixture the parser rejects), fix the *fixture*, not the recorder — the goldens must capture real resolution, not a fixture typo.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p devkit-docs --test importers_goldens`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-docs/tests/importers_goldens.rs crates/devkit-docs/tests/goldens/importers.txt
git commit -m "test(docs): record importer selection goldens"
```

---

## Task 2: The `importers` API surface — `Undeclared`, `Selection.lockfile`, `inspect`

Three additive changes in one task: they share a file, and Task 3 needs all three at once. Nothing here changes `select`'s signature or any error string.

**Why `Undeclared` must be the *outer* error, not a cause:** `undeclared` returns a single causeless `anyhow!` today. Attaching a cause with `.context(msg)` preserves `{}` but changes `{:#}`, `Debug`, and `main`'s rendering. `anyhow::Error::new` requires `std::error::Error + Send + Sync + 'static`, so `#[derive(Debug)]` alone will not compile.

**Why evidence cannot be recovered from the `Result`:** several managers *find* the declaration and then fail on something downstream — a non-registry source, a missing package row, an npm alias, a resolution fork. Those are exactly the errors whose text says to pin with `--ref`, so a caller that infers "not declared" from `Err` would drop the very library someone ref-pinned on that advice. Evidence is therefore recorded by an out-parameter at the point each manager establishes declaration, *before* the checks that can fail after it.

**Files:**
- Modify: `crates/devkit-docs/src/importers.rs`
- Test: `crates/devkit-docs/tests/importers.rs` (append)

**Interfaces:**
- Consumes: nothing new.
- Produces, all in `devkit_docs::importers`:
  - `pub struct Selection { pub workspace: PathBuf, pub version: String, pub source: String, pub lockfile: String }`
  - `pub struct Undeclared { pub package: String, pub workspace: PathBuf, message: String }` — `Display`, `Error`, `Debug`
  - `pub enum Evidence { Declared, Undeclared, Unknown }` — `Clone, Copy, Debug, PartialEq, Eq`
  - `pub struct Inspection { pub evidence: Evidence, pub result: Result<Selection> }`
  - `pub fn inspect(start: &Path, ecosystem: Ecosystem, package: &str) -> Inspection`
  - `pub fn select(start: &Path, ecosystem: Ecosystem, package: &str) -> Result<Selection>` — unchanged signature, now `inspect(..).result`

- [ ] **Step 1: Write the failing tests**

Append to `crates/devkit-docs/tests/importers.rs`:

```rust
#[test]
fn undeclared_is_downcastable_and_renders_unchanged() {
    let root = common::unique_tmp("undeclared-typed");
    std::fs::write(root.join("bun.lock"), BUN_LOCK).unwrap();
    let ws = root.join("apps/web");
    write_package_json(&ws, r#"{"name":"@app/web","dependencies":{}}"#);

    let error = importers::select(&ws, Ecosystem::Js, "transitive").unwrap_err();
    let marker = error
        .downcast_ref::<importers::Undeclared>()
        .expect("transitive misses are typed");
    assert_eq!(marker.package, "transitive");
    assert_eq!(marker.workspace, ws);

    // The three renderings must stay identical to the untyped anyhow! form:
    // a cause attached in the wrong place changes the last two only.
    let display = format!("{error}");
    assert_eq!(format!("{error:#}"), display);
    assert_eq!(format!("{error:?}"), display);
    assert!(display.contains("does not declare `transitive`"), "{display}");
}

#[test]
fn selection_names_the_lockfile_that_carried_the_version() {
    let root = common::unique_tmp("selection-lockfile");
    std::fs::write(root.join("bun.lock"), BUN_LOCK).unwrap();
    let ws = root.join("apps/api");
    write_package_json(&ws, r#"{"name":"@app/api","dependencies":{"h3":"^1.15.5"}}"#);

    let selection = importers::select(&ws, Ecosystem::Js, "h3").unwrap();
    assert_eq!(selection.lockfile, "bun.lock");
}

#[test]
fn evidence_survives_a_failure_that_runs_after_declaration() {
    // A workspace that declares the package from a non-registry source: the
    // importer establishes declaration, then rejects the resolution. `select`
    // errors; `inspect` still reports Declared. This is the case whose error
    // text recommends --ref, so a globally ref-pinned library in this state
    // must still be judged relevant to the checkout.
    let root = common::unique_tmp("evidence-post-decl");
    std::fs::write(
        root.join("bun.lock"),
        r#"{
  "lockfileVersion": 1,
  "workspaces": {
    "": { "name": "root", "dependencies": { "h3": "github:unjs/h3" } }
  },
  "packages": {
    "h3": ["h3@git+https://github.com/unjs/h3.git#abc", {}, "github:unjs/h3#abc"]
  }
}"#,
    )
    .unwrap();
    write_package_json(&root, r#"{"name":"root","dependencies":{"h3":"github:unjs/h3"}}"#);

    let inspection = importers::inspect(&root, Ecosystem::Js, "h3");
    assert!(inspection.result.is_err(), "non-registry must not resolve");
    assert_eq!(inspection.evidence, importers::Evidence::Declared);
}

#[test]
fn evidence_reports_undeclared_and_unknown_distinctly() {
    let root = common::unique_tmp("evidence-tristate");
    std::fs::write(root.join("bun.lock"), BUN_LOCK).unwrap();
    let ws = root.join("apps/web");
    write_package_json(&ws, r#"{"name":"@app/web","dependencies":{}}"#);

    // Checked and absent.
    assert_eq!(
        importers::inspect(&ws, Ecosystem::Js, "transitive").evidence,
        importers::Evidence::Undeclared
    );
    // No importer manifest for this ecosystem — the check could not run.
    assert_eq!(
        importers::inspect(&ws, Ecosystem::Rust, "serde").evidence,
        importers::Evidence::Unknown
    );
    // Git has no importer to ask.
    assert_eq!(
        importers::inspect(&ws, Ecosystem::Git, "anything").evidence,
        importers::Evidence::Unknown
    );
}

#[test]
fn an_ambiguity_probe_does_not_leak_evidence() {
    // Two lockfiles, no packageManager: resolution bails without deciding.
    // The per-lockfile probes that build the message must not set evidence.
    let root = common::unique_tmp("evidence-ambiguous");
    std::fs::write(
        root.join("bun.lock"),
        r#"{"lockfileVersion":1,"workspaces":{"":{"name":"root","dependencies":{"h3":"^1"}}},"packages":{"h3":["h3@1.15.11","",{},"sha512-a"]}}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      h3:\n        specifier: ^1\n        version: 1.0.0\npackages:\n  h3@1.0.0: {}\n",
    )
    .unwrap();
    write_package_json(&root, r#"{"name":"root","dependencies":{"h3":"^1"}}"#);

    let inspection = importers::inspect(&root, Ecosystem::Js, "h3");
    assert!(inspection.result.is_err());
    assert_eq!(inspection.evidence, importers::Evidence::Unknown);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-docs --test importers`
Expected: FAIL to compile — `no function or associated item named 'inspect'`, `cannot find type 'Undeclared'`, `no field 'lockfile' on type 'Selection'`.

- [ ] **Step 3: Add the types**

In `crates/devkit-docs/src/importers.rs`, replace the `Selection`/`select` block at the top:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub workspace: PathBuf,
    pub version: String,
    pub source: String,
    /// The lockfile that carried the version, by file name (`pnpm-lock.yaml`,
    /// `Cargo.lock`, …). Carried rather than derived: `source` is prose and
    /// `workspace` alone cannot say which of three JS lockfiles was consulted.
    pub lockfile: String,
}

/// `package` is present in the lockfile only transitively, or not at all —
/// this workspace does not declare it. Typed so a caller resolving many
/// libraries can tell an uninteresting miss from a broken lockfile.
#[derive(Debug)]
pub struct Undeclared {
    pub package: String,
    pub workspace: PathBuf,
    /// The full diagnostic, rendered verbatim by `Display` so no output
    /// changes. Keeping this the outer error with no cause is what keeps
    /// `{:#}` and `Debug` byte-identical to the untyped form.
    message: String,
}

impl std::fmt::Display for Undeclared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Undeclared {}

/// What the importer graph can say about this workspace depending on a
/// package, independent of whether a version was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// A manifest in this workspace declares the package.
    Declared,
    /// The importer graph ran and the package is transitive-only or absent.
    Undeclared,
    /// Nothing could be established — no importer manifest, a malformed
    /// lockfile, or an ecosystem with no importer to ask.
    Unknown,
}

pub struct Inspection {
    pub evidence: Evidence,
    pub result: Result<Selection>,
}

/// The full report: declaration evidence plus the resolution result. Evidence
/// is recorded where each manager establishes declaration, ahead of the checks
/// that can fail afterwards, so a post-declaration failure still reports
/// `Declared`.
pub fn inspect(start: &Path, ecosystem: Ecosystem, package: &str) -> Inspection {
    let mut evidence = Evidence::Unknown;
    let result = match ecosystem {
        Ecosystem::Js => js(start, package, &mut evidence),
        Ecosystem::Rust => cargo(start, package, &mut evidence),
        Ecosystem::Python => uv(start, package, &mut evidence),
        Ecosystem::Git => Err(anyhow::anyhow!("git entries resolve by ref, not by lockfile")),
    };
    Inspection { evidence, result }
}

/// Compatibility projection — every existing caller keeps this shape.
pub fn select(start: &Path, ecosystem: Ecosystem, package: &str) -> Result<Selection> {
    inspect(start, ecosystem, package).result
}
```

- [ ] **Step 4: Make `undeclared` return the typed error**

Replace the `anyhow::anyhow!` tail of `undeclared` (`importers.rs:130-136`) — the string it builds is unchanged:

```rust
    let message = format!(
        "{} does not declare `{package}` (it is transitive); pin the version with --ref.\n\
         versions present in the lockfile: {versions}\n\
         declared by: {declarers}{resolved}",
        workspace.display()
    );
    anyhow::Error::new(Undeclared {
        package: package.to_string(),
        workspace: workspace.to_path_buf(),
        message,
    })
```

- [ ] **Step 5: Thread `&mut Evidence` and set `lockfile` in each manager**

Signature changes (internal, no public surface):

| Function | New trailing parameter |
|---|---|
| `js(start, package, evidence: &mut Evidence)` | |
| `select_js_lock(manager, lock_dir, workspace, relative, package, evidence: &mut Evidence)` | |
| `bun(lock_dir, workspace, relative, package, evidence: &mut Evidence)` | |
| `pnpm(lock_dir, workspace, relative, package, evidence: &mut Evidence)` | |
| `npm(lock_dir, workspace, relative, package, evidence: &mut Evidence)` | |
| `cargo(start, package, evidence: &mut Evidence)` | |
| `uv(start, package, evidence: &mut Evidence)` | |
| `from_package_array(.., package, evidence: &mut Evidence)` | |

**`js`, ambiguity arm** (`importers.rs:205-228`) — each probe gets a throwaway sink, so a probe against a lockfile that does not govern never sets the caller's evidence:

```rust
                .map(|(manager, file)| {
                    let mut probe = Evidence::Unknown;
                    let outcome =
                        select_js_lock(manager, &lock_dir, &workspace, &relative, package, &mut probe);
                    match outcome {
                        Ok(selection) => format!("{file} → {}", selection.version),
                        Err(error) => format!("{file} → {error}"),
                    }
                })
```

**`bun`** — set at the declaration check, and name the lockfile in the `Selection`:

```rust
    if !json_declares(
        entry,
        &BUN_DEP_CLASSES,
        package,
        &format!("workspaces.{relative}"),
    )? {
        *evidence = Evidence::Undeclared;
        return Err(undeclared(workspace, package, &candidates));
    }
    *evidence = Evidence::Declared;
```
…and in its `Ok(Selection { .. })`: `lockfile: "bun.lock".to_string(),`

**`pnpm`** — declaration is established *inside* the class loop, before the package-row check that can fail after it:

```rust
        if let Some(entry) = dependencies.get(&package_key) {
            *evidence = Evidence::Declared;
            let location = format!("importers.{importer_key}.{class}.{package}");
            ...
        }
```
and at the empty arm:
```rust
    let version = match matches.as_slice() {
        [] => {
            *evidence = Evidence::Undeclared;
            return Err(undeclared(workspace, package, &candidates));
        }
        [version] => version.clone(),
        versions => bail!(...),
    };
```
…and in its `Ok(Selection { .. })`: `lockfile: "pnpm-lock.yaml".to_string(),`

**`npm`** — set before `assert_npm_registry_spec`, which can fail after declaration:

```rust
    if specs.is_empty() {
        *evidence = Evidence::Undeclared;
        return Err(undeclared(workspace, package, &candidates));
    }
    *evidence = Evidence::Declared;
    for spec in specs {
        assert_npm_registry_spec(spec, package, display_key(relative))?;
    }
```
…and in its `Ok(Selection { .. })`: `lockfile: "package-lock.json".to_string(),`

**`from_package_array`** — set before the version resolution and `assert_registry_source`, both of which fail after declaration:

```rust
    let pinned = match matching_edges.as_slice() {
        [] => {
            *evidence = Evidence::Undeclared;
            return Err(undeclared(workspace, package, &candidates));
        }
        [edge] => {
            *evidence = Evidence::Declared;
            edge.version.as_deref()
        }
        _ => {
            *evidence = Evidence::Declared;
            bail!(
                "{member} maps `{package}` through multiple dependency edges in {}",
                lock.display()
            )
        }
    };
```
…and in its `Ok(Selection { .. })`: `lockfile: detail.to_string(),` (`detail` is already the lockfile's file name).

`cargo` and `uv` forward their `evidence` argument into `from_package_array` unchanged.

- [ ] **Step 6: Run the new tests**

Run: `cargo test -p devkit-docs --test importers`
Expected: PASS, all tests including the five new ones.

- [ ] **Step 7: Run the goldens and the full gate**

Run: `cargo test -p devkit-docs --test importers_goldens`
Expected: PASS with **no re-recording**. A diff here means an error string changed — find it and revert the change; do not re-record.

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: green.

- [ ] **Step 8: Commit**

```bash
git add crates/devkit-docs/src/importers.rs crates/devkit-docs/tests/importers.rs
git commit -m "feat(docs): add inspect, evidence, typed undeclared"
```

---

## Task 3: `pins` — the manifest-to-outcomes readout

`pins` answers "what does this checkout pin?" from manifests and lockfiles alone. It returns `Err` only when it cannot enumerate registrations at all; a single library failing to resolve is that row's data.

**Files:**
- Create: `crates/devkit-docs/src/pins.rs`
- Modify: `crates/devkit-docs/src/lib.rs` (add `pub mod pins;`)
- Test: `crates/devkit-docs/tests/pins.rs`

**Interfaces:**
- Consumes: `importers::{inspect, Inspection, Evidence, Undeclared, Selection}` (Task 2); `manifest::{discover, Discovered, Ecosystem, LibEntry, global_docs_path}`; `names::validate_ref`.
- Produces, in `devkit_docs::pins`:
  - `pub enum Outcome { Version { version: String, workspace: PathBuf, lockfile: String }, Ref(String), Unresolved(String), Undeclared }`
  - `pub struct Pin { pub name: String, pub outcome: Outcome, pub project_scoped: bool, pub declared: Evidence }`
  - `pub fn pins(start: &Path, global: Option<&Path>) -> Result<Vec<Pin>>` — alphabetical by `name`

- [ ] **Step 1: Write the failing tests**

Create `crates/devkit-docs/tests/pins.rs`:

```rust
//! `pins` turns the merged docs manifest into per-library outcomes using
//! manifest and lockfile reads only.

use devkit_docs::importers::Evidence;
use devkit_docs::pins::{self, Outcome};
use std::path::Path;

#[allow(dead_code)]
mod common;

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// A cargo project depending on `serde`, with a global docs manifest and an
/// optional project `[docs]` section.
fn cargo_project(root: &Path, project_docs: &str) {
    write(
        &root.join("project/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde = \"1.0.200\"\n",
    );
    write(
        &root.join("project/Cargo.lock"),
        r#"version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = ["serde"]

[[package]]
name = "serde"
version = "1.0.200"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aa"
"#,
    );
    write(&root.join("project/devkit.toml"), project_docs);
}

#[test]
fn a_declared_library_reports_its_lockfile_version() {
    let root = common::unique_tmp("pins-version");
    cargo_project(&root, "[config]\nroot = true\n");
    write(
        &root.join("docs.toml"),
        "[[libs]]\nname = \"serde\"\necosystem = \"rust\"\nrepo = \"https://example.invalid/serde\"\n",
    );

    let out = pins::pins(&root.join("project"), Some(&root.join("docs.toml"))).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].name, "serde");
    assert_eq!(out[0].declared, Evidence::Declared);
    assert!(!out[0].project_scoped);
    match &out[0].outcome {
        Outcome::Version { version, lockfile, workspace } => {
            assert_eq!(version, "1.0.200");
            assert_eq!(lockfile, "Cargo.lock");
            assert_eq!(workspace, Path::new("."));
        }
        other => panic!("expected a version, got {other:?}"),
    }
}

#[test]
fn a_transitive_library_is_undeclared_not_unresolved() {
    let root = common::unique_tmp("pins-undeclared");
    cargo_project(&root, "[config]\nroot = true\n");
    write(
        &root.join("docs.toml"),
        "[[libs]]\nname = \"tokio\"\necosystem = \"rust\"\nrepo = \"https://example.invalid/tokio\"\n",
    );

    let out = pins::pins(&root.join("project"), Some(&root.join("docs.toml"))).unwrap();
    assert!(matches!(out[0].outcome, Outcome::Undeclared), "{:?}", out[0].outcome);
    assert_eq!(out[0].declared, Evidence::Undeclared);
}

#[test]
fn a_ref_pin_still_carries_importer_evidence() {
    // resolve checks `ref` before consulting the importer, so a ref pin would
    // otherwise carry no evidence in either direction. It must, or the
    // relevance filter drops libraries the project genuinely depends on.
    let root = common::unique_tmp("pins-ref-evidence");
    cargo_project(&root, "[config]\nroot = true\n");
    write(
        &root.join("docs.toml"),
        "[[libs]]\nname = \"serde\"\necosystem = \"rust\"\nref = \"v1.0.200\"\nrepo = \"https://example.invalid/serde\"\n",
    );

    let out = pins::pins(&root.join("project"), Some(&root.join("docs.toml"))).unwrap();
    assert!(matches!(&out[0].outcome, Outcome::Ref(r) if r == "v1.0.200"));
    assert_eq!(out[0].declared, Evidence::Declared);
}

#[test]
fn an_invalid_ref_is_unresolved_not_asserted() {
    // A ref the manifest accepts but `names::checkout_dir` would later reject
    // must not be printed as a pin: the table would state something `docm
    // info` refuses to serve.
    let root = common::unique_tmp("pins-bad-ref");
    cargo_project(&root, "[config]\nroot = true\n");
    write(
        &root.join("docs.toml"),
        "[[libs]]\nname = \"serde\"\necosystem = \"rust\"\nref = \"v1~2\"\nrepo = \"https://example.invalid/serde\"\n",
    );

    let out = pins::pins(&root.join("project"), Some(&root.join("docs.toml"))).unwrap();
    match &out[0].outcome {
        Outcome::Unresolved(reason) => assert!(reason.contains('~'), "{reason}"),
        other => panic!("expected unresolved, got {other:?}"),
    }
}

#[test]
fn a_project_docs_section_marks_its_entries_project_scoped() {
    let root = common::unique_tmp("pins-scope");
    cargo_project(
        &root,
        "[config]\nroot = true\n\n[[docs.libs]]\nname = \"godot\"\necosystem = \"git\"\nref = \"4.3-stable\"\nrepo = \"https://example.invalid/godot\"\n",
    );
    write(
        &root.join("docs.toml"),
        "[[libs]]\nname = \"serde\"\necosystem = \"rust\"\nrepo = \"https://example.invalid/serde\"\n",
    );

    let out = pins::pins(&root.join("project"), Some(&root.join("docs.toml"))).unwrap();
    let names: Vec<&str> = out.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["godot", "serde"], "alphabetical");
    assert!(out[0].project_scoped, "godot comes from the project devkit.toml");
    assert!(!out[1].project_scoped, "serde comes from the global manifest");
    assert_eq!(out[0].declared, Evidence::Unknown, "git has no importer to ask");
}

#[test]
fn one_ecosystem_failing_leaves_the_others_rendered() {
    let root = common::unique_tmp("pins-fail-soft");
    cargo_project(&root, "[config]\nroot = true\n");
    // A JS lockfile that will not parse, beside a working Cargo project.
    write(&root.join("project/package.json"), r#"{"name":"app"}"#);
    write(&root.join("project/pnpm-lock.yaml"), "{{{ not yaml");
    write(
        &root.join("docs.toml"),
        "[[libs]]\nname = \"kysely\"\necosystem = \"js\"\nrepo = \"https://example.invalid/kysely\"\n\n[[libs]]\nname = \"serde\"\necosystem = \"rust\"\nrepo = \"https://example.invalid/serde\"\n",
    );

    let out = pins::pins(&root.join("project"), Some(&root.join("docs.toml"))).unwrap();
    assert!(matches!(out[0].outcome, Outcome::Unresolved(_)), "{:?}", out[0].outcome);
    assert_eq!(out[0].declared, Evidence::Unknown);
    assert!(matches!(out[1].outcome, Outcome::Version { .. }), "{:?}", out[1].outcome);
}

#[test]
fn a_broken_manifest_is_an_error_not_an_empty_list() {
    // Manifest discovery failing means the caller cannot know what to
    // resolve. `docm list --project` must exit non-zero rather than print an
    // empty listing that reads as "no libraries".
    let root = common::unique_tmp("pins-broken-manifest");
    cargo_project(&root, "[config]\nroot = true\n");
    write(&root.join("docs.toml"), "this is not toml [[[");

    assert!(pins::pins(&root.join("project"), Some(&root.join("docs.toml"))).is_err());
}

#[test]
fn an_unresolved_reason_is_one_line() {
    // The `undeclared` diagnostic is three lines; that belongs in `docm info`,
    // not in injected context. Unresolved carries `{err}`, never `{err:#}`.
    let root = common::unique_tmp("pins-one-line");
    cargo_project(&root, "[config]\nroot = true\n");
    write(
        &root.join("docs.toml"),
        "[[libs]]\nname = \"kysely\"\necosystem = \"js\"\nrepo = \"https://example.invalid/kysely\"\n",
    );

    let out = pins::pins(&root.join("project"), Some(&root.join("docs.toml"))).unwrap();
    match &out[0].outcome {
        Outcome::Unresolved(reason) => assert!(!reason.contains('\n'), "{reason}"),
        other => panic!("expected unresolved, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-docs --test pins`
Expected: FAIL to compile — `unresolved import 'devkit_docs::pins'`.

- [ ] **Step 3: Write `pins.rs`**

Create `crates/devkit-docs/src/pins.rs`:

```rust
//! What this checkout pins, per registered library.
//!
//! Filesystem reads only: the merged docs manifest plus whatever lockfile the
//! importer graph consults. No clone, no fetch, no worktree, no cache lock —
//! so a session-start hook can call this on a cold machine.

use crate::importers::{self, Evidence, Undeclared};
use crate::manifest::{self, Ecosystem, LibEntry};
use crate::names;
use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The importer graph named a version. `workspace` is the directory whose
    /// manifest selected it, relative to the project root where it is under
    /// one; `lockfile` is the file that carried it.
    Version {
        version: String,
        workspace: PathBuf,
        lockfile: String,
    },
    /// A manual `ref` pin in the manifest. No lockfile is consulted.
    Ref(String),
    /// Nothing this readout can state. One line, already short enough to render.
    Unresolved(String),
    /// This workspace does not depend on the library.
    Undeclared,
}

#[derive(Debug, Clone)]
pub struct Pin {
    pub name: String,
    pub outcome: Outcome,
    /// Declared by a project's own `devkit.toml`, not the machine-wide
    /// catalog — evidence this library belongs to the checkout in hand.
    pub project_scoped: bool,
    /// What the importer graph can say about this workspace depending on the
    /// package. Computed separately from `outcome` because a `ref` pin
    /// short-circuits resolution and would otherwise carry no evidence in
    /// either direction.
    pub declared: Evidence,
}

/// Every registered library's pin for the checkout at `start`, alphabetical.
///
/// `Err` means the registrations could not be enumerated at all — a caller
/// must report that rather than print an empty listing. A single library
/// failing to resolve is data, and lands in that row's `Outcome`.
pub fn pins(start: &Path, global: Option<&Path>) -> Result<Vec<Pin>> {
    let discovered = manifest::discover(start, global)?;
    let global_path = global
        .map(Path::to_path_buf)
        .unwrap_or_else(manifest::global_docs_path);
    let project_root = discovered
        .project_devkit_toml
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf);

    let mut out: Vec<Pin> = discovered
        .manifest
        .libs
        .iter()
        .map(|entry| pin_for(start, entry, &global_path, project_root.as_deref()))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn pin_for(
    start: &Path,
    entry: &LibEntry,
    global_path: &Path,
    project_root: Option<&Path>,
) -> Pin {
    let project_scoped = entry.origin_file.as_deref() != Some(global_path);
    let package = entry.package_name();
    let inspection = match entry.ecosystem {
        Some(ecosystem) => Some(importers::inspect(start, ecosystem, &package)),
        None => None,
    };
    let declared = inspection
        .as_ref()
        .map(|i| i.evidence)
        .unwrap_or(Evidence::Unknown);

    let outcome = match entry.r#ref.as_deref() {
        // A ref wins over lockfile resolution, exactly as `resolve` orders it.
        // Validate it here: `discover` checks library names but not refs, and
        // resolution later rejects an invalid ref through `names::checkout_dir`
        // — so without this the table could state a pin `docm info` refuses.
        Some(pin) => match names::validate_ref(pin) {
            Ok(()) => Outcome::Ref(pin.to_string()),
            Err(error) => Outcome::Unresolved(format!("{error}")),
        },
        None => match (entry.ecosystem, inspection) {
            (None, _) => Outcome::Unresolved(
                "no ecosystem and no ref; add one with `docm add`".to_string(),
            ),
            (Some(Ecosystem::Git), _) => {
                Outcome::Unresolved("git entry with no ref pinned".to_string())
            }
            (Some(_), Some(inspection)) => match inspection.result {
                Ok(selection) => Outcome::Version {
                    version: selection.version,
                    workspace: relative_workspace(&selection.workspace, project_root),
                    lockfile: selection.lockfile,
                },
                Err(error) if error.downcast_ref::<Undeclared>().is_some() => Outcome::Undeclared,
                // Top-level message only: the `undeclared` diagnostic is three
                // lines, and that belongs in `docm info`, not injected context.
                Err(error) => Outcome::Unresolved(format!("{error}")),
            },
            (Some(_), None) => Outcome::Unresolved("no ecosystem resolved".to_string()),
        },
    };

    Pin {
        name: entry.name.clone(),
        outcome,
        project_scoped,
        declared,
    }
}

/// A workspace named the way a reader of this project sees it. Absolute paths
/// are noise in a table injected into a session that already knows its root.
fn relative_workspace(workspace: &Path, project_root: Option<&Path>) -> PathBuf {
    match project_root.and_then(|root| workspace.strip_prefix(root).ok()) {
        Some(relative) if relative.as_os_str().is_empty() => PathBuf::from("."),
        Some(relative) => relative.to_path_buf(),
        None => workspace.to_path_buf(),
    }
}
```

Add to `crates/devkit-docs/src/lib.rs`, keeping the module list alphabetical:

```rust
pub mod pins;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-docs --test pins`
Expected: PASS, 8 tests.

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-docs/src/pins.rs crates/devkit-docs/src/lib.rs crates/devkit-docs/tests/pins.rs
git commit -m "feat(docs): add pins readout for a checkout"
```

---

## Task 4: The relevance filter and the shared renderer

One renderer, used by `docm list --project` and by the brief. The filter is what stops the machine-wide catalog — which accumulates every library ever asked about, across every project — from appearing in every project's brief three times a session.

**Neither shortcut works.** "Render unless `Undeclared`" leaks: `Undeclared` is only ever produced by a lockfile check, and a ref pin never reaches one. "Render only on `Outcome::Version`" hides real dependencies: `resolve` checks `entry.ref` before consulting the importer, so a globally ref-pinned crate the workspace genuinely depends on yields `Ref`. The filter reads `declared`, never `outcome`.

**Files:**
- Modify: `crates/devkit-docs/src/pins.rs`
- Test: `crates/devkit-docs/tests/pins.rs` (append)

**Interfaces:**
- Consumes: `Pin`, `Outcome`, `Evidence` (Task 3).
- Produces, in `devkit_docs::pins`:
  - `pub struct Dropped { pub undeclared: usize, pub unknown: usize }` — `Debug, Default, Clone, Copy, PartialEq, Eq`
  - `pub fn relevant(pins: &[Pin]) -> (Vec<&Pin>, Dropped)`
  - `pub fn render(pins: &[Pin]) -> String` — table + footer, newline-terminated
  - `pub fn envelope(pins: &[Pin]) -> serde_json::Value`

- [ ] **Step 1: Write the failing tests**

Append to `crates/devkit-docs/tests/pins.rs`:

```rust
use devkit_docs::importers::Evidence as Ev;
use devkit_docs::pins::{Dropped, Pin};

fn pin(name: &str, outcome: Outcome, project_scoped: bool, declared: Ev) -> Pin {
    Pin { name: name.into(), outcome, project_scoped, declared }
}

fn version(v: &str, ws: &str, lock: &str) -> Outcome {
    Outcome::Version {
        version: v.into(),
        workspace: Path::new(ws).to_path_buf(),
        lockfile: lock.into(),
    }
}

#[test]
fn a_machine_wide_undeclared_pin_is_dropped_and_counted() {
    let all = vec![
        pin("kysely", version("0.28.17", "apps/web", "pnpm-lock.yaml"), false, Ev::Declared),
        pin("zod", Outcome::Undeclared, false, Ev::Undeclared),
        pin("mystery", Outcome::Unresolved("no lockfile".into()), false, Ev::Unknown),
    ];
    let (rows, dropped) = pins::relevant(&all);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "kysely");
    assert_eq!(dropped, Dropped { undeclared: 1, unknown: 1 });

    let text = pins::render(&all);
    assert!(text.contains("kysely"), "{text}");
    assert!(!text.contains("zod"), "{text}");
    assert!(
        text.contains("2 registered libraries not evidenced here (1 undeclared, 1 unknown)"),
        "{text}"
    );
    assert!(text.contains("see `docm list`"), "{text}");
}

#[test]
fn a_project_scoped_pin_renders_whatever_its_evidence() {
    let all = vec![
        pin("zod", Outcome::Undeclared, true, Ev::Undeclared),
        pin("godot", Outcome::Ref("4.3-stable".into()), true, Ev::Unknown),
    ];
    let (rows, dropped) = pins::relevant(&all);
    assert_eq!(rows.len(), 2);
    assert_eq!(dropped, Dropped::default());

    let text = pins::render(&all);
    assert!(text.contains("not declared by this workspace"), "{text}");
    assert!(text.contains("4.3-stable"), "{text}");
}

#[test]
fn a_machine_wide_ref_pin_the_workspace_declares_still_renders() {
    // The inverse of the drop rule, and it must hold at the same time: this is
    // the false negative an `Outcome::Version`-only filter would introduce.
    let all = vec![pin("serde", Outcome::Ref("v1.0.200".into()), false, Ev::Declared)];
    let (rows, _) = pins::relevant(&all);
    assert_eq!(rows.len(), 1);
    assert!(pins::render(&all).contains("ref"), "source column says ref");
}

#[test]
fn an_empty_relevant_set_says_so_explicitly() {
    let all = vec![pin("zod", Outcome::Undeclared, false, Ev::Undeclared)];
    let text = pins::render(&all);
    assert!(text.contains("no registered libraries"), "{text}");
    assert!(text.contains("1 registered library not evidenced here"), "{text}");
}

#[test]
fn a_pathological_cell_is_truncated_visibly() {
    let reason = "x".repeat(5_000);
    let all = vec![pin("huge", Outcome::Unresolved(reason.clone()), true, Ev::Unknown)];
    let text = pins::render(&all);
    assert!(text.contains('…'), "truncation marker present: {text}");
    assert!(text.len() < 4_500, "section stays inside its budget: {}", text.len());

    // The JSON envelope carries the untruncated value.
    let json = pins::envelope(&all).to_string();
    assert!(json.contains(&reason), "envelope keeps the full value");
}

#[test]
fn the_section_budget_truncates_whole_rows_with_a_marker() {
    let all: Vec<Pin> = (0..400)
        .map(|i| {
            pin(
                &format!("lib{i:04}"),
                version("1.2.3", "apps/web", "pnpm-lock.yaml"),
                true,
                Ev::Declared,
            )
        })
        .collect();
    let text = pins::render(&all);
    assert!(text.len() <= 4_096, "section budget: {}", text.len());
    assert!(text.contains("more (see `docm list --project`)"), "{text}");
    // Whole rows only: the last data row is intact.
    for line in text.lines().filter(|l| l.starts_with("lib")) {
        assert!(line.contains("1.2.3"), "clipped row: {line}");
    }
}

#[test]
fn control_and_bidi_characters_never_reach_the_table() {
    let all = vec![pin(
        "evil",
        Outcome::Unresolved("line\none\u{202e}reversed\u{7}".into()),
        true,
        Ev::Unknown,
    )];
    let text = pins::render(&all);
    assert!(!text.contains('\u{202e}'), "bidi override stripped");
    assert!(!text.contains('\u{7}'), "control char stripped");
    assert!(text.contains("line one"), "newline became a space: {text}");
}

#[test]
fn the_envelope_distinguishes_empty_from_unevidenced() {
    let none = pins::envelope(&[]);
    assert_eq!(none["pins"].as_array().unwrap().len(), 0);
    assert_eq!(none["dropped"]["undeclared"], 0);
    assert_eq!(none["dropped"]["unknown"], 0);

    let all = vec![pin("zod", Outcome::Undeclared, false, Ev::Unknown)];
    let some = pins::envelope(&all);
    assert_eq!(some["pins"].as_array().unwrap().len(), 0);
    assert_eq!(some["dropped"]["unknown"], 1);
}

#[test]
fn the_envelope_carries_the_discriminant_per_pin() {
    let all = vec![pin(
        "kysely",
        version("0.28.17", "apps/web", "pnpm-lock.yaml"),
        false,
        Ev::Declared,
    )];
    let json = pins::envelope(&all);
    let row = &json["pins"][0];
    assert_eq!(row["name"], "kysely");
    assert_eq!(row["project_scoped"], false);
    assert_eq!(row["declared"], "declared");
    assert_eq!(row["outcome"]["kind"], "version");
    assert_eq!(row["outcome"]["version"], "0.28.17");
    assert_eq!(row["outcome"]["lockfile"], "pnpm-lock.yaml");
    assert_eq!(row["outcome"]["workspace"], "apps/web");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-docs --test pins`
Expected: FAIL to compile — `cannot find function 'relevant'`, `'render'`, `'envelope'`; `cannot find type 'Dropped'`.

- [ ] **Step 3: Implement the filter, the renderer, and the envelope**

Append to `crates/devkit-docs/src/pins.rs`:

```rust
/// Registrations the filter withheld, split by why. The split matters:
/// `undeclared` is a checked answer, `unknown` means the check could not run,
/// and a project seeing several `unknown` has a configuration problem rather
/// than a short dependency list.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Dropped {
    pub undeclared: usize,
    pub unknown: usize,
}

impl Dropped {
    pub fn total(&self) -> usize {
        self.undeclared + self.unknown
    }
}

/// A project-scoped registration always renders. A machine-wide one renders
/// only when the importer graph confirms this workspace declares the package —
/// read off `declared`, never off `outcome`.
pub fn relevant(pins: &[Pin]) -> (Vec<&Pin>, Dropped) {
    let mut rows = Vec::new();
    let mut dropped = Dropped::default();
    for pin in pins {
        if pin.project_scoped {
            rows.push(pin);
            continue;
        }
        match pin.declared {
            Evidence::Declared => rows.push(pin),
            Evidence::Undeclared => dropped.undeclared += 1,
            Evidence::Unknown => dropped.unknown += 1,
        }
    }
    (rows, dropped)
}

/// `ui::table` bounds line width, not total size: wrapping a 40 KB cell across
/// 100 columns yields 400 lines, not a truncation. These bound bytes.
const CELL_BUDGET: usize = 200;
const SECTION_BUDGET: usize = 4096;
/// Reserved out of `SECTION_BUDGET` so the marker row never competes with the
/// rows it is reporting on.
const MARKER_RESERVE: usize = 64;

/// The §5 table plus the dropped-count footer. Newline-terminated.
pub fn render(pins: &[Pin]) -> String {
    let (relevant_pins, dropped) = relevant(pins);
    let rows: Vec<[String; 3]> = relevant_pins.iter().map(|pin| row(pin)).collect();

    let mut budget = SECTION_BUDGET.saturating_sub(MARKER_RESERVE);
    let mut shown = 0usize;
    for row in &rows {
        let cost = row.iter().map(String::len).sum::<usize>() + 3;
        if cost > budget {
            break;
        }
        budget -= cost;
        shown += 1;
    }

    let mut out = String::new();
    if rows.is_empty() {
        out.push_str("no registered libraries are evidenced in this checkout\n");
    } else {
        let mut table = devkit_common::ui::table(&["LIBRARY", "VERSION", "SOURCE"]);
        for row in rows.iter().take(shown) {
            table.add_row(row.to_vec());
        }
        if shown < rows.len() {
            table.add_row(vec![
                format!("… {} more", rows.len() - shown),
                String::new(),
                "see `docm list --project`".to_string(),
            ]);
        }
        out.push_str(&format!("{table}\n"));
    }
    if let Some(footer) = footer(&dropped) {
        out.push_str(&footer);
        out.push('\n');
    }
    out
}

fn row(pin: &Pin) -> [String; 3] {
    let (version, source) = match &pin.outcome {
        Outcome::Version {
            version,
            workspace,
            lockfile,
        } => (
            version.clone(),
            format!("{lockfile} ({})", workspace.display()),
        ),
        Outcome::Ref(git_ref) => (git_ref.clone(), "ref".to_string()),
        Outcome::Undeclared => (
            "—".to_string(),
            "not declared by this workspace".to_string(),
        ),
        Outcome::Unresolved(reason) => ("—".to_string(), reason.clone()),
    };
    [
        cell(&pin.name),
        cell(&version),
        cell(&source),
    ]
}

/// A filtered listing must not read as an empty catalog: `skills/docs/SKILL.md`
/// tells an agent that a library absent from the listing is unregistered, and
/// against this view that inference is false.
fn footer(dropped: &Dropped) -> Option<String> {
    let total = dropped.total();
    if total == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if dropped.undeclared > 0 {
        parts.push(format!("{} undeclared", dropped.undeclared));
    }
    if dropped.unknown > 0 {
        parts.push(format!("{} unknown", dropped.unknown));
    }
    let noun = if total == 1 { "library" } else { "libraries" };
    Some(format!(
        "{total} registered {noun} not evidenced here ({}) — see `docm list`",
        parts.join(", ")
    ))
}

/// Sanitize then bound one cell. Values come from checked-in manifests and
/// land in agent context; control and bidi characters are the hazard being
/// closed, and the reason text is lockfile-derived so it gets the same
/// treatment as names, versions and refs.
fn cell(value: &str) -> String {
    let mut clean = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\n' | '\r' | '\t' => clean.push(' '),
            '\u{061c}' | '\u{200e}' | '\u{200f}' => {}
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => {}
            c if c.is_control() => {}
            c => clean.push(c),
        }
    }
    if clean.len() <= CELL_BUDGET {
        return clean;
    }
    let mut end = CELL_BUDGET - '…'.len_utf8();
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    clean.truncate(end);
    clean.push('…');
    clean
}

/// The `--project --json` envelope. An array cannot distinguish an empty
/// catalog from a catalog whose every entry went unevidenced, and those call
/// for opposite responses: register something, versus find out why the check
/// could not run. Values here are untruncated — truncation is a property of
/// the context-injection rendering, not of the data.
pub fn envelope(pins: &[Pin]) -> serde_json::Value {
    let (relevant_pins, dropped) = relevant(pins);
    let rows: Vec<serde_json::Value> = relevant_pins
        .iter()
        .map(|pin| {
            serde_json::json!({
                "name": pin.name,
                "project_scoped": pin.project_scoped,
                "declared": match pin.declared {
                    Evidence::Declared => "declared",
                    Evidence::Undeclared => "undeclared",
                    Evidence::Unknown => "unknown",
                },
                "outcome": outcome_json(&pin.outcome),
            })
        })
        .collect();
    serde_json::json!({
        "pins": rows,
        "dropped": {
            "undeclared": dropped.undeclared,
            "unknown": dropped.unknown,
        },
    })
}

fn outcome_json(outcome: &Outcome) -> serde_json::Value {
    match outcome {
        Outcome::Version {
            version,
            workspace,
            lockfile,
        } => serde_json::json!({
            "kind": "version",
            "version": version,
            "lockfile": lockfile,
            "workspace": workspace.to_string_lossy().replace('\\', "/"),
        }),
        Outcome::Ref(git_ref) => serde_json::json!({ "kind": "ref", "ref": git_ref }),
        Outcome::Unresolved(reason) => {
            serde_json::json!({ "kind": "unresolved", "reason": reason })
        }
        Outcome::Undeclared => serde_json::json!({ "kind": "undeclared" }),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-docs --test pins`
Expected: PASS, 17 tests.

If `the_section_budget_truncates_whole_rows_with_a_marker` fails on total length, the cause is `comfy-table`'s column padding, which the byte accounting above approximates with `+ 3`. Raise the per-row constant to the measured padding rather than the budget — the budget is fixed by the spec.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-docs/src/pins.rs crates/devkit-docs/tests/pins.rs
git commit -m "feat(docs): render the pins table"
```

---

## Task 5: `docm list --project`, the JSON envelope, and SKILL.md

The second caller. `docm list` today prints the merged catalog and cannot drop a global entry — holding libraries no lockfile declares is exactly what a machine-wide manifest is for. `--project` adds a view; it does not narrow the default.

**Files:**
- Modify: `src/bin/docm.rs:61-65` (the `List` variant), `src/bin/docm.rs:121` (dispatch), `src/bin/docm.rs:296` (`cmd_list`)
- Modify: `skills/docs/SKILL.md`
- Modify: `README.md`
- Test: `tests/docm_cli.rs` (append)

**Interfaces:**
- Consumes: `devkit_docs::pins::{pins, render, envelope}` (Tasks 3–4).
- Produces: the `docm list --project` and `docm list --project --json` surfaces. No library API.

- [ ] **Step 1: Write the failing tests**

Append to `tests/docm_cli.rs`, following the existing `Env` harness in that file (it redirects `HOME` and `XDG_DATA_HOME` into a temp tree):

```rust
#[test]
fn list_project_filters_to_the_checkout_and_counts_what_it_dropped() {
    let env = Env::new("list-project");
    // Two registered libraries; only one is declared by this project.
    std::fs::write(
        env.project.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde = \"1.0.200\"\n",
    )
    .unwrap();
    std::fs::write(
        env.project.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"serde\"]\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.200\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"aa\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(env.home.join(".config/devkit")).unwrap();
    std::fs::write(
        env.home.join(".config/devkit/docs.toml"),
        format!(
            "[[libs]]\nname = \"serde\"\necosystem = \"rust\"\nrepo = \"{u}\"\n\n[[libs]]\nname = \"tokio\"\necosystem = \"rust\"\nrepo = \"{u}\"\n",
            u = env.upstream
        ),
    )
    .unwrap();

    let listing = env.docm(&["list", "--project"]);
    let text = String::from_utf8_lossy(&listing.stdout);
    assert!(listing.status.success(), "{text}");
    assert!(text.contains("serde"), "{text}");
    assert!(text.contains("1.0.200"), "{text}");
    assert!(!text.contains("tokio"), "machine-wide + undeclared is dropped: {text}");
    assert!(
        text.contains("1 registered library not evidenced here (1 undeclared)"),
        "{text}"
    );

    // The unfiltered catalog is unchanged: it still lists tokio.
    let catalog = String::from_utf8_lossy(&env.docm(&["list"]).stdout).into_owned();
    assert!(catalog.contains("tokio"), "{catalog}");
    assert!(!catalog.contains("not evidenced here"), "{catalog}");

    // --project composes with --json rather than conflicting with it.
    let json = env.docm(&["list", "--project", "--json"]);
    let body: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("valid JSON envelope");
    assert_eq!(body["pins"][0]["name"], "serde");
    assert_eq!(body["pins"][0]["declared"], "declared");
    assert_eq!(body["dropped"]["undeclared"], 1);
    assert_eq!(body["dropped"]["unknown"], 0);
}

#[test]
fn list_project_exits_non_zero_on_a_broken_manifest() {
    let env = Env::new("list-project-broken");
    std::fs::create_dir_all(env.home.join(".config/devkit")).unwrap();
    std::fs::write(env.home.join(".config/devkit/docs.toml"), "not toml [[[").unwrap();

    let listing = env.docm(&["list", "--project"]);
    assert!(!listing.status.success(), "a broken manifest is not an empty listing");
    let err = String::from_utf8_lossy(&listing.stderr);
    assert!(err.contains("docs.toml"), "{err}");
}

#[test]
fn list_project_and_info_select_the_same_version() {
    // The test that replaces "correct by construction" for the part
    // construction cannot guarantee: `pins` and `resolve` must name the same
    // version for the same library from the same cwd.
    let env = Env::new("list-project-agreement");
    std::fs::write(
        env.project.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nfixture = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        env.project.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"fixture\"]\n\n[[package]]\nname = \"fixture\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"aa\"\n",
    )
    .unwrap();
    // The upstream fixture repo carries tags v1.0.0 and v1.1.0, so resolution
    // finds a real tag for 1.0.0 rather than erroring.
    assert!(
        env.docm(&["add", "fixture", "--eco", "rust", "--repo", &env.upstream])
            .status
            .success()
    );

    let listing = String::from_utf8_lossy(&env.docm(&["list", "--project"]).stdout).into_owned();
    assert!(listing.contains("1.0.0"), "{listing}");

    let info = env.docm(&["info", "fixture"]);
    let info_text = String::from_utf8_lossy(&info.stdout);
    assert!(info.status.success(), "{}", String::from_utf8_lossy(&info.stderr));
    assert!(
        info_text.contains("1.0.0"),
        "pins and resolve disagree:\nlist --project: {listing}\ninfo: {info_text}"
    );
}

#[test]
fn a_ref_only_project_with_no_lockfile_renders_a_full_table() {
    // The lockfile-less case is the shape a git-ecosystem project has, not a
    // degradation: every row is a ref, and the table is not empty.
    let env = Env::new("list-project-refs");
    std::fs::write(
        env.project.join("devkit.toml"),
        format!(
            "[config]\nroot = true\n\n[[docs.libs]]\nname = \"godot\"\necosystem = \"git\"\nref = \"v1.0.0\"\nrepo = \"{}\"\n",
            env.upstream
        ),
    )
    .unwrap();

    let listing = env.docm(&["list", "--project"]);
    let text = String::from_utf8_lossy(&listing.stdout);
    assert!(listing.status.success(), "{text}");
    assert!(text.contains("godot"), "{text}");
    assert!(text.contains("v1.0.0"), "{text}");
    assert!(text.contains("ref"), "source column says ref: {text}");
    assert!(!text.contains("not evidenced here"), "nothing was dropped: {text}");
}
```

If `Env` has no `docm` helper, add one beside its existing command runners:

```rust
impl Env {
    fn docm(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_docm"))
            .args(args)
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_DATA_HOME", &self.data)
            .output()
            .unwrap()
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test docm_cli list_project && cargo test --test docm_cli a_ref_only_project`
Expected: FAIL — `error: unexpected argument '--project' found`.

- [ ] **Step 3: Add the flag and the branch**

In `src/bin/docm.rs`, the `List` variant:

```rust
    /// List registered libraries and their synced checkouts.
    List {
        #[arg(long)]
        json: bool,
        /// Show only what this checkout evidences, with the resolved version
        /// each manifest and lockfile names, instead of the whole catalog.
        #[arg(long)]
        project: bool,
    },
```

Dispatch: `Cmd::List { json, project } => cmd_list(json, project),`

And in `cmd_list`, before the existing body:

```rust
fn cmd_list(json: bool, project: bool) -> Result<()> {
    if project {
        // `pins` takes no lock and touches no cache. `docm`'s own `main` runs
        // `upgrade::run` before every subcommand, so this command is not
        // lock-free; the library function the brief calls is.
        let pins = devkit_docs::pins::pins(&cwd()?, None)?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&devkit_docs::pins::envelope(&pins))?
            );
        } else {
            print!("{}", devkit_docs::pins::render(&pins));
        }
        return Ok(());
    }
    let d = discovered()?;
    // …existing body unchanged…
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test docm_cli`
Expected: PASS, including the four new tests and every pre-existing one.

- [ ] **Step 5: Update SKILL.md**

In `skills/docs/SKILL.md`, three edits.

Frontmatter:
```yaml
allowed-tools: Bash(docm list --project)
```

The inline block and its lead-in, replacing the "Registered libraries — …" paragraph and the `!`docm list`` line:

```markdown
Libraries this checkout evidences — name, the version its manifests and
lockfiles name, and where that came from. A trailing count, when present, is
of registered libraries this checkout does not evidence; they are still
registered, and `docm list` shows them:

!`docm list --project`
```

Step 1, so "unregistered" stays a claim only the unfiltered listing can support:

```markdown
1. Identify which library the question is about and match it against the
   listing above. Absence from that listing means "not evidenced in this
   checkout", **not** "unregistered" — it is filtered to this project. Before
   concluding a library is unregistered, run `docm list` (unfiltered). If it
   is genuinely absent there: `docm add <package>` (registry lookup) or
   `docm add <git-url>`, always with `--notes "<workspace>: <why this
   version>"` recording which workspace's manifest/lockfile the version came
   from. Ask before adding with `--project` (that edits the repo's
   devkit.toml).
```

Add one rule to the `## Rules` list:

```markdown
- Comparing against another version is a lookup, not a recollection. The bare
  clone under the cache already holds every tag, so `git -C <checkout> show
  <other-tag>:<path>` reads the other version's file directly — do not answer
  "it changed in vX" from memory, and do not sync a second checkout for it.
```

- [ ] **Step 6: Document the flag**

In `README.md`, in the `docm` command table or list, beside the existing `list` entry:

```markdown
| `docm list --project` | Only what this checkout evidences, with the version each lockfile names. `--json` emits `{pins, dropped}`. |
```

- [ ] **Step 7: Run the full gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: green. Also confirm `cargo test -p devkit-docs --test importers_goldens` still passes untouched.

- [ ] **Step 8: Commit**

```bash
git add src/bin/docm.rs tests/docm_cli.rs skills/docs/SKILL.md README.md
git commit -m "feat(docm): add list --project"
```

**Milestone A is complete and useful on its own here.** `docm list --project` ships the content; the brief section is not required for it.

---

# Milestone C — cost

Before any hook wiring. The relevance probe runs for every machine-wide non-Git registration *including every one it drops*, and the whole reason the filter exists is that the global manifest accumulates every library ever asked about across every project. Per-session cost therefore scales with the catalog, not with the rows rendered.

## Task 6: `Selector` — parse each lockfile once

The only task that can silently regress `docm info`. Task 1's goldens plus the 25 tests in `crates/devkit-docs/tests/importers.rs` are the gate; both must pass unchanged.

**Two wrinkles that make this not a code move:**

1. **Eager parsing is a behavior change.** Today `js` chooses the manager first (`importers.rs:183`) and parses only the chosen lockfile, so a malformed *unselected* lockfile is silently ignored when `packageManager` names another. Parsing every present lockfile at construction turns that into a hard failure for a project that resolves fine today. Parse lazily and memoize.
2. **Cargo's split is a borrow problem.** `choose_member` returns `&LockPackage` borrowed from the parsed lock (`importers.rs:1088`); a context holding both is self-referential and will not compile. Change `choose_member` to return the index.

**A cached error must be replayable.** `anyhow::Error` is not `Clone`, and `Arc<anyhow::Error>` is not a drop-in either — `anyhow::Error` does not itself implement `std::error::Error`, so an `Arc` of it cannot be turned back into one. Cache `Arc<dyn std::error::Error + Send + Sync + 'static>` and re-wrap it per replay in a newtype whose `source()` delegates, which preserves `{}`, `{:#}`, and `Debug`.

**Files:**
- Modify: `crates/devkit-docs/src/importers.rs`
- Modify: `crates/devkit-docs/src/pins.rs` (`pins` holds one `Selector` per ecosystem)
- Test: `crates/devkit-docs/tests/importers.rs` (append)

**Interfaces:**
- Consumes: everything Task 2 produced.
- Produces, in `devkit_docs::importers`:
  - `pub struct Selector`
  - `pub fn Selector::new(start: &Path, ecosystem: Ecosystem) -> Result<Self>`
  - `pub fn Selector::inspect(&self, package: &str) -> Inspection`
  - `pub fn Selector::select(&self, package: &str) -> Result<Selection>`
  - Free `select` and `inspect` keep their signatures; both become projections.

- [ ] **Step 1: Write the failing tests**

Append to `crates/devkit-docs/tests/importers.rs`:

```rust
#[test]
fn a_selector_reads_each_lockfile_once() {
    // A count assertion without instrumentation: after the first package
    // forces the parse, the lockfile is deleted. Every later package must
    // still resolve, which is only possible if nothing re-reads the file.
    let root = common::unique_tmp("selector-one-read");
    std::fs::write(root.join("bun.lock"), BUN_LOCK).unwrap();
    let ws = root.join("apps/api");
    write_package_json(&ws, r#"{"name":"@app/api","dependencies":{"h3":"^1.15.5"}}"#);

    let selector = importers::Selector::new(&ws, Ecosystem::Js).unwrap();
    assert_eq!(selector.select("h3").unwrap().version, "1.15.11");

    std::fs::remove_file(root.join("bun.lock")).unwrap();
    for _ in 0..19 {
        assert_eq!(selector.select("h3").unwrap().version, "1.15.11");
    }
}

#[test]
fn a_malformed_unselected_lockfile_is_still_ignored() {
    // packageManager names pnpm; a corrupt bun.lock sits beside it. Eager
    // parsing would make this a hard failure for a project that resolves fine.
    let root = common::unique_tmp("selector-unselected-malformed");
    std::fs::write(root.join("bun.lock"), "{ this is not json").unwrap();
    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      h3:\n        specifier: ^1\n        version: 1.0.0\npackages:\n  h3@1.0.0: {}\n",
    )
    .unwrap();
    write_package_json(
        &root,
        r#"{"name":"root","packageManager":"pnpm@9.0.0","dependencies":{"h3":"^1"}}"#,
    );

    assert_eq!(
        importers::select(&root, Ecosystem::Js, "h3").unwrap().version,
        "1.0.0"
    );
    let selector = importers::Selector::new(&root, Ecosystem::Js).unwrap();
    assert_eq!(selector.select("h3").unwrap().version, "1.0.0");
}

#[test]
fn a_cached_parse_error_replays_identically() {
    // Two packages against one malformed lockfile: an `anyhow::Error` handed
    // out once would move or reconstruct, so the second caller must get the
    // same three renderings as the first.
    let root = common::unique_tmp("selector-error-replay");
    std::fs::write(root.join("pnpm-lock.yaml"), "\tnot: [valid: yaml").unwrap();
    write_package_json(&root, r#"{"name":"root","packageManager":"pnpm@9.0.0"}"#);

    let selector = importers::Selector::new(&root, Ecosystem::Js).unwrap();
    let first = selector.select("h3").unwrap_err();
    let second = selector.select("kysely").unwrap_err();
    assert_eq!(format!("{first}"), format!("{second}"));
    assert_eq!(format!("{first:#}"), format!("{second:#}"));
    assert_eq!(format!("{first:?}"), format!("{second:?}"));
}

#[test]
fn a_cargo_selector_resolves_two_packages_from_one_parse() {
    let root = common::unique_tmp("selector-cargo");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde = \"1.0.200\"\nanyhow = \"1.0.90\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("Cargo.lock"),
        format!(
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"anyhow\", \"serde\"]\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.200\"\nsource = \"{CARGO_REGISTRY}\"\nchecksum = \"aa\"\n\n[[package]]\nname = \"anyhow\"\nversion = \"1.0.90\"\nsource = \"{CARGO_REGISTRY}\"\nchecksum = \"bb\"\n"
        ),
    )
    .unwrap();

    let selector = importers::Selector::new(&root, Ecosystem::Rust).unwrap();
    assert_eq!(selector.select("serde").unwrap().version, "1.0.200");
    std::fs::remove_file(root.join("Cargo.lock")).unwrap();
    assert_eq!(selector.select("anyhow").unwrap().version, "1.0.90");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-docs --test importers`
Expected: FAIL to compile — `cannot find type 'Selector' in module 'importers'`.

- [ ] **Step 3: Add the cached-error machinery**

In `crates/devkit-docs/src/importers.rs`:

```rust
use std::cell::RefCell;
use std::sync::Arc;

/// A parse failure kept for replay. `anyhow::Error` is neither `Clone` nor
/// itself a `std::error::Error`, so neither it nor an `Arc` of it can be
/// handed out twice; a boxed std error can.
type CachedErr = Arc<dyn std::error::Error + Send + Sync + 'static>;

/// Re-wraps a cached error so `anyhow` can walk its cause chain again, which
/// is what keeps `{:#}` and `Debug` identical across replays.
#[derive(Debug, Clone)]
struct Replay(CachedErr);

impl std::fmt::Display for Replay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for Replay {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

fn cache_err(error: anyhow::Error) -> CachedErr {
    let boxed: Box<dyn std::error::Error + Send + Sync + 'static> = error.into();
    Arc::from(boxed)
}

fn replay(error: &CachedErr) -> anyhow::Error {
    anyhow::Error::new(Replay(Arc::clone(error)))
}
```

- [ ] **Step 4: Extract the JS context with lazy memoized parsing**

Split `js` (`importers.rs:163`) at its package-independent prefix (`164-182`: workspace discovery, lock-dir discovery, `rel_key`, `present`, `nearest_package_manager`) and per-package tail (`231`: `select_js_lock`):

```rust
enum ParsedLock {
    Bun(JsonValue),
    Pnpm(YamlValue),
    Npm(JsonValue),
}

struct JsContext {
    workspace: PathBuf,
    lock_dir: PathBuf,
    relative: String,
    package_manager: Option<String>,
    /// Lockfiles present in `lock_dir`, parsed on first use and cached.
    /// A parse failure is stored, not propagated: the ambiguity arm needs it
    /// as one lockfile's *outcome*, and the non-ambiguous path must keep
    /// ignoring lockfiles `packageManager` did not select.
    present: RefCell<Vec<(&'static str, Option<Result<ParsedLock, CachedErr>>)>>,
}

impl JsContext {
    fn new(start: &Path) -> Result<Self> {
        // Lines 164-182 of the current `js`, verbatim, ending with:
        let present = JS_LOCKS
            .iter()
            .copied()
            .filter(|(_, file)| lock_dir.join(file).is_file())
            .map(|(manager, _)| (manager, None))
            .collect::<Vec<_>>();
        Ok(JsContext {
            workspace,
            lock_dir,
            relative,
            package_manager,
            present: RefCell::new(present),
        })
    }

    fn present_managers(&self) -> Vec<&'static str> {
        self.present.borrow().iter().map(|(m, _)| *m).collect()
    }

    /// The parsed lockfile for `manager`, parsing on first use. The result —
    /// success or failure — is memoized, so a malformed lockfile yields the
    /// same error to every package that asks for it.
    fn parsed(&self, manager: &str) -> Result<ParsedLock> {
        let index = self
            .present
            .borrow()
            .iter()
            .position(|(m, _)| *m == manager)
            .with_context(|| format!("no {manager} lockfile in {}", self.lock_dir.display()))?;
        if self.present.borrow()[index].1.is_none() {
            let parsed = parse_js_lock(manager, &self.lock_dir).map_err(cache_err);
            self.present.borrow_mut()[index].1 = Some(parsed);
        }
        match self.present.borrow()[index].1.as_ref().expect("just filled") {
            Ok(lock) => Ok(lock.clone()),
            Err(error) => Err(replay(error)),
        }
    }
}
```

`ParsedLock` derives `Clone` so `parsed` can hand out a value while the cache keeps its copy. `JsonValue` and `YamlValue` are both `Clone`; a clone of an already-parsed tree is a memory copy, not a re-parse, and is what the whole task is buying. If profiling later shows the clone dominating, return a `Ref<'_, ParsedLock>` guard instead — do not undo the memoization.

`parse_js_lock` holds exactly the read-and-parse prefix each manager has today:

```rust
/// Read and parse one lockfile, gates included. Each arm is the prefix its
/// manager runs today, moved verbatim — same reads, same context strings,
/// same version gates, same order.
fn parse_js_lock(manager: &str, lock_dir: &Path) -> Result<ParsedLock> {
    match manager {
        "bun" => {
            let lock_path = lock_dir.join("bun.lock");
            let raw = std::fs::read_to_string(&lock_path)
                .with_context(|| format!("reading {}", lock_path.display()))?;
            let value = json5_ish(&raw)?;
            value
                .get("lockfileVersion")
                .and_then(JsonValue::as_u64)
                .context("bun.lock has no numeric `lockfileVersion`")?;
            Ok(ParsedLock::Bun(value))
        }
        "pnpm" => {
            let lock_path = lock_dir.join("pnpm-lock.yaml");
            let raw = std::fs::read_to_string(&lock_path)
                .with_context(|| format!("reading {}", lock_path.display()))?;
            let value: YamlValue =
                serde_yaml_ng::from_str(&raw).context("parsing pnpm-lock.yaml")?;
            let lockfile_version = value
                .get("lockfileVersion")
                .context("pnpm-lock.yaml has no `lockfileVersion`")?;
            match lockfile_version {
                YamlValue::String(version) if !version.is_empty() => {}
                YamlValue::Number(_) => {}
                _ => bail!("pnpm-lock.yaml `lockfileVersion` must be a string or numeric scalar"),
            }
            Ok(ParsedLock::Pnpm(value))
        }
        "npm" => {
            let lock_path = lock_dir.join("package-lock.json");
            let raw = std::fs::read_to_string(&lock_path)
                .with_context(|| format!("reading {}", lock_path.display()))?;
            let value: JsonValue =
                serde_json::from_str(&raw).context("parsing package-lock.json")?;
            let lockfile_version = value
                .get("lockfileVersion")
                .and_then(JsonValue::as_u64)
                .context("package-lock.json has no numeric `lockfileVersion`")?;
            if !matches!(lockfile_version, 2 | 3) {
                bail!("unsupported package-lock.json version {lockfile_version}; expected 2 or 3");
            }
            Ok(ParsedLock::Npm(value))
        }
        _ => bail!("unsupported JS package manager `{manager}`"),
    }
}
```

Each of `bun`, `pnpm`, `npm` loses its read-and-parse prefix and gains the parsed value as a parameter; **everything after the prefix stays exactly where it is, in the same order.** The candidate collection that precedes each declaration check exists so error messages can cite it (`importers.rs:479-486`, `728-750`, `883-891`) — do not move it past the check.

The manager-selection match (`importers.rs:183-229`) moves to `JsContext::choose`, reading `present_managers()` and `package_manager`. Its ambiguity arm calls `self.parsed(manager)` per present lockfile, which is what memoization makes affordable — and it keeps a throwaway `&mut Evidence` per probe (Task 2, Step 5).

- [ ] **Step 5: Make `choose_member` return an index**

Change the signature at `importers.rs:1088`:

```rust
fn choose_member(
    packages: &[LockPackage],
    format: PackageFormat,
    member: &str,
    manifest_version: Option<&str>,
    lock_dir: &Path,
    workspace: &Path,
) -> Result<usize>
```

Return the index rather than `&packages[i]` at each of its return sites; the single caller in `from_package_array` becomes `let own = &parsed.packages[own_index];`. This is what makes the Cargo/uv context storable: it holds the owned `PackageLock` plus a `usize`, not a reference into itself.

- [ ] **Step 6: Add the Cargo/uv context and the `Selector`**

```rust
struct TomlContext {
    lock_path: PathBuf,
    lock_dir: PathBuf,
    workspace: PathBuf,
    format: PackageFormat,
    member: String,
    parsed: PackageLock,
    own_index: usize,
}

enum Context {
    Js(JsContext),
    Toml(TomlContext),
}

pub struct Selector {
    context: Context,
}

impl Selector {
    /// Parse everything the ecosystem's lockfiles hold, once. Per-package
    /// traversal then runs against the parsed values. JS lockfiles are parsed
    /// on first use rather than here, so a malformed lockfile `packageManager`
    /// did not select stays ignored.
    pub fn new(start: &Path, ecosystem: Ecosystem) -> Result<Self> {
        let context = match ecosystem {
            Ecosystem::Js => Context::Js(JsContext::new(start)?),
            Ecosystem::Rust => Context::Toml(TomlContext::cargo(start)?),
            Ecosystem::Python => Context::Toml(TomlContext::uv(start)?),
            Ecosystem::Git => bail!("git entries resolve by ref, not by lockfile"),
        };
        Ok(Selector { context })
    }

    /// The full report. `select` is its projection, here as at the free
    /// function level.
    pub fn inspect(&self, package: &str) -> Inspection {
        let mut evidence = Evidence::Unknown;
        let result = match &self.context {
            Context::Js(context) => context.select(package, &mut evidence),
            Context::Toml(context) => context.select(package, &mut evidence),
        };
        Inspection { evidence, result }
    }

    pub fn select(&self, package: &str) -> Result<Selection> {
        self.inspect(package).result
    }
}

pub fn inspect(start: &Path, ecosystem: Ecosystem, package: &str) -> Inspection {
    match Selector::new(start, ecosystem) {
        Ok(selector) => selector.inspect(package),
        Err(error) => Inspection {
            evidence: Evidence::Unknown,
            result: Err(error),
        },
    }
}

pub fn select(start: &Path, ecosystem: Ecosystem, package: &str) -> Result<Selection> {
    inspect(start, ecosystem, package).result
}
```

`TomlContext::cargo` holds `cargo`'s prefix (`importers.rs:1355-1369`: manifest read, `[package] name`, `Cargo.lock` discovery, `cargo_manifest_version`) plus `from_package_array`'s prefix (`1280-1302`: read, `toml::from_str`, the Cargo version sweep, `choose_member`). `TomlContext::uv` holds `uv`'s prefix (`1382-1407`) and the same tail. `TomlContext::select` holds `from_package_array`'s per-package remainder (`1303-1351`), unchanged in order.

- [ ] **Step 7: Reuse one `Selector` per ecosystem in `pins`**

Batching is not free behind an unchanged call shape: `select(start, eco, pkg)` constructs a fresh `Selector` every call, so without this half the extraction buys nothing.

In `crates/devkit-docs/src/pins.rs`, replace the per-entry `importers::inspect` call with a per-ecosystem selector built once:

```rust
use crate::importers::Selector;
use std::collections::HashMap;

pub fn pins(start: &Path, global: Option<&Path>) -> Result<Vec<Pin>> {
    let discovered = manifest::discover(start, global)?;
    // …global_path and project_root as before…

    // One selector per ecosystem present, built lazily. A construction
    // failure is per-ecosystem data, not fatal: every library of that
    // ecosystem gets `Unresolved` carrying the construction error.
    let mut selectors: HashMap<Ecosystem, Result<Selector, String>> = HashMap::new();
    for entry in &discovered.manifest.libs {
        if let Some(ecosystem) = entry.ecosystem
            && ecosystem != Ecosystem::Git
        {
            selectors
                .entry(ecosystem)
                .or_insert_with(|| Selector::new(start, ecosystem).map_err(|e| format!("{e}")));
        }
    }

    let mut out: Vec<Pin> = discovered
        .manifest
        .libs
        .iter()
        .map(|entry| pin_for(entry, &selectors, &global_path, project_root.as_deref()))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}
```

`pin_for` takes the map instead of `start`, and its inspection becomes:

```rust
    let inspection = entry.ecosystem.and_then(|ecosystem| {
        match selectors.get(&ecosystem) {
            Some(Ok(selector)) => Some(selector.inspect(&package)),
            Some(Err(reason)) => Some(Inspection {
                evidence: Evidence::Unknown,
                result: Err(anyhow::anyhow!(reason.clone())),
            }),
            None => None,
        }
    });
```

`Ecosystem` needs `Hash` and `Eq` for the map key — add `Hash` to its derive in `crates/devkit-docs/src/manifest.rs:12` (it already derives `PartialEq, Eq`). `pins.rs` gains `use crate::importers::Inspection;` for the error arm above.

- [ ] **Step 8: Run every gate**

Run: `cargo test -p devkit-docs --test importers`
Expected: PASS — all 25 pre-existing tests plus the 4 new ones.

Run: `cargo test -p devkit-docs --test importers_goldens`
Expected: PASS **with no re-recording.** This is the whole point of Task 1. A diff means the refactor changed behavior: read the diff, find the reordered check or the eager parse, and fix the code. Do **not** re-record to make it pass.

Run: `cargo test -p devkit-docs --test pins && cargo test --test docm_cli`
Expected: PASS.

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: green.

- [ ] **Step 9: Commit**

```bash
git add crates/devkit-docs/src/importers.rs crates/devkit-docs/src/manifest.rs \
        crates/devkit-docs/src/pins.rs crates/devkit-docs/tests/importers.rs
git commit -m "perf(docs): parse each lockfile once per selector"
```

---

# Milestone B — the ambient carrier

## Task 7: `[brief]` config gating

The hooks ship enabled; config decides whether they produce anything. This is what makes the section reversible per user and per project rather than a build-or-don't decision.

Three properties, each non-negotiable:
- **The gate is read before the work.** The relevance probe scales with the accumulating global catalog, so a switch that suppressed output *after* paying for it would not switch anything off.
- **It reads `config::resolve`, not `load::load`.** `resolve` parses toml and merges layers; `load` additionally reads `doppler.yaml` and builds the app catalog, which is the part that fails on a docs-only project.
- **An unreadable config fails open to the defaults.** A malformed personal config costs an unwanted brief, never a silently withheld one.

**Files:**
- Modify: `crates/devkit-ports/src/config.rs:7-21` (the `Config` struct)
- Modify: `src/bin/devkit/brief.rs`
- Modify: `docs/configuration.md`
- Test: `crates/devkit-ports/src/config.rs` (its `#[cfg(test)] mod tests`), `src/bin/devkit/brief.rs` (its test module)

**Interfaces:**
- Consumes: `devkit_ports::config::resolve`.
- Produces: `devkit_ports::config::BriefConfig { enabled: bool, pins: bool }`, defaulting to `true`/`true`, reachable as `Config::brief`; and `brief::brief_config(cwd) -> BriefConfig` inside the binary.

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-ports/src/config.rs`'s test module:

```rust
#[test]
fn brief_defaults_on_and_the_project_layer_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home.toml");
    std::fs::write(&home, "[brief]\npins = true\n").unwrap();
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("devkit.toml"), "[brief]\npins = false\n").unwrap();

    let (cfg, _) = resolve_with_home(None, &project, Some(&home)).unwrap();
    assert!(cfg.brief.enabled, "enabled defaults on");
    assert!(!cfg.brief.pins, "the project layer wins");

    // A config with no [brief] table at all gets both defaults.
    let bare = tmp.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::write(bare.join("devkit.toml"), "[defaults]\n").unwrap();
    let (cfg, _) = resolve_with_home(None, &bare, None).unwrap();
    assert!(cfg.brief.enabled);
    assert!(cfg.brief.pins);
}
```

In `src/bin/devkit/brief.rs`'s test module:

```rust
#[test]
fn a_malformed_config_falls_back_to_the_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("devkit.toml"), "this is not toml [[[").unwrap();
    let cfg = brief_config(tmp.path());
    assert!(cfg.enabled, "an unreadable config costs a brief, never withholds one");
    assert!(cfg.pins);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-ports config::tests::brief_defaults && cargo test --bin devkit malformed_config`
Expected: FAIL to compile — `no field 'brief' on type 'Config'`, `cannot find function 'brief_config'`.

- [ ] **Step 3: Add `BriefConfig`**

In `crates/devkit-ports/src/config.rs`, on `Config`:

```rust
    #[serde(default)]
    pub brief: BriefConfig,
```

and beside `DaemonConfig`:

```rust
/// What `devkit brief` emits. Both default on: the hooks ship enabled and
/// config decides whether they produce anything, so turning the output off is
/// one line rather than a hook-wiring task.
#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct BriefConfig {
    /// The whole brief.
    pub enabled: bool,
    /// The library-versions section.
    pub pins: bool,
}

impl Default for BriefConfig {
    fn default() -> Self {
        BriefConfig {
            enabled: true,
            pins: true,
        }
    }
}
```

`BriefConfig` inherits the existing deep-merge layering for free, and `Provenance.origin` already records which layer won. No new resolution path.

- [ ] **Step 4: Read the gate in `brief.rs`, before any work**

In `src/bin/devkit/brief.rs`:

```rust
use devkit_ports::config::BriefConfig;

/// The `[brief]` settings for `cwd`, defaulting to on. `config::resolve` and
/// not `load::load`: `load` also reads doppler.yaml and builds the app
/// catalog, which is what fails on a docs-only project. An unreadable config
/// falls open to the defaults.
fn brief_config(cwd: &Path) -> BriefConfig {
    config::resolve(None, cwd)
        .map(|(cfg, _)| cfg.brief)
        .unwrap_or_default()
}
```

and at the top of `render`, before anything else:

```rust
fn render(cwd: &Path) -> Option<String> {
    let settings = brief_config(cwd);
    if !settings.enabled {
        return None;
    }
    // …existing body…
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p devkit-ports && cargo test --bin devkit`
Expected: PASS.

- [ ] **Step 6: Document it**

In `docs/configuration.md`, a new section beside the other config tables:

```markdown
### `[brief]`

What `devkit brief` emits. The plugin's hooks call it unconditionally; these
switches decide whether it produces anything.

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | The whole brief. `false` suppresses every section, and is read before any work is done. |
| `pins` | `true` | The library-versions section only. |

Set it in `~/.config/devkit/config.toml` as a personal default and override it
per project in that project's `devkit.toml`. A malformed `[brief]` table falls
back to these defaults rather than withholding the brief.
```

- [ ] **Step 7: Commit**

```bash
git add crates/devkit-ports/src/config.rs src/bin/devkit/brief.rs docs/configuration.md
git commit -m "feat(brief): gate output on [brief] config"
```

---

## Task 8: The brief's pins section and the `render` restructure

Two changes, both about not letting the new section break the old one.

**The pins section never gates the brief.** `render` returns `None` on any failure today; manifest discovery is a new failure surface, so a broken `docs.toml` would kill the apps, tasks and servers sections too.

**Pins render outside the devrun path entirely — and the blocker is `load`, not the membership gate.** `is_project_member` (`brief.rs:54`) already passes for any non-home `devkit.toml` layer, so a docs-only project satisfies it. The short-circuit is one line earlier: `load::load(None, cwd).ok()?` at `brief.rs:29` runs first.

Be precise about when that actually fires, because it decides which fixture proves the fix. `apps::catalog` never returns `Err` — it skips an unresolvable app with a `note:` on stderr — and a missing `doppler.yaml` is explicitly tolerated (`load.rs:19-22`). So `load::load` fails exactly when `config::resolve` fails: **no `devkit.toml` anywhere above the cwd and no `~/.config/devkit/config.toml`** (`config.rs:435-439`), or a layer that will not parse or deserialize. The case this restructure unlocks is therefore a repo with **no `devkit.toml` at all** whose lockfile declares a globally registered library — today it gets nothing, because `config::resolve` bails before any pins work happens.

**Which makes the relevance filter the membership signal for this half.** `pins_section` returns `None` when the relevant set is empty. Without that, any git repo with a readable global `docs.toml` would emit a brief announcing an empty library table — the section would leak into every repository, which is the exact failure the filter exists to prevent. A project-scoped registration is always relevant, so the docs-only project's `zod`-style "not declared by this workspace" row still renders; a machine-wide catalog that this checkout evidences nothing from renders nothing at all.

**Files:**
- Modify: `src/bin/devkit/brief.rs`
- Modify: root `Cargo.toml` (nothing — `devkit-docs` is already a dependency of the `devkit` package)
- Test: `tests/brief_pins.rs` (create), `src/bin/devkit/brief.rs` (test module)

**Interfaces:**
- Consumes: `devkit_docs::pins::{pins, render}` (Tasks 3–4), `brief_config` (Task 7).
- Produces, inside the binary:
  - `fn pins_section(cwd: &Path) -> Option<String>`
  - `fn pins_text(table: &str) -> String`
  - `fn devrun_sections(root: &str, cwd: &Path) -> Option<(String, String, Option<String>)>` returning `(apps, tasks, servers)`
  - `fn devrun_text(apps: &str, tasks: &str, servers: Option<&str>) -> String`

- [ ] **Step 1: Write the failing tests**

Create `tests/brief_pins.rs`:

```rust
//! End-to-end `devkit brief`: what a session-start hook actually receives.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

struct Project {
    root: PathBuf,
    home: PathBuf,
}

impl Project {
    /// A git checkout with a docs-only devkit.toml and a Cargo lockfile that
    /// declares `serde`, plus a global docs manifest registering it.
    fn docs_only(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("devkit-brief-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            Command::new("git").args(&args).current_dir(&repo).output().unwrap();
        }
        write(
            &repo.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde = \"1.0.200\"\n",
        );
        write(
            &repo.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"serde\"]\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.200\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"aa\"\n",
        );
        // Nothing devrun can use: no [defaults], no [apps].
        write(&repo.join("devkit.toml"), "[config]\nroot = true\n");
        write(
            &home.join(".config/devkit/docs.toml"),
            "[[libs]]\nname = \"serde\"\necosystem = \"rust\"\nrepo = \"https://example.invalid/serde\"\n",
        );
        Project { root: repo, home }
    }

    fn brief(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_devkit"))
            .arg("brief")
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            // Isolate the state dir so a test never reads or writes the
            // machine's real registry and watermarks.
            .env("XDG_STATE_HOME", self.home.join("state"))
            .env("COLUMNS", "100")
            .output()
            .unwrap()
    }

    fn docm(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_docm"))
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_STATE_HOME", self.home.join("state"))
            .env("COLUMNS", "100")
            .output()
            .unwrap()
    }

    fn set_config(&self, body: &str) {
        write(&self.root.join("devkit.toml"), body);
    }
}

#[test]
fn a_repo_with_no_devkit_toml_renders_pins() {
    // The case `load::load(..).ok()?` silently killed. `config::resolve` bails
    // when there is no devkit.toml above the cwd and no personal config, so
    // today this repo gets no brief at all — even though its lockfile declares
    // a globally registered library.
    let project = Project::docs_only("no-devkit-toml");
    std::fs::remove_file(project.root.join("devkit.toml")).unwrap();

    let out = project.brief(&[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "brief never fails: {out:?}");
    assert!(text.contains("serde"), "{text}");
    assert!(text.contains("1.0.200"), "{text}");
}

#[test]
fn an_unrelated_repo_stays_silent() {
    // The inverse, and it must hold at the same time: the machine-wide catalog
    // accumulates every library ever asked about, so a checkout that evidences
    // none of them gets no section — not an empty one.
    let project = Project::docs_only("unrelated-repo");
    std::fs::remove_file(project.root.join("devkit.toml")).unwrap();
    std::fs::remove_file(project.root.join("Cargo.lock")).unwrap();
    std::fs::remove_file(project.root.join("Cargo.toml")).unwrap();

    let out = project.brief(&[]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "{:?}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn a_docs_only_project_renders_pins() {
    // A devkit.toml with a [docs] section and nothing devrun can use.
    let project = Project::docs_only("docs-only");
    let out = project.brief(&[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "brief never fails: {out:?}");
    assert!(text.contains("serde"), "{text}");
    assert!(text.contains("1.0.200"), "{text}");
    assert!(text.contains("docm info"), "the caveat prose is present: {text}");
}

#[test]
fn a_broken_docs_manifest_leaves_the_rest_of_the_brief() {
    let project = Project::docs_only("broken-manifest");
    write(
        &project.home.join(".config/devkit/docs.toml"),
        "not toml [[[",
    );
    project.set_config("[config]\nroot = true\n\n[tasks.check]\nrun = \"cargo test\"\ndescription = \"tests\"\n");

    let out = project.brief(&[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(text.contains("check"), "tasks still render: {text}");
    assert!(!text.contains("Library versions"), "the pins section is omitted: {text}");
}

#[test]
fn brief_enabled_false_suppresses_everything() {
    let project = Project::docs_only("gate-off");
    project.set_config("[config]\nroot = true\n\n[brief]\nenabled = false\n");
    let out = project.brief(&[]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "{:?}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn brief_pins_false_suppresses_only_that_section() {
    let project = Project::docs_only("pins-off");
    project.set_config("[config]\nroot = true\n\n[brief]\npins = false\n\n[tasks.check]\nrun = \"cargo test\"\ndescription = \"tests\"\n");
    let out = project.brief(&[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("check"), "{text}");
    assert!(!text.contains("serde"), "{text}");
}

#[test]
fn the_gate_precedes_the_work() {
    // With enabled = false, no manifest is discovered and no importer runs:
    // point the config at a manifest whose resolution would fail loudly and
    // observe silence and exit 0.
    let project = Project::docs_only("gate-first");
    write(&project.home.join(".config/devkit/docs.toml"), "not toml [[[");
    project.set_config("[config]\nroot = true\n\n[brief]\nenabled = false\n");
    let out = project.brief(&[]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty(), "{:?}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_hundred_column_render_is_bounded() {
    let project = Project::docs_only("width-bound");
    // A library whose SOURCE cell is a full unresolved sentence.
    write(
        &project.home.join(".config/devkit/docs.toml"),
        "[[libs]]\nname = \"kysely\"\necosystem = \"js\"\nrepo = \"https://example.invalid/kysely\"\n",
    );
    project.set_config("[config]\nroot = true\n\n[docs]\n\n[[docs.libs]]\nname = \"kysely\"\n");

    let out = project.brief(&[]);
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        assert!(line.chars().count() <= 100, "unbounded line: {line}");
    }
}

#[test]
fn both_callers_render_the_same_rows() {
    // One renderer, asserted rather than assumed: the brief's section and
    // `docm list --project` must agree row for row from the same cwd.
    let project = Project::docs_only("both-callers");
    let listing = String::from_utf8_lossy(&project.docm(&["list", "--project"]).stdout).into_owned();
    let brief = String::from_utf8_lossy(&project.brief(&[]).stdout).into_owned();

    let rows: Vec<&str> = listing
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert!(!rows.is_empty(), "{listing}");
    for row in rows {
        assert!(
            brief.contains(row.trim_end()),
            "brief is missing a row `docm list --project` printed:\n{row}\n--- brief ---\n{brief}"
        );
    }
}

#[test]
fn a_machine_wide_undeclared_library_never_reaches_the_brief() {
    // The /docs-accumulation guard, end to end: two registered libraries where
    // only one is declared. The undeclared one produces no row and does not
    // suppress the one that resolved.
    let project = Project::docs_only("accumulation-guard");
    write(
        &project.home.join(".config/devkit/docs.toml"),
        "[[libs]]\nname = \"serde\"\necosystem = \"rust\"\nrepo = \"https://example.invalid/serde\"\n\n[[libs]]\nname = \"tokio\"\necosystem = \"rust\"\nrepo = \"https://example.invalid/tokio\"\n",
    );

    let text = String::from_utf8_lossy(&project.brief(&[]).stdout).into_owned();
    assert!(text.contains("serde"), "{text}");
    assert!(!text.contains("tokio"), "{text}");
    assert!(text.contains("1 registered library not evidenced here"), "{text}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test brief_pins`
Expected: FAIL for the right reason — `a_repo_with_no_devkit_toml_renders_pins` gets empty stdout, because `config::resolve` bails and `load::load(..).ok()?` returns `None` before any pins work runs. Every test asserting on `serde` or `1.0.200` fails too, since the section does not exist yet.

- [ ] **Step 3: Restructure `render` and add the section**

Rewrite `render` and add the two section builders in `src/bin/devkit/brief.rs`:

```rust
fn render(cwd: &Path) -> Option<String> {
    let settings = brief_config(cwd);
    if !settings.enabled {
        return None;
    }
    let cwd_str = cwd.to_str()?;
    let root = devkit_common::cmd::git(&["rev-parse", "--show-toplevel"], cwd_str)
        .ok()?
        .trim()
        .to_string();

    // Pins are computed before `load`: a devkit.toml carrying [docs] and
    // nothing devrun can use must still produce a brief.
    let pins = settings.pins.then(|| pins_section(cwd)).flatten();
    let devrun = devrun_sections(&root, cwd);
    if pins.is_none() && devrun.is_none() {
        return None;
    }

    let mut out = String::new();
    out.push_str("## devkit project context\n\n");
    out.push_str(&format!(
        "This checkout ({root}) is a devkit-managed project: dev servers, ports, \
         canned tasks, and cross-session file locks are coordinated by the devkit \
         CLIs. Load the `using-devkit` skill before using them.\n\n"
    ));
    if let Some((apps, tasks, servers)) = devrun {
        out.push_str(&devrun_text(&apps, &tasks, servers.as_deref()));
    }
    if let Some(section) = pins {
        out.push_str(&section);
    }
    Some(out)
}

/// The library-versions section, or `None` when the manifest cannot be read or
/// this checkout evidences nothing. A broken `docs.toml` omits this section; it
/// never suppresses the rest. An empty relevant set is what keeps the section
/// out of unrelated repositories — the machine-wide catalog accumulates every
/// library ever asked about, and a checkout that evidences none of them is not
/// a project this section has anything to say about.
fn pins_section(cwd: &Path) -> Option<String> {
    let pins = devkit_docs::pins::pins(cwd, None).ok()?;
    let (relevant, _) = devkit_docs::pins::relevant(&pins);
    if relevant.is_empty() {
        return None;
    }
    Some(pins_text(&devkit_docs::pins::render(&pins)))
}

/// The caveat carried once, at O(1) rather than per row.
fn pins_text(table: &str) -> String {
    let mut out = String::from("\n### Library versions in this checkout\n\n");
    out.push_str(table);
    out.push_str(
        "\nThese are the versions this checkout's manifests and lockfiles name. \
         `docm info <lib>` resolves the matching source and reports the version it \
         actually serves. Answer questions about these libraries from those \
         checkouts; training-set recall is a different version.\n",
    );
    out
}

/// Apps, tasks and live servers, or `None` when this checkout is not a
/// devrun-configured project.
fn devrun_sections(root: &str, cwd: &Path) -> Option<(String, String, Option<String>)> {
    let loaded = load::load(None, cwd).ok()?;
    let home = config::home_config_path();
    if !is_project_member(root, &loaded.provenance.layers, home.as_deref(), &loaded.catalog) {
        return None;
    }
    Some((
        apps_line(&loaded.catalog),
        task::tasks_text(&task::list(&loaded.config)),
        live_servers(root),
    ))
}
```

`devrun_text` is the body of today's `render_text` from the `- \`devrun up …\`` bullet list onward — move it verbatim, minus the heading and preamble that `render` now owns. Update the existing `render_text_sections_and_optional_servers` unit test to call `devrun_text` with the same arguments and the same assertions, dropping the two that check the moved heading (`devkit project context`, `using-devkit`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test brief_pins && cargo test --bin devkit`
Expected: PASS, 10 integration tests plus the binary's unit tests.

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add src/bin/devkit/brief.rs tests/brief_pins.rs
git commit -m "feat(brief): add the library-pins section"
```

---

## Task 9: Emission modes, the snapshot, and the hook matrix

`--pins-only` and `--if-changed`, the canonical `BriefSnapshot` the watermark hashes, and the `hooks/hooks.json` matrix that finally gives all three invocations a caller.

**The watermark hashes a structured snapshot of the whole brief**, not rendered text and not the pins alone. Two failure modes bracket this: hashing the rendered string makes the watermark width-sensitive, so a terminal resize re-injects a brief whose content did not move; hashing only the pins silently suppresses a brief whose apps, tasks or servers changed while the pins held still.

**`AGE` is excluded, deliberately.** `status_table` computes it against `now()` (`registry.rs:671-674`), so hashing rendered server rows makes the digest change every second and `--if-changed` degenerates to always-changed. Hashing the raw registry rows instead misses `LISTENING`, which is probed rather than stored, so a server going down would not re-emit.

**One probe, two consumers.** `status_table` probes liveness itself, so constructing `ServerKey` and then calling it re-probes — and a server going down between the two makes the brief hash one state while injecting another, which is a watermark certifying text nobody was shown.

**Files:**
- Modify: `src/bin/devkit/main.rs` (the `Brief` variant and its dispatch)
- Modify: `src/bin/devkit/brief.rs`
- Modify: `crates/devkit-ports/src/registry.rs:671`
- Create: `hooks/brief`, `hooks/brief.ps1`
- Modify: `hooks/hooks.json`
- Test: `tests/brief_pins.rs` (append), `crates/devkit-ports/src/registry.rs` (test module)

**Interfaces:**
- Consumes: everything from Tasks 3, 4, 7, 8.
- Produces:
  - `devkit_ports::registry::listening_view(&Data, Option<&str>) -> BTreeMap<u16, bool>`
  - `devkit_ports::registry::status_table_with(&Data, Option<&str>, &BTreeMap<u16, bool>) -> String`
  - `devkit brief --pins-only`, `devkit brief --if-changed`
  - `hooks/brief` + `hooks/brief.ps1`, invoked as `run-hook.cmd brief [flags]`

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-ports/src/registry.rs`'s test module:

```rust
#[test]
fn status_table_renders_from_a_supplied_listening_view() {
    let mut data = Data::default();
    data.entries.insert(
        4100,
        Entry {
            app: "api".into(),
            holder: "/w/root".into(),
            role: Role::Issue,
            pid: Some(42),
            logfile: None,
            ts: now(),
        },
    );
    let view: std::collections::BTreeMap<u16, bool> = [(4100u16, true)].into_iter().collect();
    let text = status_table_with(&data, Some("/w/root"), &view);
    assert!(text.contains("4100"), "{text}");
    assert!(text.contains("yes"), "the supplied view wins over a re-probe: {text}");
}
```

Append to `tests/brief_pins.rs`:

```rust
fn brief_with_stdin(project: &Project, args: &[&str], stdin: &str, columns: &str) -> Output {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .arg("brief")
        .args(args)
        .current_dir(&project.root)
        .env("HOME", &project.home)
        .env("USERPROFILE", &project.home)
        .env("XDG_STATE_HOME", project.home.join("state"))
        .env("COLUMNS", columns)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn pins_only_emits_just_the_library_section() {
    let project = Project::docs_only("pins-only");
    project.set_config("[config]\nroot = true\n\n[tasks.check]\nrun = \"cargo test\"\ndescription = \"tests\"\n");
    let out = project.brief(&["--pins-only"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("serde"), "{text}");
    assert!(!text.contains("check"), "no tasks section: {text}");
    assert!(!text.contains("devrun up"), "no devrun preamble: {text}");
}

#[test]
fn if_changed_emits_once_per_session_and_ignores_width() {
    let project = Project::docs_only("watermark");
    let session = r#"{"session_id":"abc-123"}"#;

    let first = brief_with_stdin(&project, &["--if-changed"], session, "100");
    assert!(!first.stdout.is_empty(), "first emission");

    // Same content, different terminal width: the digest is over data, not
    // rendered text, so this must stay silent.
    let second = brief_with_stdin(&project, &["--if-changed"], session, "60");
    assert!(second.stdout.is_empty(), "{:?}", String::from_utf8_lossy(&second.stdout));

    // A brief whose tasks changed while its pins held still must emit again.
    project.set_config("[config]\nroot = true\n\n[tasks.check]\nrun = \"cargo test\"\ndescription = \"tests\"\n");
    let third = brief_with_stdin(&project, &["--if-changed"], session, "100");
    assert!(!third.stdout.is_empty(), "content changed, emit again");
}

#[test]
fn two_session_ids_do_not_share_a_watermark() {
    let project = Project::docs_only("watermark-sessions");
    let a = brief_with_stdin(&project, &["--if-changed"], r#"{"session_id":"a"}"#, "100");
    assert!(!a.stdout.is_empty());
    let b = brief_with_stdin(&project, &["--if-changed"], r#"{"session_id":"b"}"#, "100");
    assert!(!b.stdout.is_empty(), "a second session gets its own watermark");

    // Two ids differing only in characters an allowlist would strip must not
    // collide: the filename is a hash of the complete raw id.
    let x = brief_with_stdin(&project, &["--if-changed"], r#"{"session_id":"s/1"}"#, "100");
    let y = brief_with_stdin(&project, &["--if-changed"], r#"{"session_id":"s:1"}"#, "100");
    assert!(!x.stdout.is_empty());
    assert!(!y.stdout.is_empty());
}

#[test]
fn no_session_id_emits_every_time() {
    // Falling back to a per-cwd key makes concurrent sessions share one
    // watermark, so A → B → A would suppress A's re-injection even though B
    // displaced it. A duplicate brief is the acceptable failure.
    let project = Project::docs_only("watermark-anonymous");
    assert!(!brief_with_stdin(&project, &["--if-changed"], "", "100").stdout.is_empty());
    assert!(!brief_with_stdin(&project, &["--if-changed"], "", "100").stdout.is_empty());
}

#[test]
fn an_unwritable_state_dir_fails_open() {
    use std::io::Write;
    let project = Project::docs_only("watermark-unwritable");
    // A session id is supplied, so this exercises the watermark path rather
    // than the no-id path.
    let run = || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_devkit"))
            .args(["brief", "--if-changed"])
            .current_dir(&project.root)
            .env("HOME", &project.home)
            .env("USERPROFILE", &project.home)
            // A regular file where the state dir should be, so every
            // create_dir_all and write beneath it fails.
            .env("XDG_STATE_HOME", project.root.join("Cargo.toml"))
            .env("COLUMNS", "100")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(br#"{"session_id":"unwritable"}"#)
            .unwrap();
        child.wait_with_output().unwrap()
    };
    assert!(!run().stdout.is_empty());
    let second = run();
    assert!(second.status.success());
    assert!(
        !second.stdout.is_empty(),
        "an unwritable watermark costs a duplicate brief, never a withheld one"
    );
}

#[test]
fn leaving_a_project_says_so_once() {
    let project = Project::docs_only("watermark-leaving");
    let session = r#"{"session_id":"leaving"}"#;
    assert!(!brief_with_stdin(&project, &["--if-changed"], session, "100").stdout.is_empty());

    // A directory outside any devkit project, same session: the earlier
    // brief's content is stale, and silence would leave it the most recent
    // thing the agent was told.
    let elsewhere = project.home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(["brief", "--if-changed"])
        .current_dir(&elsewhere)
        .env("HOME", &project.home)
        .env("USERPROFILE", &project.home)
        .env("XDG_STATE_HOME", project.home.join("state"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    // Without stdin the session id is absent, so this run cannot consult the
    // watermark: it emits nothing rather than a stale notice.
    assert!(out.status.success());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test brief_pins && cargo test -p devkit-ports registry::tests::status_table_renders`
Expected: FAIL — `error: unexpected argument '--pins-only' found`; `cannot find function 'status_table_with'`.

- [ ] **Step 3: Split the liveness probe out of `status_table`**

In `crates/devkit-ports/src/registry.rs`, replacing `status_table`:

```rust
/// One liveness probe per rendered row, taken once so a caller that both
/// hashes and renders sees a single consistent state.
pub fn listening_view(data: &Data, only_holder: Option<&str>) -> BTreeMap<u16, bool> {
    data.entries
        .iter()
        .filter(|(_, e)| only_holder.is_none_or(|h| e.holder == h))
        .map(|(port, _)| (*port, listening(*port)))
        .collect()
}

/// `status_table` against an already-taken liveness view.
pub fn status_table_with(
    data: &Data,
    only_holder: Option<&str>,
    view: &BTreeMap<u16, bool>,
) -> String {
    let mut t =
        devkit_common::ui::table(&["PORT", "APP", "ROLE", "HOLDER", "PID", "LISTENING", "AGE"]);
    let now = now();
    for (port, e) in &data.entries {
        if let Some(h) = only_holder
            && e.holder != h
        {
            continue;
        }
        let label = devkit_common::paths::leaf(&e.holder).unwrap_or(&e.holder);
        t.add_row(vec![
            port.to_string(),
            e.app.clone(),
            e.role.to_string(),
            label.to_string(),
            e.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            if *view.get(port).unwrap_or(&false) { "yes" } else { "no" }.to_string(),
            format!("{}s", now.saturating_sub(e.ts)),
        ]);
    }
    format!("{t}")
}

/// Render the port-status table shared by `portm status` and `devrun status`.
/// `only_holder = Some(h)` limits rows to that holder; `None` shows every port.
pub fn status_table(data: &Data, only_holder: Option<&str>) -> String {
    status_table_with(data, only_holder, &listening_view(data, only_holder))
}
```

- [ ] **Step 4: Add the flags**

In `src/bin/devkit/main.rs`:

```rust
    /// Print a compact project brief (apps, tasks, live servers, library
    /// versions) for the current checkout; silent outside a devkit-managed
    /// project. Intended for coding-agent session hooks.
    Brief {
        /// Emit only the library-versions section — what a post-compaction
        /// re-injection wants, without respending the context compaction
        /// just reclaimed.
        #[arg(long)]
        pins_only: bool,
        /// Print nothing when this session already received the same brief.
        /// Reads `session_id` from the hook's stdin JSON.
        #[arg(long)]
        if_changed: bool,
    },
```

Dispatch: `Cmd::Brief { pins_only, if_changed } => brief::run(pins_only, if_changed),`

- [ ] **Step 5: Add the snapshot and the watermark**

In `src/bin/devkit/brief.rs`:

```rust
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{IsTerminal, Read};

/// The canonical form `--if-changed` hashes: every section's identity, no
/// rendering and no clock. Hashing rendered text would make the watermark
/// terminal-width sensitive; hashing only the pins would suppress a brief
/// whose apps, tasks or servers changed while the pins held still.
struct BriefSnapshot {
    root: String,
    apps: Vec<String>,
    tasks: Vec<(String, String, String, String)>,
    servers: Vec<ServerKey>,
    pins: Vec<PinKey>,
}

/// Identity plus the probed listening state. `AGE` is excluded: it is computed
/// against `now()`, so including it makes the digest change every second.
struct ServerKey {
    port: u16,
    app: String,
    role: String,
    pid: String,
    listening: bool,
}

struct PinKey {
    name: String,
    project_scoped: bool,
    declared: &'static str,
    outcome: String,
}

impl BriefSnapshot {
    /// A stable byte string, so the digest does not depend on struct layout.
    fn canonical(&self) -> String {
        let mut out = format!("root\t{}\n", self.root);
        for app in &self.apps {
            out.push_str(&format!("app\t{app}\n"));
        }
        for (name, kind, app, description) in &self.tasks {
            out.push_str(&format!("task\t{name}\t{kind}\t{app}\t{description}\n"));
        }
        for s in &self.servers {
            out.push_str(&format!(
                "server\t{}\t{}\t{}\t{}\t{}\n",
                s.port, s.app, s.role, s.pid, s.listening
            ));
        }
        for p in &self.pins {
            out.push_str(&format!(
                "pin\t{}\t{}\t{}\t{}\n",
                p.name, p.project_scoped, p.declared, p.outcome
            ));
        }
        out
    }

    fn digest(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.canonical().hash(&mut hasher);
        hasher.finish()
    }
}

/// The session id from the hook's stdin JSON. `None` when there is no stdin
/// to read (an interactive run) or no id in it.
fn session_id() -> Option<String> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// The watermark file for `session`. The name is a hash of the complete raw
/// id: dropping disallowed characters is lossy, and two ids differing only in
/// what was dropped would collide onto one watermark.
fn watermark_path(session: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    session.hash(&mut hasher);
    devkit_common::paths::state_dir()
        .join("brief")
        .join(format!("{:016x}", hasher.finish()))
}
```

`run` becomes:

```rust
pub fn run(pins_only: bool, if_changed: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    // A brief is context injection, never a gate: any failure (no git, no
    // config, unreadable registry) means no output, exit 0.
    let settings = brief_config(&cwd);
    if !settings.enabled {
        return Ok(());
    }

    if !if_changed {
        let text = if pins_only {
            pins_only_text(&cwd, &settings)
        } else {
            render(&cwd, &settings)
        };
        if let Some(text) = text {
            print!("{text}");
        }
        return Ok(());
    }

    let session = session_id();
    let snapshot = snapshot(&cwd, &settings);
    let digest = snapshot.as_ref().map(|s| s.digest());
    let Some(session) = session else {
        // No id means emit without persisting: a shared per-cwd key would let
        // one session's brief suppress another's re-injection, and a withheld
        // brief is the worse failure.
        if let Some(text) = render(&cwd, &settings) {
            print!("{text}");
        }
        return Ok(());
    };

    let path = watermark_path(&session);
    let previous = std::fs::read_to_string(&path).ok();
    let current = digest.map(|d| format!("{d:016x}"));
    if previous.as_deref() == current.as_deref() && previous.is_some() {
        return Ok(());
    }
    // Fails open: an unreadable or unwritable state directory reports
    // "changed", costing a duplicate brief rather than withholding one.
    if let Some(current) = &current {
        let _ = std::fs::create_dir_all(path.parent().expect("watermark has a parent"));
        let _ = std::fs::write(&path, current);
    }
    match render(&cwd, &settings) {
        Some(text) => print!("{text}"),
        // Left the project: silence would leave the previous checkout's brief
        // as the most recent thing the agent was told.
        None if previous.is_some() => {
            let _ = std::fs::remove_file(&path);
            println!("## devkit project context\n\nThis directory is not a devkit-managed project; the earlier project brief no longer applies.");
        }
        None => {}
    }
    Ok(())
}
```

The remaining pieces. `render` gains the settings as a parameter rather than re-reading the config — Task 8's `render(cwd)` becomes `render(cwd, settings)`, and Task 8's `brief_config` call moves up into `run`:

```rust
/// Just the library-versions section, gated the same way the full brief's is.
fn pins_only_text(cwd: &Path, settings: &BriefConfig) -> Option<String> {
    settings.pins.then(|| pins_section(cwd)).flatten()
}

/// The same content `render` emits, in the canonical form the watermark
/// hashes. `None` when this checkout produces no brief at all.
fn snapshot(cwd: &Path, settings: &BriefConfig) -> Option<BriefSnapshot> {
    let cwd_str = cwd.to_str()?;
    let root = devkit_common::cmd::git(&["rev-parse", "--show-toplevel"], cwd_str)
        .ok()?
        .trim()
        .to_string();

    let pins = settings
        .pins
        .then(|| devkit_docs::pins::pins(cwd, None).ok())
        .flatten()
        .unwrap_or_default();
    let (relevant, _) = devkit_docs::pins::relevant(&pins);
    let pin_keys: Vec<PinKey> = relevant.iter().map(|pin| PinKey::of(pin)).collect();
    // `pins_section` omits an empty relevant set, so the snapshot must agree:
    // a digest that says "changed" for a brief `render` refuses to emit would
    // rewrite the watermark on every directory change through unrelated repos.

    let devrun = load::load(None, cwd).ok().filter(|loaded| {
        is_project_member(
            &root,
            &loaded.provenance.layers,
            config::home_config_path().as_deref(),
            &loaded.catalog,
        )
    });
    if pin_keys.is_empty() && devrun.is_none() {
        return None;
    }

    let (apps, tasks) = match &devrun {
        Some(loaded) => {
            let mut apps: Vec<String> = loaded
                .catalog
                .values()
                .map(|a| format!("{} ({})", a.name, a.path))
                .collect();
            apps.sort();
            let mut tasks: Vec<(String, String, String, String)> = task::list(&loaded.config)
                .into_iter()
                .map(|r| (r.name, r.kind.to_string(), r.app, r.description))
                .collect();
            tasks.sort();
            (apps, tasks)
        }
        None => (Vec::new(), Vec::new()),
    };

    // One probe, two consumers: `status_table` probes liveness itself, so
    // hashing here and rendering there would take two probes, and a server
    // going down between them would make the watermark certify text nobody
    // was shown.
    let mut servers = Vec::new();
    if let Ok(data) = registry::snapshot() {
        let view = registry::listening_view(&data, Some(&root));
        for (port, entry) in &data.entries {
            if entry.holder != root {
                continue;
            }
            servers.push(ServerKey {
                port: *port,
                app: entry.app.clone(),
                role: entry.role.to_string(),
                pid: entry.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                listening: *view.get(port).unwrap_or(&false),
            });
        }
    }
    servers.sort_by_key(|s| s.port);

    Some(BriefSnapshot {
        root,
        apps,
        tasks,
        servers,
        pins: pin_keys,
    })
}

impl PinKey {
    fn of(pin: &devkit_docs::pins::Pin) -> Self {
        use devkit_docs::importers::Evidence;
        use devkit_docs::pins::Outcome;
        PinKey {
            name: pin.name.clone(),
            project_scoped: pin.project_scoped,
            declared: match pin.declared {
                Evidence::Declared => "declared",
                Evidence::Undeclared => "undeclared",
                Evidence::Unknown => "unknown",
            },
            outcome: match &pin.outcome {
                Outcome::Version { version, workspace, lockfile } => {
                    format!("version:{version}:{lockfile}:{}", workspace.display())
                }
                Outcome::Ref(git_ref) => format!("ref:{git_ref}"),
                Outcome::Unresolved(reason) => format!("unresolved:{reason}"),
                Outcome::Undeclared => "undeclared".to_string(),
            },
        }
    }
}
```

`live_servers` (`brief.rs:86`) changes to take the same pre-probed view so the rendered table and the snapshot describe one probe:

```rust
fn live_servers(root: &str) -> Option<String> {
    let data = registry::snapshot().ok()?;
    let view = registry::listening_view(&data, Some(root));
    data.entries
        .values()
        .any(|e| e.holder == root)
        .then(|| registry::status_table_with(&data, Some(root), &view))
}
```

- [ ] **Step 6: Write the hook scripts**

`hooks/brief` (mode `0755`), following the `bootstrap-binaries` precedent — extensionless so Windows `.sh` auto-detection stays out of it:

```bash
#!/usr/bin/env bash
# Emit the devkit project brief when devkit is installed. Silent and exit 0
# otherwise, so a checkout without the binaries still starts a session.
# Hook payload arrives on stdin as JSON and is forwarded verbatim: --if-changed
# reads session_id from it.
set -u
command -v devkit >/dev/null 2>&1 || exit 0
exec devkit brief "$@"
```

`hooks/brief.ps1` — the twin for a Windows host with no Git-for-Windows bash:

```powershell
# Emit the devkit project brief when devkit is installed. Silent and exit 0
# otherwise. Stdin is forwarded so --if-changed can read session_id.
$ErrorActionPreference = 'SilentlyContinue'
if (-not (Get-Command devkit -ErrorAction SilentlyContinue)) { exit 0 }
$payload = [Console]::In.ReadToEnd()
$payload | & devkit brief @args
exit 0
```

`chmod +x hooks/brief` — and verify with `git ls-files -s hooks/brief` that it stages as mode `100755`, matching `hooks/bootstrap-binaries`.

- [ ] **Step 7: Wire the hook matrix**

In `hooks/hooks.json`, replace the bare `devkit brief` command and add the two events:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume|clear",
        "hooks": [
          {
            "type": "command",
            "command": "\"${CLAUDE_PLUGIN_ROOT}/hooks/run-hook.cmd\" bootstrap-binaries"
          },
          {
            "type": "command",
            "command": "\"${CLAUDE_PLUGIN_ROOT}/hooks/run-hook.cmd\" brief"
          }
        ]
      }
    ],
    "PostCompact": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "\"${CLAUDE_PLUGIN_ROOT}/hooks/run-hook.cmd\" brief --pins-only",
            "timeout": 10,
            "statusMessage": "Re-injecting devkit library versions"
          }
        ]
      }
    ],
    "CwdChanged": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "\"${CLAUDE_PLUGIN_ROOT}/hooks/run-hook.cmd\" brief --if-changed",
            "timeout": 10,
            "statusMessage": "Refreshing devkit project brief"
          }
        ]
      }
    ]
  }
}
```

Keep the existing `PreToolUse`, `SubagentStop`, and `SessionEnd` blocks exactly as they are — merge, do not replace.

- [ ] **Step 8: Verify the hook shim end to end**

Run the shim the way the runtime will, with a payload on stdin:

```bash
echo '{"session_id":"manual-check"}' | ./hooks/run-hook.cmd brief --if-changed
echo '{"session_id":"manual-check"}' | ./hooks/run-hook.cmd brief --if-changed   # silent
```
Expected: the first prints a brief (from a devkit project), the second prints nothing. Outside a devkit project both print nothing and exit 0.

Validate the JSON: `jq -e '.hooks.PostCompact[0].hooks[0].command' hooks/hooks.json`
Expected: exit 0, printing the command. A malformed `hooks.json` silently disables every hook in the file.

- [ ] **Step 9: Run the full gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: green — including `tests/parity.rs` and `tests/completions.rs`, which cover the CLI surface the new flags extend.

- [ ] **Step 10: Commit**

```bash
git add src/bin/devkit/main.rs src/bin/devkit/brief.rs crates/devkit-ports/src/registry.rs \
        hooks/brief hooks/brief.ps1 hooks/hooks.json tests/brief_pins.rs
git commit -m "feat(brief): add --pins-only and --if-changed"
```

---

## Landing

From outside the worktree, fast-forward `main` and remove the worktree:

```bash
git -C /home/lev/Git/lev/devkit switch main
git -C /home/lev/Git/lev/devkit merge --ff-only feat/brief-pins
git worktree remove ../devkit-worktrees/brief-pins
```

`feat/docs-pin` stays pushed and unmerged as the reference until a release makes it redundant.

---

## Unresolved questions

1. **`Selection.lockfile` for npm.** The spec's example renders `pnpm-lock.yaml (apps/web)`, and this plan sets npm's `lockfile` to `"package-lock.json"` — but npm's `source` prose already names the *install slot* (`apps/web/node_modules/h3; package-lock.json`), which is more informative and which a nested-copy resolution genuinely depends on. Should `lockfile` stay the plain file name (uniform across managers, as planned), or carry the slot for npm? Planned as the plain name; say if you want the slot.

2. **What "an explicit empty snapshot" means, in two places.** §7 says an empty result should be an explicit snapshot rather than an absent one, so moving between repositories does not leave the previous one's pins as the most recent thing the agent was told. I split that into two decisions, and only one is literally in the spec:
   - **`pins_section` returns `None` when the relevant set is empty.** Without it, any git repo with a readable global `docs.toml` emits a brief announcing an empty library table — the section leaks into every repository, which is what the filter exists to prevent. So the *section* is absent, not empty.
   - **The transition is announced instead.** Under `--if-changed` only, and only when a watermark for that session already exists, leaving a devkit project prints one line saying the earlier brief no longer applies. It never fires on a fresh session in an unrelated directory.

   Together those satisfy the intent — the agent learns the old context is stale — without putting an empty table in every repo. Confirm, or say if you want the empty section rendered instead.

3. **`DefaultHasher` for the watermark.** No hashing crate is in the workspace and none is needed: the digest only has to be stable within one session and one binary. A `devkit` upgrade mid-session invalidates every watermark, costing one duplicate brief. Acceptable, or do you want a stable hash (which means a new dependency)?

4. **`ParsedLock: Clone` in Task 6.** `JsContext::parsed` hands out a clone of the parsed tree so the cache keeps its copy. That is a memory copy per package, not a re-parse — cheap relative to the ~24 ms parse it replaces, but not free for a 597 KB lockfile. The alternative is returning a `Ref<'_, ParsedLock>` guard, which is more invasive in the per-manager signatures. Clone unless you say otherwise.

5. **Task 3 computes `declared` for every entry, including project-scoped ones**, where it never affects rendering — it is exposed in the JSON envelope, so it has a consumer, but for project-scoped ref pins it is pure cost until Task 6 lands. Keep it uniform, or skip the probe when `project_scoped && r#ref.is_some()`?
