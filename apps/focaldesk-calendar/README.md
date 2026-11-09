# FocalDesk Calendar

A unified GTK desktop calendar for existing Google Calendar and Microsoft 365
connections. The dark weekly layout follows the supplied reference: a miniature
month calendar and account filters on the left, with a seven-day time grid,
provider-colored event cards, search, week navigation, and a Create event button.
All-day events appear above the timeline. Overlapping and overnight events are
displayed within their day columns.

```sh
cargo run -p focaldesk-calendar-app
# Optional system installation:
just install-calendar
```

Release builds use the AI endpoint’s trusted executable policy, so install the
binary using `just install-calendar` to enable meeting preparation with a
production AI service.

The executable is `focaldesk-calendar`; the Cargo package has an `-app` suffix to
distinguish it from the storage library in `crates/focaldesk-calendar`.

## Connected calendars

Run the FocalDesk AI service and `focald-connectors`, connect accounts through
Settings → Calendar, and enable the provider connectors and their network
permissions in AI Console. The app reads accounts and cached events through the
existing connector-host socket. Refresh queues provider sync, then reloads the
cache. Sync is asynchronous; refresh again if a provider takes longer to respond.
The weekly view displays at most 500 cached events, with an explicit notice if
the limit is reached. Account checkboxes and search filter the displayed events.

Create an event or click an existing card to edit its title, times, and all-day
status. Times use the system's local timezone; invalid or ambiguous daylight
saving times are rejected. All-day events use exclusive midnight end dates.
Changes save to the selected provider calendar, and deletion requires
confirmation. Existing attendees, recurrence settings, descriptions, and other
provider fields are preserved during editing. Those fields cannot currently be
edited in this app. Changing or deleting a meeting can notify its attendees.

Earlier connections used read-only permissions. Reconnect those accounts to
grant Google event write access or Microsoft `Calendars.ReadWrite`. The provider
must also allow editing the selected calendar. Credentials stay in the connector
service and secrets broker; the UI never reads refresh or access tokens.

## Meeting preparation

The event editor's **Prepare for this meeting with AI** button sends the current
event title, times, and organizer to the configured FocalDesk AI provider. It
generates an agenda, questions, and preparation checklist. Preparation runs only
when clicked and does not alter the event. It does not retrieve mail or documents.

## Validation

```sh
cargo check -p focaldesk-calendar-app -p focald-connectors --offline
cargo test -p focaldesk-calendar-app -p focald-connectors -p focaldesk-ipc --offline
```

Tests cover date boundaries, overnight and all-day events, interval validation,
provider write payloads and URLs, and mutation IPC serialization. Live provider
writes require connected accounts and are not exercised by these tests.
