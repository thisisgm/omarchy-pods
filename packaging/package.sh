#!/usr/bin/env bash
set -euo pipefail
umask 022
export LC_ALL=C
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=packaging/common.sh
source "$here/common.sh"
root=$(dirname -- "$here")
binary="$root/target/release/pods"
output="$root/dist"
version=$(repo_version "$root")
arch=$(uname -m)
tree_only=0

while (($#)); do
    case "$1" in
        --binary|--output|--version|--arch)
            [[ -n ${2:-} ]] || fail "$1 requires a value"
            case "$1" in
                --binary) binary=$2 ;;
                --output) output=$2 ;;
                --version) version=$2 ;;
                --arch) arch=$2 ;;
            esac
            shift
            ;;
        --tree-only) tree_only=1 ;;
        *) fail "usage: package.sh [--binary PATH] [--output DIR] [--arch ARCH] [--version VERSION] [--tree-only]" ;;
    esac
    shift
done
valid_version "$version" || fail "invalid version: $version"
[[ $arch == x86_64 || $arch == aarch64 ]] || fail "unsupported architecture: $arch"
[[ -f $binary && -x $binary ]] || fail "missing executable: $binary"
[[ $("$binary" --version) == "pods $version" ]] || fail "binary version does not match $version: $binary"

mkdir -p -- "$output"
output=$(realpath -e -- "$output")
scratch=$(mktemp -d "$output/.pods-package.XXXXXX")
trap '[[ $scratch == "$output"/.pods-package.* ]] && rm -rf -- "$scratch"' EXIT
name="pods-linux-$arch-$version"
tree="$scratch/$name"
install -d -m 0755 "$tree" "$tree/bin" "$tree/systemd" "$tree/plugin"
install -m 0755 -- "$binary" "$tree/bin/pods"
install -m 0755 -- "$here"/{install,uninstall,dependencies}.sh "$tree"
install -m 0644 -- "$here/common.sh" "$tree"
install -m 0644 -- "$here/systemd/podsd.service" "$tree/systemd"
install -m 0644 -- "$root"/{manifest.json,*.qml,*.js,preview.png} "$tree/plugin"
printf '%s\n' "$version" > "$tree/VERSION"
(
    cd -- "$tree"
    sha256sum -- VERSION bin/pods ./*.sh systemd/podsd.service plugin/* > MANIFEST.sha256
)
if ((tree_only)); then
    [[ ! -e $output/$name && ! -L $output/$name ]] || fail "bundle tree already exists: $output/$name"
    mv -T -- "$tree" "$output/$name"
    printf '%s\n' "$output/$name"
    exit
fi

archive="$name.tar.zst"
(
    cd -- "$scratch"
    tar --sort=name --format=ustar --mtime="@${SOURCE_DATE_EPOCH:-0}" --owner=0 --group=0 --numeric-owner \
        -cf - -- "$name" | zstd -q -T1 -o "$archive"
    sha256sum -- "$archive" > "$archive.sha256"
)
mv -T -- "$scratch/$archive" "$output/$archive"
mv -T -- "$scratch/$archive.sha256" "$output/$archive.sha256"
printf '%s\n' "$output/$archive" "$output/$archive.sha256"
