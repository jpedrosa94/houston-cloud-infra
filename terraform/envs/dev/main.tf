terraform {
  required_version = ">= 1.5"

  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "~> 7.0"
    }
  }

  backend "gcs" {
    bucket = "houston-cloud-tfstate"
    prefix = "dev"
  }
}

provider "google" {
  project = var.project_id
  region  = var.region
}

provider "google-beta" {
  project = var.project_id
  region  = var.region
}

variable "project_id" {
  description = "GCP project ID"
  type        = string
}

variable "region" {
  description = "GCP region"
  type        = string
  default     = "us-central1"
}

module "vpc" {
  source       = "../../modules/vpc"
  project_id   = var.project_id
  region       = var.region
  network_name = "houston-cloud-dev"
}

module "gke" {
  source                = "../../modules/gke-cluster"
  project_id            = var.project_id
  region                = var.region
  cluster_name          = "houston-cloud-dev"
  network               = module.vpc.network_name
  subnetwork            = "houston-cloud-dev-system"
  tenant_pool_max_nodes = 10
}

module "registry" {
  source     = "../../modules/artifact-registry"
  project_id = var.project_id
  region     = var.region
}
