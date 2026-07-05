
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

## Rendering

Only windows belonging to the active workspace on each output are composited into the scene.

Inactive workspaces remain in memory but are not rendered.

This minimizes rendering work while allowing fast workspace switching.

## Focus Management

Each workspace maintains independent focus history.

Switching back to a workspace restores the previously focused window whenever possible.

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

## Future Enhancements

- Workspace overview
- Drag windows between workspaces
- Workspace thumbnails
- Animated transitions
- Dynamic workspace creation
- Named workspaces
- Per-workspace wallpapers
- Keyboard shortcuts
- Multi-monitor synchronization options

## Design Principles

- Keep workspace state isolated.
- Avoid unnecessary rendering.
- Preserve user context.
- Support independent monitor workflows.
- Make future extensions straightforward.

