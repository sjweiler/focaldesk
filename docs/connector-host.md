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
- `workflow-events` remains inside the AI supervisor, where workflow lifecycle
  events originate.

The optional calendar path is set with `FOCALDESK_LOCAL_CALENDAR_ICS` in a
user-service drop-in. No calendar location is guessed or scanned.

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
