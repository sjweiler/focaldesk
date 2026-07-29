# Credential broker

Focaldesk includes `focald-secrets`, an encrypted per-user credential store.
It exposes two interfaces backed by the same encrypted database:

- `%t/focaldesk/secrets.sock` is a length-prefixed JSON protocol for Focaldesk
  services. The broker identifies the calling systemd user unit and applies
  `/etc/focaldesk/secrets-acl.toml` in the packaged system service, or
  `$XDG_CONFIG_HOME/focaldesk/secrets-acl.toml` in development; missing grants
  are denied.
- `org.freedesktop.secrets` is the standard session-bus Secret Service API for
  applications using libsecret, `secret-tool`, or compatible libraries.

For the PAM-managed system service, the encrypted database is
`/var/lib/focald-secrets/$UID/secrets.db` in a private per-UID state directory
created by systemd. On first start after upgrading, an existing
`$XDG_DATA_HOME/focaldesk/secrets.db` is copied there atomically and retained as
a recovery copy. Development installs continue to use the XDG data path. At
login, PAM unwraps the database's random master key into a root-only staging
directory. PID 1 copies it into the broker's private, read-only systemd
credential ramfs, then PAM removes the source. A password-wrapped copy is
stored as `$XDG_DATA_HOME/focaldesk/secrets.key.enc`.
Root-owned per-user locking serializes initialization, upgrades, password
rewraps, credential staging, and service startup. If an encrypted database
already exists but its wrapped key is missing, PAM refuses to create a
replacement key.

Database mutations are copy-on-write transactions: live state changes only
after the encrypted tempfile is synced and renamed successfully. The encrypted
store is capped at 64 MiB and 4096 items. Native IPC accepts at most 64
connections and enforces bounded frame and idle timeouts, a 1 MiB frame limit,
and a 700 KiB value limit. Secret Service session creation is count- and
rate-limited.

Records created through the native API carry the reserved `focald:key`
attribute. They are deliberately hidden from the standard Secret Service API;
Secret Service clients cannot create or add that reserved attribute.

## Build and install

For a user-local development installation:

```sh
just install-secrets-service
```

For the Fedora system layout:

```sh
just install-secrets-service-fedora
just install-secrets-pam-fedora
```

The PAM recipe only installs the module. It intentionally does not edit
`/etc/pam.d`, because a bad automated PAM edit can prevent login.

The install recipe includes a migration unit, but normal broker startup does
not run it automatically because activating GNOME Keyring would race
Focaldesk for `org.freedesktop.secrets`. Run the migration explicitly before
switching the session to Focaldesk's provider. It copies readable items without
deleting or modifying the originals.

`focald-secrets` and another Secret Service provider such as
`gnome-keyring-daemon` cannot own `org.freedesktop.secrets` simultaneously.
When another provider owns the name, the native Focaldesk socket continues to
work, but `secret-tool` talks to the other provider.

Preview or run the migration manually with GNOME Keyring active:

```sh
focald-secrets-import-gnome-keyring --dry-run
focald-secrets-import-gnome-keyring --import
```

The import is idempotent and records source object paths in the encrypted
store. It does not infer provider roles from labels or attributes; after
review, explicitly copy selected provider credentials to `ai/*` keys.

Stop `focald-secrets.service` before running a manual import so it cannot write
the database concurrently.

## Unlock at login with focaldmd

FocalDesk ships a complete Fedora human-login policy at
`packaging/pam/focaldmd-fedora`. Install it together with the native secrets
PAM module:

```sh
just install-focaldm-pam-fedora
```

The policy uses `pam_focald_secrets.so` after `pam_systemd.so` to stage a
root-only credential and connect to `focald-secrets@$UID.socket`. The socket
starts with `user@$UID.service` and activates the broker without a privileged
process launch from the PAM stack. The hook waits for a broker ping before
removing the staged credential. Focaldesk then owns
`org.freedesktop.secrets`, so Chrome and other Secret Service clients use the
Focaldesk store without a second keyring prompt.

The first successful login creates a random store key and wraps it under an
Argon2id-derived key (64 MiB, three iterations, one lane). Existing
PBKDF2-based `FKEY1` files are accepted and atomically upgraded to `FKEY2` after
a successful login. All failure paths are optional and do not deny login.

Distribution PAM stacks differ; the packaged policy targets Fedora's
`password-auth` and `postlogin` stacks. On other distributions, add
`pam_focald_secrets.so` to the equivalent human login policy rather than
installing the Fedora file. If the password-change stack is not integrated,
run `focald-secrets-keytool rewrap` when changing the login password.

## Memory locking

Before loading its master key, `focald-secrets` disables process dumpability;
the packaged system unit also sets `LimitCORE=0`, uses a private mount
namespace for its credential, and applies filesystem/process sandboxing. It pins its address space with
`mlockall(MCL_CURRENT|MCL_FUTURE)` after establishing the D-Bus executor. The
broker also locks current pages before loading the key and immediately after
decrypting the store. The production unit requests a 384 MiB
`LimitMEMLOCK`, caps total memory at 512 MiB, and fails startup if memory
locking is unavailable. Its cgroup is prohibited from using swap as a second
line of defense. Development services continue with a warning so they remain
usable under restrictive shells and containers.

Confirm that the broker started without an `mlockall failed` warning:

```sh
systemctl status "focald-secrets@$(id -u).service"
journalctl -b -u "focald-secrets@$(id -u).service"
```

On a running system, `/proc/$PID/status` should report a nonzero `VmLck` for
the broker process. Do not weaken this to `--password-store=basic` for Chrome;
that bypasses protected Secret Service storage rather than fixing the session
limit.

For development sessions without focaldmd/PAM, choose an explicit private key
path and pass the same path to the daemon:

```sh
export FOCALD_SECRETS_KEYFILE="$XDG_RUNTIME_DIR/focaldesk/secrets.dev.key"
focald-secrets-keytool init   # or: focald-secrets-keytool unlock
focald-secrets
```

The daemon consumes the explicit development/recovery key configured above.
The old implicit `$XDG_RUNTIME_DIR/focaldesk/secrets.key` lookup is disabled;
it can only be enabled deliberately with
`FOCALD_SECRETS_ALLOW_LEGACY_HANDOFF=1` for migration testing.

## Store AI credentials

With the broker running as the active Secret Service provider:

```sh
printf '%s' "$OPENAI_API_KEY" |
  secret-tool store --label='Focaldesk OpenAI API key' \
  focald:key ai/openai-api-key

printf '%s' "$ANTHROPIC_API_KEY" |
  secret-tool store --label='Focaldesk Anthropic API key' \
  focald:key ai/anthropic-api-key
```

For an OpenAI-compatible vLLM endpoint, store its optional key as
`ai/vllm-api-key`. Model names and endpoint URLs are non-secret configuration
and remain environment variables.

The packaged ACL grants `focaldesk-server.service` read-only access to
`ai/*`. Restart it after adding or replacing a key:

```sh
systemctl --user restart focaldesk-server.service
```

Environment variables are still accepted when the broker or a particular key
is unavailable, which keeps existing development setups working during
migration.

## ACL guidance

Prefer `unit:` identities. Executable-path grants for interpreters such as
Python or Node grant every script run by that interpreter and are unsuitable
for credentials. Keep read and write access separate:

```toml
[grants."unit:example.service"]
allow = ["example/*"]
allow_write = ["example/state/*"]
```

ACL edits are reloaded on the next native request. The standard Secret Service
surface follows its conventional same-user trust model and does not apply the
native per-unit ACL. Native `focald:key` records are not exposed on that
surface.

The native ACL is defense-in-depth for correctly configured services, not a
hard boundary against arbitrary malicious code already running as the same
Unix user. The bootstrap key no longer crosses a user-owned path, but an
unlocked broker still intentionally serves public Secret Service items to
same-UID clients. Use separate Unix identities or mandatory access control
when mutually hostile workloads require credential isolation.
