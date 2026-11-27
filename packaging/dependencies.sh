#!/usr/bin/env bash
# Resolve the system packages needed by the Pods binary.

pods_dependencies() {
    local action=${1:-} manager package
    local -a packages command prefix
    case "$action" in
        check|install) ;;
        *) printf 'Usage: dependencies.sh install|check\n' >&2; return 2 ;;
    esac
    [[ ${PODS_SKIP_PACKAGES:-0} != 1 ]] || return 0
    for manager in pacman apt-get dnf zypper; do
        command -v "$manager" >/dev/null && break
    done

    case "$manager" in
        pacman)
            packages=(pipewire pipewire-pulse bluez)
            command=(pacman -S --needed --noconfirm)
            ;;
        apt-get)
            packages=(pipewire pipewire-pulse bluez)
            command=(apt-get install -y)
            ;;
        dnf)
            packages=(pipewire pipewire-pulseaudio bluez)
            command=(dnf install -y)
            ;;
        zypper)
            if ! command -v zypper >/dev/null; then
                printf 'pods: no supported package manager found. On NixOS, use the Pods flake.\n' >&2
                return 1
            fi
            packages=(pipewire pipewire-pulseaudio bluez)
            command=(zypper --non-interactive install)
            ;;
    esac
    if [[ $action == check ]]; then
        case "$manager" in
            pacman) pacman -Q "${packages[@]}" ;;
            apt-get)
                for package in "${packages[@]}"; do
                    # Sample input: install ok installed.
                    [[ $(dpkg-query -W -f '${Status}' "$package" 2>/dev/null) == 'install ok installed' ]] ||
                        { printf 'pods: missing package %s\n' "$package" >&2; return 1; }
                done
                ;;
            dnf|zypper) rpm -q "${packages[@]}" ;;
        esac
        return
    fi

    if pods_dependencies check >/dev/null 2>&1; then
        return 0
    fi
    if command -v omarchy >/dev/null; then
        omarchy pkg add "${packages[@]}"
        return
    fi
    prefix=()
    if [[ $(id -u) != 0 ]]; then
        command -v sudo >/dev/null || { printf 'pods: install these packages with your package manager: %s\n' "${packages[*]}" >&2; return 1; }
        prefix=(sudo)
    fi
    "${prefix[@]}" "${command[@]}" "${packages[@]}"
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    [[ $# == 1 ]] || { printf 'Usage: dependencies.sh install|check\n' >&2; exit 2; }
    pods_dependencies "$@"
fi
