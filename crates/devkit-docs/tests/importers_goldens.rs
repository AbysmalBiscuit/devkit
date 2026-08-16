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
    write(
        &root.join("apps/web/package.json"),
        r#"{"name":"@app/web"}"#,
    );
    write(
        &root.join("apps/api/package.json"),
        r#"{"name":"@app/api"}"#,
    );
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

/// A lockfile the YAML parser rejects outright. A parse failure is the one
/// class of outcome the fixtures above cannot reach, and it is the class that
/// travels through the selector's error cache — so its `{}`, `{:#}` and `{:?}`
/// belong in the record too.
fn pnpm_malformed(root: &Path) {
    write(&root.join("pnpm-lock.yaml"), "\tnot: [valid: yaml");
    write(
        &root.join("package.json"),
        r#"{"name":"root","packageManager":"pnpm@9.0.0"}"#,
    );
}

/// Rejected by the JSONC parser, so the failure carries `json5_ish`'s context
/// over the parser's own error.
fn bun_malformed(root: &Path) {
    write(&root.join("bun.lock"), "{ this is not json");
    write(&root.join("package.json"), r#"{"name":"root"}"#);
}

/// Parses as JSONC but fails the `lockfileVersion` gate — a bare context with
/// no cause beneath it, where the malformed cases carry a chain.
fn bun_bad_lockfile_version(root: &Path) {
    write(
        &root.join("bun.lock"),
        r#"{"lockfileVersion":"1","workspaces":{"":{"name":"root"}},"packages":{}}"#,
    );
    write(&root.join("package.json"), r#"{"name":"root"}"#);
}

fn cargo_malformed(root: &Path) {
    write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        &root.join("Cargo.lock"),
        "version = 4\n\n[[package\nname =\n",
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
    Case {
        name: "pnpm-malformed",
        build: pnpm_malformed,
        probes: &[(Ecosystem::Js, "", "h3")],
    },
    Case {
        name: "bun-malformed",
        build: bun_malformed,
        probes: &[(Ecosystem::Js, "", "h3")],
    },
    Case {
        name: "bun-bad-lockfile-version",
        build: bun_bad_lockfile_version,
        probes: &[(Ecosystem::Js, "", "h3")],
    },
    Case {
        name: "cargo-malformed",
        build: cargo_malformed,
        probes: &[(Ecosystem::Rust, "", "serde")],
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
                        scrub(common::message(&format!("{error:?}")), &root),
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
