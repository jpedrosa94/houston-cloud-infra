//! Control plane REST routes.
//!
//! - `POST /api/session` — authenticate + return engine URL/token (provision if new)
//! - `GET  /api/tenant/status` — check tenant provisioning status
//! - `POST /api/tenant/suspend` — suspend tenant (delete Pod only)
//! - `DELETE /api/tenant` — tear down tenant
//! - `GET  /ext-authz` — Envoy ext-authz for Gateway token injection

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Serialize;
use std::sync::Arc;

use crate::auth::{self, JwtVerifier};
use crate::tenant::{TenantStatus, TenantStore};

pub struct AppState {
    pub tenant_store: TenantStore,
    pub jwt_verifier: JwtVerifier,
    pub engine_url_template: String,
    pub dev_engine_token: Option<String>,
    pub engine_image: Option<String>,
    pub gateway_domain: Option<String>,
    pub gateway_scheme: String,
    pub gvisor: bool,
    pub storage_class: String,
}

impl AppState {
    pub(crate) fn k8s_spec(
        &self,
        tenant: &crate::tenant::Tenant,
    ) -> Option<crate::k8s::TenantSpec> {
        self.engine_image
            .as_ref()
            .map(|image| crate::k8s::TenantSpec {
                tenant_id: tenant.tenant_id.clone(),
                engine_token: tenant.engine_token.clone(),
                engine_image: image.clone(),
                gateway_domain: self.gateway_domain.clone(),
                gvisor: self.gvisor,
                storage_class: self.storage_class.clone(),
            })
    }
}

#[derive(Serialize)]
pub struct SessionResponse {
    pub engine_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_token: Option<String>,
    pub tenant_id: String,
    pub status: TenantStatus,
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub tenant_id: String,
    pub status: TenantStatus,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

fn err(status: StatusCode, msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: msg.to_string(),
        }),
    )
}

async fn authenticate(
    headers: &HeaderMap,
    verifier: &JwtVerifier,
) -> Result<auth::Claims, (StatusCode, Json<ErrorResponse>)> {
    let header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "Missing Authorization header"))?;
    let token = auth::extract_bearer(header)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "Invalid Authorization header"))?;
    verifier.verify(token).await.map_err(|e| {
        tracing::warn!("authentication failed: {e}");
        err(StatusCode::UNAUTHORIZED, "Unauthorized")
    })
}

pub(crate) async fn check_engine_health(engine_url: &str, engine_token: &str) -> bool {
    let url = format!("{}/v1/health", engine_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_default();
    client
        .get(&url)
        .header("Authorization", format!("Bearer {engine_token}"))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

pub(crate) fn response_token(state: &AppState, tenant_token: &str) -> Option<String> {
    if state.gateway_domain.is_some() {
        return None;
    }

    if let Some(ref dev_token) = state.dev_engine_token {
        Some(dev_token.clone())
    } else {
        Some(tenant_token.to_string())
    }
}

pub(crate) fn resolve_tenant_url(state: &AppState, tenant_id: &str) -> String {
    if let Some(ref domain) = state.gateway_domain {
        crate::k8s::tenant_public_url(tenant_id, &state.gateway_scheme, domain)
    } else {
        engine_url(&state.engine_url_template, tenant_id)
    }
}

pub(crate) fn engine_url(template: &str, tenant_id: &str) -> String {
    template.replace("{tenant_id}", tenant_id)
}

// --- Route handlers ---

/// `POST /api/session`
pub async fn create_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<SessionResponse>, (StatusCode, Json<ErrorResponse>)> {
    let claims = authenticate(&headers, &state.jwt_verifier).await?;

    let existing = state
        .tenant_store
        .find_by_user(&claims.sub)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, &e))?;

    match existing {
        Some(tenant) => crate::session::handle_existing(&state, tenant).await,
        None => crate::session::provision_new(&state, &claims.sub).await,
    }
}

/// `GET /api/tenant/status`
pub async fn tenant_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let claims = authenticate(&headers, &state.jwt_verifier).await?;
    let tenant = state
        .tenant_store
        .find_by_user(&claims.sub)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, &e))?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "No tenant found"))?;

    Ok(Json(StatusResponse {
        tenant_id: tenant.tenant_id,
        status: tenant.status,
    }))
}

/// `POST /api/tenant/suspend`
pub async fn suspend_tenant(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let claims = authenticate(&headers, &state.jwt_verifier).await?;
    let tenant = state
        .tenant_store
        .find_by_user(&claims.sub)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, &e))?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "No tenant found"))?;

    if tenant.status != TenantStatus::Ready {
        return Err(err(StatusCode::CONFLICT, "Tenant is not running"));
    }

    if state.engine_image.is_some() {
        crate::k8s::suspend_tenant(&tenant.tenant_id)
            .await
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, &e))?;
    }

    state
        .tenant_store
        .update_status(&tenant.tenant_id, TenantStatus::Suspended)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, &e))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/tenant`
pub async fn delete_tenant(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let claims = authenticate(&headers, &state.jwt_verifier).await?;
    let tenant = state
        .tenant_store
        .find_by_user(&claims.sub)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, &e))?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "No tenant found"))?;

    if state.engine_image.is_some() {
        crate::k8s::deprovision_tenant(&tenant.tenant_id)
            .await
            .map_err(|e| {
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("K8s deprovision failed: {e}"),
                )
            })?;
    }

    state
        .tenant_store
        .delete(&tenant.tenant_id)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, &e))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /ext-authz` — Envoy ext-authz endpoint for Gateway token injection.
pub async fn ext_authz(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, HeaderMap), (StatusCode, Json<ErrorResponse>)> {
    let claims = authenticate(&headers, &state.jwt_verifier).await?;
    let tenant = state
        .tenant_store
        .find_by_user(&claims.sub)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, &e))?
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "No tenant provisioned"))?;

    if tenant.status != TenantStatus::Ready {
        return Err(err(StatusCode::SERVICE_UNAVAILABLE, "Tenant not ready"));
    }

    // Touch activity asynchronously — don't block the auth response.
    let store = state.tenant_store.clone();
    let tid = tenant.tenant_id.clone();
    tokio::spawn(async move {
        let _ = store.touch_last_active(&tid).await;
    });

    let backend_url = crate::k8s::tenant_engine_url(&tenant.tenant_id);
    let mut resp_headers = axum::http::HeaderMap::new();
    resp_headers.insert("x-houston-tenant", tenant.tenant_id.parse().unwrap());
    resp_headers.insert("x-houston-backend", backend_url.parse().unwrap());
    resp_headers.insert(
        "authorization",
        format!("Bearer {}", tenant.engine_token).parse().unwrap(),
    );

    Ok((StatusCode::OK, resp_headers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_url_replaces_placeholder() {
        let tmpl = "http://tenant-{tenant_id}.houston-tenants.svc.cluster.local:7777";
        assert_eq!(
            engine_url(tmpl, "t-abc12345"),
            "http://tenant-t-abc12345.houston-tenants.svc.cluster.local:7777"
        );
    }

    #[test]
    fn engine_url_localhost_passthrough() {
        let tmpl = "http://localhost:7777";
        assert_eq!(engine_url(tmpl, "t-abc12345"), "http://localhost:7777");
    }

    #[test]
    fn error_response_serializes() {
        let json = serde_json::to_string(&ErrorResponse {
            error: "not found".into(),
        })
        .unwrap();
        assert!(json.contains("not found"));
    }

    #[tokio::test]
    async fn health_check_returns_false_for_unreachable() {
        assert!(!check_engine_health("http://127.0.0.1:19999", "fake-token").await);
    }

    #[test]
    fn session_response_serializes() {
        let resp = SessionResponse {
            engine_url: "http://localhost:7777".into(),
            engine_token: Some("abc".into()),
            tenant_id: "t-12345678".into(),
            status: TenantStatus::Ready,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("ready"));
        assert!(json.contains("t-12345678"));
    }

    #[test]
    fn response_token_hidden_when_gateway_enabled() {
        let state = test_state(Some("engine.example.com".to_string()), None);
        assert_eq!(response_token(&state, "tenant-token"), None);
    }

    #[test]
    fn response_token_uses_dev_token_without_gateway() {
        let state = test_state(None, Some("dev-token".to_string()));
        assert_eq!(
            response_token(&state, "tenant-token").as_deref(),
            Some("dev-token")
        );
    }

    fn test_state(gateway_domain: Option<String>, dev_engine_token: Option<String>) -> AppState {
        AppState {
            tenant_store: TenantStore::new("https://example.supabase.co", "service-key"),
            jwt_verifier: JwtVerifier::new("https://example.supabase.co", "jwt-secret"),
            engine_url_template: "http://localhost:7777".to_string(),
            dev_engine_token,
            engine_image: None,
            gateway_domain,
            gateway_scheme: "https".to_string(),
            gvisor: false,
            storage_class: "standard".to_string(),
        }
    }
}
