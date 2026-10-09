# Rules remote cache and Supabase source

## Problem

The `postgres` rules source queries the shared database on every hook. A write hook waits up to its short timeout on the network before it can inject the rules governing a file, and a machine that cannot reach the database injects nothing. Reads that run on every structured write should come from local disk.

`repo-rules-agent` also exposes its store through Supabase's Data API (the `repo_rules_api` schema of server functions, behind row-level security keyed on `auth.uid()`). devkit cannot read it there, so a machine that can make HTTPS requests but not open a Postgres connection, such as a cloud container behind a proxy or a software factory on EC2, gets no rules.

The Postgres and Supabase plumbing devkit needs already exists for todos, but each store carries its own copy: `devkit-rules::postgres::Database` and `devkit-todo-postgres::Database` repeat one connection runner, and the PostgREST client lives inside `devkit-todo-supabase` with its schema hard-coded.

## Decisions

- Remote rule sources (`postgres`, `supabase`) are always read through a local SQLite cache. The `file` source has no cache. Todo stores are not cached: a todo backend is the claim authority, and a stale local view of who holds what would mislead an agent.
- The cache refreshes at session start, when the remote's revision differs from the cached one, and on demand through `devkit rules pull`. Write hooks read only the cache.
- The cache has a devkit-owned layout built from `RuleIndex`, not the extractor's SQLite layout. Supabase's `query_rules` returns effective rule payloads without generations, fingerprints, source tiers or `discovered` flags, so one layout filled from both remotes cannot be the extractor's without inventing those columns.
- There is no generic database trait. The domain traits (`RuleSource`, `TodoStore`) are the seam backends implement; what is shared is transport: a Postgres runner and a PostgREST client.
- Supabase sign-in is driven by devkit over Supabase Auth's HTTP API, with no supabase-js and no hosted page: browser OAuth for any enabled provider, an emailed one-time code, a password from the secrets chain, or no credentials behind a proxy that attaches identity.

## 1. Shared transport crates

### `devkit-postgres`

Gains the connection runner the rules and todo stores share:

- `Database::new(url, trust, wait, label)`, where `label` names the database in errors ("rules database", "todo database").
- `Database::unusable(reason)` for a URL that is missing or does not parse.
- `run(op)`: builds a current-thread runtime, connects (TLS unless `sslmode=disable`), runs `op` within `wait`, and names the database in errors from the network or the server.
- `finish_by(deadline)`, `on_connect_failure(f)` and `target()` (credential-free `host:port/db`).

`devkit-todo-postgres` keeps its schema, `update_schema`, `check` and its SQLSTATE mapping (`DK001`, `DK002`) on top of it. `devkit-rules` keeps its storage version check and queries. Neither changes behaviour.

### `devkit-supabase` (new)

Built from `devkit-todo-supabase::Api`:

- `Api::new(url, schema, auth, wait)`, with `select`, `rpc`, `finish_by` and `on_rejected` (run on a 401). The first request that gets no answer makes every later one fail at once, so an unreachable API costs one wait.
- PostgREST's refusal body (`code`, `message`, `details`) is exposed, so each domain crate maps its own codes.
- `Auth`:
  - `None`: requests carry no credentials, for a proxy that attaches them.
  - `Key(String)`: the key goes as `apikey` and as the bearer token. This is the todo backend's mode today.
  - `User { publishable_key, sessions }`: `apikey` is the publishable key and the bearer is a signed-in user's access token.
- `auth` module: a `Session` (access token, refresh token, expiry), the password, refresh, PKCE code exchange and one-time-code grants, and a session file per project URL in devkit's state directory, mode `0600`. An expired access token, or a 401, refreshes the session; if the refresh fails and a password resolves, it signs in again.

`devkit-todo-supabase` keeps the `devkit` schema name and its code mapping, and builds its `Api` through this crate. Its behaviour does not change.

## 2. The cache source

`devkit-rules::source::Source` gains a `Cached` member:

```rust
enum Remote { Postgres(PostgresSource), Supabase(SupabaseSource) }
struct CachedSource { cache: RuleCache, remote: Remote }
```

A remote reads its rules into a `RuleIndex`, applies an edit, and reports its revision: Postgres reads `repo_rules.repositories.revision`, Supabase calls `stats_rules`. `Remote` is an enum whose members implement one internal trait, delegated as `Source` delegates `RuleSource`.

### Cache file

`<state dir>/rules-cache/<source>-<repository uuid>.sqlite`, shared by every worktree and session on the machine.

- `meta`: cache format version, source kind, repository UUID, the checkout path the index describes, remote revision, pull time.
- `files`: one row per rule file, its JSON, in index order.
- `rules`: one row per live rule, its JSON, in index order.

Every reader loads a whole `RuleIndex` and filters it in memory, so the cache holds no filter columns or membership tables. A refresh replaces every table in one transaction, so a reader sees the whole old cache or the whole new one. A cache whose format version devkit does not know, or whose `meta` names another source or repository, is ignored and replaced at the next refresh. The cache uses rollback journaling and a busy timeout, so a read during a refresh waits for it instead of failing.

### Refresh

`refresh(force)` reads the remote revision and pulls when it differs from `meta` or `force` is set.

| Caller | Refreshes | Wait | On failure |
|---|---|---|---|
| session hooks (`devkit rules context`) | when the revision changed | hook wait | one stderr line, keeps the old cache |
| `devkit rules pull` | always | CLI wait | exits non-zero naming the cause |
| `devkit rules query`, `stats` | when the revision changed | CLI wait | one stderr line, reads the old cache; exits non-zero only with no cache |
| write hook | never | none | with no cache at all, reads the remote directly within the hook wait and writes no cache |

`devkit rules pull` prints the source, the revision and the rule count.

### Edits

`add`, `edit` and `remove` go to the remote, then force a refresh, so the next read sees the edit.

### `devkit doctor`

The `rules_source` row adds the cache path, its revision and its age.

## 3. The Supabase rules source

`SupabaseSource` calls `repo_rules_api` through `devkit-supabase::Api` with schema `repo_rules_api` and `Auth::User` or `Auth::None`. `repo-rules-agent` owns the functions; devkit holds no copy of their SQL and codes against the contract below.

### The `repo_rules_api` contract

Each function is called as `POST /rest/v1/rpc/<name>` with a JSON object of named arguments, and answers one JSON object. Execution is granted to signed-in users (`authenticated`) alone. A repository the caller has no role on answers exactly as a missing one does, `{"error":"not_found"}`. Roles are `reader` (read), `editor` and `owner` (read and edit).

`query_rules(p_repo_id uuid, p_task text, p_language text, p_scope text, p_severity text, p_after_position bigint, p_after_rule_key uuid, p_limit integer = 100, p_expected_revision bigint)`. The filters are optional; devkit sends none.

- Answer: `{"revision": n, "generation": uuid, "rules": [payload, ...], "continuation": null | {"after_position": n, "after_rule_key": uuid, "expected_revision": n}}`. The next page passes the continuation's three fields as `p_after_position`, `p_after_rule_key`, `p_expected_revision`.
- `{"error": "conflict", "revision": n, "generation": uuid}` when `p_expected_revision` no longer matches.
- SQLSTATE `22023` ("invalid pagination") when only one cursor field is given, a cursor comes without `p_expected_revision`, or `p_limit` is not between 1 and the server's maximum (1000 unless the server sets `repo_rules.max_query_limit`).
- A payload is the rule's extra fields merged with `id` (12 hex characters), `rule_key` (uuid), `title`, `description`, `category`, `scope`, `severity`, `directory`, `source_file`, `pinned`, `removed`, `tasks` and `languages` (arrays of strings). Removed rules are never returned. `topics`, when present, is one of the extra fields.

`stats_rules(p_repo_id uuid)`: `{"revision": n, "generation": uuid, "conflict_count": n, "total_rules": n, "total_files": n, "by_severity": {..}, "by_category": {..}, ...}`, or `{"error":"not_found"}`.

`put_rule(p_repo_id uuid, p_rule_key uuid | null, p_expected_revision bigint, p_payload jsonb)`:

- `p_rule_key` null adds a pinned rule; otherwise it edits that rule, pins it and clears `removed`.
- `p_payload` must hold exactly `title`, `description`, `category`, `scope`, `severity`, `directory` (strings), `tasks` and `languages` (arrays of strings), plus, on an add only, an optional `source_file`. `scope` is `repo`, `directory` or `file-pattern`; `severity` is `must`, `should` or `can`; each task is `code-review`, `code-generation` or `code-questions`.
- An added rule's id is the first 12 hex characters of SHA-256 of `<source_file or "<manual>">:<title>`.
- Answer: `{"revision": n + 1, "generation": uuid, "rule_key": uuid}`, `{"error":"conflict","revision": n}`, or `{"error":"not_found"}`.
- SQLSTATE `42501` ("repository edit forbidden") for a caller below `editor`; `22023` for a missing expected revision, a missing editable field ("full editable payload required"), an unknown or immutable field ("reserved or immutable field"), or an invalid value.

`remove_rule(p_repo_id uuid, p_rule_key uuid, p_expected_revision bigint)`: marks the rule pinned and removed. Answer `{"revision": n + 1, "generation": uuid}`, a conflict or not-found object as above, and the same `42501`.

### Reads

- Revision: `stats_rules(p_repo_id)`.
- Pull: `query_rules(p_repo_id)` at its default page size, following `continuation` (`after_position`, `after_rule_key`, `expected_revision`) page by page. A page answering `{"error":"conflict"}` restarts the pull; after three restarts the pull fails.
- Each payload maps to a `Rule`.
- `{"error":"not_found"}` covers both a missing repository and one the caller may not read; the error says so and points at `repository_members`.
- devkit checks the shape of `stats_rules`'s answer and refuses one it does not recognise, as the Postgres source refuses an unknown storage version.

### Edits

Each edit pulls the rules afresh first, for the current revision and the id-to-`rule_key` mapping; the cache does not hold remote keys.

- `add`: `put_rule(repo, null, revision, payload)`. The server assigns the id as the hash of `<manual>:<title>`, the same id devkit computes.
- `edit`: `put_rule(repo, rule_key, revision, payload)`. The function requires all eight editable fields, so devkit merges the given flags over the cached rule.
- `remove`: `remove_rule(repo, rule_key, revision)`, a pinned tombstone, as the Postgres source leaves.
- A revision conflict refreshes and retries once, then fails naming the conflict.
- An id naming more than one live rule is refused, listing them, as today.
- `--topic` is refused: `put_rule` rejects `topics` as a reserved field. The error says topics change through `repo-rules-agent`.

### Errors

- SQLSTATE `42501`: the caller's role cannot edit this repository.
- `22023`: the server's message is passed through.
- 401: refresh, then password sign-in, then an error naming `devkit auth supabase`.

## 4. Sign-in

`devkit auth supabase` joins `devkit auth`'s providers. It reads `url` and `callback_port` from `[rules.supabase]`, or takes `--url`.

- Default (browser): reads the enabled providers from `GET /auth/v1/settings` and asks which, or takes `--with <provider>`. Opens `/auth/v1/authorize?provider=<p>` with a PKCE challenge and `redirect_to=http://localhost:<callback_port>/callback`, listens on `127.0.0.1:<callback_port>` for the sign-in alone, refusing when the port registry holds that port or it will not bind (the registry hands out ports from a base and cannot promise one fixed port), and exchanges the code at `/auth/v1/token?grant_type=pkce`.
- `--email`: asks Supabase to email a one-time code (`/auth/v1/otp`) and verifies the code typed in the terminal (`/auth/v1/verify`). No browser.
- `--password`: signs in with `DEVKIT_RULES_SUPABASE_EMAIL` and `DEVKIT_RULES_SUPABASE_PASSWORD`.

Project setup: enable the providers wanted, add `http://localhost:<callback_port>/callback` to the redirect allow list, and give each user a `repository_members` row.

Hooks never open a browser. With no session and no password, requests carry no credentials (`Auth::None`), which is the cloud container mode behind an identity-attaching proxy.

### Software factories

Every factory shares one Supabase user with the `reader` role, its email and password kept once in Doppler. Each factory resolves them with its Doppler service token, signs in, and fills its cache at session start. Rotating the password in Doppler reaches every factory at its next sign-in. Supabase's service-role key is not used: the `repo_rules_api` functions are granted only to `authenticated`, check `auth.uid()`, and the key would bypass row-level security across the whole project.

## Config

```toml
[rules]
source = "supabase"

[rules.supabase]
url = "https://<ref>.supabase.co"
repository = "<repository uuid>"
publishable_key = "sb_publishable_..."
callback_port = 7471           # the default; the allow list needs it exactly
doppler_project = "repo-rules" # optional
doppler_config = "ci"          # optional
```

Secrets `DEVKIT_RULES_SUPABASE_EMAIL` and `DEVKIT_RULES_SUPABASE_PASSWORD` resolve as `DEVKIT_RULES_DATABASE_URL` does: environment, Doppler, `~/.config/devkit/secrets.toml`, with hooks reading the kept copy first. The cache has no config key. `url` is read from the global config alone and ignored in a project's `devkit.toml`, as `[todo.supabase] url` is, so a checkout cannot send your session elsewhere; `DEVKIT_RULES_SUPABASE_URL` overrides it.

## Docs

- Config keys: doc comments on the new fields, regenerated into `schema/devkit-config.json`.
- Flags: clap help on `devkit rules pull` and `devkit auth supabase`.
- `plugin/skills/using-devkit/references/rules.md`: sections for the cache and the Supabase source, including sign-in and the shared factory user.
- AGENTS.md crate table: a `devkit-supabase` row, and the `devkit-postgres` row names the runner.

## Testing

Failing test first throughout.

- Extraction: the existing `devkit-todo-postgres`, `devkit-todo-supabase` and rules Postgres suites pass unchanged on the moved code.
- `devkit-supabase` auth, against a fake HTTP server: password grant, refresh on expiry, 401 then refresh then password then failure, PKCE exchange, session file mode.
- Cache, with `tempfile` scratch and a fake remote: same revision pulls nothing, changed revision replaces, a failed pull keeps the old cache, an unknown format version rebuilds, an edit refreshes, the write hook's no-cache fallback.
- Supabase source: a fake Data API that speaks the contract above, scripted per test. Paging, conflict restarts, the exact request bodies of `put_rule` and `remove_rule`, ambiguous ids, `--topic`, not-found and `42501` refusals each have a test, and the cached Supabase source joins the source parity suite with the fixture rules served as payloads. A run against a real Supabase project carrying the extractor's migrations is a manual check outside CI.
- End to end through the real entry points: `devkit hook session-start`, then make the remote unusable, then `devkit hook pre-tool-use` still injects the governing rules from the cache. `devkit rules pull` reports the revision and count.

## Out of scope

- Caching todo stores.
- SAML SSO sign-in.
- Supabase's OAuth 2.1 server.
- Changing the extractor's schema or functions.

## Open items

- The contract above is read from `repo-rules-agent`'s unmerged storage branch. A change to it before that branch merges means updating this section and the fake.
