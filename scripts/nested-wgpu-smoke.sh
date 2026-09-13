#!/usr/bin/env bash
# Accelerated-client smoke matrix for the nested wgpu/Vulkan compositor.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACTS="${FOCALDESK_WGPU_SMOKE_ARTIFACTS:-$ROOT/target/nested-wgpu-smoke}"
CLIENT_SECONDS="${FOCALDESK_WGPU_SMOKE_CLIENT_SECONDS:-6}"
BUILD=1
COMPOSITOR_PID=""
TEMP_XDG=""
DESKTOP_GPU_CLIENT_RAN=0

if [[ "${1:-}" == "--no-build" ]]; then
    BUILD=0
elif [[ -n "${1:-}" ]]; then
    echo "Usage: scripts/nested-wgpu-smoke.sh [--no-build]" >&2
    exit 2
fi

cleanup() {
    local status=$?
    if [[ -n "$COMPOSITOR_PID" ]] && kill -0 "$COMPOSITOR_PID" 2>/dev/null; then
        kill "$COMPOSITOR_PID" 2>/dev/null || true
        wait "$COMPOSITOR_PID" 2>/dev/null || true
    fi
    if [[ -n "$TEMP_XDG" && -d "$TEMP_XDG" ]]; then
        rm -rf -- "$TEMP_XDG"
    fi
    exit "$status"
}
trap cleanup EXIT INT TERM

wait_for_log() {
    local pattern=$1
    local attempts=${2:-300}
    local attempt
    for ((attempt = 0; attempt < attempts; attempt++)); do
        grep -Eq "$pattern" "$ARTIFACTS/compositor.log" 2>/dev/null && return 0
        if [[ -n "$COMPOSITOR_PID" ]] && ! kill -0 "$COMPOSITOR_PID" 2>/dev/null; then
            return 1
        fi
        sleep 0.1
    done
    return 1
}

pass() {
    echo "PASS $*" | tee -a "$ARTIFACTS/summary.txt"
}

fail() {
    echo "FAIL $*" | tee -a "$ARTIFACTS/summary.txt" >&2
    tail -100 "$ARTIFACTS/compositor.log" >&2 || true
    exit 1
}

[[ -n "${XDG_RUNTIME_DIR:-}" && -n "${WAYLAND_DISPLAY:-}" ]] \
    || fail "an existing Wayland host is required"
[[ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]] || fail "host Wayland socket is unavailable"

mkdir -p "$ARTIFACTS"
ARTIFACTS="$(cd "$ARTIFACTS" && pwd)"
rm -f -- "$ARTIFACTS"/*.log "$ARTIFACTS/summary.txt"
TEMP_XDG="$(mktemp -d "${TMPDIR:-/tmp}/focaldesk-wgpu-smoke.XXXXXX")"
mkdir -p "$TEMP_XDG/config" "$TEMP_XDG/state" "$TEMP_XDG/cache"
chmod 700 "$TEMP_XDG"

cd "$ROOT"
if [[ "$BUILD" -eq 1 ]]; then
    cargo build -p focaldesk-desktop --no-default-features --features wgpu
    pass "wgpu compositor build"
fi

HOST_DISPLAY="$WAYLAND_DISPLAY"
export XDG_CONFIG_HOME="$TEMP_XDG/config"
export XDG_STATE_HOME="$TEMP_XDG/state"
export XDG_CACHE_HOME="$TEMP_XDG/cache"
export FOCALDESK_LOG_FILE="$ARTIFACTS/compositor.log"
export FOCALDESK_DISABLE_PORTAL_ENV=1
export RUST_LOG="${RUST_LOG:-focaldesk=info,wgpu_core=warn,wgpu_hal=warn}"

WAYLAND_DISPLAY="$HOST_DISPLAY" "$ROOT/target/debug/focaldesk-desktop" \
    >"$ARTIFACTS/compositor.stderr.log" 2>&1 &
COMPOSITOR_PID=$!
wait_for_log 'FocalDesk client socket is focaldesk-[0-9]+' \
    || fail "wgpu compositor did not announce a client socket"
NESTED_DISPLAY="$(
    grep -Eo 'FocalDesk client socket is focaldesk-[0-9]+' "$ARTIFACTS/compositor.log" \
        | tail -1 | sed -E 's/.* is (focaldesk-[0-9]+)$/\1/'
)"
[[ -S "$XDG_RUNTIME_DIR/$NESTED_DISPLAY" ]] || fail "nested socket is unavailable"
wait_for_log 'initialized nested wgpu Vulkan compositor' \
    || fail "Vulkan renderer did not initialize"
pass "wgpu compositor startup ($NESTED_DISPLAY)"

if grep -Eq 'direct_dmabuf_import=true' "$ARTIFACTS/compositor.log"; then
    pass "Vulkan external-memory DMA-BUF capability"
else
    echo "SKIP Vulkan external-memory DMA-BUF unsupported" | tee -a "$ARTIFACTS/summary.txt"
fi
if grep -Eq 'dmabuf_modifier_count=[1-9][0-9]*' "$ARTIFACTS/compositor.log"; then
    pass "non-linear DRM modifiers advertised"
else
    echo "SKIP adapter exposes no importable non-linear modifier" | tee -a "$ARTIFACTS/summary.txt"
fi
if grep -Eq 'explicit_sync=true' "$ARTIFACTS/compositor.log"; then
    pass "linux-drm-syncobj-v1 enabled"
else
    echo "SKIP DRM syncobj eventfd unsupported or render node unavailable" \
        | tee -a "$ARTIFACTS/summary.txt"
fi

run_client() {
    local name=$1
    shift
    set +e
    timeout --kill-after=2 "$CLIENT_SECONDS" env WAYLAND_DISPLAY="$NESTED_DISPLAY" \
        "$@" >"$ARTIFACTS/$name.log" 2>&1
    local status=$?
    set -e
    if [[ "$status" -ne 0 && "$status" -ne 124 && "$status" -ne 137 ]]; then
        fail "$name exited with status $status"
    fi
    kill -0 "$COMPOSITOR_PID" 2>/dev/null || fail "compositor exited while running $name"
    pass "$name connection and render survival"
}

if command -v weston-simple-dmabuf-egl >/dev/null; then
    run_client weston-dmabuf weston-simple-dmabuf-egl
else
    echo "SKIP weston-simple-dmabuf-egl unavailable" | tee -a "$ARTIFACTS/summary.txt"
fi

if command -v gtk4-demo >/dev/null; then
    run_client gtk4 env GDK_BACKEND=wayland gtk4-demo
    DESKTOP_GPU_CLIENT_RAN=1
else
    echo "SKIP gtk4-demo unavailable" | tee -a "$ARTIFACTS/summary.txt"
fi

CHROMIUM=""
for candidate in google-chrome-stable google-chrome chromium chromium-browser; do
    if command -v "$candidate" >/dev/null; then
        CHROMIUM="$candidate"
        break
    fi
done
if [[ -n "$CHROMIUM" ]]; then
    run_client chromium "$CHROMIUM" --ozone-platform=wayland \
        --disable-features=Vulkan \
        --user-data-dir="$TEMP_XDG/chromium" --no-first-run \
        --no-default-browser-check about:blank
    DESKTOP_GPU_CLIENT_RAN=1
else
    echo "SKIP Chromium/Chrome unavailable" | tee -a "$ARTIFACTS/summary.txt"
fi

wait_for_log 'wgpu DMA-BUF textures: direct_imports=[1-9][0-9]*' 100 \
    || fail "no direct Vulkan DMA-BUF import was observed"
if grep -Eq 'directly imported Vulkan DMA-BUF texture.*modifier=[1-9][0-9]*' \
    "$ARTIFACTS/compositor.log"; then
    pass "non-linear DMA-BUF imported directly"
elif [[ "$DESKTOP_GPU_CLIENT_RAN" -eq 1 ]] \
    && grep -Eq 'dmabuf_modifier_count=[1-9][0-9]*' "$ARTIFACTS/compositor.log"; then
    fail "non-linear modifiers were advertised but no non-linear direct import was observed"
else
    echo "SKIP no desktop GPU client exercised a non-linear modifier" \
        | tee -a "$ARTIFACTS/summary.txt"
fi
if grep -Eq 'wgpu explicit-sync acquire point observed' "$ARTIFACTS/compositor.log"; then
    pass "explicit-sync acquire/release path exercised"
elif grep -Eq 'explicit_sync=true' "$ARTIFACTS/compositor.log"; then
    echo "SKIP clients did not submit explicit sync points" | tee -a "$ARTIFACTS/summary.txt"
fi
if grep -Eqi 'panicked at|segmentation fault|device lost|validation error|VUID-' \
    "$ARTIFACTS/compositor.log" "$ARTIFACTS/compositor.stderr.log"; then
    fail "renderer crash or validation signature found"
fi
pass "direct Vulkan DMA-BUF import observed"
pass "no renderer crash or validation signature"
