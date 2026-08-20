

# IPC Design

FocalDesk uses IPC to separate the compositor from supporting desktop services.

The goal is to keep the compositor focused on display, input, surfaces, rendering, and session behavior while moving process launching, power management, notifications, dialog brokering, automation, and future AI-assisted actions into separate services.

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
        ├── focal-launch-shared
        ├── focal-launchd
        ├── focaldesk-powerd
        ├── focaldesk-notificationsd
        ├── focaldesk-updatesd
        ├── focaldesk-dialogd
        └── focaldesk-controlsd

Services
- launch requests
- power snapshot / suspend / hibernate / reboot
- notifications queue / visibility
- system update checks / install requests
- AI permission prompts
- portal chooser prompts
- wifi, bluetooth, and audio controls
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

### focaldesk-powerd

`focaldesk-powerd` owns power snapshot collection and system power actions.

Responsibilities may include:

- Reading battery and AC status
- Reporting power snapshots to the compositor and settings UI
- Executing suspend, hibernate, reboot, and power-off requests
- Applying performance profiles

### focaldesk-notificationsd

`focaldesk-notificationsd` owns notification queueing and visibility state.

Responsibilities may include:

- Accepting notification requests
- Expiring timed notifications
- Returning visible notification snapshots to the compositor

### focaldesk-updatesd

`focaldesk-updatesd` owns package-update discovery and install jobs so the
compositor never runs PackageKit or DNF on the render thread.

Responsibilities may include:

- Checking for updates on a background worker
- Caching package name, version, and description
- Installing selected or all updates via PackageKit or `pkexec dnf`
- Posting a notification when new updates appear

### focaldesk-dialogd

`focaldesk-dialogd` owns permission-style dialogs that need a human response.

Responsibilities may include:

- Showing AI permission prompts
- Showing portal chooser prompts
- Returning typed allow/deny or selection responses

### focaldesk-controlsd

`focaldesk-controlsd` owns quick system controls that shell out to helper tools.

Responsibilities include:

- Toggling Wi-Fi
- Toggling Bluetooth
- Setting default output volume

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
    ├── Control service
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

FocalDesk service sockets are private to the current user and use Linux peer
credentials to authenticate each connection. Sensitive endpoints then apply a
deny-by-default caller policy using the peer executable and systemd cgroup
unit. For example, power requests are accepted only from the compositor and
Settings, while password-capable dialog requests are accepted only from the
PolicyKit agent, portal, and AI service.

This process identity boundary protects services from unrelated applications
in the same desktop session. It does not treat the user's own writable binaries
as a security boundary. Release builds accept executable-name grants only when
the resolved executable is root-owned and not group- or world-writable;
systemd-unit grants remain available to packaged user services. Debug builds
allow user-owned executables for repository development. The
`FOCALDESK_ALLOW_USER_OWNED_IPC_PEERS` escape hatch restores that development
behavior explicitly and must not be set in a production session.

AI and automation features must not have unrestricted control over the desktop.

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

## Wire Contract

Typed JSON requests and responses use a versioned envelope:

```json
{
  "protocol_version": 1,
  "payload": {
    "type": "GetSnapshot"
  }
}
```

Version 1 is the only supported version. Missing or unsupported versions are
rejected explicitly rather than being interpreted as a different request
shape. Requests are limited to 1 MiB and ordinary blocking connections use
five-second read and write timeouts.

Sockets normally live below `$XDG_RUNTIME_DIR/focaldesk`. The directory is
required to be owned by the current user with mode `0700`; sockets use mode
`0600`. FocalDesk refuses to replace a non-socket, symlinked runtime directory,
or foreign-owned socket path.

## Open Questions

- Should IPC use Unix domain sockets, D-Bus, or another transport?
- Should AI actions require confirmation by default?
- How should service crashes be handled?
- How should logs be correlated across services?

## Summary

The IPC layer is intended to make FocalDesk modular, recoverable, and extensible. The compositor should remain focused on core desktop behavior while services handle launching, settings, automation, and future AI workflows through explicit message contracts.
