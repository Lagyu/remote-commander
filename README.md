# Remote Commander

A self-hosted remote MCP file and terminal service, implemented mainly in Rust and hosted on Cloudflare Workers with a SQLite-backed Durable Object. The native agent runs on an explicitly paired computer and establishes an outbound authenticated WebSocket. The dashboard approves devices, shows connection status and activity, and revokes access.

This is an independent implementation of the core workflow described by [Remote Desktop Commander](https://github.com/desktop-commander/remote-desktop-commander). Its hosted implementation is proprietary; this project does not copy its implementation or visual assets. This project provides remote file/process tools and explicit opt-in screenshots, not graphical desktop streaming.

## What is included

| Area | Implementation |
| --- | --- |
| Native agent | Rust/Tokio executable for macOS and Linux; explicit folder, write, shell, and macOS screenshot permissions |
| MCP service | Rust/WASM, Streamable HTTP, 26 tools, bounded requests and responses |
| Authorization | OAuth authorization code with PKCE S256, dynamic public client registration, exact redirect and resource validation, rotating refresh tokens |
| Device access | Expiring pairing codes, owner approval, separate device credentials, connection replacement and revocation |
| Dashboard | Cloudflare Access owner login; separate administrator credential held in tab memory; independent MFA temporarily deferred |
| Infrastructure | Wrangler owns Worker/DO migrations; Terraform owns Access policies and the optional custom domain |
| Verification | Native tests plus real workerd, Rust agent, official MCP SDK, and headless-browser integration tests |

This is a single-owner service. Public deployments accept one owner-approved ChatGPT OAuth connection at a time. That connection can address all paired devices within its granted scopes. It does not implement multiple users, per-device client permissions, Windows process management, PTYs, graphical desktop control, arbitrary system-process killing, or specialized binary document editors. Shell commands can invoke locally installed tools when shell access is enabled.

## Local setup

Run commands from this directory. Install Rust through rustup, Node.js 22 or newer, and Terraform 1.5 or newer. The checked lockfiles and pinned worker-build version define the dependency set. The implementation was exercised with Rust 1.96, Node 24, and Terraform 1.5.7 on macOS ARM64.

```sh
npm ci
rustup target add wasm32-unknown-unknown
rustup component add rustfmt clippy
cargo install worker-build --version 0.8.6 --locked --root .tools
node scripts/setup-local.mjs
cargo build --release -p rdc-agent
npm run dev
```

Wrangler compiles the Worker before starting at `http://127.0.0.1:8787`. The setup script creates a private `.dev.vars` file containing a generated `ADMIN_TOKEN`. Read that file locally and use the token to unlock the dashboard. Do not paste it into an MCP client, chat, source control, or issue report.

In another terminal, pair the local agent:

```sh
./target/release/remote-commander pair \
  --server http://127.0.0.1:8787 \
  --name "My computer" \
  --insecure-localhost
```

The agent opens the dashboard and prints an eight-character code. Unlock the dashboard, find the computer using that code, verify its name and code, and approve it. Pairing expires after ten minutes. The credential is saved at `~/.config/remote-commander/device.json` with mode `0600`; `--config /path/device.json` selects another file. Existing credentials are never silently overwritten.

Start the agent with an existing directory:

```sh
./target/release/remote-commander run \
  --root /absolute/path/to/project \
  --insecure-localhost
```

This enables file reads. Add `--allow-write` for edits and `--allow-shell` for process sessions. Screenshot capture is enabled by default on macOS; pass `--no-screenshot` to disable it. The legacy `--allow-screenshot` flag remains accepted for compatibility. Screenshot capture still requires macOS Screen Recording permission for the agent binary. The `--root` capability confines the file tools; it is not a shell sandbox and does not scope screenshots. Shell execution has the operating-system user's authority. Stop with Ctrl+C, use `shutdown_device`, or revoke the computer in the dashboard.

Production pairing and agent commands use the public HTTPS origin and omit `--insecure-localhost`. An optional macOS LaunchAgent installer is available; see below.

## Deploy to Cloudflare

Cloudflare account authentication is required for a real deployment. No account credentials or domain are embedded in this project. See [deployment and recovery](docs/OPERATIONS.md) for infrastructure ownership, credential rotation, and teardown.

Generate a separate production administrator credential without printing its contents:

```sh
node scripts/create-admin-key.mjs /absolute/private/path/commander-admin.key
```

The destination's parent directory must already exist. Protect and back up that key. Authenticate Wrangler with `npx wrangler login`, or provide `CLOUDFLARE_API_TOKEN` through your secret manager. For custom domains, Terraform also requires `CLOUDFLARE_API_TOKEN` and a zone in the selected account.

For this deployment, credentials are in the project’s private, ignored `.env` (mode `0600`). See `.env.example` for supported keys. The production administrator key is a separate private file at `.deploy/admin.key`. The loader accepts only named deployment/Access variables and does not source shell code. Existing process environment values take precedence.

Provision [owner-only Cloudflare Access](infra/access/README.md) before the first production deployment. Its Terraform outputs populate the Access audience and owner email in `.env`. Public deployments require these settings; missing runtime configuration denies administration. The running Mac agent and existing ChatGPT connection do not require an interactive Access login.

```sh
npm run deploy
```

The deployment script verifies the account’s Workers subdomain and creates it if absent. It refuses to rename an existing account subdomain. The API token stays local; only the separate administrator secret is uploaded to the Worker. Explicit flags remain supported:

Deploy on a Workers subdomain:

```sh
npm run deploy -- \
  --account-id YOUR_32_CHARACTER_ACCOUNT_ID \
  --public-url https://remote-commander.YOUR_SUBDOMAIN.workers.dev \
  --admin-secret-file /absolute/private/path/commander-admin.key
```

Or deploy with a custom hostname:

```sh
npm run deploy -- \
  --account-id YOUR_32_CHARACTER_ACCOUNT_ID \
  --zone-id YOUR_32_CHARACTER_ZONE_ID \
  --public-url https://commander.example.com \
  --admin-secret-file /absolute/private/path/commander-admin.key
```

The script builds WASM, generates production configuration with local HTTP disabled, deploys the Worker, provisions `ADMIN_TOKEN` through stdin, applies the optional Terraform domain plan, and checks `/health`. Secrets are never passed as command arguments or Terraform inputs. A new Worker denies access until its administrator secret is configured. The script stops on a failed command; rerunning with the same account, hostname, and worker name is supported.

For a credentialless packaging check, use a placeholder account and hostname with `--dry-run`:

```sh
npm run deploy -- \
  --account-id 00000000000000000000000000000000 \
  --public-url https://remote-commander.example.workers.dev \
  --dry-run
```

The dry run builds and bundles the Worker and validates Terraform. It does not create cloud resources, prove account permissions, reserve the hostname, or verify live TLS/DNS.

## Connect your ChatGPT account

1. Open the deployed owner dashboard, sign in through Cloudflare Access, and unlock it using `.deploy/admin.key`. Independent MFA is currently deferred at the owner's request; see [ADR 007](docs/adr/007-defer-admin-mfa.md).
2. Click **Allow ChatGPT connection**. This revokes earlier client access and opens a ten-minute linking window.
3. In your signed-in ChatGPT account, create a private custom plugin with server URL `https://YOUR_HOST/mcp`, OAuth, and Dynamic Client Registration (DCR). Do not publish or share it.
4. Select `commander:read`, `commander:write`, and `commander:execute` for the requested full access. Set them as base scopes as well if action-specific requests should always retain all three permissions.
5. Sign in to the service and approve the request with your administrator key. The server pins the resulting OAuth token family. Another user cannot authorize a second family even with the same public OAuth client ID.

The dashboard shows whether access is locked, linking is open, or one connection is approved. **Revoke client access** invalidates tokens and closes linking. **Replace ChatGPT connection** remains available for an explicit destructive reset, but ordinary ChatGPT Refresh/Reconnect does not require it. While a connection is pinned, a fresh DCR/authorization flow may reach the administrator-key consent page; successful code exchange atomically replaces and revokes the previous token family. Until that exchange succeeds, the existing connection remains usable.

Production accepts the exact callback `https://chatgpt.com/connector_platform_oauth_redirect`, advertises RFC 9207 issuer identification, and includes `iss` in success and denial callbacks. Authorization uses PKCE S256 and the exact `/mcp` resource. Duplicate scope names are normalized because ChatGPT combines base and action/default scopes. See [OpenAI’s authentication contract](https://developers.openai.com/plugins/build/auth) and [ADR 004](docs/adr/004-private-chatgpt-connection.md).

This binds access to the connection you approve while signed in to your ChatGPT account. OAuth does not give this service your ChatGPT account ID or prove the origin of every bearer-token request. Keep the owner key and ChatGPT account private; stolen bearer tokens remain usable until expiry or revocation. IP/User-Agent claims are not treated as identity. There is no CIMD, OIDC, client-secret authentication, or public multi-user access.

Generic MCP clients are available for local integration tests only: set `CHATGPT_ONLY=false` with `ALLOW_LOCALHOST=true` on a loopback origin. Public origins always enforce the connection policy, regardless of that setting.

## Run the macOS agent at login

After deploying and building the release agent, obtain a short-lived owner Access session as described in [operations](docs/OPERATIONS.md#owner-login-for-setup-commands), then run:

```sh
npm run setup:macos -- --root "$HOME" --name "My Mac" --allow-write --allow-shell
```

This pairs the Mac using your local owner key, installs the binary and private device credential under `~/Library/Application Support/Remote Commander/`, and starts `app.remote-commander.agent`. The LaunchAgent uses `RunAtLoad=true` and `KeepAlive=false`; network reconnection happens inside the running agent. Logs are private under `~/Library/Logs/Remote Commander/`. Re-running reuses the same device and updates the binary and arguments. Shell commands run with your OS user’s authority; home is their initial working directory, not a shell sandbox.

With `--allow-shell`, installation resolves PATH from the OS account's interactive login shell (Bash or zsh), including the owner's normal startup files, and saves only that PATH in the LaunchAgent. This makes NVM/Homebrew/Cargo executables available without hardcoding a Node version or loading shell profiles for every command. Command syntax remains `/bin/sh -c`, and the selected working directory and permission flags are unchanged. Startup receives a minimal environment, has closed stdin and a 20-second deadline, and its output is not logged. Failure aborts before replacing the running agent. Re-run installation after changing the login shell or its PATH; aliases, shell functions and other profile variables are not imported. See [ADR 008](docs/adr/008-terminal-path.md).

Inspect or stop it with:

```sh
launchctl print "gui/$(id -u)/app.remote-commander.agent"
launchctl bootout "gui/$(id -u)/app.remote-commander.agent"
```

See [operations](docs/OPERATIONS.md) for restart, removal, and access recovery. `npm run check:live` verifies the actual installed agent using a disposable home-directory fixture, then revokes its temporary test authorization. It refuses to replace a linked ChatGPT connection or an open linking window; use it before connecting ChatGPT.

After connecting, start with `list_devices`, then `ping_device` and `get_config`. File paths are relative to home: `Documents/example.txt` addresses `~/Documents/example.txt`. `start_process` returns a session ID; retrieve output with `read_process_output`. Screenshot access is enabled by default unless the agent is started with `--no-screenshot`; `get_screenshot` returns a bounded JPEG MCP image for one display, defaulting to display 1 with a 1600-pixel maximum dimension.

For explicitly authorized machine-wide file access, use `--root / --allow-write --allow-shell` and grant the installed agent **Full Disk Access** in macOS settings; see [machine-wide access](docs/OPERATIONS.md#machine-wide-file-access-and-macos-privacy). With `/` as root, paths use `Users/yuya/Documents/example.txt`. Launch GUI apps through Launch Services, for example `start_process` with `/usr/bin/open -a 'Microsoft Edge'`.

## Tests

```sh
# Needed where a system Chrome installation is not present:
npx playwright install chromium

npm run check
terraform -chdir=infra init -backend=false -input=false
terraform -chdir=infra fmt -check
terraform -chdir=infra validate
```

The check command formats/lints Rust, runs native tests, builds the agent and Worker, and runs the integration suite. Integration tests use isolated fixture folders and random credentials. They cover real disk changes, traversal and symlink escapes, FIFO rejection, process input/output and cleanup, OAuth rejection and token replay, browser pairing, connection loss, Worker restart, and revocation. They deliberately exercise a 25-second lost-response deadline.

Each run retains private diagnostic logs and process lifecycle records under `logs/e2e-*`, saves dashboard screenshots under `test-results/`, and removes temporary credentials and state under `.local/`. These generated directories are ignored. No real user files or accounts are used by the suite. See [validation notes](docs/VALIDATION.md) for the observed result and remaining evidence gaps.

## Project map

| Path | Responsibility |
| --- | --- |
| [crates/protocol](crates/protocol/src/lib.rs) | Wire types, limits, tool catalog and scopes |
| [crates/agent](crates/agent/src/main.rs) | Pairing, outbound connection, file capabilities and process lifecycle |
| [crates/worker](crates/worker/src/lib.rs) | HTTP boundary, Durable Object, OAuth, device relay and metadata |
| [web](web/index.html) | Owner dashboard and consent styling |
| [wrangler.json](wrangler.json) | Worker configuration and SQLite Durable Object migration |
| [infra](infra/main.tf) / [infra/access](infra/access/main.tf) | Terraform domain mapping and owner Access policies |
| [scripts/deploy.mjs](scripts/deploy.mjs) | Validated production config, secret provisioning, deploy and health check |
| [tests](tests/e2e.test.mjs) | Full-path integration and browser tests |
| [Architecture decisions](docs/adr/README.md) | Alternatives, rationale, tradeoffs and evidence |

## Limits and operational behavior

HTTP uploads are capped at 192 KiB and five seconds; agent frames at 512 KiB. Text files are capped at 2 MiB; a write call accepts 64 KiB and a read returns at most 32 KiB. Directory traversal stops at 2,000 entries. Searches retain at most eight bounded snapshots for ten minutes. Process output retains the latest 64 KiB and reports discarded bytes explicitly.

At most eight commands are in flight and eight processes run per agent. A process has a maximum five-minute lifetime; at most 32 completed/running sessions are retained, with completed entries expiring on later starts. File moves are for regular files on the same filesystem and do not overwrite an existing destination. Atomic file replacement creates a private `0600` file; custom modes, ACLs and extended attributes are not preserved.

Tool work runs independently of connection heartbeats. OS file operations and process working-directory resolution have a 20-second caller deadline and separate eight-operation limits. A timed-out syscall can still complete; its slot remains occupied until it returns. Concurrent requests may finish out of order, so await each operation before sending one that depends on it.

Requests are not automatically retried after a lost reply. A timeout or disconnect can mean the command executed but its result was lost: inspect the file or process state before repeating a change. Device reconnects preserve in-memory agent sessions while the agent remains running. They do not recover processes or search snapshots after an agent restart.

The cloud service handles requested content in transit but does not persist tool arguments or results. Its bounded activity history stores tool names, device IDs, timestamps and outcomes. Cloudflare terminates TLS; this is not end-to-end encryption against the hosting account or operator. Read the [trust model](docs/SECURITY.md) before granting shell access.

## Binary transfers

Use `download_file` and `upload_file` for binary files up to **1 GiB (1,073,741,824 bytes)**. Downloads stream directly from the computer and support HTTP Range/resume. Uploads use a private browser page or chunked HTTP API, with SHA-256 verification, retries and atomic publication. Both links expire after one hour. See [File transfers](docs/FILE_TRANSFERS.md) for usage, security, limits and deployment.
