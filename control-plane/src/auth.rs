//! Supabase JWT verification.
//!
//! Supabase projects now default to ECC (ES256) signing keys. We fetch
//! the JWKS from the Supabase Auth endpoint and verify against the
//! public key. Falls back to HS256 (legacy shared secret) if the JWT
//! header indicates HS256.

use jsonwebtoken::{decode, decode_header, jwk, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Supabase user ID (UUID string).
    pub sub: String,
    /// Token audience.
    #[serde(default)]
    pub aud: Option<String>,
    /// Token issuer.
    #[serde(default)]
    pub iss: Option<String>,
    /// Token expiry (Unix timestamp).
    pub exp: u64,
    /// Issued at.
    pub iat: u64,
    /// Supabase role: "authenticated", "anon", "service_role".
    #[serde(default)]
    pub role: String,
    /// User email (optional, included by Supabase).
    #[serde(default)]
    pub email: Option<String>,
}

/// Cached JWKS for ES256 verification.
#[derive(Clone)]
pub struct JwtVerifier {
    jwks_url: String,
    issuer: String,
    hs256_secret: String,
    cached_jwks: Arc<RwLock<Option<jwk::JwkSet>>>,
    http: reqwest::Client,
}

impl JwtVerifier {
    pub fn new(supabase_url: &str, jwt_secret: &str) -> Self {
        let supabase_url = supabase_url.trim_end_matches('/');
        Self {
            jwks_url: format!("{supabase_url}/auth/v1/.well-known/jwks.json"),
            issuer: format!("{supabase_url}/auth/v1"),
            hs256_secret: jwt_secret.to_string(),
            cached_jwks: Arc::new(RwLock::new(None)),
            http: reqwest::Client::new(),
        }
    }

    /// Verify a JWT and extract claims. Supports both ES256 and HS256.
    pub async fn verify(&self, token: &str) -> Result<Claims, String> {
        let header = decode_header(token).map_err(|e| format!("Invalid JWT header: {e}"))?;

        match header.alg {
            Algorithm::ES256 => self.verify_es256(token, &header).await,
            Algorithm::HS256 => verify_hs256(token, &self.hs256_secret, Some(&self.issuer)),
            alg => Err(format!("Unsupported JWT algorithm: {alg:?}")),
        }
    }

    async fn verify_es256(
        &self,
        token: &str,
        header: &jsonwebtoken::Header,
    ) -> Result<Claims, String> {
        let kid = header
            .kid
            .as_deref()
            .ok_or("ES256 JWT missing kid header")?;

        let jwks = self.get_jwks().await?;
        let jwk = jwks
            .keys
            .iter()
            .find(|k| k.common.key_id.as_deref() == Some(kid))
            .ok_or_else(|| format!("No JWK found for kid: {kid}"))?;

        let key =
            DecodingKey::from_jwk(jwk).map_err(|e| format!("Failed to build key from JWK: {e}"))?;

        let validation = validation_for(Algorithm::ES256, Some(&self.issuer));

        let data = decode::<Claims>(token, &key, &validation)
            .map_err(|e| format!("JWT verification failed: {e}"))?;

        validate_authenticated_claims(data.claims)
    }

    async fn get_jwks(&self) -> Result<jwk::JwkSet, String> {
        // Check cache first.
        {
            let cached = self.cached_jwks.read().await;
            if let Some(ref jwks) = *cached {
                return Ok(jwks.clone());
            }
        }

        // Fetch from Supabase.
        let resp = self
            .http
            .get(&self.jwks_url)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch JWKS: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("JWKS fetch returned {}", resp.status()));
        }

        let jwks: jwk::JwkSet = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse JWKS: {e}"))?;

        // Cache it.
        {
            let mut cached = self.cached_jwks.write().await;
            *cached = Some(jwks.clone());
        }

        Ok(jwks)
    }
}

/// Verify a legacy HS256 JWT with shared secret.
fn verify_hs256(token: &str, jwt_secret: &str, issuer: Option<&str>) -> Result<Claims, String> {
    let key = DecodingKey::from_secret(jwt_secret.as_bytes());
    let validation = validation_for(Algorithm::HS256, issuer);

    let data = decode::<Claims>(token, &key, &validation)
        .map_err(|e| format!("JWT verification failed: {e}"))?;

    validate_authenticated_claims(data.claims)
}

fn validation_for(algorithm: Algorithm, issuer: Option<&str>) -> Validation {
    let mut validation = Validation::new(algorithm);
    validation.set_audience(&["authenticated"]);
    if let Some(issuer) = issuer {
        validation.set_issuer(&[issuer]);
    }
    validation
}

fn validate_authenticated_claims(claims: Claims) -> Result<Claims, String> {
    if claims.sub.is_empty() {
        return Err("JWT missing sub claim".into());
    }

    if claims.role != "authenticated" {
        return Err("JWT role is not authenticated".into());
    }

    Ok(claims)
}

/// Extract bearer token from an Authorization header value.
pub fn extract_bearer(header: &str) -> Option<&str> {
    header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};

    const TEST_SECRET: &str = "super-secret-jwt-token-for-testing-only";
    const TEST_ISSUER: &str = "https://example.supabase.co/auth/v1";

    fn make_hs256_jwt(sub: &str, role: &str, exp_offset: i64) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = Claims {
            sub: sub.to_string(),
            aud: Some("authenticated".to_string()),
            iss: Some(TEST_ISSUER.to_string()),
            exp: (now as i64 + exp_offset) as u64,
            iat: now,
            role: role.to_string(),
            email: Some("test@example.com".to_string()),
        };
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(TEST_SECRET.as_bytes()),
        )
        .unwrap()
    }

    #[test]
    fn valid_hs256_jwt() {
        let token = make_hs256_jwt("user-123", "authenticated", 3600);
        let claims = verify_hs256(&token, TEST_SECRET, Some(TEST_ISSUER)).unwrap();
        assert_eq!(claims.sub, "user-123");
        assert_eq!(claims.role, "authenticated");
        assert_eq!(claims.email, Some("test@example.com".to_string()));
    }

    #[test]
    fn expired_hs256_jwt_rejected() {
        let token = make_hs256_jwt("user-123", "authenticated", -3600);
        let result = verify_hs256(&token, TEST_SECRET, Some(TEST_ISSUER));
        assert!(result.is_err(), "expired JWT should be rejected");
    }

    #[test]
    fn wrong_secret_rejected() {
        let token = make_hs256_jwt("user-123", "authenticated", 3600);
        let result = verify_hs256(&token, "wrong-secret", Some(TEST_ISSUER));
        assert!(result.is_err());
    }

    #[test]
    fn wrong_issuer_rejected() {
        let token = make_hs256_jwt("user-123", "authenticated", 3600);
        let result = verify_hs256(
            &token,
            TEST_SECRET,
            Some("https://other-project.supabase.co/auth/v1"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn anon_role_rejected() {
        let token = make_hs256_jwt("user-123", "anon", 3600);
        let result = verify_hs256(&token, TEST_SECRET, Some(TEST_ISSUER));
        assert!(result.is_err());
    }

    #[test]
    fn extract_bearer_works() {
        assert_eq!(extract_bearer("Bearer abc123"), Some("abc123"));
        assert_eq!(extract_bearer("bearer abc123"), Some("abc123"));
        assert_eq!(extract_bearer("Basic abc123"), None);
        assert_eq!(extract_bearer(""), None);
    }

    #[tokio::test]
    async fn verifier_rejects_garbage_token() {
        let v = JwtVerifier::new("https://example.supabase.co", "secret");
        let result = v.verify("not.a.jwt").await;
        assert!(result.is_err());
    }
}
