//! Reading a git remote URL: its host, the repository path it names, and
//! whether it reaches a given forge host once `~/.ssh/config` has its say.

use std::{path::Path, time::Duration};

use anyhow::{Context, Result};

use crate::vcs::{Vcs, VersionControl};

/// The host a git remote URL names, before any `~/.ssh/config` alias is
/// resolved: scp-like `[user@]host:path`, `scheme://[user@]host/path`, or
/// `None` for a URL that carries no host at all, such as a bare local path.
///
/// A one-character host is a Windows drive letter (`C:/src/repo`), which is
/// scp-like by shape because the colon precedes every slash. Git makes the
/// same exception, and reads it as a path.
pub fn remote_host(url: &str) -> Option<&str> {
    let u = url.trim();
    if let Some((_, rest)) = u.split_once("://") {
        let after_user = rest.rsplit('@').next().unwrap_or(rest);
        let host = after_user.split(['/', ':']).next().unwrap_or("");
        return (!host.is_empty()).then_some(host);
    }
    let (before_colon, _) = u.split_once(':')?;
    if before_colon.contains('/') {
        return None;
    }
    let host = before_colon.rsplit('@').next().unwrap_or(before_colon);
    (host.chars().count() > 1).then_some(host)
}

/// Whether a remote is carried over ssh, and so takes its host from
/// `~/.ssh/config`. Scp-like syntax is ssh by definition; a URL with a scheme
/// is ssh only when it says so.
fn is_ssh_form(url: &str) -> bool {
    match url.trim().split_once("://") {
        Some((scheme, _)) => scheme.eq_ignore_ascii_case("ssh"),
        None => true,
    }
}

/// Parse `ssh -G <host>`'s effective configuration for the hostname it would
/// actually connect to. Keywords come back lowercased, one per line.
fn ssh_hostname_from_dump(dump: &str) -> Option<String> {
    dump.lines()
        .find_map(|l| l.strip_prefix("hostname "))
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
}

/// The hostname behind an ssh `Host` alias, per the user's own ssh config.
/// `None` when ssh is absent or the alias resolves to nothing usable.
///
/// Bounded because `ssh -G` is not a plain config read: a `Match exec` block
/// runs its command while the config is parsed, and this sits on the session
/// hook path where a wedged child would wedge the hook.
fn ssh_hostname(alias: &str) -> Option<String> {
    let dump = crate::cmd::capture_bounded("ssh", &["-G", alias], SSH_CONFIG_TIMEOUT)?;
    ssh_hostname_from_dump(&dump)
}

/// Long enough for an `ssh -G` that shells out through `Match exec`, short
/// enough that a wedged one fails instead of hanging the caller.
const SSH_CONFIG_TIMEOUT: Duration = Duration::from_secs(5);

/// Two spellings of one forge host. GitHub serves the same site at
/// `www.github.com`, and git keeps whichever spelling a clone URL used, so a
/// leading `www.` is not part of the comparison.
pub fn same_host(a: &str, b: &str) -> bool {
    fn bare(h: &str) -> &str {
        match h.get(..4) {
            Some(p) if p.eq_ignore_ascii_case("www.") => &h[4..],
            _ => h,
        }
    }
    bare(a).eq_ignore_ascii_case(bare(b))
}

/// The host a remote reaches, with `resolve` mapping an ssh alias to the
/// hostname ssh would connect to. An https host is literal: resolving one
/// through ssh config would let an unrelated `Host` block decide where an
/// https remote points. Split from `ssh_hostname` so the rules are testable
/// without an ssh config.
fn reached_host(url: &str, resolve: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let host = remote_host(url)?;
    if !is_ssh_form(url) {
        return Some(host.to_string());
    }
    Some(resolve(host).unwrap_or_else(|| host.to_string()))
}

/// Whether a remote reaches `host`. A literal match settles it without asking
/// ssh, so the common case never spawns `ssh -G`.
fn reaches(url: &str, host: &str, resolve: &dyn Fn(&str) -> Option<String>) -> bool {
    match remote_host(url) {
        None => false,
        Some(literal) if same_host(literal, host) => true,
        Some(_) => reached_host(url, resolve).is_some_and(|h| same_host(&h, host)),
    }
}

/// The host a remote reaches once `~/.ssh/config` has its say. A `Host` alias
/// substitutes the hostname wholesale, so `gh:owner/repo.git` reaches
/// github.com when ssh config maps `gh` to it.
pub fn remote_reached_host(url: &str) -> Option<String> {
    reached_host(url, &|alias| ssh_hostname(alias))
}

/// Whether a remote reaches `host`, resolving an ssh alias when the literal
/// spelling does not already match.
pub fn remote_reaches(url: &str, host: &str) -> bool {
    reaches(url, host, &|alias| ssh_hostname(alias))
}

/// Parse the repository path from a remote URL (scp-like, `ssh://`, or
/// https), stripping a trailing `.git`: `owner/repo`, or `group/sub/repo` on a
/// forge with nested namespaces. Host-blind: [`origin_slug`] is where the host
/// is checked.
pub fn slug_from_remote_url(url: &str) -> Option<String> {
    let u = url.trim();
    let rest = if let Some((_, r)) = u.split_once("://") {
        r.split_once('/').map(|(_, p)| p)?
    } else {
        remote_host(u)?;
        u.split_once(':').map(|(_, p)| p)?
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    validate_slug(rest).ok()?;
    Some(rest.to_string())
}

/// A repository slug is two or more `/`-separated segments, each non-empty and
/// made only of characters forges allow in a name. Configured slugs reach
/// cache filenames and `gh --repo` arguments, so a slug carrying `..` or an
/// empty segment is rejected where it is resolved rather than sanitized
/// downstream.
pub fn validate_slug(s: &str) -> Result<()> {
    fn ok_segment(seg: &str) -> bool {
        !seg.is_empty()
            && seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && seg != "."
            && seg != ".."
    }
    let segments: Vec<&str> = s.split('/').collect();
    anyhow::ensure!(
        segments.len() >= 2 && segments.iter().all(|seg| ok_segment(seg)),
        "`{s}` is not an owner/repo repository slug"
    );
    Ok(())
}

/// The `origin` remote URL of the checkout at `cwd`.
pub fn origin_url(cwd: &str) -> Result<String> {
    let cwd = Path::new(cwd);
    Vcs::at(cwd)
        .remote_url(cwd, "origin")
        .context("reading the `origin` remote")
}

/// The `origin` slug, only when origin reaches `host`. This is the single
/// entry point for defaulting a repository from the remote, so the host check
/// cannot be skipped by a caller that declared its forge and therefore never
/// ran detection.
pub fn origin_slug(cwd: &str, host: &str) -> Result<String> {
    let url = origin_url(cwd)?;
    anyhow::ensure!(
        remote_reaches(&url, host),
        "`origin` is not a {host} remote ({url}); {}",
        unreachable_hint(&url)
    );
    slug_from_remote_url(&url).with_context(|| format!("no owner/repo in the origin URL `{url}`"))
}

/// What to try when `origin` does not reach the forge host. An ssh alias that
/// resolves elsewhere is the case worth naming: the remote looks nothing like
/// a hostname, so "not a remote" reads as a bug rather than as an ssh config
/// that maps the alias somewhere else.
fn unreachable_hint(url: &str) -> String {
    match remote_host(url) {
        Some(host) if is_ssh_form(url) => format!(
            "ssh config resolves `{host}` to {}, so set [forge] repo (and [github] issues_repo) \
             explicitly",
            ssh_hostname(host).unwrap_or_else(|| "nothing".to_string())
        ),
        _ => "set [forge] repo (and [github] issues_repo) explicitly".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_github_remote(url: &str) -> bool {
        reaches(url, "github.com", &|_| None)
    }

    #[test]
    fn slug_parses_ssh_and_https() {
        for (url, want) in [
            ("git@github.com:acme/monorepo.git", "acme/monorepo"),
            ("git@github.com:acme/monorepo", "acme/monorepo"),
            ("https://github.com/acme/monorepo.git", "acme/monorepo"),
            ("https://github.com/acme/monorepo", "acme/monorepo"),
            ("https://github.com/acme/monorepo/", "acme/monorepo"),
            ("ssh://git@github.com/acme/monorepo.git", "acme/monorepo"),
            (
                "ssh://git@git.acme.test:2222/acme/monorepo.git",
                "acme/monorepo",
            ),
        ] {
            assert_eq!(slug_from_remote_url(url).as_deref(), Some(want), "{url}");
        }
    }

    /// GitLab nests projects in groups and subgroups, and the whole path is
    /// the project's identity.
    #[test]
    fn slug_keeps_a_nested_group_path() {
        assert_eq!(
            slug_from_remote_url("git@gitlab.com:platform/web/app.git").as_deref(),
            Some("platform/web/app")
        );
        assert_eq!(
            slug_from_remote_url("https://gitlab.example.com/a/b/c/d").as_deref(),
            Some("a/b/c/d")
        );
    }

    #[test]
    fn slug_rejects_garbage() {
        assert_eq!(slug_from_remote_url("not-a-url"), None);
        assert_eq!(slug_from_remote_url("https://github.com/onlyowner"), None);
        assert_eq!(slug_from_remote_url("https://github.com/o/../r"), None);
        assert_eq!(slug_from_remote_url(""), None);
    }

    /// A `~/.ssh/config` `Host` alias replaces the hostname outright, so the
    /// remote carries no user and no recognizable host: `gh:owner/repo.git`.
    #[test]
    fn slug_parses_an_ssh_alias_host() {
        for (url, want) in [
            ("gh:acme/monorepo.git", "acme/monorepo"),
            ("gh:acme/monorepo", "acme/monorepo"),
            ("ssh://gh/acme/monorepo.git", "acme/monorepo"),
            ("me@gh:acme/monorepo.git", "acme/monorepo"),
        ] {
            assert_eq!(slug_from_remote_url(url).as_deref(), Some(want), "{url}");
        }
    }

    /// `C:/src/repo` is scp-like by shape, since the colon precedes every
    /// slash, but a one-letter host is a Windows drive and git reads it as a
    /// path.
    #[test]
    fn slug_rejects_a_windows_drive_path() {
        assert_eq!(slug_from_remote_url("C:/src/repo"), None);
        assert_eq!(remote_host("C:/src/repo"), None);
    }

    #[test]
    fn remote_host_reads_every_url_shape() {
        for (url, want) in [
            ("gh:acme/repo.git", Some("gh")),
            ("git@github.com:acme/repo.git", Some("github.com")),
            ("ssh://git@github.com/acme/repo", Some("github.com")),
            ("https://github.com/acme/repo", Some("github.com")),
            ("/srv/git/repo.git", None),
            ("", None),
        ] {
            assert_eq!(remote_host(url), want, "{url}");
        }
    }

    /// The alias reaches a host only once ssh config says so, and the answer
    /// follows the resolver rather than the spelling of the alias.
    #[test]
    fn an_ssh_alias_reaches_a_host_only_when_the_config_says_so() {
        let to_github = |_: &str| Some("github.com".to_string());
        let to_gitlab = |_: &str| Some("gitlab.com".to_string());
        let unresolved = |_: &str| None;

        assert!(reaches("gh:acme/repo.git", "github.com", &to_github));
        assert!(reaches("ssh://gh/acme/repo.git", "github.com", &to_github));
        assert!(!reaches("gh:acme/repo.git", "github.com", &to_gitlab));
        assert!(reaches("gh:acme/repo.git", "gitlab.com", &to_gitlab));
        assert!(!reaches("gh:acme/repo.git", "github.com", &unresolved));
    }

    /// Only ssh consults `~/.ssh/config`; an https host is literal. Resolving
    /// one through ssh would let an unrelated `Host` block decide that an
    /// https remote points at github.com.
    #[test]
    fn an_https_host_ignores_ssh_config() {
        let to_github = |_: &str| Some("github.com".to_string());
        assert!(!reaches(
            "https://gh/acme/repo.git",
            "github.com",
            &to_github
        ));
        assert!(!reaches("git://gh/acme/repo.git", "github.com", &to_github));
        assert_eq!(
            reached_host("https://gh/acme/repo.git", &to_github).as_deref(),
            Some("gh")
        );
    }

    #[test]
    fn hostname_is_read_from_the_ssh_config_dump() {
        let dump = "user git\nhostname github.com\nport 22\nhostkeyalias gh\n";
        assert_eq!(ssh_hostname_from_dump(dump).as_deref(), Some("github.com"));
        assert_eq!(ssh_hostname_from_dump("user git\nport 22\n"), None);
    }

    #[test]
    fn only_the_named_host_is_reached() {
        assert!(is_github_remote("https://github.com/o/r.git"));
        assert!(is_github_remote("git@github.com:o/r.git"));
        assert!(is_github_remote("ssh://git@github.com/o/r"));
        assert!(!is_github_remote("https://gitlab.com/o/r.git"));
        assert!(!is_github_remote("git@bitbucket.org:o/r.git"));
        assert!(!is_github_remote("https://github.com.evil.test/o/r"));
        assert!(reaches(
            "git@ghe.acme.test:o/r.git",
            "ghe.acme.test",
            &|_| None
        ));
    }

    /// GitHub serves the same site at `www.github.com`, and git keeps whichever
    /// spelling the clone URL used. Only that one prefix counts: look-alikes
    /// and other subdomains do not.
    #[test]
    fn the_www_host_is_the_same_host() {
        let resolve = |alias: &str| (alias == "gh").then(|| "www.github.com".to_string());
        for (url, want) in [
            ("https://www.github.com/o/r.git", true),
            ("https://WWW.GitHub.com/o/r", true),
            ("git@www.github.com:o/r.git", true),
            ("ssh://git@www.github.com/o/r", true),
            ("gh:o/r.git", true),
            ("https://wwwgithub.com/o/r", false),
            ("https://www.github.com.evil.test/o/r", false),
            ("https://api.www.github.com/o/r", false),
            ("https://gist.github.com/o/r", false),
        ] {
            assert_eq!(reaches(url, "github.com", &resolve), want, "{url}");
        }
    }

    #[test]
    fn validate_slug_accepts_owner_repo_and_nested_groups() {
        assert!(validate_slug("K-Nette/BountyPop_GODOT").is_ok());
        assert!(validate_slug("a/b").is_ok());
        assert!(validate_slug("owner.name/repo.name").is_ok());
        assert!(validate_slug("group/sub/app").is_ok());
    }

    #[test]
    fn validate_slug_rejects_anything_that_could_escape_a_path() {
        for bad in [
            "",
            "owner",
            "../../etc",
            "owner/../..",
            "owner/",
            "/repo",
            "a//b",
            "own er/repo",
            "owner/re po",
        ] {
            assert!(validate_slug(bad).is_err(), "{bad} should be rejected");
        }
    }
}
