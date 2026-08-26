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
- Consolidate overlapping `settings.json` and `config.toml` configuration paths.
- Keep AI and automation actions explicit, permission-gated, and auditable.
- Establish repeatable alpha releases and installation verification.
- Clear the existing Clippy warning backlog, then promote warnings to CI errors.

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
- [x] Add deterministic provider-contract and agent-loop integration tests.

## Desktop experience

- Expand the completed workspace-slot controls with overview thumbnails and
  animated transitions.
- Expand the completed keybinding editor with shortcut capture, conflict
  feedback, and configurable pointer gestures.
- Mature the launcher, Settings application, file manager, notifications, power
  handling, lock screen, and accessibility behavior.
- Improve first-run setup, recovery, and uninstall workflows.

## Theme editor phases

The editor authoring phases below are implemented. Compositor-native gradient
rendering remains outstanding: gradient sources round-trip through TOML and
packages and render in the editor, but the compositor currently samples a
representative color. See the [Theme Editor guide](docs/theme-editor.md) for the
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
- Define retention and deletion behavior for logs, clipboard history, AI memory,
  and permission records.

## Longer-term exploration

- Permissioned local automation and optional AI-assisted workflows.
- Additional capture consumers and remote desktop support. The detailed design
  is tracked in [Remote Desktop Roadmap](docs/remote-desktop-roadmap.md); it is
  not implemented functionality.
- Broader protocol, distribution, hardware, and application compatibility.

## Release readiness

An alpha release should have a tagged revision, reproducible build instructions,
documented known issues, passing CI, tested install and uninstall steps, and a
short changelog. Beta criteria will be defined only after the core compositor,
session, configuration, and recovery paths are stable enough for broader use.
