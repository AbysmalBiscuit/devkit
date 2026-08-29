# `devkit` — setup and diagnostics

```sh
devkit auth <linear|slack> [--token <value>]   # validate + store a credential
devkit auth github                             # report the GitHub identity devkit would use
devkit doctor [--json]                         # check configured credentials + diagnostics
devkit brief [--pins-only|--if-changed]        # compact project brief
devkit schema                                  # JSON Schema for devkit.toml, to stdout
devkit schema init [<path>]                    # point a devkit.toml at the published schema
```

## `auth github` reports, it stores nothing

devkit keeps no GitHub credential of its own, because `gh auth login`, `GH_TOKEN` and `GITHUB_TOKEN` already cover it. Resolution order: `GH_TOKEN`, `GITHUB_TOKEN`, then `gh auth token`.

The command prints the identity behind the token devkit would send, names which of the three supplied it, then lists `gh`'s own accounts below. Those two can differ, and the token's identity is the one devkit uses. A `--token` passed here is refused rather than silently discarded.

## `brief`

Prints the current checkout's devkit orientation — configured apps, the `[tasks]` table, this worktree's live servers, and registered library versions — and prints **nothing** outside a devkit-managed project. A config that fails to load is reported rather than swallowed, so a broken `devkit.toml` is diagnosable from the brief.

The plugin's `SessionStart` hook runs it so sessions start already knowing the project. Run it by hand to re-orient mid-session.

- `--pins-only` emits only the library-versions section — what a post-compaction re-injection wants, without respending the context compaction just reclaimed.
- `--if-changed` prints nothing when this session already received the same brief (it reads `session_id` from the hook's stdin JSON). Rejected together with `--pins-only`: the watermark records the *whole* brief, so suppressing on it after emitting only the library table would tell the session it had seen a brief it never got.

The library-versions section answers for the directory it runs in. At a workspace root it rolls up the members the lockfile names, one row per version they resolve; where members disagree, both versions appear with the workspaces holding them, so an agent reads the right checkout for the app it is editing. A library the reference registry records a checkout for under this project shows even without lockfile evidence, sourced `resolved checkout`, and a checkout whose version is not the one the lockfile names is flagged `; checkout <version>`.

Which sections appear is config-driven: `[brief]` has `enabled`, `pins`, `locks`, `apps`, and `tasks` switches, all defaulting on. A section with nothing to report is omitted whatever its switch says; a switch turned off suppresses the section even when the checkout has something to put in it. Live servers this worktree holds are reported regardless of the `apps` switch — a bound port is a fact about the machine.

## `schema`

`devkit schema` prints the JSON Schema derived from the config types.

`devkit schema init` prepends the taplo header directive (`#:schema <url>` on the first line, *not* a `# $schema = "…"` key) to the config at `<path>`, defaulting to `devkit.toml`. It writes a fully-commented starter when the file does not exist, and leaves a file that already names a schema alone. See `docs/configuration.md`.
