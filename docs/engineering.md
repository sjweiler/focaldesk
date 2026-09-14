## Key Engineering Decisions
### Why I Chose Smithay
Smithay is a Rust-based library that provides the core building blocks needed to implement a Wayland compositor, including protocol
handling, input processing, rendering integration, and window management infrastructure. Using Smithay allowed FocalDesk to remain
within the Rust ecosystem while retaining control over compositor architecture and behavior. It provides reusable Wayland components
without imposing a complete desktop shell or fixed product design.
### Why I Chose Rust
Rust was selected primarily for memory safety and strong compile-time guarantees. A compositor interacts with graphics buffers,
hardware devices, IPC endpoints, input events, and multiple asynchronous subsystems. Failures in these areas can affect the entire
desktop session. Rust helps prevent common problems such as null-pointer access, use-after-free errors, data races, and unsafe
shared-state handling. Rust also provides the performance and low-level control required for systems programming without requiring
memory safety to be managed entirely through developer discipline.
### Why I Did Not Use wlroots
wlroots is a capable compositor library, but adopting it would have introduced legacy and architectural entanglements that did not
align with FocalDesk’s design goals. FocalDesk was intended to have a Rust-native architecture with direct ownership of its
compositor, session, rendering, IPC, and desktop-service behavior. Smithay provided a cleaner foundation for that architecture
without requiring a C-based integration layer or inheriting assumptions from an existing compositor ecosystem. This was not a
judgment that wlroots is unsuitable in general. It was a decision based on architectural fit and long-term maintainability for
FocalDesk.
### Why I Keep GLES and Built a Raw Vulkan Renderer
FocalDesk began with Smithay's GLES renderer because it was the shortest path to reliable desktop composition and remains the broadest
compatibility and recovery option. The Vulkan work first used wgpu in a nested backend to validate scene construction, client-buffer
imports, and input without risking the active KMS session. The production Vulkan backend instead uses Ash directly: Smithay owns DRM/KMS,
GBM allocates scanout buffers, and Vulkan imports those buffers, performs FP16 composition and HDR10/PQ encoding, exports explicit fences,
and hands them back to atomic KMS. This is more implementation work, but it exposes the exact external-memory, queue-ownership,
synchronization, 10-bit format, and KMS handoff needed by this compositor.

Raw Vulkan did not make HDR possible by itself; both renderer paths can perform the required color conversion. It made FocalDesk's HDR
pipeline easier to control and diagnose end to end without depending on the abstraction and backend behavior of the experimental wgpu
path. GLES is therefore retained rather than replaced, and the two DRM renderers provide an intentional compatibility/recovery choice.
### Why I Built a Session Manager
FocalDesk includes its own session manager so it can control the complete lifecycle of a desktop session. The session manager starts
required desktop services, tracks their state, coordinates startup and shutdown, handles failures, and ensures that components
terminate cleanly when the user logs out. This provides a defined boundary between the display manager, compositor, and supporting
user services. Managing the session lifecycle directly also reduces dependence on behavior designed around another desktop
environment and allows FocalDesk’s services to start in a predictable order.
