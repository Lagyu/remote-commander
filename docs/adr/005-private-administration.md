# 005 — Private administration with Cloudflare Access

Status: Accepted, 2026-09-20. Extends [002](002-authorization.md) and [004](004-private-chatgpt-connection.md); preserves their OAuth and pairing grants.

Amended 2026-09-21 by [007](007-defer-admin-mfa.md): independent MFA is temporarily deferred at the owner's request. The original rationale below is preserved; other administration protections remain in force.

## Decision and alternatives

Use a hostname-wide Cloudflare Access owner policy with exact email, the account-member Cloudflare IdP, MFA, and one-hour sessions. Terraform owns narrow exceptions for the existing MCP, OAuth machine endpoints, discovery, pairing, health, and authenticated device WebSockets. The root dashboard, assets, owner APIs, consent/approval and unknown URLs are private by default.

A Worker-level Access application would be simpler but currently breaks WebSocket upgrades. A blanket browser login on MCP/token endpoints would break ChatGPT. Moving only the UI to a private path leaves future routes exposed unless each is separately registered. A private tunnel/VPN for everything would require a networking capability ChatGPT does not provide here. Separate long-lived service credentials for operator scripts would add another administration credential and bypass human MFA, so scripts use a short-lived owner Access session instead.

## Application enforcement

[access.rs](../../crates/worker/src/access.rs) checks Cloudflare's authenticated `ctx.access` before uploads or Durable Object dispatch. Both the application's audience and the exact owner email must match. It never trusts a caller-supplied identity header, JWT header, or cookie directly. This uses Cloudflare's documented runtime verification instead of implementing JWT/JWKS cryptography. Missing configuration, a missing runtime identity, and wrong audience/owner all deny administration. Only an explicit loopback development origin can omit Access. Production deployment strips simulated development Access configuration.

Cloudflare Access does not propagate the context into Durable Objects, so the check belongs at the outer Worker. The DO retains the administrator-key, CSRF, OAuth and device checks. The new policy neither resets the owner-pinned token family nor changes Mac permissions. The running agent has no new credentials or interactive login dependency.

## Tradeoffs and evidence

An owner must finish interactive sign-in/MFA enrollment before using administration. The account administrator and Cloudflare runtime remain trusted. Full-home read/write and full OS-user shell authority remain the user-authorized impact boundary. A stolen ChatGPT bearer token is still usable until expiry or revocation; Access protects administration, not every machine request.

The new workerd/native-agent integration test covers simulated verified owner identity, forged headers/cookies, unknown and normalized paths, missing configuration, wrong identity/audience, real refresh without Access, 23 tools, file read/write, shell execution and agent reconnection after Worker restart. Production policies are read back through the API; anonymous URL checks and the existing ChatGPT connection are verified separately. Human MFA login cannot be inferred from API configuration and is recorded separately in [validation](../VALIDATION.md).

Sources: [Workers Access and runtime identity](https://developers.cloudflare.com/workers/configuration/cloudflare-access/), [path precedence](https://developers.cloudflare.com/cloudflare-one/access-controls/policies/app-paths/), [independent MFA](https://developers.cloudflare.com/cloudflare-one/access-controls/policies/mfa-requirements/), [CLI owner sessions](https://developers.cloudflare.com/cloudflare-one/tutorials/cli/). Consulted 2026-09-20.
