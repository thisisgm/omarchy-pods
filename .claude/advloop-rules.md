# Review rules for omapods

An Omarchy Quattro shell plugin. QML running inside the Quickshell process that
draws the bar. Judge it against these, not against generic best practice.

## Platform facts, so a finding that ignores one is false

- **One box, one user, one desktop session.** This is a Linux desktop plugin,
  not a service. Code for the shape that exists.
- **`qs.Ui` and `qs.Commons` are the shell's shared component library**, shipped
  at `/usr/share/omarchy/shell/`. `Panel`, `BarIconButton`, `KeyboardPanel`,
  `PanelKeyCatcher`, `PanelHero`, `PanelSectionHeader`, `PanelSeparator`,
  `CursorSurface`, `ToggleSwitch`, `PanelSlider`, `Style` and `Color` all come
  from there. They are not missing imports and not this plugin's to change.
- **The plugin is strictly a display.** `librepods-ctl` is the only thing that
  touches the outside world. Data acquisition belongs in that daemon, never in
  QML.
- **The daemon publishes one line of JSON to
  `$XDG_STATE_HOME/librepods/status.json` on change and removes it on quit**,
  and the panel watches that file rather than polling. Keys arrive sorted
  alphabetically because `QJsonObject` sorts. The C++ daemon leaves
  the `left`, `right` and `case` objects **absent entirely** until a battery
  packet arrives. The Rust daemon writes every key from the start, each
  battery object with `available: false` until it has a reading.
- **`connected` means the L2CAP audio link, not the daemon.** Battery keeps
  arriving over BLE while `connected` is false, which is the pods-in-case case.
- **The cursor ring appearing on panel open comes from the shared
  `PanelKeyCatcher`**, and reproduces on the stock Bluetooth panel. It is not
  this plugin's defect.
- **Volume, output device, connect, disconnect and forget are deliberately
  absent.** The stock `omarchy.audio` and `omarchy.bluetooth` panels own them,
  and `Tab` walks between panels. Duplicating them here is the defect.
- **Keyboard behaviour cannot be driven headlessly.** `wtype` does not reach a
  layer-shell surface and `ydotool` is not installed.

## Binding rules

- **Could this be simpler?** Reuse what Omarchy ships before writing anything.
  Generality nobody asked for, states the platform cannot be in, knobs with a
  single caller and modes nobody runs are defects, not thoroughness.
- **A finding about a case the platform cannot produce is answered with the
  platform fact, not with more code.**
- **One line per comment.** Two stacked comment lines above the same thing is a
  paragraph. The carve-out is upstream QML house style: two or three lines are
  allowed for a non-obvious timing or race constraint, and the OEM plugins use
  that.
- **Name every magic number**, and put a sample-input comment directly above
  every parser showing the exact format it consumes.
- **Errors are elided to a sentence, never dumped.** A parse failure returns a
  full default shape rather than throwing.
- **Nothing is polled.** The panel watches the daemon's state file, so there is
  no interval setting and no clamp to re-apply; asking for one is asking for a
  timer with nothing to do.
- **Optimistic state on click, corrected when the daemon disagrees**, and the
  hold must be bounded so a daemon that never agrees cannot freeze a control.
  Because the daemon writes only on change, every path that drops the hold has
  to re-read as well, or nothing will fire to correct the display.
- **Self-hide when there is nothing to say** rather than sitting in the bar
  empty.
- Human readable and human troubleshootable outranks clever and outranks
  minimal. Boring code survives 3am pages.

## Known QML hazards worth checking for

- A `Process` whose `running` is a **binding** re-evaluates on dependency
  change, never on assignment of the same value. Writing a binding dependency
  inside `onExited` respawns the child immediately. This plugin sets `running`
  imperatively for that reason.
- A property that shadows a final member of `QQuickItem` (`left`, `right`,
  `top`, `bottom`) breaks anchoring. Already caught once here.
