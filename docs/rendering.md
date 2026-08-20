

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

The staged linear-light path retains its SDR, FP16, overlay, and encoded output
targets between frames. Compacted damage is carried through every pass; a full
frame is used only when a target is new or resized, capture requires it, or a
normal full-redraw condition invalidates retained contents.

Color-generating shell effects have two program families. The established
encoded-SDR shaders remain unchanged and are used by the legacy target. When the
linear-SDR FP16 path is available, the compositor selects separate Display-P3
variants for glow, pulse, lightbar, etched-glass, gradient, tint, wallpaper, and
screensaver effects. Those variants convert into the extended scene-linear
Rec.709 working space without clamping, then the normal per-output matrix,
transfer function, and optional ICC LUT produce the monitor signal. Wide-gamut
program compilation is all-or-nothing; a driver rejection retains the complete
legacy family for that renderer.

The encoded-SDR wallpaper/chrome base has its own per-output generation. Client
commits, window movement/resizing, cursor updates, and egui interaction reuse the
cached base and only decode its damaged regions into the FP16 scene. Theme,
layout, hover, and conservatively classified changes advance the generation;
each scanout or capture target refreshes independently when it falls behind.

After that base decode, the bundled wallpaper receives a display-aware creative
grade over only its work-area rectangle. On wide-gamut SDR outputs, cyan and
orange artwork accents expand selectively into Display P3 while luminance stays
SDR. On HDR10 outputs, the shader keeps diffuse wallpaper at reference white
and applies a conservative lift to isolated stars, accents, and the planet rim
inside DisplayHDR 400-class headroom (about 450 nits). It does not manufacture 800–1000 nit
highlights. The white wordmark stays at graphics white, and the grade is
bounded at 450 nits before the ordinary
BT.2020/PQ output transform. It is disabled on conventional sRGB SDR outputs.

Set `FOCALDESK_RENDER_TIMINGS=1` to sample the linear pipeline every 120 frames.
Sampled frames wait for each GPU completion fence and log optional base, decode,
client, shell-overlay, optional sRGB-overlay, and output-encode latency. The waits
intentionally serialize only the sampled frame, so leave this disabled for normal
use and benchmark runs.

Enable **Log damage regions** in Settings or set
`FOCALDESK_DAMAGE_DEBUG=1` to periodically log input/compacted rectangle counts,
damaged-area percentages, fallback counts, precise/unchanged tree commits,
queued rectangles, and destroyed-surface cleanup. These measurements are the
runtime gauge for whether precise tracking is reducing rendered pixels on a
given workload.

See [Architecture](architecture.md#rendering-pipeline) for an annotated pipeline
diagram and [HDR Support](hdr.md) for the status of color-management work.
