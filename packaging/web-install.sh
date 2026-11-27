#!/bin/sh
set -eu
export LC_ALL=C
umask 077

fail() {
    printf 'pods installer: %s\n' "$*" >&2
    exit 1
}

usage() {
    printf 'Usage: install.sh [--version VERSION] [--dry-run] [--help]\n'
}

version=
dry_run=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            [ -n "${2:-}" ] || fail '--version requires a value'
            version=${2#v}
            [ -n "$version" ] || fail 'invalid version'
            shift
            ;;
        --dry-run) dry_run=1 ;;
        --help|-h) usage; exit 0 ;;
        *) usage >&2; fail "unknown option $1" ;;
    esac
    shift
done

case "$(uname -m)" in
    x86_64) arch=x86_64 ;;
    aarch64) arch=aarch64 ;;
    *) fail "unsupported architecture: $(uname -m)" ;;
esac

dependency_hint() {
    if command -v omarchy >/dev/null; then
        printf 'omarchy pkg add curl tar zstd coreutils bash diffutils findutils util-linux'
    elif command -v pacman >/dev/null; then
        printf 'sudo pacman -S curl tar zstd coreutils bash diffutils findutils util-linux'
    elif command -v apt-get >/dev/null; then
        printf 'sudo apt-get install curl tar zstd coreutils bash diffutils findutils util-linux'
    elif command -v dnf >/dev/null; then
        printf 'sudo dnf install curl tar zstd coreutils bash diffutils findutils util-linux'
    elif command -v zypper >/dev/null; then
        printf 'sudo zypper install curl tar zstd coreutils bash diffutils findutils util-linux'
    else
        printf 'install curl, tar, zstd, coreutils, bash, diffutils, findutils and util-linux'
    fi
}

for tool in curl tar zstd sha256sum bash cmp diff find flock; do
    command -v "$tool" >/dev/null || fail "missing $tool. Run: $(dependency_hint)"
done

base=https://github.com/superultrainc/pods/releases
local_url=${PODS_BASE_URL:-}
# Sample input: http://127.0.0.1:8080, the test server's origin.
if [ -n "$local_url" ]; then
    case "$local_url" in
        http://127.0.0.1:*) port=${local_url#http://127.0.0.1:} ;;
        *) fail 'PODS_BASE_URL must be a loopback HTTP origin' ;;
    esac
    case "$port" in
        ''|*[!0-9]*) fail 'PODS_BASE_URL needs a numeric port' ;;
    esac
    base=$local_url
fi

github_get() {
    curl -q --globoff --fail --silent --show-error --proto '=https' --tlsv1.2 --max-time 120 "$@"
}

download() {
    if [ -n "$local_url" ]; then
        curl -q --globoff --fail --silent --show-error --proto '=http' --noproxy '*' --max-time 120 --output "$1" "$2"
        return
    fi
    redirect=$(github_get --output "$1" --write-out '%{redirect_url}' "$2") || return 1
    # GitHub signs a short-lived asset URL on its own download host.
    case "$redirect" in
        '') ;;
        https://release-assets.githubusercontent.com/github-production-release-asset/*|https://release-assets.githubusercontent.com/github-production-release-asset-*)
            github_get --output "$1" "$redirect"
            ;;
        *) fail "unexpected asset redirect host" ;;
    esac
}

if [ -z "$version" ]; then
    [ -z "$local_url" ] || fail '--version is required with PODS_BASE_URL'
    latest=$(github_get --output /dev/null --write-out '%{redirect_url}' "$base/latest") ||
        fail 'could not resolve the latest Pods release'
    case "$latest" in
        "$base"/tag/v*) version=${latest#"$base/tag/v"} ;;
        *) fail "unexpected latest release URL: $latest" ;;
    esac
fi
# Sample input: 1.0.0 or 1.0.0-rc.1.
case "$version" in ''|*[!0-9A-Za-z.-]*) fail "invalid version: $version" ;; esac
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+([.-][0-9A-Za-z]+)*)?$' ||
    fail "invalid version: $version"

name="pods-linux-$arch-$version"
archive="$name.tar.zst"
if [ -n "$local_url" ]; then
    url="$base/$archive"
else
    url="$base/download/v$version/$archive"
fi
if [ "$dry_run" -eq 1 ]; then
    printf 'Would download %s and %s.sha256\n' "$url" "$url"
    exit
fi

temp_parent=$(realpath -e -- "${TMPDIR:-/tmp}") || fail 'invalid TMPDIR'
scratch=$(mktemp -d "$temp_parent/pods-download.XXXXXX") || fail 'could not create a download directory'
trap 'case "$scratch" in "$temp_parent"/pods-download.*) rm -rf -- "$scratch" ;; esac' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
cd -- "$scratch"
download "$archive" "$url" || fail "could not download $url"
download "$archive.sha256" "$url.sha256" || fail "could not download $url.sha256"
# Sample input: <64 hex digits><two spaces>pods-linux-x86_64-1.0.0.tar.zst.
checksum=$(cat -- "$archive.sha256")
digest=${checksum%% *}
case "$digest" in ''|*[!0-9a-fA-F]*) fail 'invalid SHA-256 digest' ;; esac
sha256_hex_digits=64
if [ "${#digest}" -ne "$sha256_hex_digits" ] || [ "$checksum" != "$digest  $archive" ]; then
    fail 'invalid SHA-256 file'
fi
printf '%s  %s\n' "$digest" "$archive" | sha256sum --check --strict --status || fail 'SHA-256 mismatch'

tar --zstd --quoting-style=escape -tf "$archive" > entries || fail 'could not list archive'
tar --zstd --quoting-style=escape -tvf "$archive" > kinds || fail 'could not list archive entry types'
if grep -qv '^[-d]' kinds; then fail 'unsafe archive entry type'; fi
[ -s entries ] || fail 'empty archive'
sort entries > sorted
uniq -d sorted > duplicates
[ ! -s duplicates ] || fail 'duplicate archive entries'

# Sample input: pods-linux-x86_64-1.0.0/plugin/Panel.qml.
while IFS= read -r entry; do
    case "$entry" in
        "$name"/|"$name"/VERSION|"$name"/MANIFEST.sha256|"$name"/bin/|"$name"/bin/pods|\
        "$name"/systemd/|"$name"/systemd/podsd.service|"$name"/plugin/|"$name"/plugin/manifest.json|\
        "$name"/plugin/preview.png|"$name"/install.sh|"$name"/uninstall.sh|"$name"/common.sh|"$name"/dependencies.sh) ;;
        "$name"/icons/|"$name"/icons/pods-buds-symbolic.svg|"$name"/icons/pods-pro-symbolic.svg|"$name"/icons/pods-max-symbolic.svg|\
        "$name"/gnome-extension/|"$name"/gnome-extension/pods@superwhisper.com/) ;;
        "$name"/gnome-extension/pods@superwhisper.com/*)
            file=${entry#"$name/gnome-extension/pods@superwhisper.com/"}
            case "$file" in
                extension.js|metadata.json|stylesheet.css|pods-buds-symbolic.svg|pods-pro-symbolic.svg|pods-max-symbolic.svg) ;;
                *) fail "unsafe archive path: $entry" ;;
            esac
            ;;
        "$name"/plugin/*.qml|"$name"/plugin/*.js)
            file=${entry#"$name/plugin/"}
            case "$file" in
                .*|*[!A-Za-z0-9_.-]*|*..*) fail "unsafe archive path: $entry" ;;
            esac
            ;;
        *) fail "unsafe archive path: $entry" ;;
    esac
done < entries

tar --zstd --no-same-owner --no-same-permissions -xf "$archive" || fail 'could not extract archive'
if [ ! -f "$name/VERSION" ] || [ "$(cat -- "$name/VERSION")" != "$version" ]; then
    fail 'archive VERSION does not match release'
fi
[ -x "$name/install.sh" ] || fail 'archive is missing install.sh'
cd -- "$name"
if ! ( : </dev/tty ) 2>/dev/null; then fail 'a terminal is required to run install.sh'; fi
./install.sh </dev/tty
