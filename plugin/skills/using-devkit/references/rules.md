# Rules

`repo-rules-agent` extracts a repository's rules from its agent docs. devkit injects the ones governing a file when an agent is about to write it, prints the repository-wide `must` rules at session start, and answers `devkit rules query`, `stats` and `context`. `devkit rules add`, `edit` and `remove` change rules by hand. `devkit schema` describes every `[rules]` key; this file covers where the rules come from and how each source behaves.

## Sources

`[rules] source` picks the store every read and edit goes through:

- `file` (the default): the JSON index the extractor writes, at `[rules] index` or the extractor's own cache path for the main worktree. A path passed to `query` or `stats` reads that index instead.
- `postgres`: one repository in the extractor's shared Postgres store, the `repo_rules` schema, which every machine and the extraction workers use. `[rules.postgres] repository` names the repository's UUID.

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

A hook that cannot read the rules injects none and prints one line on stderr naming why: the database is unreachable, gives no answer within a second, refuses the login, holds no such repository, or is at an unsupported storage version. The write itself goes ahead. `devkit rules query`, `stats`, `add`, `edit` and `remove` exit non-zero with the same cause. `devkit doctor` shows the source, the repository, where the URL resolved from and whether the database answers, never the URL itself.

### Edits

Each edit is one transaction that first locks the repository's row, as every writer to the store does, so two edits to one repository run one after the other and the second sees the first. The edit pins the rule so a re-extraction keeps it, replaces any list it sets (tasks, languages, topics) whole, and increments the repository's revision.

- `add` files the rule under the `<manual>` source, as the extractor's own edits do, so its id is the extractor's hash of `<manual>` and the title.
- `edit` keeps the rule's id, source and extraction fingerprint.
- `remove` leaves a pinned tombstone, a rule added by hand included, and every reader skips it.

Imported records can share an id. An edit or removal naming an id that more than one live rule carries changes nothing and says so; change those through `repo-rules-agent`.
