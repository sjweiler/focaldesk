#!/usr/bin/env bash
# Isolated desktop/session bus; every app/service test uses private fixtures.
set -euo pipefail
if [[ "${FOCALDESK_UI_PRIVATE_BUS:-}" != 1 ]]; then
    exec dbus-run-session -- env FOCALDESK_UI_PRIVATE_BUS=1 bash "$0" "$@"
fi
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"
runtime=$(mktemp -d /tmp/focaldesk-settings-session.XXXXXX)
chmod 700 "$runtime"
host_pid=''
cleanup() {
    if [[ -n "$host_pid" ]]; then
        kill "$host_pid" 2>/dev/null || true
        wait "$host_pid" 2>/dev/null || true
    fi
    # Keep logs and artifacts for failed runs as well as successes.
    printf 'Desktop session evidence: %s\n' "$runtime"
}
trap cleanup EXIT
export XDG_RUNTIME_DIR="$runtime"
export WAYLAND_DISPLAY=focaldesk-settings-test
export GDK_BACKEND=wayland
export LIBGL_ALWAYS_SOFTWARE=1
unset DISPLAY
export XDG_CONFIG_HOME="$runtime/config"
export XDG_DATA_HOME="$runtime/data"
export XDG_CACHE_HOME="$runtime/cache"
export XDG_STATE_HOME="$runtime/state"
if command -v weston >/dev/null; then
    weston --backend=headless-backend.so --use-gl --socket="$WAYLAND_DISPLAY" --idle-time=0 --width=1400 --height=1100 >"$runtime/host.log" 2>&1 &
elif command -v kwin_wayland >/dev/null; then
    QT_QPA_PLATFORM=offscreen kwin_wayland --virtual --no-lockscreen --no-global-shortcuts --no-kactivities --socket "$WAYLAND_DISPLAY" --width 1400 --height 1100 >"$runtime/host.log" 2>&1 &
else
    echo 'Install Weston or KWin to run the isolated Settings UI suite.' >&2
    exit 1
fi
host_pid=$!
for _ in {1..100}; do
    [[ -S "$runtime/$WAYLAND_DISPLAY" ]] && break
    kill -0 "$host_pid" 2>/dev/null || { cat "$runtime/host.log"; exit 1; }
    sleep 0.1
done
[[ -S "$runtime/$WAYLAND_DISPLAY" ]]
python3 apps/focaldesk-settings/tests/native_ui.py
