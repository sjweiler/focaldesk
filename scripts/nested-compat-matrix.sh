#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
artifacts="${FOCALDESK_COMPAT_ARTIFACTS:-$repo_root/target/compat-matrix}"
mkdir -p "$artifacts"

declare -a labels=()
declare -a commands=()

add_if_available() {
    local label="$1" executable="$2" command="$3"
    if command -v "$executable" >/dev/null 2>&1; then
        labels+=("$label")
        commands+=("$command")
    fi
}

add_if_available "weston-shm" "weston-simple-shm" "weston-simple-shm"
add_if_available "gtk4" "gtk4-demo" "gtk4-demo"
add_if_available "qt6" "qml6" "qml6"
add_if_available "weston-terminal" "weston-terminal" "weston-terminal"
add_if_available "firefox-wayland" "firefox" "firefox --new-instance --no-remote about:blank"
add_if_available "chromium-wayland" "chromium" "chromium --ozone-platform=wayland --user-data-dir=/tmp/focaldesk-chromium-smoke about:blank"

if ((${#commands[@]} == 0)); then
    echo "No supported compatibility clients are installed." >&2
    exit 77
fi

cargo build -p focaldesk-desktop --no-default-features --features winit,xwayland

summary="$artifacts/summary.txt"
: > "$summary"
for index in "${!commands[@]}"; do
    label="${labels[$index]}"
    command="${commands[$index]}"
    output="$artifacts/$label"
    echo "RUN $label: $command" | tee -a "$summary"
    if FOCALDESK_SMOKE_CLIENT="$command" FOCALDESK_SMOKE_ARTIFACTS="$output" \
        bash "$repo_root/scripts/nested-smoke.sh" --no-build; then
        echo "PASS $label" | tee -a "$summary"
    else
        echo "FAIL $label" | tee -a "$summary" >&2
        exit 1
    fi
done

echo "Compatibility matrix passed for ${#commands[@]} client(s)." | tee -a "$summary"
