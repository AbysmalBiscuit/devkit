# Multi-agent skill packaging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the `using-devkit` skill installable across Claude Code, Codex, and Cursor (with Zed/generic served by the existing `AGENTS.md`), and capture the deferred MCP-server analysis in `docs/next-steps.md`.

**Architecture:** Keep the existing `skills/using-devkit/` as the single source of truth and add thin per-agent manifests beside it (`.claude-plugin/` + `marketplace.json`, `.codex-plugin/`, `.cursor-plugin/`). Codex and Cursor have no native skill auto-discovery, so a shared, Windows-safe SessionStart hook injects a short skill-availability notice into those agents. release-please owns all manifest versions.

**Tech Stack:** JSON manifests, a bash hook script + a polyglot `.cmd` runner shim (Windows + Unix), release-please config, Markdown docs. No Rust source changes.

## Global Constraints

- **License:** GPL-3.0; SPDX id `GPL-3.0-or-later` in `Cargo.toml` and every manifest `license` field.
- **Repo / marketplace home:** `github.com/AbysmalBiscuit/devkit`; install via `/plugin marketplace add AbysmalBiscuit/devkit`.
- **Version source:** release-please only. Manifest `version` starts at `0.1.0` to match the crate. Set `version` in `plugin.json` (Claude/Codex/Cursor) but **not** in the marketplace entry — Claude version precedence is `plugin.json` > marketplace entry > git SHA, and docs warn against setting both.
- **Binary names are final:** `portm`, `lockm`, `devkitd`, `devrun`, `issue`. No `portman` / `devkit-portd` anywhere in new files.
- **Commit convention:** Conventional Commits; imperative, lowercase subject ≤50 chars; footer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` and no other trailers.
- **Platform:** Windows is the dev platform; the hook runner must work via Git Bash on Windows as well as Unix.
- **Context-injection envelopes differ per agent (verified against current docs):** Cursor reads `additional_context` (snake_case, top-level); Codex reads `hookSpecificOutput.additionalContext`. Claude Code needs no hook (native skill discovery).

---

### Task 1: License

**Files:**
- Create: `LICENSE`
- Modify: `Cargo.toml:1-5` (the `[package]` table)

**Interfaces:**
- Consumes: nothing.
- Produces: a `LICENSE` file and `package.license = "GPL-3.0-or-later"` that every later manifest's `license` field must match.

- [ ] **Step 1: Fetch the canonical GPL-3.0 text**

Run:
```bash
curl -fsSL https://www.gnu.org/licenses/gpl-3.0.txt -o LICENSE
```

- [ ] **Step 2: Verify the license downloaded intact**

Run:
```bash
head -1 LICENSE && wc -l LICENSE
```
Expected: first line is `                    GNU GENERAL PUBLIC LICENSE`; line count is `674`.

- [ ] **Step 3: Add the license field to `Cargo.toml`**

In `Cargo.toml`, add a `license` line to the `[package]` table immediately after the `version` line so it reads:

```toml
[package]
name = "devkit"
description = "Local development coordination tools: portm, lockm, devrun, issue, devkitd"
edition.workspace = true
version = "0.1.0" # x-release-please-version
license = "GPL-3.0-or-later"
```

- [ ] **Step 4: Verify Cargo still parses the manifest**

Run:
```bash
cargo metadata --no-deps --format-version 1 >/dev/null && echo OK
```
Expected: `OK` (no TOML parse error).

- [ ] **Step 5: Commit**

```bash
git add LICENSE Cargo.toml
git commit -m "chore: add GPL-3.0 license" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Claude Code plugin manifests

**Files:**
- Create: `.claude-plugin/plugin.json`
- Create: `.claude-plugin/marketplace.json`

**Interfaces:**
- Consumes: the existing `skills/using-devkit/` directory (auto-discovered by Claude Code — no `skills` field needed).
- Produces: an installable Claude Code plugin named `devkit`, self-listed in a marketplace whose plugin `source` is the repo root `"./"`.

- [ ] **Step 1: Create `.claude-plugin/plugin.json`**

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

- [ ] **Step 2: Create `.claude-plugin/marketplace.json`**

The plugin lives at the repo root, so `source` is `"./"`. The entry omits `version` (it comes from `plugin.json`).

```json
{
  "name": "devkit",
  "description": "devkit local-dev coordination skills",
  "owner": { "name": "Lev Velykoivanenko" },
  "plugins": [
    {
      "name": "devkit",
      "source": "./",
      "description": "Local-dev coordination skills for devkit: file locks, ports, dev servers, issue lifecycle"
    }
  ]
}
```

- [ ] **Step 3: Verify both manifests are valid JSON**

Run:
```bash
jq . .claude-plugin/plugin.json >/dev/null && jq . .claude-plugin/marketplace.json >/dev/null && echo OK
```
Expected: `OK`.

- [ ] **Step 4: Validate the plugin with the plugin-validator agent**

Dispatch the `plugin-dev:plugin-validator` agent against the repo root. Expected verdict: the plugin structure and `plugin.json` are valid; the `skills/using-devkit/` skill is discovered. Fix any errors it reports before committing.

- [ ] **Step 5: Commit**

```bash
git add .claude-plugin/plugin.json .claude-plugin/marketplace.json
git commit -m "feat: add claude code plugin manifest" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Codex/Cursor wiring (hooks + manifests)

Codex and Cursor have no native skill auto-discovery, so a SessionStart hook injects a short availability notice that tells the agent the skill exists and where to read it. The runner shim makes the hook work on Windows (Git Bash) and Unix.

**Files:**
- Create: `hooks/run-hook.cmd`
- Create: `hooks/announce-skill`
- Create: `hooks/hooks-codex.json`
- Create: `hooks/hooks-cursor.json`
- Create: `.codex-plugin/plugin.json`
- Create: `.cursor-plugin/plugin.json`
- Test: run `hooks/announce-skill` directly (see Step 6)

**Interfaces:**
- Consumes: nothing (the notice text is self-contained; it points the agent at `skills/using-devkit/SKILL.md`).
- Produces: `hooks/announce-skill`, which emits a JSON object on stdout — `{"additional_context": "..."}` when `CURSOR_PLUGIN_ROOT` is set, otherwise `{"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "..."}}`. Both manifests reference `./hooks/run-hook.cmd announce-skill`.

- [ ] **Step 1: Create `hooks/run-hook.cmd` (polyglot Windows/Unix runner)**

```
: << 'CMDBLOCK'
@echo off
REM Cross-platform polyglot wrapper for hook scripts.
REM On Windows: cmd.exe runs the batch portion, which finds and calls bash.
REM On Unix: the shell interprets this as a script (: is a no-op in bash).
REM
REM Hook scripts use extensionless filenames so Windows .sh auto-detection
REM doesn't interfere.
REM
REM Usage: run-hook.cmd <script-name> [args...]

if "%~1"=="" (
    echo run-hook.cmd: missing script name >&2
    exit /b 1
)

set "HOOK_DIR=%~dp0"

REM Try Git for Windows bash in standard locations
if exist "C:\Program Files\Git\bin\bash.exe" (
    "C:\Program Files\Git\bin\bash.exe" "%HOOK_DIR%%~1" %2 %3 %4 %5 %6 %7 %8 %9
    exit /b %ERRORLEVEL%
)
if exist "C:\Program Files (x86)\Git\bin\bash.exe" (
    "C:\Program Files (x86)\Git\bin\bash.exe" "%HOOK_DIR%%~1" %2 %3 %4 %5 %6 %7 %8 %9
    exit /b %ERRORLEVEL%
)

REM Try bash on PATH (user-installed Git Bash, MSYS2, Cygwin)
where bash >nul 2>nul
if %ERRORLEVEL% equ 0 (
    bash "%HOOK_DIR%%~1" %2 %3 %4 %5 %6 %7 %8 %9
    exit /b %ERRORLEVEL%
)

REM No bash found - exit silently rather than error
REM (plugin still works, just without SessionStart context injection)
exit /b 0
CMDBLOCK

# Unix: run the named script directly
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SCRIPT_NAME="$1"
shift
exec bash "${SCRIPT_DIR}/${SCRIPT_NAME}" "$@"
```

- [ ] **Step 2: Create `hooks/announce-skill` (the availability-notice emitter)**

```bash
#!/usr/bin/env bash
# SessionStart hook for the devkit plugin (Codex/Cursor).
# Injects a short notice that the using-devkit skill exists and where to read it,
# rather than the full skill body (the skill is only conditionally relevant).

set -euo pipefail

notice="A 'using-devkit' skill is available in this repository. When you are working in a checkout shared by multiple agents and about to edit files, claim them first with 'lockm acquire <paths> --as <stable-id>' (it exits 1 on conflict — do not edit a held path). The same skill covers allocating dev-server ports (portm), running local dev servers (devrun), and issue worktrees (issue). Before using any of these, read skills/using-devkit/SKILL.md and follow it."

# Escape for JSON embedding (single-pass bash substitutions).
escape_for_json() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    s="${s//$'\n'/\\n}"
    s="${s//$'\r'/\\r}"
    s="${s//$'\t'/\\t}"
    printf '%s' "$s"
}

escaped=$(escape_for_json "$notice")

# Cursor reads additional_context (snake_case); Codex reads
# hookSpecificOutput.additionalContext. printf (not heredoc) avoids a
# bash 5.3+ heredoc hang.
if [ -n "${CURSOR_PLUGIN_ROOT:-}" ]; then
  printf '{\n  "additional_context": "%s"\n}\n' "$escaped" | cat
else
  printf '{\n  "hookSpecificOutput": {\n    "hookEventName": "SessionStart",\n    "additionalContext": "%s"\n  }\n}\n' "$escaped" | cat
fi

exit 0
```

- [ ] **Step 3: Mark the hook scripts executable**

Run:
```bash
chmod +x hooks/run-hook.cmd hooks/announce-skill
```

- [ ] **Step 4: Create `hooks/hooks-codex.json`**

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume|clear",
        "hooks": [
          {
            "type": "command",
            "command": "\"${PLUGIN_ROOT}/hooks/run-hook.cmd\" announce-skill",
            "async": false
          }
        ]
      }
    ]
  }
}
```

- [ ] **Step 5: Create `hooks/hooks-cursor.json`**

```json
{
  "version": 1,
  "hooks": {
    "sessionStart": [
      {
        "type": "command",
        "command": "./hooks/run-hook.cmd announce-skill"
      }
    ]
  }
}
```

- [ ] **Step 6: Write a test that runs the hook and verify it FAILS first**

Confirm the gate works by running it before the next steps could mask a regression. Run:
```bash
CURSOR_PLUGIN_ROOT=x bash hooks/announce-skill | jq -e '.additional_context | test("using-devkit")' >/dev/null && echo CURSOR_OK
bash hooks/announce-skill | jq -e '.hookSpecificOutput.additionalContext | test("using-devkit")' >/dev/null && echo CODEX_OK
```
Expected after Steps 2-3: `CURSOR_OK` then `CODEX_OK`. (If you run this before Step 2 exists, it fails with "No such file" — that is the red state proving the test is real.)

- [ ] **Step 7: Create `.codex-plugin/plugin.json`**

```json
{
  "name": "devkit",
  "version": "0.1.0",
  "description": "Local-dev coordination skills for devkit: file locks, ports, dev servers, issue lifecycle",
  "author": { "name": "Lev Velykoivanenko" },
  "homepage": "https://github.com/AbysmalBiscuit/devkit",
  "repository": "https://github.com/AbysmalBiscuit/devkit",
  "license": "GPL-3.0-or-later",
  "keywords": ["devkit", "file-locks", "ports", "dev-servers", "worktrees"],
  "skills": "./skills/",
  "hooks": "./hooks/hooks-codex.json",
  "interface": {
    "displayName": "devkit",
    "shortDescription": "Local-dev coordination: file locks, ports, dev servers, issue lifecycle",
    "category": "Coding",
    "capabilities": ["Read", "Write"]
  }
}
```

- [ ] **Step 8: Create `.cursor-plugin/plugin.json`**

```json
{
  "name": "devkit",
  "displayName": "devkit",
  "version": "0.1.0",
  "description": "Local-dev coordination skills for devkit: file locks, ports, dev servers, issue lifecycle",
  "author": { "name": "Lev Velykoivanenko" },
  "homepage": "https://github.com/AbysmalBiscuit/devkit",
  "repository": "https://github.com/AbysmalBiscuit/devkit",
  "license": "GPL-3.0-or-later",
  "keywords": ["devkit", "file-locks", "ports", "dev-servers", "worktrees"],
  "skills": "./skills/",
  "hooks": "./hooks/hooks-cursor.json"
}
```

- [ ] **Step 9: Verify all four JSON files parse**

Run:
```bash
for f in hooks/hooks-codex.json hooks/hooks-cursor.json .codex-plugin/plugin.json .cursor-plugin/plugin.json; do jq . "$f" >/dev/null && echo "ok $f"; done
```
Expected: four `ok <file>` lines.

- [ ] **Step 10: Commit (force the executable bit into the index)**

`git add --chmod=+x` records the exec bit regardless of platform, so the hook
scripts stay runnable on Unix even when committed from Windows (`core.filemode=false`).

```bash
git add --chmod=+x hooks/run-hook.cmd hooks/announce-skill
git add hooks/hooks-codex.json hooks/hooks-cursor.json .codex-plugin/plugin.json .cursor-plugin/plugin.json
git commit -m "feat: add codex and cursor skill plugins" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: release-please version sync

**Files:**
- Modify: `release-please-config.json`

**Interfaces:**
- Consumes: the three versioned manifests from Tasks 2-3 (`.claude-plugin/plugin.json`, `.codex-plugin/plugin.json`, `.cursor-plugin/plugin.json`).
- Produces: a release-please config that bumps all three on release alongside `Cargo.toml`.

- [ ] **Step 1: Add `extra-files` to the `.` package**

Edit `release-please-config.json` so the `.` package gains an `extra-files` array. Use the generic `json` updater with a `$.version` jsonpath (numeric paths only — filter expressions `[?()]` are not reliably supported). The marketplace entry is intentionally absent (it carries no version):

```json
{
  "$schema": "https://raw.githubusercontent.com/googleapis/release-please/main/schemas/config.json",
  "include-component-in-tag": false,
  "packages": {
    ".": {
      "release-type": "rust",
      "package-name": "devkit",
      "extra-files": [
        { "type": "json", "path": ".claude-plugin/plugin.json", "jsonpath": "$.version" },
        { "type": "json", "path": ".codex-plugin/plugin.json", "jsonpath": "$.version" },
        { "type": "json", "path": ".cursor-plugin/plugin.json", "jsonpath": "$.version" }
      ]
    }
  }
}
```

- [ ] **Step 2: Verify the config is valid JSON**

Run:
```bash
jq . release-please-config.json >/dev/null && echo OK
```
Expected: `OK`.

- [ ] **Step 3: Commit**

```bash
git add release-please-config.json
git commit -m "ci: bump plugin manifest versions via release-please" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Capture the deferred MCP design in next-steps

**Files:**
- Modify: `docs/next-steps.md` (replace the `## MCP server for devkit` section starting at line 25)

**Interfaces:**
- Consumes: nothing.
- Produces: an enriched MCP section so the future MCP brainstorm starts from analysis, not a stub.

- [ ] **Step 1: Replace the MCP section body**

Replace the entire `## MCP server for devkit` section (the heading and the paragraph beneath it) with:

```markdown
## MCP server for devkit

Expose devkit's capabilities to coding agents over the Model Context Protocol so
an agent can drive port allocation, dev-server supervision, the issue lifecycle,
and file locks directly instead of shelling out to the CLIs. Deferred; this
section captures the analysis so the brainstorm starts ahead.

**Surface area (~19 actions).** `portm` (status, alloc, release, prune), `lockm`
(acquire, check, release, status, prune), `devrun` (up, down, status, logs),
`issue` (setup, status, end, prs, dashboard, review). `completions` is shell-only
and not exposed.

**Host-agnostic, assume eager loading.** Targets include Codex, Zed, Cursor, and
generic MCP clients, many of which load every tool schema up front — so keep the
top-level tool count low. (Claude Code lazy-loads via deferred-tool search, so the
count matters less there, but design for the worst case.)

**Three exposure shapes, meta-or-dispatch favored:**

1. *Flat* — one MCP tool per action (~19 tools); best discoverability and arg
   validation, heaviest footprint.
2. *Per-binary dispatch* — four tools (`portm`/`devrun`/`issue`/`lockm`), each
   taking an `action` + args; ~5x fewer tools, natural groupings, looser
   per-action schemas.
3. *True meta-MCP / progressive disclosure* — a `list`/`describe` tool plus a
   `call` tool; minimal footprint, scales as devkit grows, at the cost of a
   discovery round-trip per novel action.

Lean toward dispatch or meta given the agnostic, eager-loading hosts. Settle the
exact choice in the MCP's own brainstorm.

**Implementation stance.** Tools mirror the existing CLIs over the library crates
(`devkit-ports`, `devkit-locks`, `devkit-common`) rather than reinvent them. The
MCP server ships in the same multi-agent plugin as the `using-devkit` skill (see
`docs/superpowers/specs/2026-06-21-multi-agent-skill-packaging-design.md`).

**Daemon relationship.** An MCP server is a natural long-lived host that could keep
the in-memory registries (`devkitd`) warm.

**Open boundaries.** Which surfaces to expose first; the read-only vs. mutating
tool split.

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
```

- [ ] **Step 2: Verify the new content is present and the old stub is gone**

Run:
```bash
rg -n "Three exposure shapes" docs/next-steps.md && rg -n "Verify multi-agent plugin packaging" docs/next-steps.md && (rg -q "Open questions to settle in its own brainstorming pass" docs/next-steps.md && echo "STUB STILL PRESENT" || echo "old stub removed")
```
Expected: a line match for "Three exposure shapes" and one for "Verify multi-agent plugin packaging"; then `old stub removed`.

- [ ] **Step 3: Commit**

```bash
git add docs/next-steps.md
git commit -m "docs: capture deferred mcp server analysis" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: SKILL.md freshness + repo-wide gates

**Files:**
- Modify (only if Step 2 finds drift): `skills/using-devkit/SKILL.md`

**Interfaces:**
- Consumes: all new files from Tasks 1-5.
- Produces: a verified-clean working tree — no stale binary names anywhere, tests green.

- [ ] **Step 1: Read the skill and confirm names are current**

Check only the unambiguous old binary names (`portman`, `devkit-portd`) — not the
bare word `lock`, which appears legitimately throughout (advisory file locks). The
old `lock` CLI was renamed to `lockm`; confirm no bare `lock <subcommand>`
invocations remain by spot-reading the command examples.

Run:
```bash
rg -nw "portman|devkit-portd" skills/using-devkit/SKILL.md || echo "no stale names"
```
Expected: `no stale names` (the rename session already updated it). If anything matches, replace the stale name with its current form (`portman`→`portm`, `devkit-portd`→`devkitd`) and re-run.

- [ ] **Step 2: Grep gate — no stale binary names in any shipped doc or manifest**

Run:
```bash
rg -nw "portman|devkit-portd" skills/ AGENTS.md README.md .claude-plugin/ .codex-plugin/ .cursor-plugin/ hooks/ docs/next-steps.md || echo "GATE PASS"
```
Expected: `GATE PASS`.

- [ ] **Step 3: Sanity-check that no Rust source was disturbed**

Run:
```bash
cargo test --workspace
```
Expected: all tests pass (this change touches no Rust; the run confirms the worktree still builds clean).

- [ ] **Step 4: Commit (only if Step 1 changed `SKILL.md`)**

```bash
git add skills/using-devkit/SKILL.md
git commit -m "docs: refresh using-devkit skill binary names" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```
If Step 1 reported `no stale names` and made no edit, skip this commit.

---

### Task 7: Post-merge — install smoke test + delete the stale copy

This task runs **after the PR merges to `main` and the marketplace is reachable on GitHub**, because relative-path plugin sources only resolve when the marketplace is added via git. It cannot be completed inside the worktree.

**Files:**
- Delete: `~/.claude/skills/using-devkit/` (the hand-copied, drifting copy)

**Interfaces:**
- Consumes: the merged, pushed plugin.
- Produces: a single source of truth for the skill (the installed plugin).

- [ ] **Step 1: Add the marketplace and install the plugin in a fresh Claude Code session**

```
/plugin marketplace add AbysmalBiscuit/devkit
/plugin install devkit
```

- [ ] **Step 2: Confirm the skill resolves from the plugin**

In that session, confirm the `using-devkit` skill appears in the available-skills list (sourced from the installed plugin, not the `~/.claude` copy).

- [ ] **Step 3: Delete the stale hand-copy**

Run:
```bash
rm -rf ~/.claude/skills/using-devkit
```
Expected: the skill still resolves via the plugin in a new session; no duplicate remains.

- [ ] **Step 4 (optional, manual): Codex/Cursor install smoke test**

Install the plugin in Codex and Cursor and start a fresh session in a repo. Confirm the SessionStart hook fires and the "A 'using-devkit' skill is available" notice appears in the session's initial context. On Windows, confirm `run-hook.cmd` locates Git Bash. If an envelope is rejected, re-check the field name against current docs (Codex `hookSpecificOutput.additionalContext`, Cursor `additional_context`) and adjust `hooks/announce-skill`.

---

## Self-Review

**Spec coverage:**
- One-source / per-agent manifests → Tasks 2, 3. ✓
- Claude plugin + self-listed marketplace → Task 2. ✓
- Codex/Cursor manifests + SessionStart hook + Windows runner → Task 3. ✓
- License (GPL-3.0, LICENSE + Cargo + manifest fields) → Tasks 1-3. ✓
- Version sync via release-please extra-files → Task 4. ✓
- MCP deferral content → Task 5. ✓
- SKILL.md freshness + grep gate + cargo sanity → Task 6. ✓
- Delete `~/.claude` copy after confirming load → Task 7. ✓
- Zed/generic via `AGENTS.md` → already present; no task needed (noted in spec). ✓

**Placeholder scan:** No "TBD"/"handle errors"/"similar to Task N". The GPL text is fetched verbatim by URL rather than pasted (it is a fixed standard document, not authored content). All hook/manifest/JSON bodies are complete.

**Type/name consistency:** Hook script name `announce-skill` is identical across `run-hook.cmd` invocation, both hook JSONs, and the test. Plugin name `devkit` and version `0.1.0` match across all four manifests. Context-injection field names match the verified per-agent envelopes (`additional_context` / `hookSpecificOutput.additionalContext`).

## Unresolved questions

1. **Marketplace install form** — `/plugin marketplace add AbysmalBiscuit/devkit` assumes the repo is public. If it is private, the plan's Task 7 install step needs an auth'd git source instead. Confirm the repo's visibility.
2. **Codex/Cursor real-world envelope** — verified against current docs, but neither agent has been tried yet. Task 7 Step 4 is the live confirmation; if a field name has changed since, `hooks/announce-skill` is the single place to adjust.
