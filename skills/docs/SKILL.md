---
name: docs
description: Use whenever a question or task hinges on how a third-party library, framework, or crate actually behaves — API semantics, feature support, version differences, intended usage (e.g. "how do I cancel a JoinHandle in tokio", "why does kysely's leftJoin filter rows"). Trigger even when you already know the answer — training knowledge is version-stale; this skill resolves a version-correct local checkout of the library's own source and docs via the docm CLI (any library, registered or not) and answers from that. The library question is often wrapped in project context — file paths, line numbers, "fix our code" — if resolving it requires knowing what the dependency does or expects, use this skill first. Also use on /docs. Not for: this repo's own code, bumping dependency versions, summarizing web content, or authoring project documentation.
allowed-tools: Bash(docm list), Bash(docm list --project)
---

# Library docs lookup

Answer library-usage questions from a local, version-correct checkout of the
library's own source and docs — not from memory.

Libraries this checkout evidences — name, version, and where that version
came from. A trailing count, when present, is of registered libraries this
checkout does not evidence; they are still registered, and `docm list`
shows them:

!`docm list --project`

If the block above shows an error or literal backtick text instead of a
listing, the command did not run — run `docm list --project` yourself before
step 1.

## Steps

1. Identify which library the question is about and match it against the
   listing above. Absence from that listing means "not evidenced in this
   checkout", **not** "unregistered" — it is filtered to this project. Before
   concluding a library is unregistered, run `docm list` (unfiltered). If it
   is genuinely absent there: `docm add <package>` (registry lookup) or
   `docm add <git-url>`, always with `--notes "<workspace>: <why this
   version>"` recording which workspace's manifest/lockfile the version came
   from. Ask before adding with `--project` (that edits the repo's
   devkit.toml).
2. Run `docm info <lib>`. It resolves the checkout path from the requesting
   workspace's own manifest and lockfile, not a bare version match — `commit`
   in the output is the proof of what is actually checked out, the printed
   `version` string is not. It also prints a layout map (docs/src/examples
   dirs, doc system) and any notes. The first resolution of a new ref fetches
   git blobs and can take a few seconds.
3. Any stderr line `docm` prints is a hard stop: read and understand it
   before doing anything else. `docm` fails hard rather than silently
   falling back to a wrong version, so a stderr line is never optional
   context to relay only if it seems relevant — it means something needs
   your attention, or the human's, before the answer can be trusted.
4. Search ONLY under the printed path: the docs dir for guides and concepts,
   the source dir for API ground truth, examples for usage patterns. Use
   `rg` for text and `ast-grep` for structural queries.
5. Answer with `file:line` citations relative to the checkout.

## Rules

- Never reuse a checkout path from memory or an earlier session — versions
  differ per project. Always re-run `docm info`.
- `docm path <lib>` prints just the path when that is all you need.
- If a checkout looks wrong for reasons `docm` cannot see (e.g. an upstream
  repo whose root manifest is decoupled from its release tags), compare
  against the installed package under `node_modules` (or the ecosystem's
  equivalent) — that is ground truth for what actually runs.
- If `docm` is not on PATH, tell the user to `cargo install --path .` in the
  devkit repo.
- Comparing against another version is a lookup, not a recollection. The bare
  clone under the cache already holds every tag, so `git -C <checkout> show
  <other-tag>:<path>` reads the other version's file directly — do not answer
  "it changed in vX" from memory, and do not sync a second checkout for it.
