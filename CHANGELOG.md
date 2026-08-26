# Changelog

Notable user-visible changes will be recorded in this file. FocalDesk is alpha
software and does not yet promise a stable API or configuration format.

The format is based on Keep a Changelog, and future releases should use semantic
version numbers where practical.

## Unreleased

### Added

- Added a versioned Theme Editor with sRGB and Display P3 paint authoring,
  solid and gradient sources, SDR/HDR intent, semantic interaction states,
  layout and typography controls, wallpaper processing, contrast audits,
  debounced compositor preview/apply/revert IPC, TOML save/load, and validated
  portable `.fdtheme` packages.
- Added an installable system-default theme and wallpaper, used when no
  explicit built-in theme is selected, with a safe Eagle fallback.
- Added per-display HDR appearance tuning in Display Settings with bounded
  reference-white, highlight, saturation, and midtone controls, live
  shader-only preview, conservative presets, automatic 15-second rollback, and
  neutral reset values.
- Added a session-only absolute-nits HDR output calibration pattern for checking
  neutral levels, equal-luminance BT.2020 primaries, peak output, and banding.
- Decode BT.1886-tagged client surfaces with a distinct 2.4-power transfer
  instead of treating them as piecewise sRGB.
- Added bounded transient AI-provider retries and in-memory provider health
  telemetry for latency, outcomes, cancellations, traffic, and reported token
  usage, surfaced in the AI Console Providers page.
- Added transactional AI-memory schema migration, default 90-day/10,000-record
  lifecycle limits, automatic pruning, lifecycle status, and fresh-confirmed
  individual or bulk deletion from the AI Console.
- Added deterministic, network-independent wire-contract tests for OpenAI,
  Anthropic, Ollama, and vLLM providers plus bounded agent-loop integration
  coverage for synthesis, failures, mutation proposals, expiry, and replay.
- Project status matrix and annotated architecture documentation.
- Build, configuration, keybinding, troubleshooting, and roadmap documentation.
- Contribution and issue-reporting templates.
- Added semantic accessibility metadata and keyboard focus navigation for
  compositor-owned shell controls, including visible focus indicators.
- Added installable Fedora packaging for the native display manager, greeter
  account, configuration, systemd unit, and separate greeter PAM policy.
- Added regression coverage for bounded clipboard capture and private AI,
  clipboard-history, and memory state files.
- Completed workspace-slot shortcuts and overflow selection, including
  per-monitor switching, focused-window assignment, and focus restoration.
- Added runtime-reloadable compositor shortcut overrides with per-entry
  validation, conflict protection, Settings controls, and workspace actions.
- Added an isolated nested compositor compatibility harness with headless Weston
  fallback, Wayland registry checks, native-client survival, XWayland readiness,
  crash detection, CI coverage, and captured diagnostics.
- Added lifecycle-safe precise damage propagation for Wayland toplevel, popup,
  layer-shell, and synchronized subsurface trees, with transform, viewport,
  fractional-scale, destruction, effectiveness, and commit-storm coverage.
- Added file and folder favorites shared between Files and Launcher, including
  live updates, unavailable-item feedback, XWayland-aware Windows executable
  launches, and private atomic state updates that preserve concurrent changes.
- Added bounded, redacted `focaldesk-cli diagnostics` archives containing
  system, display, GPU, service, journal, log, and latest-crash context, plus
  atomic owner-only panic reports from the shared logging layer.

### Changed

- NVIDIA multi-output HDR permission now preserves each monitor's independent
  HDR/SDR request instead of promoting every capable sibling to HDR10.
- Documented a successful Fedora 44 and NVIDIA 595-series HDR10 configuration,
  including Google Chrome, while retaining the experimental NVIDIA safeguards.
- Clarified experimental and planned feature claims, including subsurface
  damage tracking, HDR, capture, AI, and automation.
- Provisioned the pinned native Vosk library in CI so workspace tests can link
  voice-enabled applications on Ubuntu runners.
- Moved desktop-service IPC sockets into a private per-user runtime directory,
  restricted socket permissions, authenticated peer users and processes,
  enforced endpoint-specific caller policies, added versioned message
  envelopes, and bounded request sizes.
- Locked sessions now remain locked across every suspend/resume path; PAM
  authentication runs outside the compositor loop and password buffers are
  scrubbed after use.
- Clipboard capture is limited to one in-flight request, one MiB, and two
  seconds. Clipboard history and AI state use owner-only permissions and atomic
  replacement where applicable.
- Automation is opt-in instead of part of the default service bundle, and its
  service runs with a restrictive systemd sandbox.
- Release IPC executable grants now require root-owned, non-writable installed
  binaries unless the explicit development escape hatch is enabled.
- Nested test runs can suppress portal and service environment publication so
  their private Wayland socket does not replace the host session environment.
- Disabled unused image codecs, removing the yanked `core2` dependency and
  reducing the RustSec warning set.
- Indexed damage state per surface-tree root, reused traversal storage, reduced
  client-damage expansion to a one-pixel rounding guard, and made rectangle
  compaction transitive without overlap-inflated full-frame decisions.

## Historical tags

The repository contains the earlier `v0.1-gdm-session` tag. It predates this
changelog; consult the tagged source for its exact contents.
