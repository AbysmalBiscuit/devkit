# devkit

A Rust workspace (edition 2024) that coordinates many concurrent local dev sessions, human and agent, on one machine: port and file-lock registries, a dev-server supervisor, issue worktrees, and the hooks coding agents call. The engine is project-agnostic; everything project-specific lives in `devkit.toml`. User-facing reference: `docs/commands.md` and `docs/configuration.md`.

## Commands

```sh
cargo nextest run --workspace --no-fail-fast            # test gate
cargo test --workspace --doc                            # doctests (nextest skips them)
cargo clippy --workspace --all-targets -- -D warnings   # zero warnings
devrun task fmt                                         # nightly rustfmt; stable silently ignores rustfmt.toml
```

Run all of them before committing. CI runs them on ubuntu, macos and windows.

## Layout

The root package is the `devkit` binary (`src/bin/devkit/`, one module per subcommand). `devkitd` (`src/bin/devkitd/`) is the supervisor daemon.

| Crate | Role |
|---|---|
| `devkit-config` | `devkit.toml` types, layer discovery and merge, JSON Schema |
| `devkit-common` | shared IO: `git`, `config`, `pool`, `cmd`/`github`, `tracker`, `worktree`, `harness`, `caller`, `required`, `harness_log`, `store`, `sys` |
| `devkit-ports` | port registry, app catalog, server lifecycle, tasks, command guard |
| `devkit-locks` | file-lock registry |
| `devkit-rules` | rule-index matching and context rendering |
| `devkit-command` | IO-free shell-command analyzer (tree-sitter; needs a C compiler) |
| `devkit-issue` | read-only issue and PR triage |
| `devkit-mcp` | stdio MCP server over the facades above |
| `devkit-docs` | version-matched library source checkouts |

## Rules

Each rule's reasoning is documented at the named site. Read it before changing that code.

- **Ports**: reserve before bind, and keep `RESERVATION_GRACE_SECS` above `devrun`'s readiness timeout (`devkit-ports::registry`). Go through the registry facade, with no liveness probing inside `with_lock`.
- **Agents cannot reach other worktrees' servers.** Cross-worktree `devrun down` and all of `devrun reap` require an interactive terminal (`src/bin/devkit/run/mod.rs`). MCP gets no kill action and no cross-holder argument, and its mutating `devrun` actions stay pinned by `assert_own_worktree`.
- **Supervisor**: the supervisor table decides crash vs. stop, and every restart (health probe, memory limit, OOM) goes through the crash path (`src/bin/devkitd/main.rs`, `supervisor.rs`). cgroup setup fails open (`src/bin/devkitd/cgroup.rs`).
- **Launches** whose doppler config resolves to `prd` are refused (`run::assert_not_prd`). Sequence task steps re-resolve right before they run (`task::resolve_step`).
- **Parallel work** goes through `devkit_common::pool`, never rayon's global pool.
- **Deletion needs certainty.** Baseline and worktree classifications are three-valued, and `Unknown` counts as held. The directory lock is taken before a slot lock (`src/bin/devkit/baseline/`).
- **Hooks**: no `hook` verb exits 2, only `pre-tool-use` writes stdout, and logging can never change a verdict (`src/bin/devkit/main.rs`, `hook/`, `harness_log::writer`). The shell guard fails open and its write stage fails closed. A hook invocation resolves one `git::Checkout` and passes it down.
- **External tools**: git goes through `devkit_common::git`, and repository-scoped `gh` calls go through `cmd::gh_json_in` / `cmd::gh_capture`.
- Match `Role` and `StateKind` exhaustively.

## Conventions

- TDD: the failing test comes first.
- Test scratch comes from `tempfile`, bound for as long as the path is used. Tests that drive `gh` use `tests/common/ghfake.rs`.
- Tests that spawn or reap processes poll for the expected state instead of sleeping.
- `anyhow` with `.context()` for errors.
- Project-specific values come from config, never from code. Tokens resolve through `devkit_common::secrets`.
- Every user-facing verb is a `devkit` subcommand.
- A config type's `devkit.toml` example is a doctest on that type. `schema/devkit-config.json` is committed; regenerate it with `DEVKIT_UPDATE_SCHEMA=1 cargo test`.
- Help text stays ASCII (see `src/completions.rs`).
- Conventional Commits.

## Worktrees and locks

The primary clone stays on `main`. Every branch lives in its own worktree under `../devkit-worktrees/`, and finished work lands by fast-forwarding `main` from outside that worktree. Several agent sessions share each checkout; the `using-devkit` skill covers the file-lock protocol.
