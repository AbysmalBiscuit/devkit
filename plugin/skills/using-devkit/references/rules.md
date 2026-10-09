# Rules

devkit injects a repository's coding rules into an agent's context: the rules governing a file arrive with the first structured write to it, and the repository's `must` rules arrive at session start and, under Claude Code and Codex, when a subagent starts. `repo-rules-agent` extracts the rules from the repository's agent docs. `devkit rules query`, `stats` and `context` read the same rules the hooks inject, and `devkit rules add`, `edit` and `remove` change them by hand. Each command's `-h` has its flags; `devkit schema` has every `[rules]` key. This file covers where the rules come from and how each source behaves.

## Sources

`[rules] source` picks the store every read and edit goes through:

- `file` (the default): an index file the extractor writes, of either kind: a **SQLite store** (`index.sqlite`), what `repo-rules index` writes now, or a legacy **JSON index** (`index.json`).
- `postgres`: one repository in the extractor's shared Postgres store, the `repo_rules` schema, which every machine and the extraction workers use. `[rules.postgres] repository` names the repository's UUID.
- `supabase`: the same store reached over a Supabase project's Data API, through `repo-rules-agent`'s `repo_rules_api` functions, for a machine that can make HTTPS requests but not open a Postgres connection. `[rules.supabase] repository` names the repository's UUID.

`devkit doctor` prints the `rules_source` row: the source, the kind of store it found (`sqlite`, `json`, `postgres` or `supabase`), and where it is. A remote source (`postgres` or `supabase`) is read through a local cache; see [The cache](#the-cache).

Whatever the source, a rule with no tasks applies to every task, the severity filters work as a floor (`--min-severity`) as well as an exact match, and a removed rule never appears. Reading never changes the store.

## The file source

With no `[rules] index`, devkit looks in the extractor's cache directory for the main worktree, so every worktree of a repository reads the same rules. A SQLite store there wins over a JSON index beside it, as the extractor's own `cache list` prefers it. `[rules] index`, or the path `query` and `stats` take as an argument, names a file of either kind: devkit tells them apart by the file's contents, whatever it is called.

A SQLite read opens only the store, not the `index.extraction.sqlite` archive beside it.

### Edits

`add`, `edit` and `remove` write to the same file the reads come from. The rule they touch is pinned: a rebuild of the index keeps it as written instead of extracting it again.

With no `[rules] index` and nothing yet in the cache directory, all three refuse, naming that directory: build the index with `repo-rules index` first. A JSON index `add` created there would be hidden by the SQLite store the extractor writes next. A missing file `[rules] index` names is created by `add` as a JSON index.

- **`add`** creates a pinned rule and prints its id. In a SQLite store the rule belongs to the extractor's `<manual>` source, and its id is the extractor's hash of that source and the title. `add` refuses a title whose id an existing rule already has, including the original title of a rule renamed with `edit`; the new title of a renamed rule is not refused.
- **`edit <id>`** changes the fields you pass and keeps the id, even when the title changes. A list flag (`--task`, `--lang`, `--topic`) replaces that whole list. Fields devkit has no flag for are kept.
- **`remove <id>`** leaves an extracted rule behind as a pinned tombstone, which every reader skips and a rebuild does not extract again. A rule made with `add` is deleted outright.

In a SQLite store, each edit is one transaction: it pins the rule, replaces its lists, and increments the repository's revision, and a failure anywhere rolls back all of it. The store's extraction generation and the rule's extraction fingerprint stay as they were, which is how `repo-rules index` matches the pin to its extracted original and keeps the edit.

A SQLite store allows several rules to share an id. `edit` and `remove` refuse such an id and list each rule it names, since changing any one of them would be a guess. They also refuse a store whose journal mode is not rollback journaling (`delete`, `truncate` or `persist`), and leave that setting alone. A store in WAL journal mode is neither edited nor read.

### Failures

A SQLite store that holds a row devkit cannot read, carries a storage version devkit does not support, or is in WAL journal mode injects nothing; the hook prints one stderr line naming the file and the write goes ahead. So does a configured store that is missing, when its name ends in `.sqlite`, `.sqlite3` or `.db`. `devkit rules query` and `stats` exit non-zero naming the file. devkit never opens a WAL store, since opening one creates its `-shm` and `-wal` files, and never changes its journal mode. A missing JSON index is the common case of a repository nobody has indexed, and is silent.

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

A URL Doppler gives is kept in devkit's state directory, readable only by you, and hooks reuse it instead of asking Doppler on every write. Commands and `devkit doctor` ask Doppler afresh and refresh the kept copy. Session start reuses it for a while, then asks Doppler again, and keeps using it when Doppler gives none. A write hook never asks Doppler: the kept URL names the cache it reads, so writes find their rules offline. A login the database refuses drops the kept URL, so the next session picks up a rotated credential; a database that does not answer leaves it in place.

The connection always uses TLS and verifies the server's certificate against the bundled Mozilla roots, the platform's store, and the PEM file `[rules.postgres] ca_file` names. devkit reads `ca_file` from your own `~/.config/devkit/config.toml` alone; a project's `devkit.toml` cannot add a CA. Only `sslmode=disable` in the URL connects in plaintext.

The role in the URL reads and writes the `repo_rules` tables directly, so it must pass the store's row-level security. devkit never creates or migrates the schema, and it refuses a store at a storage version it does not read.

### Failures

Hooks read the rules from the local cache (see [The cache](#the-cache)), so once the cache exists a write hook never waits on the database. With no cache yet, a hook that cannot read the rules injects none and prints one line on stderr naming why: the database is unreachable, gives no answer within the hook's short wait, refuses the login, holds no such repository, or is at an unsupported storage version. The write itself goes ahead. A hook asks Doppler for the URL when its kept copy is missing or stale, and that lookup adds its own wait before the hook connects. `devkit rules query`, `stats`, `add`, `edit` and `remove` exit non-zero with the same cause. `devkit doctor` shows the source, the repository, where the URL resolved from and whether the database answers, never the URL itself.

### Edits

Each edit is one transaction that first locks the repository's row, as every writer to the store does, so two edits to one repository run one after the other and the second sees the first. The edit pins the rule so a re-extraction keeps it, replaces any list it sets (tasks, languages, topics) whole, and increments the repository's revision.

- `add` files the rule under the `<manual>` source, as the extractor's own edits do, so its id is the extractor's hash of `<manual>` and the title.
- `edit` keeps the rule's id, source and extraction fingerprint.
- `remove` leaves a pinned tombstone, a rule added by hand included, and every reader skips it.

Imported records can share an id. An edit or removal naming an id that more than one live rule carries changes nothing and says so; change those through `repo-rules-agent`.

## The Supabase source

```toml
[rules]
source = "supabase"

[rules.supabase]
repository = "<repository uuid>"
publishable_key = "sb_publishable_..."
callback_port = 7471            # the default
doppler_project = "repo-rules"  # optional
```

The project URL, `https://<project-ref>.supabase.co`, comes from `DEVKIT_RULES_SUPABASE_URL`, else `[rules.supabase] url` in your own `~/.config/devkit/config.toml`. A project's `devkit.toml` cannot set it, so a checkout cannot send your session elsewhere.

The `repo_rules_api` functions run as a signed-in Supabase user, and the repository's `repository_members` table gives that user a role: `reader` reads, `editor` and `owner` also edit. A repository the user has no role on answers exactly as a missing one does. Requests go one of three ways:

- **As the user `devkit auth supabase` signed in.** The session is kept in devkit's state directory, readable only by you, and refreshed before it expires. See [Signing in](#signing-in).
- **With an email and password.** With no session, or one that no longer refreshes, devkit signs in with `DEVKIT_RULES_SUPABASE_EMAIL` and `DEVKIT_RULES_SUPABASE_PASSWORD`, which resolve as the Postgres URL does: the environment, then Doppler when `[rules.supabase] doppler_project` is set, then `~/.config/devkit/secrets.toml`. Hooks reuse what Doppler last gave.
- **With no credentials at all**, when `publishable_key` is unset: for a cloud container behind a proxy that attaches identity itself.

Software factories share one Supabase user with the `reader` role, its email and password kept once in Doppler. Each factory resolves them with its Doppler service token, signs in, and fills its cache at session start; rotating the password in Doppler reaches every factory at its next sign-in. Supabase's service-role key is not used: the functions are granted only to signed-in users and check who is calling, and the key would bypass row-level security across the whole project.

### Signing in

`devkit auth supabase` signs in to the project `[rules.supabase]` names (or `--url`) and keeps the session for every hook and command on the machine. It needs `publishable_key`.

- **Browser** (the default): devkit lists the sign-in providers the project enables, takes the only one or asks which (`--with <provider>` picks one), prints the provider's sign-in page and tries to open it, and waits up to five minutes for the browser to come back to `http://localhost:<callback_port>/callback`. The port defaults to 7471; devkit refuses it when its port registry holds it or something else listens there.
- **`--email`**: Supabase emails a one-time code, which you type into the terminal. No browser.
- **`--password`**: signs in with `DEVKIT_RULES_SUPABASE_EMAIL` and `DEVKIT_RULES_SUPABASE_PASSWORD`, as hooks do when they have no session.

Project setup: enable the providers wanted, add `http://localhost:<callback_port>/callback` to the project's redirect allow list exactly, and give each user a `repository_members` row. A sign-in the project refuses for its redirect says to add that URL. Hooks never open a browser: with no session and no password they fail with one stderr line naming `devkit auth supabase`.

### Edits

Each edit reads the rules afresh, for the repository's current revision and the rule's key, and sends that revision with the change. When another edit got in first, devkit reads again and retries once, then fails naming the conflict.

- `add` files the rule under `<manual>`, and its id is the hash of `<manual>` and the title, the id the server assigns.
- `edit` sends every editable field, the ones you pass over the rule's current values, and pins the rule.
- `remove` leaves a pinned tombstone.
- `--topic` is refused: topics change through `repo-rules-agent`.

An id more than one live rule carries is refused, listing their keys.

### Failures

As for the Postgres source, a hook with a cache never waits on the API, and one without a cache injects nothing and prints one stderr line when the API cannot be read. A user whose role cannot edit gets `your role cannot edit repository <uuid>`; a value the server refuses comes back in the server's words. An expired or refused session is refreshed, then replaced by a password sign-in, then reported as `not signed in ...: run devkit auth supabase`. `devkit doctor` shows where the URL and sign-in resolve from, whether a session is kept and when its token expires, and whether the API answers, never a key, token or password.

The API returns rules without the extractor's file tiers, so where the file source breaks a tie between equally specific rules by the tier of their files, this source keeps the store's order.

## The cache

A remote source's rules are read from a SQLite file under devkit's state directory, `rules-cache/<source>-<repository uuid>.sqlite`, which every worktree and session on the machine shares. The `file` source has no cache. The cache holds one repository's rules from one source; a file naming another source or repository, or written in a format devkit does not know, counts as no cache and is replaced at the next refresh. A refresh replaces the whole file in one transaction, so a reader sees the old rules or the new ones, never a mix.

- **Session start** (`devkit rules context`, which the session hooks run) asks the remote for its revision and pulls the rules only when it differs from the cached one, within the hook's short wait. When that fails it prints one stderr line and injects from the cache it has.
- **`devkit rules pull`** pulls whatever revision the cache holds and prints the source, the revision and the rule count. It exits non-zero naming the cause when the remote cannot be read, and refuses the `file` source, which has nothing to pull.
- **`devkit rules query` and `stats`** refresh as session start does, with a command's longer wait. A failed refresh prints one stderr line and the command reads the cache; it exits non-zero only when there is no cache either. Given an index file as an argument, they read that file alone and never touch the cache or the remote.
- **A write hook** reads the cache alone and never refreshes it. With no cache at all, it reads the remote directly within its short wait and fills nothing.
- **`add`, `edit` and `remove`** change the remote, then pull it into the cache, so the next read shows the change. When that pull fails the edit still stands, and one stderr line says the cache is stale.

`devkit doctor`'s `rules_cache` row shows the cache's path, the revision it holds and how long ago it was pulled, or warns that there is no cache yet.
