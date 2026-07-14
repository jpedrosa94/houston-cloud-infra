//! Background task that auto-suspends idle tenants.
//!
//! Runs every 5 minutes, queries Supabase for tenants with
//! `status=ready` and `last_active_at` older than `IDLE_TIMEOUT`.
//! Suspends idle tenants in parallel (bounded concurrency).

use crate::tenant::{TenantStatus, TenantStore, IDLE_TIMEOUT};
use std::time::Duration;

const SWEEP_INTERVAL: Duration = Duration::from_secs(300); // 5 minutes

/// Max concurrent suspend operations per sweep cycle.
const MAX_CONCURRENT_SUSPENSIONS: usize = 20;

/// Spawn the idle sweeper as a background tokio task.
pub fn spawn(tenant_store: TenantStore, k8s_enabled: bool) {
    tokio::spawn(async move {
        tracing::info!(
            "[idle-sweeper] started: sweep every {}s, idle timeout {}s, max concurrent {}",
            SWEEP_INTERVAL.as_secs(),
            IDLE_TIMEOUT.as_secs(),
            MAX_CONCURRENT_SUSPENSIONS,
        );
        loop {
            tokio::time::sleep(SWEEP_INTERVAL).await;
            if let Err(e) = sweep(&tenant_store, k8s_enabled).await {
                tracing::error!("[idle-sweeper] sweep failed: {e}");
            }
        }
    });
}

async fn sweep(store: &TenantStore, k8s_enabled: bool) -> Result<(), String> {
    let idle = store.find_idle_tenants(IDLE_TIMEOUT.as_secs()).await?;

    if idle.is_empty() {
        return Ok(());
    }

    let total = idle.len();
    tracing::info!("[idle-sweeper] found {total} idle tenant(s), suspending in parallel");

    // Process in chunks of MAX_CONCURRENT_SUSPENSIONS.
    let mut suspended = 0usize;
    let mut failed = 0usize;

    for chunk in idle.chunks(MAX_CONCURRENT_SUSPENSIONS) {
        let mut handles = Vec::with_capacity(chunk.len());

        for tenant in chunk {
            let tenant_id = tenant.tenant_id.clone();
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                suspend_one(&tenant_id, &store, k8s_enabled).await
            }));
        }

        for handle in handles {
            match handle.await {
                Ok(Ok(())) => suspended += 1,
                Ok(Err(e)) => {
                    tracing::error!("[idle-sweeper] {e}");
                    failed += 1;
                }
                Err(e) => {
                    tracing::error!("[idle-sweeper] task panicked: {e}");
                    failed += 1;
                }
            }
        }
    }

    tracing::info!("[idle-sweeper] done: {suspended} suspended, {failed} failed (of {total})");
    Ok(())
}

async fn suspend_one(
    tenant_id: &str,
    store: &TenantStore,
    k8s_enabled: bool,
) -> Result<(), String> {
    if k8s_enabled {
        crate::k8s::suspend_tenant(tenant_id)
            .await
            .map_err(|e| format!("pod suspend failed for {tenant_id}: {e}"))?;
    }

    store
        .update_status(tenant_id, TenantStatus::Suspended)
        .await
        .map_err(|e| format!("status update failed for {tenant_id}: {e}"))?;

    tracing::info!("[idle-sweeper] suspended {tenant_id}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_interval_is_five_minutes() {
        assert_eq!(SWEEP_INTERVAL.as_secs(), 300);
    }

    #[test]
    fn idle_timeout_is_one_hour() {
        assert_eq!(IDLE_TIMEOUT.as_secs(), 3600);
    }

    #[test]
    fn max_concurrent_suspensions_is_bounded() {
        assert_eq!(MAX_CONCURRENT_SUSPENSIONS, 20);
    }
}
