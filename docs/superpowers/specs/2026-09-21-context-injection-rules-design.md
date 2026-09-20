# Context injection and rules

Issue: [#62](https://github.com/AbysmalBiscuit/devkit/issues/62)

## Problem

A coding agent reads the rules file it starts next to and nothing below it.
Codex started at a repository root never sees `crates/foo/AGENTS.md`, so the
guidance nearest the code it is editing is the guidance it is least likely to
have. Repository-wide rules have the opposite problem: an agent that did read
them at startup has lost them by the time it edits the file they govern, either
to compaction or to distance.

devkit already sits on every tool call through its hooks and already knows which
files a call is about to write. That is enough to put the right guidance in
front of the agent at the moment it is about to matter.

## Scope

Two sources of injected context, one pipeline:

- **Files.** `[[context.files]]` entries in `devkit.toml` name a file to dump
  and the condition under which to dump it.
- **Rules.** A JSON rule index produced by `repo-rules-agent`, filtered to the
  rules governing the file about to be written.

devkit consumes the index. It never extracts rules, parses prose, or calls a
model; the hook path stays deterministic and free of network calls.

A user-facing `devkit rules` verb exposes the same matcher the hook uses, with
`query` and `stats` subcommands mirroring the extractor's own.

## Architecture

| Stage | Lives in |
|---|---|
| Subject: targets, cwd, harness, tool from the payload | `src/bin/devkit/hook/rules.rs` |
| Sources: config entries and the deserialized index | `devkit-rules` |
| Match: conditions, `governs`, language, task, severity floor | `devkit-rules` |
| Dedupe against the holder's fired-set | `src/bin/devkit/hook/rules.rs` |
| Render to markdown | `devkit-rules` |
| Emit | `src/bin/devkit/hook/rules.rs`, `src/bin/devkit/rules.rs` |

`devkit-rules` is a library over an index whose only IO is reading one JSON
file. It knows nothing about harnesses, payloads or sessions, because it has
three callers: the pre-tool-use stage, the session-start verb, and
`devkit rules`. Its tests are "given this index and this subject, which rules
come out", with no temporary directory.

The untracked `crates/devkit-rules/` copy of `devkit-locks` is deleted first.
It shares nothing with this design but the name.

### Session state

The ids already injected are a newline-delimited file at
`state_dir()/rules/<hash(holder)>`, appended. A rule's id is the index's own
`id` field; a file entry's id is its configured `path`.

The key is the **holder**, not the session: `holder_from_fields`
(`devkit-locks/src/hook.rs:36`) yields `session_id` for a top-level agent and
`session_id/agent_id` for a subagent. Keying on the session alone would let a
parent's injection suppress a subagent's, and the subagent's context never held
the rule the parent was shown.

`edit::release_subagent` and `edit::release_session` delete the file for the
holder they release. Those two verbs already run at exactly the right moments,
and without the delete `state_dir()/rules/` grows one file per session forever.

Writes are appends. A torn or interleaved line costs one duplicate injection,
so the reader skips lines it cannot parse rather than taking a lock. There is no
registry and no daemon: one append per event does not justify a write gate.

## Triggers

**`devkit rules context`** is a new verb, wired into each manifest's SessionStart
block beside the existing `devkit brief` (`hooks.json:13`, `hooks-codex.json:29`).
It injects repository-scope `must` rules, capped by `per_event_limit`, plus one
line naming `devkit rules query --path <p>`. It stamps what it emits into the
fired-set.

It is a verb rather than an addition to `devkit hook session-start` because only
`pre-tool-use` may write stdout from a `hook` verb (`AGENTS.md`,
`hook/mod.rs:14-17`), and on Claude Code a SessionStart hook's plain stdout is
appended to the agent's context, so a `hook` verb that printed would be a rule
violation with a live consequence.

Codex's `compact` SessionStart matcher (`hooks-codex.json:26`) runs it too. That
is intended: after a compaction the fired-set is cleared and the repository-scope
rules are exactly what the agent has lost.

**pre-tool-use** handles everything path-scoped, on write tools only. The shell
path's analyzer-derived write targets are best-effort and fail open, and a rule
that fires on a guess is worse than one that does not fire.

Query parameters are fixed per trigger: pre-tool-use passes `task =
code-generation` and the target's language; `devkit rules context` passes no task
and no language, and filters to `scope = repo` and `severity = must`. A rule
tagged only `code-review` therefore never fires on a write, which is correct.

### Subject resolution

The stage resolves its own targets. `parse_write`
(`devkit-locks/src/hook.rs:117`) returns `apply_patch` paths verbatim, relative
to the session cwd, and `claim` returns at `edit.rs:95` without resolving
anything when enforcement is switched off, so there is nothing resolved to
inherit.

For each raw target: join against the payload's `cwd` when relative, normalize
`..` lexically, then relativize against `Checkout::root()`. A target that escapes
the root after normalization is dropped. `resolve_against` (`edit.rs:162`) does
not normalize, so `/repo/src/../../etc/x` would otherwise survive a prefix strip
and pull in repository-root rules for a write outside the repository.

One call may carry several targets in several languages. Every target is matched
and the results are unioned before the cap applies.

### Denial and placement

Deny sites do not return from `guard`. `claim` prints its envelope at
`edit.rs:109` and `edit.rs:118` and returns a `Vec<String>` to `edit.rs:37`,
which falls through to the flush and the record exactly as an allow does. A
rules stage attached to that fall-through would print a second JSON object after
a denial. Claude Code parses a hook's whole stdout as one document, so two
objects parse as none, and stdout that is not JSON with exit 0 is read as plain
text carrying no decision: the denial is suppressed and the write proceeds.

So `claim` is refactored to return its verdict instead of printing it, leaving
`guard` with a single emission site. The rules stage is reachable only from the
arm where the verdict is an allow. Nothing is stamped on a denial, so the agent's
next attempt on that path is a fresh `pre-tool-use` that carries the rules.

Four placement rules, each of which is the safety argument rather than a
convention:

1. No part of the stage runs before the verdict is final. Loading config or
   parsing the index earlier, sharing the read with `enforcement_enabled_in`,
   puts a fallible step in front of a denial: a malformed `devkit.toml` under
   `?` exits 1, which Claude Code treats as a non-blocking error, and a large
   index parse can spend the manifest's four-second budget (`hooks.json:37`),
   whose timeout also allows the call.
2. The stage returns `()`. It has no `Result` and no `?`, so it has no path to
   an exit code.
3. It runs under `catch_unwind`, the profile `shell.rs:29` already pins.
4. It emits through `print_envelope` (`shell.rs:467`) in one write and flush,
   before `harness_log::record`. `println!` panics on a closed pipe, and
   `edit.rs:53-56` documents why the envelope precedes the record.

The emitted object carries `additionalContext` and no permission decision, the
shape `harness::warn_shell_json` already produces and the Codex pre-tool-use
schema already accepts. A `permissionDecision` on the allow path would turn
devkit into something that auto-approves writes the user would otherwise be
asked about.

## The index

Found at the extractor's per-repository cache path, overridable by
`[rules] index`.

The cache directory name is `<basename>-<sha8>`, where `sha8` is the first eight
hex characters of the SHA-256 of the canonicalized repository path, and
`basename` is that path's final component with every run of characters outside
`[a-zA-Z0-9._-]` replaced by `-`, stripped of leading and trailing `-`,
lowercased, falling back to `repo` when empty (`repo-rules-agent`
`rules/paths.py`).

**The hashed path is `Checkout::main_worktree()` (`git.rs:489`), not
`Checkout::root()`.** Every devkit branch lives in its own worktree under
`../devkit-worktrees/`, so an index built once in the main checkout is keyed by
the main checkout's path. Hashing the worktree root would miss it from every
worktree, which is where the work happens.

The cache root is platformdirs' user cache directory for the app name
`repo-rules`: `$XDG_CACHE_HOME/repo-rules` or `~/.cache/repo-rules` on Linux,
`~/Library/Caches/repo-rules` on macOS, and a path under `%LOCALAPPDATA%` on
Windows whose exact nesting the implementer confirms against platformdirs rather
than taking from this document. `devkit_common::paths::cache_dir` is XDG-only
and cannot be reused.

There is no staleness check. A freshly created worktree gives every source file
a newer mtime than the index, so an mtime comparison reports stale on every
event in exactly the case devkit is built around, and a per-event notice survives
dedupe as pure noise. A stale index injects like any other.

## Vocabulary

The extractor coerces every value onto its vocabulary before validation:
`languages_for` drops unknown languages and falls back to `all`, `category_for`
falls back to `best_practice`, and `topics_for` drops unknown topics
(`rules/vocabulary.py:139-172`). `tasks`, `severity` and `scope` are pydantic
`Literal`s, and a rule carrying anything else fails `model_validate` and is
dropped at extraction (`rules/extractor.py:326-343`). An index therefore never
contains a value outside the vocabulary it was built with.

That vocabulary is still not the built-in one: a repository extends languages,
categories and topics through `Vocabulary.with_extras`. So the open half and the
closed half are split:

- **`Task`, `Severity`, `Scope`** are closed enums, `strum` deriving
  `EnumString` and `Display`. A rule carrying an unknown value is dropped, which
  is what the extractor already did. `Severity` derives `Ord` so the floor is a
  comparison.
- **Language, category and topic** are normalized strings, not enums. Their only
  operations are parse, compare and display; an enum with an `Other(String)`
  variant would pay a wildcard arm at every match site and allocate for the
  repository-extras case anyway.

Normalization is one function per kind:

- **Languages**: trim, lowercase, strip a leading `.`, then the alias table.
  `TypeScript`, `typescript`, `ts`, `.ts` and `TSX` all land on `typescript`.
  The alias key is lowercase rather than snake_case, because `c#`, `c++` and
  `pl/pgsql` are real entries and snake_casing destroys them.
- **Categories and topics**: `vocabulary_key`, so `Code-Style`, `code style` and
  `code_style` agree.

Both sides normalize at load. The extractor normalizes the query side on every
comparison (`rules/query.py:220`), which is correct but repeated.

The alias table and built-in categories are ported into Rust and kept in sync by
hand. A follow-up teaches devkit to read `.agents/repo-rules-agent.toml` when it
is present, so a repository's extra vocabulary is honoured by both tools.

### Deserialization

The extractor writes with `exclude_none=True`, so every pydantic default is a
serde default: `languages` defaults to `["all"]`, `severity` to `should`, and
`tasks`, `topics` and `directory` to empty. `content` is absent unless embedded.

`id` is a computed field and is present in the JSON, but it is
`sha256(f"{source_file}:{title}")` truncated, so two rules sharing a title in one
file share an id and the fired-set treats them as one. Acceptable: they are
near-duplicates by construction.

## Matching

A rule matches a target when `governs(rule.directory, target_path)` holds, its
languages include the target's language or `all`, its tasks include the task, and
its severity meets the floor. `governs` takes the path, not its directory
(`rules/models.py:17`), and the relativize-and-drop step above is what the
extractor does at `rules/query.py:199-212`.

Two deliberate divergences from the extractor's query, both leniency in the face
of imperfect extraction:

1. **An untagged rule matches every task.** An empty `tasks` list means the model
   did not answer, not that the rule applies to nothing. The extractor's
   `task in r.tasks` (`rules/query.py:214`) drops those rules entirely.
2. **Severity is a floor in the hook, exact at the CLI.** `devkit rules query
   --severity must` keeps the extractor's meaning so the two agree.
   `--min-severity` is the new flag, and the hook uses it.

Ranking is ported: rules about a requested topic first, then deeper directories,
then severity, then discovery tier. `is_about`'s regex fallback over title and
description (`rules/query.py:143`) is ported only for `devkit rules query
--topic`, because the hook passes no topics.

## Configuration

```toml
[rules]
enabled = true
min_severity = "should"
per_event_limit = 5
max_file_bytes = 16384
max_event_bytes = 65536

[[context.files]]
path = "crates/devkit-ports/AGENTS.md"

[[context.files]]
path = "docs/deploy.md"
when = { harness = ["codex"], env = { DEPLOY_TARGET = "staging" } }
```

An entry with no `when` fires when the edited target is under the injected
file's own directory, which makes the case this issue opens with a one-line
entry. Explicit conditions are a path glob over the targets, an environment
variable present or equal to a value, and a harness match. All conditions on an
entry must hold.

Semantics the implementer does not get to choose:

- `path` resolves against the directory of the layer that declared the entry, so
  a `devkit.toml` in a subdirectory names its neighbours without repeating its
  own path.
- The array replaces rather than appends across layers, matching `HooksConfig`
  (`devkit-config/src/lib.rs:214`).
- `env` is read from the hook process environment, which the harness supplies.
  It is not the agent's shell.
- A file over `max_file_bytes` is skipped; the event's other injections still go
  out. `max_event_bytes` caps the rendered total.
- File dumps count against `per_event_limit` alongside rules.
- `harness = ["cursor"]` never fires on a write: Cursor wires only
  `beforeShellExecution` as a pre-tool-use hook (`hooks-cursor.json:25`) and has
  no edit hook at all.

Each config type carries its `devkit.toml` example as a doctest, and
`schema/devkit-config.json` is regenerated.

## `devkit rules`

`query` takes `--task`, `--lang`, `--scope`, `--severity`, `--min-severity`,
`--path`, `--topic`, `--limit` and `--format table|json|prompt`.

`stats` prints the rule and file counts and breakdowns by file, severity, task,
language, directory and topic, and reports files whose extraction failed.

Both resolve the index the way the hook does. They beat the Python equivalent on
speed for uninteresting reasons, no interpreter start and no validation pass, and
on robustness for the reasons under Matching.

## Failure

The subsystem fails open and silent throughout. A missing index, an unreadable
one, one that does not deserialize, a target outside the repository root, a file
over its cap: nothing is injected, nothing is denied, the exit code does not
move. A malformed index earns one line on stderr, because that is a breakage
rather than an absence.

## Testing

- Table tests in `devkit-rules` over a fixture index: alias resolution including
  `c#` and `.tsx`, `governs` at a directory boundary, a target normalized out of
  the root, untagged-task leniency, the severity floor, rank order, condition
  evaluation, an unknown `severity` dropping its rule.
- Binary tests feeding a payload through `pre-tool-use` against a temporary index
  and asserting the emitted JSON exactly, including a multi-target call whose
  targets differ in language.
- **The denial test**: a lock-conflict deny, with a matching rule in the index
  *and* a matching `[[context.files]]` entry, asserting stdout byte-for-byte
  against the current output and asserting the fired-set file is unchanged. A
  test over the `Unusable` deny alone does not reach the path that broke.
- A test that a subagent holder injects a rule the parent holder already fired.

## Out of scope

- Extraction of any kind. devkit does not read prose or call a model.
- The shell path's write targets.
- Injection before a write that the guard denies.
- Index staleness.
- Reading `.agents/repo-rules-agent.toml`, which is the follow-up above.
