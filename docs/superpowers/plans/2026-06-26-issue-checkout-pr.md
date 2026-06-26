# Issue checkout-pr Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an `issue checkout-pr <PR_LINEAR_ID_URL> [WORKTREE_PATH]` subcommand that resolves a GitHub PR (directly, or via a Linear issue's attached PR) and checks its branch out into a config-placed worktree.

**Architecture:** A new binary-local `checkout` module owns identifier classification (`#N` / `PREFIX-N` / bare `N` / URL), a pure fuzzy-disambiguation decision, slugification, and the resolve→checkout orchestration. Linear gains pure query-builders + parsers plus thin network wrappers to turn a Linear id into its GitHub PR and to look up issues by number. The worktree is created detached and the PR branch is laid down with `gh pr checkout` (fork-safe). `Templates` gains a `checkout_worktree_dir` template.

**Tech Stack:** Rust (edition 2024), `anyhow`, `clap`, `minijinja` (via `devkit_common::template`), `ureq` (Linear GraphQL), `gh`/`git` via `devkit_common::cmd`.

**Spec:** `docs/superpowers/specs/2026-06-26-issue-checkout-pr-design.md`

---

## File Structure

- `crates/devkit-ports/src/config.rs` — **modify**: add `checkout_worktree_dir` template field, default const, accessor.
- `crates/devkit-common/src/linear.rs` — **modify**: `LinearPr`, `LinearIssueRef`, `pr_number_from_url`, `issue_pr_query`/`parse_issue_pr`/`issue_pr`, `issues_by_number_query`/`parse_number_candidates`/`issues_by_number`.
- `src/bin/issue/checkout.rs` — **create**: the whole subcommand (args, classify, decide_fuzzy, slugify, resolve, worktree+checkout, record).
- `src/bin/issue/setup.rs` — **modify**: extract the per-app prep loop into `pub(crate) fn prep_apps` reused by `--setup`.
- `src/bin/issue/main.rs` — **modify**: declare `mod checkout`, add the `CheckoutPr` subcommand variant + dispatch.
- `README.md`, `AGENTS.md` — **modify**: document the subcommand.

Run the full gate after every task:
```
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check
```

---

### Task 1: Config — `checkout_worktree_dir` template

**Files:**
- Modify: `crates/devkit-ports/src/config.rs:118-155`
- Test: same file's `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test**

Add to the tests module in `crates/devkit-ports/src/config.rs`:

```rust
#[test]
fn default_checkout_worktree_dir_template() {
    let t = Templates::default();
    assert_eq!(t.checkout_worktree_dir(), DEFAULT_CHECKOUT_WORKTREE_DIR);
    assert!(t.checkout_worktree_dir().contains("pr_number"));
    assert!(t.checkout_worktree_dir().contains("linear_id"));
}

#[test]
fn checkout_worktree_dir_override_wins() {
    let cfg = Config::parse(
        "[templates]\ncheckout_worktree_dir = \"{{ pr_number }}\"\n",
    )
    .unwrap();
    assert_eq!(cfg.templates.checkout_worktree_dir(), "{{ pr_number }}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-ports default_checkout_worktree_dir_template`
Expected: FAIL — `no method named checkout_worktree_dir` / `cannot find value DEFAULT_CHECKOUT_WORKTREE_DIR`.

- [ ] **Step 3: Add the const, field, and accessor**

After `crates/devkit-ports/src/config.rs:122` (the `DEFAULT_SLACK` line) add:

```rust
pub const DEFAULT_CHECKOUT_WORKTREE_DIR: &str =
    "{{ pr_number }}-{{ pr_title }}{% if linear_id %}_[{{ linear_id }}]{% endif %}";
```

In `struct Templates` (after the `worktree_dir` field at line 131) add:

```rust
    pub checkout_worktree_dir: Option<String>,
```

In `impl Templates` (after the `worktree_dir` accessor at line 145) add:

```rust
    pub fn checkout_worktree_dir(&self) -> &str {
        self.checkout_worktree_dir
            .as_deref()
            .unwrap_or(DEFAULT_CHECKOUT_WORKTREE_DIR)
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-ports templates`
Expected: PASS (new tests + existing `defaults` tests still green).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/config.rs
git commit --no-gpg-sign -m "feat(config): add checkout_worktree_dir template"
```

---

### Task 2: Linear — resolve an issue to its PR and look up issues by number

**Files:**
- Modify: `crates/devkit-common/src/linear.rs`
- Test: same file's `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing tests**

Add to the tests module in `crates/devkit-common/src/linear.rs`:

```rust
#[test]
fn pr_number_parsed_from_url() {
    assert_eq!(
        pr_number_from_url("https://github.com/org/repo/pull/3340"),
        Some(3340)
    );
    assert_eq!(pr_number_from_url("https://github.com/org/repo/issues/9"), None);
}

#[test]
fn issue_pr_query_filters_team_and_number() {
    let q = issue_pr_query("ENG-42").unwrap();
    assert!(q.contains("key: { eq: \"ENG\" }"));
    assert!(q.contains("number: { eq: 42 }"));
    assert!(q.contains("attachments"));
    assert!(issue_pr_query("nodash").is_none());
}

#[test]
fn parse_issue_pr_finds_github_attachment() {
    let v = serde_json::json!({"data": {"issues": {"nodes": [{
        "title": "Fix login",
        "attachments": {"nodes": [
            {"url": "https://example.com/doc"},
            {"url": "https://github.com/org/repo/pull/3340"}
        ]}
    }]}}});
    let (pr, title) = parse_issue_pr(&v);
    assert_eq!(title, "Fix login");
    assert_eq!(pr.unwrap().number, 3340);
}

#[test]
fn parse_issue_pr_no_attachment_is_none() {
    let v = serde_json::json!({"data": {"issues": {"nodes": [{
        "title": "No PR yet", "attachments": {"nodes": []}
    }]}}});
    let (pr, title) = parse_issue_pr(&v);
    assert!(pr.is_none());
    assert_eq!(title, "No PR yet");
}

#[test]
fn parse_number_candidates_collects_ids_and_titles() {
    let v = serde_json::json!({"data": {"issues": {"nodes": [
        {"identifier": "ENG-3340", "title": "A"},
        {"identifier": "OPS-3340", "title": "B"}
    ]}}});
    let got = parse_number_candidates(&v);
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].id, "ENG-3340");
    assert_eq!(got[1].title, "B");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-common linear`
Expected: FAIL — `cannot find function pr_number_from_url` etc.

- [ ] **Step 3: Add the types, builders, parsers, and wrappers**

Add near the top of `crates/devkit-common/src/linear.rs` (after the existing structs, e.g. below `LinearIdentity` at line 16):

```rust
/// A GitHub PR linked to a Linear issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearPr {
    pub url: String,
    pub number: u64,
}

/// A Linear issue candidate from a by-number lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearIssueRef {
    pub id: String, // "ENG-42"
    pub title: String,
}

/// Parse the PR number out of a `…/pull/<n>` GitHub URL.
pub fn pr_number_from_url(url: &str) -> Option<u64> {
    let tail = url.split("/pull/").nth(1)?;
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// GraphQL fetching one issue's title + GitHub PR attachments. None for a
/// non-`TEAM-NUMBER` id.
pub fn issue_pr_query(id: &str) -> Option<String> {
    let (team, num) = id.split_once('-')?;
    Some(format!(
        "query {{ issues(filter: {{ team: {{ key: {{ eq: \"{}\" }} }}, number: {{ eq: {} }} }}) \
         {{ nodes {{ title attachments {{ nodes {{ url }} }} }} }} }}",
        team.to_uppercase(),
        num
    ))
}

/// From an `issue_pr_query` response, the first GitHub PR attachment + the title.
pub fn parse_issue_pr(resp: &serde_json::Value) -> (Option<LinearPr>, String) {
    let node = &resp["data"]["issues"]["nodes"][0];
    let title = node["title"].as_str().unwrap_or("").to_string();
    let pr = node["attachments"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["url"].as_str())
        .find(|u| u.contains("github.com") && u.contains("/pull/"))
        .and_then(|u| pr_number_from_url(u).map(|number| LinearPr { url: u.to_string(), number }));
    (pr, title)
}

/// Resolve a Linear id to its attached GitHub PR + the issue title.
pub fn issue_pr(id: &str, key: &str) -> Result<(Option<LinearPr>, String)> {
    let query = issue_pr_query(id).context("not a TEAM-NUMBER Linear id")?;
    let resp = post_graphql(&query, key)?;
    Ok(parse_issue_pr(&resp))
}

/// GraphQL for every issue (any team) with `number == n`.
pub fn issues_by_number_query(n: u64) -> String {
    format!(
        "query {{ issues(filter: {{ number: {{ eq: {} }} }}) \
         {{ nodes {{ identifier title }} }} }}",
        n
    )
}

/// Parse the candidates from an `issues_by_number_query` response.
pub fn parse_number_candidates(resp: &serde_json::Value) -> Vec<LinearIssueRef> {
    resp["data"]["issues"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            Some(LinearIssueRef {
                id: n["identifier"].as_str()?.to_string(),
                title: n["title"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// Look up every Linear issue whose number is `n`, across all teams.
pub fn issues_by_number(n: u64, key: &str) -> Result<Vec<LinearIssueRef>> {
    let resp = post_graphql(&issues_by_number_query(n), key)?;
    Ok(parse_number_candidates(&resp))
}

fn post_graphql(query: &str, key: &str) -> Result<serde_json::Value> {
    Ok(ureq::post("https://api.linear.app/graphql")
        .set("Authorization", key)
        .send_json(ureq::json!({ "query": query }))?
        .into_json()?)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-common linear`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/linear.rs
git commit --no-gpg-sign -m "feat(linear): resolve issue to PR and look up by number"
```

---

### Task 3: checkout module — classification, fuzzy decision, slugify, and resolution wiring

**Files:**
- Create: `src/bin/issue/checkout.rs`
- Modify: `src/bin/issue/main.rs:5-17` (mod list), `:33-118` (Cmd enum), `:128-206` (dispatch)
- Test: `src/bin/issue/checkout.rs` `#[cfg(test)] mod tests`

- [ ] **Step 1: Create the module with pure helpers + a resolution-only `run`**

Create `src/bin/issue/checkout.rs`:

```rust
use anyhow::{Context, Result};
use devkit_common::cmd::{capture, gh_json};
use devkit_common::linear::{self, LinearIssueRef};
use devkit_ports::config::expand_tilde;
use devkit_ports::load;
use std::io::{IsTerminal, Write};
use std::path::Path;

pub struct CheckoutArgs {
    pub target: String,
    pub worktree_path: Option<String>,
    pub setup: bool,
    pub apps: Vec<String>,
    pub dir: Option<String>,
    pub config: Option<String>,
}

/// How the raw `<PR_LINEAR_ID_URL>` input is classified before resolution.
#[derive(Debug, PartialEq, Eq)]
enum Ident {
    Pr(u64),
    Linear(String),
    Fuzzy(u64),
}

/// Classify the identifier by shape alone (no network, no key knowledge).
fn classify(input: &str) -> Result<Ident> {
    let s = input.trim();
    if s.contains("github.com") && s.contains("/pull/") {
        let n = linear::pr_number_from_url(s).context("no PR number in GitHub URL")?;
        return Ok(Ident::Pr(n));
    }
    if s.contains("linear.app") {
        let id = devkit_common::worktree::find_id(s).context("no issue id in Linear URL")?;
        return Ok(Ident::Linear(id.to_uppercase()));
    }
    if let Some(rest) = s.strip_prefix('#')
        && !rest.is_empty()
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return Ok(Ident::Pr(rest.parse().context("bad PR number")?));
    }
    if let Some((a, b)) = s.split_once('-')
        && !a.is_empty()
        && a.chars().all(|c| c.is_ascii_alphabetic())
        && !b.is_empty()
        && b.chars().all(|c| c.is_ascii_digit())
    {
        return Ok(Ident::Linear(s.to_uppercase()));
    }
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
        return Ok(Ident::Fuzzy(s.parse().context("bad number")?));
    }
    anyhow::bail!("unrecognized PR/Linear identifier: {s}");
}

/// The decision for a bare-number input after probing both sides.
#[derive(Debug, PartialEq, Eq)]
enum FuzzyDecision {
    UsePr,
    UseLinear(LinearIssueRef),
    Prompt(Vec<LinearIssueRef>),
    ErrorAmbiguous,
    ErrorNone,
}

fn decide_fuzzy(pr_exists: bool, candidates: &[LinearIssueRef], is_tty: bool) -> FuzzyDecision {
    match (pr_exists, candidates) {
        (false, []) => FuzzyDecision::ErrorNone,
        (true, []) => FuzzyDecision::UsePr,
        (false, [only]) => FuzzyDecision::UseLinear(only.clone()),
        _ if is_tty => FuzzyDecision::Prompt(candidates.to_vec()),
        _ => FuzzyDecision::ErrorAmbiguous,
    }
}

/// Lowercase, collapse non-alphanumerics to single dashes, trim dashes.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

struct Resolved {
    pr_number: u64,
    linear_id: Option<String>,
    linear_title: Option<String>,
}

fn pr_exists(n: u64, repo: &str) -> bool {
    capture("gh", &["pr", "view", &n.to_string(), "--json", "number"], Some(repo)).is_ok()
}

/// Turn a chosen Linear issue into a `Resolved`, erroring if it has no PR.
fn resolve_linear(id: &str, title: Option<String>, key: &str) -> Result<Resolved> {
    let (pr, fetched_title) = linear::issue_pr(id, key)?;
    let pr = pr.with_context(|| format!("Linear issue {id} has no associated PR to check out"))?;
    Ok(Resolved {
        pr_number: pr.number,
        linear_id: Some(id.to_string()),
        linear_title: Some(title.unwrap_or(fetched_title)),
    })
}

/// Resolve the raw input to a concrete PR. Network + interactive.
fn resolve(target: &str, key: Option<&str>, repo: &str) -> Result<Resolved> {
    match classify(target)? {
        Ident::Pr(n) => Ok(Resolved { pr_number: n, linear_id: None, linear_title: None }),
        Ident::Linear(id) => {
            let key = key.context("Linear id given but LINEAR_API_KEY is not set")?;
            resolve_linear(&id, None, key)
        }
        Ident::Fuzzy(n) => {
            // No Linear key → a bare number is a GitHub PR.
            let Some(key) = key else {
                return Ok(Resolved { pr_number: n, linear_id: None, linear_title: None });
            };
            let exists = pr_exists(n, repo);
            let candidates = linear::issues_by_number(n, key)?;
            let is_tty = std::io::stdin().is_terminal();
            match decide_fuzzy(exists, &candidates, is_tty) {
                FuzzyDecision::ErrorNone => {
                    anyhow::bail!("no PR or Linear issue found for {n}")
                }
                FuzzyDecision::ErrorAmbiguous => anyhow::bail!(
                    "ambiguous {n} — rerun as #{n} (GitHub PR) or PREFIX-{n} (Linear)"
                ),
                FuzzyDecision::UsePr => {
                    Ok(Resolved { pr_number: n, linear_id: None, linear_title: None })
                }
                FuzzyDecision::UseLinear(r) => resolve_linear(&r.id, Some(r.title), key),
                FuzzyDecision::Prompt(cands) => match prompt_choice(exists, &cands, n)? {
                    None => Ok(Resolved { pr_number: n, linear_id: None, linear_title: None }),
                    Some(r) => resolve_linear(&r.id, Some(r.title), key),
                },
            }
        }
    }
}

/// Print the options and read a choice. `Ok(None)` = the GitHub PR.
fn prompt_choice(pr_exists: bool, candidates: &[LinearIssueRef], n: u64) -> Result<Option<LinearIssueRef>> {
    println!("Multiple matches for {n}:");
    let mut options: Vec<Option<&LinearIssueRef>> = Vec::new();
    if pr_exists {
        options.push(None);
    }
    options.extend(candidates.iter().map(Some));
    for (i, opt) in options.iter().enumerate() {
        match opt {
            None => println!("  [{i}] GitHub PR #{n}"),
            Some(c) => println!("  [{i}] Linear {} — {}", c.id, c.title),
        }
    }
    print!("Choose [0]: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok();
    let idx: usize = line.trim().parse().unwrap_or(0);
    let chosen: Option<&LinearIssueRef> = *options.get(idx).context("choice out of range")?;
    Ok(chosen.cloned())
}

pub fn run(args: CheckoutArgs) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded = load::load(args.config.as_deref().map(Path::new), Path::new(&start))?;
    let cfg = &loaded.config;
    let catalog = &loaded.catalog;
    for a in &args.apps {
        anyhow::ensure!(catalog.contains_key(a), "unknown app `{a}`");
    }

    let wt_root = expand_tilde(&cfg.defaults.worktree_root);
    let monorepo = wt_root.join("monorepo");
    let monorepo_s = monorepo.to_str().context("monorepo path not UTF-8")?;

    let key = devkit_common::secrets::resolve("LINEAR_API_KEY");
    let resolved = resolve(&args.target, key.as_deref(), monorepo_s)?;

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PrMeta {
        number: u64,
        title: String,
        head_ref_name: String,
    }
    let meta: PrMeta = gh_json(
        &[
            "pr",
            "view",
            &resolved.pr_number.to_string(),
            "--json",
            "number,title,headRefName",
        ],
        monorepo_s,
    )
    .with_context(|| format!("fetching PR #{}", resolved.pr_number))?;

    let ctx = serde_json::json!({
        "pr_number": meta.number,
        "pr_title": slugify(&meta.title),
        "linear_id": resolved.linear_id.clone().unwrap_or_default(),
        "linear_title": resolved.linear_title.as_deref().map(slugify).unwrap_or_default(),
    });
    let wt_name = devkit_common::template::render(
        cfg.templates.checkout_worktree_dir(),
        &ctx,
        &cfg.templates.variables,
    )
    .context("rendering `checkout_worktree_dir` template")?
    .trim()
    .to_string();
    let worktree = match &args.worktree_path {
        Some(p) => expand_tilde(p),
        None => wt_root.join(&wt_name),
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "pr": meta.number,
            "branch": meta.head_ref_name,
            "worktree": worktree.to_string_lossy(),
        }))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lref(id: &str, title: &str) -> LinearIssueRef {
        LinearIssueRef { id: id.into(), title: title.into() }
    }

    #[test]
    fn classify_hash_is_pr() {
        assert_eq!(classify("#3340").unwrap(), Ident::Pr(3340));
    }
    #[test]
    fn classify_github_url_is_pr() {
        assert_eq!(
            classify("https://github.com/o/r/pull/12").unwrap(),
            Ident::Pr(12)
        );
    }
    #[test]
    fn classify_prefix_is_linear() {
        assert_eq!(classify("eng-42").unwrap(), Ident::Linear("ENG-42".into()));
    }
    #[test]
    fn classify_linear_url_is_linear() {
        assert_eq!(
            classify("https://linear.app/acme/issue/ENG-42/fix").unwrap(),
            Ident::Linear("ENG-42".into())
        );
    }
    #[test]
    fn classify_bare_number_is_fuzzy() {
        assert_eq!(classify("3340").unwrap(), Ident::Fuzzy(3340));
    }
    #[test]
    fn classify_garbage_errors() {
        assert!(classify("not an id").is_err());
    }

    #[test]
    fn fuzzy_none_errors() {
        assert_eq!(decide_fuzzy(false, &[], true), FuzzyDecision::ErrorNone);
    }
    #[test]
    fn fuzzy_pr_only() {
        assert_eq!(decide_fuzzy(true, &[], true), FuzzyDecision::UsePr);
    }
    #[test]
    fn fuzzy_single_linear() {
        assert_eq!(
            decide_fuzzy(false, &[lref("ENG-1", "a")], true),
            FuzzyDecision::UseLinear(lref("ENG-1", "a"))
        );
    }
    #[test]
    fn fuzzy_both_tty_prompts() {
        let cands = vec![lref("ENG-1", "a")];
        assert_eq!(
            decide_fuzzy(true, &cands, true),
            FuzzyDecision::Prompt(cands.clone())
        );
    }
    #[test]
    fn fuzzy_multi_linear_no_tty_is_ambiguous() {
        let cands = vec![lref("ENG-1", "a"), lref("OPS-1", "b")];
        assert_eq!(decide_fuzzy(false, &cands, false), FuzzyDecision::ErrorAmbiguous);
    }
    #[test]
    fn fuzzy_both_no_tty_is_ambiguous() {
        assert_eq!(
            decide_fuzzy(true, &[lref("ENG-1", "a")], false),
            FuzzyDecision::ErrorAmbiguous
        );
    }

    #[test]
    fn slugify_cleans_titles() {
        assert_eq!(slugify("Fix the Login!! page"), "fix-the-login-page");
        assert_eq!(slugify("  Trailing  "), "trailing");
        assert_eq!(slugify("ALL_CAPS"), "all-caps");
    }

    #[test]
    fn checkout_template_drops_linear_when_absent() {
        use devkit_ports::config::Templates;
        let t = Templates::default();
        let pr_only = serde_json::json!({
            "pr_number": 3340, "pr_title": "fix-login", "linear_id": "", "linear_title": ""
        });
        assert_eq!(
            devkit_common::template::render(t.checkout_worktree_dir(), &pr_only, &t.variables).unwrap(),
            "3340-fix-login"
        );
        let with_linear = serde_json::json!({
            "pr_number": 3340, "pr_title": "fix-login", "linear_id": "ENG-42", "linear_title": "x"
        });
        assert_eq!(
            devkit_common::template::render(t.checkout_worktree_dir(), &with_linear, &t.variables).unwrap(),
            "3340-fix-login_[ENG-42]"
        );
    }
}
```

- [ ] **Step 2: Wire the subcommand into `main.rs`**

In `src/bin/issue/main.rs`, add `mod checkout;` to the module list (alphabetical, before `mod dashboard;` at line 6):

```rust
mod checkout;
```

Add the variant to `enum Cmd` (after the `Setup { … }` block, before `Status` at line 48):

```rust
    /// Check out an existing PR (by number, Linear id, or URL) into a new worktree.
    CheckoutPr {
        /// `#3340` | `3340` | `PREFIX-3340` | github PR URL | linear issue URL.
        target: String,
        /// Worktree path; defaults to the config-resolved placement.
        worktree_path: Option<String>,
        #[arg(long)]
        setup: bool,
        #[arg(long, value_delimiter = ',')]
        apps: Vec<String>,
    },
```

Add the dispatch arm in `main()` (after the `Setup` arm, before `Status` at line 144):

```rust
        Some(Cmd::CheckoutPr {
            target,
            worktree_path,
            setup,
            apps,
        }) => checkout::run(checkout::CheckoutArgs {
            target,
            worktree_path,
            setup,
            apps,
            dir: cli.dir,
            config: cli.config,
        }),
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p devkit --bin issue checkout` then `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, zero warnings. (`run` is referenced by `main`, so no dead-code; `--setup`/`apps` are parsed but unused-by-effect is fine — they are read into `CheckoutArgs`.)

Note: the `setup` field is consumed by `run` only in Task 5. To keep Task 3 warning-clean, `run` already reads `args.setup`? It does not yet. Add a single guard at the end of `run` before the final print so the field is read:

```rust
    if args.setup {
        anyhow::bail!("--setup is not implemented yet");
    }
```

(Removed in Task 5.) This keeps `--setup` honest rather than silently ignored.

- [ ] **Step 4: Manual smoke (optional, needs gh auth)**

Run: `cargo run --bin issue -- checkout-pr '#<an-open-pr>'`
Expected: prints JSON with `pr`, `branch`, and the computed `worktree` path. No worktree created yet.

- [ ] **Step 5: Commit**

```bash
git add src/bin/issue/checkout.rs src/bin/issue/main.rs
git commit --no-gpg-sign -m "feat(issue): add checkout-pr identifier resolution"
```

---

### Task 4: Create the worktree and check out the PR branch

**Files:**
- Modify: `src/bin/issue/checkout.rs` (the tail of `run`, plus a `record_issue_id` helper)
- Test: `src/bin/issue/checkout.rs` tests

- [ ] **Step 1: Write the failing test for the record id helper**

Add to `checkout.rs` tests:

```rust
#[test]
fn record_issue_id_prefers_linear_then_head_ref() {
    assert_eq!(record_issue_id(&Some("ENG-42".into()), "lev/eng-9-x"), "ENG-42");
    assert_eq!(record_issue_id(&None, "lev/eng-9-fix"), "ENG-9");
    assert_eq!(record_issue_id(&None, "no-id-here"), "UNKNOWN");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit --bin issue record_issue_id_prefers`
Expected: FAIL — `cannot find function record_issue_id`.

- [ ] **Step 3: Add the helper and replace the print-only tail of `run`**

Add this helper above `run` in `checkout.rs`:

```rust
/// The id stored in `.devkit/issue.toml`: the Linear id if known, else the id
/// parsed from the PR head ref, else `UNKNOWN`.
fn record_issue_id(linear_id: &Option<String>, head_ref: &str) -> String {
    linear_id.clone().unwrap_or_else(|| {
        devkit_common::worktree::find_id(head_ref)
            .map(|s| s.to_uppercase())
            .unwrap_or_else(|| "UNKNOWN".into())
    })
}
```

Add `git` to the `cmd` import at the top:

```rust
use devkit_common::cmd::{capture, gh_json, git};
```

Replace the final `println!(…)` block of `run` (the JSON-only tail from Task 3) with:

```rust
    anyhow::ensure!(
        !worktree.exists(),
        "worktree path already exists: {}",
        worktree.display()
    );
    let worktree_s = worktree.to_str().context("worktree path not UTF-8")?;

    git(&["fetch", "origin"], monorepo_s)?;
    git(
        &["worktree", "add", "--detach", worktree_s, &cfg.defaults.baseline_ref],
        monorepo_s,
    )?;
    capture("gh", &["pr", "checkout", &meta.number.to_string()], Some(worktree_s))
        .with_context(|| format!("checking out PR #{}", meta.number))?;

    crate::record::write(
        &worktree,
        &crate::record::IssueRecord {
            issue: record_issue_id(&resolved.linear_id, &meta.head_ref_name),
            slug: slugify(&meta.title),
            apps: if args.setup { args.apps.clone() } else { vec![] },
        },
    )?;

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "pr": meta.number,
            "branch": meta.head_ref_name,
            "worktree": worktree_s,
        }))?
    );
    Ok(())
```

Keep the Task-3 `--setup` guard (`if args.setup { bail!(…) }`) immediately before this block for now.

- [ ] **Step 4: Run tests + clippy**

Run: `cargo test -p devkit --bin issue && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, zero warnings.

- [ ] **Step 5: Manual smoke (needs gh auth + a worktree_root config)**

Run: `cargo run --bin issue -- checkout-pr '#<an-open-pr>'`
Expected: creates `<worktree_root>/<n>-<slug>`, the PR branch checked out there (`git -C <path> branch --show-current` matches the PR head ref), `.devkit/issue.toml` written. Then `git worktree remove <path>` to clean up.

- [ ] **Step 6: Commit**

```bash
git add src/bin/issue/checkout.rs
git commit --no-gpg-sign -m "feat(issue): create worktree and check out PR branch"
```

---

### Task 5: `--setup` runs the per-app prep pipeline

**Files:**
- Modify: `src/bin/issue/setup.rs:136-162` (extract `prep_apps`)
- Modify: `src/bin/issue/checkout.rs` (call `prep_apps`, drop the guard)

- [ ] **Step 1: Extract `prep_apps` from `setup::run`**

In `src/bin/issue/setup.rs`, add this `pub(crate)` function (above `pub fn run`):

```rust
/// Per-app bootstrap shared by `setup` and `checkout-pr --setup`: write each
/// app's prep files (rendered against `base_ctx` plus `app`/`branch`/`worktree`),
/// then run its setup commands in its directory.
pub(crate) fn prep_apps(
    worktree: &Path,
    branch: &str,
    apps: &[String],
    catalog: &std::collections::HashMap<String, devkit_ports::apps::App>,
    base_ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
) -> Result<()> {
    for a in apps {
        let app = &catalog[a];
        let app_dir = worktree.join(&app.path);
        std::fs::create_dir_all(&app_dir).ok();

        let mut file_ctx = base_ctx.clone();
        if let Some(obj) = file_ctx.as_object_mut() {
            obj.insert("app".into(), serde_json::Value::String(a.clone()));
            obj.insert("branch".into(), serde_json::Value::String(branch.to_string()));
            obj.insert(
                "worktree".into(),
                serde_json::Value::String(worktree.to_string_lossy().into_owned()),
            );
        }
        write_prep_files(&app_dir, &app.prep_files, &file_ctx, vars)
            .with_context(|| format!("preparing files for app `{a}`"))?;

        for cmd in &app.setup {
            let (prog, rest) = cmd.split_first().context("empty setup command")?;
            capture(
                prog,
                &rest.iter().map(String::as_str).collect::<Vec<_>>(),
                app_dir.to_str(),
            )
            .with_context(|| format!("running setup `{}` for app `{a}`", cmd.join(" ")))?;
        }
    }
    Ok(())
}
```

Then replace the inline per-app loop in `run` (lines 136-162) with a single call:

```rust
    prep_apps(&worktree, &branch, &args.apps, catalog, &ctx, vars)?;
```

- [ ] **Step 2: Run setup tests to verify the extraction is green**

Run: `cargo test -p devkit --bin issue setup`
Expected: PASS — the existing `write_prep_files` tests still cover the inner behavior; `setup::run` is unchanged in effect.

- [ ] **Step 3: Call `prep_apps` from checkout, drop the guard**

In `src/bin/issue/checkout.rs`, remove the `if args.setup { anyhow::bail!("--setup is not implemented yet"); }` guard. After the `crate::record::write(…)` call (and before the final `println!`), add:

```rust
    if args.setup {
        let branch = meta.head_ref_name.clone();
        let setup_ctx = serde_json::json!({
            "prefix": cfg.defaults.branch_prefix,
            "issue": record_issue_id(&resolved.linear_id, &meta.head_ref_name),
            "slug": slugify(&meta.title),
            "apps": args.apps,
        });
        crate::setup::prep_apps(
            &worktree,
            &branch,
            &args.apps,
            catalog,
            &setup_ctx,
            &cfg.templates.variables,
        )?;
    }
```

- [ ] **Step 4: Run tests + clippy**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, zero warnings.

- [ ] **Step 5: Manual smoke (needs a configured app)**

Run: `cargo run --bin issue -- checkout-pr '#<pr>' --setup --apps <app>`
Expected: worktree created, PR checked out, the app's prep files written and setup commands run. Clean up with `git worktree remove`.

- [ ] **Step 6: Commit**

```bash
git add src/bin/issue/setup.rs src/bin/issue/checkout.rs
git commit --no-gpg-sign -m "feat(issue): run per-app prep on checkout-pr --setup"
```

---

### Task 6: Document the subcommand

**Files:**
- Modify: `README.md`, `AGENTS.md`

- [ ] **Step 1: Update the issue-binary description in `AGENTS.md`**

Find the `src/bin/issue` row in the Layout table and the line listing issue verbs (`setup, status, end, prs, dashboard, review`). Add `checkout-pr` to both, e.g.:

```
| `src/bin/issue` | issue lifecycle: `setup`, `checkout-pr`, `status`, `end`, `prs`, `dashboard`, `review` |
```

- [ ] **Step 2: Document `checkout-pr` in `README.md`**

Find the `issue` section in `README.md` and add a subsection mirroring the existing command docs:

```markdown
### `issue checkout-pr <PR_LINEAR_ID_URL> [WORKTREE_PATH]`

Check out an existing PR into a new worktree. The target may be a GitHub PR
number (`#3340`), a bare number (`3340`, probed against both GitHub and Linear),
a Linear id (`ENG-3340`), or a GitHub/Linear URL. With a Linear id, the PR linked
to the issue is used; an issue with no PR is an error. The worktree directory is
named by the `templates.checkout_worktree_dir` template (variables: `pr_number`,
`pr_title`, `linear_id`, `linear_title`). Pass `--setup [--apps a,b]` to also run
the per-app prep pipeline.
```

- [ ] **Step 3: Verify the docs build / completions still generate**

Run: `cargo run --bin issue -- completions bash | rg checkout-pr`
Expected: the completion script mentions `checkout-pr` (auto-generated from the clap variant).

- [ ] **Step 4: Commit**

```bash
git add README.md AGENTS.md
git commit --no-gpg-sign -m "docs: document issue checkout-pr"
```

---

## Self-Review Notes

- **Spec coverage:** routing table → Task 3 `classify`; bare-number disambiguation → Task 3 `decide_fuzzy` + `resolve`; Linear→PR query → Task 2; no-PR error → Task 4/`resolve_linear`; fork-safe checkout → Task 4 `gh pr checkout`; config template + slugify → Tasks 1 & 3; record + `--setup` → Tasks 4 & 5; error messages → Task 3 `resolve`; tests → each task.
- **Type consistency:** `LinearPr`/`LinearIssueRef` defined in Task 2 are used unchanged in Task 3; `Resolved`/`Ident`/`FuzzyDecision` are module-private to `checkout.rs`; `checkout_worktree_dir()` matches between Task 1 and Task 3; `prep_apps` signature matches between Task 5's definition and call site.
- **Linear-key-absent split:** an explicit `PREFIX-N`/linear URL with no key errors in `resolve` (Ident::Linear arm); a bare `N` with no key is treated as a PR (Ident::Fuzzy arm) — matching the corrected spec routing table.

## Unresolved Questions

None — all design decisions were settled during brainstorming (scope = checkout + optional `--setup`; no-PR = error; template governs the directory only; non-TTY collision = error; default template renders the `_[linear_id]` suffix only when present).
