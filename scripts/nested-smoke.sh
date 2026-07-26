#!/usr/bin/env bash
# Repeatable nested compositor compatibility smoke test.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACTS="${FOCALDESK_SMOKE_ARTIFACTS:-$ROOT/target/nested-smoke}"
START_TIMEOUT="${FOCALDESK_SMOKE_START_TIMEOUT:-30}"
CLIENT_SECONDS="${FOCALDESK_SMOKE_CLIENT_SECONDS:-3}"
BUILD=1
HOST_PID=""
COMPOSITOR_PID=""
TEMP_RUNTIME=""
TEMP_XDG=""

usage() {
    cat <<'EOF'
Usage: scripts/nested-smoke.sh [--no-build] [--artifacts DIR]

Runs FocalDesk inside the current Wayland session. If no host Wayland display
is available, the script starts a private headless Weston session.

Environment:
  FOCALDESK_SMOKE_CLIENT          Optional native Wayland client command
  FOCALDESK_SMOKE_START_TIMEOUT   Compositor startup timeout (default: 30)
  FOCALDESK_SMOKE_CLIENT_SECONDS  Client observation time (default: 3)
EOF
}

while (($#)); do
    case "$1" in
        --no-build)
            BUILD=0
            shift
            ;;
        --artifacts)
            [[ $# -ge 2 ]] || { echo "--artifacts requires a directory" >&2; exit 2; }
            ARTIFACTS="$2"
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

cleanup() {
    local status=$?
    if [[ -n "$COMPOSITOR_PID" ]] && kill -0 "$COMPOSITOR_PID" 2>/dev/null; then
        kill "$COMPOSITOR_PID" 2>/dev/null || true
        wait "$COMPOSITOR_PID" 2>/dev/null || true
    fi
    if [[ -n "$HOST_PID" ]] && kill -0 "$HOST_PID" 2>/dev/null; then
        kill "$HOST_PID" 2>/dev/null || true
        wait "$HOST_PID" 2>/dev/null || true
    fi
    if [[ -n "$TEMP_RUNTIME" && -d "$TEMP_RUNTIME" ]]; then
        rm -rf -- "$TEMP_RUNTIME"
    fi
    if [[ -n "$TEMP_XDG" && -d "$TEMP_XDG" ]]; then
        rm -rf -- "$TEMP_XDG"
    fi
    exit "$status"
}
trap cleanup EXIT INT TERM

mkdir -p "$ARTIFACTS"
ARTIFACTS="$(cd "$ARTIFACTS" && pwd)"
rm -f -- "$ARTIFACTS/compositor.log" "$ARTIFACTS/compositor.stderr" \
    "$ARTIFACTS/host.log" "$ARTIFACTS/wayland-info.txt" "$ARTIFACTS/summary.txt"
TEMP_XDG="$(mktemp -d "${TMPDIR:-/tmp}/focaldesk-smoke-xdg.XXXXXX")"
chmod 700 "$TEMP_XDG"
mkdir -p "$TEMP_XDG/config" "$TEMP_XDG/state" "$TEMP_XDG/cache"

pass() {
    echo "PASS $*" | tee -a "$ARTIFACTS/summary.txt"
}

fail() {
    echo "FAIL $*" | tee -a "$ARTIFACTS/summary.txt" >&2
    if [[ -f "$ARTIFACTS/compositor.stderr" ]]; then
        tail -80 "$ARTIFACTS/compositor.stderr" >&2 || true
    fi
    if [[ -f "$ARTIFACTS/compositor.log" ]]; then
        tail -80 "$ARTIFACTS/compositor.log" >&2 || true
    fi
    if [[ -f "$ARTIFACTS/client.log" ]]; then
        tail -40 "$ARTIFACTS/client.log" >&2 || true
    fi
    if [[ -f "$ARTIFACTS/host.log" ]]; then
        tail -80 "$ARTIFACTS/host.log" >&2 || true
    fi
    exit 1
}

wait_for_path() {
    local path=$1
    local seconds=$2
    local attempts=$((seconds * 10))
    local attempt
    for ((attempt = 0; attempt < attempts; attempt++)); do
        [[ -S "$path" ]] && return 0
        sleep 0.1
    done
    return 1
}

wait_for_log() {
    local pattern=$1
    local seconds=$2
    local attempts=$((seconds * 10))
    local attempt
    for ((attempt = 0; attempt < attempts; attempt++)); do
        if grep -Eq "$pattern" "$ARTIFACTS/compositor.log" \
            "$ARTIFACTS/compositor.stderr" 2>/dev/null; then
            return 0
        fi
        if [[ -n "$COMPOSITOR_PID" ]] && ! kill -0 "$COMPOSITOR_PID" 2>/dev/null; then
            # Give redirected output a moment to flush so a just-written startup
            # marker or renderer error is visible before reporting the failure.
            sleep 0.1
            if grep -Eq "$pattern" "$ARTIFACTS/compositor.log" \
                "$ARTIFACTS/compositor.stderr" 2>/dev/null; then
                return 0
            fi
            return 1
        fi
        sleep 0.1
    done
    return 1
}

cd "$ROOT"

if [[ "$BUILD" -eq 1 ]]; then
    cargo build -p focaldesk-desktop --no-default-features --features winit,xwayland
    pass "nested compositor build"
fi

COMPOSITOR="$ROOT/target/debug/focaldesk-desktop"
[[ -x "$COMPOSITOR" ]] || fail "missing compositor binary: $COMPOSITOR"

if [[ -z "${XDG_RUNTIME_DIR:-}" || -z "${WAYLAND_DISPLAY:-}" \
    || ! -S "${XDG_RUNTIME_DIR:-/nonexistent}/${WAYLAND_DISPLAY:-missing}" ]]; then
    command -v weston >/dev/null || fail "no host Wayland display and weston is unavailable"
    TEMP_RUNTIME="$(mktemp -d "${TMPDIR:-/tmp}/focaldesk-smoke-runtime.XXXXXX")"
    chmod 700 "$TEMP_RUNTIME"
    export XDG_RUNTIME_DIR="$TEMP_RUNTIME"
    export WAYLAND_DISPLAY="focaldesk-host"
    weston --backend=headless-backend.so --use-gl --socket="$WAYLAND_DISPLAY" --idle-time=0 \
        --width=1280 --height=720 >"$ARTIFACTS/host.log" 2>&1 &
    HOST_PID=$!
    wait_for_path "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" 15 \
        || fail "headless Weston did not create its Wayland socket"
    pass "private headless Wayland host"
else
    pass "existing Wayland host $WAYLAND_DISPLAY"
fi

HOST_DISPLAY="$WAYLAND_DISPLAY"
export XDG_CONFIG_HOME="$TEMP_XDG/config"
export XDG_STATE_HOME="$TEMP_XDG/state"
export XDG_CACHE_HOME="$TEMP_XDG/cache"
export FOCALDESK_LOG_FILE="$ARTIFACTS/compositor.log"
export FOCALDESK_DISABLE_PORTAL_ENV=1
export RUST_LOG="${RUST_LOG:-focaldesk=debug,smithay=error}"

WAYLAND_DISPLAY="$HOST_DISPLAY" "$COMPOSITOR" \
    >"$ARTIFACTS/compositor.stderr" 2>&1 &
COMPOSITOR_PID=$!

wait_for_log 'FocalDesk client socket is focaldesk-[0-9]+' "$START_TIMEOUT" \
    || fail "nested compositor did not announce a client socket"

NESTED_DISPLAY="$(
    grep -hEo 'FocalDesk client socket is focaldesk-[0-9]+' \
        "$ARTIFACTS/compositor.log" "$ARTIFACTS/compositor.stderr" \
        | tail -1 | sed -E 's/.* is (focaldesk-[0-9]+)$/\1/'
)"
[[ -n "$NESTED_DISPLAY" ]] || fail "could not parse nested Wayland display"
wait_for_path "$XDG_RUNTIME_DIR/$NESTED_DISPLAY" 5 \
    || fail "nested Wayland socket $NESTED_DISPLAY is missing"
kill -0 "$COMPOSITOR_PID" 2>/dev/null || fail "compositor exited after startup"
pass "nested socket $NESTED_DISPLAY"

if command -v wayland-info >/dev/null; then
    if timeout 10 env WAYLAND_DISPLAY="$NESTED_DISPLAY" wayland-info \
        >"$ARTIFACTS/wayland-info.txt" 2>&1; then
        grep -Eq 'interface: .wl_compositor.' "$ARTIFACTS/wayland-info.txt" \
            || fail "wl_compositor global was not advertised"
        grep -Eq 'interface: .xdg_wm_base.' "$ARTIFACTS/wayland-info.txt" \
            || fail "xdg_wm_base global was not advertised"
        pass "Wayland registry and round-trip"
    else
        fail "wayland-info could not complete against FocalDesk"
    fi
else
    echo "SKIP wayland-info unavailable" | tee -a "$ARTIFACTS/summary.txt"
fi

CLIENT="${FOCALDESK_SMOKE_CLIENT:-}"
if [[ -z "$CLIENT" ]]; then
    for candidate in weston-simple-shm gtk4-demo weston-terminal; do
        if command -v "$candidate" >/dev/null; then
            CLIENT="$candidate"
            break
        fi
    done
fi

if [[ -n "$CLIENT" ]]; then
    set +e
    timeout "$CLIENT_SECONDS" env WAYLAND_DISPLAY="$NESTED_DISPLAY" \
        bash -lc "$CLIENT" >"$ARTIFACTS/client.log" 2>&1
    CLIENT_STATUS=$?
    set -e
    if [[ "$CLIENT_STATUS" -ne 0 && "$CLIENT_STATUS" -ne 124 ]]; then
        fail "native client failed with status $CLIENT_STATUS: $CLIENT"
    fi
    kill -0 "$COMPOSITOR_PID" 2>/dev/null || fail "compositor exited while running native client"
    pass "native client connection and render survival ($CLIENT)"
else
    echo "SKIP no native demo client found" | tee -a "$ARTIFACTS/summary.txt"
fi

if grep -Eqi 'panicked at|thread .* panicked|stack backtrace|segmentation fault' \
    "$ARTIFACTS/compositor.log" "$ARTIFACTS/compositor.stderr" 2>/dev/null; then
    fail "panic or crash signature found in compositor logs"
fi
pass "no compositor panic or crash signature"

if grep -Eq 'XWayland ready' "$ARTIFACTS/compositor.log" \
    "$ARTIFACTS/compositor.stderr" 2>/dev/null; then
    pass "XWayland startup"
else
    fail "XWayland did not become ready"
fi

{
    echo "host_display=$HOST_DISPLAY"
    echo "nested_display=$NESTED_DISPLAY"
    echo "compositor=$COMPOSITOR"
    echo "artifacts=$ARTIFACTS"
} >>"$ARTIFACTS/summary.txt"

pass "nested compatibility smoke test complete"
