# Design: `--timing` IO/network instrumentation

Date: 2026-07-02
Branch: `feat/issue-timing`
Status: approved

## Problem

`issue` (and the other CLIs) spend most of their wall-clock time in subprocess
spawns (git/gh) and HTTP calls (GitHub/Linear/Slack). Recent perf work
parallelized several fan-outs, but there is no visibility into where the time
goes or how much the concurrency actually buys. A single elapsed timer would
mislead: a fanned-out command's wall time is the `max()` of its concurrent
branches, not their sum. We need per-op timing that also exposes concurrency, to
target future perf work.

## Goals

- Attribute wall-clock time to each network/subprocess operation.
- Show how much parallelism the fan-outs are buying (serial-sum vs. real busy time).
- Optional structured output for comparing runs over time.
- Zero cost when disabled; no change to stdout (human tables / `--json`).

## Non-goals

- Profiling CPU/compute (these tools do little).
- A persistent metrics store or external exporter.
- Distributed tracing / span nesting across processes.

## Decisions

| Question | Decision |
|---|---|
| Scope | CLI flags on `issue` and `devrun`. `devkitd` deferred (no clap CLI; signal-terminated so the Drop guard won't print). Its primitives still carry inert spans, so it can opt in later. |
| Instrument where | Shared primitives, not per-subcommand code. |
| Backend | `tracing` + a custom aggregating `Layer`. |
| Verbosity | Summary table default; per-op list behind `--timing=trace`. |
| Structured output | `--timing-log <FILE>`, JSON-lines (one record per op). |
| git/gh grouping | By program + subcommand (`git status`, `gh pr`). |
| Summary columns | `op / count / total / max / p50` (keep p50). |
| Env | `DEVKIT_TIMING=summary\|trace\|1` as a fallback when the flag is absent; the flag wins when present. No override kill-switch. |

## Architecture

### Why instrument primitives, not subcommands

Every subprocess spawn funnels through `cmd::capture`. GitHub HTTP funnels
through `github::graphql` + `rest_get_opt`. Linear and Slack each construct their
own `ureq::post` inline — Linear at 6 sites, Slack at 2 — so those are refactored
through one shared transport first. Instrumenting these ~5 choke points covers
all seven `issue` subcommands and `devrun` with no per-command code, and leaves
the spans in place for `devkitd` to activate later.

### Why flat spans (the key design choice)

All concurrent fan-out in the codebase uses `std::thread::scope` + `s.spawn`
(no async runtime). A `tracing` span's context does **not** propagate across
`std::thread::spawn`, so a naive parent/child span tree would lose every
worker-thread op.

Instead the timing layer is installed as the **global default** subscriber and
groups spans by an `op` field, deriving concurrency purely from each span's
recorded (start-offset, duration). A span opened on any worker thread reaches the
same global layer and is aggregated identically — no parent context needed. This
both avoids the cross-thread gotcha and keeps the aggregation a pure function,
testable without any tracing global state.

### Module: `devkit-common/src/timing.rs`

- `enum Mode { Off, Summary, Trace }`.
- Span builders:
  - `subprocess_span(program, args) -> tracing::Span`
  - `io_span(op, detail) -> tracing::Span`
  - Span name `"devkit_io"`, fields `op` (group key) and `detail` (full command / URL path).
- `classify_subprocess(program, args) -> (op, detail)` — pure; git/gh group by
  the first positional token (skipping a leading `-C <dir>` / `-c <k=v>`), else
  the program name. `detail` is the full command line.
- Pure aggregation (unit-tested, no tracing):
  - `Record { op, detail, start: Duration, dur: Duration, thread }`
  - `OpStat { op, count, total, max, p50 }`
  - `Summary { rows, serial_sum, wall, io_busy, hottest }`, `Summary::overlap()`
  - `summarize(records, wall) -> Summary`; `union_coverage` merges op intervals
    to get real IO-busy time.
  - `render_summary`, `render_trace`, `record_json`.
- Runtime:
  - `Collector { start: Instant, mode, records: Mutex<Vec<Record>>, log: Option<Mutex<File>> }`
  - `TimingLayer(Arc<Collector>)`: `on_new_span` stashes op/detail/start/thread in
    the span's extensions; `on_close` computes offset+duration, pushes a `Record`,
    and streams a JSON line if a log is configured.
  - `init(mode, log) -> Guard`; `Guard::drop` prints the summary (and trace) to
    stderr. `Mode::Off` installs nothing and returns an inert guard.

### Output

To **stderr** (matching the existing spinner/warning convention; stdout is left
entirely to the human tables and ad-hoc `--json`):

```
timing — wall 1.31s, IO busy 0.95s over 16 ops (serial 2.85s, 3.0× overlap)
  op               count   total     max     p50
  git fetch            1   0.62s   0.62s   0.62s
  linear graphql       2   0.58s   0.33s   0.25s
  git status          12   0.44s   0.09s   0.03s
  github REST          1   0.21s   0.21s   0.21s
  hottest: git fetch (0.62s)
```

`--timing=trace` additionally emits a per-op list ordered by start offset:
`+<offset>  <dur>  <op>  <thread>  <full command>`.

`--timing-log <FILE>` writes one JSON object per op:
`{"op","detail","start_ms","dur_ms","thread"}`.

### CLI wiring

Each of `issue` and `devrun` gains two global flags:

- `--timing[=summary|trace]` — `default_missing_value = "summary"`, so bare
  `--timing` is summary and `--timing=trace` is the verbose form.
- `--timing-log <FILE>`.

Each `main` resolves the mode (flag, else `DEVKIT_TIMING`, else off) and binds
`let _timing = devkit_common::timing::init(mode, log)` before dispatch. The guard
drops at end of scope, printing on both success and error-return paths.

`devkitd` is out of scope for this change: it has no clap CLI (it hand-parses
`argv[1]`) and is terminated by signal, so a `Drop`-based summary would rarely
print. Its instrumented primitives carry inert spans until it opts in later.

## Regression risk / invariant

`linear::validate` and `slack::validate` currently keep the raw `ureq` error as
the top-level error (no `.context`) so a caller can downcast it to distinguish an
unreachable host from a rejected credential. The shared `linear::send()` must do
transport only (`send_json(..)?.into_json()?`) and propagate that error
uncontexted; GraphQL-level error interpretation (the `errors[]` check, identity
parsing) stays at each call site. A test asserts the downcast still works.

## Known limitation

The summary prints from a `Drop` guard. `issue` and `devrun` return from `main`,
so they always print (including on error return). The release profile sets
`panic = "abort"`, so a panic skips the guard — acceptable, since a panicking
run's timing is not the priority. (This `Drop`-on-signal gap is also why
`devkitd`, which exits via SIGTERM, is out of scope.)

## Testing

TDD, pure unit tests in `timing.rs`:
- `classify_subprocess`: git skips `-C <dir>`; gh subcommand; plain program.
- `summarize`: overlapping ops counted once (`io_busy < serial_sum`); disjoint
  ops no overlap; per-op count/total/max/p50; `hottest` = longest op.
- `fmt_dur` unit scaling; `record_json` round-trips.
- `parse_env_mode`: `DEVKIT_TIMING` string → `Mode`.
- End-to-end layer test: open/close an `io_span` under a scoped subscriber, assert
  one `Record` with the right `op` is collected.

The Linear `validate` downcast invariant is preserved by construction — `send`
uses the same `?` conversion the call sites used and adds no `.context`, so the
raw `ureq` error still propagates. It isn't unit-tested (needs a live transport
error); the existing GraphQL-error parse tests stay green and cover the call-site
error path.

Gate: `cargo test --workspace` (327 tests must stay green), `cargo clippy
--workspace --all-targets -- -D warnings`, `cargo fmt --all`.

Manual: `issue status --timing` and `--timing=trace` against a real worktree —
confirm overlap > 1× and that stdout is unchanged.

## Deps

`tracing = "0.1"` and `tracing-subscriber = { default-features = false, features
= ["std","registry"] }` added to `devkit-common` (only crate that needs them; the
binaries call `timing::init` and hold the guard).

## Task breakdown (per-task commits)

1. `feat(timing): add tracing-based IO timing collector` — deps + `timing.rs` + `lib.rs`.
2. `refactor(linear): route graphql calls through one transport` — `send()` + 6
   call sites; no behavior change; preserves `validate` error semantics.
3. `feat(timing): instrument subprocess and http primitives` — `capture`,
   `github`×2, `linear::send`, `slack`×2.
4. `feat: add --timing / --timing-log to issue and devrun` — CLI + init wiring.
5. `docs: document --timing` — README / AGENTS note.

## Open questions

None outstanding — all resolved during brainstorming.
