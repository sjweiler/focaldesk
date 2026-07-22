

# Rendering Pipeline

FocalDesk uses a GPU-accelerated rendering pipeline built on OpenGL ES.

The renderer is responsible for composing application surfaces, shell UI, shaders, cursors, and desktop effects into the final image presented through DRM/KMS or other backends.

Rendering is designed to be modular so additional effects, color management, HDR, and future rendering backends can be added without redesigning the compositor.

Damage is tracked at output and top-level window granularity to avoid full
redraws where possible. Precise damage propagation for Wayland subsurfaces is
planned and is not yet represented as a complete optimization.

See [Architecture](architecture.md#rendering-pipeline) for an annotated pipeline
diagram and [HDR Support](hdr.md) for the status of color-management work.
