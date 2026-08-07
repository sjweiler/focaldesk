# Experimental external shell clients

FocalDesk's panel and dock are rendered by the compositor. Two independent
Wayland layer-shell clients remain available as an experimental alternative,
but they are not enabled or launched by the default session:

- `focal-panel` (`focal-panel` namespace) can own the top bar and its exclusive
  top-edge zone.
- `focal-dock` (`focal-dock` namespace) can own the vertical sidebar and its
  exclusive left-edge zone.

The applications are separate processes from the compositor. Their primary
runtime is `focaldesk-shell-client`, which creates a layer surface per output
and owns its Wayland connection, EGL display, GLES context, render loop, and
GPU resources. The shell crate owns copies of the top-bar/sidebar layout,
shader sources, SVG icon-atlas builder, theme translation, and IBM Plex font
atlas. It has no dependency on `focaldesk-ui` or `focaldesk-engine`, does not
borrow compositor renderer objects, and does not share their GPU lifetime.

`focaldesk-shell-gtk` is the reliability fallback. If EGL/GLES initialization,
resource compilation, or frame presentation fails, the client tears down the
GLES runtime and starts the corresponding GTK4 layer-shell UI. Set
`FOCALDESK_SHELL_FORCE_GTK=1` to select that fallback explicitly for diagnosis
or for systems without a working GLES Wayland path.

Only the trusted panel and dock namespaces contribute to FocalDesk's internal
work-area calculation. Other layer-shell clients continue to render through
the normal Smithay layer map but cannot move normal-window placement.
Namespace filtering is a protocol boundary, not a cryptographic identity
mechanism. Experimental deployments should launch these clients from trusted
user services and additionally validate the connecting UID and executable or
a launch token before relying on the namespace as identity.

Both renderers consume the existing desktop snapshot IPC for workspace and
shell state and send interactions through typed desktop-action IPC. The
compositor remains the authority for workspaces, window state, notifications,
and settings; neither the GLES nor GTK shell renderer contains compositor
state.
