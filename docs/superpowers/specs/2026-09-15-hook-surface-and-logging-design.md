# Hook surface unification and harness logging

Two parts, one change. Part A restructures how harness events enter devkit; part B adds the logging subsystem that hangs off that surface. A carries two behaviour fixes it uncovers (harness misdetection and hook exit codes) and otherwise preserves every verdict.

They ship together, as one pull request, and every verb is wired in the manifests devkit installs.

Shipping both at once puts the manifest flip in the same release as the verbs, which is where the skew hazard bites: a binary the plugin bootstrap did not install is never upgraded, so a manifest calling `devkit hook` can meet a binary that has no such subcommand. The exit-code rule below is what keeps that from blocking every tool call, but it only helps once the binary carries it. The session-start bootstrap therefore gains a capability probe: it runs `devkit hook --help` once per version stamp, and a binary that does not answer is reported as too old for the installed manifests, in the same place the bootstrap already reports a failed install.

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
| `session-end` | release the session's claims, record, run `auto_prune` |
| `subagent-stop` | release the subagent's claims, record |
| `session-start` | record the session frame |
| `subagent-start` | record |
| `permission-request` | record what the harness asked about |
| `permission-denied` | record what the harness's own classifier blocked |
| `stop` | record the turn boundary |
| `stop-failure` | record an API-error turn end |
| `pre-compact` | record |
| `post-compact` | record |
| `cwd-changed` | record |
| `worktree-create` | record |
| `worktree-remove` | record |
| `user-prompt-submit` | record, behind its own fidelity key |

`post-tool-use` and `post-tool-use-failure` stay separate rather than folding into one verb with a flag. Claude Code and Cursor both split success from failure at the vendor level, and a verb per vendor event is what keeps the manifest from needing to encode which is which.

Every verb except `pre-tool-use`, `session-end` and `subagent-stop` is record-only. None of them takes an action, so with logging off each is a process spawn that reads the global config, learns logging is off, and exits. That cost is real, and the record-only dispatch is written to make it as small as it can be: it reads the global config and nothing else, never loading a project layer or touching tree-sitter. The per-event spawn is measured on the slowest supported platform before release, and a verb whose cost does not justify its record is dropped from the shipped manifests while staying available to a hand-wired one.

`worktree-create` and `worktree-remove` are record-only, not registrations. A worktree becomes a port holder when `devrun up` allocates under it, not when the directory appears, and `registry::holder_alive` is `Path::exists`, so `prune` already reclaims every row of a removed worktree with no hook involved. There is no holder to register and nothing to release. Releasing rows from the hook would at best duplicate `prune`, and stopping the worktree's servers would drive a cross-worktree `devrun down` from a hook with no terminal, which the TTY gate exists to forbid.

Dispatch happens before any config load or tree-sitter work, so an Edit payload pays nothing for the shell path.

### The `HookEvent` enum

Lives in `src/bin/devkit/hook/mod.rs`, named `HookEvent`. `devkit-locks` already exports a `HookEvent` of its own with different variants, and two types under one name in one workspace is a reading hazard whichever crate you are in; that one is renamed `LockAction` as part of this work.

This type is devkit's own CLI vocabulary and deliberately not a model of the payload's `hook_event_name`. The two vocabularies differ: Codex maps both `Stop` and `Interrupt` onto `stop`, and Cursor spells the same events in camelCase.

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

clap's kebab-case rename produces the spellings. `pretooluse` stays as a permanent alias so that the one-word spelling in every installed `lockm hook pretooluse` keeps resolving after the retired commands are removed.

clap rather than strum: `devkit-ports::registry::Role` and the docs manifest enum already derive `ValueEnum` on library types, so the precedent exists, and this type does not need to live in `devkit-common`, which is the one crate worth keeping free of clap since everything depends on it.

### Exit codes are part of the contract

A hook's exit code is a control channel on two of the three harnesses, and clap's default behaviour drives it into the blocking value. Exit 2 blocks the tool call on Claude Code `PreToolUse`, erases the prompt on `UserPromptSubmit`, prevents stopping on `Stop`, and any non-zero exit aborts a `WorktreeCreate`; on Codex, exit 2 with non-empty stderr sets `should_block` (`codex-rs/hooks/src/events/pre_tool_use.rs:261`), while any other non-zero exit is a non-blocking failure (`:278`). clap exits 2 for a usage error and writes it to stderr, so an unrecognised verb, an unrecognised `--harness`, or a missing argument would block every tool call rather than fail loudly.

So the `hook` family never exits 2 unless it is deliberately blocking. It parses with `try_get_matches`, prints the error to stderr, and exits 1. The same wrapper catches panics, which would otherwise exit 101 and abort a `WorktreeCreate`. "An unrecognised event is an error, not silence" is preserved exactly: the message reaches stderr and the exit is non-zero. What changes is that the error never reads as a deny.

`guard_shell`'s existing fail-open and fail-closed contract sits inside `pre-tool-use` and is untouched. The wrapper covers the argument parse, which happens before `guard_shell` is reached, and the verbs that have no verdict to fail toward.

### The `--harness` flag

`--harness <claude-code|codex|cursor>` on each hook verb, written into all three manifests devkit ships. The flag wins when present; shape inference stays as the fallback for hand-wired hooks and older installed manifests.

This fixes a live misdetection rather than guarding against a hypothetical one. `parse_shell_payload` reads Cursor as the absence of `hook_event_name` and Codex as the presence of `turn_id` or `model`, and Cursor now sends `hook_event_name`, `model`, `model_id`, `model_params`, `cursor_version`, `conversation_id`, `generation_id`, `workspace_roots`, `user_email` and `transcript_path` on every hook. A Cursor payload therefore resolves to Codex, and the resulting `hookSpecificOutput` envelope does not match Cursor's response schema, which Cursor treats as a reason to block the action. Cursor's shipped wiring is `beforeShellExecution`, so whether this is reachable today depends on that one event's fields; on the `preToolUse` this design moves to, it is certain.

The inference fallback is corrected at the same time, and detects Cursor by the presence of `cursor_version` rather than the absence of a field. Positive evidence does not rot when a vendor adds a key. The doc comment on `parse_shell_payload` is rewritten with it, because it currently states the opposite of what Cursor sends.

devkit writes all three manifests, so each one already knows which harness will read it. Passing that in beats inferring it.

It also shrinks `raw_shell_context`, which exists only to recover harness identity from a payload that would not parse. With the flag, identity never depended on parsing the payload.

### Per-harness payload shapes

The flag settles which harness. These differences still have to be read per harness:

| | Claude Code | Codex | Cursor |
|---|---|---|---|
| session identity | `session_id` | `session_id` | `conversation_id` |
| subagent identity | `agent_id` | `agent_id` | `parent_conversation_id` on subagent events |
| shell `tool_name` | `Bash` | `Bash` | `Shell` |
| shell command | `tool_input.command` | `tool_input.command` | `tool_input.command` |
| working directory | `cwd` | `cwd` | `tool_input.working_directory` |

`SHELL_TOOLS` gains `Shell`. Cursor's `workspaceOpen` fires outside any agent session and carries no conversation id at all, which is what the record layout's missing-id fallback is for.

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

Cursor is wired through its generic tool trio rather than its action-specific hooks. It offers both: `preToolUse` fires for every tool, while `beforeShellExecution`, `beforeMCPExecution` and `beforeReadFile` fire for particular ones. Wiring both would fire devkit twice for a single shell command, analysing and claiming the same targets on each. The generic trio also carries `tool_use_id`, which the shell-specific pair does not.

The merged `PreToolUse` block keeps a matcher, the union of the two it replaces: `Edit`, `MultiEdit`, `Write`, `NotebookEdit`, `Bash`, `PowerShell` on Claude Code, `apply_patch`, `Write`, `Edit`, `Bash` on Codex, `Shell` and the edit tools on Cursor. Without one the hook would spawn on every Read, Grep, Glob, MCP and Agent call. It inherits a single timeout of 30 seconds, which is harmless because the edit path returns in milliseconds.

Cursor's shipped `hooks-cursor.json` wires `beforeShellExecution` and `sessionStart`, with no session-end hook. Cursor claims no locks today (`writes_on` excludes `Harness::Cursor`), so nothing is currently leaking; adding `sessionEnd` is what lets that stay true if Cursor ever gains the write stage, and it is where Cursor's `auto_prune` sweep runs.

Cursor's Tab completions edit files through `afterTabFileEdit`, which is post-only. There is no pre-write Tab hook, so a Tab edit cannot be lock-guarded. That gap is noted, not solved; `afterTabFileEdit` is left unwired because a record of a write devkit could not have prevented answers no question the design asks.

`session-start`, `post-compact` and `cwd-changed` sit alongside the existing `devkit brief` invocations rather than replacing them. Each manifest already runs several commands per event, and `brief` keeps its own flags.

Claude Code gives all `SessionEnd` hooks a shared 1.5 second budget unless the manifest sets a longer timeout, and `session-end` does three things. They run in this order: release the session's claims, write the record, then prune. Release first because it is the one with a correctness consequence, prune last because it is the only one that can be skipped without loss. The manifest sets an explicit timeout covering the sweep.

### Stdout is silent on every verb but `pre-tool-use`

`pre-tool-use` is the only verb that emits an envelope. Every other verb writes nothing to stdout and exits 0.

That is a requirement, not an observation. `PostToolUse` ignores its output, but `UserPromptSubmit` on Claude Code appends plain stdout to the prompt as context and Codex parses it; `Stop` and `PermissionRequest` honour a JSON decision; Cursor's `beforeSubmitPrompt` reads a `continue` field. A stray `println!` in a record-only verb would therefore inject text into a prompt or veto a turn. Diagnostics from these verbs go to stderr.

### `parse_event`

`devkit-locks`'s `parse_event(&str, ...)` is what the string-matching verb dispatch goes through today, and it is the source of the silent-typo failure mode: a misspelled event falls to the `_ => Ignore` arm of the locks enum, exits 0, claims nothing, and reports nothing.

It is replaced by three functions, `parse_write`, `parse_subagent_stop` and `parse_session_end`, each taking the payload and returning `Option`. The binary matches its own clap enum and calls the one that applies. A library cannot import the binary's clap enum, so passing `HookEvent` into `devkit-locks` is not available; splitting by verb is what removes the string match instead.

`None` carries what `Ignore` legitimately meant: a pre-tool-use for a non-write tool, a subagent-stop with no agent id, a session-end with no session id. What it no longer carries is an event nobody recognised, because there is no longer a place for an unrecognised string to arrive.

### Compatibility and migration

`lockm hook <event>` and `devkit harness shell` stay as hidden aliases for at least one release after the manifests move.

The manifests flip in the same release, so the skew is handled rather than waited out. It is not bounded on its own: `docs/agents.md` states that binaries the bootstrap hook did not install are recorded as externally managed and never upgraded, so a `cargo install`ed devkit stays where it is indefinitely, and a manifest calling a subcommand that binary lacks is an unrecognised-subcommand error on every tool call. The exit-code rule above fixes that going forward but cannot reach a binary that predates it, which is why the session-start bootstrap probes for the verb family and reports a binary too old for the installed manifests.

## Part B: harness logging

### Architecture and the failure contract

`devkit_common::harness_log` owns the record types, redaction, the writer, and the prune sweep. It lives in `devkit-common` rather than a crate of its own because it needs `paths::state_dir` and `secrets`, and because moving `timing` in alongside it would invert an existing dependency: `cmd`, `git`, `github`, `slack` and `tracker::linear` all call `crate::timing`, so a crate holding `timing` would be a dependency of `devkit-common` and could not use `paths` or `secrets` in turn.

`devkit-command` stays out of `devkit-common`'s dependencies. Adding it would make every library crate in the workspace compile six tree-sitter C grammars, and would put the IO-free analyser underneath the IO crate. So `harness_log` defines its own serde projection types, and the `Analysis` to projection mapping lives in the binary, which already depends on both. `Analysis` and its `Uncertainty` derive no `Serialize` today and do not gain one.

The call sites are all in the one binary, since `dispatch_shim` routes every short name through it:

| Where | Record kind |
|---|---|
| `hook pre-tool-use`, shell path, after the verdict | `shell_pre`: command, context, analysis projection, verdict |
| `hook pre-tool-use`, edit path | `edit_pre`: tool, targets, conflict or not |
| `hook post-tool-use`, `hook post-tool-use-failure` | `shell_post`: outcome |
| `hook session-start`, `session-end`, `subagent-start`, `subagent-stop` | `session`: frame |
| `hook permission-request`, `permission-denied` | `permission`: what the harness asked or blocked |
| `hook stop`, `stop-failure`, `pre-compact`, `post-compact`, `cwd-changed` | `lifecycle`: turn and context boundaries |
| `hook worktree-create`, `worktree-remove` | `worktree`: the worktree path and which way it moved |
| `hook user-prompt-submit` | `prompt`: gated by its own fidelity key |

Every verb beyond `pre-tool-use` is a thin dispatch: parse, build a record, call `record`, exit. Only `pre-tool-use` analyses, only it and the two release verbs take an action, and only `session-end` also runs `auto_prune`.

The module presents one infallible entry point. `harness_log::record(&Record)` returns `()`, wraps its body in `catch_unwind`, and discards every error inside. No call site can propagate a logging failure because there is nothing to propagate.

That signature is load-bearing, not fussiness. `guard_shell` already wraps `respond` in `catch_unwind`, and its `Err(_)` arm checks whether `write_stage` was set: if it was, a panic becomes a denial. A logging panic anywhere after the write stage claims its `OnceLock` would therefore deny a command the guard had already decided to allow. Logging that can change a verdict is worse than no logging.

Four ordering rules:

The envelope is printed and flushed before any record is written. `catch_unwind` makes `record` panic-safe, not block-safe, and `dir` is user-configurable to a network home, so `create_dir_all` and the first append can stall on a hung mount. A stall before the envelope exists runs into the manifest's 30-second timeout, and a harness timeout allows the call, which turns a denial the write stage had already decided into an allow. Printing first costs nothing: a closed stdout pipe fails the write immediately rather than blocking, and the record is still written afterwards.

`record` runs under its own short deadline, on the `writes::with_deadline` pattern already in the shell hook. A worker still blocked on the disk is abandoned rather than waited on. The budget is well under a second because nothing downstream is waiting on the record.

The deadline branch logs before it exits. It calls `std::process::exit(0)` with a worker still blocked on the registry. A deadline miss is one of the operational signals this exists to collect, and it is the one path that would otherwise never reach a log. Envelope, then record, then exit.

The panic arm logs. `guard_shell`'s `Err(_)` gets a minimal record. An analyser panic on real traffic is the highest-value record in the corpus, and today it produces one stderr line that scrolls away. Only `write_stage` is hoisted out of the `catch_unwind` closure today, so a second `OnceLock` carries the harness, session and tool ids out with it; without that the panic record can name the stage and nothing else.

Two early returns become log-then-return: a payload that does not parse, and one carrying no cwd. A payload devkit could not read is precisely what this is for.

When logging is on and enforcement is off, the analysis runs anyway and the early return is skipped, so every guarded call pays a tree-sitter parse. That is the accepted trade: a record with an empty verdict is half a record. The cost is bounded by logging being off by default and enablable only from the global config.

### Config surface and resolution

```toml
[harness.log]
enabled = true              # global config only
command = "redacted"        # full | redacted | hashed
prompt = "off"              # off | hashed | redacted | full
auto_prune = true           # global config only
dir = "${HOME}/logs/devkit" # global config only; defaults under state_dir
max_age_days = 30           # global config only; absent means unlimited
max_bytes = 2_000_000_000   # global config only; absent means unlimited
```

The split is by key, and it is a rule rather than a property of one key. `enabled = true`, `dir`, `auto_prune`, `max_age_days` and `max_bytes` are read from the global config alone and ignored wherever else they appear. A project layer may do exactly two things: set `enabled = false`, and lower `command` or `prompt`. Everything a project layer can do tightens.

`dir` is on that list for the same reason `enabled = true` is. It resolves through the config crate's `${VAR}` expansion and layer-relative path rules, so a project layer setting `dir = "./.logs"` would land command text inside the checkout, which is the one outcome the global-only boundary exists to prevent.

Global-only is not read by position in the layer vector. `resolve_rules` pushes the global layer only when `global_config_path()` resolves and the file parses, so index 0 is a project layer on any machine with no global config. The resolver carries the global table separately rather than indexing. A test covers exactly that case, because the indexing version passes every test anyone would think to write and then breaks on a fresh machine.

The boundary this draws is honest but narrow: `global_config_path()` honours `DEVKIT_CONFIG`, which can point anywhere including into a repository. So global-only is a real boundary against a `devkit.toml` that a project ships to its contributors, and not against a user's own environment. Document it that way.

`DEVKIT_HARNESS_LOG` overrides, parsed by the existing `parse_env_override`, matching `DEVKIT_ENFORCE_WRITES`. It is the user's own environment and so it wins over a project layer's `enabled = false`. Full precedence: the env override decides if set; otherwise logging is on when the global config enables it and no layer sets `enabled = false`.

Fidelity clamps downward. `command` takes one of hashed < redacted < full and `prompt` one of off < hashed < redacted < full, each resolving as the minimum of the global value and every project layer's value. Minimum is monotone and order-independent, so layer precedence stops mattering for these keys and no project can raise fidelity from anywhere in the chain. `command` defaults to `redacted` and `prompt` to `off`, so neither unsafe mode is reached by accident.

Both caps are `Option`. Absent means unlimited; `0` is rejected as invalid rather than guessed at. The two readings of `max_age_days = 0` differ by the whole corpus.

The doctor row reports the effective mode, not the configured one. The runtime probe reads each `[harness]` key independently so one bad key cannot take the others down, which means a misspelled key is invisible at runtime; `HarnessSection` carries no `deny_unknown_fields`, and adding one would not help, because nothing deserialises through the struct on that path. A row printing what you are actually getting is the mechanism that catches it. It is named `harness_log`, names the resolved directory, its size on disk, and whether a global config was found at all, and it carries a warning marker with the row in yellow when the effective `command` mode is `full`. `ui` has a `yellow` helper and no orange one, and adding a colour to `doctor`'s human output means adding the first colour path to `print_human`. The `--json` output has no colour at all, so the effective mode is a data field there.

`LogSection` carries a worked `devkit.toml` example as a doctest on the type, per the config-crate convention, and `schema/devkit-config.json` is regenerated with `DEVKIT_UPDATE_SCHEMA=1`.

### Record schema and file layout

`<dir>/<YYYY-MM-DD>/<session_id>[-<agent_id>].jsonl`, opened with `append(true)`, one `write` per record, no lock. `agent_id` is in the filename because parallel subagents share a `session_id` and would otherwise contend on one file. The default `dir` is `state_dir()/harness-log`, inheriting the new-versus-legacy pick in `paths`.

The day directory is UTC, not local. Prune's age arithmetic and its never-delete-today guard both read the directory name, and a local-time name makes both wrong twice a year.

Both id components are sanitised before they reach a path: every character outside `[A-Za-z0-9._-]` becomes `_`, and a component that is empty after sanitising is dropped. A payload with no session id at all writes to `unknown-<pid>.jsonl` in that day's directory. Cursor's `workspaceOpen` carries no conversation id, and a garbage payload carries nothing, so this is a live path rather than a defensive one.

Append atomicity is relied on but not guaranteed. `append(true)` maps to `O_APPEND` on Unix and `FILE_APPEND_DATA` on Windows, both of which make the offset update atomic, and a single write of a few kilobytes to a regular file does not interleave in practice. The design does not stake data on it: the reader skips lines that do not parse and reports how many it skipped. A torn record costs one sample.

Every record carries `schema_version`, `recorded_at`, `devkit_version`, `analyzer_version`, `harness`, `event`, `vendor_event`, `session_id`, `agent_id`, `tool_use_id`, `cwd`, `project_root`, and a `kind` tag of `shell_pre`, `shell_post`, `edit_pre`, `session`, `permission`, `lifecycle`, `worktree` or `prompt`.

`event` is the devkit verb and `vendor_event` the payload's own `hook_event_name`. Both are needed because the mapping is not injective: Codex sends `Stop` and `Interrupt` to one verb, and a reader asking which vendor event produced a record cannot recover it from the verb alone.

`analyzer_version` is a hand-bumped `pub const ANALYZER_VERSION: u32` in `devkit-command`, raised when analysis semantics change. It cannot be the crate version: every crate here is stamped by release-please, so a crate version moves on every release whether the parser changed or not, and a regression diff keyed on it would call the whole corpus stale at each release. `devkit_version` stays alongside it for provenance.

Records join on their components, `(session_id, agent_id, tool_use_id)`, not on a derived hash. A hash is one more step between the reader and the data, and when a join fails it hides why. No dedup or privacy requirement here argues for one.

The `shell_pre` analysis projection is a summary rather than the tree: resolved write targets, tree effects, script files, the program names from the invocations, every `Uncertainty` with its kind, detail and byte span, `analyze_micros`, and four counts (invocations, resolved write targets, unresolved write targets, uncertainties) so that a cohort query can rank without reading each projection. The full invocation tree is reconstructable by replaying the command through `analyze`, which is what makes `analyzer_version` load-bearing: a record whose stamp differs from the current binary is a regression candidate, not stale data.

The verdict stores the decision plus the full text of every block and warning message. Those messages are the correction devkit offered, and whether a denial was a good one is not answerable without them.

Redaction is best-effort pattern matching over known token shapes and over the environment variable names devkit itself resolves credentials from: `LINEAR_API_KEY`, `LINEAR_WORKSPACE`, `SLACK_TOKEN` from `secrets`, plus `GH_TOKEN` and `GITHUB_TOKEN`, which are resolved elsewhere and are the ones most likely to appear in a command. The substitute names the kind so the command's structure survives. It will miss novel formats. The spec says so rather than implying that `redacted` is safe to hand to a third party.

A command over 128 KiB is truncated with a flag in the record. A heredoc carrying a whole file is not corpus signal.

### Pairing semantics

`shell_post` carries `exit_code`, `duration_ms`, the error and interrupt flags, and the byte lengths of stdout and stderr, each recorded as absent when the harness does not supply it. Codex is the case that forces that wording: its post payload exposes `tool_response` as output text rather than a structured result, so no exit code reaches the hook. For a backgrounded call `post_tool_use_response` returns nothing, but `post_tool_use_payload` falls back to the model-facing output body, so the hook still fires with text in `tool_response`. Byte length and duration still land. Not the output itself. Command output is large and is where credentials actually surface: a `gh auth status`, a printenv, a failed curl echoing its own headers. Redaction over command text is already best-effort; over arbitrary program output it would be theatre. Failure text, if ever wanted, is a separate opt-in with its own argument to make.

The join is offline and best-effort. Four cases, one of which is not a problem at all:

A `shell_pre` with `verdict: deny` and no matching post is correct. The call never ran. Analysis that counts unpaired pre records as loss would flag every denial as a bug.

A `shell_pre` with `verdict: allow` and no post means the harness has no post event, the session died, or the call was aborted. That last one is a real Codex case rather than a hypothetical: it gates its post hooks on the tool call having succeeded, and while a shell command exiting nonzero still counts as success there, an aborted call does not. An interrupted command therefore leaves an unpaired pre record on Codex by design.

A `shell_post` with no pre means logging came on mid-session.

Where a harness sends no `tool_use_id`, nothing joins and both records stand alone.

That last case sets the rule for the whole schema: every record is useful by itself. The join adds analysis; it is never a prerequisite for reading one.

Two invariants. Records are append-only and never rewritten, so pairing never mutates a prior line, no reader-writer lock is ever needed, and a process killed mid-session cannot corrupt what is already there. And file order is not logical order: call B's pre can land before call A's post, so readers sort by `recorded_at`.

A `session` record is written at both `session-start` and `session-end`, each naming which end it is. Neither is a dependency: session end does not fire on a crash or a `kill -9`, and per-call records repeat harness, project and cwd, so a missing session record costs nothing. They are framing, and the start record is what gives a crashed session any frame at all.

### Retention and prune

Part A empties the `harness` CLI family, so putting these under `harness log` would resurrect that verb for an unrelated purpose. They are top-level under `hook-log`, not `log`: `paths::logs_dir()` is already `state_dir()/logs` for daemon and server logs, and `devrun logs` already tails those, so a bare `devkit log` would name the wrong thing twice. `devkit hook-log path` prints the directory for piping into jq and `devkit hook-log prune` sweeps. No short-name hardlink; this is occasional rather than per-session.

Prune deletes whole files and never rewrites one, removing a day-directory once it is empty. Age first: a directory whose UTC name is older than `max_age_days`, measured from the name rather than from any mtime, since a file touched late in a stale directory should not keep it alive. Then size: oldest first until the total is under `max_bytes`. Per-file deletion is what keeps the append-only invariant intact, because a file is either wholly there or gone and no reader ever sees a partial one.

The size sweep terminates when the cap cannot be reached rather than deleting past its guards. Today's directory alone over the cap, or every remaining file inside the recency window, means the sweep stops and reports that it stopped. A sweep that cannot meet the cap is not a reason to delete a live session's records.

`auto_prune` defaults to true and runs the full sweep from the `session-end` hook, after the release and the record. `max_age_days` and `max_bytes` are already a promise about how much data is kept, and a promise nothing enforces is not one. Both sweeps run, not just the age one: a size sweep is a readdir plus a stat per file over a few thousand files, which is milliseconds and page-cached.

Three guards, covering three different races.

A non-blocking `try_lock` on `<dir>/prune.lock` coordinates pruners with each other. A held lock means bail immediately: silently on the auto path, with a message on the manual one. Never a blocking wait, because session end must not sit behind another session's sweep.

An mtime recency window of one hour is the only thing standing between a sweep and a live session's file, because writers never take a lock at all. Prune skips any file whose mtime falls inside the window. Without it, `auto_prune` turns a rare accident into a scheduled one: a session starting at 23:50 UTC and running two hours loses its earlier records the first time any other session ends after midnight and finds yesterday's directory over a cap. This is an approximation of a liveness check and is documented as one, but it approximates the case that actually happens.

The current UTC day's directory is never deleted, whatever the size sweep says.

A `NotFound` from a removal is success, not an error, for a lock file deleted by hand or a pruner killed mid-sweep. The whole sweep is fail-open: a prune failure never changes the hook's exit.

### The offline path

`corpus_probe` needs less work than expected. Its `command_of` already reads a top-level `command` field, which is where `shell_pre` puts it, and its nested-object fallback means an existing externally-produced corpus keeps working unchanged. Orphaning accumulated data would be a bad trade for a format change. The one real edit is dialect selection, which currently infers from harness plus platform and should prefer the recorded `dialect` when a record has one.

A second mode re-analyses each record and diffs the fresh projection against the recorded one, reporting where they disagree. With `ANALYZER_VERSION` stamped per record, that turns the corpus into a regression gate to run before shipping a parser change. It stays in the example behind the `corpus` feature, so its output format is free to churn.

Cohort statistics stay offline for the same reason. The shipped surface is `devkit hook-log path`, `devkit hook-log prune`, and the doctor row.

## Testing

`harness_log` is mostly pure functions with one IO seam, so most of this is unit-level and fast.

Pure and tested directly: the fidelity clamp for both keys, asserting that minimum is order-independent and that a project layer can lower but never raise, rather than a handful of examples; redaction over a table of token shapes, including the case that matters most, that a non-secret command comes through byte-identical; the projection built from a known `Analysis`; the truncation flag at the 128 KiB boundary; id sanitising, including the missing-id fallback.

Config resolution gets two tests that the obvious implementation passes and the correct one is needed for. Global-only resolution on a machine with no `~/.config/devkit/config.toml`, where the global layer was never pushed and index 0 is a project layer. And a project layer setting `dir`, `max_bytes` and `enabled = true`, asserting that the first two are ignored and the third does not enable anything.

Prune gets a `tempfile::tempdir` tree with planted mtimes: age sweep by directory name, size sweep, the current-day guard, the recency window sparing a fresh file, an empty-after-deletion directory being removed, a sweep that cannot reach the cap stopping rather than deleting, and a held `prune.lock` causing an immediate bail. Per the workspace convention, the `TempDir` guard is bound for as long as the paths are used.

Exit codes get their own tests, because they are the finding that would be silent until it reached a user: an unknown verb, an unknown `--harness`, and a missing argument each exit 1 with a message on stderr, and none exits 2.

Harness resolution gets a Cursor `preToolUse` payload carrying `hook_event_name`, `model` and `cursor_version`, asserting it resolves to Cursor both with `--harness cursor` and, through the corrected inference, without it. That payload is the one that misresolves today.

End to end, `devkit hook pre-tool-use` is driven with a payload on stdin against a temp state dir, asserting that a record lands, parses, and carries the verdict. Then the two that matter most. A panic injected into the logging path leaves the verdict unchanged, and a `record` that blocks past its deadline neither delays nor alters the envelope. Both are driven by `DEVKIT_HARNESS_LOG_FAULT`, read only under `#[cfg(debug_assertions)]` so the knob does not exist in a release binary. The failure contract is those two claims, and without tests they are comments.

Round-tripping covers both record formats, since new `shell_pre` records and an existing externally-produced corpus both have to feed `corpus_probe`.

Part A's tests are about equivalence: each retired invocation and its replacement produce the same envelope for the same payload, and the `pretooluse` alias still resolves. The existing `run_hook` helpers in `tests/harness_guard.rs`, `tests/harness_shell_writes.rs`, `tests/locks.rs`, `tests/lock_harness_race.rs` and `tests/abort_hook.rs` are the template, and each suite runs against both spellings for as long as the aliases exist.

Two things deliberately untested. Append atomicity, because the design concedes it and handles torn lines by skipping them. And the exact JSON text of a record, because a golden-file test on a schema that will churn is a maintenance tax that catches nothing real. Assertions are on parsed fields.

## Harness event support

Codex is confirmed against its own source at `rust-v0.154.0`. Its event vocabulary is `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`, `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `SubagentStart`, `SubagentStop`, `Stop`, `Interrupt`, at `codex-rs/protocol/src/protocol.rs:1576`. Hook config files key those in PascalCase (`codex-rs/config/src/hook_config.rs:37`), which is what the shipped `hooks-codex.json` already writes, so the manifest spelling needs no change.

Its post-tool payload carries `session_id`, `cwd`, `tool_use_id`, `tool_name`, `tool_input`, and `tool_response` (`codex-rs/core/src/hook_runtime.rs:294`), so pairing has the key it needs. Payloads also carry `agent_id` (`codex-rs/hooks/src/schema.rs:283`), `turn_id` and `model`.

Post hooks run only when the tool call succeeded (`codex-rs/core/src/tools/registry.rs:674`), but for shell commands `success_for_logging` is unconditionally true (`codex-rs/core/src/tools/context.rs:375`), so a nonzero exit still fires the hook. An aborted call does not (`context.rs:321`).

Codex exit semantics, which the exit-code rule above turns on: exit 0 with empty stdout is a clean allow, exit 2 with non-empty stderr blocks, and any other non-zero exit is a non-blocking failure (`codex-rs/hooks/src/events/pre_tool_use.rs:213-290`).

Claude Code's vocabulary is the widest of the three, and most of it is outside devkit's business. Events left unwired: `MessageDisplay`, `Notification`, `TeammateIdle`, `Elicitation`, `ElicitationResult`, `InstructionsLoaded`, `TaskCreated`, `TaskCompleted`, `DirectoryAdded`, `Setup`, `PostToolBatch`, `ConfigChange`, `FileChanged`, `UserPromptExpansion`, `PreModelSwitch`, `PostModelSwitch`. None carries a devkit action, none answers a question this design asks, and `MessageDisplay` fires while assistant text is being displayed, which would spawn a process per burst of output.

Cursor's `afterAgentResponse`, `afterAgentThought`, `beforeTabFileRead` and `afterTabFileEdit` are left unwired for the same reason. `workspaceOpen` is wired: it is the closest thing Cursor has to `CwdChanged` and carries the `cwd-changed` verb, with the caveat that it fires outside any agent session and so carries no conversation id.

The Cursor event list and payload shape come from the vendor's documentation, not from the `cursor-hooks` npm package's JSON schema. At `1.1.6` that schema publishes six events and omits `sessionStart`, which the currently shipped `hooks-cursor.json` uses and which does fire. The schema lags the product, so it is not a source to plan against.

Three things about Cursor could not be confirmed from the documentation and are settled by capturing a real payload before the manifest moves: whether empty stdout from `preToolUse` and `beforeSubmitPrompt` is an allow, whether a plugin `hooks.json` at `"version": 1` accepts `subagentStart`, `workspaceOpen` and `postToolUseFailure`, and Cursor's edit tool names for the `preToolUse` matcher. The shipped hook prints nothing on an allow from `beforeShellExecution` and works, which is evidence for the first but not proof for the generic trio.

## Fidelity of prompt text

`user-prompt-submit` records the prompt verbatim at `full`, which is the richest context for answering why an agent ran a given command and the worst thing in the corpus from a privacy angle. It is gated by the `prompt` key rather than the `command` one, resolving by the same downward clamp, and defaults to `off`, where the verb records that a prompt was submitted and none of its text. A corpus can carry full command text without carrying what the human typed.

## References

Each harness's hook vocabulary, to check against when a verb is added or a payload field is assumed:

- Claude Code: `https://code.claude.com/docs/en/hooks`, which carries the per-event payloads and the exit-code table. The settings schema at `https://json.schemastore.org/claude-code-settings.json` names the hook events but not their payloads.
- Codex: `https://developers.openai.com/codex/config-schema.json`, and the source of truth behind it at `codex-rs/protocol/src/protocol.rs` plus `codex-rs/config/src/hook_config.rs` in `openai/codex`.
- Cursor: `https://cursor.com/docs/hooks`. Not the `cursor-hooks` npm schema, which lags the product.

## Docs to update

`docs/agents.md` hook table, which grows from six rows to the full per-harness mapping and gains the three documentation links from the References section above, so the next person adding a verb checks the vendor rather than recalling it.

`docs/commands.md` for the `hook` and `hook-log` families, replacing the `harness` section.

`docs/configuration.md` for `[harness.log]`, and for the four places that name the retired commands: the `[harness]` table's description of what the hooks invoke, and the three surrounding paragraphs that spell `lockm hook <event>`.

`skills/using-devkit/references/locks.md`, whose lock protocol names `devkit harness shell`.

The AGENTS.md layout rows for `src/bin/devkit/` and `crates/devkit-common`, and the shell-hook invariant, which gains the exit-code rule.
