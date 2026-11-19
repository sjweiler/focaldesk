#!/usr/bin/env bash
# Exercise GPU and compositor events over loopback SNMPv3 INFORMs.
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
umask 077
lab=$(mktemp -d /tmp/focaldesk-notify.XXXXXXXX)
children=()
cleanup() {
    if (( ${#children[@]} )); then kill "${children[@]}" 2>/dev/null || true; fi
    wait 2>/dev/null || true
    rm -rf -- "$lab"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

root=.1.3.6.1.4.1.67726.1.1
notification=.1.3.6.1.4.1.67726.1.0.4
stale_notification=.1.3.6.1.4.1.67726.1.0.5
resumed_notification=.1.3.6.1.4.1.67726.1.0.6
display_failed_notification=.1.3.6.1.4.1.67726.1.0.7
display_recovered_notification=.1.3.6.1.4.1.67726.1.0.8
auth=$(python3 -c 'import secrets; print(secrets.token_hex(24))')
priv=$(python3 -c 'import secrets; print(secrets.token_hex(24))')
mkdir "$lab/receiver-state" "$lab/master-state"
printf 'createUser focalNotify SHA-256 %s AES %s\n' "$auth" "$priv" > "$lab/receiver-state/snmptrapd.conf"
printf 'authUser log focalNotify priv\n' > "$lab/receiver.conf"
cat > "$lab/master.conf" <<CONFIG
agentAddress udp:127.0.0.1:11664
master agentx
agentXSocket $lab/agentx.sock
agentXPerms 0600 0700
trapsess -Ci -v3 -l authPriv -u focalNotify -a SHA-256 -A $auth -x AES -X $priv -t 1 -r 1 udp:127.0.0.1:11665
CONFIG
SNMP_PERSISTENT_DIR="$lab/receiver-state" snmptrapd -f -Lo -On -C \
    -c "$lab/receiver.conf,$lab/receiver-state/snmptrapd.conf" udp:127.0.0.1:11665 > "$lab/receiver.log" 2>&1 &
children+=("$!")
SNMP_PERSISTENT_DIR="$lab/master-state" snmpd -f -Lo -C \
    -c "$lab/master.conf" -p "$lab/master.pid" > "$lab/master.log" 2>&1 &
children+=("$!")

write_snapshot() {
    python3 - "$lab" "$1" "$2" "${3:-notification-fixture}" "${4:-active}" "${5:-0}" "${6:-0}" <<'PY'
from pathlib import Path
import json, sys, time

lab, state, attempts = Path(sys.argv[1]), sys.argv[2], int(sys.argv[3])
sample = {
    "schema_version": 1, "producer_epoch": sys.argv[4], "uptime_seconds": 10,
    "sample_unix_ms": time.time_ns() // 1_000_000,
    "gpus": {"test-gpu-1": {
        "state": state, "device_losses": 1, "rebuild_attempts": attempts,
        "rebuild_successes": 0, "displays": {"DP-3": {
            "state": sys.argv[5], "failures": int(sys.argv[6]), "recoveries": int(sys.argv[7]),
            "presented_frames": 10, "width": 3840, "height": 2160, "refresh_millihz": 120000,
        }},
    }},
}
temporary = lab / "snapshot.tmp"
temporary.write_text(json.dumps(sample))
temporary.replace(lab / "snapshot.json")
PY
}

write_snapshot ready 0
python3 "$repo/scripts/focaldesk-monitor-agentx.py" --socket "$lab/agentx.sock" \
    --root "$root" --gpu-failure-notification "$notification" --compositor-notifications --display-notifications \
    --snapshot "$lab/snapshot.json" --registry "$lab/indexes.sqlite3" > "$lab/agent.log" 2>&1 &
children+=("$!")
sleep 3
write_snapshot ready 0 notification-fixture disconnected
sleep 2
write_snapshot ready 0 notification-fixture disabled
sleep 2
if grep -Fq "$display_failed_notification" "$lab/receiver.log"; then
    echo 'Unexpected display failure event on unplug or disable.' >&2
    exit 1
fi
write_snapshot failed 1 notification-fixture failed 1 0

for ((attempt=0; attempt<20; attempt++)); do
    if grep -Fq "$notification" "$lab/receiver.log"; then break; fi
    sleep 0.5
done
sleep 2
if ! grep -F "$notification" "$lab/receiver.log" | grep -F 'test-gpu-1' |
    grep -F 'notification-fixture' || [[ $(grep -Fc "$notification" "$lab/receiver.log") != 1 ]]; then
    tail -30 "$lab/agent.log" "$lab/receiver.log" >&2
    exit 1
fi
echo 'Verified one SNMPv3 authPriv INFORM for the failed GPU, with no duplicates while it stayed failed.'

write_snapshot ready 1 notification-fixture active 1 1
for ((attempt=0; attempt<20; attempt++)); do
    if grep -Fq "$display_recovered_notification" "$lab/receiver.log"; then break; fi
    sleep 0.5
done
if ! grep -F "$display_failed_notification" "$lab/receiver.log" | grep -F 'DP-3' ||
   ! grep -F "$display_recovered_notification" "$lab/receiver.log" | grep -F 'DP-3' ||
   [[ $(grep -Fc "$display_failed_notification" "$lab/receiver.log") != 1 ]] ||
   [[ $(grep -Fc "$display_recovered_notification" "$lab/receiver.log") != 1 ]]; then
    tail -30 "$lab/agent.log" "$lab/receiver.log" >&2
    exit 1
fi
echo 'Verified one display failure, one recovery, and no failure for unplug or disable.'

# Remove the copy while preserving the monitor and its last fresh epoch.
mv "$lab/snapshot.json" "$lab/snapshot-paused.json"
for ((attempt=0; attempt<75; attempt++)); do
    if grep -Fq "$stale_notification" "$lab/receiver.log"; then break; fi
    sleep 0.5
done
write_snapshot ready 0 notification-fixture-restarted
for ((attempt=0; attempt<20; attempt++)); do
    if grep -Fq "$resumed_notification" "$lab/receiver.log"; then break; fi
    sleep 0.5
done
if ! grep -F "$stale_notification" "$lab/receiver.log" | grep -F 'notification-fixture' ||
   ! grep -F "$resumed_notification" "$lab/receiver.log" | grep -F 'notification-fixture-restarted' |
      grep -F "$root.1.13.0 = INTEGER: 1" ||
   [[ $(grep -Fc "$stale_notification" "$lab/receiver.log") != 1 ]] ||
   [[ $(grep -Fc "$resumed_notification" "$lab/receiver.log") != 1 ]]; then
    tail -30 "$lab/agent.log" "$lab/receiver.log" >&2
    exit 1
fi
echo 'Verified one stale event and one recovery event with restart detection.'
