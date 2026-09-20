# 007 — Defer administration MFA while preserving owner access controls

Status: Accepted, 2026-09-21. Amends only the independent-MFA requirement in [005](005-private-administration.md).

## Decision and alternatives

The owner explicitly requested removal of the MFA requirement while retaining the other protections because enrollment could not be completed while away. Set `mfa_config.mfa_disabled = true` only on the administration application in [Access Terraform](../../infra/access/main.tf). Keep the exact owner email, Cloudflare account-member identity provider, one-hour sessions, runtime identity/audience gate, administrator key, private-by-default routes, protocol exceptions and disabled preview URLs. Preserve the existing device credentials and approved ChatGPT OAuth connection.

Keeping mandatory MFA would continue to prevent administration until enrollment. Removing Cloudflare Access or adding an anonymous/service-token bypass would unnecessarily weaken the identity boundary. The application-level switch is the smallest reversible change and leaves the Cloudflare account's own authentication settings unchanged. No automatic re-enablement date was requested.

## Tradeoffs and recovery

Administration no longer requires a separate Cloudflare Access factor. The Cloudflare identity account and its active sessions therefore carry more responsibility, while the independent administrator key still protects owner actions. This is an explicit owner choice, not equivalent assurance to the prior MFA requirement. The retained App Launcher supports later enrollment. After enrollment, restore `mfa_disabled = false`, review and apply the Terraform plan, then verify owner login and ChatGPT connectivity.

## Validation

Review the saved Terraform plan for one in-place administration change, read the applied policy back through Cloudflare's API, verify unauthenticated administration and unauthenticated MCP remain denied, check a real owner browser login, and ping the Mac through the existing connected plugin. Record outcomes and any blocked checks in [validation](../VALIDATION.md). Worker and native code do not change.

Source: [Cloudflare Access application MFA setting](https://developers.cloudflare.com/api/resources/zero_trust/subresources/access/subresources/applications/methods/update/), consulted 2026-09-21. The documented `mfa_disabled` field disables MFA for the selected resource.
