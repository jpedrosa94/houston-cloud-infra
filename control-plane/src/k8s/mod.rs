//! Kubernetes tenant provisioning.
//!
//! Renders the tenant pod template and applies PVC + Secret + Pod + Service
//! via the K8s API. Each tenant gets isolated resources in the
//! `houston-tenants` namespace.

mod manifests;

use k8s_openapi::api::core::v1::{PersistentVolumeClaim, Pod, Secret, Service};
use kube::{
    api::{Api, DeleteParams, PostParams},
    Client,
};

pub use manifests::{build_httproute, build_pod, build_pvc, build_secret, build_service};

const NAMESPACE: &str = "houston-tenants";

/// Configuration for provisioning a tenant.
pub struct TenantSpec {
    pub tenant_id: String,
    pub engine_token: String,
    pub engine_image: String,
    pub gateway_domain: Option<String>,
    pub gvisor: bool,
    pub storage_class: String,
}

/// Ignore K8s AlreadyExists errors — makes provisioning idempotent.
/// Returns Ok(true) if created, Ok(false) if already existed.
fn ignore_already_exists(result: Result<impl std::any::Any, kube::Error>) -> Result<bool, String> {
    match result {
        Ok(_) => Ok(true),
        Err(kube::Error::Api(ref e)) if e.code == 409 => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

fn httproute_api_resource() -> kube::api::ApiResource {
    kube::api::ApiResource {
        group: "gateway.networking.k8s.io".into(),
        version: "v1".into(),
        api_version: "gateway.networking.k8s.io/v1".into(),
        kind: "HTTPRoute".into(),
        plural: "httproutes".into(),
    }
}

/// Provision all K8s resources for a tenant (idempotent, parallelized).
pub async fn provision_tenant(spec: &TenantSpec) -> Result<(), String> {
    let client = Client::try_default()
        .await
        .map_err(|e| format!("K8s client init failed: {e}"))?;

    // Phase 1: PVC + Secret in parallel.
    let pvc_api: Api<PersistentVolumeClaim> = Api::namespaced(client.clone(), NAMESPACE);
    let secret_api: Api<Secret> = Api::namespaced(client.clone(), NAMESPACE);
    let pp = PostParams::default();

    let pvc_manifest = build_pvc(spec);
    let secret_manifest = build_secret(spec);
    let (pvc_result, secret_result) = tokio::join!(
        pvc_api.create(&pp, &pvc_manifest),
        secret_api.create(&pp, &secret_manifest),
    );
    let mut created = false;
    created |=
        ignore_already_exists(pvc_result).map_err(|e| format!("Failed to create PVC: {e}"))?;
    created |= ignore_already_exists(secret_result)
        .map_err(|e| format!("Failed to create Secret: {e}"))?;

    // Phase 2: Pod (depends on PVC + Secret).
    let pod_api: Api<Pod> = Api::namespaced(client.clone(), NAMESPACE);
    created |= ignore_already_exists(pod_api.create(&pp, &build_pod(spec)).await)
        .map_err(|e| format!("Failed to create Pod: {e}"))?;

    // Phase 3: Service + HTTPRoute in parallel.
    let svc_api: Api<Service> = Api::namespaced(client.clone(), NAMESPACE);

    if spec.gateway_domain.is_some() {
        let route_json = build_httproute(spec);
        let route_api: Api<kube::api::DynamicObject> =
            Api::namespaced_with(client.clone(), NAMESPACE, &httproute_api_resource());
        let route_obj: kube::api::DynamicObject =
            serde_json::from_value(route_json).map_err(|e| format!("Invalid HTTPRoute: {e}"))?;

        let svc_manifest = build_service(spec);
        let (svc_result, route_result) = tokio::join!(
            svc_api.create(&pp, &svc_manifest),
            route_api.create(&pp, &route_obj),
        );
        created |= ignore_already_exists(svc_result)
            .map_err(|e| format!("Failed to create Service: {e}"))?;
        created |= ignore_already_exists(route_result)
            .map_err(|e| format!("Failed to create HTTPRoute: {e}"))?;
    } else {
        let svc_manifest = build_service(spec);
        created |= ignore_already_exists(svc_api.create(&pp, &svc_manifest).await)
            .map_err(|e| format!("Failed to create Service: {e}"))?;
    }

    if created {
        tracing::info!(
            "[k8s] provisioned tenant {}: PVC + Secret + Pod + Service{}",
            spec.tenant_id,
            if spec.gateway_domain.is_some() {
                " + HTTPRoute"
            } else {
                ""
            }
        );
    } else {
        tracing::debug!("[k8s] tenant {} already fully provisioned", spec.tenant_id);
    }
    Ok(())
}

/// Delete all K8s resources for a tenant.
pub async fn deprovision_tenant(tenant_id: &str) -> Result<(), String> {
    let client = Client::try_default()
        .await
        .map_err(|e| format!("K8s client init failed: {e}"))?;

    let dp = DeleteParams::default();
    let name = format!("tenant-{tenant_id}");
    let secret_name = format!("tenant-{tenant_id}-token");
    let pvc_name = format!("tenant-{tenant_id}-data");

    let route_api: Api<kube::api::DynamicObject> =
        Api::namespaced_with(client.clone(), NAMESPACE, &httproute_api_resource());
    let svc_api: Api<Service> = Api::namespaced(client.clone(), NAMESPACE);
    let pod_api: Api<Pod> = Api::namespaced(client.clone(), NAMESPACE);

    let (_, _, _) = tokio::join!(
        route_api.delete(&name, &dp),
        svc_api.delete(&name, &dp),
        pod_api.delete(&name, &dp),
    );

    let secret_api: Api<Secret> = Api::namespaced(client.clone(), NAMESPACE);
    let pvc_api: Api<PersistentVolumeClaim> = Api::namespaced(client.clone(), NAMESPACE);

    let (_, _) = tokio::join!(
        secret_api.delete(&secret_name, &dp),
        pvc_api.delete(&pvc_name, &dp),
    );

    tracing::info!("[k8s] deprovisioned tenant {tenant_id}");
    Ok(())
}

/// Check if a tenant's pod exists in K8s.
pub async fn pod_exists(tenant_id: &str) -> Result<bool, String> {
    let client = Client::try_default()
        .await
        .map_err(|e| format!("K8s client init failed: {e}"))?;
    let pods: Api<Pod> = Api::namespaced(client, NAMESPACE);
    let name = pod_name(tenant_id);
    match pods.get(&name).await {
        Ok(_) => Ok(true),
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(false),
        Err(e) => Err(format!("Failed to check pod: {e}")),
    }
}

/// Suspend a tenant — delete only the Pod.
pub async fn suspend_tenant(tenant_id: &str) -> Result<(), String> {
    let client = Client::try_default()
        .await
        .map_err(|e| format!("K8s client init failed: {e}"))?;

    let pods: Api<Pod> = Api::namespaced(client, NAMESPACE);
    pods.delete(&pod_name(tenant_id), &DeleteParams::default())
        .await
        .map_err(|e| format!("Failed to delete pod: {e}"))?;

    tracing::info!("[k8s] suspended tenant {tenant_id}");
    Ok(())
}

/// Wake a suspended tenant — recreate the Pod with existing PVC.
pub async fn wake_tenant(spec: &TenantSpec) -> Result<(), String> {
    let client = Client::try_default()
        .await
        .map_err(|e| format!("K8s client init failed: {e}"))?;

    let pods: Api<Pod> = Api::namespaced(client, NAMESPACE);
    pods.create(&PostParams::default(), &build_pod(spec))
        .await
        .map_err(|e| format!("Failed to create pod: {e}"))?;

    tracing::info!(
        "[k8s] woke tenant {}: pod recreated with existing PVC",
        spec.tenant_id
    );
    Ok(())
}

fn pod_name(tenant_id: &str) -> String {
    format!("tenant-{tenant_id}")
}

/// Public URL for a tenant via the Gateway (browser-accessible).
pub fn tenant_public_url(tenant_id: &str, scheme: &str, domain: &str) -> String {
    format!("{scheme}://{tenant_id}.{domain}")
}

/// Cluster-internal URL for a tenant (control plane health checks).
pub fn tenant_engine_url(tenant_id: &str) -> String {
    format!("http://tenant-{tenant_id}.{NAMESPACE}.svc.cluster.local:7777")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_spec() -> TenantSpec {
        TenantSpec {
            tenant_id: "t-abc12345".to_string(),
            engine_token: "test-token-abc123".to_string(),
            engine_image: "houston/engine:latest".to_string(),
            gateway_domain: None,
            gvisor: false,
            storage_class: "standard".to_string(),
        }
    }

    #[test]
    fn pod_name_matches_tenant_id() {
        assert_eq!(pod_name("t-abc12345"), "tenant-t-abc12345");
    }

    #[test]
    fn tenant_engine_url_format() {
        assert_eq!(
            tenant_engine_url("t-abc12345"),
            "http://tenant-t-abc12345.houston-tenants.svc.cluster.local:7777"
        );
    }

    #[test]
    fn tenant_public_url_format() {
        assert_eq!(
            tenant_public_url("t-abc12345", "https", "engine.houston.cloud"),
            "https://t-abc12345.engine.houston.cloud"
        );
    }

    #[test]
    fn suspend_targets_correct_pod() {
        assert_eq!(pod_name("t-abc12345"), "tenant-t-abc12345");
    }

    #[test]
    fn ignore_already_exists_returns_true_on_create() {
        assert!(ignore_already_exists(Ok::<_, kube::Error>(())).unwrap());
    }

    #[test]
    fn ignore_already_exists_returns_false_on_409() {
        let err = kube::Error::Api(kube::error::ErrorResponse {
            status: "Failure".into(),
            message: "already exists".into(),
            reason: "AlreadyExists".into(),
            code: 409,
        });
        assert!(!ignore_already_exists(Err::<(), _>(err)).unwrap());
    }

    #[test]
    fn ignore_already_exists_fails_on_other_errors() {
        let err = kube::Error::Api(kube::error::ErrorResponse {
            status: "Failure".into(),
            message: "forbidden".into(),
            reason: "Forbidden".into(),
            code: 403,
        });
        assert!(ignore_already_exists(Err::<(), _>(err)).is_err());
    }

    #[test]
    fn wake_builds_pod_with_existing_pvc() {
        let spec = test_spec();
        let pod = build_pod(&spec);
        let volumes = pod.spec.as_ref().unwrap().volumes.as_ref().unwrap();
        let pvc_ref = volumes[0].persistent_volume_claim.as_ref().unwrap();
        assert_eq!(pvc_ref.claim_name, "tenant-t-abc12345-data");
    }
}
