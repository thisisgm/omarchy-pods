---
type: reference
title: The pods status file, key by key
description: The exact wire format the panel parses, where the daemon publishes it, and the keys that vanish entirely rather than going false
tags: [pods, ipc, schema]
status: stable
verified:
  - by: read from a running daemon against a connected AirPods Pro 3, then read back against the publish and quit paths in linux/main.cpp
    at: 2026-08-16
---

# Where it comes from

The daemon writes the whole status object, one line of compact JSON, to
`$XDG_STATE_HOME/pods/status.json` through a `QSaveFile`, so a reader
never sees a half-written file. Three properties of that path are load-bearing
for the panel:

- **It writes only on change.** The publish path compares the rendered line
  against the last one and returns early when they match, so a control verb the
  pods ignored produces no write at all.
- **It removes the file on quit**, from `aboutToQuit`, and the daemon's own
  comment says so: "An absent state file is how a watcher learns the daemon
  stopped." Measured: `systemctl --user stop pods` removes it.
- **It is created late.** The shell can start before the daemon; a `FileView`
  with `watchChanges: true` picks the file up when it appears, measured at
  under 6s, which is why the panel needs no startup ramp.

`pods-ctl status` prints the same object over the control socket. The panel
does not use it, and runs `pods-ctl` only for the control verbs below.

# The keys the panel reads

| Key | Type | Meaning |
|---|---|---|
| `schema_version` | int | currently 1, gates incompatible bumps |
| `connected` | bool | the L2CAP audio link, not whether the daemon is up |
| `device_name` | string | the BlueZ alias |
| `noise_mode` | int | 0 Off, 1 Noise Cancellation, 2 Transparency, 3 Adaptive, -1 unknown |
| `left`, `right` | object | `{available, level, charging, in_ear}` |
| `case` | object | `{available, level, charging}`, no `in_ear` |
| `conversational_awareness` | bool | Pro only |
| `adaptive_noise_level` | int | 0-100, only meaningful while `noise_mode` is 3 |
| `one_bud_anc_mode` | bool | Pro only |
| `model_name` | string | marketing name, empty until the device is identified |
| `is_pro_series` | bool | the Pro silhouette in the bar, and the panel's fallback for `supports_adaptive`, `supports_conversational_awareness` and `supports_one_bud_anc` when those are absent |
| `supports_noise_control` | bool | false on AirPods 1, 2, 3 and the plain 4, which have no modes at all; absent reads as true, not as `is_pro_series` |
| `supports_adaptive` | bool | H2 parts with ANC: AirPods 4 (ANC), Pro 2, Pro 3, Max 2 |
| `supports_conversational_awareness` | bool | same four |
| `supports_one_bud_anc` | bool | noise control and a second bud, so never on a Max |
| `ear_detection_behavior` | int | 0 pause when one is out, 1 when both are out, 2 never |
| `lid_state` | int | 0 open, 1 closed, 2 unknown |

The line arrives with **keys sorted alphabetically**, not in the daemon's insert
order, because `QJsonObject` sorts. Anything reading the line positionally, or a
sample-input comment written from the insert calls, will be wrong.

Thirteen `*_total` counters also appear, along with `model_int` and
`model_number`. They are daemon telemetry and identity, not panel data, and
nothing in the plugin reads them.

# Capability keys are additive, and absence is not false

The four `supports_*` capability keys landed on 2026-08-20 without moving
`schema_version`, the same way `is_headset` and `supports_noise_off` did before
them: a panel that reads none of them keeps working, and the version gate exists
for shape changes that would break a reader, not for new keys.

That makes absence meaningful. `Model.parseStatus` falls back to `is_pro_series`
for adaptive, Conversation Awareness and One-Bud ANC, and to true for
`supports_noise_control`, which is exactly the behaviour the panel had before,
wrong answers included: an AirPods Pro 1 is offered Adaptive and Conversation
Awareness it does not have, and an AirPods 4 with ANC is denied both. A parser
that treats a missing key as false instead would strip Adaptive from a Pro 2 on
any older daemon, which is the worse trade.

Not everything here was additive. Four AirPods Pro 3 codes that Apple never
published, `A3066` and `A3334` through `A3336`, were dropped the same day, so a
device reporting one of them now reads `Unknown` rather than Pro 3. `model_name`
for the noise-cancelling AirPods 4 also changed, from `AirPods 4 with ANC` to
`AirPods 4`, because the panel names the family and the capability keys say what
it can do. Apple's own retail name for that unit is longer, and it is not what
this field carries.

# Two shapes that bite

**`left`, `right` and `case` are absent entirely** until a battery packet has
arrived, rather than present with `available: false`. A parser that assumes the
keys exist reads `undefined` on a fresh daemon. `Model.parseStatus` returns a
complete default shape on every path for this reason.

**`connected` false does not mean nothing is known.** Battery keeps arriving
over the BLE advertisement while the audio link is down, which is exactly the
in-case state where the user wants to see it. The panel therefore gates the
battery section on any known level and the control sections on `connected`.

# Control verbs

`noise:off`, `noise:anc`, `noise:transparency`, `noise:adaptive`,
`ear:one`, `ear:both`, `ear:off`, `ca:on`, `ca:off`, `onebud:on`,
`onebud:off`, `adaptive:N` for N in 0-100.

The daemon also offers `connect`, `disconnect` and `forget`, which shell out to
`bluetoothctl`. The panel does not use them: `omarchy bluetooth device` and the
stock Bluetooth panel already own that job.
