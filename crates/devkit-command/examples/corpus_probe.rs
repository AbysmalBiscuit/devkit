//! Run the analyzer over a frozen shell-call corpus and print how the default
//! write policy would treat it. Local measurement only: pass the path to a
//! `corpus.jsonl` from `data_analysis.local`; nothing is written anywhere.

use std::collections::BTreeMap;

use devkit_command::{Context, Dialect, Limits, PathStyle, Target, UncertaintyKind};

fn command_of(record: &serde_json::Value) -> Option<&str> {
    record.get("command").and_then(|c| c.as_str()).or_else(|| {
        record
            .as_object()?
            .values()
            .find_map(|v| v.get("command").and_then(|c| c.as_str()))
    })
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: corpus_probe <corpus.jsonl>");
    let body = std::fs::read_to_string(&path).expect("read corpus");
    let mut cohorts: BTreeMap<String, [usize; 7]> = BTreeMap::new();
    let mut details: BTreeMap<String, usize> = BTreeMap::new();
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let record: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");
        let Some(command) = command_of(&record) else {
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
        let dialect = if tool == "PowerShell" || (harness == "codex" && platform == "windows") {
            Dialect::PowerShell
        } else {
            Dialect::Bash
        };
        let ctx = Context {
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
        };
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
