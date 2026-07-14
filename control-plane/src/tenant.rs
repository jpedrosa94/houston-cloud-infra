//! Tenant CRUD via Supabase PostgREST.
//!
//! The control plane uses the `service_role` key to bypass RLS and
//! manage tenant rows. Each user gets exactly one tenant (1:1).

use rand::Rng;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// How long a tenant can be idle before auto-suspend.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(3600); // 1 hour

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tenant {
    pub id: String,
    pub user_id: String,
    pub tenant_id: String,
    pub engine_token: String,
    pub status: TenantStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TenantStatus {
    Pending,
    Provisioning,
    Ready,
    Error,
    Suspended,
}

impl std::fmt::Display for TenantStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Provisioning => write!(f, "provisioning"),
            Self::Ready => write!(f, "ready"),
            Self::Error => write!(f, "error"),
            Self::Suspended => write!(f, "suspended"),
        }
    }
}

/// Generate a short tenant ID: "t-" + 8 hex chars.
pub fn generate_tenant_id() -> String {
    let mut rng = rand::thread_rng();
    let bytes: [u8; 4] = rng.gen();
    format!("t-{}", hex::encode(&bytes))
}

/// Generate a 48-char alphanumeric engine token.
pub fn generate_engine_token() -> String {
    use rand::distributions::Alphanumeric;
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect()
}

/// Supabase PostgREST client for tenant operations.
#[derive(Clone)]
pub struct TenantStore {
    client: reqwest::Client,
    base_url: String,
    service_key: String,
}

impl TenantStore {
    pub fn new(supabase_url: &str, service_role_key: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: format!("{}/rest/v1", supabase_url.trim_end_matches('/')),
            service_key: service_role_key.to_string(),
        }
    }

    /// Attach Supabase service-role auth headers to a request.
    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.header("apikey", &self.service_key)
            .header("Authorization", format!("Bearer {}", self.service_key))
    }

    /// Send a request and check the response status.
    async fn send_ok(
        &self,
        req: reqwest::RequestBuilder,
        context: &str,
    ) -> Result<reqwest::Response, String> {
        let resp = self
            .auth(req)
            .send()
            .await
            .map_err(|e| format!("Supabase {context} failed: {e}"))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Supabase {context} error: {body}"));
        }

        Ok(resp)
    }

    /// Find tenant by user_id. Returns None if no tenant exists.
    pub async fn find_by_user(&self, user_id: &str) -> Result<Option<Tenant>, String> {
        let req = self
            .client
            .get(format!("{}/tenants", self.base_url))
            .query(&[("user_id", format!("eq.{user_id}")), ("select", "*".into())]);

        let tenants: Vec<Tenant> = self
            .send_ok(req, "find_by_user")
            .await?
            .json()
            .await
            .map_err(|e| format!("Failed to parse tenant response: {e}"))?;

        Ok(tenants.into_iter().next())
    }

    /// Create a new tenant row.
    pub async fn create(&self, user_id: &str) -> Result<Tenant, String> {
        let tenant_id = generate_tenant_id();
        let engine_token = generate_engine_token();

        #[derive(Serialize)]
        struct Insert {
            user_id: String,
            tenant_id: String,
            engine_token: String,
            status: String,
        }

        let body = Insert {
            user_id: user_id.to_string(),
            tenant_id: tenant_id.clone(),
            engine_token: engine_token.clone(),
            status: "provisioning".to_string(),
        };

        let req = self
            .client
            .post(format!("{}/tenants", self.base_url))
            .header("Prefer", "return=representation")
            .json(&body);

        let tenants: Vec<Tenant> = self
            .send_ok(req, "insert")
            .await?
            .json()
            .await
            .map_err(|e| format!("Failed to parse created tenant: {e}"))?;

        tenants
            .into_iter()
            .next()
            .ok_or_else(|| "No tenant returned after insert".to_string())
    }

    /// Update tenant status.
    pub async fn update_status(&self, tenant_id: &str, status: TenantStatus) -> Result<(), String> {
        #[derive(Serialize)]
        struct Patch {
            status: String,
        }

        let req = self
            .client
            .patch(format!("{}/tenants", self.base_url))
            .query(&[("tenant_id", format!("eq.{tenant_id}"))])
            .json(&Patch {
                status: status.to_string(),
            });

        self.send_ok(req, "update_status").await?;
        Ok(())
    }

    /// Update last_active_at to now. Called on every session request.
    pub async fn touch_last_active(&self, tenant_id: &str) -> Result<(), String> {
        #[derive(Serialize)]
        struct Patch {
            last_active_at: String,
        }

        let req = self
            .client
            .patch(format!("{}/tenants", self.base_url))
            .query(&[("tenant_id", format!("eq.{tenant_id}"))])
            .json(&Patch {
                last_active_at: now_iso(),
            });

        self.send_ok(req, "touch_last_active").await?;
        Ok(())
    }

    /// Find all tenants with status=ready that have been idle longer than the timeout.
    pub async fn find_idle_tenants(&self, idle_seconds: u64) -> Result<Vec<Tenant>, String> {
        let cutoff_time = chrono::Utc::now() - chrono::Duration::seconds(idle_seconds as i64);
        let cutoff = format!("lt.{}", cutoff_time.to_rfc3339());

        let req = self
            .client
            .get(format!("{}/tenants", self.base_url))
            .query(&[
                ("status", "eq.ready".to_string()),
                ("last_active_at", cutoff),
                ("select", "*".to_string()),
            ]);

        self.send_ok(req, "find_idle")
            .await?
            .json()
            .await
            .map_err(|e| format!("Failed to parse idle tenants: {e}"))
    }

    /// Delete tenant row (for account teardown).
    pub async fn delete(&self, tenant_id: &str) -> Result<(), String> {
        let req = self
            .client
            .delete(format!("{}/tenants", self.base_url))
            .query(&[("tenant_id", format!("eq.{tenant_id}"))]);

        self.send_ok(req, "delete").await?;
        Ok(())
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

// hex encoding helper (avoid adding a dep for 2 lines)
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_id_format() {
        let id = generate_tenant_id();
        assert!(id.starts_with("t-"), "should start with t-: {id}");
        assert_eq!(id.len(), 10, "t- + 8 hex chars: {id}");
        // All chars after "t-" should be hex
        assert!(id[2..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn engine_token_length() {
        let token = generate_engine_token();
        assert_eq!(token.len(), 48);
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn tenant_id_uniqueness() {
        let a = generate_tenant_id();
        let b = generate_tenant_id();
        assert_ne!(a, b, "two generated IDs should differ");
    }

    #[test]
    fn tenant_status_display() {
        assert_eq!(TenantStatus::Pending.to_string(), "pending");
        assert_eq!(TenantStatus::Provisioning.to_string(), "provisioning");
        assert_eq!(TenantStatus::Ready.to_string(), "ready");
        assert_eq!(TenantStatus::Error.to_string(), "error");
        assert_eq!(TenantStatus::Suspended.to_string(), "suspended");
    }

    #[test]
    fn idle_duration_is_one_hour() {
        assert_eq!(super::IDLE_TIMEOUT.as_secs(), 3600);
    }

    #[test]
    fn tenant_status_serde_roundtrip() {
        let json = serde_json::to_string(&TenantStatus::Ready).unwrap();
        assert_eq!(json, "\"ready\"");
        let back: TenantStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TenantStatus::Ready);
    }
}
