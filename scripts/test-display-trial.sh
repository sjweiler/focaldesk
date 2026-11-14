#!/usr/bin/env bash
# Exercise compositor-owned display recovery inside a private nested session.
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"
if [[ "${1:-}" != "--no-build" ]]; then
    cargo build -p focaldesk-desktop --no-default-features --features winit,xwayland
    cargo build -p focaldesk-cli
fi
if [[ -z "${WAYLAND_DISPLAY:-}" ]]; then
    while IFS='=' read -r key value; do
        case "$key" in
            WAYLAND_DISPLAY|DISPLAY|XDG_RUNTIME_DIR|DBUS_SESSION_BUS_ADDRESS) export "$key=$value" ;;
        esac
    done < <(systemctl --user show-environment 2>/dev/null || true)
fi
host_pid=""
private_runtime=""
cleanup() {
    if [[ -n "$host_pid" ]]; then
        kill "$host_pid" 2>/dev/null || true
        wait "$host_pid" 2>/dev/null || true
    fi
    if [[ -n "$private_runtime" ]]; then rm -rf -- "$private_runtime"; fi
}
trap cleanup EXIT
if [[ -z "${WAYLAND_DISPLAY:-}" || ! -S "${XDG_RUNTIME_DIR:-/nonexistent}/${WAYLAND_DISPLAY:-missing}" ]]; then
    command -v weston >/dev/null || { echo "A Wayland session or Weston is required" >&2; exit 1; }
    private_runtime=$(mktemp -d /tmp/focaldesk-display-host.XXXXXX)
    chmod 700 "$private_runtime"
    export XDG_RUNTIME_DIR="$private_runtime"
    export WAYLAND_DISPLAY=focaldesk-display-host
    export LIBGL_ALWAYS_SOFTWARE=1
    weston --backend=headless-backend.so --use-gl --socket="$WAYLAND_DISPLAY" --idle-time=0 \
        --width=1280 --height=720 >"$private_runtime/weston.log" 2>&1 &
    host_pid=$!
    for ((attempt=0; attempt<150; attempt++)); do
        [[ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]] && break
        kill -0 "$host_pid" 2>/dev/null || { cat "$private_runtime/weston.log" >&2; exit 1; }
        sleep .1
    done
    [[ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]] || { cat "$private_runtime/weston.log" >&2; exit 1; }
fi
python3 apps/focaldesk-settings/tests/display_runtime.py
