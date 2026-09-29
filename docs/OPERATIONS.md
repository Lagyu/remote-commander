# Deployment and recovery

## Resource ownership

`wrangler.json` declares the Worker entrypoint, compatibility date, Durable Object binding and initial SQLite migration. The deployment script generates `.deploy/wrangler.json` with the selected account, HTTPS public origin, local HTTP disabled, ChatGPT restriction enabled, and the required Access audience/owner. Wrangler owns Worker code, secrets and Durable Object migrations. Terraform owns the optional domain mapping in `infra/main.tf` and owner Access configuration in the independent `infra/access` root. A `workers.dev` deployment does not need a domain resource. The deployment script manages initial account-subdomain creation through Cloudflare’s documented API, verifies it, and refuses to rename an existing subdomain. Provider 5.25.0 has no account-subdomain resource.

Do not manage the Worker independently with a second Terraform resource. Do not rename the Durable Object class, binding, migration tag or singleton name as routine configuration changes: they determine state identity and migration behavior. Add new migrations deliberately and preserve prior entries.

The Cloudflare account must permit Worker deployment, secret updates and Durable Object usage. A custom domain additionally requires permission to manage the Worker domain mapping in its zone. Obtain account/zone IDs and appropriately scoped API credentials from the account operator. Terraform reads `CLOUDFLARE_API_TOKEN`; credentials are not committed or included in generated tfvars.

Terraform uses local state under `infra/` and `infra/access/`. Keep both state files and lockfiles when moving the deployment. State and plans are private, ignored operational data. Use an access-controlled remote backend before sharing management between operators.

## Private administration

Follow [Access provisioning](../infra/access/README.md) before production deployment. Cloudflare protects the entire service hostname with the exact owner's Cloudflare account identity. Sessions last one hour. Independent MFA for the administration application is temporarily disabled at the owner's request; the identity check and separate administrator key remain required. Open the dashboard and sign in normally. The App Launcher retains a separate owner-only policy for future authenticator enrollment; its token cannot authorize the Commander administration app. See [ADR 007](adr/007-defer-admin-mfa.md) and the [restoration steps](../infra/access/README.md).

Only the explicitly listed machine endpoints bypass Cloudflare's interactive login. They retain Rust's OAuth/PKCE/device checks. The Rust outer Worker checks the runtime-verified Access audience and owner for all other requests, including unknown paths under a bypass prefix. A forged `Cf-Access-Jwt-Assertion`, identity header, or cookie cannot satisfy that check. Production does not accept local development Access simulations. See [ADR 005](adr/005-private-administration.md).

Do not put a blanket Access policy on `/mcp`, `/oauth/token`, or `/agent/*`; ChatGPT and the installed Mac agent cannot complete browser login. Do not use Worker-level Access for this service: Cloudflare documents that this mode currently rejects WebSocket upgrades. Keep the hostname-based Terraform rules and preserve their path exceptions.

## Owner login for setup commands

The running LaunchAgent needs only its existing device credential. The installer and pre-connection live-check script also call protected owner APIs; use a short-lived Access application token for those commands. Install `cloudflared` from Cloudflare or Homebrew if needed, then:

```sh
umask 077
cloudflared access login https://remote-commander.yuya-commander.workers.dev > .deploy/access-login-output.txt
cloudflared access token --app=https://remote-commander.yuya-commander.workers.dev > .deploy/access.jwt
```

Complete the browser login and any authentication requested by the Cloudflare account itself. Output stays in private ignored files. For another deployment replace the hostname with that deployment's origin. `REMOTE_COMMANDER_ACCESS_TOKEN_FILE` can override the token file. The helper checks its file permissions, format, audience and expiration locally; Cloudflare performs the actual signature/policy validation. The scripts send it only to protected owner endpoints, alongside the existing administrator key. No long-lived service-token bypass is installed. Expiration requires another owner login but does not interrupt ChatGPT or the agent. Do not run `check:live` after linking ChatGPT; it intentionally refuses to replace that connection.

## Initial deployment

Follow the README's build and deploy commands. The script uses Workers' automatic HTTPS endpoint for `workers.dev`, or Terraform's Worker custom-domain mapping for an existing Cloudflare zone. For custom domains, avoid hostnames already assigned to another service. The production script disables the alternate `workers.dev` route when a custom domain is selected, and disables preview URLs.

Read and approve the saved Terraform plan when managing infrastructure separately. Running `npm run deploy` intentionally applies the generated plan as part of that explicit deployment command. It does not silently deploy as part of `npm run check` or `--dry-run`.

On success the script verifies `/health` and prints the public dashboard and MCP URLs. Complete a real pairing using a disposable folder, then authorize the desired external MCP client and exercise read/write/process operations according to the local flags. Local integration tests do not replace this final hosted check.

## Changes and rollback

Run `npm run check` and the deployment dry run before a release. Preserve `Cargo.lock`, `package-lock.json`, the provider lockfile, previous source versions and the Worker migration history. Apply new code with the same account, Worker name and public origin to retain the existing device registry.

Rebuild and redeploy a previously verified source version for a code rollback. Storage migrations need their own compatibility review; a code rollback does not undo or validate a destructive state change. Native agent search results and process-session state are in memory and cannot be restored after agent exit. Avoid changing the public origin casually: agents store it locally and OAuth uses it as the exact resource identifier.

The script stops if Worker upload, secret provisioning, Terraform, or public health verification fails. If the Worker upload succeeded before a later failure, that code may already be live. Fix the failed stage and rerun with the same arguments; do not assume a failed health check rolled back anything. DNS or certificate propagation may require checking the account before retrying. A missing/short administrator secret produces HTTP 503.

## Rotate access

Create a new private administrator-key file using `scripts/create-admin-key.mjs`. Apply it to an existing deployment with:

```sh
npx wrangler secret put ADMIN_TOKEN --config .deploy/wrangler.json < /absolute/private/path/new-admin.key
```

Lock old dashboard tabs and unlock using the new key. For an incident, also use “Revoke client access” and revoke affected computers. Administrator-key rotation alone leaves issued client/device credentials valid. Device revocation deletes its credential hash and sends a close frame; the native agent stops and cleans up tracked processes. A device must pair again using a new credential file. After a legitimate computer replacement, remove the old local credential only after revoking that old registration.

## Lifecycle and logs

The foreground CLI supports Ctrl+C or SIGTERM. The optional `scripts/install-agent.mjs` creates a macOS LaunchAgent with `RunAtLoad=true`, `KeepAlive=false`, explicit permissions/root, a private installed binary/device credential, and durable logs. It confirms both the running launchctl PID and the hosted device’s online state before reporting success. Its status record is `.deploy/agent-installation.json`.

```sh
# Inspect service and connection logs
launchctl print "gui/$(id -u)/app.remote-commander.agent"
tail -n 40 "$HOME/Library/Logs/Remote Commander/agent.stderr.log"

# Restart a loaded service after a deliberate stop/crash
launchctl kickstart "gui/$(id -u)/app.remote-commander.agent"

# Stop and unload it
launchctl bootout "gui/$(id -u)/app.remote-commander.agent"

# Reload the installed configuration
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/app.remote-commander.agent.plist"
```

Remote shutdown and device revocation do not trigger a KeepAlive restart loop. `RunAtLoad` runs at a later login, but a revoked device credential still fails authentication. To permanently disable startup, boot out the service and remove its specific LaunchAgent plist. Revoke the device before deleting its saved credential. The installer refuses to silently replace a credential from another deployment or a revoked device.

The setup command reads only this project’s `.env`, which must be an owned regular private file. Cloudflare/API/administrator credentials are not inherited by the LaunchAgent. When shell access is enabled, setup separately runs the OS account's Bash/zsh interactive login startup in its home directory with only HOME, USER, LOGNAME, SHELL, a bootstrap PATH and LANG. It captures only PATH into the plist, not the rest of the terminal environment. Startup has no stdin, a 20-second timeout, bounded output and process-group cleanup; failure leaves the running installation unchanged. Profiles are trusted owner code and may have their usual local side effects. Re-run setup after changing PATH or switching the login shell. Remote commands still use `/bin/sh -c` and the requested cwd. The agent clears shell environment variables except PATH, HOME and LANG. With the root set to home, file tools can access sensitive files beneath home as explicitly authorized; macOS privacy controls still apply.

### Machine-wide file access and macOS privacy

An owner who wants file tools to reach every OS-accessible location can install with `--root / --allow-write --allow-shell`. Paths remain relative to that root: `Users/yuya/Documents/example.txt` addresses `/Users/yuya/Documents/example.txt`; shell sessions default to `/`. This grants the existing connected client the same expanded file access, and does not make the agent a root process or bypass system-protected files.

In **System Settings → Privacy & Security → Full Disk Access**, add the installed executable at `~/Library/Application Support/Remote Commander/remote-commander` and enable it. Authenticate locally when macOS asks, then restart the agent. The permission must cover the installed binary, not just Terminal or a development build. Recheck it after replacing the binary if macOS prompts again.

A pending Downloads/Desktop/Documents or Screen Recording prompt can block an OS operation. The agent processes other requests and heartbeats independently, returns a descriptive timeout after 20 seconds, and bounds outstanding OS work. Timed-out work may still finish after consent; inspect state before retrying changes. Logs show each tool's start, outcome and elapsed time without its arguments. `get_config` and `ping_device` remain available during blocked file or screenshot operations.

Screenshot access is independent of the file root and is enabled by default; start or install the agent with `--no-screenshot` to disable it. Enable Screen Recording for the exact agent executable in System Settings → Privacy & Security → Screen Recording. `get_screenshot` captures one display, defaults to display 1, and downscales/compresses the result before relay so it stays within the protocol frame limit.

To launch Microsoft Edge on macOS, use `start_process` with `command: "/usr/bin/open -a 'Microsoft Edge'"`, then inspect the returned session with `read_process_output`. Launch Services owns the GUI application's lifetime. Directly running the app executable remains subject to the session timeout and process-group cleanup.

`logs/e2e-*/lifecycle.json` records every test-owned process PID and exit outcome; test-owned services are terminated and temporary state is removed by the harness. A timeout or failed test does not authorize operating on unrelated processes. Diagnostics and screenshots remain local and ignored. Inspect them before sharing, even though integration fixtures are synthetic.

## Teardown

Revoke devices and stop their agents first. Export any operational information you need before deleting cloud state. To intentionally remove a custom-domain mapping, run Terraform destroy from `infra/` using the same variables and retained state. To intentionally remove the Worker, use Wrangler's delete command with the generated production configuration. These are destructive operator actions and are not run by the test or deploy scripts. Verify the domain no longer serves the application and review the account for retained Durable Object storage before considering cleanup complete.

## Primary references

- [Workers Rust support](https://developers.cloudflare.com/workers/languages/rust/)
- [Durable Object WebSockets](https://developers.cloudflare.com/durable-objects/best-practices/websockets/)
- [Wrangler configuration](https://developers.cloudflare.com/workers/wrangler/configuration/)
- [Worker custom domains](https://developers.cloudflare.com/workers/configuration/routing/custom-domains/)
- [Terraform Worker custom-domain resource](https://registry.terraform.io/providers/cloudflare/cloudflare/latest/docs/resources/workers_custom_domain)
