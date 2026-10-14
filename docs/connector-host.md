# Managed Connector Host

`focald-connectors` is the execution boundary between enabled connector
manifests and the AIOS Event Fabric. Installing the service does not enable a
connector, enable an event source, grant network access, or start data
retention. Those decisions remain explicit in the AI Console.

## Local adapters

The host provides bounded adapters for the built-in connector identities:

- `desktop-events` publishes a minimized event only when focus or workspace
  changes;
- `service-health` publishes state changes and reports unavailability only
  after three consecutive failed checks;
- `notifications` publishes each currently visible unread notification once;
- `local-calendar` reads only an explicitly configured absolute ICS file of at
  most 1 MiB and extracts UID, summary, start, end, and organizer;
- `google-calendar` performs read-only incremental Google Calendar sync for
  explicitly configured accounts after calendar-source and network consent;
- `microsoft-calendar` performs read-only Microsoft Graph calendar-view delta
  sync under the same consent controls;
- `workflow-events` remains inside the AI supervisor, where workflow lifecycle
  events originate.

The optional calendar path is set with `FOCALDESK_LOCAL_CALENDAR_ICS` in a
user-service drop-in. No calendar location is guessed or scanned.

Cloud accounts are declared in
`$XDG_STATE_HOME/focaldesk/calendar-accounts.json` (normally
`~/.local/state/focaldesk/calendar-accounts.json`). Settings writes it only
through the typed connector-host IPC. The file contains no tokens:

```json
{
  "version": 1,
  "accounts": [
    {
      "id": "school-google",
      "provider": "google",
      "enabled": false,
      "client_id": "YOUR_DESKTOP_OAUTH_CLIENT_ID",
      "calendar_id": "primary",
      "calendar_ids": ["primary"],
      "reminder_minutes": 10
    },
    {
      "id": "work-m365",
      "provider": "microsoft",
      "enabled": false,
      "client_id": "YOUR_PUBLIC_CLIENT_ID",
      "calendar_id": "primary",
      "calendar_ids": ["primary"],
      "reminder_minutes": 10,
      "tenant": "common"
    }
  ]
}
```

OAuth refresh tokens live only at the opaque secret handles
`calendar/<account-id>/refresh-token`; an optional Google client secret uses
`calendar/<account-id>/client-secret`. Provider connectors, account entries,
network access, and the Event Fabric calendar source must all be enabled
before synchronization starts. Cached normalized events and reminder-delivery
state are kept in `$XDG_STATE_HOME/focaldesk/calendar.db`. Event descriptions,
locations, attendee lists, conference URLs, and message bodies are neither
stored nor published.

The Settings **Calendar** page starts authorization through typed connector
IPC. Google uses the system browser, a random loopback port, PKCE-S256, and a
validated state value. Microsoft uses the device-code flow and displays the
short user code before opening the system browser. Authorization expires after
ten minutes (Google) or the provider-supplied Microsoft deadline. Settings
receives status and public verification data only; refresh tokens and optional
Google client secrets never return over the connector IPC.

The same page lists each account's provider, connection state, cached-event
count, last connector run, and current connector error. **Calendars** securely
discovers the provider's calendar list, lets the user select up to 32 calendars,
and configures reminders as off, 5, 10, 15, 30, or 60 minutes before an event.
**Sync now** queues an immediate provider poll, and the page previews up to 20
cached events from the next seven days. **Disconnect** removes
the refresh token and any saved client secret through the broker. The separate
**Delete local data** option also removes that account's cached events and sync
cursor; leaving it unchecked retains the local cache while preventing further
cloud synchronization.

Google authorization must use the
`https://www.googleapis.com/auth/calendar.readonly` scope. Microsoft
authorization must use delegated `Calendars.Read` and `offline_access`. Both
providers use incremental cursors, refresh the bounded 30-days-past to
365-days-future window weekly, and poll at most once every five minutes.
Upcoming normalized events can be inspected without exposing tokens:

```sh
focaldesk-cli calendar upcoming 7
```

Each adapter runs only when both its connector and at least one declared Event
Fabric source are enabled. Events are checked again by the AI service against
the installed manifest, supported field schema, source policy, and emergency
disconnect before entering the redacted journal.

## Managed external connectors

An external manifest can add a `runtime` declaration containing an absolute
executable, bounded arguments, poll interval, memory limit, and CPU quota. On
each due poll the host starts a transient `systemd --user` service with a
60-second runtime limit, private temporary directory, read-only system and home
trees, namespace and personality restrictions, no-new-privileges, and
write/execute memory protection.

Without explicit network consent, the child receives only `AF_UNIX`. With
consent, the host resolves only the manifest's exact declared domains and gives
the transient unit an IP deny-all policy plus allow rules for the resolved
addresses. This is a bounded launch-time allowlist, not protection against DNS
changes after launch or another same-user process.

The executable must be an absolute regular executable and must not be group- or
world-writable. It writes JSON Lines to standard output:

```json
{"source":"calendar","payload":{"event":"calendar meeting","title":"Example","start_time":"2026-10-08T13:00:00Z"}}
```

Output is capped at 64 KiB per poll. Every line must decode to a declared event
source and payload; the AI service performs the authoritative schema and
consent check. Five consecutive host failures quarantine a connector. Retry
backoff is exponential and capped at five minutes.

## Operations

The Event Fabric page in the AI Console shows host runtime state and provides
Poll now, Pause, Resume, and Clear quarantine controls. Pausing is session-only
and leaves stored connector and source consent unchanged. The control protocol
uses the private `focald-connectors.sock` runtime socket and accepts only
authorized FocalDesk executables.

For a local development installation:

```sh
just install-focald-connectors
```

The full `just install-services` and Fedora service recipes include the host.
The service is enabled as infrastructure, but every connector and every Event
Fabric source still starts disabled.
