#!/usr/bin/env bash
# Run a disposable Net-SNMP master and a read-only FocalDesk AgentX subagent.
set -euo pipefail

root=${1:?provide an authorized numeric fdObjects OID}
snapshot=${2:-${XDG_RUNTIME_DIR:?set XDG_RUNTIME_DIR or pass a snapshot}/focaldesk-vulkan-metrics.json}
port=${FOCALDESK_SNMP_LAB_PORT:-11661}
address=${FOCALDESK_SNMP_LAB_ADDRESS:-127.0.0.1}
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
subagent="$repo/scripts/focaldesk-monitor-agentx.py"

# Validate the value before inserting it into a Net-SNMP configuration file.
python3 - "$subagent" "$root" "$port" "$address" <<'PY'
import ipaddress, runpy, sys
runpy.run_path(sys.argv[1])["parse_root"](sys.argv[2])
if not 1024 <= int(sys.argv[3]) <= 65535 or not sys.argv[3].isascii() or not sys.argv[3].isdecimal():
    raise SystemExit("expected an unprivileged UDP port")
address = ipaddress.IPv4Address(sys.argv[4])
private_networks = ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16")
if not (address.is_loopback or any(address in ipaddress.IPv4Network(net) for net in private_networks)):
    raise SystemExit("expected a loopback or private IPv4 address")
PY

umask 077
lab=$(mktemp -d /tmp/focaldesk-agentx.XXXXXXXX)
master_pid=
agent_pid=
cleanup() {
    if [[ -n "$agent_pid" ]]; then kill "$agent_pid" 2>/dev/null || true; fi
    if [[ -n "$master_pid" ]]; then kill "$master_pid" 2>/dev/null || true; fi
    wait 2>/dev/null || true
    rm -rf -- "$lab"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

auth=$(python3 -c 'import secrets; print(secrets.token_hex(24))')
priv=$(python3 -c 'import secrets; print(secrets.token_hex(24))')
mkdir "$lab/state"
cat > "$lab/master.conf" <<EOF
agentAddress udp:$address:$port
sysObjectID $root
master agentx
agentXSocket $lab/agentx.sock
agentXPerms 0600 0700
view focalLabView included .1.3.6.1.2.1.1
view focalLabView included $root
rouser focalLab priv -V focalLabView
EOF
printf 'createUser focalLab SHA-256 %s AES %s\n' "$auth" "$priv" > "$lab/state/snmpd.conf"
cat > "$lab/snmp.conf" <<EOF
defVersion 3
defSecurityName focalLab
defSecurityLevel authPriv
defAuthType SHA-256
defAuthPassphrase $auth
defPrivType AES
defPrivPassphrase $priv
EOF
export SNMPCONFPATH="$lab"
export SNMP_PERSISTENT_DIR="$lab/state"
target="udp:$address:$port"
start_master() {
    snmpd -f -Lo -C -c "$lab/master.conf,$lab/state/snmpd.conf" -p "$lab/master.pid" > "$lab/master.log" 2>&1 &
    master_pid=$!
}
start_master
python3 "$subagent" --socket "$lab/agentx.sock" --root "$root" \
    --snapshot "$snapshot" --registry "$lab/indexes.sqlite3" > "$lab/agent.log" 2>&1 &
agent_pid=$!

wait_for_inventory() {
    local attempt
    for ((attempt=0; attempt<30; attempt++)); do
        if ! kill -0 "$master_pid" "$agent_pid" 2>/dev/null; then break; fi
        if snmpget -On -t 0.2 -r 0 "$target" "$root.1.1.0" 2>/dev/null |
            grep -q 'Timeticks:'; then return 0; fi
        sleep 0.2
    done
    tail -30 "$lab/master.log" "$lab/agent.log" >&2
    return 1
}

wait_for_inventory
snmpget -Oqvn -t 1 -r 0 "$target" .1.3.6.1.2.1.1.2.0 | grep -Fx ".${root#.}"
snmpbulkwalk -On -t 1 -r 0 "$target" "$root" > "$lab/walk"
grep -q 'Counter64:' "$lab/walk"
cat "$lab/walk"
if snmpset -On -t 1 -r 0 "$target" "$root.1.1.0" t 1 > "$lab/set" 2>&1; then
    echo "unexpected successful SET" >&2
    exit 1
fi
grep -Eqi 'noAccess|notWritable|authorizationError' "$lab/set"
if snmpget -v3 -u unknownFocalLab -l noAuthNoPriv -t 0.3 -r 0 \
    "$target" "$root.1.1.0" >/dev/null 2>&1; then
    echo "unexpected access for an unknown SNMPv3 user" >&2
    exit 1
fi
kill "$master_pid"
wait "$master_pid" || true
master_pid=
start_master
wait_for_inventory
echo "Verified authPriv GET/GETBULK, denied write and unknown user, and AgentX reconnect."

if [[ ${FOCALDESK_SNMP_LAB_HOLD:-0} == 1 ]]; then
    printf 'Lab endpoint: %s:%s\nPrivate SNMPv3 client config: %s/snmp.conf\n' "$address" "$port" "$lab"
    printf 'Press Ctrl-C to stop the lab.\n'
    wait "$master_pid"
fi
