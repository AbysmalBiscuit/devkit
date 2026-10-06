# Rules

devkit injects a repository's coding rules into an agent's context: the rules governing a file arrive with the first structured write to it, and the repository's `must` rules arrive at session start. `repo-rules-agent` extracts the rules from the repository's agent docs. `devkit rules query`, `stats` and `context` read the same rules the hooks inject, and `devkit rules add`, `edit` and `remove` change them by hand. Each command's `-h` has its flags; `devkit schema` has every `[rules]` key. This file covers where the rules come from and how each source behaves.

## Sources

`[rules] source` picks the store every read and edit goes through:

- `file` (the default): an index file the extractor writes, of either kind: a **SQLite store** (`index.sqlite`), what `repo-rules index` writes now, or a legacy **JSON index** (`index.json`).
- `postgres`: one repository in the extractor's shared Postgres store, the `repo_rules` schema, which every machine and the extraction workers use. `[rules.postgres] repository` names the repository's UUID.

`devkit doctor` prints the `rules_source` row: the source, the kind of store it found (`sqlite`, `json` or `postgres`), and where it is.

Whatever the source, a rule with no tasks applies to every task, the severity filters work as a floor (`--min-severity`) as well as an exact match, and a removed rule never appears. Reading never changes the store.

## The file source

With no `[rules] index`, devkit looks in the extractor's cache directory for the main worktree, so every worktree of a repository reads the same rules. A SQLite store there wins over a JSON index beside it, as the extractor's own `cache list` prefers it. `[rules] index`, or the path `query` and `stats` take as an argument, names a file of either kind: devkit tells them apart by the file's contents, whatever it is called.

A SQLite read opens only the store, not the `index.extraction.sqlite` archive beside it.

### Edits

`add`, `edit` and `remove` write to the same file the reads come from. The rule they touch is pinned: a rebuild of the index keeps it as written instead of extracting it again.

- **`add`** creates a pinned rule and prints its id. In a SQLite store the rule belongs to the extractor's `<manual>` source, and its id is the extractor's hash of that source and the title. A title a rule made with `add` already has is refused.
- **`edit <id>`** changes the fields you pass and keeps the id, even when the title changes. A list flag (`--task`, `--lang`, `--topic`) replaces that whole list. Fields devkit has no flag for are kept.
- **`remove <id>`** leaves an extracted rule behind as a pinned tombstone, which every reader skips and a rebuild does not extract again. A rule made with `add` is deleted outright.

In a SQLite store, each edit is one transaction: it pins the rule, replaces its lists, and increments the repository's revision, and a failure anywhere rolls back all of it. The store's extraction generation and the rule's extraction fingerprint stay as they were, which is how `repo-rules index` matches the pin to its extracted original and keeps the edit.

A SQLite store allows several rules to share an id. `edit` and `remove` refuse such an id and list each rule it names, since changing any one of them would be a guess. They also refuse a store whose journal mode is not rollback journaling (`delete`, `truncate` or `persist`), and leave that setting alone.

### Failures

A SQLite store that holds a row devkit cannot read or carries a storage version devkit does not support injects nothing; the hook prints one stderr line naming the file and the write goes ahead. So does a configured store that is missing, when its name ends in `.sqlite`, `.sqlite3` or `.db`. `devkit rules query` and `stats` exit non-zero naming the file. A missing JSON index is the common case of a repository nobody has indexed, and is silent.

## The Postgres source

```toml
[rules]
source = "postgres"

[rules.postgres]
repository = "<repository uuid>"
doppler_project = "repo-rules"   # optional
```

The connection URL comes from `DEVKIT_RULES_DATABASE_URL`: the environment first, then Doppler when `[rules.postgres] doppler_project` (and optionally `doppler_config`) is set, then `~/.config/devkit/secrets.toml`:

```toml
devkit_rules_database_url = "postgres://..."
```

A URL Doppler gives is kept in devkit's state directory, readable only by you, and hooks reuse it for a while instead of asking Doppler on every write. Commands and `devkit doctor` ask Doppler afresh and refresh the kept copy, and a hook that fails to connect with it drops it, so the next hook picks up a rotated credential or a moved database.

The connection always uses TLS and verifies the server's certificate against the bundled Mozilla roots, the platform's store, and the PEM file `[rules.postgres] ca_file` names. devkit reads `ca_file` from your own `~/.config/devkit/config.toml` alone; a project's `devkit.toml` cannot add a CA. Only `sslmode=disable` in the URL connects in plaintext.

The role in the URL reads and writes the `repo_rules` tables directly, so it must pass the store's row-level security. devkit never creates or migrates the schema, and it refuses a store at a storage version it does not read.

### Failures

A hook that cannot read the rules injects none and prints one line on stderr naming why: the database is unreachable, gives no answer within the hook's short wait, refuses the login, holds no such repository, or is at an unsupported storage version. The write itself goes ahead. A hook asks Doppler for the URL when its kept copy is missing or stale, and that lookup adds its own wait before the hook connects. `devkit rules query`, `stats`, `add`, `edit` and `remove` exit non-zero with the same cause. `devkit doctor` shows the source, the repository, where the URL resolved from and whether the database answers, never the URL itself.

### Edits

Each edit is one transaction that first locks the repository's row, as every writer to the store does, so two edits to one repository run one after the other and the second sees the first. The edit pins the rule so a re-extraction keeps it, replaces any list it sets (tasks, languages, topics) whole, and increments the repository's revision.

- `add` files the rule under the `<manual>` source, as the extractor's own edits do, so its id is the extractor's hash of `<manual>` and the title.
- `edit` keeps the rule's id, source and extraction fingerprint.
- `remove` leaves a pinned tombstone, a rule added by hand included, and every reader skips it.

Imported records can share an id. An edit or removal naming an id that more than one live rule carries changes nothing and says so; change those through `repo-rules-agent`.
