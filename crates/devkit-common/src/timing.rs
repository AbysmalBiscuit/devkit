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
    let op = match subcommand(args).filter(|_| program == "git" || program == "gh") {
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
    let th_w = recs
        .iter()
        .map(|r| r.thread.len())
        .max()
        .unwrap_or(6)
        .max(6);
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
        assert_eq!(op, "bun");
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
