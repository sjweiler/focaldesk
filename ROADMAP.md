# FocalDesk Roadmap

This roadmap communicates direction rather than a release promise. Priorities
may change as compositor correctness, hardware behavior, and security findings
develop. The [README status table](README.md#project-status) is the source of
truth for what is usable today.

## Current stabilization priorities

- Improve compositor crash recovery, diagnostics, and reproducible bug reports.
- Exercise multi-monitor hotplug, scale, transform, and mixed-refresh behavior.
- Harden XWayland, portal capture, session startup, and user-service lifecycle.
- Consolidate overlapping `settings.json` and `config.toml` configuration paths.
- Keep AI and automation actions explicit, permission-gated, and auditable.
- Establish repeatable alpha releases and installation verification.
- Clear the existing Clippy warning backlog, then promote warnings to CI errors.

## Rendering and display work

- Add precise Wayland subsurface damage propagation.
- Reduce full-output damage fallbacks and document damage metrics.
- Continue HDR, wide-gamut, ICC, and SDR-composition validation.
- Expand hardware-cursor, direct-scanout, multi-GPU, and presentation testing.

## Desktop experience

- Complete workspace-slot activation, assignment, and overflow behavior.
- Make keybindings configurable and keep Settings hints synchronized with code.
- Mature the launcher, Settings application, file manager, notifications, power
  handling, lock screen, and accessibility behavior.
- Improve first-run setup, recovery, and uninstall workflows.

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
