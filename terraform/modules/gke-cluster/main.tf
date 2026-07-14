variable "project_id" {
  description = "GCP project ID"
  type        = string
}

variable "region" {
  description = "GCP region"
  type        = string
  default     = "us-central1"
}

variable "cluster_name" {
  description = "GKE cluster name"
  type        = string
  default     = "houston-cloud"
}

variable "network" {
  description = "VPC network name"
  type        = string
}

variable "subnetwork" {
  description = "VPC subnetwork name"
  type        = string
}

variable "tenant_pool_max_nodes" {
  description = "Max nodes in the tenant sandbox pool"
  type        = number
  default     = 50
}

# ---------------------------------------------------------------------------
# GKE cluster
# ---------------------------------------------------------------------------

resource "google_container_cluster" "main" {
  name       = var.cluster_name
  location   = var.region
  project    = var.project_id
  network    = var.network
  subnetwork = var.subnetwork

  # Use separately managed node pools
  remove_default_node_pool = true
  initial_node_count       = 1
  deletion_protection      = false

  # Temporary default pool — use standard disks to avoid SSD quota.
  # Ignored after creation since the pool is immediately removed.
  node_config {
    disk_type    = "pd-standard"
    disk_size_gb = 30
  }

  lifecycle {
    ignore_changes = [node_config]
  }

  release_channel {
    channel = "REGULAR"
  }

  # GKE Dataplane V2 (Cilium) — eBPF-based networking with L7 policies,
  # DNS-based egress rules, Hubble observability, and WireGuard encryption.
  # Replaces Calico; standard K8s NetworkPolicy manifests still work.
  datapath_provider = "ADVANCED_DATAPATH"

  # Use our named secondary ranges so GKE doesn't auto-create its own.
  ip_allocation_policy {
    cluster_secondary_range_name  = "pods"
    services_secondary_range_name = "services"
  }

  workload_identity_config {
    workload_pool = "${var.project_id}.svc.id.goog"
  }

  private_cluster_config {
    enable_private_nodes    = true
    enable_private_endpoint = false
    master_ipv4_cidr_block  = "172.16.0.0/28"
  }

  # Enable Gateway API for ingress
  gateway_api_config {
    channel = "CHANNEL_STANDARD"
  }
}

# ---------------------------------------------------------------------------
# System node pool (control plane, ingress, monitoring)
# ---------------------------------------------------------------------------

resource "google_container_node_pool" "system" {
  name     = "system"
  location = var.region
  cluster  = google_container_cluster.main.name
  project  = var.project_id

  autoscaling {
    min_node_count = 2
    max_node_count = 5
  }

  node_config {
    machine_type = "e2-medium"
    disk_type    = "pd-standard"
    disk_size_gb = 50

    labels = {
      "houston.ai/role" = "system"
    }

    oauth_scopes = [
      "https://www.googleapis.com/auth/cloud-platform",
    ]

    workload_metadata_config {
      mode = "GKE_METADATA"
    }
  }
}

# ---------------------------------------------------------------------------
# Tenant sandbox node pool (gVisor runtime)
# ---------------------------------------------------------------------------

resource "google_container_node_pool" "tenant_sandbox" {
  provider = google-beta
  name     = "tenant-sandbox"
  location = var.region
  cluster  = google_container_cluster.main.name
  project  = var.project_id

  autoscaling {
    min_node_count = 1
    max_node_count = var.tenant_pool_max_nodes
  }

  node_config {
    machine_type = "n2d-standard-2"
    image_type   = "COS_CONTAINERD"
    disk_type    = "pd-standard"
    disk_size_gb = 50

    # gVisor sandbox — kernel-level isolation per pod.
    # Requires n2/n2d/c2/c2d machine types (e2 not supported).
    sandbox_config {
      type = "GVISOR"
    }

    labels = {
      "houston.ai/role" = "tenant-engine"
    }

    # GKE auto-adds taint sandbox.gke.io/runtime=gvisor:NoSchedule
    # when sandbox_config is set. Do not specify it manually.

    oauth_scopes = [
      "https://www.googleapis.com/auth/cloud-platform",
    ]

    workload_metadata_config {
      mode = "GKE_METADATA"
    }
  }
}

# ---------------------------------------------------------------------------
# Outputs
# ---------------------------------------------------------------------------

output "cluster_name" {
  value = google_container_cluster.main.name
}

output "cluster_endpoint" {
  value     = google_container_cluster.main.endpoint
  sensitive = true
}

output "cluster_ca_certificate" {
  value     = google_container_cluster.main.master_auth[0].cluster_ca_certificate
  sensitive = true
}
