

# Rendering Pipeline

FocalDesk uses a GPU-accelerated rendering pipeline built on OpenGL ES.

An experimental `focaldesk-render` boundary now contains a Vulkan-only wgpu
nested compositor implementation. It selects a Vulkan adapter, owns the wgpu
surface and pipelines, handles resize and surface-loss recovery, and composites
Wayland `ARGB8888`/`XRGB8888` surface trees with premultiplied alpha. SHM
buffers use damage-aware uploads. Single-plane BGRA and RGBA DMA-BUF modifier
support is queried from Vulkan and advertised with linux-dmabuf feedback.
DMA-BUFs use wgpu-hal's Vulkan external-memory import, with a synchronized
mapped upload for linear buffers as a compatibility fallback. The Vulkan render
node also backs `linux-drm-syncobj-v1`: acquire timeline points install
asynchronous Smithay commit blockers, and client buffers remain retained until
wgpu reports completion of the submission that sampled them. That permits the
release timeline point to signal without a compositor-thread GPU idle wait. Window
subsurfaces and popups follow Smithay's surface stacking and committed offsets,
and normal-orientation viewport crops/scaling are mapped into texture
coordinates. Layer-shell surfaces are placed around the window stack according
to their background, bottom, top, or overlay layer. The production DRM/KMS and
established nested compositor paths continue to use OpenGL ES.

The nested wgpu path forwards host keyboard, pointer, button, and scroll events
through the compositor's normal focus and Wayland seat path. Its themed cursor
is composited as a final scaled RGBA texture with the cursor hotspot applied;
client-provided SHM cursor surface trees are composited through the same path.

GPU textures and bind groups are retained per `wl_buffer`. The backend tracks
Smithay commit counters per buffer (important for rotating buffer pools) and
uploads only accumulated damage rectangles when a cached upload buffer returns.
Unused entries age out of the cache. All eight Wayland buffer rotations and
reflections are applied while mapping viewport texture coordinates.

The Vulkan path also has a retained output scene. Pending physical output
damage is clamped and applied as render-pass scissors; the complete retained
scene is then sampled into whichever swapchain image was acquired. This avoids
depending on swapchain-image preservation while limiting scene recomposition to
dirty pixels. With no compositor damage, connected idle clients no longer
schedule frames continuously.

The shell pass now renders configured wallpaper fit/tint/dim behavior, cached
font-atlas text, state-tinted SVG icons, antialiased rounded panels, and
notification text above clients while keeping cursors foremost. Advanced GLES
effect parity, multi-plane DMA-BUFs, and color-managed/HDR output remain.

When `drm-wgpu` is combined with the real DRM backend, FocalDesk creates a
Vulkan device matched by DRM render-node major/minor and polls it from the DRM
render loop. At output initialization it imports the backend's real single-plane
GBM allocation into Vulkan as a color attachment and submits a bounded render
probe. This establishes the correct multi-GPU ownership and target-import
boundary. The current KMS frame still comes from the established GLES
offscreen/DrmOutput path; full Vulkan scene output plus explicit Vulkan/KMS
fences is the next DRM migration stage.

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

The compositor also retains the built shell UI for an output while its logical
size, scale, theme, chrome metrics, item configuration, and calculated geometry
remain unchanged. Lock-screen text preparation follows the same invalidation
model: static labels are prepared once per theme, while the message and password
glyphs are refreshed only when their source values change. Renderer resets
invalidate these text caches together with the font atlas.

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
