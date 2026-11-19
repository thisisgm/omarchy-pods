---
type: log
title: Knowledge bundle changelog
description: What was added or corrected in this bundle, and when
tags: [omapods]
---

## 2026-08-16

Bundle created alongside the first working panel.

- `bluez-has-no-airpods-battery`: measured with `busctl introspect` against a
  connected AirPods Pro 3. Closed the question of whether a stock mechanism
  could supply battery.
- `airpods-pro-3-identity`: `bluetoothctl info` and `pactl list short sinks`
  taken while the pods were connected.
- `librepods-status-schema`: read out of the daemon's `status` branch. The
  absent-keys behaviour and the BLE-versus-L2CAP split were found by reading
  the insert calls rather than by observing a live daemon, which is noted here
  because the daemon had not been built when the bundle was written.
- `librepods-build-requirements`: upstream and AUR checked directly rather
  than assumed.
- `ipc-socket-location`: written with the change that moved the socket.
- `librepods-status-schema`: corrected once the daemon was actually built and
  polled: the wire order is alphabetical, not the insert order the file had been
  written from. The first version of that fact was read out of source and was
  wrong about it.
- `plugin-design-decisions`: later the same day, recorded why there is no mic
  section and no Spatial Audio row, both asked for against the macOS Sound menu
  and both rejected on measured grounds rather than taste.
- `airpods-pro-3-has-no-off-mode`: found by driving the write path with the
  pods in ear, which also cleared up the earlier appearance that control was
  broken entirely (the pods had been in the case).
- `nerd-font-glyph-coverage`: added after trying to replace the drawn mark with
  nf-md-earbuds. fontconfig reported the codepoint as covered and it rendered as
  junk in the live bar, so the drawn mark stayed and nf-md-check replaced the
  drawn selection dot.
- `plugin-design-decisions`: poll cadence timed through a logging stub; the
  cursor-ring behaviour attributed to the shared panel chrome after
  reproducing it on the stock Bluetooth panel.

## 2026-08-16, after the adversarial review

Three cold reviewers read the whole plugin. Most of what they found was
documentation left behind by the move from polling `librepods-ctl status` to
watching the daemon's state file, so the corrections are recorded here rather
than quietly rewritten.

- `plugin-design-decisions`: the "Poll cadence, measured" section described a
  `refreshIntervalSec` setting, a 1s open cadence and poll counts, none of which
  survived the move to a `FileView`. Replaced with what was actually measured,
  including the startup case.
- `librepods-status-schema`: said the panel execs `librepods-ctl status` once
  per poll. It does not. Rewritten around the state file, and the three
  properties of the daemon's publish path the panel depends on were read out of
  `linux/main.cpp` and measured: write only on change, remove on quit, and
  created late.
- `plugin-design-decisions`: claimed the selected mode is a drawn dot, and that
  the optimistic hold is six settle ticks. Both were true of an earlier build.
- `log.md`: em dashes replaced throughout, per the project prose rule.
- Two claims that looked like defects and were not, both settled on the box: the
  daemon does remove `status.json` when systemd stops it, and a `FileView` does
  pick up a file created after the shell started.

## 2026-09-10

- `airpods-pro-3-has-no-off-mode` renamed to `airpods-pro-3-off-mode` and
  rewritten. A Pro 3 on current firmware accepts the Off packet and reports
  `noise_mode` 0 back, reversing the 2026-08-16 reading. The model rule in
  `supportsNoiseOff` that hid the row is gone, and `tst_enums` pins the new
  behaviour.
