# Readable todo ids

## Problem

`devkit todo ready`, `plan`, `list` and the injected context name each todo by its id, shortened by `short_id`: a uuid shows as its first 8 hex characters, any other id whole. A person scanning `plan` or `ready` to see where agents are gets nothing from `96432cd6`: it does not say which ticket a todo is, and it does not stick in memory between two runs.

Ticket todos already carry a readable name, the tracker key (`TicketRef.key`), and every synced description starts with its short form (`todo_text` in `crates/devkit-todo/src/ticket.rs` writes `"{key} {title}"`, a GitHub key shortened to `#N`). Yet the key cannot be typed back: `TodoStore::get` resolves only stored ids and, on uuid backends, uuid prefixes of 8 or more hex characters, so `devkit todo start SWE-123` fails with `no todo SWE-123`.

## Who uses it

- **A person in a terminal** reading `plan` and `ready` to check progress. This is the main reader.
- **Agents**, which read the same names in the injected lists and type them into `start`, `done`, `block` and the rest.
- **Scripts**, which want the stored ids.

## Goals

- Every todo has one readable name, shown the same way in every human-readable output.
- Every name a listing can show is accepted wherever a todo id is taken, whatever the display setting.
- The display style is chosen in config and overridden per command by a flag.
- Names are stable: a todo's name never changes when other todos are added or removed.

## Non-goals

- Tab completion of todo ids. A separate follow-up that builds on the resolver this adds.
- Storing names. Names are derived from what the store already holds.
- Changing `Todo.id`, `TicketRef`, or any backend's id format.
- MCP: the MCP server has no todo tools.

## Anti-goals

- Output that depends on whether stdout is a TTY. A piped `todo ready | rg x` must show what the screen showed.
- Names that renumber under concurrent sessions (taskwarrior-style working-set integers). `todo done 3` closing the wrong todo after another session added one is the failure this rules out.
- A displayed name that a command refuses as input.
- A second ticket-key matcher beside the ones `todo sync --ticket` and the native-tool mirror use.

## Design

### Id kinds

Backends store two kinds of id. The taskchampion, Postgres and Supabase backends store uuids. The builtin backend, the default, stores a counter as text (`"1"`, `"65"`; `crates/devkit-todo-builtin/src/lib.rs`), which is already short and memorable. Word names and hex prefixes exist only for uuid ids. A non-uuid id shows whole under every style, as `short_id` does today.

### Display styles

One setting, `[todo] ids`, with three values. `--ids <style>`, a global flag on `devkit todo`, overrides it for one command. The default is `name`.

| Style | Ticket todo | Other uuid todo | Non-uuid todo |
|---|---|---|---|
| `name` | tracker key | word name, e.g. `brave-otter` | the id |
| `short` | tracker key | first 8 hex characters (today's `short_id`) | the id |
| `full` | stored id | stored id | stored id |

The config key's doc comment carries the meaning (it becomes the schema description); `schema/devkit-config.json` is regenerated. Outputs outside `devkit todo` (the injected context, the stop hook, `devkit activity`) have no flag and follow the config.

`short` is a fixed width so it stays stable and needs no read of the whole store to render. The git-style part is on input: any unique hex prefix of 4 or more characters resolves, and an ambiguous one is an error listing the matches, as `by_prefix` does today.

`full` replaces the earlier idea of an `--id` flag.

### Where the name goes

The name takes the id's place in each format: the id column of `plan` and `ready` rows, and the trailing parentheses of the markdown lists (`- [ ] title (#65, in progress: x)`).

### Ticket todos

The tracker key is the name. A Linear key renders as `SWE-123`. A GitHub key renders as `#N` only when the caller's tracker reads `#N` back as that same key; otherwise it renders whole, `owner/repo#N`. A listing that spans repositories (`list --all`, the global list) therefore never shows two different todos as the same `#N`, and every shown key resolves from where it was shown.

Since the description already starts with the key, a row strips the prefix `todo_text` writes for that tracker and key (`"#65 "` for GitHub, `"SWE-123 "` for Linear), so the key is not printed twice. A description that does not start with that prefix (re-described by hand) prints whole.

`#` starts a comment in bash and fish, so `devkit todo start #65` reaches devkit as `devkit todo start`. The bare number is the typed spelling of `#N`, and input already accepts it. `render::guide`, the text injected ahead of every agent's lists, and `plugin/skills/using-devkit/references/todo.md` both say so. The display keeps `#65`, matching the description and the tracker.

### Word names

Every uuid todo has a word name, ticket todos included, where it is never shown, so resolution is one rule for every uuid todo. The name is two words from an embedded list, joined by `-`, picked by bits of the uuid itself: no hash function, so names are the same on every backend and every Rust release.

The name space is at least 2^24: two draws from a list of at least 4096 words. Expected colliding pairs are about n² / 2^25, under one for a 5000-todo store, closed todos included. The list is ASCII, lowercase and short-worded, and excludes every word spelled only with hex letters (`bead`, `face`), so a word name never reads as a hex prefix.

Display shows the name regardless of collisions; an input naming more than one todo is an error listing each match by its 8-hex prefix, which is always accepted.

### Input resolution

The resolver is the CLI `Store` wrapper's `TodoStore::get` (`src/bin/devkit/todo/store.rs`). It already holds the config and the caller's checkout for the tracker, and every existing `store.get` caller inherits it: `apply_checked` (start, stop, done, undone, cancel), `start`'s blocker check, `purge`, and `resolve_id` / `resolve_ids` (`block`, `unblock`, `add --blocked-by`). `describe`, `move` and `add --parent` pass the raw string into their `Edit` or `NewTodo` today; each gains a `resolve_id` call.

Resolution order:

1. **Exact stored id.** The backend's own `get`. A match wins and nothing else is tried, so on the builtin backend `65` is todo 65 even when GitHub issue #65 is synced. A uuid prefix of `SHORT_ID` or more characters that names one todo also resolves here, as today, so the 8-hex id a listing prints always resolves; a ticket whose number reads as such a prefix is reached by `#N` or its full key.
2. Otherwise the input is matched against one listing of the whole store, every status included (`undone` and `purge` take closed todos), as each of these forms:
   - **a ticket key**. A full key (`owner/repo#65`, `SWE-123`) matches a stored `TicketRef.key` directly, case-insensitively, from any directory and for any repository. A short form (`#65`, `65`, the issue URL) follows devkit's existing issue identification: it goes through `Tracker::ticket_key`, the parser `todo sync --ticket` uses, with its host and repository checks, and only when the caller's tracker is GitHub; with no GitHub tracker in reach (none, or Linear) it is not a ticket key. Listings show `#N` only where a GitHub tracker reads it back, so every shown key still resolves, and a bare number never reaches another repository's todo. Matching shares `names_issue` in `src/bin/devkit/todo/tickets.rs`, not `named_ticket`, whose fallback matches description text. A parse error from `ticket_key`, or a parsed key that matches no `TicketRef`, means "not a ticket key" and the next form is tried; it is never the command's failure. (GitHub's parser rejects non-key input; Linear's uppercases anything.) A key resolves to the todo carrying that `TicketRef`, never by description: after `todo promote`, a cancelled todo whose text is the bare key sits beside the live ticket todo.
   - **a word name**.
   - **a hex prefix** of 4 or more characters.

   More than one match across these forms (a bare `1234` that is both an issue number and a hex prefix) is an error naming every match.

Inputs that are not full ids or long uuid prefixes never reach a backend's query as text; they are matched against the listing, keeping the guarantee `is_uuid_prefix` documents. A command reads that listing at most once.

### Output paths

Every id printed for a reader goes through one display function that takes the style:

- `render.rs`: plan, ready, the markdown lists, the context block, `render::guide`;
- `src/bin/devkit/todo/mod.rs`: `add`'s printed id, `unblocked:` lines, the cycle refusal and other messages naming a todo;
- the stop hook's hold reason (`src/bin/devkit/hook/todo.rs`, which renders the lists);
- `devkit activity`'s held-todos table (`src/bin/devkit/activity.rs`);
- `push.rs` error text naming a todo;
- the `short_id` and message sites #373 adds.

Full ids stay in `list --json` and any other JSON output, and in the native-tool map, the activity log and anything else devkit stores. `plan --mermaid` node identifiers stay `t{8-hex}` as today, already Mermaid-safe; only the node labels use the display name.

## Constraints

- Can start on main now: the key parser it reuses (`Tracker::ticket_key`) is already there. #373 (push parents and blockers) has merged, and its `short_id` sites go through the display function. #374 (promote) touches `mod.rs`, `ticket.rs` and `tickets.rs`; rebase after it merges. #374's promote is the case the "resolve by `TicketRef`, not text" rule exists for.
- Cost: `name`, `short` and `full` render without reading more of the store than each output reads today. Resolving anything but an exact id or long uuid prefix costs one whole-store listing per command, which `start` without `--force` already reads.
- Help text stays ASCII.
- Tests drive the CLI entry points: `ready`/`plan` output under each style on a uuid backend and on builtin, `start <word name>`, `start 65` for a GitHub ticket, `start <4-char hex>`, an exact builtin id winning over a ticket number, the config default and the flag override, and the ambiguity errors.

## Decided by the designer, not the requester

The requester set the goals, the config key with a flag override, both word names and git-style prefixes, and left the default open. These were the designer's calls:

- `name` is the default style.
- One setting with three styles; `full` stands in for the requested `--id` flag.
- `short` displays a fixed 8 hex characters; the git-style shortest-unique part is on input only, so names stay stable and rendering needs no whole-store read.
- Word names and hex prefixes apply to uuid ids only; builtin's integer ids show whole.
- Input accepts every form whatever the style; an exact stored id wins outright.
- Word-name collisions show as-is and fail as input with a list of matches.
- The key prefix `todo_text` writes is stripped from a ticket todo's description in rows.
- A GitHub key shows short only where it resolves back short; the bare number is its typed spelling.
- The resolver lives in the CLI `Store` wrapper's `get` and searches the whole store, read once per command.

## Left to the builder

- The word list's source, within the constraints above.
- Where the display function lives (`devkit-todo` versus the binary).
- Column alignment in `ready` and `plan` once names vary in width.
