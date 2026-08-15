#!/usr/bin/env bash
set -uo pipefail

uid=$(id -u)
failed=0

check() {
    local description=$1
    shift
    if "$@" >/dev/null 2>&1; then
        printf 'PASS: %s\n' "$description"
    else
        printf 'FAIL: %s\n' "$description" >&2
        failed=1
    fi
}

check "credential-path watcher is active" \
    systemctl is-active --quiet "focald-secrets@${uid}.path"
check "credential broker is active" \
    systemctl is-active --quiet "focald-secrets@${uid}.service"
check "native credential socket exists" \
    test -S "/run/user/${uid}/focaldesk/secrets.sock"
check "Secret Service name has a live session-bus owner" \
    busctl --user status org.freedesktop.secrets
check "Secret Service API responds" \
    busctl --user introspect org.freedesktop.secrets /org/freedesktop/secrets \
        org.freedesktop.Secret.Service
check "stale per-user D-Bus activation is absent" \
    test ! -e "$HOME/.local/share/dbus-1/services/org.freedesktop.secrets.service"
check "stale system D-Bus activation is absent" \
    test ! -e /usr/share/dbus-1/services/org.freedesktop.secrets.service

if (( failed != 0 )); then
    printf '\nRecent broker log:\n' >&2
    journalctl -b -u "focald-secrets@${uid}.service" --no-pager -n 30 >&2 || true
    exit 1
fi

printf '\nSecret Service is healthy end to end.\n'
