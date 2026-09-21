//! The edit guard's stdout, pinned.
//!
//! A context-injection stage attached to this hook has to stay off the deny
//! path, and this file is what keeps it there. A second JSON object appended
//! after a denial makes the whole of stdout unparseable, and a harness that
//! cannot parse a hook's stdout treats it as plain text carrying no decision.
//! The denial is lost and the write proceeds.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

/// A private git project with the write harness enforced.
pub fn project() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(p.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::write(
        p.path().join("devkit.toml"),
        "[harness]\nenforce_writes = true\n",
    )
    .unwrap();
    p
}

/// `devkit hook pre-tool-use` against a private HOME and state home, so a run
/// started from inside a coding agent resolves the same holder CI does.
pub fn run_hook(project: &Path, state: &Path, payload: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(["hook", "pre-tool-use", "--harness", "claude-code"])
        .current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_ENFORCE_WRITES")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    testenv::scrub_identity(&mut cmd);
    let mut child = cmd.spawn().expect("spawn the devkit hook");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().expect("hook output")
}

/// Claim `path` for `holder`, the way another session would have. `devkit locks
/// acquire` is the same verb `lockm acquire` reaches, so no shim is needed.
pub fn hold(project: &Path, state: &Path, path: &str, holder: &str) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(["locks", "acquire", path, "--as", holder])
        .current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1");
    testenv::scrub_identity(&mut cmd);
    let out = cmd.output().expect("spawn devkit locks acquire");
    assert!(out.status.success(), "the other holder should acquire");
}

pub fn write_payload(session: &str, agent: Option<&str>, cwd: &Path, target: &str) -> String {
    let mut payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_input": { "file_path": target }
    });
    if let Some(agent) = agent {
        payload["agent_id"] = serde_json::Value::String(agent.to_string());
    }
    payload.to_string()
}

/// Stdout as exactly one JSON object, or `None` when it is empty. Parsing is
/// the assertion: two objects would fail here, which is the whole point.
pub fn one_object(out: &Output) -> Option<serde_json::Value> {
    assert_eq!(
        out.status.code(),
        Some(0),
        "the guard always exits 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.trim().is_empty() {
        return None;
    }
    Some(serde_json::from_str(&stdout).expect("stdout parses as exactly one JSON object"))
}

#[test]
fn a_conflicting_write_denies_with_one_object() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    hold(proj.path(), state.path(), "src/a.rs", "other-session");

    let payload = write_payload("S", None, proj.path(), "src/a.rs");
    let out = run_hook(proj.path(), state.path(), &payload);

    let v = one_object(&out).expect("a conflict denies");
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
}

#[test]
fn an_unconflicted_write_emits_nothing() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let payload = write_payload("S", None, proj.path(), "src/a.rs");
    let out = run_hook(proj.path(), state.path(), &payload);
    assert_eq!(one_object(&out), None, "an allow is silent");
}

/// A project whose `[rules]` are on, carrying an index at a known path and a
/// `[[context.files]]` entry under `crates/foo`.
fn rules_project(state: &Path) -> tempfile::TempDir {
    let p = project();
    let index = p.path().join("index.json");
    std::fs::copy("crates/devkit-rules/tests/fixtures/index.json", &index).unwrap();
    std::fs::create_dir_all(p.path().join("crates/foo")).unwrap();
    std::fs::write(p.path().join("crates/foo/AGENTS.md"), "foo house rules").unwrap();
    std::fs::write(
        p.path().join("devkit.toml"),
        format!(
            // A literal string, not a basic one: a Windows path carries
            // backslashes, and `"C:\\Users..."` is an invalid escape.
            "[harness]\nenforce_writes = true\n\n\
             [rules]\nenabled = true\nmin_severity = \"should\"\n\
             index = '{}'\n\n\
             [[context.files]]\npath = \"crates/foo/AGENTS.md\"\n",
            index.display()
        ),
    )
    .unwrap();
    let _ = state;
    p
}

fn injected_text(out: &Output) -> String {
    let v = one_object(out).expect("an allow with matching rules emits");
    assert!(
        v["hookSpecificOutput"].get("permissionDecision").is_none(),
        "the rules stage never carries a decision: {v}"
    );
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext")
        .to_string()
}

/// The deny path, byte-identical, with a rule *and* a file both matching. A
/// test over an unmatched deny would pass without ever reaching the code that
/// could break this.
#[test]
fn a_denial_emits_no_rules_and_stamps_nothing() {
    let state = tempfile::tempdir().unwrap();
    let proj = rules_project(state.path());
    hold(
        proj.path(),
        state.path(),
        "crates/foo/src/a.rs",
        "other-session",
    );

    let payload = write_payload("S", None, proj.path(), "crates/foo/src/a.rs");
    let out = run_hook(proj.path(), state.path(), &payload);

    let v = one_object(&out).expect("a conflict denies");
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
    let text = serde_json::to_string(&v).unwrap();
    assert!(!text.contains("Foo should"), "no rule rides along: {text}");
    assert!(
        !text.contains("foo house rules"),
        "no file rides along: {text}"
    );

    // Byte-for-byte against the same conflict with rules switched off: the deny
    // path must be indistinguishable from what it emitted before this feature.
    let off_state = tempfile::tempdir().unwrap();
    let off = rules_project(off_state.path());
    std::fs::write(
        off.path().join("devkit.toml"),
        "[harness]\nenforce_writes = true\n\n[rules]\nenabled = false\n",
    )
    .unwrap();
    hold(
        off.path(),
        off_state.path(),
        "crates/foo/src/a.rs",
        "other-session",
    );
    let baseline = run_hook(
        off.path(),
        off_state.path(),
        &write_payload("S", None, off.path(), "crates/foo/src/a.rs"),
    );
    // Each output is normalized against its own project root, so the two are
    // compared on content rather than on which tempdir produced them.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).replace(proj.path().to_string_lossy().as_ref(), ""),
        String::from_utf8_lossy(&baseline.stdout)
            .replace(off.path().to_string_lossy().as_ref(), ""),
        "the deny envelope is unchanged by the rules feature"
    );

    let fired = state.path().join("devkit/rules");
    assert!(
        !fired.exists() || std::fs::read_dir(&fired).unwrap().next().is_none(),
        "a denial stamps nothing, so the retry carries the rules"
    );
}

#[test]
fn an_allow_emits_the_matching_rule_once_per_holder() {
    let state = tempfile::tempdir().unwrap();
    let proj = rules_project(state.path());
    let payload = write_payload("S", None, proj.path(), "crates/foo/src/a.rs");

    let first = injected_text(&run_hook(proj.path(), state.path(), &payload));
    assert!(first.contains("Foo should"), "the directory rule: {first}");
    assert!(first.contains("Root must"), "the repo rule: {first}");
    assert!(!first.contains("Foo can"), "below the floor: {first}");
    assert!(first.contains("foo house rules"), "the file dump: {first}");

    let second = run_hook(proj.path(), state.path(), &payload);
    assert_eq!(one_object(&second), None, "the second call is silent");
}

#[test]
fn a_subagent_gets_a_rule_its_parent_already_fired() {
    let state = tempfile::tempdir().unwrap();
    let proj = rules_project(state.path());
    let target = "crates/foo/src/a.rs";

    let parent = write_payload("S", None, proj.path(), target);
    injected_text(&run_hook(proj.path(), state.path(), &parent));

    let child = write_payload("S", Some("a1"), proj.path(), target);
    let text = injected_text(&run_hook(proj.path(), state.path(), &child));
    assert!(
        text.contains("Foo should"),
        "the subagent's context never held what the parent was shown: {text}"
    );
}

/// An `apply_patch` envelope, the only multi-target write in devkit's model:
/// every other write tool names one `tool_input.file_path`.
fn patch_payload(session: &str, cwd: Option<&Path>, targets: &[&str]) -> String {
    let mut patch = String::from("*** Begin Patch\n");
    for target in targets {
        patch.push_str(&format!("*** Update File: {target}\n"));
    }
    patch.push_str("*** End Patch\n");
    let mut payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "apply_patch",
        "session_id": session,
        "tool_input": { "command": patch }
    });
    if let Some(cwd) = cwd {
        payload["cwd"] = serde_json::Value::String(cwd.to_string_lossy().into_owned());
    }
    payload.to_string()
}

/// No `cwd` key, so a relative target cannot be resolved.
/// `apply_patch_paths` takes paths verbatim, relative to the session's cwd.
#[test]
fn a_payload_without_cwd_drops_relative_targets_and_keeps_absolute_ones() {
    let state = tempfile::tempdir().unwrap();
    let proj = rules_project(state.path());
    let absolute = proj.path().join("crates/foo/src/a.rs");
    let absolute = absolute.to_string_lossy().into_owned();
    let payload = patch_payload("S", None, &["relative/b.rs", &absolute]);

    let text = injected_text(&run_hook(proj.path(), state.path(), &payload));
    assert!(
        text.contains("Foo should"),
        "the absolute target still matches: {text}"
    );
}

/// A parent and its subagents append to one file.
#[test]
fn a_torn_line_in_the_fired_set_does_not_suppress_the_rest() {
    let state = tempfile::tempdir().unwrap();
    let proj = rules_project(state.path());
    let payload = write_payload("S", None, proj.path(), "crates/foo/src/a.rs");

    let first = injected_text(&run_hook(proj.path(), state.path(), &payload));
    assert!(first.contains("Foo should"));

    let dir = state.path().join("devkit/rules");
    let file = std::fs::read_dir(&dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut body = std::fs::read_to_string(&file).unwrap();
    body.push_str("\u{0}\u{1}torn\n");
    std::fs::write(&file, body).unwrap();

    let out = run_hook(proj.path(), state.path(), &payload);
    assert_eq!(one_object(&out), None, "the valid ids still suppress");
}

#[test]
fn a_multi_target_call_unions_the_rules_for_every_target() {
    let state = tempfile::tempdir().unwrap();
    let proj = rules_project(state.path());
    let payload = patch_payload("S", Some(proj.path()), &[
        "crates/foo/src/a.rs",
        "README.md",
    ]);

    let text = injected_text(&run_hook(proj.path(), state.path(), &payload));
    assert!(text.contains("Foo should"), "the crates/foo target: {text}");
    assert!(text.contains("Root must"), "the root target: {text}");
}

/// `[[context.files]]` must fire whether the project is reached through its
/// real path or through a symlink to it: git always reports the checkout root
/// resolved, and layer discovery keeps the spelling it was reached with.
#[cfg(unix)]
#[test]
fn context_files_fire_when_the_project_is_reached_through_a_symlink() {
    let state = tempfile::tempdir().unwrap();
    let real = rules_project(state.path());
    let link_dir = tempfile::tempdir().unwrap();
    let link = link_dir.path().join("via-symlink");
    std::os::unix::fs::symlink(real.path(), &link).unwrap();

    let payload = write_payload("S", None, &link, "crates/foo/src/a.rs");
    let text = injected_text(&run_hook(&link, state.path(), &payload));
    assert!(
        text.contains("foo house rules"),
        "the file dump through a symlinked root: {text}"
    );
}

/// An absolute target spelled through a symlinked project root, the shape a
/// harness produces on macOS. The target does not exist yet (the tool is
/// about to write it), so it cannot be canonicalized directly.
#[cfg(unix)]
#[test]
fn an_absolute_target_matches_through_a_symlinked_root() {
    let state = tempfile::tempdir().unwrap();
    let real = rules_project(state.path());
    let link_dir = tempfile::tempdir().unwrap();
    let link = link_dir.path().join("via-symlink");
    std::os::unix::fs::symlink(real.path(), &link).unwrap();

    let absolute = link.join("crates/foo/src/a.rs");
    let payload = write_payload("S", None, &link, &absolute.to_string_lossy());
    let text = injected_text(&run_hook(&link, state.path(), &payload));
    assert!(
        text.contains("Foo should"),
        "the absolute target through the symlink still matches: {text}"
    );
}

/// A project with three root-level context files that all fire for every
/// target, so `per_event_limit` is the only thing standing between "one" and
/// "all three" landing in one event.
fn many_files_project(cap: usize) -> tempfile::TempDir {
    let p = project();
    for name in ["one.md", "two.md", "three.md"] {
        std::fs::write(p.path().join(name), format!("marker-{name}")).unwrap();
    }
    std::fs::write(
        p.path().join("devkit.toml"),
        format!(
            "[harness]\nenforce_writes = true\n\n\
             [rules]\nenabled = true\nper_event_limit = {cap}\n\n\
             [[context.files]]\npath = \"one.md\"\n\n\
             [[context.files]]\npath = \"two.md\"\n\n\
             [[context.files]]\npath = \"three.md\"\n"
        ),
    )
    .unwrap();
    p
}

#[test]
fn per_event_limit_bounds_files_as_well_as_rules() {
    let state = tempfile::tempdir().unwrap();
    let proj = many_files_project(1);
    let payload = write_payload("S", None, proj.path(), "src/a.rs");

    let text = injected_text(&run_hook(proj.path(), state.path(), &payload));
    let shown = ["one.md", "two.md", "three.md"]
        .iter()
        .filter(|name| text.contains(**name))
        .count();
    assert_eq!(shown, 1, "per_event_limit = 1 must cap files too: {text}");
}

/// Two files whose combined render exceeds `max_event_bytes`, so the second
/// one is cut. `render::block`'s file section for `big1.md` alone is 94
/// bytes; the cap sits above that and below the combined 188, so exactly one
/// file survives the cap.
fn capped_files_project(max_event_bytes: usize) -> tempfile::TempDir {
    let p = project();
    std::fs::write(p.path().join("big1.md"), "x".repeat(80)).unwrap();
    std::fs::write(p.path().join("big2.md"), "y".repeat(80)).unwrap();
    std::fs::write(
        p.path().join("devkit.toml"),
        format!(
            "[harness]\nenforce_writes = true\n\n\
             [rules]\nenabled = true\nmax_event_bytes = {max_event_bytes}\n\n\
             [[context.files]]\npath = \"big1.md\"\n\n\
             [[context.files]]\npath = \"big2.md\"\n"
        ),
    )
    .unwrap();
    p
}

#[test]
fn content_the_byte_cap_cuts_is_not_marked_as_fired() {
    let state = tempfile::tempdir().unwrap();
    let proj = capped_files_project(150);
    let payload = write_payload("S", None, proj.path(), "src/a.rs");

    let first = injected_text(&run_hook(proj.path(), state.path(), &payload));
    assert!(
        first.contains("big1.md"),
        "the file that fits is shown: {first}"
    );
    assert!(
        !first.contains("big2.md"),
        "the file that does not fit is dropped whole, not cut: {first}"
    );

    let second = injected_text(&run_hook(proj.path(), state.path(), &payload));
    assert!(
        second.contains("big2.md"),
        "what the cap cut must still be offered later, since it was never \
         actually shown: {second}"
    );
}
