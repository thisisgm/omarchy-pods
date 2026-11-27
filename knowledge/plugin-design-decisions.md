---
type: reference
title: What this panel owns, and what it deliberately leaves to Omarchy
description: The split against the stock audio and Bluetooth panels, why nothing is polled, and the behaviours that belong to the shared panel chrome rather than to this plugin
tags: [omarchy, quickshell, design]
status: stable
verified:
  - by: measured against the stock panels on the box, and by counting the plugin's spawns and watching a daemon stop and start under it
    at: 2026-08-16
---

# The split

macOS merges audio routing with device control in one Sound menu. Omarchy
separates them, and that separation is the design rather than a compromise.

| Control | Owner |
|---|---|
| Volume, output device list, per-app mixer | stock `omarchy.audio` |
| Connect, disconnect, forget, pairing | stock `omarchy.bluetooth`, `omarchy bluetooth device` |
| Per-pod and case battery | this plugin |
| Listening mode, adaptive level, Conversation Awareness, One-Bud ANC | this plugin |
| Ear detection, case lid | this plugin |
| Spatial Audio | nobody: no renderer on Linux, so no row |
| Mic mode, input mute, input device | stock `omarchy.audio`: not an AirPods control at all |

Panels sit side by side and `Tab` walks between them, so the honest answer to
"I want one menu like macOS" is that the audio panel is one keystroke away.
Reimplementing the sink list here would duplicate something that already
survives `omarchy update`.

# Why there is no mic section

macOS shows Standard and Voice Isolation under a Mic Mode heading whenever an
app holds the microphone. That is macOS DSP applied to the input stream for any
source. No packet in `crates/proto` selects those host-side microphone modes,
so there is nothing to send.

Two real Linux options were considered and rejected. A PipeWire `rnnoise`
filter chain is honest noise suppression but applies to every microphone, so it
belongs to audio configuration rather than to an AirPods panel. A card-profile
row switching A2DP against HFP would be genuinely AirPods-specific, but
PipeWire already switches profiles when an application opens the source, so it
would be a knob nobody turns.

# No polling at all, measured

The panel watches `$XDG_STATE_HOME/pods/status.json` with a `FileView` and
runs no process while idle. An earlier design polled `pods-ctl status` on a
`refreshIntervalSec` manifest setting; the daemon gained a state file instead,
so the setting, its clamp and the timer were all deleted.

Measured on the box: 0 spawns across 20 idle samples, a change visible in under
2s, and an absent file read as a stopped daemon. `FileView` with
`watchChanges: true` also picks up a file **created** after the shell started:
with the daemon stopped and the shell restarted, starting the daemon moved the
panel from `Unknown` to the live mode within 6s. That is why there is no startup
ramp.

# Optimistic state

A click writes the new value locally and records the field as pending, so the
control moves at once. An incoming read cannot overwrite that field until the
daemon reports the same value, which stops a write already in flight from
snapping the control back. The hold is one 4000 ms `settleTimer`, and when it
expires it re-reads the file as well as dropping the hold, because a verb the
pods ignored changes nothing and the daemon dedupes its writes against the last
line, so no watch would ever fire to correct the display.

# Behaviours that are not this plugin's

The cursor ring appears on a row as soon as the panel opens, before any
keypress. That comes from the shared `PanelKeyCatcher`, not from here: the
stock Bluetooth panel does the same thing when opened by IPC with the pointer
parked off-panel. `onOpenedChanged` does reset `cursorActive` and `cursorIndex`;
the catcher then re-establishes a cursor on its own.

Keyboard behaviour cannot be driven headlessly at all. `wtype` does not reach a
layer-shell surface and `ydotool` is not installed, so the key map is verified
by reading it and at the keyboard, and the automated checks drive the plugin's
own IPC verbs instead.

# The mark

The bar icon is drawn from primitives rather than shipped as an SVG, because a
two-stem earbud silhouette loses its stems to rasterisation at bar size. The
selected listening mode is marked with `nf-md-check` U+F012C, which was measured
rendering correctly, unlike the earbuds codepoints. See
[nerd-font-glyph-coverage](nerd-font-glyph-coverage.md).
