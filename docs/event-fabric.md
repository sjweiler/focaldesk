# Event Fabric and Consent Center

The Event Fabric is the consent and minimization boundary between typed system
events and the Attention engine. All sources are disabled by default. Enabling
a source requires an explicit field allowlist, retention duration, and choice
of whether its redacted summary may be forwarded to Attention.

The supported sources and fields are:

- `desktop`: `event`, `app_id`, `workspace_id`, `window_title`
- `calendar`: `event`, `title`, `start_time`, `end_time`, `organizer`
- `notification`: `event`, `app_id`, `summary`, `body`, `urgency`
- `service_health`: `event`, `service`, `state`, `message`, `failure_count`
- `workflow`: `event`, `workflow_id`, `run_id`, `state`, `error`

The AI Console's **Event Fabric** workspace shows these schemas and the live
policies. Desktop events already entering the typed AI event dispatcher and
terminal workflow events from the AI supervisor are connected producers.
Calendar, notification, and service-health integrations can publish through
the same authenticated same-user AI IPC only after their source policy is
enabled. The fabric does not scrape applications, screen pixels, notification
history, or calendar databases itself.

## Data minimization and retention

An incoming payload must be a JSON object no larger than 16 KiB. The fabric
copies only supported, explicitly allowlisted scalar fields into a new
envelope. Objects, arrays, unknown fields, and the original payload are not
retained or forwarded. Each retained envelope carries a random ID, source,
producer provenance, redacted payload, derived summary, creation time, and
expiry.

Retention is configurable per source from 60 seconds through 24 hours. The
in-memory journal holds at most 256 envelopes and can be cleared immediately.
Expiry pruning occurs during ordinary access. Restarting the AI service clears
the journal, while source policies persist through the private, atomically
updated [Connector Trust Store](connector-sdk.md).

## Simulation and control

Simulation applies the current consent policy and Attention rules but retains
neither an event nor a suggestion. A retained event can be replayed as a
simulation fixture using its ID; replay uses only the already-redacted
envelope.

**Emergency disconnect** rejects all event intake immediately while leaving
individual source policies unchanged. Reconnecting does not enable a source.
The Attention emergency pause is separate: it permits event journaling but
suppresses routine suggestions.

Forwarding to Attention can only create an inert suggestion. Event content is
untrusted evidence and cannot start a run, grant context, widen capabilities,
answer a permission prompt, or approve a mutation.
