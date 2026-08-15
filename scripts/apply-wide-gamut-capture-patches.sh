#!/usr/bin/env bash
# Apply the companion OBS and xdg-desktop-portal-wlr patches required by
# FOCALDESK_PORTAL_COLOR=bt2020-sdr. Run this against clean, version-matched
# source trees, then build/install those projects using their normal tooling.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ $# -ne 2 ]]; then
    echo "Usage: $0 /path/to/obs-studio-32.1.1 /path/to/xdg-desktop-portal-wlr-0.8.1" >&2
    exit 2
fi

OBS_SOURCE="$1"
XDPW_SOURCE="$2"
OBS_PATCH="$ROOT/patches/obs-studio-32.1.1-bt2020-sdr.patch"
XDPW_PATCH="$ROOT/patches/xdg-desktop-portal-wlr-0.8.1-bt2020-sdr.patch"

for source_dir in "$OBS_SOURCE" "$XDPW_SOURCE"; do
    if [[ ! -d "$source_dir/.git" ]]; then
        echo "Not a Git source tree: $source_dir" >&2
        exit 1
    fi
done

apply_patch() {
    local source_dir="$1"
    local patch_file="$2"
    local component="$3"

    if git -C "$source_dir" apply --check "$patch_file" 2>/dev/null; then
        git -C "$source_dir" apply "$patch_file"
        echo "Applied $component BT.2020 SDR patch."
    elif git -C "$source_dir" apply --reverse --check "$patch_file" 2>/dev/null; then
        echo "$component BT.2020 SDR patch is already applied."
    else
        git -C "$source_dir" apply --check "$patch_file" || true
        echo "Cannot apply $component patch cleanly in $source_dir" >&2
        exit 1
    fi
}

apply_patch "$OBS_SOURCE" "$OBS_PATCH" "OBS Studio"
apply_patch "$XDPW_SOURCE" "$XDPW_PATCH" "xdg-desktop-portal-wlr"

echo "FocalDesk BT.2020 SDR patches are ready. Build and install both projects,"
echo "then enable FOCALDESK_PORTAL_COLOR=bt2020-sdr for the FocalDesk session."
