#!/usr/bin/env bash
# End-to-end split-screen test using a real nested compositor and XDG clients.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACTS="${FOCALDESK_SPLIT_E2E_ARTIFACTS:-$ROOT/target/nested-split-e2e}"
START_TIMEOUT="${FOCALDESK_SPLIT_E2E_START_TIMEOUT:-30}"
BUILD=1
HOST_PID=""
COMPOSITOR_PID=""
CLIENT_A_PID=""
CLIENT_B_PID=""
CLIENT_C_PID=""
CLIENT_D_PID=""
CLIENT_E_PID=""
TEMP_RUNTIME=""
TEMP_XDG=""
COMPOSITOR_LOG=""
COMPOSITOR_STDERR=""
NESTED_DISPLAY=""

if [[ "${1:-}" == "--no-build" ]]; then
    BUILD=0
elif [[ -n "${1:-}" ]]; then
    echo "Usage: scripts/nested-split-e2e.sh [--no-build]" >&2
    exit 2
fi

cleanup() {
    local status=$?
    for pid in "$CLIENT_A_PID" "$CLIENT_B_PID" "$CLIENT_C_PID" "$CLIENT_D_PID" \
        "$CLIENT_E_PID" "$COMPOSITOR_PID" "$HOST_PID"; do
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
    if [[ -n "$TEMP_RUNTIME" && -d "$TEMP_RUNTIME" ]]; then
        rm -rf -- "$TEMP_RUNTIME"
    fi
    if [[ -n "$TEMP_XDG" && -d "$TEMP_XDG" ]]; then
        rm -rf -- "$TEMP_XDG"
    fi
    exit "$status"
}
trap cleanup EXIT INT TERM

pass() {
    echo "PASS $*" | tee -a "$ARTIFACTS/summary.txt"
}

fail() {
    echo "FAIL $*" | tee -a "$ARTIFACTS/summary.txt" >&2
    tail -100 "${COMPOSITOR_STDERR:-$ARTIFACTS/compositor.stderr}" 2>/dev/null || true
    tail -100 "${COMPOSITOR_LOG:-$ARTIFACTS/compositor.log}" 2>/dev/null || true
    exit 1
}

wait_for_path() {
    local path=$1 attempts=$((START_TIMEOUT * 10))
    for ((attempt = 0; attempt < attempts; attempt++)); do
        [[ -S "$path" ]] && return 0
        sleep 0.1
    done
    return 1
}

wait_for_log() {
    local pattern=$1 attempts=$((START_TIMEOUT * 10))
    for ((attempt = 0; attempt < attempts; attempt++)); do
        grep -Eq "$pattern" "$COMPOSITOR_LOG" "$COMPOSITOR_STDERR" \
            2>/dev/null && return 0
        [[ -n "$COMPOSITOR_PID" ]] && kill -0 "$COMPOSITOR_PID" 2>/dev/null || return 1
        sleep 0.1
    done
    return 1
}

start_nested_compositor() {
    local suffix=$1 attempts=$((START_TIMEOUT * 10))
    COMPOSITOR_LOG="$ARTIFACTS/compositor${suffix}.log"
    COMPOSITOR_STDERR="$ARTIFACTS/compositor${suffix}.stderr"
    export FOCALDESK_LOG_FILE="$COMPOSITOR_LOG"
    WAYLAND_DISPLAY="$HOST_DISPLAY" "$COMPOSITOR" >"$COMPOSITOR_STDERR" 2>&1 &
    COMPOSITOR_PID=$!
    wait_for_log 'FocalDesk client socket is focaldesk-[0-9]+' \
        || fail "nested compositor did not announce its client socket"
    NESTED_DISPLAY="$(
        grep -hEo 'FocalDesk client socket is focaldesk-[0-9]+' \
            "$COMPOSITOR_LOG" "$COMPOSITOR_STDERR" \
            | tail -1 | sed -E 's/.* is (focaldesk-[0-9]+)$/\1/'
    )"
    wait_for_path "$XDG_RUNTIME_DIR/$NESTED_DISPLAY" || fail "nested Wayland socket is missing"
    for ((attempt = 0; attempt < attempts; attempt++)); do
        [[ -S "$XDG_RUNTIME_DIR/focaldesk/desktop.sock" ]] && return 0
        sleep 0.1
    done
    fail "desktop IPC socket is missing"
}

stop_nested_session() {
    if [[ -n "$COMPOSITOR_PID" ]] && kill -0 "$COMPOSITOR_PID" 2>/dev/null; then
        kill "$COMPOSITOR_PID" 2>/dev/null || true
        wait "$COMPOSITOR_PID" 2>/dev/null || true
    fi
    COMPOSITOR_PID=""
    for pid in "$CLIENT_A_PID" "$CLIENT_B_PID" "$CLIENT_C_PID" "$CLIENT_D_PID" "$CLIENT_E_PID"; do
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
    CLIENT_A_PID=""
    CLIENT_B_PID=""
    CLIENT_C_PID=""
    CLIENT_D_PID=""
    CLIENT_E_PID=""
}

wait_for_snapshot_direction() {
    local direction=$1 expected=$2 path="$XDG_STATE_HOME/focaldesk/session.json"
    local attempts=$((START_TIMEOUT * 10)) count
    for ((attempt = 0; attempt < attempts; attempt++)); do
        count=$(grep -c "\"direction\": \"$direction\"" "$path" 2>/dev/null || true)
        (( count >= expected )) && return 0
        sleep 0.1
    done
    return 1
}

wait_for_window() {
    local title=$1 attempts=$((START_TIMEOUT * 10))
    for ((attempt = 0; attempt < attempts; attempt++)); do
        "$CLI" window-geometry "$title" >/dev/null 2>&1 && return 0
        sleep 0.1
    done
    return 1
}

geometry() {
    "$CLI" window-geometry "$1"
}

wait_for_split() {
    local expected_total_min=$1 attempts=$((START_TIMEOUT * 10))
    for ((attempt = 0; attempt < attempts; attempt++)); do
        read -r ax ay aw ah < <(geometry split-e2e-a)
        read -r bx by bw bh < <(geometry split-e2e-b)
        if (( ax + aw == bx && ay == by && ah == bh && aw + bw >= expected_total_min )); then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

wait_for_balanced_split() {
    local expected_total_min=$1 attempts=$((START_TIMEOUT * 10)) difference
    for ((attempt = 0; attempt < attempts; attempt++)); do
        read -r ax ay aw ah < <(geometry split-e2e-a)
        read -r bx by bw bh < <(geometry split-e2e-b)
        difference=$((aw - bw))
        (( difference < 0 )) && difference=$((-difference))
        if (( ax + aw == bx && ay == by && ah == bh && aw + bw >= expected_total_min \
            && difference <= 1 )); then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

wait_for_three_columns() {
    local expected_total_min=$1 attempts=$((START_TIMEOUT * 10))
    for ((attempt = 0; attempt < attempts; attempt++)); do
        read -r ax ay aw ah < <(geometry split-e2e-a)
        read -r bx by bw bh < <(geometry split-e2e-b)
        read -r cx cy cw ch < <(geometry split-e2e-c)
        if (( ax + aw == bx && bx + bw == cx && ay == by && by == cy \
            && ah == bh && bh == ch && aw + bw + cw >= expected_total_min )); then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

wait_for_quadrants() {
    local expected_width_min=$1 expected_height_min=$2 attempts=$((START_TIMEOUT * 10))
    for ((attempt = 0; attempt < attempts; attempt++)); do
        read -r ax ay aw ah < <(geometry split-e2e-a)
        read -r bx by bw bh < <(geometry split-e2e-b)
        read -r cx cy cw ch < <(geometry split-e2e-c)
        read -r dx dy dw dh < <(geometry split-e2e-d)
        if (( ax + aw == bx && cx + cw == dx && ay + ah == cy && by + bh == dy \
            && ax == cx && bx == dx && ay == by && cy == dy \
            && aw == cw && bw == dw && ah == bh && ch == dh \
            && aw + bw >= expected_width_min && ah + ch >= expected_height_min )); then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

wait_for_three_quadrants() {
    local expected_width_min=$1 expected_height_min=$2 attempts=$((START_TIMEOUT * 10))
    for ((attempt = 0; attempt < attempts; attempt++)); do
        read -r ax ay aw ah < <(geometry split-e2e-a)
        read -r bx by bw bh < <(geometry split-e2e-b)
        read -r cx cy cw ch < <(geometry split-e2e-c)
        if (( ax + aw == bx && ay + ah == cy && ax == cx && ay == by \
            && aw == cw && ah == bh && aw + bw >= expected_width_min \
            && ah + ch >= expected_height_min )); then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

ratio_is_near() {
    local actual=$1 expected=$2 tolerance=${3:-3}
    (( actual >= expected - tolerance && actual <= expected + tolerance ))
}

wait_for_geometry() {
    local title=$1 expected=$2 attempts=$((START_TIMEOUT * 10))
    for ((attempt = 0; attempt < attempts; attempt++)); do
        [[ "$(geometry "$title")" == "$expected" ]] && return 0
        sleep 0.1
    done
    return 1
}

wait_for_client_configure() {
    local path=$1 width=$2 height=$3 attempts=$((START_TIMEOUT * 10))
    for ((attempt = 0; attempt < attempts; attempt++)); do
        grep -Fxq "$width $height" "$path" 2>/dev/null && return 0
        sleep 0.1
    done
    return 1
}

command -v weston >/dev/null || {
    echo "SKIP weston is required for the nested split E2E test" >&2
    exit 77
}

mkdir -p "$ARTIFACTS"
ARTIFACTS="$(cd "$ARTIFACTS" && pwd)"
rm -f -- "$ARTIFACTS/compositor.log" "$ARTIFACTS/compositor.stderr" \
    "$ARTIFACTS/compositor-restart-1.log" "$ARTIFACTS/compositor-restart-1.stderr" \
    "$ARTIFACTS/compositor-restart-2.log" "$ARTIFACTS/compositor-restart-2.stderr" \
    "$ARTIFACTS/host.log" "$ARTIFACTS/client-a.log" "$ARTIFACTS/client-b.log" \
    "$ARTIFACTS/client-c.log" "$ARTIFACTS/client-d.log" "$ARTIFACTS/client-e.log" \
    "$ARTIFACTS/client-a-configures.txt" "$ARTIFACTS/client-b-configures.txt" \
    "$ARTIFACTS/client-c-configures.txt" "$ARTIFACTS/client-d-configures.txt" \
    "$ARTIFACTS/client-e-configures.txt" \
    "$ARTIFACTS/summary.txt"

TEMP_RUNTIME="$(mktemp -d "${TMPDIR:-/tmp}/focaldesk-split-e2e-runtime.XXXXXX")"
TEMP_XDG="$(mktemp -d "${TMPDIR:-/tmp}/focaldesk-split-e2e-xdg.XXXXXX")"
chmod 700 "$TEMP_RUNTIME" "$TEMP_XDG"
mkdir -p "$TEMP_XDG/config/focaldesk" "$TEMP_XDG/state" "$TEMP_XDG/cache"

cd "$ROOT"
if [[ "$BUILD" -eq 1 ]]; then
    cargo build -p focaldesk-desktop --no-default-features --features winit
    cargo build -p focaldesk-cli -p focaldesk-color-tag-test
    pass "E2E binaries built"
fi

COMPOSITOR="$ROOT/target/debug/focaldesk-desktop"
CLI="$ROOT/target/debug/focaldesk-cli"
CLIENT="$ROOT/target/debug/focaldesk-color-tag-test"
[[ -x "$COMPOSITOR" && -x "$CLI" && -x "$CLIENT" ]] || fail "required binaries are missing"

export XDG_RUNTIME_DIR="$TEMP_RUNTIME"
export WAYLAND_DISPLAY="focaldesk-split-host"
weston --backend=headless-backend.so --use-gl --socket="$WAYLAND_DISPLAY" --idle-time=0 \
    --width=2560 --height=1440 >"$ARTIFACTS/host.log" 2>&1 &
HOST_PID=$!
wait_for_path "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" || fail "headless Weston did not start"
pass "private 2560x1440 Wayland host"

export XDG_CONFIG_HOME="$TEMP_XDG/config"
export XDG_STATE_HOME="$TEMP_XDG/state"
export XDG_CACHE_HOME="$TEMP_XDG/cache"
export FOCALDESK_DISABLE_PORTAL_ENV=1
export RUST_LOG="${RUST_LOG:-focaldesk=debug,smithay=error}"

printf '%s\n' '{"workspaces":{"split_screen_enabled":true,"restore_session":true,"restore_split_layouts":true}}' \
    >"$XDG_CONFIG_HOME/focaldesk/settings.json"

HOST_DISPLAY="$WAYLAND_DISPLAY"
start_nested_compositor ""
pass "nested compositor and trusted IPC"

WAYLAND_DISPLAY="$NESTED_DISPLAY" "$CLIENT" --title split-e2e-a \
    --app-id com.focaldesk.SplitE2eClient.A \
    --configure-log "$ARTIFACTS/client-a-configures.txt" \
    >"$ARTIFACTS/client-a.log" 2>&1 &
CLIENT_A_PID=$!
WAYLAND_DISPLAY="$NESTED_DISPLAY" "$CLIENT" --title split-e2e-b \
    --app-id com.focaldesk.SplitE2eClient.B \
    --configure-log "$ARTIFACTS/client-b-configures.txt" \
    >"$ARTIFACTS/client-b.log" 2>&1 &
CLIENT_B_PID=$!
WAYLAND_DISPLAY="$NESTED_DISPLAY" "$CLIENT" --title split-e2e-c \
    --app-id com.focaldesk.SplitE2eClient.C \
    --configure-log "$ARTIFACTS/client-c-configures.txt" \
    >"$ARTIFACTS/client-c.log" 2>&1 &
CLIENT_C_PID=$!
WAYLAND_DISPLAY="$NESTED_DISPLAY" "$CLIENT" --title split-e2e-d \
    --app-id com.focaldesk.SplitE2eClient.D \
    --configure-log "$ARTIFACTS/client-d-configures.txt" \
    >"$ARTIFACTS/client-d.log" 2>&1 &
CLIENT_D_PID=$!
WAYLAND_DISPLAY="$NESTED_DISPLAY" "$CLIENT" --title split-e2e-e \
    --app-id com.focaldesk.SplitE2eClient.E \
    --configure-log "$ARTIFACTS/client-e-configures.txt" \
    >"$ARTIFACTS/client-e.log" 2>&1 &
CLIENT_E_PID=$!
wait_for_window split-e2e-a || fail "first XDG client did not map"
wait_for_window split-e2e-b || fail "second XDG client did not map"
wait_for_window split-e2e-c || fail "third XDG client did not map"
wait_for_window split-e2e-d || fail "fourth XDG client did not map"
wait_for_window split-e2e-e || fail "replacement XDG client did not map"
ORIGINAL_A="$(geometry split-e2e-a)"
ORIGINAL_B="$(geometry split-e2e-b)"
ORIGINAL_C="$(geometry split-e2e-c)"
ORIGINAL_D="$(geometry split-e2e-d)"
ORIGINAL_E="$(geometry split-e2e-e)"
pass "five instrumented XDG clients mapped"

"$CLI" split-window split-e2e-a left
"$CLI" split-window split-e2e-b right
wait_for_split 2000 || fail "clients did not form a side-by-side split"
wait_for_balanced_split 2000 || fail "clients did not settle into the balanced split"
read -r ax ay aw ah < <(geometry split-e2e-a)
read -r bx by bw bh < <(geometry split-e2e-b)
wait_for_client_configure "$ARTIFACTS/client-a-configures.txt" "$aw" "$ah" \
    || fail "left client did not receive its split configure"
wait_for_client_configure "$ARTIFACTS/client-b-configures.txt" "$bw" "$bh" \
    || fail "right client did not receive its split configure"
pass "side-by-side placement reached both Wayland clients"

# Exercise the compositor's keyboard-action path against real clients. The
# shortcut-to-action map is unit tested; this verifies geometry, focus, HUD
# state, and undo after those actions reach DesktopState.
KEYBOARD_BASE_A="$(geometry split-e2e-a)"
KEYBOARD_BASE_B="$(geometry split-e2e-b)"
read -r _ _ keyboard_base_aw _ <<<"$KEYBOARD_BASE_A"
"$CLI" split-key split-e2e-a resize-right
HUD_PERCENT=$("$CLI" split-resize-percent)
(( HUD_PERCENT > 0 && HUD_PERCENT < 100 )) \
    || fail "keyboard resize did not expose a valid percentage HUD"
wait_for_split 2000 || fail "coarse keyboard resize broke the split group"
read -r _ _ keyboard_coarse_aw _ < <(geometry split-e2e-a)
COARSE_DELTA=$((keyboard_coarse_aw - keyboard_base_aw))
(( COARSE_DELTA > 0 )) || fail "coarse keyboard resize did not enlarge the left pane"
pass "coarse keyboard resize updated real clients and exposed the percentage HUD"

"$CLI" split-key split-e2e-a undo
wait_for_geometry split-e2e-a "$KEYBOARD_BASE_A" \
    || fail "keyboard undo did not restore the left pane: expected $KEYBOARD_BASE_A, got $(geometry split-e2e-a)"
wait_for_geometry split-e2e-b "$KEYBOARD_BASE_B" \
    || fail "keyboard undo did not restore the right pane: expected $KEYBOARD_BASE_B, got $(geometry split-e2e-b)"
pass "keyboard undo restored both client geometries"

"$CLI" split-key split-e2e-a resize-right-fine
"$CLI" split-resize-percent >/dev/null \
    || fail "fine keyboard resize did not expose the percentage HUD"
wait_for_split 2000 || fail "fine keyboard resize broke the split group"
read -r _ _ keyboard_fine_aw _ < <(geometry split-e2e-a)
FINE_DELTA=$((keyboard_fine_aw - keyboard_base_aw))
(( FINE_DELTA > 0 && FINE_DELTA < COARSE_DELTA )) \
    || fail "fine keyboard resize was not smaller than the coarse step"
"$CLI" split-key split-e2e-a undo
wait_for_geometry split-e2e-a "$KEYBOARD_BASE_A" \
    || fail "undo after fine resize did not restore the left pane"
pass "fine keyboard resize used the smaller step and remained undoable"

"$CLI" split-key split-e2e-a focus-next
[[ "$("$CLI" focused-window-title)" == "split-e2e-b" ]] \
    || fail "forward split-pane focus did not reach the right pane"
"$CLI" split-key split-e2e-b focus-previous
[[ "$("$CLI" focused-window-title)" == "split-e2e-a" ]] \
    || fail "reverse split-pane focus did not return to the left pane"
pass "keyboard split-pane focus cycled forward and backward"

"$CLI" split-ratio split-e2e-a 650
wait_for_split 2000 || fail "split group broke while moving the divider"
read -r ax ay aw ah < <(geometry split-e2e-a)
read -r bx by bw bh < <(geometry split-e2e-b)
(( aw > bw )) || fail "divider ratio did not enlarge the left pane"
wait_for_client_configure "$ARTIFACTS/client-a-configures.txt" "$aw" "$ah" \
    || fail "left client did not receive the divider configure"
wait_for_client_configure "$ARTIFACTS/client-b-configures.txt" "$bw" "$bh" \
    || fail "right client did not receive the divider configure"
pass "divider movement configured both clients"

"$CLI" display-mode focaldesk-winit 3200 1800 1.0
wait_for_split 2800 || fail "split group did not reflow after output resize"
read -r ax ay aw ah < <(geometry split-e2e-a)
read -r bx by bw bh < <(geometry split-e2e-b)
(( aw + bw >= 2800 )) || fail "resized output did not expose the larger split work area"
(( aw > bw )) || fail "divider ratio was not preserved after output resize"
wait_for_client_configure "$ARTIFACTS/client-a-configures.txt" "$aw" "$ah" \
    || fail "left client did not receive the resized-output configure"
wait_for_client_configure "$ARTIFACTS/client-b-configures.txt" "$bw" "$bh" \
    || fail "right client did not receive the resized-output configure"
pass "output resize preserved ratio and configured both clients"

"$CLI" split-exit split-e2e-a
wait_for_geometry split-e2e-a "$ORIGINAL_A" || fail "pair exit did not restore first client"
wait_for_geometry split-e2e-b "$ORIGINAL_B" || fail "pair exit did not restore second client"
pass "pair exit restored both floating geometries"

"$CLI" split-layout split-e2e-a left-third
"$CLI" split-assist split-e2e-b
"$CLI" split-assist split-e2e-c
wait_for_three_columns 2800 || fail "Split Assist did not build a three-column group"
read -r ax ay aw ah < <(geometry split-e2e-a)
read -r bx by bw bh < <(geometry split-e2e-b)
read -r cx cy cw ch < <(geometry split-e2e-c)
wait_for_client_configure "$ARTIFACTS/client-a-configures.txt" "$aw" "$ah" \
    || fail "left-third client did not receive its configure"
wait_for_client_configure "$ARTIFACTS/client-b-configures.txt" "$bw" "$bh" \
    || fail "center-third client did not receive its configure"
wait_for_client_configure "$ARTIFACTS/client-c-configures.txt" "$cw" "$ch" \
    || fail "right-third client did not receive its configure"
pass "Split Assist filled all three columns"

OLD_FIRST_BOUNDARY=$((ax + aw))
OLD_SECOND_BOUNDARY=$((bx + bw))
"$CLI" split-divider split-e2e-a right 300
"$CLI" split-divider split-e2e-c left 700
wait_for_three_columns 2800 || fail "three-column group broke while moving both dividers"
read -r ax ay aw ah < <(geometry split-e2e-a)
read -r bx by bw bh < <(geometry split-e2e-b)
read -r cx cy cw ch < <(geometry split-e2e-c)
(( ax + aw != OLD_FIRST_BOUNDARY )) || fail "first column divider did not move"
(( bx + bw != OLD_SECOND_BOUNDARY )) || fail "second column divider did not move"
pass "both three-column dividers resized their adjacent clients"

"$CLI" split-exit split-e2e-b
wait_for_geometry split-e2e-a "$ORIGINAL_A" || fail "three-column exit did not restore A"
wait_for_geometry split-e2e-b "$ORIGINAL_B" || fail "three-column exit did not restore B"
wait_for_geometry split-e2e-c "$ORIGINAL_C" || fail "three-column exit did not restore C"
pass "three-column group exit restored every client"

"$CLI" split-layout split-e2e-a top-left
"$CLI" split-assist split-e2e-b
"$CLI" split-assist split-e2e-c
"$CLI" split-assist split-e2e-d
wait_for_quadrants 2800 1500 || fail "Split Assist did not build four quadrants"
pass "Split Assist filled all four quadrants"

"$CLI" display-mode focaldesk-winit 3840 2160 1.5
wait_for_quadrants 2200 1000 || fail "quadrants did not survive resolution and scale change"
read -r ax ay aw ah < <(geometry split-e2e-a)
read -r bx by bw bh < <(geometry split-e2e-b)
read -r cx cy cw ch < <(geometry split-e2e-c)
read -r dx dy dw dh < <(geometry split-e2e-d)
wait_for_client_configure "$ARTIFACTS/client-d-configures.txt" "$dw" "$dh" \
    || fail "bottom-right client did not receive the scaled-output configure"
pass "four-window layout survived resolution and scaling changes"

OLD_VERTICAL_BOUNDARY=$((ax + aw))
OLD_HORIZONTAL_BOUNDARY=$((ay + ah))
"$CLI" split-divider split-e2e-a right 600
"$CLI" split-divider split-e2e-a bottom 600
wait_for_quadrants 2200 1000 || fail "quadrants broke while moving row and column dividers"
read -r ax ay aw ah < <(geometry split-e2e-a)
read -r bx by bw bh < <(geometry split-e2e-b)
read -r cx cy cw ch < <(geometry split-e2e-c)
read -r dx dy dw dh < <(geometry split-e2e-d)
(( ax + aw != OLD_VERTICAL_BOUNDARY )) || fail "quadrant column divider did not move"
(( ay + ah != OLD_HORIZONTAL_BOUNDARY )) || fail "quadrant row divider did not move"
pass "independent quadrant row and column dividers resized all four clients"

OLD_A="$(geometry split-e2e-a)"
OLD_B="$(geometry split-e2e-b)"
"$CLI" split-swap split-e2e-a right
wait_for_geometry split-e2e-a "$OLD_B" || fail "right swap did not move A into B's slot"
wait_for_geometry split-e2e-b "$OLD_A" || fail "right swap did not move B into A's slot"
pass "directional swap exchanged adjacent quadrant members"

REPLACE_TARGET="$(geometry split-e2e-a)"
"$CLI" split-replace split-e2e-a
"$CLI" split-assist split-e2e-e
wait_for_geometry split-e2e-e "$REPLACE_TARGET" || fail "replacement client did not take the target slot"
wait_for_geometry split-e2e-a "$ORIGINAL_A" || fail "replaced client did not return to floating geometry"
pass "replacement moved a fifth client into the selected group slot"

"$CLI" split-exit split-e2e-e
wait_for_geometry split-e2e-b "$ORIGINAL_B" || fail "four-window exit did not restore B"
wait_for_geometry split-e2e-c "$ORIGINAL_C" || fail "four-window exit did not restore C"
wait_for_geometry split-e2e-d "$ORIGINAL_D" || fail "four-window exit did not restore D"
wait_for_geometry split-e2e-e "$ORIGINAL_E" || fail "four-window exit did not restore E"
pass "four-window group exit restored every member"

"$CLI" split-layout split-e2e-a top-left
"$CLI" split-assist split-e2e-b
"$CLI" split-assist split-e2e-c
"$CLI" split-assist split-e2e-d
wait_for_quadrants 2200 1000 || fail "final quadrant group did not form"
printf '%s\n' '{"workspaces":{"split_screen_enabled":false}}' \
    >"$XDG_CONFIG_HOME/focaldesk/settings.json"
"$CLI" reload-settings
wait_for_geometry split-e2e-a "$ORIGINAL_A" || fail "disable did not restore A"
wait_for_geometry split-e2e-b "$ORIGINAL_B" || fail "disable did not restore B"
wait_for_geometry split-e2e-c "$ORIGINAL_C" || fail "disable did not restore C"
wait_for_geometry split-e2e-d "$ORIGINAL_D" || fail "disable did not restore D"
if "$CLI" split-layout split-e2e-a top-left >/dev/null 2>&1; then
    fail "split layout remained available after disabling split screen"
fi
pass "disabling split screen restored every window and rejected new splits"

printf '%s\n' '{"workspaces":{"split_screen_enabled":true,"restore_session":true,"restore_split_layouts":true}}' \
    >"$XDG_CONFIG_HOME/focaldesk/settings.json"
"$CLI" reload-settings
"$CLI" create-workspace
"$CLI" focus-workspace 1
"$CLI" split-layout split-e2e-a left-third
"$CLI" split-assist split-e2e-b
"$CLI" split-assist split-e2e-c
"$CLI" split-divider split-e2e-a right 340
"$CLI" split-divider split-e2e-c left 660
"$CLI" split-workspace split-e2e-a 2
wait_for_three_columns 2200 || fail "restart test could not prepare three columns"
[[ "$("$CLI" window-workspace split-e2e-a)" == "2" ]] \
    || fail "three-column group did not move to workspace 2"
read -r ax ay aw ah < <(geometry split-e2e-a)
read -r bx by bw bh < <(geometry split-e2e-b)
read -r cx cy cw ch < <(geometry split-e2e-c)
RESTORE_TOTAL_WIDTH=$((aw + bw + cw))
RESTORE_FIRST_PM=$((aw * 1000 / RESTORE_TOTAL_WIDTH))
RESTORE_SECOND_PM=$(((aw + bw) * 1000 / RESTORE_TOTAL_WIDTH))
"$CLI" checkpoint-session
wait_for_snapshot_direction center 1 || fail "three-column session snapshot was not written"
pass "three-column layout, custom dividers, and workspace were checkpointed"

stop_nested_session
start_nested_compositor "-restart-1"
WAYLAND_DISPLAY="$NESTED_DISPLAY" "$CLIENT" --title split-e2e-a \
    --app-id com.focaldesk.SplitE2eClient.A \
    --configure-log "$ARTIFACTS/client-a-restart-1-configures.txt" \
    >"$ARTIFACTS/client-a-restart-1.log" 2>&1 &
CLIENT_A_PID=$!
wait_for_window split-e2e-a || fail "restored client A did not map"
WAYLAND_DISPLAY="$NESTED_DISPLAY" "$CLIENT" --title split-e2e-b \
    --app-id com.focaldesk.SplitE2eClient.B \
    --configure-log "$ARTIFACTS/client-b-restart-1-configures.txt" \
    >"$ARTIFACTS/client-b-restart-1.log" 2>&1 &
CLIENT_B_PID=$!
wait_for_window split-e2e-b || fail "restored client B did not map"
WAYLAND_DISPLAY="$NESTED_DISPLAY" "$CLIENT" --title split-e2e-c \
    --app-id com.focaldesk.SplitE2eClient.C \
    --configure-log "$ARTIFACTS/client-c-restart-1-configures.txt" \
    >"$ARTIFACTS/client-c-restart-1.log" 2>&1 &
CLIENT_C_PID=$!
wait_for_window split-e2e-c || fail "restored client C did not map"
wait_for_three_columns 2200 || fail "restart did not restore all three column slots"
read -r ax ay aw ah < <(geometry split-e2e-a)
read -r bx by bw bh < <(geometry split-e2e-b)
read -r cx cy cw ch < <(geometry split-e2e-c)
RESTORED_TOTAL_WIDTH=$((aw + bw + cw))
ratio_is_near $((aw * 1000 / RESTORED_TOTAL_WIDTH)) "$RESTORE_FIRST_PM" \
    || fail "restart changed the first three-column divider ratio"
ratio_is_near $(((aw + bw) * 1000 / RESTORED_TOTAL_WIDTH)) "$RESTORE_SECOND_PM" \
    || fail "restart changed the second three-column divider ratio"
[[ "$("$CLI" window-workspace split-e2e-a)" == "2" \
    && "$("$CLI" window-workspace split-e2e-b)" == "2" \
    && "$("$CLI" window-workspace split-e2e-c)" == "2" ]] \
    || fail "restart did not restore the three-column workspace"
pass "three-column slots, divider positions, and workspace survived compositor restart"

"$CLI" split-exit split-e2e-b
WAYLAND_DISPLAY="$NESTED_DISPLAY" "$CLIENT" --title split-e2e-d \
    --app-id com.focaldesk.SplitE2eClient.D \
    --configure-log "$ARTIFACTS/client-d-restart-1-configures.txt" \
    >"$ARTIFACTS/client-d-restart-1.log" 2>&1 &
CLIENT_D_PID=$!
wait_for_window split-e2e-d || fail "fourth restore client did not map"
"$CLI" window-move-workspace split-e2e-d 2
"$CLI" split-layout split-e2e-a top-left
"$CLI" split-assist split-e2e-b
"$CLI" split-assist split-e2e-c
"$CLI" split-assist split-e2e-d
"$CLI" split-divider split-e2e-a right 600
"$CLI" split-divider split-e2e-a bottom 600
wait_for_quadrants 2200 1000 || fail "restart test could not prepare four quadrants"
read -r ax ay aw ah < <(geometry split-e2e-a)
read -r bx by bw bh < <(geometry split-e2e-b)
read -r cx cy cw ch < <(geometry split-e2e-c)
QUAD_VERTICAL_PM=$((aw * 1000 / (aw + bw)))
QUAD_HORIZONTAL_PM=$((ah * 1000 / (ah + ch)))
"$CLI" checkpoint-session
wait_for_snapshot_direction bottom_right 1 || fail "four-quadrant session snapshot was not written"
pass "four-quadrant layout and custom dividers were checkpointed"

stop_nested_session
start_nested_compositor "-restart-2"
WAYLAND_DISPLAY="$NESTED_DISPLAY" "$CLIENT" --title split-e2e-a \
    --app-id com.focaldesk.SplitE2eClient.A \
    --configure-log "$ARTIFACTS/client-a-restart-2-configures.txt" \
    >"$ARTIFACTS/client-a-restart-2.log" 2>&1 &
CLIENT_A_PID=$!
wait_for_window split-e2e-a || fail "second-restart client A did not map"
WAYLAND_DISPLAY="$NESTED_DISPLAY" "$CLIENT" --title split-e2e-b \
    --app-id com.focaldesk.SplitE2eClient.B \
    --configure-log "$ARTIFACTS/client-b-restart-2-configures.txt" \
    >"$ARTIFACTS/client-b-restart-2.log" 2>&1 &
CLIENT_B_PID=$!
wait_for_window split-e2e-b || fail "second-restart client B did not map"
WAYLAND_DISPLAY="$NESTED_DISPLAY" "$CLIENT" --title split-e2e-c \
    --app-id com.focaldesk.SplitE2eClient.C \
    --configure-log "$ARTIFACTS/client-c-restart-2-configures.txt" \
    >"$ARTIFACTS/client-c-restart-2.log" 2>&1 &
CLIENT_C_PID=$!
wait_for_window split-e2e-c || fail "second-restart client C did not map"
wait_for_three_quadrants 2200 1000 || fail "missing app corrupted the other quadrant slots"
read -r ax ay aw ah < <(geometry split-e2e-a)
read -r bx by bw bh < <(geometry split-e2e-b)
read -r cx cy cw ch < <(geometry split-e2e-c)
ratio_is_near $((aw * 1000 / (aw + bw))) "$QUAD_VERTICAL_PM" \
    || fail "missing app changed the restored column divider ratio"
ratio_is_near $((ah * 1000 / (ah + ch))) "$QUAD_HORIZONTAL_PM" \
    || fail "missing app changed the restored row divider ratio"
if "$CLI" window-geometry split-e2e-d >/dev/null 2>&1; then
    fail "missing fourth application unexpectedly mapped"
fi
pass "missing quadrant application left the other restored slots intact"

WAYLAND_DISPLAY="$NESTED_DISPLAY" "$CLIENT" --title split-e2e-d \
    --app-id com.focaldesk.SplitE2eClient.D \
    --configure-log "$ARTIFACTS/client-d-restart-2-configures.txt" \
    >"$ARTIFACTS/client-d-restart-2.log" 2>&1 &
CLIENT_D_PID=$!
wait_for_window split-e2e-d || fail "delayed fourth application did not map"
wait_for_quadrants 2200 1000 || fail "delayed application did not reclaim its saved quadrant"
[[ "$("$CLI" window-workspace split-e2e-d)" == "2" ]] \
    || fail "delayed application did not return to its saved workspace"
pass "delayed application safely reclaimed the missing quadrant"

kill -0 "$COMPOSITOR_PID" "$CLIENT_A_PID" "$CLIENT_B_PID" "$CLIENT_C_PID" \
    "$CLIENT_D_PID" 2>/dev/null || fail "a restored compositor or client exited unexpectedly"
pass "nested multi-window split-screen restart E2E test complete"
