

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

The Vulkan path also has a retained output scene. With no compositor damage,
connected idle clients do not schedule frames continuously. Each output keeps
bounded damage history for its GBM buffers, repaints the regions an acquired
buffer missed, and falls back to a full repaint if that history is incomplete.
The same current-frame regions are forwarded as KMS damage clips.

The shell pass now renders configured wallpaper fit/tint/dim behavior, cached
font-atlas text, state-tinted SVG icons, antialiased rounded panels, and
notification text above clients while keeping cursors foremost. Advanced GLES
effect parity, multi-plane DMA-BUFs, and color-managed/HDR output remain.

The real DRM executable contains two independent rendering paths. The existing
path uses Smithay's EGL/GLES renderer with GBM and KMS. The Vulkan path uses raw
ash while retaining Smithay as the DRM/KMS owner. GBM swapchain DMA-BUFs are
imported with explicit DRM modifiers, rendered as Vulkan color attachments,
released to the foreign queue family, and submitted to atomic KMS with an
exported sync-file. Client DMA-BUF textures use the corresponding foreign queue
ownership transfers; SHM textures use Vulkan staging uploads. There is no
Vulkan display surface, Vulkan WSI swapchain, GLES copy, or EGL import bridge.

`focaldmd` selects the renderer before the user session starts. The Vulkan DRM
backend creates an explicit-sync GBM swapchain for every enabled connected
output and applies the saved mode, fractional scale, logical position, and
primary-output selection from `displays.json`. It resets every compositor-owned
swapchain across libseat pause/resume, shares the established DRM input
dispatcher and deferred action pumps used by the GLES backend, and runs the
normal XWayland lifecycle when that feature is enabled. GLES remains the
recovery renderer and retains HDR and zero-copy DMA-BUF capture support. Raw
Vulkan supports screenshots, SHM portal capture, and the local remote-frame
transport through asynchronous output readback. Udev connector events and
display-settings IPC updates rebuild the raw Vulkan KMS topology.
The Vulkan renderer paints egui panels as native indexed Vulkan meshes, with
retained texture updates, physical-pixel scaling, and per-mesh clipping; it does
not create an EGL or GLES context for panel rendering.
DRM connector EDID supplies the same monitor make, model, serial, physical size,
and ICC-profile matching inputs used by the GLES backend. The selected profile
is retained in output state. Vulkan SDR composition un-premultiplies and decodes
sRGB clients, wallpaper, solids, and shell meshes, blends them in linear light,
applies the selected output-primaries matrix, and uses an sRGB KMS attachment for
the matching output encode. Arbitrary ICC LUT/TRC transforms and extended-range
HDR scanout still require the FP16 output pipeline.

Raw Vulkan submissions and KMS presents have bounded progress deadlines. A
stalled GPU fence recreates the Vulkan device and scanout in-process, while a
missing vblank rebuilds the KMS scanout. The renderer skips blocking Vulkan
teardown for a wedged device.

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
