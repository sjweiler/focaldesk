# Attention and Routine Engine

The Attention and Routine Engine is FocalDesk's bounded proactive layer. It
accepts typed events, evaluates declarative rules, and can publish an inert
suggestion. Event delivery never starts an agent, workflow, tool call, or
desktop mutation.

The AI Console's **Attention** workspace exposes the current routine state, an
event simulator, suggestion controls, and the global emergency pause. The
initial built-in routines are:

- `morning-briefing`, matching a `context` event containing `morning`;
- `meeting-preparation`, matching a `calendar` event containing `meeting`;
- `repeated-service-failure`, matching a `service` event containing
  `repeated failure`.

Events are bounded to a kind, a short value, and a provenance source. The
[Event Fabric](event-fabric.md) connects consented typed desktop and workflow
events and provides controlled intake for calendar, notification, and service
producers. It does not silently scrape notification, calendar, file, or screen
contents.

## Attention controls

Every routine declares its match phrase, priority, suggested promotion target,
cooldown, hourly limit, and optional UTC quiet hours. Matching is local and
deterministic. The inspector explains whether an event did not match, was
suppressed by pause/quiet hours/cooldown/rate limiting, or published a
suggestion. Normal and simulated evaluations use the same policy checks, but
simulation does not record a firing or create a suggestion.

The engine retains at most 64 suggestions in memory. Suggestions expire after
24 hours and include the routine, trigger kind, trigger source, trigger value,
reason, priority, and proposed destination. Repeated equivalent events are
deduplicated within the routine cooldown.

**Emergency pause** suppresses all routine matches immediately. It does not
stop an agent or workflow the user already chose to run; those remain visible
and controllable in Agent Studio.

## Explicit promotion

A suggestion remains inert until the user supplies its ID to **Promote to
agent/workflow**. Promotion is single-use. The AI service then starts the named
registered agent or bounded workflow through its ordinary capability lease,
budget, permission, and native one-shot mutation-confirmation path. A failed
start releases the claim so the suggestion can be retried. Dismissal makes a
suggestion non-actionable.

Routine event text is evidence, not authority. It cannot grant context,
increase a capability ceiling, answer a permission prompt, or approve a
mutation.
