# Configuring devkit: `devkit.toml`

## Layers

- `devkit.toml` at the repository root holds what the project shares. `devkit.local.toml` beside it holds what one machine or checkout needs, overrides its twin, and belongs in `.gitignore`.
- `~/.config/devkit/config.toml` is the base layer under every project.
- Every `devkit.toml` from the filesystem root down to the working directory merges. Tables merge key by key. Scalars and arrays replace wholesale, so a deeper `[[context.files]]` or `[hooks]` list replaces the parent's list.
- `[config] root = true` stops the walk at that directory.

An edit is done when `devkit config --origin` shows each value you set coming from the file you edited. A config that fails to load shows up in `devkit brief`.

## `[rules]`: inject rules at write time

When an agent is about to write a file, devkit injects the rules governing that file from a prebuilt JSON index. `repo-rules-agent` builds the index and devkit only reads it. Without an `index` key, devkit looks where the extractor writes for this checkout's main worktree, so a built index needs no config.

```toml
[rules]
min_severity = "must"                # must | should | can: the least severe rule injected
per_event_limit = 3                  # rules plus context files one write may inject
# index = "/abs/path/index.json"     # an index outside the extractor's cache
# enabled = false                    # turn off rule and file injection
```

Injection fires:

- on structured edits the write stage allows (`Edit`, `Write`, `MultiEdit`, `NotebookEdit`, `apply_patch`), with or without `[harness] enforce_writes`. Shell writes inject nothing.
- once per rule per session. Compaction resets that record.
- only for targets inside the repository.

Separately, the session-start and post-compact hooks run `devkit rules context`, which prints the repository's `must` rules.

Verify with the same index resolution the hook uses. `devrules` is the short spelling.

```sh
devkit rules stats                                              # counts, per-file table, extraction errors
devkit rules query --path src/auth/session.rs                   # what a write there matches
devkit rules query --path src/auth/session.rs --format prompt   # the exact injected block
```

When `stats` reports no index, build one with `repo-rules-agent` or point `index` at the right file.

## `[[context.files]]`: inject whole files

Each entry names a file injected when a write matches. `path` is relative to the directory of the `devkit.toml` that declares it.

```toml
# No `when`: fires for writes under the file's own directory.
[[context.files]]
path = "crates/devkit-ports/AGENTS.md"

# With `when`: every condition present must hold.
[[context.files]]
path = "docs/deploy.md"
when = { path = "deploy/**", harness = ["codex"], env = { DEPLOY_TARGET = "" } }
```

- `when.path` is a glob over repo-relative write targets.
- `when.harness` lists harness names the caller must match.
- `when.env` lists variables the hook process must have. An empty value accepts any value.
- A file fires once per session and counts against `per_event_limit`. A missing file, a directory, non-UTF-8 content, or a file over `max_file_bytes` is skipped.
- A deeper layer's list replaces the parent's, so repeat the parent's entries you still want.

## `[harness.commands.<name>]`: command-guard rules

With `[harness] enforce_commands = true`, a rule refuses a shell command and names the replacement.

```toml
[harness.commands.worktree-add]
programs = ["git"]                  # matched against the program's basename
args = ["worktree", "add"]          # leading args, after the program's global options
reason = "Use `issue setup --slug <branch>` to create worktrees"
# action = "warn"                   # allow the command and show the reason
```

A rule sees nested commands (`bash -c '...'`, `subprocess.run([...])`) and ignores quoted text that only mentions the program. Rules merge across layers by name. A child layer switches an inherited rule off with `enabled = false`. devkit's own binaries always run. `references/locks.md` covers `enforce_writes`.

## `[tasks.<name>]`: canned commands

```toml
[tasks.test]
description = "Run workspace tests"
run = ["cargo", "nextest", "run", "--workspace"]
guard = true    # the command guard redirects a typed `cargo nextest run ...` here
```

A task sets `run` (one command) or `steps` (a sequence). `run` and `env` are minijinja templates: `{{ ports['api'] }}` renders a registry port, and any other name they read becomes an arg. `devrun task <name> --dry-run` renders the task with real ports and runs nothing. `references/tasks.md` covers args and the `require_live` gate.
