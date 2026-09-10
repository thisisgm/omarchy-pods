---
type: index
title: Platform facts behind omapods
description: Machine-readable index of the measured facts this plugin depends on
tags: [omarchy, airpods, librepods, quickshell]
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
| [librepods-status-schema](librepods-status-schema.md) | the wire format the panel parses, and the two shapes that bite |
| [librepods-build-requirements](librepods-build-requirements.md) | why the packaged librepods cannot drive this panel |
| [ipc-socket-location](ipc-socket-location.md) | why the control socket left /tmp, and the Qt rule that made it a one-line move |
| [plugin-design-decisions](plugin-design-decisions.md) | what this panel owns against the stock audio and Bluetooth panels |
| [airpods-pro-3-off-mode](airpods-pro-3-off-mode.md) | the Pro 3 gained Off on current firmware, and why the mode list still comes from the daemon |
| [nerd-font-glyph-coverage](nerd-font-glyph-coverage.md) | why the bar mark is drawn: fontconfig claims glyphs the font cannot draw |
