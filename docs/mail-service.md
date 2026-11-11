# Background mail service

FocalMail's mail connectors run in a separate per-user process. FocalMail's
window and FocalDesk's MCP tools call the same service. The window may close
while synchronization continues. Gmail, Microsoft Graph, and IMAP/SMTP are
implemented. POP3 and generic IMAP OAuth remain configuration-only.

## Run and install

```sh
cargo run -p focalmail -- --mail-service   # foreground daemon; no window needed
cargo run -p focalmail                    # opens client; starts daemon if needed
just install-focalmail-service            # builds/installs client and user unit
systemctl --user status focalmaild.service
journalctl --user -u focalmaild.service
```

The installed user unit starts with the user's systemd default target, restarts
after failures, and uses the desktop Secret Service. Browser authorization needs
a graphical session. This does not import Evolution accounts or credentials.
Add accounts through FocalMail's Settings → Accounts first. See the
[FocalMail setup guide](../apps/focalmail/README.md) for each provider.

The daemon and client currently share one executable, so they use the existing
provider implementations without a second credential store or cache migration.
Running the daemon does not initialize Tauri or its webview or create a window. For development,
stop a foreground daemon with Ctrl+C before rebuilding. If the window launched a
standalone daemon, locate its `focalmail --mail-service` process and terminate it
before installing or restarting an updated daemon. Only one daemon can own the
socket. After installation, use the systemd service and restart it after updates.

## Ownership and recovery

- The service owns OAuth sign-in, token refresh, provider calls, mail mutations,
  attachment retrieval, sending, and scheduled sync. Refresh credentials remain
  in Secret Service under provider-specific FocalMail keyring entries. IMAP/SMTP
  passwords also stay in Secret Service; account profiles in SQLite contain no credentials.
- SQLite remains at `$XDG_DATA_HOME/focalmail/mail.sqlite3`. Existing messages,
  sync cursors, resumable scans, drafts and attachments are retained. The client
  reads the cache and owns its local composer drafts. It refreshes its view every
  two seconds, including changes made while the window was closed.
- Account states describe the last result: `connected`, `reconnect_required`,
  `offline`, or `sync_error`. Revoked/expired refresh authorization is distinct
  from temporary errors. Cancellation retains the prior state and cached mail.
- Reconnect uses the saved client configuration. No credential deletion is needed.
  The signed-in address must match the selected account. Missing keyring entries
  require signing in again using Reconnect or the desktop client JSON.
- Background sync runs shortly after startup and every five minutes after a successful scan.
  Failed reads retry after 15, 30, 60, 120, 240, then at most every 300 seconds.
  login1 suspend/resume and NetworkManager connectivity recovery schedule a fresh
  sync after two seconds. Suspend or network loss cancels only background reads;
  the persisted manual pause remains in effect. Watchers reconnect when system
  services become available; periodic retries work without those services.
  Wake requests cannot be overwritten by completion of an older interrupted scan.
  Accounts requiring sign-in are skipped by automatic sync until reconnected,
  while other accounts continue. Missing credentials require sign-in; locked
  keyrings and network failures remain retryable. Restart clears stale syncing
  status while keeping checkpoints, drafts, cached messages, and the pause setting.
  Interactive operations cancel a background scan and wait up to 35
  seconds for it to yield. Partial Gmail scans resume from their checkpoints.
  Another foreground operation returns a busy error rather than executing twice.
- Sends are never retried automatically. A confirmed send returns before its
  follow-up sync, which runs in the background. If the service or connection dies
  during sending, inspect Sent before retrying.

## Additional providers and notifications

Microsoft sign-in defaults to system-browser authorization-code flow with PKCE,
a random state, and a localhost public-client callback registered on the app.
The listener binds before the browser opens, validates method/path/state and
rejects ambiguous callbacks, and retains verifier/state only in memory. This
uses the same browser approach as FocalDesk Calendar. An explicit DeviceCode
profile option remains available for tenants that permit it; it is not an
automatic fallback when browser sign-in is denied. Existing profiles default to
Browser. Error 530035 identifies Security Defaults, which block device-code flow;
FocalMail keeps those policies in place and requests ordinary browser sign-in/MFA.

The service sends a structured authorization prompt through progress; the desktop
retains the browser URL or short user code and expiry only in memory until the
operation finishes. The UI subscribes before requesting authorization, shows a
persistent modal, and polls desktop state if an event is missed. Browser/code,
copy, and cancellation actions remain available while authorization is pending.
Microsoft error codes and denial reasons are preserved without including token
or private device-code response fields. Successful authorization returns before
mailbox download and schedules background sync. Rotating refresh authorization
stays in the keyring.
Microsoft sync uses one Graph delta cursor per folder. Folder discovery preserves custom paths and empty folders. Pages commit in batches of at most 32 messages before the next/delta link advances, so new mail is visible during import and interrupted pages replay safely. Incremental sync requests only changes; unchanged message versions retain their cached content. Folder ownership prevents a move's source removal from deleting a destination copy. Expired delta state restarts only that folder and removes stale rows after its replacement scan completes. Checkpoints and folder membership survive service restarts and are cleared on account removal. Expanding history resets folder checkpoints while retaining cached messages. See [Microsoft's message delta API](https://learn.microsoft.com/en-us/graph/delta-query-messages).

IMAP/SMTP supports a shared password/app password with verified TLS or required STARTTLS. IMAP scans all selectable folders, reuses cached message bodies, and replaces its account snapshot only after the entire scan succeeds; IMAP scans are not yet checkpointed.

SMTP uses a configurable Sent-folder copy. A failure after SMTP acceptance never becomes a retryable send failure. Microsoft and SMTP reuse the client MIME composer, including reply headers and attachments. Full raw-message retrieval is limited to 40 MiB. IMAP credentials are submitted through the private socket by the client; do not put passwords in shell command-line arguments.

Desktop notifications run after successful account syncs. First imports are silent; subsequent syncs count new unread Inbox IDs and emit one content-free notification per account. The preference is persisted in SQLite and editable in Settings. Notification failures do not affect synchronization. No message content or account identity appears in a notification, and click-to-open is not implemented.

Per-account synchronization defaults to one calendar year. `accounts[].sync_history`
is one of `one_month`, `three_months`, `six_months`, `one_year`, or `all`. The
`set_sync_history` operation uses the same single-writer lock as synchronization:
it interrupts background work, retains completed cursors when narrowing the range, and
schedules a new scan. Reducing history prunes local cache only; local drafts and
server mail are unchanged. Expanding history keeps cached and partially imported
messages until a successful replacement scan. Expanding the range or changing an unfinished scan resets its scan cursor. Gmail stores its initial cutoff
with the resumable scan so every page and retry uses the same date query.
Large partial-cache upgrades, Gmail import completion, and local history pruning
commit batches of at most 32 messages. Gmail completion records its clearing and
promotion phases durably; cancellation or restart resumes that phase without
losing already-promoted messages or advancing history early. Incremental Gmail
history updates write only changed IDs and retain the old cursor until every
batch commits, so an empty refresh does not reload or reindex the mailbox. Previously
cached message IDs use `format=minimal` to refresh labels while retaining all
cached content; only missing IDs use `format=full`. Completed history cursors
survive the one-year-default upgrade and narrower history selections. The UI
labels a necessary reconciliation as Checking mailbox and avoids using an
unfiltered profile message total for a date-scoped scan. See [Gmail's cache and
synchronization guidance](https://developers.google.com/workspace/gmail/api/guides/sync).

Microsoft scans lightweight delta metadata without a server date filter (filtered delta queries have a 5,000-message cap), then fetches message content only within the local history cutoff. IMAP uses `UID SEARCH SINCE` and initially
fetches headers, deferring MIME content and attachments until the message opens.
Search remains local: fetch a wider history to search earlier messages.

Sync phase/counters are persisted in `mail_sync_progress` and included under
`accounts[].progress` in status, alongside `cached_count` and `sync_paused`.
Pausing persists a local preference and cancels only `background-sync`; an
explicit `sync` request clears the preference. The Tauri window polls these
small status records independently of mailbox pages and message rendering.

HTML is sanitized into an allowlisted layout before caching. The Tauri reader
uses an opaque-origin sandbox and a hash-pinned bridge; remote resources cannot
be fetched from the frame. Explicit remote-image loading is handled by a
separate bounded native loader with public-address validation and DNS pinning,
without mail credentials or cookies. See the FocalMail guide for image limits.

## Local API

Socket: `$XDG_RUNTIME_DIR/focalmail/service.sock`. The directory is mode `0700`
and the socket `0600`. A process lock prevents competing daemons; stale sockets
are replaced only after acquiring the lock. This is a same-user trust boundary.
It does not isolate mutually untrusted applications running under the same UID.

One UTF-8 JSON envelope per connection, terminated by a newline:

```json
{"id":"unique-operation-id","request":{"method":"status"}}
```

Responses are newline-delimited `progress`, followed by one `done` or `error`:

```json
{"event":"progress","detail":"Checking Gmail access…"}
{"event":"done","value":{"version":1,"accounts":[],"active_operation":null}}
```

Errors contain `event: "error"` and `detail`. Frames are limited to 96 MiB
(including serialized attachment byte arrays). At most sixteen connections are
handled concurrently; incoming request frames time out after ten seconds.
Provider operations run serially; status, cached search/read, cancellation, and
change subscriptions remain available during sync.

| Method | Fields | Result |
| --- | --- | --- |
| `status` | none | protocol version, instance ID, revision, accounts with capabilities, active operation ID |
| `pause_sync` | none | persistently pause background sync; cancel only its owned operation |
| `search` | `query`, nullable `account`, `limit` | up to 100 indexed cached summaries, 160-character snippets |
| `read` | `account`, `id` | cached plain text, up to 128,000 characters, truncation flag |
| `changes` | `after`, `wait_seconds` | instance ID and revision; waits up to 25 seconds for a change |
| `sync` | none | synchronize all connected provider accounts |
| `set_sync_history` | `account`, `history` | set per-account cache range, invalidate scan checkpoints, and schedule background sync; server mail is unchanged |
| `connect` | `path` | import Google Desktop OAuth JSON and authorize Gmail |
| `connect_profile` | `profile`, `password` | authorize Microsoft or verify IMAP/SMTP and save in keyring; password is empty for Microsoft |
| `reconnect` | `email` | authorize the selected account using saved OAuth configuration |
| `modify` | `account`, `id`, `action` | read/unread, star/unstar, archive, trash |
| `bulk_modify` | `targets` (1–100 account/id pairs), `action` | confirmed, failed, unattempted targets; warnings and cancellation state |
| `remove_account` | `account` | clear local account cache/sign-in; retain drafts and server mail |
| `send` | `draft`, `files` | send through the draft's account |
| `content` | `account`, `id` | retrieve/cache full message content |
| `download` | `account`, `id`, `attachment` | save a matching cached attachment to Downloads |
| `forward` | `source`, `selected`, `remaining` | fetch selected source attachments within the byte budget |
| `cancel` | `operation` | cancel only the matching active operation |

Action values use the existing enum encoding: `{"Read":true}`, `{"Star":false}`,
`"Archive"`, or `"Trash"`. Draft and attachment schemas are the serde types in
`apps/focalmail/src/drafts.rs` and `attachments.rs`. Message/attachment lookups
use the service cache, rather than trusting caller-supplied message content.

Revisions change when an operation starts or completes. They signal that clients
should refresh cached data or status; they are not durable message deltas.
Compare `instance` to detect a daemon restart and reset the saved revision.
For initial full-sync progress, the client also polls the SQLite cache.

For scripts, the client executable wraps the socket protocol and prints the
result as JSON. It starts the daemon if needed:

```sh
focalmail --mail-request '{"method":"status"}'
focalmail --mail-request '{"method":"search","query":"invoice","account":null,"limit":20}'
focalmail --mail-request '{"method":"reconnect","email":"you@gmail.com"}'
```

## AI integration

The FocalDesk MCP adapter exposes `get_mail_status`, `search_mail`,
`read_mail_message`, and `send_mail` through this socket. It does not read the
mail database or retrieve provider credentials. Search/read responses are bounded
and mail content is described as untrusted data.

Read tools follow the existing session policy. `send_mail` requires its explicit
session capability (`FOCALDESK_MCP_CAPABILITIES=send_mail`) and confirmation of
that exact sender, recipients, subject and body. Native callers may use the
existing `execute_confirmed_tool` path after obtaining user approval. AI sending
currently supports new plain-text messages; the FocalMail client also supports
replies and attachments. Direct same-user socket clients are trusted and must
provide their own approval UX.

## Verification

```sh
cargo fmt -p focalmail -p focaldesk-mcp --check
cargo test -p focalmail -p focaldesk-mcp --offline
cargo clippy -p focalmail -p focaldesk-mcp --offline --all-targets -- -D warnings
```

Socket and mock HTTP tests need permission to bind local sockets. The daemon
integration test uses temporary runtime/data directories, removes desktop display
environment variables, and never accesses real accounts. Live Google sign-in,
revocation/reconnect, mailbox changes and delivery require manual account checks.
