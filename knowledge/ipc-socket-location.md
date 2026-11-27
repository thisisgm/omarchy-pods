---
type: reference
title: The daemon control socket lives under XDG_RUNTIME_DIR
description: Why the socket moved off /tmp and how the shared Rust client and daemon resolve it
tags: [pods, ipc, security]
status: stable
verified:
  - by: reading Qt's QLocalServer path handling, then building both binaries against the shared header
    at: 2026-08-16
---

# What changed

The daemon used to call `server.listen("app_server")`, which Qt resolves
against the temp dir, so the control channel sat at a predictable
world-visible `/tmp/app_server`. It now binds
`$XDG_RUNTIME_DIR/pods.sock`, which on a systemd session is
`/run/user/<uid>`, mode 0700.

`QLocalServer` and `QLocalSocket` treat a name beginning with `/` as a full
filesystem path rather than a basename under the temp dir. That single rule is
what makes the move a path change instead of a protocol change.

# Shared path resolution

The Rust daemon and client both call `socket_path` in
[`crates/daemon/src/ipc_path.rs`](../crates/daemon/src/ipc_path.rs). If
`XDG_RUNTIME_DIR` is unset or empty, the command exits with a diagnostic.
The unit tests pin the path calculation and the integration tests run the real
client and daemon against temporary runtime directories.
