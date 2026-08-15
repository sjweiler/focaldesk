#!/bin/bash
# Integration test for focald-secrets.
# Runs inside dbus-run-session:
#   1. third-party compatibility via secret-tool (libsecret, encrypted DH session)
#   2. native IPC surface incl. ACL allow/deny and hot reload
#   3. persistence across daemon restart
set -u
if [ -z "${FOCALD_SECRETS_TEST_DBUS_SESSION:-}" ]; then
    exec dbus-run-session -- env FOCALD_SECRETS_TEST_DBUS_SESSION=1 bash "$0" "$@"
fi
cd "$(dirname "$0")/.."

export XDG_RUNTIME_DIR=$(mktemp -d)
export HOME_TEST=$(mktemp -d)
export FOCALD_SECRETS_DB="$HOME_TEST/secrets.db"
export FOCALD_SECRETS_ACL="$HOME_TEST/acl.toml"
export FOCALD_SECRETS_KEYFILE="$HOME_TEST/session.key"
export FOCALD_SECRETS_IPC_TIMEOUT_MS=300
dd if=/dev/urandom of="$FOCALD_SECRETS_KEYFILE" bs=32 count=1 status=none
chmod 600 "$FOCALD_SECRETS_KEYFILE"
export RUST_LOG=debug
if [ -z "${BIN:-}" ]; then
    cargo build -p focald-secrets
    BIN=../../target/debug/focald-secrets
fi

PYEXE=$(readlink -f "$(which python3)")
cat > "$FOCALD_SECRETS_ACL" <<EOF
[grants."exe:$PYEXE"]
allow = ["google/*", "test/*"]
EOF

PASS=0; FAIL=0
check() { if [ "$1" = 0 ]; then echo "PASS: $2"; PASS=$((PASS+1)); else echo "FAIL: $2"; FAIL=$((FAIL+1)); fi }

UNIT=../../packaging/systemd/system/focald-secrets@.service
PATH_UNIT=../../packaging/systemd/system/focald-secrets@.path
RUNTIME_UNIT=../../packaging/systemd/system/focaldesk-runtime-dir@.service
USER_DROPIN=../../packaging/systemd/system/user@.service.d/90-focald-secrets.conf
SELINUX_TE=../../packaging/selinux/focaldesk_secrets.te
SELINUX_FC=../../packaging/selinux/focaldesk_secrets.fc
grep -q '^Environment=FOCALD_SECRETS_REQUIRE_MLOCK=1$' "$UNIT" &&
    grep -q '^LimitMEMLOCK=384M$' "$UNIT" &&
    grep -q '^MemoryMax=512M$' "$UNIT" &&
    grep -q '^MemorySwapMax=0$' "$UNIT" &&
    grep -q '^LimitNOFILE=512$' "$UNIT" &&
    grep -q '^TasksMax=128$' "$UNIT"
check $? "production unit requires locked memory and bounded resources"

grep -q '^PathExists=/run/focald-secrets/%i/master$' "$PATH_UNIT" &&
    grep -q '^Unit=focald-secrets@%i.service$' "$PATH_UNIT" &&
    grep -q '^User=%i$' "$RUNTIME_UNIT" &&
    grep -q '^ExecStart=/usr/bin/mkdir -p /run/user/%i/focaldesk$' "$RUNTIME_UNIT" &&
    grep -q '^ProtectHome=read-only$' "$RUNTIME_UNIT" &&
    grep -q '^Wants=focald-secrets@%i.path$' "$USER_DROPIN" &&
    ! grep -q 'focald-secrets@%i.socket' "$UNIT"
check $? "login watches the root-only credential before starting the broker"

grep -q '^type focaldesk_secrets_home_t;' "$SELINUX_TE" &&
    grep -q '^userdom_search_user_home_content(xdm_t)$' "$SELINUX_TE" &&
    grep -q '^manage_dirs_pattern(xdm_t, focaldesk_secrets_home_t, focaldesk_secrets_home_t)$' "$SELINUX_TE" &&
    grep -q '^manage_files_pattern(xdm_t, focaldesk_secrets_home_t, focaldesk_secrets_home_t)$' "$SELINUX_TE" &&
    grep -q 'focaldesk/secrets\\.key\\.enc' "$SELINUX_FC" &&
    [ ! -e ../../packaging/dbus/org.freedesktop.secrets.service ]
check $? "Fedora packaging grants scoped PAM key access without invalid D-Bus activation"

LOCK_TEST=$(mktemp -d)
dd if=/dev/urandom of="$LOCK_TEST/key" bs=32 count=1 status=none
chmod 600 "$LOCK_TEST/key"
(
    ulimit -l 0
    FOCALD_SECRETS_REQUIRE_MLOCK=1 \
        FOCALD_SECRETS_KEYFILE="$LOCK_TEST/key" \
        FOCALD_SECRETS_DB="$LOCK_TEST/secrets.db" \
        FOCALD_SECRETS_ACL="$LOCK_TEST/acl.toml" \
        XDG_RUNTIME_DIR="$LOCK_TEST" \
        "$BIN" >"$LOCK_TEST/daemon.log" 2>&1
)
[ $? -ne 0 ]; check $? "production mode fails closed when memory locking is unavailable"

CREDENTIAL_DIR=$(mktemp -d)
CREDENTIAL_HOME=$(mktemp -d)
dd if=/dev/urandom of="$CREDENTIAL_DIR/focald.master" bs=32 count=1 status=none
chmod 600 "$CREDENTIAL_DIR/focald.master"
env -u FOCALD_SECRETS_KEYFILE \
    CREDENTIALS_DIRECTORY="$CREDENTIAL_DIR" \
    XDG_RUNTIME_DIR="$CREDENTIAL_HOME" \
    FOCALD_SECRETS_DB="$CREDENTIAL_HOME/secrets.db" \
    FOCALD_SECRETS_ACL="$CREDENTIAL_HOME/acl.toml" \
    $BIN > "$CREDENTIAL_HOME/daemon.log" 2>&1 &
HPID=$!
sleep 1
[ -e "$CREDENTIAL_DIR/focald.master" ]; check $? "daemon reads systemd credential without mutating it"
kill $HPID 2>/dev/null; wait $HPID 2>/dev/null

LEGACY_RUNTIME=$(mktemp -d)
LEGACY_HOME=$(mktemp -d)
mkdir -m 700 "$LEGACY_RUNTIME/focaldesk"
dd if=/dev/urandom of="$LEGACY_RUNTIME/focaldesk/secrets.key" bs=32 count=1 status=none
chmod 600 "$LEGACY_RUNTIME/focaldesk/secrets.key"
env -u FOCALD_SECRETS_KEYFILE -u CREDENTIALS_DIRECTORY \
    XDG_RUNTIME_DIR="$LEGACY_RUNTIME" \
    FOCALD_SECRETS_DB="$LEGACY_HOME/secrets.db" \
    FOCALD_SECRETS_ACL="$LEGACY_HOME/acl.toml" \
    $BIN > "$LEGACY_HOME/daemon.log" 2>&1 &
LPID=$!
wait $LPID 2>/dev/null
LEGACY_RC=$?
[ $LEGACY_RC -ne 0 ]; check $? "daemon rejects implicit same-UID runtime handoff"
[ -e "$LEGACY_RUNTIME/focaldesk/secrets.key" ]; check $? "rejected legacy handoff is not consumed"

$BIN > "$HOME_TEST/daemon.log" 2>&1 &
DPID=$!
sleep 1.5

echo "=== third-party surface (secret-tool / libsecret, DH session) ==="
printf 'hunter2-oauth-token' | secret-tool store --label="Google OAuth" service google account steven 2>&1
check $? "secret-tool store"

OUT=$(secret-tool lookup service google account steven)
[ "$OUT" = "hunter2-oauth-token" ]; check $? "secret-tool lookup roundtrip (got: $OUT)"

printf 'replaced-token' | secret-tool store --label="Google OAuth" service google account steven
OUT=$(secret-tool lookup service google account steven)
[ "$OUT" = "replaced-token" ]; check $? "secret-tool replace semantics"

secret-tool search service google 2>&1 | grep -q "attribute.account = steven"
check $? "secret-tool search returns attributes"

printf 'second-secret' | secret-tool store --label="Other" service other account bob
OUT=$(secret-tool lookup service other)
[ "$OUT" = "second-secret" ]; check $? "second item independent (got: $OUT)"

secret-tool clear service other account bob
secret-tool lookup service other account bob > /dev/null 2>&1
[ $? -ne 0 ]; check $? "secret-tool clear deletes item"

python3 - <<'PYEOF'
import dbus
import os
import sys

address = os.environ["DBUS_SESSION_BUS_ADDRESS"]
owner = dbus.bus.BusConnection(address)
attacker = dbus.bus.BusConnection(address)
service = owner.get_object("org.freedesktop.secrets", "/org/freedesktop/secrets")
api = dbus.Interface(service, "org.freedesktop.Secret.Service")
_, session_path = api.OpenSession("plain", dbus.String("", variant_level=1))

rejected = False
try:
    foreign = attacker.get_object("org.freedesktop.secrets", session_path)
    dbus.Interface(foreign, "org.freedesktop.Secret.Session").Close()
except dbus.DBusException:
    rejected = True

owned = owner.get_object("org.freedesktop.secrets", session_path)
dbus.Interface(owned, "org.freedesktop.Secret.Session").Close()

sessions = []
limited = False
try:
    for _ in range(32):
        _, path = api.OpenSession("plain", dbus.String("", variant_level=1))
        sessions.append(path)
    try:
        api.OpenSession("plain", dbus.String("", variant_level=1))
    except dbus.DBusException:
        limited = True
finally:
    for path in sessions:
        session = owner.get_object("org.freedesktop.secrets", path)
        dbus.Interface(session, "org.freedesktop.Secret.Session").Close()

sys.exit(0 if rejected and limited else 1)
PYEOF
check $? "D-Bus sessions enforce ownership and per-caller limits"

printf 'must-not-store' |
    secret-tool store --label="Reserved attribute injection" focald:key injected \
    >/dev/null 2>&1
[ $? -ne 0 ]; check $? "D-Bus rejects reserved native attributes"

echo "=== native IPC surface (ACL) ==="
python3 - <<'PYEOF'
import socket, struct, json, base64, os, sys, time

sock_path = os.environ["XDG_RUNTIME_DIR"] + "/focaldesk/secrets.sock"
def rpc(req):
    s = socket.socket(socket.AF_UNIX)
    s.connect(sock_path)
    body = json.dumps(req).encode()
    s.sendall(struct.pack(">I", len(body)) + body)
    ln = struct.unpack(">I", s.recv(4))[0]
    data = b""
    while len(data) < ln: data += s.recv(ln - len(data))
    s.close()
    return json.loads(data)

results = []
def check(cond, name): results.append((bool(cond), name))

r = rpc({"op": "ping"}); check(r.get("ok"), "ipc ping")

# Resolve the identity exactly as the daemon did. In production-like test
# environments Python can belong to a systemd unit, so assuming exe:python
# would test the wrong ACL principal.
identity = None
deadline = time.monotonic() + 2
needle = "peer pid=%d identity=" % os.getpid()
while time.monotonic() < deadline and identity is None:
    with open(os.environ["HOME_TEST"] + "/daemon.log") as log:
        for line in log:
            if needle in line:
                identity = line.split("identity=", 1)[1].strip()
    if identity is None:
        time.sleep(0.02)
check(identity is not None, "ipc test resolved daemon peer identity")
if identity is None:
    sys.exit(1)
escaped_identity = identity.replace("\\", "\\\\").replace('"', '\\"')
acl_path = os.environ["FOCALD_SECRETS_ACL"]
open(acl_path, "w").write(
    '[grants."%s"]\nallow = ["google/*", "test/*"]\n' % escaped_identity
)
os.utime(acl_path)

val = base64.b64encode(b"refresh-token-xyz").decode()
r = rpc({"op": "set", "key": "google/oauth-refresh", "value_b64": val,
         "label": "Google refresh", "attributes": {"account": "steven"}})
check(r.get("ok"), "ipc set allowed key")

r = rpc({"op": "get", "key": "google/oauth-refresh"})
check(r.get("ok") and base64.b64decode(r["value_b64"]) == b"refresh-token-xyz",
      "ipc get roundtrip")

too_large = base64.b64encode(b"x" * (700 * 1024 + 1)).decode()
r = rpc({"op": "set", "key": "test/too-large", "value_b64": too_large})
check(not r.get("ok") and "limit" in r.get("error", ""), "ipc rejects oversized secrets")

r = rpc({"op": "set", "key": "microsoft/token", "value_b64": val})
check(not r.get("ok") and "denied" in r.get("error",""), "ipc ACL denies microsoft/* write")

r = rpc({"op": "get", "key": "microsoft/anything"})
check(not r.get("ok"), "ipc ACL denies microsoft/* read")

r = rpc({"op": "list", "prefix": None})
keys = [e["key"] for e in r.get("entries", [])]
check(r.get("ok") and "google/oauth-refresh" in keys, "ipc list shows granted keys")

# Cross-surface: item stored via secret-tool is NOT visible to list (no focald:key)
check(all(k.startswith(("google/", "test/")) for k in keys), "ipc list only broker-keyed, ACL-passing items")

# Hot reload: revoke google/*, expect deny
open(acl_path, "w").write(
    '[grants."%s"]\nallow = ["test/*"]\n' % escaped_identity
)
os.utime(acl_path)
r = rpc({"op": "get", "key": "google/oauth-refresh"})
check(not r.get("ok"), "ipc ACL hot reload revokes access")

r = rpc({"op": "set", "key": "test/scratch", "value_b64": val})
r = rpc({"op": "delete", "key": "test/scratch"})
check(r.get("ok"), "ipc delete")

slow = socket.socket(socket.AF_UNIX)
slow.settimeout(2)
slow.connect(sock_path)
slow.sendall(struct.pack(">I", 64) + b"{")
time.sleep(0.5)
check(slow.recv(1) == b"", "ipc closes incomplete frames after timeout")
slow.close()

oversized = socket.socket(socket.AF_UNIX)
oversized.settimeout(2)
oversized.connect(sock_path)
oversized.sendall(struct.pack(">I", (1 << 20) + 1))
check(oversized.recv(1) == b"", "ipc rejects oversized frames before allocation")
oversized.close()

ok = all(c for c, _ in results)
for c, name in results: print(("PASS: " if c else "FAIL: ") + name)
sys.exit(0 if ok else 1)
PYEOF
check $? "native IPC suite"

echo "=== cross-surface isolation ==="
# Native broker records are reserved and must not be exposed through the
# conventional same-user-open Secret Service API.
OUT=$(secret-tool lookup account steven focald:key google/oauth-refresh 2>/dev/null)
[ -z "$OUT" ]; check $? "libsecret cannot read ACL-protected broker item (got: $OUT)"

echo "=== persistence across restart ==="
kill $DPID; wait $DPID 2>/dev/null
$BIN >> "$HOME_TEST/daemon.log" 2>&1 &
DPID=$!
sleep 1.5
OUT=$(secret-tool lookup service google account steven)
[ "$OUT" = "replaced-token" ]; check $? "items survive daemon restart (encrypted store reload)"

kill $DPID 2>/dev/null; wait $DPID 2>/dev/null
echo
echo "RESULTS: $PASS passed, $FAIL failed"
[ $FAIL = 0 ]
