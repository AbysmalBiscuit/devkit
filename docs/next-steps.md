# Next steps

Follow-ups intentionally left out of the `issue` consolidation branch.

## Update external callers to the `issue` binary

`issue-prep`, `issue-end`, and `pr-status` are gone. The concrete old→new command
mapping and the per-file list of callers to update lives in an uncommitted note in
the base repo: `../devkit/docs/issue-binary-migration.md` (i.e. the non-worktree
checkout). Callers include `~/.claude/commands/{issue-setup,issue-end,migration-review}.md`,
`~/.claude/scripts/issue-end-*.sh`, and `~/.local/bin/{pr-status,issue-end}.py`.

## Authoritative in-memory mode for the lock registry

`devkitd` serves both the port and lock registries from memory over `ports.sock`
and `locks.sock`, write-through to the files, gated by `devkitd.lock`. Delivered by
`docs/superpowers/plans/2026-06-21-authoritative-in-memory-locks.md` and the spec at
`docs/superpowers/specs/2026-06-21-authoritative-in-memory-locks-design.md`.

## MCP server for devkit

v1 is implemented (`crates/devkit-mcp` + `src/bin/devkit-mcp`): a meta-MCP stdio
server (`devkit_describe` + `devkit_call`) exposing the 9 port + lock actions over
the library facades. Design: `docs/superpowers/specs/2026-06-21-devkit-mcp-server-design.md`.
Plan: `docs/superpowers/plans/2026-06-21-devkit-mcp-server.md`.

Deferred follow-ups:

- **Daemon-aware locks.** v1 lock writes go straight through `FlockStore` and will
  hard-error (`DaemonHoldsLock`) under a live `devkitd`. Full cooperation needs
  daemon-aware resolved-context lock facade variants — owned by the "Authoritative
  in-memory mode for the lock registry" section above. Until then, run lock actions
  without a daemon, or wire that work first.
- **`devrun` actions (phase 2 — shipped).** `devrun.status`, `devrun.up`
  (non-blocking kick-and-poll), `devrun.down`, and `devrun.logs` are registered
  MCP actions over the new `devkit-ports::run` facade. `devrun up`'s blocking
  readiness wait stays CLI-only; the MCP `up` returns `starting` and the agent
  polls `devrun.status`.
- **`issue` read actions (phase 3 — shipped, read-only).** `issue.status` and
  `issue.prs` are registered MCP actions over the new `devkit-issue` facade
  (`status::gather`, `prs::gather`). The `issue` binary was refactored to consume
  the facade. Still deferred: the mutating `issue.review` (push/PR/Slack) and
  `issue.end` (worktree removal) actions, which need confirm-gating; and
  `issue setup`/`issue dashboard`, which are not request/response fits.
- **Live MCP registration for Codex and Cursor (manual verification pending).**
  Registration configs ship for all three hosts (`.mcp.json`, `.cursor/mcp.json`,
  `.codex/config.toml`), each pointing at `devkit-mcp`. Claude Code is confirmed
  live — it connects and both tools are callable (`devkit_describe`/`devkit_call`).
  Codex and Cursor are **not yet verified in a running host** (neither was available
  when the configs were added). When one is, verify end-to-end:
    1. `cargo install --path .` so `devkit-mcp` is on `PATH`.
    2. Open the host in this repo (Codex: trust the project); run `/mcp` (Codex) or
       check Settings → MCP (Cursor) and confirm `devkit` lists both tools.
    3. Invoke one, e.g. `devkit_describe` then `devkit_call` → `ports.status`.
  If a host fails to connect, suspect the fixed `protocolVersion` (`2024-11-05`, the
  MCP baseline) — the negotiation follow-up below is the fix.
- **`initialize` protocol-version negotiation.** The server returns a fixed
  `protocolVersion`; confirm it against the versions the target hosts send and
  negotiate if a host requires it.
- **Ports holder is the project root.** Ports actions use `root` as the registry
  holder (the liveness path), while locks use the minted session-token holder.
  Confirm this matches how an agent expects to address its allocations across
  multiple worktrees.

## Verify multi-agent plugin packaging

The packaging is shipped but not yet exercised end-to-end. Two things still need
a live test:

- **Claude marketplace install.** `github.com/AbysmalBiscuit/devkit` is private for
  now and will be made public later. Once it is public, run
  `/plugin marketplace add AbysmalBiscuit/devkit` then `/plugin install devkit` in a
  fresh session and confirm the `using-devkit` skill resolves from the plugin. Until
  then the relative-path marketplace source cannot be added via git.
- **Codex/Cursor SessionStart hook.** The context-injection envelopes
  (`hookSpecificOutput.additionalContext` for Codex, `additional_context` for Cursor)
  are verified against current docs but neither agent has been run yet. Install the
  plugin in Codex and Cursor, start a session, and confirm the "A 'using-devkit'
  skill is available" notice appears. On Windows, confirm `run-hook.cmd` locates Git
  Bash. If an envelope is rejected, adjust `hooks/announce-skill`.
- **Cursor hook command path resolution.** `hooks/hooks-cursor.json` invokes the
  runner as the relative `./hooks/run-hook.cmd` (matching the obra/superpowers
  reference), whereas `hooks/hooks-codex.json` uses the root-anchored
  `${PLUGIN_ROOT}/hooks/run-hook.cmd`. The relative form only resolves if Cursor runs
  the hook with its working directory set to the plugin root; if it runs from the
  session's repo instead, the hook silently no-ops (and looks like an envelope bug).
  Confirm Cursor's cwd on the first live install. If it is not the plugin root, switch
  the command to a root-anchored variable (e.g. `${CURSOR_PLUGIN_ROOT}/...`) once its
  expansion in command position is confirmed.

## Setup help/oauth for linear and slack

**Status:** RESOLVED 2026-06-24 — the `devkit` binary provides `devkit auth
<linear|slack>` (validate a token against the live API and store it in
`~/.config/devkit/secrets.toml`, `0600`) and `devkit doctor` (report each
credential's source and validity). Tokens resolve env-first, then from the
secrets file, via `devkit-common::secrets`. OAuth browser flows and an OS-keyring
backend are deferred follow-ups. See
`docs/superpowers/specs/2026-06-24-devkit-credential-setup-design.md` and
`docs/superpowers/plans/2026-06-24-devkit-credential-setup.md`.

## Ability to dump/show devrun/devkit config

**Status:** RESOLVED 2026-06-23 — `devrun config show [--origin] [--json]` prints the
effective merged config (TOML by default; `--origin` annotates each value with its
source file or `# (default)`; `--json` emits JSON), and `devrun config apps [--json]`
lists the configured app catalog. See
`docs/superpowers/specs/2026-06-23-layered-config-and-config-command-design.md` (§2) and
`docs/superpowers/plans/2026-06-23-layered-config-and-config-command.md`.

## Ability to resolve devkit.toml config files hierarchically, the same way claude code resolves CLAUDE.md files

**Status:** RESOLVED 2026-06-23 — `config::resolve` layers every `devkit.toml` from the
filesystem root down to the cwd over the `~/.config/devkit/config.toml` base layer and
deep-merges them (tables merge key by key; scalars and arrays replace wholesale), so the
deepest file wins per value. `[config] root = true` stops the upward walk and drops all
shallower layers including home; `--config`/`$DEVKIT_CONFIG` selects a single file
verbatim, bypassing layering. Routed through `load::load`, so every binary and the MCP
server inherit it. See the spec/plan referenced above (§1).

Original intent (kept for context):

> Given: `~/path/to/project/{repo1,repo2,repo3,...}/.git`
> A `devkit.toml` file here: `~/path/to/project/devkit.toml` will get resolved and applied to all devkit calls inside any repos/worktrees.
> The same applies to `~/path/to/devkit.toml`
> With the deepest hierarchy file taking priority.

Deferred follow-up:

- **Remove the orphaned `config::locate`.** RESOLVED 2026-06-24 — deleted the dead
  function and repointed the `devkit-locks::hook::global_config_path` doc comment at
  the resolver's `~/.config/devkit/config.toml` base-layer fallback.

## Configurable templates for messages

**Status:** RESOLVED 2026-06-24 — Slack review text and PR title/body are
minijinja templates under `[templates]` (`slack`, `pr_title`, `pr_body`), with
defaults reproducing prior behavior. See
`docs/superpowers/specs/2026-06-24-config-templating-design.md` and
`docs/superpowers/plans/2026-06-24-config-templating.md`.

## Configurable templates for issue start

**Status:** RESOLVED 2026-06-24 — `issue setup` renders the branch name and
worktree directory from `[templates]` (`branch`, `worktree_dir`), and persists a
`.devkit/issue.toml` record so review-time templates can reference `issue`/`slug`/`apps`.
See the spec/plan referenced above.
