---
name: docs
description: Use when the user asks how to use, configure, or debug an external library or framework (e.g. "how do I cancel a JoinHandle in tokio", "what does this godot node do"), or invokes /docs. Resolves a version-correct local checkout of the library's source and docs via the docm CLI, then searches it. First word of the argument is the library name; the rest is the question.
---

# Library docs lookup

Answer library-usage questions from a local, version-correct checkout of the
library's own source and docs — not from memory.

## Steps

1. Identify the library: the first token of the `/docs` argument, or infer it
   from the question. `docm list` prints the registered names; match against
   those.
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
- If the library is not registered: `docm add <package>` (registry lookup) or
  `docm add <git-url>`, then retry. Ask before adding with `--project`
  (that edits the repo's devkit.toml).
- `docm path <lib>` prints just the path when that is all you need.
- If `docm` is not on PATH, tell the user to `cargo install --path .` in the
  devkit repo.
