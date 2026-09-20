#!/usr/bin/env bash
set -euo pipefail

units=(
    focaldesk-system-rail.service
    focaldesk-task-shelf.service
)

if ! command -v systemctl >/dev/null 2>&1; then
    echo "restart-shell-clients.sh: systemctl is not installed" >&2
    exit 1
fi

echo "Restarting the FocalDesk system rail and task shelf..."
if ! systemctl --user restart "${units[@]}"; then
    echo "restart-shell-clients.sh: failed to restart the shell clients" >&2
    systemctl --user status "${units[@]}" --no-pager -l >&2 || true
    exit 1
fi

for unit in "${units[@]}"; do
    if ! systemctl --user is-active --quiet "$unit"; then
        echo "restart-shell-clients.sh: $unit is not active after restart" >&2
        systemctl --user status "$unit" --no-pager -l >&2 || true
        exit 1
    fi
done

echo "FocalDesk system rail and task shelf restarted successfully."
