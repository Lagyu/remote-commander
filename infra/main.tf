terraform {
  required_version = ">= 1.5.0, < 2.0.0"
  required_providers {
    cloudflare = {
      source  = "cloudflare/cloudflare"
      version = "= 5.25.0"
    }
  }
}

# Authentication comes from CLOUDFLARE_API_TOKEN; do not put it in tfvars or state.
provider "cloudflare" {}

variable "account_id" {
  description = "Cloudflare account that owns the Wrangler-deployed Worker."
  type        = string
  validation {
    condition     = can(regex("^[a-f0-9]{32}$", var.account_id))
    error_message = "account_id must be a 32-character Cloudflare account identifier."
  }
}

variable "worker_name" {
  description = "Existing Worker service name, managed by wrangler.json."
  type        = string
  default     = "remote-commander"
  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{2,62}$", var.worker_name))
    error_message = "worker_name must contain 3–63 lowercase letters, digits, or hyphens."
  }
}

variable "zone_id" {
  description = "Cloudflare DNS zone identifier for the custom hostname."
  type        = string
  validation {
    condition     = can(regex("^[a-f0-9]{32}$", var.zone_id))
    error_message = "zone_id must be a 32-character Cloudflare zone identifier."
  }
}

variable "hostname" {
  description = "Custom domain for the service, without scheme or path."
  type        = string
  validation {
    condition     = can(regex("^[a-z0-9]([a-z0-9.-]*[a-z0-9])?\\.[a-z]{2,}$", var.hostname))
    error_message = "hostname must be a lowercase DNS name."
  }
}

# Wrangler owns the Worker and Durable Object migrations. Terraform owns only
# this domain mapping, avoiding two tools managing the same resource.
resource "cloudflare_workers_custom_domain" "commander" {
  account_id = var.account_id
  zone_id    = var.zone_id
  hostname   = var.hostname
  service    = var.worker_name
}

output "dashboard_url" {
  value = "https://${cloudflare_workers_custom_domain.commander.hostname}"
}

output "mcp_url" {
  value = "https://${cloudflare_workers_custom_domain.commander.hostname}/mcp"
}
