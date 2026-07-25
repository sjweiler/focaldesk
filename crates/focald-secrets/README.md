# focald-secrets

Focaldesk's credential broker. One encrypted store, two surfaces:

1. **Native focaldm-ipc surface** — length-prefixed JSON over a Unix socket at
   `$XDG_RUNTIME_DIR/focaldesk/secrets.sock`, SO_PEERCRED-verified, with
   **per-client ACLs** resolved to systemd user units. This is what Focaldesk
   daemons use.
2. **`org.freedesktop.secrets`** on the session bus — a spec-compliant Secret
   Service provider, so third-party apps (libsecret consumers: nm-applet,
   browsers, `secret-tool`, python-secretstorage, oo7 clients) work unmodified
   with no gnome-keyring in the stack.

Both surfaces read and write the same store, so a token stored by a Focaldesk
service is visible to libsecret consumers via its attributes, and vice versa.

## Verified compatibility

The integration suite (`tests/integration.sh`, run under `dbus-run-session`)
exercises libsecret (`secret-tool`) store, lookup, search, replace, and clear
over an encrypted `dh-ietf1024-sha256-aes128-cbc-pkcs7` session. It also checks
native-surface ACL allow/deny, ACL hot reload, cross-surface visibility, and
persistence across daemon restart.

## Store

* `~/.local/share/focaldesk/secrets.db` — JSON encrypted with
  ChaCha20-Poly1305 (`FSDB1 || nonce || ciphertext`), atomic tempfile+rename
  writes, 0600/0700 permissions, secret values zeroized on drop.
* Master key (32 bytes): `$FOCALD_SECRETS_KEYFILE`, else
  `$XDG_RUNTIME_DIR/focaldesk/secrets.key` (tmpfs; dies with the session).
* **Key provisioning** (`pam-module/`, `keywrap/`): the master key is random,
  wrapped under a KEK derived from the login password
  (PBKDF2-HMAC-SHA256, 600k iterations, ChaCha20-Poly1305 wrap) and stored at
  `~/.local/share/focaldesk/secrets.key.enc`. `pam_focald_secrets.so` in the
  focaldmd stack unwraps it at login and writes the runtime key
  (see `config/pam-focaldesk.example`); first login initializes transparently;
  password changes rewrap without touching the store. Because the store key is
  random and merely rewrapped, a password change never re-encrypts the db.
  Without PAM (SSH sessions, testing): `focald-secrets-keytool init|unlock|rewrap|status`.
  The PAM module never blocks login: every failure path logs to syslog and
  returns success/ignore — worst case is a locked keyring, never a locked-out user.
* After establishing its D-Bus executor thread, the daemon calls
  `mlockall(MCL_CURRENT|MCL_FUTURE)` so key material and decrypted secrets
  can't be swapped. Both the broker unit and its parent `user@.service` manager
  need a 128 MiB `LimitMEMLOCK`; a user unit cannot raise the hard limit it
  inherits from the manager. On failure the broker warns and continues.
  Locking before zbus creates its worker can make thread-stack allocation fail
  under `RLIMIT_MEMLOCK`. In-flight secret buffers are `Zeroizing` end-to-end;
  IPC frames are scrubbed after use.

## ACL (native surface only)

`~/.config/focaldesk/secrets-acl.toml`, default-deny, mtime-hot-reloaded:

```toml
[grants."unit:focaldesk-server.service"]
allow = ["ai/*"]
allow_write = []
```

Peer identity: SO_PEERCRED pid, immediately pinned with `pidfd_open` (pid
numbers can't be recycled while the pidfd is held, closing the TOCTOU window)
→ `org.freedesktop.systemd1` `GetUnitByPID` (`unit:<name>`, unforgeable for
user services) → fallback `exe:/proc/<pid>/exe`. Cross-uid peers are rejected
before any ACL check. Grants keyed to interpreter binaries (`exe:.../python3`
etc.) match every script that interpreter runs and are warned about at config
load — use `unit:` identities for anything real.
The D-Bus surface intentionally has no ACL — it exists for third-party
compatibility and matches the standard Secret Service trust model (any
same-user client). Anything you want compartmentalized should live behind
broker keys and be accessed via the native surface by ACL'd units; see
"Limitations" below.

## Native protocol

4-byte big-endian length + JSON. Ops: `ping`, `get`, `set`, `delete`, `list`.
Broker keys (`google/oauth-refresh`) map to store items via the reserved
attribute `focald:key`. See `examples/fsctl.rs` for a zero-dependency client
(`cargo run --example fsctl -- set google/oauth-refresh "tok"`).

## Install

From the Focaldesk repository root, run `just install-secrets-service` for a
user-local development installation or `just install-secrets-service-fedora`
for the Fedora system layout. PAM installation and key provisioning are
documented in [Credential broker](../../docs/secrets.md).

The service must not run alongside gnome-keyring-daemon (both claim
`org.freedesktop.secrets`); if the name is taken, focald-secrets logs an error
and keeps serving the native surface.

## Design notes / spec deviations

* **Single collection model.** One always-unlocked collection at
  `/org/freedesktop/secrets/collection/default`, also served at the
  `aliases/default` path; `CreateCollection` resolves to it. This is the
  common-case behavior apps expect (gnome-keyring's "login" keyring).
* **No prompts, no locking.** The key is session-provisioned, so everything is
  unlocked for the session lifetime; all prompt returns are `"/"` (no prompt
  required, per spec). `Lock` reports nothing locked.
* **zbus flattening gotcha** (if you extend the D-Bus surface): zbus flattens a
  top-level struct return into multiple out-arguments. `Item.GetSecret` returns
  a 1-tuple `(WireSecret,)` to keep the wire signature `(oayays)`. libsecret
  tolerates the flattened form; python-secretstorage does not.
* **DH interop detail:** the shared secret is left-padded to 128 bytes before
  HKDF — matching libsecret/oo7. Skipping the pad breaks ~0.4% of handshakes
  (leading-zero secrets) in a way that's miserable to debug.

## Limitations & roadmap

* The D-Bus surface is same-user-open by convention; per-sender ACLs there
  would need `GetConnectionUnixProcessID` lookups — planned, same identity code
  path as the native surface.
* OAuth refresh brokering (daemon holds refresh tokens, clients only ever see
  short-lived access tokens) is the intended next layer: add a
  `get_access_token` op that refreshes server-side. The ACL and storage
  layers here are what it needs.
* `memfd_secret` for the key page would remove it even from kernel-side
  accessors; mlockall + zeroizing is the current stance.
