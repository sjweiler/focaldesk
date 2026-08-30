# Replaceable GTK shell clients

FocalDesk's alternative shell is formed by independent GTK4 Wayland
layer-shell clients launched by the normal desktop session:

- `focaldesk-system-rail` (`focaldesk-system-rail` namespace) is a narrow,
  floating right-edge control rail. It owns system state, workspaces, clock,
  notifications, and power, and reserves 80 logical pixels at the right edge.
- `focaldesk-task-shelf` (`focaldesk-task-shelf` namespace) is a grouped,
  bottom-centered application shelf. It shows pinned, running, and utility
  groups and intentionally claims no exclusive zone.

The applications are separate processes from the compositor. Their primary
runtime is `focaldesk-shell-gtk`, which creates a GTK4 layer surface per output.
It consumes renderer-neutral theme tokens and has no dependency on
`focaldesk-ui`, `focaldesk-engine`, compositor renderer objects, shaders, or the
compositor's GPU lifetime.

The former independent GLES renderer remains available for legacy diagnostics
by setting `FOCALDESK_SHELL_FORCE_GLES=1`; it retains the old `focal-panel` and
`focal-dock` namespaces. The compositor retains its native chrome as a startup
fallback and stops drawing it once a renderable trusted shell surface claims
the work area.

Only the trusted panel and dock namespaces contribute to FocalDesk's internal
work-area calculation. Other layer-shell clients continue to render through
the normal Smithay layer map but cannot move normal-window placement.
Namespace filtering is a protocol boundary, not a cryptographic identity
mechanism. The session launches these clients from trusted user services and
the IPC transport validates the connecting UID and executable. Namespace
filtering is used for layout ownership, not as IPC authentication.

Both clients consume the existing desktop snapshot IPC for per-output window,
workspace, and shell state and send interactions through typed desktop-action
IPC. The compositor remains the authoritative shell service for window state,
workspaces, notifications, settings, secure prompts, and the lock screen;
neither GTK process reaches into Smithay internals.
