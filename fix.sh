#!/usr/bin/env bash
set -Eeuo pipefail

daemon=/usr/sbin/focaldmd
local_daemon=/usr/local/bin/focaldmd
service=focaldmd.service

if (( EUID != 0 )); then
    exec sudo -- "$0" "$@"
fi

for command in getent id install make realpath semanage semodule restorecon systemctl; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "fix.sh: required command not found: $command" >&2
        exit 1
    fi
done

desktop_user=${SUDO_USER:-}
if [[ -z "$desktop_user" || "$desktop_user" == root ]]; then
    echo "fix.sh: run this with sudo from the affected desktop user" >&2
    exit 1
fi
passwd_entry=$(getent passwd "$desktop_user")
IFS=: read -r _ _ _ _ _ desktop_home _ <<<"$passwd_entry"
if [[ -z "$desktop_home" || "$desktop_home" == / ]]; then
    echo "fix.sh: could not determine a safe home for $desktop_user" >&2
    exit 1
fi

policy_dir=$(realpath -m "$(dirname "$0")/packaging/selinux")
policy_package="$policy_dir/focaldesk_secrets.pp"
if [[ ! -f /usr/share/selinux/devel/Makefile ]]; then
    echo "fix.sh: selinux-policy-devel is required to build the repair policy" >&2
    exit 1
fi

echo "Installing the scoped credential-key SELinux policy..."
make -C "$policy_dir" focaldesk_secrets.pp
semodule -i "$policy_package"
restorecon -RFv "$desktop_home/.local/share/focaldesk"

echo "Removing the obsolete Secret Service D-Bus activation entry..."
rm -f /usr/share/dbus-1/services/org.freedesktop.secrets.service
rm -f "$desktop_home/.local/share/dbus-1/services/org.freedesktop.secrets.service"

echo "Installing credential-gated broker startup..."
systemctl stop "focald-secrets@$(id -u "$desktop_user").socket" 2>/dev/null || true
install -Dm644 "$(dirname "$0")/packaging/systemd/system/focald-secrets@.service" \
    /usr/lib/systemd/system/focald-secrets@.service
install -Dm644 "$(dirname "$0")/packaging/systemd/system/focald-secrets@.path" \
    /usr/lib/systemd/system/focald-secrets@.path
install -Dm644 "$(dirname "$0")/packaging/systemd/system/user@.service.d/90-focald-secrets.conf" \
    /usr/lib/systemd/system/user@.service.d/90-focald-secrets.conf
rm -f /usr/lib/systemd/system/focald-secrets@.socket
systemctl daemon-reload
desktop_uid=$(id -u "$desktop_user")
systemctl reset-failed "focald-secrets@${desktop_uid}.service" \
    "focald-secrets@${desktop_uid}.socket" 2>/dev/null || true
systemctl start "focald-secrets@${desktop_uid}.path"
if ! systemctl is-active --quiet "focald-secrets@${desktop_uid}.path"; then
    echo "fix.sh: credential-path watcher did not start" >&2
    systemctl status "focald-secrets@${desktop_uid}.path" --no-pager -l >&2 || true
    exit 1
fi

declare -a daemons=()
[[ -x "$daemon" ]] && daemons+=("$daemon")
[[ -x "$local_daemon" ]] && daemons+=("$local_daemon")

if (( ${#daemons[@]} == 0 )); then
    echo "fix.sh: no installed focaldmd executable found" >&2
    exit 1
fi

echo "Applying the persistent SELinux display-manager label..."
for installed_daemon in "${daemons[@]}"; do
    # Fedora maps /usr/sbin to /usr/bin in SELinux file-context policy, so
    # canonicalize that path before creating the persistent rule.  Keep the
    # development /usr/local install covered too: an /etc systemd test unit
    # may override the packaged unit and launch that copy instead.
    selinux_path=$(realpath -m "$installed_daemon")
    semanage fcontext -a -t xdm_exec_t "$selinux_path" 2>/dev/null \
        || semanage fcontext -m -t xdm_exec_t "$selinux_path"
    restorecon -v "$installed_daemon"
done

echo "Restarting $service..."
systemctl restart "$service"

if ! systemctl is-active --quiet "$service"; then
    echo "fix.sh: $service did not start successfully" >&2
    systemctl status "$service" --no-pager -l >&2 || true
    exit 1
fi

echo "Repair complete. Installed label and service status:"
ls -lZ "${daemons[@]}"
systemctl status "$service" --no-pager -l | sed -n '1,8p'
echo "Return to the login screen and try signing in again."
