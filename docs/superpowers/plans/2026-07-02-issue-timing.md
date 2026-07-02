# `--timing` Instrumentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add opt-in per-operation timing of subprocess/network IO to `issue` and `devrun`, surfacing where wall-clock time goes and how much the fan-outs overlap.

**Architecture:** A `tracing` layer in `devkit-common` collects flat spans (grouped by an `op` field) from the shared IO primitives — `cmd::capture`, `github::{graphql,rest_get_opt}`, a new `linear::send`, and `slack::{post_message,validate}`. Because the layer is the global default and derives overlap from each span's start-offset + duration, spans from `std::thread::scope` workers are aggregated with no parent propagation. The binaries hold a guard that prints a summary to stderr on exit.

**Tech Stack:** Rust 2024, `anyhow`, `clap`, `ureq`, new deps `tracing` + `tracing-subscriber`.

**Spec:** `docs/superpowers/specs/2026-07-02-issue-timing-design.md`

**Worktree note:** This worktree already contains *draft* versions of the Task 1–3 changes from the design phase (uncommitted). The plan is authoritative. If executing with fresh subagents, reset to the spec commit first (`git reset --hard ac6169f && git clean -fd crates/ Cargo.toml`) so each task starts clean; if executing inline, treat the draft as a starting point and still run each task's verification.

---

### Task 1: Timing module — deps, collector, aggregation, span builders

**Files:**
- Modify: `Cargo.toml` (workspace deps)
- Modify: `crates/devkit-common/Cargo.toml`
- Modify: `crates/devkit-common/src/lib.rs`
- Create: `crates/devkit-common/src/timing.rs`

- [ ] **Step 1: Add the workspace dependencies**

In `Cargo.toml`, under `[workspace.dependencies]`, after the `rpassword = "7"` line, add:

```toml
tracing = "0.1"
tracing-subscriber = { version = "0.3", default-features = false, features = ["std", "registry"] }
```

- [ ] **Step 2: Depend on them from devkit-common**

In `crates/devkit-common/Cargo.toml`, in `[dependencies]`, after `minijinja.workspace = true`, add:

```toml
tracing.workspace = true
tracing-subscriber.workspace = true
```

- [ ] **Step 3: Register the module**

In `crates/devkit-common/src/lib.rs`, add `pub mod timing;` in alphabetical position (between `pub mod template;` and `pub mod ui;`).

- [ ] **Step 4: Create `crates/devkit-common/src/timing.rs`**

Write the full module. It contains the pure aggregation (with unit tests), the span builders, `parse_env_mode`/`mode_from_env`, and the runtime (Collector/Layer/Guard/init):

```rust
//! Opt-in timing instrumentation for network and subprocess IO.
//!
//! When enabled, every subprocess spawn ([`crate::cmd::capture`]) and HTTP
//! request (GitHub / Linear / Slack) is wrapped in a `tracing` span tagged with
//! an `op` group and a `detail` string. A custom [`Layer`] collects each span as
//! it closes; on process exit the collected ops are summarised to stderr:
//!
//! ```text
//! timing — wall 1.31s, IO busy 0.95s over 16 ops (serial 2.85s, 3.0× overlap)
//!   op                  count   total     max     p50
//!   git fetch               1   0.62s   0.62s   0.62s
//!   linear graphql          2   0.58s   0.33s   0.25s
//!   git status              12   0.44s   0.09s   0.03s
//!   github REST             1   0.21s   0.21s   0.21s
//! ```
//!
//! [`Mode::Trace`] additionally lists every op with its start offset and thread,
//! and a log path streams one JSON record per op for cross-run comparison.
//!
//! Spans are kept flat — no parent/child nesting. The layer is installed as the
//! global default, so a span opened on any `std::thread::scope` worker is
//! collected identically, and the overlap metric is derived from each op's start
//! offset and duration. This sidesteps tracing's rule that a span context does
//! not follow `std::thread::spawn`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

/// All timing spans share this name; the layer ignores anything else.
const SPAN_NAME: &str = "devkit_io";

/// How much timing output to emit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// No instrumentation; `init` installs nothing and spans are inert.
    Off,
    /// Print the per-op summary table on exit.
    Summary,
    /// Summary table plus a per-op trace (start offset + thread).
    Trace,
}

/// Map a `DEVKIT_TIMING` value to a [`Mode`]. Unknown / unset → [`Mode::Off`].
pub fn parse_env_mode(v: Option<&str>) -> Mode {
    match v {
        Some("trace") => Mode::Trace,
        Some("summary" | "1" | "on") => Mode::Summary,
        _ => Mode::Off,
    }
}

/// The mode requested by the `DEVKIT_TIMING` env var (the flag-absent fallback).
pub fn mode_from_env() -> Mode {
    parse_env_mode(std::env::var("DEVKIT_TIMING").ok().as_deref())
}

// --- span constructors ------------------------------------------------------

/// A span for a subprocess spawn, grouped by program + subcommand.
pub fn subprocess_span(program: &str, args: &[&str]) -> tracing::Span {
    let (op, detail) = classify_subprocess(program, args);
    tracing::info_span!(SPAN_NAME, op = op.as_str(), detail = detail.as_str())
}

/// A span for an HTTP call, grouped by the caller-supplied `op`.
pub fn io_span(op: &str, detail: &str) -> tracing::Span {
    tracing::info_span!(SPAN_NAME, op = op, detail = detail)
}

/// Split a subprocess invocation into a coarse `op` group and a full `detail`
/// command line. `git`/`gh` group by their subcommand (skipping a leading
/// `-C <dir>` for git); anything else groups by the program name alone.
pub fn classify_subprocess(program: &str, args: &[&str]) -> (String, String) {
    let op = match subcommand(args) {
        Some(sub) => format!("{program} {sub}"),
        None => program.to_string(),
    };
    let detail = if args.is_empty() {
        program.to_string()
    } else {
        format!("{program} {}", args.join(" "))
    };
    (op, detail)
}

/// The first positional token, skipping flags and the value of `-C`/`-c`
/// (git's global dir / config flags, which precede the subcommand).
fn subcommand(args: &[&str]) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if *a == "-C" || *a == "-c" || *a == "--git-dir" {
            it.next(); // consume the flag's value
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        return Some((*a).to_string());
    }
    None
}

// --- collected data + aggregation (pure, no tracing) ------------------------

/// One completed IO op.
#[derive(Clone, Debug)]
pub struct Record {
    pub op: String,
    pub detail: String,
    /// Offset of this op's start from the collector's start.
    pub start: Duration,
    pub dur: Duration,
    pub thread: String,
}

/// Aggregated stats for one `op` group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpStat {
    pub op: String,
    pub count: usize,
    pub total: Duration,
    pub max: Duration,
    pub p50: Duration,
}

/// The full summary derived from a set of records.
#[derive(Clone, Debug)]
pub struct Summary {
    /// Per-op stats, sorted by total time descending.
    pub rows: Vec<OpStat>,
    /// Sum of every op's duration (the time IO would take run serially).
    pub serial_sum: Duration,
    /// Wall clock of the whole process (includes non-IO work).
    pub wall: Duration,
    /// Union of all op intervals — real time spent doing any IO.
    pub io_busy: Duration,
    /// The op group consuming the most total time, if any.
    pub hottest: Option<(String, Duration)>,
}

impl Summary {
    /// serial_sum / io_busy — how much concurrency the fan-out is buying.
    pub fn overlap(&self) -> f64 {
        if self.io_busy.is_zero() {
            return 1.0;
        }
        self.serial_sum.as_secs_f64() / self.io_busy.as_secs_f64()
    }
}

/// Group records by `op` and compute the [`Summary`]. `wall` is the whole
/// process wall clock, reported alongside the IO-only figures.
pub fn summarize(records: &[Record], wall: Duration) -> Summary {
    let mut groups: BTreeMap<&str, Vec<Duration>> = BTreeMap::new();
    for r in records {
        groups.entry(r.op.as_str()).or_default().push(r.dur);
    }
    let mut rows: Vec<OpStat> = groups
        .into_iter()
        .map(|(op, mut durs)| {
            durs.sort_unstable();
            let count = durs.len();
            let total: Duration = durs.iter().sum();
            let max = durs.iter().copied().max().unwrap_or_default();
            let p50 = durs[durs.len() / 2];
            OpStat {
                op: op.to_string(),
                count,
                total,
                max,
                p50,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.op.cmp(&b.op)));

    let serial_sum: Duration = records.iter().map(|r| r.dur).sum();
    let hottest = rows.first().map(|r| (r.op.clone(), r.total));
    Summary {
        rows,
        serial_sum,
        wall,
        io_busy: union_coverage(records),
        hottest,
    }
}

/// Total time during which at least one op was in flight (merged intervals).
fn union_coverage(records: &[Record]) -> Duration {
    let mut ivs: Vec<(Duration, Duration)> =
        records.iter().map(|r| (r.start, r.start + r.dur)).collect();
    ivs.sort_by_key(|iv| iv.0);
    let mut total = Duration::ZERO;
    let mut cur: Option<(Duration, Duration)> = None;
    for (s, e) in ivs {
        match cur {
            None => cur = Some((s, e)),
            Some((cs, ce)) if s <= ce => cur = Some((cs, ce.max(e))),
            Some((cs, ce)) => {
                total += ce - cs;
                cur = Some((s, e));
            }
        }
    }
    if let Some((cs, ce)) = cur {
        total += ce - cs;
    }
    total
}

// --- rendering --------------------------------------------------------------

fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s >= 1.0 {
        format!("{s:.2}s")
    } else if d.as_millis() >= 1 {
        format!("{}ms", d.as_millis())
    } else {
        format!("{}µs", d.as_micros())
    }
}

/// The summary table + headline, ready to print to stderr.
pub fn render_summary(s: &Summary) -> String {
    let mut out = String::new();
    let n: usize = s.rows.iter().map(|r| r.count).sum();
    out.push_str(&format!(
        "timing — wall {}, IO busy {} over {} op{} (serial {}, {:.1}× overlap)\n",
        fmt_dur(s.wall),
        fmt_dur(s.io_busy),
        n,
        if n == 1 { "" } else { "s" },
        fmt_dur(s.serial_sum),
        s.overlap(),
    ));
    let op_w = s.rows.iter().map(|r| r.op.len()).max().unwrap_or(2).max(2);
    out.push_str(&format!(
        "  {:<op_w$}  {:>5}  {:>7}  {:>7}  {:>7}\n",
        "op", "count", "total", "max", "p50"
    ));
    for r in &s.rows {
        out.push_str(&format!(
            "  {:<op_w$}  {:>5}  {:>7}  {:>7}  {:>7}\n",
            r.op,
            r.count,
            fmt_dur(r.total),
            fmt_dur(r.max),
            fmt_dur(r.p50),
        ));
    }
    if let Some((op, t)) = &s.hottest {
        out.push_str(&format!("  hottest: {op} ({})\n", fmt_dur(*t)));
    }
    out
}

/// A per-op trace, ordered by start offset — the [`Mode::Trace`] extra.
pub fn render_trace(records: &[Record]) -> String {
    let mut recs: Vec<&Record> = records.iter().collect();
    recs.sort_by_key(|r| r.start);
    let op_w = recs.iter().map(|r| r.op.len()).max().unwrap_or(2).max(2);
    let th_w = recs.iter().map(|r| r.thread.len()).max().unwrap_or(6).max(6);
    let mut out = String::from("trace (by start offset):\n");
    for r in recs {
        out.push_str(&format!(
            "  +{:>7}  {:>7}  {:<op_w$}  {:<th_w$}  {}\n",
            fmt_dur(r.start),
            fmt_dur(r.dur),
            r.op,
            r.thread,
            r.detail,
        ));
    }
    out
}

fn record_json(r: &Record) -> String {
    serde_json::json!({
        "op": r.op,
        "detail": r.detail,
        "start_ms": r.start.as_secs_f64() * 1000.0,
        "dur_ms": r.dur.as_secs_f64() * 1000.0,
        "thread": r.thread,
    })
    .to_string()
}

// --- runtime: collector, layer, init/guard ----------------------------------

/// Shared sink the layer writes to and the guard reads from on drop.
struct Collector {
    start: Instant,
    mode: Mode,
    records: Mutex<Vec<Record>>,
    log: Option<Mutex<File>>,
}

/// Per-span state stashed in the span's extensions between open and close.
struct SpanData {
    op: String,
    detail: String,
    start: Instant,
    thread: String,
}

#[derive(Default)]
struct FieldVisitor {
    op: String,
    detail: String,
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "op" => self.op = value.to_string(),
            "detail" => self.detail = value.to_string(),
            _ => {}
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let v = format!("{value:?}");
        let v = v.trim_matches('"');
        match field.name() {
            "op" if self.op.is_empty() => self.op = v.to_string(),
            "detail" if self.detail.is_empty() => self.detail = v.to_string(),
            _ => {}
        }
    }
}

struct TimingLayer(Arc<Collector>);

impl<S> Layer<S> for TimingLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if attrs.metadata().name() != SPAN_NAME {
            return;
        }
        let mut v = FieldVisitor::default();
        attrs.record(&mut v);
        let cur = std::thread::current();
        let thread = cur
            .name()
            .map(str::to_string)
            .unwrap_or_else(|| format!("{:?}", cur.id()));
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanData {
                op: v.op,
                detail: v.detail,
                start: Instant::now(),
                thread,
            });
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let ext = span.extensions();
        let Some(data) = ext.get::<SpanData>() else {
            return;
        };
        let rec = Record {
            op: data.op.clone(),
            detail: data.detail.clone(),
            start: data.start.saturating_duration_since(self.0.start),
            dur: data.start.elapsed(),
            thread: data.thread.clone(),
        };
        if let Some(log) = &self.0.log
            && let Ok(mut f) = log.lock()
        {
            let _ = writeln!(f, "{}", record_json(&rec));
        }
        if let Ok(mut recs) = self.0.records.lock() {
            recs.push(rec);
        }
    }
}

/// Kept alive for the process; prints the summary to stderr on drop.
pub struct Guard {
    collector: Option<Arc<Collector>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let Some(c) = &self.collector else { return };
        let wall = c.start.elapsed();
        let Ok(recs) = c.records.lock() else { return };
        if recs.is_empty() {
            eprintln!("timing: no IO recorded");
            return;
        }
        if c.mode == Mode::Trace {
            eprint!("{}", render_trace(&recs));
        }
        eprint!("{}", render_summary(&summarize(&recs, wall)));
    }
}

/// Install the global timing subscriber. Returns a [`Guard`] the caller holds
/// for the life of the program; dropping it prints the summary. A [`Mode::Off`]
/// call installs nothing and returns an inert guard, so timing off is a true
/// no-op. `log`, when set, receives one JSON record per op.
pub fn init(mode: Mode, log: Option<PathBuf>) -> Guard {
    if mode == Mode::Off {
        return Guard { collector: None };
    }
    let log = log.and_then(|p| match File::create(&p) {
        Ok(f) => Some(Mutex::new(f)),
        Err(e) => {
            eprintln!("warning: cannot open timing log {}: {e}", p.display());
            None
        }
    });
    let collector = Arc::new(Collector {
        start: Instant::now(),
        mode,
        records: Mutex::new(Vec::new()),
        log,
    });
    // Best-effort: if a global default is somehow already set, timing is simply
    // inert rather than a hard error.
    let subscriber = tracing_subscriber::registry().with(TimingLayer(collector.clone()));
    if tracing::subscriber::set_global_default(subscriber).is_err() {
        return Guard { collector: None };
    }
    Guard {
        collector: Some(collector),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(op: &str, start_ms: u64, dur_ms: u64) -> Record {
        Record {
            op: op.into(),
            detail: op.into(),
            start: Duration::from_millis(start_ms),
            dur: Duration::from_millis(dur_ms),
            thread: "t".into(),
        }
    }

    #[test]
    fn classify_git_skips_dir_flag() {
        let (op, detail) = classify_subprocess("git", &["-C", "/repo", "status", "--porcelain"]);
        assert_eq!(op, "git status");
        assert_eq!(detail, "git -C /repo status --porcelain");
    }

    #[test]
    fn classify_gh_uses_subcommand() {
        let (op, _) = classify_subprocess("gh", &["pr", "view", "42"]);
        assert_eq!(op, "gh pr");
    }

    #[test]
    fn classify_plain_program_is_name_only() {
        let (op, _) = classify_subprocess("bun", &["install"]);
        assert_eq!(op, "bun install");
        let (op, _) = classify_subprocess("doppler", &[]);
        assert_eq!(op, "doppler");
    }

    #[test]
    fn parse_env_mode_maps_values() {
        assert_eq!(parse_env_mode(Some("trace")), Mode::Trace);
        assert_eq!(parse_env_mode(Some("summary")), Mode::Summary);
        assert_eq!(parse_env_mode(Some("1")), Mode::Summary);
        assert_eq!(parse_env_mode(Some("0")), Mode::Off);
        assert_eq!(parse_env_mode(None), Mode::Off);
    }

    #[test]
    fn overlap_counts_concurrent_ops_once() {
        // Two ops fully overlapping: serial 180ms, but only 100ms of wall.
        let recs = [rec("a", 0, 100), rec("b", 0, 80)];
        let s = summarize(&recs, Duration::from_millis(120));
        assert_eq!(s.serial_sum, Duration::from_millis(180));
        assert_eq!(s.io_busy, Duration::from_millis(100));
        assert!((s.overlap() - 1.8).abs() < 1e-6);
        // Rows sorted by total desc; hottest is the longest op.
        assert_eq!(s.rows[0].op, "a");
        assert_eq!(s.hottest.as_ref().unwrap().0, "a");
    }

    #[test]
    fn disjoint_ops_have_no_overlap() {
        let recs = [rec("a", 0, 50), rec("b", 60, 40)];
        let s = summarize(&recs, Duration::from_millis(100));
        assert_eq!(s.io_busy, Duration::from_millis(90));
        assert_eq!(s.serial_sum, Duration::from_millis(90));
        assert!((s.overlap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn stats_aggregate_per_op() {
        let recs = [rec("x", 0, 10), rec("x", 5, 30), rec("x", 5, 20)];
        let s = summarize(&recs, Duration::from_millis(40));
        let x = &s.rows[0];
        assert_eq!(x.count, 3);
        assert_eq!(x.total, Duration::from_millis(60));
        assert_eq!(x.max, Duration::from_millis(30));
        assert_eq!(x.p50, Duration::from_millis(20)); // median of 10,20,30
    }

    #[test]
    fn fmt_dur_scales_units() {
        assert_eq!(fmt_dur(Duration::from_millis(1500)), "1.50s");
        assert_eq!(fmt_dur(Duration::from_millis(42)), "42ms");
        assert_eq!(fmt_dur(Duration::from_micros(300)), "300µs");
    }

    #[test]
    fn render_summary_shows_ops_and_headline() {
        let recs = [rec("git fetch", 0, 620), rec("linear graphql", 2, 330)];
        let out = render_summary(&summarize(&recs, Duration::from_millis(700)));
        assert!(out.contains("git fetch"));
        assert!(out.contains("linear graphql"));
        assert!(out.contains("overlap"));
        assert!(out.contains("hottest: git fetch"));
    }

    #[test]
    fn record_json_is_valid() {
        let v: serde_json::Value =
            serde_json::from_str(&record_json(&rec("git fetch", 5, 620))).unwrap();
        assert_eq!(v["op"], "git fetch");
        assert_eq!(v["dur_ms"], 620.0);
        assert_eq!(v["start_ms"], 5.0);
    }

    #[test]
    fn layer_collects_span_as_record() {
        let collector = Arc::new(Collector {
            start: Instant::now(),
            mode: Mode::Summary,
            records: Mutex::new(Vec::new()),
            log: None,
        });
        let subscriber = tracing_subscriber::registry().with(TimingLayer(collector.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let span = io_span("test op", "detail");
            let _e = span.enter();
        });
        let recs = collector.records.lock().unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].op, "test op");
        assert_eq!(recs[0].detail, "detail");
    }
}
```

- [ ] **Step 5: Run the timing tests**

Run: `cargo test -p devkit-common timing::`
Expected: PASS (10 tests: classify ×3, parse_env_mode, overlap ×2, stats, fmt_dur, render, record_json, layer_collects).

- [ ] **Step 6: Clippy + fmt**

Run: `cargo clippy -p devkit-common --all-targets -- -D warnings && cargo fmt --all`
Expected: no warnings, no diff.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/devkit-common/Cargo.toml crates/devkit-common/src/lib.rs crates/devkit-common/src/timing.rs
git commit -m "feat(timing): add tracing-based IO timing collector"
```

---

### Task 2: Route Linear GraphQL through one transport (no behavior change)

Refactors the six inline `ureq::post(...)` sites in `linear.rs` through a single `send()`. Pure refactor — no timing yet. Preserves `validate`'s raw-`ureq`-error propagation (no `.context`).

**Files:**
- Modify: `crates/devkit-common/src/linear.rs`

- [ ] **Step 1: Add `send` and update `post_graphql`**

Replace the existing `post_graphql` function (currently starting `fn post_graphql(query: &str, key: &str) -> Result<serde_json::Value> {`) with:

```rust
/// The single transport for every Linear GraphQL call: POST the body, decode the
/// JSON envelope. `detail` labels the call for timing (see [`crate::timing`]).
/// GraphQL-level error interpretation stays with each caller — this preserves
/// the raw `ureq` error so `validate` can downcast to distinguish an unreachable
/// host from a rejected key.
fn send(body: serde_json::Value, key: &str, detail: &str) -> Result<serde_json::Value> {
    let _ = detail; // consumed by the timing span added in a later task
    let v: serde_json::Value = ureq::post("https://api.linear.app/graphql")
        .set("Authorization", key)
        .send_json(body)?
        .into_json()?;
    Ok(v)
}

fn post_graphql(query: &str, key: &str, detail: &str) -> Result<serde_json::Value> {
    let v = send(ureq::json!({ "query": query }), key, detail)?;
    if let Some(errors) = v.get("errors").and_then(|e| e.as_array())
        && !errors.is_empty()
    {
        let msg = errors
            .first()
            .and_then(|e| e["message"].as_str())
            .unwrap_or("unknown GraphQL error");
        anyhow::bail!("Linear API error: {msg}");
    }
    Ok(v)
}
```

- [ ] **Step 2: Update the two `post_graphql` callers**

In `issue_pr`, change `let resp = post_graphql(&query, key)?;` to:
```rust
    let resp = post_graphql(&query, key, "issue_pr")?;
```
In `issues_by_number`, change `let resp = post_graphql(&issues_by_number_query(n), key)?;` to:
```rust
    let resp = post_graphql(&issues_by_number_query(n), key, "issues_by_number")?;
```

- [ ] **Step 3: Route `validate` through `send`**

Replace the body of `validate` (the `let resp: serde_json::Value = ureq::post(...)...into_json()?;` statement) with:
```rust
    let resp = send(
        ureq::json!({
            "query": "query { viewer { email } organization { urlKey name } }"
        }),
        key,
        "validate",
    )?;
    parse_identity(&resp)
```

- [ ] **Step 4: Route `fetch_url_key` through `send`**

Replace its `let resp: serde_json::Value = ureq::post(...)...into_json()?;` with:
```rust
    let resp = send(
        ureq::json!({ "query": "query { organization { urlKey } }" }),
        key,
        "workspace_url_key",
    )?;
    Ok(resp["data"]["organization"]["urlKey"]
        .as_str()
        .map(String::from))
```

- [ ] **Step 5: Route `fetch` (states) through `send`**

Replace its leading `let resp: serde_json::Value = ureq::post(...)...into_json()?;` with:
```rust
    let resp = send(ureq::json!({ "query": query }), key, "states")?;
```
(Leave the rest of `fetch` — the `out` HashMap build — unchanged.)

- [ ] **Step 6: Route the `assigned_issue_history_with_progress` loop through `send`**

Inside the `loop`, replace `let resp: serde_json::Value = ureq::post(...).send_json(ureq::json!({ "query": assigned_query(after.as_deref()) }))?.into_json()?;` with:
```rust
        let resp = send(
            ureq::json!({ "query": assigned_query(after.as_deref()) }),
            key,
            "assigned_history",
        )?;
```

- [ ] **Step 7: Route `viewer_created_at` through `send`**

Replace its `let resp: serde_json::Value = ureq::post(...)...into_json()?;` with:
```rust
    let resp = send(
        ureq::json!({ "query": "query { viewer { createdAt } }" }),
        key,
        "viewer",
    )?;
    resp["data"]["viewer"]["createdAt"]
        .as_str()
        .map(String::from)
        .context("viewer.createdAt missing from Linear response")
```

- [ ] **Step 8: Verify no behavior change**

Run: `cargo test -p devkit-common linear::`
Expected: PASS (all existing linear parse tests green; no new failures).

Run: `cargo clippy -p devkit-common --all-targets -- -D warnings`
Expected: no warnings (note: `send`'s `detail` param is intentionally unused here — the `let _ = detail;` line keeps it explicit and warning-free).

- [ ] **Step 9: Commit**

```bash
git add crates/devkit-common/src/linear.rs
git commit -m "refactor(linear): route graphql calls through one transport"
```

---

### Task 3: Instrument the primitives

Wrap each IO choke point in a timing span. All are one-line additions.

**Files:**
- Modify: `crates/devkit-common/src/cmd.rs`
- Modify: `crates/devkit-common/src/github.rs`
- Modify: `crates/devkit-common/src/linear.rs`
- Modify: `crates/devkit-common/src/slack.rs`

- [ ] **Step 1: Instrument `cmd::capture`**

In `capture`, immediately after the `pub fn capture(...) -> Result<String> {` line, add as the first statement:
```rust
    let _span = crate::timing::subprocess_span(program, args).entered();
```

- [ ] **Step 2: Instrument GitHub's two choke points**

In `github::graphql`, after `pub fn graphql(query: &str) -> Result<Value> {`, add as the first statement:
```rust
    let _span = crate::timing::io_span("github graphql", "graphql").entered();
```
In `github::rest_get_opt`, after `pub fn rest_get_opt(path: &str) -> Result<Option<Value>> {`, add as the first statement:
```rust
    let _span = crate::timing::io_span("github REST", path).entered();
```

- [ ] **Step 3: Instrument `linear::send`**

In `linear::send` (from Task 2), replace the `let _ = detail; // consumed by the timing span added in a later task` line with:
```rust
    let _span = crate::timing::io_span("linear graphql", detail).entered();
```

- [ ] **Step 4: Instrument Slack**

In `slack::post_message`, after `pub fn post_message(token: &str, channel: &str, text: &str) -> Result<()> {`, add as the first statement:
```rust
    let _span = crate::timing::io_span("slack", "chat.postMessage").entered();
```
In `slack::validate`, after `pub fn validate(token: &str) -> Result<SlackIdentity> {`, add as the first statement:
```rust
    let _span = crate::timing::io_span("slack", "auth.test").entered();
```

- [ ] **Step 5: Verify the workspace still builds and tests pass**

Run: `cargo test --workspace`
Expected: PASS (327 existing + the new timing tests; no regressions).

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: no warnings, no diff.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-common/src/cmd.rs crates/devkit-common/src/github.rs crates/devkit-common/src/linear.rs crates/devkit-common/src/slack.rs
git commit -m "feat(timing): instrument subprocess and http primitives"
```

---

### Task 4: Wire `--timing` / `--timing-log` into issue and devrun

**Files:**
- Modify: `src/bin/issue/main.rs`
- Modify: `src/bin/devrun/main.rs`

- [ ] **Step 1: Add the shared CLI helper snippet to `issue`**

In `src/bin/issue/main.rs`, add `use std::path::PathBuf;` to the imports, and after the `use clap_complete::Shell;` line add this enum + helper:

```rust
/// `--timing` verbosity, parsed by clap. `--timing` alone = summary,
/// `--timing=trace` = per-op detail.
#[derive(Clone, Copy, clap::ValueEnum)]
enum TimingFlag {
    Summary,
    Trace,
}

/// Resolve the timing mode: the flag wins; otherwise fall back to `DEVKIT_TIMING`.
fn timing_mode(flag: Option<TimingFlag>) -> devkit_common::timing::Mode {
    use devkit_common::timing::Mode;
    match flag {
        Some(TimingFlag::Summary) => Mode::Summary,
        Some(TimingFlag::Trace) => Mode::Trace,
        None => devkit_common::timing::mode_from_env(),
    }
}
```

- [ ] **Step 2: Add the flags to `issue`'s `Cli` struct**

In the `struct Cli { ... }` block, after the `config: Option<String>,` field, add:

```rust
    /// Print IO timing to stderr. `--timing` = summary, `--timing=trace` = per-op.
    #[arg(long, global = true, value_name = "MODE", num_args = 0..=1, default_missing_value = "summary")]
    timing: Option<TimingFlag>,
    /// Write one JSON record per timed IO op to FILE.
    #[arg(long = "timing-log", global = true, value_name = "FILE")]
    timing_log: Option<PathBuf>,
```

- [ ] **Step 3: Initialise timing in `issue`'s `main`**

In `fn main()`, immediately after `let cli = Cli::parse();`, add:

```rust
    let _timing = devkit_common::timing::init(timing_mode(cli.timing), cli.timing_log.clone());
```

(The guard lives to the end of `main`, printing on every return path.)

- [ ] **Step 4: Repeat for `devrun`**

In `src/bin/devrun/main.rs`: add `use std::path::PathBuf;` if not already imported; add the same `TimingFlag` enum + `timing_mode` helper (copy the block from Step 1 verbatim); add the same two fields after `config: Option<String>,` in its `struct Cli`; and after `let cli = Cli::parse();` in its `main`, add:

```rust
    let _timing = devkit_common::timing::init(timing_mode(cli.timing), cli.timing_log.clone());
```

- [ ] **Step 5: Build and smoke-test both binaries**

Run: `cargo build`
Expected: success.

Run: `cargo run --bin issue -- completions bash >/dev/null && echo OK`
Expected: `OK` (bare invocation still works; no timing output because the flag is absent and `DEVKIT_TIMING` is unset).

Run: `DEVKIT_TIMING=summary cargo run --bin issue -- completions bash >/dev/null`
Expected: stderr shows `timing: no IO recorded` (completions does no IO — proves the guard fires and stdout is untouched).

Run: `cargo run --bin issue -- --help | grep -- --timing`
Expected: the `--timing` and `--timing-log` flags appear in help.

- [ ] **Step 6: Full gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS, no warnings, no diff.

- [ ] **Step 7: Commit**

```bash
git add src/bin/issue/main.rs src/bin/devrun/main.rs
git commit -m "feat: add --timing / --timing-log to issue and devrun"
```

---

### Task 5: Document `--timing`

**Files:**
- Modify: `README.md`
- Modify: `AGENTS.md`

- [ ] **Step 1: Add a README section**

In `README.md`, add a short subsection under the `issue`/`devrun` documentation describing the flags. Use this content:

```markdown
### Timing (`--timing`)

`issue` and `devrun` accept a global `--timing` flag that prints a per-operation
breakdown of subprocess and network IO to stderr on exit:

- `--timing` (or `--timing=summary`) — a table of ops (`git fetch`, `github REST`,
  `linear graphql`, …) with count / total / max / p50, plus a headline showing
  wall time, IO-busy time, serial sum, and the concurrency factor the parallel
  fan-outs achieve.
- `--timing=trace` — additionally lists every op with its start offset, thread,
  and full command line.
- `--timing-log <FILE>` — streams one JSON record per op (`op`, `detail`,
  `start_ms`, `dur_ms`, `thread`) for comparing runs.

`DEVKIT_TIMING=summary|trace` enables the summary/trace form without the flag.
stdout (tables, `--json`) is never affected. Example:

    issue status --timing
```

- [ ] **Step 2: Add an AGENTS.md note**

In `AGENTS.md`, under the `issue`/`devrun` bin descriptions or Conventions, add one line noting the shared instrumentation:

```markdown
- **Timing:** `issue`/`devrun` accept `--timing[=trace]` / `--timing-log <FILE>`
  (or `DEVKIT_TIMING`). Timing wraps the shared IO primitives (`cmd::capture`,
  `github`, `linear::send`, `slack`) via `devkit-common::timing`; a global tracing
  layer aggregates flat spans by op and prints a stderr summary on exit. `devkitd`
  carries the same spans but has no activation flag yet.
```

- [ ] **Step 3: Commit**

```bash
git add README.md AGENTS.md
git commit -m "docs: document --timing"
```

---

## Manual verification (after all tasks)

Run against a real issue worktree with network access:

```bash
issue status --timing
issue status --timing=trace
issue dashboard --timing --timing-log /tmp/dash-timing.jsonl
```

Confirm: the summary prints to stderr; overlap is > 1× on `status`/`dashboard`
(the fan-outs are concurrent); stdout tables are unchanged; the JSON log has one
line per op.

## Self-review notes

- **Spec coverage:** primitives instrumented (Task 3) ✓; Linear transport refactor
  preserving `validate` semantics (Task 2) ✓; tracing layer + pure aggregation +
  env parse (Task 1) ✓; CLI on issue + devrun, devkitd deferred (Task 4) ✓;
  JSON-lines log (Task 1 `record_json` + Task 4 flag) ✓; docs (Task 5) ✓.
- **Type consistency:** `Mode`, `timing::init(Mode, Option<PathBuf>) -> Guard`,
  `io_span(&str,&str)`, `subprocess_span(&str,&[&str])`, `mode_from_env()`,
  `TimingFlag` used consistently across tasks.
- **No placeholders:** every code step shows complete code.
```
