#!/usr/bin/env bash
# Shared paths and ownership checks for the source installer and release bundle.
# shellcheck disable=SC2034

fail() {
    printf 'pods: %s\n' "$*" >&2
    exit 1
}

exists() {
    [[ -e $1 || -L $1 ]]
}

# Sample input: 1.0.0 or 1.0.0-rc.1.
valid_version() {
    [[ $1 =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+([.-][0-9A-Za-z]+)*)?$ ]]
}

# Sample input: version = "1.0.0" in crates/daemon/Cargo.toml.
repo_version() {
    sed -n 's/^version = "\([^"]*\)"$/\1/p' "$1/crates/daemon/Cargo.toml"
}

setup_paths() {
    [[ $(id -u) != 0 ]] || fail 'run as your own user'
    [[ ${HOME:-} == /?* && -d $HOME && -O $HOME ]] || fail 'HOME must be an absolute directory owned by the user'
    [[ ${XDG_RUNTIME_DIR:-} == /?* && -d $XDG_RUNTIME_DIR && -O $XDG_RUNTIME_DIR ]] ||
        fail 'XDG_RUNTIME_DIR must be an absolute directory owned by the user'
    home_real=$(realpath -e -- "$HOME")
    runtime_real=$(realpath -e -- "$XDG_RUNTIME_DIR")
    [[ $home_real != / && $runtime_real != / ]] || fail 'HOME and XDG_RUNTIME_DIR must be user directories'

    config_home=${XDG_CONFIG_HOME:-$HOME/.config}
    data_home=${XDG_DATA_HOME:-$HOME/.local/share}
    state_home=${XDG_STATE_HOME:-$HOME/.local/state}
    local dir
    for dir in "$config_home" "$data_home" "$state_home"; do
        [[ $dir == /?* ]] || fail "XDG directories must be absolute: $dir"
    done
    config_home=$(realpath -m -- "$config_home")
    data_home=$(realpath -m -- "$data_home")
    state_home=$(realpath -m -- "$state_home")
    [[ $config_home != / && $data_home != / && $state_home != / ]] || fail 'XDG paths must be user directories'
    app="$data_home/pods/app"
    bin_dir="$HOME/.local/bin"
    unit="$config_home/systemd/user/podsd.service"
    default_data_home=$(realpath -m -- "$HOME/.local/share")
    shadow_units=()
    local candidate
    for dir in "$data_home" "$default_data_home"; do
        candidate="$dir/systemd/user/podsd.service"
        if [[ $candidate != "$unit" && $candidate != "${shadow_units[0]:-}" ]]; then
            shadow_units+=("$candidate")
        fi
    done
    omarchy_dir="$config_home/omarchy"
    plugin_link="$omarchy_dir/plugins/superwhisper.pods"
    pods_link="$app/current/bin/pods"
    if [[ -L $data_home/pods || -L $app ]]; then
        fail "refusing a symlinked app directory: $app"
    fi
    exec 9< "$runtime_real"
    flock -x 9
}

link_is() {
    [[ -L $1 && $(readlink -- "$1") == "$2" ]]
}

# Sample input: current -> 1.0.0, relative to the app directory.
version_link() {
    exists "$1" || return 0
    [[ -L $1 ]] || fail "expected a version symlink: $1"
    local target
    target=$(readlink -- "$1")
    if ! valid_version "$target" || [[ ! -d $app/$target || -L $app/$target ]]; then
        fail "unexpected version symlink: $1 -> $target"
    fi
    printf '%s\n' "$target"
}

units() {
    if [[ ${PODS_SKIP_SERVICE:-0} != 1 ]]; then
        systemctl --user "$@"
    fi
}

readonly path_open='# >>> Pods: pods on PATH >>>'
readonly path_close='# <<< Pods: pods on PATH <<<'
# shellcheck disable=SC2016
readonly path_export='export PATH="$HOME/.local/bin:$PATH"'
# shellcheck disable=SC2016
readonly path_env='PATH=${HOME}/.local/bin:${PATH}'
# shellcheck disable=SC2016
readonly path_fish='fish_add_path "$HOME/.local/bin"'

path_file() {
    printf '%s\n' "$path_open" "$2" "$path_close" | cmp -s - "$1"
}

write_path_file() {
    local file=$1
    if exists "$file"; then
        path_file "$file" "$2" || printf 'pods: left existing PATH file %s\n' "$file" >&2
        return 0
    fi
    mkdir -p -- "$(dirname -- "$file")" &&
        (set -o noclobber; printf '%s\n' "$path_open" "$2" "$path_close" > "$file")
}

# Sample input: a shell rc file containing the exact three-line Pods block.
remove_rc_block() {
    local file=$1 line pending=0 scratch
    [[ -f $file ]] && grep -Fqx -- "$path_open" "$file" || return 0
    scratch=$(mktemp "$XDG_RUNTIME_DIR/pods-path.XXXXXX") || return 1
    while IFS= read -r line || [[ -n $line ]]; do
        if ((pending == 2)); then
            pending=0
            [[ $line == "$path_close" ]] && continue
            printf '%s\n' "$path_open" "$path_export"
        elif ((pending == 1)); then
            if [[ $line == "$path_export" ]]; then
                pending=2
                continue
            fi
            pending=0
            printf '%s\n' "$path_open"
        fi
        if [[ $line == "$path_open" ]]; then
            pending=1
        else
            printf '%s\n' "$line"
        fi
    done < "$file" > "$scratch"
    if ((pending >= 1)); then printf '%s\n' "$path_open" >> "$scratch"; fi
    if ((pending == 2)); then printf '%s\n' "$path_export" >> "$scratch"; fi
    if ! cmp -s -- "$scratch" "$file"; then
        cat -- "$scratch" > "$file" || printf 'pods: could not update %s\n' "$file" >&2
    fi
    rm -f -- "$scratch"
}
