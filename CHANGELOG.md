# Changelog

Notable user-visible changes will be recorded in this file. FocalDesk is alpha
software and does not yet promise a stable API or configuration format.

The format is based on Keep a Changelog, and future releases should use semantic
version numbers where practical.

## Unreleased

### Added

- Added a selectable raw-Ash Vulkan DRM renderer with GBM DMA-BUF scanout,
  explicit synchronization, multi-output layout and scaling, XWayland,
  damage-aware retained FP16 composition, SDR/ICC color management, guarded
  HDR10/PQ output, hardware-cursor fallback, capture readback, and bounded GPU
  and KMS recovery. Hardware validation on Fedora 44/NVIDIA 595 confirmed mixed
  HDR/SDR output, sRGB and Display-P3 images, video, and compositor egui panels.
- Raw Vulkan now verifies live HDR connector state periodically and atomically
  re-arms BT.2020 signaling, link depth, and static metadata after a monitor
  picture-mode change or DisplayPort link retrain silently drops them.
- Raw Vulkan now completes live HDR-to-SDR connector transitions before
  logout, suspend, restart, and shutdown. A bounded fallback executes the
  requested session action even if the driver never reports a transition
  vblank, preventing the dark shutdown screen from hanging indefinitely.
- Changing the Focaldesk output gamut or ICC profile now updates the color
  transform in place instead of rebuilding KMS scanout and disturbing HDR.
- Raw Vulkan screenshots now support packed 10-bit HDR scanout formats and a
  failed one-shot capture no longer retries GPU readback on every frame.
- Added manifest-driven installation verification and a scoped uninstaller that
  preserves user data by default, plus a repeatable install-lifecycle test.
- Added automatic, atomic migration from legacy `config.toml` desktop settings
  into `settings.json` while retaining the legacy source as a recovery copy.
- Added a maintained alpha known-issues document.
- Added a tag-driven, prerelease packaging workflow and a multi-client nested
  compatibility matrix with per-client diagnostic artifacts.
- Added 30-day clipboard-history pruning, bounded fallback-log rotation, and
  documented retention/deletion semantics for permission records.
- Added remote frame latency, drop, damage-area, refresh, and queue-depth
  telemetry with bounded periodic reporting.
- Added compositor-native linear and radial theme paint with up to eight stops,
  authored geometry, wide-gamut/HDR values, and alpha interpolation modes.
- Added animated workspace overview cards with normalized live window-layout
  thumbnails and window summaries.
- Added compacted compositor-damage metadata to the versioned remote-capture
  protocol and damage-region RDP bitmap updates, with bounded queues, idle-frame
  suppression, and full-refresh recovery after skipped frames or resize.
- Added durable desktop-session restoration behind the existing Settings
  switch, with atomic checkpoints, trusted desktop-entry relaunches, workspace
  and output recovery, and Wayland/XWayland window-state placement.
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

- Raw Vulkan now bridges implicit DMA-BUF synchronization into Vulkan: each
  distinct client buffer's writer fence is imported as a temporary acquire
  semaphore, and the completed Vulkan read fence is published back to the
  reservation object. This prevents partially updated browser/video frames.
- PolicyKit cancellation now reaches the matching in-flight authentication
  session, and dialog IPC waits no longer block the GLib authentication loop.
- Settings can capture shortcuts directly from key presses and reports invalid
  or conflicting combinations before persisting them.
- The nested compatibility harness now probes an apparent host Wayland socket
  and falls back to private Weston when the socket is stale or non-responsive.
- Delayed compositor-native panel and dock fallbacks briefly during production
  session startup, preventing partially initialized chrome from flashing while
  the GTK system rail and task shelf start; crash fallback remains immediate.
- Display mode and refresh-rate rebuilds now restart the standalone GPU-rendered
  system rail and task shelf after recovered scanout, preventing stale output
  bindings from leaving either client visible but unresponsive.
- Restricted the root PAM session hook's native credential-broker access to a
  non-secret readiness ping; all secret operations remain limited to same-UID
  peers and their configured ACL grants.
- Upgraded the PolicyKit D-Bus binding to `zbus_polkit` 5.1.0, closing the
  PID-reuse authorization bypass reported as RUSTSEC-2026-0278.
- DRM suspend/resume now uses explicit lifecycle states, abandons stale flip
  bookkeeping, waits for libseat activation, resets connectors and planes,
  reprobes resources, invalidates GPU caches, and forces a complete modeset with
  timestamped recovery diagnostics. Deferred NVIDIA udev notifications no
  longer destroy the retained DRM device during resume, and FocalDesk no longer
  overrides the platform's firmware-backed deep-sleep policy with `s2idle`.
  Firmware-backed resume also recreates the EGL/GL renderer instead of reusing
  a context that NVIDIA may have invalidated while asleep. NVIDIA's recreated
  renderers use implicit KMS synchronization in every session, avoiding the
  driver regression that rejects otherwise-valid `IN_FENCE_FD` plane fences
  for the rest of the boot. The external GTK rail and dock are restarted after
  the first recovered page flip so they cannot retain partial GPU caches.
- Extended Wayland color-management negotiation and HDR calibration controls,
  including RGB color-representation advertising, per-output client metadata,
  live KMS metadata refresh, and session-only calibration patterns.
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
- Suspend and resume now release compositor keyboard state that libinput can no
  longer report, preventing a stale modifier from disabling lock-screen text
  entry after repeated sleep cycles.
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
- Reduced steady-state compositor overhead by reusing unchanged per-output
  chrome layouts, preparing static shell glyphs only when their inputs change,
  and avoiding message formatting for disabled log levels. Release builds now
  use thin link-time optimization with a single code-generation unit.

## Historical tags

The repository contains the earlier `v0.1-gdm-session` tag. It predates this
changelog; consult the tagged source for its exact contents.
