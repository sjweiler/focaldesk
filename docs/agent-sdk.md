# FocalDesk Agent SDK

FocalDesk agents are declarative runtime profiles. A manifest can select a
bounded subset of the audited desktop-tool catalog and add planning
instructions; it cannot load executable code, grant itself permissions, or
bypass native confirmation.

## Rust builder

The public `focaldesk_ai::AgentBuilder` constructs and validates the same
versioned manifest used by the daemon:

```rust
use focaldesk_ai::AgentBuilder;

let agent = AgentBuilder::new("workspace-guide", "Workspace Guide")
    .description("Explains the active workspace.")
    .instructions("Use observed desktop metadata only.")
    .allow_tools(["list_windows", "list_workspaces"])
    .max_tool_steps(2)
    .max_context_chars(16_000)
    .max_output_tokens(512)
    .timeout_seconds(45)
    .voice(true)
    .build()?;

std::fs::write("agent.toml", agent.to_toml()?)?;
# Ok::<(), anyhow::Error>(())
```

Validation caps tool steps at 4, context at 128,000 characters, model output at
4,096 tokens, and run time at 120 seconds. These budgets are enforced by the
runtime and visible in run status.

Place manifests in `$XDG_CONFIG_HOME/focaldesk/agents` (normally
`~/.config/focaldesk/agents`) or set `FOCALDESK_AGENT_DIR` for development.
The service rejects symlinks, files larger than 64 KiB, duplicate IDs, unknown
fields, unsupported versions, and attempts to impersonate a built-in agent.

```toml
manifest_version = 1
id = "workspace-guide"
name = "Workspace Guide"
description = "Explains the current workspace and proposes navigation actions."
instructions = "Be concise. Describe every proposed change before asking for confirmation."
tool_allowlist = ["list_windows", "list_workspaces", "focus_window"]
max_tool_steps = 3
max_context_chars = 32000
max_output_tokens = 512
timeout_seconds = 90
memory = false
voice = true

[[triggers]]
id = "session-start"
kind = "desktop_event"
objective = "Inspect the new session and report anything that needs attention."
match_value = "session_started"
cooldown_seconds = 300
max_runs_per_hour = 2
enabled = true
```

A package may be a single `*.toml` file or a directory containing
`agent.toml`, such as `agents/workspace-guide/agent.toml`. Package directories
remain declarative: sibling documentation and assets are ignored, and no
executable hook is loaded. Symlinked packages and manifests are rejected.

For deterministic contract tests, the SDK exports `ScriptedAgentProvider` and
`MockAgentTools`. They implement the normal provider and tool traits, retain
requests and calls for assertions, and never access the network or desktop.

## Triggers

Declarative triggers support `schedule`, `desktop_event`, `voice_phrase`,
`hotkey`, and `ipc_event`. Schedule triggers use `interval_seconds` from 60 to
86,400 and leave `match_value` empty. Event triggers require an exact
`match_value`; voice phrases compare trimmed ASCII text case-insensitively and
also require `voice = true`. A manifest may define at most 16 triggers.

Every trigger has a cooldown of at most one day and an hourly limit from 1 to
60. The daemon reconstructs these limits from durable run history, so restart
does not reset them. Scheduled triggers wait one full interval after startup.
Event dispatch never activates a microphone: an explicitly enabled voice
service can forward a recognized phrase through the typed event API. Triggers merely start
the normal agent permission path. Any proposed mutation still pauses for a
fresh one-shot confirmation.

```sh
focaldesk-cli ai agent-trigger workspace-guide session-start
focaldesk-cli ai agent-event desktop_event session_started
focaldesk-cli ai agent-event voice_phrase "hello focaldesk"
focaldesk-cli ai agent-triggers suspend
```

## Agent Studio

For the console controls and package-agent lifecycle, see the
[Agent Studio guide](agent-studio.md).

The AI Console's **Agent Studio** provides a visual builder for identity,
instructions, audited tools, resource budgets, voice/memory metadata, and one
trigger. **Validate & preview** generates normalized TOML without writing.
**Install package** writes to `$XDG_CONFIG_HOME/focaldesk/agents/<id>/agent.toml`;
an existing package requires the explicit update checkbox and is backed up to
`agent.toml.bak`. Built-in IDs and symlink destinations cannot be replaced.

Agent Studio hot-reloads validated packages, can reversibly swap the current
manifest with `agent.toml.bak`, and persistently enables or disables individual
agents. Its dry-run simulator asks the provider for a bounded plan but never
invokes a tool; mutating steps are labeled as requiring confirmation. The live
health view reports daily runs, failures, tokens, and estimated cost when the
manifest supplies explicit per-million-token pricing. Daily token and estimated
cost ceilings fail closed before another run starts.

For signed `.fai` packages, activation and package rollback disable the
package's agents until the user enables them again. Ordinary server restarts
restore each active packaged agent's persisted enable state.

Agent Studio also provides searchable retained run history with trigger source
labels and provider-reported token totals. Its emergency switch persistently
suspends all scheduled and
event-driven starts while leaving manual runs available. Suspending triggers
does not cancel a run already in progress; use that run's cancellation control
when required.

The public Rust contract is `focaldesk_ai::AgentDefinition`. Use
`AgentDefinition::from_toml` to validate one manifest or
`load_agent_definitions` to apply the same bounded directory-loading rules as
the daemon. `ListAgents` discovers registered profiles over AI IPC. Select one
with `AgentRequest.agent_id`; an omitted ID selects the built-in `desktop`
profile.

## AIOS Supervisor

The supervisor runs validated `WorkflowDefinition` DAGs. A workflow contains at
most 16 nodes, permits at most four ready nodes in parallel, and has one shared
deadline and token ceiling. Nodes select existing registered agents, so a
workflow cannot add tools or permissions. Every child run still uses the normal
permission prompt and one-shot mutation confirmation path.

Completed nodes publish bounded
`application/vnd.focaldesk.agent-result+json` artifacts. Downstream objectives
receive only their declared dependency artifacts, labeled as untrusted
evidence. Workflow runs can be paused (which stops new scheduling), resumed,
cancelled, inspected, or retried. Running children are not frozen by pause;
cancel explicitly cancels them. State and completed artifacts persist in the
private runtime database. After daemon interruption, an active workflow is
failed closed and retry starts a fresh run from those durable artifacts.

Built-ins include `morning-briefing`, `workspace-troubleshooter`, and
`meeting-preparation`. Agent Studio visualizes their edges and node/child-run
state. The CLI exposes the same lifecycle through
`focaldesk-cli ai workflow ...`.

## Capability kernel

An optional `capability_policy` narrows an agent to named filesystem roots,
HTTP(S) origins, applications, workspaces, services, and opaque secret handles.
Blank scope fields mean unrestricted for compatibility; an explicit empty list
means no access to that resource class. Filesystem paths must be absolute and
normalized, and network entries must be origins without paths. Agent Studio
uses `-` to author an explicit empty scope.

Every managed run receives a random, expiring capability lease. The execution
wrapper checks both the leased tool list and resource-bearing arguments before
read-only or confirmed execution. A delayed mutation therefore fails if its
lease was revoked or expired even after native confirmation. Workflow ceilings
are intersected with the agent policy; filesystem intersection always selects
the narrower root. When network origins are scoped, the selected model
provider's configured endpoint must also fit the lease. No workflow can add authority.

`PreviewCapabilities` shows effective authority without starting a run.
`ListCapabilityLeases` and `RevokeCapabilityLease` provide the live authority
map and immediate revocation. Grants, uses, denials, expiration, and revocation
are recorded in the bounded private control audit. Secret values are never
returned: policies carry only opaque handle names for tools that understand
those handles.

`context_kinds` optionally limits expiring Context Fabric envelopes to
`active_window`, `workspace`, `notifications`, `calendar`, `files`, and
`conversation`. Context requires a separate live per-agent grant even when the
manifest leaves this scope unrestricted. An explicit empty list denies every
kind. Envelope payloads enter the objective as provenance-labeled untrusted
evidence and never as instructions or authorization.

For interactive clients, send `StartAgent` to receive a run ID immediately,
then use `WatchAgentRun { run_id, after_sequence }` to long-poll the sequenced
event journal until the run reaches `awaiting_confirmation` or a terminal
state. `GetAgentRun` remains the snapshot API. Results and the latest 64 events
are retained with the bounded run record, and `CancelAgentRun`
works while queueing, inference, or confirmation is pending. A cancellation
requested during a native permission prompt is applied as soon as that prompt
returns; it never dismisses or answers the prompt on the user's behalf.

Run records and their retry requests are checkpointed in the private
`$XDG_DATA_HOME/focaldesk/agent-runs.db` SQLite database. On daemon restart,
terminal history is restored. Interrupted permission, queue, planning, and tool
runs are marked failed and can be retried under a new run ID. Completed
read-only observations are checkpointed and supplied to the new planning loop,
so already recorded tools are not repeated. An interrupted confirmation is
invalidated, removed from the response, and must be planned again; recovery
never executes or revives a pending mutation.

Managed runs use an iterative loop: plan one tool, execute one read-only tool,
append its bounded observation to context, and replan. The manifest's
`max_tool_steps` caps the loop, each model call is capped at 1,024 output tokens,
tool results are capped at 16,000 characters, combined observation context is
capped at 48,000 characters, and the service deadline covers the entire run.
A mutating tool is never executed by the loop; it becomes a proposed action and
pauses for native one-shot confirmation.

```sh
focaldesk-cli ai agents
focaldesk-cli ai agent --profile accessibility "What windows are open?"
focaldesk-cli ai agent-retry <failed-run-id>
```

`voice = true` only advertises that a profile is suitable for voice clients.
It does not activate the microphone or authorize an action. The AI Console can
continuously transcribe an agent objective while its **Voice objective** control
is active; the user still explicitly runs the objective. It then enters the
same permission, tool, plan, and one-shot mutation-confirmation path as typed
text. The separate opt-in [Ambient Voice](voice.md) workspace can route a
wake-gated command to a voice-enabled profile, but it cannot answer a native
approval prompt or broaden that profile's capability lease.

## Built-in profiles

- `desktop`: general desktop inspection and confirmed actions.
- `troubleshooter`: read-only session, service, log, display, and rendering diagnosis.
- `accessibility`: concise voice-friendly inspection and navigation proposals.

Future executable agent packages should run out of process and require a
separate signed-package and sandbox contract. Manifest version 1 intentionally
does not provide executable hooks.
