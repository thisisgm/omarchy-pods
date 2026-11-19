---
type: reference
title: AirPods Pro 3 do have an Off listening mode on current firmware
description: The Off packet applies and the pods report noise_mode 0 back, reversing the August reading, so the model rule that hid the row is gone
tags: [airpods, aap, listening-mode]
status: stable
verified:
  - by: three consecutive noise:off attempts against a real AirPods Pro 3 with both pods in ear, with noise:anc and noise:transparency as controls
    at: 2026-09-10
---

# The measurement

With both pods in ear and the daemon holding the AAP link:

```
start mode: 3
after noise:off attempt 1 -> mode 0
after noise:off attempt 2 -> mode 0
after noise:off attempt 3 -> mode 0
control: after noise:anc -> mode 1
control: after noise:transparency -> mode 2
```

Off then held across four reads over twelve seconds rather than snapping back.

`noise_mode` is only ever written from an inbound packet in `main.cpp`, and
`DeviceInfo` defaults to `Transparency` rather than `Off`, so a 0 can only have
come from the pods.

# What this reverses

An earlier measurement on 2026-08-16 sent the same three attempts to a Pro 3
and the mode did not move, which is why `supportsNoiseOff` carried a Pro 3
exception and the panel drew three rows. The firmware appears to have gained
the mode since. That reading is recorded here rather than deleted, because a
Pro 3 on older firmware will still behave the way it describes.

# A second, separate rejection

Every listening-mode change is ignored while the pods are out of the ears,
whatever the mode. That has not changed, and it is still the first thing to
check when a mode will not apply.

# What the code does

`supportsNoiseOff` returns true for every model. The daemon still exports
`supports_noise_off` in the status line and the panel still builds both its row
list and its right-click cycle from `Service.availableModes()`, so a model that
turns out to lack Off can be corrected in the daemon without touching the
display. A daemon too old to send the field is treated as supporting Off.
