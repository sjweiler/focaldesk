# FocalMail

Tauri 2 desktop mail client with a Svelte 5 / TypeScript frontend, Rust backend,
and shared per-user background mail service.

## Run

From the FocalDesk repository root:

```sh
npm ci --prefix apps/focalmail/frontend
cargo run -p focalmail
```

The desktop build requires Node.js 22.12+ and npm. Cargo builds the Svelte
frontend and embeds `dist/`; the installed application needs neither Node nor a
development server. Frontend source lives in `frontend/src`, with FocalDesk-style
semantic CSS tokens in `style.css`. Tauri configuration lives in `tauri.conf.json`.

The three-pane shell has persistent, resizable account and message panes. Drag
the separators or focus them and use Left/Right (Home restores the default).
Mail is ordered by its timestamp, newest first, and grouped by local calendar
day (Today, Yesterday, and dated headings). List times, date tooltips, and reader
metadata use the same local time zone; the sender’s original Date header stays
available in the reader’s date tooltip.

Messages open in up to twelve reader tabs, which remain open when switching
folders. Ctrl/Cmd+K focuses search; Ctrl/Cmd+N starts a draft. Account setup,
replies, attachments, and local drafts use the existing Rust service bridge.
Remote content stays blocked and links require explicit browser confirmation.
AI trust badges, phishing classification, and FocalVector search are not yet
connected; the interface shows only supported mail actions and privacy policy.

On Fedora, install desktop prerequisites:

```sh
sudo dnf install gtk3-devel webkit2gtk4.1-devel libsoup3-devel dbus-devel openssl-devel librsvg2-devel
```

For a service-only build without GTK/WebKit:

```sh
cargo run -p focalmail --no-default-features -- --mail-service
```

FocalMail is a member of the FocalDesk Cargo workspace and uses the root lockfile
and target directory. From `apps/focalmail`, plain `cargo run` also works.

Opening FocalMail starts a separate `focalmail --mail-service` process when the
service is unavailable. Closing the window leaves that process syncing mail.
For automatic startup and restart, install the systemd user service:

```sh
just install-focalmail-service
```

See [the mail service API and architecture](../../docs/mail-service.md) for
headless operation, AI tools, and development lifecycle notes.

Linux needs a running desktop session and a Secret Service implementation (for example KDE Wallet with its Secret Service interface enabled, or GNOME Keyring). Credentials are never written to SQLite or silently stored in plaintext when the keyring is unavailable.

Background sync recovers after suspend and network restoration, with bounded
retries for temporary failures. Accounts with revoked or expired sign-in remain
visible with **Reconnect** guidance while other accounts continue syncing.
A manual pause stays paused across wake, network changes, and service restarts.
Interactive sends are never replayed automatically.

Accounts sync independently, so a long Gmail import does not hold up Microsoft
365 or IMAP accounts. Pause and cancel still stop all background account workers.
Expand an account in the sidebar to browse its own Inbox, Sent, drafts and other
folders. Microsoft folder discovery includes empty folders and nested folders;
custom folder paths are preserved instead of being grouped into Archive.

## Sync history

Each account defaults to the last **1 year**. In **Settings → Connected accounts**,
choose **1 month**, **3 months**, **6 months**, **1 year**, or **All mail** under
**Keep mail available offline**. The range uses calendar months and applies to
Gmail, Microsoft 365, and IMAP. **Load older mail** expands that account to the
next range and schedules a sync; the message list has a shortcut to these settings.
Search covers cached messages; expand history to search older mail.

Reducing history clears only local cached messages outside the range. It never
sends delete commands to the mail server or removes local drafts. Expanding history
preserves downloaded messages while the new scan runs. Gmail import checkpoints
keep their original cutoff across retries and restarts. Existing accounts get the
one-year default on upgrade, and their cache remains available until a successful
refresh. Restart the mail service along with the desktop after upgrading.

Gmail downloads a message body only when that message ID is missing from the
cache. Reconciliation scans and history updates use `format=minimal` for cached
IDs to refresh read/star/folder labels while retaining bodies, HTML, reply headers,
and attachments. Normal refreshes use Gmail history; an unchanged mailbox makes
no message-content requests. A narrower range preserves a completed history
cursor. A required rescan is labeled **Checking mailbox**, and scoped scans do
not display the entire account's message count as their download target.

Attachments are deferred during background synchronization. IMAP initially caches
headers and flags; opening a message fetches its body and attachments, which then
remain available offline. Gmail and Microsoft cache message content during sync
and retrieve additional attachment content when needed.

## Formatted messages and signatures

The composer supports bold, italic, lists, HTTP/HTTPS links, and undo/redo.
Pasted HTML is reduced to basic formatting; scripts, remote images, embedded
content, and custom styles are removed. Sending includes both plain text and
HTML using the same MIME path for Gmail, Microsoft 365, and SMTP, including
attachments and reply headers. Existing plain-text drafts remain readable;
editing and saving a draft preserves its HTML locally.

Under **Settings → Connected accounts → Signature**, edit each account's
formatted signature and choose whether to insert it into new messages and/or
replies and forwards. Signatures start disabled. Inserted signatures are editable
in the composer. Switching the From account replaces the automatic signature
while preserving the message. Reopening a saved draft does not insert another
signature. Removing an account clears its signature settings while retaining
its local drafts.

## Connect Gmail

1. In your Google Cloud project, enable the **Gmail API**.
2. Configure Google OAuth consent. If the app is in Testing, add your Gmail address as a test user.
3. Create an OAuth client with application type **Desktop app** and download its JSON file. A calendar-only access token does not authorize Gmail.
4. Run FocalMail and open **Settings → Add or edit an account**. Select **Google / Gmail**, enter the absolute path to the JSON file, then click **Connect account**.
5. Complete authorization in the system browser. The mail service receives the response on a temporary loopback port, saves refresh credentials to the desktop keyring, then synchronizes the mailbox.

The Google OAuth file path is entered in Settings. For headless sign-in:

```sh
cargo run -p focalmail -- --mail-request '{"method":"connect","path":"/absolute/path/client_secret.json"}'
```

The path is used only to import configuration; do not commit your downloaded JSON. Google may require OAuth verification for public distribution of Gmail scopes. External apps in Testing may receive refresh tokens with a limited lifetime; reconnect if Google revokes or expires authorization.

## Account settings

Account setup lives under **Settings** rather than a provider-specific toolbar button. Connected Gmail accounts show their last connection state and can be reauthorized
with **Reconnect**. This opens browser sign-in using the account’s existing
keyring configuration; importing the JSON again is unnecessary. Signing in as a
different Google account is rejected before replacing the selected credentials.

- **Google / Gmail:** Gmail REST API with OAuth 2.0; mailbox synchronization, message actions, and MIME sending.
- **Microsoft 365 / Outlook:** Microsoft Graph REST API with device-code OAuth, mailbox sync, read/star/archive/Trash actions, MIME sending, replies and attachments. Refresh authorization stays in Secret Service.
- **Other mail server:** IMAP incoming and SMTP outgoing with password/app-password authentication. Both connections require verified SSL/TLS or STARTTLS. POP3 and generic IMAP OAuth remain configuration-only.

The Tauri account form exposes the supported Gmail, Graph, and IMAP/SMTP backends. Existing configuration-only profiles remain in SQLite. OAuth is an authentication method, separate from the protocol used to access mail.

## Connect Outlook / Microsoft 365

1. Register an application in Microsoft Entra with the supported account types you need. In **Authentication → Add a platform → Mobile and desktop applications**, register **http://localhost** as a redirect URI. FocalMail uses an ephemeral loopback port, as FocalDesk Calendar does. No application secret is needed.
2. Add delegated Microsoft Graph permissions **User.Read**, **Mail.ReadWrite**, **Mail.Send**, and **offline_access**. Organization policy may require administrator consent.
3. Open **Settings → Add or edit an account → Microsoft 365 / Outlook**. Enter the mailbox address, application client ID, and tenant. Keep **Browser sign-in (recommended)** selected.
4. Click **Connect account**, complete sign-in and MFA in your browser, then return to FocalMail. Browser authorization uses PKCE and a validated random callback state. The signed-in mailbox address must match the configured address.

Use **Reconnect** to authorize again. Existing profiles default to browser sign-in. Authorization finishes before the mailbox download; background sync honors the manual pause setting. The sign-in dialog stays visible with Cancel and Open Microsoft sign-in, and recovers its current prompt if a progress event is missed. Microsoft error codes and reasons are retained without returning token/device-code response secrets.

**Device code** remains an explicit alternative for tenants that permit it. It shows a persistent code with Copy, Open Microsoft sign-in, expiry, and Cancel. Error **530035** means **BlockedBySecurityDefaults**; Microsoft's Security Defaults block device-code flow. Use browser sign-in and satisfy the tenant's MFA requirements. FocalMail does not change or disable tenant policies. See [Microsoft Security Defaults](https://learn.microsoft.com/en-us/entra/fundamentals/security-defaults).

Sync uses Microsoft Graph's per-folder delta API. Each downloaded page becomes visible immediately and saves its continuation link after its cache writes commit. Interrupted imports resume at that page; subsequent syncs reuse each folder's delta link to fetch changes only. Moves and deletions use immutable message IDs and folder ownership so source-folder removals cannot delete an already-synced destination copy. Expired cursors rebuild only the affected folder and retain existing mail until reconciliation completes. The first sync after upgrading establishes these cursors once. Metadata is scanned without a date filter to avoid Graph's 5,000-message cap; message content is fetched only inside the selected history range. Cached mail survives failed or cancelled scans. Full MIME/attachments load on demand and are cached offline. Raw-message retrieval is limited to 40 MiB; individual attachment download/forward limits remain 20 MiB.

## Connect IMAP / SMTP

Choose **Other mail server** (IMAP/SMTP with password authentication). Enter incoming and SMTP hosts, ports, TLS modes, and the login username (defaults to the mailbox address). Enter the password and click **Connect account**. Both connections are checked before the password is saved in Secret Service. No plaintext fallback or unverified certificates are accepted.

The same login is used for both servers. Use **Edit** on a connected account to change settings/password; **Reconnect** tests the saved incoming credentials and synchronizes. SMTP accounts with separate outgoing credentials are not yet supported.

IMAP scans selectable folders, reuses downloaded bodies by mailbox/UIDVALIDITY/UID, refreshes flags, and commits a complete snapshot. New messages use BODY.PEEK so reading does not mark them read. Large raw messages retain header-only placeholders. Archive/Trash require an existing recognized destination folder and server support for UID MOVE; no global EXPUNGE is used. Unknown folders appear under Archive in this first version.

SMTP sends MIME with To/Cc, reply headers, and attachments. **Save a copy in the server Sent folder** is enabled by default; disable it when your SMTP server already saves copies. A failed Sent copy is reported as a successful send with a warning, and the delivered draft is cleared. Delivery is never automatically retried.

## Sync visibility and recovery

The sidebar shows cached message counts, the current sync phase, and downloaded
message counts. Gmail shows an estimated total when available. Microsoft saves
and displays each downloaded page during import; IMAP saves its completed snapshot. Gmail import
counts resume from staged messages after interruption. During a long initial
import, a bounded recent-Inbox refresh runs about once a minute, downloading
missing recent messages without resetting the historical scan’s page/history
cursor. Resuming a large import checks the recent Inbox before older pages. Progress survives restarts
and polls independently every two seconds, without resetting the reader or tabs.

**Pause** cancels only the background scan and persists the pause across restarts;
it never cancels a send or sign-in. **Resume**, **Retry**, or Refresh explicitly
resumes sync. Network errors preserve downloaded mail and show retry guidance.
Account-specific details remain in Settings. Reading cached mail works during
sync and while offline. Outlook/IMAP scans still commit at completion rather
than checkpointing partial scans.

## Desktop notifications

**Settings → Notify about new unread mail** controls count-only notifications from the background service. Notifications require a running `org.freedesktop.Notifications` session service. They show neither subject, sender, body nor account address. Initial mailbox imports are silent; later successful syncs notify for newly cached unread Inbox messages. Existing unread mail and read/flag changes do not notify. A missing notification server does not fail sync. Notifications currently have no click-to-open action.

## Implemented

- Browser OAuth authorization code flow with PKCE, random state validation, loopback-only callback, cancellation, and a three minute sign-in timeout.
- Gmail `gmail.modify` scope for reading, label changes, Trash, and sending; access tokens stay in memory and refresh credentials (including desktop client configuration) live in Secret Service. **Reconnect accounts added by the read-only prototype** to grant the new permission.
- Multiple Gmail accounts, refreshed shortly after service startup, every five minutes after background sync completes, after sending, and manually with Refresh. Network and sign-in work runs in the mail service; cancellation preserves the previous cache until a successful sync commits.
- First sync follows all message-list pages, including older mail, Sent, Archive, Trash, Spam, and server drafts. Later refreshes follow paginated Gmail history and fetch changed messages only. An expired history cursor triggers a full sync. Reads are paced to at most 75 requests/minute, with cancellable exponential backoff for temporary rate limits and server errors. Daily/project quota exhaustion is reported separately. Large first-time mailboxes may take time and use substantial memory; downloaded messages appear during sync, with progress saved across cancellation, quota failures, and restarts.
- SQLite account/message/sync-state cache, searchable offline on restart. Incremental changes and the history cursor commit together after successful retrieval. Initial full sync stages each downloaded message for immediate display, resumes saved pages without redownloading completed messages, and only removes stale cached messages and advances history when the full scan finishes. Failed requests retain both previous cached mail and downloaded progress, while other accounts can still sync successfully. The old inbox-only cursor is invalidated once during migration.
- Nested MIME plain-text and HTML alternatives, sender/subject/date headers, unread/starred display. HTML is parsed into a sanitized document model; remote resources are never loaded. HTML-only messages also get searchable plain text extracted from the safe document.
- Mark read/unread, toggle star, archive, and move to Trash through Gmail. Opening a message does not automatically mark it read. Sent messages can appear in both Inbox and Sent when Gmail labels require it.
- Multiple local drafts with independent sender, To/Cc, reply context, text, and attachments; the active draft is restored on restart. Reply/reply-all quote the original message and preserve Gmail threading. Forward quotes the original and offers optional attachment selection. Send clears only the delivered draft. Sends are never automatically retried; after an uncertain network result, check Sent before retrying.

The cache uses SQLite WAL mode so window reads can overlap service writes.
Schema migrations run only when needed; ordinary cache opens do not request a
write transaction. After updating the installed binary, restart the mail service
with `systemctl --user restart focalmaild.service` to load database fixes. A
retry shows **Syncing mail…** instead of retaining the previous error; cancelling
that retry shows **Sync paused**.

Cache: `$XDG_DATA_HOME/focalmail/mail.sqlite3` (normally `~/.local/share/focalmail/mail.sqlite3`). The containing directory is private to the user. Mail and drafts are locally stored without application-level encryption; use disk encryption if required.

POP3, generic IMAP OAuth, server draft editing,
and FocalDesk Vulkan/theme integration
remain future work. Server drafts are listed separately from the local composer
draft. The desktop window uses the system WebKit webview on Linux. Mailboxes load in
pages of 100 summaries; search covers complete cached bodies, and opening a
message loads its full cached content. A local summary index is built once on
upgrade and stays current during background sync.

## Selection, search, and accounts

Message checkboxes select mail without opening it. **Select page** selects the
current page, with at most 100 messages selected across pages. Bulk controls mark
mail read/unread, star/unstar, archive, or move it to Trash after confirmation.
Results distinguish confirmed changes, failures, and unattempted messages; failed
changes remain selected and are never retried automatically. Cancelling preserves
already confirmed changes. IMAP moves retain readable cached content and block
further mutations until folder identities refresh.

Offline search uses SQLite FTS5 over sender, address, subject, and complete cached
plain text. Words are combined with AND and match word prefixes; punctuation is
literal rather than query syntax. Queries support up to 16 words and 512
characters. Existing mail is indexed in small background batches; **Preparing
search** reports remaining work and warns when results may be incomplete. New
and changed messages update the index in the same cache transaction.

In Settings, **Remove** asks before clearing the selected account's local sign-in,
settings, and cached mail. It does not delete server mail. Local drafts and their
attachments remain; choose a connected sender before sending them. A keyring
failure leaves the account and cache in place so removal can be retried.

## Replies, forwards, and drafts

- **New message**, **Reply**, **Reply all**, and **Forward** each create a separate draft
  after saving the current one. Click **Drafts** in the sidebar to open, switch,
  or delete saved drafts. Closing the composer retains the draft; **Discard draft** deletes just that draft and its copied attachments.
- Reply honors Reply-To. Reply-all includes original To/Cc recipients, deduplicates
  addresses, and excludes connected accounts, the delivery address, and Gmail
  dot/plus aliases of those accounts. Other aliases cannot be inferred; review
  To and Cc before sending. Bcc recipients are never copied.
- Replies quote the original plain text and include Message-ID/References and
  Gmail thread context when available. Changing the sender or subject drops the
  original thread context. Old cached messages fetch their missing metadata on
  demand before preparing a reply or forward.
- Forward includes the original sender/date/subject/To/Cc and text. Select
  original attachments and click **Include selected attachments** to copy them
  into this draft. Unselected attachments are not forwarded. The existing
  20 MiB attachment total applies across forwarded and manually attached files.
- The former single local draft, including sender and attachment bytes, migrates
  once without changing cached mail. Draft text and changed attachment contents
  save transactionally, and failed sends retain the draft. Drafts remain local;
  they do not synchronize with Gmail's server drafts.

## Formatted email

- New HTML messages retain sanitized tables, newsletter layouts, headings, links,
  and a conservative set of inline text, color, spacing, and border styles. The
  layout lives in a sandboxed iframe with an opaque origin. Sender scripts, forms,
  event handlers, embedded documents, SVG, external stylesheets, CSS URLs, and
  automatic navigation are removed. The only script allowed in the frame is the
  shipped, hash-pinned link/height bridge. The frame cannot access the app DOM or
  invoke mail actions. **Plain text / Formatted** switches views.
- Remote images and tracking pixels are never loaded automatically. **Load images**
  opens a confirmation explaining that the sender may learn the message was opened.
  The native loader accepts only image URLs from the selected cached document,
  validates and pins public DNS addresses, checks every redirect, uses no proxy,
  cookies, authentication, or referrer, and permits only public HTTPS on port 443.
  It returns raster data URLs; the frame itself cannot make network requests.
- Remote loading is limited to 20 PNG/JPEG/GIF/WebP images, 2 MiB per image and a
  10 MiB display budget. Private/local addresses, SVG, unsupported content, and
  oversized images remain blocked. Loaded remote images stay in memory for that
  message tab; closing it or restarting resets consent.
- CID images present in cached MIME are available offline, within the same raster
  type/size limits. Gmail can retrieve missing CID parts when **Load full message
  and inline images** is selected; unrelated attachments are not fetched.
- Links open a full destination preview in the app. **Open in browser** is a
  separate explicit action. Only credential-free HTTP/HTTPS destinations are
  accepted; sender content cannot navigate the frame or open windows on its own.
- Older cached messages keep their existing offline text/formatted view. Use
  **Load full message and inline images** once to retrieve and cache the new layout.
  HTML source is limited to 1 MiB, with nesting, node-count, and output limits.
  No additional mail OAuth scope is needed.

## Attachments

- Incoming messages list attachment names and sizes. Click **Save to Downloads**
  to download a file; duplicate names receive a numeric prefix. Files are never
  opened automatically. The per-file download limit is 20 MiB.
- Messages cached by older builds have a **Load full message and inline images** button. This
  fetches content for just that message without restarting full mailbox sync.
- In the composer, click **Attach file**, select a file in the system file chooser.
  **Remove** removes it from the draft. Outgoing attachments may total 20 MiB;
  message text may be up to 1 MiB. Files are sent as MIME attachments.
- Attachment contents are copied into SQLite with the local draft, so moving or
  deleting the original file does not change the draft. Discarding the draft or
  confirmed delivery clears those copies. Local draft attachments have the same
  storage/privacy properties as the mail cache.
- File reads and Gmail downloads run off the UI thread and can run while mailbox
  sync continues. No additional OAuth scope is needed.

## Manual account checks

1. Reconnect an existing Gmail account and approve mail modification/sending.
2. Refresh a mailbox with more than 500 messages; confirm older mail and folders.
3. Change labels in Gmail and refresh FocalMail; confirm read/star/archive/Trash changes in both directions.
4. Send a plain-text message to an address you control; check delivery and Sent.
5. Create two drafts with different recipients and attachments, switch between them in Drafts, and restart. Send or delete one and verify the other remains intact. Send an attachment to yourself and compare downloaded bytes.
6. Reply-all to a message with To/Cc and a Reply-To address; verify self exclusion and Gmail threading. Forward another message with only one selected attachment; verify only that file is delivered.
7. Open an HTML-only newsletter and a multipart plain/HTML message. Check formatting, Plain text, image placeholders, link destination preview, and offline display after restart.
8. Disconnect the network or lock the keyring and refresh; cached mail and the draft should remain available. Cancel a long sync and retry.

## Verification

```sh
cargo fmt -p focalmail --check
cargo test -p focalmail --no-default-features --offline
cargo clippy -p focalmail --no-default-features --offline --all-targets -- -D warnings
npm run check --prefix apps/focalmail/frontend
npm test --prefix apps/focalmail/frontend
```

Tests also cover Microsoft pagination/origin validation and cache preservation, IMAP UID identities, flag refresh and STARTTLS refusal, shared MIME, notification filtering, the headless daemon, private socket permissions, single-instance locking, account status persistence, cancellation ownership, background sync preemption, and AI mail transport/confirmation. Tests cover callback validation, MIME body selection and composition, header injection rejection, mock HTTP pagination/history/deletion/fallback, label-action requests, send failures, cache replacement isolation, migration, and draft persistence. Mock HTTP tests bind temporary loopback ports. Real account sign-in requires your Google client and browser consent and is a manual integration check.

Protocol references: [Google desktop OAuth](https://developers.google.com/identity/protocols/oauth2/native-app), [Gmail client synchronization](https://developers.google.com/workspace/gmail/api/guides/sync).

Provider references: [Microsoft device-code authorization](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-device-code), [Microsoft Graph MIME sending](https://learn.microsoft.com/en-us/graph/api/user-sendmail), [IMAP session API](https://docs.rs/imap/latest/imap/struct.Session.html), [SMTP TLS modes](https://docs.rs/lettre/latest/lettre/transport/smtp/client/enum.Tls.html).
