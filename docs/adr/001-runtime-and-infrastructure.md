# ADR 001: Rust agent and Cloudflare Durable Object relay

Date: 2026-09-20. Status: accepted.

## Context and alternatives

The requested product connects remote MCP clients to a local computer and asks for Rust as the primary language, Cloudflare hosting and infrastructure as code. Alternatives were a TypeScript-only Worker/agent, a Rust server on a VM with Cloudflare Tunnel, and a Rust WASM Worker plus native Rust agent. TypeScript has broader MCP server libraries, but would put the core service outside the requested primary language. A VM supports a conventional Rust HTTP stack and PTYs, but adds an always-running host and lifecycle management.

## Decision

Use `workers-rs` for the HTTP/MCP service and a SQLite-backed Durable Object to own device connections, authorization records and bounded activity history. Use native Rust/Tokio for filesystem and process operations. Keep protocol types/tool definitions in a shared crate, with a small vanilla JavaScript dashboard. This uses one Cloudflare stateful primitive and avoids a separate database, KV, queue, R2 bucket or container where none is required by the implemented workflow.

Wrangler owns Worker code, bindings and migration history. Terraform owns the optional custom-domain mapping. The deployment script derives production configuration from one canonical Wrangler file, provisions the administrator secret through stdin, and checks the public endpoint. Two tools do not own the same cloud resource.

## Evidence, tradeoffs and validation

The Worker packages as WebAssembly with worker-build 0.8.6, and the native agent compiles on macOS ARM64. Local workerd integration exercises persistent state, WebSocket routing and agent reconnection after runtime restart. Terraform initialization uses the pinned 5.25.0 provider; the actual provider schema confirmed that the obsolete `environment` field is optional/deprecated, so it was removed.

The one-owner Durable Object serializes authorization state and limits concurrent relay requests. It is not a multi-region scale benchmark or a multi-tenant architecture. Hibernation preserves socket attachments, but an in-flight request lost during runtime restart may have unknown execution state. The stateful singleton is a deliberate limit appropriate to this release.

Two packaging/runtime issues were found experimentally: full symbol stripping removed the externref table needed by wasm-bindgen, so the release profile strips only debug information; mutable security headers cannot be added to the Fetch API's immutable redirect response, so OAuth constructs a normal 303 response with a Location header. Oversized-request tests also exposed a live upload stream crossing the Worker/DO boundary after an early rejection; requests are now bounded and received at the outer Worker before forwarding.

Live Cloudflare upload, account permissions, DNS/TLS and production performance require an authenticated account. See [validation](../VALIDATION.md) for the performed checks rather than treating a local runtime as hosted evidence.

Code: [Worker entrypoint](../../crates/worker/src/lib.rs), [relay](../../crates/worker/src/relay.rs), [Wrangler](../../wrangler.json), [Terraform](../../infra/main.tf), [deployment](../../scripts/deploy.mjs).

Sources: [Cloudflare Rust](https://developers.cloudflare.com/workers/languages/rust/), [WebSocket hibernation](https://developers.cloudflare.com/durable-objects/best-practices/websockets/), [wasm-bindgen symbol-stripping issue](https://github.com/wasm-bindgen/wasm-bindgen/issues/4905).
