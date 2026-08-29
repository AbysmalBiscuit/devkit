# Installing devkit

## Prebuilt binaries

No Rust toolchain needed. The [dist](https://opensource.axo.dev/cargo-dist/)-generated installer downloads the matching binaries from the latest GitHub release, verifies checksums, and puts them on your `PATH`.

```sh
# Linux / macOS / WSL
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/AbysmalBiscuit/devkit/releases/latest/download/devkit-installer.sh | sh
```

```powershell
# Windows (PowerShell)
irm https://github.com/AbysmalBiscuit/devkit/releases/latest/download/devkit-installer.ps1 | iex
```

Pin a specific release by swapping `latest/download` for `download/v0.8.0`. Prebuilt targets are Linux x86_64 (gnu + musl) and arm64, macOS x86_64 and arm64, and Windows x86_64 and arm64. Upgrade in place later with `devkit-update`.

The installer places only the binaries. To use devkit inside a coding agent, register the plugin afterward. See [agents.md](agents.md).

## From source

Install `devkit` and `devkitd` into `~/.cargo/bin` with one command, from a clone:

```sh
cargo install --path .
```

or straight from GitHub without cloning:

```sh
cargo install --git https://github.com/AbysmalBiscuit/devkit --force
```

This builds with default features, which include the `devkitd` supervisor daemon. `devkitd` serves both the port registry (`ports.sock`) and the lock registry (`locks.sock`) from memory, writing through to the files, and is used by `devrun up --supervise`. To skip the daemon, build a lean set with `--no-default-features`, which omits `devkitd` and `devrun`'s `--supervise` support.

Or just build into `target/release` without installing:

```sh
cargo build --release
```

## Old-name links

`devkit` bundles the whole CLI surface as subcommands, and installs the old names (`portm`, `devrun`, `issue`, `lockm`, `docm`, `devkit-mcp`) beside itself as hardlinks, so `docm list` and `devkit docs list` are the same command. `devkit --help` ends with a block mapping every old name to its subcommand.

- Running `devkit` at all is enough: every invocation links the old names beside the executable it ran from. A `target/release` after a source build, or the directory a release archive was unpacked into, gains all six names on the first command you run there, `devkit --help` included.
- `devkit install-links` does the same pass explicitly, next to whichever `devkit` executable you run it from.
- A name already occupied by something devkit cannot identify as itself is reported as skipped and left alone. `--force` claims such a name anyway, deleting whatever holds it, an unrelated tool of the same name included. That is what the flag is for, and the reason it is not the default.
- Upgrading from 0.13.x: the separate `portm`, `devrun`, `issue`, `lockm`, `docm` and `devkit-mcp` binaries predate the identity probe devkit recognizes itself by, so they are reported skipped too. `devkit install-links --force` claims them; each name is then a hardlink carrying the probe, and later upgrades need no flag.
- The links refresh on their own: any `devkit` invocation relinks when the binary's version, directory, or modification time stops matching the recorded stamp, so an upgrade or a move needs no manual step. A pass that could not finish, whether a name it failed to create or probes slow enough to reach the pass's own deadline, is retried by a later invocation instead of being recorded as done.
- `devkit doctor` reports a row per name: which are linked to the running binary, what version each of the others resolves to, and which command claims it.
- Set `DEVKIT_SKIP_AUTOLINK` (to any value) to turn that automatic pass off. That is the opt-out for a packager who manages the links itself, or for anything that must not write the state directory.

## State and cache locations

| Data | Path |
|---|---|
| Port registry | `~/.local/state/devkit/ports.json` |
| Server logs | `~/.local/state/devkit/logs/` |
| File-lock registry | `~/.local/state/devkit/locks.json` |
| PR diff cache (`issue prs`) | `$XDG_CACHE_HOME/devkit/pr-status/` (or `~/.cache/devkit/pr-status/`) |
| Docs library manifest | `~/.config/devkit/docs.toml` |
| Docs library checkouts | `$XDG_DATA_HOME/devkit/docs/` (or `~/.local/share/devkit/docs/`) |

The state home honors `$XDG_STATE_HOME` (default `~/.local/state`). A legacy `~/.claude/state/devkit` home is migrated automatically on first run.
