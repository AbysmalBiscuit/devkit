//! `pins` turns the merged docs manifest into per-library outcomes using
//! manifest and lockfile reads only.

use devkit_docs::importers::Evidence;
use devkit_docs::importers::Evidence as Ev;
use devkit_docs::pins::{self, Dropped, Outcome, Pin};
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
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    cargo_project(root, "[config]\nroot = true\n");
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
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    cargo_project(root, "[config]\nroot = true\n");
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
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    cargo_project(root, "[config]\nroot = true\n");
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
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    cargo_project(root, "[config]\nroot = true\n");
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
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    cargo_project(
        root,
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
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    cargo_project(root, "[config]\nroot = true\n");
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
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    cargo_project(root, "[config]\nroot = true\n");
    write(&root.join("docs.toml"), "this is not toml [[[");

    assert!(pins::pins(&root.join("project"), Some(&root.join("docs.toml"))).is_err());
}

#[test]
fn a_globally_registered_library_stays_relative_without_any_devkit_toml() {
    // Root-level resolution: the workspace is also the lockfile's own
    // directory, so there is no project root anywhere above `start`.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    write(
        &root.join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde = \"1.0.200\"\n",
    );
    write(
        &root.join("app/Cargo.lock"),
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
    write(
        &root.join("docs.toml"),
        "[[libs]]\nname = \"serde\"\necosystem = \"rust\"\nrepo = \"https://example.invalid/serde\"\n",
    );

    let out = pins::pins(&root.join("app"), Some(&root.join("docs.toml"))).unwrap();
    match &out[0].outcome {
        Outcome::Version { workspace, .. } => assert_eq!(workspace, Path::new(".")),
        other => panic!("expected a version, got {other:?}"),
    }

    // Member resolution: the workspace sits below the lockfile's directory,
    // and still no `devkit.toml` exists anywhere above `start`.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
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
    write(
        &root.join("docs.toml"),
        "[[libs]]\nname = \"serde\"\necosystem = \"rust\"\nrepo = \"https://example.invalid/serde\"\n",
    );

    let out = pins::pins(&root.join("crates/app"), Some(&root.join("docs.toml"))).unwrap();
    match &out[0].outcome {
        Outcome::Version { workspace, .. } => assert_eq!(workspace, Path::new("crates/app")),
        other => panic!("expected a version, got {other:?}"),
    }
}

#[test]
fn an_unresolved_reason_is_one_line() {
    // The `undeclared` diagnostic is three lines; that belongs in `docm info`,
    // not in injected context. Unresolved carries `{err}`, never `{err:#}`.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    cargo_project(root, "[config]\nroot = true\n");
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

fn pin(name: &str, outcome: Outcome, project_scoped: bool, declared: Ev) -> Pin {
    Pin {
        name: name.into(),
        outcome,
        project_scoped,
        declared,
        resolved: None,
    }
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
        pin(
            "kysely",
            version("0.28.17", "apps/web", "pnpm-lock.yaml"),
            false,
            Ev::Declared,
        ),
        pin("zod", Outcome::Undeclared, false, Ev::Undeclared),
        pin(
            "mystery",
            Outcome::Unresolved("no lockfile".into()),
            false,
            Ev::Unknown,
        ),
    ];
    let (rows, dropped) = pins::relevant(&all);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "kysely");
    assert_eq!(
        dropped,
        Dropped {
            undeclared: 1,
            unknown: 1
        }
    );

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
        pin(
            "godot",
            Outcome::Ref("4.3-stable".into()),
            true,
            Ev::Unknown,
        ),
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
    let all = vec![pin(
        "serde",
        Outcome::Ref("v1.0.200".into()),
        false,
        Ev::Declared,
    )];
    let (rows, _) = pins::relevant(&all);
    assert_eq!(rows.len(), 1);
    assert!(pins::render(&all).contains("ref"), "source column says ref");
}

#[test]
fn an_empty_relevant_set_says_so_explicitly() {
    let all = vec![pin("zod", Outcome::Undeclared, false, Ev::Undeclared)];
    let text = pins::render(&all);
    assert!(text.contains("no registered libraries"), "{text}");
    assert!(
        text.contains("1 registered library not evidenced here"),
        "{text}"
    );
}

#[test]
fn a_pathological_cell_is_truncated_visibly() {
    let reason = "x".repeat(5_000);
    let all = vec![pin(
        "huge",
        Outcome::Unresolved(reason.clone()),
        true,
        Ev::Unknown,
    )];
    let text = pins::render(&all);
    assert!(text.contains('…'), "truncation marker present: {text}");
    assert!(
        text.len() <= 4_096,
        "section stays inside its budget: {}",
        text.len()
    );

    // The JSON envelope carries the untruncated value.
    let json = pins::envelope(&all).to_string();
    assert!(json.contains(&reason), "envelope keeps the full value");
}

#[test]
fn a_wide_cell_forces_column_widening_that_the_budget_still_respects() {
    // `ui::table`'s dynamic arrangement sizes each column to its widest cell
    // across every row, so one cell approaching CELL_BUDGET widens (and
    // wraps) every row in the table at once. A per-row cost estimate that
    // only looks at each row's own cells cannot see this coming; only a
    // render-then-measure pass can. Cell widths climb from 10 to 194 bytes
    // across 24 rows — a spread wide enough to actually trigger wrapping,
    // where the old estimate-only render undercounted the true cost by more
    // than 10% and landed at 4593 bytes.
    let all: Vec<Pin> = (0..24)
        .map(|i| {
            pin(
                &format!("lib{i:02}"),
                Outcome::Unresolved("x".repeat(10 + i * 8)),
                true,
                Ev::Unknown,
            )
        })
        .collect();
    let text = pins::render(&all);
    assert!(text.len() <= 4_096, "section budget: {}", text.len());
}

#[test]
fn a_multibyte_cell_is_truncated_on_a_char_boundary() {
    // "本" is 3 bytes in UTF-8; 67 repeats is 201 bytes, one past
    // CELL_BUDGET (200). CELL_BUDGET - '…'.len_utf8() = 197, and the
    // largest 3-byte-aligned boundary at or below 197 is 195 (65 chars) — a
    // naive 200-byte cut instead lands mid-character.
    let reason = "本".repeat(67);
    let all = vec![pin(
        "multibyte",
        Outcome::Unresolved(reason.clone()),
        true,
        Ev::Unknown,
    )];
    let text = pins::render(&all);

    // `ui::table`'s dynamic arrangement may wrap this long cell across
    // physical lines, so a contiguous substring check would be sensitive to
    // wrapping rather than to the truncation boundary. Counting characters
    // is not: exactly 65 survive the cut, never 64 (off by one short) or 66
    // (sliced past the boundary).
    let survivors = text.chars().filter(|&c| c == '本').count();
    assert_eq!(survivors, 65, "char-aligned cut keeps exactly 65: {text}");
    assert!(text.contains('…'), "truncation marker present: {text}");
    assert!(
        !text.contains(&reason),
        "the untruncated reason must not appear: {text}"
    );
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

#[test]
fn a_git_entry_without_a_ref_says_so_rather_than_reading_as_unregistered() {
    // A git entry has no importer to ask, so it never reaches a selector; the
    // row must still name why, not fall through to the "no ecosystem" reason.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    cargo_project(
        root,
        "[config]\nroot = true\n\n[[docs.libs]]\nname = \"godot\"\necosystem = \"git\"\nrepo = \"https://example.invalid/godot\"\n",
    );
    write(&root.join("docs.toml"), "");

    let out = pins::pins(&root.join("project"), Some(&root.join("docs.toml"))).unwrap();
    assert_eq!(
        out[0].outcome,
        Outcome::Unresolved("git entry with no ref pinned".to_string())
    );
    assert_eq!(out[0].declared, Evidence::Unknown);
}

/// A pnpm workspace whose root importer declares `declared` libraries, over a
/// lockfile padded with `filler` unrelated packages and their dependency
/// edges — the shape of a large monorepo lockfile.
fn pnpm_workspace(root: &Path, declared: usize, filler: usize) {
    let mut lock = String::from("lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n");
    for i in 0..declared {
        lock.push_str(&format!(
            "      lib-{i}:\n        specifier: ^1.0.0\n        version: 1.0.{i}\n"
        ));
    }
    lock.push_str("packages:\n");
    for i in 0..declared {
        lock.push_str(&format!(
            "  lib-{i}@1.0.{i}:\n    resolution: {{integrity: sha512-a{i}}}\n"
        ));
    }
    for i in 0..filler {
        lock.push_str(&format!(
            "  '@scope{}/filler-{i}@2.{i}.0':\n    resolution: {{integrity: sha512-f{i}}}\n    engines: {{node: '>=18'}}\n",
            i % 40
        ));
    }
    lock.push_str("snapshots:\n");
    for i in 0..declared {
        lock.push_str(&format!("  lib-{i}@1.0.{i}: {{}}\n"));
    }
    for i in 0..filler {
        lock.push_str(&format!(
            "  '@scope{}/filler-{i}@2.{i}.0':\n    dependencies:\n",
            i % 40
        ));
        for k in 0..4 {
            let j = (i * 7 + k * 13) % filler.max(1);
            lock.push_str(&format!("      '@scope{}/filler-{j}': 2.{j}.0\n", j % 40));
        }
    }
    write(&root.join("pnpm-lock.yaml"), &lock);

    let dependencies: Vec<String> = (0..declared)
        .map(|i| format!("\"lib-{i}\":\"^1.0.0\""))
        .collect();
    write(
        &root.join("package.json"),
        &format!(
            "{{\"name\":\"root\",\"packageManager\":\"pnpm@9.0.0\",\"dependencies\":{{{}}}}}",
            dependencies.join(",")
        ),
    );
    write(&root.join("devkit.toml"), "[config]\nroot = true\n");
}

fn js_manifest(count: usize, declared: usize) -> String {
    (0..count)
        .map(|i| {
            let name = if i < declared {
                format!("lib-{i}")
            } else {
                format!("absent-{i}")
            };
            format!(
                "[[libs]]\nname = \"{name}\"\necosystem = \"js\"\nrepo = \"https://example.invalid/{name}\"\n"
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn resolving_a_library_reads_only_the_rows_that_bear_on_it() {
    // Every registered library costs one traversal of whatever the resolution
    // touches, so a listing must touch only the rows that decide its own
    // answer. A package row nothing in this workspace depends on is one of
    // those: reaching it at all is the sweep this bounds.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    pnpm_workspace(root, 1, 4);
    let mut lock = std::fs::read_to_string(root.join("pnpm-lock.yaml")).unwrap();
    lock.push_str("  unrelated@9.9.9: not-a-mapping\n");
    write(&root.join("pnpm-lock.yaml"), &lock);
    write(&root.join("docs.toml"), &js_manifest(2, 1));

    let out = pins::pins(root, Some(&root.join("docs.toml"))).unwrap();
    assert!(
        matches!(&out[1].outcome, Outcome::Version { version, .. } if version == "1.0.0"),
        "{:?}",
        out[1].outcome
    );
    assert_eq!(
        out[0].outcome,
        Outcome::Undeclared,
        "an absent package is a checked answer, not a lockfile complaint"
    );
}

#[test]
fn a_listing_does_not_cost_a_lockfile_traversal_per_library() {
    // `devkit brief` runs this against a machine-wide catalog that grows with
    // every `/docs` lookup, and the `PostCompact`/`CwdChanged` hooks that call
    // it bound the run to a 10-second timeout, so the cost must sit on the
    // lockfile's size rather than on the product of that and the number of
    // registrations.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    pnpm_workspace(root, 1, 3000);
    let manifest = root.join("docs.toml");

    let elapsed = |libraries: usize, declared: usize| {
        write(&manifest, &js_manifest(libraries, declared));
        (0..3)
            .map(|_| {
                let started = std::time::Instant::now();
                let out = pins::pins(root, Some(&manifest)).unwrap();
                assert_eq!(out.len(), libraries);
                started.elapsed()
            })
            .min()
            .unwrap()
    };

    let one = elapsed(1, 1);
    let many = elapsed(60, 1);
    assert!(
        many < one * 5,
        "60 registrations cost {many:?} against {one:?} for one — \
         the per-library term dominates the parse"
    );
}

const BUN_MONOREPO: &str = r#"{
  "lockfileVersion": 1,
  "workspaces": {
    "": { "name": "root" },
    "apps/api": { "name": "@app/api", "dependencies": { "h3": "^1.15.5" } },
    "apps/web": { "name": "@app/web", "dependencies": { "h3": "^2.0.0" } },
    "apps/docs": { "name": "@app/docs", "dependencies": {} }
  },
  "packages": {
    "h3": ["h3@1.15.11", "", {}, "sha512-a"],
    "@app/web/h3": ["h3@2.0.1", "", {}, "sha512-c"]
  }
}"#;

/// A bun workspace root that declares nothing itself: every dependency lives
/// in a member. This is the monorepo-root shape a session starts in.
fn bun_monorepo(root: &Path) {
    write(&root.join("package.json"), r#"{"name":"root"}"#);
    write(&root.join("bun.lock"), BUN_MONOREPO);
    write(
        &root.join("docs.toml"),
        "[[libs]]\nname = \"h3\"\necosystem = \"js\"\nrepo = \"https://example.invalid/h3\"\n",
    );
}

#[test]
fn a_workspace_root_rolls_up_its_members() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    bun_monorepo(root);

    let out = pins::pins(root, Some(&root.join("docs.toml"))).unwrap();
    match &out[0].outcome {
        Outcome::Rollup { versions, lockfile } => {
            assert_eq!(lockfile, "bun.lock");
            assert_eq!(
                versions,
                &vec![
                    ("1.15.11".to_string(), vec!["apps/api".to_string()]),
                    ("2.0.1".to_string(), vec!["apps/web".to_string()]),
                ]
            );
        }
        other => panic!("expected a rollup, got {other:?}"),
    }
    assert_eq!(
        out[0].declared,
        Evidence::Declared,
        "a rolled-up library must survive the relevance filter"
    );
}

#[test]
fn a_rolled_up_row_names_its_members() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    bun_monorepo(root);

    let out = pins::pins(root, Some(&root.join("docs.toml"))).unwrap();
    let table = pins::render(&out);
    assert!(table.contains("apps/api"), "{table}");
    assert!(table.contains("1.15.11"), "{table}");
    assert!(table.contains("2.0.1"), "{table}");
}

#[test]
fn a_leaf_workspace_reports_its_own_version_without_rolling_up() {
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    bun_monorepo(root);
    write(
        &root.join("apps/api/package.json"),
        r#"{"name":"@app/api","dependencies":{"h3":"^1.15.5"}}"#,
    );

    let out = pins::pins(&root.join("apps/api"), Some(&root.join("docs.toml"))).unwrap();
    match &out[0].outcome {
        Outcome::Version { version, .. } => assert_eq!(version, "1.15.11"),
        other => panic!("expected a plain version, got {other:?}"),
    }
}

/// A reference registry holding one row per (project, lib).
fn registry(cache: &Path, rows: &[(&Path, &str, &str)]) {
    let rows: Vec<String> = rows
        .iter()
        .map(|(project, lib, version)| {
            format!(
                r#"{{"project":{},"lib":"{lib}","version":"{version}","git_ref":"","commit":"c","resolved_at":1,"revision":0}}"#,
                serde_json::to_string(&project.to_string_lossy()).unwrap()
            )
        })
        .collect();
    write(
        &cache.join("registry.json"),
        &format!(r#"{{"version":1,"rows":[{}]}}"#, rows.join(",")),
    );
}

#[test]
fn a_registry_row_surfaces_a_library_no_lockfile_evidences() {
    // `docm` resolved a checkout for this project; that is evidence the
    // project uses the library even when the importer graph cannot say so.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let cache = root.join("cache");
    bun_monorepo(root);
    write(
        &root.join("docs.toml"),
        "[[libs]]\nname = \"zod\"\necosystem = \"js\"\nrepo = \"https://example.invalid/zod\"\n",
    );
    registry(&cache, &[(root, "zod", "4.4.3")]);

    let out = pins::pins_with_cache(root, Some(&root.join("docs.toml")), Some(&cache)).unwrap();
    assert_eq!(out[0].resolved.as_deref(), Some("4.4.3"));
    let (rows, _) = pins::relevant(&out);
    assert_eq!(rows.len(), 1, "a resolved row is relevant");
    let table = pins::render(&out);
    assert!(table.contains("4.4.3"), "{table}");
}

#[test]
fn a_registry_row_that_disagrees_with_the_lockfile_is_flagged() {
    // The checkout an agent would read is not the version this project
    // resolves — silently showing the lockfile version would hide that.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let cache = root.join("cache");
    bun_monorepo(root);
    registry(&cache, &[(root, "h3", "1.0.0")]);

    let out = pins::pins_with_cache(root, Some(&root.join("docs.toml")), Some(&cache)).unwrap();
    let table = pins::render(&out);
    assert!(table.contains("checkout 1.0.0"), "{table}");
}

#[test]
fn a_registry_row_for_a_sibling_project_is_not_borrowed() {
    // Worktrees sit beside each other under one parent; a row keyed to a
    // sibling says nothing about the checkout in hand.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let cache = root.join("cache");
    bun_monorepo(root);
    write(
        &root.join("docs.toml"),
        "[[libs]]\nname = \"zod\"\necosystem = \"js\"\nrepo = \"https://example.invalid/zod\"\n",
    );
    registry(&cache, &[(&root.join("../other"), "zod", "4.4.3")]);

    let out = pins::pins_with_cache(root, Some(&root.join("docs.toml")), Some(&cache)).unwrap();
    assert_eq!(out[0].resolved, None);
}

#[test]
fn an_encoded_checkout_dirname_is_not_a_disagreement() {
    // The registry records the checkout *directory*, where a ref's `/` is
    // encoded as `~`. Comparing that against the ref verbatim would report
    // every slash-bearing ref as stale.
    let root_dir = tempfile::tempdir().unwrap();
    let root = root_dir.path();
    let cache = root.join("cache");
    bun_monorepo(root);
    write(
        &root.join("docs.toml"),
        "[[libs]]\nname = \"typescript-go\"\necosystem = \"git\"\nref = \"typescript/v7.0.2\"\nrepo = \"https://example.invalid/tsgo\"\n",
    );
    registry(&cache, &[(root, "typescript-go", "typescript~v7.0.2")]);

    let out = pins::pins_with_cache(root, Some(&root.join("docs.toml")), Some(&cache)).unwrap();
    assert_eq!(out[0].resolved.as_deref(), Some("typescript/v7.0.2"));
    let table = pins::render(&out);
    assert!(!table.contains("checkout"), "{table}");
}
