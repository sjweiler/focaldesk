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
### Why I Chose OpenGL ES Instead of Vulkan
FocalDesk needed the simplest rendering solution that could perform desktop composition reliably. Its rendering workload primarily
consists of compositing window textures, drawing desktop elements, applying shaders, handling transparency and rounded geometry, and
presenting frames through DRM/KMS. OpenGL ES provides the required capabilities without the additional resource management,
synchronization, and implementation complexity of Vulkan. Vulkan would not inherently improve HDR support or provide a meaningful
user-visible advantage for FocalDesk’s current rendering workload. OpenGL ES therefore offers the better balance of capability,
reliability, and maintainability.
### Why I Built a Session Manager
FocalDesk includes its own session manager so it can control the complete lifecycle of a desktop session. The session manager starts
required desktop services, tracks their state, coordinates startup and shutdown, handles failures, and ensures that components
terminate cleanly when the user logs out. This provides a defined boundary between the display manager, compositor, and supporting
user services. Managing the session lifecycle directly also reduces dependence on behavior designed around another desktop
environment and allows FocalDesk’s services to start in a predictable order.
