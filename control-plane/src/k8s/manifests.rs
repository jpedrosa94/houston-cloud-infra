//! K8s manifest builders for tenant resources.
//!
//! Pure functions that produce typed K8s objects from a `TenantSpec`.
//! No I/O, no K8s client calls.

use k8s_openapi::api::core::v1::{PersistentVolumeClaim, Pod, Secret, Service};

use super::{TenantSpec, NAMESPACE};

/// Build the PVC manifest for a tenant.
pub fn build_pvc(spec: &TenantSpec) -> PersistentVolumeClaim {
    let name = format!("tenant-{}-data", spec.tenant_id);
    serde_json::from_value(serde_json::json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {
            "name": name,
            "namespace": NAMESPACE,
            "labels": {
                "app": "houston-engine",
                "houston.ai/tenant": spec.tenant_id
            }
        },
        "spec": {
            "accessModes": ["ReadWriteOnce"],
            "storageClassName": spec.storage_class,
            "resources": {
                "requests": {
                    "storage": "10Gi"
                }
            }
        }
    }))
    .expect("valid PVC JSON")
}

/// Build the Secret manifest for a tenant's engine token.
pub fn build_secret(spec: &TenantSpec) -> Secret {
    let name = format!("tenant-{}-token", spec.tenant_id);
    serde_json::from_value(serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": NAMESPACE,
            "labels": {
                "houston.ai/tenant": spec.tenant_id
            }
        },
        "type": "Opaque",
        "data": {
            "token": base64_encode(&spec.engine_token)
        }
    }))
    .expect("valid Secret JSON")
}

/// Build the Pod manifest for a tenant engine.
pub fn build_pod(spec: &TenantSpec) -> Pod {
    let name = format!("tenant-{}", spec.tenant_id);
    let secret_name = format!("tenant-{}-token", spec.tenant_id);
    let pvc_name = format!("tenant-{}-data", spec.tenant_id);

    let mut pod_json = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "namespace": NAMESPACE,
            "labels": {
                "app": "houston-engine",
                "houston.ai/role": "tenant-engine",
                "houston.ai/tenant": spec.tenant_id
            }
        },
        "spec": {
            "serviceAccountName": "tenant-engine",
            "automountServiceAccountToken": false,
            "tolerations": [{
                "key": "sandbox.gke.io/runtime",
                "operator": "Equal",
                "value": "gvisor",
                "effect": "NoSchedule"
            }],
            "securityContext": {
                "runAsNonRoot": true,
                "runAsUser": 10001,
                "fsGroup": 10001,
                "seccompProfile": {"type": "RuntimeDefault"}
            },
            "containers": [{
                "name": "engine",
                "image": spec.engine_image,
                "securityContext": {
                    "allowPrivilegeEscalation": false,
                    "readOnlyRootFilesystem": true,
                    "capabilities": {"drop": ["ALL"]}
                },
                "ports": [{"name": "http", "containerPort": 7777, "protocol": "TCP"}],
                "env": [
                    {"name": "HOUSTON_BIND", "value": "0.0.0.0:7777"},
                    {"name": "HOUSTON_BIND_ALL", "value": "1"},
                    {"name": "HOUSTON_NO_PARENT_WATCHDOG", "value": "1"},
                    {"name": "HOUSTON_HOME", "value": "/data/.houston"},
                    {"name": "HOUSTON_DOCS", "value": "/data/Houston"},
                    {"name": "RUST_LOG", "value": "info,houston_file_watcher=warn"},
                    {
                        "name": "HOUSTON_ENGINE_TOKEN",
                        "valueFrom": {
                            "secretKeyRef": {
                                "name": secret_name,
                                "key": "token"
                            }
                        }
                    }
                ],
                "resources": {
                    "requests": {"cpu": "100m", "memory": "512Mi"},
                    "limits": {"cpu": "1000m", "memory": "1Gi"}
                },
                "volumeMounts": [
                    {"name": "data", "mountPath": "/data"},
                    {"name": "tmp", "mountPath": "/tmp"}
                ],
                "readinessProbe": {
                    "exec": {
                        "command": ["/bin/sh", "-c", "curl -sf -H \"Authorization: Bearer ${HOUSTON_ENGINE_TOKEN}\" http://localhost:7777/v1/health"]
                    },
                    "initialDelaySeconds": 3,
                    "periodSeconds": 10,
                    "timeoutSeconds": 3
                },
                "livenessProbe": {
                    "exec": {
                        "command": ["/bin/sh", "-c", "curl -sf -H \"Authorization: Bearer ${HOUSTON_ENGINE_TOKEN}\" http://localhost:7777/v1/health"]
                    },
                    "initialDelaySeconds": 10,
                    "periodSeconds": 30,
                    "timeoutSeconds": 5
                }
            }],
            "volumes": [
                {
                    "name": "data",
                    "persistentVolumeClaim": {"claimName": pvc_name}
                },
                {
                    "name": "tmp",
                    "emptyDir": {}
                }
            ],
            "restartPolicy": "Always"
        }
    });

    if spec.gvisor {
        pod_json["spec"]["runtimeClassName"] = serde_json::json!("gvisor");
    }

    serde_json::from_value(pod_json).expect("valid Pod JSON")
}

/// Build a ClusterIP Service so the control plane can reach the tenant pod.
pub fn build_service(spec: &TenantSpec) -> Service {
    let name = format!("tenant-{}", spec.tenant_id);

    serde_json::from_value(serde_json::json!({
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {
            "name": name,
            "namespace": NAMESPACE,
            "labels": {
                "houston.ai/tenant": spec.tenant_id
            }
        },
        "spec": {
            "selector": {
                "houston.ai/tenant": spec.tenant_id
            },
            "ports": [{
                "name": "http",
                "port": 7777,
                "targetPort": 7777,
                "protocol": "TCP"
            }],
            "type": "ClusterIP"
        }
    }))
    .expect("valid Service JSON")
}

/// Build a Gateway API HTTPRoute for subdomain-based routing.
///
/// Routes `{tenant_id}.{domain}` → Service `tenant-{tenant_id}:7777`.
/// Requires a Gateway resource named `houston-gateway` in the
/// `houston-system` namespace (created by Terraform/manual setup).
pub fn build_httproute(spec: &TenantSpec) -> serde_json::Value {
    let domain = spec
        .gateway_domain
        .as_deref()
        .expect("gateway_domain required for HTTPRoute");
    let name = format!("tenant-{}", spec.tenant_id);
    let hostname = format!("{}.{}", spec.tenant_id, domain);
    let svc_name = format!("tenant-{}", spec.tenant_id);

    serde_json::json!({
        "apiVersion": "gateway.networking.k8s.io/v1",
        "kind": "HTTPRoute",
        "metadata": {
            "name": name,
            "namespace": NAMESPACE,
            "labels": {
                "houston.ai/tenant": spec.tenant_id
            }
        },
        "spec": {
            "parentRefs": [{
                "name": "houston-gateway",
                "namespace": "houston-system",
                "kind": "Gateway"
            }],
            "hostnames": [hostname],
            "rules": [{
                "matches": [{"path": {"type": "PathPrefix", "value": "/"}}],
                "backendRefs": [{
                    "name": svc_name,
                    "port": 7777
                }]
            }]
        }
    })
}

fn base64_encode(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
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
    fn pvc_has_correct_name_and_labels() {
        let pvc = build_pvc(&test_spec());
        let meta = pvc.metadata;
        assert_eq!(meta.name.as_deref(), Some("tenant-t-abc12345-data"));
        assert_eq!(meta.namespace.as_deref(), Some(NAMESPACE));
        let labels = meta.labels.unwrap();
        assert_eq!(labels.get("houston.ai/tenant").unwrap(), "t-abc12345");
    }

    #[test]
    fn secret_has_correct_name() {
        let secret = build_secret(&test_spec());
        assert_eq!(
            secret.metadata.name.as_deref(),
            Some("tenant-t-abc12345-token")
        );
    }

    #[test]
    fn pod_has_correct_structure() {
        let pod = build_pod(&test_spec());
        let meta = &pod.metadata;
        assert_eq!(meta.name.as_deref(), Some("tenant-t-abc12345"));
        assert_eq!(meta.namespace.as_deref(), Some(NAMESPACE));

        let labels = meta.labels.as_ref().unwrap();
        assert_eq!(labels.get("houston.ai/tenant").unwrap(), "t-abc12345");
        assert_eq!(labels.get("app").unwrap(), "houston-engine");

        let spec = pod.spec.as_ref().unwrap();
        assert_eq!(spec.containers.len(), 1);
        assert_eq!(
            spec.containers[0].image.as_deref(),
            Some("houston/engine:latest")
        );
    }

    #[test]
    fn service_selects_tenant_pod() {
        let svc = build_service(&test_spec());
        let meta = &svc.metadata;
        assert_eq!(meta.name.as_deref(), Some("tenant-t-abc12345"));

        let spec = svc.spec.as_ref().unwrap();
        let selector = spec.selector.as_ref().unwrap();
        assert_eq!(selector.get("houston.ai/tenant").unwrap(), "t-abc12345");
    }

    #[test]
    fn httproute_has_correct_hostname_and_backend() {
        let mut spec = test_spec();
        spec.gateway_domain = Some("engine.houston.cloud".to_string());
        let route = build_httproute(&spec);
        let json = serde_json::to_value(&route).unwrap();

        let hostnames = json["spec"]["hostnames"].as_array().unwrap();
        assert_eq!(hostnames[0], "t-abc12345.engine.houston.cloud");

        let backend = &json["spec"]["rules"][0]["backendRefs"][0];
        assert_eq!(backend["name"], "tenant-t-abc12345");
        assert_eq!(backend["port"], 7777);
    }

    #[test]
    fn httproute_not_built_without_domain() {
        let spec = test_spec();
        assert!(spec.gateway_domain.is_none());
    }

    #[test]
    fn pod_without_gvisor_has_no_runtime_class() {
        let spec = test_spec();
        let pod = build_pod(&spec);
        assert!(pod.spec.as_ref().unwrap().runtime_class_name.is_none());
    }

    #[test]
    fn pod_with_gvisor_has_runtime_class() {
        let mut spec = test_spec();
        spec.gvisor = true;
        let pod = build_pod(&spec);
        assert_eq!(
            pod.spec.as_ref().unwrap().runtime_class_name.as_deref(),
            Some("gvisor")
        );
    }

    #[test]
    fn wake_builds_pod_with_existing_pvc() {
        let spec = test_spec();
        let pod = build_pod(&spec);
        let volumes = pod.spec.as_ref().unwrap().volumes.as_ref().unwrap();
        let pvc_ref = volumes[0].persistent_volume_claim.as_ref().unwrap();
        assert_eq!(pvc_ref.claim_name, "tenant-t-abc12345-data");
    }

    #[test]
    fn base64_encode_works() {
        assert_eq!(base64_encode("hello"), "aGVsbG8=");
        assert_eq!(
            base64_encode("test-token-abc123"),
            "dGVzdC10b2tlbi1hYmMxMjM="
        );
    }
}
