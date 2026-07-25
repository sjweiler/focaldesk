# Credential broker

Focaldesk includes `focald-secrets`, an encrypted per-user credential store.
It exposes two views of the same data:

- `%t/focaldesk/secrets.sock` is a length-prefixed JSON protocol for Focaldesk
  services. The broker identifies the calling systemd user unit and applies
  `$XDG_CONFIG_HOME/focaldesk/secrets-acl.toml`; missing grants are denied.
- `org.freedesktop.secrets` is the standard session-bus Secret Service API for
  applications using libsecret, `secret-tool`, or compatible libraries.

The encrypted database is
`$XDG_DATA_HOME/focaldesk/secrets.db`. Its random master key exists in the
runtime directory only while the login session is unlocked. A password-wrapped
copy is stored as `$XDG_DATA_HOME/focaldesk/secrets.key.enc`.

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

The policy uses `pam_focald_secrets.so` to provision the runtime key before the
desktop starts. Focaldesk then owns `org.freedesktop.secrets`, so Chrome and
other Secret Service clients use the Focaldesk store without a second keyring
prompt.

The first successful login creates a random store key, wraps it under a key
derived from the login password, and writes the session copy below
`XDG_RUNTIME_DIR`. All failure paths are optional and do not deny login.

Distribution PAM stacks differ; the packaged policy targets Fedora's
`password-auth` and `postlogin` stacks. On other distributions, add
`pam_focald_secrets.so` to the equivalent human login policy rather than
installing the Fedora file. If the password-change stack is not integrated,
run `focald-secrets-keytool rewrap` when changing the login password.

## Memory locking

`focald-secrets` pins its address space with
`mlockall(MCL_CURRENT|MCL_FUTURE)` after establishing the D-Bus executor. The
broker unit requests a 128 MiB `LimitMEMLOCK`, but a user service cannot raise
that limit above the hard limit inherited by its `user@.service` manager.
Fedora commonly starts the user manager with an 8 MiB ceiling, which is too
small for the broker's approximately 75 MiB virtual address space.

The Fedora install recipe places
`90-focaldesk-memlock.conf` under
`/usr/lib/systemd/system/user@.service.d/`, raising the manager ceiling to
128 MiB. The new ceiling takes effect for user managers created after the
change, so log out and back in after installing or upgrading.

Confirm that the broker started without an `mlockall failed` warning:

```sh
systemctl --user status focald-secrets.service
journalctl --user -b -u focald-secrets.service
```

On a running system, `/proc/$PID/status` should report a nonzero `VmLck` for
the broker process. Do not weaken this to `--password-store=basic` for Chrome;
that bypasses protected Secret Service storage rather than fixing the session
limit.

For development sessions without focaldmd/PAM:

```sh
focald-secrets-keytool init
systemctl --user start focald-secrets.service
```

On later sessions, use `focald-secrets-keytool unlock` before starting the
service.

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
native per-unit ACL.
