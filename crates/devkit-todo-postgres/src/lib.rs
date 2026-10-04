//! Agent todo lists kept in a Postgres database, the one authority every
//! machine's agents claim against, with their activity records beside them.

mod activity;
mod database;
mod store;

pub use activity::PostgresActivity;
pub use database::{Database, is_unreachable};
pub use devkit_common::tls::Trust;
pub use store::PostgresStore;
