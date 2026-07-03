

# IPC Design

FocalDesk uses IPC to separate the compositor from supporting desktop services.

The goal is to keep the compositor focused on display, input, surfaces, rendering, and session behavior while moving process launching, automation, and future AI-assisted actions into separate services.

## Goals

- Keep process launching out of the compositor
- Provide a clear boundary between shell UI and desktop services
- Support recoverable services
- Enable future permission checks
- Enable future AI and voice workflows
- Avoid turning the compositor into one large monolithic process

## Current Components

```text
FocalDesk Compositor / Shell UI
        │
        ▼
focal-launch-shared
        │
        ▼
focal-launchd
        │
        ▼
Applications
```

## Components

### FocalDesk Compositor

The compositor sends requests when the user performs desktop actions such as launching an application.

It should not directly own long-running process management logic.

### focal-launchd

`focal-launchd` is the launcher daemon. It receives launch requests and starts applications outside the compositor process.

Responsibilities may include:

- Starting applications
- Preparing environment variables
- Returning success or failure status
- Logging launch attempts
- Future permission validation
- Future app metadata handling

### focal-launch-shared

`focal-launch-shared` contains shared IPC message types used by both the compositor and launcher daemon.

This prevents duplicated request/response definitions.

## Message Flow

```text
User clicks launcher item
        │
        ▼
Shell UI creates launch request
        │
        ▼
IPC message sent to focal-launchd
        │
        ▼
focal-launchd validates request
        │
        ▼
focal-launchd starts process
        │
        ▼
Response sent back to compositor
```

## Example Message Types

```rust
pub enum LaunchRequest {
    LaunchCommand {
        command: String,
        args: Vec<String>,
        working_dir: Option<String>,
    },
    LaunchDesktopFile {
        desktop_file: String,
    },
}

pub enum LaunchResponse {
    Started {
        pid: u32,
    },
    Failed {
        reason: String,
    },
}
```

## Future IPC Use Cases

```text
Compositor
    │
    ├── Launcher service
    ├── Settings service
    ├── AI assistant service
    ├── Voice command service
    ├── Plugin service
    └── Session manager
```

Future IPC may support:

- Launching applications
- Querying open windows
- Switching workspaces
- Taking screenshots
- Locking the session
- Adjusting settings
- AI-triggered desktop actions
- Voice commands
- Permission-gated automation

## Permission Model

Future AI and automation features should not have unrestricted control over the desktop.

Possible permission levels:

```text
Read-only:
- Query windows
- Query workspace state
- Query settings

User-approved:
- Launch applications
- Change volume
- Switch workspace
- Take screenshot

Restricted:
- Run shell commands
- Modify files
- Change security settings
- Close applications
- Power off / reboot
```

## Design Principles

- Keep messages explicit
- Prefer typed request/response structures
- Log failed requests
- Do not block the compositor on long-running work
- Keep the compositor stable if a service crashes
- Treat AI actions as untrusted until approved
- Keep IPC contracts documented and versioned

## Open Questions

- Should IPC use Unix domain sockets, D-Bus, or another transport?
- Should services have separate permissions?
- Should messages be versioned?
- Should AI actions require confirmation by default?
- How should service crashes be handled?
- How should logs be correlated across services?

## Summary

The IPC layer is intended to make FocalDesk modular, recoverable, and extensible. The compositor should remain focused on core desktop behavior while services handle launching, settings, automation, and future AI workflows through explicit message contracts.
