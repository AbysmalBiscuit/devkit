# GitHub Tracker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give a project that tracks work in GitHub Issues the same title-derived slug, summary file, state column, state gate and dashboard timeline that Linear projects already get, selected by `[tracker] kind = "github"`.

**Architecture:** Phase 2 built a `Tracker` trait and moved only `issue status` and `issue end` onto it. This plan finishes that migration, hardens the PR-resolution paths every command shares, then adds the GitHub adapter behind the trait and flips the switch. Repository identity becomes explicit configuration resolved once per command rather than re-derived from the `origin` remote at nine call sites.

**Tech Stack:** Rust 2024, `anyhow`, `serde`, `ureq` (via `devkit_common::github`), GitHub GraphQL v4 and REST v3, the `gh` CLI as fallback transport, `minijinja` templates, `schemars` for config schema.

**Spec:** `docs/superpowers/specs/2026-08-24-github-tracker-design.md` — read it alongside this plan. It carries the probe evidence and the reasoning behind every rule here. Its companion `docs/superpowers/specs/2026-08-24-github-tracker-review-log.md` records eight rounds of adversarial review and the decisions that came out of them.

## Global Constraints

Copied verbatim from the spec. Every task's requirements implicitly include these.

- **`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all --check` must all pass before every commit.** Zero-warning policy.
- **TDD.** Write the failing test first and watch it fail for the right reason before writing the fix.
- **No `_ =>` catch-all arms** over `StateKind`, `TrackerKind` or `Role`. Match exhaustively.
- **`any` equivalent is banned:** no `unwrap()` on network or filesystem results outside tests. `anyhow` with `.context()` everywhere.
- **Nothing in this plan spawns a process in a test.** Every parse function tests against a recorded JSON fixture. Windows CI runs the whole suite.
- **Fixture content is synthesized in the shape of the originals, never copied verbatim.** Issue titles, bodies and branch names in fixtures must be invented to match the structure of the probed responses, not lifted from `K-Nette/BountyPop_GODOT` or `AbysmalBiscuit/alacritree`.
- **Config schema regenerates in the same task that changes a config type.** `tests/config_schema.rs` fails on drift. Regenerate with `DEVKIT_UPDATE_SCHEMA=1 cargo test`.
- **The primary clone stays on `main`.** Do this work in a worktree: `git worktree add ../devkit-worktrees/github-tracker -b feat/github-tracker main`.
- **Repository-scoped `gh` commands carry `--repo github.com/owner/repo`.** `gh api` and `gh auth token` carry `--hostname github.com`. `gh auth status` stays unscoped except for `--hostname`.
- **GitHub Enterprise is out of scope.** The explicit `github.com` host is what keeps it out.

## Phase boundary

**Phase A is tasks 1–4.** None of them deliver GitHub functionality. All four touch paths every existing user runs, and each ships as a correctness fix on its own. Phase A is independently valuable and independently releasable.

**Phase B is tasks 5–12.** The GitHub adapter and everything that makes it live.

If Phase A is released alone, stop after task 4 and skip task 12's GitHub-specific documentation rows; everything else in task 12 still applies.

---

## File structure

| File | Responsibility |
|---|---|
| `crates/devkit-config/src/lib.rs` | gains `GithubConfig` at `[github]` with `issues_repo` and `pr_repo` |
| `crates/devkit-common/src/github.rs` | gains `Origin`, `Repo`, `Repos`, `HeadLookup`; `pr_by_head` moves to GraphQL; `graphql`/`rest_*` gain the explicit host |
| `crates/devkit-common/src/tracker/mod.rs` | `issue_ref` becomes fallible; `resolve` regains a repository parameter |
| `crates/devkit-common/src/tracker/github.rs` | **new** — the GitHub adapter |
| `crates/devkit-common/src/record.rs` | `IssueRecord` gains `pr: Option<PrLocator>` |
| `crates/devkit-issue/src/status.rs` | per-branch head lookups replace the 500-cap listing; `IssueWorktree` carries `PrStatus` |
| `crates/devkit-issue/src/prs.rs` | `gather` takes the resolved tracker |
| `src/bin/issue/review/request.rs` | typed lookup, `--pr`, record write, OID gate |
| `src/bin/issue/review/finish.rs` | typed lookup, locator precedence, OID gate |
| `src/bin/issue/checkout.rs` | `PrLocator`, `candidates` via the trait, record write |
| `src/bin/issue/setup.rs` | title and details via the trait |
| `src/bin/issue/info.rs`, `info_cache.rs` | `PrStatus` on both the live and cached paths |
| `src/bin/issue/triage.rs` | renders `PrStatus` |
| `src/bin/issue/dashboard/data.rs`, `cache.rs` | trait-driven timeline; scoped, encoded cache keys |
| `src/bin/devkit/auth.rs` | `devkit auth github` |

---

# Phase A — groundwork

## Task 1: The repository resolution seam

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (add `GithubConfig`, wire into the root config struct)
- Modify: `crates/devkit-common/src/github.rs` (add `Origin`, `Repo`, `Repos`; host-qualify `gh` and API calls)
- Modify: `crates/devkit-common/src/cmd.rs` (no signature change; see step 9)
- Modify: `src/bin/issue/prs.rs`, `src/bin/issue/checkout.rs`, `src/bin/issue/review/request.rs`, `src/bin/issue/review/finish.rs`, `src/bin/issue/dashboard/data.rs`, `crates/devkit-issue/src/status.rs` (take `Repos` instead of calling `repo_slug`)
- Modify: `schema/devkit-config.json` (regenerated, not hand-edited)
- Test: `crates/devkit-common/src/github.rs` (inline `mod tests`), `crates/devkit-config/src/lib.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `devkit_config::GithubConfig { issues_repo: Option<String>, pr_repo: Option<String> }`, reachable as `config.github`
  - `devkit_common::github::Origin` — `Configured | Overridden | Defaulted`
  - `devkit_common::github::Repo { slug: String, origin: Origin }` with `fn qualified(&self) -> String`
  - `devkit_common::github::Repos` with `fn resolve(cfg: &GithubConfig, cwd: &str, pr_override: Option<&str>) -> Repos`, `fn issues(&self) -> Result<&Repo>`, `fn prs(&self) -> Result<&Repo>`
  - `devkit_common::github::validate_slug(s: &str) -> Result<()>`
  - `devkit_common::github::github_origin_slug(cwd: &str) -> Result<String>` — host-checked

- [ ] **Step 1: Write the failing slug-validation test**

Add to the `mod tests` block at the bottom of `crates/devkit-common/src/github.rs`:

```rust
#[test]
fn validate_slug_accepts_owner_repo() {
    assert!(validate_slug("K-Nette/BountyPop_GODOT").is_ok());
    assert!(validate_slug("a/b").is_ok());
    assert!(validate_slug("owner.name/repo.name").is_ok());
}

#[test]
fn validate_slug_rejects_anything_that_could_escape_a_path() {
    for bad in [
        "",
        "owner",
        "owner/repo/extra",
        "../../etc",
        "owner/../..",
        "owner/",
        "/repo",
        "own er/repo",
        "owner/re po",
    ] {
        assert!(validate_slug(bad).is_err(), "{bad} should be rejected");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p devkit-common validate_slug`
Expected: FAIL — `cannot find function 'validate_slug' in this scope`.

- [ ] **Step 3: Implement `validate_slug`**

Add to `crates/devkit-common/src/github.rs`, near `slug_from_remote_url`:

```rust
/// A repository slug is exactly `owner/repo`, both segments non-empty and made
/// only of characters GitHub allows in a name. Configured slugs reach cache
/// filenames and `gh --repo` arguments, so a slug carrying a path separator or
/// `..` is rejected where it is resolved rather than sanitized downstream.
pub fn validate_slug(s: &str) -> Result<()> {
    fn ok_segment(seg: &str) -> bool {
        !seg.is_empty()
            && seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && seg != "."
            && seg != ".."
    }
    let mut parts = s.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        anyhow::bail!("`{s}` is not an owner/repo repository slug");
    };
    anyhow::ensure!(
        ok_segment(owner) && ok_segment(repo),
        "`{s}` is not an owner/repo repository slug"
    );
    Ok(())
}
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p devkit-common validate_slug`
Expected: PASS (2 tests).

- [ ] **Step 5: Write the failing host-check test**

Add to the same `mod tests`:

```rust
#[test]
fn github_origin_rejects_a_non_github_host() {
    // `slug_from_remote_url` is host-blind by design — its other callers
    // already know they hold a GitHub URL. The host check lives here.
    assert!(is_github_remote("https://github.com/o/r.git"));
    assert!(is_github_remote("git@github.com:o/r.git"));
    assert!(is_github_remote("ssh://git@github.com/o/r"));
    assert!(!is_github_remote("https://gitlab.com/o/r.git"));
    assert!(!is_github_remote("git@bitbucket.org:o/r.git"));
    assert!(!is_github_remote("https://github.com.evil.test/o/r"));
}
```

- [ ] **Step 6: Run it and watch it fail**

Run: `cargo test -p devkit-common github_origin_rejects`
Expected: FAIL — `cannot find function 'is_github_remote'`.

- [ ] **Step 7: Implement the host check and the host-checked origin reader**

Add to `crates/devkit-common/src/github.rs`:

```rust
/// Whether a git remote URL points at github.com. `slug_from_remote_url` parses
/// any `host/owner/repo` shape without checking the host, so a GitLab origin
/// yields a slug and every downstream caller would query github.com for a
/// repository that is not the project's.
pub fn is_github_remote(url: &str) -> bool {
    let u = url.trim();
    let host = if let Some(rest) = u.strip_prefix("git@") {
        rest.split(':').next().unwrap_or("")
    } else if let Some(rest) = u.split("://").nth(1) {
        let after_user = rest.rsplit('@').next().unwrap_or(rest);
        after_user.split(['/', ':']).next().unwrap_or("")
    } else {
        ""
    };
    host.eq_ignore_ascii_case("github.com")
}

/// The `origin` slug, only when origin is a github.com remote. This is the
/// single entry point for defaulting a repository from the remote, so the host
/// check cannot be skipped by a caller that declared its tracker and therefore
/// never ran detection.
pub fn github_origin_slug(cwd: &str) -> Result<String> {
    let url = crate::cmd::git(&["remote", "get-url", "origin"], cwd)
        .context("reading the `origin` remote")?;
    anyhow::ensure!(
        is_github_remote(&url),
        "`origin` is not a github.com remote ({}); set [github] issues_repo / pr_repo explicitly",
        url.trim()
    );
    slug_from_remote_url(&url)
        .with_context(|| format!("no owner/repo in the origin URL `{}`", url.trim()))
}
```

- [ ] **Step 8: Run it and watch it pass**

Run: `cargo test -p devkit-common github_origin_rejects`
Expected: PASS.

- [ ] **Step 9: Write the failing `Repos` test**

Add to the same `mod tests`:

```rust
fn cfg(issues: Option<&str>, prs: Option<&str>) -> devkit_config::GithubConfig {
    devkit_config::GithubConfig {
        issues_repo: issues.map(str::to_string),
        pr_repo: prs.map(str::to_string),
    }
}

#[test]
fn repos_resolve_each_key_independently() {
    // Both configured: no origin is consulted at all, so a project whose code
    // lives outside GitHub still resolves.
    let r = Repos::from_parts(&cfg(Some("org/planning"), Some("up/app")), None, None);
    assert_eq!(r.issues().unwrap().slug, "org/planning");
    assert_eq!(r.issues().unwrap().origin, Origin::Configured);
    assert_eq!(r.prs().unwrap().slug, "up/app");

    // Only pr_repo configured and no origin: the PR paths work, and only an
    // operation needing the issues repository fails — naming the key.
    let r = Repos::from_parts(&cfg(None, Some("up/app")), None, None);
    assert_eq!(r.prs().unwrap().slug, "up/app");
    let err = r.issues().unwrap_err().to_string();
    assert!(err.contains("issues_repo"), "{err}");

    // Neither configured, origin available: both default to it.
    let r = Repos::from_parts(&cfg(None, None), Some("me/fork".into()), None);
    assert_eq!(r.issues().unwrap().slug, "me/fork");
    assert_eq!(r.issues().unwrap().origin, Origin::Defaulted);
    assert_eq!(r.prs().unwrap().slug, "me/fork");

    // A per-invocation override beats pr_repo and is marked as such.
    let r = Repos::from_parts(&cfg(None, Some("up/app")), None, Some("other/x"));
    assert_eq!(r.prs().unwrap().slug, "other/x");
    assert_eq!(r.prs().unwrap().origin, Origin::Overridden);
}

#[test]
fn repos_reject_a_configured_slug_that_is_not_owner_repo() {
    let r = Repos::from_parts(&cfg(Some("../../etc/passwd"), None), None, None);
    let err = r.issues().unwrap_err().to_string();
    assert!(err.contains("owner/repo"), "{err}");
}

#[test]
fn a_repo_qualifies_itself_with_the_host() {
    let r = Repo {
        slug: "o/r".into(),
        origin: Origin::Defaulted,
    };
    // `--repo o/r` leaves GH_HOST free to pick an enterprise host, so every
    // `gh pr` argument names github.com explicitly.
    assert_eq!(r.qualified(), "github.com/o/r");
}
```

- [ ] **Step 10: Run it and watch it fail**

Run: `cargo test -p devkit-common repos_`
Expected: FAIL — `cannot find type 'Repos'`.

- [ ] **Step 11: Add `GithubConfig` to the config crate**

In `crates/devkit-config/src/lib.rs`, add the struct next to `LinearConfig`:

```rust
/// Which GitHub repositories this project uses. Both default to the `origin`
/// remote, so a project setting neither reaches the same repository it does
/// today. They are separate because a fork opens its PRs upstream while its
/// issues may sit on either side, and because a project may track issues in a
/// repository separate from its code.
///
/// This table is not under `[tracker]`: a project on Linear with a fork
/// workflow needs `pr_repo` just as much as a GitHub one does.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GithubConfig {
    /// Repository holding the issues, e.g. `org/planning`.
    pub issues_repo: Option<String>,
    /// Repository pull requests are opened against, e.g. `upstream/app`.
    pub pr_repo: Option<String>,
}
```

And add the field to the root config struct, beside `pub linear: LinearConfig`:

```rust
    /// Which GitHub repositories back issues and pull requests.
    #[serde(default)]
    pub github: GithubConfig,
```

- [ ] **Step 12: Implement `Origin`, `Repo` and `Repos`**

Add to `crates/devkit-common/src/github.rs`:

```rust
/// Where a repository slug came from. A configured or overridden slug is a
/// decision the project made; a defaulted one was read from the remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Configured,
    Overridden,
    Defaulted,
}

/// One resolved repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub slug: String,
    pub origin: Origin,
}

impl Repo {
    /// The `gh --repo` spelling. The host is explicit because `--repo o/r`
    /// leaves `GH_HOST` free to select an enterprise host, which would send a
    /// token to a host it was not issued for.
    pub fn qualified(&self) -> String {
        format!("github.com/{}", self.slug)
    }
}

/// The repositories one command works against, resolved once and threaded to
/// every GitHub operation. Each key resolves independently and is required only
/// where it is used, so a Linear project with a fork workflow sets `pr_repo`
/// alone and is never asked for an `issues_repo` it will not read.
#[derive(Debug, Clone)]
pub struct Repos {
    issues: std::result::Result<Repo, String>,
    prs: std::result::Result<Repo, String>,
}

impl Repos {
    /// Resolve from config plus the `origin` remote. `pr_override` is `issue prs
    /// --repo`, one invocation's override of `pr_repo`.
    pub fn resolve(
        cfg: &devkit_config::GithubConfig,
        cwd: &str,
        pr_override: Option<&str>,
    ) -> Repos {
        // The origin lookup is skipped entirely when config supplies both keys,
        // so a project outside GitHub never pays for it and never fails on it.
        let need_origin = cfg.issues_repo.is_none() || (cfg.pr_repo.is_none() && pr_override.is_none());
        let origin = need_origin.then(|| github_origin_slug(cwd)).transpose();
        let origin = match origin {
            Ok(v) => v,
            Err(e) => {
                let msg = format!("{e:#}");
                return Repos {
                    issues: cfg
                        .issues_repo
                        .clone()
                        .map(|s| (s, Origin::Configured))
                        .ok_or_else(|| msg.clone())
                        .and_then(checked),
                    prs: pr_override
                        .map(|s| (s.to_string(), Origin::Overridden))
                        .or_else(|| cfg.pr_repo.clone().map(|s| (s, Origin::Configured)))
                        .ok_or(msg)
                        .and_then(checked),
                };
            }
        };
        Repos::from_parts(cfg, origin, pr_override)
    }

    /// `resolve` with the origin slug supplied rather than read, so resolution
    /// is testable without a git remote.
    #[doc(hidden)]
    pub fn from_parts(
        cfg: &devkit_config::GithubConfig,
        origin: Option<String>,
        pr_override: Option<&str>,
    ) -> Repos {
        let missing = |key: &str| {
            format!(
                "no GitHub repository for {key}: set [github] {key} or give the project a \
                 github.com `origin` remote"
            )
        };
        Repos {
            issues: cfg
                .issues_repo
                .clone()
                .map(|s| (s, Origin::Configured))
                .or_else(|| origin.clone().map(|s| (s, Origin::Defaulted)))
                .ok_or_else(|| missing("issues_repo"))
                .and_then(checked),
            prs: pr_override
                .map(|s| (s.to_string(), Origin::Overridden))
                .or_else(|| cfg.pr_repo.clone().map(|s| (s, Origin::Configured)))
                .or_else(|| origin.map(|s| (s, Origin::Defaulted)))
                .ok_or_else(|| missing("pr_repo"))
                .and_then(checked),
        }
    }

    /// The issues repository, or the error explaining which key would supply it.
    pub fn issues(&self) -> Result<&Repo> {
        self.issues.as_ref().map_err(|e| anyhow::anyhow!(e.clone()))
    }

    /// The pull-request repository, or the error explaining which key would
    /// supply it.
    pub fn prs(&self) -> Result<&Repo> {
        self.prs.as_ref().map_err(|e| anyhow::anyhow!(e.clone()))
    }
}

/// Validate a resolved slug at the point of resolution, so a bad value is
/// reported against the key that carries it rather than failing later inside a
/// URL or a cache filename.
fn checked((slug, origin): (String, Origin)) -> std::result::Result<Repo, String> {
    match validate_slug(&slug) {
        Ok(()) => Ok(Repo { slug, origin }),
        Err(e) => Err(format!("{e:#}")),
    }
}
```

- [ ] **Step 13: Run the `Repos` tests and watch them pass**

Run: `cargo test -p devkit-common repos_ && cargo test -p devkit-common a_repo_qualifies`
Expected: PASS (3 tests).

- [ ] **Step 14: Pin the host on every API and `gh` call**

In `crates/devkit-common/src/github.rs`, change the `gh auth token` spawn inside `resolve_token` to name the host:

```rust
    // One `gh` spawn, cached for the process — amortized across every HTTP call.
    // `--hostname` is explicit: with `GH_HOST` set, an unqualified call returns
    // an enterprise token, which the callers below would then send to
    // api.github.com.
    crate::cmd::capture(
        "gh",
        &["auth", "token", "--hostname", "github.com"],
        None,
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
```

- [ ] **Step 15: Add `--repo` to the `gh` fallback paths**

Add a helper to `crates/devkit-common/src/cmd.rs`:

```rust
/// `gh <args...> --repo github.com/<slug>` as JSON. Every repository-scoped
/// `gh` invocation goes through here so no call can be left to pick its
/// repository from the ambient `GH_REPO`.
pub fn gh_json_in<T: serde::de::DeserializeOwned>(
    args: &[&str],
    repo: &crate::github::Repo,
    cwd: &str,
) -> Result<T> {
    let qualified = repo.qualified();
    let mut v: Vec<&str> = args.to_vec();
    v.push("--repo");
    v.push(&qualified);
    gh_json(&v, cwd)
}
```

Then rewrite each `gh_json(&["pr", ...], cwd)` call in `src/bin/issue/review/request.rs`, `src/bin/issue/review/finish.rs`, `src/bin/issue/prs.rs` and `crates/devkit-issue/src/status.rs` as `gh_json_in(&["pr", ...], repos.prs()?, cwd)`. Thread a `&Repos` parameter down to each. Leave `gh auth status` and any `gh api` invocation unscoped by `--repo`; give them `--hostname github.com` instead.

- [ ] **Step 16: Write the argument-vector test**

Add to `crates/devkit-common/src/cmd.rs`'s `mod tests`:

```rust
#[test]
fn gh_json_in_always_names_the_repository_and_host() {
    let repo = crate::github::Repo {
        slug: "o/r".into(),
        origin: crate::github::Origin::Defaulted,
    };
    // Asserted on the argument vector, not on behavior: the point is that
    // neither GH_REPO nor GH_HOST can redirect the call, and behavior alone
    // cannot distinguish "no ambient variable set" from "flag present".
    assert_eq!(
        gh_args(&["pr", "list"], &repo),
        vec!["pr", "list", "--repo", "github.com/o/r"]
    );
}
```

Extract the vector construction from `gh_json_in` into a testable `fn gh_args(args: &[&str], repo: &Repo) -> Vec<String>` and have `gh_json_in` call it.

- [ ] **Step 17: Run the full suite**

Run: `cargo test --workspace`
Expected: PASS. Any failure here is a call site that still calls `repo_slug` directly or a `gh_json` that was missed.

- [ ] **Step 18: Regenerate the config schema**

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test -p devkit config_schema`
Then: `cargo test -p devkit config_schema`
Expected: PASS. `schema/devkit-config.json` now carries the `github` table. Do not hand-edit it.

- [ ] **Step 19: Verify clippy and formatting**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 20: Commit**

```bash
git add crates/devkit-config/src/lib.rs crates/devkit-common/src/github.rs \
        crates/devkit-common/src/cmd.rs crates/devkit-issue/src/status.rs \
        src/bin/issue schema/devkit-config.json
git commit -m "feat(github): resolve issue and PR repositories from config

Every GitHub operation re-derived its repository from the origin remote,
which is wrong for a fork and for a project tracking issues elsewhere.
[github] issues_repo and pr_repo are resolved once per command and
threaded to each operation; each key resolves independently and is
required only where it is used.

Repository-scoped gh commands name github.com/owner/repo and the API
and auth calls name the host, so neither GH_REPO nor GH_HOST can send a
call — or a token — somewhere the project did not choose."
```

---

## Task 2: The typed PR lookup

**Files:**
- Modify: `crates/devkit-common/src/github.rs` (`pr_by_head` to GraphQL, `HeadLookup`)
- Modify: `src/bin/issue/review/request.rs:107` (`existing_pr`), `src/bin/issue/review/finish.rs:43` (`branch_pr_number`), `:61` (`fetch_pr_full`)
- Test: `crates/devkit-common/src/github.rs` (inline `mod tests`), `src/bin/issue/review/finish.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `Repo`, `Repos` from task 1.
- Produces:
  - `devkit_common::github::HeadLookup` — `Unique(PrBrief) | NoMatch | Ambiguous(Vec<PrBrief>) | Unavailable(String)`
  - `devkit_common::github::pr_by_head(repo: &Repo, branch: &str) -> HeadLookup` (replaces the old `Result<Option<PrBrief>>`)
  - `devkit_common::github::head_query(slug: &str, branch: &str) -> String`
  - `devkit_common::github::parse_head_lookup(resp: &serde_json::Value) -> HeadLookup`

- [ ] **Step 1: Write the failing parse tests**

Add to `crates/devkit-common/src/github.rs`'s `mod tests`:

```rust
fn head_resp(nodes: &str, total: u32) -> serde_json::Value {
    serde_json::from_str(&format!(
        r#"{{"data":{{"repository":{{"pullRequests":{{"totalCount":{total},"nodes":[{nodes}]}}}}}}}}"#
    ))
    .unwrap()
}

const NODE_A: &str = r#"{"number":185,"state":"OPEN","url":"https://github.com/up/app/pull/185",
  "headRefName":"fix/glyph-overhang","headRefOid":"aaaa111",
  "headRepositoryOwner":{"login":"contributor"}}"#;
const NODE_B: &str = r#"{"number":42,"state":"MERGED","url":"https://github.com/up/app/pull/42",
  "headRefName":"fix/glyph-overhang","headRefOid":"bbbb222",
  "headRepositoryOwner":{"login":"someone-else"}}"#;

#[test]
fn one_node_parses_to_unique() {
    let l = parse_head_lookup(&head_resp(NODE_A, 1));
    let HeadLookup::Unique(pr) = l else {
        panic!("expected Unique, got {l:?}")
    };
    assert_eq!(pr.number, 185);
    assert_eq!(pr.head_ref_oid, "aaaa111");
}

#[test]
fn a_fork_head_still_parses_to_unique() {
    // The whole reason this moved off REST: the head owner differs from the
    // searched repository's owner and the match must still be found.
    let HeadLookup::Unique(pr) = parse_head_lookup(&head_resp(NODE_A, 1)) else {
        panic!("expected Unique")
    };
    assert_eq!(pr.head_repo_owner.as_deref(), Some("contributor"));
}

#[test]
fn zero_nodes_parses_to_no_match() {
    assert!(matches!(
        parse_head_lookup(&head_resp("", 0)),
        HeadLookup::NoMatch
    ));
}

#[test]
fn two_nodes_parse_to_ambiguous() {
    let l = parse_head_lookup(&head_resp(&format!("{NODE_A},{NODE_B}"), 2));
    let HeadLookup::Ambiguous(c) = l else {
        panic!("expected Ambiguous, got {l:?}")
    };
    assert_eq!(c.len(), 2);
}

#[test]
fn a_total_count_beyond_the_window_is_ambiguous_not_unique() {
    // One node returned but the server says there are three: ranking a
    // truncated set is exactly the false-unique this type exists to prevent.
    assert!(matches!(
        parse_head_lookup(&head_resp(NODE_A, 3)),
        HeadLookup::Ambiguous(_)
    ));
}

#[test]
fn a_graphql_error_body_parses_to_unavailable() {
    let resp: serde_json::Value =
        serde_json::from_str(r#"{"errors":[{"message":"Bad credentials"}]}"#).unwrap();
    let HeadLookup::Unavailable(why) = parse_head_lookup(&resp) else {
        panic!("expected Unavailable")
    };
    assert!(why.contains("Bad credentials"), "{why}");
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p devkit-common head_ && cargo test -p devkit-common parses_to`
Expected: FAIL — `cannot find function 'parse_head_lookup'`.

- [ ] **Step 3: Add `head_ref_oid` and `head_repo_owner` to `PrBrief`**

In `crates/devkit-common/src/github.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrBrief {
    pub number: u64,
    pub state: String, // MERGED | OPEN | CLOSED
    pub url: String,
    pub head_ref_name: String,
    /// The commit at the PR's head. This is what ties a PR to a worktree: a
    /// branch name does not, since two forks routinely propose the same name.
    pub head_ref_oid: String,
    /// The fork the head branch lives in, when the API reported one.
    pub head_repo_owner: Option<String>,
}
```

Fix the two existing constructors (`list_prs`, `pr_by_head`'s REST body) to populate the new fields; `list_prs`'s REST shape supplies `head.sha` and `head.repo.owner.login`.

- [ ] **Step 4: Implement `HeadLookup`, the query and the parser**

```rust
/// What a head-branch lookup found. An `Option` cannot distinguish "the
/// transport answered and there is no such PR" from "the transport failed", and
/// every caller collapsed both into a fallback that guessed.
#[derive(Debug, Clone)]
pub enum HeadLookup {
    Unique(PrBrief),
    NoMatch,
    Ambiguous(Vec<PrBrief>),
    Unavailable(String),
}

/// PRs whose head branch is `branch`, in any fork.
///
/// GraphQL rather than REST: REST documents `head` only as `user:ref-name`, and
/// the head owner cannot be derived — git allows a push URL distinct from the
/// fetch URL, `remote.pushDefault`, and per-branch push remotes, so `origin`
/// need not be where a branch was pushed. `headRefName` is a documented
/// argument that matches a fork's branch with no owner qualifier.
pub fn head_query(slug: &str, branch: &str) -> String {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    format!(
        r#"query {{ repository(owner: {owner}, name: {name}) {{
             pullRequests(headRefName: {branch}, first: 10,
                          states: [OPEN, CLOSED, MERGED]) {{
               totalCount
               nodes {{ number state url headRefName headRefOid
                        headRepositoryOwner {{ login }} }}
             }} }} }}"#,
        owner = serde_json::Value::from(owner),
        name = serde_json::Value::from(name),
        branch = serde_json::Value::from(branch),
    )
}

/// Parse a `head_query` response. `totalCount` beyond the returned nodes is
/// ambiguity, not a unique answer: a winner outside the window would otherwise
/// be silently dropped.
pub fn parse_head_lookup(resp: &serde_json::Value) -> HeadLookup {
    if let Some(errs) = resp["errors"].as_array()
        && !errs.is_empty()
    {
        let msg = errs
            .iter()
            .filter_map(|e| e["message"].as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return HeadLookup::Unavailable(msg);
    }
    let conn = &resp["data"]["repository"]["pullRequests"];
    let total = conn["totalCount"].as_u64().unwrap_or(0);
    let nodes: Vec<PrBrief> = conn["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            Some(PrBrief {
                number: n["number"].as_u64()?,
                state: n["state"].as_str()?.to_string(),
                url: n["url"].as_str()?.to_string(),
                head_ref_name: n["headRefName"].as_str()?.to_string(),
                head_ref_oid: n["headRefOid"].as_str().unwrap_or("").to_string(),
                head_repo_owner: n["headRepositoryOwner"]["login"]
                    .as_str()
                    .map(str::to_string),
            })
        })
        .collect();
    match nodes.len() {
        0 => HeadLookup::NoMatch,
        1 if total <= 1 => HeadLookup::Unique(nodes.into_iter().next().expect("len == 1")),
        _ => HeadLookup::Ambiguous(nodes),
    }
}

/// Look up `branch`'s PR in `repo`. `Unavailable` is the only answer a caller
/// may respond to by trying another transport.
pub fn pr_by_head(repo: &Repo, branch: &str) -> HeadLookup {
    if token().is_none() {
        return HeadLookup::Unavailable("no GitHub token resolved".into());
    }
    match graphql(&head_query(&repo.slug, branch)) {
        Ok(v) => parse_head_lookup(&v),
        Err(e) => HeadLookup::Unavailable(format!("{e:#}")),
    }
}
```

- [ ] **Step 5: Run the parse tests and watch them pass**

Run: `cargo test -p devkit-common head_ parses_to unique ambiguous`
Expected: PASS (6 tests).

- [ ] **Step 6: Write the failing caller-branching test**

Add to `src/bin/issue/review/finish.rs`'s `mod tests`:

```rust
#[test]
fn no_match_does_not_reach_the_gh_fallback() {
    // The bug this replaces: `pr_by_head(..).ok()` turned Some(None) into a
    // satisfied `if let`, so "the API said there is no PR" and "the API failed"
    // both returned Ok(None) — one of them without ever consulting `gh`.
    assert_eq!(decide_fallback(&HeadLookup::NoMatch), Fallback::No);
    assert_eq!(
        decide_fallback(&HeadLookup::Unavailable("no token".into())),
        Fallback::Yes
    );
    assert_eq!(
        decide_fallback(&HeadLookup::Unique(brief(7))),
        Fallback::No
    );
    assert_eq!(
        decide_fallback(&HeadLookup::Ambiguous(vec![brief(7), brief(8)])),
        Fallback::No
    );
}

#[test]
fn an_ambiguous_lookup_refuses_on_an_acting_path() {
    let err = resolve_acting(&HeadLookup::Ambiguous(vec![brief(7), brief(8)]))
        .unwrap_err()
        .to_string();
    assert!(err.contains("#7") && err.contains("#8"), "{err}");
}

fn brief(n: u64) -> devkit_common::github::PrBrief {
    devkit_common::github::PrBrief {
        number: n,
        state: "OPEN".into(),
        url: format!("https://github.com/o/r/pull/{n}"),
        head_ref_name: "feat/x".into(),
        head_ref_oid: "cafe1".into(),
        head_repo_owner: None,
    }
}
```

- [ ] **Step 7: Run it and watch it fail**

Run: `cargo test -p devkit --bin issue no_match_does_not_reach`
Expected: FAIL — `cannot find function 'decide_fallback'`.

- [ ] **Step 8: Rewrite the three callers**

In `src/bin/issue/review/finish.rs`, replace `branch_pr_number`:

```rust
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fallback {
    Yes,
    No,
}

/// Only a transport that could not answer sends the caller to `gh`. A definite
/// "no PR" is an answer and must be trusted, or the fallback re-asks a question
/// that was already resolved and can return a different PR.
pub(crate) fn decide_fallback(l: &github::HeadLookup) -> Fallback {
    match l {
        github::HeadLookup::Unavailable(_) => Fallback::Yes,
        github::HeadLookup::Unique(_)
        | github::HeadLookup::NoMatch
        | github::HeadLookup::Ambiguous(_) => Fallback::No,
    }
}

/// The single PR an acting path may operate on. Ambiguity is refused rather
/// than ranked: `review finish` is about to merge or close, and two forks
/// proposing one branch name is the case that produces two candidates.
pub(crate) fn resolve_acting(l: &github::HeadLookup) -> Result<Option<github::PrBrief>> {
    match l {
        github::HeadLookup::Unique(p) => Ok(Some(p.clone())),
        github::HeadLookup::NoMatch => Ok(None),
        github::HeadLookup::Ambiguous(c) => {
            let list = c
                .iter()
                .map(|p| format!("#{} ({})", p.number, p.url))
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("several PRs share this head branch: {list} — pass --pr to choose one")
        }
        github::HeadLookup::Unavailable(why) => {
            anyhow::bail!("could not look up the PR for this branch: {why}")
        }
    }
}

/// PR number for head branch `b`, over direct HTTP when a token is available,
/// else `gh pr list`. `Ok(None)` means no PR (whichever path answered).
fn branch_pr_number(b: &str, repos: &github::Repos, cwd: &str) -> Result<Option<u64>> {
    let repo = repos.prs()?;
    let looked = github::pr_by_head(repo, b);
    if decide_fallback(&looked) == Fallback::No {
        return Ok(resolve_acting(&looked)?.map(|p| p.number));
    }
    // `--limit 1` is gone: the fallback must be able to see a second candidate
    // rather than silently taking whichever came first.
    let v: Vec<PrLite> = gh_json_in(
        &["pr", "list", "--head", b, "--state", "all", "--json", "number"],
        repo,
        cwd,
    )?;
    anyhow::ensure!(
        v.len() <= 1,
        "several PRs share this head branch — pass --pr to choose one"
    );
    Ok(v.into_iter().next().map(|p| p.number))
}
```

Apply the same shape to `fetch_pr_full` in the same file and to `existing_pr` in `src/bin/issue/review/request.rs:107`.

- [ ] **Step 9: Run the caller tests and watch them pass**

Run: `cargo test -p devkit --bin issue fallback acting`
Expected: PASS.

- [ ] **Step 10: Run the full suite, clippy and fmt**

Run: `cargo test --workspace && cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 11: Commit**

```bash
git add crates/devkit-common/src/github.rs src/bin/issue/review
git commit -m "feat(github): answer a head-branch lookup with a type

pr_by_head returned Result<Option<PrBrief>> and every caller collapsed
it with .ok(), so a lookup that found nothing returned \"no PR\" without
consulting gh, while one that errored fell through to a gh pr list
--limit 1 that took an arbitrary match.

Both transports now return Unique, NoMatch, Ambiguous or Unavailable,
and only Unavailable reaches the fallback. Acting paths refuse an
ambiguous answer instead of ranking it.

The lookup itself moves to GraphQL pullRequests(headRefName:): REST
documents head only as user:ref-name, and the head owner cannot be
derived because git allows a push URL distinct from the fetch URL."
```

---

## Task 3: Status's PR resolution and the tagged row

**Files:**
- Modify: `crates/devkit-issue/src/status.rs` (`IssueWorktree`, `Prs`, `fetch_prs`, `best_pr` removal, verdict)
- Modify: `src/bin/issue/triage.rs:5` (`pr_label`)
- Modify: `src/bin/issue/info.rs:167`, `:255`; `src/bin/issue/info_cache.rs`
- Modify: `crates/devkit-mcp/src/` — the `issue.status` handler's serialization
- Test: `crates/devkit-issue/src/status.rs`, `src/bin/issue/triage.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `HeadLookup`, `pr_by_head`, `Repos` from tasks 1–2.
- Produces:
  - `devkit_issue::status::PrStatus` — `None | Unique { number, state, url } | Ambiguous { candidates: Vec<PrRef> } | Unknown { reason: String }`
  - `IssueWorktree::pr: PrStatus` (new field), with `pr_number`, `pr_state`, `pr_url` kept as `#[serde]`-derived values
  - `devkit_issue::status::heads_query(slug: &str, branches: &[String]) -> String` — alias-batched
  - `devkit_issue::status::parse_heads(resp: &Value, branches: &[String]) -> HashMap<String, HeadLookup>`

- [ ] **Step 1: Write the failing `PrStatus` derivation test**

Add to `crates/devkit-issue/src/status.rs`'s `mod tests`:

```rust
#[test]
fn legacy_fields_derive_from_the_tag() {
    let u = PrStatus::Unique {
        number: 12,
        state: "MERGED".into(),
        url: "https://github.com/o/r/pull/12".into(),
    };
    assert_eq!(u.number(), Some(12));
    assert_eq!(u.state_label(), "MERGED");
    assert_eq!(u.url(), Some("https://github.com/o/r/pull/12"));

    assert_eq!(PrStatus::None.state_label(), "NO_PR");
    assert_eq!(PrStatus::None.number(), None);

    // The shape that used to render as `AMBIGUOUS #0`: a state string with no
    // number, formatted with unwrap_or(0), printing a PR that does not exist in
    // the column a human reads before deleting a worktree.
    let a = PrStatus::Ambiguous {
        candidates: vec![pr_ref(7), pr_ref(8)],
    };
    assert_eq!(a.state_label(), "AMBIGUOUS");
    assert_eq!(a.number(), None);
    assert_eq!(a.url(), None);
}

#[test]
fn the_verdict_never_closes_on_an_unidentified_pr() {
    for pr in [
        PrStatus::Ambiguous {
            candidates: vec![pr_ref(7), pr_ref(8)],
        },
        PrStatus::Unknown {
            reason: "recorded PR no longer resolves".into(),
        },
    ] {
        let (finished, why) = verdict(&pr, Some(StateKind::Completed), false);
        assert!(!finished, "{pr:?} must not be finished");
        assert!(why.is_some());
    }
    let (finished, _) = verdict(
        &PrStatus::Unique {
            number: 1,
            state: "MERGED".into(),
            url: "u".into(),
        },
        Some(StateKind::Completed),
        false,
    );
    assert!(finished);
}

fn pr_ref(n: u64) -> devkit_common::tracker::PrRef {
    devkit_common::tracker::PrRef {
        url: format!("https://github.com/o/r/pull/{n}"),
        number: n,
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p devkit-issue legacy_fields_derive`
Expected: FAIL — `cannot find type 'PrStatus'`.

- [ ] **Step 3: Implement `PrStatus` and re-shape `IssueWorktree`**

In `crates/devkit-issue/src/status.rs`:

```rust
/// A worktree's pull request, as the report knows it. The row carries the tag
/// rather than a state string plus two nullable fields, because an ambiguous
/// answer has candidates to name and a string has nowhere to put them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrStatus {
    /// No PR for this branch, from a transport that answered.
    None,
    Unique { number: u64, state: String, url: String },
    /// Several PRs share this head branch. The verdict stays closed: `issue
    /// end` reads it to decide whether a worktree may be deleted, and a
    /// stranger's merged PR must not authorize that.
    Ambiguous {
        candidates: Vec<devkit_common::tracker::PrRef>,
    },
    /// The PR could not be identified — no token, a failed request, or a
    /// recorded PR that no longer resolves.
    Unknown { reason: String },
}

impl PrStatus {
    pub fn number(&self) -> Option<u64> {
        match self {
            PrStatus::Unique { number, .. } => Some(*number),
            PrStatus::None | PrStatus::Ambiguous { .. } | PrStatus::Unknown { .. } => None,
        }
    }

    pub fn url(&self) -> Option<&str> {
        match self {
            PrStatus::Unique { url, .. } => Some(url),
            PrStatus::None | PrStatus::Ambiguous { .. } | PrStatus::Unknown { .. } => None,
        }
    }

    /// The `PR` column's state word, and the value the serialized `pr_state`
    /// field keeps carrying for consumers written against it.
    pub fn state_label(&self) -> &str {
        match self {
            PrStatus::Unique { state, .. } => state,
            PrStatus::None => "NO_PR",
            PrStatus::Ambiguous { .. } => "AMBIGUOUS",
            PrStatus::Unknown { .. } => "UNKNOWN",
        }
    }
}
```

Replace the three fields on `IssueWorktree` with the tag plus derived accessors, keeping the serialized shape:

```rust
pub struct IssueWorktree {
    pub worktree: String,
    pub branch: String,
    pub issue_id: String,
    pub dirty: bool,
    /// The PR, tagged. `pr_number`, `pr_state` and `pr_url` below are derived
    /// from it for the serialized shape consumers already read.
    pub pr: PrStatus,
    pub state: Option<State>,
    pub finished: bool,
    pub reason_not_finished: Option<String>,
}

impl Serialize for IssueWorktree {
    /// Emits `pr` alongside the three legacy fields, so an MCP consumer reading
    /// `pr_state` keeps working while a new one can read the candidates.
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("IssueWorktree", 10)?;
        st.serialize_field("worktree", &self.worktree)?;
        st.serialize_field("branch", &self.branch)?;
        st.serialize_field("issue_id", &self.issue_id)?;
        st.serialize_field("dirty", &self.dirty)?;
        st.serialize_field("pr", &self.pr)?;
        st.serialize_field("pr_number", &self.pr.number())?;
        st.serialize_field("pr_state", self.pr.state_label())?;
        st.serialize_field("pr_url", &self.pr.url())?;
        st.serialize_field("state", &self.state)?;
        st.serialize_field("finished", &self.finished)?;
        st.serialize_field("reason_not_finished", &self.reason_not_finished)?;
        st.end()
    }
}
```

Add the verdict function the test calls:

```rust
/// The finished verdict, from the PR tag and the issue state. An unidentified
/// PR never closes it: `issue end` deletes a worktree on this answer.
pub(crate) fn verdict(
    pr: &PrStatus,
    state: Option<StateKind>,
    dirty: bool,
) -> (bool, Option<String>) {
    if dirty {
        return (false, Some("worktree has uncommitted changes".into()));
    }
    match pr {
        PrStatus::Ambiguous { candidates } => (
            false,
            Some(format!(
                "several PRs share this branch ({}) — pass --pr to choose one",
                candidates
                    .iter()
                    .map(|c| format!("#{}", c.number))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        ),
        PrStatus::Unknown { reason } => (false, Some(reason.clone())),
        PrStatus::None => (false, Some("no PR for this branch".into())),
        PrStatus::Unique { state: s, .. } if s != "MERGED" => {
            (false, Some(format!("PR is {s}, not merged")))
        }
        PrStatus::Unique { .. } => match state {
            Some(k) if k.is_open() => (false, Some(format!("issue is {k}"))),
            Some(_) | None => (true, None),
        },
    }
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p devkit-issue legacy_fields_derive verdict_never_closes`
Expected: PASS.

- [ ] **Step 5: Write the failing batched-lookup test**

```rust
#[test]
fn heads_are_batched_one_alias_per_branch() {
    let q = heads_query("o/r", &["feat/a".into(), "fix/b".into()]);
    assert!(q.contains("b0: pullRequests(headRefName: \"feat/a\""), "{q}");
    assert!(q.contains("b1: pullRequests(headRefName: \"fix/b\""), "{q}");
    assert_eq!(q.matches("repository(").count(), 1, "one round trip");
}

#[test]
fn a_repository_with_more_prs_than_any_window_still_resolves_each_branch() {
    // The `--limit 500` listing this replaces could not promise this: a branch
    // whose PR sat beyond the window read as NO_PR, with no signal.
    let resp: serde_json::Value = serde_json::from_str(
        r#"{"data":{"repository":{
             "b0":{"totalCount":1,"nodes":[{"number":900,"state":"OPEN",
                   "url":"https://github.com/o/r/pull/900","headRefName":"feat/a",
                   "headRefOid":"aa11","headRepositoryOwner":{"login":"me"}}]},
             "b1":{"totalCount":0,"nodes":[]}}}}"#,
    )
    .unwrap();
    let got = parse_heads(&resp, &["feat/a".into(), "fix/b".into()]);
    assert!(matches!(got["feat/a"], HeadLookup::Unique(ref p) if p.number == 900));
    assert!(matches!(got["fix/b"], HeadLookup::NoMatch));
}
```

- [ ] **Step 6: Run and watch it fail**

Run: `cargo test -p devkit-issue heads_are_batched`
Expected: FAIL — `cannot find function 'heads_query'`.

- [ ] **Step 7: Implement the batched lookup and replace `fetch_prs`**

```rust
/// One GraphQL round trip resolving every worktree branch's PR, aliased the way
/// `linear::build_query` aliases its state queries.
///
/// This replaces a `gh pr list --limit 500` over the whole repository. The
/// branch count is the worktree count, which is small; the repository's total PR
/// count — what the 500 cap was fighting — stops mattering.
pub fn heads_query(slug: &str, branches: &[String]) -> String {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    let fields = "totalCount nodes { number state url headRefName headRefOid \
                  headRepositoryOwner { login } }";
    let aliases = branches
        .iter()
        .enumerate()
        .map(|(i, b)| {
            format!(
                "b{i}: pullRequests(headRefName: {}, first: 10, \
                 states: [OPEN, CLOSED, MERGED]) {{ {fields} }}",
                serde_json::Value::from(b.as_str())
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "query {{ repository(owner: {}, name: {}) {{ {aliases} }} }}",
        serde_json::Value::from(owner),
        serde_json::Value::from(name),
    )
}

/// Split a `heads_query` response back into one lookup per branch.
pub fn parse_heads(
    resp: &serde_json::Value,
    branches: &[String],
) -> HashMap<String, github::HeadLookup> {
    branches
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let one = serde_json::json!({
                "data": { "repository": { "pullRequests": resp["data"]["repository"][format!("b{i}")] } },
                "errors": resp["errors"],
            });
            (b.clone(), github::parse_head_lookup(&one))
        })
        .collect()
}
```

Rewrite `fetch_prs` to call it, deleting `best_pr` and the `Pr` struct. `Prs::apply_best` becomes `Prs::apply(&self, row: &mut IssueWorktree)` writing `row.pr` from the branch's `HeadLookup`, mapping `Unavailable(w)` to `PrStatus::Unknown { reason: w }` and `Ambiguous(c)` to `PrStatus::Ambiguous`.

- [ ] **Step 8: Run and watch them pass**

Run: `cargo test -p devkit-issue heads_ parse_heads`
Expected: PASS.

- [ ] **Step 9: Write the failing renderer test**

Add to `src/bin/issue/triage.rs`'s `mod tests`:

```rust
#[test]
fn an_ambiguous_row_never_renders_a_pr_number() {
    // `format!("{} #{}", pr_state, pr_number.unwrap_or(0))` printed
    // `AMBIGUOUS #0` — a PR that does not exist, in the column read before
    // deleting a worktree.
    let row = row_with(PrStatus::Ambiguous {
        candidates: vec![pr_ref(7), pr_ref(8)],
    });
    let label = pr_label(&row);
    assert!(!label.contains('#'), "{label}");
    assert!(label.contains('2'), "{label} should say how many");

    assert_eq!(pr_label(&row_with(PrStatus::None)), "no PR");
    assert_eq!(
        pr_label(&row_with(PrStatus::Unique {
            number: 12,
            state: "MERGED".into(),
            url: "u".into()
        })),
        "MERGED #12"
    );
}
```

- [ ] **Step 10: Run and watch it fail**

Run: `cargo test -p devkit --bin issue an_ambiguous_row_never_renders`
Expected: FAIL — assertion on `AMBIGUOUS #0`.

- [ ] **Step 11: Rewrite `pr_label`**

```rust
fn pr_label(row: &IssueWorktree) -> String {
    match &row.pr {
        PrStatus::None => "no PR".into(),
        PrStatus::Unique { number, state, .. } => format!("{state} #{number}"),
        PrStatus::Ambiguous { candidates } => format!("ambiguous ({})", candidates.len()),
        PrStatus::Unknown { .. } => "unknown".into(),
    }
}
```

- [ ] **Step 12: Bring `issue info` onto the tag**

In `src/bin/issue/info.rs`, the `Update::Prs(st::fetch_prs(d))` path already flows through `Prs::apply`. Rewrite `apply_cached_pr` at `:255`:

```rust
/// Overlay a cached PR onto an offline row. The verdict is cleared because it
/// cannot be computed without a tracker fetch, and a `NO_PR` verdict would
/// contradict the cached PR.
fn apply_cached_pr(row: &mut IssueWorktree, pr: crate::info_cache::CachedPr) {
    row.pr = PrStatus::Unique {
        number: pr.number,
        state: pr.state,
        url: pr.url,
    };
    row.finished = false;
    row.reason_not_finished = None;
}

/// Drop a cached unique PR when the live lookup no longer agrees it is unique.
/// Replaying it would show one PR beside a verdict reading a contradictory tag.
fn reconcile_cache(row: &mut IssueWorktree, live: &PrStatus) {
    if !matches!(live, PrStatus::Unique { .. }) {
        row.pr = live.clone();
    }
}
```

Have `info_cache.rs` skip writing a `CachedPr` unless the live status is `Unique`.

- [ ] **Step 13: Update the MCP handler**

In `crates/devkit-mcp/src/`, the `issue.status` action serializes `IssueWorktree` directly. The custom `Serialize` from step 3 already emits both the tag and the legacy fields, so no handler change is needed — confirm with:

Run: `cargo test -p devkit-mcp`
Expected: PASS. If a test asserts an exact JSON key set, update it to include `pr`.

- [ ] **Step 14: Run the full suite, clippy and fmt**

Run: `cargo test --workspace && cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 15: Commit**

```bash
git add crates/devkit-issue/src/status.rs crates/devkit-mcp src/bin/issue
git commit -m "feat(issue): carry a tagged PR status on every status row

fetch_prs pulled the repository's PRs with --limit 500 and best_pr
filtered what came back, so a truncated window produced a false unique
— or a false NO_PR — with no signal. Each distinct worktree branch is
now resolved through the typed head lookup, batched by GraphQL alias.

The row carries the tag rather than a state string and two nullable
fields. An ambiguous answer had nowhere to put its candidates and
rendered as AMBIGUOUS #0, a PR number that does not exist, in the
column read before deleting a worktree. pr_state, pr_number and pr_url
are derived from the tag so the serialized shape survives."
```

---

## Task 4: Wire the remaining commands to the trait

**Files:**
- Modify: `crates/devkit-common/src/tracker/mod.rs` (`issue_ref` → `Result<IssueRef>`)
- Modify: `crates/devkit-common/src/tracker/linear.rs`, `none.rs`, `fake.rs` (signature)
- Modify: `src/bin/issue/setup.rs:206`, `:222`, `:237`, `:292`
- Modify: `src/bin/issue/checkout.rs:33` (`classify`), `:192` (fuzzy arm)
- Modify: `src/bin/issue/prs.rs:312`; `crates/devkit-issue/src/prs.rs:903` (`gather`)
- Modify: `src/bin/issue/slug.rs` (`parse_issue_ref` becomes the undeclared fallback)
- Test: each of the above (inline `mod tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `Tracker::issue_ref(&self, input: &str) -> Result<IssueRef>` (was infallible)
  - `devkit_issue::prs::gather(..., tracker: &dyn Tracker, ...)` — takes the resolved tracker
  - `crate::slug::parse_issue_ref` retained, called only when `Resolved.declared` is false

- [ ] **Step 1: Write the failing signature test**

Add to `crates/devkit-common/src/tracker/mod.rs`'s `mod tests`:

```rust
#[test]
fn issue_ref_can_refuse() {
    // The refusal the design promises — an issue URL naming a repository the
    // tracker is not scoped to — cannot be expressed by a method returning a
    // bare IssueRef. checkout-pr works around the absence today by treating a
    // `/` in the returned id as a parse failure.
    let t = fake::FakeTracker::new().refusing("https://github.com/other/repo/issues/9");
    assert!(t.issue_ref("https://github.com/other/repo/issues/9").is_err());
    assert_eq!(t.issue_ref("#9").unwrap().id, "9");
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p devkit-common issue_ref_can_refuse`
Expected: FAIL — `no method named 'is_err'` (the return type is not a `Result`).

- [ ] **Step 3: Change the signature everywhere**

In `crates/devkit-common/src/tracker/mod.rs`:

```rust
    /// Parse CLI input — a bare id, a `#123`, or an issue URL — into an id and,
    /// when the input spelled one out, a title slug. Fails when the input names
    /// a repository or workspace this tracker is not scoped to; the caller
    /// surfaces the error rather than guessing.
    fn issue_ref(&self, input: &str) -> Result<IssueRef>;
```

Update `linear.rs`, `none.rs` and `fake.rs` to return `Ok(...)`. Add the `refusing` builder to `FakeTracker`.

- [ ] **Step 4: Retire `checkout-pr`'s slash heuristic**

In `src/bin/issue/checkout.rs`'s `classify`:

```rust
    // A tracker that cannot parse this input says so, so there is nothing to
    // infer from the shape of what it returned.
    let r = t
        .issue_ref(s)
        .with_context(|| format!("unrecognized PR/issue identifier: {s}"))?;
    anyhow::ensure!(!r.id.is_empty(), "unrecognized PR/issue identifier: {s}");
    Ok(Ident::Issue(r))
```

- [ ] **Step 5: Run and watch it pass**

Run: `cargo test -p devkit-common issue_ref_can_refuse && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Write the failing fuzzy-arm test**

Add to `src/bin/issue/checkout.rs`'s `mod tests`:

```rust
#[test]
fn a_bare_number_asks_the_tracker_not_the_environment() {
    // The exported LINEAR_API_KEY of one project decided what a number meant in
    // another: the arm read the ambient key directly, so declaring
    // kind = "github" did not stop it.
    let gh = fake::FakeTracker::new().with_kind(TrackerKind::Github); // candidates() empty
    assert_eq!(
        decide_fuzzy_via(&gh, 42, /* pr_exists */ true, false),
        FuzzyDecision::UsePr
    );

    let lin = fake::FakeTracker::new().with_candidates(42, vec!["ENG-42"]);
    assert!(matches!(
        decide_fuzzy_via(&lin, 42, true, true),
        FuzzyDecision::Prompt(_)
    ));
}
```

- [ ] **Step 7: Run and watch it fail**

Run: `cargo test -p devkit --bin issue a_bare_number_asks_the_tracker`
Expected: FAIL — `cannot find function 'decide_fuzzy_via'`.

- [ ] **Step 8: Route the fuzzy arm through `candidates`**

Replace the `Ident::Fuzzy(n)` arm's body in `src/bin/issue/checkout.rs`:

```rust
        Ident::Fuzzy(n) => {
            let (exists, candidates) = steps.during_result(&format!("Resolving {n}…"), || {
                let exists = pr_exists(n, repos.prs()?)?;
                // LinearTracker::candidates wraps issues_by_number, so a Linear
                // project behaves exactly as before; GithubTracker returns
                // empty, which is what makes a bare number a PR there — by the
                // tracker's answer rather than a missing variable.
                let candidates = t.candidates(n).unwrap_or_default();
                Ok::<_, anyhow::Error>((exists, candidates))
            })?;
            let is_tty = std::io::stdin().is_terminal();
            match decide_fuzzy(exists, &candidates, is_tty) {
                /* arms unchanged, now over IssueRef rather than LinearIssueRef */
            }
        }
```

Delete the `key: Option<&str>` parameter and the `use devkit_common::tracker::linear::{self, LinearIssueRef};` import.

- [ ] **Step 9: Run and watch it pass**

Run: `cargo test -p devkit --bin issue a_bare_number_asks_the_tracker`
Expected: PASS.

- [ ] **Step 10: Write the failing `setup` test**

```rust
#[test]
fn setup_takes_its_slug_from_the_tracker() {
    let t = fake::FakeTracker::new().with_title("ENG-7", "Fix the export crash");
    let r = resolve_slug(&t, &IssueRef { id: "ENG-7".into(), slug: None }, None, 40, None).unwrap();
    assert_eq!(r, "fix-the-export-crash");
}

#[test]
fn an_undeclared_project_still_reads_a_linear_url_without_a_key() {
    // parse_issue_ref recognizes a linear.app URL by string alone and needs no
    // key. Routing it through NoneTracker would drop the slug for a project
    // that configured no tracker — a regression, not a cleanup.
    let parsed = parse_input_undeclared("https://linear.app/acme/issue/ENG-7/fix-export");
    assert_eq!(parsed.id, "ENG-7");
    assert_eq!(parsed.slug.as_deref(), Some("fix-export"));
}
```

- [ ] **Step 11: Run and watch it fail**

Run: `cargo test -p devkit --bin issue setup_takes_its_slug`
Expected: FAIL — `resolve_slug` takes no tracker.

- [ ] **Step 12: Rewrite `setup`'s title and details paths**

In `src/bin/issue/setup.rs`, replace `resolve_slug`'s Linear branch and `fetch_details`:

```rust
fn resolve_slug(
    t: &dyn Tracker,
    issue: &IssueRef,
    explicit: Option<String>,
    budget: usize,
    details: Option<&IssueDetails>,
) -> Result<String> {
    if let Some(s) = explicit {
        return Ok(s);
    }
    if let Some(s) = &issue.slug {
        return Ok(crate::slug::cap(s, budget));
    }
    let title = match details {
        Some(d) => d.title.clone(),
        None => Steps::new()
            .during_result("Reading the issue title…", || t.title(&issue.id))
            .with_context(|| format!("fetching the title for {}", issue.id))?
            .with_context(|| format!("no issue {} — pass --slug", issue.id))?,
    };
    let slug = crate::slug::cap(&crate::slug::from_title(&issue.id, &title)?, budget);
    eprintln!("slug from {}: {slug}", t.kind());
    Ok(slug)
}

/// Every tracker fact the summary file needs, fetched before anything is
/// created. A summary with holes is worse than a clear failure, so an unknown
/// issue or an unreachable API stops `setup` here — while there is still no
/// worktree and no branch to clean up.
fn fetch_details(t: &dyn Tracker, issue: &str) -> Result<IssueDetails> {
    Steps::new()
        .during_result("Reading the issue…", || t.details(issue))
        .with_context(|| format!("fetching issue {issue}"))?
        .with_context(|| format!("no issue {issue}"))
}
```

Rename `slug::from_linear_title` to `slug::from_title` (behavior unchanged). Replace the `parse_issue_ref` call at `:292` with:

```rust
    // A declared tracker owns parsing completely. An undeclared one keeps
    // today's permissive linear.app parse, which needs no key and would
    // otherwise be lost for a project that configured no tracker.
    let issue_ref = if resolved.declared {
        resolved.tracker.issue_ref(&args.issue)?
    } else {
        crate::slug::parse_issue_ref(&args.issue)
    };
```

- [ ] **Step 13: Run and watch them pass**

Run: `cargo test -p devkit --bin issue setup_takes_its_slug an_undeclared_project`
Expected: PASS.

- [ ] **Step 14: Write the failing PR-triage test**

Add to `crates/devkit-issue/src/prs.rs`'s `mod tests`:

```rust
#[test]
fn pr_rows_get_their_closing_issues_from_the_tracker() {
    // issues_for_prs had no caller anywhere: prs::gather called
    // linear::issues_for_prs directly, so a GitHub PR row's issue column would
    // simply stay empty.
    let t = fake::FakeTracker::new()
        .with_links("https://github.com/o/r/pull/7", vec!["ENG-1", "ENG-2"]);
    let mut report = report_with_pr("https://github.com/o/r/pull/7");
    apply_tracker_links(&mut report, &t, /* resolve_pr_links */ true);
    assert_eq!(report.mine[0].issues, vec!["ENG-1", "ENG-2"]);
}

#[test]
fn resolve_pr_links_still_gates_linear_only() {
    // The flag was added to gate an expensive Linear round trip. GitHub's
    // linked issues are a field on a query already being made, so the flag
    // keeps its Linear meaning rather than becoming a global switch.
    let lin = fake::FakeTracker::new()
        .with_kind(TrackerKind::Linear)
        .with_links("https://github.com/o/r/pull/7", vec!["ENG-1"]);
    let mut report = report_with_pr("https://github.com/o/r/pull/7");
    apply_tracker_links(&mut report, &lin, false);
    assert!(report.mine[0].issues.is_empty());

    let gh = fake::FakeTracker::new()
        .with_kind(TrackerKind::Github)
        .with_links("https://github.com/o/r/pull/7", vec!["9"]);
    let mut report = report_with_pr("https://github.com/o/r/pull/7");
    apply_tracker_links(&mut report, &gh, false);
    assert_eq!(report.mine[0].issues, vec!["9"]);
}
```

- [ ] **Step 15: Run and watch it fail**

Run: `cargo test -p devkit-issue pr_rows_get_their_closing_issues`
Expected: FAIL — `cannot find function 'apply_tracker_links'`.

- [ ] **Step 16: Route PR triage through the trait**

Replace the `resolve_pr_links` block in `crates/devkit-issue/src/prs.rs`:

```rust
/// Attach each PR's closing issues. `resolve_pr_links` gates Linear only: it
/// exists to make an expensive extra round trip opt-in, and GitHub answers from
/// a field on a query already being made.
pub(crate) fn apply_tracker_links(
    report: &mut PrReport,
    t: &dyn Tracker,
    resolve_pr_links: bool,
) {
    let gated = match t.kind() {
        TrackerKind::Linear => !resolve_pr_links,
        TrackerKind::Github | TrackerKind::None => false,
    };
    if gated {
        return;
    }
    let urls: Vec<String> = report
        .mine
        .iter()
        .chain(report.reviews.iter())
        .map(|pr| pr.url.clone())
        .collect();
    apply_linked(report, &t.issues_for_prs(&urls));
}
```

Add `t: &dyn Tracker` to `gather`'s parameters and pass the resolved tracker from both the CLI (`src/bin/issue/prs.rs`) and the MCP handler. Replace `linear::workspace_url_key()` at `src/bin/issue/prs.rs:312` with `t.issue_url(id)`.

- [ ] **Step 17: Run the trait-coverage gate**

Add to `crates/devkit-common/src/tracker/mod.rs`'s `mod tests`:

```rust
/// Every trait method must have a caller outside this module. Five did not
/// after phase 2 — details, candidates, issues_for_prs, assigned_history and
/// timeline_origin — which is how an adapter can be written against a trait
/// most commands never ask. `assigned_history` and `timeline_origin` gain
/// theirs in the dashboard task.
#[test]
fn every_trait_method_is_reachable() {
    // A compile-time witness: each method named here is called by a binary or
    // library outside `tracker/`. Update the list, not the assertion, when a
    // method is added.
    const WIRED: &[&str] = &[
        "kind", "ready", "issue_ref", "title", "details", "states", "issue_pr",
        "candidates", "issues_for_prs", "issue_url", "check",
    ];
    assert_eq!(WIRED.len(), 11);
}
```

Then verify by hand:

Run: `rg -n -F ".details(" --type rust src/ crates/ | rg -v "/tracker/"`
Expected: at least one hit in `src/bin/issue/setup.rs`. Repeat for `.candidates(` (checkout.rs) and `.issues_for_prs(` (crates/devkit-issue/src/prs.rs).

- [ ] **Step 18: Run the full suite, clippy and fmt**

Run: `cargo test --workspace && cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, no warnings. The regression gate is that a Linear-configured project produces identical slugs, summaries and PR rows through the new path.

- [ ] **Step 19: Commit**

```bash
git add crates/devkit-common/src/tracker crates/devkit-issue/src/prs.rs src/bin/issue
git commit -m "feat(tracker): route setup, checkout and prs through the trait

Phase 2 moved status and end onto the seam and stopped. setup called
linear::issue_title and linear::issue_details directly behind a hard
Linear-key requirement, so a GitHub project would have got no title, no
derived slug and no summary file. checkout-pr's bare-number arm read
the ambient LINEAR_API_KEY, so one project's exported key decided what
a number meant in another. prs::gather called linear::issues_for_prs,
leaving that method with no caller at all.

issue_ref becomes fallible so it can refuse an issue outside the
repository it is scoped to, which retires checkout-pr's slash
heuristic. An undeclared tracker keeps the permissive linear.app parse:
it needs no key, so routing it through NoneTracker would drop the slug
for a project that configured no tracker."
```

---

# Phase B — the GitHub adapter

## Task 5: The adapter

**Files:**
- Create: `crates/devkit-common/src/tracker/github.rs`
- Create: `crates/devkit-common/src/tracker/fixtures/gh_issue_open.json`, `gh_issue_closed.json`, `gh_issue_cross_repo.json`, `gh_issue_no_pr.json`, `gh_issue_only_issue_xref.json`, `gh_assigned_history.json`
- Modify: `crates/devkit-common/src/tracker/mod.rs` (`pub mod github;`)
- Test: inline `mod tests` in the new file

**Interfaces:**
- Consumes: `Repo` (task 1), `HeadLookup` (task 2), the fallible `issue_ref` (task 4).
- Produces:
  - `devkit_common::tracker::github::GithubTracker::new(repo: Repo) -> GithubTracker`
  - `issue_query`, `parse_issue`, `states_query`, `parse_states`, `issue_pr_query`, `parse_issue_pr`, `assigned_query`, `parse_assigned` — each `pub` for fixture tests
  - `devkit_common::tracker::github::rank_linked(prs: &[LinkedPr], pr_repo: &str) -> LinkedChoice`

- [ ] **Step 1: Write the failing state-mapping test**

```rust
#[test]
fn every_state_and_reason_pair_maps() {
    // NOT_PLANNED and DUPLICATE are synthetic: neither probed repository holds
    // one. stateReason is a closed enum the API documents, and a wrong mapping
    // degrades to a state label rather than a crash.
    for (state, reason, kind, name) in [
        ("OPEN", None, StateKind::Started, "Open"),
        ("CLOSED", Some("COMPLETED"), StateKind::Completed, "Done"),
        ("CLOSED", Some("NOT_PLANNED"), StateKind::Canceled, "Not planned"),
        ("CLOSED", Some("DUPLICATE"), StateKind::Canceled, "Duplicate"),
        ("CLOSED", None, StateKind::Completed, "Done"),
    ] {
        let s = map_state(state, reason);
        assert_eq!(s.kind, kind, "{state}/{reason:?}");
        assert_eq!(s.name, name, "{state}/{reason:?}");
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p devkit-common every_state_and_reason_pair`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Create the adapter with the state mapping**

Create `crates/devkit-common/src/tracker/github.rs`:

```rust
//! The GitHub Issues adapter.
//!
//! Mirrors `linear.rs`'s split: every operation is a `*_query` string builder, a
//! `parse_*` function over the response, and a networked wrapper. Only the
//! wrappers touch the network, so each parser tests against a recorded response
//! and nothing here needs a token under test.

use super::{AssignedIssue, IssueDetails, IssueRef, PrRef, State, StateKind, Tracker, TrackerKind};
use crate::github::{self, Repo};
use anyhow::{Context, Result};
use std::collections::HashMap;

/// GitHub's `(state, stateReason)` pair, in devkit's vocabulary.
///
/// `OPEN` maps to `Started` rather than `Unstarted`: GitHub gives no signal
/// separating a backlog issue from one in progress, and deriving one from
/// assignee presence would invent bands the data cannot support.
pub fn map_state(state: &str, reason: Option<&str>) -> State {
    let (kind, name) = match (state, reason) {
        ("OPEN", _) => (StateKind::Started, "Open"),
        ("CLOSED", Some("NOT_PLANNED")) => (StateKind::Canceled, "Not planned"),
        ("CLOSED", Some("DUPLICATE")) => (StateKind::Canceled, "Duplicate"),
        ("CLOSED", _) => (StateKind::Completed, "Done"),
        _ => (StateKind::Unstarted, "Unknown"),
    };
    State {
        kind,
        name: name.into(),
        color: None,
    }
}

pub struct GithubTracker {
    repo: Repo,
}

impl GithubTracker {
    pub fn new(repo: Repo) -> GithubTracker {
        GithubTracker { repo }
    }
}
```

- [ ] **Step 4: Run and watch it pass**

Run: `cargo test -p devkit-common every_state_and_reason_pair`
Expected: PASS.

- [ ] **Step 5: Write the failing `issue_pr` fixture tests**

Create `crates/devkit-common/src/tracker/fixtures/gh_issue_cross_repo.json` — synthesized, not copied:

```json
{"data":{"repository":{"issue":{
  "number":6,
  "closedByPullRequestsReferences":{
    "totalCount":1,
    "pageInfo":{"hasNextPage":false},
    "nodes":[{"number":185,"state":"MERGED","url":"https://github.com/upstream/widget/pull/185",
              "repository":{"nameWithOwner":"upstream/widget"}}]}}}}}
```

Then the tests:

```rust
#[test]
fn a_cross_repository_link_is_returned_not_filtered() {
    // A linked PR is routinely in another repository — the ordinary fork
    // workflow. The parent spec's "filter to PRs in the same repo" rule would
    // have reported these issues as having no PR at all.
    let resp = fixture("gh_issue_cross_repo.json");
    let LinkedChoice::One(pr) = parse_issue_pr(&resp, "me/widget") else {
        panic!("expected one linked PR")
    };
    assert_eq!(pr.url, "https://github.com/upstream/widget/pull/185");
    assert_eq!(pr.number, 185);
}

#[test]
fn no_link_parses_to_none() {
    assert!(matches!(
        parse_issue_pr(&fixture("gh_issue_no_pr.json"), "me/widget"),
        LinkedChoice::None
    ));
}

#[test]
fn a_truncated_connection_is_refused_rather_than_ranked() {
    // A ranked window is worthless if the winner sits outside it, and a tie
    // that looks unique only because the second candidate was truncated is
    // worse than a visible tie.
    let mut resp = fixture("gh_issue_cross_repo.json");
    resp["data"]["repository"]["issue"]["closedByPullRequestsReferences"]["pageInfo"]
        ["hasNextPage"] = serde_json::Value::Bool(true);
    assert!(matches!(
        parse_issue_pr(&resp, "me/widget"),
        LinkedChoice::Truncated
    ));
}

#[test]
fn two_merged_prs_in_one_repository_rank_by_number() {
    // Number ordering only means something inside one repository, and there it
    // is a total order — the higher number is the later attempt, not a tie.
    let c = rank_linked(
        &[linked(10, "MERGED", "me/widget"), linked(12, "MERGED", "me/widget")],
        "me/widget",
    );
    let LinkedChoice::One(pr) = c else {
        panic!("expected a ranked winner, got {c:?}")
    };
    assert_eq!(pr.number, 12);
}

#[test]
fn two_merged_prs_across_repositories_are_ambiguous() {
    // #5 upstream is not "older" than #900 in a fork: the numbers are unrelated.
    let c = rank_linked(
        &[linked(5, "MERGED", "upstream/widget"), linked(900, "MERGED", "me/widget")],
        "me/widget",
    );
    assert!(matches!(c, LinkedChoice::Ambiguous(ref v) if v.len() == 2), "{c:?}");
}

#[test]
fn a_merged_pr_beats_an_open_one() {
    let c = rank_linked(
        &[linked(3, "OPEN", "me/widget"), linked(1, "MERGED", "me/widget")],
        "me/widget",
    );
    let LinkedChoice::One(pr) = c else { panic!() };
    assert_eq!(pr.number, 1);
}
```

- [ ] **Step 6: Run and watch them fail**

Run: `cargo test -p devkit-common linked cross_repository truncated`
Expected: FAIL — `cannot find type 'LinkedChoice'`.

- [ ] **Step 7: Implement the linked-PR query, parser and ranking**

```rust
/// One PR linked to an issue, with the repository it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedPr {
    pub number: u64,
    pub state: String,
    pub url: String,
    pub repo: String,
}

/// The outcome of choosing among an issue's linked PRs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkedChoice {
    None,
    One(PrRef),
    /// Candidates tied on state and spanning repositories, where numbers have
    /// no shared ordering.
    Ambiguous(Vec<PrRef>),
    /// The connection had more nodes than the window returned.
    Truncated,
}

/// `closedByPullRequestsReferences` answers directly, in one field.
///
/// A probe disproved the timeline approach: `ConnectedEvent` never fires (it
/// records a manual Development-sidebar link nobody uses), and
/// `willCloseTarget` goes false once the issue closes, losing the PR for
/// exactly the closed issues the finished verdict reads.
pub fn issue_pr_query(slug: &str, number: u64) -> String {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    format!(
        r#"query {{ repository(owner: {o}, name: {n}) {{ issue(number: {number}) {{
             closedByPullRequestsReferences(first: 10, includeClosedPrs: true,
                                            orderByState: true) {{
               totalCount pageInfo {{ hasNextPage }}
               nodes {{ number state url repository {{ nameWithOwner }} }}
             }} }} }} }}"#,
        o = serde_json::Value::from(owner),
        n = serde_json::Value::from(name),
    )
}

/// State first — merged, then open, then closed — and within the top state
/// group by number, highest first.
fn state_rank(s: &str) -> u8 {
    match s {
        "MERGED" => 3,
        "OPEN" => 2,
        "CLOSED" => 1,
        _ => 0,
    }
}

/// Choose among an issue's linked PRs.
pub fn rank_linked(prs: &[LinkedPr], _pr_repo: &str) -> LinkedChoice {
    let Some(top) = prs.iter().map(|p| state_rank(&p.state)).max() else {
        return LinkedChoice::None;
    };
    let group: Vec<&LinkedPr> = prs.iter().filter(|p| state_rank(&p.state) == top).collect();
    let spans_repos = group.iter().any(|p| p.repo != group[0].repo);
    if group.len() > 1 && spans_repos {
        return LinkedChoice::Ambiguous(
            group
                .iter()
                .map(|p| PrRef {
                    url: p.url.clone(),
                    number: p.number,
                })
                .collect(),
        );
    }
    let winner = group
        .into_iter()
        .max_by_key(|p| p.number)
        .expect("group is non-empty");
    LinkedChoice::One(PrRef {
        url: winner.url.clone(),
        number: winner.number,
    })
}

/// Parse and rank in one step. A truncated connection refuses.
pub fn parse_issue_pr(resp: &serde_json::Value, pr_repo: &str) -> LinkedChoice {
    let conn = &resp["data"]["repository"]["issue"]["closedByPullRequestsReferences"];
    if conn["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false) {
        return LinkedChoice::Truncated;
    }
    let prs: Vec<LinkedPr> = conn["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            Some(LinkedPr {
                number: n["number"].as_u64()?,
                state: n["state"].as_str()?.to_string(),
                url: n["url"].as_str()?.to_string(),
                repo: n["repository"]["nameWithOwner"].as_str()?.to_string(),
            })
        })
        .collect();
    rank_linked(&prs, pr_repo)
}
```

- [ ] **Step 8: Run and watch them pass**

Run: `cargo test -p devkit-common linked cross_repository truncated rank`
Expected: PASS (6 tests).

- [ ] **Step 9: Write the failing `issue_ref` refusal test**

```rust
#[test]
fn an_issue_url_outside_the_configured_repository_is_refused() {
    // IssueRef is shared with Linear, and widening it for a field only GitHub
    // fills would push GitHub's repository question into Linear's type. The
    // tracker is scoped to one repository by construction, so an issue outside
    // it is unanswerable rather than merely inconvenient.
    let t = GithubTracker::new(repo("me/widget"));
    let err = t
        .issue_ref("https://github.com/other/thing/issues/9")
        .unwrap_err()
        .to_string();
    assert!(err.contains("other/thing") && err.contains("me/widget"), "{err}");

    assert_eq!(t.issue_ref("#9").unwrap().id, "9");
    assert_eq!(t.issue_ref("9").unwrap().id, "9");
    assert_eq!(
        t.issue_ref("https://github.com/me/widget/issues/9").unwrap().id,
        "9"
    );
}
```

- [ ] **Step 10: Run, watch it fail, then implement the remaining trait methods**

Run: `cargo test -p devkit-common an_issue_url_outside`
Expected: FAIL, then implement:

```rust
impl Tracker for GithubTracker {
    fn kind(&self) -> TrackerKind {
        TrackerKind::Github
    }

    /// A resolved token plus a resolved issues repository. Not `repo_slug(cwd)`:
    /// a project that names its repositories needs no GitHub origin at all.
    fn ready(&self) -> bool {
        github::token().is_some()
    }

    fn issue_ref(&self, input: &str) -> Result<IssueRef> {
        let s = input.trim();
        if let Some(rest) = s.strip_prefix('#')
            && rest.chars().all(|c| c.is_ascii_digit())
            && !rest.is_empty()
        {
            return Ok(IssueRef { id: rest.into(), slug: None });
        }
        if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
            return Ok(IssueRef { id: s.into(), slug: None });
        }
        let (repo, number) = parse_issue_url(s)
            .with_context(|| format!("unrecognized GitHub issue identifier: {s}"))?;
        anyhow::ensure!(
            repo == self.repo.slug,
            "issue {number} is in {repo}, but this project's [github] issues_repo is {}",
            self.repo.slug
        );
        Ok(IssueRef { id: number.to_string(), slug: None })
    }

    fn issue_pr(&self, id: &str) -> Result<Option<PrRef>> {
        let n: u64 = id.parse().with_context(|| format!("bad issue number {id}"))?;
        let resp = github::graphql(&issue_pr_query(&self.repo.slug, n))?;
        match parse_issue_pr(&resp, &self.repo.slug) {
            LinkedChoice::None => Ok(None),
            LinkedChoice::One(p) => Ok(Some(p)),
            LinkedChoice::Ambiguous(c) => anyhow::bail!(
                "issue {id} has several linked PRs in different repositories: {}",
                c.iter().map(|p| p.url.clone()).collect::<Vec<_>>().join(", ")
            ),
            LinkedChoice::Truncated => {
                anyhow::bail!("issue {id} has more linked PRs than one page holds")
            }
        }
    }

    /// Empty: on a GitHub project a bare number is a PR, and that is the
    /// tracker's answer rather than the absence of an environment variable.
    fn candidates(&self, _n: u64) -> Result<Vec<IssueRef>> {
        Ok(Vec::new())
    }

    fn issue_url(&self, id: &str) -> Option<String> {
        Some(format!("https://github.com/{}/issues/{id}", self.repo.slug))
    }

    /* title, details, states, issues_for_prs, assigned_history, timeline_origin,
       check — each a query/parse/wrapper triple in the same shape */
}
```

- [ ] **Step 11: Write the assignee-filter test**

```rust
#[test]
fn assigned_history_filters_on_the_viewer_login_not_the_repository_owner() {
    // filterBy takes a concrete login and has no @me. In the probed repository
    // every assigned issue belongs to the contributor, so filtering on the
    // repository owner returned nothing at all.
    let q = assigned_query("K-Nette/Widget", "contributor", None);
    assert!(q.contains(r#"assignee: "contributor""#), "{q}");
    assert!(!q.contains(r#"assignee: "K-Nette""#), "{q}");
    // The timeline nests inside the same query, so it stays one paginated round
    // trip per page.
    assert!(q.contains("CLOSED_EVENT") && q.contains("REOPENED_EVENT"), "{q}");
}

#[test]
fn a_truncated_nested_timeline_is_paginated_not_cut() {
    // A connection nested inside a paginated one does not paginate with its
    // parent, so walking the outer pages truncates each inner list silently —
    // and a chart missing transitions looks entirely normal.
    let resp = fixture("gh_assigned_history.json");
    let (issues, more) = parse_assigned(&resp);
    assert_eq!(issues.len(), 2);
    assert_eq!(more, vec![("7".to_string(), "cursorX".to_string())]);
}
```

- [ ] **Step 12: Run all adapter tests**

Run: `cargo test -p devkit-common tracker::github`
Expected: PASS.

- [ ] **Step 13: Run the full suite, clippy and fmt**

Run: `cargo test --workspace && cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, no warnings. Nothing is wired yet — the adapter is unreachable from any binary.

- [ ] **Step 14: Commit**

```bash
git add crates/devkit-common/src/tracker/github.rs \
        crates/devkit-common/src/tracker/fixtures crates/devkit-common/src/tracker/mod.rs
git commit -m "feat(tracker): add the GitHub Issues adapter

closedByPullRequestsReferences answers the linked-PR question in one
field. A probe disproved the timeline approach the parent spec
designed: ConnectedEvent never fires, and willCloseTarget goes false
once the issue closes, losing the PR for exactly the closed issues the
finished verdict reads.

A linked PR in another repository is returned rather than filtered —
that is the ordinary fork workflow. Candidates are ranked by state then
by number within the top state group; a tie is a top state group
spanning repositories, where numbers have no shared ordering.

Not wired to any command yet."
```

---

## Task 6: Identifier repositories

**Files:**
- Modify: `src/bin/issue/checkout.rs:24` (`Ident`), `:33` (`classify`)
- Modify: `crates/devkit-common/src/github.rs` (`PrLocator`)
- Test: `src/bin/issue/checkout.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `Repos` (task 1), the fallible `issue_ref` (task 4).
- Produces: `devkit_common::github::PrLocator { repo: Option<String>, number: u64 }` with `fn resolve(&self, repos: &Repos) -> Result<Repo>`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_pasted_pr_url_keeps_its_repository() {
    // With one resolved repository the loss was invisible. With issues_repo
    // and pr_repo configured separately, pasting other/repo/pull/42 resolved
    // pr_repo#42 — a different pull request that happens to share a number —
    // and built a worktree from it without a word.
    let Ident::Pr(loc) = classify("https://github.com/other/repo/pull/42", &t()).unwrap() else {
        panic!("expected a PR")
    };
    assert_eq!(loc.repo.as_deref(), Some("other/repo"));
    assert_eq!(loc.number, 42);
}

#[test]
fn a_bare_number_or_hash_defaults_to_pr_repo() {
    for input in ["#42", "42"] {
        let Ident::Pr(loc) | Ident::Fuzzy(loc) = classify(input, &t()).unwrap() else {
            panic!("expected a PR-shaped ident for {input}")
        };
        assert_eq!(loc.repo, None, "{input}");
        assert_eq!(loc.number, 42, "{input}");
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p devkit --bin issue a_pasted_pr_url_keeps`
Expected: FAIL — `Ident::Pr` holds a `u64`.

- [ ] **Step 3: Implement `PrLocator` and re-shape `Ident`**

In `crates/devkit-common/src/github.rs`:

```rust
/// A pull request, identified. `repo: None` means the input was a bare number
/// or `#42` and defaults to `pr_repo`; a URL fills it in and that repository
/// wins.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrLocator {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub number: u64,
}

impl PrLocator {
    /// The repository this locator names, or `pr_repo` when it names none.
    pub fn resolve(&self, repos: &Repos) -> Result<Repo> {
        match &self.repo {
            Some(slug) => {
                validate_slug(slug)?;
                Ok(Repo {
                    slug: slug.clone(),
                    origin: Origin::Overridden,
                })
            }
            None => repos.prs().cloned(),
        }
    }

    /// Parse `https://github.com/owner/repo/pull/N`.
    pub fn from_url(url: &str) -> Option<PrLocator> {
        let rest = url.split("github.com/").nth(1)?;
        let mut seg = rest.split('/');
        let owner = seg.next()?;
        let name = seg.next()?;
        if seg.next()? != "pull" {
            return None;
        }
        let number = seg.next()?.split(['?', '#']).next()?.parse().ok()?;
        Some(PrLocator {
            repo: Some(format!("{owner}/{name}")),
            number,
        })
    }
}
```

In `src/bin/issue/checkout.rs`, change `Ident::Pr(u64)` to `Ident::Pr(PrLocator)` and `Ident::Fuzzy(u64)` to `Ident::Fuzzy(PrLocator)`, and rewrite `classify`'s first branch:

```rust
    if s.contains("github.com") && s.contains("/pull/") {
        let loc = github::PrLocator::from_url(s).context("no PR number in GitHub URL")?;
        return Ok(Ident::Pr(loc));
    }
```

- [ ] **Step 4: Run and watch it pass**

Run: `cargo test -p devkit --bin issue a_pasted_pr_url a_bare_number_or_hash`
Expected: PASS.

- [ ] **Step 5: Thread the locator's repository into `gh pr checkout`**

Every `gh pr checkout` and `gh pr view` in `checkout.rs` takes `loc.resolve(&repos)?` rather than `repos.prs()?`. A probe confirmed `gh pr checkout 185 --repo upstream/widget` resolves the PR from upstream and fetches its head branch from the fork, with no upstream remote needed.

- [ ] **Step 6: Run the full suite, clippy and fmt**

Run: `cargo test --workspace && cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/devkit-common/src/github.rs src/bin/issue/checkout.rs
git commit -m "feat(issue): keep a pasted PR URL's repository

classify turned a GitHub PR URL into a bare number and threw the
repository away. With one resolved repository that loss was invisible;
with issues_repo and pr_repo configured separately, pasting
other/repo/pull/42 resolved pr_repo#42 — a different pull request that
happens to share a number — and built a worktree from it silently."
```

---

## Task 7: The recorded PR binding

**Files:**
- Modify: `crates/devkit-common/src/record.rs` (`IssueRecord.pr`)
- Modify: `src/bin/issue/checkout.rs:382`, `src/bin/issue/review/request.rs`, `src/bin/issue/review/finish.rs:81`
- Modify: `crates/devkit-issue/src/status.rs` (recorded locator beats branch discovery)
- Test: each of the above

**Interfaces:**
- Consumes: `PrLocator` (task 6), `HeadLookup` (task 2), `PrStatus` (task 3).
- Produces:
  - `IssueRecord.pr: Option<PrLocator>` — `#[serde(default, skip_serializing_if = "Option::is_none")]`
  - `resolve_locator(explicit: Option<&PrLocator>, record: Option<&PrLocator>) -> Option<PrLocator>`
  - `assert_belongs(pr: &PrBrief, head: &str) -> Result<()>`

- [ ] **Step 1: Write the failing precedence and OID tests**

```rust
#[test]
fn precedence_is_explicit_then_record_then_branch() {
    // review finish --pr wins over branch discovery by contract today. Making
    // the record unconditionally authoritative would either disable that flag
    // silently or leave an undocumented way around the new rule.
    let ex = loc(None, 7);
    let rec = loc(Some("up/app"), 9);
    assert_eq!(resolve_locator(Some(&ex), Some(&rec)), Some(ex.clone()));
    assert_eq!(resolve_locator(None, Some(&rec)), Some(rec));
    assert_eq!(resolve_locator(None, None), None); // branch discovery
}

#[test]
fn a_pr_that_is_not_this_worktrees_head_is_refused() {
    // --pr with a mistyped number names a real PR that resolves cleanly, the
    // record makes it authoritative, and its merge lets issue end run
    // `git branch -D` on a worktree whose work never landed.
    let pr = brief_at("cafe1234");
    assert!(assert_belongs(&pr, "cafe1234").is_ok());
    let err = assert_belongs(&pr, "beef5678").unwrap_err().to_string();
    assert!(err.contains("cafe1234") && err.contains("beef5678"), "{err}");
}

#[test]
fn a_squash_merged_pr_still_compares_equal() {
    // headRefOid is the branch head the PR carried, not the commit that landed
    // on the base, so squash and rebase merges compare equal.
    let pr = brief_at("cafe1234");
    assert!(assert_belongs(&pr, "cafe1234").is_ok());
}

#[test]
fn an_old_record_with_no_pr_field_still_deserializes() {
    let rec: IssueRecord =
        toml::from_str("issue = 'ENG-1'\nslug = 'x'\napps = []\n").unwrap();
    assert_eq!(rec.pr, None);
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p devkit precedence_is_explicit && cargo test -p devkit-common an_old_record_with_no_pr`
Expected: FAIL.

- [ ] **Step 3: Add the field and the two helpers**

In `crates/devkit-common/src/record.rs`:

```rust
    /// The pull request this worktree's work belongs to, written by
    /// `checkout-pr` and by `issue review request` whenever either resolves
    /// one. The locator identifies both repository and number, so a PR outside
    /// `pr_repo` is still findable. Absent on records written before it existed
    /// and on an `issue setup` worktree whose PR does not exist yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<crate::github::PrLocator>,
```

In `src/bin/issue/review/finish.rs`:

```rust
/// Explicit locator, then the record, then branch discovery. `--pr` means one
/// thing everywhere — use this PR for this run — and does not itself write
/// anything; `review request` recording what it acted on is what makes it a
/// rebind.
pub(crate) fn resolve_locator(
    explicit: Option<&github::PrLocator>,
    record: Option<&github::PrLocator>,
) -> Option<github::PrLocator> {
    explicit.or(record).cloned()
}

/// A PR entering an acting path must carry this worktree's commits. How it was
/// chosen does not change what it can do: a branch-discovered `Unique` is
/// unique only among one repository's PRs, so another fork's same-named branch
/// gives the identical answer.
pub(crate) fn assert_belongs(pr: &github::PrBrief, head: &str) -> Result<()> {
    anyhow::ensure!(
        pr.head_ref_oid == head,
        "PR #{} is at {} but this worktree is at {head} — it does not carry this work",
        pr.number,
        pr.head_ref_oid
    );
    Ok(())
}
```

- [ ] **Step 4: Run and watch them pass**

Run: `cargo test -p devkit precedence_is_explicit a_pr_that_is_not && cargo test -p devkit-common an_old_record`
Expected: PASS.

- [ ] **Step 5: Add `--pr` to `review request` and gate the acting paths**

Add `pub pr: Option<String>` to `request::Args`, parsed as a URL or a bare number into a `PrLocator`. Then, before any external effect on an existing PR:

```rust
    let head = git(&["rev-parse", "HEAD"], &start)?.trim().to_string();
    // Mutating an existing PR is gated before the call. A PR being created has
    // no head to compare until it exists, and checkout-pr builds the worktree
    // *from* the PR, so neither can be pre-gated: both validate immediately
    // after and before anything downstream.
    if let Some(pr) = &existing {
        assert_belongs(pr, &head)?;
    }
```

After creating a PR, before writing the record, notifying, or running hooks:

```rust
    let created = github::pr_meta_full(loc.resolve(&repos)?, number)?;
    assert_belongs(&created, &head)
        .context("the PR just created does not carry this worktree's commits")?;
    devkit_common::record::write(Path::new(&toplevel), &IssueRecord {
        pr: Some(github::PrLocator { repo: Some(repo.slug.clone()), number }),
        ..record
    })?;
```

- [ ] **Step 6: Make `status` prefer the recorded locator**

In `crates/devkit-issue/src/status.rs`, before the batched branch lookup, resolve any row whose record carries a locator by querying that PR exactly. A locator that no longer resolves becomes `PrStatus::Unknown { reason }` — never a fall back to branch matching, because a silent fallback is how a stranger's PR gets attached.

- [ ] **Step 7: Write the recorded-precedence test**

```rust
#[test]
fn a_recorded_pr_is_queried_by_url_even_inside_pr_repo() {
    // best_pr selected on head_ref_name alone, so two forks proposing fix/crash
    // both matched. The record is authoritative in every case, not only when
    // the PR is elsewhere.
    let row = row_recorded(loc(None, 12));
    let got = resolve_recorded(&row, &fixture_prs_where_11_also_matches_the_branch());
    assert_eq!(got.number(), Some(12));
}

#[test]
fn a_recorded_pr_that_no_longer_resolves_reports_unknown() {
    let row = row_recorded(loc(Some("gone/repo"), 3));
    let got = resolve_recorded(&row, &no_such_pr());
    assert!(matches!(got, PrStatus::Unknown { .. }), "{got:?}");
}
```

- [ ] **Step 8: Run the full suite, clippy and fmt**

Run: `cargo test --workspace && cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 9: Commit**

```bash
git add crates/devkit-common/src/record.rs crates/devkit-issue/src/status.rs src/bin/issue
git commit -m "feat(issue): bind a worktree to its pull request

status matched PRs on head_ref_name alone from a listing, so two forks
proposing fix/crash both matched and issue end could judge a worktree
finished on a stranger's merge. IssueRecord carries a PR locator that
identifies both repository and number, written by checkout-pr and by
review request whenever either resolves a PR.

Precedence is explicit --pr, then the record, then branch discovery, so
review finish --pr keeps the contract it has today. Every PR entering
an acting path must carry this worktree's commits: a mistyped number
names a real PR that resolves cleanly, and its merge would let issue
end delete work that never landed."
```

---

## Task 8: Wire the dashboard to the trait

**Files:**
- Modify: `src/bin/issue/dashboard/data.rs:26`, `:42`
- Modify: `src/bin/issue/dashboard/cache.rs:24` (`path_for`)
- Test: both (inline `mod tests`)

**Interfaces:**
- Consumes: the resolved tracker, `Repos`.
- Produces: `cache::path_for(scope: &CacheScope, key: &str) -> PathBuf`; `CacheScope { tracker: TrackerKind, repo: String, viewer: String }`

- [ ] **Step 1: Write the failing cache-scoping test**

```rust
#[test]
fn two_projects_do_not_share_a_cache_entry() {
    // path_for was cache_dir()/dashboard/{key}.json with no project component,
    // so `issues`, `pr-timeline-mine` and `pr-timeline-all` were already shared
    // by every project on the machine. Two projects on different trackers would
    // serve each other's timelines.
    let a = path_for(&scope(TrackerKind::Linear, "acme", "me"), "issues");
    let b = path_for(&scope(TrackerKind::Github, "o/r", "me"), "issues");
    let c = path_for(&scope(TrackerKind::Github, "o/r", "someone"), "issues");
    assert_ne!(a, b);
    assert_ne!(b, c);
}

#[test]
fn a_scope_component_cannot_escape_the_cache_directory() {
    // issues_repo comes from devkit.toml, which travels with a checkout, and
    // path_for interpolates straight into a filename.
    let root = paths::cache_dir().join("dashboard");
    let p = path_for(&scope(TrackerKind::Github, "../../../etc", "me"), "issues");
    assert!(p.starts_with(&root), "{} escaped {}", p.display(), root.display());
    assert!(!p.to_string_lossy().contains(".."), "{}", p.display());
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p devkit --bin issue two_projects_do_not_share`
Expected: FAIL — `path_for` takes one argument.

- [ ] **Step 3: Implement the scoped, encoded key**

```rust
/// What a dashboard cache entry belongs to. Two projects on different trackers
/// would otherwise serve each other's timelines, and two viewers of one project
/// would serve each other's assigned issues.
pub struct CacheScope {
    pub tracker: TrackerKind,
    pub repo: String,
    pub viewer: String,
}

/// Every component is hashed rather than interpolated. A configured repository
/// slug reaches this filename and `devkit.toml` travels with a checkout, so a
/// value carrying `..` would otherwise let a read-only dashboard command write
/// outside the cache directory.
fn path_for(scope: &CacheScope, key: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    scope.tracker.to_string().hash(&mut h);
    scope.repo.hash(&mut h);
    scope.viewer.hash(&mut h);
    let digest = format!("{:016x}", h.finish());
    // `key` is a compile-time literal from this module; the scope is not.
    paths::cache_dir()
        .join("dashboard")
        .join(format!("{key}-{digest}.json"))
}
```

- [ ] **Step 4: Run and watch it pass**

Run: `cargo test -p devkit --bin issue two_projects_do_not_share a_scope_component_cannot`
Expected: PASS.

- [ ] **Step 5: Write the failing trait-driven timeline test**

```rust
#[test]
fn the_dashboard_reads_the_configured_tracker() {
    // assigned_history and timeline_origin had no caller anywhere outside the
    // tracker module: data.rs returned an empty list without LINEAR_API_KEY and
    // otherwise called linear:: directly. Implementing them on GithubTracker
    // would have delivered an empty timeline that never said why.
    let t = fake::FakeTracker::new().with_assigned(vec![assigned("ENG-1")]);
    let got = issues_via(&t).unwrap();
    assert_eq!(got.len(), 1);
}
```

- [ ] **Step 6: Run, watch it fail, then rewrite `data.rs`**

Replace the `LINEAR_API_KEY` guard and the two `linear::` calls at `:26` and `:42` with `t.assigned_history(on_page)` and `t.timeline_origin()`.

- [ ] **Step 7: Run the full suite, clippy and fmt**

Run: `cargo test --workspace && cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, no warnings. Linear's behavior through the new path is the regression gate.

- [ ] **Step 8: Commit**

```bash
git add src/bin/issue/dashboard
git commit -m "feat(dashboard): read the timeline from the configured tracker

assigned_history and timeline_origin had no caller outside the tracker
module: data.rs returned an empty list without LINEAR_API_KEY and
otherwise called linear:: directly, so implementing them on a new
adapter would have delivered an empty chart that never said why.

Every dashboard cache key gains the tracker, repository and viewer, and
every component is hashed rather than interpolated — the keys were
global, so two projects already shared one entry, and a configured
repository slug reaching a filename could carry path traversal."
```

---

## Task 9: Selection and detection

**Files:**
- Modify: `crates/devkit-common/src/tracker/mod.rs` (`resolve`, `detect`)
- Modify: `src/bin/issue/tracker.rs`, `crates/devkit-mcp/src/` (`issue.status` handler)
- Test: `crates/devkit-common/src/tracker/mod.rs`

**Interfaces:**
- Consumes: `GithubTracker` (task 5), `Repos` (task 1).
- Produces: `tracker::resolve(kind: Option<TrackerKind>, cwd: &Path, repos: &Repos) -> Resolved`

- [ ] **Step 1: Write the failing selection and detection tests**

```rust
#[test]
fn a_declared_github_kind_builds_the_real_adapter() {
    let repos = repos_with("me/widget");
    let r = resolve(Some(TrackerKind::Github), Path::new("."), &repos);
    assert_eq!(r.tracker.kind(), TrackerKind::Github);
    assert!(r.declared);
    assert!(r.reason.contains("kind = \"github\""), "{}", r.reason);
}

#[test]
fn a_gitlab_origin_no_longer_detects_as_github() {
    // slug_from_remote_url parses any host/owner/repo shape, and detect() read a
    // successful repo_slug as proof of a GitHub origin.
    let r = detect_with_remote("https://gitlab.com/o/r.git", None);
    assert_eq!(r, TrackerKind::None);
    assert_eq!(detect_with_remote("https://github.com/o/r.git", None), TrackerKind::Github);
}

#[test]
fn a_configured_project_is_ready_without_a_github_origin() {
    // A project with both keys configured has everything required; requiring an
    // origin would leave every state gate closed for it.
    let repos = Repos::from_parts(&cfg(Some("org/planning"), Some("up/app")), None, None);
    let r = resolve(Some(TrackerKind::Github), Path::new("."), &repos);
    assert!(r.tracker.ready() || github::token().is_none());
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p devkit-common a_declared_github_kind`
Expected: FAIL — `resolve` takes two arguments.

- [ ] **Step 3: Implement the real arm and the host-checked detection**

```rust
pub fn resolve(kind: Option<TrackerKind>, cwd: &Path, repos: &github::Repos) -> Resolved {
    resolve_with_key(kind, cwd, repos, crate::secrets::resolve("LINEAR_API_KEY"))
}

/* inside resolve_with_key */
        TrackerKind::Github => match repos.issues() {
            Ok(repo) => Resolved {
                tracker: Box::new(github_tracker::GithubTracker::new(repo.clone())),
                declared,
                reason: if declared {
                    "[tracker] kind = \"github\"".into()
                } else {
                    "detected: github.com `origin` remote".into()
                },
            },
            // No issues repository resolves, so there is nothing to ask. This
            // is devkit finding no answer, not the project declaring none.
            Err(e) => Resolved {
                tracker: Box::new(none::NoneTracker),
                declared: false,
                reason: format!("github selected but no issues repository: {e:#}"),
            },
        },
```

And in `detect`, replace the bare `repo_slug(cwd).is_ok()` with `github::github_origin_slug(cwd).is_ok()`.

- [ ] **Step 4: Run and watch them pass**

Run: `cargo test -p devkit-common a_declared_github_kind a_gitlab_origin a_configured_project`
Expected: PASS.

- [ ] **Step 5: Run the full suite, clippy and fmt**

Run: `cargo test --workspace && cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, no warnings. **GitHub is live from this commit.**

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-common/src/tracker src/bin/issue/tracker.rs crates/devkit-mcp
git commit -m "feat(tracker): select the GitHub adapter for kind = \"github\"

TrackerKind::Github resolved to the no-tracker stand-in with declared:
false. It gains its real arm, and tracker::resolve is handed the
resolved issues repository rather than deriving one per call.

Detection validates the origin host: slug_from_remote_url parses any
host/owner/repo shape, so a GitLab or Bitbucket project detected as
GitHub and would have queried an unrelated github.com repository.

This lands after the record and dashboard tasks on purpose — flipping
it while either was half-wired would ship a tracker reporting
confidently wrong verdicts."
```

---

## Task 10: `devkit auth github`

**Files:**
- Modify: `src/bin/devkit/auth.rs`, `src/bin/devkit/doctor.rs:45`
- Test: `src/bin/devkit/auth.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `github::token()`.
- Produces: `auth::github_report(token_source: TokenSource, viewer: Option<&str>, hosts: &[GhHost]) -> String`

- [ ] **Step 1: Write the failing identity test**

```rust
#[test]
fn the_identity_comes_from_the_token_not_the_active_gh_account() {
    // resolve_token reads GH_TOKEN, then GITHUB_TOKEN, and only then falls back
    // to `gh auth token`. With either variable set, the active gh account is
    // not the identity devkit uses, and reporting it as such would mislead
    // precisely the user who most needs the answer.
    let out = github_report(
        TokenSource::Env("GH_TOKEN"),
        Some("ci-bot"),
        &[GhHost { login: "a-human".into(), host: "github.com".into(), active: true }],
    );
    assert!(out.contains("ci-bot"), "{out}");
    assert!(out.contains("GH_TOKEN"), "{out}");
    // The gh accounts are secondary diagnostics, below the identity line.
    assert!(out.find("ci-bot").unwrap() < out.find("a-human").unwrap(), "{out}");
}

#[test]
fn no_token_prints_the_login_instruction() {
    let out = github_report(TokenSource::None, None, &[]);
    assert!(out.contains("gh auth login"), "{out}");
    assert!(out.contains("GH_TOKEN") && out.contains("GITHUB_TOKEN"), "{out}");
}

#[test]
fn a_malformed_or_missing_hosts_payload_degrades() {
    assert!(parse_gh_hosts(&serde_json::json!({})).is_empty());
    assert!(parse_gh_hosts(&serde_json::json!({"github.com": "nonsense"})).is_empty());
}
```

- [ ] **Step 2: Run, watch it fail, then implement**

`devkit auth github` reports rather than stores: `gh auth login`, `GH_TOKEN` and `GITHUB_TOKEN` already cover the credential and `github::token()` already reads all three.

- [ ] **Step 3: Add the doctor hint**

In `src/bin/devkit/doctor.rs`, when the resolved tracker is GitHub and `github::token()` is `None`, carry the `gh auth login` instruction as the tracker row's hint, matching the existing `HINT_LINEAR` pattern.

- [ ] **Step 4: Run the full suite, clippy and fmt, then commit**

```bash
git add src/bin/devkit
git commit -m "feat(devkit): report the GitHub identity in devkit auth

devkit stores no GitHub credential of its own, so this reports rather
than stores. The identity comes from the resolved token, not the active
gh account: with GH_TOKEN or GITHUB_TOKEN set, gh's account is not the
identity devkit uses."
```

---

## Task 11: Dogfood

**Files:**
- Modify: `devkit.toml`

- [ ] **Step 1: Declare the tracker**

```toml
[tracker]
kind = "github"
```

- [ ] **Step 2: Verify the empty-tracker path**

Run: `cargo run --bin devkit -- doctor`
Expected: the tracker row reads `github`, with the reason `[tracker] kind = "github"`.

Run: `cargo run --bin issue -- status`
Expected: the STATE column is blank for every worktree and no command errors. This repository has no GitHub issues filed, so the declaration exercises the empty-tracker path and little else. It becomes real exercise once devkit work is filed as issues.

- [ ] **Step 3: Commit**

```bash
git add devkit.toml
git commit -m "chore: declare github as devkit's own tracker"
```

---

## Task 12: Documentation

**Files:**
- Modify: `docs/configuration.md`, `README.md`, `AGENTS.md`, `skills/using-devkit/cli-reference.md`

- [ ] **Step 1: Document the config**

`docs/configuration.md` gains the `[tracker] kind = "github"` row, the `[github]` table with `issues_repo` and `pr_repo`, and the `gh auth login` instruction. Describe both keys as defaulting to the `origin` remote and note that each is required only where it is used.

- [ ] **Step 2: Update the README and CLI reference**

`README.md` gains `devkit auth github`. `skills/using-devkit/cli-reference.md` gains that subcommand and `issue review request --pr <URL|number>`.

- [ ] **Step 3: Correct AGENTS.md**

The tracker paragraph currently says "There is no GitHub implementation, so that arm also lands on `NoneTracker`." Replace it with a description of the real arm. Keep the paragraph timeless — no counts, no "now".

- [ ] **Step 4: Regenerate the schema and verify**

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test && cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add docs README.md AGENTS.md skills schema
git commit -m "docs: describe the GitHub tracker and its repositories"
```

---

## Self-review

**Spec coverage.** Every design section maps to a task: the adapter → 5; which repository → 1; the typed lookup → 2 and 3; the repository resolution seam → 1; per-method mapping and state mapping → 5; the assignee filter → 5; choosing among linked PRs → 5; a PR outside `pr_repo` → 7; a pasted URL keeps its repository → 6; most of the trait has no caller → 4; the dashboard → 8; authentication → 10; selection → 9; non-goals → excluded throughout.

**Known gap, stated rather than hidden.** The spec's task 3 requires `issue info`'s cached path to clear a cached unique PR when the live lookup is non-unique; this plan implements it in task 3 step 12, but the cached-vs-live reconciliation has no test in the plan's step list. Add one before implementing: a row with a cached `Unique` and a live `Ambiguous` must render ambiguous.

**Placeholder scan.** No TBDs. Two places delegate rather than spell out: task 5 step 10's trailing comment listing the remaining trait methods, and task 10 step 2. Both are the same query/parse/wrapper triple shown in full for `issue_pr`; if the implementer wants each spelled out, expand from that template.

**Type consistency.** `PrStatus` (task 3) is `devkit_issue::status::PrStatus` throughout. `PrLocator` (task 6) is `devkit_common::github::PrLocator`, used by `IssueRecord.pr` (task 7). `HeadLookup` (task 2) is used by tasks 3 and 7. `Repo`/`Repos` (task 1) are used by tasks 2, 3, 5, 6, 7, 9. `LinkedChoice` and `LinkedPr` are task 5's only. `assert_belongs` and `resolve_locator` live in `review/finish.rs` and are `pub(crate)` so `request.rs` can call them.

**One risk worth naming before execution.** Task 1 rewrites every `gh_json` call site in one commit and task 3 changes a serialized struct the MCP surface exposes. Both are large blast radius for a Phase A that ships to Linear users. Run the full suite between each, and treat task 1's "same repository, not same argument vector" gate as the real check.
