//! The weakened-tests gate: what a branch did to its test files since its
//! merge base. A suite can be made to pass by deleting or skipping tests, so
//! each change that thins one is flagged, and a `Test changes:` line in the
//! PR body puts the reason in front of the reviewer.

mod scan;

use std::{fmt, path::Path};

use anyhow::{Context, Result, bail};
use devkit_common::{
    test_files::TestFiles,
    vcs::{Vcs, VersionControl},
};
use devkit_config::{CONFIG_FILE, GateMode, PrConfig};
use scan::{Counts, Lang};

/// One way a branch weakened a test file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rule {
    DeletedFile,
    FewerTests,
    FewerAssertions,
    MoreSkips,
    Focused,
    Tautology,
}

impl Rule {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::DeletedFile => "deleted-test-file",
            Self::FewerTests => "fewer-tests",
            Self::FewerAssertions => "fewer-assertions",
            Self::MoreSkips => "more-skips",
            Self::Focused => "focused-test",
            Self::Tautology => "tautology",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Flag {
    pub path: String,
    pub rule: Rule,
    pub detail: String,
}

impl fmt::Display for Flag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}: {}", self.path, self.rule.name(), self.detail)
    }
}

/// The flags for one test file, read at the merge base (`base`) and at the
/// head (`head`). `None` is a side with no such file. A deleted test file is
/// flagged in any language; the other rules need one the scanner reads.
fn compare(path: &str, lang: Option<Lang>, base: Option<&str>, head: Option<&str>) -> Vec<Flag> {
    let flag = |rule, detail: String| Flag {
        path: path.to_string(),
        rule,
        detail,
    };
    let Some(head) = head else {
        return vec![flag(Rule::DeletedFile, "the branch deletes it".into())];
    };
    let Some(lang) = lang else {
        return Vec::new();
    };
    let now = scan::counts(lang, head);
    let Some(base) = base else {
        return tautologies(&Counts::default(), &now, &flag);
    };
    let was = scan::counts(lang, base);
    let mut flags = Vec::new();
    let mut fewer = |rule, what: &str, was: usize, now: usize| {
        if now < was {
            flags.push(flag(rule, format!("{what} {was} -> {now}")));
        }
    };
    fewer(Rule::FewerTests, "test declarations", was.tests, now.tests);
    fewer(
        Rule::FewerAssertions,
        "assertions",
        was.assertions,
        now.assertions,
    );
    if now.skips > was.skips {
        flags.push(flag(
            Rule::MoreSkips,
            format!("skip markers {} -> {}", was.skips, now.skips),
        ));
    }
    if now.focused > was.focused {
        flags.push(flag(
            Rule::Focused,
            format!(
                "focus markers (`.only(`) {} -> {}: every other test in the file stops running",
                was.focused, now.focused
            ),
        ));
    }
    flags.extend(tautologies(&was, &now, &flag));
    flags
}

fn tautologies(was: &Counts, now: &Counts, flag: &impl Fn(Rule, String) -> Flag) -> Vec<Flag> {
    now.tautologies
        .iter()
        .filter(|(t, n)| was.tautologies.get(*t).copied().unwrap_or(0) < **n)
        .map(|(t, _)| flag(Rule::Tautology, format!("`{t}` always passes")))
        .collect()
}

/// Every flag between revisions `base` and `head` of the repository at
/// `dir`, for the files `tests` names.
pub(crate) fn check(dir: &Path, base: &str, head: &str, tests: &TestFiles) -> Result<Vec<Flag>> {
    let vcs = Vcs::at(dir);
    let mut flags = Vec::new();
    for change in vcs.changed_files(dir, base, head)? {
        let old = change.old.filter(|p| tests.matches(p));
        let new = change.new.filter(|p| tests.matches(p));
        let Some(path) = new.as_deref().or(old.as_deref()) else {
            continue;
        };
        let lang = Lang::of(path);
        if lang.is_none() && new.is_some() {
            continue;
        }
        let read = |rev: &str, p: Option<&str>| -> Result<Option<String>> {
            match p {
                Some(p) => vcs.file_at(dir, rev, p),
                None => Ok(None),
            }
        };
        let before = read(base, old.as_deref())?;
        let after = read(head, new.as_deref())?;
        let label = match (&old, &new) {
            (Some(o), Some(n)) if o != n => format!("{o} -> {n}"),
            _ => path.to_string(),
        };
        flags.extend(compare(&label, lang, before.as_deref(), after.as_deref()));
    }
    flags.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(flags)
}

/// The merge base of HEAD with PR base branch `base`, which resolves as
/// `origin/<base>`, else `<base>`; `None` when neither is a commit here.
/// `pr create` reads its gates there, and the brief reads them the same way.
pub(crate) fn merge_base(dir: &Path, base: &str) -> Result<Option<String>> {
    let vcs = Vcs::at(dir);
    let origin = format!("origin/{base}");
    for rev in [origin.as_str(), base] {
        if vcs.has_revision(dir, rev)? {
            return vcs.merge_base(dir, rev, "HEAD").map(Some);
        }
    }
    Ok(None)
}

/// `[pr]` as the `devkit.toml` at the root of revision `rev` has it, the
/// defaults when that revision has none.
pub(crate) fn config_at(dir: &Path, rev: &str) -> Result<PrConfig> {
    match Vcs::at(dir).file_at(dir, rev, CONFIG_FILE)? {
        Some(text) => PrConfig::from_layer(&text)
            .with_context(|| format!("reading {CONFIG_FILE} at {}", short(rev))),
        None => Ok(PrConfig::default()),
    }
}

fn short(rev: &str) -> &str {
    &rev[..rev.len().min(12)]
}

/// Whether `body` has a line starting `Test changes:` that says something,
/// after any list bullet or bold.
pub(crate) fn explains_test_changes(body: &str) -> bool {
    const LABEL: &str = "test changes:";
    body.lines().any(|line| {
        let t = line.trim_start();
        let t = t.strip_prefix(['-', '*', '+']).unwrap_or(t).trim_start();
        let t = t.trim_start_matches('*');
        t.get(..LABEL.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(LABEL))
            && !t[LABEL.len()..].trim_matches(['*', ' ', '\t']).is_empty()
    })
}

fn listing(flags: &[Flag]) -> String {
    flags
        .iter()
        .map(|f| format!("  {f}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The weakened-tests gate for one `pr create`: the mode the merge base
/// configures and, unless it is off, the flags.
pub(crate) struct Gate {
    mode: GateMode,
    flags: Vec<Flag>,
}

impl Gate {
    /// The gate `cfg`, read at `merge_base`, sets for the branch at `dir`.
    pub(crate) fn check(dir: &Path, merge_base: &str, cfg: &PrConfig) -> Result<Self> {
        let mode = cfg.gates.weakened_tests;
        let flags = match mode {
            GateMode::Off => Vec::new(),
            GateMode::Advise | GateMode::Require => {
                check(dir, merge_base, "HEAD", &TestFiles::new(&cfg.test_files)?)?
            }
        };
        Ok(Self { mode, flags })
    }

    /// `body` as the PR should carry it: refused under `require` when it
    /// leaves a flag unexplained, and under `advise` with the flags listed.
    pub(crate) fn apply(&self, body: String) -> Result<String> {
        if self.flags.is_empty() || explains_test_changes(&body) {
            return Ok(body);
        }
        match self.mode {
            GateMode::Off => Ok(body),
            GateMode::Advise => {
                let items: Vec<String> = self.flags.iter().map(|f| format!("- `{f}`")).collect();
                Ok(format!(
                    "{}\n\n### Weakened tests\n\n{}\n",
                    body.trim_end(),
                    items.join("\n")
                ))
            }
            GateMode::Require => bail!(
                "refusing to open a PR that weakens tests without saying why:\n{}\n\
                 Restore the tests, or explain each change on a line of the PR body \
                 starting `Test changes:`",
                listing(&self.flags)
            ),
        }
    }
}

/// `pr weakened-tests`: the check on its own, between `base`'s merge base
/// with `head` and `head`. Needs git and nothing else.
pub(crate) fn run(dir: &Path, base: &str, head: &str, body_file: Option<&Path>) -> Result<()> {
    let vcs = Vcs::at(dir);
    for (what, rev) in [("BASE", base), ("HEAD", head)] {
        if rev.trim().is_empty() {
            bail!("{what} is empty: name a branch or commit");
        }
        if !vcs.has_revision(dir, rev)? {
            bail!("{what} `{rev}` is not a commit in this repository; fetch it first");
        }
    }
    let body = match body_file {
        Some(p) if p == Path::new("-") => Some(
            std::io::read_to_string(std::io::stdin()).context("reading the PR body from stdin")?,
        ),
        Some(p) => {
            Some(std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?)
        }
        None => None,
    };
    let merge_base = vcs.merge_base(dir, base, head)?;
    let cfg = config_at(dir, &merge_base)?;
    let flags = check(dir, &merge_base, head, &TestFiles::new(&cfg.test_files)?)?;
    if flags.is_empty() {
        println!(
            "no weakened tests between {} and {head}",
            short(&merge_base)
        );
        return Ok(());
    }
    for flag in &flags {
        println!("{flag}");
    }
    if body.as_deref().is_some_and(explains_test_changes) {
        println!("explained by the body's `Test changes:` line");
        return Ok(());
    }
    bail!(
        "{} weakened-test flag(s); restore the tests, or explain each change on a line of the \
         PR body starting `Test changes:` and pass it with --body-file",
        flags.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_changes_line_must_say_something() {
        assert!(explains_test_changes(
            "Summary\n\nTest changes: dropped the flaky retry test"
        ));
        assert!(explains_test_changes(
            "- **Test changes:** merged two tests"
        ));
        assert!(!explains_test_changes("Test changes:"));
        assert!(!explains_test_changes("Test changes: **"));
        assert!(!explains_test_changes("No test changes: none"));
        assert!(!explains_test_changes(""));
    }
}
