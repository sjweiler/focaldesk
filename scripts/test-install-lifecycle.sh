#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
test_root="$(mktemp -d)"
trap 'rm -rf -- "$test_root"' EXIT
user_root="$test_root/user"
system_root="$test_root/system"

seed_manifest() {
    local root="$1" manifest="$2" mode relative target
    while read -r mode relative; do
        [[ -n "${mode:-}" && "${mode:0:1}" != "#" ]] || continue
        target="${root%/}/$relative"
        install -Dm"$mode" /dev/null "$target"
    done < "$manifest"
}

seed_manifest "$user_root" "$repo_root/packaging/install-manifest-user.txt"
seed_manifest "$system_root" "$repo_root/packaging/install-manifest-system.txt"
install -Dm600 /dev/null "$user_root/.config/focaldesk/settings.json"

FOCALDESK_VERIFY_USER_ROOT="$user_root" FOCALDESK_VERIFY_SYSTEM_ROOT="$system_root" \
    bash "$repo_root/scripts/verify-installation.sh" all
FOCALDESK_UNINSTALL_USER_ROOT="$user_root" FOCALDESK_UNINSTALL_SYSTEM_ROOT="$system_root" \
    bash "$repo_root/scripts/uninstall.sh" --scope all --yes --no-services

if find "$user_root/.local/bin" "$system_root" -type f -print -quit | grep -q .; then
    echo "Manifest-owned files remained after uninstall" >&2
    exit 1
fi
test -e "$user_root/.config/focaldesk/settings.json"

FOCALDESK_UNINSTALL_USER_ROOT="$user_root" FOCALDESK_UNINSTALL_SYSTEM_ROOT="$system_root" \
    bash "$repo_root/scripts/uninstall.sh" --scope user --yes --purge --no-services
test ! -e "$user_root/.config/focaldesk"
echo "Install lifecycle test passed."
