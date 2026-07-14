variable "project_id" {
  description = "GCP project ID"
  type        = string
}

variable "region" {
  description = "GCP region"
  type        = string
  default     = "us-central1"
}

variable "network_name" {
  description = "VPC network name"
  type        = string
  default     = "houston-cloud"
}

# ---------------------------------------------------------------------------
# VPC
# ---------------------------------------------------------------------------

resource "google_compute_network" "main" {
  name                    = var.network_name
  project                 = var.project_id
  auto_create_subnetworks = false
}

# ---------------------------------------------------------------------------
# Subnets
# ---------------------------------------------------------------------------

resource "google_compute_subnetwork" "system" {
  name          = "${var.network_name}-system"
  project       = var.project_id
  region        = var.region
  network       = google_compute_network.main.id
  ip_cidr_range = "10.0.0.0/20"

  secondary_ip_range {
    range_name    = "pods"
    ip_cidr_range = "10.4.0.0/14"
  }

  secondary_ip_range {
    range_name    = "services"
    ip_cidr_range = "10.8.0.0/20"
  }

  private_ip_google_access = true

  # GKE may add its own secondary ranges — don't fight over them.
  lifecycle {
    ignore_changes = [secondary_ip_range]
  }
}

# ---------------------------------------------------------------------------
# Cloud NAT (outbound internet for private nodes)
# ---------------------------------------------------------------------------

resource "google_compute_router" "main" {
  name    = "${var.network_name}-router"
  project = var.project_id
  region  = var.region
  network = google_compute_network.main.id
}

resource "google_compute_router_nat" "main" {
  name                               = "${var.network_name}-nat"
  project                            = var.project_id
  router                             = google_compute_router.main.name
  region                             = var.region
  nat_ip_allocate_option             = "AUTO_ONLY"
  source_subnetwork_ip_ranges_to_nat = "ALL_SUBNETWORKS_ALL_IP_RANGES"

  log_config {
    enable = true
    filter = "ERRORS_ONLY"
  }
}

# ---------------------------------------------------------------------------
# Outputs
# ---------------------------------------------------------------------------

output "network_id" {
  value = google_compute_network.main.id
}

output "network_name" {
  value = google_compute_network.main.name
}

output "subnet_system_id" {
  value = google_compute_subnetwork.system.id
}
