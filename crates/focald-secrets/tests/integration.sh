#!/bin/bash
# Integration test for focald-secrets.
# Runs inside dbus-run-session:
#   1. third-party compatibility via secret-tool (libsecret, encrypted DH session)
#   2. native IPC surface incl. ACL allow/deny and hot reload
#   3. persistence across daemon restart
set -u
cd "$(dirname "$0")/.."

export XDG_RUNTIME_DIR=$(mktemp -d)
export HOME_TEST=$(mktemp -d)
export FOCALD_SECRETS_DB="$HOME_TEST/secrets.db"
export FOCALD_SECRETS_ACL="$HOME_TEST/acl.toml"
export RUST_LOG=debug
BIN=${BIN:-../../target/debug/focald-secrets}

PYEXE=$(readlink -f "$(which python3)")
cat > "$FOCALD_SECRETS_ACL" <<EOF
[grants."exe:$PYEXE"]
allow = ["google/*", "test/*"]
EOF

PASS=0; FAIL=0
check() { if [ "$1" = 0 ]; then echo "PASS: $2"; PASS=$((PASS+1)); else echo "FAIL: $2"; FAIL=$((FAIL+1)); fi }

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
[ "$OUT" = "second-secret" ]; check $? "second item independent"

secret-tool clear service other account bob
secret-tool lookup service other account bob > /dev/null 2>&1
[ $? -ne 0 ]; check $? "secret-tool clear deletes item"

echo "=== native IPC surface (ACL) ==="
python3 - <<'PYEOF'
import socket, struct, json, base64, os, sys

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

val = base64.b64encode(b"refresh-token-xyz").decode()
r = rpc({"op": "set", "key": "google/oauth-refresh", "value_b64": val,
         "label": "Google refresh", "attributes": {"account": "steven"}})
check(r.get("ok"), "ipc set allowed key")

r = rpc({"op": "get", "key": "google/oauth-refresh"})
check(r.get("ok") and base64.b64decode(r["value_b64"]) == b"refresh-token-xyz",
      "ipc get roundtrip")

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
acl_path = os.environ["FOCALD_SECRETS_ACL"]
open(acl_path, "w").write('[grants."exe:%s"]\nallow = ["test/*"]\n' % os.path.realpath(sys.executable))
os.utime(acl_path)
r = rpc({"op": "get", "key": "google/oauth-refresh"})
check(not r.get("ok"), "ipc ACL hot reload revokes access")

r = rpc({"op": "set", "key": "test/scratch", "value_b64": val})
r = rpc({"op": "delete", "key": "test/scratch"})
check(r.get("ok"), "ipc delete")

ok = all(c for c, _ in results)
for c, name in results: print(("PASS: " if c else "FAIL: ") + name)
sys.exit(0 if ok else 1)
PYEOF
check $? "native IPC suite"

echo "=== cross-surface visibility ==="
# Item stored via native IPC should be findable by libsecret via its attributes
OUT=$(secret-tool lookup account steven focald:key google/oauth-refresh 2>/dev/null)
[ "$OUT" = "refresh-token-xyz" ]; check $? "libsecret can read broker-stored item"

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
