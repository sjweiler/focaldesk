# AIOS Mission Control

Mission Control is the unified operational view for FocalDesk's AI layer. It
correlates already-bounded state from the Event Fabric, Context Fabric,
Attention engine, agent runtime, workflow supervisor, capability kernel,
connector registry, and durable control audit.

## Timeline privacy

The service constructs typed timeline entries rather than sending raw runtime
objects to the Console. Entries may contain:

- Event Fabric's already-redacted summary and producer provenance;
- context kind, sensitivity, and provenance;
- routine title, reason, and suggestion state;
- agent lifecycle transitions and tool names;
- workflow lifecycle and aggregate node/token counts;
- bounded control-plane action metadata.

The unified timeline does not contain tool arguments, tool results, context
payloads, agent answers, workflow artifacts, or raw failure messages. Control
audit details following a capability decision's `result=` field are removed.
Search is performed by the AI service over this minimized representation and
is bounded to 200 returned entries.

## Runtime inventory

The snapshot reports non-terminal agent and workflow runs, active capability
leases, active context grants, per-agent daily token/cost usage and limits,
connector identity/health, pending suggestion count, and the current proactive
safety state. The Console also joins read-only microphone-owner and managed
connector-host runtime status. Secret values are never present; secret
capability references remain opaque handles.

The Console refreshes this snapshot every two seconds while Live refresh is
selected. A manual search or refresh uses the same typed request.

## Controls

Mission Control provides scoped cancellation for a named agent run or workflow
run. Cancellation uses the existing runtime lifecycle and does not manufacture
completion. A retained Event Fabric ID can be replayed only through the
existing non-retaining simulation path.

Emergency pause performs three service-owned changes:

1. suspend new scheduled and event-driven agent triggers;
2. suspend Attention routine dispatch;
3. disconnect Event Fabric intake.

It then asks the managed connector host to pause. The Event Fabric disconnect
is authoritative even if that separate host is unavailable. Existing runs are
not cancelled; use the scoped cancellation controls when that is intended.

The emergency state is persisted in the private agent runtime database and is
reapplied after an AI-service restart. There is deliberately no one-click
global resume. Explicitly resuming triggers, routines, or Event Fabric through
their owning workspace clears the global latch and makes the recovery choice
visible.
