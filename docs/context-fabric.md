# Context Fabric

The Context Fabric is the AI service's bounded bridge between current desktop
state and agents. It does not capture screen pixels, clipboard contents,
keystrokes, secrets, or arbitrary files. The AI Console publishes only a
selected typed compositor snapshot or the active Console conversation.

Every `ContextEnvelope` contains a random ID, a typed kind, provenance,
sensitivity (`public`, `private`, or `restricted`), creation time, expiry time,
and a JSON payload capped at 16 KiB. Envelopes live only in memory, expire after
5–3,600 seconds, and the broker retains at most 128 items.

## Agent access

Publishing does not share an envelope with an agent. The user must create an
expiring `ContextGrant` for a registered agent and one or more context kinds.
At run start, the broker intersects that grant with the agent manifest's
`capability_policy.context_kinds`. Missing grants and explicit empty capability
scopes deny access. Revocation applies to the next run immediately; **Clear all
context** removes every envelope.

Permitted envelopes are appended to the objective under a dedicated marker and
are explicitly labeled untrusted evidence. Payload text cannot add tools,
expand a capability lease, answer a permission prompt, or approve a mutation.

## Intent routing

The router handles a small explainable rule set before ordinary chat:

- “summarize this” or “explain this” routes to the accessibility agent and
  declares active-window and conversation context requirements.
- Meeting-preparation phrases route to the `meeting-preparation` workflow and
  declare calendar context.
- “what is wrong here?” and related repair phrases route to the
  `workspace-troubleshooter` workflow and declare active-window/workspace
  context.

Unmatched input is marked ambiguous and remains normal chat. Routing selects a
bounded agent or workflow; it does not grant the declared context or execute a
mutation by itself.

## Suggestion inbox

Agents and trusted clients can publish a bounded suggestion containing an
agent ID, short title, explanation, and expiry. The broker retains at most 64
suggestions in memory. Suggestions appear in the Context Fabric inspector and
can be dismissed explicitly. Publishing a suggestion never starts a workflow,
invokes a tool, or converts the suggestion into approval.
