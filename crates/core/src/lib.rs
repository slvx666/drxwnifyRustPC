//! Shared models, settings, SQLite storage, HTTP client and the app event bus.
//! Both editions (Full and Mini) build on this crate; UI code never talks to the network or the database directly.

pub mod db;
pub mod error;
pub mod http;
pub mod images;
pub mod keys;
pub mod paths;
pub mod prefs;
pub mod secrets;
pub mod shared_prefs;
pub mod store;

pub use error::{CoreError, Result};

/// App version shown in the UI and sent to friends (same field as on Android).
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
