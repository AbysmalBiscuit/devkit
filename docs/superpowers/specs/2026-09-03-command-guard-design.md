# Command guard — design

## Goal

Steer coding agents away from running commands devkit already has a wired-up path for. An agent that launches an app's dev server directly collides on ports, leaves the process untracked so `devrun down`/`logs`/`status` cannot see it, and dodges the `prd` doppler check. An agent that retypes a task's command by hand loses the app directory, the layered env, and the port values only the registry can supply.

The guard reads a harness's pre-execution hook payload, decides whether the command has a devkit equivalent, and denies with the exact replacement command.

## Scope

- A `devkit harness shell` subcommand that reads a hook payload on stdin and emits the harness's deny envelope, or exits silently.
- A `[harness] enforce_commands` gate and a `[harness.commands.<name>]` rule table in `devkit.toml`.
- A `guard` field on `[tasks.<name>]`.
- Registration in all three hook manifests.

### Non-goals

- Enforcing anything. The guard never blocks a command it cannot redirect, and never blocks when it cannot decide.
- Replacing `run::assert_not_prd`. That check stays where it is and remains the only thing standing between a launch and a production doppler config.
- Write locks. `lockm hook pretooluse` keeps that job unchanged, and shell-level writes made through `Bash` stay outside the harness's scope.
- Cursor write-lock coverage. `hooks-cursor.json` has no pre-tool hook today; adding one is its own change, though this design's payload work unblocks it.

## Background

`devkit-locks` already does this shape of work. `crates/devkit-locks/src/hook.rs` parses a harness payload, decides, and emits `deny_json`; `enforcement_enabled` gates on `[harness] enforce_writes` across an env override, the project layers, and the global config. `hooks/hooks.json` registers it under a `PreToolUse` matcher naming the write tools. This design follows that structure rather than inventing a second one.

The prior art outside the repo is a per-project shell hook that hardcodes a framework list and resolves app names by exact prefix match. Everything it knows that is project-specific belongs in config; everything it knows that is an ecosystem fact belongs in devkit, the way `devkit-docs` already ships importer graphs for pnpm, bun, npm, Cargo, and uv.

## Design

### Entry point

`devkit harness shell` — a new top-level `Harness` subcommand on the `devkit` binary, carrying one event today.

Top-level rather than under `devrun`, because the first rule source is workspace policy ("this workspace is bun-only") with nothing to do with the dev-server runner. Adding a namespace rewrites no shipped interface; `lockm hook <event>` stays exactly where it is, so the cost that ruled out consolidating all hook glue does not apply here.

`harness`, not `hook`, because `[hooks]` already names something else in this config: the lifecycle commands devkit *runs* after `issue setup` and `issue end`. `devkit hook shell` would read as "run the shell lifecycle hook". `harness` is the table that configures this feature, so the subcommand and its config agree.

The event argument is `shell` rather than a harness's own event name, because the three do not agree on one: Claude Code and Codex fire `PreToolUse` with a `Bash` matcher, Cursor fires `beforeShellExecution`.

The logic lives in a new `guard` module in `devkit-ports`, which already owns `apps`, `task`, and `run`. The `devkit` binary already depends on it.

Two supporting moves keep the crate graph acyclic:

- `deny_json` and the payload helpers move from `devkit_locks::hook` into a new `devkit_common::harness`, together with `enforcement_enabled`, `global_config_path` and `parse_env_override`, generalized over which flag they read. `enforcement_enabled` calls `devkit_common::git::main_checkout`, so it cannot go any lower than `devkit-common`.
- `HarnessSection` moves from `devkit-locks` into `devkit-config`, where every other config table's shape lives. It is not added as a field on `Config`; see below.

### Payload shapes

| | Claude Code / Codex | Cursor |
|---|---|---|
| event | `PreToolUse`, matcher `Bash` | `beforeShellExecution` |
| discriminator | `hook_event_name` present | `hook_event_name` absent |
| command | `tool_input.command` | top-level `command` |
| cwd | `cwd` | `cwd` |
| deny | `hookSpecificOutput.permissionDecision` | `{"permission": "deny", "agent_message": "…"}` |

Discriminate on `hook_event_name`, not on the presence of `tool_input`. Cursor has a *generic* `preToolUse` hook whose payload also carries `tool_input`, so a `tool_input` test would emit a Claude Code envelope into a Cursor session if anyone ever registered that event. Codex's unified exec (`exec_command`) matches as `Bash` and needs no separate arm.

Cursor's deny reason goes in `agent_message`. `user_message` is shown to the human; only `agent_message` reaches the agent, and the whole point is handing the agent a command it can retry with.

Deny rather than ask. A denial hands the agent an actionable correction; an approved "ask" runs the original command verbatim and a rejected one tells the agent only that a human said no.

A payload that does not parse, or that carries no command string, exits 0 silently. It is not a failure worth a warning: harnesses send events this hook does not model.

### Config

```toml
[harness]
enforce_commands = true

[harness.commands.bun-only]
programs = ["node", "npm", "npx", "pnpm", "yarn", "tsx", "ts-node", "nodemon"]
reason   = "This workspace is bun-only. Use bun / bunx / bun run."

[harness.commands.compose-v2]
programs = ["docker-compose"]
args     = ["up"]
reason   = "Use `docker compose up`, not docker-compose."

[harness.app_match]
fuzzy     = true
max_typos = 1
min_score = 60
```

`enforce_commands` resolves exactly as `enforce_writes` does: the `DEVKIT_ENFORCE_COMMANDS` env override wins, else any project layer opting in turns it on, else the global config.

A rule names `programs` and optionally `args`. It never names a regex. The command parsing below is devkit's job, and a rule that had to restate it would get it wrong. `args` matches as a prefix of the typed arguments.

Rules inherit down the layer stack the way every other config table does, and by the same code: the probe merges the `[harness]` tables it reads through `devkit_config::merge_layers`, which becomes `pub`. Tables merge key by key, so a child layer adds rule names to what its parents declared; arrays replace wholesale, so a child that redefines a parent's rule *by the same name* overrides exactly the keys it sets and inherits the rest. That override is the only off switch, and it is the same one every config value has. There is no bespoke suppression mechanism, and no second copy of the merge semantics.

`[harness.app_match]` tunes the last rung of app-name resolution, described under "Naming the app" below. It is an ordinary table under `[harness]`, so it merges the same way the rules do: a layer setting only `max_typos` inherits the other two.

The global config participates. It is the lowest-precedence layer for rules exactly as it is for the gate, so a machine-wide `[harness.commands.*]` applies everywhere and any project layer can override it by name.

A project needing to exempt a subtree writes an overriding rule of the same name there:

```toml
# apps/legacy-node/devkit.toml
[harness.commands.bun-only]
programs = []
```

Empty `programs` matches nothing, which is what makes that exemption work. Absent `programs` is invalid. Validation runs on the *merged* rule, not on each layer's fragment, so the child above is valid even though it names no `programs` of its own; a rule that names none after the merge is skipped with a warning.

`[tasks.<name>]` gains `guard: Option<bool>`, which forces the decision below in either direction.

### Reading the config

Three invariants, all load-bearing.

**The `[harness]` probe reads each key independently.** Today `harness_flag_in` deserializes the whole table into one struct and treats any parse failure as "off":

```rust
toml::from_str::<HarnessProbe>(body).map(|p| p.harness.enforce_writes).unwrap_or(false)
```

Adding `commands` to that struct would make a mistyped rule (`programs = "node"` instead of `programs = ["node"]`) fail the whole probe, so the same file's `enforce_writes = true` would read as false. A typo in the fail-open feature would silently disable the fail-closed one.

So the probe parses the body as a `toml::Table`, takes `harness`, and reads `enforce_writes`, `enforce_commands`, `commands` and `app_match` separately, deserializing each rule on its own and skipping a rule that will not parse. An `app_match` that will not parse falls back to the defaults with a warning rather than taking its siblings down. `HarnessSection` stays a typed struct so `devkit schema` describes the table, but nothing at runtime deserializes the table through it.

Skipped rules are *returned* to the caller, not printed. `enforcement_enabled` is shared with `lockm hook pretooluse`, and a rule warning printed from inside it would fire on every `Edit` as well as every `Bash`. Only `devkit harness shell` prints them.

For the same reason `HarnessSection` is *not* added as a field on `Config`. `Config` has no `harness` field today, so a guard-rule typo cannot make `devrun up` or `issue status` fail. The schema already reaches the table through the composite `Document` root in `src/bin/devkit/schema.rs`; the move to `devkit-config` only changes the path that struct names.

What the per-key probe does *not* fix, and cannot: `project_layers` calls `apply_cutoff`, whose `declares_root` returns `Err` on a TOML syntax error in any layer it scans, and `harness_enabled` maps that to `false`. A stray `[[[` in one `devkit.toml` turns *both* flags off for the whole stack. That is today's behaviour for `enforce_writes` and this design does not change it, but the cross-feature test covers it so nobody discovers it later and calls it new.

**The probe merges, the resolver is not involved.** Source 2 is answered without a full config load, so rule inheritance cannot ride on the resolver's merge. The probe reads the global file and every project layer, parses each to a `toml::Table`, projects out `[harness]`, and merges them lowest-to-highest through the same `devkit_config::merge_layers` the resolver uses. Making that function `pub` is what keeps this from being a second implementation of the same semantics.

Note this differs from the *gate*, which combines layers with `any` rather than by precedence, because enforcement ratchets on. Rules merge; the flag ratchets. Both read the same set of layer files in one pass.

**The guard is read-only.** It allocates no port, writes no registry row, and takes no lock. Concretely: it never calls `task::resolve` or `resolve_step`, because `resolve_command` calls `registry::alloc` and writes a pid-less reservation. The hook fires on every Bash call across every concurrent session; a reservation per keystroke would be a disaster. It reads the catalog through `load::load_quiet`, since `load::load` prints skipped apps to stderr on every call and those gaps are not this command's business.

One inherited quirk, worth naming because it makes sources 2 and 3–5 see different layer sets: `$DEVKIT_CONFIG` is the *sole* layer for the full resolver (`discover`), but only replaces the global file for the harness probe, and the probe reads the global file even when `[config] root = true` drops it from the resolver. Left as is; changing it is a change to `enforce_writes` semantics.

### Parsing the command

The command string is a shell fragment, so it is lexed, not regex-scanned. The prior-art shell hook regex-scans and gets this wrong; porting the defect into Rust would be the worst outcome of this change.

A word-splitter is not enough. `shell-words` and `shlex` strip quotes and hand back a flat `Vec<String>` with no operator positions, so the operator split below has nothing to split on, and `2>&1` would have its `&` read as a separator. This needs a small purpose-built lexer, which is the honest scope: single-pass, tracking quote state, emitting words and operators.

What it must recognise:

- **Quoting.** `'…'`, `"…"`, and backslash escapes. A quoted string is one opaque token, never a place to find a command word.
- **Operators**, unquoted only: `|`, `||`, `&`, `&&`, `;`, `(`, `)`, `$(`, and newline. These end a segment.
- **Redirections** as words, not operators: `>`, `>>`, `<`, `2>`, `2>&1`, `&>`. A redirection and its target are consumed, not treated as a split.
- **Heredocs.** `<<WORD`, `<<-WORD`, `<<'WORD'`: skip to the terminator line and treat the body as inert. This is the case that matters most in practice, because `cat > notes.md <<EOF` bodies are exactly where an agent writes the words "next dev" and "uvicorn app". Without heredoc handling, newline-as-separator turns every line of that body into a segment and the guard denies a file write.

`--` is deliberately **not** a separator. It exists in the prior-art regex only to model `doppler run … --`, which runner stripping already handles, and as a separator it denies `cargo run -- next dev` and `rg -- vite`.

Per segment, before matching:

- Strip leading `VAR=value` assignments. `env FOO=1 vite` is a `vite` launch.
- Strip process wrappers that pass their argv through: `nohup`, `setsid`, `exec`, `time`, `timeout <duration>`. `nohup bun run dev > /tmp/dev.log 2>&1 &` is the canonical agent launch and misses entirely if the command word stays `nohup`.
- Strip runner prefixes: `bunx`, `bun run`, `npx`, `pnpm exec`, `uv run`, `uvx`, and a `doppler run … --` wrapper. Stripping applies to all of sources 2 through 5, not only the catalog. Remember what was stripped; the task predicate below needs it.
- Compare the command word by basename, so `./node_modules/.bin/vite` is a `vite` launch. `run::assert_not_prd` already matches `doppler` this way.

Cases this must get right, all of which the unlexed version gets wrong:

| Command | Verdict |
|---|---|
| `git commit -m "fix: crash under uvicorn; retry"` | pass (the `;` is quoted) |
| `gh pr create --body "... && next dev ..."` | pass |
| `cargo run -- next dev` | pass |
| `rg -- vite` | pass |
| `rg vite`, `cat uvicorn.log` | pass (not command words) |
| `cat > notes.md <<EOF` / `next dev` / `EOF` | pass (heredoc body is inert) |
| `foo && next dev` | deny |
| `cd x; uvicorn app` | deny |
| `nohup bun run dev > /tmp/dev.log 2>&1 &` | deny |

### Matching

Each segment is evaluated independently. Sources resolve in order, first match wins:

1. **The command word is a devkit shim** (`devkit`, `devrun`, `lockm`, `portm`, `docm`) — skip the segment. Never gate the tools being promoted.
2. **`[harness.commands.*]`** — deny with the rule's `reason`. Decided from the `[harness]` probe alone, so no full config load happens.
3. **`[tasks.<name>]`** — deny, redirecting to `devrun task <name>`.
4. **`[apps.<name>].launch`** — deny, redirecting to `devrun up <app>`.
5. **The built-in dev-server catalog** — deny, redirecting to `devrun up <app>` when an app resolves, else naming `devkit config apps`.

Sources 3 through 5 need the resolved config, so they are reached only after the gate has passed and no user rule has matched. The first segment to produce a denial decides the whole command; later segments are not evaluated.

#### What "matches a task" and "matches a launch" mean

Sources 3 and 4 compare the typed segment against an argv vector from config, and those vectors carry minijinja (`{{ port }}`, `ports['api']`, `[templates.variables]`) and their own `doppler run` wrappers.

Both sides are normalized first: a `run` or `launch` goes through the same assignment, process-wrapper and runner-prefix stripping the typed segment does, so `bun run dev` in config and `bun run dev` typed reduce to the same thing.

Reduce the normalized config argv to a **signature**:

1. Truncate at the first template-bearing token or the first flag token (`-`-prefixed), whichever comes first, dropping it and everything after. What survives is the command word plus its leading positionals, which is the part a human retypes.
2. Reject the signature when a bare positional survives *after* the cut, meaning a token that is neither a flag nor a template. Such a launch carries a verb the signature does not, so matching on the prefix would deny every sibling verb.
3. Reject a one-token signature whose token is a generic interpreter or multiplexer: `python`, `python3`, `node`, `bun`, `deno`, `docker`, `cargo`, `go`, `uv`, `sh`, `bash`. Everything they run looks alike from the outside.

Match when the typed argv starts with the surviving signature.

Rule 1 exists because truncating at the template alone assumes the template sits last, and it usually does not. Rules 2 and 3 keep rule 1 from over-firing in the other direction:

| Normalized config argv | Signature | Because |
|---|---|---|
| `["nitro", "dev", "--port", "{{ port }}"]` | `["nitro", "dev"]` | nothing bare after the cut |
| `["uvicorn", "app:app", "--reload", "--port", "{{ p }}"]` | `["uvicorn", "app:app"]` | same |
| `["dev", "--", "--port", "{{ port }}"]` | `["dev"]` | an app's `bun run dev`, after runner stripping |
| `["docker", "compose", "-p", "{{ p }}", "up"]` | none (rule 2) | `up` survives the cut, so `docker compose down` would be denied |
| `["python", "-m", "{{ module }}"]` | none (rule 3) | `["python"]` would deny `python -m pytest` |

**The catalog outranks a launch prefix.** When the command word is a catalog program, the catalog's verb rules decide whether the command starts a server, and the launch match only supplies the app name. Without that ordering, an app launching `["vite", "--port", "{{ port }}"]` would make `vite build` a denial through source 4 before source 5 got to say that `build` is not a server.

**Longest signature wins.** Two tasks `["bun", "test"]` and `["bun", "test", "--watch"]` both match `bun test --watch`; `cfg.tasks` is a `HashMap`, so without an order the deny message names a different task on each run. Rank matches by signature length, break a tie by hint resolution, then by name. Sort before comparing so the answer is the same every time.

Worked against `launch = ["doppler", "run", "-c", "dev_local", "--", "nitro", "dev", "--port", "{{ port }}"]`, whose signature is `["nitro", "dev"]`:

| Typed | Result |
|---|---|
| `nitro dev` | deny |
| `nitro dev --port 3000` | deny (starts with the signature) |
| `bun run nitro dev` | deny (runner stripped) |
| `nitro build` | no match; falls through to source 5, which passes it |

### When a task match denies

`devrun task <name>` is worth redirecting to when the process it launches differs from the argv the agent typed. That is true when the matched task:

- sets `app` (a different cwd and the app's `static_env`), or
- sets `env`, or
- references a port template in `run` or `env`, or
- carries a *different doppler wrapper* than the typed command. Typing `doppler run -c prd -- bun test` against a task whose `run` is `["doppler", "run", "-c", "dev", "--", "bun", "test"]` is not the same process, and the doppler config is exactly the difference that matters.

A task with none of those resolves to the identical process in the same directory, and blocking it buys nothing.

"Wrapper" in that fourth term means the doppler wrapper and nothing else, normalized to the pair `(config, project)` so `--config dev` and `-c dev` compare equal rather than denying on spelling. Absent on one side and present on the other is a difference. Runner prefixes are deliberately *not* wrappers here: `bunx vitest` against a task's `["bun", "run", "vitest"]` is close enough to the same process that denying it would be noise.

Use `devkit_common::template::referenced_ports` for the port test rather than scanning for `{{`. It renders against a recording context, never touches the registry, and already handles `ports["web"]` and `{% if port %}` guards.

Two fields are deliberately absent from that list. `steps` cannot appear: `TaskConfig` makes `run` and `steps` mutually exclusive, so a sequence task has no `run` for a typed command to match, and no test can be written for it. `require_live` cannot appear alone: `resolve_command` rejects a `require_live` entry not referenced through `ports[...]`, so it always implies a port template and the port test has already fired.

`guard = true` denies regardless; `guard = false` passes regardless.

### The built-in catalog

A const table of ecosystem facts, extended by pull request rather than by config:

| program | counts as a server |
|---|---|
| `next`, `nitro`, `wrangler`, `mintlify` | `<prog> dev` |
| `uvicorn` | bare |
| `flask` | `flask […] run` |
| `vite` | bare, `dev`, `serve`; never `build`, `preview`, `optimize` |

An info flag prints and exits, so it overrides the verb for every program in the table whether or not a verb is present: neither `uvicorn --version` nor `next dev --help` is a server. The long spellings `--version` and `--help` always read that way; the short `-v` and `-h` read that way everywhere except `flask`, whose `run` spends `-h` on the bind host, so `flask run -h 0.0.0.0` is a server. A flag the table does not name leaves the verdict to the verb, so `vite --port 3000` is a server and `vite build --minify` is not.

`bun run dev` is deliberately absent. It is caught by source 4, because it is literally what such an app's `launch` says. Deriving from `[apps]` is what makes the catalog small.

### Naming the app

Source 5 knows a command is a dev server without knowing which app it belongs to, and source 4 has the same problem whenever several apps share one `launch` (three apps that all launch `bun run dev`, typed from the repo root). Both use the same resolution.

The candidate set narrows before the hint is consulted: when the launch match produced any apps, only those are candidates, and a single one is named without needing a hint at all. Only a catalog hit with no launch match searches the whole catalog. Naming the wrong app sends the agent to the wrong server, which is worse than naming none.

The hint is, in order:

1. The first `(apps|packages)/<name>` path anywhere in the segment.
2. The value of a `--filter`, `-F`, `--dir`, `-C` or `--cwd` argument, if the segment has one.
3. The hook's `cwd`, relative to the checkout root. That is the checkout the command runs in, not the repository's main checkout: in the primary clone the two are the same, and in a linked worktree the cwd is under the worktree, never under main.

A hint matches an app when it equals the app's name, equals its path, or is a path *under* its path, so `apps/web/src` names `web`. `frizbee` then rescues near-misses, so `--filter lab-tools` still resolves against an app declared `lab_tools`. Below a score threshold no app is named and the message falls back to `devkit config apps`.

That last rung is the only place the guard guesses, so it is the only part of app naming a project can tune:

| `[harness.app_match]` | default | effect |
|---|---|---|
| `fuzzy` | `true` | `false` stops after exact and path matching, so an unrecognised hint names no app |
| `max_typos` | `1` | substitutions, insertions and deletions the matcher forgives |
| `min_score` | `60` | below this, name no app |

The one-typo default is devkit's, not the library's. `frizbee::Config::default()` allows zero, which filters exactly the `lab-tools` against `lab_tools` case the rung exists for. Raising `max_typos` buys confidently wrong app names, and a wrong name points the agent at another app's server; a project that would rather see `devkit config apps` than a guess sets `fuzzy = false`. None of the three is clamped: the numbers are the project's call.

Hint resolution answers "which app", never "which match". When several apps match with signatures of *different* lengths, the longest wins outright and the hint is not consulted. The hint breaks ties only among equal-length matches, and when it resolves none of them the message names each candidate (`devrun up web` or `devrun up admin`) rather than picking one.

### Failure is silence

Any failure — a config that will not load, a malformed rule, an unexpected panic — warns on stderr and exits 0, letting the command run. The guard body runs inside `catch_unwind`. `devkit`'s `main` installs `report::install_panic_hook` for every subcommand, and that hook prints its bug report and returns rather than exiting, so a panic on the guard path writes the report to stderr, then the guard's own allow message, and still exits 0. `catch_unwind` catches nothing under an aborting panic strategy, so `harness.rs` carries a `#[cfg(panic = "abort")] compile_error!` that fails the build if the release profile ever stops unwinding.

This is the opposite of `enforce_writes`, deliberately. A missed write lock corrupts another session's work, so that hook fails closed. A missed nudge costs nothing, while a false denial blocks legitimate work on every Bash call until someone finds the malformed key. The `prd` safety net is `run::assert_not_prd` at launch time, which this path does not touch.

`DEVKIT_ENFORCE_COMMANDS=0` disables the guard for a session. It stays out of the deny reasons: an env prefix inside the command string is invisible to the hook, so advertising the bypass to an agent only produces a retry loop, and the bypass is the human's call.

### Cost

Measured on this repo's binary, hyperfine, `-N`, 200+ runs, min-of-N:

| Case | Cost |
|---|---|
| gate off, or outside a devkit project | ~1.0 ms |
| gate on, full resolution (2 layers) | ~11.1 ms |
| gate on, full resolution (5 layers) | ~12.5 ms |

The gate-off path is what almost every project pays. It parses each layer's `[harness]` table but never resolves the config, never shells out to git beyond finding the checkout root, and never builds the app catalog. A user rule matching (source 2) stops at the same point.

The guarded cost is dominated by three `git` subprocesses, not by anything the guard adds: `git worktree list --porcelain` at 3.3 ms plus `git rev-parse --show-toplevel` twice at 1.4 ms each, against ~3.0 ms of actual layer parsing and merging.

The registration carries `"timeout": 10`, matching the `devkit brief` entries in the same manifests, so a pathological resolution cannot stall a tool call. The existing `lockm hook pretooluse` entry carries none; leave it alone.

## Rejected alternatives

**Caching the resolved config under `.devkit/`.** It buys back the ~3 ms of parsing, not the ~6 ms of git, since the checkout root has to be resolved to find the cache at all. The layers span the global config and the main checkout, so a per-worktree cache keys on files outside its own worktree, and discovering which files to stat is part of the cost being avoided. The hook fires on every Bash call across concurrent sessions, so writes would need the flock'd store, making a lock acquisition per Bash call. If this latency ever genuinely bites, the consistent answer is a resolved-signatures response from `devkitd`, which holds authoritative state in memory and has no staleness window.

**Invalidating such a cache on a counter.** A config edit would take effect at an unpredictable moment, with nothing to explain why the guard still cites the old app. Layer mtime and size is roughly five stat calls and is correct.

**rayon or jwalk in config resolution.** Both measured slower than the baseline in every location. rayon costs 1.0–2.3 ms building a pool against ~3.0 ms of total work, and it is paid even in a directory with no `devkit.toml`. Parallelizing `apply_cutoff` also breaks `layers::tests::root_marker_hides_a_malformed_layer_above_it`, because the root barrier's semantics depend on the scan being ordered. jwalk has no site: layer discovery walks ancestors upward with two `is_file()` probes per directory, and a `read_dir` of a whole checkout root to answer a two-file question is strictly more work. `devkit-config` is a leaf crate and cannot reach `devkit_common::pool` anyway, so either would use rayon's global pool, which `AGENTS.md` forbids.

**A raw `pattern` regex on user rules.** `programs` plus `args` covers the cases that have come up, and devkit owns the lexing the pattern would have to restate. A bad regex would surface as a hook-time failure on every Bash call. Addable later without breaking a rule already written.

**A project-configurable dev-server catalog.** The catalog is ecosystem knowledge, not project convention, and `[harness.commands.*]` already covers additions. A project needing to suppress a built-in signature is the case that would justify it, and none has come up.

**Regex-scanning the command string.** What the prior-art shell hook does, and the source of its false denies on quoted text and on `--`. A lexer is more code and removes a class of bug that would otherwise be reported as "devkit blocks my commits".

**An off-the-shelf word splitter (`shell-words`, `shlex`).** Cheaper than a lexer and cannot do the job: quotes are stripped before the caller sees them, operator positions are gone, and heredocs are not modelled. The heredoc gap alone would deny any `cat > file <<EOF` whose body mentions a dev server.

**Truncating a launch signature at the template alone.** Simpler, and wrong whenever the template is not the last token, which is most of the time. It turns `["vite", "--port", "{{ port }}"]` into the signature `["vite"]` and denies `vite build`.

**Routing source 2 through the full config resolver.** Rules would inherit through the resolver's own merge with nothing new written, but it puts `[harness.commands]` on `Config`, which reintroduces exactly the coupling the per-key probe exists to remove: a malformed rule would then fail `devrun up`. Making `merge_layers` public gets the same inheritance without the coupling.

**Consolidating all hook glue under one namespace.** `lockm hook <event>` is a shipped interface across three manifests. `devkit harness shell` is new; moving the existing one is a separate change if it is ever wanted.

## Testing

- Lexing: every row of the parsing table above. The quoted `git commit -m "…; …"`, `cargo run -- next dev`, and the heredoc body are the false-deny regression tests; `nohup bun run dev … &` is the miss regression test.
- Stripping across all four matching sources: `doppler run -c local --`, `env FOO=1 vite`, `timeout 30 vite`.
- Basename matching: `./node_modules/.bin/vite` denies.
- Signature reduction: every row of the signature table. `docker compose down` and `python -m pytest` pass, which is rules 2 and 3 doing their job. An app launching `bun run dev -- --port {{ port }}` still denies typed `bun run dev`, which is rule 3 *not* over-firing. Both sides normalize, so a task `run = ["bun", "run", "lint"]` matches typed `bun run lint`.
- The doppler term: a task `run = ["doppler", "run", "-c", "dev", "--", "bun", "test"]` denies typed `doppler run -c prd -- bun test` and allows the same command with `-c dev`. This is the test that fails if either side skips normalization.
- Catalog outranks launch: an app whose `launch` is `["vite", "--port", "{{ port }}"]` still lets `vite build` through.
- Determinism: tasks `["bun","test"]` and `["bun","test","--watch"]` against `bun test --watch` name the longer one, on every run of a loop.
- Catalog: `vite build`, `vite preview`, and `vite --version` pass; bare `vite`, `vite dev`, and `vite serve` deny.
- Task derivation: `app`, `env`, and a port template each deny on their own; `-c prd` against a task's `-c dev` denies; `--config dev` against `-c dev` passes; a bare `run` with a matching wrapper passes; `guard` overrides both ways.
- Rule merge: a child layer's `programs = []` exempts a subtree while inheriting the parent's `reason`; a global-config rule applies in a project that declares none; a rule with no `programs` after merging is skipped.
- App naming: exact beats fuzzy; a cwd *inside* an app (`apps/web/src`) names it; `lab-tools` resolves `lab_tools`; a below-threshold hint names no app and falls back to `devkit config apps`; several apps sharing a `launch` with no resolving hint names each; a catalog hit with exactly one matching launch names that app without any hint at all. Every case that exercises the fuzzy rung needs two or more candidates, or the single-candidate short circuit answers before the matcher runs and the test proves nothing.
- `[harness.app_match]`: `fuzzy = false` drops `lab-tools` while still resolving `apps/web`; an impossible `min_score` drops it too; `max_typos = 0` drops it, which pins why devkit does not inherit frizbee's default. The table merges key by key across layers, and one that will not parse falls back to the defaults with a warning while its sibling rules survive.
- Payload shapes: a Claude Code payload and a Cursor payload over the same command produce the same decision in their own envelopes, with the Cursor reason in `agent_message`.
- **Cross-feature**: a `[harness]` table carrying `enforce_writes = true` beside a malformed `[harness.commands.*]` rule still enforces writes, and the bad rule is skipped. A rule warning is *not* printed by `lockm hook pretooluse`. And a layer with a TOML syntax error turns both flags off, which is today's behaviour, pinned so nobody reports it as new.
- Gate off exits 0 and reads no `[apps]`, `[tasks]` or catalog. It does parse each layer's `[harness]`, since that is how the gate is answered.
- Fail-open: an unparseable config with the gate on exits 0 and writes to stderr.
- Read-only: a full guarded run against a project with tasks leaves `ports.json` byte-identical.
- Integration: real payload JSON on stdin asserting the deny envelope, driven the way `tests/locks.rs` drives the lock hook.

## Docs to update

- `docs/configuration.md` `[harness]`: `enforce_commands`, `[harness.commands.*]` and `[harness.app_match]` alongside `enforce_writes`. The line stating that `Bash` is outside the harness's scope becomes narrower, since the harness now intercepts `Bash` for *commands*; shell-level *writes* remain uncovered.
- `docs/configuration.md` "Activation requires `lockm` on `PATH`": `devkit` becomes a second requirement. It is already needed for `devkit brief`, so this is a doc line, not a behaviour change.
- `docs/commands.md`: no hook subcommand is documented there today, so `devkit harness shell` establishes the pattern.
- `schema/devkit-config.json` is regenerated (`DEVKIT_UPDATE_SCHEMA=1`).

## Open questions

1. Codex hooks were understood to be experimental, off by default, and unavailable on Windows; current documentation appears to contradict all three, and `hooks-codex.json` already uses `commandWindows`. Worth one verification pass. The answer does not gate the design: the Codex manifest already registers `lockm hook pretooluse`, so adding a `Bash` matcher changes nothing about the risk profile — if Codex hooks are off, both are off.
