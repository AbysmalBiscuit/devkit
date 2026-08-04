---
name: docs
description: Use whenever a question or task hinges on how a third-party library, framework, or crate actually behaves — API semantics, feature support, version differences, intended usage (e.g. "how do I cancel a JoinHandle in tokio", "why does kysely's leftJoin filter rows"). Trigger even when you already know the answer — training knowledge is version-stale; this skill resolves a version-correct local checkout of the library's own source and docs via the docm CLI (any library, registered or not) and answers from that. The library question is often wrapped in project context — file paths, line numbers, "fix our code" — if resolving it requires knowing what the dependency does or expects, use this skill first. Also use on /docs. Not for: this repo's own code, bumping dependency versions, summarizing web content, or authoring project documentation.
allowed-tools: Bash(docm list)
---

# Library docs lookup

Answer library-usage questions from a local, version-correct checkout of the
library's own source and docs — not from memory.

Registered libraries — name, ecosystem, pinned ref, and origin on the first
line, each synced checkout's directory, commit, and ref indented below it:

!`docm list`

If the block above shows an error or literal backtick text instead of a
listing, the command did not run — run `docm list` yourself before step 1.

## Steps

1. Identify which library the question is about and match it against the
   registered names above. If it is not registered: `docm add <package>`
   (registry lookup) or `docm add <git-url>`, always with
   `--notes "<workspace>: <why this version>"` recording which workspace's
   manifest/lockfile the version came from. Ask before adding with
   `--project` (that edits the repo's devkit.toml).
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
