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

pub fn run(args: InstallLinksArgs) -> Result<()> {
    let exe = std::env::current_exe().context("resolving the running executable")?;
    let dir = exe
        .parent()
        .context("the running executable has no parent directory")?;
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
