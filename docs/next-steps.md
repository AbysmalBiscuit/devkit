# Next steps

Follow-ups intentionally left out of the `issue` consolidation branch.

## Add the issue skills to the repo as references

The `issue-*` skills that drive the `issue` binary (`issue-setup`, `issue-start`,
`issue-review`, `issue-end`, `migration-review`, and friends) currently live only
under `~/.claude/` on the dev machine, untracked. Vendor copies into the repo as
reference material so the CLI ships alongside the workflows it's meant to power and
the skills survive a fresh checkout. Open questions: where they live (e.g.
`docs/skills/` vs. a packaged `skills/` consumed by the plugin), and whether the
in-repo copies are the source of truth or a mirror of the `~/.claude/` originals.

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

- **Daemon-aware locks (shipped).** Lock actions route through explicit-context
  `devkit_locks::{acquire,check,release,release_all,status}_resolved` (and the
  already-daemon-aware `prune`), which try a live `devkitd` over `locks.sock` first and
  fall back to `FlockStore`. The MCP locks handler no longer hits `FlockStore` directly,
  so it cooperates with a running daemon instead of erroring `DaemonHoldsLock`.
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
- **Live MCP registration for Codex and Cursor (Cursor pending).**
  Registration configs ship for all three hosts (`.mcp.json`, `.cursor/mcp.json`,
  `.codex/config.toml`), each pointing at `devkit-mcp`. Claude Code is confirmed
  live — it connects and both tools are callable (`devkit_describe`/`devkit_call`).
  Codex registers the server via the installed plugin (`codex mcp list` shows
  `devkit` enabled, 2026-07-02), though an in-session `tools/call` has not been
  exercised. Cursor is **not yet verified in a running host**. When it is,
  verify end-to-end:
    1. `cargo install --path .` so `devkit-mcp` is on `PATH`.
    2. Open the host in this repo (Codex: trust the project); run `/mcp` (Codex) or
       check Settings → MCP (Cursor) and confirm `devkit` lists both tools.
    3. Invoke one, e.g. `devkit_describe` then `devkit_call` → `ports.status`.
  If a host fails to connect, suspect the fixed `protocolVersion` (`2024-11-05`, the
  MCP baseline) — the negotiation follow-up below is the fix.
- **`initialize` protocol-version negotiation (shipped).** The server echoes the
  client's requested `protocolVersion` back (falling back to the `2024-11-05` baseline
  when absent). devkit-mcp is version-agnostic (only `tools/list` + `tools/call`), so
  echoing maximizes host compatibility.
- **Ports holder is the project root (resolved).** Confirmed correct by design: the
  holder is the worktree root **path** because `holder_alive` = `path.exists()` is the
  liveness signal that auto-reclaims a worktree's ports on removal — distinct from locks'
  session-token holder. Documented in `AGENTS.md` (Registry facade).

## Verify multi-agent plugin packaging

The packaging is shipped. Each host resolves its own marketplace manifest —
`.claude-plugin/marketplace.json` (Claude Code), `.agents/plugins/marketplace.json`
(Codex, native schema with `policy`/`category`), `.cursor-plugin/marketplace.json`
(Cursor, unverified location) — so `<host> plugin marketplace add
AbysmalBiscuit/devkit` followed by the host's install verb
(`claude plugin install devkit@devkit` / `codex plugin add devkit@devkit`)
works from a public clone; Cursor installs from its Customize panel or a team
marketplace import instead (no git-repo CLI install). See the README's
"Installing for coding agents" for the user-facing steps. Codex is verified
end-to-end (2026-06-24, re-verified 2026-07-02 against the native manifest);
Claude and Cursor still need a live test.

- **Codex marketplace manifest location — VERIFIED 2026-07-02 (Codex v0.142.0).**
  Codex reads `.agents/plugins/marketplace.json` (preferred, its native schema)
  and falls back to `.claude-plugin/marketplace.json`; a manifest at
  `.codex-plugin/marketplace.json` is **not** a supported location ("marketplace
  root does not contain a supported manifest" when it's the only one). Plugin
  `source.path` resolves relative to the marketplace root, not the manifest
  directory. The plugin manifest, by contrast, *does* live at
  `.codex-plugin/plugin.json` (the curated `linear` plugin uses the same layout).
  Verified end-to-end with a renamed copy: `codex plugin marketplace add <dir>`
  picked the `.agents` manifest over the `.claude-plugin` one, and
  `codex plugin add devkit@<marketplace>` installed 0.8.0 cleanly.
- **Codex plugin manifest carries the MCP server — VERIFIED 2026-07-02.**
  `"mcpServers": "./.mcp.json"` in `.codex-plugin/plugin.json` (same field as
  Claude's manifest and the curated `linear` plugin) registers the server on
  install: `codex mcp list` shows `devkit → devkit-mcp` enabled, with no
  `mcp_servers` entry in `~/.codex/config.toml`. Isolated by installing a copy
  with `.claude-plugin/` deleted and the server renamed. The same field was
  added to `.cursor-plugin/plugin.json` on the assumption Cursor follows the
  shared schema — unverified until the Cursor live test.

- **Codex install — VERIFIED 2026-06-24.** From the now-public repo,
  `codex plugin marketplace add AbysmalBiscuit/devkit` then
  `codex plugin add devkit@devkit` install cleanly (Codex v0.142.0, ~4 MB — the
  git clone excludes `target/`/`.worktrees/`). Codex selects `.codex-plugin/plugin.json`
  and registers the `using-devkit` skill *natively* from `skills: "./skills/"`: it
  appears in Codex's `<skills_instructions>` block in every fresh session, so the
  agent is told the skill exists. That native registration — not the SessionStart
  hook — is the working delivery path on Codex.
  - **The `announce-skill` SessionStart hook does not fire on Codex, and is
    redundant there.** Codex discovers hooks from a `hooks.json` at the *plugin
    root* with *relative* command paths and tool-scoped events (`PostToolUse`,
    `Stop` — per the curated `figma`/`replayio` plugins). devkit's hook sits at
    `hooks/hooks-codex.json` behind a `hooks:` pointer Codex ignores, uses
    `${PLUGIN_ROOT}`, and keys on `SessionStart`, which no curated Codex plugin
    uses. The hook is silently inert, but native skill registration already covers
    the awareness goal. (The same root-discovery rule means the lockm
    `hooks/hooks.json` does not fire on Codex either — a separate question if
    Codex ever needs the file-lock hooks.)
  - **Post-compaction pins on Codex — WIRED, UNVERIFIED IN HOST.**
    `hooks/hooks-codex.json` carries a second `SessionStart` block matching
    `compact` that runs `brief-context --pins-only`; Codex re-fires `SessionStart`
    with `source: "compact"` after compacting, and `PostCompact` has no
    `additionalContext` field to inject through. Whether it runs at all depends on
    the discovery question above, last checked against Codex v0.142.0 while the
    installed CLI is now v0.147.0. To verify: start a Codex session in a devkit
    project with a registered library, force a compaction, and look for the
    library table. If the pointer is still ignored, the fix is a root-level
    `hooks.json` with relative paths, which would also revive `announce-skill`.
- **Claude marketplace install — NOT DONE (ready).** The repo is public; a fresh
  clone resolves `.claude-plugin/marketplace.json` (`source: "./"`) and
  `skills/using-devkit/SKILL.md`. Remaining is the live smoke test only a fresh
  Claude Code session can run: `claude plugin marketplace add AbysmalBiscuit/devkit`
  then `claude plugin install devkit@devkit` (or the same arguments via `/plugin`
  in a session), confirm `using-devkit` resolves.
- **Cursor install — NOT DONE.** Cursor is not installed on the dev machine, so
  the SessionStart context injection (`additional_context` envelope from
  `hooks/announce-skill`) has not been exercised in a running Cursor host. Install
  the plugin in Cursor, start a session, and confirm the "A 'using-devkit' skill is
  available" notice appears (or that Cursor registers the skill natively, as Codex
  does). On Windows, confirm `run-hook.cmd` locates Git Bash. If the envelope is
  rejected, adjust `hooks/announce-skill`. `sessionStart` is the only Cursor event
  that can inject context — `preCompact` is observational and `workspaceOpen`
  fires outside a session and returns only `pluginPaths` — so Cursor gets the
  brief once per session and has no analogue of `--pins-only` or `--if-changed`. Also confirm which marketplace manifest
  Cursor's "Import from Repo" reads — `.cursor-plugin/marketplace.json` ships on
  the assumption it mirrors the `.cursor-plugin/plugin.json` convention, but the
  location is unverified (Codex, for comparison, ignores `.codex-plugin/` for the
  marketplace manifest and wants `.agents/plugins/`).
- **Cursor hook command path resolution — NOT DONE (depends on the Cursor test).**
  `hooks/hooks-cursor.json` invokes the runner as the relative `./hooks/run-hook.cmd`
  (matching the obra/superpowers reference), whereas `hooks/hooks-codex.json` uses
  `${PLUGIN_ROOT}/hooks/run-hook.cmd`. The relative form only resolves if Cursor runs
  the hook with its working directory set to the plugin root; otherwise it silently
  no-ops (and looks like an envelope bug). Doc research (2026-06-24) suggests Cursor
  has **no** `${CURSOR_PLUGIN_ROOT}` expansion in manifest command position — a known
  structural gap — which would make the relative form the *only* working option and
  rule out the previously-proposed root-anchored switch. Confirm Cursor's hook cwd and
  variable-expansion behavior on the first live install before changing anything.

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
