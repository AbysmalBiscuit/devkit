# Required args across every `--arg` surface

**Date:** 2026-09-16
**Status:** approved
**Issue:** https://github.com/AbysmalBiscuit/devkit/issues/68

## Problem

A caller can drop an `--arg` that the project expects it to pass, and devkit
says nothing. The run proceeds on whatever constant `[templates.variables]`
happens to hold.

Required-ness is derived, and the derivation has exactly one lever.
`task::required_args` is `args(cfg, name)` minus every name
`[templates.variables]` supplies, so an arg is optional precisely when the
project gives it a value. Declaring a default is therefore the only way to make
an arg optional, and doing so makes it omittable by every caller at once.

An agent that drops `--arg msg=...` from a commit task does not fail. It
commits under the project constant, and nothing in the run reports that an arg
went unsupplied. The command guard puts this one redirect away from a typed
`git commit`: `guard = true` sends the command to `devrun task commit`, and the
redirect's usage hint names only the derived-required args.

A project needs to say "this one must be passed, by an agent, even though a
default exists". Today it cannot.

## What exists today

Verified against `main` at `73e15f9`.

Four commands accept `--arg`:

| Command | Templates it renders | How `--arg` is validated |
|---|---|---|
| `devrun task <name>` | the task's `run` and `env`, across sequence steps | name must be read by a template, or declared in `[templates.variables]` |
| `issue pr` | `pr_title`, `pr_body` | name must be declared in `[templates.variables]` |
| `issue review request` | `review_request` | same |
| `issue review finish` | `review_finish` | same |

`[templates.variables]` is `BTreeMap<String, String>`. It is read as a plain
name-to-value map in six places, including `task::variables`,
`issue/pr/create.rs` (`tmpls.variables.clone()` then extend with the parsed
`--arg`) and `issue/review/mod.rs`.

Variables are merged *underneath* the render context.
`template::merged_context` inserts with `or_insert_with`, and
`task::variables` does `vars.extend(args)`. Both are fallback semantics: an
entry is used only when nothing else supplies the name.

An absent name is not an empty string. minijinja runs under
`UndefinedBehavior::Strict`, so reading an undeclared name is an error:

```
Error: scanning templates of app `api`
Caused by:
    0: scanning template `hello {{ msg }}` for port references
    1: rendering template
    2: undefined value (in t:1)
```

A caller's nature is already classified, in `devkit-locks::ident`.
`HARNESS_SESSION_VARS` names the environment variables a
coding-agent harness sets (`CLAUDE_CODE_SESSION_ID`, `CODEX_SESSION_ID`), and
`decide_anchor_pid` documents "agent-via-Bash sessions (no tmux, no tty)".
`decide_anchor_pid` is pure, with `anchor_pid()` doing the syscalls at the
edge.

## Design

### Config shape

Three forms in `[templates.variables]`, the first unchanged:

```toml
[templates.variables]
team = "platform"                              # plain string, as today
msg = { default = "wip", required = "agents" } # has a default, agents must pass it
ticket = { required = "always" }               # declared, no default, everyone must pass
```

```rust
#[serde(untagged)]
pub enum VariableDecl {
    Value(String),
    Table {
        #[serde(default)] default: Option<String>,
        #[serde(default)] required: Required,
    },
}

#[serde(rename_all = "lowercase")]
pub enum Required { #[default] Never, Humans, Agents, Always }
```

The key is `default`, not `value`, because that is what these entries are: a
fallback used when nothing else supplies the name. The `Table` variant carries
`deny_unknown_fields`, for the reason `RunArg::Split` already documents in this
repo. Under untagged matching a misspelled key beside a well-formed pair
deserializes silently, and the author sees no diagnostic while their guard does
nothing.

Per task, a map:

```toml
[tasks.commit]
run = ["git", "commit", "-m", "{{ msg }}"]
required_args = { msg = "always" }
```

`Templates` grows two accessors over the one field. `defaults()` flattens to
name-to-value for rendering and omits valueless entries, so `ticket` is absent
from the render context until a caller passes it and strict-undefined still
applies. `declared()` returns every name including valueless ones, which is
what the `issue` allowlist checks. Without that split `ticket` would be
unpassable, because `parse_args` rejects any key not in the declared map.

Untagged serialization keeps `team = "platform"` round-tripping as a bare
string, so `toml::to_string` output and the existing round-trip tests are
unaffected.

### Resolution rule

```
required(name) = has_no_default(name)
              || binds(declared(name), caller)

declared(name) = task `required_args` entry, else variable entry, else Never

binds(Never,   _) = false          binds(Always,  _) = true
binds(Agents,  c) = c is Agent     binds(Humans,  c) = c is Human
```

Two precedences, deliberately not the same one:

- A task declaration beats a variable declaration.
- Neither beats the derivation. The derived rule is a floor.

That floor is what keeps every existing config behaving as it does today. It is
also what gives `never` its only real use: relaxing a variable-level marking
for one task. `never` cannot lower the floor, so it is meaningful exactly where
a default exists.

| effective marking | has default | agent | human |
|---|---|---|---|
| none (`never`) | no | required | required |
| none (`never`) | yes | optional | optional |
| `always` | yes | required | required |
| `agents` | yes | required | optional |
| `humans` | yes | optional | required |
| `never` (task overriding a variable `always`) | yes | optional | optional |
| `always`/`agents`/`humans` | no | required | required |
| `never` | no | config error | config error |

Rows one and two are today's behaviour untouched. Row seven is redundant but
legal on purpose: a marking is a floor, so it keeps holding if someone later
adds a default. Row eight is an error at either declaration level, because
`never` asks to relax a requirement with nothing to fall back on, and honouring
it would replace a named error with the strict-undefined chain above.

### Caller identity

```rust
pub enum Caller { Human, Agent }

pub fn decide_caller(harness_session: bool, is_tty: bool) -> Caller  // pure
pub fn caller() -> Caller                                            // reads env + TTY
```

`caller()` resolves in this order: `DEVKIT_CALLER` when set to `agent` or
`human` (case- and whitespace-insensitive; blank or unrecognised falls
through, the tri-state shape `harness::parse_env_override` already uses), else
`Agent` when any `HARNESS_SESSION_VARS` entry is set, else `Agent` when
`std::io::stdin()` is not a terminal, else `Human`.

The harness variable leads because it is positive evidence of a coding agent
rather than the absence of a terminal, so a pipe, a cron job and
`devrun task foo < /dev/null` stop being mistaken for agents. The TTY check
stays as a backstop, because `HARNESS_SESSION_VARS` lists only Claude Code and
Codex while the hook family also serves Cursor, which is recognised by a
payload field rather than an environment variable.

The classification leans toward `Agent` on purpose. An agent misread as human
means the `agents` requirement never fires and the agent silently takes the
default, which is the defect this whole design exists to close. A human misread
as agent means being asked for an arg they expected to be defaulted:
recoverable, and never silent.

`caller()` is called once per command at the CLI edge and threaded down. No
config key sets it, unlike `enforce_writes`: caller identity is a property of
who is running right now, and a file checked into a repo cannot know that.

Two callers never consult it. The command guard inside
`devkit hook pre-tool-use` constructs `Caller::Agent` outright, because its
stdin is a harness pipe and a hook only fires because an agent acted. Any
future MCP task action does the same, for the same reason. There is no MCP
surface taking `--arg` today.

### Per-surface enforcement

One entry point, pure:

```rust
pub fn missing_args(
    cfg: &Config,
    task: Option<&str>,        // None for the issue surfaces
    reads: &BTreeSet<String>,  // names this run's rendered templates read
    given: &BTreeMap<String, String>,
    caller: Caller,
) -> Vec<String>
```

A requirement binds only when the run actually renders a template that reads
the name. Each surface computes `reads` with `template::undeclared`, which
`task::args` already uses:

- `devrun task` passes the existing `task::args()` set: template reads across
  every sequence step, minus the port names the registry supplies and the issue
  fields the worktree fills. This replaces the requirement half of
  `check_args`. The other half, rejecting an `--arg` nothing reads, is
  unchanged.
- `issue pr` passes the names of `pr_title` and `pr_body` together, minus that
  command's context keys. Both always render: `--pr-title` and `--pr-body` fill
  each template's `{{ input }}` rather than replacing it, and the defaults are
  literally `{{ input }}`.
- `issue review request` and `issue review finish` pass their single
  template's names the same way.

"Rendered" here means the templates the command can render, taken statically,
not the ones a given run reaches. `issue pr` builds its title and body in
closures that `ensure` may not call for a PR needing no update, and gating on
that would move the error after the push. Failing before any work matches what
`task::resolve` already promises. What the rule does exclude is the other
commands' templates: a name only `review_finish` reads never binds `issue pr`.

A `required_args` entry naming something the task never reads is an error, as
`require_live`'s unknown-app check already is, and the task shows as `invalid`
in the listing rather than being hidden.

### Errors and listings

The error keeps today's shape and names the audience when the requirement is
caller-specific:

```
task `commit` needs --arg msg=...
task `commit` needs --arg msg=... (required for agents)
issue pr needs --arg ticket=...
```

The parenthetical exists so an agent's report is explicable. Without it the
agent says "it needs `msg`", a human runs the same command, it succeeds, and
nothing accounts for the difference.

The `devrun task` args column stays caller-relative: bare when required for
whoever ran it, bracketed when not. A human sees `[msg]` for an agents-only
marking; an agent sees `msg`. A human inspecting the agent's view runs
`DEVKIT_CALLER=agent devrun task`.

Required-ness is computed once per row through the shared function. The listing
never re-derives it. That is the defect this design is most exposed to: the
earlier attempt at this feature re-derived required-ness inline in `list()`,
and the listing disagreed with the check.

The command guard's hint is unchanged in mechanism and correct by
construction, since the guard is always `Caller::Agent`.

## Crate placement

- `devkit-config` holds shape only: `Required`, `VariableDecl`, the
  `defaults()` and `declared()` accessors, and the load-time check that
  `never` without a default is an error. It is the leaf crate (anyhow,
  schemars, serde, toml) and shape validation needs nothing else.
- `devkit-common` holds behaviour: `Caller`, `decide_caller`, `caller`, and
  `missing_args`. It sits above `devkit-config` and below `devkit-locks`,
  `devkit-ports` and the binary, so every consumer reaches it, and `harness.rs`
  is already its coding-agent glue.
- `HARNESS_SESSION_VARS` moves down from `devkit-locks::ident` into
  `devkit-common`, and `ident.rs` reads it from there. One list, two consumers.

## Out of scope

Per-arg descriptions, per-surface opt-in lists, a config key for caller
identity, and any requirement on a name that is neither declared in
`[templates.variables]` nor read by a template. None was asked for, and each
adds a config surface to explain.

## Migration

None. Rows one and two of the truth table are current behaviour, a plain
string entry still parses and still serializes as a bare string, and
`schema/devkit-config.json` regenerates from the derives with
`DEVKIT_UPDATE_SCHEMA=1`.

`docs/configuration.md` gains the new keys and must state that minijinja's
`default` filter and the config's `default` key are different things: the
config key decides required-ness, the filter does not. The existing sentence
"A `| default(...)` or `is defined` in the template does not make an arg
optional" stays true and now needs that contrast drawn explicitly.

## Testing

Three layers, each cheap because the decisions are pure.

- The rule: table-driven over the truth table crossed with both callers. No
  tempdir, no env, no TTY.
- `decide_caller`: four combinations, pure.
- Config load: `never` without a default errors and names the variable; the
  reserved-name check still passes; all three declaration forms round-trip.
- End to end: `tests/task_cmd.rs` gets the agent path for free, having no TTY,
  and the human path via `DEVKIT_CALLER=human`. The issue surfaces are
  `gh`-backed and use `tests/common/ghfake.rs`.

Forcing `Agent` in a test needs no override, since setting a harness session
variable does it. Forcing `Human` is the case `DEVKIT_CALLER` exists for: no
environment variable can make a process a terminal, so without it the `humans`
branch cannot be exercised through the real binary at all.

## Superseded

This replaces `2026-09-16-task-required-args-design.md` and its plan, which
scoped the feature to `devrun task` alone, made required-ness a bare list with
no caller distinction, and read the issue's "when an agent calls tasks etc." as
covering one surface. PR #83 implements that rejected design.
