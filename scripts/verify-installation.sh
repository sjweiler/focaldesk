#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
scope="${1:-all}"
user_root="${FOCALDESK_VERIFY_USER_ROOT:-${HOME}}"
system_root="${FOCALDESK_VERIFY_SYSTEM_ROOT:-/}"
failures=0

case "$scope" in user|system|all) ;; *) echo "Usage: $0 [user|system|all]" >&2; exit 2 ;; esac

verify_manifest() {
    local root="$1" manifest="$2" mode relative target actual
    while read -r mode relative; do
        [[ -n "${mode:-}" && "${mode:0:1}" != "#" ]] || continue
        target="${root%/}/$relative"
        if [[ ! -e "$target" ]]; then
            echo "MISSING $target" >&2
            failures=$((failures + 1))
            continue
        fi
        actual="$(stat -c '%a' "$target")"
        if [[ "$actual" != "$mode" ]]; then
            echo "MODE $target expected=$mode actual=$actual" >&2
            failures=$((failures + 1))
        fi
    done < "$manifest"
}

[[ "$scope" == system ]] || verify_manifest "$user_root" "$repo_root/packaging/install-manifest-user.txt"
[[ "$scope" == user ]] || verify_manifest "$system_root" "$repo_root/packaging/install-manifest-system.txt"

if ((failures)); then
    echo "Installation verification failed: $failures problem(s)" >&2
    exit 1
fi
echo "FocalDesk ${scope} installation verified."
