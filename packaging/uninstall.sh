#!/usr/bin/env bash
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=packaging/common.sh
source "$here/common.sh"

purge=0
case ${1:-} in
    '') [[ $# == 0 ]] || fail 'unexpected argument' ;;
    --keep-keys|--purge)
        [[ $# == 1 ]] || fail 'use either --keep-keys or --purge'
        if [[ $1 == --purge ]]; then purge=1; fi
        ;;
    -h|--help) printf 'Usage: uninstall.sh [--keep-keys | --purge]\n'; exit 0 ;;
    *) fail 'usage: uninstall.sh [--keep-keys | --purge]' ;;
esac

setup_paths
state="$state_home/pods"
# Sample input: /home/user/.local/state/pods, returned by pods state-dir.
if ((purge)); then
    if link_is "$bin_dir/pods" "$pods_link" && [[ -x $pods_link ]]; then
        state=$(timeout --kill-after=2s 2s "$bin_dir/pods" state-dir) || fail 'pods state-dir failed'
    fi
    [[ $state == /?* && $state != *$'\n'* ]] || fail "invalid state directory: $state"
    resolved=$(realpath -m -- "$state")
    [[ $resolved == "$home_real"/?* ]] || fail "refusing to purge state outside HOME: $state"
    if [[ -L $state ]]; then
        state_parent=$(realpath -m -- "$(dirname -- "$state")")
        [[ $state_parent == "$home_real" || $state_parent == "$home_real"/?* ]] ||
            fail "refusing to remove state link outside HOME: $state"
    fi
fi

installed_units=()
for path in "$unit" "${shadow_units[@]}"; do
    if exists "$path"; then installed_units+=("$path"); fi
done
if ((${#installed_units[@]})); then
    units disable --now podsd.service
    rm -f -- "${installed_units[@]}"
    units daemon-reload
fi
if link_is "$bin_dir/pods" "$pods_link"; then rm -- "$bin_dir/pods"; fi
if link_is "$bin_dir/pods-ctl" pods; then rm -- "$bin_dir/pods-ctl"; fi
for rc in "$HOME/.bashrc" "$HOME/.zshrc"; do
    remove_rc_block "$rc" || printf 'pods: could not update %s\n' "$rc" >&2
done
env_file="$config_home/environment.d/60-pods-path.conf"
fish_file="$config_home/fish/conf.d/pods.fish"
if [[ ! -L $env_file ]] && path_file "$env_file" "$path_env"; then rm -- "$env_file"; fi
if [[ ! -L $fish_file ]] && path_file "$fish_file" "$path_fish"; then rm -- "$fish_file"; fi

if [[ -L $plugin_link ]]; then
    target=$(realpath -m -- "$plugin_link")
    raw_target=$(readlink -- "$plugin_link")
    if [[ $target == "$app"/?* || $raw_target == "$app/current/plugin" ]]; then
        if command -v omarchy >/dev/null && ! omarchy plugin disable superwhisper.pods; then
            printf 'pods: could not disable the widget in the Omarchy shell\n' >&2
        fi
        rm -- "$plugin_link"
        if command -v omarchy-shell >/dev/null && ! omarchy-shell -q shell rescanPlugins; then
            printf 'pods: could not rescan plugins in the Omarchy shell\n' >&2
        fi
    fi
fi

if link_is "$gnome_link" "$gnome_target"; then
    if command -v gnome-extensions >/dev/null && ! gnome-extensions disable "$gnome_uuid"; then
        printf 'pods: could not disable %s in GNOME Shell\n' "$gnome_uuid" >&2
    fi
    rm -- "$gnome_link"
fi

removed_icons=0
for path in "$status_icons"/pods-{buds,pro,max}-symbolic.svg; do
    icon=${path##*/}
    if checksum_matches "$path" "$app/current/icons/$icon" || checksum_matches "$path" "$app/previous/icons/$icon"; then
        rm -- "$path"
        removed_icons=1
    fi
done
if ((removed_icons)); then refresh_icon_cache; fi

[[ $app == "$data_home"/pods/app && $data_home == /?* ]] || fail "invalid app directory: $app"
rm -rf -- "$app"
rm -f -- "$XDG_RUNTIME_DIR/pods.sock"
if ((purge)); then
    rm -rf -- "$resolved"
    if [[ -L $state ]]; then rm -- "$state"; fi
    printf 'Removed Pods and purged %s\n' "$resolved"
else
    printf 'Removed Pods. Kept state and pairing keys in %s\n' "$state"
fi
