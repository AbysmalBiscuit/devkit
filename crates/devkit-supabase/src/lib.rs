//! The Supabase project client devkit's Data-API-backed stores share:
//! PostgREST requests under one schema, the refusals PostgREST answers, and
//! Supabase Auth sessions. What a store asks for and how it reads its own
//! refusal codes stay with the store.

mod api;
#[cfg(feature = "test-support")]
pub mod fakehttp;

pub use api::{Api, Auth, Refused, is_unreachable};
pub use reqwest::{StatusCode, blocking::Response};
