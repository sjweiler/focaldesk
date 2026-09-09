#!/usr/bin/env bash
# Capture repeatable evidence from a real FocalDesk DRM/KMS session.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACTS="${FOCALDESK_DRM_ARTIFACTS:-$ROOT/target/drm-hardware}"
CLI="${FOCALDESK_CLI:-$ROOT/target/release/focaldesk-cli}"

usage() {
    cat <<'EOF'
Usage: scripts/drm-hardware-regression.sh capture STAGE
       scripts/drm-hardware-regression.sh verify

Stages: baseline, hotplug, mixed-mode, resumed

Run capture from a real FocalDesk DRM/KMS session at each stage. This harness
never changes display configuration or suspends the machine itself.
EOF
}

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

snapshot() {
    local stage=$1
    case "$stage" in
        baseline|hotplug|mixed-mode|resumed) ;;
        *) fail "unknown stage: $stage" ;;
    esac

    command -v jq >/dev/null || fail "jq is required"
    mkdir -p "$ARTIFACTS"
    [[ -x "$CLI" ]] || fail "missing CLI; run cargo build --release -p focaldesk-cli"
    "$CLI" desktop-snapshot >"$ARTIFACTS/$stage.json"
    jq -e '.rendering.backend == "drm" and .rendering.compositor_ready == true' \
        "$ARTIFACTS/$stage.json" >/dev/null \
        || fail "$stage was not captured from a ready DRM compositor"
    jq -e '.outputs | length > 0' "$ARTIFACTS/$stage.json" >/dev/null \
        || fail "$stage has no outputs"

    {
        printf 'stage=%s\n' "$stage"
        printf 'revision=%s\n' "$(git -C "$ROOT" rev-parse HEAD)"
        printf 'captured_at=%s\n' "$(date --iso-8601=seconds)"
        uname -a
        if [[ -r /etc/os-release ]]; then
            sed -n 's/^\(ID\|VERSION_ID\|PRETTY_NAME\)=/os_\1=/p' /etc/os-release
        fi
        command -v lspci >/dev/null && lspci -nn | grep -Ei 'vga|3d|display' || true
    } >"$ARTIFACTS/$stage.environment.txt"

    journalctl --user -b --no-pager -o short-iso \
        -u focaldesk-session.target >"$ARTIFACTS/$stage.user-journal.txt" 2>/dev/null || true
    journalctl -b --no-pager -o short-iso _COMM=focaldesk-desktop \
        >"$ARTIFACTS/$stage.compositor-journal.txt" 2>/dev/null || true
    printf 'PASS: captured %s in %s\n' "$stage" "$ARTIFACTS"
}

verify() {
    command -v jq >/dev/null || fail "jq is required"
    local stage
    for stage in baseline hotplug mixed-mode resumed; do
        [[ -s "$ARTIFACTS/$stage.json" ]] || fail "missing $stage capture"
        jq -e '.rendering.backend == "drm" and (.outputs | length > 0)' \
            "$ARTIFACTS/$stage.json" >/dev/null || fail "invalid $stage capture"
    done

    jq -s -e '
        def topology: [.outputs[] | {
            connector, make, model, serial, width, height, refresh_mhz, x, y, scale
        }] | sort_by(.connector);
        (.[0] | topology) != (.[1] | topology)
    ' \
        "$ARTIFACTS/baseline.json" "$ARTIFACTS/hotplug.json" >/dev/null \
        || fail "hotplug capture did not change output topology"
    jq -e '[.outputs[].scale] | unique | length > 1' \
        "$ARTIFACTS/mixed-mode.json" >/dev/null \
        || fail "mixed-mode capture does not contain multiple scales"
    jq -e '[.outputs[].refresh_mhz] | unique | length > 1' \
        "$ARTIFACTS/mixed-mode.json" >/dev/null \
        || fail "mixed-mode capture does not contain multiple refresh rates"
    jq -e '.rendering.compositor_ready == true' "$ARTIFACTS/resumed.json" >/dev/null \
        || fail "compositor was not ready after resume"

    if grep -Eqi 'panicked at|segmentation fault|first post-resume.*fail' \
        "$ARTIFACTS"/*.compositor-journal.txt; then
        fail "crash or post-resume failure found in compositor journal"
    fi
    printf 'PASS: DRM hotplug, mixed-scale/refresh, and resume evidence verified\n'
}

[[ $# -ge 1 ]] || { usage; exit 2; }
case "$1" in
    capture) [[ $# -eq 2 ]] || { usage; exit 2; }; snapshot "$2" ;;
    verify) [[ $# -eq 1 ]] || { usage; exit 2; }; verify ;;
    -h|--help) usage ;;
    *) usage >&2; exit 2 ;;
esac
