#!/usr/bin/env bash
set -euo pipefail
umask 077

state=$1
runtime=$2
snapshot=$3
vector_snapshot=${4:-}
export SNMP_PERSISTENT_DIR="$state/state"
test -r "$SNMP_PERSISTENT_DIR/snmpd.conf"

receiver_address() {
    if [[ -r "$state/notify-client.conf" ]]; then
        local network=${FOCALDESK_OPENNMS_NETWORK:-opennms-net}
        local container=${FOCALDESK_OPENNMS_CONTAINER:-opennms-horizon}
        /usr/bin/podman inspect --format '{{json .NetworkSettings.Networks}}' "$container" 2>/dev/null |
            /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin).get(sys.argv[1], {}).get("IPAddress", ""))' "$network" 2>/dev/null ||
            true
    fi
}

receiver=$(receiver_address)
trap_port=${FOCALDESK_OPENNMS_TRAP_PORT:-1162}
notify_root=.1.3.6.1.4.1.67726.1.0
objects_root=.1.3.6.1.4.1.67726.1.1
/usr/bin/python3 - "$state" "$runtime" "$receiver" "$trap_port" "$notify_root" <<'PY'
from ipaddress import IPv4Address
from pathlib import Path
import re
import sys

state, runtime = map(Path, sys.argv[1:3])
receiver, port, notification = sys.argv[3:]
config = "\n".join(line for line in (state / "master.conf").read_text().splitlines()
                   if not line.lstrip().lower().startswith("agentxsocket ")) + "\n"
config += f"agentXSocket {runtime / 'agentx.sock'}\n"
if receiver:
    address = IPv4Address(receiver)
    port = int(port)
    if not 1 <= port <= 65535:
        raise ValueError("invalid OpenNMS trap port")
    fields = dict(line.split(None, 1) for line in (state / "notify-client.conf").read_text().splitlines()
                  if line.startswith("def"))
    user, auth, privacy = (fields[key] for key in
                           ("defSecurityName", "defAuthPassphrase", "defPrivPassphrase"))
    if not all(re.fullmatch(r"[A-Za-z0-9_.-]+", value) for value in (user, auth, privacy)):
        raise ValueError("use letters, digits, dots, underscores or hyphens in notification credentials")
    for suffix in range(4, 9):
        config += f"notificationFilter focaldesk {notification}.{suffix} 0x00 included\n"
    config += f"trapsess -profile focaldesk -Ci -v3 -l authPriv -u {user} -a SHA-256 "
    config += f"-A {auth} -x AES -X {privacy} -t 1 -r 2 udp:{address}:{port}\n"
(runtime / "master.conf").write_text(config)
PY

if [[ -S "$runtime/agentx.sock" ]]; then unlink "$runtime/agentx.sock"; fi
/usr/bin/snmpd -f -Lo -C -c "$runtime/master.conf,$SNMP_PERSISTENT_DIR/snmpd.conf" -p "$runtime/master.pid" &
master=$!
options=()
if [[ -n "$receiver" ]]; then
    options+=(--gpu-failure-notification "$notify_root.4" --compositor-notifications --display-notifications)
fi
if [[ -x /usr/bin/nvidia-smi && -c /dev/nvidiactl ]]; then options+=(--nvidia-smi /usr/bin/nvidia-smi); fi
/usr/bin/python3 "$state/focaldesk-monitor-agentx.py" \
    --socket "$runtime/agentx.sock" --root "$objects_root" --snapshot "$snapshot" \
    --registry "$state/indexes.sqlite3" "${options[@]}" &
agent=$!
(
    while sleep 30; do
        if [[ $(receiver_address) != "$receiver" ]]; then exit 1; fi
    done
) &
watcher=$!

children=("$master" "$agent" "$watcher")
if [[ -n "$vector_snapshot" && -r "$state/focal-vector-agentx.py" ]]; then
    /usr/bin/python3 "$state/focal-vector-agentx.py" \
        --socket "$runtime/agentx.sock" --snapshot-socket "$vector_snapshot" \
        --registry "$state/vector-indexes.sqlite3" &
    children+=("$!")
fi

cleanup() {
    kill "${children[@]}" 2>/dev/null || true
    wait "${children[@]}" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 0' TERM INT
wait -n "${children[@]}" || true
exit 1
