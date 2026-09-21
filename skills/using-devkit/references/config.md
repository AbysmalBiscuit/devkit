# Configuring devkit: `devkit.toml`

`docs/configuration.md` in the devkit repository is the full reference. This file covers the sections an agent is most often asked to set up, and how to check the result. When a key's name, type or default is in question, `devkit schema` is the authority.

## Where config lives and how it merges

- `devkit.toml` at the repository root holds what the project shares. `devkit.local.toml` beside it holds what one machine or checkout needs. It overrides its twin and should be gitignored.
- `~/.config/devkit/config.toml` is the base layer under every project.
- Every `devkit.toml` from the filesystem root down to the working directory merges. Tables merge key by key. Scalars and arrays replace wholesale, so a deeper `[[context.files]]` or `[hooks]` list replaces the parent's list instead of appending to it.
- `[config] root = true` stops the walk at that directory.

Check what you wrote before calling it done:

```sh
devkit config                 # the merged result, headed by its layer files
devkit config --origin        # which file each value came from
devkit schema init            # add the #:schema line so an editor validates the file
devkit brief                  # a config that fails to load is reported here
```

## `[rules]`: inject rules at write time

devkit reads a prebuilt JSON rule index and, when an agent is about to write a file, injects the rules that govern that file into the agent's context. `repo-rules-agent` builds the index from the repository's instruction docs, and devkit only reads it. With no `index` key, devkit looks where the extractor writes for this checkout's main worktree, so a built index needs no config.

```toml
[rules]
enabled = true            # default true; false turns off rule and file injection
min_severity = "should"   # must | should | can; rules below the floor are dropped
per_event_limit = 5       # rules plus files one write may inject
max_file_bytes = 16384    # a context file larger than this is skipped, not truncated
max_event_bytes = 65536   # one event's rendered total is truncated to this
# index = "/abs/path/index.json"   # only when the index lives somewhere else
```

When it fires:

- On structured edits that the write stage allows: `Edit`, `Write`, `MultiEdit`, `NotebookEdit`, `apply_patch`. Shell writes inject nothing.
- Whether or not `[harness] enforce_writes` is on.
- Once per rule per session. Compaction clears that record, so the rules can fire again afterward.
- Never for a target outside the repository.

Session start is separate. The session-start and post-compact hooks run `devkit rules context`, which prints the repository's `must` rules.

Verify with the same index resolution the hook uses:

```sh
devkit rules stats                                   # counts, per-file table, extraction errors
devkit rules query --path src/auth/session.rs        # what a write there would match
devkit rules query --path src/auth/session.rs --format prompt   # the exact injected block
```

`devrules` is the short spelling of `devkit rules`. When `stats` finds no index, build one with `repo-rules-agent` or point `index` at the right file.

## `[[context.files]]`: inject whole files

Each entry names a file injected into the agent's context when a write matches. `path` is relative to the directory of the `devkit.toml` that declares it.

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
- `when.env` lists variables that must be set in the hook process. An empty value means any value.
- A file fires once per session, shares `per_event_limit` with rules, and is skipped when it is missing, a directory, not UTF-8, or over `max_file_bytes`.
- A deeper layer's list replaces the parent's list. Repeat the parent's entries if you still want them.

## `[harness]`: write locks and the command guard

```toml
[harness]
enforce_writes = true      # the write hook claims a lock on every file an edit touches
enforce_commands = true    # refuse shell commands devkit already has a wired-up path for

[harness.commands.worktree-add]
programs = ["git"]                  # matched against the program's basename
args = ["worktree", "add"]          # leading args, after the program's global options
reason = "Use `issue setup --slug <branch>` to create worktrees"
# action = "warn"                   # default "block"
```

A rule matches nested commands (`bash -c '...'`, `subprocess.run([...])`) but not quoted text that only mentions the program. Rules merge across layers by name. A child layer turns an inherited rule off with `enabled = false` or `programs = []`. devkit's own binaries are never refused.

`enforce_writes` also turns on from the global config or `DEVKIT_ENFORCE_WRITES=1`. `references/locks.md` covers what enforcement does once on.

## `[tasks.<name>]`: canned commands

```toml
[tasks.test]
description = "Run workspace tests"
run = ["cargo", "nextest", "run", "--workspace"]
guard = true    # the command guard redirects a typed `cargo nextest run ...` here
```

A task sets `run` (one command) or `steps` (a sequence), never both. `run` and `env` are minijinja templates. `{{ ports['api'] }}` renders a registry port, and any other name they read becomes an arg. Check a task with `devrun task <name> --dry-run`, which renders it with real ports and runs nothing. `references/tasks.md` covers args and the `require_live` gate.
