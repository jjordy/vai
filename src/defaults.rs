//! Build-time configurable defaults for server and dashboard URLs.
//!
//! Set `VAI_DEFAULT_SERVER_URL` at compile time to bake in a real server URL
//! for production releases.  Dev builds fall back to `http://localhost:8080`.
//!
//! Resolution order at runtime (highest priority first):
//! 1. `--server-url` CLI flag
//! 2. `VAI_SERVER_URL` env var
//! 3. `~/.vai/credentials.toml` `server_url` field
//! 4. This compile-time constant

/// Default server base URL.
///
/// Override at build time with `VAI_DEFAULT_SERVER_URL=https://your-server.example.com cargo build`.
pub const DEFAULT_SERVER_URL: &str = if let Some(url) = option_env!("VAI_DEFAULT_SERVER_URL") {
    url
} else {
    "http://localhost:8080"
};
