
# Workspace Management

## Overview

FocalDesk provides virtual workspaces to help users organize applications and improve productivity.

Each workspace contains its own collection of windows while sharing the same desktop session.

## Goals

- Fast workspace switching
- Low rendering overhead
- Per-monitor workspace support
- Predictable window placement
- Future animation support

## Current Implementation

Each workspace maintains:

- Window list
- Focus history
- Active window
- Layout state
- Rendering state

Only the active workspace for an output is rendered.

## High-Level Architecture

```text
Monitor 1
    │
    ├── Workspace 1
    ├── Workspace 2
    ├── Workspace 3
    └── Workspace 4

Monitor 2
    │
    ├── Workspace A
    ├── Workspace B
    ├── Workspace C
    └── Workspace D
```

Each monitor maintains its own active workspace.

## Window Lifecycle

```text
Application Launch
        │
        ▼
Assigned Workspace
        │
        ▼
Window Created
        │
        ▼
Receives Focus
        │
        ▼
Rendered if Workspace Active
```

## Switching Workspaces

When the user switches workspaces:

1. Save focus state for the current workspace.
2. Change the active workspace.
3. Restore focus in the new workspace.
4. Schedule a repaint.
5. Update shell UI.

The default shortcuts are `Alt+1` through `Alt+9`. `Alt+Shift+1` through
`Alt+Shift+9` move the focused window and follow it to the destination.
`Alt+0` opens a selectable list containing every workspace, including entries
that do not fit in the sidebar.

## Rendering

Only windows belonging to the active workspace on each output are composited into the scene.

Inactive workspaces remain in memory but are not rendered.

This minimizes rendering work while allowing fast workspace switching.

## Focus Management

Each workspace maintains independent focus history.

Switching back to a workspace restores the previously focused window whenever possible.

## Restoring a Login Session

When **Settings → Workspaces → Restore session** is enabled, the compositor
checkpoints workspace names and restorable top-level windows to
`$XDG_STATE_HOME/focaldesk/session.json`. On the next login it launches matching
installed desktop entries and restores their workspace, output, geometry,
minimized state, maximized/fullscreen state, and focus. Geometry is stored
relative to the output and clamped to the available work area if the display
layout changed.

Transient dialogs, menus, override-redirect X11 windows, and windows without a
stable Wayland app ID or X11 class are not restored. Applications remain
responsible for their internal state, such as open documents, browser tabs, and
terminal processes. Disabling **Restore session** removes the saved snapshot.

**Restore split layouts** is a separate, disabled-by-default option. When both
session restore and split screen are enabled, it saves pane assignments,
workspace, display connector, floating restore geometry, and normalized divider
positions. Returning applications reclaim their panes independently, so startup
does not wait for a missing application. Layouts are reflowed against the
display's current logical work area and scaling; if the original display is
missing or the panes no longer meet minimum sizes, those windows return as
floating windows instead.

## Multi-Monitor Behavior

Each monitor can display a different workspace.

Example:

```text
Left Monitor
Workspace 2

Right Monitor
Workspace 5
```

Changing the workspace on one monitor does not necessarily affect the other.

## Optional Split Screen

Split screen is disabled by default. Enable it under **Settings → Workspaces →
Enable split screen**. `Super+Left` and `Super+Right` place supported windows in
side-by-side panes; `Super+Up` and `Super+Down` use top and bottom panes.
Repeating the active placement restores the window.

`Super+Z` opens a chooser for half, two-thirds/one-third, stacked, and quadrant
layouts. Hovering a preset shows its pane before applying it. Presets that
cannot meet the logical pane minimums are disabled, and the chooser is not
available while split screen is off.

After placing a window, **Split Assist** offers the other eligible windows on
that monitor and workspace as window cards. Selecting one fills the exact
remaining pane; `Escape` leaves it empty. Dialogs, minimized or fullscreen
windows, and windows whose declared minimum size will not fit are omitted.
Both the layout chooser and Split Assist support arrow-key selection, `Enter`
to apply, and `Escape` to cancel while retaining mouse selection.

Dragging a supported window into a 40-logical-pixel zone along a work-area
edge previews a half-screen target; corners preview quadrants. The translucent
preview disappears when the pointer leaves the zone, and dropping applies the
shown target. Thirds remain available through `Super+Z`.

For an occupied two-pane split, `Super+Shift+Arrow` swaps the focused window
with its neighbor in that direction. Right-clicking the shared divider opens
controls to swap panes, replace the focused pane through Split Assist, or exit
the group and restore both windows to their earlier floating geometry. Moving
either member to another workspace moves the group and retains its layout and
divider ratio.

When complementary panes are occupied on the same monitor and workspace,
FocalDesk draws a shared divider. Dragging it resizes both windows while
respecting their declared minimum sizes. Side-by-side panes require at least
800 logical pixels each, and stacked panes require at least 500 logical pixels
each. Unsupported layouts remain unavailable rather than relying on the
monitor's physical resolution.

Maximizing a split window temporarily fills the work area without discarding
its pane. Unmaximizing returns it to the same pane and restores the existing
divider ratio; the paired window remains assigned underneath. Pane geometry is
kept when switching away from and back to the workspace.

Moving or individually resizing a split window restores it to its prior
floating geometry. Turning split screen off restores all currently split
windows immediately.

## Future Enhancements

- Workspace overview
- Drag windows between workspaces
- Workspace thumbnails
- Animated transitions
- Per-workspace wallpapers
- Multi-monitor synchronization options

## Design Principles

- Keep workspace state isolated.
- Avoid unnecessary rendering.
- Preserve user context.
- Support independent monitor workflows.
- Make future extensions straightforward.
