//! Credentials management for `~/.vai/credentials.toml`.
//!
//! All CLI commands that authenticate read credentials in this order:
//! 1. `VAI_API_KEY` env var (highest priority).
//! 2. `~/.vai/credentials.toml` (`[default]` profile).
//! 3. Error: "Not logged in. Run `vai login`."

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors from credential operations.
#[derive(Debug, Error)]
pub enum CredentialsError {
    /// No credentials are available — the user needs to run `vai login`.
    #[error("not logged in — run `vai login`")]
    NotLoggedIn,

    /// An I/O error occurred while reading or writing the credentials file.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The credentials file could not be parsed as valid TOML.
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    /// The credentials could not be serialized as TOML.
    #[error("TOML serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
}

/// A stored credential profile, persisted as `[default]` in `~/.vai/credentials.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    /// Base URL of the vai server (e.g. `https://vai.example.com`).
    pub server_url: String,
    /// Plaintext API key for authentication.
    pub api_key: String,
    /// User ID returned by the server (optional — not all auth flows provide this).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// User email address (optional — not all auth flows provide this).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_email: Option<String>,
}

/// The top-level structure of `~/.vai/credentials.toml`.
#[derive(Debug, Serialize, Deserialize)]
struct CredentialsFile {
    default: Credentials,
}

/// Returns the path to the user's `~/.vai/` config directory.
///
/// Checks `$HOME` on Unix, then `$USERPROFILE` on Windows.
pub fn vai_config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".vai"))
}

/// Returns the path to `~/.vai/credentials.toml`, or `None` if the home
/// directory cannot be determined.
pub fn credentials_path() -> Option<PathBuf> {
    vai_config_dir().map(|d| d.join("credentials.toml"))
}

/// Writes `creds` to `~/.vai/credentials.toml`.
///
/// Creates `~/.vai/` with mode `0700` if it does not exist.
/// Sets the file mode to `0600` on Unix platforms.
/// Overwrites any existing credentials cleanly.
pub fn write(creds: &Credentials) -> Result<(), CredentialsError> {
    let vai_dir = vai_config_dir().ok_or_else(|| {
        CredentialsError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot determine home directory",
        ))
    })?;

    // Ensure ~/.vai/ exists with restricted permissions.
    if !vai_dir.exists() {
        std::fs::create_dir_all(&vai_dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&vai_dir, std::fs::Permissions::from_mode(0o700))?;
    }

    let path = vai_dir.join("credentials.toml");
    let content = toml::to_string_pretty(&CredentialsFile {
        default: creds.clone(),
    })?;

    std::fs::write(&path, content)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

/// Reads the `[default]` profile from `~/.vai/credentials.toml`.
///
/// Returns [`CredentialsError::NotLoggedIn`] if the file does not exist.
pub fn read() -> Result<Credentials, CredentialsError> {
    let path = credentials_path().ok_or(CredentialsError::NotLoggedIn)?;
    if !path.exists() {
        return Err(CredentialsError::NotLoggedIn);
    }
    let content = std::fs::read_to_string(&path)?;
    let file: CredentialsFile = toml::from_str(&content)?;
    Ok(file.default)
}

/// Loads an API key using the standard resolution order:
///
/// 1. `VAI_API_KEY` environment variable (highest priority).
/// 2. `~/.vai/credentials.toml` `[default]` profile.
/// 3. Returns [`CredentialsError::NotLoggedIn`].
///
/// Returns `(api_key, server_url)`. When the key comes from the env var,
/// `server_url` is taken from `VAI_SERVER_URL` (may be `None`).
pub fn load_api_key() -> Result<(String, Option<String>), CredentialsError> {
    if let Ok(key) = std::env::var("VAI_API_KEY") {
        if !key.is_empty() {
            let server_url = std::env::var("VAI_SERVER_URL").ok();
            return Ok((key, server_url));
        }
    }
    let creds = read()?;
    Ok((creds.api_key, Some(creds.server_url)))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize and deserialize a `Credentials` value through the TOML file format.
    #[test]
    fn round_trip_credentials_toml() {
        let creds = Credentials {
            server_url: "https://vai.example.com".to_string(),
            api_key: "vai_abc123".to_string(),
            user_id: Some("user-42".to_string()),
            user_email: Some("dev@example.com".to_string()),
        };
        let file = CredentialsFile { default: creds.clone() };
        let toml_str = toml::to_string_pretty(&file).unwrap();
        let parsed: CredentialsFile = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.default.server_url, creds.server_url);
        assert_eq!(parsed.default.api_key, creds.api_key);
        assert_eq!(parsed.default.user_id, creds.user_id);
        assert_eq!(parsed.default.user_email, creds.user_email);
    }

    /// Optional fields are omitted when `None`.
    #[test]
    fn optional_fields_omitted() {
        let creds = Credentials {
            server_url: "https://vai.example.com".to_string(),
            api_key: "vai_abc123".to_string(),
            user_id: None,
            user_email: None,
        };
        let file = CredentialsFile { default: creds };
        let toml_str = toml::to_string_pretty(&file).unwrap();
        assert!(!toml_str.contains("user_id"));
        assert!(!toml_str.contains("user_email"));
    }

    /// Write and read back credentials through an actual temp file.
    #[test]
    fn write_and_read_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Override HOME so credentials are written to the temp dir.
        let original_home = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());

        let creds = Credentials {
            server_url: "https://vai.example.com".to_string(),
            api_key: "vai_testkey00000000000000000000000000".to_string(),
            user_id: Some("u-1234".to_string()),
            user_email: Some("tester@example.com".to_string()),
        };

        write(&creds).expect("write should succeed");

        let path = credentials_path().unwrap();
        assert!(path.exists(), "credentials.toml should exist after write");

        // Verify 0600 permissions on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(&path).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }

        let loaded = read().expect("read should succeed");
        assert_eq!(loaded.server_url, creds.server_url);
        assert_eq!(loaded.api_key, creds.api_key);
        assert_eq!(loaded.user_id, creds.user_id);
        assert_eq!(loaded.user_email, creds.user_email);

        // Restore HOME.
        match original_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    /// Reading from a missing file returns `NotLoggedIn`.
    #[test]
    fn read_missing_returns_not_logged_in() {
        let tmp = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());

        let result = read();
        assert!(matches!(result, Err(CredentialsError::NotLoggedIn)));

        match original_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}
