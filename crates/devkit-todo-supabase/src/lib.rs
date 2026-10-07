//! Agent todo lists kept in the todo database the postgres backend uses,
//! reached over a Supabase project's HTTPS Data API, for a machine that can
//! make HTTP requests but not open a Postgres connection.

mod api;
mod store;

pub use api::{Api, is_unreachable};
pub use store::SupabaseStore;
