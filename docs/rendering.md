

# Rendering Pipeline

FocalDesk uses a GPU-accelerated rendering pipeline built on OpenGL ES.

The renderer is responsible for composing application surfaces, shell UI, shaders, cursors, and desktop effects into the final image presented through DRM/KMS or other backends.

Rendering is designed to be modular so additional effects, color management, HDR, and future rendering backends can be added without redesigning the compositor.

Damage is tracked per output. Wayland client commits use Smithay's per-surface
damage history, including synchronized subsurfaces, buffer transforms, buffer
scales, and viewporter crops. Surface placement or viewport changes damage both
the old and new regions. Top-level window damage remains as a conservative
fallback for commits that cannot be associated with a mapped surface tree.

See [Architecture](architecture.md#rendering-pipeline) for an annotated pipeline
diagram and [HDR Support](hdr.md) for the status of color-management work.
