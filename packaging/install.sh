#!/usr/bin/env bash
set -euo pipefail
umask 022
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=packaging/common.sh
source "$here/common.sh"
# shellcheck source=packaging/dependencies.sh
source "$here/dependencies.sh"

usage() {
    printf 'Usage: install.sh [--binary PATH | --from-source]\n'
}

binary=
mode=auto
case ${1:-} in
    '') [[ $# == 0 ]] || fail 'unexpected argument' ;;
    --binary) [[ $# == 2 && -n $2 ]] || fail '--binary requires one path'; mode=binary; binary=$2 ;;
    --from-source) [[ $# == 1 ]] || fail '--from-source takes no argument'; mode=source ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
esac

# Sample input: 1.0.0 and its newline, verified by the checksum manifest.
verify_bundle() {
    local actual expected
    [[ -f $here/VERSION && -f $here/MANIFEST.sha256 && -x $here/bin/pods ]] ||
        fail 'bundle is missing VERSION, MANIFEST.sha256 or bin/pods'
    version=$(< "$here/VERSION")
    valid_version "$version" || fail "invalid bundle version: $version"
    actual=$(cd -- "$here" && find VERSION MANIFEST.sha256 bin systemd plugin icons gnome-extension ./*.sh -type l -print)
    [[ -z $actual ]] || fail "unexpected bundle symlink: $actual"
    expected=$(cd -- "$here" && bundle_hashes) ||
        fail 'could not hash the bundle files'
    [[ $expected == "$(< "$here/MANIFEST.sha256")" ]] || fail 'bundle MANIFEST.sha256 does not match the files'
    (cd -- "$here" && sha256sum --check --strict --status MANIFEST.sha256) ||
        fail 'bundle checksum verification failed'
}

if [[ $mode == auto ]] && { exists "$here/VERSION" || exists "$here/MANIFEST.sha256" || exists "$here/bin/pods"; }; then
    export LC_ALL=C
    verify_bundle
    binary="$here/bin/pods"
    plugin_source="$here/plugin"
    unit_source="$here/systemd/podsd.service"
else
    root=$(dirname -- "$here")
    [[ -f $root/crates/daemon/Cargo.toml ]] || fail 'source mode needs a Pods checkout'
    version=$(repo_version "$root")
    valid_version "$version" || fail "invalid checkout version: $version"
    plugin_source=$root
    unit_source="$here/systemd/podsd.service"
    if [[ $mode != binary ]]; then
        mode=source
        binary="$root/target/release/pods"
    fi
fi

setup_paths
if [[ $mode == source ]]; then
    command -v cargo >/dev/null || fail 'cargo is required for --from-source'
else
    [[ -f $binary && -x $binary ]] || fail "missing executable: $binary"
fi
pods_dependencies install
if [[ $mode == source ]]; then
    (cd -- "$root" && cargo build --release --locked -p pods --target-dir "$root/target")
fi
[[ -f $binary && -x $binary ]] || fail "missing executable: $binary"
[[ $("$binary" --version) == "pods $version" ]] || fail "binary version does not match $version: $binary"

old_version=$(version_link "$app/current")
old_previous=$(version_link "$app/previous")
if exists "$bin_dir/pods" && ! link_is "$bin_dir/pods" "$pods_link"; then
    # The previous source installer wrote a plain executable alongside its stock user unit.
    old_binary=0
    if [[ -f $bin_dir/pods && ! -L $bin_dir/pods ]]; then
        for path in "${shadow_units[@]}" "$unit"; do
            if [[ -f $path ]] && cmp -s -- "$unit_source" "$path"; then old_binary=1; fi
        done
    fi
    ((old_binary)) || fail "existing executable: $bin_dir/pods"
fi
if exists "$bin_dir/pods-ctl" && ! link_is "$bin_dir/pods-ctl" pods; then
    fail "existing client: $bin_dir/pods-ctl"
fi

legacy_dirs=()
for dir in "$config_home" "$state_home"; do
    legacy="$dir/librepods"
    if exists "$legacy"; then
        [[ -d $legacy && ! -L $legacy ]] || fail "legacy path is not a directory: $legacy"
        ! exists "$dir/pods" || fail "legacy migration would overwrite $dir/pods"
        [[ $dir == "${legacy_dirs[0]:-}" ]] || legacy_dirs+=("$dir")
    fi
done
legacy_units=()
for dir in "$config_home" "$data_home" "$default_data_home"; do
    candidate="$dir/systemd/user/librepods.service"
    if exists "$candidate"; then legacy_units+=("$candidate"); fi
done

mkdir -p -- "$app" "$bin_dir" "$(dirname -- "$unit")"
scratch=$(mktemp -d "$app/.install.XXXXXX")
version_dir="$app/$version"
changed=0
published=0
legacy_disabled=0
legacy_active=0
legacy_enabled=0
migrated=()
paths=("$unit" "${shadow_units[@]}" "$bin_dir/pods" "$bin_dir/pods-ctl")
prior_active=0
prior_enabled=0
first_plugin=0
first_gnome=0

atomic_link() {
    local link_tmp
    link_tmp=$(mktemp -d "$(dirname -- "$2")/.pods-link.XXXXXX") || return 1
    if ln -s -- "$1" "$link_tmp/link" && mv -Tf -- "$link_tmp/link" "$2"; then
        rmdir -- "$link_tmp"
    else
        rm -f -- "$link_tmp/link"
        rmdir -- "$link_tmp"
        return 1
    fi
}

restore_link() {
    if [[ -n $1 ]]; then
        atomic_link "$1" "$2"
    else
        rm -f -- "$2"
    fi
}

rollback() {
    printf 'pods: rolling back %s\n' "$version" >&2
    if ((prior_active == 0)); then units stop podsd.service; fi
    if ((prior_enabled == 0)); then units disable podsd.service; fi
    if ((first_plugin)) && link_is "$plugin_link" "$app/current/plugin"; then rm -- "$plugin_link"; fi
    if ((first_gnome)) && link_is "$gnome_link" "$gnome_target"; then rm -- "$gnome_link"; fi
    restore_link "$old_version" "$app/current"
    restore_link "$old_previous" "$app/previous"
    for index in "${!paths[@]}"; do
        rm -f -- "${paths[index]}"
        if exists "$scratch/prior-$index"; then
            cp -a -- "$scratch/prior-$index" "${paths[index]}"
        fi
    done
    for dir in "${migrated[@]}"; do
        mv -nT -- "$dir/pods" "$dir/librepods"
    done
    units daemon-reload
    if ((prior_active)); then units restart podsd.service; fi
    if ((legacy_disabled && legacy_enabled)); then units enable librepods.service; fi
    if ((legacy_disabled && legacy_active)); then units start librepods.service; fi
    if ((published)); then rm -rf -- "$version_dir"; fi
}

cleanup() {
    local result=$?
    set +e
    if ((changed)); then rollback; fi
    if [[ $scratch == "$app"/.install.* ]]; then rm -rf -- "$scratch"; fi
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

install -d -m 0755 "$scratch/version" "$scratch/version/bin" "$scratch/version/plugin"
install -m 0755 -- "$binary" "$scratch/version/bin/pods"
install -m 0644 -- "$plugin_source"/{manifest.json,*.qml,*.js,preview.png} "$scratch/version/plugin"
stage_desktop_assets "$here" "$scratch/version"
if exists "$version_dir"; then
    if [[ ! -d $version_dir || -L $version_dir ]] || ! diff -qr -- "$scratch/version" "$version_dir"; then
        fail "installed version $version has different files; use a new version"
    fi
fi

for index in "${!paths[@]}"; do
    if exists "${paths[index]}"; then
        [[ ! -d ${paths[index]} ]] || fail "expected a file: ${paths[index]}"
        cp -a -- "${paths[index]}" "$scratch/prior-$index"
    fi
done
if [[ ${PODS_SKIP_SERVICE:-0} != 1 ]]; then
    if systemctl --user is-active --quiet podsd.service; then prior_active=1; fi
    if systemctl --user is-enabled --quiet podsd.service; then prior_enabled=1; fi
fi

changed=1
if ! exists "$version_dir"; then
    mv -T -- "$scratch/version" "$version_dir"
    published=1
fi
if ((${#legacy_units[@]})); then
    if [[ ${PODS_SKIP_SERVICE:-0} != 1 ]]; then
        if systemctl --user is-enabled --quiet librepods.service; then legacy_enabled=1; fi
        if systemctl --user is-active --quiet librepods.service; then legacy_active=1; fi
        legacy_disabled=1
        units disable --now librepods.service
    fi
fi
for dir in "${legacy_dirs[@]}"; do
    mv -nT -- "$dir/librepods" "$dir/pods"
    ! exists "$dir/librepods" || fail "could not migrate $dir/librepods"
    migrated+=("$dir")
done
atomic_link "$version" "$app/current"
atomic_link "$pods_link" "$bin_dir/pods"
atomic_link pods "$bin_dir/pods-ctl"
# The unit may sit on a different filesystem from the version tree.
unit_tmp=$(mktemp "$(dirname -- "$unit")/.podsd.XXXXXX")
if ! install -m 0644 -- "$unit_source" "$unit_tmp" || ! mv -Tf -- "$unit_tmp" "$unit"; then
    rm -f -- "$unit_tmp"
    fail "could not install $unit"
fi
if ((${#shadow_units[@]})); then rm -f -- "${shadow_units[@]}"; fi
# Publish desktop links before podsd checks for duplicate indicators at startup.
if command -v omarchy >/dev/null && [[ -d $omarchy_dir ]]; then
    if ! exists "$plugin_link"; then
        mkdir -p -- "$omarchy_dir/plugins"
        ln -s -- "$app/current/plugin" "$plugin_link"
        first_plugin=1
    elif ! link_is "$plugin_link" "$app/current/plugin"; then
        fail "existing plugin path: $plugin_link"
    fi
fi
if [[ ${XDG_CURRENT_DESKTOP:-} == *GNOME* ]] || command -v gnome-shell >/dev/null; then
    if ! exists "$gnome_link"; then
        if mkdir -p -- "$(dirname -- "$gnome_link")" && ln -s -- "$gnome_target" "$gnome_link"; then
            first_gnome=1
        else
            printf 'pods: could not link GNOME extension at %s\n' "$gnome_link" >&2
        fi
    elif ! link_is "$gnome_link" "$gnome_target"; then
        printf 'pods: keeping existing GNOME extension at %s\n' "$gnome_link" >&2
    fi
fi
units daemon-reload
units enable podsd.service
units restart podsd.service

# Sample input: {"connected":false,"schema_version":1} from the JSON-validating Pods client.
healthy() {
    local reply
    reply=$("$bin_dir/pods" status 2>/dev/null) || return 1
    [[ $reply == '{'*'}' && $reply =~ \"schema_version\"[[:space:]]*:[[:space:]]*[1-9][0-9]*[[:space:]]*[,}] ]]
}

if [[ ${PODS_SKIP_SERVICE:-0} != 1 ]]; then
    readonly service_wait_s=10
    deadline=$((SECONDS + service_wait_s))
    until healthy; do
        ((SECONDS < deadline)) || fail 'podsd.service failed health check. See journalctl --user -u podsd.service'
        sleep 1
    done
fi
if [[ -n $old_version && $old_version != "$version" ]]; then
    atomic_link "$old_version" "$app/previous"
fi
changed=0

install_icons() {
    local source path icon
    mkdir -p -- "$status_icons" || return 1
    for source in "$version_dir"/icons/pods-*-symbolic.svg; do
        icon=${source##*/}
        path="$status_icons/$icon"
        if checksum_matches "$path" "$source"; then
            continue
        fi
        if exists "$path" && ! checksum_matches "$path" "$app/${old_version:-current}/icons/$icon" &&
            ! checksum_matches "$path" "$app/${old_previous:-current}/icons/$icon"; then
            printf 'pods: keeping existing status icon at %s\n' "$path" >&2
            continue
        fi
        install -m 0644 -- "$source" "$path" || return 1
    done
    refresh_icon_cache
}
install_icons || printf 'pods: could not install status icons in %s\n' "$status_icons" >&2

if ((first_gnome)); then
    if ! command -v gnome-extensions >/dev/null || ! gnome-extensions enable "$gnome_uuid" > "$scratch/gnome-enable.log" 2>&1; then
        printf 'pods: log out and back in, then enable %s in GNOME Extensions\n' "$gnome_uuid" >&2
    fi
fi

if ((${#legacy_units[@]})); then
    rm -f -- "${legacy_units[@]}"
    units daemon-reload
fi
if link_is "$bin_dir/librepods-ctl" librepods; then
    rm -f -- "$bin_dir/librepods-ctl"
fi

# Sample input: sha256sum reports a 64-hex digest followed by two spaces and a hyphen.
remove_legacy_asset() {
    local path=$1 expected=$2 checksum
    if [[ -f $path && ! -L $path ]]; then
        checksum=$(sha256sum < "$path")
        if [[ ${checksum%% *} == "$expected" ]]; then rm -f -- "$path"; fi
    fi
}
for dir in "$data_home" "$default_data_home"; do
    remove_legacy_asset "$dir/applications/me.kavishdevar.librepods.desktop" \
        a485b2f5939e9f334fa8ffcdc0a37a292b1d349e9357ec1c710e5f712e5bed23
    remove_legacy_asset "$dir/icons/hicolor/scalable/apps/librepods.svg" \
        8d3d2c7bd6ecbed70afe21c5530ce03e35811b5090087bf5b1ca2fffbf63ef19
done

write_path_file "$config_home/environment.d/60-pods-path.conf" "$path_env" ||
    printf 'pods: could not write environment.d PATH file\n' >&2
if [[ -z ${INVOCATION_ID:-} ]]; then
    for rc in "$HOME/.bashrc" "$HOME/.zshrc"; do
        if ! grep -Fqx -- "$path_open" "$rc" 2>/dev/null; then
            if ! printf '\n%s\n%s\n%s\n' "$path_open" "$path_export" "$path_close" >> "$rc"; then
                printf 'pods: could not update %s\n' "$rc" >&2
            fi
        fi
    done
    write_path_file "$config_home/fish/conf.d/pods.fish" "$path_fish" ||
        printf 'pods: could not write fish PATH file\n' >&2
fi

enable_omarchy_plugin() {
    local plugin=$1
    # The shell returns from rescanPlugins before the asynchronous manifest scan completes.
    local -r plugin_wait_s=5
    local deadline=$((SECONDS + plugin_wait_s))
    until omarchy plugin enable "$plugin" > "$scratch/enable.log" 2>&1; do
        ((SECONDS < deadline)) || return 1
        sleep 1
    done
}

restore_legacy_plugin() {
    case "${legacy_kind:-}" in
        symlink) ln -s -- "$legacy_target" "$legacy_link" || true ;;
        dir)
            if [[ -e $legacy_backup/$legacy_plugin ]] && mv -T -- "$legacy_backup/$legacy_plugin" "$legacy_link"; then
                rmdir -- "$legacy_backup" || true
            fi
            ;;
    esac
    if [[ -n ${legacy_kind:-} ]]; then
        omarchy-shell -q shell rescanPlugins || true
    fi
    if ((legacy_disabled)); then
        if ! enable_omarchy_plugin "$legacy_plugin"; then
            printf 'pods: could not re-enable legacy widget %s\n' "$legacy_plugin" >&2
        fi
    fi
}

if command -v omarchy >/dev/null && [[ -d $omarchy_dir ]]; then
    legacy_plugin=io.github.thisisgm.omapods
    legacy_link="$omarchy_dir/plugins/$legacy_plugin"
    legacy_kind=
    legacy_target=
    legacy_backup=
    legacy_disabled=0
    if exists "$legacy_link"; then
        if [[ -f $legacy_link/manifest.json ]]; then
            if omarchy plugin disable "$legacy_plugin"; then
                legacy_disabled=1
            else
                printf 'pods: could not disable legacy plugin %s; moving it aside\n' "$legacy_plugin" >&2
            fi
        fi
        if [[ -L $legacy_link ]]; then
            legacy_kind=symlink
            legacy_target=$(readlink -- "$legacy_link")
            rm -- "$legacy_link"
        else
            legacy_kind=dir
            legacy_backup=$(mktemp -d "$omarchy_dir/pods-legacy.XXXXXX")
            mv -T -- "$legacy_link" "$legacy_backup/$legacy_plugin"
        fi
    fi
    if ! omarchy-shell -q shell rescanPlugins; then
        if ((first_plugin)); then rm -f -- "$plugin_link"; fi
        restore_legacy_plugin
        fail 'Omarchy shell could not rescan its plugins'
    fi
    if ((first_plugin)) && ! enable_omarchy_plugin superwhisper.pods; then
        rm -f -- "$plugin_link"
        cat -- "$scratch/enable.log" >&2
        restore_legacy_plugin
        fail 'Omarchy could not enable superwhisper.pods'
    fi
    if [[ $legacy_kind == dir ]]; then
        printf 'pods: previous plugin saved in %s\n' "$legacy_backup"
    fi
fi

# Sample input: as 1 "or_patterns".
if ! monitor=$(busctl --system --timeout=2 get-property org.bluez /org/bluez/hci0 \
    org.bluez.AdvertisementMonitorManager1 SupportedMonitorTypes 2>/dev/null) || [[ $monitor != *or_patterns* ]]; then
    printf 'BlueZ experimental monitoring is off. Enable it for case, lid and ear state while AirPods are away.\n'
fi
printf 'Installed Pods %s. Reopen your shell for the PATH update.\n' "$version"
