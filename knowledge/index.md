---
type: index
title: Platform facts behind pods
description: Machine-readable index of the measured facts this plugin depends on
tags: [omarchy, airpods, pods, quickshell]
---

# Knowledge bundle

An [Open Knowledge Format](https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md)
bundle. One file per fact, each carrying YAML frontmatter with a `type`.

Every fact here was measured on a running machine, Omarchy Quattro on Arch with
a connected AirPods Pro 3, rather than inferred from documentation. Where a
fact was not observable, the file says so in its own words instead of guessing.
`log.md` records what changed and when.

| Fact | Why it matters |
|---|---|
| [bluez-has-no-airpods-battery](bluez-has-no-airpods-battery.md) | the daemon is a requirement, not a convenience: nothing else on the box knows the numbers |
| [airpods-pro-3-identity](airpods-pro-3-identity.md) | the BlueZ identity, the AAP service UUID, and why the generation cannot be read off the name |
| [pods-status-schema](pods-status-schema.md) | the wire format the panel parses, and the two shapes that bite |
| [pods-build-requirements](pods-build-requirements.md) | why the packaged pods cannot drive this panel |
| [ipc-socket-location](ipc-socket-location.md) | why the control socket left /tmp, and the Qt rule that made it a one-line move |
| [plugin-design-decisions](plugin-design-decisions.md) | what this panel owns against the stock audio and Bluetooth panels |
| [airpods-pro-3-has-no-off-mode](airpods-pro-3-has-no-off-mode.md) | why the mode list comes from the daemon and not from a constant |
| [nerd-font-glyph-coverage](nerd-font-glyph-coverage.md) | why the bar mark is drawn: fontconfig claims glyphs the font cannot draw |
