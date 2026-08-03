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
    "h3-v2": ["h3@2.0.1-rc.20", "", { "transitive": "^3.0.0" }, "sha512-b"],
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
    let root = common::unique_tmp("bun-alias");
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
fn a_transitive_package_is_a_hard_error_that_lists_truthful_candidates() {
    let root = common::unique_tmp("bun-transitive");
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

    let pnpm_root = common::unique_tmp("pnpm-transitive");
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

    let npm_root = common::unique_tmp("npm-transitive");
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

    let uv_root = common::unique_tmp("uv-transitive");
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
    let uv_error = importers::select(&uv_root, Ecosystem::Python, "certifi")
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
    let root = common::unique_tmp("pnpm-peer");
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
    let root = common::unique_tmp("two-locks");
    std::fs::write(root.join("bun.lock"), BUN_LOCK).unwrap();
    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  apps/api:\n    dependencies:\n      h3:\n        specifier: ^1.15.5\n        version: 1.15.7\n",
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
    let root = common::unique_tmp("uv-fork");
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

    let error = importers::select(&root, Ecosystem::Python, "httpx")
        .unwrap_err()
        .to_string();
    assert!(error.contains("0.27.0"), "{error}");
    assert!(error.contains("0.28.1"), "{error}");
    assert!(error.contains("fork"), "{error}");
}

#[test]
fn uv_dev_optional_and_dependency_groups_are_direct_dependencies() {
    let root = common::unique_tmp("uv-groups");
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

[[package]]
name = "uvloop"
version = "0.21.0"

[[package]]
name = "ruff"
version = "0.9.1"
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
            importers::select(&root, Ecosystem::Python, package)
                .unwrap()
                .version,
            version
        );
    }

    let duplicate = common::unique_tmp("uv-member-duplicate");
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

[[package]]
name = "rich"
version = "13.9.4"
"#,
    )
    .unwrap();
    std::fs::write(
        duplicate.join("pyproject.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"httpx\"]\n",
    )
    .unwrap();
    assert_eq!(
        importers::select(&duplicate, Ecosystem::Python, "httpx")
            .unwrap()
            .version,
        "0.28.1"
    );
    let error = importers::select(&duplicate, Ecosystem::Python, "rich")
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

[[package]]
name = "rich"
version = "13.9.4"
"#,
    )
    .unwrap();
    let error = importers::select(&duplicate, Ecosystem::Python, "httpx")
        .unwrap_err()
        .to_string();
    assert!(error.contains("ambiguous"), "{error}");
}

#[test]
fn a_cargo_member_gets_its_own_dependency_not_another_members() {
    let root = common::unique_tmp("cargo-ws");
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
    let root = common::unique_tmp("cargo-dup");
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

[[package]]
name = "serde"
version = "0.9.15"
"#,
    )
    .unwrap();

    assert_eq!(
        importers::select(&root, Ecosystem::Rust, "serde")
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

[[package]]
name = "serde"
version = "0.9.15"
"#,
    )
    .unwrap();
    let error = importers::select(&root, Ecosystem::Rust, "serde")
        .unwrap_err()
        .to_string();
    assert!(error.contains("ambiguous"), "{error}");
}

#[test]
fn npm_resolves_the_nearest_nested_copy_walking_up_from_the_workspace() {
    let root = common::unique_tmp("npm-nested");
    std::fs::write(
        root.join("package-lock.json"),
        r#"{
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "root" },
    "apps/api": { "name": "@app/api", "dependencies": { "h3": "^1.0.0" } },
    "apps/api/node_modules/h3": { "version": "1.15.11" },
    "node_modules/h3": { "version": "2.0.1" }
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
    let root = common::unique_tmp("pnpm-alias");
    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      h3-v2:\n        specifier: npm:h3@2.0.1\n        version: h3@2.0.1\npackages:\n  h3@2.0.1: {}\n",
    )
    .unwrap();
    write_package_json(
        &root,
        r#"{"name":"root","dependencies":{"h3-v2":"npm:h3@2.0.1"}}"#,
    );

    assert!(importers::select(&root, Ecosystem::Js, "h3").is_err());
    assert_eq!(
        importers::select(&root, Ecosystem::Js, "h3-v2")
            .unwrap()
            .version,
        "2.0.1"
    );

    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      h3-v2:\n        specifier: npm:h3@2.0.1\n        version: ghost@2.0.1\npackages:\n  h3@2.0.1: {}\n",
    )
    .unwrap();
    let error = importers::select(&root, Ecosystem::Js, "h3-v2")
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
        let error = importers::select(&root, Ecosystem::Js, "h3-v2")
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported"), "{error}");
        assert!(error.contains(locator), "{error}");
    }
}

#[test]
fn every_direct_dependency_class_resolves_in_its_js_format() {
    let bun = common::unique_tmp("js-classes-bun");
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
        &bun,
        r#"{"name":"root","devDependencies":{"vitest":"^3"},"optionalDependencies":{"fsevents":"^2"},"peerDependencies":{"react":"^19"}}"#,
    );
    for (package, version) in [
        ("vitest", "3.2.4"),
        ("fsevents", "2.3.3"),
        ("react", "19.1.0"),
    ] {
        assert_eq!(
            importers::select(&bun, Ecosystem::Js, package)
                .unwrap()
                .version,
            version
        );
    }

    let pnpm = common::unique_tmp("js-classes-pnpm");
    std::fs::write(
        pnpm.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  .:\n    devDependencies:\n      vitest:\n        specifier: ^3\n        version: 3.2.4\n    optionalDependencies:\n      fsevents:\n        specifier: ^2\n        version: 2.3.3\n",
    )
    .unwrap();
    write_package_json(
        &pnpm,
        r#"{"name":"root","devDependencies":{"vitest":"^3"},"optionalDependencies":{"fsevents":"^2"}}"#,
    );
    for (package, version) in [("vitest", "3.2.4"), ("fsevents", "2.3.3")] {
        assert_eq!(
            importers::select(&pnpm, Ecosystem::Js, package)
                .unwrap()
                .version,
            version
        );
    }

    let npm = common::unique_tmp("js-classes-npm");
    std::fs::write(
        npm.join("package-lock.json"),
        r#"{"lockfileVersion":3,"packages":{"":{"name":"root","devDependencies":{"vitest":"^3"},"optionalDependencies":{"fsevents":"^2"},"peerDependencies":{"react":"^19"}},"node_modules/vitest":{"version":"3.2.4"},"node_modules/fsevents":{"version":"2.3.3"},"node_modules/react":{"version":"19.1.0"}}}"#,
    )
    .unwrap();
    write_package_json(
        &npm,
        r#"{"name":"root","devDependencies":{"vitest":"^3"},"optionalDependencies":{"fsevents":"^2"},"peerDependencies":{"react":"^19"}}"#,
    );
    for (package, version) in [
        ("vitest", "3.2.4"),
        ("fsevents", "2.3.3"),
        ("react", "19.1.0"),
    ] {
        assert_eq!(
            importers::select(&npm, Ecosystem::Js, package)
                .unwrap()
                .version,
            version
        );
    }
}
