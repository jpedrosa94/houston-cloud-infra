//! Session creation logic — the core of `POST /api/session`.
//!
//! Handles existing tenants (wake, health check, reprovision) and
//! new tenant provisioning. Extracted from routes.rs for readability.

use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

use crate::routes::{AppState, ErrorResponse, SessionResponse};
use crate::tenant::{Tenant, TenantStatus};

type RouteResult = Result<Json<SessionResponse>, (StatusCode, Json<ErrorResponse>)>;

fn err(status: StatusCode, msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: msg.to_string(),
        }),
    )
}

fn ise(msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    err(StatusCode::INTERNAL_SERVER_ERROR, msg)
}

/// Handle an existing tenant: wake if suspended, health-check if provisioning,
/// reprovision if pod missing.
pub async fn handle_existing(state: &Arc<AppState>, tenant: Tenant) -> RouteResult {
    state
        .tenant_store
        .touch_last_active(&tenant.tenant_id)
        .await
        .map_err(|e| ise(&e))?;

    let token = state
        .dev_engine_token
        .as_deref()
        .unwrap_or(&tenant.engine_token);

    if tenant.status == TenantStatus::Suspended {
        return wake_suspended(state, tenant).await;
    }

    let url = super::routes::resolve_tenant_url(state, &tenant.tenant_id);
    let health_url = crate::k8s::tenant_engine_url(&tenant.tenant_id);
    let status = resolve_status(state, &tenant, &health_url, token).await?;

    Ok(Json(SessionResponse {
        engine_url: url,
        engine_token: super::routes::response_token(state, &tenant.engine_token),
        tenant_id: tenant.tenant_id,
        status,
    }))
}

async fn wake_suspended(state: &Arc<AppState>, tenant: Tenant) -> RouteResult {
    let k8s_spec = state
        .k8s_spec(&tenant)
        .ok_or_else(|| ise("K8s not configured"))?;
    crate::k8s::wake_tenant(&k8s_spec)
        .await
        .map_err(|e| ise(&format!("Wake failed: {e}")))?;
    state
        .tenant_store
        .update_status(&tenant.tenant_id, TenantStatus::Provisioning)
        .await
        .map_err(|e| ise(&e))?;

    let url = super::routes::resolve_tenant_url(state, &tenant.tenant_id);
    Ok(Json(SessionResponse {
        engine_url: url,
        engine_token: super::routes::response_token(state, &tenant.engine_token),
        tenant_id: tenant.tenant_id,
        status: TenantStatus::Provisioning,
    }))
}

async fn resolve_status(
    state: &Arc<AppState>,
    tenant: &Tenant,
    url: &str,
    token: &str,
) -> Result<TenantStatus, (StatusCode, Json<ErrorResponse>)> {
    if tenant.status != TenantStatus::Provisioning && tenant.status != TenantStatus::Error {
        return Ok(tenant.status.clone());
    }

    if super::routes::check_engine_health(url, token).await {
        state
            .tenant_store
            .update_status(&tenant.tenant_id, TenantStatus::Ready)
            .await
            .map_err(|e| ise(&e))?;
        return Ok(TenantStatus::Ready);
    }

    if let Some(ref k8s_spec) = state.k8s_spec(tenant) {
        reprovision_if_missing(state, tenant, k8s_spec).await?;
    }
    Ok(TenantStatus::Provisioning)
}

async fn reprovision_if_missing(
    state: &Arc<AppState>,
    tenant: &Tenant,
    k8s_spec: &crate::k8s::TenantSpec,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let exists = crate::k8s::pod_exists(&tenant.tenant_id)
        .await
        .map_err(|e| ise(&format!("K8s unreachable: {e}")))?;

    if exists {
        return Ok(());
    }

    tracing::info!(
        "[session] pod missing for tenant {}, reprovisioning",
        tenant.tenant_id
    );
    if let Err(e) = crate::k8s::provision_tenant(k8s_spec).await {
        tracing::error!("[session] provision failed for {}: {e}", tenant.tenant_id);
        state
            .tenant_store
            .update_status(&tenant.tenant_id, TenantStatus::Error)
            .await
            .map_err(|e| ise(&e))?;
        return Err(ise(&e));
    }
    state
        .tenant_store
        .update_status(&tenant.tenant_id, TenantStatus::Provisioning)
        .await
        .map_err(|e| ise(&e))?;
    Ok(())
}

/// Provision a brand new tenant (no existing row in DB).
pub async fn provision_new(state: &Arc<AppState>, user_id: &str) -> RouteResult {
    let tenant = state
        .tenant_store
        .create(user_id)
        .await
        .map_err(|e| ise(&e))?;

    let (url, health_url, token) = if let Some(ref k8s_spec) = state.k8s_spec(&tenant) {
        if let Err(e) = crate::k8s::provision_tenant(k8s_spec).await {
            tracing::error!("[session] K8s provisioning failed: {e}");
            state
                .tenant_store
                .update_status(&tenant.tenant_id, TenantStatus::Error)
                .await
                .map_err(|e| ise(&e))?;
            return Err(ise(&e));
        }
        let url = super::routes::resolve_tenant_url(state, &tenant.tenant_id);
        let health = crate::k8s::tenant_engine_url(&tenant.tenant_id);
        (url, health, tenant.engine_token.clone())
    } else {
        let url = super::routes::engine_url(&state.engine_url_template, &tenant.tenant_id);
        let tok = state
            .dev_engine_token
            .clone()
            .unwrap_or(tenant.engine_token.clone());
        (url.clone(), url, tok)
    };

    let status = if super::routes::check_engine_health(&health_url, &token).await {
        state
            .tenant_store
            .update_status(&tenant.tenant_id, TenantStatus::Ready)
            .await
            .map_err(|e| ise(&e))?;
        TenantStatus::Ready
    } else {
        TenantStatus::Provisioning
    };

    Ok(Json(SessionResponse {
        engine_url: url,
        engine_token: super::routes::response_token(state, &tenant.engine_token),
        tenant_id: tenant.tenant_id,
        status,
    }))
}
