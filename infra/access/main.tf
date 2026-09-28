terraform {
  required_version = ">= 1.5.0, < 2.0.0"
  required_providers {
    cloudflare = {
      source  = "cloudflare/cloudflare"
      version = "= 5.25.0"
    }
  }
}

provider "cloudflare" {}

variable "account_id" {
  type = string
  validation {
    condition     = can(regex("^[a-f0-9]{32}$", var.account_id))
    error_message = "A Cloudflare account ID is required."
  }
}

variable "hostname" {
  type = string
  validation {
    condition     = can(regex("^[a-z0-9.-]+\\.[a-z]{2,}$", var.hostname))
    error_message = "Use the service hostname, without a scheme or path."
  }
}

variable "team_name" {
  type = string
  validation {
    condition     = can(regex("^[a-z0-9][a-z0-9-]{2,62}$", var.team_name))
    error_message = "Use a Cloudflare Access team name."
  }
}

variable "owner_email" {
  type      = string
  sensitive = true
  validation {
    condition     = can(regex("^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$", var.owner_email))
    error_message = "Exactly one owner's email address is required."
  }
}

# This root is independent from the optional custom-domain root. Import an
# existing organization/IdP before applying; never replace an existing team.
resource "cloudflare_zero_trust_organization" "commander" {
  account_id                                  = var.account_id
  auth_domain                                 = "${var.team_name}.cloudflareaccess.com"
  name                                        = "Remote Commander"
  session_duration                            = "1h"
  mfa_required_for_all_apps                   = false
  deny_unmatched_requests                     = false
  deny_unmatched_requests_exempted_zone_names = []
  mfa_config = {
    allowed_authenticators = ["totp", "biometrics", "security_key"]
    session_duration       = "0m"
  }
  lifecycle { prevent_destroy = true }
}

resource "cloudflare_zero_trust_access_identity_provider" "owner" {
  account_id = var.account_id
  name       = "Cloudflare"
  type       = "cloudflare"
  config = {
    restrict_to_account_members = true
  }
  depends_on = [cloudflare_zero_trust_organization.commander]
}

# Cloudflare's enrollment UI requires an App Launcher policy. It authenticates
# the same owner, but cannot require an already-enrolled factor for first setup.
# This app has a different audience and grants no access to Commander itself.
resource "cloudflare_zero_trust_access_application" "enrollment" {
  account_id = var.account_id
  # Cloudflare normalizes this special application's name and landing design.
  name                      = "App Launcher"
  type                      = "app_launcher"
  landing_page_design       = {}
  session_duration          = "1h"
  allowed_idps              = [cloudflare_zero_trust_access_identity_provider.owner.id]
  auto_redirect_to_identity = true
  policies = [{
    name       = "Owner enrollment only"
    decision   = "allow"
    precedence = 1
    include    = [{ email = { email = var.owner_email } }]
  }]
}

locals {
  # These exceptions bypass only Access's interactive browser login. The Rust
  # server still enforces OAuth, device credentials, PKCE and owner approval.
  protocol_paths = toset([
    "mcp", ".well-known/oauth-protected-resource",
    ".well-known/oauth-authorization-server", "oauth/register", "oauth/token",
    "oauth/revoke", "agent/*", "pair/start", "pair/token", "health",
    # Transfer handlers require a one-hour, single-file capability token.
    "download/*", "upload/*", "transfer.js", "transfer.css"
  ])
}

resource "cloudflare_zero_trust_access_application" "protocol" {
  for_each             = local.protocol_paths
  account_id           = var.account_id
  name                 = "Remote Commander protocol: ${each.value}"
  type                 = "self_hosted"
  domain               = "${var.hostname}/${each.value}"
  app_launcher_visible = false
  policies = [{
    name       = "Application authentication"
    decision   = "bypass"
    precedence = 1
    include    = [{ everyone = {} }]
  }]
  depends_on = [cloudflare_zero_trust_organization.commander]
}

resource "cloudflare_zero_trust_access_application" "admin" {
  account_id                 = var.account_id
  name                       = "Remote Commander administration"
  type                       = "self_hosted"
  domain                     = var.hostname
  session_duration           = "1h"
  app_launcher_visible       = false
  allowed_idps               = [cloudflare_zero_trust_access_identity_provider.owner.id]
  auto_redirect_to_identity  = true
  http_only_cookie_attribute = true
  same_site_cookie_attribute = "lax"
  path_cookie_attribute      = false
  options_preflight_bypass   = false
  mfa_config = {
    allowed_authenticators = ["totp", "biometrics", "security_key"]
    session_duration       = "0m"
    # Temporarily deferred by the owner; see docs/adr/007-defer-admin-mfa.md.
    mfa_disabled = true
  }
  policies = [{
    name       = "Owner only"
    decision   = "allow"
    precedence = 1
    include    = [{ email = { email = var.owner_email } }]
  }]
  # Install protocol exceptions first to avoid interrupting the existing Mac
  # WebSocket or ChatGPT's token refresh while the catch-all is introduced.
  depends_on = [cloudflare_zero_trust_access_application.protocol]
}

output "admin_audience" {
  value = cloudflare_zero_trust_access_application.admin.aud
}

output "access_team_domain" {
  value = cloudflare_zero_trust_organization.commander.auth_domain
}
