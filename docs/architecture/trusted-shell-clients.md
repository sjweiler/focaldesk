# Trusted shell clients

FocalDesk is migrating the compositor-rendered topbar and sidebar into two
Wayland layer-shell clients:

- `focal-panel` (`focal-panel` namespace): top-anchored, exclusive top zone.
- `focal-dock` (`focal-dock` namespace): left-anchored, exclusive left zone.

Each client now creates one layer surface per advertised output and adds/removes
surfaces as outputs are hotplugged.

The compositor still renders the legacy chrome until the clients provide the
full action, accessibility, and theme surfaces. The clients in this first slice
are therefore safe to run independently: they exercise configure, damage,
restart, and reservation behavior without changing the default session.

Only these namespaces contribute to FocalDesk's internal work-area calculation;
other layer-shell clients continue to render through the normal Smithay layer
map but cannot move normal-window placement. Namespace filtering is a protocol
boundary, not a cryptographic identity mechanism. Before enabling these clients
by default, session startup should launch them from the trusted user service
and the compositor should additionally validate the connecting client's UID and
executable or a launch token.

The next migration step is moving the existing `UiTree` action model and
theme-derived drawing into the clients, then disabling the corresponding legacy
compositor chrome only after the client has committed its first frame.

The panel client now proves the first part of that boundary: it polls the
existing desktop snapshot IPC, paints a small client-owned workspace indicator,
and sends pointer activation through the existing typed desktop-action IPC.
Those actions are intentionally conservative until the complete topbar model
has moved across.
