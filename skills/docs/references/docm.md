# `docm`: how checkouts are resolved and kept

Checkouts live under `~/.local/share/devkit/docs/`, named for the exact ref they hold rather than a bare version (`h3/v1.15.11`, `openapi-ts/@hey-api~client-fetch@0.13.1`, where `/` encodes as `~`). The global manifest is `~/.config/devkit/docs.toml`; a project's `[[docs.libs]]` entries in `devkit.toml` override it field by field.

```sh
docm add tokio                    # registry lookup (crates.io/npm/PyPI)
docm add https://github.com/godotengine/godot --ref 4.3-stable
docm add react --project          # write to this repo's devkit.toml [docs]
docm add zod --eco js             # name the ecosystem instead of probing for it
docm add h3 --src-dir src --docs-dir docs   # override the detected layout
docm add typescript-go --exclude testdata/  # leave bulky paths out of every checkout
docm list --project               # what this checkout evidences; --json emits {pins, dropped}
docm list --refresh               # re-measure every checkout's size
docm sync                         # fetch, re-resolve, re-materialize, verify
docm rm tokio                     # drop from the manifest (aliases: remove, delete)
docm forget tokio                 # release this project's reference to it
docm prune                        # drop checkouts no live project references
```

## Version resolution

A manual `ref` pin wins. Otherwise the requesting workspace's own dependency graph (`Cargo.lock`, `pnpm-lock.yaml`, `package-lock.json`, `bun.lock`, `uv.lock`) is matched against the repository's git tags.

Only a registry install resolves this way, because a version number identifies upstream's code only when the lockfile says it came from the registry the repository publishes to. A git, path, workspace, link or archive dependency is refused by name and needs `--ref`. A remote tarball is judged by the spec that declares it, since npm records the same `resolved` URL for a tarball from the registry host as for an ordinary range.

When nothing pins a version (no matching tag, no importer manifest, an ambiguous ecosystem, a lockfile that contradicts itself), `docm` fails with the cause and the fix. `--allow-default-branch` on `add`, `sync`, `path` or `info` opts into the default branch for one run.

## References and pruning

Resolving a library from a project records a reference: the project root, the library, and the checkout it received. A reference holds that checkout against `docm prune` and keeps the library in `docm list --project` even when nothing declares it any more, which is how a `--ref` pin or a dropped library keeps showing. `docm forget <lib>` releases the reference and leaves the checkout for `prune`.

## Checkout sizes

Each checkout is measured when it is materialized or re-pointed, and the size is recorded beside its commit in the library's `meta.toml`. `docm list` shows it and `devkit doctor` reads it. A cache from a docm that recorded no sizes fills them in on its next `docm` command. `docm path` and `docm info` re-resolve on every call, so they never measure. A checkout that grows after its pin (ignored build output, say) goes unnoticed until `docm list --refresh`, which re-measures the whole shared cache, libraries outside the current manifest included.

## Disk use

`exclude` in a global manifest entry (`docm add --exclude <pattern>`, repeatable) takes gitignore-style patterns for paths no checkout of that library holds. Their content is never downloaded. Changing the list re-applies it to existing checkouts on their next resolve. A project's `[[docs.libs]]` cannot set it, because every project shares the same checkouts.

## Reserved names

A library or ref name cannot collide with the cache's control files: `registry` (and anything starting `registry.`) at the cache root, `manifest` for the manifest lock, and `repo.git`/`meta.toml` inside each library's directory. Register such a package under another name with `docm add <other-name> --package <package>`. A library already registered under a reserved name cannot be removed with `docm rm`: delete its manifest entry and its cache directory by hand.

## Caches from devkit 0.12.x

The first `docm` command against a 0.12.x cache migrates its layout: nested scoped directories (`@scope/pkg/`) are renamed to the new encoding and their worktrees repaired, and legacy entries keep protecting their checkout until the library re-resolves. `docm prune` then reclaims what the migration leaves, including retired `default` checkouts.

A 0.12.x `meta.toml` is not migrated, because three of its tag patterns (`name-dash`, `name-dash-v`, `name-at`) no longer parse and guessing would serve the wrong tag. Every `docm` command fails naming each such file; delete them and run `docm` again. Everything they held is re-derived.
