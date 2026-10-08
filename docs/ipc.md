

# IPC Design

FocalDesk uses IPC to separate the compositor from supporting desktop services.

The goal is to keep the compositor focused on display, input, surfaces, rendering, and session behavior while moving process launching, power management, notifications, dialog brokering, automation, and future AI-assisted actions into separate services.

## Goals

- Keep process launching out of the compositor
- Provide a clear boundary between shell UI and desktop services
- Support recoverable services
- Enable future permission checks
- Enable future AI and voice workflows
- Avoid turning the compositor into one large monolithic process

## Current Components

```text
FocalDesk Compositor / Shell UI
        │
        ├── focal-launch-shared
        ├── focal-launchd
        ├── focaldesk-powerd
        ├── focaldesk-notificationsd
        ├── focaldesk-updatesd
        ├── focaldesk-dialogd
        └── focaldesk-controlsd

Services
- launch requests
- power snapshot / suspend / hibernate / reboot
- notifications queue / visibility
- system update checks / install requests
- AI permission prompts
- portal chooser prompts
- wifi, bluetooth, and audio controls
```

## Components

### FocalDesk Compositor

The compositor sends requests when the user performs desktop actions such as launching an application.

It should not directly own long-running process management logic.

### focal-launchd

`focal-launchd` is the launcher daemon. It receives launch requests and starts applications outside the compositor process.

Responsibilities may include:

- Starting applications
- Preparing environment variables
- Returning success or failure status
- Logging launch attempts
- Future permission validation
- Future app metadata handling

### focal-launch-shared

`focal-launch-shared` contains shared IPC message types used by both the compositor and launcher daemon.

This prevents duplicated request/response definitions.

### focaldesk-powerd

`focaldesk-powerd` owns power snapshot collection and system power actions.

Responsibilities may include:

- Reading battery and AC status
- Reporting power snapshots to the compositor and settings UI
- Executing suspend, hibernate, reboot, and power-off requests
- Applying performance profiles

### focaldesk-notificationsd

`focaldesk-notificationsd` owns notification queueing and visibility state.

Responsibilities may include:

- Accepting notification requests
- Expiring timed notifications
- Returning visible notification snapshots to the compositor

### focaldesk-updatesd

`focaldesk-updatesd` owns package-update discovery and install jobs so the
compositor never runs PackageKit or DNF on the render thread.

Responsibilities may include:

- Checking for updates on a background worker
- Caching package name, version, and description
- Installing selected or all updates via PackageKit or `pkexec dnf`
- Posting a notification when new updates appear

### focaldesk-dialogd

`focaldesk-dialogd` owns permission-style dialogs that need a human response.

Responsibilities may include:

- Showing AI permission prompts
- Showing portal chooser prompts
- Returning typed allow/deny or selection responses

### focaldesk-controlsd

`focaldesk-controlsd` owns quick system controls that shell out to helper tools.

Responsibilities include:

- Toggling Wi-Fi
- Toggling Bluetooth
- Setting default output volume

## Message Flow

```text
User clicks launcher item
        │
        ▼
Shell UI creates launch request
        │
        ▼
IPC message sent to focal-launchd
        │
        ▼
focal-launchd validates request
        │
        ▼
focal-launchd starts process
        │
        ▼
Response sent back to compositor
```

## Example Message Types

```rust
pub enum LaunchRequest {
    LaunchCommand {
        command: String,
        args: Vec<String>,
        working_dir: Option<String>,
    },
    LaunchDesktopFile {
        desktop_file: String,
    },
}

pub enum LaunchResponse {
    Started {
        pid: u32,
    },
    Failed {
        reason: String,
    },
}
```

## Future IPC Use Cases

```text
Compositor
    │
    ├── Launcher service
    ├── Settings service
    ├── AI assistant service
    ├── Voice command service
    ├── Plugin service
    ├── Control service
    └── Session manager
```

Future IPC may support:

- Launching applications
- Querying open windows
- Switching workspaces
- Taking screenshots
- Locking the session
- Adjusting settings
- AI-triggered desktop actions
- Voice commands
- Permission-gated automation

## Permission Model

FocalDesk service sockets are private to the current user and use Linux peer
credentials to authenticate each connection. Sensitive endpoints then apply a
deny-by-default caller policy using the peer executable and systemd cgroup
unit. For example, power requests are accepted only from the compositor and
Settings, while password-capable dialog requests are accepted only from the
PolicyKit agent, portal, and AI service.

This process identity boundary protects services from unrelated applications
in the same desktop session. It does not treat the user's own writable binaries
as a security boundary. Release builds accept executable-name grants only when
the resolved executable is root-owned and not group- or world-writable;
systemd-unit grants remain available to packaged user services. Debug builds
allow user-owned executables for repository development. The
`FOCALDESK_ALLOW_USER_OWNED_IPC_PEERS` escape hatch restores that development
behavior explicitly and must not be set in a production session.

AI and automation features must not have unrestricted control over the desktop.

Possible permission levels:

```text
Read-only:
- Query windows
- Query workspace state
- Query settings

User-approved:
- Launch applications
- Change volume
- Switch workspace
- Take screenshot

Restricted:
- Run shell commands
- Modify files
- Change security settings
- Close applications
- Power off / reboot
```

## Design Principles

- Keep messages explicit
- Prefer typed request/response structures
- Log failed requests
- Do not block the compositor on long-running work
- Keep the compositor stable if a service crashes
- Treat AI actions as untrusted until approved
- Keep IPC contracts documented and versioned

## Wire Contract

Typed JSON requests and responses use a versioned envelope:

```json
{
  "protocol_version": 1,
  "payload": {
    "type": "GetSnapshot"
  }
}
```

### AI protocol compatibility

The AI socket uses the standard transport envelope above and places an
AI-specific v2 envelope in its payload:

```json
{
  "protocol_version": 1,
  "payload": {
    "ai_protocol_version": 2,
    "request_id": "1234-9",
    "payload": { "type": "Status" }
  }
}
```

Request IDs are echoed in responses and clients reject mismatches. IDs are
limited to 64 ASCII letters, digits, hyphens, or underscores. AI requests are
limited to 256 KiB and responses to 512 KiB, within the transport-wide 1 MiB
limit.

For rolling upgrades, the daemon also accepts the former bare AI request as
legacy protocol v1 and responds in that same form. A v2 client retries once
with v1 only when an older daemon explicitly reports that the v2 payload was
invalid before executing it. Unsupported explicit AI versions receive an
error containing the supported version. This compatibility path is temporary
and should be removed only at a documented breaking release.

Streaming chat is v2-only. A `ChatStream` request keeps its connection open
and receives newline-delimited v2 response envelopes whose payloads contain
`started`, `delta`, and exactly one terminal `completed`, `failed`, or
`cancelled` event. Both the envelope and event carry the original request ID,
and clients reject either mismatch. Every frame and the accumulated response
are bounded to 512 KiB. `CancelStream` is sent over a separate connection so
the stream remains readable until the daemon emits its terminal event.

The AI `Status` response includes an in-memory telemetry snapshot for every
registered provider. Counters cover logical requests, successes, failures,
cancellations, timeouts, retries, latency, byte totals, and token usage when a
provider reports it. Telemetry contains only bounded error summaries and is
reset when the daemon restarts; prompts and response bodies are never stored in
the snapshot.

Agent execution has a separate, service-owned lifecycle. `RunAgent` responses
include a random run ID. `ListAgentRuns` and `GetAgentRun` expose bounded
in-memory records with permission, queue, execution, confirmation, terminal,
deadline, and completed-tool-step state. `CancelAgentRun` can cancel queued or
running work and can discard an action awaiting confirmation; it never confirms
or executes a mutation. The daemon retains at most 128 run records, evicting
only terminal records, and checkpoints them in a private SQLite database.
Terminal history survives restart. Interrupted runs are recovered as failed;
`RetryAgentRun` starts a new run with the normal permission flow and resumes
from any checkpointed read-only observations. Pending confirmations are
deliberately invalidated on restart.

`StartAgent` is the non-blocking form: it validates and registers the run
synchronously, returns `AgentStarted` with its run ID, and continues permission,
queue, model, and tool work in the daemon. `GetAgentRun` includes the bounded
final `AgentResponse` once available, so clients can disconnect, reconnect, and
recover results. `WatchAgentRun { run_id, after_sequence }` long-polls the
service-owned journal and returns newer sequenced `AgentRunEvents` or the stable
run state. The latest 64 lifecycle, planning, tool, proposal, and terminal
events are retained. The AI Console uses this event-driven path while offering
cancellation through the known run ID. `RunAgent` remains as the blocking
compatibility operation.

The CLI exposes these operations as `ai agent-runs`, `ai agent-status <run-id>`,
`ai agent-cancel <run-id>`, and `ai agent-retry <run-id>`.

`ListAgents` returns the service-owned registry of built-in and validated custom
agent definitions. `RunAgent` accepts an optional `agent_id`; the runtime limits
the planner to that definition's tool allowlist and step budget before any model
call or tool execution. Manifests never add tools or permissions.

`FireAgentTrigger` starts one named declarative trigger. `DispatchAgentEvent`
matches a bounded desktop, voice, hotkey, or IPC event against enabled trigger
definitions and returns `AgentTriggersStarted`. Schedule events cannot be
spoofed through IPC; the daemon owns their timers. Cooldowns and hourly limits
are evaluated from durable run records, and every triggered run records its
source before entering the ordinary permission and confirmation lifecycle.
`GetAgentTriggerState` and `SetAgentTriggersSuspended` expose the persistent
global emergency state. Suspension blocks new scheduled and event-driven runs,
but does not cancel manual or already-running work.

`InstallAgent` writes a validated package through the daemon's configured agent
directory, creates the explicit update backup, audits the change, and reloads
the registry. `ReloadAgents` atomically replaces the live custom-agent registry
only after every package validates. `GetAgentControlStatuses` reports per-agent enablement,
daily health, token usage, and explicitly priced estimated cost.
`SetAgentEnabled` persists lifecycle state, while `RollbackAgent` restores the
validated backup and reloads it. `DryRunAgent` performs one bounded provider
planning call and returns its plan and usage without invoking any tool. These
lifecycle operations are written to the private control-event audit table.

The AIOS supervisor uses `ListWorkflows`, `StartWorkflow`,
`ListWorkflowRuns`, `GetWorkflowRun`, `SetWorkflowPaused`, `CancelWorkflow`,
and `RetryWorkflow`. Workflow status includes the validated node graph state,
child agent run IDs, shared token consumption, deadline, errors, and typed
dependency artifacts. Child nodes cannot bypass the ordinary agent permission,
budget, tool-allowlist, or mutation-confirmation boundaries. Active workflow
state is recovered fail-closed after restart while completed artifacts remain
available to a fresh retry.

`PreviewCapabilities`, `ListCapabilityLeases`, and `RevokeCapabilityLease`
expose the AIOS capability kernel. Leases include tool authority, resource
scopes, issue/expiry timestamps, revocation state, and related agent/workflow
run IDs, but never secret values. Enforcement happens again immediately before
every tool invocation, including a mutation that has received native approval.

AI memory lifecycle operations are also part of v2. `MemoryStatus` reports the
schema version, current record count, retention window, capacity, and oldest
and newest record timestamps. `ClearMemory` atomically deletes relational and
vector rows and returns the number removed. `Forget` and `ClearMemory` require
a fresh native one-shot confirmation; legacy or persisted chat approval is not
accepted as destructive-data consent.

The outer transport remains at version 1. Missing or unsupported outer
versions are rejected explicitly rather than being interpreted as a different
request shape. Requests are limited to 1 MiB. Ordinary blocking connections
use five-second read and write timeouts.

Sockets normally live below `$XDG_RUNTIME_DIR/focaldesk`. The directory is
required to be owned by the current user with mode `0700`; sockets use mode
`0600`. FocalDesk refuses to replace a non-socket, symlinked runtime directory,
or foreign-owned socket path.

`focald-connectors.sock` exposes the managed connector host's typed status,
poll-now, pause/resume, and quarantine-clear operations. Only the AI Console
and FocalDesk CLI transport identities are accepted. Event publication itself
goes to the AI service, which revalidates connector enablement, manifest schema,
source consent, and the Event Fabric emergency state.

`GetMissionControl` returns a bounded, optionally filtered operational snapshot
covering minimized timeline entries, active agent/workflow runs, capability
leases, context grants, agent budgets, connector health, suggestions, and proactive
safety state. It excludes tool arguments/results, context payloads, artifacts,
answers, and raw failures. `ActivateMissionControlPause` persistently suspends
agent triggers and routines and disconnects Event Fabric; it does not cancel
already-running work. The Console separately requests connector-host pause and
uses the existing cancellation and simulation IPC for scoped controls.

`EvaluateScenario` accepts one bounded, explicitly synthetic Scenario Lab
fixture and returns a deterministic report. Evaluation constructs only fresh
shadow state and records zero provider calls, tool executions, or live
mutations. `CaptureScenario` converts a bounded Mission Control query into
inert minimized timeline steps and returns the fixture without writing it.
Unknown fixture fields and unmarked, oversized, or unbounded fixtures are
rejected before evaluation.

AIOS package management uses `InspectPackage`, `TrustPackageSigner`,
`StagePackage`, `ActivatePackage`, `RollbackPackage`, and `ListPackages`.
Bundles are capped at 2 MiB; the AI IPC request ceiling is 3 MiB to allow for
the signed bundle envelope. Staging requires a trusted valid signature and a
passing bundled Scenario Lab suite. Activation never enables a packaged
connector or grants it network authority.

Package Forge adds `GeneratePackageSigner` and `BuildPackageProject`.
Generation and signing execute inside the AI service: the private Ed25519 seed
is stored under `aios/signers/<id>/ed25519` in `focald-secrets` and is never
returned over AI IPC. `BuildPackageProject` validates the typed project, runs
its complete synthetic scenario suite, and returns only the signed bundle.

### Microphone and speech sessions

`focald-mic.sock` accepts the typed `MicrophoneIpcRequest` contract. Authorized
executables can request normal dictation or ambient wake-gated capture, poll a
bounded sequenced event journal, clear retained events, stop their lease, or
operate the FocalDesk-wide kill gate. The daemon binds ownership to the
authenticated peer executable rather than trusting a caller-supplied name. One
exclusive lease exists at a time; ambient leases expire when their heartbeat
is abandoned.

`focald-speech.sock` accepts `SpeechIpcRequest`. Interrupt-priority requests
replace current output, and live microphone activity issues a stop request for
barge-in. Neither endpoint can resolve an AI mutation confirmation.

## Open Questions

- Should IPC use Unix domain sockets, D-Bus, or another transport?
- Should AI actions require confirmation by default?
- How should service crashes be handled?
- How should logs be correlated across services?

## Summary

The IPC layer is intended to make FocalDesk modular, recoverable, and extensible. The compositor should remain focused on core desktop behavior while services handle launching, settings, automation, and future AI workflows through explicit message contracts.
