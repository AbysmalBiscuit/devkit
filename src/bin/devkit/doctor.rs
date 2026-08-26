use anyhow::Result;
use devkit_common::progress::Steps;
use devkit_common::secrets::{self, Source};
use devkit_common::slack;
use devkit_common::tracker::{Resolved, TrackerKind, linear};

#[derive(Debug, PartialEq, Eq)]
enum Check {
    Ok(String),
    Warn(String),
    Invalid(String),
    Unreachable,
    Unset(&'static str),
}

struct Row {
    key: &'static str,
    source: Source,
    check: Check,
}

const HINT_LINEAR: &str = "run: devkit auth linear   (https://linear.app/settings/api)";
const HINT_SLACK: &str = "run: devkit auth slack    (Slack app → OAuth & Permissions)";
const HINT_WORKSPACE: &str = "optional — falls back to the Linear API for issue links";
const HINT_GITHUB: &str = "run: gh auth login   (or set GH_TOKEN/GITHUB_TOKEN)";

/// Exit non-zero when a credential that is set fails validation, or when a
/// config exists that does not load. An unset credential is a warning; an
/// unreachable host is not a hard failure.
fn worst_exit(rows: &[Row]) -> i32 {
    if rows.iter().any(|r| matches!(r.check, Check::Invalid(_))) {
        1
    } else {
        0
    }
}

fn is_unreachable(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<ureq::Error>(),
        Some(ureq::Error::Transport(_))
    )
}

fn validate_linear(v: &str) -> Check {
    match linear::validate(v) {
        Ok(id) => Check::Ok(format!(
            "workspace \"{}\" ({})",
            id.workspace_url_key, id.viewer_email
        )),
        Err(e) if is_unreachable(&e) => Check::Unreachable,
        Err(e) => Check::Invalid(e.to_string()),
    }
}

fn validate_slack(v: &str) -> Check {
    match slack::validate(v) {
        Ok(id) => Check::Ok(format!("team \"{}\" (user {})", id.team, id.user)),
        Err(e) if is_unreachable(&e) => Check::Unreachable,
        Err(e) => Check::Invalid(e.to_string()),
    }
}

/// Severity of the "servers outside devrun" check by stray count.
fn stray_check(count: usize) -> Check {
    match count {
        0 => Check::Ok("no servers running outside devrun".into()),
        n => Check::Warn(format!(
            "{n} server(s) running outside devrun — run `devrun reap`"
        )),
    }
}

/// Count strays for the current directory's config. Resilient: any error
/// (no registry, no config) yields 0 so `doctor` never fails on this row.
fn count_strays() -> usize {
    let Ok(data) = devkit_ports::registry::snapshot() else {
        return 0;
    };
    let Ok(loaded) = devkit_ports::load::load(None, std::path::Path::new(".")) else {
        return 0;
    };
    devkit_ports::strays::scan(&loaded.config, &data).len()
}

/// Which tracker `issue` talks to here, and how devkit arrived at it.
/// Detection is ambient — a globally exported `LINEAR_API_KEY` resolves Linear
/// for every project on the machine, including one that has nothing in Linear —
/// so without this row nothing on any CLI path reveals the choice or its cause.
fn resolve_tracker(start: &std::path::Path) -> Resolved {
    let cfg = devkit_ports::load::load(None, start).ok().map(|l| l.config);
    let (kind, github) = match cfg {
        Some(c) => (c.tracker.kind, c.github),
        None => (None, devkit_config::GithubConfig::default()),
    };
    let repos = devkit_common::github::Repos::resolve(&github, &start.to_string_lossy(), None);
    devkit_common::tracker::resolve(kind, start, &repos)
}

/// A tracker devkit fell back to answers nothing while looking like an answer:
/// every issue-state gate stays closed and `issue end` cleans up nothing, with
/// no error to explain it. Naming a `kind` turns that into a decision — except
/// for a project that already named one devkit could not build, whose fix is
/// the reason the row already carries.
///
/// GitHub is checked ahead of `declared` because a built `GithubTracker` is
/// ready only when a token is, and an unset token is the single way this arm
/// is reached: a `kind = "github"` whose issues repository does not resolve
/// never builds the adapter and arrives here as `TrackerKind::None`. So the
/// fix is a token, not naming a tracker the project already named.
fn tracker_check(r: &Resolved) -> Check {
    let detail = format!("{} — {}", r.tracker.kind().as_str(), r.reason);
    match (r.tracker.kind(), r.tracker.ready()) {
        (TrackerKind::Github, false) => {
            Check::Warn(format!("{detail}; no GitHub token — {HINT_GITHUB}"))
        }
        (TrackerKind::Linear, _) | (TrackerKind::Github, true) | (TrackerKind::None, _) => {
            if r.declared || r.tracker.ready() {
                Check::Ok(detail)
            } else if r.unbuilt_reason().is_some() {
                Check::Warn(format!("{detail}; issue state gates stay closed"))
            } else {
                Check::Warn(format!(
                    "{detail}; issue state gates stay closed — set `[tracker] kind` \
                     to name this project's tracker"
                ))
            }
        }
    }
}

/// Whether the config reachable from the cwd loads. A config that does not
/// load makes every other devkit command fail, so it is a hard failure here
/// rather than a warning; having none at all is how any non-devkit directory
/// looks and is reported without complaint.
fn config_check(start: &std::path::Path) -> Check {
    let main_checkout = devkit_common::git::main_checkout(start).ok().flatten();
    match devkit_config::health(start, main_checkout.as_deref()) {
        devkit_config::Health::Ok => Check::Ok("devkit.toml loads".into()),
        devkit_config::Health::Absent => Check::Ok("no devkit.toml — not a devkit project".into()),
        // A toml error breaks its own lines; the rows here are one line each.
        devkit_config::Health::Broken(why) => {
            Check::Invalid(why.split_whitespace().collect::<Vec<_>>().join(" "))
        }
    }
}

fn docs_cache_check() -> Check {
    let root = devkit_docs::cache::docs_root();
    if !root.is_dir() {
        return Check::Ok("empty".into());
    }
    let s = devkit_docs::doctor_summary(&root);
    let msg = format!(
        "{} libs, {} MiB, {} unreferenced checkouts",
        s.libs,
        s.bytes / (1024 * 1024),
        s.unreferenced
    );
    if !s.problems.is_empty() {
        return Check::Warn(format!(
            "{msg}; {} checkout(s) do not match what docm resolved:\n  {}",
            s.problems.len(),
            s.problems.join("\n  ")
        ));
    }
    if s.unreferenced > 0 {
        Check::Warn(format!("{msg} — run `docm prune`"))
    } else {
        Check::Ok(msg)
    }
}

fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.split('.').map(|p| p.parse::<u64>().ok());
    match (it.next(), it.next(), it.next(), it.next()) {
        (Some(Some(a)), Some(Some(b)), Some(Some(c)), None) => Some((a, b, c)),
        _ => None,
    }
}

/// Compare the running binary's version against the newest plugin checkout in
/// the coding-agent plugin cache. When the binaries are older, agents read
/// docs describing features the installed binaries lack. Fail-soft: anything
/// unparseable (or no plugin cache) is Ok, never a warning.
fn version_skew_check(binary: &str, plugin: Option<&str>) -> Check {
    let Some(p) = plugin else {
        return Check::Ok(format!("{binary} (no plugin cache)"));
    };
    match (parse_semver(binary), parse_semver(p)) {
        (Some(b), Some(pv)) if b < pv => Check::Warn(format!(
            "binaries {binary} older than plugin {p} — \
             run `cargo install --path .` in the plugin checkout"
        )),
        _ => Check::Ok(format!("{binary} (plugin cache {p})")),
    }
}

/// Newest semver-named subdirectory of a plugin cache dir (`0.9.1`, `0.10.0`, …).
fn newest_plugin_version(dir: &std::path::Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            parse_semver(&name).map(|v| (v, name))
        })
        .max()
        .map(|(_, name)| name)
}

/// The devkit plugin's checkout cache in the coding-agent home
/// (`~/.claude/plugins/cache/devkit/devkit/<version>/`).
fn plugin_cache_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|s| !s.is_empty()))?;
    Some(std::path::PathBuf::from(home).join(".claude/plugins/cache/devkit/devkit"))
}

/// One row per shim name: linked, missing, or held by something else. The
/// automatic linker warns once and then stays quiet, so this is where a name it
/// could not claim stays visible.
fn shim_rows() -> Vec<Row> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    crate::shim::SHIMS
        .iter()
        .map(|s| {
            let path = dir.join(if cfg!(windows) {
                format!("{}.exe", s.name)
            } else {
                s.name.to_string()
            });
            let check = if !path.exists() {
                Check::Unset("run: devkit install-links")
            } else if crate::links::same_file(&exe, &path) {
                Check::Ok(format!("linked to {}", exe.display()))
            } else {
                Check::Warn(format!(
                    "{} is not this devkit; run: devkit install-links --force",
                    path.display()
                ))
            };
            Row {
                key: s.name,
                // Doctor's non-credential rows already use `Unset`; a shim has
                // no env or secrets.toml origin to report.
                source: Source::Unset,
                check,
            }
        })
        .collect()
}

fn gather(steps: &Steps) -> Vec<Row> {
    let mut rows = vec![
        Row {
            key: "linear_api_key",
            source: secrets::source("LINEAR_API_KEY"),
            check: match secrets::resolve("LINEAR_API_KEY") {
                Some(v) => steps.during("Validating Linear API key…", || validate_linear(&v)),
                None => Check::Unset(HINT_LINEAR),
            },
        },
        Row {
            key: "linear_workspace",
            source: secrets::source("LINEAR_WORKSPACE"),
            check: match secrets::resolve("LINEAR_WORKSPACE") {
                Some(v) => Check::Ok(v),
                None => Check::Unset(HINT_WORKSPACE),
            },
        },
        Row {
            key: "slack_token",
            source: secrets::source("SLACK_TOKEN"),
            check: match secrets::resolve("SLACK_TOKEN") {
                Some(v) => steps.during("Validating Slack token…", || validate_slack(&v)),
                None => Check::Unset(HINT_SLACK),
            },
        },
        Row {
            key: "binary_version",
            source: Source::Unset,
            check: version_skew_check(
                env!("CARGO_PKG_VERSION"),
                plugin_cache_dir()
                    .and_then(|d| newest_plugin_version(&d))
                    .as_deref(),
            ),
        },
        Row {
            key: "config",
            source: Source::Unset,
            check: config_check(std::path::Path::new(".")),
        },
        Row {
            key: "tracker",
            source: Source::Unset,
            check: tracker_check(&resolve_tracker(std::path::Path::new("."))),
        },
        Row {
            key: "devrun_strays",
            source: Source::Unset,
            check: stray_check(count_strays()),
        },
        Row {
            key: "docs_cache",
            source: Source::Unset,
            check: docs_cache_check(),
        },
    ];
    rows.extend(shim_rows());
    rows
}

fn source_label(s: &Source) -> &'static str {
    match s {
        Source::Env => "env",
        Source::File => "file",
        Source::Unset => "unset",
    }
}

fn print_human(rows: &[Row]) {
    for r in rows {
        let (mark, detail) = match &r.check {
            Check::Ok(d) => ("✓", d.clone()),
            Check::Warn(d) => ("⚠", d.clone()),
            Check::Invalid(d) => ("✗", d.clone()),
            Check::Unreachable => ("?", "unreachable".to_string()),
            Check::Unset(hint) => ("·", format!("unset — {hint}")),
        };
        println!("{mark} {:16} {:5} {detail}", r.key, source_label(&r.source));
    }
}

fn print_json(rows: &[Row]) {
    let arr: Vec<_> = rows
        .iter()
        .map(|r| {
            let (status, detail): (&str, Option<String>) = match &r.check {
                Check::Ok(d) => ("ok", Some(d.clone())),
                Check::Warn(d) => ("warn", Some(d.clone())),
                Check::Invalid(d) => ("invalid", Some(d.clone())),
                Check::Unreachable => ("unreachable", None),
                Check::Unset(h) => ("unset", Some((*h).to_string())),
            };
            serde_json::json!({
                "key": r.key,
                "source": source_label(&r.source),
                "status": status,
                "detail": detail,
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&arr).unwrap());
}

pub fn run(json: bool) -> Result<()> {
    let total = usize::from(secrets::resolve("LINEAR_API_KEY").is_some())
        + usize::from(secrets::resolve("SLACK_TOKEN").is_some());
    let steps = Steps::with_total(total);
    let rows = gather(&steps);
    steps.clear();
    if json {
        print_json(&rows);
    } else {
        print_human(&rows);
    }
    if worst_exit(&rows) != 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_common::tracker::none;

    fn row(check: Check) -> Row {
        Row {
            key: "x",
            source: Source::Unset,
            check,
        }
    }

    #[test]
    fn invalid_fails_exit() {
        let rows = vec![
            row(Check::Ok("ok".into())),
            row(Check::Invalid("bad".into())),
        ];
        assert_eq!(worst_exit(&rows), 1);
    }

    /// A config that exists and does not load is the one case that has to
    /// fail: silence here is what sent users to `devrun config show` to find
    /// out why every command was erroring. (`Health::Absent` is covered where
    /// the home layer can be held out of the resolution — see
    /// `config::health_tells_an_absent_config_from_a_broken_one`.)
    #[test]
    fn a_config_that_does_not_load_fails_doctor() {
        let tmp = tempfile::tempdir().unwrap();
        let broken = tmp.path().join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("devkit.toml"), "this is not toml [[[").unwrap();

        let check = config_check(&broken);
        assert!(matches!(check, Check::Invalid(_)), "{check:?}");
        assert_eq!(worst_exit(&[row(check)]), 1);
    }

    /// The row exists so ambient detection is debuggable: on a machine with a
    /// globally exported `LINEAR_API_KEY`, a project with nothing in Linear
    /// resolves to Linear and ready, and no other CLI path says so.
    #[test]
    fn the_tracker_row_names_the_kind_and_why_it_resolved() {
        let declared = Resolved {
            tracker: Box::new(none::NoneTracker),
            declared: true,
            reason: "[tracker] kind = \"none\"".into(),
        };
        match tracker_check(&declared) {
            Check::Ok(d) => assert_eq!(d, "none — [tracker] kind = \"none\""),
            other => panic!("a declared tracker is not a fault: {other:?}"),
        }

        let fell_back = Resolved {
            tracker: Box::new(none::NoneTracker),
            declared: false,
            reason: "detected: no LINEAR_API_KEY and no GitHub origin remote".into(),
        };
        match tracker_check(&fell_back) {
            Check::Warn(d) => {
                assert!(
                    d.starts_with("none — detected: no LINEAR_API_KEY"),
                    "the detail leads with the kind and the reason: {d}"
                );
                assert!(
                    d.contains("[tracker] kind"),
                    "the warning says what to set: {d}"
                );
            }
            other => panic!("a tracker devkit fell back to is worth a warning: {other:?}"),
        }
    }

    /// A github fake tracker with `ready` forced false, standing in for a real
    /// `GithubTracker` with no token — `ready()` on the real adapter reads the
    /// process-global `github::token()`, which a unit test cannot pin.
    fn unready_github(declared: bool, reason: &str) -> Resolved {
        let mut tracker =
            devkit_common::tracker::fake::FakeTracker::new().with_kind(TrackerKind::Github);
        tracker.ready = false;
        Resolved {
            tracker: Box::new(tracker),
            declared,
            reason: reason.into(),
        }
    }

    /// A project that names `[tracker] kind = "github"` still warns about a
    /// missing token: the adapter is the project's own declared choice, so the
    /// `declared || ready` arm would otherwise mark it Ok with no token in play.
    #[test]
    fn a_declared_github_tracker_with_no_token_warns_about_the_token() {
        let r = unready_github(true, "[tracker] kind = \"github\"");
        match tracker_check(&r) {
            Check::Warn(d) => {
                assert!(d.contains("no GitHub token"), "{d}");
                assert!(d.contains("gh auth login"), "{d}");
            }
            other => panic!("a declared github tracker with no token is a warning: {other:?}"),
        }
    }

    /// A detected GitHub origin with no token warns about the token, not about
    /// naming `[tracker] kind` — the tracker already resolved to the real
    /// adapter, so the generic "name your tracker" advice would be wrong.
    #[test]
    fn a_detected_github_tracker_with_no_token_warns_about_the_token_not_kind() {
        let r = unready_github(
            false,
            &format!(
                "{}github.com `origin` remote",
                devkit_common::tracker::DETECTED
            ),
        );
        match tracker_check(&r) {
            Check::Warn(d) => {
                assert!(d.contains("no GitHub token"), "{d}");
                assert!(!d.contains("[tracker] kind"), "{d}");
            }
            other => {
                panic!("a detected github tracker with no token warns about the token: {other:?}")
            }
        }
    }

    #[test]
    fn stray_check_severity_by_count() {
        assert!(matches!(stray_check(0), Check::Ok(_)));
        assert!(matches!(stray_check(3), Check::Warn(_)));
    }

    #[test]
    fn version_skew_warns_only_when_binary_is_older() {
        assert!(matches!(
            version_skew_check("0.10.0", Some("0.11.0")),
            Check::Warn(_)
        ));
        assert!(matches!(
            version_skew_check("0.11.0", Some("0.11.0")),
            Check::Ok(_)
        ));
        assert!(matches!(
            version_skew_check("0.12.0", Some("0.11.0")),
            Check::Ok(_)
        ));
        // fail-soft: no plugin cache or unparseable versions never warn
        assert!(matches!(version_skew_check("0.11.0", None), Check::Ok(_)));
        assert!(matches!(
            version_skew_check("0.11.0", Some("garbage")),
            Check::Ok(_)
        ));
    }

    #[test]
    fn skew_warning_names_both_versions() {
        let Check::Warn(msg) = version_skew_check("0.10.0", Some("0.11.0")) else {
            panic!("older binary must warn");
        };
        assert!(msg.contains("0.10.0") && msg.contains("0.11.0"), "{msg}");
    }

    #[test]
    fn newest_plugin_version_compares_numerically() {
        let dir = tempfile::tempdir().unwrap();
        for v in ["0.9.1", "0.10.0", "not-a-version"] {
            std::fs::create_dir(dir.path().join(v)).unwrap();
        }
        assert_eq!(
            newest_plugin_version(dir.path()).as_deref(),
            Some("0.10.0"),
            "0.10.0 > 0.9.1 numerically (lexical order would pick 0.9.1)"
        );
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(newest_plugin_version(empty.path()), None);
    }

    #[test]
    fn unset_and_unreachable_pass_exit() {
        let rows = vec![
            row(Check::Unset("h")),
            row(Check::Unreachable),
            row(Check::Ok("ok".into())),
        ];
        assert_eq!(worst_exit(&rows), 0);
    }
}
