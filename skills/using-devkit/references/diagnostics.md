# `devkit` — setup and diagnostics

```sh
devkit auth <linear|slack> [--token <value>]   # validate + store a credential
devkit auth github                             # report the GitHub identity devkit would use
devkit doctor [--json]                         # check configured credentials + diagnostics
devkit brief [--pins-only|--if-changed]        # compact project brief
devkit schema                                  # JSON Schema for devkit.toml, to stdout
devkit schema init [<path>]                    # point a devkit.toml at the published schema
```

`auth <linear|slack>` prompts without echo (or reads `--token` or piped stdin), validates the token against the live API, and stores it in `~/.config/devkit/secrets.toml` (`0600`). For Linear it also stores the workspace slug, so issue links work without `LINEAR_WORKSPACE`. Tokens always resolve env-first.

## `config`

`devkit config` prints the merged config as TOML, headed by its layer files in precedence order. `--origin` annotates each value with the file it came from (`# (default)` for a serde default) and, where several layers set it, what each overridden layer held. `--json` emits the bare config; `--origin --json` emits `{ config, layers, origins, overrides }`. `config apps` and `config tasks` list the configured apps and tasks (the latter is the same listing as a bare `devrun task`), with no live state.

## `doctor`

One row per credential (source `env`/`file`/`unset`, and live validity), plus a `config` row. It exits non-zero when a set credential fails validation or a `devkit.toml` exists that does not load; no `devkit.toml` at all is fine. It also warns about installed binaries older than the newest plugin checkout, servers running outside devrun, baselines nothing references, unreferenced docs checkouts, and missing or foreign old-name shims (a shim problem never changes the exit code). The `tracker` row names the tracker and whether `[tracker] kind` or detection chose it. The `harness_log` row shows the logging mode in force.

## `auth github` reports, it stores nothing

devkit keeps no GitHub credential of its own, because `gh auth login`, `GH_TOKEN` and `GITHUB_TOKEN` already cover it. Resolution order: `GH_TOKEN`, `GITHUB_TOKEN`, then `gh auth token`.

The command prints the identity behind the token devkit would send, names which of the three supplied it, then lists `gh`'s own accounts below. Those two can differ, and the token's identity is the one devkit uses. A `--token` passed here is refused rather than silently discarded.

## `brief`

Prints the current checkout's devkit orientation — configured apps, the `[tasks]` table, this worktree's live servers, and registered library versions — and prints **nothing** outside a devkit-managed project. A config that fails to load is reported rather than swallowed, so a broken `devkit.toml` is diagnosable from the brief.

The plugin's `SessionStart` hook runs it so sessions start already knowing the project. Run it by hand to re-orient mid-session.

- `--pins-only` emits only the library-versions section — what a post-compaction re-injection wants, without respending the context compaction just reclaimed.
- `--if-changed` prints nothing when this session already received the same brief (it reads `session_id` from the hook's stdin JSON). Rejected together with `--pins-only`: the watermark records the *whole* brief, so suppressing on it after emitting only the library table would tell the session it had seen a brief it never got.
- `--additional-context` wraps the output in the JSON envelope Codex and Cursor read a hook's context from. Claude Code takes plain stdout.

The plugin runs a full brief at `SessionStart`, `--pins-only` at `PostCompact`, and `--if-changed` at `CwdChanged`.

The library-versions section answers for the directory it runs in. At a workspace root it rolls up the members the lockfile names, one row per version they resolve; where members disagree, both versions appear with the workspaces holding them, so an agent reads the right checkout for the app it is editing. A library the reference registry records a checkout for under this project shows even without lockfile evidence, sourced `resolved checkout`, and a checkout whose version is not the one the lockfile names is flagged `; checkout <version>`.

Which sections appear is config-driven: `[brief]` has `enabled`, `pins`, `locks`, `apps`, and `tasks` switches, all defaulting on. A section with nothing to report is omitted whatever its switch says; a switch turned off suppresses the section even when the checkout has something to put in it. Live servers this worktree holds are reported regardless of the `apps` switch — a bound port is a fact about the machine.

## `schema`

`devkit schema` prints the JSON Schema derived from the config types, so it is
the authority on every key's name, type and default, and it cannot drift from
what the binary accepts. Dump it when a key is in question. Each key's
description names its env override where it has one. `references/config.md`
carries how keys constrain one another.

`devkit schema init` prepends the taplo header directive (`#:schema <url>` on the first line, *not* a `# $schema = "..."` key) to the config at `<path>`, defaulting to `devkit.toml`. It writes a fully-commented starter when the file does not exist, and leaves a file that already names a schema alone.

## Timing

`issue` and `devrun` take a global `--timing` that prints a per-operation breakdown of subprocess and network IO to stderr on exit: count, total, max and p50 per op (`git fetch`, `github REST`, ...), plus wall time, IO-busy time and the concurrency the fan-outs reached. `--timing=trace` adds every op with its start offset, thread and command line; `--timing-log <FILE>` streams one JSON record per op for comparing runs. `DEVKIT_TIMING=summary|trace` does the same without the flag. stdout is never affected.
