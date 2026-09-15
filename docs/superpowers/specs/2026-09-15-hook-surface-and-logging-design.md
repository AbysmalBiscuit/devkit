# Hook surface unification and harness logging

Two parts, one change. Part A restructures how harness events enter devkit; part B adds the logging subsystem that hangs off that surface. A is a pure refactor with no behaviour change and lands first, so B is written against a surface that is not moving.

## Problem

`devkit harness shell` analyses every shell command a coding agent runs, and then discards everything except the verdict. Nothing persists what agents tried to run, what devkit decided, or why. `crates/devkit-command/examples/corpus_probe.rs` already exists as the offline consumer of exactly that data, reading a `corpus.jsonl` produced by an external capture script rather than by devkit.

The command text alone is the weaker half. The hook holds the `Analysis` and the verdict together at the moment of decision, and that pairing is what ranks which parser to fix next, and what distinguishes a good denial from a false positive.

Alongside it, several operational failures are invisible today: write-stage deadline misses, lock contention, analyser latency, and rule fire counts.

## Part A: hook surface unification

### Why the current cut does not hold

Hook entry points are split by subsystem: shell events under `harness`, file edits under `lockm`. That split is already broken in the code. `harness shell` calls `writes::enforce` to claim locks, so the shell hook is a locks consumer as well as a ports and tasks consumer.

One harness event needs several subsystems to answer it. The cut that survives that is by caller: everything a harness sends enters one verb family, and the dispatch inside reads `tool_name` to pick the path.

### Verb mapping

| Today | After |
|---|---|
| `devkit harness shell` | `devkit hook pre-tool-use` |
| `lockm hook pretooluse` | `devkit hook pre-tool-use` |
| `lockm hook subagent-stop` | `devkit hook subagent-stop` |
| `lockm hook session-end` | `devkit hook session-end` |

Both `PreToolUse` rows collapse onto one verb. The payload's own `tool_name` decides shell path or edit path, which is where that decision belongs; the two separate matcher blocks in the manifests exist only because two subsystems answered the same event.

The family then grows to cover what the three harnesses actually offer. Each verb maps one-to-one onto a vendor event, so the manifest stays a translation table with no logic in it:

| Verb | What devkit does |
|---|---|
| `pre-tool-use` | guard the command, claim write targets, record the attempt |
| `post-tool-use` | record the outcome |
| `post-tool-use-failure` | record the failed outcome |
| `session-start` | record the session frame |
| `session-end` | release the session's claims, record, run `auto_prune` |
| `subagent-start` | record |
| `subagent-stop` | release the subagent's claims, record |
| `permission-request` | record what the harness asked about |
| `permission-denied` | record what the harness's own classifier blocked |
| `stop` | record the turn boundary |
| `stop-failure` | record an API-error turn end |
| `pre-compact` | record |
| `post-compact` | record |
| `cwd-changed` | record |
| `worktree-create` | register the new worktree as a holder devkit knows about |
| `worktree-remove` | release what that worktree held |
| `user-prompt-submit` | record, behind its own fidelity key |

`post-tool-use` and `post-tool-use-failure` stay separate rather than folding into one verb with a flag. Claude Code and Cursor both split success from failure at the vendor level, and a verb per vendor event is what keeps the manifest from needing to encode which is which.

`worktree-create` and `worktree-remove` are the only new verbs that take an action rather than record. Claude Code creates worktrees for `--worktree` and for subagents with `isolation: "worktree"`. A worktree root is a port holder as far as `registry::holder_alive` is concerned, so today those are holders devkit never learns about and never reclaims. Wiring the pair closes a correctness gap that exists whether or not logging is ever switched on.

Dispatch happens before any config load or tree-sitter work, so an Edit payload pays nothing for the shell path.

### The `HookEvent` enum

Lives in `src/bin/devkit/hook/mod.rs`. It is devkit's own CLI vocabulary and deliberately not a model of the payload's `hook_event_name` field: Cursor sends no such field at all, and `harness.rs` detects Cursor by its absence. That detection logic stays where it is.

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum HookEvent {
    #[value(alias = "pretooluse")]
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    SessionStart,
    SessionEnd,
    SubagentStart,
    SubagentStop,
    PermissionRequest,
    PermissionDenied,
    Stop,
    StopFailure,
    PreCompact,
    PostCompact,
    CwdChanged,
    WorktreeCreate,
    WorktreeRemove,
    UserPromptSubmit,
}
```

clap's kebab-case rename produces the spellings. `pretooluse` stays as a permanent alias, because an already-installed plugin manifest spells it that way and a user's binary can move ahead of their plugin.

clap rather than strum: `devkit-ports::registry::Role` and the docs manifest enum already derive `ValueEnum` on library types, so the precedent exists, and this type does not need to live in `devkit-common`, which is the one crate worth keeping free of clap since everything depends on it.

An unknown value is now a clap usage error rather than a string reaching a match arm.

### The `--harness` flag

`--harness <claude-code|codex|cursor>` on each hook verb, written into all three manifests devkit ships. The flag wins when present; the existing shape inference stays as the fallback for hand-wired hooks and older installed manifests.

This removes a real fragility rather than being cosmetic. Codex is currently distinguished from Claude Code by the presence of `turn_id` or `model`, which is a guess about which fields a vendor happens to send. If Claude Code ever adds a `model` field to hook payloads, every Claude Code session silently starts receiving Codex-shaped deny envelopes.

devkit writes all three manifests, so each one already knows which harness will read it. Passing that in beats inferring it.

It also shrinks `raw_shell_context`, which exists only to recover harness identity from a payload that would not parse. With the flag, identity never depended on parsing the payload.

### Manifest rewrites

Vendor-specific naming stays in the file that is already vendor-specific. No code learns what each harness calls an event; one line in one JSON file does.

| devkit verb | Claude Code | Codex | Cursor |
|---|---|---|---|
| `pre-tool-use` | `PreToolUse` | `PreToolUse` | `preToolUse` |
| `post-tool-use` | `PostToolUse` | `PostToolUse` | `postToolUse` |
| `post-tool-use-failure` | `PostToolUseFailure` | none | `postToolUseFailure` |
| `session-start` | `SessionStart` | `SessionStart` | `sessionStart` |
| `session-end` | `SessionEnd` | `SessionEnd` | `sessionEnd` |
| `subagent-start` | `SubagentStart` | `SubagentStart` | `subagentStart` |
| `subagent-stop` | `SubagentStop` | `SubagentStop` | `subagentStop` |
| `permission-request` | `PermissionRequest` | `PermissionRequest` | none |
| `permission-denied` | `PermissionDenied` | none | none |
| `stop` | `Stop` | `Stop`, `Interrupt` | `stop` |
| `stop-failure` | `StopFailure` | none | none |
| `pre-compact` | `PreCompact` | `PreCompact` | `preCompact` |
| `post-compact` | `PostCompact` | `PostCompact` | none |
| `cwd-changed` | `CwdChanged` | none | `workspaceOpen` |
| `worktree-create` | `WorktreeCreate` | none | none |
| `worktree-remove` | `WorktreeRemove` | none | none |
| `user-prompt-submit` | `UserPromptSubmit` | `UserPromptSubmit` | `beforeSubmitPrompt` |

Cursor is wired through its generic tool trio rather than its action-specific hooks. It offers both: `preToolUse` fires for every tool, while `beforeShellExecution`, `beforeMCPExecution` and `beforeReadFile` fire for particular ones. Wiring both would fire devkit twice for a single shell command, analysing and claiming the same targets on each. The generic trio also carries `tool_use_id`, which the shell-specific pair does not, and it mirrors Claude Code and Codex exactly, so all three harnesses converge on one shape.

The existing `hooks-cursor.json` wires `beforeShellExecution` and no session-end hook at all, so a Cursor session's claims persist until their TTL expires. Moving Cursor onto `preToolUse` and adding `sessionEnd` fixes that, and is worth doing on its own terms.

Cursor's Tab completions edit files through `afterTabFileEdit`, which is post-only. There is no pre-write Tab hook, so a Tab edit cannot be lock-guarded. That gap is noted, not solved; `afterTabFileEdit` is left unwired because a record of a write devkit could not have prevented answers no question the design asks.

The merged `PreToolUse` block inherits a single timeout of 30 seconds. Harmless: the edit path returns in milliseconds.

`session-start`, `post-compact` and `cwd-changed` sit alongside the existing `devkit brief` invocations rather than replacing them. Each manifest already runs several commands per event, `brief` keeps its own flags, and the extra spawn lands at session start rather than per tool call.

### The `post-tool-use` contract

Pure recording. It emits no envelope, denies nothing, and its output is ignored. So it cannot affect a tool call on any harness, and a vendor whose post event has a different response contract does not matter to us.

### `parse_event`

Takes the enum instead of `&str`. The `_ => HookEvent::Ignore` catch-all goes, since the enum makes it unreachable, and with it goes the silent-typo failure mode where a misspelled event exits 0, claims nothing, and reports nothing.

The `Ignore` variant stays. It has three legitimate producers: a pre-tool-use for a non-write tool, a subagent-stop with no agent id, and a session-end with no session id.

### Compatibility

`lockm hook <event>` and `devkit harness shell` stay as hidden aliases. The plugin bootstrap keeps the binary in lockstep with the plugin version, so the skew window is small, but a pinned binary must not break.

## Part B: harness logging

### Architecture and the failure contract

`devkit_common::harness_log` owns the record types, redaction, the writer, and the prune sweep. Four call sites, all in the one binary, since `dispatch_shim` routes every short name through it:

| Where | Record kind |
|---|---|
| `hook pre-tool-use`, shell path, after the verdict | `shell_pre`: command, context, analysis projection, verdict |
| `hook pre-tool-use`, edit path | `edit_pre`: tool, targets, conflict or not |
| `hook post-tool-use`, `hook post-tool-use-failure` | `shell_post`: outcome |
| `hook session-start`, `session-end`, `subagent-start`, `subagent-stop` | `session`: frame |
| `hook permission-request`, `permission-denied` | `permission`: what the harness asked or blocked |
| `hook stop`, `stop-failure`, `pre-compact`, `post-compact`, `cwd-changed` | `lifecycle`: turn and context boundaries |
| `hook worktree-create`, `worktree-remove` | `worktree`: holder registered or released |
| `hook user-prompt-submit` | `prompt`: gated by its own fidelity key |

Every verb beyond `pre-tool-use` is a thin dispatch: parse, build a record, call `record`, exit. Only `pre-tool-use` analyses, only it and the two worktree verbs take an action, and only `session-end` also runs `auto_prune`.

The module presents one infallible entry point. `harness_log::record(&Record)` returns `()`, wraps its body in `catch_unwind`, and discards every error inside. No call site can propagate a logging failure because there is nothing to propagate.

That signature is load-bearing, not fussiness. `guard_shell` already wraps `respond` in `catch_unwind`, and its `Err(_)` arm checks whether `write_stage` was set: if it was, a panic becomes a denial. A logging panic anywhere after the write stage claims its `OnceLock` would therefore deny a command the guard had already decided to allow. Logging that can change a verdict is worse than no logging.

Three ordering rules:

Emit before the envelope is printed. The record is a few kilobytes to an already-open append fd, which is nothing next to the tree-sitter parse that just ran, and writing first means a record exists even when stdout is a closed pipe.

The deadline branch logs before it exits. It calls `std::process::exit(0)` with a worker still blocked on the registry. A deadline miss is one of the operational signals this exists to collect, and it is the one path that would otherwise never reach a log.

The panic arm logs. `guard_shell`'s `Err(_)` gets a minimal record naming which stage was live. An analyser panic on real traffic is the highest-value record in the corpus, and today it produces one stderr line that scrolls away.

Two early returns become log-then-return: a payload that does not parse, and one carrying no cwd. A payload devkit could not read is precisely what this is for.

When logging is on and enforcement is off, the analysis runs anyway and the early return is skipped, so every guarded call pays a tree-sitter parse. That is the accepted trade: a record with an empty verdict is half a record. The cost is bounded by logging being off by default and enablable only from the global config.

### Config surface and resolution

```toml
[harness.log]
enabled = true              # global config only
command = "redacted"        # full | redacted | hashed
auto_prune = true
dir = "${HOME}/logs/devkit" # optional; defaults under state_dir
max_age_days = 30           # optional; absent means unlimited
max_bytes = 2_000_000_000   # optional; absent means unlimited
```

`enabled` is read from the global config alone. Not by position in the layer vector: `resolve_rules` pushes the global layer only when `global_config_path()` resolves and the file parses, so index 0 is a project layer on any machine with no global config. The resolver carries the global table separately rather than indexing. A test covers exactly that case, because the indexing version passes every test anyone would think to write and then breaks on a fresh machine.

The boundary this draws is honest but narrow: `global_config_path()` honours `DEVKIT_CONFIG`, which can point anywhere including into a repository. So global-only is a real boundary against a `devkit.toml` that a project ships to its contributors, and not against a user's own environment. Document it that way.

`DEVKIT_HARNESS_LOG` overrides, parsed by the existing `parse_env_override`, matching `DEVKIT_ENFORCE_WRITES`.

`dir` resolves through the config crate's `${VAR}` expansion and layer-relative path rules like any other path leaf. Tilde is not expanded, so an example writes `${HOME}` rather than `~`.

Fidelity clamps downward. `command` takes one of hashed < redacted < full, and resolves as the minimum of the global value and every project layer's value. Minimum is monotone and order-independent, so layer precedence stops mattering for this key and no project can raise fidelity from anywhere in the chain.

`enabled` is a separate key and sits below all three fidelities: any layer setting `enabled = false` wins, because turning logging off is the strongest form of tightening, while only the global config can set it true. The default `command` is `redacted`, so the unsafe mode is never reached by accident.

Both caps are `Option`. Absent means unlimited; `0` is rejected as invalid rather than guessed at. The two readings of `max_age_days = 0` differ by the whole corpus.

The doctor row reports the effective mode, not the configured one. The runtime probe reads each `[harness]` key independently so one bad key cannot take the others down, which means a misspelled key is invisible at runtime; `deny_unknown_fields` would not help, because nothing deserialises through the struct on that path. A row printing what you are actually getting is the mechanism that catches it. It is named `harness_log`, names the directory and its size on disk, and renders in orange with a warning marker when the effective mode is `full`.

`LogSection` carries a worked `devkit.toml` example as a doctest on the type, per the config-crate convention, and `schema/devkit-config.json` is regenerated with `DEVKIT_UPDATE_SCHEMA=1`.

### Record schema and file layout

`<dir>/<YYYY-MM-DD>/<session_id>[-<agent_id>].jsonl`, opened with `append(true)`, one `write` per record, no lock. `agent_id` is in the filename because parallel subagents share a `session_id` and would otherwise contend on one file. The default `dir` is `state_dir()/harness-log`, inheriting the new-versus-legacy pick in `paths`.

Append atomicity is relied on but not guaranteed. `append(true)` maps to `O_APPEND` on Unix and `FILE_APPEND_DATA` on Windows, both of which make the offset update atomic, and a single write of a few kilobytes to a regular file does not interleave in practice. The design does not stake data on it: the reader skips lines that do not parse and reports how many it skipped. A torn record costs one sample.

Every record carries `schema_version`, `recorded_at`, `devkit_version`, `analyzer_version`, `harness`, `session_id`, `agent_id`, `tool_use_id`, `cwd`, `project_root`, and a `kind` tag of `shell_pre`, `shell_post`, `edit_pre`, or `session`.

Records join on their components, `(session_id, agent_id, tool_use_id)`, not on a derived hash. A hash is one more step between the reader and the data, and when a join fails it hides why. No dedup or privacy requirement here argues for one.

The `shell_pre` analysis projection is a summary rather than the tree: resolved write targets, tree effects, script files, the program names from the invocations, every `Uncertainty` with its kind, detail and byte span, the four counts, and `analyze_micros`. The full invocation tree is reconstructable by replaying the command through `analyze`, which is what makes `analyzer_version` load-bearing: a record whose stamp differs from the current binary is a regression test, not stale data.

The verdict stores the decision plus the full text of every block and warning message. Those messages are the correction devkit offered, and whether a denial was a good one is not answerable without them.

Redaction is best-effort pattern matching over known token shapes and the environment variable names `secrets` knows, substituting a placeholder that names the kind so the command's structure survives. It will miss novel formats. The spec says so rather than implying that `redacted` is safe to hand to a third party.

A command over 128 KiB is truncated with a flag in the record. A heredoc carrying a whole file is not corpus signal.

### Pairing semantics

`shell_post` carries `exit_code`, `duration_ms`, the error and interrupt flags, and the byte lengths of stdout and stderr, each recorded as absent when the harness does not supply it. Codex is the case that forces that wording: its post payload exposes `tool_response` as the truncated output text rather than a structured result, so no exit code reaches the hook, and `post_tool_use_response` returns nothing at all for a backgrounded call with a `process_id`. Byte length and duration still land. Not the output itself. Command output is large and is where credentials actually surface: a `gh auth status`, a printenv, a failed curl echoing its own headers. Redaction over command text is already best-effort; over arbitrary program output it would be theatre. Failure text, if ever wanted, is a separate opt-in with its own argument to make.

The join is offline and best-effort. Four cases, one of which is not a problem at all:

A `shell_pre` with `verdict: deny` and no matching post is correct. The call never ran. Analysis that counts unpaired pre records as loss would flag every denial as a bug.

A `shell_pre` with `verdict: allow` and no post means the harness has no post event, the session died, or the call was aborted. That last one is a real Codex case rather than a hypothetical: it gates its post hooks on the tool call having succeeded, and while a shell command exiting nonzero still counts as success there, an aborted call does not. An interrupted command therefore leaves an unpaired pre record on Codex by design.

A `shell_post` with no pre means logging came on mid-session.

Where a harness sends no `tool_use_id`, nothing joins and both records stand alone.

That last case sets the rule for the whole schema: every record is useful by itself. The join adds analysis; it is never a prerequisite for reading one.

Two invariants. Records are append-only and never rewritten, so pairing never mutates a prior line, no reader-writer lock is ever needed, and a process killed mid-session cannot corrupt what is already there. And file order is not logical order: call B's pre can land before call A's post, so readers sort by `recorded_at`.

The `session` record is written at session end only. `SessionStart` already runs `devkit brief`, and a second hook there would put startup latency on every session including non-devkit ones, to record a start time the first per-call record already implies. Session end does not fire on a crash or a `kill -9`, so session records are unreliable framing either way. They are a convenience, never a dependency: per-call records repeat harness, project and cwd, so a missing session record costs nothing.

### Retention and prune

Part A empties the `harness` CLI family, so putting these under `harness log` would resurrect that verb for an unrelated purpose. They are top-level: `devkit log path` prints the directory for piping into jq, and `devkit log prune` sweeps. No short-name hardlink; this is occasional rather than per-session.

Prune deletes whole day-directories and never rewrites a file. Age first: directories older than `max_age_days`. Then size: oldest directories until the total is under `max_bytes`. Whole-directory deletion is what keeps the append-only invariant intact and means prune cannot corrupt a file a live session is mid-write on.

`auto_prune` defaults to true and runs the full sweep from the `session-end` hook. `max_age_days` and `max_bytes` are already a promise about how much data is kept, and a promise nothing enforces is not one. Both sweeps run, not just the age one: a size sweep is a readdir plus a stat per file over a few thousand files, which is milliseconds and page-cached, and session end is not a latency path the way pre-tool-use is.

Three guards, covering three different races.

A non-blocking `try_lock` on `<dir>/prune.lock` coordinates pruners with each other. A held lock means bail immediately: silently on the auto path, with a message on the manual one. Never a blocking wait, because session end must not sit behind another session's sweep.

An mtime recency window of one hour is the only thing standing between a sweep and a live session's file, because writers never take a lock at all. Prune skips any file whose mtime falls inside the window, and removes a day-directory only once it is empty. Without it, `auto_prune` turns a rare accident into a scheduled one: a session starting at 23:50 and running two hours loses its earlier records the first time any other session ends after midnight and finds yesterday's directory over a cap. This is an approximation of a liveness check and is documented as one, but it approximates the case that actually happens.

The current day's directory is never deleted, whatever the size sweep says.

A `NotFound` from a removal is success, not an error, for a lock file deleted by hand or a pruner killed mid-sweep. The whole sweep is fail-open: a prune failure never changes the hook's exit.

### The offline path

`corpus_probe` needs less work than expected. Its `command_of` already reads a top-level `command` field, which is where `shell_pre` puts it, and its nested-object fallback means an existing externally-produced corpus keeps working unchanged. Orphaning accumulated data would be a bad trade for a format change. The one real edit is dialect selection, which currently infers from harness plus platform and should prefer the recorded `dialect` when a record has one.

A second mode re-analyses each record and diffs the fresh projection against the recorded one, reporting where they disagree. With `analyzer_version` stamped per record, that turns the corpus into a regression gate to run before shipping a parser change. It stays in the example behind the `corpus` feature, so its output format is free to churn.

Cohort statistics stay offline for the same reason. The shipped surface is `devkit log path`, `devkit log prune`, and the doctor row.

## Testing

`harness_log` is mostly pure functions with one IO seam, so most of this is unit-level and fast.

Pure and tested directly: the fidelity clamp, asserting that minimum is order-independent and that a project layer can lower but never raise, rather than a handful of examples; redaction over a table of token shapes, including the case that matters most, that a non-secret command comes through byte-identical; the projection built from a known `Analysis`; the truncation flag at the 128 KiB boundary.

Global-only resolution gets an explicit test for a machine with no `~/.config/devkit/config.toml`, where the global layer was never pushed and index 0 is a project layer. That test is the reason the resolver carries the global table separately, and it should fail loudly if someone simplifies it back to indexing.

Prune gets a `tempfile::tempdir` tree with planted mtimes: age sweep, size sweep, the current-day guard, the recency window sparing a fresh file, an empty-after-deletion directory being removed, and a held `prune.lock` causing an immediate bail. Per the workspace convention, the `TempDir` guard is bound for as long as the paths are used.

End to end, `devkit hook pre-tool-use` is driven with a payload on stdin against a temp state dir, asserting that a record lands, parses, and carries the verdict. Then the one that matters most: a panic injected into the logging path leaves the verdict unchanged. The failure contract is that claim, and without a test it is a comment.

Round-tripping covers both record formats, since new `shell_pre` records and an existing externally-produced corpus both have to feed `corpus_probe`.

Part A's tests are about equivalence: each retired invocation and its replacement produce the same envelope for the same payload, the `pretooluse` alias still resolves, an unknown event is a usage error, and `--harness` wins over inference while inference still answers when the flag is absent.

Two things deliberately untested. Append atomicity, because the design concedes it and handles torn lines by skipping them. And the exact JSON text of a record, because a golden-file test on a schema that will churn is a maintenance tax that catches nothing real. Assertions are on parsed fields.

## Harness event support

Codex is confirmed against its own source at `rust-v0.154.0`. Its event vocabulary is `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`, `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `SubagentStart`, `SubagentStop`, `Stop`, `Interrupt`, at `codex-rs/protocol/src/protocol.rs:1576`. Hook config files key those in PascalCase (`codex-rs/config/src/hook_config.rs:37`), which is what the shipped `hooks-codex.json` already writes, so the manifest spelling needs no change.

Its post-tool payload carries `session_id`, `cwd`, `tool_use_id`, `tool_name`, `tool_input`, and `tool_response` (`codex-rs/core/src/hook_runtime.rs:288`), so pairing has the key it needs.

Post hooks run only when the tool call succeeded (`codex-rs/core/src/tools/registry.rs:674`), but for shell commands `success_for_logging` is unconditionally true (`codex-rs/core/src/tools/context.rs:375`), so a nonzero exit still fires the hook. An aborted call does not (`context.rs:321`).

Claude Code's vocabulary is the widest of the three, and most of it is outside devkit's business. Events left unwired: `MessageDisplay`, `Notification`, `TeammateIdle`, `Elicitation`, `ElicitationResult`, `InstructionsLoaded`, `TaskCreated`, `TaskCompleted`, `DirectoryAdded`, `Setup`, `PostToolBatch`, `ConfigChange`, `FileChanged`, `UserPromptExpansion`. None carries a devkit action, none answers a question this design asks, and `MessageDisplay` fires while assistant text is being displayed, which would spawn a process per burst of output.

Cursor's `afterAgentResponse`, `afterAgentThought`, `beforeTabFileRead`, `afterTabFileEdit` and `workspaceOpen` are left unwired for the same reason, with `workspaceOpen` the exception: it is the closest thing Cursor has to `CwdChanged` and carries the `cwd-changed` verb.

The Cursor event list comes from the vendor's documentation, not from the `cursor-hooks` npm package's JSON schema. At `1.1.6` that schema publishes six events and omits `sessionStart`, which the currently shipped `hooks-cursor.json` uses and which does fire. The schema lags the product, so it is not a source to plan against.

## Fidelity of prompt text

`user-prompt-submit` records the prompt verbatim, which is the richest context for answering why an agent ran a given command and the worst thing in the corpus from a privacy angle. It is gated by its own fidelity key rather than the `command` one, resolving by the same downward clamp, and defaults to off. A corpus can carry full command text without carrying what the human typed.

## References

Each harness's hook vocabulary, to check against when a verb is added or a payload field is assumed:

- Claude Code: `https://json.schemastore.org/claude-code-settings.json`, under `properties.hooks.properties`.
- Codex: `https://developers.openai.com/codex/config-schema.json`, and the source of truth behind it at `codex-rs/protocol/src/protocol.rs` plus `codex-rs/config/src/hook_config.rs` in `openai/codex`.
- Cursor: `https://cursor.com/docs/hooks`. Not the `cursor-hooks` npm schema, which lags the product.

## Docs to update

`docs/agents.md` hook table, which grows from six rows to the full per-harness mapping and gains the three schema and documentation links from the References section above, so the next person adding a verb checks the vendor rather than recalling it. `docs/commands.md` for the `hook` and `log` families. `docs/configuration.md` for `[harness.log]`. The AGENTS.md layout rows for `src/bin/devkit/` and `crates/devkit-common`.
