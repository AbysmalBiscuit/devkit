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
- Per-agent bootstrap runtimes / skill-search hooks of the kind superpowers ships
  (see "Open research item"). devkit has one description-triggered skill, not a
  skill library that needs session-start injection.
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
  .claude-plugin/
    plugin.json                      # NEW — Claude Code plugin manifest
    marketplace.json                 # NEW — self-listing marketplace
  .codex-plugin/
    plugin.json                      # NEW — Codex manifest, "skills": "./skills/"
  .cursor-plugin/
    plugin.json                      # NEW — Cursor manifest, "skills": "./skills/"
```

### Agent coverage

| Agent | Mechanism | Notes |
|---|---|---|
| Claude Code | `.claude-plugin/plugin.json` + `.claude-plugin/marketplace.json` | Native skill discovery from `skills/`. Install: `/plugin marketplace add <repo>` → `/plugin install devkit`. |
| Codex | `.codex-plugin/plugin.json` (`"skills": "./skills/"`, plus an `interface` display block) | Mirrors superpowers' `.codex-plugin/plugin.json`. |
| Cursor | `.cursor-plugin/plugin.json` (`"skills": "./skills/"`) | Mirrors superpowers' `.cursor-plugin/plugin.json`. |
| Zed | `AGENTS.md` (already present) | Zed has no skill-plugin manifest; it reads `AGENTS.md`. This is the documented ceiling, not a gap. |
| Any other / generic | `AGENTS.md` + "point at the cloned repo" | The CLIs install via `cargo install --path .` regardless of agent; the skill is guidance on top. |

### Concrete manifest shapes (from superpowers, adapted)

`.claude-plugin/plugin.json`:

```json
{
  "name": "devkit",
  "description": "Local-dev coordination skills for devkit: file locks, ports, dev servers, issue lifecycle",
  "version": "<x.y.z>",
  "author": { "name": "Lev Velykoivanenko" },
  "homepage": "<repo url>",
  "repository": "<repo url>",
  "license": "<license>",
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
    { "name": "devkit", "description": "...", "version": "<x.y.z>", "source": "./" }
  ]
}
```

`.codex-plugin/plugin.json` and `.cursor-plugin/plugin.json` carry the same
metadata plus `"skills": "./skills/"`; Codex additionally takes an `interface`
display block (displayName, category, capabilities). Exact field sets are
verified against each agent's plugin spec during the plan phase.

## Open research item (resolve in the plan phase)

**Do Codex and Cursor surface a skill from a manifest alone, or do they require a
bootstrap hook?** Superpowers ships per-agent hooks
(`hooks/hooks-codex.json`, `hooks/hooks-cursor.json`) precisely because its skills
need a session-start bootstrap. devkit's single description-triggered skill may
not. The plan must confirm, per agent, whether `"skills": "./skills/"` is enough
or a minimal hook is required — by reading each agent's current plugin/skill spec
and testing a real install. If a hook is required, add the smallest one that makes
the skill discoverable; do not port superpowers' skill-search runtime.

## Version sync

The manifests each carry a `version`. Keep them in lockstep with a small
`scripts/bump-version.sh` (superpowers has one), or wire the versions into the
existing release-please configuration. The plan picks one mechanism; do not
hand-edit versions in N files.

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
- Codex/Cursor: confirm the skill is discoverable after a real install (this is
  the same test that resolves the open research item).
- A grep gate confirms no stale binary names (`portman`, `devkit-portd`) appear in
  any new manifest or in `SKILL.md`/`AGENTS.md`/`README.md`.

## Sequencing

The CLI rename is merged, so there is no gate left. Work proceeds in this
worktree (`mcp-skill-packaging`) and lands as a PR to `main`. No file locking is
needed — the worktree is the isolation.

## Light SKILL.md freshness check

Binary names in `SKILL.md` are already current. A quick accuracy pass only; no
rewrite expected.

## Open questions

1. **License** — `AGENTS.md`/manifests need a license field; the repo has no
   `LICENSE` file yet. What license should the plugin declare (and should a
   `LICENSE` be added)?
2. **Marketplace home** — self-list in this repo (decided), but is the repo
   public / will it be pushed somewhere installable via `/plugin marketplace add`?
3. **Version source** — adopt a `bump-version.sh`, or drive manifest versions from
   release-please? (Affects whether the plugin version tracks the crate version.)
4. **Codex/Cursor hook** — pending the plan-phase research: are bootstrap hooks
   required, or do the manifests alone surface the skill?
