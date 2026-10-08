# AIOS Scenario Lab

Scenario Lab is FocalDesk's deterministic shadow evaluator for AIOS routing,
consent, capability, confirmation, and budget contracts. It never calls a
provider, invokes a tool, publishes a live event, starts an agent, or mutates a
runtime service.

## Fixture boundary

Version 1 fixtures must set `synthetic` to `true`, contain 1–128 steps, and
encode to no more than 192 KiB. The schema accepts:

- synthetic voice phrases with an optional expected deterministic route;
- connector events plus explicit source disclosure and connector schemas;
- context kind, sensitivity, provenance, and field names—but no values;
- routine events evaluated by a fresh shadow routine engine;
- agent plan tool names, mutation/confirmation state, lease state, and token
  estimate—but no arguments or results;
- isolated provider/connector failure injection and shadow restart recovery;
- privacy-minimized Mission Control timeline observations.

Unknown JSON fields are rejected. Identifiers, text, collections, payload size,
step count, and total fixture size are bounded. A fixture that is not explicitly
marked synthetic is rejected.

## Safety invariants

The evaluator currently checks:

- Event Fabric connectivity and explicit source disclosure;
- connector enablement, declared source, and payload-field schema;
- deterministic voice routing when an expectation is supplied;
- simulated-only routine evaluation;
- agent tool allowlists and active capability leases;
- native confirmation presence for proposed mutations;
- aggregate scenario token budget;
- provider/connector unavailability until a matching shadow restart, plus
  isolation of failure, restart, and observed-trace steps.

Each failing invariant produces a stable violation code. A scenario passes only
when the set of observed codes exactly equals `expected_violation_codes`. This
supports both safe-path tests and deliberate negative tests without treating an
expected denial as a CI failure. Every report explicitly returns zero provider
calls, tool executions, and live mutations.

## Console workflow

Open **AI Console → Scenario Lab**. The editor starts with a valid safe example.
Select **Evaluate isolated scenario** to receive a structured report. **Capture
minimized trace** converts up to 100 filtered Mission Control entries into inert
`observed_timeline` steps. Capture does not include event payloads, context
payloads, tool arguments/results, answers, artifacts, or raw failures, and it
does not save a file automatically.

## CI workflow

The CLI evaluates the same Rust contract locally, so the AI service does not
need to be running:

```sh
focaldesk-cli ai scenario docs/scenario-safe.example.json
```

It prints the JSON report and exits nonzero for invalid fixtures or expectation
mismatches. The CLI accepts only a regular non-symlink UTF-8 file no larger than
192 KiB. The checked-in [safe example](scenario-safe.example.json) is suitable
as a starting point for repository-specific regression fixtures.
