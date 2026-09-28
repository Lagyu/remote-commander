# Validation record

Date: 2026-09-20. Environment: macOS ARM64, Rust 1.96.0, Node.js 24.14.1, Wrangler 4.135.0, workers-rs/worker-build 0.8.6, official TypeScript MCP SDK 1.30.0, Terraform 1.5.7 and Cloudflare provider 5.25.0.

## Completed checks

| Check | Observed result |
| --- | --- |
| Rust formatting | `cargo fmt --all --check` passed |
| Native/protocol lint | Clippy over all native targets passed with warnings denied |
| WASM lint | Clippy for `rdc-worker` on `wasm32-unknown-unknown` passed with warnings denied |
| Native and protocol tests | Six tests passed; no ignored or failing tests |
| Native release build | `cargo build --release -p rdc-agent` passed; the resulting executable reported version 0.1.0 |
| Worker release build | Rust compiled to WASM and worker-build generated the deployable shim |
| Integration suite | Original integration suite plus ChatGPT connection-policy/browser suite: 19 reported tests passed; two deployment credential/bootstrap tests also passed, zero failures/skips |
| Browser verification | Headless Chrome performed owner login, device approval, native CLI pairing, lock/logout and responsive viewport checks |
| Visual inspection | Desktop and mobile screenshots reviewed; activity table adjusted to keep readable columns in a bounded scroll area |
| Secret generation | A temporary generated key had 43 base64url characters and mode 0600; a second creation refused to overwrite it |
| JavaScript syntax | Deployment, key-generation, installer, live-check and dashboard scripts parsed successfully |
| Terraform | Provider initialization, formatting and configuration validation passed |
| Cloudflare deployment packaging | The real deployment script's `--dry-run` completed without credentials or cloud mutations; upload bundle 794.76 KiB, 294.15 KiB gzip |

## End-to-end evidence

The integration harness starts a real local Cloudflare workerd runtime with durable storage, runs the native Rust agent against disposable fixtures, and connects the official MCP client. It does not substitute a mocked relay or filesystem. The final run is recorded in `logs/setup-final-check.log`; deployment packaging is recorded in `logs/deploy-dry-run.log`. The two credential/bootstrap tests were run separately after they were added and are included in subsequent `npm run check` runs. Each integration run has process logs and a `lifecycle.json` record under `logs/e2e-*`.

The scenarios cover:

1. OAuth discovery and authorization, MCP initialization/tool listing, device connection, and single-use pairing redemption.
2. Real paginated reads, multi-file results, atomic file changes, exact edit preconditions, moves, directory listing and search snapshots.
3. Parent traversal, symlink escapes, special-file/FIFO rejection and malformed tool arguments.
4. Interactive process stdin, bounded output with discarded-byte reporting, deadlines, explicit termination and actual PID cleanup.
5. Wrong PKCE verifiers/resources, exact redirect matching, consent/code replay, scope enforcement, refresh narrowing and refresh-token replay-family revocation.
6. Unauthenticated requests, administrator-key rejection as an MCP token, hostile Origins, protocol versions, oversized requests, slow uploads, batch rejection and non-executing notifications.
7. Browser approval of a code printed by the native pairing CLI, private credential creation, local read-only permission enforcement and remote agent shutdown.
8. Device-bound response correlation, a second device's forged reply, connection loss, response timeout and absence of automatic redelivery.
9. Durable device/token state surviving a Worker restart, followed by native reconnection and successful file access.
10. Device revocation stopping the agent and descendant process group, plus client revocation invalidating both issued tokens and an outstanding authorization code.

The slow-upload assertion uses the HTTP client to observe an early 408 response. A streaming half-duplex Fetch client withheld that response until upload completion even though the Worker logged the five-second timeout; that initial test was corrected without weakening the expected server status. The final suite also verifies that rejected uploads do not break subsequent requests.

Screenshots are generated at `test-results/dashboard-desktop.png` and `test-results/dashboard-mobile.png`. Test credentials and durable state are removed during cleanup; logs and synthetic screenshots remain local and ignored. The test harness treats forced process termination as a cleanup failure.

After the final run, all five test-owned processes were confirmed exited and the temporary-state directory was empty. Local Markdown links resolved successfully. A separate private `.dev.vars` was generated for the operator's development use with mode 0600; its value was not printed in the implementation session.

## Hosted setup and account connection

On 2026-09-20 (Asia/Tokyo), deployed the production Worker and SQLite Durable Object to `https://remote-commander.yuya-commander.workers.dev`. The final code upload reported version `3b8d6b79-dc2e-4613-a5f7-b5401a7246c1`; a subsequent administrator-secret update may produce a newer Cloudflare version. Public DNS/TLS and `/health` passed. Logs: `logs/deploy-final.log`.

The provided Cloudflare account/token are stored only in the project's `.env`, verified as mode `0600`, a regular file, and excluded by `.gitignore`. The administrator key is independently generated in `.deploy/admin.key`, also mode `0600`. A live Worker-settings inspection found only ADMIN_TOKEN, PUBLIC_URL, ALLOW_LOCALHOST, CHATGPT_ONLY and COMMANDER bindings: the Cloudflare API credential was not uploaded as a Worker binding. The account subdomain was created through the documented API and verified. No paid plan upgrade was performed.

The macOS LaunchAgent was installed, paired, and confirmed running with `--root /Users/yuya --allow-write --allow-shell`, `RunAtLoad=true` and `KeepAlive=false`. Its installed credential, binary and durable logs live in the user's Library folders. Cloudflare reported the device online. Installation evidence is in `.deploy/agent-installation.json` and `logs/agent-install.log`.

`node scripts/live-check.mjs` exercised the actual public Worker, official MCP SDK, persistent local agent, and a disposable file below home. It verified 23 tools; exact root and permission flags; an on-disk write followed by an MCP read; a shell command reporting its working directory and exiting successfully; 401 rejection of unauthenticated/admin-key MCP requests; blocking of an additional authorization; token revocation; and fixture removal. Its temporary OAuth family was revoked before actual ChatGPT linking. Evidence: `logs/live-check.log`, `.deploy/live-validation.json`. This live SDK test alone is not evidence of ChatGPT account identity.

The signed-in ChatGPT personal account then created **Remote Commander — My Mac**, using DCR, OAuth, all three default/base scopes, and the stable documented callback. Owner approval was performed against the authorization flow initiated in that browser, and the PKCE callback was completed in ChatGPT. The production policy now reports one linked connection, no open linking window, and all three scopes; ChatGPT settings reports **Connected accounts → Primary**, OAuth used, and review status **development**. Refreshing the plugin successfully loaded all 23 tool definitions into ChatGPT, including reads, writes, and shell actions. Details: `.deploy/chatgpt-connection.json`. The service does not learn the owner's ChatGPT account ID.

Two real-browser integration defects were found and fixed: ChatGPT repeats base/default scopes, so authorization normalizes duplicates; Chromium sends `Origin: null` on a form POST under `Referrer-Policy: no-referrer`, so the policy now uses `same-origin` while continuing to suppress cross-origin referrers. CSP permits the exact ChatGPT callback. The dedicated browser test submits a real consent form, verifies a 303 redirect and its issuer/state/error fields, and intercepts only the synthetic destination page.

The final local test lifecycle records confirmed all eight processes exited. All live verification fixture files were removed. The production LaunchAgent intentionally remains running.

## Private-administration deployment

The Access hardening was deployed on 2026-09-20. Code upload version: `1a399783-a561-41cd-804a-0b87041027f3` (secret provisioning may create a later version). Terraform created the organization, Cloudflare account-member identity provider, exact-owner administration policy, MFA enrollment launcher, and narrow protocol exceptions. The live API readback confirmed the correct audience, owner allow policy, MFA enabled with `0m` reauthentication duration, and a one-hour application session. No paid plan was added. Logs: `logs/access-deploy.log`, `logs/access-enrollment-apply.log`.

`npm run check` passed all Rust formatting/lint/native checks and 26 reported Node tests, including five new Access integration checks. The separate owner-session file safeguard test also passed after addition. The Access test uses workerd's official simulated identity configuration, never a caller-controlled identity header. It covers wrong/missing identity and audience, unconfigured administration, forged headers/cookies, path variants, owner-key requirements, real refresh without Access, native agent reconnect, and full file/shell permissions. Diagnostics: `logs/access-check.log`.

The final deployment dry run packaged the Worker and validated both Terraform roots (`logs/access-dry-run.log`). A fresh Access plan reports no differences between infrastructure and configuration (`logs/access-idempotence.log`). All local Markdown links resolve, test lifecycle records report no remaining running processes, disposable Access test folders are removed, and `.env`, the owner key and Access state have mode `0600`.

Anonymous live requests to `/`, dashboard assets, `/api/*`, and OAuth consent/approval redirect to Cloudflare Access. The administrator key alone and forged Access identity headers/cookies also redirect. Discovery and health remain reachable; unauthenticated MCP/device requests reject with 401; invalid token exchange rejects with 400. Unknown paths beneath protocol exceptions reject with the Rust `access_required` error (403), demonstrating the application gate still applies when the edge path exception matches. Evidence: `.deploy/access-live-checks.json`.

After deployment, the existing **Remote Commander — My Mac** connected plugin successfully called `list_devices`, `ping_device`, `get_config`, `write_file`, `read_file`, `start_process`, and `read_process_output` against the real hosted service and existing Mac agent. Home remained the root; writes and shell remained enabled. The disposable write was verified on local disk and removed. The shell exited with code 0 and the expected marker/working directory. No OAuth connection replacement or device credential rotation occurred. Evidence: `.deploy/access-plugin-validation.json`. The LaunchAgent retained PID 15558 and an established TLS connection.

Microsoft Edge's saved Cloudflare credentials successfully completed identity sign-in. The application correctly required independent MFA, showing no authenticator enrolled. The owner-only App Launcher now presents Touch ID, security-key and authenticator-app enrollment choices. Human enrollment and the final post-MFA administration page check are pending; API policy configuration alone is not recorded as a completed MFA login. The existing ChatGPT connection already works without that browser enrollment.

Operator CLI Access-token transport has local credential validation tests; its live authenticated owner-API check also awaits the owner's MFA enrollment. No temporary service-token policy or production authentication bypass was used to substitute for that check.

## Edge launch and agent responsiveness repair

On 2026-09-20, production reported the Mac online while `get_config` and `list_sessions` returned `unknown_execution_state`. The installed PID 15558 was sampled in `FileTools::walk → openat`. The macOS TCC log at 22:47:53 JST showed an outstanding Downloads-folder consent request. Waiting for that file call inside the WebSocket receive loop blocked subsequent commands and heartbeats. The stack sample is retained privately in `logs/agent-hang.sample.txt`.

`tests/agent-responsiveness.test.mjs` reproduces head-of-line blocking with a real child that does not consume stdin. It failed against the original binary (`logs/edge-regression-before.log`) and passed after the repair (`logs/edge-regression-after.log`). The updated agent lets health checks and another shell launch complete while input is blocked. A native regression verifies timed-out OS calls retain their bounded slot until the call returns, followed by successful reuse.

`npm run check` passed all seven native/protocol tests, Rust formatting, native/WASM Clippy, the Worker build, and all 29 reported Node tests with no skips (`logs/edge-fix-check.log`). Final adjustments to reserve immediate health/shutdown handling and move process-directory metadata off the event loop passed native Clippy, a debug build and the focused integration regression. The release build passed (`logs/edge-release-build.log`). Integration lifecycle records show test processes exited and fixture directories were removed. The installer now waits for a booted-out LaunchAgent to unload before bootstrap; immediate bootstrap was observed to fail with EIO while the original blocked agent was still terminating.

Installed the new executable and changed the LaunchAgent root from home to `/` at the owner's explicit request, keeping writes/shell enabled, `KeepAlive=false`, the existing device credential and the existing ChatGPT connection. Through the connected **Remote Commander — My Mac** plugin, `get_config` reports `/`, `ping_device` succeeds, and file metadata outside home (`Applications/Microsoft Edge.app`) is readable. A disposable `/private/tmp/remote-commander-access-*` file was written and read through the plugin, independently checked on disk and removed.

The live plugin ran `/usr/bin/open -a 'Microsoft Edge' 'about:blank'`; `read_process_output` confirmed exit code 0 without timeout. The Edge accessibility tree independently showed the new selected `about:blank` tab. No cloud redeployment, new OAuth grant or pairing replacement was needed.

Full Disk Access is awaiting the owner's local authentication in the open System Settings password prompt. The filesystem root change does not itself grant that macOS permission or root privileges. Protected-folder access is not yet recorded as verified.

## Administration MFA deferred at the owner's request

On 2026-09-21 (Asia/Tokyo), applied [ADR 007](adr/007-defer-admin-mfa.md). The reviewed Terraform plan contained exactly one in-place change: the administration application's `mfa_config.mfa_disabled` changed from `false` to `true`. A structural comparison of the saved plan confirmed every other resource value was unchanged before applying it. No Worker deployment, agent restart, credential replacement, OAuth relinking or account-authentication change was performed. Logs: `logs/mfa-defer-plan.log`, `logs/mfa-defer-apply.log`.

The live Cloudflare API confirmed MFA disabled for that application, the same exact owner allowlist and account-member identity provider, the original audience, and one-hour sessions. Worker settings still require Access, the original owner/audience, the administrator secret and the ChatGPT-only connection policy; local development remains disabled and preview URLs remain off. Private credential and Terraform-state files retain mode `0600`.

Anonymous dashboard, asset, owner-API and OAuth-consent requests still redirect to Cloudflare Access. Unauthenticated MCP returns 401, unknown MCP paths and malformed agent paths return 403, and health/discovery remain available. Evidence: `.deploy/mfa-defer-validation.json`. The initial probe incorrectly expected the malformed agent path to reach device authentication; its expected status was corrected to the existing strict path gate's 403. Cloudflare's organization response omitted the optional global MFA boolean, so no claim about that field was inferred from its absence.

In Microsoft Edge, the existing owner identity session completed the Cloudflare redirect and reached the real dashboard's administrator-key prompt without an independent MFA prompt. No administrator key was entered during this check. The existing connected **Remote Commander — My Mac** plugin successfully ran `ping_device`, returning `ok: true` and platform `macos`. The verification tab was closed after inspection.

Terraform formatting and validation passed, a fresh plan reported no changes, and all 43 local links in the updated operating/security/decision documents resolved. Idempotence log: `logs/mfa-defer-idempotence.log`. Rust and Node integration suites were not repeated for this infrastructure-only setting change; executable application behavior was unchanged. Independent MFA enrollment is now optional until the owner requests restoration; the older pending-MFA-login notes above describe the previous policy.

## Terminal PATH restoration

On 2026-09-25, applied [ADR 008](adr/008-terminal-path.md). The existing LaunchAgent's fixed PATH omitted NVM, causing a plain remote `node` command to exit 127. The OS account's configured login shell was verified as `/bin/zsh`; a clean Bash login did not select Node, while interactive-login zsh restored the expected executable paths. The installer now captures only PATH from that configured shell before changing an installation. No terminal startup file was edited.

`npm run check` completed successfully after the test correction below: Rust formatting, native/WASM Clippy, eight native-agent tests, two protocol tests, builds, and all 36 JavaScript/integration tests passed, with no failures or skips. Seven new tests cover Bash/zsh startup, noninteractive executable lookup, variable isolation, exact PATH preservation, malformed output, startup failures, output limits and deadline/descendant cleanup. Evidence: `logs/path-environment-check-verified.log`. All 36 local links checked in the affected operating/decision documents resolved.

The first complete run exposed a race in the new timeout test: its 500-millisecond deadline could expire before the fixture wrote its child PID. The corrected test observes child readiness and uses a five-second deadline before verifying termination. The complete suite was then rerun successfully; the production resolver's twenty-second deadline was not changed. The initial result remains in `logs/path-environment-check.log`.

Applied the same resolver to the already-installed LaunchAgent using a separate one-shot updater with `KeepAlive=false`, a private previous-plist backup, active-child checks and rollback on restart failure. The production agent restarted from PID 22432 to 32592 and reconnected using the same paired device and credential. The native binary and Cloudflare Worker were not changed. Structural comparison verified that PATH was the only changed plist field: root `/`, command arguments, write/shell permissions, log settings and `KeepAlive=false` were preserved. The completed updater job was unloaded after its successful exit.

Fresh `start_process` calls through the existing connected plugin, with no PATH override, no profile sourcing and no absolute executable prefix, successfully ran `node --version` (v24.14.1), `npm --version` (11.11.0), and `cargo --version` (1.96.0). The process PATH exactly matched the installed LaunchAgent's 22-entry PATH, and cwd remained `/`. `get_config` confirmed the unchanged root, permissions and operation limits. The final live verification exited zero. Evidence: `.deploy/terminal-path-release-status.json`, `.deploy/terminal-path-live-validation.json`, and `logs/terminal-path-deploy.stdout.log`; the release report identifies the private rollback backup.

An initial live diagnostic's overly strict environment-key assertion rejected the observed `__CF_USER_TEXT_ENCODING` marker after all executable checks had succeeded. Final validation records the actual keys and verifies PATH/configuration identity; it does not misrepresent that runtime marker as a copied profile variable. Separate startup tests verify that deployment secrets are not inherited and profile variables other than PATH are not copied.

PATH is a deployment-time snapshot, not a per-command login shell. Subsequent PATH or login-shell changes require a fresh setup/deployment. Aliases and shell functions are intentionally not imported, and command syntax remains `/bin/sh -c`.

## Not established by these checks

Production WebSocket hibernation billing under load, long-duration soak behavior, Linux execution, Windows support, independent penetration testing, tenant isolation and full binary-document/graphical-desktop feature parity were not verified. File access remains subject to macOS privacy controls. Shell commands run with the OS user's full authority; the configured root is their initial working directory, not an OS sandbox.

The OAuth policy binds one owner-approved token family. It does not prove OpenAI network origin or independently verify a ChatGPT account ID. Signed client assertions/mTLS are not implemented. Possession of an unexpired bearer token remains sufficient until revocation.

See [operations](OPERATIONS.md), [security boundaries](SECURITY.md), and [ADR 004](adr/004-private-chatgpt-connection.md).
