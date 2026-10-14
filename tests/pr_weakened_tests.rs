//! `devkit pr weakened-tests`: each rule flags the change that breaks it,
//! by file and rule, and a suite the branch left alone or only grew passes.

#[path = "common/testenv.rs"]
mod testenv;

use std::{path::Path, process::Output};

/// A repository on `main` holding `files`, then a branch `feature` that
/// writes each of `changes` (a `None` deletes the file) in one commit.
struct Repo {
    dir: tempfile::TempDir,
}

fn git(dir: &Path, args: &[&str]) {
    devkit_git::Git::fixture(dir)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e:#}"));
}

fn write(dir: &Path, path: &str, text: Option<&str>) {
    let file = dir.join(path);
    match text {
        Some(text) => {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        None => std::fs::remove_file(file).unwrap(),
    }
}

impl Repo {
    fn new(files: &[(&str, &str)], changes: &[(&str, Option<&str>)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-q", "-b", "main"]);
        write(p, "README", Some("x"));
        for (path, text) in files {
            write(p, path, Some(text));
        }
        git(p, &["add", "-A"]);
        git(p, &["commit", "-q", "-m", "base"]);
        git(p, &["checkout", "-q", "-b", "feature"]);
        for (path, text) in changes {
            write(p, path, *text);
        }
        git(p, &["add", "-A"]);
        git(p, &["commit", "-q", "--allow-empty", "-m", "change"]);
        Self { dir }
    }

    fn check(&self, extra: &[&str]) -> (bool, String) {
        let (_home, mut cmd) = testenv::isolated(env!("CARGO_BIN_EXE_devkit"));
        testenv::scrub_identity(&mut cmd);
        let out: Output = cmd
            .args(["pr", "-C"])
            .arg(self.dir.path())
            .args(["weakened-tests", "main"])
            .args(extra)
            .env_remove("DEVKIT_CONFIG")
            .output()
            .expect("spawn devkit");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }
}

const RUST: &str = "\
#[test]
fn adds() {
    assert_eq!(add(1, 2), 3);
}

#[test]
fn subtracts() {
    assert_eq!(sub(3, 2), 1);
    assert!(sub(0, 0) == 0);
}
";

const JS: &str = r#"import { add } from "../src/add";

describe("add", () => {
  it("adds", () => {
    expect(add(1, 2)).toBe(3);
  });
  it("adds negatives", () => {
    expect(add(-1, -2)).toBe(-3);
  });
});
"#;

const PY: &str = "\
from pkg.models import add

def test_adds():
    assert add(1, 2) == 3

def test_negatives():
    assert add(-1, -2) == -3
";

fn suite() -> Vec<(&'static str, &'static str)> {
    vec![
        ("tests/math.rs", RUST),
        ("web/add.test.ts", JS),
        ("pkg/test_models.py", PY),
        (
            "src/add.rs",
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
        ),
    ]
}

/// Asserts the branch writing `changes` over the suite fails, naming
/// `path` and `rule` on one line.
fn assert_flags(changes: &[(&str, Option<&str>)], path: &str, rule: &str) {
    let repo = Repo::new(&suite(), changes);
    let (ok, out) = repo.check(&[]);
    assert!(!ok, "{path} {rule} must fail the check: {out}");
    assert!(
        out.lines()
            .any(|l| l.starts_with(&format!("{path}: {rule}: "))),
        "names {path} and {rule}: {out}"
    );
}

#[test]
fn each_rule_fires_on_a_diff_that_breaks_it() {
    assert_flags(
        &[("tests/math.rs", None)],
        "tests/math.rs",
        "deleted-test-file",
    );
    assert_flags(
        &[(
            "tests/math.rs",
            Some(&RUST.replace("#[test]\nfn subtracts", "fn subtracts")),
        )],
        "tests/math.rs",
        "fewer-tests",
    );
    assert_flags(
        &[(
            "web/add.test.ts",
            Some(&JS.replace("  it(\"adds negatives\"", "  // it(\"adds negatives\"")),
        )],
        "web/add.test.ts",
        "fewer-tests",
    );
    assert_flags(
        &[(
            "pkg/test_models.py",
            Some(&PY.replace("def test_negatives", "def negatives")),
        )],
        "pkg/test_models.py",
        "fewer-tests",
    );
    assert_flags(
        &[(
            "tests/math.rs",
            Some(&RUST.replace("    assert!(sub(0, 0) == 0);\n", "")),
        )],
        "tests/math.rs",
        "fewer-assertions",
    );
    assert_flags(
        &[(
            "pkg/test_models.py",
            Some(&PY.replace("    assert add(-1, -2) == -3\n", "    add(-1, -2)\n")),
        )],
        "pkg/test_models.py",
        "fewer-assertions",
    );
    assert_flags(
        &[(
            "tests/math.rs",
            Some(&RUST.replace("#[test]\nfn subtracts", "#[test]\n#[ignore]\nfn subtracts")),
        )],
        "tests/math.rs",
        "more-skips",
    );
    assert_flags(
        &[(
            "web/add.test.ts",
            Some(&JS.replace("it(\"adds negatives\"", "it.skip(\"adds negatives\"")),
        )],
        "web/add.test.ts",
        "more-skips",
    );
    assert_flags(
        &[(
            "pkg/test_models.py",
            Some(&PY.replace(
                "def test_negatives",
                "@pytest.mark.skip(reason=\"flaky\")\ndef test_negatives",
            )),
        )],
        "pkg/test_models.py",
        "more-skips",
    );
    assert_flags(
        &[(
            "tests/math.rs",
            Some(&RUST.replace("assert_eq!(add(1, 2), 3);", "assert!(true);")),
        )],
        "tests/math.rs",
        "tautology",
    );
    assert_flags(
        &[(
            "web/add.test.ts",
            Some(&JS.replace("expect(add(1, 2)).toBe(3);", "expect(true).toBe(true);")),
        )],
        "web/add.test.ts",
        "tautology",
    );
    assert_flags(
        &[(
            "pkg/test_models.py",
            Some(&PY.replace("assert add(1, 2) == 3", "assert True")),
        )],
        "pkg/test_models.py",
        "tautology",
    );
    assert_flags(
        &[(
            "tests/new.rs",
            Some("#[test]\nfn same() {\n    let x = 1;\n    assert_eq!(x, x);\n}\n"),
        )],
        "tests/new.rs",
        "tautology",
    );
}

#[test]
fn an_unchanged_or_growing_suite_raises_no_flag() {
    for changes in [
        vec![],
        vec![(
            "src/add.rs",
            Some("pub fn add(a: i32, b: i32) -> i32 { b + a }\n"),
        )],
        vec![(
            "tests/math.rs",
            Some(&*format!(
                "{RUST}\n#[test]\nfn zero() {{\n    assert_eq!(add(0, 0), 0);\n}}\n"
            )),
        )],
        vec![(
            "web/more.test.ts",
            Some("it(\"new\", () => { expect(add(1, 1)).toBe(2); });\n"),
        )],
    ] {
        let repo = Repo::new(&suite(), &changes);
        let (ok, out) = repo.check(&[]);
        assert!(ok, "{changes:?}: {out}");
        assert!(out.contains("no weakened tests"), "{out}");
    }
}

#[test]
fn skip_if_todo_and_only_added_to_an_existing_file_are_each_flagged() {
    for (from, to, rule) in [
        (
            "it(\"adds negatives\"",
            "it.skipIf(process.env.CI)(\"adds negatives\"",
            "more-skips",
        ),
        (
            "it(\"adds negatives\", () => {\n    expect(add(-1, -2)).toBe(-3);\n  });",
            "it(\"adds negatives\", () => {\n    expect(add(-1, -2)).toBe(-3);\n  });\n  it.todo(\"adds floats\");",
            "more-skips",
        ),
        ("it(\"adds\"", "it.only(\"adds\"", "focused-test"),
    ] {
        assert!(JS.contains(from), "{from}");
        assert_flags(
            &[("web/add.test.ts", Some(&JS.replace(from, to)))],
            "web/add.test.ts",
            rule,
        );
    }
}

#[test]
fn a_test_changes_line_in_the_body_clears_the_flags() {
    let repo = Repo::new(&suite(), &[("tests/math.rs", None)]);
    let body = repo.dir.path().join("body.md");

    std::fs::write(&body, "Summary only.\n").unwrap();
    let (ok, out) = repo.check(&["--body-file", body.to_str().unwrap()]);
    assert!(!ok, "{out}");

    std::fs::write(&body, "Summary.\n\nTest changes: math moved to doctests\n").unwrap();
    let (ok, out) = repo.check(&["--body-file", body.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert!(out.contains("tests/math.rs: deleted-test-file"), "{out}");
}

#[test]
fn only_files_matching_the_test_patterns_are_checked() {
    let files = [("checks/math.rs", RUST), ("tests/math.rs", RUST)];
    let deletions = [("checks/math.rs", None), ("tests/math.rs", None)];

    let mut configured = files.to_vec();
    configured.push(("devkit.toml", "[pr]\ntest_files = [\"checks/**\"]\n"));
    let (ok, out) = Repo::new(&configured, &deletions).check(&[]);
    assert!(!ok, "{out}");
    assert!(out.contains("checks/math.rs: deleted-test-file"), "{out}");
    assert!(
        !out.contains("tests/math.rs"),
        "outside the patterns: {out}"
    );

    let (ok, out) = Repo::new(&files, &deletions).check(&[]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("tests/math.rs: deleted-test-file"),
        "the defaults cover tests/: {out}"
    );
    assert!(!out.contains("checks/math.rs"), "{out}");
}

/// The branch's own devkit.toml decides nothing: the patterns come from the
/// merge base.
#[test]
fn a_branch_cannot_redefine_its_test_files() {
    let (ok, out) = Repo::new(&suite(), &[
        ("tests/math.rs", None),
        ("devkit.toml", Some("[pr]\ntest_files = [\"nothing/**\"]\n")),
    ])
    .check(&[]);
    assert!(!ok, "{out}");
    assert!(out.contains("tests/math.rs: deleted-test-file"), "{out}");
}

#[test]
fn an_unterminated_string_ending_in_a_multibyte_character_is_read() {
    let (ok, out) = Repo::new(&suite(), &[(
        "pkg/test_models.py",
        Some(&format!("{PY}x = \"\"\"\u{e9}a\n")),
    )])
    .check(&[]);
    assert!(ok, "{out}");
    assert!(out.contains("no weakened tests"), "{out}");
}

#[test]
fn a_deleted_test_file_is_flagged_in_a_language_the_scanner_does_not_read() {
    let files = [
        (
            "devkit.toml",
            "[pr]\ntest_files = [\"spec/**/*_spec.rb\"]\n",
        ),
        (
            "spec/models/add_spec.rb",
            "it { expect(add(1, 2)).to eq(3) }\n",
        ),
        (
            "spec/models/sub_spec.rb",
            "it { expect(sub(3, 2)).to eq(1) }\n",
        ),
    ];
    let (ok, out) = Repo::new(&files, &[
        ("spec/models/add_spec.rb", None),
        ("spec/models/sub_spec.rb", None),
        ("lib/sub.rb", Some("it { expect(sub(3, 2)).to eq(1) }\n")),
    ])
    .check(&[]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("spec/models/add_spec.rb: deleted-test-file"),
        "{out}"
    );
    assert!(
        out.contains("spec/models/sub_spec.rb: deleted-test-file"),
        "renamed out of the patterns: {out}"
    );
}

#[test]
fn an_added_js_regex_literal_is_not_read_as_an_assertion() {
    let (ok, out) = Repo::new(&suite(), &[(
        "web/add.test.ts",
        Some(&format!(
            "{JS}it(\"names\", () => {{\n  expect(x).toMatch(/assert(true)/);\n}});\n"
        )),
    )])
    .check(&[]);
    assert!(ok, "an added regex is not a tautology: {out}");
    assert!(out.contains("no weakened tests"), "{out}");
}

#[test]
fn a_removed_js_regex_literal_is_not_read_as_a_test() {
    let (ok, out) = Repo::new(
        &[(
            "web/add.test.ts",
            &*format!("{JS}const pattern = /test(foo)/;\n"),
        )],
        &[("web/add.test.ts", Some(JS))],
    )
    .check(&[]);
    assert!(ok, "a removed regex is not a removed test: {out}");
    assert!(out.contains("no weakened tests"), "{out}");
}
