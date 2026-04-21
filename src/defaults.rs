//! Build-time configurable defaults for server and dashboard URLs.
//!
//! Set `VAI_DEFAULT_SERVER_URL` / `VAI_DEFAULT_DASHBOARD_URL` at compile time
//! to bake in production URLs for releases.  Dev builds fall back to the Fly
//! server and `http://localhost:3000` respectively.
//!
//! Resolution order at runtime (highest priority first):
//! 1. `--server-url` / `--dashboard-url` CLI flag
//! 2. `VAI_SERVER_URL` / `VAI_DASHBOARD_URL` env var
//! 3. `~/.vai/credentials.toml` `server_url` field (server only)
//! 4. This compile-time constant

/// Default server base URL.
///
/// Override at build time with `VAI_DEFAULT_SERVER_URL=https://your-server.example.com cargo build`.
pub const DEFAULT_SERVER_URL: &str = if let Some(url) = option_env!("VAI_DEFAULT_SERVER_URL") {
    url
} else {
    "https://vai-server-polished-feather-2668.fly.dev"
};

/// Default dashboard base URL.
///
/// Override at build time with `VAI_DEFAULT_DASHBOARD_URL=https://your-dashboard.example.com cargo build`.
pub const DEFAULT_DASHBOARD_URL: &str =
    if let Some(url) = option_env!("VAI_DEFAULT_DASHBOARD_URL") {
        url
    } else {
        "http://localhost:3000"
    };
