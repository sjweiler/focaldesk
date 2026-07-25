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

The install recipe also installs a first-session migration unit. It runs before
the broker, asks the session Secret Service to enumerate GNOME Keyring, and
copies readable items without deleting or modifying the originals. Locked or
failed items are reported and the migration retries on a later session.

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
the database concurrently. The migration unit handles ordering automatically.

## Unlock at login with focaldmd

FocalDesk ships a complete Fedora human-login policy at
`packaging/pam/focaldmd-fedora`. Install it together with the native secrets
PAM module:

```sh
just install-focaldm-pam-fedora
```

The policy also includes `pam_gnome_keyring.so` in its auth, password, and
session stacks. This passes the verified login password to GNOME Keyring and
starts it unlocked, so Chrome and other Secret Service clients do not block on
an additional keyring prompt after a focaldmd login.

The first successful login creates a random store key, wraps it under a key
derived from the login password, and writes the session copy below
`XDG_RUNTIME_DIR`. All failure paths are optional and do not deny login.

Distribution PAM stacks differ; the packaged policy targets Fedora's
`password-auth` and `postlogin` stacks. On other distributions, add
`pam_gnome_keyring.so` and `pam_focald_secrets.so` to the equivalent human
login policy rather than installing the Fedora file. If the password-change
stack is not integrated, run `focald-secrets-keytool rewrap` when changing the
login password.

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
