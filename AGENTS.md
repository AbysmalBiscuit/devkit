# devkit

A Rust workspace (edition 2024) that coordinates many concurrent local dev sessions, human and agent, on one machine: port and file-lock registries, a dev-server supervisor, issue worktrees, and the hooks coding agents call. The engine is project-agnostic; everything project-specific lives in `devkit.toml`. User-facing reference: `devkit schema` for config keys, each command's `-h` for flags, and `plugin/skills/using-devkit/references/` for behavior. `docs/configuration.md` covers setup, and `plugin/skills/using-devkit/references/config.md` covers layering.

## Commands

```sh
cargo nextest run --workspace --no-fail-fast            # test gate
cargo test --workspace --doc                            # doctests (nextest skips them)
cargo clippy --workspace --all-targets -- -D warnings   # zero warnings
devrun task fmt                                         # nightly rustfmt; stable silently ignores rustfmt.toml
```

Run all of them before committing. CI runs them on ubuntu, macos and windows.

Text written for agents (the brief, hook injections, help an agent reads) also has an opt-in eval, since CI cannot judge whether an agent understands it. It needs a logged-in `claude`, and every run is billed:

```sh
devrun task eval --arg eval_case=brief                   # this checkout, 3 runs
evals/run.sh brief main=/path/to/old/devkit new=target/debug/devkit   # compare builds
```

A text case under `evals/` is a `render.sh` that prints the text from a devkit binary, a `preamble.md`, and a `questions.json` answer key. Each question takes its expected answer from real behavior, and `proof` names the test that pins that behavior. The runner asks isolated agents for structured answers and prints a table per label: questions answered correctly, how many of those the text stated outright, confidence, and cost. `EVAL_REPS` and `EVAL_MODEL` override the run count and model. Results, including every spot the agents flagged as ambiguous, land in `target/evals/`.

Scenario evals grade what agents do rather than what they understand. Each run is a headless `claude` session with `plugin/` loaded, its hooks calling that checkout's build, in a fresh repository whose devkit state and git config stay in a scratch directory:

```sh
devrun task eval-scenario                                # every scenario, this checkout, 3 runs each
evals/scenario.sh held-lock main=/path/to/old/checkout new=.   # compare checkouts
```

A scenario under `evals/scenarios/` is a `prompt.md`, an optional `fixture/` copied in as the first commit, optional `setup.sh` and `teardown.sh` run in the repository, and a `scenario.json` holding `max_turns` and the checks. `evals/lib/transcript.jq` documents the check kinds: a tool call, the final reply, or a path the run changed. The table gives each check's pass count per label, how many runs passed every check, and the mean turns, guard denials and seconds, plus total cost. Transcripts land in `target/evals/scenarios/`.

## Layout

The root package is the `devkit` binary (`src/bin/devkit/`, one module per subcommand). `devkitd` (`src/bin/devkitd/`) is the supervisor daemon. `plugin/` is the directory each harness copies on install: its manifests, hooks, skills and `.mcp.json`. Every path a plugin manifest names stays inside it; the marketplace files stay at the repo root.

| Crate | Role |
|---|---|
| `devkit-config` | `devkit.toml` types, layer discovery and merge, JSON Schema |
| `devkit-common` | shared IO: `vcs`, `git`, `config`, `args`, `pool`, `cmd`/`github`, `forge`, `tracker`, `worktree`, `harness`, `caller`, `required`, `harness_log`, `store`, `sys` |
| `devkit-ports` | port registry, app catalog, server lifecycle, tasks, named templates, command guard |
| `devkit-locks` | file-lock registry |
| `devkit-rules` | rule-index matching and context rendering |
| `devkit-command` | IO-free shell-command analyzer (tree-sitter; needs a C compiler) |
| `devkit-issue` | read-only issue and PR triage |
| `devkit-mcp` | stdio MCP server over the facades above |
| `devkit-docs` | version-matched library source checkouts |
| `devkit-vcs` | the `VersionControl` trait: what devkit asks of a project's repository |

## Rules

Each rule's reasoning is documented at the named site. Read it before changing that code.

- **Ports**: reserve before bind, and keep `RESERVATION_GRACE_SECS` above `devrun`'s readiness timeout (`devkit-ports::registry`). Go through the registry facade, with no liveness probing inside `with_lock`.
- **Agents cannot reach other worktrees' servers.** Cross-worktree `devrun down` and all of `devrun reap` require an interactive terminal (`src/bin/devkit/run/mod.rs`). MCP gets no kill action and no cross-holder argument, and its mutating `devrun` actions stay pinned by `assert_own_worktree`.
- **Supervisor**: the supervisor table decides crash vs. stop, and every restart (health probe, memory limit, OOM) goes through the crash path (`src/bin/devkitd/main.rs`, `supervisor.rs`). cgroup setup fails open (`src/bin/devkitd/cgroup.rs`).
- **Launches** whose doppler config resolves to `prd` are refused (`run::assert_not_prd`). Sequence task steps re-resolve right before they run (`task::resolve_step`).
- **Parallel work** goes through `devkit_common::pool`, never rayon's global pool.
- **Deletion needs certainty.** Baseline and worktree classifications are three-valued, and `Unknown` counts as held. The directory lock is taken before a slot lock (`src/bin/devkit/baseline/`).
- **Hooks**: no `hook` verb exits 2, only `pre-tool-use` writes stdout, and logging can never change a verdict (`src/bin/devkit/main.rs`, `hook/`, `harness_log::writer`). The shell guard fails open and its write stage fails closed. A hook invocation resolves one `vcs::Checkout` and passes it down. Payloads are read and answered through the `pabal` crate; `hook/payload.rs` adds only the Cursor spellings it does not model.
- **External tools**: the project's repository is reached through `devkit_common::vcs`, whose backends implement `devkit_vcs::VersionControl`. Only the git backend, third-party checkouts and test fixtures spawn git, through `devkit_common::git::Git`. Every pull-request operation goes through a `forge::Forge`, and repository-scoped `gh` calls go through `cmd::gh_json_in` / `cmd::gh_capture`.
- Match `Role` and `StateKind` exhaustively.

## Conventions

- TDD: the failing test comes first.
- Test scratch comes from `tempfile`, bound for as long as the path is used. Tests that drive `gh` use `tests/common/ghfake.rs`.
- Tests that spawn or reap processes poll for the expected state instead of sleeping.
- `anyhow` with `.context()` for errors.
- Project-specific values come from config, never from code. Tokens resolve through `devkit_common::secrets`.
- Every user-facing verb is a `devkit` subcommand.
- Each fact has one home, and that home is generated or loaded: a config key's meaning is the doc comment on its field (it becomes the schema description), a flag's is its clap help, and behavior an agent needs goes in the skill references. `docs/` restates none of them.
- A config type's `devkit.toml` example is a doctest on that type. `schema/devkit-config.json` is committed; regenerate it with `DEVKIT_UPDATE_SCHEMA=1 cargo test`.
- Help text stays ASCII (see `src/completions.rs`).
- Conventional Commits.

## Worktrees and locks

The primary clone stays on `main`. Every branch lives in its own worktree under `../devkit-worktrees/`, and finished work lands by fast-forwarding `main` from outside that worktree. Several agent sessions share each checkout; the `using-devkit` skill covers the file-lock protocol.
