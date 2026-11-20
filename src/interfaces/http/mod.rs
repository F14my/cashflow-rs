//! HTTP API (axum). Routes live in `router`, payloads in `dto`.

pub mod dto;
pub mod router;

pub use router::{router, AppState};
