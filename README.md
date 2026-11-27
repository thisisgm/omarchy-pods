# Pods

Pods brings AirPods battery readings, listening controls, ear detection and
high-resolution microphone capture to Linux. It captures microphone audio
alongside A2DP playback and leaves the playback profile under PipeWire and
WirePlumber control.

## Install on Linux

Run this from a terminal as your regular user.

```sh
curl -fsSL https://github.com/superultrainc/pods/releases/latest/download/install.sh | sh
```

The installer selects the x86-64 or ARM64 bundle, verifies its SHA-256 checksum,
installs the PipeWire and BlueZ dependencies, and starts the `podsd.service`
systemd user unit. Pass `sh -s -- --version 1.0.0` to select a release, or
`sh -s -- --version 1.0.0 --dry-run` to see its download URLs.

Supported distributions are Omarchy, Arch, CachyOS, Ubuntu 24.04 or later,
Linux Mint 22 or later, Debian 13 (trixie) or later, Fedora 40 or later,
RHEL, AlmaLinux, Rocky Linux and CentOS Stream 10, and openSUSE Tumbleweed.
On NixOS, use the flake below.

### Manual or source installation

Download the archive for your architecture and its matching `.sha256` from
[GitHub Releases](https://github.com/superultrainc/pods/releases), then run the
bundled installer from your terminal. For x86-64 and version 1.0.0:

```sh
sha256sum --check pods-linux-x86_64-1.0.0.tar.zst.sha256 &&
  tar --zstd -xf pods-linux-x86_64-1.0.0.tar.zst &&
  cd pods-linux-x86_64-1.0.0 &&
  ./install.sh
```

From a checkout, run `./setup --from-source` to build with the pinned Rust
toolchain, or `./setup --binary /path/to/pods` to use a matching build.

Run `./uninstall.sh` from a bundle or `./setup --uninstall` from the checkout.
Pairing keys and state are kept by default. Add `--purge` to delete the state
directory when it resolves inside your home.

### NixOS and Home Manager

Both `x86_64-linux` and `aarch64-linux` are supported. Try the package with
`nix run github:superultrainc/pods -- --version`. The flake also provides an
overlay with `pkgs.pods`.

Add the flake input and import its Home Manager module into your user
configuration.

```nix
# flake.nix inputs
inputs.pods.url = "github:superultrainc/pods";

# Home Manager configuration with inputs passed as a special argument
imports = [ inputs.pods.homeManagerModules.default ];
services.pods.enable = true;
```

The module installs both CLI names and the hardened user service. Enable BlueZ
and PipeWire with PulseAudio compatibility in the system configuration.

## Desktop controls

### Omarchy

The installer registers the `superwhisper.pods` widget on Omarchy. The panel
displays battery levels for each pod and the case and exposes the listening
modes the connected model supports, Conversation Awareness, One-Bud ANC and
ear detection. Press `j`/`k` or the arrow keys to move, Enter or Space to activate,
and Esc to close. The widget hides when the AirPods disconnect. To keep it visible:

```sh
omarchy bar set superwhisper.pods hideWhenDisconnected false --json
```

![The AirPods panel in the Omarchy bar](preview.png)

### KDE Plasma and Waybar

Pods provides a built-in D-Bus tray item and menu. It appears on KDE Plasma
and in the Waybar tray on Hyprland and Sway when AirPods are connected or
reporting battery levels nearby. Set `PODS_TRAY=0` or `PODS_TRAY=1` in the
daemon's environment to disable or force the tray. The daemon normally
suppresses it when it finds the Omarchy widget or the GNOME extension on GNOME.

### GNOME Shell

The installer links the `pods@superwhisper.com` extension on GNOME Shell 46
through 51. It shows battery levels and listening controls in the top bar.
Log out and back in once so GNOME Shell discovers the extension. If needed,
open GNOME Extensions or run `gnome-extensions enable pods@superwhisper.com`.

For Home Manager, set `services.pods.gnomeExtension.enable = true;` alongside
`services.pods.enable`, then enable the extension in GNOME after logging in.

## Superwhisper Linux integration

Pods exposes the high-resolution AirPods input as a PipeWire source on demand.
Superwhisper Linux identifies Pods devices by the `pods_input.` node prefix and
the `pods.device.id` card property. See the [D-Bus API](docs/dbus-api.md) for
dictation methods and source discovery.

## CLI and D-Bus

```sh
pods status
pods watch
pods set noise anc
pods-ctl status
busctl --user introspect com.superwhisper.Pods /com/superwhisper/Pods com.superwhisper.Pods1
```

The session bus name is `com.superwhisper.Pods`, the object path is
`/com/superwhisper/Pods`, and the interface is `com.superwhisper.Pods1`.
Errors start with `com.superwhisper.Pods1.Error.`. See the [D-Bus API](docs/dbus-api.md)
and the [local socket protocol](docs/protocol-1.md) for integration details.

## BlueZ experimental features

Set `Experimental = true` under `[General]` in `/etc/bluetooth/main.conf` to let
BlueZ expose the advertisement monitor. This allows Pods to see case battery,
lid and ear state while the AirPods are away. Restart the Bluetooth service or
reboot to apply the setting.

## Credits and license

Pods credits Kavish Devar and [LibrePods](https://github.com/librepods-org/librepods)
for the AirPods protocol work. Pods source is released under the [MIT License](LICENSE).

The three product outlines in `AirPodsIcon.qml` come from
[Apple's AirPods page](https://www.apple.com/airpods/). Apple, AirPods and
AirPods Max are Apple Inc. trademarks. Apple does not sponsor or endorse Pods.
The MIT license excludes Apple's outlines.
