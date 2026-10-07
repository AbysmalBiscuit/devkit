//! Agent todo lists kept in the todo database the postgres backend uses,
//! reached over a Supabase project's HTTPS Data API, for a machine that can
//! make HTTP requests but not open a Postgres connection, with their activity
//! records beside them.

mod activity;
mod api;
mod store;

pub use activity::SupabaseActivity;
pub use api::{Api, is_unreachable};
pub use store::SupabaseStore;
