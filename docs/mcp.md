# FocalDesk MCP

`focaldesk-mcp` is a security and translation layer for Model Context Protocol
clients. It is not an alternative desktop API. The compositor and session
services continue to own state and behavior through the typed, versioned IPC
contracts in `focaldesk-ipc`.

```text
MCP client
    │ JSON-RPC over stdio
    ▼
focaldesk-mcp
    │ tool policy, capability checks, confirmation, audit
    ▼
authenticated FocalDesk IPC
    │
    ├── compositor snapshot and window actions
    └── notification service
```

The initial catalog is deliberately read-heavy:

- `get_session_status`
- `list_outputs`
- `get_output_details`
- `list_windows`
- `list_workspaces`
- `get_service_health`
- `search_recent_logs`
- `get_rendering_status`
- `show_notification`
- `focus_window`
- `move_window_to_workspace`
- `open_settings_panel`

Window lists contain metadata only. They do not contain screenshots, window
contents, clipboard contents, keystrokes, or client protocol payloads. Log
search is bounded to 200 results, truncates long lines, and redacts common
secret-shaped fields. Desktop snapshots are capped at 32 outputs, 64
workspaces, and 256 windows; client-controlled metadata fields are truncated.
Service health currently means that the private session endpoint exists; it
does not claim that every internal subsystem is healthy.

## Policy and authorization

Every tool publishes `focaldesk/toolPolicy` metadata in its MCP `_meta`:

```rust
ToolPolicy {
    access: AccessLevel::Session,
    mutability: Mutability::ReadOnly,
    confirmation: Confirmation::None,
    data_class: DataClass::SystemMetadata,
    audit: AuditMode::Full,
}
```

Read-only tools are available to a client that can start `focaldesk-mcp` in the
user session. Mutating tools are denied by default. Grant only the exact tools
needed by the client:

```sh
FOCALDESK_MCP_CAPABILITIES=show_notification,focus_window focaldesk-mcp
```

`focus_window`, `move_window_to_workspace`, and `open_settings_panel` also
require `confirmed: true` in that specific call. A client must set it only after
obtaining approval for the concrete action; it is not a persistent consent
flag.

Each attempted call writes one structured audit event to stderr. Events include
the tool, declared policy, authorization decision, duration, result status, and
parameter names. Parameter values and tool results are intentionally excluded.
Stdout is reserved for MCP JSON-RPC messages.

## Client configuration

Build the server:

```sh
cargo build -p focaldesk-mcp
```

A typical stdio client entry is:

```json
{
  "mcpServers": {
    "focaldesk": {
      "command": "/path/to/focaldesk/target/debug/focaldesk-mcp",
      "env": {
        "FOCALDESK_MCP_CAPABILITIES": "show_notification"
      }
    }
  }
}
```

The server implements MCP initialization, ping, tool listing, and tool calls
using newline-delimited JSON-RPC over stdio. Messages over 1 MiB are rejected.

## Secrets boundary

`focald-secrets` must never return plaintext secrets through MCP. There is no
secret or credential retrieval tool in this catalog, and `focaldesk-mcp` does
not depend on `focaldesk-secrets-client`.

Future secret-related tools, if any, must describe a narrow operation such as
signing, unlocking a named capability, or creating a short-lived opaque handle.
They must not expose arbitrary credential values, decrypted database fields, or
Secret Service item contents.

Power, session termination, display reconfiguration, file modification,
service restarts, and credential operations are intentionally outside this
initial catalog. Adding any of them requires a separately reviewed policy,
authorization, confirmation, and audit design.
