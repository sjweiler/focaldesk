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

Both surfaces use the same encrypted database, but records carrying the
reserved `focald:key` attribute are native-only. They are neither enumerable
nor readable through the conventional same-user-open Secret Service API, and
Secret Service clients cannot create or add the reserved attribute.

## Verified compatibility

The integration suite (`tests/integration.sh`, run under `dbus-run-session`)
exercises libsecret (`secret-tool`) store, lookup, search, replace, and clear
over an encrypted `dh-ietf1024-sha256-aes128-cbc-pkcs7` session. It also checks
native-surface ACL allow/deny, ACL hot reload, native/Secret-Service isolation,
and persistence across daemon restart.

The `fuzz/` cargo-fuzz target feeds arbitrary inputs through the production
native request parser, encrypted-store deserializer, and wrapped-key parser:

```sh
cd crates/focald-secrets
cargo fuzz run security_inputs
```

## Store

* `~/.local/share/focaldesk/secrets.db` for development, or the packaged
  system service's `/var/lib/focald-secrets/$UID/secrets.db` — JSON encrypted
  with ChaCha20-Poly1305 (`FSDB1 || nonce || ciphertext`), atomic
  tempfile+rename writes, 0600/0700 permissions, secret values zeroized on
  drop. Mutations use copy-on-write transactions: encrypted persistence must
  succeed before live in-memory state changes. The encrypted database is
  limited to 64 MiB and 4096 items.
* Master key (32 bytes): a `focald.master` systemd service credential supplied
  by the system manager. PAM stages plaintext only in a root-owned `0700`
  directory, starts `focald-secrets@$UID.service`, and removes the source after
  PID 1 copies it into the service's private, read-only credential ramfs.
  `$FOCALD_SECRETS_KEYFILE` is an explicit testing/recovery override.
* **Key provisioning** (`pam-module/`, `keywrap/`): the master key is random,
  wrapped under a KEK derived from the login password
  (Argon2id, 64 MiB, 3 iterations, one lane; ChaCha20-Poly1305 wrap) and stored at
  `~/.local/share/focaldesk/secrets.key.enc`. `pam_focald_secrets.so` in the
  focaldmd stack unwraps it at login and starts the system broker
  (see `config/pam-focaldesk.example`); first login initializes transparently;
  password changes rewrap without touching the store. Because the store key is
  random and merely rewrapped, a password change never re-encrypts the db.
  A root-owned per-UID lock serializes initialization, upgrade, rewrap,
  credential staging, and broker startup. A missing wrapper is never replaced
  when an encrypted database already exists.
  Legacy `FKEY1` PBKDF2 wrappers are accepted and upgraded atomically after a
  successful login. Without PAM, the keytool supports explicit
  development/recovery key files only.
  The PAM module never blocks login: every failure path logs to syslog and
  returns success/ignore — worst case is a locked keyring, never a locked-out user.
* Before reading any key material, the daemon disables process dumpability.
  The packaged system unit also disables core dumps and privately mounts its
  service credential. After establishing its
  D-Bus executor thread, the daemon calls
  `mlockall(MCL_CURRENT|MCL_FUTURE)` so key material and decrypted secrets
  can't be swapped. It also pins current pages before loading the key and
  immediately after decrypting the store. The system unit grants a 384 MiB
  `LimitMEMLOCK` directly and sets `FOCALD_SECRETS_REQUIRE_MLOCK=1`, so
  production startup fails closed if any locking step fails. Development
  sessions warn and continue.
  Locking before zbus creates its worker can make thread-stack allocation fail
  under `RLIMIT_MEMLOCK`. Secret values and protocol copies under this crate's
  control are zeroized after use, but allocator, D-Bus library, and kernel
  transport copies are outside that guarantee.

## ACL (native surface only)

The packaged system service uses root-managed
`/etc/focaldesk/secrets-acl.toml`; development installs use
`~/.config/focaldesk/secrets-acl.toml`. Both are default-deny and
mtime-hot-reloaded:

```toml
[grants."unit:focaldesk-server.service"]
allow = ["ai/*"]
allow_write = []
```

Peer identity: SO_PEERCRED pid, immediately pinned with `pidfd_open` (pid
numbers can't be recycled while the pidfd is held, closing the TOCTOU window)
→ `org.freedesktop.systemd1` `GetUnitByPID` (`unit:<name>`) → fallback
`exe:/proc/<pid>/exe`. Cross-uid peers are rejected
before any ACL check. Grants keyed to interpreter binaries (`exe:.../python3`
etc.) match every script that interpreter runs and are warned about at config
load — use `unit:` identities for anything real.
The D-Bus surface intentionally has no per-application ACL — it exists for
third-party compatibility and matches the standard Secret Service trust model
(any same-user client can access unlocked public items). Native broker records
are excluded from that surface.

## Threat model

The native ACL prevents accidental credential sharing between correctly
configured services. It is not a hard sandbox against arbitrary hostile code
already running as the desktop user:

* the user controls their systemd user manager and can override user units;
* an unlocked broker intentionally serves public Secret Service items to
  same-UID clients;
* a same-uid attacker may have other OS-level avenues unless the surrounding
  services use process sandboxing.

Use separate Unix users, containers, or a mandatory-access-control policy when
credentials must remain secret from actively malicious same-user code. The
system service isolates its bootstrap credential, disables ptrace/core-dump
access, and keeps native records hidden from Secret Service, but those are
defense-in-depth rather than a new same-UID security boundary.

## Native protocol

4-byte big-endian length + JSON. Ops: `ping`, `get`, `set`, `delete`, `list`.
Broker keys (`google/oauth-refresh`) map to store items via the reserved
attribute `focald:key`. See `examples/fsctl.rs` for a zero-dependency client
(`cargo run --example fsctl -- set google/oauth-refresh "tok"`).
The broker accepts at most 64 simultaneous native connections, applies bounded
read/write/idle timeouts, rejects frames over 1 MiB and secret values over
700 KiB, and limits Secret Service session counts and creation rate.

## Install

From the Focaldesk repository root, run `just install-secrets-service` for an
explicit-key user-local development installation. Production Fedora installs
need both `just install-secrets-service-fedora` and
`just install-secrets-pam-fedora`. PAM installation and key provisioning are
documented in [Credential broker](../../docs/secrets.md).

The Fedora service recipe requires `selinux-policy-devel`. It installs the
scoped `focaldesk_secrets_home_t` policy used by the display manager's PAM hook
to read and atomically update the wrapped key, and relabels existing Focaldesk
secret state. The broker is deliberately not D-Bus or public-socket activated:
a system path unit starts it only after PAM creates the root-only staged
credential. The broker then binds its per-user client socket and claims
`org.freedesktop.secrets`.

The service must not run alongside gnome-keyring-daemon (both claim
`org.freedesktop.secrets`); if the name is taken, focald-secrets logs an error
and keeps serving the native surface.

## Design notes / spec deviations

* **Single collection model.** One always-unlocked collection at
  `/org/freedesktop/secrets/collection/default`, also served at the
  `aliases/default` path; `CreateCollection` resolves to it. This is the
  common-case behavior apps expect (gnome-keyring's "login" keyring).
* **No prompts, no locking.** The key is session-provisioned, so public items are
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

* OAuth refresh brokering (daemon holds refresh tokens, clients only ever see
  short-lived access tokens) is the intended next layer: add a
  `get_access_token` op that refreshes server-side. The ACL and storage
  layers here are what it needs.
* A user-visible lock/unlock prompt and in-process locked-state machine remain
  future work; `Lock` still reports the session-unlocked model.
* `memfd_secret` for the key page would remove it even from kernel-side
  accessors; process non-dumpability, fail-closed production memory locking,
  and zeroizing are the current stance.
