# Configuring devkit: `devkit.toml`

`devkit schema` holds every key's name, type, default and behavior. It is generated from the doc comments on the config types, so it always describes the installed binary, and `devkit schema init` points an editor at the same text for hover. This file covers what spans every table: how layers combine, where to read a table's rules, and how to check an edit.

## Layers

- `devkit.toml` at the repository root holds what the project shares. `devkit.local.toml` beside it holds what one machine or checkout needs, overrides its twin, and belongs in `.gitignore`.
- `~/.config/devkit/config.toml` is the base layer under every project.
- Every `devkit.toml` from the filesystem root down to the working directory merges. Tables merge key by key. Scalars and arrays replace wholesale, so a deeper `[[context.files]]` or `[hooks]` list replaces the parent's list.
- `[config] root = true` stops the walk at that directory and drops every shallower layer, the home config included.
- `--config <path>` or `$DEVKIT_CONFIG` selects one file verbatim, with no layering and no home base.

The `[harness]` switches and `[harness.log]` resolve across layers by rules of their own, which their descriptions give.

## Before editing a table

Read the table's schema description first. A trap that belongs to one key or table is written there and nowhere else:

```sh
devkit schema | jq -r --arg t tasks '(.properties[$t] | ."$ref" // .additionalProperties."$ref" | ltrimstr("#/$defs/")) as $d | ."$defs"[$d] | .description, (.properties | to_entries[] | "\n\(.key): \(.value.description)")'
```

Swap `tasks` for the table's name. A nested table (`[harness.log]`, `[harness.commands.<name>]`, `[[context.files]]`) is its own entry under `$defs`, and `devkit schema | jq '."$defs" | keys'` lists them.

## Checking an edit

An edit is done when `devkit config --origin` shows each value you set coming from the file you edited. A config that fails to load shows up in `devkit brief`.
