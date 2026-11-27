---
type: reference
title: Daemon build requirements
description: The Omarchy plugin uses the daemon shipped by this project
tags: [pods, packaging]
status: stable
verified:
  - by: reading packaging/install.sh and packaging/package.sh and rust-toolchain.toml
    at: 2026-11-27
---

The daemon is built from the Rust workspace at the repository root. The
installer verifies the release bundle against its `MANIFEST.sha256` or builds
the binary with the pinned toolchain when called with `--from-source`.

The daemon uses the system PipeWire library and the BlueZ D-Bus service. See the
[README](../README.md) for installation and the case battery requirements.
