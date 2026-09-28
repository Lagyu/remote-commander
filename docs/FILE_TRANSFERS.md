# Binary file transfers

Remote Commander supports downloads and uploads up to **1 GiB (1,073,741,824 bytes)** per file, including decimal 1 GB files. Binary data travels in 1 MiB chunks over the existing authenticated agent connection. Small text tools retain their existing limits; they are not the binary transfer API.

## MCP tools

`download_file({device_id, path})` opens a regular file within the agent root and returns an MCP `resource_link`, plus `structuredContent.download_url`, byte length and expiry. Open that link in a browser or use an HTTP client. The file is streamed directly from the Mac; it is not staged in Cloudflare storage.

`upload_file({device_id, path, size, overwrite?, sha256?})` creates an upload session for exactly `size` bytes and returns `structuredContent.upload_url`. Open the link, select a local file, and press **Start upload**. The page displays progress and the completed file's SHA-256. The destination parent directory must already exist. An empty file is supported with `size: 0`.

`overwrite` defaults to `false`. Set it explicitly to `true` only when replacing an existing regular file is intended. `sha256`, when provided, must contain the expected 64 hexadecimal SHA-256 characters. A mismatch prevents publication and leaves the existing destination unchanged.

Paths are relative to the configured agent root, as with other file tools. Absolute paths, `..`, final symlinks and non-regular files are rejected. Upload creation requires the `commander:write` OAuth scope and the agent's local `--allow-write` permission. Download creation requires `commander:read`.

Example tool arguments for an agent rooted at `/`:

```json
{"device_id":"YOUR_DEVICE_ID","path":"Users/you/Downloads/archive.zip"}
```

```json
{"device_id":"YOUR_DEVICE_ID","path":"Users/you/Downloads/incoming.zip","size":1073741824,"overwrite":false}
```

## Limits and lifetime

| Property | Limit / behavior |
| --- | --- |
| File size | 0 through 1,073,741,824 bytes, inclusive |
| Binary chunk | 1 through 1,048,576 bytes |
| Link / session lifetime | One hour from creation, not extended by activity |
| Retained sessions | Eight per agent; cancel finished links to release slots immediately |
| Agent WebSocket message | 2 MiB including base64 and metadata |
| Non-transfer HTTP request body | Existing 192 KiB limit |
| Transfer chunk HTTP body timeout | 60 seconds, bounded before the Durable Object boundary |
| Agent command timeout | Existing 25 seconds per chunk / operation |

The agent must remain online and have sufficient disk space. Transfers do not survive an agent process restart: create a new link after restarting. A brief WebSocket interruption can be resumed while the agent session remains alive. A Worker restart may interrupt a download response; retry with an HTTP byte range while the link and agent session remain valid.

Downloads retain an open source handle and check size and modification time before and after each chunk. Modification is rejected rather than silently combining different file versions. This is not a filesystem snapshot: avoid changing the source while downloading it.

## HTTP API

The random URL token is a bearer capability. **Keep the complete link private.** It grants access to one already-approved transfer until expiry; do not paste it in public logs, tickets or messages. Neither administrator credentials nor OAuth tokens should be added to these URLs.

### Download

- `GET /download/{token}` streams the file with attachment headers and the actual content length.
- `HEAD /download/{token}` reports metadata without downloading the body.
- `Range: bytes=START-END`, `bytes=START-`, and `bytes=-SUFFIX` support a single byte range. Successful ranges return `206` and `Content-Range`; invalid or multiple ranges return `416`.
- `ETag` and `If-Range` are supported for resuming the same session.
- `DELETE /download/{token}` revokes the link and releases the source handle when the agent is reachable.

Example, after assigning a private link locally:

```sh
curl --fail --location --output archive.zip "$DOWNLOAD_URL"
# Continue an interrupted download using the same unexpired link:
curl --fail --location --continue-at - --output archive.zip "$DOWNLOAD_URL"
```

### Upload

1. `GET /upload/{token}/status` returns `size`, `bytes_received`, `complete`, `chunk_bytes`, `expires_at` and (after completion) `sha256`.
2. `POST /upload/{token}/chunk?offset=N` with `Content-Type: application/octet-stream` sends up to 1 MiB of raw binary data at the given byte offset. Chunks are sequential; there must be no gaps. A retry wholly inside acknowledged data is accepted only when the bytes match exactly.
3. `POST /upload/{token}/complete` verifies the exact length and optional SHA-256, syncs the temporary file, then publishes it atomically. Completion is idempotent for the same live session.
4. `DELETE /upload/{token}` revokes the link and removes incomplete temporary data when the agent is reachable. It never deletes a completed destination file.

The browser page replays and verifies any acknowledged prefix before continuing, so selecting a different same-size file cannot silently corrupt a resumed upload. Clients that resume directly at `bytes_received` are responsible for selecting the same source file; supply an expected SHA-256 for end-to-end identity checking.

The response to a lost completion request may be ambiguous. Check `/status` before deciding whether to retry. `429` indicates busy; retry the same chunk after a delay. `503` indicates a temporarily unavailable agent or uncertain delivery. `409` represents transfer conflicts such as wrong offset, mismatched retry data, source mutation or a failed final checksum. Do not blindly restart destructive operations.

## Storage, security and cleanup

Cloudflare Durable Object storage contains only short-lived transfer metadata. The full file is never buffered in Worker memory or persisted in Cloudflare storage. Backpressure limits downloads to bounded chunks. This design avoids single-request upload limits and per-file memory growth; no R2 bucket is required.

Uploads use random, private, `create_new` temporary files in the destination directory. Default publication uses an atomic no-clobber hard link; explicit replacement uses rename. The destination is not exposed as a partial file. A concurrently created destination prevents a default upload from overwriting it. Existing symlink destinations are refused.

Active session cleanup runs at least once per minute. Normal cancellation, expiration and graceful process exit remove incomplete temporary files. A hard process crash or power loss can leave a `.rdc-upload-*.tmp` file in the selected destination directory; such files are not automatically deleted by scanning the entire configured root. Confirm that no transfer is using a leftover before removing it manually.

Revoking an OAuth client prevents new link creation but does not immediately revoke already-issued capability links. Use the transfer DELETE endpoint, revoke the device, or let the link expire.

Capability routes bypass Cloudflare Access's interactive browser login, **not application authorization**. Link creation remains authenticated through MCP, transfer requests require an exact unexpired token and a still-paired device, and ordinary administration remains protected by owner-only Cloudflare Access. Device revocation prevents new transfer requests; an already-running HTTP response may continue until the socket closes or the link expires. Upload pages have no third-party resources. Responses use `no-store`, attachment headers for downloads, and a restrictive Content Security Policy.

## Build, test and deploy

```sh
cargo fmt --all --check
cargo clippy -p rdc-agent -p rdc-protocol --all-targets -- -D warnings
cargo clippy -p rdc-worker --target wasm32-unknown-unknown -- -D warnings
cargo test -p rdc-agent -p rdc-protocol
cargo build -p rdc-agent
npm run build:worker
npm run test:e2e
# Explicit large-file test: sends and receives every byte using the production agent.
cargo build --release -p rdc-agent
RDC_TEST_GIB=1 node --test --test-name-pattern='full 1 GiB upload' tests/transfers.test.mjs
RDC_TEST_GIB=1 node --test --test-name-pattern='full 1 GiB streamed download' tests/transfers.test.mjs
```

Deploy the Worker and review/apply the Access Terraform plan for `download/*`, `upload/*`, `transfer.js` and `transfer.css`. Upgrade the native agent as well; the old agent does not understand the new transfer commands. No Durable Object migration is required. Preserve the existing device configuration, agent permissions and OAuth client connection.

After deployment, `/health` reports `file_transfers.max_bytes`, `chunk_bytes` and `expires_in`. `get_config` reports the corresponding installed-agent limits. The MCP tool catalog contains 26 tools. A client that caches its tool catalog may need its connection refreshed to discover `download_file` and `upload_file`; do not revoke an existing authorization merely to validate deployment.

Run `npm run check:transfers-live` for non-destructive hosted route and authentication checks. This check does not create or revoke OAuth clients and does not touch Mac files.

### Full-size validation runtimes

The optional HTTP capacity tests allow 45 minutes per direction because they transfer every byte through the local Worker and agent. Run them from a local terminal; Remote Commander's shell-command lifetime is independently capped at five minutes. That shell-command cap is not the file-transfer lifetime (one hour). A capacity test stopped by that command cap is not a completed integrity test.

The native transfer implementation can also be exercised independently at full size, including actual disk writes, atomic publication, per-chunk reads and complete-file SHA-256 verification:

```sh
cargo test --release -p rdc-agent full_gib_native_transfer_integrity -- --ignored --nocapture
```

## Refresh an existing ChatGPT connection

A previously connected ChatGPT conversation can retain its older tool catalog even after the server and native agent are upgraded. Remote Commander 0.2.0 supports ChatGPT Refresh/Reconnect while a connection is already pinned: complete the administrator-key OAuth consent when prompted, then start a new conversation so ChatGPT can attach the refreshed 26-tool catalog. The replacement code exchange revokes the prior token family only after the new authorization succeeds, so uninstalling the app or using **Replace ChatGPT connection** is not required for ordinary metadata refresh.

The MCP initialization instructions also direct binary and large-file operations to these tools, rather than the bounded text tools. The tool returns a private transfer link; actual file bytes are sent through the browser/HTTP transfer endpoints, not embedded in MCP arguments. This does not change ChatGPT's own attachment-size limits.

Official metadata-refresh documentation: https://developers.openai.com/plugins/deploy/connect-chatgpt#refresh-metadata
