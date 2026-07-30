

# Rendering Pipeline

FocalDesk uses a GPU-accelerated rendering pipeline built on OpenGL ES.

The renderer is responsible for composing application surfaces, shell UI, shaders, cursors, and desktop effects into the final image presented through DRM/KMS or other backends.

Rendering is designed to be modular so additional effects, color management, HDR, and future rendering backends can be added without redesigning the compositor.

Damage is tracked per output. Wayland client commits use Smithay's per-surface
damage history for toplevel, popup, and layer-shell trees, including
synchronized subsurfaces, buffer transforms, buffer scales, and viewporter
crops. Surface placement or viewport changes damage both the old and new
regions; detach and destruction repaint the saved old placement. Commits that
cannot be associated with a mapped tree use a conservative bounding-box or
full-output fallback. Visually unchanged commits carrying frame callbacks
schedule a one-pixel presentation so client frame pacing does not stall.

Per-root indexes and reusable traversal storage keep normal commits off the
global surface map and avoid repeated hot-path allocations. Output damage is
clipped and transitively compacted; overlapping rectangles are counted once
before the renderer decides whether a full frame is cheaper.

Enable **Log damage regions** in Settings or set
`FOCALDESK_DAMAGE_DEBUG=1` to periodically log input/compacted rectangle counts,
damaged-area percentages, fallback counts, precise/unchanged tree commits,
queued rectangles, and destroyed-surface cleanup. These measurements are the
runtime gauge for whether precise tracking is reducing rendered pixels on a
given workload.

See [Architecture](architecture.md#rendering-pipeline) for an annotated pipeline
diagram and [HDR Support](hdr.md) for the status of color-management work.
