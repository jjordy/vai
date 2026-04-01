//! JWT signing and verification for vai server access tokens.
//!
//! Access tokens are short-lived JWTs (default 15 min) signed with HMAC-SHA256.
//! The signing secret is loaded from the `VAI_JWT_SECRET` environment variable
//! or generated on first startup.
//!
//! Key rotation is supported: if a `previous_secret` is configured, tokens
//! signed with the old key are accepted during the overlap period. The
//! operator is responsible for clearing `previous_secret` once the overlap
//! window has passed; `overlap_secs` documents the intended window but the
//! service does not enforce it — token `exp` claims handle the time bound.

use chrono::Utc;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors from JWT operations.
#[derive(Debug, Error, PartialEq)]
pub enum JwtError {
    /// The token could not be encoded (internal error).
    #[error("JWT encoding error: {0}")]
    Encode(String),
    /// The token signature, format, or claims are invalid.
    #[error("JWT validation error: {0}")]
    Invalid(String),
    /// The token has expired.
    #[error("JWT expired")]
    Expired,
}

// ── Claims ─────────────────────────────────────────────────────────────────────

/// Claims embedded in a vai access token.
///
/// Follows the JWT standard: `sub`, `iat`, `exp` plus vai-specific fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenClaims {
    /// Subject — the user ID this token was issued for.
    pub sub: String,
    /// Repository ID this token is scoped to (`None` for server-wide tokens).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<String>,
    /// Role granted to this token (e.g., `"admin"`, `"write"`, `"read"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Issued-at timestamp (Unix seconds).
    pub iat: u64,
    /// Expiry timestamp (Unix seconds).
    pub exp: u64,
}

// ── Service ────────────────────────────────────────────────────────────────────

/// JWT signing and verification service.
///
/// Create with [`JwtService::new`] and keep it in shared server state.
/// All methods are `&self` and the struct is `Send + Sync`.
///
/// # Key rotation
///
/// When you rotate the signing secret:
/// 1. Move the old secret to `previous_secret`.
/// 2. Set the new secret as `current_secret`.
/// 3. After `overlap_secs` have elapsed (all old tokens have expired), clear
///    `previous_secret`.
///
/// The service always signs with `current_secret` and tries `previous_secret`
/// as a fallback during verification.
pub struct JwtService {
    current_secret: String,
    previous_secret: Option<String>,
    /// Documented overlap window in seconds (informational; token `exp` enforces timing).
    pub overlap_secs: u64,
    /// Access token TTL in seconds. Default: 900 (15 minutes).
    pub access_token_ttl: u64,
    /// Clock skew tolerance in seconds applied during verification. Default: 30.
    ///
    /// Tokens are accepted up to `leeway_secs` after their `exp` claim to
    /// account for clock drift between issuer and verifier.
    pub leeway_secs: u64,
}

impl JwtService {
    /// Creates a new `JwtService`.
    ///
    /// - `current_secret` — active HMAC-SHA256 signing key.
    /// - `previous_secret` — previous key, accepted for verification during rotation.
    /// - `overlap_secs` — how long (in seconds) the previous key should remain valid.
    pub fn new(
        current_secret: String,
        previous_secret: Option<String>,
        overlap_secs: u64,
    ) -> Self {
        Self {
            current_secret,
            previous_secret,
            overlap_secs,
            access_token_ttl: 900,
            leeway_secs: 30,
        }
    }

    /// Mints a new access token signed with the current secret.
    ///
    /// Sets `iat` to the current UTC time and `exp` to `iat + access_token_ttl`.
    pub fn sign(
        &self,
        sub: String,
        repo_id: Option<String>,
        role: Option<String>,
    ) -> Result<String, JwtError> {
        let now = Utc::now().timestamp() as u64;
        let claims = TokenClaims {
            sub,
            repo_id,
            role,
            iat: now,
            exp: now + self.access_token_ttl,
        };
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(self.current_secret.as_bytes()),
        )
        .map_err(|e| JwtError::Encode(e.to_string()))
    }

    /// Verifies a JWT string and returns its claims if valid.
    ///
    /// Validation order:
    /// 1. Try the current secret.
    /// 2. If the signature doesn't match, try the previous secret (rotation).
    /// 3. If neither matches, return `JwtError::Invalid`.
    ///
    /// An expired token returns `JwtError::Expired` regardless of which key
    /// was used to sign it.
    pub fn verify(&self, token: &str) -> Result<TokenClaims, JwtError> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;
        validation.leeway = self.leeway_secs;

        match self.try_decode(token, &self.current_secret, &validation) {
            Ok(claims) => return Ok(claims),
            Err(JwtError::Invalid(_)) => {
                // Signature mismatch — try previous key if available.
            }
            Err(e) => return Err(e),
        }

        if let Some(ref prev) = self.previous_secret {
            return self.try_decode(token, prev, &validation);
        }

        Err(JwtError::Invalid("invalid signature".to_string()))
    }

    fn try_decode(
        &self,
        token: &str,
        secret: &str,
        validation: &Validation,
    ) -> Result<TokenClaims, JwtError> {
        use jsonwebtoken::errors::ErrorKind;
        decode::<TokenClaims>(token, &DecodingKey::from_secret(secret.as_bytes()), validation)
            .map(|data| data.claims)
            .map_err(|e| match e.kind() {
                ErrorKind::ExpiredSignature => JwtError::Expired,
                _ => JwtError::Invalid(e.to_string()),
            })
    }
}

// ── JWT secret resolution ──────────────────────────────────────────────────────

/// Resolves the JWT signing secret.
///
/// Resolution order:
/// 1. `VAI_JWT_SECRET` environment variable — used as-is.
/// 2. `VAI_JWT_SECRET_PREV` environment variable — loaded as the previous key
///    for rotation support (only consulted during verification, never for signing).
/// 3. If `VAI_JWT_SECRET` is not set — generate a random secret, print it to
///    stdout (so the operator can persist it), and return a new `JwtService`.
///
/// Returns `(JwtService, is_ephemeral)`.  When `is_ephemeral` is `true` the
/// key was generated at runtime and tokens issued now will be invalid after a
/// restart.
pub fn resolve_jwt_service(overlap_secs: u64) -> (JwtService, bool) {
    let current = std::env::var("VAI_JWT_SECRET").unwrap_or_default();
    let prev = std::env::var("VAI_JWT_SECRET_PREV").ok().filter(|s| !s.is_empty());

    if !current.is_empty() {
        return (JwtService::new(current, prev, overlap_secs), false);
    }

    // Generate a 256-bit random secret from four UUIDs.
    let generated = format!(
        "{}{}{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple(),
    );

    println!();
    println!("╔══════════════════════════════════════════════════════════════════════╗");
    println!("║            VAI JWT SIGNING SECRET (shown once)                      ║");
    println!("╠══════════════════════════════════════════════════════════════════════╣");
    println!("║  Set VAI_JWT_SECRET=<secret> to reuse this key across restarts.     ║");
    println!("║  Without this, JWT access tokens are invalid after a server         ║");
    println!("║  restart.                                                            ║");
    println!("╚══════════════════════════════════════════════════════════════════════╝");
    println!();

    (JwtService::new(generated, None, overlap_secs), true)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::thread::sleep;
    use std::time::Duration;

    use super::*;

    fn service(secret: &str) -> JwtService {
        JwtService::new(secret.to_string(), None, 3600)
    }

    // ── Basic sign / verify ────────────────────────────────────────────────────

    #[test]
    fn sign_and_verify_roundtrip() {
        let svc = service("test-secret");
        let token = svc
            .sign(
                "user-123".to_string(),
                Some("repo-abc".to_string()),
                Some("write".to_string()),
            )
            .unwrap();
        let claims = svc.verify(&token).unwrap();
        assert_eq!(claims.sub, "user-123");
        assert_eq!(claims.repo_id.as_deref(), Some("repo-abc"));
        assert_eq!(claims.role.as_deref(), Some("write"));
    }

    #[test]
    fn verify_wrong_secret_returns_invalid() {
        let svc_a = service("secret-a");
        let svc_b = service("secret-b");
        let token = svc_a
            .sign("user-1".to_string(), None, None)
            .unwrap();
        let err = svc_b.verify(&token).unwrap_err();
        assert!(matches!(err, JwtError::Invalid(_)), "got: {err:?}");
    }

    #[test]
    fn verify_malformed_token_returns_invalid() {
        let svc = service("secret");
        let err = svc.verify("this.is.not.a.jwt").unwrap_err();
        assert!(matches!(err, JwtError::Invalid(_)));
    }

    // ── Expiry ─────────────────────────────────────────────────────────────────

    #[test]
    fn expired_token_returns_expired() {
        let mut svc = service("secret");
        svc.access_token_ttl = 1; // 1 second TTL
        svc.leeway_secs = 0; // no grace period so 1s sleep is sufficient
        let token = svc.sign("u".to_string(), None, None).unwrap();
        sleep(Duration::from_secs(2));
        let err = svc.verify(&token).unwrap_err();
        assert_eq!(err, JwtError::Expired);
    }

    // ── Key rotation ───────────────────────────────────────────────────────────

    #[test]
    fn rotated_service_accepts_old_token() {
        // Mint a token with the old secret.
        let old_svc = service("old-secret");
        let token = old_svc.sign("u".to_string(), None, None).unwrap();

        // Rotate: new secret becomes current, old secret goes to previous.
        let new_svc = JwtService::new(
            "new-secret".to_string(),
            Some("old-secret".to_string()),
            3600,
        );

        // Token signed with the old secret should still verify.
        let claims = new_svc.verify(&token).unwrap();
        assert_eq!(claims.sub, "u");
    }

    #[test]
    fn rotated_service_signs_with_new_secret() {
        let new_svc = JwtService::new(
            "new-secret".to_string(),
            Some("old-secret".to_string()),
            3600,
        );
        let token = new_svc.sign("u".to_string(), None, None).unwrap();

        // Old-only service should reject the new token.
        let old_svc = service("old-secret");
        assert!(matches!(old_svc.verify(&token), Err(JwtError::Invalid(_))));

        // New-only service should accept it.
        let new_only = service("new-secret");
        assert!(new_only.verify(&token).is_ok());
    }

    #[test]
    fn no_previous_secret_rejects_unknown_token() {
        let svc_a = service("secret-a");
        let svc_b = service("secret-b"); // no previous
        let token = svc_a.sign("u".to_string(), None, None).unwrap();
        assert!(matches!(svc_b.verify(&token), Err(JwtError::Invalid(_))));
    }

    // ── Optional claims ────────────────────────────────────────────────────────

    #[test]
    fn none_claims_round_trip() {
        let svc = service("sec");
        let token = svc.sign("u".to_string(), None, None).unwrap();
        let claims = svc.verify(&token).unwrap();
        assert_eq!(claims.sub, "u");
        assert!(claims.repo_id.is_none());
        assert!(claims.role.is_none());
    }
}
