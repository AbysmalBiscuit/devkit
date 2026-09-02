# Help verbosity design

## Goal

Give `devkit` two help views instead of one, and route each to the reader it
suits. A person at a terminal gets the short list of subcommands they get
today. A coding agent, which reaches devkit through a pipe, gets the whole
command tree in a single call.

Today all three spellings (`devkit -h`, `devkit --help`, `devkit help`) print
byte-identical output: one line per top-level subcommand, no descent. An agent
that wants to know `issue review request` or `docs prune` exists has to run a
second `--help` per group, or guess, or fall back to searching the repository.

## Scope

**In:** a full-tree help renderer; the clap-resolved intercept that routes to
it; the verbosity decision (explicit flag, then env override, then stdout TTY);
a `--full` flag on the `help` subcommand form; `about` strings shortened to
phrases with their prose moved to `long_about`; clap's `wrap_help` feature; and
dropping the `help` subtree from generated completion scripts.

### Non-goals

- **A machine-readable dump** (JSON of the tree with flags and types). Nothing
  consumes it, and the MCP server already gives agents a typed surface for the
  actions it exposes. Add one when something asks for it.
- **A man page.** Agents read `AGENTS.md` and the `using-devkit` skill for
  "which verb for which situation" prose. That guidance belongs there, not in
  the CLI.
- **A global `--help-full` flag.** Threading an extra flag through every
  subcommand costs more clap plumbing than it earns. `help --full` already
  reaches the same renderer, and it works under the shim names too.
- **Sniffing agent environment variables** (`CLAUDECODE`, `CURSOR_*`,
  `CODEX_*`). Each new harness would need a devkit release to be recognized.
  Absence of a TTY is the property that actually matters and is true of every
  agent, present and future.
- **Any change to leaf `--help`.** A command with no subcommands keeps clap's
  rendering: flags, arguments, defaults, gates. The tree cannot carry those and
  does not try.
- **Cascading.** `devkit issue --help` renders the `issue` subtree only, never
  its siblings. A group shows what is under it and stops.
- **Any MCP surface change.**

`-h` is a near-non-goal worth stating precisely rather than listing above. It
stays terse under every condition, outranks every other signal, and keeps the
shim footer. Its *text* does change: shortening `about` moves prose into
`long_about`, and `-h` renders the short form. `devkit auth -h` loses the
parenthetical it carries today, which `devkit auth --help` then gains.

## Background

### Where the idea came from

The full tree was first seen by accident, on a machine running nushell.
`devkit completions nushell` emits one `export extern "<full path>" [...]`
declaration per node of clap's command tree, each carrying that node's clap
`about` string as its description. Nushell has no subcommand nesting; a
declaration named `"devkit docs add"` is a single command whose name contains
spaces. Once the script is sourced, nushell owns the `devkit` name and answers
`--help` from its own registry without executing the binary, listing every
declaration in scope whose name starts with `devkit `, alphabetically, at any
depth.

Two consequences worth recording. The rendering is not a tree walk but a flat
prefix scan, which is why an orphan declaration whose parent was never declared
still appears in the list. And a large share of the declarations are the `help`
subtree that clap auto-generates at every level, so `devkit help docs list` is
listed beside `devkit docs list` and the output reads as though everything
appears twice.

So the artifact people liked is devkit's own help text, rendered by a shell,
reachable only where that script is sourced. This design puts it in the binary,
ordered sensibly, without the duplication.

### What the code already has

`shim_command(subcommand, shim_name)` builds a shim's `clap::Command` rooted
under its own name, which is why `portm --help` already prints
`Usage: portm [OPTIONS] [COMMAND]` rather than naming `devkit ports`.
`Cli::command()` gives the `devkit` root.

`devkit_common::ui::stdout_is_tty()` already exists and is already the signal
devkit uses to decide between a rendering for a reader and output for a caller.

`Cargo.toml` builds clap with `default-features = false` and no `wrap_help`, so
no help text wraps at all. The longest `about` line today runs past two hundred
characters, which an eighty-column terminal soft-wraps into a paragraph blob.
That is a defect in its own right and the reason the current help reads as a
wall.

## Design

### The two views

| Spelling | On a TTY | Not on a TTY |
|---|---|---|
| `<cmd> -h` | terse | terse |
| `<cmd> --help` | terse | full tree |
| `<cmd> help` | terse | full tree |
| `<cmd> help --full` | full tree | full tree |

**Terse** means clap's own rendering at the resolved node, whatever that is
after the `about` and `long_about` changes below. It is not byte-identical to
today's output: moving prose into `long_about` gives clap's `--help` a long
description block at the root and long help on flags, where `-h` keeps the
short forms. The subcommand *list* is unaffected, because clap renders each
child's `about` in a parent's listing under both spellings.

**Full tree** is the new renderer.

`help` matching `--help` is deliberate: the two spellings mean the same request
and should not disagree.

`--full` is accepted anywhere in a help request, not only after `help`.
`devkit --help --full` and `devkit help --full` both force the tree. The
documented spelling is `help --full`; accepting the other costs nothing and
rejecting it would need machinery that buys no safety.

### The verbosity decision

One pure function owns it, so the rule is unit-testable without a pseudo
terminal:

```rust
pub enum Verbosity { Terse, Full }

pub struct Decision {
    pub verbosity: Verbosity,
    /// Set when `DEVKIT_HELP` held a value that is neither `terse` nor `full`.
    /// The caller prints it; keeping it out of this function is what keeps the
    /// function pure and its tests free of captured output.
    pub warning: Option<String>,
}

/// Precedence: an explicit `--full`, then `DEVKIT_HELP`, then whether stdout
/// is a terminal. An unrecognized `DEVKIT_HELP` value falls through to the TTY
/// signal rather than failing a help request.
pub fn decide(full_flag: bool, env: Option<&str>, stdout_is_tty: bool) -> Decision
```

`DEVKIT_HELP` takes `terse` or `full`. It exists for two reasons: a person who
wants deterministic output regardless of pipes, and the integration tests,
which cannot get a TTY under `cargo nextest`.

`-h` sits above all three and is handled before `decide` is called.

### Resolving the target node

Walking argv by hand cannot work. The walker would have to know which flags
take values, and `AGENTS.md` documents `issue -C ~/git/acme/app status` as a
supported shape: a hand-walker skips `-C`, meets the bare path token, fails to
match it against a subcommand, and stops at the `issue` root, rendering the
`issue` tree for what was a request about `issue status`. The same class of
error covers `--` separators, short-flag clusters (`-hC review`), aliases
(`docm remove`), and `--help=full`. That table of grammar would go stale on the
next flag anyone adds. clap already knows all of it, so clap resolves the node.

Resolution runs on a **probe: a clone of the command tree, built only to answer
"which node, and was help asked for". It is discarded afterwards.** The real
parse that follows is untouched.

The probe is built by walking the tree recursively and, at every node:

- `disable_help_flag(true)`, so clap does not consume `-h`/`--help` and exit.
- `disable_help_subcommand(true)`, replaced by an explicit one below.
- Add three **non-global** `SetTrue` args: `--help`, `-h`, `--full`.
- `subcommand_required(false)` and `mut_args(|a| a.required(false))`.
- Only on a node that already has at least one subcommand, add an explicit
  `help` subcommand taking trailing `Vec<String>` positionals plus the same
  three args.

`disable_help_flag`, `disable_help_subcommand` and `ignore_errors` are each
global settings in clap, verified in
`clap_builder-4.6.6/src/builder/command.rs` where all three call
`global_setting`. The recursive walk is still needed for the per-node args and
the explicit `help` subcommand, which have no global form.

Three of those bullets are load-bearing and each fixes a specific failure:

**Non-global help args, not global ones.** A global arg propagates its value
down the whole chain, which erases *which level* asked for help. Per-node args
preserve it, and that is what gives first-help-wins: `devkit issue --help
review` sets help at the `issue` level, so the target is `issue`, matching what
clap prints today. A global arg would resolve to `review` and make the full
view disagree with the terse view for the same argv.

**Clearing `required` and `subcommand_required` instead of `ignore_errors`.**
`ignore_errors(true)` would tolerate *every* parse error, so
`devkit issue typo --help` would resolve to `issue`, render the tree, and exit
zero, when clap rejects the unrecognized subcommand today. Turning invalid argv
into successful output is worse than the problem being solved. Clearing
required args and required subcommands removes the only error class a help
request legitimately produces (`devkit issue setup --help`, where `--slug` is
required), and leaves genuine errors as errors.

**An explicit `help` subcommand only where clap would have generated one.**
clap auto-generates `help` for nodes that have subcommands and not for leaves,
so today `devkit docs add help` registers a library named `help`. Adding a
`help` subcommand to every node would swallow that positional. Matching clap's
rule preserves it.

That explicit `help` subcommand is what makes the `help` spellings work without
a token scan. `devkit help docs list` parses as the `help` subcommand carrying
positionals `["docs", "list"]`, and `devkit issue help status` parses the same
way one level down, so nested `help` needs no special case. Scanning argv for a
bare `help` token instead would eat the `docs add help` positional, the same
class of bug as the hand-walker.

The algorithm:

1. Build the probe from `Cli::command()` or `shim_command(...)` per argv[0].
2. `probe.try_get_matches_from(args)`. On `Err`, decline: return and let the
   real parse report the error.
3. Walk the subcommand chain from the root, reading each level's own `--help`,
   `-h` and `--full`.
4. If any level set `-h`, the view is terse. Return and let the real parse
   render it. `-h` outranks the TTY signal, `DEVKIT_HELP`, and `--full`.
5. The request is a help request when some level set `--help`, or the `help`
   subcommand was used. If neither, return.
6. The target node is the **shallowest** level whose own `--help` or `-h` was
   set. For the `help` subcommand form, it is the node its positionals name,
   resolved against the real tree; positionals that name nothing resolve to the
   node carrying the `help`. When both appear, the flag wins:
   `devkit --help help issue` targets the root, the same first-help-wins rule
   clap applies to `devkit --help issue`.
7. `--full` is set if any level set it. Resolve verbosity via `decide`.
8. On `Terse`, return without consuming anything and let the real parse render
   help the way it does today.
9. On `Full`, print and exit: the tree when the target node has subcommands,
   and that node's clap long help when it is a leaf.

Two details the walk in step 6 depends on. The real tree must be **built**
before a node is taken out of it: `Command::build` is what assigns each
subcommand its `devkit issue status` usage name and copies a parent's
`global(true)` arguments down, so a node cloned out of an unbuilt tree prints
`Usage: status [IDS]...` and silently drops `-C`, `--config` and `--timing`.
And clearing `required` on the probe is not enough on its own, because
`issue setup`'s positional is `required_unless_present`, a separate condition
clap evaluates independently.

Writing the output treats a broken pipe as the reader being done rather than as
a failure, which is what `completions::emit` already does. Otherwise
`devkit --help | head` exits non-zero.

Step 9's leaf branch is load-bearing. `devkit help docs add --full` asks for
full output about a leaf; falling through would hand `--full` to the real
parse's `help` subcommand, which rejects it as an unexpected argument. Printing
the leaf's long help directly answers the request and keeps `--full` away from
the real parse.

That generalizes to an invariant worth stating: **whenever `--full` appears in
a help request, this intercept handles it and exits, so the real parse never
sees `--full`.** When `--full` appears outside a help request
(`devkit docs list --full`), the intercept declines, the real parse runs, and
clap reports the unexpected argument. That is the correct outcome.

### Where the intercept goes

**After `links::ensure_current`, immediately before `Cli::parse()` and
`dispatch_shim`.** Not beside the `shim::PROBE_FLAG` intercept at the top of
`main()`, which returns before the panic hook, the state migration, and the
automatic linking pass.

`docs/install.md` promises that running devkit at all creates the six shim
hardlinks beside the executable, and names `devkit --help` as an invocation
that does it. Placing the help intercept next to the probe intercept would
silently break that for exactly the command the documentation cites.

### The renderer

New module `src/help.rs`, beside `src/completions.rs`, so integration tests can
call it directly:

```rust
/// Render `cmd` and every subcommand under it, one line per node, as
/// `<full path>  <about>`. `path` is the invoked path of `cmd` itself, which
/// is what roots a shim's tree at the name the caller typed.
pub fn tree(cmd: &clap::Command, path: &str, out: &mut dyn Write) -> io::Result<()>
```

Rules:

- **The root gets its own line**, first, before its children. It answers "what
  is this" before "what can it do", and it is what keeps
  `tests/shim_dispatch.rs` passing for the right reason rather than by being
  pinned.
- **Paths are rooted at the invoked name.** Under `docm`, nodes read
  `docm add`, not `add` and not `devkit docs add`. This is the existing
  `shim_command` rule applied to the tree: what the reader typed is what the
  reader can copy and run.
- **Depth-first in declaration order**, not alphabetical. A group is
  immediately followed by its own children, so the shape of the CLI survives
  the flattening. Alphabetical order is what made the nushell rendering hard to
  read.
- **Skip `help` nodes and hidden nodes.** The `help` subtree is noise, and a
  node clap hides is hidden for a reason.
- **One line per node, never wrapped.** Paths are padded to the longest path in
  the rendered set, computed at render time. A line longer than the cap is
  truncated with a trailing ASCII `...`, never `…`: help text reaches the
  PowerShell completion scripts verbatim, and `AGENTS.md` records that Windows
  PowerShell 5.1 reads a BOM-less UTF-8 `.ps1` as cp1252, where the trailing
  byte of an ellipsis becomes a quote character that closes a string early.
- **Cap at one hundred columns.** Fixed, not terminal-derived: the tree's
  primary reader is a pipe, and a fixed width makes the output deterministic
  and the test that asserts it meaningful.
- **The shim footer is appended**, the same block `-h` carries. The tree says
  `devkit docs add`; the footer is the only thing telling a reader that
  `docm add` is the same command, so the tree needs it at least as much as the
  terse view does.

Truncation is a safety net, not the mechanism. The `about` cap below is what
actually keeps lines short.

### `about` is a phrase, prose is `long_about`

Every `about` string becomes a phrase of at most seventy characters. The prose
that some of them carry today moves into `long_about`, which in a derive doc
comment means putting it after a blank line.

Seventy is chosen against the layout: the longest command path plus the
two-space gutter, subtracted from the hundred-column cap, leaves enough room
that no node truncates today. A test enforces the cap across the whole tree, so
a future long `about` fails the build rather than silently truncating.

Nothing is lost from `--help`. `devkit auth --help` still prints the full
explanation, because `long_about` is what clap renders there. `-h` is where the
prose disappears, which is the intended trade.

Enabling clap's `wrap_help` feature in the workspace `Cargo.toml` is part of
this: the terse view should wrap to terminal width, which it currently cannot
do at all.

### Completions drop the `help` subtree

`emit` in `src/completions.rs` calls `cmd.build()` on a command it owns by
value. `help` subcommands do not exist before that call, and clap exposes no
way to remove a subcommand after it. So the change is
`cmd.disable_help_subcommand(true)` on that local clone, immediately before
`build()`, not a post-build filter. `disable_help_subcommand` is a global
setting, so one call covers every level.

Placing it inside `emit` covers every generated script, including the separate
`devkit` and shim clones `every_completion_script` builds. Hiding the nodes
instead of removing them would not be enough: the fish generator already emits
hidden nodes, `locks hook` among them.

Because the clone is local to `emit`, runtime behavior is untouched:
`devkit help docs` keeps working, it simply stops being *completed* and stops
being *declared*.

The immediate effect is on nushell, where those declarations are what doubles
the rendered list. Every other shell gains a smaller script and loses
completions nobody wants. `every_completion_script` is otherwise unaffected: it
probes for a `completions` subcommand on unbuilt commands, which this does not
touch.

To see the size of the tree, or check the effect of this change:

```sh
devkit completions nushell | rg -c 'export extern'
devkit help --full | wc -l
```

## Testing

The TTY branch cannot be exercised under `cargo nextest`, which is why the
decision lives in a pure function and `DEVKIT_HELP` exists. Unit tests cover
the rule; integration tests cover the rendering, pinned with the env var.

**Unit, on `help::decide`:** every combination of the three inputs, asserting
precedence order and that an unrecognized env value falls through to the TTY
signal while returning a warning.

**Unit, on the command tree:** every `about` in `Cli::command()`, walked
recursively, is at most seventy characters.

**Integration, node resolution.** These are the cases that killed the argv
walker, so each gets its own test. All run under `DEVKIT_HELP=full`.

- `issue -C <tmpdir> status --help` prints `issue status`'s argument help,
  identified by one of its own flags, not the `issue` tree.
- `devkit issue typo --help` exits non-zero and reports the unrecognized
  subcommand. This is the test that would catch a regression to
  `ignore_errors(true)`.
- `devkit issue --help review` prints the `issue` tree, not `review`'s. First
  help wins, matching what clap prints for the same argv today.
- `devkit issue setup --help` prints `setup`'s help despite `--slug` being
  required, proving required args are cleared on the probe.
- `devkit docs path <name> -- --help` does not print help at all: after `--`,
  `--help` is a positional value.
- `devkit docs add help` is not a help request. `docs add` is a leaf, so `help`
  is the library name.
- `docm remove --help` resolves through the alias to the `rm` node.
- `devkit --help -h` prints the terse view. `-h` outranks everything.
- `devkit help docs add --full` prints `docs add`'s long help and exits zero,
  rather than failing on an unexpected argument.
- `devkit issue help status`, `devkit help issue status` and
  `devkit issue status --help` produce identical output.

**Integration, rendering:**

- `--help` under `DEVKIT_HELP=full` names a nested node from two different
  groups; the same command under `DEVKIT_HELP=terse` names neither.
- `-h` under `DEVKIT_HELP=full` still prints the terse view and still carries
  every shim name in its footer. This is what proves `-h` is unconditional.
- The full tree also carries every shim name in its footer.
- `help` and `--help` produce identical output under the same environment.
- `help --full` prints the tree under `DEVKIT_HELP=terse`, proving the flag
  outranks the env var.
- Scoping: `issue --help` names an `issue` child and does not name a `docs`
  child. Under the shim name, `docm --help` names a `docs` child and does not
  name an `issue` child.
- Under `docm`, every rendered path begins with `docm`, and the root line
  carries the `docs` about text.
- No rendered tree line exceeds one hundred columns, no rendered path contains
  a `help` segment, and no rendered line contains a non-ASCII byte.
- A generated completion script declares nothing under a `help` path.

**Integration, intercept placement:** running `devkit --help` from a directory
holding only a copied `devkit` executable creates the six shim hardlinks beside
it, the promise `docs/install.md` makes. This is what fails if the intercept
drifts above `links::ensure_current`.

**Existing tests that this changes.** Both currently pipe stdout, which under
this design means the full tree, so both must keep passing on the tree rather
than being pinned to `DEVKIT_HELP=terse`. Pinning them would let a real
regression through.

- `tests/cli_ergonomics.rs::devkit_help_names_every_shim` asserts all six
  footer mapping lines. Passes because the tree appends the footer.
- `tests/shim_dispatch.rs::portm_shim_parses_portm_arguments` asserts the
  output contains `Port registry` and not `Configure and diagnose`. Passes
  because the tree prints a root line.

## Files

| Path | Change |
|---|---|
| `src/help.rs` | new: `Verbosity`, `Decision`, `decide`, `tree`, the probe builder |
| `src/lib.rs` | declare the module |
| `src/bin/devkit/main.rs` | the intercept, placed after `links::ensure_current`; shortened `about` strings on the top-level subcommands |
| `src/bin/devkit/{docs,locks,ports,schema}.rs`, `src/bin/devkit/{run,issue}/mod.rs` | shortened `about` strings where they exceed the cap |
| `src/completions.rs` | `disable_help_subcommand(true)` before `build()` in `emit` |
| `Cargo.toml` | add clap's `wrap_help` feature |
| `tests/cli_ergonomics.rs` | the help integration tests; existing shim-footer test now exercises the tree |
| `tests/shim_dispatch.rs` | existing `portm --help` test now exercises the tree |
| `tests/completions.rs` | assert no `help` declarations |
| `docs/commands.md` | document the two views and `DEVKIT_HELP` |
| `docs/agents.md` | note that a piped `--help` yields the tree |

## Costs accepted

`--help` output now depends on where it is pointed. Reading help through a
pager or into a file yields the agent view. `-h` is the unconditional human
view and is what the documentation points a reader at.

`-h` loses the prose that moves to `long_about`.

`docs/commands.md` has to describe two outputs rather than one.

The probe is a recursive rebuild of the command tree plus a second parse of
argv, on every invocation that reaches the intercept. It does no IO and runs
once per process.

## Unresolved questions

1. Should the tree include each node's aliases (`docs rm` also answers to
   `remove` and `delete`)? Omitting them keeps one line per node; including
   them is a second column that is empty for nearly every row.
2. Is seventy characters the right `about` cap, or should the cap be derived at
   render time from the longest path so the budget adjusts as the CLI grows?
   Deriving it removes a magic number but makes the failing test's message
   harder to act on.
