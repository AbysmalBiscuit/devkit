# Multi-agent skill packaging for devkit (+ MCP deferral)

## Goal

Make the `using-devkit` skill installable across the coding agents in use —
Claude Code, Codex, Cursor, Zed — instead of being hand-copied into
`~/.claude/skills/`. Follow the multi-agent packaging model that
[obra/superpowers](https://github.com/obra/superpowers) uses: keep one canonical
`skills/` directory and add a thin per-agent manifest beside it, each pointing at
the same `./skills/` source.

Separately, capture the MCP-server design analysis from this brainstorm into
`docs/next-steps.md` so the deferred MCP work starts from the analysis rather than
a stub. The MCP itself is **out of scope** for this round.

## Background — current state

The CLI-rename session already landed the groundwork this design builds on:

- Binaries renamed to the mirrored scheme: `portm`, `lockm`, `devkitd`
  (`devrun`, `issue` keep their names).
- `AGENTS.md` is the canonical contributor-instructions doc; `CLAUDE.md` is a
  one-line pointer (`@AGENTS.md`).
- `skills/using-devkit/SKILL.md` already uses the new binary names.

Two distinct docs serve two distinct audiences — do not conflate them:

- **`AGENTS.md` / `CLAUDE.md`** — instructions for agents working *on* the devkit
  codebase (build, invariants, conventions).
- **`skills/using-devkit/SKILL.md`** — guidance for agents *using* devkit's CLIs
  in any checkout (the file-lock collaboration workflow, ports, dev servers,
  issue lifecycle). **This is the artifact being packaged.**

Today the installed copy at `~/.claude/skills/using-devkit/` is a plain copy (not
a symlink), so it drifts from the repo source. The plugin mechanism replaces it.

## Non-goals

- Building the MCP server (deferred — see "MCP deferral" below).
- Superpowers' skill-search / meta-skill runtime. devkit has one
  description-triggered skill, not a skill library that needs an on-demand loader.
  Codex/Cursor still need a *minimal* session-start hook (see "Codex/Cursor
  hook") — but only to announce the one skill, not to bootstrap a runtime.
- Cross-agent skill auto-install where no convention exists (e.g. Zed) — those
  agents are served by their native context file, `AGENTS.md`.

## Architecture — one source, thin per-agent manifests

The single source of truth is the existing `skills/` directory. Each agent
ecosystem gets a small manifest that references `./skills/`. No skill content is
duplicated; only metadata is per-agent.

```
devkit/
  skills/using-devkit/SKILL.md      # canonical skill (already exists)
  AGENTS.md                          # canonical context file (already exists)
  CLAUDE.md                          # pointer → AGENTS.md (already exists)
  LICENSE                            # NEW — GPL-3.0
  .claude-plugin/
    plugin.json                      # NEW — Claude Code plugin manifest
    marketplace.json                 # NEW — self-listing marketplace
  .codex-plugin/
    plugin.json                      # NEW — Codex manifest, "skills": "./skills/", "hooks": "./hooks/hooks-codex.json"
  .cursor-plugin/
    plugin.json                      # NEW — Cursor manifest, "skills": "./skills/", "hooks": "./hooks/hooks-cursor.json"
  hooks/
    hooks-codex.json                 # NEW — SessionStart wiring (Codex format)
    hooks-cursor.json                # NEW — sessionStart wiring (Cursor format)
    announce-skill                   # NEW — emits the skill-availability notice
    run-hook.cmd                     # NEW — cross-platform runner shim (Windows-safe)
```

### Agent coverage

| Agent | Mechanism | Notes |
|---|---|---|
| Claude Code | `.claude-plugin/plugin.json` + `.claude-plugin/marketplace.json` | Native skill discovery from `skills/`. Install: `/plugin marketplace add AbysmalBiscuit/devkit` → `/plugin install devkit`. |
| Codex | `.codex-plugin/plugin.json` (`"skills"`, `"hooks"`, `interface` block) + `hooks/hooks-codex.json` | Manifest is metadata; the `SessionStart` hook is what actually surfaces the skill. |
| Cursor | `.cursor-plugin/plugin.json` (`"skills"`, `"hooks"`) + `hooks/hooks-cursor.json` | Same — the `sessionStart` hook surfaces the skill. |
| Zed | `AGENTS.md` (already present) | Zed has no skill-plugin manifest; it reads `AGENTS.md`. This is the documented ceiling, not a gap. |
| Any other / generic | `AGENTS.md` + "point at the cloned repo" | The CLIs install via `cargo install --path .` regardless of agent; the skill is guidance on top. |

### Concrete manifest shapes (from superpowers, adapted)

`.claude-plugin/plugin.json`:

```json
{
  "name": "devkit",
  "description": "Local-dev coordination skills for devkit: file locks, ports, dev servers, issue lifecycle",
  "version": "0.1.0",
  "author": { "name": "Lev Velykoivanenko" },
  "homepage": "https://github.com/AbysmalBiscuit/devkit",
  "repository": "https://github.com/AbysmalBiscuit/devkit",
  "license": "GPL-3.0-or-later",
  "keywords": ["devkit", "file-locks", "ports", "dev-servers", "worktrees"]
}
```

`.claude-plugin/marketplace.json` (self-listing, `"source": "./"`):

```json
{
  "name": "devkit",
  "description": "devkit skills",
  "owner": { "name": "Lev Velykoivanenko" },
  "plugins": [
    { "name": "devkit", "description": "...", "version": "0.1.0", "source": "./" }
  ]
}
```

The `version` fields start at `0.1.0` to match the crate; release-please owns them
thereafter (see "Version sync").

`.codex-plugin/plugin.json` and `.cursor-plugin/plugin.json` carry the same
metadata plus `"skills": "./skills/"`; Codex additionally takes an `interface`
display block (displayName, category, capabilities). Exact field sets are
verified against each agent's plugin spec during the plan phase.

## Codex/Cursor hook

Codex and Cursor have **no native skill auto-discovery** — confirmed from the
superpowers clone. There, `"skills": "./skills/"` is metadata only; a `SessionStart`
hook (`hooks/session-start-codex`) reads `SKILL.md` and injects it into the session
as `additionalContext`. That injection is what makes the skill visible. devkit
needs the same wiring, scaled down:

- **What superpowers injects:** the full body of its *meta* skill (`using-superpowers`),
  which teaches the agent to load other skills on demand.
- **What devkit injects:** only a short **availability notice** — the
  `using-devkit` skill exists, here is its trigger description, read
  `skills/using-devkit/SKILL.md` when coordinating shared-checkout edits or
  running dev servers. The skill is conditionally relevant, so injecting its full
  body every session would pollute context for sessions that never touch shared
  files. The agent pulls the full content when the trigger fires.

Files (mirroring superpowers' shapes):

- `hooks/hooks-codex.json` — `SessionStart` with `matcher: "startup|resume|clear"`,
  running the announce script via the runner shim.
- `hooks/hooks-cursor.json` — `{"version": 1, "hooks": {"sessionStart": [...]}}`
  (Cursor's lowercased event + envelope).
- `hooks/announce-skill` — emits the JSON `additionalContext` notice (the
  skill's frontmatter `description` + the path to read).
- `hooks/run-hook.cmd` — cross-platform runner shim so the hook works on Windows
  (the development platform) as well as Unix.

The plan still verifies the exact hook envelope against each agent's *current*
plugin spec and tests a real install, but the mechanism is no longer open.

## Version sync

The four manifests each carry a `version`; release-please owns them. The repo
already uses release-please (`release-please-config.json`, single `rust` package
at `.`, currently `0.1.0`). Add an `extra-files` entry per manifest to the `.`
package so a release bumps them alongside `Cargo.toml`:

```jsonc
// release-please-config.json → packages["."]
"extra-files": [
  { "type": "json", "path": ".claude-plugin/plugin.json",   "jsonpath": "$.version" },
  { "type": "json", "path": ".claude-plugin/marketplace.json", "jsonpath": "$.plugins[0].version" },
  { "type": "json", "path": ".codex-plugin/plugin.json",    "jsonpath": "$.version" },
  { "type": "json", "path": ".cursor-plugin/plugin.json",   "jsonpath": "$.version" }
]
```

No `bump-version.sh` — release-please is the single source of version truth. The
plan verifies the `json`/`jsonpath` updater syntax against the release-please
schema.

## License

The repo has no `LICENSE` yet. Add a GPL-3.0 `LICENSE` file at the root, set
`license = "GPL-3.0-or-later"` in the root `Cargo.toml`, and use the same SPDX id
in every manifest's `license` field. (Note: superpowers is MIT; GPL-3.0 is
copyleft, so downstreams distributing modified devkit must release their changes
under GPL-3.0 — acceptable for a personal tool, worth knowing if it is ever
embedded elsewhere.)

## MCP deferral → `docs/next-steps.md`

Replace the brief MCP stub in `docs/next-steps.md` with the analysis from this
brainstorm so the future pass starts ahead:

- **Surface area (~19 actions):** `portm` (status, alloc, release, prune),
  `lockm` (acquire, check, release, status, prune), `devrun` (up, down, status,
  logs), `issue` (setup, status, end, prs, dashboard, review). `completions` is
  shell-only and not exposed.
- **Host-agnostic, assume eager loading.** Targets include Codex, Zed, Cursor,
  generic MCP clients — many load every tool schema up front, so keep the
  top-level tool count low. (Claude Code lazy-loads via deferred-tool search, so
  the count matters less there, but design for the worst case.)
- **Three exposure shapes, meta-or-dispatch favored:**
  1. *Flat* — one MCP tool per action (~19 tools); best discoverability and arg
     validation, heaviest footprint.
  2. *Per-binary dispatch* — four tools (`portm`/`devrun`/`issue`/`lockm`), each
     taking an `action` + args; ~5× fewer tools, natural groupings, looser
     per-action schemas.
  3. *True meta-MCP / progressive disclosure* — a `list`/`describe` tool + a
     `call` tool; minimal footprint, scales as devkit grows, at the cost of a
     discovery round-trip per novel action.
  Lean toward dispatch or meta given the agnostic, eager-loading hosts and prior
  positive experience with a meta-MCP shape. Settle the exact choice in the MCP's
  own brainstorm.
- **Implementation stance:** tools mirror the existing CLIs over the library
  crates (`devkit-ports`, `devkit-locks`, `devkit-common`) rather than reinvent
  them. The MCP server ships in this same plugin when built.
- **Daemon relationship:** an MCP server is a natural long-lived host that could
  keep the in-memory registries (`devkitd`) warm.
- **Open boundaries:** which surfaces to expose first; the read-only vs. mutating
  tool split.

## Cleanup

After confirming the Claude Code plugin loads the skill (the `using-devkit` skill
resolves via the installed plugin), delete `~/.claude/skills/using-devkit/` so
there is a single source of truth.

## Verification

- `cargo test --workspace` is unaffected — this is a docs/packaging change with no
  Rust source touched. Run it once to confirm green.
- Validate the Claude plugin manifests with the `plugin-dev:plugin-validator`
  agent.
- Confirm the skill loads from the installed plugin in a fresh Claude Code session
  before deleting the `~/.claude` copy.
- Codex/Cursor: confirm the `SessionStart`/`sessionStart` hook fires and the
  availability notice appears in session context after a real install (test on
  Windows, since that is the dev platform and the runner shim must work there).
- A grep gate confirms no stale binary names (`portman`, `devkit-portd`) appear in
  any new manifest or in `SKILL.md`/`AGENTS.md`/`README.md`.

## Sequencing

The CLI rename is merged, so there is no gate left. Work proceeds in this
worktree (`mcp-skill-packaging`) and lands as a PR to `main`. No file locking is
needed — the worktree is the isolation.

## Light SKILL.md freshness check

Binary names in `SKILL.md` are already current. A quick accuracy pass only; no
rewrite expected.

## Resolved decisions

1. **License** — GPL-3.0 (`GPL-3.0-or-later`); add a `LICENSE` file + set the
   field in `Cargo.toml` and every manifest.
2. **Marketplace home** — self-listed in this repo, hosted at
   `github.com/AbysmalBiscuit/devkit`; install via
   `/plugin marketplace add AbysmalBiscuit/devkit`.
3. **Version source** — release-please, via `extra-files` updaters (no
   `bump-version.sh`).
4. **Codex/Cursor hook** — required (manifests alone do not surface skills there);
   use the lightweight availability-notice hook described above.

## Open questions

None blocking. The plan phase verifies live details against current specs: the
exact hook envelope per agent, the release-please `json`/`jsonpath` updater
syntax, and the Codex `interface` field set.
