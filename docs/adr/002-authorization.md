# ADR 002: Single-owner OAuth and explicit device pairing

Date: 2026-09-20. Status: accepted for single-owner scope.

## Alternatives

A static bearer key is small but does not provide a useful OAuth linking/consent experience or independently scoped client access. Cloudflare Access or an established external identity provider would provide mature identity and potentially MFA, but would require an operator identity configuration and additional client compatibility work. A minimal built-in authorization server can complete the requested owner-managed workflow without inventing an external identity account.

## Decision

Use owner-key approval, OAuth authorization code plus PKCE S256 and dynamic public client registration. Bind codes to exact client IDs, registered redirect URIs, a requested scope set and the public MCP resource. Use short-lived opaque access tokens and rotating refresh-token families, persisting only token hashes. Global revocation invalidates issued grants and outstanding consent/codes through an epoch. Administrator and device credentials cannot substitute for OAuth bearer credentials.

Use a separate short-code approval protocol for device enrollment. A local agent initiates pairing, the operator verifies the code/name, and one redemption receives a random device credential. This is not claimed to be a drop-in OAuth device-grant implementation. The administrator key is confined to an unlocked browser tab's memory or a private local secret file. Production redirects require HTTPS; local HTTP callbacks require development configuration.

## Rationale and tradeoffs

This keeps all persistent application state in the Durable Object, avoids invented identity dependencies, and supports MCP clients that use DCR and S256. The server does not fetch client metadata URLs, so it has no CIMD fetch/SSRF surface. It also does not support OIDC, federated login, user profiles, MFA or per-device grants. OpenAI recommends established identity providers; this local authorization implementation is a deliberate single-owner scope tradeoff, not a claim of equivalent assurance. Multi-user deployment should supersede this ADR with a reviewed identity provider and tenancy design.

## Evidence and validation

The integration suite exercises real owner consent, code exchange, official MCP client initialization, wrong verifiers/resources, exact redirect matching, consent/code replay, read-only scope enforcement, refresh-token replay-family revocation and global revocation. Browser pairing exercises the native pairing CLI rather than writing credentials directly. Hosted external-client OAuth linking and an independent security audit remain evidence gaps.

Code: [OAuth discovery/consent](../../crates/worker/src/oauth.rs), [token rotation](../../crates/worker/src/oauth_tokens.rs), [pairing](../../crates/worker/src/pairing.rs), [integration tests](../../tests/e2e.test.mjs).

Sources: [MCP authorization](https://modelcontextprotocol.io/specification/2025-06-18/basic/authorization), [OpenAI authentication guidance](https://developers.openai.com/plugins/build/auth).
