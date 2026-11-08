# FocalMail

Native Rust/egui mail client with a shared per-user background mail service.

## Run

From the FocalDesk repository root:

```sh
cargo run -p focalmail
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

## Connect Gmail

1. In your Google Cloud project, enable the **Gmail API**.
2. Configure Google OAuth consent. If the app is in Testing, add your Gmail address as a test user.
3. Create an OAuth client with application type **Desktop app** and download its JSON file. A calendar-only access token does not authorize Gmail.
4. Run FocalMail and open **Settings → Accounts → Add account**. Select **Google / Gmail**, expand **Advanced · application OAuth configuration**, enter the absolute path to the JSON file, then click **Sign in with Google**.
5. Complete authorization in the system browser. The mail service receives the response on a temporary loopback port, saves refresh credentials to the desktop keyring, then synchronizes the mailbox.

You can prefill the file path:

```sh
FOCALMAIL_GOOGLE_CLIENT_JSON=/absolute/path/client_secret.json cargo run -p focalmail
```

The path is used only to import configuration; do not commit your downloaded JSON. Google may require OAuth verification for public distribution of Gmail scopes. External apps in Testing may receive refresh tokens with a limited lifetime; reconnect if Google revokes or expires authorization.

## Account settings

Account setup lives under **Settings → Accounts** rather than a provider-specific toolbar button. Connected Gmail accounts show their last connection state and can be reauthorized
with **Reconnect**. This opens browser sign-in using the account’s existing
keyring configuration; importing the JSON again is unnecessary. Signing in as a
different Google account is rejected before replacing the selected credentials.

- **Google / Gmail:** Gmail REST API with OAuth 2.0; mailbox synchronization, message actions, and plain-text sending.
- **Microsoft 365 / Outlook:** Microsoft Graph REST API with device-code OAuth, mailbox sync, read/star/archive/Trash actions, MIME sending, replies and attachments. Refresh authorization stays in Secret Service.
- **Other mail server:** IMAP incoming and SMTP outgoing with password/app-password authentication. Both connections require verified SSL/TLS or STARTTLS. POP3 and generic IMAP OAuth remain configuration-only.

Saved configurations are kept in SQLite separately from connected accounts and are explicitly labeled configuration-only. No passwords are collected for unavailable backends. OAuth is an authentication method, separate from the protocol used to access mail.

## Connect Outlook / Microsoft 365

1. Register an application in Microsoft Entra, selecting the supported account types you need (personal Outlook, organization accounts, or both). Enable **Allow public client flows**. No application secret is needed.
2. Add delegated Microsoft Graph permissions **User.Read**, **Mail.ReadWrite**, **Mail.Send**, and **offline_access**. Organization policy may require administrator consent.
3. Open **Settings → Accounts → Add account → Microsoft 365 / Outlook**. Enter the mailbox address and, under Advanced, the application client ID and tenant (`common`, `consumers`, `organizations`, or your directory).
4. Click **Sign in with Microsoft**. The status area displays a device code; enter it at the Microsoft sign-in page opened in your browser. The signed-in mailbox address must match the configured address.

Use **Reconnect** to authorize again. Sync reads all `/me/messages` pages, maps the standard folders, and commits a complete snapshot only on success. It currently repeats this scan on each refresh; Graph delta sync and immediate throttling retries are future improvements. Cached mail survives a failed or cancelled scan. Full MIME/attachments load on demand and are cached for offline use. The raw-message retrieval limit is 40 MiB; individual attachment download/forward limits remain 20 MiB.

## Connect IMAP / SMTP

Choose **Other mail server**, **IMAP**, and **Password / app password**. Enter incoming and SMTP hosts, ports, TLS modes, and the login username (defaults to the mailbox address). Enter the password and click **Connect IMAP and SMTP**. Both connections are checked before the password is saved in Secret Service. No plaintext fallback or unverified certificates are accepted.

The same login is used for both servers. Use **Edit** on a connected account to change settings/password; **Reconnect** tests the saved incoming credentials and synchronizes. SMTP accounts with separate outgoing credentials are not yet supported.

IMAP scans selectable folders, reuses downloaded bodies by mailbox/UIDVALIDITY/UID, refreshes flags, and commits a complete snapshot. New messages use BODY.PEEK so reading does not mark them read. Large raw messages retain header-only placeholders. Archive/Trash require an existing recognized destination folder and server support for UID MOVE; no global EXPUNGE is used. Unknown folders appear under Archive in this first version.

SMTP sends MIME with To/Cc, reply headers, and attachments. **Save a copy in the server Sent folder** is enabled by default; disable it when your SMTP server already saves copies. A failed Sent copy is reported as a successful send with a warning, and the delivered draft is cleared. Delivery is never automatically retried.

## Desktop notifications

**Settings → Notifications → Notify about new unread mail** controls count-only notifications from the background service. Notifications require a running `org.freedesktop.Notifications` session service. They show neither subject, sender, body nor account address. Initial mailbox imports are silent; later successful syncs notify for newly cached unread Inbox messages. Existing unread mail and read/flag changes do not notify. A missing notification server does not fail sync. Notifications currently have no click-to-open action.

## Implemented

- Browser OAuth authorization code flow with PKCE, random state validation, loopback-only callback, cancellation, and a three minute sign-in timeout.
- Gmail `gmail.modify` scope for reading, label changes, Trash, and sending; access tokens stay in memory and refresh credentials (including desktop client configuration) live in Secret Service. **Reconnect accounts added by the read-only prototype** to grant the new permission.
- Multiple Gmail accounts, refreshed shortly after service startup, every five minutes after background sync completes, after sending, and manually with Refresh. Network and sign-in work runs in the mail service; cancellation preserves the previous cache until a successful sync commits.
- First sync follows all message-list pages, including older mail, Sent, Archive, Trash, Spam, and server drafts. Later refreshes follow paginated Gmail history and fetch changed messages only. An expired history cursor triggers a full sync. Reads are paced to at most 75 requests/minute, with cancellable exponential backoff for temporary rate limits and server errors. Daily/project quota exhaustion is reported separately. Large first-time mailboxes may take time and use substantial memory; downloaded messages appear during sync, with progress saved across cancellation, quota failures, and restarts.
- SQLite account/message/sync-state cache, searchable offline on restart. Incremental changes and the history cursor commit together after successful retrieval. Initial full sync stages each downloaded message for immediate display, resumes saved pages without redownloading completed messages, and only removes stale cached messages and advances history when the full scan finishes. Failed requests retain both previous cached mail and downloaded progress, while other accounts can still sync successfully. The old inbox-only cursor is invalidated once during migration.
- Nested MIME plain-text and HTML alternatives, sender/subject/date headers, unread/starred display. HTML is parsed into native formatted text; remote resources are never loaded. HTML-only messages also get searchable plain text extracted from the safe document.
- Mark read/unread, toggle star, archive, and move to Trash through Gmail. Opening a message does not automatically mark it read. Sent messages can appear in both Inbox and Sent when Gmail labels require it.
- Multiple local drafts with independent sender, To/Cc, reply context, text, and attachments; the active draft is restored on restart. Reply/reply-all quote the original message and preserve Gmail threading. Forward quotes the original and offers optional attachment selection. Send clears only the delivered draft. Sends are never automatically retried; after an uncertain network result, check Sent before retrying.

Cache: `$XDG_DATA_HOME/focalmail/mail.sqlite3` (normally `~/.local/share/focalmail/mail.sqlite3`). The containing directory is private to the user. Mail and drafts are locally stored without application-level encryption; use disk encryption if required.

POP3, generic IMAP OAuth, server draft editing,
and FocalDesk Vulkan/theme integration
remain future work. Server drafts are listed separately from the local composer
draft. The current standalone window renders with OpenGL.

## Replies, forwards, and drafts

- **New**, **Reply**, **Reply all**, and **Forward** each create a separate draft
  after saving the current one. Click **Drafts** in the sidebar to open, switch,
  or delete saved drafts. Closing the composer retains the draft; **Discard
  draft** deletes just that draft and its copied attachments.
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

- HTML alternatives display with headings, bold/italic emphasis, paragraphs,
  lists, quotes, code, and simplified table rows. Use **Formatted / Plain text**
  to switch views. This is a native text renderer, not a full browser layout;
  sender CSS, exact table geometry, and custom fonts are ignored.
- Scripts, forms, embedded frames, SVG, media, event handlers, redirects, and
  stylesheets are omitted. Images (including tracking pixels and inline images)
  remain placeholders; no sender resources are requested automatically.
- Only absolute HTTP/HTTPS links without embedded credentials are actionable.
  Clicking a link control previews its full destination; **Open in browser**
  launches it only after that explicit click. Other URL schemes are inert.
- Older cached messages offer **Load formatted message and attachments** to
  retrieve the selected message. Large body parts stored separately by Gmail
  are fetched on demand. The formatted source limit is 1 MiB, with limits on
  nesting, node count, and formatted output. Loading runs off the UI thread.
- Safe formatted content is cached for offline reading. No new Google OAuth
  permissions are required.

## Attachments

- Incoming messages list attachment names and sizes. Click **Save to Downloads**
  to download a file; duplicate names receive a numeric prefix. Files are never
  opened automatically. The per-file download limit is 20 MiB.
- Messages cached by older builds have a **Load formatted message and attachments** button. This
  fetches content for just that message without restarting full mailbox sync.
- In the composer, click **Attach file…**, navigate to a file, and select it.
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
cargo test -p focalmail --offline
cargo clippy -p focalmail --offline --all-targets -- -D warnings
```

Tests also cover Microsoft pagination/origin validation and cache preservation, IMAP UID identities, flag refresh and STARTTLS refusal, shared MIME, notification filtering, the headless daemon, private socket permissions, single-instance locking, account status persistence, cancellation ownership, background sync preemption, and AI mail transport/confirmation. Tests cover callback validation, MIME body selection and composition, header injection rejection, mock HTTP pagination/history/deletion/fallback, label-action requests, send failures, cache replacement isolation, migration, and draft persistence. Mock HTTP tests bind temporary loopback ports. Real account sign-in requires your Google client and browser consent and is a manual integration check.

Protocol references: [Google desktop OAuth](https://developers.google.com/identity/protocols/oauth2/native-app), [Gmail client synchronization](https://developers.google.com/workspace/gmail/api/guides/sync).

Provider references: [Microsoft device-code authorization](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-device-code), [Microsoft Graph MIME sending](https://learn.microsoft.com/en-us/graph/api/user-sendmail), [IMAP session API](https://docs.rs/imap/latest/imap/struct.Session.html), [SMTP TLS modes](https://docs.rs/lettre/latest/lettre/transport/smtp/client/enum.Tls.html).
