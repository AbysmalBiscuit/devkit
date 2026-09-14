//! `devkit harness shell` with write enforcement: claims, conflicts, policy,
//! and the deadline, through the real binary. Each test has a private
//! project, HOME, and state directory, so no developer config, daemon, or
//! registry is reached.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Env {
    project: tempfile::TempDir,
    state: tempfile::TempDir,
}

fn env(config: &str) -> Env {
    let project = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(project.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::write(project.path().join("devkit.toml"), config).unwrap();
    Env {
        project,
        state: tempfile::tempdir().unwrap(),
    }
}

const WRITES: &str = "[harness]\nenforce_writes = true\n";

fn devkit(e: &Env, args: &[&str], stdin: Option<&str>, extra: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(args)
        .current_dir(e.project.path())
        .env("HOME", e.state.path())
        .env("XDG_STATE_HOME", e.state.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_ENFORCE_WRITES")
        .env_remove("DEVKIT_ENFORCE_COMMANDS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let mut pipe = child.stdin.take().unwrap();
    if let Some(s) = stdin {
        pipe.write_all(s.as_bytes()).unwrap();
    }
    drop(pipe);
    child.wait_with_output().unwrap()
}

fn payload(e: &Env, session: Option<&str>, tool: &str, command: &str) -> String {
    let mut p = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "prompt_id": "p",
        "tool_input": { "command": command },
        "cwd": e.project.path().to_string_lossy(),
    });
    if let Some(s) = session {
        p["session_id"] = s.into();
    }
    p.to_string()
}

fn unusable_shell_payload(e: &Env, codex: bool) -> String {
    let mut p = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "prompt_id": "p",
        "session_id": if codex { "C1" } else { "S1" },
        "tool_input": {},
        "cwd": e.project.path().to_string_lossy(),
    });
    if codex {
        p["turn_id"] = "t".into();
        p["model"] = "m".into();
    }
    p.to_string()
}

fn hook(e: &Env, session: Option<&str>, command: &str) -> Output {
    devkit(
        e,
        &["harness", "shell"],
        Some(&payload(e, session, "Bash", command)),
        &[],
    )
}

fn acquire(e: &Env, holder: &str, path: &str) {
    let out = devkit(e, &["locks", "acquire", "--as", holder, path], None, &[]);
    assert!(
        out.status.success(),
        "acquire: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn envelope(out: &Output) -> Option<serde_json::Value> {
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    (!s.trim().is_empty()).then(|| serde_json::from_str(&s).expect("stdout is JSON"))
}

fn denial(out: &Output) -> Option<String> {
    envelope(out).and_then(|v| {
        (v["hookSpecificOutput"]["permissionDecision"] == "deny").then(|| {
            v["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
    })
}

/// `(root-relative path, holder)` for every live row.
fn rows(e: &Env) -> Vec<(String, String)> {
    let file = e.state.path().join("devkit/locks.json");
    let Ok(body) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let mut out: Vec<(String, String)> = v["locks"]
        .as_object()
        .map(|m| {
            m.values()
                .map(|r| {
                    (
                        r["path"].as_str().unwrap().to_string(),
                        r["holder"].as_str().unwrap().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

#[test]
fn a_free_redirect_target_is_claimed_for_the_session() {
    let e = env(WRITES);
    let out = hook(&e, Some("S1"), "printf '%s\\n' 'print(1)' > .temp_demo.py");
    assert_eq!(denial(&out), None);
    assert_eq!(rows(&e), [(".temp_demo.py".to_string(), "S1".to_string())]);
    assert!(
        !e.project.path().join(".temp_demo.py").exists(),
        "the hook never runs the command"
    );
}

#[test]
fn another_sessions_lock_denies() {
    let e = env(WRITES);
    acquire(&e, "S2", "a.txt");
    let reason = denial(&hook(&e, Some("S1"), "echo x > a.txt")).expect("denied");
    assert!(reason.contains("S2"), "{reason}");
    assert_eq!(rows(&e), [("a.txt".to_string(), "S2".to_string())]);
}

#[test]
fn own_and_ancestor_claims_allow() {
    let e = env(WRITES);
    acquire(&e, "S1", "a.txt");
    assert_eq!(denial(&hook(&e, Some("S1"), "echo x > a.txt")), None);
    let sub = devkit(
        &e,
        &["harness", "shell"],
        Some(&{
            let mut p: serde_json::Value =
                serde_json::from_str(&payload(&e, Some("S1"), "Bash", "echo x > a.txt")).unwrap();
            p["agent_id"] = "a1".into();
            p.to_string()
        }),
        &[],
    );
    assert_eq!(denial(&sub), None);
}

#[test]
fn both_ends_of_a_rename_are_checked() {
    let e = env(WRITES);
    acquire(&e, "S2", "b.txt");
    assert!(denial(&hook(&e, Some("S1"), "mv a.txt b.txt")).is_some());
}

#[test]
fn an_outer_redirect_around_a_devkit_command_is_enforced() {
    let e = env("[harness]\nenforce_writes = true\nenforce_commands = true\n");
    acquire(&e, "S2", "shared.txt");
    assert!(denial(&hook(&e, Some("S1"), "devrun task check > shared.txt")).is_some());
}

#[test]
fn an_unresolved_write_blocks_by_default_and_warns_when_configured() {
    let e = env(WRITES);
    let reason = denial(&hook(&e, Some("S1"), "echo x > \"$OUT\"")).expect("denied");
    assert!(reason.contains("literal"), "{reason}");

    let w = env("[harness]\nenforce_writes = true\nunresolved_writes = \"warn\"\n");
    let v = envelope(&hook(&w, Some("S1"), "echo x > \"$OUT\"")).expect("a warning envelope");
    assert!(
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .is_some_and(|s| s.contains("could not be determined")),
        "{v}"
    );
    assert!(
        v["hookSpecificOutput"].get("permissionDecision").is_none(),
        "{v}"
    );
}

/// A `..` component, or an absolute later argument, carries a temp-derived
/// path back out of the directory that made it uncontendable.
#[test]
fn a_temp_path_that_leaves_its_fresh_directory_is_not_exempt() {
    let e = env(WRITES);
    acquire(&e, "B", "victim.txt");
    let outside = e.project.path().join("victim.txt");
    let outside = outside.to_string_lossy();
    let cases = [
        "python3 -c \"import tempfile, os; d = tempfile.mkdtemp(dir='.'); \
             open(os.path.join(d, '../victim.txt'), 'w').write('x')\""
            .to_string(),
        format!(
            "python3 -c \"import tempfile, os; d = tempfile.mkdtemp(); \
             open(os.path.join(d, '{outside}'), 'w').write('x')\""
        ),
        "python3 -c \"import tempfile, pathlib; d = tempfile.mkdtemp(); \
             pathlib.Path(d, '../victim.txt').write_text('x')\""
            .to_string(),
        "bun -e \"const fs = require('fs'); const path = require('path'); \
             const d = fs.mkdtempSync('./fresh-'); \
             fs.writeFileSync(path.join(d, '../victim.txt'), 'x')\""
            .to_string(),
        "D=$(mktemp -d ./fresh.XXXXXX); echo x > \"$D/../victim.txt\"".to_string(),
        "python3 -c \"import tempfile, pathlib; \
         pathlib.Path(tempfile.mkdtemp()).joinpath('../victim.txt').write_text('x')\""
            .to_string(),
        "python3 -c \"import tempfile, pathlib; \
         (pathlib.Path(tempfile.mkdtemp(dir='.')).parent / 'victim.txt').write_text('x')\""
            .to_string(),
        "python3 -c \"import tempfile, pathlib; \
         p = pathlib.Path(tempfile.mkdtemp()).joinpath('victim.txt'); \
         open(p.name, 'w').write('x')\""
            .to_string(),
        "python3 -c \"import tempfile, pathlib; \
         pathlib.Path(tempfile.mkdtemp(dir='.')).parent.rename('moved')\""
            .to_string(),
    ];
    for c in &cases {
        let reason = denial(&hook(&e, Some("S1"), c)).unwrap_or_else(|| panic!("allowed: {c}"));
        assert!(reason.contains("could not be determined"), "{c}: {reason}");
    }
}

/// A destination outside the fresh directory is an ordinary target, so it is
/// claimed and the holder is named rather than reported as undeterminable.
#[test]
fn a_copy_or_move_out_of_a_fresh_directory_names_the_holder() {
    let e = env(WRITES);
    acquire(&e, "B", "victim.txt");
    let cases = [
        "python3 -c \"import tempfile, pathlib; \
         pathlib.Path(tempfile.mkdtemp()).copy('victim.txt')\"",
        "python3 -c \"import tempfile, pathlib; \
         pathlib.Path(tempfile.mkdtemp()).move('victim.txt')\"",
        "python3 -c \"import tempfile, pathlib; \
         pathlib.Path(tempfile.mkdtemp()).rename('victim.txt')\"",
    ];
    for c in cases {
        let reason = denial(&hook(&e, Some("S1"), c)).unwrap_or_else(|| panic!("allowed: {c}"));
        assert!(reason.contains("locked by another agent"), "{c}: {reason}");
    }
}

/// An unknown suffix cannot show that the destination stayed inside the fresh
/// directory, so it does not inherit the exemption.
#[test]
fn an_unknown_component_does_not_keep_a_temp_path_exempt() {
    let e = env(WRITES);
    let cases = [
        "python3 -c \"import tempfile, os; d = tempfile.mkdtemp(); \
         open(os.path.join(d, os.environ['REL']), 'w').write('x')\"",
        "python3 -c \"import tempfile, os; d = tempfile.mkdtemp(dir=os.environ['X']); \
         open(os.path.join(d, 'out.txt'), 'w').write('x')\"",
    ];
    for c in cases {
        assert!(denial(&hook(&e, Some("S1"), c)).is_some(), "allowed: {c}");
    }
}

/// A fresh random name cannot be held by anyone, but the directory it is made
/// in can be, and that claim covers everything born under it.
#[test]
fn a_fresh_entry_under_a_held_directory_is_denied() {
    let e = env(WRITES);
    acquire(&e, "B", ".");
    let cases = [
        "python3 -c \"import tempfile; f = tempfile.NamedTemporaryFile(dir='.'); f.write(b'x')\"",
        "T=$(mktemp -p .); echo x > \"$T\"",
        "T=$(mktemp -p . fresh.XXXXXX); echo x > \"$T\"",
        "D=$(mktemp -d ./fresh.XXXXXX); echo x > \"$D\"",
        "python3 -c \"import tempfile, os; d = tempfile.mkdtemp(dir='.'); \
         open(os.path.join(d, 'out.txt'), 'w').write('x')\"",
        "python3 -c \"import tempfile, pathlib; \
         pathlib.Path(tempfile.mkdtemp(dir='.')).write_text('x')\"",
    ];
    for c in cases {
        let reason = denial(&hook(&e, Some("S1"), c)).unwrap_or_else(|| panic!("allowed: {c}"));
        assert!(reason.contains("locked by another agent"), "{reason}");
    }
}

/// `move` and `move_into` rename the source away, so the source is written
/// too, not just the destination.
#[test]
fn a_move_names_the_source_it_renames_away() {
    let e = env(WRITES);
    std::fs::write(e.project.path().join("held.txt"), "x").unwrap();
    std::fs::create_dir(e.project.path().join("free")).unwrap();
    acquire(&e, "B", "held.txt");
    let cases = [
        "python3 -c \"import pathlib; pathlib.Path('held.txt').move('free.txt')\"",
        "python3 -c \"import pathlib; pathlib.Path('held.txt').move_into('free')\"",
    ];
    for c in cases {
        let reason = denial(&hook(&e, Some("S1"), c)).unwrap_or_else(|| panic!("allowed: {c}"));
        assert!(reason.contains("held.txt (held by B)"), "{reason}");
    }
}

/// `mkdir` and `rmdir` on a resolved `pathlib.Path` write, and were reaching
/// the registry only through a freshly created receiver.
#[test]
fn a_pathlib_directory_method_reaches_the_registry() {
    let e = env(WRITES);
    std::fs::create_dir(e.project.path().join("build")).unwrap();
    acquire(&e, "B", "build");
    let cases = [
        "python3 -c \"import pathlib; pathlib.Path('build').rmdir()\"",
        "python3 -c \"import pathlib; pathlib.Path('build/sub').mkdir()\"",
    ];
    for c in cases {
        let reason = denial(&hook(&e, Some("S1"), c)).unwrap_or_else(|| panic!("allowed: {c}"));
        assert!(reason.contains("locked by another agent"), "{reason}");
    }
}

/// A write mode still writes when the path it opens could not be determined,
/// and a read mode still writes nothing.
#[test]
fn an_open_on_an_undetermined_path_reports_the_write() {
    let e = env(WRITES);
    let write = "python3 -c \"import tempfile, pathlib; \
                 p = pathlib.Path(tempfile.mkdtemp(dir='.')).parent.joinpath('victim.txt'); \
                 p.open('w').write('x')\"";
    let reason = denial(&hook(&e, Some("S1"), write)).unwrap_or_else(|| panic!("allowed"));
    assert!(reason.contains("could not be determined"), "{reason}");

    let read = "python3 -c \"import tempfile, pathlib; \
                p = pathlib.Path(tempfile.mkdtemp(dir='.')).parent.joinpath('notes.txt'); \
                p.open().read()\"";
    assert_eq!(denial(&hook(&e, Some("S1"), read)), None);
}

/// A quoted `mktemp` argument names the same directory the shell would use.
#[test]
fn a_quoted_mktemp_directory_is_read_without_its_quotes() {
    let e = env(WRITES);
    std::fs::create_dir(e.project.path().join("held")).unwrap();
    acquire(&e, "B", "held");
    let cases = [
        "T=$(mktemp -p 'held' fresh.XXXXXX); echo x > \"$T\"",
        "T=$(mktemp -p \"held\"); echo x > \"$T\"",
    ];
    for c in cases {
        let reason = denial(&hook(&e, Some("S1"), c)).unwrap_or_else(|| panic!("allowed: {c}"));
        assert!(reason.contains("held (held by B)"), "{reason}");
    }
}

/// A claim below a directory reaches no name created fresh in it: the fresh
/// name is a sibling of the held path, never a parent of it.
#[test]
fn a_claim_below_a_directory_leaves_a_fresh_entry_in_it_alone() {
    let e = env(WRITES);
    std::fs::create_dir(e.project.path().join("src")).unwrap();
    std::fs::write(e.project.path().join("src/model.rs"), "x").unwrap();
    acquire(&e, "B", "src/model.rs");
    let cases = [
        "python3 -c \"import tempfile; f = tempfile.NamedTemporaryFile(dir='.'); f.write(b'x')\"",
        "T=$(mktemp -p .); echo x > \"$T\"",
        // Everything under a directory created a moment ago is itself fresh, so
        // a recursive writer rooted there reaches nothing anyone holds.
        "rm -rf \"$(mktemp -d -p .)\"",
    ];
    for c in cases {
        assert_eq!(denial(&hook(&e, Some("S1"), c)), None, "denied: {c}");
    }
    assert_eq!(rows(&e), [("src/model.rs".to_string(), "B".to_string())]);
}

/// A claim above a directory covers every name created in it, and the refusal
/// names that claim rather than the directory the command asked about.
#[test]
fn a_claim_above_a_directory_blocks_a_fresh_entry_and_names_the_claim() {
    let e = env(WRITES);
    std::fs::create_dir(e.project.path().join("src")).unwrap();
    acquire(&e, "B", ".");
    let c = "T=$(mktemp -p src); echo x > \"$T\"";
    let reason = denial(&hook(&e, Some("S1"), c)).unwrap_or_else(|| panic!("allowed: {c}"));
    assert!(reason.contains(". (held by B)"), "{reason}");
}

/// A writer that rewrites files it did not create still conflicts with a claim
/// under its directory, which is what the broader check is for.
#[test]
fn a_tree_writer_still_conflicts_with_a_claim_under_its_directory() {
    let e = env(WRITES);
    std::fs::create_dir(e.project.path().join("build")).unwrap();
    std::fs::write(e.project.path().join("build/out.o"), "x").unwrap();
    acquire(&e, "B", "build/out.o");
    let reason = denial(&hook(&e, Some("S1"), "rm -rf build")).unwrap_or_else(|| panic!("allowed"));
    assert!(reason.contains("locked by another agent"), "{reason}");
}

/// `mktemp` failure leaves the variable empty, which turns the rest of the
/// word into an absolute path nobody claimed on this session's behalf.
#[test]
fn a_failed_mktemp_substitution_does_not_launder_the_suffix() {
    let e = env(WRITES);
    acquire(&e, "B", "victim.txt");
    let victim = e.project.path().join("victim.txt");
    let c = format!(
        "D=$(mktemp -d /missing-parent/XXXXXX); echo x > \"$D{}\"",
        victim.to_string_lossy()
    );
    assert!(denial(&hook(&e, Some("S1"), &c)).is_some(), "allowed: {c}");
}

/// The exemption this feature exists for: a destination that provably stays
/// inside a directory created fresh under a random name.
#[test]
fn a_write_that_stays_inside_a_fresh_directory_is_allowed() {
    let e = env(WRITES);
    acquire(&e, "B", "victim.txt");
    let cases = [
        "python3 -c \"import tempfile, os; d = tempfile.mkdtemp(); \
         open(os.path.join(d, 'out.txt'), 'w').write('x')\"",
        "python3 -c \"import tempfile, pathlib; d = tempfile.mkdtemp(); \
         pathlib.Path(d, 'sub', 'out.txt').write_text('x')\"",
        "python3 -c \"import tempfile; f = tempfile.NamedTemporaryFile(); f.write(b'x')\"",
        "bun -e \"const fs = require('fs'); const path = require('path'); \
         const d = fs.mkdtempSync('/tmp/fresh-'); \
         fs.writeFileSync(path.join(d, 'out.txt'), 'x')\"",
        "T=$(mktemp); echo x > \"$T\"",
        "python3 -c \"import tempfile, pathlib; \
         pathlib.Path(tempfile.mkdtemp()).joinpath('sub', 'out.txt').write_text('x')\"",
    ];
    for c in cases {
        let out = hook(&e, Some("S1"), c);
        assert!(denial(&out).is_none(), "denied: {c}");
    }
    assert_eq!(rows(&e), [("victim.txt".to_string(), "B".to_string())]);
}

#[test]
fn a_root_claim_does_not_unblock_an_unresolved_write() {
    let e = env(WRITES);
    acquire(&e, "S1", ".");
    let reason = denial(&hook(&e, Some("S1"), "echo x > \"$OUT\"")).expect("denied");
    assert!(reason.contains("could not be determined"), "{reason}");
}

#[test]
fn a_warning_cannot_override_a_known_conflict() {
    let e = env("[harness]\nenforce_writes = true\nunresolved_writes = \"warn\"\n");
    acquire(&e, "S2", "a.txt");
    assert!(denial(&hook(&e, Some("S1"), "echo x > a.txt; echo y > \"$OUT\"")).is_some());
}

#[test]
fn an_argv_bound_python_edit_claims_its_target() {
    let e = env(WRITES);
    let command = "f=src/a.ts; python3 - \"$f\" <<'PY'\nimport sys\nfrom pathlib import Path\nPath(sys.argv[1]).write_text('x')\nPY\n";
    assert_eq!(denial(&hook(&e, Some("S1"), command)), None);
    assert_eq!(rows(&e), [("src/a.ts".to_string(), "S1".to_string())]);
}

#[test]
fn read_only_commands_and_quoted_text_claim_nothing() {
    let e = env(WRITES);
    for command in [
        "cat README.md | rg foo",
        "git commit -m \"echo x > y.txt\"",
        "node -e \"console.log(require.resolve('x'))\"",
    ] {
        assert_eq!(denial(&hook(&e, Some("S1"), command)), None, "{command}");
    }
    assert!(rows(&e).is_empty(), "{:?}", rows(&e));
}

#[test]
fn a_tree_writer_is_checked_and_claims_nothing() {
    let e = env(WRITES);
    assert_eq!(denial(&hook(&e, Some("S1"), "cargo fmt")), None);
    assert!(rows(&e).is_empty());
    acquire(&e, "S2", "src/lib.rs");
    assert!(denial(&hook(&e, Some("S1"), "cargo fmt")).is_some());
    assert_eq!(rows(&e), [("src/lib.rs".to_string(), "S2".to_string())]);
}

#[test]
fn unsupported_language_and_script_file_policies() {
    let e = env(WRITES);
    assert!(denial(&hook(&e, Some("S1"), "perl -e 'print 1'")).is_some());
    assert_eq!(denial(&hook(&e, Some("S1"), "python3 tools/gen.py")), None);

    let open = env(
        "[harness]\nenforce_writes = true\nunsupported_language = \"allow\"\nscript_files = \"block\"\n",
    );
    assert_eq!(denial(&hook(&open, Some("S1"), "perl -e 'print 1'")), None);
    assert!(denial(&hook(&open, Some("S1"), "python3 tools/gen.py")).is_some());
}

#[test]
fn a_write_without_a_session_id_is_denied() {
    let e = env(WRITES);
    let reason = denial(&hook(&e, None, "echo x > a.txt")).expect("denied");
    assert!(reason.contains("session_id"), "{reason}");
}

#[test]
fn with_write_enforcement_off_the_hook_writes_no_registry_row() {
    let e = env("[harness]\nenforce_commands = true\n");
    assert_eq!(denial(&hook(&e, Some("S1"), "echo x > a.txt")), None);
    assert!(!e.state.path().join("devkit/locks.json").exists());
}

#[test]
fn a_malformed_rule_does_not_disable_write_enforcement() {
    let e = env(
        "[harness]\nenforce_writes = true\nenforce_commands = true\n[harness.commands.bad]\nprograms = \"git\"\n",
    );
    assert_eq!(denial(&hook(&e, Some("S1"), "echo x > a.txt")), None);
    assert_eq!(rows(&e), [("a.txt".to_string(), "S1".to_string())]);
}

/// An advisory lock on `path`, created if absent. The caller takes the guard
/// from it and holds the guard for as long as the lock should be held.
fn hold(path: &Path) -> fd_lock::RwLock<std::fs::File> {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .unwrap();
    fd_lock::RwLock::new(file)
}

#[test]
fn a_stalled_registry_denies_within_the_deadline() {
    let e = env(WRITES);
    let mut lock = hold(&e.state.path().join("devkit/locks.lock"));
    let _held = lock.write().unwrap();
    let start = Instant::now();
    let reason = denial(&hook(&e, Some("S1"), "echo x > a.txt")).expect("denied");
    assert!(reason.contains("did not answer"), "{reason}");
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "took {:?}",
        start.elapsed()
    );
}

#[test]
fn a_registry_failure_denies() {
    let e = env(WRITES);
    let mut gate = hold(&e.state.path().join("devkit/devkitd.lock"));
    let _held = gate.write().unwrap();
    let reason = denial(&hook(&e, Some("S1"), "echo x > a.txt")).expect("denied");
    assert!(reason.contains("registry error"), "{reason}");
}

#[test]
fn the_powershell_tool_is_read_as_powershell_whatever_the_hook_env_says() {
    let e = env(WRITES);
    let out = devkit(
        &e,
        &["harness", "shell"],
        Some(&payload(
            &e,
            Some("S1"),
            "PowerShell",
            "Set-Content -Path out.txt -Value x",
        )),
        &[("SHELL", "/usr/bin/bash"), ("MSYSTEM", "MINGW64")],
    );
    assert_eq!(denial(&out), None);
    assert_eq!(rows(&e), [("out.txt".to_string(), "S1".to_string())]);
}

#[test]
fn a_codex_payload_claims_through_the_same_path() {
    let e = env(WRITES);
    let p = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "turn_id": "t",
        "model": "m",
        "session_id": "C1",
        "tool_input": { "command": "echo x > c.txt" },
        "cwd": e.project.path().to_string_lossy(),
    });
    assert_eq!(
        denial(&devkit(&e, &["harness", "shell"], Some(&p.to_string()), &[],)),
        None
    );
    assert_eq!(rows(&e), [("c.txt".to_string(), "C1".to_string())]);
}

#[test]
fn a_cursor_payload_never_claims() {
    let e = env(WRITES);
    let p = serde_json::json!({
        "command": "echo x > a.txt",
        "cwd": e.project.path().to_string_lossy(),
        "conversation_id": "x",
    });
    let out = devkit(&e, &["harness", "shell"], Some(&p.to_string()), &[]);
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
    assert!(rows(&e).is_empty());
}

#[test]
fn an_unusable_claude_shell_payload_denies_when_writes_are_enabled() {
    let e = env(WRITES);
    let out = devkit(
        &e,
        &["harness", "shell"],
        Some(&unusable_shell_payload(&e, false)),
        &[],
    );
    let reason = denial(&out).expect("fail-closed denial");
    assert!(
        reason.contains("shell payload") && reason.contains("fail-closed"),
        "{reason}"
    );
}

#[test]
fn an_unusable_codex_shell_payload_denies_when_writes_are_enabled() {
    let e = env(WRITES);
    let out = devkit(
        &e,
        &["harness", "shell"],
        Some(&unusable_shell_payload(&e, true)),
        &[],
    );
    let reason = denial(&out).expect("fail-closed denial");
    assert!(
        reason.contains("shell payload") && reason.contains("fail-closed"),
        "{reason}"
    );
}

#[test]
fn an_unusable_shell_payload_is_silent_when_writes_are_disabled() {
    let e = env("[harness]\n");
    for codex in [false, true] {
        let out = devkit(
            &e,
            &["harness", "shell"],
            Some(&unusable_shell_payload(&e, codex)),
            &[],
        );
        assert_eq!(denial(&out), None, "codex={codex}");
        assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
    }
}

#[test]
fn a_non_shell_claude_payload_stays_silent_when_writes_are_enabled() {
    let e = env(WRITES);
    let p = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "prompt_id": "p",
        "session_id": "S1",
        "tool_input": {},
        "cwd": e.project.path().to_string_lossy(),
    });
    let out = devkit(&e, &["harness", "shell"], Some(&p.to_string()), &[]);
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
    assert!(rows(&e).is_empty());
}
