# ADR 006: Keep the agent responsive during OS permission waits

Date: 2026-09-20. Status: accepted. Extends [003](003-execution-boundaries.md).

## Evidence and alternatives

The installed Mac stayed connected but every ChatGPT request timed out. A native stack sample showed `FileTools::walk` blocked in `openat`; macOS TCC logs recorded a pending Downloads-folder consent request for the agent. `run_agent` awaited that operation inside its socket reader, preventing all later commands and heartbeats. A real blocked-stdin integration test reproduced the same head-of-line blocking on the old binary.

Granting Full Disk Access addresses this owner's requested filesystem access but does not prevent slow mounts or other blocked syscalls. Restarting alone loses sessions and leaves the bug intact. Running every operation in a killable subprocess would support hard cancellation but would require a new protocol and search-state ownership. Choose bounded asynchronous dispatch and OS-worker deadlines without new dependencies.

## Decision and tradeoffs

Poll up to eight tool futures independently of the WebSocket reader and heartbeat. Keep accepted futures and recent-result deduplication across reconnects; never create a replacement execution after a lost response. Suppress duplicate IDs that are still in flight. Shutdown remains available even when operation slots are occupied.

File tools and process working-directory resolution each use at most eight blocking slots, with a 20-second caller deadline ahead of the relay's 25-second timeout. Keep a slot until the underlying OS work actually returns, even after the caller times out. Health/configuration calls do not use filesystem slots. Log tool names, outcomes and elapsed time locally without recording arguments or results.

[Tokio cannot cancel a started blocking task](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html). A deadline therefore reports that work may still complete, including writes; clients must inspect state before retrying. After managed process cleanup, runtime shutdown waits at most two seconds for blocking work. macOS consent remains controlled by the OS; the application does not bypass it.

Independent requests may complete out of order. Clients must await a write before issuing a dependent read or another edit. Eight unresolved OS calls exhaust that pool until permission is resolved or the operator restarts the agent; this bounds resource use while shell and health operations remain available.

## Validation

The blocked-stdin regression fails against the original binary. It checks prompt health/configuration responses and a second successful shell launch while input is blocked, then verifies recovery and cleanup. A native test holds an OS-worker slot past its deadline, verifies retries do not start additional work, releases it and confirms recovery. Full integration and installed-agent evidence are recorded in [validation](../VALIDATION.md).

Code: [connection loop](../../crates/agent/src/main.rs), [bounded OS calls](../../crates/agent/src/blocking.rs), [process startup](../../crates/agent/src/processes.rs), [integration regression](../../tests/agent-responsiveness.test.mjs).
