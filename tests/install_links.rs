#[path = "common/shimtest.rs"]
mod shimtest;

use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// Retry `attempt` briefly on a transient `ExecutableFileBusy`. `staged()`
/// just finished writing the binary `attempt` executes moments earlier, and
/// `cargo test`'s default parallelism runs several of these tests at once,
/// each copying-then-executing its own freshly staged binary; occasionally
/// that races a kernel-level "busy" window on the just-closed write handle
/// (never a real conflict between two tests — each stages into its own
/// tempdir). The race clears within milliseconds. `attempt` is called again
/// from scratch on each retry so it can build a fresh `Command` (and fresh
/// `Stdio`s, which are not `Clone`) every time.
fn retry_on_busy<T>(mut attempt: impl FnMut() -> std::io::Result<T>) -> T {
    let mut attempts = 0;
    loop {
        match attempt() {
            Ok(v) => return v,
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 40 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => panic!("spawn: {e}"),
        }
    }
}

/// Run `install-links` (with `args` appended) against `exe`, isolated to a
/// throwaway `HOME`/`XDG_STATE_HOME`. `links::run` takes the `links.lock`
/// gate and writes the `links-version` stamp itself now (so a manual run
/// leaves the same steady state an automatic one would); without this, every
/// test that goes through this helper would write both into the developer's
/// real state dir.
fn run(exe: &std::path::Path, args: &[&str]) -> Output {
    let state = tempfile::tempdir().expect("state dir");
    retry_on_busy(|| {
        Command::new(exe)
            .args(args)
            .env("HOME", state.path())
            .env("XDG_STATE_HOME", state.path())
            .output()
    })
}

/// Run `install-links` against a throwaway directory holding a copy of the
/// built binary, so the test never touches the real CARGO_HOME.
fn staged() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let exe = dir.path().join(if cfg!(windows) {
        "devkit.exe"
    } else {
        "devkit"
    });
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

/// Runs with the automatic pass fully live (no `DEVKIT_SKIP_AUTOLINK`,
/// `run`'s own isolated `HOME`/`XDG_STATE_HOME` aside) — this is the
/// configuration real users run in, and the one `ensure_current` must stay
/// out of: were it to link ahead of `install-links` here, every shim would
/// already exist by the time `link_all` looked, and the report below would
/// say `current` for all six instead of `created`.
#[test]
fn creates_every_shim() {
    let (dir, exe) = staged();
    let out = run(&exe, &["install-links"]);
    assert!(
        out.status.success(),
        "install-links failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    for name in ["issue", "devrun", "portm", "lockm", "docm", "devkit-mcp"] {
        assert!(
            shim_path(dir.path(), name).exists(),
            "install-links did not create {name}"
        );
        assert!(
            text.lines()
                .any(|l| l.contains("created") && l.contains(name)),
            "should report {name} created, not preempted by the automatic pass: {text}"
        );
    }
}

/// The upgrade path: a real devkit binary already sits at a shim name.
#[test]
fn replaces_an_existing_devkit_binary() {
    let (dir, exe) = staged();
    let stale = shim_path(dir.path(), "portm");
    std::fs::copy(env!("CARGO_BIN_EXE_devkit"), &stale).expect("stage a stale portm");
    let out = run(&exe, &["install-links"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.lines()
            .any(|l| l.contains("replaced") && l.contains("portm")),
        "should report portm replaced: {text}"
    );
    assert!(
        shimtest::same_inode(&exe, &stale),
        "portm should now be a hardlink to devkit"
    );
}

/// The upgrade path must work at every shim name, not just `portm`. This is
/// the test that would have caught `lockm`'s hidden-subcommand breakage:
/// `replaces_an_existing_devkit_binary` alone never touched `lockm`, whose
/// `hook` subcommand is `#[command(hide = true)]` and so never appears in
/// its own `--help` output.
#[test]
fn replaces_a_real_devkit_binary_at_every_shim_name() {
    let (dir, exe) = staged();
    let mut stale_paths = Vec::new();
    for name in ["issue", "devrun", "portm", "lockm", "docm", "devkit-mcp"] {
        let stale = shim_path(dir.path(), name);
        std::fs::copy(env!("CARGO_BIN_EXE_devkit"), &stale)
            .unwrap_or_else(|e| panic!("stage a stale {name}: {e}"));
        stale_paths.push((name, stale));
    }
    let out = run(&exe, &["install-links"]);
    assert!(
        out.status.success(),
        "install-links failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    for (name, stale) in &stale_paths {
        assert!(
            text.lines()
                .any(|l| l.contains("replaced") && l.contains(name)),
            "should report {name} replaced: {text}"
        );
        assert!(
            shimtest::same_inode(&exe, stale),
            "{name} should now be a hardlink to devkit"
        );
    }
}

/// Coverage for the hide-filter itself, not just the marker responder: a
/// real devkit copy always answers the marker probe and is `Accepted` before
/// the `--help` path ever runs, so `replaces_a_real_devkit_binary_at_every_shim_name`
/// alone never reaches the subcommand check `lockm`'s hidden `hook` broke.
/// This wrapper is a faithful pre-marker devkit: it forwards `--version` and
/// `--help` to the real staged binary (`exec -a` forces its argv[0] to
/// `lockm`, so the output is genuinely lockm's own — `hook` is a real
/// subcommand but never printed in `--help`) while refusing the probe flag
/// outright, forcing `is_devkit_binary` down the `--help`/subcommand path
/// this test exists to guard. Unix-only: `exec -a` is a bash extension with
/// no portable Windows equivalent.
#[test]
#[cfg(unix)]
fn replaces_a_faithful_pre_marker_lockm() {
    let (dir, exe) = staged();
    let wrapper = shim_path(dir.path(), "lockm");
    let script = format!(
        "#!/usr/bin/env bash\nif [ \"$1\" = \"--devkit-shim-probe\" ]; then\n  exit 1\nfi\nexec -a lockm \"{}\" \"$@\"\n",
        exe.display()
    );
    std::fs::write(&wrapper, script).expect("write pre-marker lockm wrapper");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
            .expect("make wrapper executable");
    }
    let out = run(&exe, &["install-links"]);
    assert!(
        out.status.success(),
        "install-links failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.lines()
            .any(|l| l.contains("replaced") && l.contains("lockm")),
        "a faithful pre-marker lockm must be replaced via the subcommand path: {text}"
    );
    assert!(
        shimtest::same_inode(&exe, &wrapper),
        "lockm should now be a hardlink to devkit"
    );
}

/// A name held by something else is never destroyed. On Unix the script is
/// made executable so it actually runs and answers `--version` with output
/// that does not name devkit — reaching the content-comparison branch, not
/// just the "won't even execute" fallback. On Windows the same bytes are not
/// a valid PE, so there this test only re-exercises the can't-spawn branch;
/// the content-comparison coverage is Unix-only.
#[test]
fn leaves_a_foreign_binary_alone() {
    let (dir, exe) = staged();
    let foreign = shim_path(dir.path(), "issue");
    std::fs::write(&foreign, b"#!/bin/sh\necho not devkit\n").expect("write foreign issue");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o755))
            .expect("make foreign script executable");
    }
    let out = run(&exe, &["install-links"]);
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
    assert!(
        text.lines()
            .any(|l| l.contains("skipped") && l.contains("issue")),
        "should report the skip: {text}"
    );
}

/// A foreign binary that convincingly answers `--version` with `issue 1.2.3`
/// must still be rejected. Matching the version line alone is exactly what
/// the old, too-loose check accepted (a prefix match against the whole shim
/// set) — the fix requires `--help` to also name every real `issue`
/// subcommand, which this script's generic help text does not. On Unix the
/// script is made executable so it actually runs this scenario; on Windows
/// the bytes are not a valid executable, so there this test only re-exercises
/// the can't-spawn branch, the same as `leaves_a_foreign_binary_alone` — the
/// anchored-match/subcommand-probe rejection is Unix-only coverage.
#[test]
fn leaves_a_convincingly_named_foreign_binary_alone() {
    let (dir, exe) = staged();
    let foreign = shim_path(dir.path(), "issue");
    let script: &[u8] = b"#!/bin/sh\ncase \"$1\" in\n  --version) echo \"issue 1.2.3\" ;;\n  --help) echo \"issue 1.2.3\"; echo \"a foreign issue tracker, unrelated to devkit\" ;;\n  *) exit 1 ;;\nesac\n";
    std::fs::write(&foreign, script).expect("write convincing foreign issue");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o755))
            .expect("make foreign script executable");
    }
    let out = run(&exe, &["install-links"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&foreign).expect("foreign still readable"),
        script,
        "a foreign file at a shim name must not be replaced, even one echoing a matching version"
    );
    assert!(
        !shimtest::same_inode(&exe, &foreign),
        "a binary that only echoes a matching version string must not be linked over: {text}"
    );
    assert!(
        text.lines()
            .any(|l| l.contains("skipped") && l.contains("issue")),
        "should report the skip: {text}"
    );
}

/// The zero-subcommand exploit: `devkit-mcp` has no subcommands (`McpCli`
/// is a unit struct), so a foreign binary cannot be accepted by the
/// subcommand-set probe — there is nothing in it to check — and it does not
/// know to answer the identity-probe marker either. A bare anchored-version
/// check alone would have wrongly accepted this script.
#[test]
fn leaves_a_convincing_but_markerless_devkit_mcp_alone() {
    let (dir, exe) = staged();
    let foreign = shim_path(dir.path(), "devkit-mcp");
    let script: &[u8] = b"#!/bin/sh\ncase \"$1\" in\n  --version) echo \"devkit-mcp 1.2.3\" ;;\n  --help) echo \"devkit-mcp 1.2.3\"; echo \"a foreign mcp-like tool, unrelated to devkit\" ;;\n  *) exit 1 ;;\nesac\n";
    std::fs::write(&foreign, script).expect("write convincing foreign devkit-mcp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o755))
            .expect("make foreign script executable");
    }
    let out = run(&exe, &["install-links"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&foreign).expect("foreign still readable"),
        script,
        "a foreign file at devkit-mcp must not be replaced, even one echoing a matching version"
    );
    assert!(
        !shimtest::same_inode(&exe, &foreign),
        "a binary with no subcommands to verify against must not be linked over: {text}"
    );
    assert!(
        text.lines()
            .any(|l| l.contains("skipped") && l.contains("devkit-mcp")),
        "should report the skip: {text}"
    );
}

#[test]
fn force_takes_over_a_foreign_binary() {
    let (dir, exe) = staged();
    let foreign = shim_path(dir.path(), "issue");
    std::fs::write(&foreign, b"#!/bin/sh\necho not devkit\n").expect("write foreign issue");
    let out = run(&exe, &["install-links", "--force"]);
    assert!(out.status.success());
    assert!(
        shimtest::same_inode(&exe, &foreign),
        "--force should claim the name"
    );
}

/// Running install-links again once every shim is already the right hardlink
/// must be a no-op that reports it, not a remove-and-relink cycle.
#[test]
fn running_twice_is_idempotent() {
    let (dir, exe) = staged();
    let first = run(&exe, &["install-links"]);
    assert!(first.status.success());

    let second = run(&exe, &["install-links"]);
    assert!(second.status.success());
    let text = String::from_utf8_lossy(&second.stdout).to_string();
    assert!(
        text.contains("current"),
        "second run should report already-linked shims: {text}"
    );
    for name in ["issue", "devrun", "portm", "lockm", "docm", "devkit-mcp"] {
        assert!(
            shimtest::same_inode(&exe, &shim_path(dir.path(), name)),
            "{name} should still be a hardlink to devkit after the second run"
        );
    }
}

/// `ensure_current` calls `is_devkit_binary`'s probes on every shim
/// invocation, and `lockm hook pretooluse` reads its real payload from
/// stdin — a foreign binary sitting at a shim name while being probed must
/// never be able to consume that payload. Pipes a known sentinel into the
/// outer `install-links` process's own stdin and asserts a foreign script at
/// a shim name never received it: with the probe's stdin correctly nulled,
/// the script sees immediate EOF and writes nothing to the capture file.
#[test]
#[cfg(unix)]
fn probes_never_consume_the_callers_stdin() {
    use std::io::Write;

    let (dir, exe) = staged();
    let foreign = shim_path(dir.path(), "docm");
    let capture = dir.path().join("stdin-capture");
    let script = format!(
        "#!/bin/sh\ncat >> \"{}\" 2>/dev/null\nexit 1\n",
        capture.display()
    );
    std::fs::write(&foreign, script).expect("write stdin-capturing foreign docm");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o755))
            .expect("make foreign script executable");
    }

    let state = tempfile::tempdir().expect("state dir");
    let mut child = retry_on_busy(|| {
        Command::new(&exe)
            .arg("install-links")
            .env("HOME", state.path())
            .env("XDG_STATE_HOME", state.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    });
    child
        .stdin
        .take()
        .expect("install-links stdin")
        .write_all(b"SENTINEL-DATA-NOT-FOR-A-PROBE\n")
        .expect("write sentinel to install-links stdin");
    child.wait().expect("wait for install-links");

    let captured = std::fs::read(&capture).unwrap_or_default();
    assert!(
        captured.is_empty(),
        "a probe must not consume the caller's stdin: captured {:?}",
        String::from_utf8_lossy(&captured)
    );
}

/// A first run links without being asked.
#[test]
fn first_run_links_automatically() {
    let (dir, exe) = staged();
    let state = tempfile::tempdir().expect("state dir");
    let out = retry_on_busy(|| {
        Command::new(&exe)
            .arg("doctor")
            .env("HOME", state.path())
            .env("XDG_STATE_HOME", state.path())
            .output()
    });
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
    retry_on_busy(|| {
        Command::new(&exe)
            .arg("doctor")
            .env("HOME", state.path())
            .env("XDG_STATE_HOME", state.path())
            .output()
    });
    assert_eq!(
        std::fs::read(&foreign).expect("foreign still readable"),
        b"#!/bin/sh\necho not devkit\n",
        "the automatic path must never claim a foreign name"
    );
}

/// The stamp's version field names this build, across repeated runs. This
/// checks the stamp's *content*, not that a second run skips relinking —
/// `stamped_run_skips_relinking_work` is the test that proves that.
#[test]
fn second_run_writes_this_versions_stamp() {
    let (_dir, exe) = staged();
    let state = tempfile::tempdir().expect("state dir");
    for _ in 0..2 {
        retry_on_busy(|| {
            Command::new(&exe)
                .arg("doctor")
                .env("HOME", state.path())
                .env("XDG_STATE_HOME", state.path())
                .output()
        });
    }
    let stamp = state.path().join("devkit/links-version");
    let content = std::fs::read_to_string(&stamp).expect("stamp written");
    let version = content
        .trim()
        .split('\t')
        .next()
        .expect("stamp has a version field");
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
}

/// A stamped run must skip the actual linking work, not merely leave the
/// stamp's own content alone. Removing one shim after the first run and
/// invoking `devkit` a second time proves it: a real second linking pass
/// would recreate `portm`, so it staying gone is what shows `ensure_current`
/// returned before ever reaching the per-shim loop again.
///
/// Also removes `links.lock` after the first run, not just `portm`: the fast
/// path must return before the gate is ever opened, so if a regression
/// hoisted `create_dir_all`/the lock `open` above the stamp check, the lock
/// file would reappear even though nothing needed relinking. (A hoisted
/// `create_dir_all` alone is not observable this way — `state` already
/// exists by the second run regardless of ordering — so this closes the gate
/// half of that gap, not the directory-creation half.)
#[test]
fn stamped_run_skips_relinking_work() {
    let (dir, exe) = staged();
    let state = tempfile::tempdir().expect("state dir");
    retry_on_busy(|| {
        Command::new(&exe)
            .arg("doctor")
            .env("HOME", state.path())
            .env("XDG_STATE_HOME", state.path())
            .output()
    });
    let portm = shim_path(dir.path(), "portm");
    assert!(portm.exists(), "first run should have linked portm");
    std::fs::remove_file(&portm).expect("remove portm to detect a relink");
    let lock = state.path().join("devkit/links.lock");
    assert!(lock.exists(), "first run should have opened the gate");
    std::fs::remove_file(&lock).expect("remove links.lock to detect a reopen");

    retry_on_busy(|| {
        Command::new(&exe)
            .arg("doctor")
            .env("HOME", state.path())
            .env("XDG_STATE_HOME", state.path())
            .output()
    });
    assert!(
        !portm.exists(),
        "a stamped run must not redo the linking pass: portm was removed and must stay gone"
    );
    assert!(
        !lock.exists(),
        "a stamped run must not reopen the gate: links.lock was removed and must stay gone"
    );
}

/// The marker-probe intercept in `main` must answer before `ensure_current`
/// ever runs. This covers only the marker flag itself: `is_devkit_binary`'s
/// other two probes (`--version`, `--help`) are ordinary args that fall
/// straight through to `ensure_current` in a real invocation too, and what
/// bounds *those* is the `try_write` gate inside `ensure_current`, not this
/// intercept (see the comment there). Invoking the staged binary with the
/// marker flag directly, against a completely fresh state directory, must
/// produce neither a stamp file nor any shim link: if `ensure_current` ran
/// ahead of the intercept, this exact call — with no prior stamp and no gate
/// contention — would relink every shim and write the stamp before ever
/// answering the marker.
#[test]
fn probe_flag_never_triggers_linking() {
    let (dir, exe) = staged();
    let state = tempfile::tempdir().expect("state dir");
    let out = retry_on_busy(|| {
        Command::new(&exe)
            .arg("--devkit-shim-probe")
            .env("HOME", state.path())
            .env("XDG_STATE_HOME", state.path())
            .output()
    });
    assert!(out.status.success(), "the probe flag call must succeed");
    assert!(
        !shim_path(dir.path(), "portm").exists(),
        "a probe call must never link shims"
    );
    assert!(
        !state.path().join("devkit/links-version").exists(),
        "a probe call must never write the links stamp"
    );
}

/// The gate is a `try_write`, not a blocking wait: a `devkit` invocation that
/// finds `links.lock` already held must skip linking and still let the real
/// command run, rather than hang or error out. This only demonstrates that a
/// losing acquirer backs off cleanly — it says nothing about two genuine
/// concurrent writers never corrupting each other's output, which no
/// single-process test can observe.
#[test]
fn a_contended_gate_is_skipped_without_blocking() {
    let (dir, exe) = staged();
    let state = tempfile::tempdir().expect("state dir");
    let state_devkit = state.path().join("devkit");
    std::fs::create_dir_all(&state_devkit).expect("create state dir");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(state_devkit.join("links.lock"))
        .expect("open links.lock");
    let mut gate = fd_lock::RwLock::new(lock_file);
    let _held = gate.try_write().expect("hold the gate for the test");

    // `--json` so the check is on doctor's own report reaching stdout, not on
    // its exit code — a real config problem in the ambient cwd can make
    // `doctor` exit non-zero on its own merits, which is irrelevant here.
    let out = retry_on_busy(|| {
        Command::new(&exe)
            .args(["doctor", "--json"])
            .env("HOME", state.path())
            .env("XDG_STATE_HOME", state.path())
            .output()
    });
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .trim_start()
            .starts_with('['),
        "doctor must still run its report when the linking gate is contended: stdout {:?} stderr {:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !shim_path(dir.path(), "portm").exists(),
        "linking must not happen while another process holds the gate"
    );
}

/// A same-version reinstall — a fresh build replacing the file at `exe`
/// while the old hardlinks keep pointing at the old inode, the ordinary
/// pre-release loop of `cargo install --path .` — must relink even though
/// `CARGO_PKG_VERSION` didn't change. The stamp's mtime field is what
/// catches this: a version-only stamp from the first run would already
/// match and every shim would keep running the stale binary until the next
/// version bump.
#[test]
fn same_version_reinstall_relinks() {
    let (dir, exe) = staged();
    let state = tempfile::tempdir().expect("state dir");
    retry_on_busy(|| {
        Command::new(&exe)
            .arg("doctor")
            .env("HOME", state.path())
            .env("XDG_STATE_HOME", state.path())
            .output()
    });
    let portm = shim_path(dir.path(), "portm");
    assert!(portm.exists(), "first run should have linked portm");
    let first_stamp = std::fs::read_to_string(state.path().join("devkit/links-version"))
        .expect("first stamp written");

    // Reinstall at the same version: replace the file at `exe` (the old
    // hardlinks, `portm` included, keep pointing at the old inode) and force
    // a distinct mtime — a fresh copy's own mtime can otherwise land in the
    // same filesystem timestamp tick as the original, which would make this
    // test flaky rather than wrong.
    std::fs::remove_file(&exe).expect("remove for reinstall");
    std::fs::copy(env!("CARGO_BIN_EXE_devkit"), &exe).expect("reinstall devkit");
    let bumped = std::fs::metadata(&exe)
        .expect("exe metadata")
        .modified()
        .expect("exe mtime")
        + Duration::from_secs(120);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&exe)
        .expect("open exe to bump mtime")
        .set_modified(bumped)
        .expect("bump mtime");

    retry_on_busy(|| {
        Command::new(&exe)
            .arg("doctor")
            .env("HOME", state.path())
            .env("XDG_STATE_HOME", state.path())
            .output()
    });

    let second_stamp = std::fs::read_to_string(state.path().join("devkit/links-version"))
        .expect("second stamp written");
    assert_ne!(
        first_stamp, second_stamp,
        "a same-version reinstall must produce a different stamp (the mtime moved)"
    );
    assert!(
        shimtest::same_inode(&exe, &portm),
        "portm must be relinked to the reinstalled binary, not left pointing at the old one"
    );
}

/// `DEVKIT_SKIP_AUTOLINK` must prevent the linking work itself, not merely
/// let the surrounding command exit 0 regardless of whether it ran. Checked
/// on what would otherwise be a completely fresh first run: no shim gets
/// created and no stamp gets written.
#[test]
fn skip_autolink_env_prevents_linking() {
    let (dir, exe) = staged();
    let state = tempfile::tempdir().expect("state dir");
    retry_on_busy(|| {
        Command::new(&exe)
            .arg("doctor")
            .env("HOME", state.path())
            .env("XDG_STATE_HOME", state.path())
            .env("DEVKIT_SKIP_AUTOLINK", "1")
            .output()
    });
    assert!(
        !shim_path(dir.path(), "portm").exists(),
        "DEVKIT_SKIP_AUTOLINK must prevent linking, not just let the command exit 0"
    );
    assert!(
        !state.path().join("devkit/links-version").exists(),
        "DEVKIT_SKIP_AUTOLINK must prevent writing the stamp"
    );
}

/// `install-links` must take the same `links.lock` gate `ensure_current`
/// uses, held for its whole pass — not just check the stamp. Without this,
/// a probe child spawned mid-pass at a candidate that really is a devkit
/// binary would find the gate free and run its own automatic pass against
/// the same directory concurrently with this one. Simulated here by holding
/// the gate ourselves before invoking `install-links`: unlike
/// `ensure_current`'s silent backoff, a losing `install-links` must hard
/// error (the user asked for this pass specifically) and name the
/// contention, not silently link nothing while reporting success.
#[test]
fn install_links_hard_errors_when_the_gate_is_contended() {
    let (dir, exe) = staged();
    let state = tempfile::tempdir().expect("state dir");
    let state_devkit = state.path().join("devkit");
    std::fs::create_dir_all(&state_devkit).expect("create state dir");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(state_devkit.join("links.lock"))
        .expect("open links.lock");
    let mut gate = fd_lock::RwLock::new(lock_file);
    let _held = gate.try_write().expect("hold the gate for the test");

    let out = retry_on_busy(|| {
        Command::new(&exe)
            .arg("install-links")
            .env("HOME", state.path())
            .env("XDG_STATE_HOME", state.path())
            .output()
    });
    assert!(
        !out.status.success(),
        "install-links must hard-error when the gate is contended"
    );
    assert!(
        !shim_path(dir.path(), "portm").exists(),
        "no shim should be linked while the gate is contended"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("already linking"),
        "the error should explain the contention: {stderr}"
    );
}

#[test]
fn doctor_reports_a_foreign_shim_name() {
    let (dir, exe) = staged();
    let state = tempfile::tempdir().expect("state dir");
    let foreign = shim_path(dir.path(), "issue");
    std::fs::write(&foreign, b"#!/bin/sh\necho not devkit\n").expect("write foreign issue");
    let out = Command::new(&exe)
        .arg("doctor")
        // Without this, doctor's config row reads whatever devkit.toml sits
        // at the test process's own cwd — this repo's own, which is missing
        // `[defaults]` and fails independently of shim state. `dir` has none.
        .current_dir(dir.path())
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
