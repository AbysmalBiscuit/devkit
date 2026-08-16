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
end-to-end (2026-06-24, re-verified 2026-07-02 against the native manifest, and
2026-08-16 against v0.147.0 for hooks and context injection); Claude and Cursor
still need a live test.

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
  - **Codex session hooks — VERIFIED 2026-08-16 (Codex v0.147.0).** The
    `hooks:` pointer in `.codex-plugin/plugin.json` is honored,
    `${PLUGIN_ROOT}` expands, and `hookSpecificOutput.additionalContext` is
    accepted, so `hooks/hooks-codex.json` delivers context exactly as written.
    What makes a hook look inert is **hook trust**: Codex refuses to run a new
    or modified hook until it is reviewed once in an interactive session
    ("Hooks need review"), and the trust state lives in `$CODEX_HOME/state_*.sqlite`,
    not `config.toml`. An untrusted hook is silently skipped, which reads
    identically to an unsupported event. `codex exec` has
    `--dangerously-bypass-hook-trust` for automation.
  - **Post-compaction pins on Codex — VERIFIED 2026-08-16.**
    `hooks/hooks-codex.json`'s second `SessionStart` block, matching `compact`,
    runs `brief-context --pins-only` and its table reaches the model. The firing
    is deferred by one turn: compaction *queues* the source
    (`core/src/session/mod.rs`, `queue_pending_session_start_source(Compact)`) and
    the turn path drains it (`core/src/session/turn.rs` →
    `hook_runtime::run_pending_session_start_hooks`), so the hook runs when the
    next message is sent, not at the moment of compaction. `PostCompact` is not an
    alternative: its output schema
    (`hooks/schema/generated/post-compact.command.output.schema.json`) is
    `additionalProperties: false` over `continue`/`stopReason`/`suppressOutput`/
    `systemMessage`, with no `hookSpecificOutput` — there is no way to inject
    context through it.
  - **Session hooks inject the brief, not a skill notice.** Codex and Cursor
    both register `using-devkit` natively from the manifest's `skills:`
    pointer, so a hardcoded "this skill exists" notice only duplicates it. Both
    startup blocks run `brief-context`; the notice emitter is gone.
  - **Write enforcement on Codex — WIRED 2026-08-16, UNVERIFIED IN HOST.**
    `hooks/hooks-codex.json` now carries the same `PreToolUse`/`SubagentStop`/
    `SessionEnd` handlers as the Claude Code file. Codex's edit tool is
    `apply_patch`, whose payload is a whole patch envelope in
    `tool_input.command` rather than a single `tool_input.file_path`, so
    `devkit_locks::hook` parses the envelope and claims every file it names
    (`Add File`/`Update File`/`Delete File`/`Move to`, both ends of a rename).
    `Write` and `Edit` stay in the matcher because Codex accepts them as
    aliases for `apply_patch` (`core/src/tools/hook_names.rs`). To verify: two
    Codex sessions in one checkout, one holding a lock, the other attempting an
    `apply_patch` on the held file — expect the deny. Note the multi-path
    decision is per file, so a patch that is half-blocked leaves the free
    halves claimed by the denied session; they release on `SessionEnd`.
- **Claude marketplace install — VERIFIED 2026-08-16.** A fresh clone resolves
  `.claude-plugin/marketplace.json` (`source: "./"`) and
  `skills/using-devkit/SKILL.md`; `claude plugin marketplace add
  AbysmalBiscuit/devkit` then `claude plugin install devkit@devkit` installs and
  `using-devkit` resolves. Confirmed on a second machine.
- **Cursor manifests — MATCH THE DOCS 2026-08-16.** The reference confirms what
  ships: the marketplace manifest belongs at `.cursor-plugin/marketplace.json`
  in the repo root, requiring `name`, `owner{name}`, and `plugins[]`; the plugin
  manifest at `.cursor-plugin/plugin.json` takes `hooks` as either a file path
  or inline config, so pointing it at `hooks/hooks-cursor.json` instead of the
  conventional `hooks/hooks.json` is supported; and `skills` registers the skill
  natively, exactly as Codex does. Still unverified in a running host, because
  Cursor is not installed on the dev machine.
- **Cursor context injection is broken upstream — BLOCKED, NOT ON US.**
  `sessionStart`'s `additional_context` is silently dropped: a Cursor developer
  confirmed on 2026-04-20 that the hook runs before the composer handle exists,
  so the field is discarded even when the log reports it merged. No fix version
  and no ETA as of the last report on 2026-08-03; the only offered workaround is
  duplicating static text into `.cursor/rules`, which a dynamic brief cannot use.
  The same pipeline drops `postToolUse` context. Nothing devkit ships can work
  around this, and `sessionStart` is Cursor's only context-injecting session
  event — `preCompact` is observational and `workspaceOpen` fires outside a
  session and returns only `pluginPaths`. Native skill registration is therefore
  the whole delivery path on Cursor today, as it already is on Codex. Re-test
  when Cursor announces a fix.
- **Cursor hook command path resolution — STILL UNKNOWN.** `hooks/hooks-cursor.json`
  invokes the runner as the relative `./hooks/run-hook.cmd`, whereas
  `hooks/hooks-codex.json` uses `${PLUGIN_ROOT}/hooks/run-hook.cmd`. The docs give a
  working directory for user, project, and enterprise hooks but say nothing about
  plugin hooks, and document no plugin-root expansion — only `${VAR}` plugin
  *variables*. So the relative form remains the only option available, and whether it
  resolves depends on a cwd only a live install can reveal.
- **Write enforcement is not portable to Cursor.** Its `preToolUse` returns
  `permission`/`user_message`/`agent_message`, not
  `hookSpecificOutput.permissionDecision`, and its payload carries
  `conversation_id` rather than `session_id` — so both `devkit_locks::hook`'s
  parser and its deny envelope miss. Supporting Cursor means a third adapter,
  not a config port. Cursor also exposes `afterFileEdit` (post hoc) and
  `beforeReadFile`, neither of which gates a write the way `PreToolUse` does.

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
