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
devkit config tasks [name] [--json]
devkit config variables [--json]
```

### `up`

Apps you don't name are auto-detected by diffing `<baseline ref>...HEAD`, so **on a fresh worktree with no diff yet, name the apps explicitly**. Selecting an app with a `url_env` pulls in the `provides_url` app automatically and wires its URL into that env var.

Default `--role issue`. `--role both` runs the issue branch and its baseline side-by-side on separate ports for A/B comparison. The baseline is the worktree at the merge base with the baseline ref, shared by every worktree cut from the same commit and created the first time it is asked for. `--supervise` hands servers to the daemon so they restart on crash within the crash-loop budget. `--dry-run` prints the launch plan without starting; under `--role baseline` it names the directory a real run would use without creating it.

Ports are allocated dynamically from the live registry at start time — `issue setup` reserves none.

`up` is idempotent: an (app, role) row that already has a live pid reports the existing server instead of spawning a duplicate.

### `status`

Lists tracked servers for this worktree (`--all` for every worktree), each with its rendered URL. Below them, an **untracked** section lists dev servers listening in a configured app's port band, or matching an app's launch signature, that the registry does not own: started outside `devrun up`. Read-only.

### `down`

Stops servers **and releases their ports** (prints `released ports {...}`). Defaults to this worktree plus the baseline it alone references; a baseline no other worktree names is this worktree's own.

| Command | Effect |
|---|---|
| `devrun down` | stop + release everything in this worktree and its own baseline |
| `devrun down --role baseline` | this worktree's own baseline only |
| `devrun down api` | this worktree and its own baseline, fuzzy-match `api` |
| `devrun down --all` | every server, every worktree (one prompt) |
| `devrun down --others` | every server in every *other* worktree |
| `devrun down --holder ../wt/feat-x` | one specific worktree |

Reaching another worktree, or a baseline another worktree also names, needs an explicit scope flag (`--all`/`--others`/`--holder`) **and** an interactive terminal to confirm. An agent has no PTY, so it cannot stop another worktree's servers.

A bare positional selector substring-matches across holder, app, port, role, and pid, and is mutually exclusive with the column filters: `--app`, `--port`, `--role`, `--pid`, `--listening`/`--not-listening`, and `--older-than` (`90s`/`30m`/`2h`/`1d`, a bare number is seconds). `--all`/`--batch` collapse cross-worktree confirmation into one prompt.

### `baseline list` and `baseline prune`

`devrun baseline list` enumerates the baseline directory on disk rather than asking git, so a tree git has lost track of still shows. Its state column reads `registered` (git knows it), `orphaned` (marked, but this repository has no registration), `unmarked` (no `.devkit/baseline.toml`, not devkit's to touch) or `unreadable` (a marker that can be neither read nor ruled out). When some worktree's `.devkit/issue.toml` cannot be read it says so on stderr: no baseline is provably unreferenced then, and a sweep reclaims nothing.

`devrun baseline prune` removes every baseline no worktree names, one pass under the baseline directory's lock. A `registered` one goes through `git worktree remove`; an `orphaned` one is deleted as a plain directory only once nothing stands behind its `.git`. It refuses a baseline with running servers (`--force` waives), with modified tracked files (`--discard-edits` waives; untracked prep files and installed dependencies never count), one that is `git worktree lock`ed, and the directory you are standing in. The two waivers are separate so that getting past an edit never also switches off the running-servers gate. One refusal does not stop the sweep, and a run that refused any exits non-zero. `--dry-run` runs every gate on the same inputs and removes nothing, and takes no per-slot lock.

### `reap`

Kills dev servers running *outside* the registry — started by hand, or orphaned. It always requires an interactive terminal and has no `--yes`/`--force` bypass, so an agent cannot run it. Ask the user to reap.

Agents get detection only: the untracked section of `devrun status`, `devkit doctor`'s `devrun_strays` row, and the `ports.strays` MCP action.

## `portm` — port registry

A reservation row is written *before* any process binds, which is what stops two concurrent callers grabbing the same port. The registry lives in `~/.local/state/devkit/ports.json`.

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
