//! Time the analyzer over one or more shell-call corpora and name the slowest
//! inputs. Local measurement only: pass `corpus.jsonl` files from
//! `data_analysis.local`; identical calls across snapshots are timed once.
//!
//! ```sh
//! cargo run -p devkit-command --release --features corpus --example corpus_bench -- \
//!     data_analysis.local/outputs*/corpus.jsonl
//! ```

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use devkit_command::{Context, Dialect, Limits, PathStyle};

const RUNS: usize = 3;
const SLOWEST: usize = 10;

struct Sample {
    elapsed: Duration,
    corpus: String,
    row: usize,
    cohort: String,
}

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    assert!(!paths.is_empty(), "usage: corpus_bench <corpus.jsonl>...");
    let mut seen = HashSet::new();
    let mut samples = Vec::new();
    for path in &paths {
        let body = std::fs::read_to_string(path).expect("read corpus");
        for (row, line) in body.lines().enumerate() {
            let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let field = |key| record.get(key).and_then(|v| v.as_str());
            let Some(command) = field("command") else {
                continue;
            };
            let harness = field("harness").unwrap_or("?");
            let platform = field("platform").unwrap_or("?");
            let tool = field("tool_name").unwrap_or("Bash");
            if !seen.insert(format!("{harness}\0{platform}\0{tool}\0{command}")) {
                continue;
            }
            let dialect = if tool == "PowerShell" || (harness == "codex" && platform == "windows") {
                Dialect::PowerShell
            } else {
                Dialect::Bash
            };
            let ctx = Context {
                dialect,
                cwd: field("cwd").map(str::to_string),
                path_style: if platform == "windows" && dialect == Dialect::PowerShell {
                    PathStyle::Windows
                } else {
                    PathStyle::Unix
                },
                limits: Limits::default(),
            };
            // The fastest of several runs separates the input's cost from
            // scheduler noise.
            let elapsed = (0..RUNS)
                .map(|_| {
                    let start = Instant::now();
                    std::hint::black_box(devkit_command::analyze(command, &ctx));
                    start.elapsed()
                })
                .min()
                .expect("at least one run");
            samples.push(Sample {
                elapsed,
                corpus: path.clone(),
                row,
                cohort: format!("{harness}/{platform} {tool}"),
            });
        }
    }
    assert!(!samples.is_empty(), "no commands found");

    samples.sort_by_key(|s| std::cmp::Reverse(s.elapsed));
    let n = samples.len();
    let total: Duration = samples.iter().map(|s| s.elapsed).sum();
    let percentile = |p: f64| samples[n - 1 - ((n as f64 * p) as usize).min(n - 1)].elapsed;
    println!("unique calls {n}, fastest of {RUNS} runs each");
    println!(
        "total {total:.2?}  mean {:.1?}  p50 {:.1?}  p90 {:.1?}  p99 {:.1?}  p99.9 {:.1?}  max {:.1?}",
        total / n as u32,
        percentile(0.5),
        percentile(0.9),
        percentile(0.99),
        percentile(0.999),
        samples[0].elapsed,
    );
    println!("\nslowest (row is the 0-based line index):");
    for s in samples.iter().take(SLOWEST) {
        println!(
            "{:>10.1?}  {} row {}  {}",
            s.elapsed, s.corpus, s.row, s.cohort
        );
    }
}
