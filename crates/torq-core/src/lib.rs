//! TorQ daemon core.
//!
//! Owns the librqbit engine session, download queue, persistence, and (in the
//! next phase) the REST + SSE API. The TUI and CLI are stateless clients.

pub mod api;
pub mod config;
pub mod daemon;
pub mod engine;
pub mod rss;
pub mod watch;

pub const APP_NAME: &str = "torq";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
