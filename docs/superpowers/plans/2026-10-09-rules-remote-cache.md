# Rules remote cache and Supabase source Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve remote rule sources (Postgres, Supabase) from a local SQLite cache refreshed at session start, add a Supabase Data API rules source with headless and browser sign-in, and share the Postgres and Supabase transport code between the rules and todo stores.

**Architecture:** `devkit-postgres` gains the connection runner both Postgres stores copy today; a new `devkit-supabase` crate holds the PostgREST client and Supabase Auth sessions. `devkit-rules` gains a `RuleCache` (SQLite) and a `Cached` member of `Source` wrapping a `Remote` (`PostgresSource` or the new `SupabaseSource`). The `devkit` binary wires refresh into `devkit rules context` (the session hooks), and adds `devkit rules pull` and `devkit auth supabase`.

**Tech Stack:** Rust 2024, tokio-postgres, rusqlite, reqwest (blocking), ring (SHA-256, randomness, HMAC in tests), base64 0.22, serde_json, clap. PostgREST and Postgres in docker for gated tests.

**Spec:** `docs/superpowers/specs/2026-10-09-rules-remote-cache-design.md`

## Global Constraints

- Test gate before every commit: `cargo nextest run --workspace --no-fail-fast`, `cargo test --workspace --doc`, `cargo clippy --workspace --all-targets -- -D warnings`, `devrun task fmt`.
- Commit with `devkit commit --files <paths> --subject '<type(scope): subject>' --coauthor '<model> <noreply@anthropic.com>'`, never `git add` or `git commit`.
- Errors never print a database URL, an access or refresh token, a password, or a key.
- `anyhow` with `.context()`; test scratch from `tempfile`, bound for as long as the path is used; tests poll, never sleep.
- Hook wait is `HOOK_DATABASE_WAIT` (1s), command wait `CLI_DATABASE_WAIT` (15s), both in `src/bin/devkit/rules.rs`; Supabase uses the same two.
- Cache path: `devkit_common::paths::state_dir().join("rules-cache").join(format!("{kind}-{repository}.sqlite"))`, where `kind` is `postgres` or `supabase`. Cache format version `1`.
- Session files: `state_dir().join("supabase-sessions").join(format!("{host}.json"))`, mode `0600`, written by rename from a sibling temp file.
- `[rules.supabase] url` is read from the global config alone (`crate::secret::global_setting(&["rules", "supabase", "url"])`); `DEVKIT_RULES_SUPABASE_URL` overrides it. `callback_port` defaults to `7471`.
- Secrets `DEVKIT_RULES_SUPABASE_EMAIL` and `DEVKIT_RULES_SUPABASE_PASSWORD` resolve through `crate::secret::Secret`, as `DEVKIT_RULES_DATABASE_URL` does.
- Supabase Auth endpoints, pinned against the `supabase/auth` source:
  - `GET /auth/v1/settings` answers `{"external": {"<provider>": bool, ...}}`.
  - `POST /auth/v1/token?grant_type=password` with `{email, password}`; `grant_type=refresh_token` with `{refresh_token}`; `grant_type=pkce` with `{auth_code, code_verifier}`.
  - `POST /auth/v1/otp` with `{email, create_user: false}`; `POST /auth/v1/verify` with `{type: "email", email, token}`.
  - `GET /auth/v1/authorize?provider=<p>&redirect_to=<u>&code_challenge=<c>&code_challenge_method=s256` redirects, after sign-in, to `<u>?code=<auth_code>`.
  - Token answers carry `access_token`, `refresh_token`, `expires_at` (unix seconds). Every auth request carries `apikey: <publishable_key>`.
- `repo_rules_api` contract: the spec's "The `repo_rules_api` contract" section. devkit holds no copy of the extractor's SQL; never vendor it. Tests run against a fake Data API speaking that contract.
- Help text stays ASCII. Parallel work goes through `devkit_common::pool`.

## Review Focus

1. Two sessions starting at once both refresh one cache file: neither fails, and a reader during the write sees the whole old or the whole new index (Task 5, `concurrent_refreshes_leave_one_whole_index`).
2. `[rules.postgres] repository` changes to another UUID, or a cache file holds another source's rules: the stale cache is never served (Task 5, `meta_for_another_repository_reads_as_no_cache`).
3. A session file that is truncated or not JSON: sign-in proceeds as if no session existed, and a hook injects nothing with one stderr line instead of panicking (Task 4, `corrupt_session_file_reads_as_none`).
4. An access token expiring within the clock-skew margin (60s) is refreshed before use, not sent and refused (Task 4, `token_inside_margin_is_refreshed_first`).
5. `devkit rules query <explicit index path>` with a remote source configured reads that file and never touches the cache or the network (Task 7, `explicit_index_bypasses_cache`).

---

### Task 1: Shared Postgres runner in `devkit-postgres`, adopted by the todo store

**Files:**
- Create: `crates/devkit-postgres/src/database.rs`, moved from `crates/devkit-todo-postgres/src/database.rs:415-650` (`Database`, `State`, `block_on`, `TlsSetup`, `is_unreachable`)
- Modify: `crates/devkit-postgres/src/lib.rs` (`mod database; pub use database::{Database, is_unreachable};`), `crates/devkit-postgres/Cargo.toml`
- Modify: `crates/devkit-todo-postgres/src/database.rs` (keeps `SCHEMA`, `CLAIMED`, `UNKNOWN_TODO`, `schema_objects`, `missing_schema`; becomes a wrapper), `crates/devkit-todo-postgres/src/lib.rs`
- Test: `crates/devkit-postgres/tests/database.rs`

**Interfaces:**
- Produces (`devkit_postgres`):
  - `pub struct Database`, not `Arc`-wrapped; callers wrap it.
  - `Database::new(url: &str, wait: Duration, trust: &Trust, label: &'static str) -> anyhow::Result<Database>`; a parse error reads `"the {label} URL does not parse"`.
  - `Database::unusable(reason: impl Display) -> Database`.
  - `finish_by(&self, at: Instant)`, `on_connect_failure(&self, f: impl Fn() + Send + Sync + 'static)`, `target(&self) -> String`.
  - `run<T>(&self, op: impl AsyncFn(&mut Client) -> Result<T>) -> Result<T>`; network, timeout and server errors gain the context `"{label} {target}"`, other errors pass through unchanged.
  - `pub fn is_unreachable(e: &anyhow::Error) -> bool`.
- Produces (`devkit_todo_postgres`): its public API is unchanged. `Database::new(url, wait, trust) -> Result<Arc<Self>>` keeps its signature and wraps `devkit_postgres::Database` with label `"todo database"`. Its `run` passes the shared `run` a closure that retries `op` once after `batch_execute(&SCHEMA)` when `missing_schema` holds, on the same connection and budget, exactly as today.

- [ ] **Step 1: Write the failing tests** in `crates/devkit-postgres/tests/database.rs`. None needs a server.
  - `unusable_fails_every_run_with_its_reason`: `Database::unusable("X is not set").run(async |_| Ok(()))` errors with `"X is not set"`.
  - `unreachable_server_calls_on_connect_failure_and_names_the_label`: `Database::new("postgres://u:secret@127.0.0.1:1/db?sslmode=disable", Duration::from_secs(2), &Trust::default(), "rules database")`; `run` errors; the message contains `"rules database 127.0.0.1:1/db"` and not `"secret"`; the `on_connect_failure` counter is `1`; `is_unreachable(&err)` is true.
  - `a_failure_fails_the_next_run_at_once`: after that failure, a second `run` returns within 100ms with the same reason.
  - `deadline_in_the_past_gives_up_at_once`: after `finish_by(Instant::now())`, `run` errors within 100ms.
- [ ] **Step 2: Run** `cargo nextest run -p devkit-postgres`. Expected: FAIL, `Database` not found.
- [ ] **Step 3: Move the runner** into `crates/devkit-postgres/src/database.rs` with the signatures above, replacing the hard-coded `"todo database"` strings in `run` and `TlsSetup` with `label`.
- [ ] **Step 4: Rewrite `devkit-todo-postgres::Database`** as `struct Database { inner: devkit_postgres::Database }`, delegating `finish_by`, `on_connect_failure`, `target`, `check`, `update_schema` and `run`; re-export `devkit_postgres::is_unreachable`.
- [ ] **Step 5: Run** `cargo nextest run -p devkit-postgres -p devkit-todo-postgres -p devkit`, then with `eval "$(crates/devkit-todo-postgres/testdb/up.sh)"` run `cargo nextest run -p devkit-todo-postgres -p devkit --test todo_postgres`. Expected: PASS, the gated suites unchanged.
- [ ] **Step 6: Commit** `refactor(postgres): share the connection runner`.

### Task 2: Rules Postgres source on the shared runner

**Files:**
- Modify: `crates/devkit-rules/src/postgres.rs:30-160` (delete its `Database`, `connect`, `NoAnswer`, `from_database`; keep the queries)
- Modify: `src/bin/devkit/rules.rs:78-122` (`open_database`), `src/bin/devkit/doctor.rs:745-810`
- Test: `tests/rules_postgres.rs`; `crates/devkit-rules/tests/source_parity.rs` must stay green

**Interfaces:**
- Consumes: Task 1 `devkit_postgres::Database`, with label `"rules database"`.
- Produces:
  - `PostgresSource::new(db: Arc<devkit_postgres::Database>, repository: Option<&str>, repo: &Path)`.
  - `crate::rules::open_database(...) -> (Arc<devkit_postgres::Database>, secrets::Source)`, calling `db.on_connect_failure(...)` on the shared value instead of the builder.
  - `PostgresSource::revision(&self) -> Result<i64>`: after `check_version`, one statement, `SELECT revision FROM repo_rules.repositories WHERE repo_id = $1::uuid`.
  - `PostgresSource::pull(&self) -> Result<(i64, RuleIndex)>`: reads the revision inside the same REPEATABLE READ transaction `read` uses today. `read` becomes `self.pull().map(|(_, index)| Some(index))`.

- [ ] **Step 1: Write the failing test** `revision_counts_each_edit` in `tests/rules_postgres.rs`, gated on `DEVKIT_TEST_POSTGRES_URL`: import a store; `revision()` is `r0`; `add` a rule; `revision()` is `r0 + 1` and `pull().0 == r0 + 1`.
- [ ] **Step 2: Run**, testdb up, `cargo nextest run -p devkit --test rules_postgres`. Expected: FAIL, no `revision`.
- [ ] **Step 3: Switch `PostgresSource`** to `Arc<devkit_postgres::Database>` and add `revision` and `pull`. Ops passed to `run` become `AsyncFn`, cloning what they capture.
- [ ] **Step 4: Run** the gate plus the gated rules suites. Expected: PASS, `source_parity` unchanged.
- [ ] **Step 5: Commit** `refactor(rules): use the shared postgres runner`.

### Task 3: `devkit-supabase` crate, adopted by the todo store

**Files:**
- Create: `crates/devkit-supabase/Cargo.toml`, `src/lib.rs`, `src/api.rs`, `src/fakehttp.rs` (behind feature `test-support`)
- Modify: `crates/devkit-todo-supabase/src/{api.rs,store.rs,activity.rs,lib.rs}`, root `Cargo.toml` (workspace member and dependency), `src/bin/devkit/todo/store.rs:316-360`, `AGENTS.md` crate table
- Test: `crates/devkit-supabase/tests/api.rs`

**Interfaces:**
- Produces (`devkit_supabase`):
  - `pub enum Auth { None, Key(String) }`. Task 4 adds `User`.
  - `Api::new(url: &str, schema: &'static str, auth: Auth, wait: Duration, label: &'static str) -> Result<Api>`, `Api::unusable(reason: impl Display, label: &'static str) -> Api`.
  - `url(&self) -> &str`, `finish_by(&self, at: Instant)`, `on_rejected(&self, f: impl Fn() + Send + Sync + 'static)`.
  - `select<T: DeserializeOwned>(&self, table: &str, query: &[(&str, String)]) -> Result<Vec<T>>`, `select_page<T>(...) -> Result<(Vec<T>, Option<usize>)>`, `call(&self, function: &str, args: &serde_json::Value) -> Result<Response>`, `body<T: DeserializeOwned>(&self, resp: Response) -> Result<T>`.
  - `#[derive(Debug)] pub struct Refused { pub status: StatusCode, pub code: Option<String>, pub message: String, pub details: Option<String>, label: &'static str, url: String }`, implementing `Error`, displayed as `"{label} {url} answered {status}: {message}"`. Every non-2xx answer comes back as `anyhow::Error::new(Refused { .. })`.
  - `pub fn is_unreachable(e: &anyhow::Error) -> bool`.
  - Feature `test-support`: `pub mod fakehttp` with `FakeServer::start(answers: Vec<(u16, serde_json::Value)>) -> FakeServer`, serving the answers in order on `127.0.0.1:0`, `url(&self) -> String`, `requests(&self) -> Vec<Recorded>`, and `Recorded { method: String, path_and_query: String, headers: Vec<(String, String)>, body: String }`.
- Produces (`devkit_todo_supabase`): its public API is unchanged. `Api` becomes `devkit_supabase::Api` with schema `"devkit"` and label `"todo API"`. A private `fn todo_error(e: anyhow::Error) -> anyhow::Error` downcasts `Refused` and maps `DK001` to `Claimed`, `DK002` to its message, and appends today's `advice(code)` text; every `call` and `select*` in `store.rs` and `activity.rs` maps its error through it.
- Produces (binary): `todo::store::open_api` passes `"devkit"`, `"todo API"`, and `Auth::Key(key)` when a key resolved, else `Auth::None`.

- [ ] **Step 1: Write the failing tests** in `crates/devkit-supabase/tests/api.rs`, using `FakeServer`:
  - `key_goes_as_apikey_and_bearer`: `Auth::Key("k".into())`, schema `"repo_rules_api"`, `call("f", &json!({}))`; the request has `apikey: k`, `authorization: Bearer k`, `content-profile: repo_rules_api`, path `/rest/v1/rpc/f`.
  - `no_auth_sends_no_credentials`: with `Auth::None`, neither header is present.
  - `refusal_carries_code_message_details`: answer `400 {"code":"42501","message":"m","details":"d"}`; `err.downcast_ref::<Refused>()` carries those three; with label `"todo API"`, `err.to_string()` starts with `"todo API "`.
  - `a_401_runs_on_rejected`: answer `401 {}`; the `on_rejected` counter is `1`.
  - `one_unanswered_request_fails_the_rest_at_once`: URL `http://127.0.0.1:1`; the first `call` errors with `is_unreachable`; the second returns within 100ms.
- [ ] **Step 2: Run** `cargo nextest run -p devkit-supabase --features test-support`. Expected: FAIL, crate missing.
- [ ] **Step 3: Create the crate** from `devkit-todo-supabase/src/api.rs`: move `Api`, `Unreachable`, `budget`, `send`, `endpoint`, `body`; `schema` replaces the `SCHEMA` constant in `Accept-Profile` and `Content-Profile`; `refusal` builds `Refused` and runs `on_rejected` on a 401, with nothing todo-specific left in it.
- [ ] **Step 4: Rewrite `devkit-todo-supabase`** on it, with `todo_error` as above, and update `todo::store::open_api`.
- [ ] **Step 5: Add** the AGENTS.md crate table row: `devkit-supabase` | the Supabase project client the Data-API-backed stores share: PostgREST requests under a schema, refusals, and Supabase Auth sessions.
- [ ] **Step 6: Run** the gate, then, testdb up, `cargo nextest run -p devkit-todo-supabase`. Expected: PASS, unchanged.
- [ ] **Step 7: Commit** `refactor(supabase): extract the data api client`.

### Task 4: Supabase Auth sessions

**Files:**
- Create: `crates/devkit-supabase/src/auth.rs`
- Modify: `crates/devkit-supabase/src/{lib.rs,api.rs}`, `crates/devkit-supabase/Cargo.toml` (`base64`, `ring`)
- Test: `crates/devkit-supabase/tests/auth.rs`

**Interfaces:**
- Produces (`devkit_supabase::auth`):
  - `#[derive(Clone, Serialize, Deserialize)] pub struct Session { pub access_token: String, pub refresh_token: String, pub expires_at: i64 }`.
  - `pub struct Credentials { pub email: String, pub password: String }`.
  - `pub struct Client` with `Client::new(url: &str, publishable_key: &str, wait: Duration) -> Client` and:
    - `password(&self, c: &Credentials) -> Result<Session>`
    - `refresh(&self, refresh_token: &str) -> Result<Session>`
    - `pkce(&self, auth_code: &str, verifier: &str) -> Result<Session>`
    - `send_code(&self, email: &str) -> Result<()>`, `verify_code(&self, email: &str, code: &str) -> Result<Session>`
    - `providers(&self) -> Result<Vec<String>>`: sorted names whose `external` value is `true`, excluding `email`, `phone` and `anonymous_users`.
    - `authorize_url(&self, provider: &str, redirect_to: &str, challenge: &str) -> String`
    - `publishable_key(&self) -> &str`, `url(&self) -> &str`
  - `pub struct Pkce { pub verifier: String, pub challenge: String }`, `Pkce::new() -> Pkce`: 32 random bytes from `ring::rand::SystemRandom`, base64url without padding; `challenge` is base64url of SHA-256 of `verifier`.
  - `pub struct SessionFile`, `SessionFile::for_url(state_dir: &Path, url: &str) -> SessionFile`, `load(&self) -> Option<Session>` (`None` when missing or unparsable), `save(&self, s: &Session) -> Result<()>` (temp file then rename, mode `0600` on unix).
  - `pub struct Sessions`, `Sessions::new(client: Client, file: SessionFile, credentials: Box<dyn Fn() -> Option<Credentials> + Send + Sync>) -> Sessions`, `access_token(&self) -> Result<String>`, `renew(&self) -> Result<String>`, `client(&self) -> &Client`.
- `access_token`: loads the session; when `expires_at - now < 60` it calls `renew`; with no session it signs in with the credentials; with neither, it errors `"not signed in to {url}: run `devkit auth supabase`"`.
- `renew`: refreshes with the stored refresh token; on failure, signs in with the password when credentials resolve; saves and returns the new access token; otherwise the same not-signed-in error.
- `devkit_supabase::Auth::User(Arc<Sessions>)`: `apikey` is the client's publishable key and the bearer is `access_token()`. A 401 runs `on_rejected`, calls `renew` and retries the request once.

- [ ] **Step 1: Write the failing tests** in `crates/devkit-supabase/tests/auth.rs`, with `FakeServer` and a tempfile state directory:
  - `password_grant_posts_credentials_and_saves_the_session`: path `/auth/v1/token?grant_type=password`, body `{"email":"f@x","password":"p"}`, header `apikey: pk`; the session file holds the answer; on unix its mode is `0o600`.
  - `token_inside_margin_is_refreshed_first`: a saved session with `expires_at = now + 30`; `access_token()` sends `grant_type=refresh_token` with the stored refresh token and returns the new access token.
  - `failed_refresh_falls_back_to_password`: refresh answers `400`, password answers a session; `access_token()` returns its token.
  - `no_session_no_credentials_names_the_command`: the error contains `devkit auth supabase`.
  - `corrupt_session_file_reads_as_none`: with contents `"{not json"`, `SessionFile::load()` is `None`.
  - `api_retries_once_after_a_401`: rpc answers `401`, refresh answers a session, rpc answers `200 {}`; `call` succeeds; exactly two rpc requests were made, the second with the new bearer.
  - `providers_lists_enabled_external_providers`: `/settings` answers `{"external":{"github":true,"google":false,"email":true}}`; the result is `["github"]`.
  - `pkce_challenge_is_s256_of_verifier`: the decoded `challenge` equals SHA-256 of `verifier`, and `verifier.len() == 43`.
  - `authorize_url_carries_pkce`: contains `provider=github`, `code_challenge_method=s256`, and the url-encoded `redirect_to`.
- [ ] **Step 2: Run** `cargo nextest run -p devkit-supabase --features test-support --test auth`. Expected: FAIL.
- [ ] **Step 3: Implement** `auth.rs` and `Auth::User` in `api.rs`.
- [ ] **Step 4: Run** the tests and the gate. Expected: PASS.
- [ ] **Step 5: Commit** `feat(supabase): sign in and keep auth sessions`.

### Task 5: `RuleCache`

**Files:**
- Create: `crates/devkit-rules/src/cache.rs`
- Modify: `crates/devkit-rules/src/lib.rs`, `crates/devkit-rules/src/model.rs` (`RuleFile` and `RuleIndex` also derive `Serialize`)
- Test: `crates/devkit-rules/tests/cache.rs`

**Interfaces:**
- Produces (`devkit_rules::cache`):
  - `pub struct CacheKey { pub kind: &'static str, pub repository: String }`.
  - `pub struct Meta { pub revision: i64, pub pulled_at: i64 }`.
  - `pub struct RuleCache`, `RuleCache::new(path: PathBuf, key: CacheKey)`, `RuleCache::at_state_dir(state_dir: &Path, key: CacheKey) -> RuleCache`, `path(&self) -> &Path`.
  - `meta(&self) -> Option<Meta>`: `None` when the file is missing, its format is not `1`, or its `kind` or `repository` differ from the key.
  - `read(&self) -> Result<Option<RuleIndex>>`: `None` on the same conditions; an unreadable row is an error naming the file.
  - `write(&self, revision: i64, index: &RuleIndex) -> Result<()>`: creates the parent directory and tables when missing, then in one `BEGIN IMMEDIATE` transaction deletes every row and inserts `meta`, `files` and `rules`.
- Layout: `meta(singleton INTEGER PRIMARY KEY CHECK (singleton = 1), format INTEGER NOT NULL, kind TEXT NOT NULL, repository TEXT NOT NULL, repo TEXT NOT NULL, revision INTEGER NOT NULL, pulled_at INTEGER NOT NULL)`, `files(position INTEGER PRIMARY KEY, json TEXT NOT NULL)`, `rules(position INTEGER PRIMARY KEY, json TEXT NOT NULL)`. Every connection sets `journal_mode = DELETE` and `busy_timeout = 2000`.

- [ ] **Step 1: Write the failing tests** in `crates/devkit-rules/tests/cache.rs`, loading `tests/fixtures/index.json` as the `RuleIndex`:
  - `round_trips_the_index`: `write(7, &index)`; `read()` equals `index` compared through `serde_json::to_value`; `meta().unwrap().revision == 7`.
  - `missing_file_reads_as_none`.
  - `meta_for_another_repository_reads_as_no_cache`: written under repository `a`, opened at the same path with repository `b`; `meta()` and `read()` are `None`.
  - `unknown_format_reads_as_none`: after `UPDATE meta SET format = 99` through rusqlite, `read()` is `None`; a new `write` succeeds and `read()` is `Some`.
  - `concurrent_refreshes_leave_one_whole_index`: two threads each `write` a different index 20 times while a third `read`s 50 times; no call errors, and every read equals one of the two indexes.
- [ ] **Step 2: Run** `cargo nextest run -p devkit-rules --test cache`. Expected: FAIL.
- [ ] **Step 3: Implement** `cache.rs`.
- [ ] **Step 4: Run** the tests and the gate. Expected: PASS.
- [ ] **Step 5: Commit** `feat(rules): add a sqlite rules cache`.

### Task 6: `CachedSource` and `Remote`

**Files:**
- Create: `crates/devkit-rules/src/remote.rs`
- Modify: `crates/devkit-rules/src/source.rs`, `crates/devkit-rules/Cargo.toml` (feature `test-remote`)
- Test: `crates/devkit-rules/tests/cached_source.rs`

**Interfaces:**
- Consumes: Task 2 `PostgresSource::{revision, pull}`; Task 5 `RuleCache`.
- Produces:
  - `#[ambassador::delegatable_trait] pub trait RemoteRules: RuleSource { fn revision(&self) -> Result<i64>; fn pull(&self) -> Result<(i64, RuleIndex)>; }` in `remote.rs`.
  - `pub enum Remote { Postgres(PostgresSource) }`, delegating `RuleSource` and `RemoteRules`. Task 8 adds `Supabase`. Under feature `test-remote`, `Remote::Fake(FakeRemote)` with `pub struct FakeRemote { pub revision: Cell<i64>, pub index: RefCell<RuleIndex>, pub fail: Cell<bool>, pub pulls: Cell<usize> }`, whose `edit` changes the named rule's title and bumps `revision`.
  - `pub struct CachedSource`, `CachedSource::new(cache: RuleCache, remote: Remote)`.
  - `pub struct Refreshed { pub revision: i64, pub rules: usize, pub pulled: bool }`.
  - `CachedSource::refresh(&self, force: bool) -> Result<Refreshed>`: calls `remote.revision()`, then pulls and writes the cache when `force` is set, there is no cache, or the revision differs from `meta`.
  - `impl RuleSource for CachedSource`:
    - `read`: `cache.read()`; when that is `None`, `remote.pull()`, writing nothing.
    - `add`, `edit`, `remove`: the remote, then `refresh(true)`; a refresh failure after a successful edit is one stderr line and the edit still returns `Ok`.
    - `location`: `"{remote location} (cache {path})"`; `kind`: the remote's.
  - `Source::Cached(CachedSource)`; `Source::refresh(&self, force: bool) -> Option<Result<Refreshed>>`, `None` for `Json` and `Sqlite`; `Source::present` for `Cached` is `cache.meta().is_some()` or the remote's check.

- [ ] **Step 1: Write the failing tests** in `crates/devkit-rules/tests/cached_source.rs`, with a tempfile cache:
  - `unchanged_revision_pulls_nothing`: refresh twice; `pulls == 1`; the second `Refreshed.pulled` is `false`.
  - `changed_revision_replaces_the_cache`: bump the revision and change the index; `refresh(false)` pulls; `read()` returns the new index.
  - `failed_pull_keeps_the_old_cache`: with `fail` set, `refresh` errors and `read()` returns the old index.
  - `edit_refreshes_the_cache`: `edit(id, fields)` with a new title; `read()` shows it.
  - `no_cache_reads_the_remote_and_writes_nothing`: `read()` returns the fake's index and no cache file exists.
  - `force_pulls_at_the_same_revision`.
- [ ] **Step 2: Run** `cargo nextest run -p devkit-rules --features test-remote --test cached_source`. Expected: FAIL.
- [ ] **Step 3: Implement** `remote.rs` and the `source.rs` changes.
- [ ] **Step 4: Run** the tests and the gate. Expected: PASS.
- [ ] **Step 5: Commit** `feat(rules): read remote sources through the cache`.

### Task 7: The cache in the binary, and `devkit rules pull`

**Files:**
- Modify: `crates/devkit-rules/src/source.rs` (`for_checkout` wraps `Postgres` in `Cached`)
- Modify: `src/bin/devkit/rules.rs` (`Pull` command; refresh in `context_cmd`, `query_cmd`, `stats_cmd`), `src/bin/devkit/doctor.rs` (`rules_cache` row)
- Modify: `plugin/skills/using-devkit/references/rules.md` ("The cache" section)
- Test: `tests/rules_cache.rs`

**Interfaces:**
- Consumes: Task 6 `Source::refresh`, `CachedSource`, `RuleCache::at_state_dir`.
- Produces:
  - `Source::for_checkout(settings, checkout, database, state_dir: &Path)`: for `RulesSource::Postgres`, `Source::Cached(CachedSource::new(RuleCache::at_state_dir(state_dir, CacheKey { kind: "postgres", repository }), Remote::Postgres(..)))`. `crate::rules::source` passes `devkit_common::paths::state_dir()`.
  - `RulesCommand::Pull`, help `Refresh the local cache of a remote rules source.`, printing `"{location}: revision {revision}, {rules} rules"`.
- Refresh points:
  - `context_cmd`: `refresh(false)` with `Reader::Hook`; an error is one stderr line, then it reads the cache.
  - `query_cmd`, `stats_cmd` with no explicit path: `refresh(false)` with `Reader::Cli`; an error is one stderr line, and the read errors only when nothing loads.
  - The write hook (`src/bin/devkit/hook/rules.rs:172`) is unchanged: `load()` already reads the cache, then the remote.
  - `Pull`: `refresh(true)` with `Reader::Cli`; errors exit non-zero; on the `file` source it errors `"rules source `file` has no cache to pull"`.
- Doctor row `rules_cache`: `Check::Ok("{path}: revision {r}, pulled {age} ago")`, or `Check::Warn("no cache yet: run `devkit rules pull`")`; absent for the `file` source.

- [ ] **Step 1: Write the failing tests** in `tests/rules_cache.rs`, gated on `DEVKIT_TEST_POSTGRES_URL`, with a project config pointing at an imported store and `XDG_STATE_HOME` and `HOME` in a tempfile:
  - `session_hook_fills_the_cache_and_writes_read_it_offline`: run `devkit rules context`; then set `DEVKIT_RULES_DATABASE_URL=postgres://x@127.0.0.1:1/x?sslmode=disable`; `devkit hook pre-tool-use` with a `Write` payload for a governed file prints the governing rule's title.
  - `pull_prints_revision_and_count`: stdout matches `revision \d+, \d+ rules`.
  - `pull_on_file_source_errors`: non-zero exit, stderr contains `has no cache to pull`.
  - `explicit_index_bypasses_cache`: with the unreachable URL and no cache, `devkit rules query <copy of tests/fixtures/index.json>` succeeds and no cache file appears.
- [ ] **Step 2: Run**, testdb up, `cargo nextest run -p devkit --test rules_cache`. Expected: FAIL.
- [ ] **Step 3: Implement** the wiring above.
- [ ] **Step 4: Write "The cache"** in `rules.md`: where it lives, when it refreshes (the spec's refresh table as prose), `devkit rules pull`, the write hook's fallback with no cache, and the `rules_cache` doctor row.
- [ ] **Step 5: Run** the gate and the gated suites. Expected: PASS.
- [ ] **Step 6: Commit** `feat(rules): cache remote rules and add rules pull`.

### Task 8: `SupabaseSource` in `devkit-rules`

**Files:**
- Create: `crates/devkit-rules/src/supabase.rs`
- Create: `crates/devkit-rules/tests/common/fakeapi.rs`, `crates/devkit-rules/tests/supabase_source.rs`
- Modify: `crates/devkit-rules/src/{lib.rs,remote.rs}`, `crates/devkit-rules/Cargo.toml` (`devkit-supabase`, and `devkit-supabase` with `test-support` as a dev-dependency), `crates/devkit-rules/tests/source_parity.rs`

**Interfaces:**
- Consumes: Tasks 3 and 4 `devkit_supabase::Api` (schema `"repo_rules_api"`, label `"rules API"`) and `fakehttp::FakeServer`; Task 6 `RemoteRules`; the spec's "The `repo_rules_api` contract" section, which is the only description of the server devkit has.
- Produces:
  - `SupabaseSource::new(api: Arc<Api>, repository: Option<&str>, repo: &Path)`; `Remote::Supabase(SupabaseSource)`; `kind()` is `"supabase"`.
  - `revision`: `call("stats_rules", {"p_repo_id": repo})`. An answer with neither an integer `revision` nor an `error` is refused: `"rules API {url} answered an unrecognised stats_rules shape"`.
  - `pull`: `call("query_rules", {"p_repo_id": repo, "p_limit": page})`, then again with `p_after_position`, `p_after_rule_key` and `p_expected_revision` from `continuation` until it is `null`. `{"error":"conflict"}` restarts the pull; after 3 restarts it errors `"rules changed during the pull 3 times; try again"`. `page` defaults to `100`, the function's own default; `#[doc(hidden)] pub fn with_page_size(self, n: u32) -> Self` sets it for tests.
  - Payload to `Rule`: `id`, `title`, `description`, `category`, `scope` to `scope_raw`, `severity` to `severity_raw`, `directory`, `source_file`, `tasks`, `languages`, `topics` from the payload's extra keys (empty when absent), `pinned`, `removed`. `files` are the distinct `source_file`s in order, with `tier = 0`.
  - `{"error":"not_found"}`: `"repository {uuid} is not in the rules API, or you have no access to it; check repository_members"`.
  - `add`, `edit`, `remove`: pull afresh; resolve `id` to one live `rule_key`, refusing several and listing them as `postgres.rs` does; call `put_rule` or `remove_rule` with `p_expected_revision`. `edit` sends exactly `title, description, category, scope, severity, directory, tasks, languages`; `add` sends the same eight. `Fields::topics` set is refused before any request: `"topics change through repo-rules-agent on the supabase source"`. A `{"error":"conflict"}` answer pulls again and retries once.
  - `Refused` codes: `42501` reads `"your role cannot edit repository {uuid}"`; `22023` passes the server's message through.
- `tests/common/fakeapi.rs`: `fn payloads(index: &RuleIndex) -> Vec<Value>`, each live rule as the contract's payload, with a fresh `rule_key` and its `topics` as an extra field; `fn page(revision: i64, rules: &[Value], continuation: Option<(i64, &str)>) -> Value`; `fn stats(revision: i64) -> Value`. Tests script `FakeServer` answers with these, so every answer has the contract's shape.

- [ ] **Step 1: Write the failing tests** in `crates/devkit-rules/tests/supabase_source.rs`, each against a scripted `FakeServer` and an `Api` built on `Auth::None`:
  - `pull_pages_through_every_rule`: page size 2, the fixture's payloads served two per page with continuations; `pull().1` equals the fixture's live rules; request 2's body carries request 1's continuation as `p_after_position`, `p_after_rule_key`, `p_expected_revision`.
  - `revision_reads_stats_rules`: `stats(5)` gives `5`.
  - `unrecognised_stats_shape_is_refused`: answer `{"ok":true}`; the error contains `unrecognised stats_rules shape`.
  - `conflict_mid_pull_restarts`: page 1 with a continuation, page 2 `{"error":"conflict","revision":8,"generation":"g"}`, then one full page; `pull` succeeds after 3 rpc calls.
  - `three_conflicts_fail_the_pull`.
  - `add_sends_null_key_and_eight_fields`: after a pull, `add` sends `put_rule` with `p_rule_key: null`, the pulled `p_expected_revision`, and a `p_payload` whose keys are exactly the eight; it returns the first 12 hex characters of SHA-256 of `"<manual>:<title>"`, computed in the test with `ring::digest`.
  - `edit_merges_flags_over_the_pulled_rule`: `edit(id, title only)` sends the pulled rule's other seven fields unchanged and the new title, with that rule's `rule_key`.
  - `remove_sends_rule_key_and_revision`.
  - `edit_conflict_pulls_and_retries_once`: `put_rule` answers a conflict, the next pull and `put_rule` succeed; exactly two `put_rule` calls.
  - `reader_cannot_edit`: `put_rule` answers `403 {"code":"42501","message":"repository edit forbidden"}`; the error contains `cannot edit repository`.
  - `topics_are_refused`: `edit` with `topics: Some(..)` errors with `topics change through repo-rules-agent` and the fake recorded no request.
  - `ambiguous_id_is_refused`: two payloads share an `id`; `edit` errors listing both `rule_key`s.
  - `unknown_repository_is_not_found`: `stats_rules` answers `{"error":"not_found"}`; the error contains `repository_members`.
  - In `source_parity.rs`, `sources()` adds `("supabase", Source::Cached(..Remote::Supabase(..)))` over a `FakeServer` that answers `stats(1)` then one page of the fixture's payloads; `every_source_reads_the_same_records_the_same_way` passes for it. It is not gated: it needs no database.
- [ ] **Step 2: Run** `cargo nextest run -p devkit-rules --test supabase_source --test source_parity`. Expected: FAIL.
- [ ] **Step 3: Implement** `supabase.rs` and `fakeapi.rs`.
- [ ] **Step 4: Run** the tests and the gate. Expected: PASS.
- [ ] **Step 5: Commit** `feat(rules): read and edit rules through the supabase api`.

### Task 9: `source = "supabase"` in config and the binary

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (`RulesSource::Supabase`, `RulesSupabaseConfig`, `RulesConfig::supabase`), `schema/devkit-config.json` (regenerate with `DEVKIT_UPDATE_SCHEMA=1 cargo test`)
- Modify: `crates/devkit-rules/src/source.rs` (`for_checkout` also takes an API opener), `src/bin/devkit/rules.rs` (`open_api`), `src/bin/devkit/doctor.rs`
- Modify: `plugin/skills/using-devkit/references/rules.md` ("The Supabase source")
- Test: the config doctest, `tests/rules_supabase.rs`

**Interfaces:**
- Produces (`devkit_config`): `pub struct RulesSupabaseConfig { pub url: Option<String>, pub repository: Option<String>, pub publishable_key: Option<String>, pub callback_port: u16, pub doppler_project: Option<String>, pub doppler_config: Option<String> }`, `#[serde(default, deny_unknown_fields)]`, `callback_port` defaulting to `7471`. The doc comment on `url` says it is read from the global config alone and that `DEVKIT_RULES_SUPABASE_URL` overrides it. The doctest parses the spec's Config block and asserts `source == RulesSource::Supabase`, and `callback_port == 7471` when omitted.
- Produces (binary): `crate::rules::open_api(config: &RulesSupabaseConfig, wait: Duration, lookup: SecretLookup) -> (Arc<Api>, ApiSources)` with `ApiSources { url: secrets::Source, credentials: secrets::Source }`.
  - URL: `DEVKIT_RULES_SUPABASE_URL`, else `global_setting(&["rules", "supabase", "url"])`, else `Api::unusable("DEVKIT_RULES_SUPABASE_URL is not set, nor [rules.supabase] url in the global config", "rules API")`.
  - Auth: with no `publishable_key`, `Auth::None`. Otherwise `Auth::User(Arc::new(Sessions::new(Client::new(url, key, wait), SessionFile::for_url(&state_dir(), url), credentials)))`, where `credentials` resolves both secrets through `Secret { var, cache_dir: state_dir().join("rules-supabase-email") }` and `state_dir().join("rules-supabase-password")`, with `lookup` and the `[rules.supabase]` Doppler scope. A 401 forgets the kept copies through `on_rejected`.
  - `Source::for_checkout` for `RulesSource::Supabase` returns `Source::Cached(CachedSource::new(RuleCache::at_state_dir(.., CacheKey { kind: "supabase", repository }), Remote::Supabase(..)))`.
- Doctor row `rules_api`: where the URL and the credentials resolved from, whether a session file exists and when it expires, and whether `revision()` answered, or its error. Never a key, token or password.

- [ ] **Step 1: Write the failing tests:**
  - The config doctest above.
  - `context_hook_reads_supabase_rules` in `tests/rules_supabase.rs`, against a `FakeServer` scripted with Task 8's `fakeapi` helpers (copy them into `tests/common/` or expose them from `devkit-rules` under feature `test-remote`): `DEVKIT_RULES_SUPABASE_URL` points at the fake; the project `devkit.toml` has `source = "supabase"`, `repository` and any `publishable_key`; a session file pre-written under `$XDG_STATE_HOME/devkit` holds any token with `expires_at = now + 3600`. `devkit rules context` prints the fixture's `must` rules, every rpc request carried that token as its bearer, and `devkit rules pull` prints `revision`.
  - `project_url_is_ignored`: `url` only in the project `devkit.toml`; `devkit rules pull` errors with `[rules.supabase] url in the global config`.
- [ ] **Step 2: Run** them. Expected: FAIL.
- [ ] **Step 3: Implement** the config, `open_api`, `for_checkout` and the doctor row, and regenerate the schema.
- [ ] **Step 4: Write "The Supabase source"** in `rules.md`: config, the three ways to authenticate, the shared reader user for software factories with its credentials in Doppler, why the service-role key is not used, `--topic`, and failures.
- [ ] **Step 5: Run** the gate and the gated suites. Expected: PASS.
- [ ] **Step 6: Commit** `feat(rules): add the supabase rules source`.

### Task 10: `devkit auth supabase`

**Files:**
- Move: `src/bin/devkit/auth.rs` to `src/bin/devkit/auth/mod.rs`
- Create: `src/bin/devkit/auth/supabase.rs`
- Modify: `src/bin/devkit/main.rs` (`Provider::Supabase`; `Cmd::Auth` gains `#[command(flatten)] supabase: SupabaseLogin`)
- Modify: `plugin/skills/using-devkit/references/rules.md` (sign-in subsection)
- Test: `tests/auth_supabase.rs`

**Interfaces:**
- Consumes: Task 4 `auth::{Client, Pkce, SessionFile, Credentials}`; Task 9 `RulesSupabaseConfig` and its URL resolution, factored into `crate::rules::supabase_url() -> Result<String>`.
- Produces: `pub struct SupabaseLogin { #[arg(long)] with: Option<String>, #[arg(long)] email: bool, #[arg(long, conflicts_with = "email")] password: bool, #[arg(long)] url: Option<String> }`. Any of these with another provider is refused: `"--with, --email, --password and --url apply to `devkit auth supabase` alone"`. `--token` with `supabase` is refused.
- Behaviour:
  - No `publishable_key`: refused, `"[rules.supabase] publishable_key is not set"`.
  - `--password`: resolves `Credentials` with `SecretLookup::Doppler`, calls `Client::password`, saves, prints `"signed in to {url} as {email}"`.
  - `--email`: needs an interactive terminal; prompts for the address, `send_code`, prompts for the code, `verify_code`, saves.
  - Default: `providers()`. `--with` must name one of them, else the error lists them. Without `--with`, it takes the only provider or prompts with a numbered list. It refuses when `devkit_ports::registry::load()` holds `callback_port`, naming the holder; binds `127.0.0.1:{callback_port}`, and on failure errors `"port {p} is in use; set [rules.supabase] callback_port"`; makes a `Pkce`; prints `authorize_url(provider, "http://localhost:{p}/callback", challenge)` and tries `xdg-open`, `open` or `cmd /c start` on it, ignoring failures; accepts one request within 5 minutes. A `code` parameter is exchanged with `pkce`, the session saved, and the browser answered `"Signed in. You can close this tab."`; an `error_description` parameter exits non-zero with it and the hint `add http://localhost:{p}/callback to the project's redirect allow list` when it mentions the redirect.

- [ ] **Step 1: Write the failing tests** in `tests/auth_supabase.rs`, against `FakeServer` (`devkit-supabase` feature `test-support` as a dev-dependency):
  - `password_sign_in_saves_a_session`: with `DEVKIT_RULES_SUPABASE_URL` set to the fake, both secrets in the environment and a project `publishable_key`, `devkit auth supabase --password` exits 0 and the session file under `$XDG_STATE_HOME/devkit` exists.
  - `browser_flow_exchanges_the_code`: `callback_port` set to a free port; spawn `devkit auth supabase --with github` with `PATH` lacking any opener; poll until the port listens; GET `http://127.0.0.1:{p}/callback?code=abc`; the process exits 0 and the fake recorded `grant_type=pkce` with `"auth_code":"abc"`.
  - `unknown_provider_lists_the_enabled_ones`: `/settings` enables `github`; `--with gitlab` exits non-zero naming `github`.
  - `flags_refused_for_other_providers`: `devkit auth linear --with github` exits non-zero with the refusal text.
- [ ] **Step 2: Run** `cargo nextest run -p devkit --test auth_supabase`. Expected: FAIL.
- [ ] **Step 3: Implement** `auth/supabase.rs` and the clap changes.
- [ ] **Step 4: Write the sign-in subsection** in `rules.md`: the three modes, the allow-list entry, the shared factory user, and that hooks never open a browser.
- [ ] **Step 5: Run** the gate. Expected: PASS.
- [ ] **Step 6: Commit** `feat(auth): sign in to supabase for rules`.

## Unresolved questions

- Will the `repo_rules_api` contract change before `repo-rules-agent`'s storage branch merges? A change means updating the spec's contract section and Task 8's fake.
- The source is never run against a real Supabase project in CI. Before relying on it, run `devkit rules pull` and an `edit` against a project carrying the extractor's migrations, signed in as a `reader` and as an `editor`.
