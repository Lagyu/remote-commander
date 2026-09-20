# ADR 003: File capabilities, bounded sessions and uncertain delivery

Date: 2026-09-20. Status: accepted.

## Alternatives and decision

String-prefix path checks are inadequate when symlinks or concurrent filesystem changes affect resolution. Use an opened `cap_std` directory capability for file operations, reject absolute paths/parent components, and require a local flag for changes. Atomic writes use a new private temporary file in the held destination directory and a rename; exact-text edits verify the expected occurrence count. File moves use a no-overwrite hard-link/unlink operation, accepting same-filesystem regular-file scope.

A full shell sandbox would require a platform-specific OS boundary. Instead, shell access is explicitly off by default and documented as the OS user's authority. Track only agent-created process groups, impose hard lifetimes, bound retained output, and clean up on cancellation and shutdown. Piped stdin/stdout/stderr satisfy the tested interaction workflow; PTYs and arbitrary system-process control are outside this release.

Persistently queued or automatically retried commands could duplicate writes or process launches when a reply is lost. Dispatch directly over the authenticated connection and never replay on reconnect. Bound response waits to 25 seconds and return an explicit unknown-execution-state error after an ambiguous loss. A small agent-side cache suppresses recent repeated request IDs during the same process lifetime; it is not an exactly-once guarantee or durable journal.

## Evidence and limits

Tests use actual files and shell processes to verify traversal/symlink rejection, non-overwriting moves, replacement preconditions, read-only flags, input delivery, ring-buffer truncation, deadlines, process termination and descendant cleanup on revocation. A FIFO regression test verifies that special-file reads do not block indefinitely. Relay tests use two authenticated device sockets to prove a foreign device cannot resolve a pending command, then intentionally drop or withhold replies.

File reads/writes have explicit size limits; search snapshots are bounded and ephemeral. Atomic replacement creates mode 0600 and does not preserve ACLs or extended attributes. External writers can still race an edit between its read and replacement. A malicious command can daemonize outside the tracked process group. Existing hard links and local OS permissions remain part of the trust model. These are documented constraints, not tested isolation guarantees.

Code: [file operations](../../crates/agent/src/files.rs), [process lifecycle](../../crates/agent/src/processes.rs), [native connection](../../crates/agent/src/main.rs), [relay](../../crates/worker/src/relay.rs), [security](../SECURITY.md).
