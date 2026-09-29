# Trust model and boundaries

This release assumes one trusted operator, an uncompromised Cloudflare account, and explicitly paired computers. It is not a multi-tenant boundary. The single approved ChatGPT connection can use its granted scopes on all paired computers; separate deployments are required for independent trust domains.

## File and process authority

File operations resolve through an open `cap_std::fs::Dir`, reject absolute paths and `..`, and cannot traverse outside that capability through symbolic links. Listing/searching skips symlinks. Direct reads may follow symlinks only within the capability. Reads reject non-regular files, including FIFOs, and open nonblocking to avoid a replacement race blocking a thread. Atomic writes reject a symlink destination and create a private temporary file in the held parent directory.

Path containment does not provide isolation from a malicious local user, hard links already present within the root, or another process concurrently editing the same file. Exact replacement counts protect against accidental broad replacements, but there is no general compare-and-swap/version check against external editors. Atomic replacement does not preserve modes, ownership, ACLs, or extended attributes. The agent's own writes are sequential.

`--allow-shell` explicitly enables commands with the operating-system user's full authority. The configured root is only the initial/selected working directory for commands. A command can change directory, access the network and other files, or spawn subprocesses. The agent clears inherited environment variables except PATH, HOME and LANG, but commands can still access anything the OS user can access. Use a dedicated low-privilege OS account or externally managed sandbox when that boundary is required.

`get_screenshot` is enabled by default and can read the visible contents of a macOS display; use `--no-screenshot` to opt out. It is not confined by `--root`; macOS Screen Recording permission remains the operating-system boundary. Captures use fixed system binaries without shell execution, are converted to bounded JPEG data in a private temporary path, removed after encoding, and relayed as MCP image content. Do not run with screenshot access enabled on a device where the authorized client should not see other applications, notifications, or secrets visible on screen.

Sessions use separate process groups and bounded output. Ctrl+C, SIGTERM, device shutdown, revocation and process deadlines terminate tracked groups and reap the shell. A program that deliberately daemonizes into a different session can escape that process group. This is lifecycle management, not hostile-code containment. `force_terminate` only targets known sessions; it cannot target an arbitrary system PID.

## Credentials and authorization

The administrator key approves device pairings and OAuth consent and authorizes the owner dashboard. It is not accepted as an MCP bearer token. The browser retains it only in JavaScript memory; navigation or locking requires reentry. A compromised browser or extension can still access an unlocked tab. Protect the Cloudflare account and local credential files as privileged access.

Device credentials and OAuth tokens are random 256-bit values. Only token hashes are persisted server-side. Native credentials are owned regular files with no group/world permissions. Pairing codes expire in ten minutes and require a matching owner approval; redemption is one-time. Pairing is this project's private wire protocol, not a promise of RFC 8628 endpoint compatibility.

OAuth clients use exact registered redirect URIs, PKCE S256 and the deployment's exact `/mcp` resource. Access tokens last 15 minutes. Refresh tokens rotate, expire after 30 days and revoke their token family on detected reuse. Global client revocation changes an epoch checked by access grants, refresh grants, pending consent and authorization codes. Already executing operations may complete; use device revocation to stop the corresponding agent.

Production begins locked. The owner opens a ten-minute linking window for the first connection; one completed authorization pins both client ID and token family in durable storage. Once linked, ChatGPT may perform a new DCR/authorization flow without a destructive dashboard reset, but administrator-key consent is still required. That authorization records the currently pinned client/family and successful code exchange performs a compare-and-swap replacement: the old family is revoked before the new pin is published, and a competing or stale replacement code fails. Initial concurrent codes likewise admit only one family. Restart and refresh-token rotation preserve the pin. Owner reset still invalidates outstanding grants/consent/codes before clearing it. RFC 9207 binds callbacks to this issuer, and only the exact documented ChatGPT callback is permitted.

The server does not receive an authenticated ChatGPT account ID. The owner selects the account by approving the OAuth flow they initiated there. Bearer-token possession remains the runtime credential: this does not cryptographically authenticate ChatGPT’s network origin, and it is not mTLS or a signed-client-assertion implementation. Do not claim account-specific verification from callback URLs, client names, IP addresses, or User-Agent headers. See [ADR 004](adr/004-private-chatgpt-connection.md).

Production administration additionally requires Cloudflare Access: the exact owner email, the Cloudflare account-member identity provider and one-hour sessions. Independent MFA for the administration application is temporarily disabled at the owner's request, so this layer currently relies on the owner's Cloudflare sign-in without an additional Access factor. The separate administrator key remains required. The outer Rust Worker checks Cloudflare's runtime-verified Access context, audience and owner before reading private-route request bodies or dispatching to the Durable Object. Header/cookie values alone are never trusted. Missing configuration denies administration. A separate owner-only App Launcher policy permits authenticator enrollment; its audience does not authorize the application. See [ADR 005](adr/005-private-administration.md) and its [MFA amendment](adr/007-defer-admin-mfa.md).

The custom OAuth service still uses bounded state and owner-key approval and has no multi-user directory or independent security certification. MCP, discovery, OAuth machine endpoints, device pairing and authenticated WebSockets remain publicly reachable as required by ChatGPT and the agent. Registration and pairing are rate/capacity limited within one deployment. Limits are shared rather than isolated per source IP, so a determined caller can affect availability. Private administration does not contain an already authorized malicious tool call or protect home-directory secrets from that authorized agent.

## Network and data handling

Production configuration requires HTTPS. The native agent permits HTTP only for loopback with an explicit development flag. Requests with an Origin header must match the deployment; the public request URL must also match the configured origin. The dashboard uses same-origin external assets, no third-party scripts, a restrictive CSP and no browser persistence for secrets.

Tool results pass through Cloudflare and its operator's trust boundary. They are not end-to-end encrypted against the cloud relay. The implementation does not write command text, file contents, access tokens or results to the cloud activity history. The history retains only the latest 200 operation metadata entries. Local development request logging and external platform/account telemetry have their own retention behavior; do not share raw logs without reviewing them.

The relay binds a response to its request ID, device ID and connection generation. An unrelated device cannot resolve another device's request. Results exceeding the frame cap are rejected. A request has a 25-second response deadline and is never automatically replayed. After connection loss, the execution state may be unknown; repeating a write or process start can duplicate effects.

## Reporting and response

Report issues privately to the operator responsible for the deployment, with a minimal reproduction using disposable files. Do not include administrator keys, device credentials, OAuth tokens or real document contents.

For suspected client compromise, use the dashboard's client revocation, then revoke affected devices as necessary. Rotate the administrator secret if it may have been exposed, revoke all existing client grants, and re-pair compromised devices. Rotating the administrator secret alone does not revoke existing OAuth or device credentials. See [operations](OPERATIONS.md).
