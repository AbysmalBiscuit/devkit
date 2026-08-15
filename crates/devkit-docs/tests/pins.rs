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
        Outcome::Version {
            version,
            lockfile,
            workspace,
        } => {
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
    assert!(
        matches!(out[0].outcome, Outcome::Undeclared),
        "{:?}",
        out[0].outcome
    );
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
    assert!(
        out[0].project_scoped,
        "godot comes from the project devkit.toml"
    );
    assert!(
        !out[1].project_scoped,
        "serde comes from the global manifest"
    );
    assert_eq!(
        out[0].declared,
        Evidence::Unknown,
        "git has no importer to ask"
    );
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
    assert!(
        matches!(out[0].outcome, Outcome::Unresolved(_)),
        "{:?}",
        out[0].outcome
    );
    assert_eq!(out[0].declared, Evidence::Unknown);
    assert!(
        matches!(out[1].outcome, Outcome::Version { .. }),
        "{:?}",
        out[1].outcome
    );
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
