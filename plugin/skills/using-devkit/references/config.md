# Configuring devkit: `devkit.toml`

`devkit schema` holds every key's name, type, default and behavior. It is generated from the doc comments on the config types, so it always describes the installed binary, and `devkit schema init` points an editor at the same text for hover. This file covers what spans every table: how layers combine, where to read a table's rules, and how to check an edit.

## Layers

- `devkit.toml` at the repository root holds what the project shares. `devkit.local.toml` beside it holds what one machine or checkout needs, overrides its twin, and belongs in `.gitignore`.
- `~/.config/devkit/config.toml` is the base layer under every project.
- Every `devkit.toml` from the filesystem root down to the working directory merges. Tables merge key by key. Scalars and arrays replace wholesale, so a deeper `[[context.files]]` or `[hooks]` list replaces the parent's list.
- A linked worktree also reads its main checkout's two files. The order is the main checkout's `devkit.toml`, the worktree's `devkit.toml`, the main checkout's `devkit.local.toml`, then the worktree's `devkit.local.toml`, so the overrides in the main checkout's `devkit.local.toml` apply in every worktree.
- `[config] root = true` stops the walk at that directory and drops every shallower layer, the home config included. In a linked worktree it keeps the main checkout's files.
- `--config <path>` or `$DEVKIT_CONFIG` selects one file verbatim, with no layering and no home base.
- A devkit child gets the `--config` file of the devkit that starts it. The parent sets `$DEVKIT_PARENT_CONFIG` to the absolute path of the file, and `$DEVKIT_PARENT_CONFIG_CHECKOUT` to the root of its checkout. The children are those that get `$DEVKIT_OPTIONS`. In that checkout, the file outranks `$DEVKIT_CONFIG`. A child that names its own `--config` reads that file. A task whose `env` sets `DEVKIT_CONFIG` does not get the parent's file, so its step reads the file the task names. A child in another checkout ignores the parent's file and resolves its config as with no parent. An empty `$DEVKIT_PARENT_CONFIG_CHECKOUT` limits the file to directories outside every checkout. The parent's file never becomes the global config: the child reads `$DEVKIT_CONFIG` or the home config as the parent does.
- The home config's `[todo]` table is the exception to both: where todos are kept is a fact about the machine, so it stays the base layer under a rooted or explicit config, which can still override it.
- `-o KEY=VALUE` (`--option`) sets one key for one command, as the top layer over the files or `--config`. The value is TOML, so a string keeps its quotes: `-o tracker.kind='"none"'`. `--origin` names `<command line>` as its source. devkit refuses the flag for an agent caller.
- `$DEVKIT_OPTIONS` holds `-o` values, one `KEY=VALUE` per line, as a layer under the command line's `-o` values. A hook reads it, because the harness starts a hook with no `-o` flag. `--origin` names `$DEVKIT_OPTIONS` as its source. A devkit child gets the `-o` values of the devkit that starts it through the variable, so the child resolves the same config. The children are a `devrun task` step, a `devkit pr repro` run, a background todo or ticket sync, and a ticket event. When the parent has `-o` or `$DEVKIT_OPTIONS` values, the variable it passes replaces a `DEVKIT_OPTIONS` entry in a task's `env`. When the parent has no values, the task's entry stays. A relative host path, such as `defaults.worktree_root`, reaches the child anchored as the parent anchors it, so a step in an app directory resolves the same path. A relative repository path, such as `defaults.doppler_yaml`, stays relative, so a child in another checkout resolves that checkout's file. The built-in command rule `devkit-options` refuses an agent command that sets the variable. When `pre-tool-use` cannot read the variable, it ignores the variable and judges the call as with no `-o` values. It writes the error to stderr and to its log record. Every other hook verb fails on the error.

The `[harness]` switches and `[harness.log]` resolve across layers by rules of their own, which their descriptions give.

A renamed key still loads under its old name, read as the new one before layers merge, so a layer using the old name overrides a lower layer using the new one and the reverse. `[issue]` reads as `[ticket]`, `defaults.issue_summary` as `defaults.ticket_summary`, and `[harness.issue_tools]` as `[harness.ticket_tools]`. Each `templates.issue_*` key reads as its `templates.ticket_*` key: `issue_title`, `issue_body`, `issue_summary_path` and `issue_summary`. One layer setting a key under both names is refused. The hooks read `[harness]` apart from the rest of the config, so there a layer with both tool tables keeps `ticket_tools` and reports a warning.

## Before editing a table

Read the table's schema description first. A trap that belongs to one key or table is written there and nowhere else:

```sh
devkit schema | jq -r --arg t tasks '(.properties[$t] | ."$ref" // .additionalProperties."$ref" | ltrimstr("#/$defs/")) as $d | ."$defs"[$d] | .description, (.properties | to_entries[] | "\n\(.key): \(.value.description)")'
```

Swap `tasks` for the table's name. A nested table (`[harness.log]`, `[harness.commands.<name>]`, `[[context.files]]`) is its own entry under `$defs`, and `devkit schema | jq '."$defs" | keys'` lists them.

## Checking an edit

An edit is done when `devkit config --origin` shows each value you set coming from the file you edited. A config that fails to load shows up in `devkit brief`.
