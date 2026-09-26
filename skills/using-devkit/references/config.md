# Configuring devkit: `devkit.toml`

Every key's name, type, default and one-paragraph meaning is in `devkit schema`, generated from the doc comments on the config types; an editor pointed at it with `devkit schema init` shows the same text on hover. This file carries what does not fit on one key: how keys interact, and what bites.

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

`query` takes an index path as an optional first argument and otherwise finds the one built for this checkout. It orders matches by a requested `--topic`, then deeper directories, then severity, then the source file's discovery tier. `--severity` is exact and `--min-severity` a floor. `--path` and `--topic` repeat; `--path` keeps repo-wide rules alongside directory ones, and `--topic` ranks rather than filters. `--format table|json|prompt`. `stats` also lists files the extractor recorded errors against. `context` prints nothing, and exits 0, outside a devkit project or with rules off.

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

A rule sees nested commands (`bash -c '...'`, `subprocess.run([...])`) and ignores quoted text that only mentions the program. Rules merge across layers by name. A child layer switches an inherited rule off with `enabled = false`. devkit's own binaries always run. `enforce_writes` is under "`[harness]`: write enforcement" below.

## `[tasks.<name>]`: canned commands

```toml
[tasks.test]
description = "Run workspace tests"
run = ["cargo", "nextest", "run", "--workspace"]
guard = true    # the command guard redirects a typed `cargo nextest run ...` here
```

A task sets `run` (one command) or `steps` (a sequence). `run` and `env` are minijinja templates: `{{ ports['api'] }}` renders a registry port, and any other name they read becomes an arg. `devrun task <name> --dry-run` renders the task with real ports and runs nothing. `references/tasks.md` covers args and the `require_live` gate.

- An arg is optional only when `[templates.variables]` gives it a default. `{{ msg | default("wip") }}` or `is defined` in the template does not make it optional.
- Give an arg a `description` in `[templates.variables]` (`msg = { description = "imperative summary" }`) instead of explaining it in a guard rule's reason. devkit shows it wherever it asks for the arg, including `issue pr create` and `issue review` errors.
- `issue`, `slug` and `branch` are undefined outside an issue worktree, so `{{ issue }}` fails there. A task that runs in both writes `{% if issue is defined %}`.
- A Doppler invocation in a task goes through the same `prd` refusal as an app launch.
- A `{ split = ..., on = ... }` entry reaches the program as separate argv entries with no shell, so spaces and quotes survive. Pick a delimiter the values cannot contain.

## `[apps.<name>]`: launches

- `{{ ports['other'] }}` in `launch` or `static_env` names another app's port in this worktree. When `other` is not running, devkit writes a pid-less reservation that a later `devrun up other` claims, so a consumer can bake the port first. A misspelled app name is an error.
- devkit refuses a Doppler launch whose config is `prd` or cannot be resolved. It reads `-c`/`--config` from `launch`, then `DOPPLER_CONFIG` from the app's env, then `doppler configure get config --scope <app dir>`.
- Configs from before launches were verbatim set `[defaults].doppler_config` and let devkit prepend `doppler run`. Move that wrapper into each app's `launch`, fold any `--preserve-env=...` into it, and delete `doppler_config`, `doppler_project` and `preserve_env`.
- A per-app memory cap that the daemon does not restart: `static_env = { NODE_OPTIONS = "--max-old-space-size=2048" }`, or a `ulimit -v` wrapper in `launch`. The runtime aborts on breach and the crash path respawns it. `[daemon] memory_max_mb` is the kernel-enforced alternative on Linux. Its cgroup setup fails open, and it does nothing on macOS or Windows.

## `[defaults]` paths

`worktree_root`, `baseline_dir` and `doppler_yaml` resolve once, when the config loads:

1. `${VAR}` becomes that environment variable. An unset one is an error naming the key and the variable. `$$` is a literal `$`, and a `$` followed by anything else is left alone.
2. A leading `~/` becomes `$HOME`.
3. A path still relative anchors to what it names, never to the working directory, and `.` and `..` fold out either way. `worktree_root` and `baseline_dir` are places on this machine, so they anchor to the directory of the config file that declared them. `doppler_yaml` is a file in the repository, so it anchors to the checkout reading the config, and each worktree reads its own copy.

`branch_prefix` gets step 1 only. That is what lets a project commit its `devkit.toml`: only `branch_prefix` is personal, so it goes in `devkit.local.toml` or reads `"${USER}/"`.

## `worktree_include` patterns

- `a/**` matches every path below `a`, direct children included. A bare `**` covers a file at the checkout root. `a/./b/*` and `a//b/*` both mean `a/b/*`.
- A symlink match is reproduced as a link with the same target, not copied through. On Windows that needs Developer Mode or administrator rights; a refused link warns and is skipped.
- A symlinked directory named as a pattern's own anchor (`linked/**`) is walked through, because the walk starts there. Write `linked/` to get the link.
- A directory match reads its whole subtree into memory before copying, so peak memory follows the largest single include.
- `issue sync-includes --overwrite` replaces files a worktree already has, and needs a scope: selectors, or `--all`.

## Baselines

A baseline is a worktree at the merge base of a branch and `baseline_ref`, shared by every worktree cut from that commit and created the first time a `--role baseline` server is asked for. Each carries `.devkit/baseline.toml`, recording its commit. A directory under `baseline_dir` without that marker is reported and never touched. A missing marker on a baseline devkit expects is rebuilt in place, and an unreadable one is refused. A worktree's `.devkit/issue.toml` records which baseline it uses, and the baseline goes once no worktree names it.

## `[harness]`: write enforcement

`enforce_writes` resolves per checkout, first answer wins:

1. `DEVKIT_ENFORCE_WRITES`: `1`/`true`/`yes`/`on` forces it on, `0`/`false`/`no`/`off` forces it off. Anything else falls through.
2. The project layers for the written path, `devkit.local.toml` and a linked worktree's main-checkout layer included. Any layer setting it turns it on. git names the checkout root.
3. The global config (`$DEVKIT_CONFIG`, else `~/.config/devkit/config.toml`), which turns it on for every checkout with no per-project file.

`enforce_writes` and `enforce_commands` turn on if any layer sets them. `shell` and the three policy keys take the closest layer's value.

What gets checked:

- Structured edits: `Edit`, `MultiEdit`, `Write`, `NotebookEdit`, and Codex's `apply_patch`.
- Shell tools (`Bash`, Claude Code's `PowerShell`, Cursor's `Shell`): the command and the scripts it runs are parsed. Redirects, `tee`, `cp`, `mv`, `rm`, `touch`, `dd`, `sed -i`, `perl -i`, the git verbs that rewrite files, common formatters, inline Python/JavaScript/TypeScript file APIs (a target from `sys.argv`/`process.argv` included) and PowerShell's content and item cmdlets all resolve to targets.
- A whole-tree writer (`cargo fmt`, `git checkout`) claims nothing and is refused while another session holds a lock under the tree.
- Build tools and package managers are not treated as writers. `devrun task <name>` is not expanded, so a task that formats the tree goes unchecked.
- Cursor gets the command guard only; its shell calls claim nothing.

A checkout opting in reads only its `[harness]` table, so the file may hold nothing else. With the global default on, `DEVKIT_ENFORCE_WRITES=off` opts one session out.

The policy keys decide what happens to a write devkit could not resolve: `block` refuses and says how to make the target explicit, `warn` allows and says what went unchecked, `allow` says nothing. A warning never overrides a conflict on a target devkit did resolve.

Failure modes: with enforcement off the hook exits at once and takes no locks. With `devkit` missing from `PATH` the hook fails and the write goes through. A registry error with enforcement on denies the write and names the error.

## `[harness.log]`

- The global-only keys are a boundary against a `devkit.toml` a project ships, not against your own environment: `$DEVKIT_CONFIG` can point anywhere, a repository included.
- Redaction matches `LINEAR_API_KEY`, `LINEAR_WORKSPACE`, `SLACK_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN` and the token prefixes of those services, replacing each with a placeholder naming its kind. It misses novel formats, credentials under other names, and anything read from a file. A `redacted` corpus is not safe to hand to a third party on that basis alone.
- The runtime reads each `[harness]` key on its own so one bad key cannot break the rest, which means a misspelled key changes nothing and reports nothing. `devkit doctor`'s `harness_log` row shows the mode in force.

## `[tracker]` and `[github]`

- Detection only decides when no config resolves: a directory outside any devkit project, or a config that fails to load. A `LINEAR_API_KEY` exported machine-wide resolves Linear for every such directory, which is why `kind` exists.
- Under the GitHub tracker a bare number is a PR, since issues and PRs share one numbering. With no resolvable `issues_repo` the project runs with no tracker, and `devkit doctor` says why.
- `kind = "none"` declares no issue states, so a merged PR and a clean tree finish a worktree. Detection that finds nothing is different: it holds the verdict open, because `issue end` deletes branches and never acts on an unanswered question.
- Every tracker question goes through the resolved tracker: `issue setup`'s slug and summary, `issue pr checkout`'s bare-number disambiguation, `issue dashboard`'s timeline, the ISSUE column of `issue prs`. `LINEAR_WORKSPACE` and `[linear] resolve_pr_links` stay Linear-specific. Under the GitHub tracker the ISSUE column carries each PR's closing issues regardless.
- Each `[github]` key resolves on its own and only when an operation needs it, so a project that only reads PRs never supplies `issues_repo`.
- `[github]` and `[preserve.<name>]` are the only tables that reject unknown keys. A misspelled `issue_repo` silently ignored would default from `origin` and query another repository's issues.
- An ssh-alias remote (`gh:owner/repo.git`) counts when `ssh -G gh` resolves it to github.com. That needs OpenSSH; otherwise name the keys, and the error names the alias and what it resolved to.

## `[hooks]`

- Hooks fail open: one that cannot render, spawn, or exit zero prints a `warning:` and the rest still run. Output is captured and discarded.
- The `issue end` keys fire after the removals and the run's summary, so a failing hook never un-reports a removal. When the main repository root does not resolve, both are skipped with a warning. A `devkit.toml` that fails to load leaves `issue end` with no hook keys, so none run.
- Each key is a list, so a project defining one replaces a machine-wide list from `~/.config/devkit/config.toml` entirely.

## `[preserve.<name>]`

- `from = [".scratch/"]` or `".scratch/**"` archives the whole tree; `dir/*` takes direct children only.
- A pattern that could leave the worktree (absolute, rooted, holding `..`, or drive-relative like `C:scratch`) is skipped with a warning. The default issue summary sits beside the worktrees directory, so no pattern reaches it; set `templates.issue_summary_path = "{{ worktree }}/.devkit/issue.md"` to make it preservable.
- A destination inside any worktree the run removes is skipped. The check asks the filesystem, so a symlink, a `..`, or different casing on a case-insensitive filesystem does not slip past it.
- Issue fields come from the worktree's `.devkit/issue.toml` and render empty without one. An entry using `primary` fails when the primary checkout cannot be resolved.
- Entries run serially in sorted name order, before any removal. Two worktrees writing the same filename into one `to` collide; template `{{ issue }}` into `to`.
- Symlinks are followed, the opposite of `worktree_include`: an archive may outlive the link's target. A copy truncates before writing, so an interrupted copy over an existing archive leaves a short file.

## `[brief]`

A section's switch and its bullets go together: `apps = false` drops the `Apps` line and the `devrun up` and `portm status` bullets, and the intro names only what survives. Set personal defaults in `~/.config/devkit/config.toml` and override per project.

## `[templates]`

Rendering is strict: an undefined variable is an error, so a typo surfaces on the first run.
