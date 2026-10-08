# FocalDesk Roadmap

This roadmap communicates direction rather than a release promise. Priorities
may change as compositor correctness, hardware behavior, and security findings
develop. The [README status table](README.md#project-status) is the source of
truth for what is usable today.

## Current stabilization priorities

- Improve compositor crash recovery, diagnostics, and reproducible bug reports.
- Use the completed nested compatibility harness to expand application coverage
  and exercise multi-monitor hotplug, scale, transform, and mixed-refresh
  behavior on recorded hardware configurations.
- Harden XWayland, portal capture, session startup, and user-service lifecycle.
- [x] Migrate legacy `config.toml` users atomically to the canonical
  `settings.json` configuration path while retaining a recovery copy.
- Keep AI and automation actions explicit, permission-gated, and auditable.
- [x] Establish repeatable alpha release automation and manifest-driven
  installation verification and uninstall checks.
- Keep the workspace warning-free under the CI-enforced `-D warnings` policy.

## Rendering and display work

- Profile the completed subsurface damage path across GTK, Qt, browsers, games,
  mixed-scale outputs, and direct DRM/KMS sessions.
- Continue reducing categorized full-output fallbacks using captured damage
  metrics and GPU measurements.
- Continue HDR, wide-gamut, ICC, and SDR-composition validation.
- Expand hardware-cursor, direct-scanout, multi-GPU, and presentation testing.

## AI integration milestones

- [x] Route console and CLI chat through the private AI service endpoint.
- [x] Support Ollama, OpenAI, Anthropic, and configurable vLLM providers.
- [x] Load cloud credentials through the native secrets broker.
- [x] Gate model requests with native prompts, persisted decisions, and revocation.
- [x] Provide opt-in semantic memory with remember, recall, and forget operations.
- [x] Provide a capability-gated, audited MCP desktop-tool catalog.
- [x] Implement a typed, bounded agent loop for desktop inspection and action proposals.
- [x] Require expiring plans and native one-shot confirmation for model-selected desktop mutations.
- [x] Version the AI IPC contract with request IDs and a legacy migration path.
- [x] Add bounded streaming responses and cancellation across Ollama, AI IPC, CLI, and Console.
- [x] Add bounded provider retries and provider telemetry.
- [x] Define and implement AI memory retention, bulk deletion, and migration policy.
- [x] Run semantic retrieval through the private FocalVector sidecar and add
  bounded text-document ingestion with source citations.
- [x] Add PDF/DOCX extraction, indexed-source refresh/removal, hybrid FTS5 and
  vector retrieval, deterministic reranking, retrieval evaluation, and an
  Indexed Sources console page.
- [x] Add deterministic provider-contract and agent-loop integration tests.
- [x] Add service-owned agent run lifecycle records, queue visibility, bounded
  retention, status inspection, and cancellation over typed AI IPC.
- [x] Add versioned declarative agent manifests, built-in profiles, bounded
  tool allowlists, runtime discovery, and a public validation API.
- [x] Add non-blocking agent starts, reconnectable retained results, live
  Console lifecycle polling, and direct cancellation by run ID.
- [x] Replan after each bounded tool observation and expose retained sequenced
  run events through `WatchAgentRun` for live Console timelines.
- [x] Persist bounded agent run history and retry requests in private SQLite,
  recover interrupted work fail-closed, and retry under a fresh permissioned run.
- [x] Add a public validation-backed `AgentBuilder`, per-agent context/output/
  deadline budgets, declarative package directories, and deterministic SDK mocks.
- [x] Add bounded declarative schedules, desktop/voice/hotkey/IPC triggers,
  durable trigger auditing, cooldowns, hourly limits, and daemon-owned timers.
- [x] Add an AI Console Agent Studio with bounded package authoring, validation,
  explicit backed-up updates, searchable run history, and emergency suspension.
- [x] Add the agent control plane: hot reload, persistent enable/disable,
  reversible rollback, no-tool plan simulation, health, usage, and daily budgets.
- [x] Add a durable AIOS workflow supervisor with bounded DAG validation,
  typed handoffs, parallel scheduling, shared budgets, recovery, and lifecycle UI.
- [x] Add an AIOS capability kernel with resource scopes, expiring leases,
  workflow intersection, runtime enforcement, revocation, previews, and audit.
- [x] Add an opt-in ambient voice runtime with local wake-phrase detection,
  bounded RAM-only capture, visible activity, barge-in, session-only transcript
  controls, a hard microphone gate, and agent/workflow routing.
- [x] Add a typed AIOS context and intent fabric with expiring provenance,
  sensitivity labels, explicit per-agent grants, capability intersection,
  inspection/revocation, and deterministic voice routing.
- [x] Add an AIOS attention and routine engine with typed event intake,
  suggestion-only declarative rules, quiet hours, cooldown/rate/deduplication
  controls, simulation, explicit promotion, and a global emergency pause.
- [x] Add an AIOS Event Fabric and Consent Center with disabled-by-default
  sources, supported-field disclosure, bounded redacted retention, typed
  desktop/workflow hooks, simulation replay, clearing, and emergency disconnect.
- [x] Add a connector SDK and private trust store with versioned schemas,
  authenticated/replay-protected producer identity, persistent consent,
  built-in local connector identities, health controls, explicit network
  authority, validated installation, updates, and rollback.
- [x] Add a managed connector host with minimized local adapters, transient
  systemd-sandboxed custom runtimes, resource limits, bounded retry/backoff,
  quarantine, control IPC, Console operations, and disabled-by-default consent.
- [x] Add AIOS Mission Control with a minimized cross-runtime timeline,
  provenance, live inventory, bounded audit search, simulation replay, scoped
  cancellation, and a restart-persistent proactive emergency pause.
- [x] Add an isolated AIOS Scenario Lab with bounded synthetic fixtures,
  minimized trace capture, deterministic routing/routine evaluation, safety
  invariant codes, expected-denial contracts, Console authoring, and local CI.
- [x] Add an AIOS Package Manager with signed declarative `.fai` bundles,
  explicit signer trust, authority diffs, mandatory Scenario Lab gates,
  staged activation, dependency checks, and rollback.
- [x] Add AIOS Package Forge with safe scaffolding, secrets-backed signer
  generation, reproducible builds, local testing and verification, an offline
  exact-dependency registry, Console authoring, and a CI template.
- [x] Add an opt-in private AIOS registry with authenticated immutable
  publishing, signed rollback-resistant catalogs, organization signer policy,
  revocation quarantine, lockfiles, explicit verified downloads, audit history,
  and Console catalog review.

## Desktop experience

- [x] Expand the completed workspace-slot controls with animated window-layout
  overview thumbnails.
- Add full-desktop workspace transition animations.
- [x] Expand the completed keybinding editor with direct shortcut capture and
  conflict feedback.
- Add configurable pointer gestures.
- Mature the launcher, Settings application, file manager, notifications, power
  handling, lock screen, and accessibility behavior.
- Improve first-run setup, recovery, and uninstall workflows.

## Theme editor phases

The editor authoring phases and compositor-native primary-paint gradients below
are implemented. See the [Theme Editor guide](docs/theme-editor.md) for the
current workflow and limitations.

1. sRGB saturation/value square with a separate hue slider.
2. Display P3 gamut mode using the same picker.
3. sRGB gamut boundary while editing Display P3 colors.
4. Optional hue ring replacing the slider, without changing color semantics.
5. Solid, linear-gradient, and radial-gradient editing with per-stop colors.
6. SDR/HDR dynamic-range selection and an independent HDR luminance control.
7. TOML save/load for custom themes, with validation and unsaved-change tracking.
8. Debounced live preview, apply, and revert through versioned compositor IPC.
9. Wallpaper assets, fit/tint controls, and safely installable theme packaging.
10. Semantic surface tokens, inherited interaction states, and a simultaneous state-matrix preview.
11. Compositor renderer parity for semantic states, geometry, typography,
    wallpaper processing, HDR intent, capability reporting, and contrast audits.

## Services and security

- Version IPC messages and document compatibility expectations.
- Narrow service privileges and validate socket ownership and permissions.
- Expand permission policy for automation, capture, files, and model providers.
- [x] Define retention and deletion behavior for logs, clipboard history, AI
  memory, and permission records.

## Longer-term exploration

- Permissioned local automation and optional AI-assisted workflows.
- Additional capture consumers and remote desktop support. The detailed design
  is tracked in [Remote Desktop Roadmap](docs/remote-desktop-roadmap.md). A
  loopback-only, single-output, view-only Phase 3 service is implemented;
  efficient transport, input, clipboard, production authentication, settings,
  indicators, and multi-monitor support remain planned.
- Broader protocol, distribution, hardware, and application compatibility.

## Release readiness

An alpha release should have a tagged revision, reproducible build instructions,
documented known issues, passing CI, tested install and uninstall steps, and a
short changelog. Beta criteria will be defined only after the core compositor,
session, configuration, and recovery paths are stable enough for broader use.
