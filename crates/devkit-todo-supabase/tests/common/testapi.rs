//! The test API: PostgREST at `DEVKIT_TEST_SUPABASE_URL`, behind `/rest/v1/`
//! as on Supabase, serving the database `DEVKIT_TEST_POSTGRES_URL` names.
//! `crates/devkit-todo-postgres/testdb/up.sh` starts both.

use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use devkit_todo::{Edit, Filter, Holder, TodoStore};
use devkit_todo_supabase::{Api, SupabaseStore};

/// The role and grants the todo reference sets up, for the role PostgREST
/// serves a request that carries no key as. PostgREST here logs in as a
/// superuser, so the grant to Supabase's `authenticator` is left out.
const GRANTS: &str = "
DO $$ BEGIN
    CREATE ROLE devkit_agent NOLOGIN;
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;
GRANT USAGE ON SCHEMA devkit TO devkit_agent;
GRANT SELECT, INSERT, UPDATE, DELETE ON devkit.todos TO devkit_agent;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA devkit TO devkit_agent;
ALTER DEFAULT PRIVILEGES IN SCHEMA devkit GRANT EXECUTE ON FUNCTIONS TO devkit_agent;
NOTIFY pgrst, 'reload schema';
";

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The database PostgREST serves.
pub fn direct_url() -> Option<String> {
    var("DEVKIT_TEST_POSTGRES_URL")
}

/// Whether the API reads devkit's table and calls its functions.
fn serves(api: &str) -> Result<(), anyhow::Error> {
    let probe = SupabaseStore::new(Api::new(api, None, Duration::from_secs(5))?.into(), "probe");
    probe.list(&Filter::all())?;
    probe.apply(&Edit::ReleaseAll {
        holder: Holder::new("probe"),
    })?;
    Ok(())
}

/// The API's URL once it serves devkit's schema to its role, which the
/// first test to find it unserved sets up; `None` without a test database.
pub fn api_url() -> Option<String> {
    static READY: OnceLock<Option<String>> = OnceLock::new();
    READY
        .get_or_init(|| {
            let (api, direct) = (var("DEVKIT_TEST_SUPABASE_URL")?, direct_url()?);
            if serves(&api).is_ok() {
                return Some(api);
            }
            prepare(&direct);
            let deadline = Instant::now() + Duration::from_secs(30);
            while let Err(e) = serves(&api) {
                assert!(Instant::now() < deadline, "PostgREST never served: {e:#}");
                std::thread::sleep(Duration::from_millis(100));
            }
            Some(api)
        })
        .clone()
}

/// Creates devkit's schema through the postgres backend, then the role and
/// its grants.
fn prepare(direct: &str) {
    let db = devkit_todo_postgres::Database::new(
        direct,
        Duration::from_secs(10),
        &devkit_todo_postgres::Trust::default(),
    )
    .unwrap();
    devkit_todo_postgres::PostgresStore::new(db, "schema")
        .list(&Filter::all())
        .unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, connection) = tokio_postgres::connect(direct, tokio_postgres::NoTls)
                .await
                .unwrap();
            tokio::spawn(connection);
            client.batch_execute(GRANTS).await.unwrap();
        });
}
