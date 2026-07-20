---
name: docs
description: Use whenever a question or task hinges on how a third-party library, framework, or crate actually behaves — API semantics, feature support, version differences, intended usage (e.g. "how do I cancel a JoinHandle in tokio", "why does kysely's leftJoin filter rows"). Trigger even when you already know the answer — training knowledge is version-stale; this skill resolves a version-correct local checkout of the library's own source and docs via the docm CLI (any library, registered or not) and answers from that. The library question is often wrapped in project context — file paths, line numbers, "fix our code" — if resolving it requires knowing what the dependency does or expects, use this skill first. Also use on /docs. Not for: this repo's own code, bumping dependency versions, summarizing web content, or authoring project documentation.
allowed-tools: Bash(docm list)
---

# Library docs lookup

Answer library-usage questions from a local, version-correct checkout of the
library's own source and docs — not from memory.

Registered libraries (name, ecosystem, pinned ref, synced versions):

!`docm list`

If the block above shows an error or literal backtick text instead of a
table, the command did not run — run `docm list` yourself before step 1.

## Steps

1. Identify which library the question is about and match it against the
   registered names above. If it is not registered: `docm add <package>`
   (registry lookup) or `docm add <git-url>`, then continue. Ask before
   adding with `--project` (that edits the repo's devkit.toml).
2. Run `docm info <lib>`. It prints the checkout path (version-matched to the
   current project's lockfile), the resolved version, a layout map
   (docs/src/examples dirs, doc system), and any notes. The first resolution
   of a new version fetches git blobs and can take a few seconds. Warnings on
   stderr (e.g. "falling back to default branch") are context — relay them if
   the answer depends on the version.
3. Search ONLY under the printed path: the docs dir for guides and concepts,
   the source dir for API ground truth, examples for usage patterns. Use
   `rg` for text and `ast-grep` for structural queries.
4. Answer with `file:line` citations relative to the checkout.

## Rules

- Never reuse a checkout path from memory or an earlier session — versions
  differ per project. Always re-run `docm info`.
- `docm path <lib>` prints just the path when that is all you need.
- If `docm` is not on PATH, tell the user to `cargo install --path .` in the
  devkit repo.
