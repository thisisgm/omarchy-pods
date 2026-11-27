---
type: reference
title: AirPods Pro 3 have no Off listening mode
description: The Off packet is accepted and silently ignored, so the panel takes its mode list from the daemon rather than assuming four modes
tags: [airpods, aap, listening-mode]
status: stable
verified:
  - by: three consecutive noise:off attempts against a real AirPods Pro 3 with both pods in ear, with noise:transparency as the control
    at: 2026-08-16
---

# The measurement

With both pods in ear and the daemon running:

```
start mode: 1
after noise:off attempt 1 -> mode 1
after noise:off attempt 2 -> mode 1
after noise:off attempt 3 -> mode 1
control: after noise:transparency -> mode 2
```

Three rejections, then the control applied immediately. `pods-ctl` exits 0
either way and `noise_control_changes_total` counts the attempt, so nothing on
the daemon side reports the failure: the pods simply ignore the packet.

macOS agrees. Its Sound menu on a Pro 3 lists Transparency, Adaptive and Noise
Cancellation, with no Off entry, while a Pro 2 has one.

# A second, separate rejection

Every listening-mode change is ignored while the pods are **out of the ears**,
whatever the mode. That is why an early test appeared to show the whole write
path broken: the pods were in the case. In-ear, every other mode applies within
about two seconds.

# What the code does

The daemon exports `supports_noise_off` in the status line, computed from the
model in `enums.h`, and the panel builds both its row list and its right-click
cycle from `Service.availableModes()`. Device knowledge stays in the daemon,
where it can be corrected for one model without touching the display. A daemon
too old to send the field is treated as supporting Off, which was true of every
model before the Pro 3.
