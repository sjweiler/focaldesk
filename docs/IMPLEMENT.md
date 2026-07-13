# Native Remote Desktop Implementation

## Status

This document is an implementation roadmap, not a description of completed functionality. Items marked with checkboxes remain to be implemented unless noted otherwise. Decisions listed under **Open Decisions** must be resolved before their corresponding phase begins.

## Goal

Provide a compositor-integrated remote desktop service for FocalDesk. A remote client should be able to view an authorized desktop session and, when permitted, provide input, synchronize the clipboard, and use one or more monitors.

The compositor remains responsible for rendering, output topology, damage tracking, input routing, Wayland selections, and session policy. A separate, tightly scoped `focaldesk-remoted` process handles untrusted network traffic, TLS, remote protocol parsing, authentication coordination, and encoding. This still presents one native FocalDesk feature to the user while keeping network-facing code out of the compositor process.

## MVP Scope

The first usable milestone intentionally has a narrow scope:

- One connected client.
- One selected physical output.
- Mirror the currently active graphical session.
- View-only operation.
- Localhost-only listener.
- Development authentication token.
- Shared-memory frame transfer.
- Full-frame updates before damage-only updates are enabled.

## Non-Goals for the MVP

- Headless or independent remote login sessions.
- Virtual outputs.
- Multiple simultaneous controllers.
- Public internet exposure.
- File transfer.
- Audio redirection.
- USB or device redirection.
- Hardware video encoding.
- Mobile-specific clients.

## Architecture

```text
Remote client
    |  RDP over TLS
    v
focaldesk-remoted
    - network listener
    - protocol and authentication
    - frame encoding and congestion control
    - clipboard channel
    |  versioned Unix-socket IPC
    |  shared memory initially, DMA-BUF later
    v
RemoteSessionManager in focaldesk-engine
    - authorization and session lifecycle
    - output capture and damage
    - input injection
    - clipboard integration
    - output topology
```

### Process boundary

`focaldesk-remoted` must not receive direct access to arbitrary compositor state. Communication uses a narrow, versioned IPC protocol over a Unix-domain socket. The compositor validates every request and remains the final authority for capture, input, clipboard, and lock-screen policy.

The network-facing service must never block the compositor event loop. Encoding, socket writes, authentication work, and GPU readback run outside that loop. Slow clients are handled by dropping obsolete frames rather than building an unbounded queue.

### Proposed source layout

```text
crates/focaldesk-remote-protocol/
    Cargo.toml
    src/lib.rs

crates/focaldesk-engine/src/core/capture/
    mod.rs
    broker.rs
    frame.rs

crates/focaldesk-engine/src/core/remote/
    mod.rs
    session.rs
    input.rs
    clipboard.rs
    topology.rs

services/focaldesk-remoted/
    Cargo.toml
    src/main.rs
    src/server.rs
    src/auth.rs
    src/encoder.rs
    src/compositor.rs

packaging/systemd/user/focaldesk-remoted.service
```

## Existing Integration Points

The implementation should extend existing FocalDesk mechanisms instead of creating parallel ones:

- `core/wayland/portal_capture.rs` already exposes output capture through Smithay's image-copy-capture protocols.
- `core/portal.rs` already renders portal output frames to textures and DMA-BUFs.
- `core/backend_render.rs` already clips and compacts per-output damage.
- `core/input.rs` defines backend-independent `FlowInputEvent` values.
- `DesktopState::handle_input` already provides the common input-routing path.
- `core/wayland/data_device.rs` already observes and installs Wayland clipboard selections.
- The output store and topology code already track logical location, physical size, scale, and transform.

Portal screen sharing and remote desktop should become consumers of one trusted output-capture broker. They should not maintain separate rendering implementations.

## IPC Model

The exact serialization format is an open decision, but the protocol should express messages equivalent to the following:

```rust
pub enum RemoteRequest {
    StartCapture { output: OutputId },
    StopCapture { session: RemoteSessionId },
    InjectInput {
        session: RemoteSessionId,
        event: RemoteInputEvent,
    },
    SetClipboard {
        session: RemoteSessionId,
        generation: u64,
        mime_type: String,
        data: Vec<u8>,
    },
}

pub enum RemoteEvent {
    CaptureStarted {
        session: RemoteSessionId,
        width: u32,
        height: u32,
        scale: f64,
    },
    FrameReady {
        session: RemoteSessionId,
        frame_id: u64,
        damage: Vec<RemoteRect>,
        buffer: RemoteBuffer,
    },
    ClipboardChanged {
        generation: u64,
        mime_type: String,
        data: Vec<u8>,
    },
    OutputsChanged { outputs: Vec<RemoteOutput> },
    PermissionChanged { permissions: RemotePermissions },
    SessionEnded { reason: SessionEndReason },
}
```

All messages require explicit size limits. File descriptors and shared buffers must be validated before import. The service identity should be checked through Unix socket peer credentials.

## Implementation Phases

### Phase 1: Shared output-capture broker

- [ ] Add common capture frame, consumer, session, and error types.
- [ ] Add an `OutputCaptureBroker` owned by the compositor state.
- [ ] Register and remove capture consumers without leaking sessions or buffers.
- [ ] Extract reusable output-export logic from portal capture.
- [ ] Convert portal capture to use the shared broker.
- [ ] Preserve current portal behavior and DMA-BUF constraints.
- [ ] Associate each frame with an output, frame serial, timestamp, buffer, and damage list.
- [ ] Force full damage for a new consumer and after size, scale, or transform changes.
- [ ] Add bounded per-consumer queues and slow-consumer frame dropping.
- [ ] Add unit tests for registration, removal, full-frame recovery, and queue limits.

### Phase 2: Local frame transport

- [ ] Add `focaldesk-remote-protocol` with an explicit protocol version.
- [ ] Add a compositor-owned Unix-domain socket endpoint.
- [ ] Authenticate the service using peer credentials.
- [ ] Implement `StartCapture` and `StopCapture` for one output.
- [ ] Export frames through bounded shared-memory buffers.
- [ ] Deliver full frames to a local diagnostic client.
- [ ] Ensure the compositor never waits for the diagnostic client.
- [ ] Clean up buffers and capture state after malformed messages or disconnects.
- [ ] Add protocol round-trip, truncation, oversize-message, and disconnect tests.

### Phase 3: Remote service and view-only RDP

- [ ] Add `services/focaldesk-remoted` to the Cargo workspace.
- [ ] Add an initially disabled systemd user service.
- [ ] Select and document the RDP server library.
- [ ] Listen only on `127.0.0.1` by default.
- [ ] Allow one client and reject or queue additional clients explicitly.
- [ ] Use an expiring development token during this phase.
- [ ] Negotiate the selected output's dimensions and pixel format.
- [ ] Convert compositor frames to RDP bitmap updates.
- [ ] Implement clean startup, disconnect, reconnect, and compositor-restart behavior.
- [ ] Verify interoperability with at least two RDP clients where possible.

### Phase 4: Efficient damage and encoding

- [ ] Pass the renderer's actual compacted damage rectangles to capture consumers.
- [ ] Do not produce remote updates when an output has no damage.
- [ ] Send changed rectangles instead of full frames where supported.
- [ ] Force a complete refresh after dropped updates, resize, reconnect, or decoder loss.
- [ ] Keep no more than one or two pending frames per client.
- [ ] Collect frame latency, dropped-frame, damage-area, and queue-depth metrics.
- [ ] Add DMA-BUF transport from compositor to service.
- [ ] Investigate direct DMA-BUF import into the selected hardware encoder.
- [ ] Keep shared memory and software encoding as compatibility fallbacks.

### Phase 5: Remote input

- [ ] Add an input origin such as `Physical` or `Remote(RemoteSessionId)`.
- [ ] Translate remote events into the existing `FlowInputEvent` path.
- [ ] Convert absolute client coordinates into global logical compositor coordinates.
- [ ] Account for output location, fractional scale, and transform.
- [ ] Support pointer motion and buttons.
- [ ] Support vertical and horizontal scrolling.
- [ ] Support keyboard press, release, modifiers, and repeat semantics.
- [ ] Choose and document physical-keycode versus keysym translation behavior.
- [ ] Track pressed keys and buttons per session.
- [ ] Synthesize releases when a client disconnects or loses input permission.
- [ ] Make sessions view-only unless control permission is explicitly granted.
- [ ] Define how local input and remote input interact.
- [ ] Reject remote input while the session is locked unless a dedicated secure-login design permits it.
- [ ] Initially allow no more than one controlling session.

A dedicated Smithay virtual seat may eventually support independent concurrent input, but reusing the active seat is the smaller first implementation. This choice must be revisited before concurrent sessions are supported.

### Phase 6: Clipboard synchronization

- [ ] Add a remote clipboard selection owner to the Wayland data-device integration.
- [ ] Advertise and negotiate MIME types.
- [ ] Support UTF-8 plain text first.
- [ ] Send local clipboard changes only to sessions with clipboard permission.
- [ ] Install authorized remote clipboard content as the Wayland selection.
- [ ] Attach generation identifiers to prevent local-to-remote synchronization loops.
- [ ] Apply byte limits, transfer timeouts, and cancellation.
- [ ] Clear remote-owned clipboard state safely when its session disconnects.
- [ ] Add image MIME types only after text synchronization is reliable.
- [ ] Treat file transfer as a later, separately permissioned feature.

### Phase 7: Production authentication and authorization

- [ ] Replace the development token with PAM authentication, secure device pairing, or the chosen production model.
- [ ] Generate, store, rotate, and display TLS certificate identity safely.
- [ ] Require TLS for every network connection.
- [ ] Add authentication rate limiting and temporary lockout.
- [ ] Add expiring one-time pairing codes if pairing is supported.
- [ ] Require local confirmation according to settings and session state.
- [ ] Represent view, input, clipboard, and future file-transfer permissions separately.
- [ ] Revoke permissions immediately and propagate revocation to the service.
- [ ] End remote sessions when the local graphical session terminates.
- [ ] Record security-relevant session events without recording pixels, clipboard content, or keystrokes.
- [ ] Review the network service and IPC parser as hostile-input boundaries.

### Phase 8: Settings and desktop indicators

- [ ] Add remote desktop configuration to the settings model and IPC.
- [ ] Keep the feature disabled by default.
- [ ] Require explicit confirmation before listening on a non-loopback address.
- [ ] Provide output-selection controls.
- [ ] Provide separate input and clipboard permission controls.
- [ ] Show certificate or pairing identity where useful.
- [ ] Display a persistent compositor-owned indicator while any remote client is connected.
- [ ] Indicate whether the session is view-only or controlling the desktop.
- [ ] Provide a prominent local disconnect action.
- [ ] Notify the user when a session connects, changes permissions, or disconnects.
- [ ] Ensure remote content cannot obscure or counterfeit the compositor-owned indicator.

### Phase 9: Multi-monitor support

- [ ] Expose stable output identifiers and human-readable names.
- [ ] Send logical origin, logical size, physical size, scale, transform, and primary status.
- [ ] Support selecting any single output.
- [ ] Support switching the selected output safely during a session.
- [ ] Support a combined bounding desktop while preserving negative coordinates.
- [ ] Implement native RDP multi-monitor topology if supported by the chosen library.
- [ ] Maintain separate damage and frame state per output.
- [ ] Notify clients when outputs are connected, disconnected, rotated, scaled, or rearranged.
- [ ] Test mixed scale, rotation, and refresh-rate configurations.

### Phase 10: Optional advanced capabilities

- [ ] Investigate compositor-created virtual outputs.
- [ ] Investigate independent headless login sessions.
- [ ] Investigate audio output and microphone redirection.
- [ ] Investigate permission-scoped file transfer.
- [ ] Investigate multiple view-only clients.
- [ ] Investigate multiple independent seats or controllers.
- [ ] Add adaptive bitrate and resolution where protocol support permits it.

These capabilities are not required for the initial remote desktop release and should not delay the secure single-session implementation.

## Frame and Damage Rules

Each remote consumer maintains its own frame history. A consumer must receive a full frame when:

- Capture starts.
- The client reconnects.
- An output changes size, scale, transform, or pixel format.
- The service reports loss of decoder or frame state.
- A required intermediate update was dropped.
- The compositor cannot prove the client has a valid base frame.

Normal updates carry output-local physical damage rectangles. Rectangles must be clipped to the output. Existing damage compaction policy should be reused rather than independently reimplemented in the remote service.

Frames should be replaceable while queued. If frame 12 has not begun encoding when frame 13 arrives, frame 12 may be dropped when frame 13 contains everything required to advance the client safely. Memory and descriptor counts must remain bounded under a stalled client.

## Input Rules

- Input requires an authenticated session and explicit control permission.
- Coordinate conversion occurs in the compositor, which owns authoritative topology.
- The compositor validates the target output and coordinate bounds.
- Disconnecting or revoking control releases every key and button owned by that session.
- Secure attention sequences and compositor-reserved shortcuts require an explicit policy.
- Remote events must not be accepted merely because a network connection remains open.
- Lock and login screens must use a deny-by-default policy.

## Clipboard Rules

- Clipboard access is independently permissioned from viewing and input.
- Text is the only required initial data type.
- MIME types and byte sizes are validated before allocation or forwarding.
- Generation IDs prevent echo loops.
- Clipboard data is never written to diagnostic or audit logs.
- Password-manager and sensitive-field behavior must be tested before enabling clipboard synchronization by default.

## Security Requirements

- Remote desktop is disabled by default.
- The default listener is loopback-only.
- Non-loopback listening requires an explicit user action.
- All network connections use TLS.
- All sessions authenticate before capture begins.
- Permissions are capability-specific and revocable.
- A compositor-owned indicator is visible for the lifetime of every connection.
- The user can disconnect a session locally at any time.
- The compositor does not trust the remote service to authorize privileged actions.
- IPC and network messages have strict size, count, and time limits.
- Frame queues, clipboard transfers, and connection counts are bounded.
- No authentication secrets, clipboard content, pixels, or keystrokes are logged.
- The feature fails closed when session, lock, authorization, or service state is uncertain.

## Proposed Configuration

The final shape should follow the existing settings conventions. Conceptually it needs fields equivalent to:

```rust
pub struct RemoteDesktopSettings {
    pub enabled: bool,
    pub listen_address: String,
    pub port: u16,
    pub allow_input: bool,
    pub allow_clipboard: bool,
    pub require_local_confirmation: bool,
}
```

Configuration alone does not grant a connected client access. Runtime authorization remains per session.

## Testing Plan

### Unit tests

- Capture consumer lifecycle and queue bounds.
- Damage clipping, compaction, and full-frame recovery.
- Coordinate conversion across scale and transform combinations.
- Pressed-key and pressed-button cleanup.
- Clipboard MIME filtering, size limits, and loop prevention.
- IPC version negotiation and malformed-message rejection.
- Permission transitions and revocation.

### Integration tests

- Start, view, disconnect, and reconnect a single-output session.
- Kill and restart `focaldesk-remoted` without destabilizing the compositor.
- Kill and restart the compositor without leaving the service authorized.
- Stall a client and verify bounded memory and descriptor usage.
- Resize or rotate an output during capture.
- Connect and disconnect monitors during a session.
- Exercise mixed-DPI and negative-coordinate layouts.
- Revoke input while keys and pointer buttons are held.
- Lock the local session during remote viewing and control.
- Synchronize clipboard content in both directions without loops.
- Reject unauthorized, expired, oversized, and rate-limited requests.

### Performance tests

- Idle desktop CPU and GPU usage.
- End-to-end input-to-pixel latency.
- Full-screen motion throughput.
- Small-damage update bandwidth.
- Software versus hardware encoding cost.
- Slow-client behavior and frame-drop recovery.
- Multi-monitor memory and descriptor usage.

### Security tests

- Attempt capture before authentication.
- Attempt input and clipboard use without their permissions.
- Fuzz or property-test network and IPC decoding.
- Verify lock-screen and session-termination behavior.
- Verify socket ownership and peer-credential checks.
- Verify secrets and user data do not appear in logs.

## Release Criteria

The first supported release should not ship until:

- Authentication and TLS are enabled by default.
- The service remains disabled until explicitly configured.
- At least one output can be viewed reliably.
- Damage updates recover safely from dropped frames.
- Input permission can be granted and revoked without stuck state.
- Text clipboard permission can be granted and revoked independently.
- Locking or ending the local session produces the documented safe behavior.
- The persistent indicator and local disconnect action work reliably.
- Slow or malicious clients cannot cause unbounded compositor resource use.
- The threat model and user-facing limitations are documented.

## Open Decisions

- Which Rust RDP server implementation satisfies the required server, graphics, clipboard, and multi-monitor capabilities?
- Should production authentication use PAM, device pairing, or both?
- Where should TLS keys and certificates be stored and rotated?
- Which IPC serialization format provides safe bounded decoding and versioning?
- Should the remote service be a user service, a system service, or split into both roles?
- Should remote input reuse the active Smithay seat or create a dedicated virtual seat?
- What precise content is visible while the local session is locked?
- Which hardware encoders and GPU APIs are initially supported?
- Is multi-monitor delivered as separate streams, one bounding desktop, native RDP topology, or a negotiated combination?
- How should HDR output be tone-mapped for remote clients that expect SDR?
- Which compositor surfaces, notifications, or protected content must never be captured?
- What local confirmation policy applies to unattended access?

## Immediate Next Step

Implement Phase 1 without adding a network listener. Introduce a shared output-capture broker, convert the existing portal capture path to consume it, and verify that portal behavior remains unchanged. This establishes the central abstraction needed by remote desktop while keeping the first change local, testable, and security-neutral.
