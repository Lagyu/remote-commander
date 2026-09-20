# Owner-only administration

This independent Terraform root owns a Zero Trust organization, a Cloudflare account identity provider, a single-owner administration application, an owner-only MFA enrollment launcher, and narrow protocol exceptions. It does not own Worker code, device credentials, OAuth grants, or billing subscriptions. Keep its local state and `.terraform.lock.hcl` when moving the project.

Create a private `.deploy/access.tfvars.json` (mode `0600`) with:

```json
{
  "account_id": "YOUR_CLOUDFLARE_ACCOUNT_ID",
  "hostname": "remote-commander.YOUR_SUBDOMAIN.workers.dev",
  "team_name": "YOUR_ACCESS_TEAM_NAME",
  "owner_email": "YOUR_EXACT_CLOUDFLARE_ACCOUNT_EMAIL"
}
```

The account and hostname must match the project's private `.env`. The API token needs Access application/policy and organization/identity-provider write permissions, in addition to the existing Worker permissions.

For a fresh Zero Trust account, run:

```sh
node scripts/access-infra.mjs bootstrap
node scripts/access-infra.mjs plan
# Review the plan, then apply that exact saved plan:
node scripts/access-infra.mjs apply
npm run deploy
```

Bootstrap uses the documented organization POST API and imports it because provider 5.25.0 uses PUT for this resource's create operation. No paid subscription is created. If the account requires dashboard onboarding or lacks API permissions, stop and complete that prerequisite; the script does not change billing or broaden token permissions. For an existing Zero Trust organization, import and review its configuration first; do not replace its team domain, identity providers, or other applications. If Cloudflare has already created a default Cloudflare IdP, import that IdP into `cloudflare_zero_trust_access_identity_provider.owner` before planning. This deployment's organization POST did not create one automatically.

For subsequent changes, use `plan` and `apply`. Apply writes the audience and owner email into the private `.env`, and ordinary Worker deployments retain the required Access gate. The generated plan and Terraform state are private operational data. The organization has `prevent_destroy` enabled.

The hostname-wide owner policy requires the exact owner email, the account-member identity provider, and a one-hour application session. Independent MFA for the administration application is temporarily disabled at the owner's request; see [ADR 007](../../docs/adr/007-defer-admin-mfa.md). Exceptions are created before that policy, so existing OAuth refresh and agent WebSockets continue. Exceptions bypass only Cloudflare's browser login; Rust still authenticates MCP and device requests. Unknown paths under those prefixes fail the Rust runtime gate. Do not change these to a Worker-level Access application: Cloudflare currently documents a WebSocket limitation for that mode.

The owner completes interactive Cloudflare sign-in. Existing ChatGPT tokens and the Mac agent do not need an Access session. Administrator approval still additionally requires the existing private owner key. The Cloudflare account's own authentication settings are separate and unchanged.

To restore independent MFA after the owner is ready, enroll an authenticator through the existing owner-only App Launcher, set the administration application's `mfa_config.mfa_disabled` to `false` in `main.tf`, review `node scripts/access-infra.mjs plan`, and run `node scripts/access-infra.mjs apply`. Verify the next owner login requests MFA and the existing ChatGPT connection still works. This application-only policy change does not require a Worker deployment. MFA is not automatically re-enabled on a schedule.
