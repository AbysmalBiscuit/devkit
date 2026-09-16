//! Run the analyzer over a frozen shell-call corpus and print how the default
//! write policy would treat it. Local measurement only: pass the path to a
//! `corpus.jsonl` — one `devkit hook-log path` produces, or one an external
//! capture script wrote — and nothing is written anywhere.
//!
//! ```sh
//! cargo run -p devkit-command --release --features corpus --example corpus_probe -- <corpus.jsonl>
//! cargo run -p devkit-command --release --features corpus --example corpus_probe -- --diff <corpus.jsonl>
//! ```
//!
//! `--diff` re-analyses each record that carries a recorded projection and
//! reports where this binary disagrees with it. With `analyzer_version` stamped
//! per record, that turns an accumulated corpus into a regression gate to run
//! before shipping a parser change.
//!
//! Output format is deliberately unstable: this lives in an example behind the
//! `corpus` feature so it is free to churn.

use std::collections::BTreeMap;

use devkit_command::{
    ANALYZER_VERSION, Context, Dialect, Limits, PathStyle, Target, UncertaintyKind,
    corpus::{command_of, dialect_of, infer_dialect},
};

/// A line that does not parse costs one sample. The reader says how many it
/// skipped rather than failing: append atomicity is relied on but not
/// guaranteed, and a torn record is the expected shape of that.
struct Corpus {
    records: Vec<serde_json::Value>,
    skipped: usize,
}

fn read(path: &str) -> Corpus {
    let body = std::fs::read_to_string(path).expect("read corpus");
    let mut records = Vec::new();
    let mut skipped = 0;
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str(line) {
            Ok(v) => records.push(v),
            Err(_) => skipped += 1,
        }
    }
    Corpus { records, skipped }
}

/// The context a record was analysed in, preferring what it recorded over what
/// can be guessed from it.
fn context_of(record: &serde_json::Value) -> Context {
    let field = |key: &str, fallback: &'static str| {
        record
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(fallback)
            .to_string()
    };
    let harness = field("harness", "?");
    let platform = field("platform", "?");
    let tool = field("tool_name", "Bash");
    let dialect = dialect_of(record).unwrap_or_else(|| infer_dialect(&harness, &platform, &tool));
    Context {
        dialect,
        cwd: record
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        path_style: if platform == "windows" && dialect == Dialect::PowerShell {
            PathStyle::Windows
        } else {
            PathStyle::Unix
        },
        limits: Limits::default(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let diff = args.iter().any(|a| a == "--diff");
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .expect("usage: corpus_probe [--diff] <corpus.jsonl>");
    let corpus = read(path);
    if corpus.skipped > 0 {
        println!("skipped {} line(s) that did not parse\n", corpus.skipped);
    }
    if diff {
        return regression_diff(&corpus);
    }
    let mut cohorts: BTreeMap<String, [usize; 7]> = BTreeMap::new();
    let mut details: BTreeMap<String, usize> = BTreeMap::new();
    for record in &corpus.records {
        let Some(command) = command_of(record) else {
            continue;
        };
        let harness = record
            .get("harness")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let platform = record
            .get("platform")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let tool = record
            .get("tool_name")
            .and_then(|v| v.as_str())
            .unwrap_or("Bash");
        let ctx = context_of(record);
        let a = devkit_command::analyze(command, &ctx);
        let unresolved = a
            .file_effects
            .iter()
            .any(|e| e.target == Target::Unresolved)
            || a.uncertainties.iter().any(|u| {
                matches!(
                    u.kind,
                    UncertaintyKind::UnresolvedWrite
                        | UncertaintyKind::ParseError
                        | UncertaintyKind::LimitExhausted(_)
                        | UncertaintyKind::UnresolvedInvocation
                )
            });
        let unsupported = a
            .uncertainties
            .iter()
            .any(|u| matches!(u.kind, UncertaintyKind::UnsupportedLanguage(_)));
        let row = cohorts
            .entry(format!("{harness}/{platform} {tool}"))
            .or_default();
        row[0] += 1;
        row[1] += usize::from(
            a.file_effects
                .iter()
                .any(|e| matches!(e.target, Target::Path(_))),
        );
        row[2] += usize::from(!a.tree_effects.is_empty());
        row[3] += usize::from(unresolved);
        row[4] += usize::from(unsupported);
        row[5] += usize::from(!a.script_files.is_empty());
        row[6] += usize::from(unresolved || unsupported);
        for u in &a.uncertainties {
            let shape: String = u
                .detail
                .split('`')
                .enumerate()
                .map(|(i, part)| if i % 2 == 1 { "`_`" } else { part })
                .collect();
            *details.entry(shape).or_default() += 1;
        }
    }
    println!(
        "cohort | calls | claims | tree | unresolved | unsupported | script files | refused by default"
    );
    for (cohort, r) in &cohorts {
        println!(
            "{cohort} | {} | {} | {} | {} | {} | {} | {}",
            r[0], r[1], r[2], r[3], r[4], r[5], r[6]
        );
    }
    let mut ranked: Vec<_> = details.into_iter().collect();
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    println!("\nmost common uncertainty shapes (names elided):");
    for (shape, n) in ranked.into_iter().take(20) {
        println!("{n:>6}  {shape}");
    }
}

/// Re-analyse every record that carries a projection, and report where this
/// binary disagrees with what was recorded.
///
/// A record whose `analyzer_version` differs from this binary's is a regression
/// *candidate*, not stale data: the corpus is still the evidence, and a
/// disagreement is either the improvement the bump was for or the regression it
/// brought. Both are listed, separated by whether the stamp moved.
fn regression_diff(corpus: &Corpus) {
    let mut same_version = Vec::new();
    let mut across_versions = Vec::new();
    let mut compared = 0usize;
    for record in &corpus.records {
        let (Some(command), Some(recorded)) = (command_of(record), record.get("analysis")) else {
            continue;
        };
        compared += 1;
        let fresh = devkit_command::analyze(command, &context_of(record));
        let mut fresh_writes: Vec<&str> = fresh
            .file_effects
            .iter()
            .filter_map(|e| match &e.target {
                Target::Path(p) => Some(p.as_str()),
                _ => None,
            })
            .collect();
        fresh_writes.sort_unstable();
        let mut was: Vec<&str> = recorded["resolved_writes"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        was.sort_unstable();
        let fresh_unresolved = fresh
            .file_effects
            .iter()
            .filter(|e| e.target == Target::Unresolved)
            .count();
        let was_unresolved = recorded["counts"]["unresolved_writes"]
            .as_u64()
            .unwrap_or_default() as usize;
        if fresh_writes == was && fresh_unresolved == was_unresolved {
            continue;
        }
        let stamp = record["analyzer_version"].as_u64().unwrap_or_default() as u32;
        let line = format!(
            "  v{stamp} -> v{ANALYZER_VERSION}  resolved {} -> {}, unresolved {was_unresolved} -> {fresh_unresolved}\n    {command}",
            was.len(),
            fresh_writes.len()
        );
        if stamp == ANALYZER_VERSION {
            same_version.push(line);
        } else {
            across_versions.push(line);
        }
    }
    println!("compared {compared} record(s) against ANALYZER_VERSION {ANALYZER_VERSION}");
    // The list that matters: the stamp says the semantics did not change, so
    // every disagreement here is a regression.
    println!(
        "\n{} disagreement(s) at the same analyzer version — each is a regression:",
        same_version.len()
    );
    for line in &same_version {
        println!("{line}");
    }
    println!(
        "\n{} disagreement(s) across an analyzer version bump — expected where the bump was the point:",
        across_versions.len()
    );
    for line in across_versions.iter().take(40) {
        println!("{line}");
    }
}
