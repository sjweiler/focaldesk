#!/usr/bin/env bash
# Assemble a staged install with Tauri as the default and GTK as a fallback.
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"
if [[ $# != 1 || "$1" != /* || "$1" == / ]]; then
    echo 'Usage: stage-settings-preview.sh /absolute/staging/directory' >&2
    exit 2
fi
stage_root=$1
npm ci --prefix apps/focaldesk-settings/frontend
npm run build --prefix apps/focaldesk-settings/frontend
cargo build --release -j 2 -p focaldesk-settings --bin focaldesk-settings
cargo build --release -j 2 -p focaldesk-settings --no-default-features --features tauri-ui --bin focaldesk-settings-next
install -Dm755 target/release/focaldesk-settings "$stage_root/usr/bin/focaldesk-settings-gtk"
install -Dm755 target/release/focaldesk-settings-next "$stage_root/usr/bin/focaldesk-settings-next"
install -Dm755 packaging/bin/focaldesk-settings "$stage_root/usr/bin/focaldesk-settings"
install -Dm644 packaging/applications/focaldesk-settings.desktop "$stage_root/usr/share/applications/focaldesk-settings.desktop"
install -Dm644 LICENSE "$stage_root/usr/share/licenses/focaldesk-settings/LICENSE"
printf 'Staged default Settings and GTK fallback in %s\n' "$stage_root"
