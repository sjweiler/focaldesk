#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
scope="all"
assume_yes=false
purge=false
manage_services=true
user_root="${FOCALDESK_UNINSTALL_USER_ROOT:-${HOME}}"
system_root="${FOCALDESK_UNINSTALL_SYSTEM_ROOT:-/}"

usage() {
    echo "Usage: $0 [--scope user|system|all] [--yes] [--purge] [--no-services]"
    echo "Removes only paths recorded in FocalDesk's install manifests."
    echo "User settings and state are preserved unless --purge is supplied."
}

while (($#)); do
    case "$1" in
        --scope) scope="${2:-}"; shift 2 ;;
        --yes) assume_yes=true; shift ;;
        --purge) purge=true; shift ;;
        --no-services) manage_services=false; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

case "$scope" in user|system|all) ;; *) echo "Invalid scope: $scope" >&2; exit 2 ;; esac

if ! $assume_yes; then
    if [[ ! -t 0 ]]; then
        echo "Refusing non-interactive uninstall without --yes" >&2
        exit 2
    fi
    read -r -p "Remove FocalDesk ${scope} installation files? [y/N] " answer
    [[ "$answer" == "y" || "$answer" == "Y" ]] || exit 0
fi

user_units=(
    focaldesk-session.target focaldesk-server.service focaldesk-remoted.service
    focaldesk-system-rail.service focaldesk-task-shelf.service focaldesk-powerd.service
    focaldesk-notificationsd.service focaldesk-updatesd.service focaldesk-dialogd.service
    focaldesk-controlsd.service focal-launchd.service focaldesk-settingsd.service
    focaldesk-automation.service focaldesk-portald.service focald-secrets.service
    focald-secrets.socket focald-secrets-import.service focald-voice.service
    focald-speech.service focald-mic.service
)

if $manage_services && [[ "$scope" != system ]] && command -v systemctl >/dev/null; then
    systemctl --user disable --now "${user_units[@]}" >/dev/null 2>&1 || true
fi

remove_manifest() {
    local root="$1" manifest="$2" privileged="$3"
    local mode relative target
    while read -r mode relative; do
        [[ -n "${mode:-}" && "${mode:0:1}" != "#" ]] || continue
        if [[ "$relative" == /* || "$relative" == *".."* ]]; then
            echo "Unsafe manifest path: $relative" >&2
            exit 1
        fi
        target="${root%/}/$relative"
        if [[ -e "$target" || -L "$target" ]]; then
            if [[ "$privileged" == true && "$root" == "/" ]]; then
                sudo rm -f -- "$target"
            else
                rm -f -- "$target"
            fi
            echo "Removed $target"
        fi
    done < "$manifest"
}

if [[ "$scope" == user || "$scope" == all ]]; then
    remove_manifest "$user_root" "$repo_root/packaging/install-manifest-user.txt" false
fi
if [[ "$scope" == system || "$scope" == all ]]; then
    remove_manifest "$system_root" "$repo_root/packaging/install-manifest-system.txt" true
fi

if $purge; then
    state_paths=(
        "$user_root/.config/focaldesk"
        "$user_root/.local/share/focaldesk"
        "$user_root/.local/state/focaldesk"
        "$user_root/.cache/focaldesk"
    )
    for state_path in "${state_paths[@]}"; do
        [[ "$state_path" == "$user_root"/* ]] || { echo "Unsafe purge path" >&2; exit 1; }
        if [[ -e "$state_path" ]]; then
            rm -rf -- "$state_path"
            echo "Purged $state_path"
        fi
    done
else
    echo "Preserved FocalDesk user settings, secrets, themes, logs, and state."
fi

if $manage_services && command -v systemctl >/dev/null; then
    systemctl --user daemon-reload >/dev/null 2>&1 || true
    if [[ "$scope" != user && "$system_root" == "/" ]]; then
        sudo systemctl daemon-reload >/dev/null 2>&1 || true
    fi
fi
