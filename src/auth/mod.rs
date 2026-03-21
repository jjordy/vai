//! API key management for vai server authentication.
//!
//! Keys are stored in `.vai/keys.db` (SQLite). The plaintext key is shown
//! exactly once at creation; only a SHA-256 hash is persisted on disk.
//!
//! Key format: `vai_<32 hex chars>` (UUID v4 without dashes).

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, ErrorCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors from the auth module.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("key not found: {0}")]
    NotFound(String),

    #[error("key name already exists: {0}")]
    Duplicate(String),
}

// ── Data types ────────────────────────────────────────────────────────────────

/// Metadata for an API key (does not include the plaintext secret or its hash).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    /// Unique identifier for this key record.
    pub id: String,
    /// Human-readable name, unique per repository.
    pub name: String,
    /// Display prefix — first 12 characters of the plaintext key.
    pub key_prefix: String,
    /// When this key was created.
    pub created_at: DateTime<Utc>,
    /// When this key was last used to authenticate a request (`None` if never).
    pub last_used_at: Option<DateTime<Utc>>,
    /// Whether this key has been revoked.
    pub revoked: bool,
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Opens (or creates) the API-key database at `<vai_dir>/keys.db`.
fn open_db(vai_dir: &Path) -> Result<Connection, AuthError> {
    let path = vai_dir.join("keys.db");
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS api_keys (
            id           TEXT PRIMARY KEY,
            name         TEXT UNIQUE NOT NULL,
            key_hash     TEXT NOT NULL,
            key_prefix   TEXT NOT NULL,
            created_at   TEXT NOT NULL,
            last_used_at TEXT,
            revoked      INTEGER NOT NULL DEFAULT 0
        );",
    )?;
    Ok(conn)
}

/// Returns the SHA-256 digest of `data` as a lowercase hex string.
fn sha256_hex(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

/// Generates a fresh plaintext API key.
///
/// The key is `vai_` followed by 32 hex characters (UUID v4 without dashes),
/// giving 128 bits of entropy.
fn new_plaintext_key() -> String {
    let raw = Uuid::new_v4().simple().to_string();
    format!("vai_{raw}")
}

/// Parses a stored RFC-3339 timestamp, falling back to the current time on error.
fn parse_ts(s: String) -> DateTime<Utc> {
    s.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now())
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Creates a new API key with the given name.
///
/// Returns `(ApiKey, plaintext_key)`. The plaintext key is shown exactly once
/// and must be stored securely by the caller — only its hash is kept on disk.
pub fn create(vai_dir: &Path, name: &str) -> Result<(ApiKey, String), AuthError> {
    let conn = open_db(vai_dir)?;

    let plaintext = new_plaintext_key();
    let hash = sha256_hex(&plaintext);
    // Display prefix: "vai_" + 8 chars so the user can identify the key.
    let key_prefix = plaintext[..12].to_string();
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let now_str = now.to_rfc3339();

    let result = conn.execute(
        "INSERT INTO api_keys (id, name, key_hash, key_prefix, created_at, revoked)
         VALUES (?1, ?2, ?3, ?4, ?5, 0)",
        params![id, name, hash, key_prefix, now_str],
    );

    match result {
        Err(rusqlite::Error::SqliteFailure(err, _))
            if err.code == ErrorCode::ConstraintViolation =>
        {
            return Err(AuthError::Duplicate(name.to_string()));
        }
        Err(e) => return Err(AuthError::Sqlite(e)),
        Ok(_) => {}
    }

    let key = ApiKey {
        id,
        name: name.to_string(),
        key_prefix,
        created_at: now,
        last_used_at: None,
        revoked: false,
    };

    Ok((key, plaintext))
}

/// Lists all API keys (active and revoked), ordered by creation time.
pub fn list(vai_dir: &Path) -> Result<Vec<ApiKey>, AuthError> {
    let conn = open_db(vai_dir)?;
    let mut stmt = conn.prepare(
        "SELECT id, name, key_prefix, created_at, last_used_at, revoked
         FROM api_keys
         ORDER BY created_at ASC",
    )?;

    let keys = stmt
        .query_map([], |row| {
            let last_used_str: Option<String> = row.get(4)?;
            Ok(ApiKey {
                id: row.get(0)?,
                name: row.get(1)?,
                key_prefix: row.get(2)?,
                created_at: parse_ts(row.get::<_, String>(3)?),
                last_used_at: last_used_str.map(parse_ts),
                revoked: row.get::<_, i64>(5)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(keys)
}

/// Revokes the key with the given name.
///
/// Returns [`AuthError::NotFound`] if the name is not found or is already revoked.
pub fn revoke(vai_dir: &Path, name: &str) -> Result<(), AuthError> {
    let conn = open_db(vai_dir)?;
    let updated = conn.execute(
        "UPDATE api_keys SET revoked = 1 WHERE name = ?1 AND revoked = 0",
        params![name],
    )?;
    if updated == 0 {
        return Err(AuthError::NotFound(name.to_string()));
    }
    Ok(())
}

/// Validates a plaintext API key.
///
/// Returns `Some(ApiKey)` if the key exists and is not revoked, and updates
/// `last_used_at` as a side-effect. Returns `None` for unknown or revoked keys.
pub fn validate(vai_dir: &Path, plaintext: &str) -> Result<Option<ApiKey>, AuthError> {
    let conn = open_db(vai_dir)?;
    let hash = sha256_hex(plaintext);
    let now_str = Utc::now().to_rfc3339();

    let result = conn.query_row(
        "SELECT id, name, key_prefix, created_at, last_used_at, revoked
         FROM api_keys
         WHERE key_hash = ?1",
        params![hash],
        |row| {
            let last_used_str: Option<String> = row.get(4)?;
            Ok(ApiKey {
                id: row.get(0)?,
                name: row.get(1)?,
                key_prefix: row.get(2)?,
                created_at: parse_ts(row.get::<_, String>(3)?),
                last_used_at: last_used_str.map(parse_ts),
                revoked: row.get::<_, i64>(5)? != 0,
            })
        },
    );

    match result {
        Ok(key) if !key.revoked => {
            let _ = conn.execute(
                "UPDATE api_keys SET last_used_at = ?1 WHERE id = ?2",
                params![now_str, key.id],
            );
            Ok(Some(key))
        }
        Ok(_) => Ok(None), // key exists but is revoked
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(AuthError::Sqlite(e)),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn tmp_vai_dir() -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let vai_dir = tmp.path().join(".vai");
        std::fs::create_dir_all(&vai_dir).unwrap();
        (tmp, vai_dir)
    }

    #[test]
    fn create_and_validate_key() {
        let (_tmp, vai_dir) = tmp_vai_dir();
        let (meta, plaintext) = create(&vai_dir, "agent-alpha").unwrap();
        assert_eq!(meta.name, "agent-alpha");
        assert!(!meta.revoked);
        assert!(plaintext.starts_with("vai_"));

        let found = validate(&vai_dir, &plaintext).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "agent-alpha");
    }

    #[test]
    fn invalid_key_returns_none() {
        let (_tmp, vai_dir) = tmp_vai_dir();
        let found = validate(&vai_dir, "vai_notavalidkey00000000000000000").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn revoked_key_returns_none() {
        let (_tmp, vai_dir) = tmp_vai_dir();
        let (_, plaintext) = create(&vai_dir, "temp-agent").unwrap();
        revoke(&vai_dir, "temp-agent").unwrap();
        let found = validate(&vai_dir, &plaintext).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn duplicate_name_returns_error() {
        let (_tmp, vai_dir) = tmp_vai_dir();
        create(&vai_dir, "same-name").unwrap();
        let result = create(&vai_dir, "same-name");
        assert!(matches!(result, Err(AuthError::Duplicate(_))));
    }

    #[test]
    fn list_keys() {
        let (_tmp, vai_dir) = tmp_vai_dir();
        create(&vai_dir, "alpha").unwrap();
        create(&vai_dir, "beta").unwrap();
        revoke(&vai_dir, "beta").unwrap();
        let keys = list(&vai_dir).unwrap();
        assert_eq!(keys.len(), 2);
        assert!(!keys[0].revoked);
        assert!(keys[1].revoked);
    }

    #[test]
    fn revoke_nonexistent_returns_not_found() {
        let (_tmp, vai_dir) = tmp_vai_dir();
        let result = revoke(&vai_dir, "ghost");
        assert!(matches!(result, Err(AuthError::NotFound(_))));
    }
}
