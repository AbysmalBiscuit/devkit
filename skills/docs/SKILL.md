---
name: docs
description: "Use when the answer depends on how third-party software behaves: a library, framework, CLI, engine, or app. Trigger even when you already know the answer, because training knowledge is version-stale. Trigger too when the question arrives wrapped in project context (file paths, \"fix our code\") and answering it requires knowing what a dependency does or expects."
allowed-tools: Bash(docm:*), Bash(devkit docs:*), Bash(rg:*), Bash(ast-grep:*), Glob, Grep, Read
argument-hint: "[software names] <question about external software, e.g. \"how does terminal handle sixel\">"
disable-model-invocation: false
user-invocable: true
---

# Software docs lookup

Answer from a version-correct local checkout of the software's own source and docs. Every claim traces to a file under that checkout.

`docm` is a hardlink to `devkit docs`, so `docm list` and `devkit docs list` run the same code. Use whichever resolves, for every command below.

Registered software, the version resolved here, and where that version came from:

!`docm list --project`

If the block above shows an error or literal backtick text, the command failed to run. Run `docm list --project` yourself before step 1.

Every stderr line `docm` prints is a hard stop: read and understand it before doing anything else. `docm` fails loudly rather than falling back to a wrong version, so a stderr line always means something needs your attention, or the human's, before the answer can be trusted.

## Steps

1. Match the software the question is about against the listing above. If it is missing there, go to **Registering software**.
2. Run `docm info <lib>`. It resolves the checkout from the requesting workspace's own manifest and lockfile, not a bare version match: `commit` in the output is the proof of what is checked out, the printed `version` string is not. The output also maps the layout (docs/src/examples dirs, doc system) and carries any notes. The first resolution of a new ref fetches git blobs and takes a few seconds.
3. Search only under the printed path: the docs dir for guides and concepts, the source dir for API ground truth, examples for usage patterns. Grep and Glob always work. Where they are on PATH, prefer `rg` for multiline patterns and wide context, and `ast-grep -p` when the match depends on code structure rather than text (a signature's shape, a call's arguments).
4. Answer with `file:line` citations relative to the checkout.

## Registering software

`docm list --project` is filtered to this project, so absence there means "not evidenced in this checkout", not "unregistered". Run `docm list` unfiltered before concluding anything is missing.

Genuinely absent: `docm add <package>` (registry lookup) or `docm add <git-url>`, always with `--notes "<workspace>: <why this version>"` naming the workspace whose manifest or lockfile the version came from. `--project` edits the repo's devkit.toml, so ask the human before adding with it.

## Rules

- When a `docm` error offers `--allow-default-branch`, stop and ask. It answers from the default branch instead of the version this project pins, which is the one thing this skill exists to prevent.
- Re-run `docm info` every time. Versions differ per project, so a checkout path carried over from an earlier session points at the wrong one.
- `docm path <lib>` prints just the path when that is all you need.
- Read another version's file straight out of the bare clone: `git -C <checkout> show <other-tag>:<path>`. Every tag is already there, so a version comparison is a lookup, not a recollection, and it needs no second checkout.
- When a checkout looks wrong for reasons `docm` cannot see (an upstream repo whose root manifest is decoupled from its release tags, say), compare against the installed package under `node_modules` or the ecosystem's equivalent. That is ground truth for what actually runs.
