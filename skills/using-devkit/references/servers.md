# Dev servers and ports — `devrun`, `portm`

`devrun` launches and supervises dev servers for one worktree; `portm` is the port registry underneath it. `devrun` usually drives `portm` for you.

## The holder model

A port's **holder is the worktree root path**, not a session token. `registry::holder_alive` is a directory-exists check, so a worktree's ports auto-reclaim when its directory disappears (`git worktree remove`). Get your own holder with `git rev-parse --show-toplevel`; address another worktree by its root path.

## `devrun` — supervised dev servers

```sh
devrun up [apps…] [--role issue|baseline|both] [--env K=V] [--env-file F] [--supervise] [--dry-run]
devrun down [selector] [--role …] [--all|--others|--holder <path>] [--app …] [--older-than 30m]
devrun status [--all]                                 # tracked servers (this worktree, or all)
devrun reap [--all]                                   # kill servers running OUTSIDE devrun (needs a TTY)
devrun logs <app> [--role …] [-f]                     # print or follow one app's log
devkit config [--origin] [--json]                     # resolved config + its layers
devkit config apps [--json]
devkit config tasks [--json]
```

### `up`

Apps you don't name are auto-detected by diffing against the baseline ref, so **on a fresh worktree with no diff yet, name the apps explicitly**. Selecting a webapp pulls in `api` automatically and wires its URL.

Default `--role issue`. `--role both` runs the issue branch and a fresh baseline side-by-side on separate ports for A/B comparison. `--supervise` hands servers to the daemon so they restart on crash. `--dry-run` prints the launch plan without starting.

Ports are allocated dynamically from the live registry at start time — `issue setup` reserves none.

`up` is idempotent: an (app, role) row that already has a live pid reports the existing server instead of spawning a duplicate.

### `down`

Stops servers **and releases their ports** (prints `released ports {…}`). Defaults to this worktree only.

| Command | Effect |
|---|---|
| `devrun down` | stop + release everything in this worktree |
| `devrun down --role baseline` | this worktree, baseline only |
| `devrun down api` | this worktree, fuzzy-match `api` |
| `devrun down --all` | every server, every worktree (one prompt) |
| `devrun down --others` | every server in every *other* worktree |
| `devrun down --holder ../wt/feat-x` | one specific worktree |

Reaching another worktree needs an explicit scope flag (`--all`/`--others`/`--holder`) **and** an interactive terminal to confirm. An agent has no PTY, so it cannot stop another worktree's servers.

A bare positional selector substring-matches across holder, app, port, role, and pid, and is mutually exclusive with the column filters: `--app`, `--port`, `--role`, `--pid`, `--listening`/`--not-listening`, and `--older-than` (`90s`/`30m`/`2h`/`1d`). `--batch` collapses cross-worktree confirmation into one prompt.

### `reap`

Kills dev servers running *outside* the registry — started by hand, or orphaned. It always requires an interactive terminal and has no `--yes`/`--force` bypass, so an agent cannot run it. Ask the user to reap.

Agents get detection only: the untracked section of `devrun status`, `devkit doctor`'s `devrun_strays` row, and the `ports.strays` MCP action.

## `portm` — port registry

A reservation row is written *before* any process binds, which is what stops two concurrent callers grabbing the same port.

```sh
portm status                                          # reserved/live ports (this project, every worktree)
portm alloc <apps…> [--holder <path>] [--role issue|baseline]    # alias: reserve
portm release [apps…] [--holder <path>] [--role …]    # no apps = everything the holder has
portm prune                                           # drop stale reservations
```

- `--holder` defaults to the current worktree's root. Pass it only to act on another worktree.
- `release` frees reservations in the registry; it does not stop processes. `devrun down` stops *and* releases.
- `portm status` covers the current project only. There is no cross-project flag.

## `devkitd`

The background daemon owning the port and lock registries. `portm` and `devrun` start it automatically; you rarely invoke it directly.
