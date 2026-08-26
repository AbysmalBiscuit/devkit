# Single-binary restructure

**Date:** 2026-08-26
**Status:** proposed

## Problem

The package ships eight executables that each link the same workspace
libraries. Measured on this machine at 0.13.3 with `cargo build --release`,
after `strip`:

| binary | size |
|---|---|
| `issue` | 5.9 MB |
| `devkit` | 5.1 MB |
| `devkit-mcp` | 4.9 MB |
| `docm` | 4.2 MB |
| `devrun` | 3.5 MB |
| `portm` | 1.8 MB |
| `lockm` | 1.5 MB |
| `devkitd` | 1.0 MB |

Regenerate with `cargo build --release && ls -l target/release/{issue,devkit,devkit-mcp,docm,devrun,portm,lockm,devkitd}`.

Nearly all of that is the same code repeated. Six dist targets multiply it.

Size is the measurable cost. The one a person feels is discovery. `devkit.toml`
in a checkout names exactly one command, and `devkit --help` today lists only
`auth`, `brief`, `doctor`, `schema`, and `completions`. Nothing in that output
leads to `issue`, `devrun`, `portm`, `lockm`, or `docm`. An agent that does not
load the `using-devkit` skill, and that runs without the MCP server, has no path
from the config file to the tools that read it.

Behind both: eight `--version` outputs that can disagree, and six completion
scripts to install.

## What already works (verified, no change needed)

- **`devkitd` is reached by path, not by name alone.** `devkitd_bin()`
  (`crates/devkit-ports/src/daemon/client.rs:35`) prefers a `devkitd` sibling
  next to `current_exe()` and falls back to PATH.
- **`devkitd install-service` writes `ExecStart` from `current_exe()`**
  (`src/bin/devkitd/service.rs`), so an installed systemd unit names a concrete
  file path that must keep resolving across upgrades.
- **The `daemon` feature already gates `devkitd` via `required-features`**
  (`Cargo.toml`), so `--no-default-features` builds still produce the rest.
- **`hooks/bootstrap-binaries` already runs at every session start**, checks
  `command -v` for the binaries the plugin drives, and runs the cargo-dist
  installer when they are missing. It writes a version stamp under
  `$XDG_STATE_HOME/devkit` and refuses to overwrite binaries it did not install.
- **`issue` and `portm` declare `cmd: Option<Cmd>`.** Bare `issue` runs
  `status::run` (`src/bin/issue/main.rs:410`). That default has to survive.
- **Every CLI already carries `completions <shell>`** via `clap_complete`, each
  passing its own literal name to `generate`.

## Design

### 1. One binary, dispatch on argv[0]

The package builds two executables: `devkit` and `devkitd`.

`devkit` reads the file-name stem of `argv[0]`. When it matches a known shim
name, the process behaves as that tool: the remaining arguments parse against
that tool's clap command, exactly as they do today. Any other stem, `devkit`
included, parses the full `devkit` command.

Dispatch reads `argv[0]` rather than `current_exe()` because the two disagree by
link type. `current_exe()` resolves a symlink to its target and would report
`devkit` for every shim; `argv[0]` carries the name the caller actually invoked
under both hardlinks and symlinks.

An unrecognised stem falls through to `devkit` parsing rather than erroring, so
a renamed or copied binary stays usable.

### 2. Subcommand namespace

Each merged CLI becomes a `devkit` subcommand whose arguments are the existing
`Cli` struct, nested whole. Global flags that today sit before a subcommand
(`issue --dir`, `--config`, `--timing`, `--timing-log`) become that
subcommand's own arguments and keep their spelling.

| shim | subcommand |
|---|---|
| `issue` | `devkit issue` |
| `devrun` | `devkit run` |
| `portm` | `devkit ports` |
| `lockm` | `devkit locks` |
| `docm` | `devkit docs` |
| `devkit-mcp` | `devkit mcp` |

`auth`, `brief`, `doctor`, `schema`, and `completions` stay where they are. None
of the new names collide with them.

`issue` and `portm` keep `Option<Cmd>`, so `devkit issue` with no further
argument runs `status` exactly as bare `issue` does now.

### 3. Shim links

`devkit install-links` creates one link per shim name in the directory holding
the running executable, using `std::fs::hard_link`. A hardlink is a second name
for the same inode: no disk cost, no indirection at exec time, and no symlink
privilege requirement on Windows.

The shim set is `issue`, `devrun`, `portm`, `lockm`, `docm`, `devkit-mcp`. On
Windows each carries the `.exe` extension. `devkitd` is not in the set; see
section 6.

The command prints what it created, what it replaced, and what it skipped, and
exits non-zero if any link failed.

### 4. Replacing an existing multi-binary install

Upgrading from 0.13.x leaves real 0.13.x executables at every shim name in
`CARGO_HOME/bin`. The dist installer places only `devkit` and `devkitd`, so
without intervention the stale copies stay on PATH and keep serving the old
version.

`install-links` therefore replaces an existing file at a shim name, but only
after confirming it is a devkit binary: it runs the candidate with `--version`
and requires the output to name the package. A file at one of those names that
is not a devkit binary is left alone and reported as skipped, so a foreign
`issue` on PATH is never destroyed.

Replacement is remove-then-link, not link-over, because the destination may be a
running executable on Windows.

### 5. Bootstrap hook

`hooks/bootstrap-binaries` changes its presence check from
`devkit lockm devkit-mcp` to `devkit`, and runs `devkit install-links` after a
successful install and after any run where a shim name is missing from PATH.
Failure to link warns and exits 0, matching the hook's existing rule that a
session must start even when everything else fails.

Source builds (`cargo install --path .`) get no hook. `README.md` documents
`devkit install-links` as the second step, and `devkit doctor` grows a row
reporting which shims are present.

### 6. `devkitd` stays a separate binary

`devkitd` keeps its own `[[bin]]` target, its `required-features = ["daemon"]`
gate, and its own file on disk.

Two things depend on that file being real. `devkitd_bin()` finds it as a sibling
of `current_exe()`, and `install-service` writes its path into a systemd unit
that persists across upgrades. Turning it into a shim would mean rewriting both,
and migrating units already on users' disks, for the smallest binary in the set.

The merged set is about 27 MB of the current 28 MB. `devkitd` is what the last
megabyte buys, and it costs a migration.

### 7. Completions

`<shim> completions <shell>` emits a script for that shim's own name, with that
tool's subcommands at the top level, so an installed script keeps working
unchanged. The implementation takes the subcommand's `clap::Command`, renames it
to the shim name, and generates from that as the root.

`devkit completions <shell>` emits the full nested tree.

### 8. Help

`devkit --help` lists the merged subcommands directly, which is the discovery
fix on its own. Below them, `after_help` names the shim spellings, since nothing
in a subcommand list tells a reader that `issue status` also works.

A test enumerates the shim set and asserts every name appears in
`devkit --help`, so a name added later fails the build rather than going
unlisted.

## Non-goals

- Changing any command's own arguments, output, or behavior. This restructures
  where code is reachable from, nothing else.
- Removing the old names. `issue` and the rest keep working, permanently, not as
  a deprecation window.
- Merging `devkitd`.
- Shell aliases or shell detection. Hooks (`lockm hook pretooluse`) and the MCP
  server run from PATH in non-interactive processes that have no aliases.

## Delivery

Ordered so the tree builds and tests pass at every step.

1. `devkit install-links` and its shim-set constant, while the separate binaries
   still exist. Links are created but nothing dispatches yet.
2. argv[0] dispatch in `devkit`, with each merged CLI's `Cli` nested as a
   subcommand. The old `[[bin]]` targets stay, so both paths work.
3. Completions under a shim name.
4. Delete the merged `[[bin]]` sources and rework the four test files that reach
   binaries through `env!("CARGO_BIN_EXE_<name>")`.
5. `hooks/bootstrap-binaries`, `dist-workspace.toml`, `doctor`, `README.md`, and
   the AGENTS.md convention that currently says the operational verbs stay in
   their own binaries.

## Testing

- Dispatch: invoke the built `devkit` through a hardlink at each shim name and
  assert it parses that tool's arguments. Covers the argv[0] path end to end
  rather than the stem-matching function alone.
- Bare-shim defaults: `issue` through a link with no arguments runs `status`.
- `install-links`: creates every shim; replaces a devkit binary at a shim name;
  leaves a non-devkit file at a shim name alone and reports it skipped.
- Completions: each shim's script names that shim, preserving what
  `tests/completions.rs` asserts today.
- Version: every shim reports the package version, preserving
  `tests/cli_version.rs`.
- Help: `devkit --help` names every shim in the set.

`tests/cli_version.rs`, `tests/completions.rs`, `tests/cli_ergonomics.rs`, and
`tests/parity.rs` all resolve binaries via `env!("CARGO_BIN_EXE_<name>")`. Those
variables stop existing for removed targets, so each file moves to a helper that
resolves `CARGO_BIN_EXE_devkit` and invokes it through a link in a `tempfile`
directory.

## Risks

- **A stale binary shadows the new one.** Section 4 is the mitigation; if
  `--version` probing misjudges a file, a user keeps running 0.13.x under a name
  they believe is current. `devkit doctor` reporting shim presence and version is
  what makes that visible.
- **Hardlinks need one filesystem.** `CARGO_HOME/bin` is a single directory, so
  source and destination always share a volume. A packaging layout that splits
  them would break linking; `install-links` reports the error rather than
  falling back to a copy, which would silently restore the size cost.
- **Windows cannot replace a running executable.** Relinking while a supervised
  `devrun` is live fails. The command reports which name it could not replace.
- **Binary size on the merged file.** One binary carries every dependency, so
  `lockm hook pretooluse` on a `PreToolUse` hook now execs a larger image. Only
  touched pages fault in, so the expected effect is nil, but it is worth
  measuring with `hyperfine` before and after on the hook path.

## Integration with `issue sync-includes`

`issue sync-includes` is being built in a separate session and lands first. It
needs nothing from this spec beyond arriving as another subcommand under
`devkit issue`, which the nested-`Cli` approach in section 2 handles without
per-subcommand work. This branch rebases onto it.

## Open questions

1. Should `devkit-mcp` keep a shim, or should `.mcp.json` move to
   `devkit mcp`? Keeping the shim is zero churn for anyone with an existing MCP
   config. Moving is one fewer link and one fewer name.
2. Should `install-links` run automatically on first use of a shim that is
   missing, or stay explicit? Automatic is friendlier and writes to
   `CARGO_HOME/bin` as a side effect of an unrelated command.
3. Does `clap_complete` generate a usable script from a renamed subcommand
   `Command` used as a root? Section 7 assumes yes. Verify before task 3.
