use devkit_docs::importers;
use devkit_docs::manifest::Ecosystem;

#[allow(dead_code)]
mod common;

const BUN_LOCK: &str = r#"{
  // bun.lock accepts JSONC.
  "lockfileVersion": 1,
  "workspaces": {
    "": { "name": "root" },
    "apps/api": { "name": "@app/api", "dependencies": { "h3": "^1.15.5" } },
    "apps/web": { "name": "@app/web", "dependencies": {} },
  },
  "packages": {
    "h3": ["h3@1.15.11", "", {}, "sha512-a"],
    "h3-v2": ["h3@2.0.1-rc.20", "", { "dependencies": { "transitive": "^3.0.0" } }, "sha512-b"],
    "@compat/h3": ["h3@2.0.1", "", {}, "sha512-c"],
    "transitive": ["transitive@3.2.1", "", {}, "sha512-d"], /* block comment */
  },
}"#;

fn write_package_json(dir: &std::path::Path, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("package.json"), body).unwrap();
}

#[test]
fn a_bun_alias_never_wins_over_the_declared_dependency() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(root.join("bun.lock"), BUN_LOCK).unwrap();
    let ws = root.join("apps/api");
    write_package_json(
        &ws,
        r#"{"name":"@app/api","dependencies":{"h3":"^1.15.5"}}"#,
    );

    let selection = importers::select(&ws, Ecosystem::Js, "h3").unwrap();
    assert_eq!(selection.version, "1.15.11");
    assert_eq!(selection.workspace, ws);
    assert!(
        selection.source.contains("apps/api"),
        "{}",
        selection.source
    );
    assert!(
        selection.source.contains("bun.lock"),
        "{}",
        selection.source
    );
    assert!(
        selection.source.contains("2 other versions present"),
        "{}",
        selection.source
    );

    std::fs::write(root.join("bun.lock"), "").unwrap();
    let error = importers::select(&ws, Ecosystem::Js, "h3")
        .unwrap_err()
        .to_string();
    assert!(error.contains("empty"), "{error}");
}

#[test]
fn bun_candidates_decode_valid_tuple_variants_and_info_slots() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(
        root.join("bun.lock"),
        r#"{
  "lockfileVersion": 1,
  "workspaces": {
    "": { "name": "root", "dependencies": {} }
  },
  "packages": {
    "root": ["root@root:", { "bin": "bin/root" }],
    "local": ["local@workspace:packages/local"],
    "linked": ["linked@link:../linked", { "devDependencies": { "transitive": "^1" } }],
    "folder": ["folder@file:../folder", { "optionalDependencies": { "transitive": "^1" } }],
    "archive": ["archive@https://example.invalid/archive.tgz", { "peerDependencies": { "transitive": "^1" } }],
    "gitdep": ["gitdep@git+https://example.invalid/repo.git#abc", { "dependencies": { "transitive": "^1" } }, "github:example/repo#abc"],
    "h3": ["h3@1.15.11", "", { "dependencies": { "transitive": "^1" } }, "sha512-a"],
    "transitive": ["transitive@3.2.1", "", {}, "sha512-b"]
  }
}"#,
    )
    .unwrap();
    write_package_json(root, r#"{"name":"root","dependencies":{}}"#);

    let error = importers::select(root, Ecosystem::Js, "transitive")
        .unwrap_err()
        .to_string();
    assert!(error.contains("does not declare"), "{error}");
    for declarer in ["linked", "folder", "archive", "gitdep", "h3"] {
        assert!(error.contains(declarer), "missing {declarer}: {error}");
    }
    assert!(error.contains("3.2.1"), "{error}");
}

#[test]
fn bun_rejects_a_selected_non_registry_resolution() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(
        root.join("bun.lock"),
        r#"{
  "lockfileVersion": 1,
  "workspaces": {
    "": { "name": "root", "dependencies": { "local": "workspace:*" } }
  },
  "packages": {
    "local": ["local@workspace:packages/local"]
  }
}"#,
    )
    .unwrap();
    write_package_json(
        root,
        r#"{"name":"root","dependencies":{"local":"workspace:*"}}"#,
    );

    let error = importers::select(root, Ecosystem::Js, "local")
        .unwrap_err()
        .to_string();
    assert!(error.contains("non-registry"), "{error}");
    assert!(error.contains("workspace:packages/local"), "{error}");
}

const BUN_LOCK_EMBEDDED_AT: &str = r#"{
  "lockfileVersion": 1,
  "workspaces": {
    "": {
      "name": "root",
      "dependencies": {
        "pkg": "git+ssh://git@github.com/owner/repo.git#abc123",
        "@types/node": "^20.11.0"
      }
    }
  },
  "packages": {
    "pkg": ["pkg@git+ssh://git@github.com/owner/repo.git#abc123", {}, "github:owner/repo#abc123"],
    "@types/node": ["@types/node@20.11.0", "", {}, "sha512-x"]
  }
}"#;

fn write_embedded_at_workspace(root: &std::path::Path) {
    write_package_json(
        root,
        r#"{"name":"root","dependencies":{"pkg":"git+ssh://git@github.com/owner/repo.git#abc123","@types/node":"^20.11.0"}}"#,
    );
}

#[test]
fn a_git_ssh_resolution_with_an_embedded_at_sign_decodes_as_git() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(root.join("bun.lock"), BUN_LOCK_EMBEDDED_AT).unwrap();
    write_embedded_at_workspace(root);

    // The ssh-user marker inside the URL (`git@github.com`) must not be
    // mistaken for the name/resolution separator: the resolution should
    // decode whole, as a non-registry git dependency, not fail tuple arity.
    let error = importers::select(root, Ecosystem::Js, "pkg")
        .unwrap_err()
        .to_string();
    assert!(error.contains("non-registry"), "{error}");
    assert!(
        error.contains("git+ssh://git@github.com/owner/repo.git#abc123"),
        "{error}"
    );
    assert!(!error.contains("fields; expected"), "{error}");
}

#[test]
fn a_scoped_registry_name_still_resolves_beside_an_embedded_at_sign() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(root.join("bun.lock"), BUN_LOCK_EMBEDDED_AT).unwrap();
    write_embedded_at_workspace(root);

    let selection = importers::select(root, Ecosystem::Js, "@types/node").unwrap();
    assert_eq!(selection.version, "20.11.0");
}

#[test]
fn a_transitive_package_is_a_hard_error_that_lists_truthful_candidates() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(root.join("bun.lock"), BUN_LOCK).unwrap();
    let ws = root.join("apps/web");
    write_package_json(&ws, r#"{"name":"@app/web","dependencies":{}}"#);

    let error = importers::select(&ws, Ecosystem::Js, "h3")
        .unwrap_err()
        .to_string();
    assert!(error.contains("does not declare"), "{error}");
    assert!(error.contains("--ref"), "{error}");
    assert!(error.contains("1.15.11"), "{error}");
    assert!(error.contains("2.0.1"), "{error}");
    assert!(error.contains("declared by: apps/api"), "{error}");
    for fabricated in [
        "2.0.1 (required by apps/api)",
        "2.0.1-rc.20 (required by apps/api)",
    ] {
        assert!(!error.contains(fabricated), "{error}");
    }

    let tuple_error = importers::select(&ws, Ecosystem::Js, "transitive")
        .unwrap_err()
        .to_string();
    assert!(tuple_error.contains("declared by: h3-v2"), "{tuple_error}");
    assert!(!tuple_error.contains("required by h3-v2"), "{tuple_error}");

    let pnpm_root_dir = tempfile::tempdir().unwrap();
    let pnpm_root = pnpm_root_dir.path();
    std::fs::write(
        pnpm_root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  apps/api:\n    dependencies:\n      h3:\n        specifier: ^1.15.5\n        version: 1.15.11\n  apps/web: {}\npackages:\n  h3@1.15.11: {}\nsnapshots:\n  h3@1.15.11: {}\n",
    )
    .unwrap();
    let pnpm_ws = pnpm_root.join("apps/web");
    write_package_json(&pnpm_ws, r#"{"name":"web","dependencies":{}}"#);
    let pnpm_error = importers::select(&pnpm_ws, Ecosystem::Js, "h3")
        .unwrap_err()
        .to_string();
    assert!(pnpm_error.contains("1.15.11"), "{pnpm_error}");
    assert!(pnpm_error.contains("declared by: apps/api"), "{pnpm_error}");
    assert!(
        pnpm_error.contains("1.15.11 (required by apps/api)"),
        "{pnpm_error}"
    );

    let npm_root_dir = tempfile::tempdir().unwrap();
    let npm_root = npm_root_dir.path();
    std::fs::write(
        npm_root.join("package-lock.json"),
        r#"{"lockfileVersion":3,"packages":{"":{"name":"root"},"apps/api":{"dependencies":{"h3":"^1"}},"apps/web":{},"node_modules/h3":{"version":"1.15.11"}}}"#,
    )
    .unwrap();
    let npm_ws = npm_root.join("apps/web");
    write_package_json(&npm_ws, r#"{"name":"web","dependencies":{}}"#);
    let npm_error = importers::select(&npm_ws, Ecosystem::Js, "h3")
        .unwrap_err()
        .to_string();
    assert!(npm_error.contains("1.15.11"), "{npm_error}");
    assert!(npm_error.contains("declared by: apps/api"), "{npm_error}");
    assert!(!npm_error.contains("required by apps/api"), "{npm_error}");

    let uv_root_dir = tempfile::tempdir().unwrap();
    let uv_root = uv_root_dir.path();
    std::fs::write(
        uv_root.join("uv.lock"),
        r#"version = 1

[[package]]
name = "app"
version = "0.1.0"
dependencies = [{ name = "httpx" }]

[[package]]
name = "httpx"
version = "0.28.1"
dependencies = [{ name = "certifi", version = "2024.7.4" }]

[[package]]
name = "certifi"
version = "2024.7.4"
"#,
    )
    .unwrap();
    std::fs::write(
        uv_root.join("pyproject.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"httpx\"]\n",
    )
    .unwrap();
    let uv_error = importers::select(uv_root, Ecosystem::Python, "certifi")
        .unwrap_err()
        .to_string();
    assert!(uv_error.contains("2024.7.4"), "{uv_error}");
    assert!(uv_error.contains("declared by: httpx"), "{uv_error}");
    assert!(
        uv_error.contains("2024.7.4 (required by httpx)"),
        "{uv_error}"
    );
}

#[test]
fn a_pnpm_peer_qualified_locator_yields_a_bare_version() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  apps/api:\n    dependencies:\n      vitest:\n        specifier: ^3.2.0\n        version: 3.2.4(@types/node@25.5.0)\npackages:\n  vitest@3.2.4:\n    resolution: {integrity: sha512-x}\n",
    )
    .unwrap();
    let ws = root.join("apps/api");
    write_package_json(&ws, r#"{"name":"api","dependencies":{"vitest":"^3.2.0"}}"#);

    assert_eq!(
        importers::select(&ws, Ecosystem::Js, "vitest")
            .unwrap()
            .version,
        "3.2.4"
    );

    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: {}\nimporters:\n  apps/api:\n    dependencies:\n      vitest:\n        specifier: ^3.2.0\n        version: 3.2.4\n",
    )
    .unwrap();
    let error = importers::select(&ws, Ecosystem::Js, "vitest")
        .unwrap_err()
        .to_string();
    assert!(error.contains("lockfileVersion"), "{error}");
    assert!(error.contains("scalar"), "{error}");
}

#[test]
fn competing_js_lockfiles_use_the_nearest_valid_packagemanager() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(root.join("bun.lock"), BUN_LOCK).unwrap();
    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  apps/api:\n    dependencies:\n      h3:\n        specifier: ^1.15.5\n        version: 1.15.7\npackages:\n  h3@1.15.7: {}\n",
    )
    .unwrap();
    let ws = root.join("apps/api");
    write_package_json(
        &ws,
        r#"{"name":"@app/api","dependencies":{"h3":"^1.15.5"}}"#,
    );

    let error = importers::select(&ws, Ecosystem::Js, "h3")
        .unwrap_err()
        .to_string();
    assert!(error.contains("packageManager"), "{error}");
    assert!(error.contains("bun.lock"), "{error}");
    assert!(error.contains("pnpm-lock.yaml"), "{error}");
    assert!(error.contains("1.15.11"), "{error}");
    assert!(error.contains("1.15.7"), "{error}");

    std::fs::write(
        root.join("package.json"),
        r#"{"name":"root","packageManager":"bun@1.2.0"}"#,
    )
    .unwrap();
    assert_eq!(
        importers::select(&ws, Ecosystem::Js, "h3").unwrap().version,
        "1.15.11"
    );

    write_package_json(
        &ws,
        r#"{"name":"@app/api","packageManager":"pnpm@9.0.0","dependencies":{"h3":"^1.15.5"}}"#,
    );
    assert_eq!(
        importers::select(&ws, Ecosystem::Js, "h3").unwrap().version,
        "1.15.7"
    );

    for (body, expected) in [
        ("{", "parsing"),
        (r#"{"name":"api","packageManager":9}"#, "string"),
        (
            r#"{"name":"api","packageManager":"yarn@4.0.0"}"#,
            "unsupported packageManager",
        ),
    ] {
        write_package_json(&ws, body);
        let error = importers::select(&ws, Ecosystem::Js, "h3")
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn a_uv_fork_recording_two_versions_is_a_hard_error() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(
        root.join("uv.lock"),
        r#"version = 1

[[package]]
name = "app"
version = "0.1.0"
dependencies = [{ name = "httpx" }]

[[package]]
name = "httpx"
version = "0.27.0"

[[package]]
name = "httpx"
version = "0.28.1"
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"httpx\"]\n",
    )
    .unwrap();

    let error = importers::select(root, Ecosystem::Python, "httpx")
        .unwrap_err()
        .to_string();
    assert!(error.contains("0.27.0"), "{error}");
    assert!(error.contains("0.28.1"), "{error}");
    assert!(error.contains("fork"), "{error}");
}

#[test]
fn uv_selects_a_versionless_dynamic_member_by_local_source() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(
        root.join("uv.lock"),
        r#"version = 1

[[package]]
name = "app"
source = { editable = "." }
dependencies = [{ name = "httpx", version = "0.28.1" }]

[[package]]
name = "app"
version = "9.9.9"
source = { registry = "https://example.invalid/simple" }

[[package]]
name = "httpx"
version = "0.28.1"
source = { registry = "https://pypi.org/simple" }
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"app\"\ndynamic = [\"version\"]\ndependencies = [\"httpx\"]\n",
    )
    .unwrap();

    let selection = importers::select(root, Ecosystem::Python, "httpx").unwrap();
    assert_eq!(selection.version, "0.28.1");
    assert_eq!(selection.workspace, root);
}

#[test]
fn uv_dev_optional_and_dependency_groups_are_direct_dependencies() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(
        root.join("uv.lock"),
        r#"version = 1

[[package]]
name = "app"
version = "0.1.0"

[package.dev-dependencies]
dev = [{ name = "pytest" }]

[package.optional-dependencies]
speed = [{ name = "uvloop" }]

[package.dependency-groups]
lint = [{ name = "ruff" }]

[[package]]
name = "pytest"
version = "8.3.2"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "uvloop"
version = "0.21.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "ruff"
version = "0.9.1"
source = { registry = "https://pypi.org/simple" }
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    for (package, version) in [("pytest", "8.3.2"), ("uvloop", "0.21.0"), ("ruff", "0.9.1")] {
        assert_eq!(
            importers::select(root, Ecosystem::Python, package)
                .unwrap()
                .version,
            version
        );
    }

    let duplicate_dir = tempfile::tempdir().unwrap();
    let duplicate = duplicate_dir.path();
    std::fs::write(
        duplicate.join("uv.lock"),
        r#"version = 1

[[package]]
name = "app"
version = "0.1.0"
source = { editable = "." }
dependencies = [{ name = "httpx" }]

[[package]]
name = "app"
version = "0.1.0"
source = { registry = "https://example.invalid/simple" }
dependencies = [{ name = "rich" }]

[[package]]
name = "httpx"
version = "0.28.1"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "rich"
version = "13.9.4"
source = { registry = "https://pypi.org/simple" }
"#,
    )
    .unwrap();
    std::fs::write(
        duplicate.join("pyproject.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"httpx\"]\n",
    )
    .unwrap();
    assert_eq!(
        importers::select(duplicate, Ecosystem::Python, "httpx")
            .unwrap()
            .version,
        "0.28.1"
    );
    let error = importers::select(duplicate, Ecosystem::Python, "rich")
        .unwrap_err()
        .to_string();
    assert!(error.contains("does not declare"), "{error}");

    std::fs::write(
        duplicate.join("uv.lock"),
        r#"version = 1

[[package]]
name = "app"
version = "0.1.0"
dependencies = [{ name = "httpx" }]

[[package]]
name = "app"
version = "0.1.0"
dependencies = [{ name = "rich" }]

[[package]]
name = "httpx"
version = "0.28.1"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "rich"
version = "13.9.4"
source = { registry = "https://pypi.org/simple" }
"#,
    )
    .unwrap();
    let error = importers::select(duplicate, Ecosystem::Python, "httpx")
        .unwrap_err()
        .to_string();
    assert!(error.contains("ambiguous"), "{error}");
}

#[test]
fn a_cargo_member_gets_its_own_dependency_not_another_members() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"a\", \"b\"]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("Cargo.lock"),
        r#"version = 4

[[package]]
name = "a"
version = "0.1.0"
dependencies = ["serde"]

[[package]]
name = "b"
version = "0.1.0"

[[package]]
name = "serde"
version = "1.0.210"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#,
    )
    .unwrap();
    for (member, dependency) in [("a", "serde = \"1\"\n"), ("b", "")] {
        let dir = root.join(member);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{member}\"\nversion = \"0.1.0\"\n\n[dependencies]\n{dependency}"
            ),
        )
        .unwrap();
    }

    assert_eq!(
        importers::select(&root.join("a"), Ecosystem::Rust, "serde")
            .unwrap()
            .version,
        "1.0.210"
    );
    let error = importers::select(&root.join("b"), Ecosystem::Rust, "serde")
        .unwrap_err()
        .to_string();
    assert!(error.contains("1.0.210"), "{error}");
    assert!(error.contains("declared by: a"), "{error}");
    assert!(!error.contains("unspecified"), "{error}");
}

#[test]
fn a_cargo_edge_disambiguates_duplicate_package_and_member_names() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("Cargo.lock"),
        r#"version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = ["serde 1.0.210 (registry+https://github.com/rust-lang/crates.io-index)"]

[[package]]
name = "app"
version = "0.1.0"
source = "registry+https://example.invalid/index"
dependencies = ["serde 0.9.15"]

[[package]]
name = "serde"
version = "1.0.210"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "serde"
version = "0.9.15"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#,
    )
    .unwrap();

    assert_eq!(
        importers::select(root, Ecosystem::Rust, "serde")
            .unwrap()
            .version,
        "1.0.210"
    );

    std::fs::write(
        root.join("Cargo.lock"),
        r#"version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = ["serde 1.0.210"]

[[package]]
name = "app"
version = "0.1.0"
dependencies = ["serde 0.9.15"]

[[package]]
name = "serde"
version = "1.0.210"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "serde"
version = "0.9.15"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#,
    )
    .unwrap();
    let error = importers::select(root, Ecosystem::Rust, "serde")
        .unwrap_err()
        .to_string();
    assert!(error.contains("ambiguous"), "{error}");
}

#[test]
fn npm_resolves_the_nearest_nested_copy_walking_up_from_the_workspace() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(
        root.join("package-lock.json"),
        r#"{
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "root" },
    "apps/api": { "name": "@app/api", "dependencies": { "h3": "^1.0.0" } },
    "apps/api/node_modules/h3": { "version": "1.15.11", "resolved": "https://registry.npmjs.org/h3/-/h3-1.15.11.tgz" },
    "node_modules/h3": { "version": "2.0.1", "resolved": "https://registry.npmjs.org/h3/-/h3-2.0.1.tgz" }
  }
}"#,
    )
    .unwrap();
    let ws = root.join("apps/api");
    write_package_json(&ws, r#"{"name":"@app/api","dependencies":{"h3":"^1.0.0"}}"#);

    assert_eq!(
        importers::select(&ws, Ecosystem::Js, "h3").unwrap().version,
        "1.15.11"
    );
}

#[test]
fn a_pnpm_alias_resolves_and_non_registry_locators_are_rejected() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      h3-v2:\n        specifier: npm:h3@2.0.1\n        version: h3@2.0.1\npackages:\n  h3@2.0.1: {}\n",
    )
    .unwrap();
    write_package_json(
        root,
        r#"{"name":"root","dependencies":{"h3-v2":"npm:h3@2.0.1"}}"#,
    );

    assert!(importers::select(root, Ecosystem::Js, "h3").is_err());
    assert_eq!(
        importers::select(root, Ecosystem::Js, "h3-v2")
            .unwrap()
            .version,
        "2.0.1"
    );

    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      h3-v2:\n        specifier: npm:h3@2.0.1\n        version: ghost@2.0.1\npackages:\n  h3@2.0.1: {}\n",
    )
    .unwrap();
    let error = importers::select(root, Ecosystem::Js, "h3-v2")
        .unwrap_err()
        .to_string();
    assert!(error.contains("ghost@2.0.1"), "{error}");
    assert!(error.contains("package row"), "{error}");

    for locator in ["link:../h3", "file:../h3", "workspace:*"] {
        std::fs::write(
            root.join("pnpm-lock.yaml"),
            format!(
                "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      h3-v2:\n        specifier: npm:h3@2.0.1\n        version: {locator}\n"
            ),
        )
        .unwrap();
        let error = importers::select(root, Ecosystem::Js, "h3-v2")
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported"), "{error}");
        assert!(error.contains(locator), "{error}");
    }
}

#[test]
fn pnpm_rejects_a_direct_locator_without_a_target_row() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      h3:\n        specifier: ^9\n        version: 9.9.9\npackages:\n  h3@1.15.11: {}\n",
    )
    .unwrap();
    write_package_json(root, r#"{"name":"root","dependencies":{"h3":"^9"}}"#);

    let error = importers::select(root, Ecosystem::Js, "h3")
        .unwrap_err()
        .to_string();
    assert!(error.contains("h3@9.9.9"), "{error}");
    assert!(error.contains("package row"), "{error}");
}

#[test]
fn every_direct_dependency_class_resolves_in_its_js_format() {
    let bun_dir = tempfile::tempdir().unwrap();
    let bun = bun_dir.path();
    std::fs::write(
        bun.join("bun.lock"),
        r#"{
  "lockfileVersion": 1,
  "workspaces": {
    "": {
      "name": "root",
      "devDependencies": { "vitest": "^3" },
      "optionalDependencies": { "fsevents": "^2" },
      "peerDependencies": { "react": "^19" }
    }
  },
  "packages": {
    "vitest": ["vitest@3.2.4", "", {}, ""],
    "fsevents": ["fsevents@2.3.3", "", {}, ""],
    "react": ["react@19.1.0", "", {}, ""]
  }
}"#,
    )
    .unwrap();
    write_package_json(
        bun,
        r#"{"name":"root","devDependencies":{"vitest":"^3"},"optionalDependencies":{"fsevents":"^2"},"peerDependencies":{"react":"^19"}}"#,
    );
    for (package, version) in [
        ("vitest", "3.2.4"),
        ("fsevents", "2.3.3"),
        ("react", "19.1.0"),
    ] {
        assert_eq!(
            importers::select(bun, Ecosystem::Js, package)
                .unwrap()
                .version,
            version
        );
    }

    let pnpm_dir = tempfile::tempdir().unwrap();
    let pnpm = pnpm_dir.path();
    std::fs::write(
        pnpm.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  .:\n    devDependencies:\n      vitest:\n        specifier: ^3\n        version: 3.2.4\n    optionalDependencies:\n      fsevents:\n        specifier: ^2\n        version: 2.3.3\npackages:\n  vitest@3.2.4: {}\n  fsevents@2.3.3: {}\n",
    )
    .unwrap();
    write_package_json(
        pnpm,
        r#"{"name":"root","devDependencies":{"vitest":"^3"},"optionalDependencies":{"fsevents":"^2"}}"#,
    );
    for (package, version) in [("vitest", "3.2.4"), ("fsevents", "2.3.3")] {
        assert_eq!(
            importers::select(pnpm, Ecosystem::Js, package)
                .unwrap()
                .version,
            version
        );
    }

    let npm_dir = tempfile::tempdir().unwrap();
    let npm = npm_dir.path();
    std::fs::write(
        npm.join("package-lock.json"),
        r#"{"lockfileVersion":3,"packages":{"":{"name":"root","devDependencies":{"vitest":"^3"},"optionalDependencies":{"fsevents":"^2"},"peerDependencies":{"react":"^19"}},"node_modules/vitest":{"version":"3.2.4","resolved":"https://registry.npmjs.org/vitest/-/vitest-3.2.4.tgz"},"node_modules/fsevents":{"version":"2.3.3","resolved":"https://registry.npmjs.org/fsevents/-/fsevents-2.3.3.tgz"},"node_modules/react":{"version":"19.1.0","resolved":"https://registry.npmjs.org/react/-/react-19.1.0.tgz"}}}"#,
    )
    .unwrap();
    write_package_json(
        npm,
        r#"{"name":"root","devDependencies":{"vitest":"^3"},"optionalDependencies":{"fsevents":"^2"},"peerDependencies":{"react":"^19"}}"#,
    );
    for (package, version) in [
        ("vitest", "3.2.4"),
        ("fsevents", "2.3.3"),
        ("react", "19.1.0"),
    ] {
        assert_eq!(
            importers::select(npm, Ecosystem::Js, package)
                .unwrap()
                .version,
            version
        );
    }
}

const CARGO_REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

fn write_cargo_app(root: &std::path::Path, lock_packages: &str) {
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmylib = \"1\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("Cargo.lock"),
        format!(
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\
             dependencies = [\"mylib\"]\n{lock_packages}"
        ),
    )
    .unwrap();
}

#[test]
fn cargo_rejects_a_non_registry_source_for_the_selected_package() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();

    write_cargo_app(
        root,
        &format!(
            "\n[[package]]\nname = \"mylib\"\nversion = \"1.2.3\"\nsource = \"{CARGO_REGISTRY}\"\n"
        ),
    );
    assert_eq!(
        importers::select(root, Ecosystem::Rust, "mylib")
            .unwrap()
            .version,
        "1.2.3"
    );

    let git = "git+https://github.com/me/mylib?branch=x#abc123";
    write_cargo_app(
        root,
        &format!("\n[[package]]\nname = \"mylib\"\nversion = \"1.2.3\"\nsource = \"{git}\"\n"),
    );
    let error = importers::select(root, Ecosystem::Rust, "mylib")
        .unwrap_err()
        .to_string();
    assert!(error.contains("non-registry"), "{error}");
    assert!(error.contains(git), "{error}");
    assert!(error.contains("--ref"), "{error}");
}

#[test]
fn cargo_rejects_a_path_dependency_for_the_selected_package() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    write_cargo_app(
        root,
        "\n[[package]]\nname = \"mylib\"\nversion = \"0.3.0\"\n",
    );

    let error = importers::select(root, Ecosystem::Rust, "mylib")
        .unwrap_err()
        .to_string();
    assert!(error.contains("mylib"), "{error}");
    assert!(error.contains("0.3.0"), "{error}");
    assert!(error.contains("no registry source"), "{error}");
    assert!(error.contains("--ref"), "{error}");
}

#[test]
fn a_cargo_registry_and_git_pair_is_named_by_source_not_by_fork() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let git = "git+https://github.com/me/mylib#abc123";

    // Same version: the version keys collapse to one, so the resolution-fork
    // error cannot fire and the git row must be what the user is told about.
    write_cargo_app(
        root,
        &format!(
            "\n[[package]]\nname = \"mylib\"\nversion = \"1.2.3\"\nsource = \"{CARGO_REGISTRY}\"\n\
             \n[[package]]\nname = \"mylib\"\nversion = \"1.2.3\"\nsource = \"{git}\"\n"
        ),
    );
    let error = importers::select(root, Ecosystem::Rust, "mylib")
        .unwrap_err()
        .to_string();
    assert!(error.contains("non-registry"), "{error}");
    assert!(error.contains(git), "{error}");
    assert!(!error.contains("fork"), "{error}");

    // Different versions with an unpinned edge: the fork error already names
    // both versions and the escape hatch, which is actionable as it stands.
    write_cargo_app(
        root,
        &format!(
            "\n[[package]]\nname = \"mylib\"\nversion = \"1.2.3\"\nsource = \"{CARGO_REGISTRY}\"\n\
             \n[[package]]\nname = \"mylib\"\nversion = \"2.0.0\"\nsource = \"{git}\"\n"
        ),
    );
    let error = importers::select(root, Ecosystem::Rust, "mylib")
        .unwrap_err()
        .to_string();
    assert!(error.contains("fork"), "{error}");
    assert!(error.contains("1.2.3"), "{error}");
    assert!(error.contains("2.0.0"), "{error}");
    assert!(error.contains("--ref"), "{error}");

    // Different versions with the edge pinning the git row: the pin resolves,
    // so the source check is what has to stop it.
    std::fs::write(
        root.join("Cargo.lock"),
        format!(
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\
             dependencies = [\"mylib 2.0.0\"]\n\
             \n[[package]]\nname = \"mylib\"\nversion = \"1.2.3\"\nsource = \"{CARGO_REGISTRY}\"\n\
             \n[[package]]\nname = \"mylib\"\nversion = \"2.0.0\"\nsource = \"{git}\"\n"
        ),
    )
    .unwrap();
    let error = importers::select(root, Ecosystem::Rust, "mylib")
        .unwrap_err()
        .to_string();
    assert!(error.contains("non-registry"), "{error}");
    assert!(error.contains(git), "{error}");
    assert!(error.contains("2.0.0"), "{error}");
}

fn write_uv_app(root: &std::path::Path, httpx_source: &str) {
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"httpx\"]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("uv.lock"),
        format!(
            "version = 1\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\
             source = {{ editable = \".\" }}\ndependencies = [{{ name = \"httpx\" }}]\n\
             \n[[package]]\nname = \"httpx\"\nversion = \"0.28.1\"\n{httpx_source}"
        ),
    )
    .unwrap();
}

#[test]
fn uv_rejects_a_non_registry_source_for_the_selected_package() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();

    write_uv_app(
        root,
        "source = { registry = \"https://pypi.org/simple\" }\n",
    );
    assert_eq!(
        importers::select(root, Ecosystem::Python, "httpx")
            .unwrap()
            .version,
        "0.28.1"
    );

    for locator in [
        "git = \"https://github.com/me/httpx?rev=abc123\"",
        "directory = \"vendor/httpx\"",
        "url = \"https://example.invalid/httpx.tar.gz\"",
    ] {
        write_uv_app(root, &format!("source = {{ {locator} }}\n"));
        let error = importers::select(root, Ecosystem::Python, "httpx")
            .unwrap_err()
            .to_string();
        assert!(error.contains("non-registry"), "{error}");
        assert!(error.contains(locator), "{error}");
        assert!(error.contains("--ref"), "{error}");
    }
}

fn write_npm_app_declaring(root: &std::path::Path, package: &str, spec: &str, row: &str) {
    let manifest = format!(r#"{{"name":"root","dependencies":{{"{package}":"{spec}"}}}}"#);
    write_package_json(root, &manifest);
    std::fs::write(
        root.join("package-lock.json"),
        format!(
            r#"{{"lockfileVersion":3,"packages":{{"":{manifest},"node_modules/{package}":{row}}}}}"#
        ),
    )
    .unwrap();
}

fn write_npm_app(root: &std::path::Path, h3_row: &str) {
    write_npm_app_declaring(root, "h3", "^1", h3_row);
}

#[test]
fn npm_rejects_a_non_registry_resolution_for_the_selected_package() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();

    write_npm_app(
        root,
        r#"{"version":"1.15.11","resolved":"https://registry.npmjs.org/h3/-/h3-1.15.11.tgz","integrity":"sha512-x"}"#,
    );
    assert_eq!(
        importers::select(root, Ecosystem::Js, "h3")
            .unwrap()
            .version,
        "1.15.11"
    );

    for (row, locator) in [
        (
            r#"{"version":"1.15.11","resolved":"git+https://github.com/me/h3.git#abc123"}"#,
            "git+https://github.com/me/h3.git#abc123",
        ),
        (
            r#"{"version":"1.15.11","resolved":"file:../h3"}"#,
            "file:../h3",
        ),
        (r#"{"resolved":"packages/h3","link":true}"#, "packages/h3"),
    ] {
        write_npm_app(root, row);
        let error = importers::select(root, Ecosystem::Js, "h3")
            .unwrap_err()
            .to_string();
        assert!(error.contains("non-registry"), "{error}");
        assert!(error.contains(locator), "{error}");
        assert!(error.contains("--ref"), "{error}");
    }

    write_npm_app(root, r#"{"version":"1.15.11"}"#);
    let error = importers::select(root, Ecosystem::Js, "h3")
        .unwrap_err()
        .to_string();
    assert!(error.contains("no registry resolution"), "{error}");
    assert!(error.contains("node_modules/h3"), "{error}");
    assert!(error.contains("--ref"), "{error}");
}

/// A remote-tarball install writes an ordinary https `resolved` — for a
/// tarball fetched from the registry host, one byte-identical to what a
/// registry range install writes — so the installed row cannot tell the two
/// apart. The declaring spec can: npm copies the tarball URL into it verbatim,
/// where a registry install leaves a semver range or dist-tag.
#[test]
fn npm_rejects_a_remote_tarball_declaration_for_the_selected_package() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();

    for spec in [
        "https://example.com/h3-fork.tgz",
        "https://registry.npmjs.org/h3/-/h3-1.15.1.tgz",
        "http://internal.invalid/h3-1.15.11.tgz",
    ] {
        write_npm_app_declaring(
            root,
            "h3",
            spec,
            &format!(r#"{{"version":"1.15.11","resolved":"{spec}","integrity":"sha512-x"}}"#),
        );
        let error = importers::select(root, Ecosystem::Js, "h3")
            .unwrap_err()
            .to_string();
        assert!(error.contains("non-registry"), "{error}");
        assert!(error.contains(spec), "{error}");
        assert!(error.contains("--ref"), "{error}");
    }

    // A semver range and an `npm:` spec naming the same package are both
    // registry installs and must keep resolving.
    write_npm_app_declaring(
        root,
        "h3",
        "^1.15.11",
        r#"{"version":"1.15.11","resolved":"https://registry.npmjs.org/h3/-/h3-1.15.11.tgz","integrity":"sha512-x"}"#,
    );
    assert_eq!(
        importers::select(root, Ecosystem::Js, "h3")
            .unwrap()
            .version,
        "1.15.11"
    );

    write_npm_app_declaring(
        root,
        "h3",
        "npm:h3@^1.15.11",
        r#"{"name":"h3","version":"1.15.11","resolved":"https://registry.npmjs.org/h3/-/h3-1.15.11.tgz","integrity":"sha512-x"}"#,
    );
    assert_eq!(
        importers::select(root, Ecosystem::Js, "h3")
            .unwrap()
            .version,
        "1.15.11"
    );
}

/// An npm alias fills the install slot named by the dependency key with a
/// different package, and the version in that row is the *target's* release
/// number. Mapping it onto the queried package's git tags serves an unrelated
/// repository's tree under a plausible version, so a row naming another
/// package is refused.
#[test]
fn npm_rejects_an_install_slot_holding_another_package() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();

    // A project depending on a fork through the upstream name.
    write_npm_app_declaring(
        root,
        "h3",
        "npm:h3-fork@^1.0.0",
        r#"{"name":"h3-fork","version":"1.4.0","resolved":"https://registry.npmjs.org/h3-fork/-/h3-fork-1.4.0.tgz","integrity":"sha512-x"}"#,
    );
    let error = importers::select(root, Ecosystem::Js, "h3")
        .unwrap_err()
        .to_string();
    assert!(error.contains("h3-fork"), "{error}");
    assert!(error.contains("node_modules/h3"), "{error}");
    assert!(error.contains("--ref"), "{error}");

    // The same shape the other way round: a fork name serving upstream.
    write_npm_app_declaring(
        root,
        "h3-fork",
        "npm:h3@^1.15.11",
        r#"{"name":"h3","version":"1.15.11","resolved":"https://registry.npmjs.org/h3/-/h3-1.15.11.tgz","integrity":"sha512-x"}"#,
    );
    let error = importers::select(root, Ecosystem::Js, "h3-fork")
        .unwrap_err()
        .to_string();
    assert!(error.contains("node_modules/h3-fork"), "{error}");
    assert!(error.contains("--ref"), "{error}");

    // An alias whose target is a tarball: the `npm:` prefix keeps the spec out
    // of the tarball check, and the row's `resolved` is an ordinary https URL.
    write_npm_app_declaring(
        root,
        "h3-fork",
        "npm:h3@https://example.com/h3.tgz",
        r#"{"name":"h3","version":"1.15.11","resolved":"https://example.com/h3.tgz","integrity":"sha512-x"}"#,
    );
    let error = importers::select(root, Ecosystem::Js, "h3-fork")
        .unwrap_err()
        .to_string();
    assert!(error.contains("node_modules/h3-fork"), "{error}");
    assert!(error.contains("--ref"), "{error}");

    // An alias under a different key does not fill the queried slot at all.
    write_npm_app_declaring(
        root,
        "h3-alias",
        "npm:h3@^1.15.11",
        r#"{"name":"h3","version":"1.15.11","resolved":"https://registry.npmjs.org/h3/-/h3-1.15.11.tgz","integrity":"sha512-x"}"#,
    );
    let error = importers::select(root, Ecosystem::Js, "h3")
        .unwrap_err()
        .to_string();
    assert!(!error.contains("1.15.11"), "{error}");

    // An ordinary registry install records no `name`, and one that records a
    // matching `name` is equally ordinary: neither may be refused.
    write_npm_app_declaring(
        root,
        "h3",
        "^1.15.11",
        r#"{"version":"1.15.11","resolved":"https://registry.npmjs.org/h3/-/h3-1.15.11.tgz","integrity":"sha512-x"}"#,
    );
    assert_eq!(
        importers::select(root, Ecosystem::Js, "h3")
            .unwrap()
            .version,
        "1.15.11"
    );

    write_npm_app_declaring(
        root,
        "@scope/pkg",
        "^2.0.0",
        r#"{"name":"@scope/pkg","version":"2.0.1","resolved":"https://registry.npmjs.org/@scope/pkg/-/pkg-2.0.1.tgz","integrity":"sha512-x"}"#,
    );
    assert_eq!(
        importers::select(root, Ecosystem::Js, "@scope/pkg")
            .unwrap()
            .version,
        "2.0.1"
    );
}

#[test]
fn undeclared_is_downcastable_and_renders_unchanged() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
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
    assert_eq!(common::message(&format!("{error:?}")), display);
    assert!(
        display.contains("does not declare `transitive`"),
        "{display}"
    );
}

#[test]
fn selection_names_the_lockfile_that_carried_the_version() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(root.join("bun.lock"), BUN_LOCK).unwrap();
    let ws = root.join("apps/api");
    write_package_json(
        &ws,
        r#"{"name":"@app/api","dependencies":{"h3":"^1.15.5"}}"#,
    );

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
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
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
    write_package_json(
        root,
        r#"{"name":"root","dependencies":{"h3":"github:unjs/h3"}}"#,
    );

    let inspection = importers::inspect(root, Ecosystem::Js, "h3");
    assert!(inspection.result.is_err(), "non-registry must not resolve");
    assert_eq!(inspection.evidence, importers::Evidence::Declared);
}

#[test]
fn evidence_reports_undeclared_and_unknown_distinctly() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
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
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
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
    write_package_json(root, r#"{"name":"root","dependencies":{"h3":"^1"}}"#);

    let inspection = importers::inspect(root, Ecosystem::Js, "h3");
    assert!(inspection.result.is_err());
    assert_eq!(inspection.evidence, importers::Evidence::Unknown);
}

#[test]
fn a_selector_reads_each_lockfile_once() {
    // A count assertion without instrumentation: after the first package
    // forces the parse, the lockfile is deleted. Every later package must
    // still resolve, which is only possible if nothing re-reads the file.
    // The later packages are all differently named, so a cache keyed by
    // (lockfile, package) would have to re-read the deleted file too.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(root.join("bun.lock"), BUN_LOCK).unwrap();
    let ws = root.join("apps/api");
    write_package_json(
        &ws,
        r#"{"name":"@app/api","dependencies":{"h3":"^1.15.5"}}"#,
    );

    let selector = importers::Selector::new(&ws, Ecosystem::Js).unwrap();
    assert_eq!(selector.select("h3").unwrap().version, "1.15.11");

    std::fs::remove_file(root.join("bun.lock")).unwrap();
    for index in 0..19 {
        // An undeclared diagnostic is only reachable once the lockfile has
        // parsed and the workspace entry has been found; a re-read would fail
        // with `reading …` long before naming the workspace.
        let absent = format!("absent-{index}");
        let error = selector.select(&absent).unwrap_err().to_string();
        assert!(error.contains("does not declare `absent-"), "{error}");
        assert_eq!(selector.select("h3").unwrap().version, "1.15.11");
    }
}

#[test]
fn a_malformed_unselected_lockfile_is_still_ignored() {
    // packageManager names pnpm; a corrupt bun.lock sits beside it. Eager
    // parsing would make this a hard failure for a project that resolves fine.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(root.join("bun.lock"), "{ this is not json").unwrap();
    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      h3:\n        specifier: ^1\n        version: 1.0.0\npackages:\n  h3@1.0.0: {}\n",
    )
    .unwrap();
    write_package_json(
        root,
        r#"{"name":"root","packageManager":"pnpm@9.0.0","dependencies":{"h3":"^1"}}"#,
    );

    assert_eq!(
        importers::select(root, Ecosystem::Js, "h3")
            .unwrap()
            .version,
        "1.0.0"
    );
    let selector = importers::Selector::new(root, Ecosystem::Js).unwrap();
    assert_eq!(selector.select("h3").unwrap().version, "1.0.0");
}

#[test]
fn a_cached_parse_error_replays_identically() {
    // Two packages against one malformed lockfile: an `anyhow::Error` handed
    // out once would move or reconstruct, so the second caller must get the
    // same three renderings as the first. The lockfile is deleted in between,
    // so only a memoized *failure* can produce the second error at all — a
    // re-parse would report `reading …` instead.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    std::fs::write(root.join("pnpm-lock.yaml"), "\tnot: [valid: yaml").unwrap();
    write_package_json(root, r#"{"name":"root","packageManager":"pnpm@9.0.0"}"#);

    let selector = importers::Selector::new(root, Ecosystem::Js).unwrap();
    let first = selector.select("h3").unwrap_err();
    std::fs::remove_file(root.join("pnpm-lock.yaml")).unwrap();
    let second = selector.select("kysely").unwrap_err();
    assert!(
        format!("{first}").contains("parsing pnpm-lock.yaml"),
        "{first}"
    );
    assert_eq!(format!("{first}"), format!("{second}"));
    assert_eq!(format!("{first:#}"), format!("{second:#}"));
    assert_eq!(
        common::message(&format!("{first:?}")),
        common::message(&format!("{second:?}"))
    );
}

#[test]
fn a_cargo_selector_resolves_two_packages_from_one_parse() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
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

    let selector = importers::Selector::new(root, Ecosystem::Rust).unwrap();
    assert_eq!(selector.select("serde").unwrap().version, "1.0.200");
    std::fs::remove_file(root.join("Cargo.lock")).unwrap();
    assert_eq!(selector.select("anyhow").unwrap().version, "1.0.90");
}
