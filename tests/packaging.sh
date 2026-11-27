#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
fixture_parent=$(realpath -e -- "${TMPDIR:-/tmp}")
fixture=$(mktemp -d "$fixture_parent/pods-packaging.XXXXXX")
touch "$fixture/.pods-packaging-fixture"
server=
cleanup() {
    local result=$?
    if [[ -n $server ]]; then
        kill "$server" 2>/dev/null || true
        wait "$server" 2>/dev/null || true
    fi
    if [[ $fixture == "$fixture_parent"/pods-packaging.* && -f $fixture/.pods-packaging-fixture ]]; then
        if ((result != 0)); then
            for log in "$fixture"/{result,commands,server}.log; do
                if [[ -f $log ]]; then cat -- "$log" >&2; fi
            done
        fi
        rm -rf -- "$fixture"
    fi
}
trap cleanup EXIT
[[ $fixture == /?* && -f $fixture/.pods-packaging-fixture ]]
printf 'Packaging fixture: %s\n' "$fixture"
cd -- "$fixture"
export HOME="$fixture/home"
export XDG_CONFIG_HOME="$HOME/config" XDG_DATA_HOME="$HOME/data" XDG_STATE_HOME="$HOME/state"
export XDG_CACHE_HOME="$HOME/cache" XDG_RUNTIME_DIR="$fixture/runtime" TMPDIR="$fixture/tmp"
export TEST_COMMANDS="$fixture/commands.log"
if [[ ${1:-} == --check-distro ]]; then
    PODS_SKIP_PACKAGES=0 "$root/packaging/dependencies.sh" check
fi
export PODS_SKIP_PACKAGES=1
unset PODS_SKIP_SERVICE INVOCATION_ID
mkdir -p -- "$HOME" "$XDG_RUNTIME_DIR" "$TMPDIR" "$XDG_CACHE_HOME" stubs
export PATH="$fixture/stubs:$PATH"
app="$XDG_DATA_HOME/pods/app"
install="$root/packaging/install.sh"
uninstall="$root/packaging/uninstall.sh"
plugin="$XDG_CONFIG_HOME/omarchy/plugins"
unit="$XDG_CONFIG_HOME/systemd/user/podsd.service"
shadow="$XDG_DATA_HOME/systemd/user/podsd.service"
default_shadow="$HOME/.local/share/systemd/user/podsd.service"
marker='# >>> Pods: pods on PATH >>>'

expect_error() {
    local pattern=$1
    shift
    if "$@" > result.log 2>&1; then
        printf 'Expected failure from %s\n' "$*" >&2
        return 1
    fi
    grep -Fq -- "$pattern" result.log
}

cat > stubs/systemctl <<'STUB'
#!/bin/sh
set -eu
printf '%s\n' "systemctl $*" >> "$TEST_COMMANDS"
[ "$1" = --user ] || exit 3
case "$2" in
    is-active|is-enabled) test -f "$XDG_RUNTIME_DIR/$4.${2#is-}" ;;
    enable) touch "$XDG_RUNTIME_DIR/$3.enabled" ;;
    start|restart)
        readlink "$XDG_DATA_HOME/pods/app/current" >> "$TEST_COMMANDS" || true
        touch "$XDG_RUNTIME_DIR/$3.active"
        ;;
    stop) rm -f -- "$XDG_RUNTIME_DIR/$3.active" ;;
    disable)
        case "$3" in
            --now) rm -f -- "$XDG_RUNTIME_DIR/$4.active" "$XDG_RUNTIME_DIR/$4.enabled" ;;
            *) rm -f -- "$XDG_RUNTIME_DIR/$3.enabled" ;;
        esac
        ;;
    daemon-reload) ;;
    *) exit 4 ;;
esac
STUB
cat > stubs/busctl <<'STUB'
#!/bin/sh
printf 'as 0\n'
STUB
cat > stubs/omarchy <<'STUB'
#!/bin/sh
printf '%s\n' "omarchy $*" >> "$TEST_COMMANDS"
STUB
cat > stubs/omarchy-shell <<'STUB'
#!/bin/sh
printf '%s\n' "omarchy-shell $*" >> "$TEST_COMMANDS"
STUB
chmod 0755 -- stubs/*

write_binary() {
    local path=$1 version=$2 status=$3
    printf '#!/bin/sh\nversion="%s"\nstatus=\x27%s\x27\n' "$version" "$status" > "$path"
    cat >> "$path" <<'STUB'
case "$1" in
    --version) printf 'pods %s\n' "$version" ;;
    state-dir) printf '%s\n' "${PODS_TEST_STATE_DIR:-$XDG_STATE_HOME/pods}" ;;
    status) printf '%s\n' "$status" ;;
    *) exit 6 ;;
esac
STUB
    chmod 0755 -- "$path"
}

version=1.0.0
upgrade=1.1.0
status='{"connected":false,"schema_version":1}'
arch=$(uname -m)
name="pods-linux-$arch-$version"
archive="$name.tar.zst"
write_binary "$fixture/pods-good" "$version" "$status"
write_binary "$fixture/pods-bad" "$upgrade" 'schema_version but no JSON'
write_binary "$fixture/pods-upgrade" "$upgrade" "$status"

printf '%s\n' 'Testing archive reproducibility and bundle checksums'
pack="$root/packaging/package.sh"
"$pack" --binary "$fixture/pods-good" --output "$fixture/fixture-dist" > /dev/null
"$pack" --binary "$fixture/pods-good" --output "$fixture/repeat-dist" > /dev/null
cmp -- "fixture-dist/$archive" "repeat-dist/$archive"
(cd -- fixture-dist && sha256sum -c --strict "$archive.sha256")
mkdir extracted
tar --zstd -xf "fixture-dist/$archive" -C extracted
(cd -- "extracted/$name" && sha256sum -c --strict MANIFEST.sha256 && [[ -x bin/pods ]])
bad_bundle="$fixture/corrupt/$name"
mkdir corrupt
cp -a -- "extracted/$name" "$bad_bundle"
printf 'corrupt\n' > "$bad_bundle/plugin/Panel.qml"
expect_error 'MANIFEST.sha256' "$bad_bundle/install.sh"
rm -- "$bad_bundle/VERSION"
expect_error 'bundle is missing VERSION' "$bad_bundle/install.sh"

printf '%s\n' 'Testing source checkout, legacy migration, and PATH idempotency'
expect_error 'missing executable' "$install" --binary "$fixture/no-such-binary"
expect_error 'requires one path' "$install" --binary
expect_error 'XDG_RUNTIME_DIR must be an absolute directory' env XDG_RUNTIME_DIR=relative "$install" --binary "$fixture/pods-good"
mkdir -p -- "$XDG_CONFIG_HOME/librepods" "$XDG_STATE_HOME/librepods" \
    "$plugin/io.github.thisisgm.omapods" "$(dirname -- "$shadow")" \
    "$(dirname -- "$default_shadow")" "$XDG_DATA_HOME/applications"
printf 'prefs\n' > "$XDG_CONFIG_HOME/librepods/prefs.json"
printf 'keys\n' > "$XDG_STATE_HOME/librepods/keys.json"
printf 'legacy plugin\n' > "$plugin/io.github.thisisgm.omapods/manifest.json"
printf 'old unit\n' > "$(dirname -- "$shadow")/librepods.service"
printf 'shadow unit\n' > "$shadow"
printf 'default shadow unit\n' > "$default_shadow"
printf 'custom desktop\n' > "$XDG_DATA_HOME/applications/me.kavishdevar.librepods.desktop"
printf 'export KEEP=1\n' > "$HOME/.bashrc"
printf 'read-only zshrc\n' > "$HOME/.zshrc"
chmod 0444 -- "$HOME/.zshrc"
"$root/setup" --binary "$fixture/pods-good" > result.log 2>&1
[[ $(readlink -- "$app/current") == "$version" ]]
[[ $(readlink -- "$HOME/.local/bin/pods") == "$app/current/bin/pods" ]]
[[ $(readlink -- "$HOME/.local/bin/pods-ctl") == pods ]]
[[ $("$HOME/.local/bin/pods" --version) == "pods $version" ]]
[[ -f $unit && ! -e $shadow && ! -e $default_shadow && ! -e $(dirname -- "$shadow")/librepods.service ]]
[[ -f $XDG_STATE_HOME/pods/keys.json && -f $XDG_CONFIG_HOME/pods/prefs.json ]]
[[ ! -e $XDG_STATE_HOME/librepods && ! -e $XDG_CONFIG_HOME/librepods ]]
[[ $(< "$XDG_DATA_HOME/applications/me.kavishdevar.librepods.desktop") == 'custom desktop' ]]
[[ $(readlink -- "$plugin/superwhisper.pods") == "$app/current/plugin" ]]
[[ -f $plugin/superwhisper.pods/Panel.qml && ! -e $plugin/io.github.thisisgm.omapods ]]
grep -Fqx 'legacy plugin' "$XDG_CONFIG_HOME"/omarchy/pods-legacy.*/io.github.thisisgm.omapods/manifest.json
grep -Fqx 'systemctl --user disable --now librepods.service' "$TEST_COMMANDS"
grep -Fqx 'omarchy plugin disable io.github.thisisgm.omapods' "$TEST_COMMANDS"
grep -Fqx 'omarchy-shell -q shell rescanPlugins' "$TEST_COMMANDS"
grep -Fq 'BlueZ experimental monitoring is off' result.log
[[ $(< "$HOME/.zshrc") == 'read-only zshrc' ]]
chmod 0644 -- "$HOME/.zshrc"
"$install" --binary "$fixture/pods-good"
[[ $(grep -Fxc -- "$marker" "$HOME/.bashrc") == 1 ]]
[[ $(grep -Fxc -- "$marker" "$HOME/.zshrc") == 1 ]]
[[ $(grep -Fxc 'omarchy plugin enable superwhisper.pods' "$TEST_COMMANDS") == 1 ]]
grep -Fqx -- "$marker" "$XDG_CONFIG_HOME/environment.d/60-pods-path.conf"
grep -Fqx -- "$marker" "$XDG_CONFIG_HOME/fish/conf.d/pods.fish"

printf '%s\n' 'Testing upgrade and rollback after a failed health check'
failed=$("$pack" --version "$upgrade" --binary "$fixture/pods-bad" --output "$fixture/failed-dist" --tree-only)
expect_error 'failed health check' "$failed/install.sh"
[[ $(readlink -- "$app/current") == "$version" && ! -e $app/$upgrade ]]
[[ ! -L $app/previous && $("$HOME/.local/bin/pods" --version) == "pods $version" ]]
[[ $(tail -n 1 "$TEST_COMMANDS") == "$version" ]]
good=$("$pack" --version "$upgrade" --binary "$fixture/pods-upgrade" --output "$fixture/upgrade-dist" --tree-only)
"$good/install.sh"
[[ $(readlink -- "$app/current") == "$upgrade" && $(readlink -- "$app/previous") == "$version" ]]

printf '%s\n' 'Testing uninstall ownership, state retention, and purge boundaries'
mkdir -p -- outside
printf 'outside\n' > outside/keys.json
expect_error 'refusing to purge state outside HOME' env XDG_STATE_HOME="$fixture/outside" "$uninstall" --purge
expect_error 'refusing to purge state outside HOME' env PODS_TEST_STATE_DIR="$fixture/outside" "$uninstall" --purge
[[ -f $unit && -f outside/keys.json ]]
printf 'custom PATH file\n' >> "$XDG_CONFIG_HOME/fish/conf.d/pods.fish"
touch "$XDG_RUNTIME_DIR/pods.sock"
"$root/setup" --uninstall
[[ -f $XDG_STATE_HOME/pods/keys.json && ! -e $unit && ! -e $app && ! -e $XDG_RUNTIME_DIR/pods.sock ]]
[[ ! -L $HOME/.local/bin/pods && ! -L $HOME/.local/bin/pods-ctl && ! -L $plugin/superwhisper.pods ]]
[[ ! -e $XDG_CONFIG_HOME/environment.d/60-pods-path.conf ]]
grep -Fqx 'custom PATH file' "$XDG_CONFIG_HOME/fish/conf.d/pods.fish"
grep -Fqx 'export KEEP=1' "$HOME/.bashrc"
if grep -Fqx -- "$marker" "$HOME/.bashrc"; then exit 1; fi
"$uninstall" --purge
[[ ! -e $XDG_STATE_HOME/pods && -f outside/keys.json ]]
mkdir -p -- "$XDG_STATE_HOME/pods" "$fixture/outside-state"
printf 'inside\n' > "$XDG_STATE_HOME/pods/keys.json"
ln -s -- "$XDG_STATE_HOME/pods" "$fixture/outside-state/pods"
expect_error 'refusing to remove state link outside HOME' env XDG_STATE_HOME="$fixture/outside-state" "$uninstall" --purge
[[ -L $fixture/outside-state/pods && -f $XDG_STATE_HOME/pods/keys.json ]]
rm -rf -- "$fixture/outside-state" "$XDG_STATE_HOME/pods"

printf '%s\n' 'Testing bundle web install via loopback, checksum refusal, and archive path refusal'
cp -a -- fixture-dist web
python3 - "$fixture/web" "$fixture/port" > server.log 2>&1 <<'PY' &
import functools
import http.server
import pathlib
import sys

handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=sys.argv[1])
with http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler) as server:
    pathlib.Path(sys.argv[2]).write_text(str(server.server_port))
    server.serve_forever()
PY
server=$!
startup_checks=100
for ((i=0; i<startup_checks; i++)); do
    [[ ! -f port ]] || break
    kill -0 "$server"
    sleep 0.1
done
PODS_BASE_URL="http://127.0.0.1:$(< port)"
export PODS_BASE_URL
web_install="$root/packaging/web-install.sh"
sh "$web_install" --version "v$version" --dry-run | grep -Fq "Would download $PODS_BASE_URL/$archive"
expect_error 'PODS_BASE_URL must be a loopback' env PODS_BASE_URL=https://example.test sh "$web_install" --version "$version"
expect_error 'PODS_BASE_URL needs a numeric port' env PODS_BASE_URL='http://127.0.0.1:80@attacker.example:8080' sh "$web_install" --version "$version"
expect_error 'invalid version' sh "$web_install" --version '../bad'
mkdir -p -- redirect-stub
cat > redirect-stub/curl <<'STUB'
#!/bin/sh
set -eu
out=
write_out=0
url=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output) out=$2; shift ;;
        --write-out) write_out=1; shift ;;
        -q|--globoff|--fail|--silent|--show-error) ;;
        --proto|--tlsv1.2|--max-time) [ "$1" = --tlsv1.2 ] || shift ;;
        *) url=$1 ;;
    esac
    shift
done
if [ "$write_out" -eq 1 ]; then
    case "$url" in
        https://github.com/superultrainc/pods/releases/download/v*/pods-linux-*.sha256)
            printf '%s' "$PODS_STUB_REDIRECT_BASE/sha256"
            ;;
        https://github.com/superultrainc/pods/releases/download/v*/pods-linux-*.tar.zst)
            printf '%s' "$PODS_STUB_REDIRECT_BASE/archive"
            ;;
        *) exit 22 ;;
    esac
else
    case "$url" in
        */sha256) cp -- "$PODS_STUB_SHA" "$out" ;;
        */archive) cp -- "$PODS_STUB_ARCHIVE" "$out" ;;
        *) exit 22 ;;
    esac
fi
STUB
chmod 0755 -- redirect-stub/curl
expect_error 'unexpected asset redirect host' env -u PODS_BASE_URL PATH="$fixture/redirect-stub:$PATH" \
    PODS_STUB_REDIRECT_BASE='https://evil.example/github-production-release-asset/123456' \
    PODS_STUB_ARCHIVE="$fixture/fixture-dist/$archive" PODS_STUB_SHA="$fixture/fixture-dist/$archive.sha256" \
    sh "$web_install" --version "$version"
printf -v redirect_cmd 'sh %q --version %q' "$web_install" "$version"
env -u PODS_BASE_URL PATH="$fixture/redirect-stub:$PATH" \
    PODS_STUB_REDIRECT_BASE='https://release-assets.githubusercontent.com/github-production-release-asset/123456' \
    PODS_STUB_ARCHIVE="$fixture/fixture-dist/$archive" PODS_STUB_SHA="$fixture/fixture-dist/$archive.sha256" \
    script -qe -c "$redirect_cmd" /dev/null < /dev/null
[[ $("$HOME/.local/bin/pods" --version) == "pods $version" ]]
"$root/setup" --uninstall --keep-keys
mkdir -p -- "$plugin/io.github.thisisgm.omapods"
printf 'already disabled\n' > "$plugin/io.github.thisisgm.omapods/manifest.json"
cp -- stubs/omarchy stubs/omarchy.orig
cat > stubs/omarchy <<'STUB'
#!/bin/sh
printf 'omarchy %s\n' "$*" >> "$TEST_COMMANDS"
if [ "$1 $2 $3" = 'plugin disable io.github.thisisgm.omapods' ]; then exit 1; fi
STUB
chmod 0755 -- stubs/omarchy
"$install" --binary "$fixture/pods-good" 2> legacy-disable.log
grep -Fq 'could not disable legacy plugin io.github.thisisgm.omapods' legacy-disable.log
[[ ! -e $plugin/io.github.thisisgm.omapods && -f $plugin/superwhisper.pods/Panel.qml ]]
mv -f -- stubs/omarchy.orig stubs/omarchy
"$root/setup" --uninstall --keep-keys
sha_name="$archive.sha256"
printf '%064d  %s\n' 0 "$archive" > "web/$sha_name"
expect_error 'SHA-256 mismatch' sh "$web_install" --version "$version"
mkdir -p -- unsafe
touch unsafe/benign
tar --zstd --transform='s|^benign$|../outside/keys.json|' -cf "web/$archive" -C unsafe benign
(cd -- web && sha256sum -- "$archive" > "$sha_name")
expect_error 'unsafe archive path' sh "$web_install" --version "$version"
rm -- "web/$archive"
ln -s -- ../outside/keys.json unsafe/link
tar --zstd --transform="s|^link$|$name/plugin/Panel.qml|" -cf "web/$archive" -C unsafe link
(cd -- web && sha256sum -- "$archive" > "$sha_name")
expect_error 'unsafe archive entry type' sh "$web_install" --version "$version"
cp -- "fixture-dist/$archive" "fixture-dist/$sha_name" web/
printf -v terminal_cmd 'sh %q --version %q' "$web_install" "$version"
script -qe -c "$terminal_cmd" /dev/null < /dev/null
[[ $("$HOME/.local/bin/pods" --version) == "pods $version" ]]
[[ -f outside/keys.json && $(< outside/keys.json) == outside ]]
"$root/setup" --uninstall --keep-keys

printf '%s\n' 'Testing source build dispatch and non-interactive service mode'
checkout="$fixture/source"
mkdir -p -- "$checkout/crates/daemon"
cp -a -- "$root/packaging" "$root"/{setup,manifest.json,*.qml,*.js,preview.png} "$checkout"
cp -- "$root/crates/daemon/Cargo.toml" "$checkout/crates/daemon"
export PODS_TEST_BINARY="$fixture/pods-good"
cat > stubs/cargo <<'STUB'
#!/bin/sh
set -eu
[ "$*" = "build --release --locked -p pods --target-dir $PWD/target" ] || exit 1
install -Dm755 "$PODS_TEST_BINARY" "$PWD/target/release/pods"
STUB
chmod 0755 -- stubs/cargo
cp -- "$HOME/.bashrc" before.bashrc
calls_before=$(grep -c '^systemctl ' "$TEST_COMMANDS")
INVOCATION_ID=test PODS_SKIP_SERVICE=1 "$checkout/setup" --from-source
cmp -- before.bashrc "$HOME/.bashrc"
[[ $(grep -c '^systemctl ' "$TEST_COMMANDS") == "$calls_before" ]]
PODS_SKIP_SERVICE=1 "$checkout/setup" --uninstall
PODS_SKIP_SERVICE=1 XDG_CONFIG_HOME="$XDG_DATA_HOME" "$install" --binary "$fixture/pods-good"
[[ -f $XDG_DATA_HOME/systemd/user/podsd.service ]]
PODS_SKIP_SERVICE=1 XDG_CONFIG_HOME="$XDG_DATA_HOME" "$uninstall"
[[ ! -e $XDG_DATA_HOME/systemd/user/podsd.service ]]

printf '%s\n' 'Testing dependency command selection'
mkdir -p -- managers
export PODS_SKIP_PACKAGES=0
ln -s -- "$(command -v bash)" managers/bash
ln -s -- "$(command -v id)" managers/id
cat > managers/sudo <<'STUB'
#!/bin/sh
printf '%s\n' "sudo $*" >> "$TEST_COMMANDS"
STUB
cat > managers/pacman <<'STUB'
#!/bin/sh
exit 1
STUB
cp -- managers/pacman managers/rpm
cp -- managers/pacman managers/dpkg-query
cp -- managers/pacman managers/apt-get
cp -- managers/pacman managers/dnf
cp -- managers/pacman managers/zypper
chmod 0755 -- managers/{sudo,pacman,rpm,dpkg-query,apt-get,dnf,zypper}
deps="$root/packaging/dependencies.sh"
PATH="$fixture/managers" "$deps" install
grep -Fqx 'sudo pacman -S --needed --noconfirm pipewire pipewire-pulse bluez' "$TEST_COMMANDS"
cp -- stubs/omarchy managers/omarchy
PATH="$fixture/managers" "$deps" install
grep -Fqx 'omarchy pkg add pipewire pipewire-pulse bluez' "$TEST_COMMANDS"
rm -- managers/{omarchy,pacman}
PATH="$fixture/managers" "$deps" install
grep -Fqx 'sudo apt-get install -y pipewire pipewire-pulse bluez' "$TEST_COMMANDS"
rm -- managers/apt-get
PATH="$fixture/managers" "$deps" install
grep -Fqx 'sudo dnf install -y pipewire pipewire-pulseaudio bluez' "$TEST_COMMANDS"
rm -- managers/dnf
PATH="$fixture/managers" "$deps" install
grep -Fqx 'sudo zypper --non-interactive install pipewire pipewire-pulseaudio bluez' "$TEST_COMMANDS"
printf '%s\n' 'Packaging tests passed'
