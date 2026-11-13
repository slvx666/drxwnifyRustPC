//! Shared models, settings, SQLite storage, HTTP client and the app event bus.
//! Both editions (Full and Mini) build on this crate; UI code never talks to the network or the database directly.

/// App version shown in the UI and sent to friends (same field as on Android).
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
