//! Create the old command names as hardlinks beside the running executable.
//!
//! A hardlink is a second name for the same inode: no disk cost, no exec-time
//! indirection, and no symlink privilege requirement on Windows. It also keeps
//! `argv[0]` reporting the name the caller typed, which is what dispatch reads.

use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::shim::{PROBE_FLAG, PROBE_MARKER, SHIMS, Shim};

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Created,
    Replaced,
    AlreadyLinked,
    SkippedForeign(String),
    Failed(String),
}

#[derive(clap::Args)]
pub struct InstallLinksArgs {
    /// Claim a shim name even when the file there is not a devkit binary.
    #[arg(long)]
    pub force: bool,
}

/// How long a single probe gets before its process is killed and treated as
/// foreign. Generous enough that a cold-start binary on a loaded CI runner
/// still answers in time.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether two paths name the same file, so an existing correct hardlink is
/// left alone instead of being deleted and recreated. On Windows that matters
/// beyond tidiness: deleting a running executable fails.
///
/// Delegated to the `same-file` crate rather than hand-rolled per-platform
/// metadata comparison: the Windows identity check needs
/// `GetFileInformationByHandle`, which is unsafe FFI on a path whose failure
/// mode is deleting the wrong file, and `same-file` already wraps it safely
/// (as it does the Unix `dev`+`ino` pair). An error resolving either path
/// (e.g. one no longer exists) is "not the same file", not a crash.
pub fn same_file(a: &Path, b: &Path) -> bool {
    same_file::is_same_file(a, b).unwrap_or(false)
}

/// Run `<path> <arg>`, polling for exit against a deadline instead of
/// blocking forever. `std::process::Output`'s `output()` has no timeout, so a
/// foreign program that hangs (waiting on a socket, say) would otherwise wedge
/// the caller indefinitely.
///
/// Stdin is a null device, never inherited from the caller: a probe must
/// never consume real input meant for something else. That matters beyond
/// tidiness once `ensure_current` runs this on every shim invocation —
/// `lockm hook pretooluse` reads its payload from stdin, and a foreign
/// binary sitting at that shim name must not be able to eat it.
///
/// Both output streams are read on background threads, started before the
/// wait loop so a chatty child can't deadlock against a full pipe buffer
/// while nothing drains it. Each thread hands its buffer back over an `mpsc`
/// channel rather than being `join`ed: `join` has no timeout of its own, so a
/// grandchild that inherited a pipe write end could keep a reader thread
/// blocked past the deadline even after the child we spawned has exited.
/// `recv_timeout` against the deadline's remaining budget bounds that too;
/// a thread that is still blocked when its budget runs out is left detached
/// (never joined) and the probe reports foreign.
fn probe(path: &Path, arg: &str, timeout: Duration) -> Option<Output> {
    let mut child = Command::new(path)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let (stdout_tx, stdout_rx) = mpsc::channel();
    let (stderr_tx, stderr_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = stdout_tx.send(buf);
    });
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        let _ = stderr_tx.send(buf);
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };

    let Ok(stdout) = stdout_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    else {
        return None;
    };
    let Ok(stderr) = stderr_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    else {
        return None;
    };
    Some(Output {
        status,
        stdout,
        stderr,
    })
}

/// Whether `token` looks like a dotted numeric version (`0.13.3`,
/// `1.0.0-beta.1`): the part before any `-`/`+` pre-release/build suffix is
/// non-empty, contains a `.`, and is only digits and dots.
fn looks_like_version(token: &str) -> bool {
    let core = token.split(['-', '+']).next().unwrap_or(token);
    !core.is_empty() && core.contains('.') && core.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Whether `text`'s first line is `<name> <version>` — `name` as the whole
/// first token, not merely a prefix of a longer one, followed by something
/// that looks like a version.
fn version_line_matches(text: &str, name: &str) -> bool {
    let mut tokens = text.lines().next().unwrap_or("").split_whitespace();
    tokens.next() == Some(name) && tokens.next().is_some_and(looks_like_version)
}

/// Whether `needle` appears in `haystack` bounded by non-word characters on
/// both sides (or the start/end of the text). A bare substring search would
/// accept a subcommand named `rm` inside "confirm", or `info` inside
/// "information" — exactly the kind of prose a foreign `--help` output is
/// likely to contain.
fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let is_word_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
    let bytes = haystack.as_bytes();
    haystack.match_indices(needle).any(|(start, matched)| {
        let end = start + matched.len();
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_word_byte(bytes[end]);
        before_ok && after_ok
    })
}

/// Whether `path` answers the identity probe (`PROBE_FLAG`) with exactly
/// `PROBE_MARKER` on stdout. This is the accept path a current build (and any
/// future one) uses: `main` intercepts the flag before clap parsing or
/// touching stdin, so it costs one cheap subprocess call and never risks
/// waking `devkit-mcp`'s stdio server loop.
fn answers_probe_marker(path: &Path, timeout: Duration) -> bool {
    let Some(out) = probe(path, PROBE_FLAG, timeout) else {
        return false;
    };
    out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == PROBE_MARKER
}

/// The result of judging whether a file is genuinely the devkit binary a
/// shim selects. `Foreign` carries a human-readable account of what the file
/// actually reported, for the skip message.
#[derive(Debug, PartialEq, Eq)]
pub enum Judgement {
    Accepted,
    Foreign(String),
}

/// Whether the file at `path` is genuinely the devkit binary `shim` selects,
/// judged by running it — never by inspecting its bytes.
///
/// `--version` must report `shim`'s own name (anchored to the first token,
/// not a prefix match against the whole shim set) followed by a version; a
/// failure here is foreign outright. Past that, identity is settled by
/// *either* of two probes:
///
/// 1. The marker probe (`PROBE_FLAG`/`PROBE_MARKER`, see `answers_probe_marker`).
/// 2. `--help` names every *visible* subcommand this build's `shim_command`
///    exposes for `shim` (hidden ones, like `lockm hook`, are never printed
///    by a genuine binary either, so they're excluded from what's required).
///    Read from clap at runtime rather than a hardcoded list, so the check
///    tracks the CLI as it grows. An *empty* expected set (`devkit-mcp` has
///    no subcommands) can never satisfy this probe on its own — otherwise
///    the version line alone would decide, which is the check this replaced.
///
/// The marker probe is what keeps a subcommand-less shim linkable at all
/// once one run has installed a marker-aware binary; the subcommand probe is
/// what a pre-marker devkit build relies on instead. A pre-marker binary at a
/// subcommand-less shim name satisfies neither and costs one `--force`.
///
/// A file that will not execute, that times out, or that satisfies neither
/// probe is foreign and left alone. That is the safe direction to err in: a
/// stale or ambiguous binary is skipped rather than silently accepted.
pub fn is_devkit_binary(path: &Path, shim: &Shim) -> Judgement {
    let Some(version_out) = probe(path, "--version", PROBE_TIMEOUT) else {
        return Judgement::Foreign("did not answer --version".to_string());
    };
    if !version_out.status.success() {
        return Judgement::Foreign("--version exited non-zero".to_string());
    }
    let version_text = format!(
        "{}{}",
        String::from_utf8_lossy(&version_out.stdout),
        String::from_utf8_lossy(&version_out.stderr)
    );
    let first_line = version_text.lines().next().unwrap_or("").trim();
    if !version_line_matches(&version_text, shim.name) {
        return Judgement::Foreign(format!("reports `{first_line}`"));
    }

    if answers_probe_marker(path, PROBE_TIMEOUT) {
        return Judgement::Accepted;
    }

    let Some(help_out) = probe(path, "--help", PROBE_TIMEOUT) else {
        return Judgement::Foreign(format!(
            "reports `{first_line}` but did not answer the probe marker or --help"
        ));
    };
    if !help_out.status.success() {
        return Judgement::Foreign(format!(
            "reports `{first_line}` but did not answer the probe marker, and --help exited non-zero"
        ));
    }
    let help_text = format!(
        "{}{}",
        String::from_utf8_lossy(&help_out.stdout),
        String::from_utf8_lossy(&help_out.stderr)
    );
    let expected = crate::shim_command(shim.sub.name(), shim.name);
    let expected_names: Vec<&str> = expected
        .get_subcommands()
        .filter(|c| !c.is_hide_set())
        .map(|c| c.get_name())
        .collect();
    if expected_names.is_empty() {
        return Judgement::Foreign(format!(
            "reports `{first_line}` but this shim has no subcommands to verify against, \
             and did not answer the probe marker either — a pre-restructure devkit binary \
             here costs one --force (this build then carries the marker for future runs)"
        ));
    }
    let missing: Vec<&str> = expected_names
        .into_iter()
        .filter(|n| !contains_word(&help_text, n))
        .collect();
    if !missing.is_empty() {
        return Judgement::Foreign(format!(
            "reports `{first_line}` but did not answer the probe marker, and --help is \
             missing subcommand(s) {}",
            missing.join(", ")
        ));
    }
    Judgement::Accepted
}

fn shim_file_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Whether something already sits at `dest` — including a broken symlink.
/// `Path::exists` follows symlinks and reports a broken one as absent, which
/// would send it down the "create" path below, where `hard_link` then fails
/// with `EEXIST` because a (dangling) directory entry is already there.
fn dest_occupied(dest: &Path) -> bool {
    std::fs::symlink_metadata(dest).is_ok()
}

/// Link every shim name in `dir` at `exe`. Returns one outcome per shim, in
/// `SHIMS` order, so the caller renders and exits on the whole set.
pub fn link_all(exe: &Path, dir: &Path, force: bool) -> Vec<(&'static str, Outcome)> {
    SHIMS
        .iter()
        .map(|s| {
            (
                s.name,
                link_one(exe, s, &dir.join(shim_file_name(s.name)), force),
            )
        })
        .collect()
}

fn link_one(exe: &Path, shim: &Shim, dest: &Path, force: bool) -> Outcome {
    if dest_occupied(dest) {
        if same_file(exe, dest) {
            return Outcome::AlreadyLinked;
        }
        if !force && let Judgement::Foreign(reason) = is_devkit_binary(dest, shim) {
            return Outcome::SkippedForeign(reason);
        }
        if let Err(e) = std::fs::remove_file(dest) {
            return Outcome::Failed(format!("removing {}: {e}", dest.display()));
        }
        return match std::fs::hard_link(exe, dest) {
            Ok(()) => Outcome::Replaced,
            Err(e) => Outcome::Failed(format!("linking {}: {e}", dest.display())),
        };
    }
    match std::fs::hard_link(exe, dest) {
        Ok(()) => Outcome::Created,
        Err(e) => Outcome::Failed(format!("linking {}: {e}", dest.display())),
    }
}

/// Env var whose mere presence (any value) skips the automatic linking pass
/// entirely, before even reading the stamp. Two reasons to reach for it: a
/// test exercising anything other than this feature, which must not touch
/// the real build's `exe.parent()` directory with real hardlinks (`link_all`
/// always targets it, regardless of an isolated state dir) or write a real
/// stamp; and a user or distro packager who wants no automatic linking at
/// all, who otherwise has no way to opt out.
pub const SKIP_AUTOLINK_ENV: &str = "DEVKIT_SKIP_AUTOLINK";

/// How long the automatic linking pass may run before abandoning whatever
/// shims it has not yet reached. Six shims x three probes x `PROBE_TIMEOUT`
/// is a 90s worst case, and this pass runs on the first invocation of *every*
/// shim after a version bump — including `lockm hook pretooluse` under an
/// editor's own hook timeout. Checked only before starting the next shim,
/// not mid-probe, so the real bound is this plus one shim's own worst case
/// (roughly 35s, not 20s) — still far short of 90s, without adding a second,
/// finer-grained cancellation path into `probe`.
const AUTOLINK_DEADLINE: Duration = Duration::from_secs(20);

/// The stamp line for `exe`: its version, the directory it links into, and
/// its own mtime (nanoseconds since the epoch — a portable, stable integer;
/// there is no cross-platform way to read a file-identity handle like
/// `file_index`/inode from outside the process holding it, which is why
/// this uses mtime instead). Comparing the whole line, not the version
/// alone, is what makes two cases relink instead of being silently skipped
/// forever because the version string didn't move: a same-version reinstall
/// (a fresh build replacing the file the old hardlinks still point at — an
/// ordinary pre-release loop) changes the mtime, and a second install at
/// another prefix changes the directory. `None` if the executable's own
/// metadata or mtime can't be read — `ensure_current` treats that the same
/// as any other failure to compute the current stamp: it links nothing and
/// returns, rather than guessing either way.
fn stamp_line(exe: &Path, dir: &Path) -> Option<String> {
    let modified = std::fs::metadata(exe).ok()?.modified().ok()?;
    let nanos = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!(
        "{}\t{}\t{nanos}",
        env!("CARGO_PKG_VERSION"),
        dir.display()
    ))
}

/// Open (creating if absent) `links.lock` under `state`, ready for
/// `try_write`. Shared by `ensure_current` and `run` so both take the same
/// gate before touching any shim name.
fn open_gate(state: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(state.join("links.lock"))
}

/// Best-effort record of who holds the gate, written into `links.lock`'s own
/// content right after acquiring it. Advisory only: a plain read (unlike
/// another `try_write`) isn't blocked by the flock, so `run`'s contention
/// error can read this back to name a likely culprit — this is never
/// consulted to make a linking decision. A crashed holder can leave a stale
/// pid behind and pids get reused, so it is a hint, not a fact.
fn record_gate_owner(held: &mut std::fs::File) {
    use std::io::Write as _;
    let _ = held.set_len(0);
    let _ = held.write_all(std::process::id().to_string().as_bytes());
}

/// Read back the hint `record_gate_owner` wrote. `None` on anything
/// unreadable or empty, which the caller degrades to a generic message.
fn gate_owner_hint(state: &Path) -> Option<String> {
    let text = std::fs::read_to_string(state.join("links.lock")).ok()?;
    let pid = text.trim();
    (!pid.is_empty()).then(|| pid.to_string())
}

/// Link the shim names when the stamp does not match this build (see
/// `stamp_line`).
///
/// Runs before dispatch on every invocation, so the match case must stay
/// cheap: `lockm hook pretooluse` takes this path on every editor write.
/// `DEVKIT_SKIP_AUTOLINK` (any value) skips the whole pass ahead of even
/// that — see its own doc comment.
///
/// Every failure is a warning; linking never blocks the command the user
/// asked for, and a whole-pass deadline (`AUTOLINK_DEADLINE`) bounds how long
/// one hung foreign binary at a shim name can hold up the rest.
pub fn ensure_current(exe: &Path) {
    if std::env::var_os(SKIP_AUTOLINK_ENV).is_some() {
        return;
    }
    let Some(dir) = exe.parent() else { return };
    let Some(current) = stamp_line(exe, dir) else {
        return;
    };
    let state = devkit_common::paths::state_dir();
    let stamp = state.join("links-version");
    if std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == current) {
        return;
    }
    if std::fs::create_dir_all(&state).is_err() {
        return;
    }
    let Ok(gate) = open_gate(&state) else {
        return;
    };
    let mut gate = fd_lock::RwLock::new(gate);
    // This is also the reentrancy guard against `is_devkit_binary`'s own
    // `--version` and `--help` probes: both fall straight through the
    // probe-flag intercept in `main` (only the marker flag is intercepted)
    // and re-enter `ensure_current` in the spawned child. A `try_write` —
    // never a blocking `write` — is what keeps that bounded: the child finds
    // the gate already held by its own ancestor and returns immediately
    // instead of relinking (and probing) all over again. `run` (`devkit
    // install-links`) takes the same gate around its own whole pass, so an
    // explicit run and an automatic one never link concurrently either.
    let Ok(mut held) = gate.try_write() else {
        // Another process — an ancestor probe, a genuinely concurrent
        // automatic pass, or a manual `install-links` — is linking right
        // now; it will write the stamp.
        return;
    };
    record_gate_owner(&mut held);
    let deadline = Instant::now() + AUTOLINK_DEADLINE;
    let mut truncated = false;
    for s in SHIMS {
        if Instant::now() >= deadline {
            truncated = true;
            break;
        }
        match link_one(exe, s, &dir.join(shim_file_name(s.name)), false) {
            Outcome::Failed(e) => eprintln!("devkit: could not link {}: {e}", s.name),
            Outcome::SkippedForeign(why) => eprintln!(
                "devkit: {} on PATH is not a devkit binary ({why}); left alone",
                s.name
            ),
            Outcome::Created | Outcome::Replaced | Outcome::AlreadyLinked => {}
        }
    }
    if truncated {
        eprintln!(
            "devkit: automatic linking ran out of time; run `devkit install-links` to finish"
        );
    }
    let _ = std::fs::write(&stamp, format!("{current}\n"));
}

/// `devkit install-links` — the manual pass. Takes the same `links.lock`
/// gate as `ensure_current`, held for its entire run: without it, a probe
/// child spawned at a candidate that really is a devkit binary would find
/// the gate free during this call and run its own automatic pass against
/// the same directory concurrently with this one. Unlike `ensure_current`,
/// contention here is a hard error — the user asked for this pass
/// specifically and a silent no-op would look like success.
pub fn run(args: InstallLinksArgs) -> Result<()> {
    let exe = std::env::current_exe().context("resolving the running executable")?;
    let dir = exe
        .parent()
        .context("the running executable has no parent directory")?;
    let state = devkit_common::paths::state_dir();
    std::fs::create_dir_all(&state).with_context(|| format!("creating {}", state.display()))?;
    let lock_path = state.join("links.lock");
    let gate_file =
        open_gate(&state).with_context(|| format!("opening {}", lock_path.display()))?;
    let mut gate = fd_lock::RwLock::new(gate_file);
    let Ok(mut held) = gate.try_write() else {
        let holder = gate_owner_hint(&state)
            .map(|pid| format!(" (pid {pid}, if it is still alive)"))
            .unwrap_or_default();
        anyhow::bail!(
            "another devkit invocation is already linking{holder} — holding {}; \
             try again once it finishes",
            lock_path.display()
        );
    };
    record_gate_owner(&mut held);

    let results = link_all(&exe, dir, args.force);
    let mut failed = 0;
    for (name, outcome) in &results {
        match outcome {
            Outcome::Created => println!("created   {name}"),
            Outcome::Replaced => println!("replaced  {name}"),
            Outcome::AlreadyLinked => println!("current   {name}"),
            Outcome::SkippedForeign(reason) => {
                println!("skipped   {name} ({reason}; --force to claim it anyway)");
            }
            Outcome::Failed(e) => {
                failed += 1;
                eprintln!("failed    {name}: {e}");
            }
        }
    }
    if let Some(current) = stamp_line(&exe, dir) {
        let _ = std::fs::write(state.join("links-version"), format!("{current}\n"));
    }
    anyhow::ensure!(failed == 0, "{failed} link(s) could not be created");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_version_accepts_dotted_numbers() {
        assert!(looks_like_version("0.13.3"));
        assert!(looks_like_version("1.0.0-beta.1"));
        assert!(!looks_like_version("beta"));
        assert!(!looks_like_version("1"));
        assert!(!looks_like_version(""));
    }

    #[test]
    fn version_line_matches_requires_the_exact_first_token() {
        assert!(version_line_matches("issue 1.2.3\n", "issue"));
        assert!(!version_line_matches("issuetracker 1.2.3\n", "issue"));
        assert!(!version_line_matches("devkitty 2.0\n", "devkit"));
        assert!(!version_line_matches("issue\n", "issue"));
        assert!(!version_line_matches("", "issue"));
    }

    /// The exact false-positive the re-reviewer reproduced against a bare
    /// substring search: `rm` inside "confirm", `info` inside "information".
    #[test]
    fn contains_word_rejects_a_substring_inside_a_longer_word() {
        assert!(!contains_word("please confirm your choice", "rm"));
        assert!(!contains_word("see the information page", "info"));
        assert!(contains_word("run `rm` to delete it", "rm"));
        assert!(contains_word("info: nothing to do", "info"));
        assert!(!contains_word("anything", ""));
    }

    /// A path that cannot even be spawned is judged foreign, not accepted —
    /// the fallback taken before either probe ever runs.
    #[test]
    fn is_devkit_binary_rejects_a_path_that_cannot_execute() {
        let shim = SHIMS
            .iter()
            .find(|s| s.name == "issue")
            .expect("issue shim");
        assert!(matches!(
            is_devkit_binary(Path::new("/no/such/binary-at-all"), shim),
            Judgement::Foreign(_)
        ));
    }

    /// A process that outlives the deadline is killed and treated as
    /// unanswered, not waited out. `probe` must return well before `sleep`'s
    /// own 30s would elapse — this is the behavior that keeps a foreign
    /// program hanging on an unrecognized flag from wedging the caller.
    #[test]
    #[cfg(unix)]
    fn probe_kills_a_hung_process_within_its_deadline() {
        let start = std::time::Instant::now();
        let result = probe(Path::new("sleep"), "30", Duration::from_millis(300));
        assert!(
            result.is_none(),
            "a process that outlives the deadline must be treated as unanswered"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "probe must not wait for the full hang duration: took {:?}",
            start.elapsed()
        );
    }
}
