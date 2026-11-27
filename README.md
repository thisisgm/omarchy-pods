<h1 align="center">AirPods for Omarchy</h1>

<p align="center">
  <strong>Check battery and change listening modes from the Omarchy bar. Use the AirPods microphone for dictation and calls through the librepods daemon.</strong>
</p>

<p align="center">
  <a href="https://github.com/tcballard/omarchy-badges"><img src="https://raw.githubusercontent.com/tcballard/omarchy-badges/85f859029e236e784e7b05ada6dbe73506d07a91/badges/v1/built-for-omarchy.svg" alt="Built for Omarchy"></a>
  <a href="https://omarchyplugins.com/plugin.html?id=io.github.thisisgm.omapods"><img alt="On omarchyplugins.com" src="https://img.shields.io/badge/omarchyplugins.com-listed-8b5cf6"></a>
  <a href="https://github.com/thisisgm/omarchy-pods/tags"><img alt="Latest tag" src="https://img.shields.io/github/v/tag/thisisgm/omarchy-pods?label=version"></a>
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#panel">Panel</a> ·
  <a href="#microphone">Microphone</a> ·
  <a href="#keyboard">Keyboard</a> ·
  <a href="https://github.com/thisisgm/omarchy-pods/releases">Releases</a>
</p>

<p align="center">
  <img src="preview.png" alt="The AirPods panel open in the Omarchy bar" width="420">
</p>

## Install

Use Omarchy with BlueZ, PipeWire and its PulseAudio compatibility service.
Pair your AirPods in Omarchy's Bluetooth panel. Run setup as your desktop user.

```bash
omarchy plugin add https://github.com/thisisgm/omarchy-pods --enable
```

The widget goes on the right side of the bar. Then install the daemon.

```bash
~/.config/omarchy/plugins/io.github.thisisgm.omapods/packaging/setup
```

On x86-64, setup downloads the release binary named in
`packaging/librepods.sha256` and refuses it unless its checksum matches. To
build the daemon on this machine instead, install the Rust toolchain and pass
`--from-source`.

```bash
omarchy install dev-env rust
~/.config/omarchy/plugins/io.github.thisisgm.omapods/packaging/setup --from-source
```

Setup installs `librepods` in `~/.local/bin` and links `librepods-ctl` to it,
so commands written for the C++ daemon still work. It enables and starts the
`librepods.service` user unit for the graphical session. Use the same setup
command to update the daemon after updating the plugin with
`omarchy plugin update io.github.thisisgm.omapods`. If the service fails to
start, check its log.

```bash
journalctl --user -u librepods.service -n 30
```

## Panel

- See battery and charging state for each pod and the case, and which pods
  are in your ears. AirPods Max show one battery row.
- Choose the listening modes your model supports. The noise level slider
  appears in Adaptive mode.
- Toggle Conversation Awareness or One-Bud ANC on models that support them.
- Set ear detection to pause playback when one pod comes out, when both
  come out, or never.

Left click the bar icon to toggle the panel. Right click to cycle the available
listening modes. Use Omarchy's Audio panel for volume and device selection.
Use its Bluetooth panel to connect, disconnect or forget your AirPods.

The plugin watches `${XDG_STATE_HOME:-$HOME/.local/state}/librepods/status.json`.
A missing file shows as `librepods is not running` when the panel is visible.

### Case battery while away

Connect your AirPods once so the daemon can save the keys it needs to read their
Bluetooth advertisements. When the AirPods disconnect, the daemon uses BlueZ's
advertisement monitor to read battery, lid and in-ear state.

If setup reports that the monitor is unavailable, set `Experimental = true`
under `[General]` in `/etc/bluetooth/main.conf` as root. Restart Bluetooth or
reboot. Restarting Bluetooth disconnects all Bluetooth devices.

The daemon pauses the monitor while its control link is up. Battery can still
update over that link. The lid indicator keeps its last reading until the
monitor hears another advertisement.

### Microphone

Connect your AirPods, put in a bud and select the AirPods microphone as the
input in your app or Omarchy's Audio panel. The daemon exposes it in PipeWire
once it confirms microphone support. It tries to make it the default input
once per connection.

The daemon uses Apple's DoAP dictation service for Superwhisper capture and
history when the service is ready. It uses the AirPods 0x58 uplink for calls and
other recognized recording streams, or when DoAP is unavailable or fails.
Both paths let playback stay on A2DP. If you select a Bluetooth headset profile
(HFP), the daemon withdraws its microphone input.

The microphone setting persists across restarts. Run `librepods set microphone`
with one of these values:

- `auto` (default) starts capture as needed.
- `always` keeps the uplink ready while you wear a bud, even before an app records.
- `off` removes the daemon's microphone input.

### Keyboard

| Key | Action |
|-----|--------|
| `j` / `k`, `↓` / `↑` | Move between rows |
| `Enter` / `Space` | Activate the focused button or toggle |
| `h` / `l` or `←` / `→` | Adjust the focused adaptive noise level in steps of 5 |
| `o` | Off |
| `t` | Transparency |
| `a` | Adaptive |
| `n` | Noise Cancellation |
| `c` | Toggle Conversation Awareness |
| `b` | Toggle One-Bud ANC |
| `e` | Cycle ear detection |
| `r` | Refresh from the state file |
| `Tab` | Move to the next panel |
| `Esc` | Close |

The first navigation key focuses the current row. Shortcuts for unsupported
listening modes and toggles have no effect.

### Settings

| Setting | Default | Effect |
|---------|---------|--------|
| `hideWhenDisconnected` | `true` | Hide the widget when the AirPods are disconnected and no battery reading is available. |
| `ctlPath` | empty | Path to `librepods-ctl`, used only with the C++ daemon. An empty value uses `PATH`. The panel ignores it with the Rust daemon. |

To keep the icon visible, including when the daemon is down:

```bash
omarchy bar set io.github.thisisgm.omapods hideWhenDisconnected false --json
```

## Screenshots

These panels use sample status data.

| | | |
|:---:|:---:|:---:|
| <img src="docs/panel-model-airpods4.png" alt="AirPods 4 with ANC"><br>AirPods 4 with ANC | <img src="docs/panel-model-pro3.png" alt="AirPods Pro 3"><br>AirPods Pro 3 | <img src="docs/panel-model-max2.png" alt="AirPods Max 2"><br>AirPods Max 2 |

| | |
|:---:|:---:|
| <img src="docs/panel-noise-cancellation.png" alt="Noise Cancellation"><br>Noise Cancellation | <img src="docs/panel-adaptive.png" alt="Adaptive"><br>Adaptive, with Conversation Awareness on |
| <img src="docs/panel-transparency.png" alt="Transparency"><br>Transparency | <img src="docs/panel-one-bud.png" alt="One pod in the case"><br>One pod in the case, One-Bud ANC on |
| <img src="docs/panel-in-case.png" alt="Both pods in the case"><br>Both pods charging, lid closed | <img src="docs/panel-daemon-down.png" alt="Daemon not running"><br>librepods stopped |

## Remove

Run the daemon uninstaller before removing the plugin. The uninstaller stops
the service and removes the installed daemon files and its state directory,
including saved keys and settings. Add `--keep-keys` to the first command to
keep the keys and settings for a later reinstall.

```bash
~/.config/omarchy/plugins/io.github.thisisgm.omapods/packaging/setup --uninstall
omarchy plugin remove io.github.thisisgm.omapods
```

The uninstaller leaves `~/.config/AirPodsTrayApp` in place. Plugin version
1.3.6 and earlier kept the C++ daemon's pairing keys and settings there, and
the Rust daemon does not read those keys. Once you no longer run the C++
daemon, remove it if it exists.

```bash
rm -r ~/.config/AirPodsTrayApp
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for builds and tests. To test the panel's
status parsing and command formatting, run:

```bash
deno run --allow-read tests/model.test.js
```

## Credits

This project builds on [librepods](https://github.com/kavishdevar/librepods)
by Kavish Devar, who reverse-engineered the AirPods control protocol and
Bluetooth advertisements. See [daemon/UPSTREAM.md](daemon/UPSTREAM.md) for
the original daemon's source and changes.

## Support

If this saved you an afternoon, you can
[sponsor me on GitHub](https://github.com/sponsors/thisisgm) or
[buy me a coffee](https://buymeacoffee.com/thisisgm).

<p>
  <a href="https://buymeacoffee.com/thisisgm"><img src="https://img.shields.io/badge/Buy_me_a_coffee-FFDD00?style=flat-square&logo=buymeacoffee&logoColor=black" height="20" alt="Buy me a coffee"></a>
  <a href="https://www.greptile.com/?utm_source=oss_badge&utm_medium=readme&utm_campaign=greptile_for_open_source"><img src="https://www.greptile.com/badge.svg" height="20" alt="Greptile: The War on Bugs"></a>
</p>

## Licence

- Omarchy plugin in the repository root, [MIT](LICENSE).
- Legacy C++ librepods fork in `daemon/`, [GPL-3.0](daemon/LICENSE).
- Rust daemon in `crates/`, [GPL-3.0-only](crates/LICENSE).

The plugin and daemon are separate programs. They communicate through a state
file and the command line. The MIT licence excludes the Apple product outlines
in `AirPodsIcon.qml`, sourced from [Apple's AirPods site](https://www.apple.com/airpods/).
AirPods, AirPods Pro and AirPods Max are trademarks of Apple Inc.
Apple does not sponsor or endorse this plugin.
