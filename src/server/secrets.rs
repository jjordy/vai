//! Server-side encrypted secret storage for per-repo agent secrets (PRD 28).
//!
//! Secrets are encrypted with AES-256-GCM using a server master key read from
//! the `VAI_SECRETS_MASTER_KEY` environment variable (32 bytes, base64-encoded).
//! Each secret gets a unique 12-byte random nonce stored alongside the
//! ciphertext in the `repo_agent_secrets` table.
//!
//! ## Key rotation
//! To rotate the master key:
//!
//! 1. Re-encrypt all rows: decrypt with the old key, re-encrypt with the new key, update the row.
//! 2. Swap the new key into `VAI_SECRETS_MASTER_KEY`.
//!
//! Keep the old key available during step 1 (e.g. via a transition env var).
//! No downtime is required if done atomically per-row.
//!
//! ## Security invariants
//! - The master key is never logged.
//! - Secret values are never stored in `Debug` impls.
//! - Decrypted values live only in memory for the duration of a request.
//! - [`list_secret_keys`] returns only key names, never values.

// HTTP handlers (issue #351) will wire these into routes; suppress dead_code until then.
#![allow(dead_code)]

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

/// Errors from the secrets vault.
#[derive(Debug, Error)]
pub enum SecretsError {
    /// `VAI_SECRETS_MASTER_KEY` is unset or empty.
    #[error("VAI_SECRETS_MASTER_KEY is not set")]
    MasterKeyMissing,

    /// The env var is set but is not valid base64 or not exactly 32 bytes.
    #[error("invalid master key: {0}")]
    MasterKeyInvalid(String),

    /// The requested secret key does not exist for this repo.
    #[error("secret not found")]
    KeyNotFound,

    /// Decryption failed — wrong master key or corrupt ciphertext.
    #[error("decryption failed — wrong master key or corrupt ciphertext")]
    DecryptionFailed,

    /// A database operation failed.
    #[error("database error: {0}")]
    Database(String),
}

impl From<sqlx::Error> for SecretsError {
    fn from(e: sqlx::Error) -> Self {
        SecretsError::Database(e.to_string())
    }
}

/// Decode and validate a master key string (base64, must be 32 bytes).
///
/// Extracted so unit tests can drive this without touching env vars.
fn decode_master_key(raw: &str) -> Result<[u8; 32], SecretsError> {
    if raw.is_empty() {
        return Err(SecretsError::MasterKeyMissing);
    }
    let bytes = BASE64
        .decode(raw.trim())
        .map_err(|e| SecretsError::MasterKeyInvalid(format!("base64 decode failed: {e}")))?;
    if bytes.len() != 32 {
        return Err(SecretsError::MasterKeyInvalid(format!(
            "expected 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

/// Read and decode the master key from `VAI_SECRETS_MASTER_KEY`.
fn get_master_key() -> Result<[u8; 32], SecretsError> {
    let raw = std::env::var("VAI_SECRETS_MASTER_KEY").unwrap_or_default();
    decode_master_key(&raw)
}

/// Encrypt `value` with AES-256-GCM. Returns `(ciphertext, nonce)`.
fn encrypt_value(key: &[u8; 32], value: &str) -> Result<(Vec<u8>, Vec<u8>), SecretsError> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| SecretsError::MasterKeyInvalid(e.to_string()))?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, value.as_bytes())
        .map_err(|_| SecretsError::DecryptionFailed)?;
    Ok((ciphertext, nonce.to_vec()))
}

/// Decrypt `ciphertext` with `key` and `nonce_bytes`. Returns plaintext string.
fn decrypt_value(
    key: &[u8; 32],
    ciphertext: &[u8],
    nonce_bytes: &[u8],
) -> Result<String, SecretsError> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| SecretsError::MasterKeyInvalid(e.to_string()))?;
    let nonce = Nonce::from_slice(nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| SecretsError::DecryptionFailed)?;
    String::from_utf8(plaintext).map_err(|_| SecretsError::DecryptionFailed)
}

/// Store or overwrite a secret for a repository.
///
/// The plaintext `value` is encrypted with AES-256-GCM under the server master
/// key before being written to `repo_agent_secrets`. If a secret with `key`
/// already exists for this repo it is overwritten (upsert on `(repo_id, key)`).
pub async fn set_secret(
    db: &PgPool,
    repo_id: &Uuid,
    key: &str,
    value: &str,
) -> Result<(), SecretsError> {
    let master_key = get_master_key()?;
    let (ciphertext, nonce) = encrypt_value(&master_key, value)?;

    sqlx::query(
        r#"
        INSERT INTO repo_agent_secrets (repo_id, key, encrypted_value, nonce)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (repo_id, key)
        DO UPDATE SET encrypted_value = EXCLUDED.encrypted_value,
                      nonce           = EXCLUDED.nonce
        "#,
    )
    .bind(repo_id)
    .bind(key)
    .bind(ciphertext.as_slice())
    .bind(nonce.as_slice())
    .execute(db)
    .await?;

    Ok(())
}

/// Retrieve and decrypt a secret for a repository.
///
/// Returns `None` if no secret with `key` exists for this repo.
/// Returns `Err(SecretsError::DecryptionFailed)` if decryption fails (wrong
/// master key or corrupt data).
pub async fn get_secret(
    db: &PgPool,
    repo_id: &Uuid,
    key: &str,
) -> Result<Option<String>, SecretsError> {
    let master_key = get_master_key()?;

    let maybe_row = sqlx::query(
        "SELECT encrypted_value, nonce FROM repo_agent_secrets WHERE repo_id = $1 AND key = $2",
    )
    .bind(repo_id)
    .bind(key)
    .fetch_optional(db)
    .await?;

    let Some(row) = maybe_row else {
        return Ok(None);
    };

    let encrypted_value: Vec<u8> = row
        .try_get("encrypted_value")
        .map_err(|e| SecretsError::Database(e.to_string()))?;
    let nonce_bytes: Vec<u8> = row
        .try_get("nonce")
        .map_err(|e| SecretsError::Database(e.to_string()))?;

    let plaintext = decrypt_value(&master_key, &encrypted_value, &nonce_bytes)?;
    Ok(Some(plaintext))
}

/// Delete a secret for a repository.
///
/// Idempotent — succeeds even if the key does not exist.
pub async fn delete_secret(db: &PgPool, repo_id: &Uuid, key: &str) -> Result<(), SecretsError> {
    sqlx::query(
        "DELETE FROM repo_agent_secrets WHERE repo_id = $1 AND key = $2",
    )
    .bind(repo_id)
    .bind(key)
    .execute(db)
    .await?;

    Ok(())
}

/// List the key names stored for a repository.
///
/// Returns only key names — never decrypts or returns values.
pub async fn list_secret_keys(db: &PgPool, repo_id: &Uuid) -> Result<Vec<String>, SecretsError> {
    let rows = sqlx::query(
        "SELECT key FROM repo_agent_secrets WHERE repo_id = $1 ORDER BY key",
    )
    .bind(repo_id)
    .fetch_all(db)
    .await?;

    rows.iter()
        .map(|r| {
            r.try_get::<String, _>("key")
                .map_err(|e| SecretsError::Database(e.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        k
    }

    fn test_key_alt() -> [u8; 32] {
        let mut k = [0xff_u8; 32];
        k[0] = 0xab;
        k
    }

    #[test]
    fn decode_master_key_missing() {
        assert!(matches!(
            decode_master_key(""),
            Err(SecretsError::MasterKeyMissing)
        ));
    }

    #[test]
    fn decode_master_key_invalid_base64() {
        assert!(matches!(
            decode_master_key("not-valid-base64!!!"),
            Err(SecretsError::MasterKeyInvalid(_))
        ));
    }

    #[test]
    fn decode_master_key_wrong_length() {
        // 16 bytes — valid base64 but wrong key length
        let short = BASE64.encode([0u8; 16]);
        assert!(matches!(
            decode_master_key(&short),
            Err(SecretsError::MasterKeyInvalid(_))
        ));
    }

    #[test]
    fn decode_master_key_ok() {
        let raw = BASE64.encode([42u8; 32]);
        let key = decode_master_key(&raw).unwrap();
        assert_eq!(key, [42u8; 32]);
    }

    #[test]
    fn roundtrip_encrypt_decrypt() {
        let key = test_key();
        let (ct, nonce) = encrypt_value(&key, "hello secret").unwrap();
        let plaintext = decrypt_value(&key, &ct, &nonce).unwrap();
        assert_eq!(plaintext, "hello secret");
    }

    #[test]
    fn wrong_key_returns_decryption_failed() {
        let (ct, nonce) = encrypt_value(&test_key(), "secret").unwrap();
        assert!(matches!(
            decrypt_value(&test_key_alt(), &ct, &nonce),
            Err(SecretsError::DecryptionFailed)
        ));
    }

    #[test]
    fn corrupt_ciphertext_returns_decryption_failed() {
        let key = test_key();
        let (mut ct, nonce) = encrypt_value(&key, "secret").unwrap();
        ct[0] ^= 0xff;
        assert!(matches!(
            decrypt_value(&key, &ct, &nonce),
            Err(SecretsError::DecryptionFailed)
        ));
    }

    #[test]
    fn each_encrypt_produces_unique_nonce() {
        let key = test_key();
        let (_, nonce1) = encrypt_value(&key, "same").unwrap();
        let (_, nonce2) = encrypt_value(&key, "same").unwrap();
        assert_ne!(nonce1, nonce2, "nonces must be unique per call");
    }
}
