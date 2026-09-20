# 004 — Pin one owner-approved ChatGPT connection

Status: Accepted. Date: 2026-09-20. Extends [002](002-authorization.md).

## Problem and decision

The owner wants file reads, writes and shell commands beneath their home directory available through their own ChatGPT connection. A public OAuth client ID can be shared by many users, so allowing a client ID alone cannot enforce this request.

Public deployments start locked. An administrator opens a ten-minute linking window and approves the authorization initiated from their signed-in ChatGPT account. Successful code exchange pins both the client ID and the newly issued token family. Registration, consent, code exchange, refresh and MCP authorization enforce that pin. Concurrent code exchanges use the Durable Object's mutation gate; only one family can succeed. Refresh rotation retains the family. Revocation/replacement first changes the authorization epoch, invalidating access/refresh tokens and outstanding consent/codes, then closes or opens linking explicitly. Local loopback tests can opt into generic clients; public origins cannot.

ChatGPT uses DCR with public-client PKCE S256. Only its exact stable callback is allowed. Metadata advertises RFC 9207 issuer identification and success/denial redirects include the exact issuer. Browser CSP permits the callback. Repeated scope names are treated as a set, accommodating ChatGPT's observed concatenation of base and default scopes.

## Alternatives and tradeoffs

| Alternative | Assessment |
| --- | --- |
| Allow a client name, User-Agent or public OAuth client ID | Spoofable or shared across users; insufficient owner isolation |
| Pin one token family after owner approval | Chosen; works with refresh rotation and existing single-owner infrastructure |
| External identity provider | Useful for a user directory, MFA and multiple owners; additional integration and account dependency for this single owner |
| OpenAI mTLS, signed client assertions or egress filtering | Can strengthen platform identification, but does not identify the individual ChatGPT account; requires additional infrastructure/protocol work |

This design authenticates possession of owner-approved OAuth credentials. It does not receive or verify a ChatGPT account ID, nor prove that every bearer request originates inside OpenAI. An attacker with the owner key can replace access; someone holding an unexpired bearer token can use its permissions. The owner must initiate and approve the intended ChatGPT flow. A lost/expired refresh grant requires explicitly replacing the connection. Already running commands may complete after token revocation; revoke the device or stop the agent to stop its tracked processes.

## Validation and implementation

[Connection policy](../../crates/worker/src/connection.rs), [authorization](../../crates/worker/src/oauth.rs), [token exchange](../../crates/worker/src/oauth_tokens.rs), and [integration tests](../../tests/chatgpt.test.mjs).

Real workerd tests cover closed-by-default linking, administrator enforcement, exact callback attacks, duplicated scopes, two concurrent code exchanges sharing one client ID, browser denial redirects, refresh/restart continuity, full file/process permissions, revocation and recovery. Hosted verification uses the installed Rust agent and a disposable home-directory fixture, then removes the fixture and revokes the temporary verification family before actual ChatGPT linking. See [validation evidence](../VALIDATION.md) for observed results and remaining gaps.

## Sources

- [OpenAI authentication contract](https://developers.openai.com/plugins/build/auth): DCR, PKCE, exact resource, stable callback with issuer identification, platform identity options.
- [RFC 9207](https://www.rfc-editor.org/rfc/rfc9207): authorization response issuer identification.
- Live ChatGPT plugin builder and OAuth request inspected during setup: DCR supported; all three scopes discovered; base and default scopes concatenated without deduplication.
