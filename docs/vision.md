# FlowOS Vision

## What FocalDesk is
FlowOS is a retro-futuristic, aerospace-inspired desktop environment built around a custom Wayland compositor and a cohesive “operating system experience” layer. The goal is a responsive, keyboard-first UI that remains visually distinctive while staying technically solid (latency, stability, security).

## Principles
- **Fast by default:** predictable frame pacing, low input latency, minimal jank.
- **Compositor-first design:** the UI model is rooted in Wayland primitives (outputs, surfaces, input).
- **Aesthetic with purpose:** a 1970s aerospace control-panel vibe that improves readability and orientation.
- **Sane defaults, deep customization:** great out-of-box behavior, but scriptable and configurable.
- **Safety boundaries:** automation and “AI features” must be permissioned and observable.

## Target users
- Developers who want a distraction-minimized, high-velocity workflow
- Power users who prefer keyboard navigation and structured workspaces
- Builders who enjoy a coherent theme and “instrument panel” UI metaphor

## Core experience
- **Multi-monitor aware**: stable handling of hotplug, scaling, and layout.
- **Structured workspace model**: named workspaces, predictable focus, fast switching.
- **System/navigation/context bars**: layered information density without clutter.
- **Rules + profiles**: window rules, app grouping, per-workspace behaviors.
- **Diagnostics**: “what is the compositor doing?” introspection that helps debugging.

## Non-goals (for v0.x)
- Being a general-purpose distro
- Replacing every desktop feature immediately (e.g., full settings panel suite)
- Shipping “AI that acts on your behalf” without strong guardrails

## Milestones

- **v0.1**: compositor boots reliably, renders clients, basic input and focus, single-output (known-good baseline).
- **v0.2**: workspace model, top bar + side navigation, basic layout policy, config file, crash-safe logging.
- **v0.3**: multi-monitor support, rules engine, IPC, dev tooling, repeatable releases.
- **v0.4+**: optional automation / AI layer behind explicit permission and audit logs.


