// Standalone DRM/KMS scanout for the greeter, deliberately independent of
// focaldesk-engine's backend::drm (which is 3000+ lines of GBM/EGL/GLES/
// wayland-server machinery for the real compositor). The greeter only ever
// needs to show a login box on one output, so it uses legacy (non-atomic)
// modesetting. Buffer allocation and FB creation follow the same rules as
// the compositor: `GbmAllocator` with `RENDERING|SCANOUT`, plane∩EGL
// modifiers, and `framebuffer_from_bo` (AddFB2 + modifiers).
//
// Rendering is GPU-accelerated when that path can bind a scanout dmabuf as a
// GLES FBO. The background shader and UI (`render::paint_frame_gpu` + glyph
// atlas) draw straight into the back-buffer dmabuf. LINEAR|WRITE|SCANOUT
// buffers cannot be GLES FBOs on NVIDIA; those remain the CPU fallback
// (`render::paint_frame`).
//
// Known corners cut, deliberately:
// - Legacy `set_crtc` / `page_flip` present (no atomic DrmOutput / fencing).
// - No xkbcommon; keycodes come from `crate::keymap`'s fixed US-QWERTY table.

use anyhow::{anyhow, Context, Result};
use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf};
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBuffer, GbmBufferFlags};
use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::drm::gbm::{framebuffer_from_bo, GbmFramebuffer};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::renderer::gles::{
    ffi, GlesPixelProgram, GlesRenderer, GlesTexture, Uniform, UniformName, UniformType,
};
use smithay::backend::renderer::{Bind, ExportMem, Frame, Offscreen, Renderer, Texture};
use smithay::backend::session::{
    libseat::LibSeatSession, libseat::LibSeatSessionNotifier, Session,
};
use smithay::backend::udev::primary_gpu;
use smithay::reexports::drm::control::{
    connector, crtc, Device as ControlDevice, Event, Mode, ModeTypeFlags, PageFlipFlags,
};
use smithay::reexports::gbm::Device as GbmDevice;
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{Buffer, DeviceFd, Physical, Rectangle, Size, Transform};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::glyph_atlas::GlyphAtlas;
use crate::render;

const GREETER_BACKGROUND2_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

uniform vec2 u_resolution;
uniform float u_time;

out vec4 frag_color;

mat2 rotate_2d(float angle)
{
    float c = cos(angle);
    float s = sin(angle);

    return mat2(c, -s, s, c);
}

void main()
{
    vec2 uv = (
        gl_FragCoord.xy - 0.5 * u_resolution
    ) / u_resolution.y;

    float rotation =
        sin(u_time * 1.25) * 2.4;

    uv = rotate_2d(rotation) * uv;

    float radius = length(uv);
    float angle = atan(uv.y, uv.x);

    float tunnel = log(radius + 0.025);

    float rings =
        sin(tunnel * 18.0 - u_time * 4.0);

    float spiral =
        sin(angle * 7.0 + tunnel * 11.0 + u_time * 2.0);

    float pattern =
        rings * 0.6 + spiral * 0.4;

    float bands =
        smoothstep(0.1, 0.9, pattern);

    vec3 orange = vec3(1.0, 0.12, 0.02);
    vec3 violet = vec3(0.18, 0.02, 0.75);

    float phase =
        0.5 + 0.5 * sin(angle * 3.0 + tunnel * 6.0);

    vec3 color = mix(orange, violet, phase);
    color *= bands;

    float center_glow =
        0.045 / (radius + 0.04);

    color += center_glow * vec3(1.0, 0.5, 0.2);

    frag_color = vec4(color, 1.0);
}
"#;

const GREETER_BACKGROUND_FRAG: &str = r#"
#ifdef GL_ES
precision mediump float;
#endif

uniform vec2 u_resolution;
uniform float u_time;

varying vec2 v_coords;

void main() {
    vec2 uv = v_coords;
    vec2 center = vec2(0.5, 0.48);
    vec2 c1 = vec2(0.32 + sin(u_time * 0.55) * 0.05, 0.28 + cos(u_time * 0.75) * 0.04);
    vec2 c2 = vec2(0.72 + cos(u_time * 0.40) * 0.03, 0.74 + sin(u_time * 0.60) * 0.05);

    float d1 = dot(uv - c1, uv - c1);
    float d2 = dot(uv - c2, uv - c2);
    float glow1 = pow(max(1.0 - d1 / 0.18, 0.0), 3.0);
    float glow2 = pow(max(1.0 - d2 / 0.14, 0.0), 3.0);
    float vignette = clamp(1.0 - dot(uv - center, uv - center) * 1.55, 0.0, 1.0);
    float scan = sin((uv.y * u_resolution.y) * 0.04 + u_time * 6.0) * 0.018;

    vec3 base = vec3(0.05, 0.07, 0.11);
    vec3 blue = vec3(0.09, 0.24, 0.38);
    vec3 teal = vec3(0.14, 0.46, 0.52);
    vec3 amber = vec3(0.82, 0.56, 0.20);

    vec3 color = base
        + blue * glow1
        + teal * glow2 * 0.7
        + amber * (glow2 * 0.14)
        + vec3(vignette * 0.08)
        + vec3(scan);

    gl_FragColor = vec4(clamp(color, 0.0, 1.0), 1.0);
}
"#;

const GPU_COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Xrgb8888, Fourcc::Argb8888];

pub struct GreeterOutput {
    session: LibSeatSession,
    fd: DrmDeviceFd,
    // Kept alive alongside `bos`: the C gbm_device must outlive any buffer
    // objects allocated from it.
    #[allow(dead_code)]
    gbm: GbmDevice<DrmDeviceFd>,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    // Two buffers so `render` never writes into the one currently on
    // screen: mapping and mutating a GBM buffer object while it's pinned as
    // the active CRTC framebuffer races the display controller's scanout of
    // that same memory — on the proprietary NVIDIA driver in particular,
    // that's not just tearing, it's a plausible hang. `front` is the index
    // into `bos`/`fbs` currently on screen; `render` always writes the
    // other one, then page-flips onto it.
    bos: [GbmBuffer; 2],
    fbs: [GbmFramebuffer; 2],
    front: usize,
    flip_pending: bool,
    background_style: render::BackgroundStyle,
    gpu: Option<GpuRenderer>,
    direct_gpu_scanout: bool,
}

struct GpuRenderer {
    renderer: GlesRenderer,
    background_program: GlesPixelProgram,
    atlas: GlyphAtlas,
    offscreen: Option<GlesTexture>,
}

impl GpuRenderer {
    fn new(
        gbm: &GbmDevice<DrmDeviceFd>,
        sizes: render::FontSizes,
        background_style: render::BackgroundStyle,
    ) -> Result<Self> {
        let display = unsafe { EGLDisplay::new(gbm.clone()) }
            .context("failed to create EGL display for greeter")?;
        let context =
            EGLContext::new(&display).context("failed to create EGL context for greeter")?;
        let mut renderer = unsafe { GlesRenderer::new(context) }
            .context("failed to create GLES renderer for greeter")?;

        let shader = match background_style {
            render::BackgroundStyle::Aurora => GREETER_BACKGROUND_FRAG,
            render::BackgroundStyle::SpiralTunnel => GREETER_BACKGROUND2_FRAG,
        };
        let background_program = renderer
            .compile_custom_pixel_shader(
                shader,
                &[
                    UniformName::new("u_resolution", UniformType::_2f),
                    UniformName::new("u_time", UniformType::_1f),
                ],
            )
            .context("failed to compile greeter background shader")?;

        let atlas = GlyphAtlas::build(&mut renderer, sizes)
            .context("failed to build greeter glyph atlas")?;

        Ok(Self {
            renderer,
            background_program,
            atlas,
            offscreen: None,
        })
    }

    fn egl_render_formats(&self) -> &FormatSet {
        self.renderer.egl_context().dmabuf_render_formats()
    }

    /// Confirms this dmabuf can be used as a GLES framebuffer before we commit
    /// to the GPU present path (LINEAR scanout buffers fail here on NVIDIA).
    fn probe_bind(&mut self, dmabuf: &mut Dmabuf) -> Result<()> {
        let _target = self
            .renderer
            .bind(dmabuf)
            .context("failed to bind greeter scanout dmabuf during GPU probe")?;
        Ok(())
    }

    fn render_to(
        &mut self,
        dmabuf: &mut Dmabuf,
        state: &render::FrameState,
        width: u32,
        height: u32,
    ) -> Result<render::FrameHitTargets> {
        let rect_f =
            Rectangle::<f64, Buffer>::new((0.0, 0.0).into(), (width as f64, height as f64).into());
        let rect_i =
            Rectangle::<i32, Physical>::new((0, 0).into(), (width as i32, height as i32).into());
        let buffer_size = Size::<i32, Buffer>::from((width as i32, height as i32));
        let physical_size = Size::<i32, Physical>::from((width as i32, height as i32));

        let mut target = self
            .renderer
            .bind(dmabuf)
            .context("failed to bind greeter scanout dmabuf")?;
        let mut frame = self
            .renderer
            .render(&mut target, physical_size, Transform::Normal)
            .context("failed to begin greeter GLES frame")?;

        let uniforms = [
            Uniform::new("u_resolution", [width as f32, height as f32]),
            Uniform::new("u_time", state.pulse_phase),
        ];

        frame
            .render_pixel_shader_to(
                &self.background_program,
                rect_f,
                rect_i,
                buffer_size,
                Some(std::slice::from_ref(&rect_i)),
                1.0,
                &uniforms,
            )
            .context("failed to draw greeter shader background")?;

        let hit_targets = render::paint_frame_gpu(&mut frame, &self.atlas, width, height, state)
            .context("failed to draw greeter UI")?;

        let sync = frame
            .finish()
            .context("failed to finish greeter GLES frame")?;
        self.renderer
            .wait(&sync)
            .context("failed to wait for greeter GLES frame")?;

        Ok(hit_targets)
    }

    /// Render on the GPU when the KMS scanout buffer itself cannot be bound as
    /// an EGL framebuffer, then read the completed frame back for the final
    /// copy into a CPU-mappable linear scanout buffer.
    fn render_offscreen(
        &mut self,
        state: &render::FrameState,
        width: u32,
        height: u32,
    ) -> Result<(render::FrameHitTargets, Vec<u8>)> {
        let size = Size::<i32, Buffer>::from((width as i32, height as i32));
        let recreate = self
            .offscreen
            .as_ref()
            .is_none_or(|texture| texture.size() != size);
        if recreate {
            self.offscreen = Some(
                <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(
                    &mut self.renderer,
                    Fourcc::Xbgr8888,
                    size,
                )
                .context("failed to create greeter GPU offscreen target")?,
            );
        }

        let texture = self.offscreen.as_mut().expect("offscreen created");
        let rect_f =
            Rectangle::<f64, Buffer>::new((0.0, 0.0).into(), (width as f64, height as f64).into());
        let rect_i =
            Rectangle::<i32, Physical>::new((0, 0).into(), (width as i32, height as i32).into());
        let physical_size = Size::<i32, Physical>::from((width as i32, height as i32));
        let mut target = self.renderer.bind(texture)?;
        let mut frame = self
            .renderer
            .render(&mut target, physical_size, Transform::Normal)?;
        frame.render_pixel_shader_to(
            &self.background_program,
            rect_f,
            rect_i,
            size,
            Some(std::slice::from_ref(&rect_i)),
            1.0,
            &[
                Uniform::new("u_resolution", [width as f32, height as f32]),
                Uniform::new("u_time", state.pulse_phase),
            ],
        )?;
        let hit_targets = render::paint_frame_gpu(&mut frame, &self.atlas, width, height, state)?;
        let sync = frame.finish()?;
        self.renderer.wait(&sync)?;

        self.renderer.with_context(|gl| unsafe {
            gl.BindBuffer(ffi::PIXEL_PACK_BUFFER, 0);
        })?;
        let region = Rectangle::<i32, Buffer>::new((0, 0).into(), size);
        // ARGB8888 readback is BGRA byte order on little-endian systems, which
        // is exactly the memory layout required by the XRGB8888 KMS buffer.
        let mapping = self
            .renderer
            .copy_framebuffer(&target, region, Fourcc::Argb8888)?;
        self.renderer.with_context(|gl| unsafe { gl.Finish() })?;
        let pixels = self.renderer.map_texture(&mapping)?.to_vec();
        Ok((hit_targets, pixels))
    }
}

impl GreeterOutput {
    /// Opens the primary GPU via libseat, mode-sets the first connected
    /// output at its preferred mode, and hands back the pieces the caller
    /// needs to register with calloop: the session notifier (Pause/Activate
    /// events) and a libinput context (keyboard/pointer events).
    ///
    /// Expects the greeter VT to already be the foreground console (focaldmd
    /// switches with `VT_ACTIVATE` before spawn). Without an active seat
    /// session, libseat opens the card without DRM master and NVIDIA scanout
    /// buffer allocation fails.
    pub fn open() -> Result<(Self, LibSeatSessionNotifier, Libinput)> {
        let (mut session, notifier) = LibSeatSession::new()
            .map_err(|e| anyhow!("could not initialize libseat session: {e}"))?;

        ensure_session_active(&mut session)?;

        let gpu_path = primary_gpu(session.seat())
            .context("failed to enumerate GPUs for seat")?
            .ok_or_else(|| anyhow!("no primary GPU found for seat {}", session.seat()))?;

        tracing::info!(
            seat = %session.seat(),
            gpu = %gpu_path.display(),
            active = session.is_active(),
            "opening greeter DRM device"
        );

        let raw_fd = session
            .open(&gpu_path, OFlags::RDWR | OFlags::CLOEXEC)
            .map_err(|e| anyhow!("failed to open {}: {e:?}", gpu_path.display()))?;
        let fd = DrmDeviceFd::new(DeviceFd::from(raw_fd));

        let res = fd
            .resource_handles()
            .context("failed to load DRM resource handles")?;

        let connector_info: Vec<connector::Info> = res
            .connectors()
            .iter()
            .flat_map(|handle| fd.get_connector(*handle, false))
            .collect();

        let con = connector_info
            .iter()
            .find(|c| c.state() == connector::State::Connected)
            .ok_or_else(|| anyhow!("no connected connector found on {}", gpu_path.display()))?;

        let mode = *con
            .modes()
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or_else(|| con.modes().first())
            .ok_or_else(|| anyhow!("connector {:?} has no modes", con.handle()))?;

        let crtc_handle = pick_crtc(&fd, &res, con)
            .ok_or_else(|| anyhow!("no CRTC available for connector {:?}", con.handle()))?;

        // Query primary-plane formats the same way the compositor does, then
        // drop the DrmDevice — we still present with legacy set_crtc/page_flip.
        let plane_formats = primary_plane_formats(&fd, crtc_handle)?;

        let gbm = GbmDevice::new(fd.clone()).context("failed to create GBM device")?;
        let (width, height) = mode.size();
        let sizes = render::font_sizes(height as u32);
        let background_style = select_background_style();
        tracing::info!(
            style = background_style.as_str(),
            "selected greeter background"
        );

        let (bos, fbs, gpu, direct_gpu_scanout) = match try_open_gpu_scanout(
            &gbm,
            &fd,
            &plane_formats,
            width,
            height,
            sizes,
            background_style,
        ) {
            Ok(gpu_path) => {
                tracing::info!(
                    "greeter using GPU scanout path (GbmAllocator + framebuffer_from_bo)"
                );
                let (bos, fbs, gpu) = gpu_path;
                (bos, fbs, gpu, true)
            }
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "direct GPU greeter scanout unavailable, trying GPU offscreen rendering"
                );
                let (bos, fbs) = make_cpu_buffers(&gbm, &fd, width, height)?;
                match GpuRenderer::new(&gbm, sizes, background_style) {
                    Ok(gpu) => {
                        tracing::info!(
                            "greeter using GPU offscreen rendering with linear KMS transfer"
                        );
                        (bos, fbs, Some(gpu), false)
                    }
                    Err(gpu_err) => {
                        tracing::warn!(
                            error = ?gpu_err,
                            "GPU offscreen greeter unavailable, using CPU rendering"
                        );
                        (bos, fbs, None, false)
                    }
                }
            }
        };

        fd.set_crtc(
            crtc_handle,
            Some(*fbs[0].as_ref()),
            (0, 0),
            &[con.handle()],
            Some(mode),
        )
        .context("failed to set CRTC")?;

        let mut libinput = Libinput::new_with_udev::<
            smithay::backend::libinput::LibinputSessionInterface<LibSeatSession>,
        >(session.clone().into());
        libinput
            .udev_assign_seat(&session.seat())
            .map_err(|e| anyhow!("failed to assign libinput seat: {e:?}"))?;

        let output = Self {
            session,
            fd,
            gbm,
            crtc: crtc_handle,
            connector: con.handle(),
            mode,
            bos,
            fbs,
            front: 0,
            flip_pending: false,
            background_style,
            gpu,
            direct_gpu_scanout,
        };

        Ok((output, notifier, libinput))
    }

    pub fn is_session_active(&self) -> bool {
        self.session.is_active()
    }

    pub fn change_vt(&mut self, vt: i32) -> Result<()> {
        self.session
            .change_vt(vt)
            .map_err(|e| anyhow!("VT switch to {vt} failed: {e:?}"))
    }

    pub fn mode_size(&self) -> (u32, u32) {
        let (w, h) = self.mode.size();
        (w as u32, h as u32)
    }

    pub fn drm_fd(&self) -> DrmDeviceFd {
        self.fd.clone()
    }

    pub fn flip_pending(&self) -> bool {
        self.flip_pending
    }

    pub fn background_style(&self) -> render::BackgroundStyle {
        self.background_style
    }

    /// Re-applies the CRTC after a `SessionEvent::ActivateSession`. Another
    /// process may have taken DRM master and scanned out something else
    /// while we were paused; this is a best-effort re-assertion, not a full
    /// atomic-KMS resume path. Re-asserts whichever buffer is currently
    /// `front` — its content is always our last completed frame, since
    /// `render` only ever touches the other one.
    pub fn reassert_scanout(&mut self) -> Result<()> {
        self.fd
            .set_crtc(
                self.crtc,
                Some(*self.fbs[self.front].as_ref()),
                (0, 0),
                &[self.connector],
                Some(self.mode),
            )
            .context("failed to re-set CRTC on session resume")
    }

    pub fn handle_drm_events(&mut self) -> Result<bool> {
        let mut saw_page_flip = false;

        loop {
            let events = match self.fd.receive_events() {
                Ok(events) => events,
                Err(err) => {
                    return Err(anyhow!("failed to read DRM events: {err}"));
                }
            };

            let mut saw_event = false;
            for event in events {
                saw_event = true;
                if matches!(event, Event::PageFlip(_)) {
                    self.flip_pending = false;
                    saw_page_flip = true;
                }
            }

            if !saw_event || saw_page_flip {
                break;
            }
        }

        Ok(saw_page_flip)
    }

    pub fn render(&mut self, state: &render::FrameState<'_>) -> Result<render::FrameHitTargets> {
        if self.flip_pending {
            return Ok(render::FrameHitTargets::default());
        }

        let back = 1 - self.front;
        let (width, height) = self.mode.size();

        let layout = if self.direct_gpu_scanout {
            let mut dmabuf = self.bos[back]
                .export()
                .context("failed to export greeter scanout buffer")?;
            self.gpu
                .as_mut()
                .unwrap()
                .render_to(&mut dmabuf, state, width as u32, height as u32)?
        } else if let Some(gpu) = self.gpu.as_mut() {
            let (layout, pixels) = gpu
                .render_offscreen(state, width as u32, height as u32)
                .context("GPU offscreen greeter frame failed")?;
            let stride = self.bos[back].stride() as usize;
            let row_bytes = width as usize * 4;
            self.bos[back]
                .map_mut(0, 0, width.into(), height.into(), |mapping| {
                    let dst = mapping.buffer_mut();
                    // OpenGL readback starts at the bottom row; KMS linear
                    // buffers start at the top row.
                    for y in 0..height as usize {
                        let src_y = height as usize - 1 - y;
                        let src = &pixels[src_y * row_bytes..(src_y + 1) * row_bytes];
                        let dst_row = &mut dst[y * stride..y * stride + row_bytes];
                        dst_row.copy_from_slice(src);
                    }
                })
                .context("failed to transfer GPU frame into KMS buffer")?;
            layout
        } else {
            let stride = self.bos[back].stride();
            self.bos[back]
                .map_mut(0, 0, width.into(), height.into(), |mapping| {
                    let buf = mapping.buffer_mut();
                    let frame_state = render::FrameState {
                        login: state.login,
                        pointer: state.pointer,
                        power_menu_open: state.power_menu_open,
                        pulse_phase: state.pulse_phase,
                        background_style: self.background_style,
                        paint_background: true,
                    };

                    render::paint_frame(buf, stride, width as u32, height as u32, &frame_state)
                })
                .context("failed to map GBM buffer object for render")?
        };

        self.fd
            .page_flip(
                self.crtc,
                *self.fbs[back].as_ref(),
                PageFlipFlags::EVENT,
                None,
            )
            .context("failed to queue page flip")?;

        self.front = back;
        self.flip_pending = true;
        Ok(layout)
    }
}

fn pick_crtc(
    fd: &DrmDeviceFd,
    res: &smithay::reexports::drm::control::ResourceHandles,
    con: &connector::Info,
) -> Option<crtc::Handle> {
    con.encoders().iter().find_map(|enc| {
        let enc_info = fd.get_encoder(*enc).ok()?;
        res.filter_crtcs(enc_info.possible_crtcs())
            .into_iter()
            .next()
    })
}

fn primary_plane_formats(fd: &DrmDeviceFd, crtc: crtc::Handle) -> Result<FormatSet> {
    let (drm, _notifier) =
        DrmDevice::new(fd.clone(), false).context("failed to open DrmDevice for plane query")?;
    let planes = drm
        .planes(&crtc)
        .context("failed to query planes for greeter CRTC")?;
    let primary = planes
        .primary
        .first()
        .ok_or_else(|| anyhow!("no primary plane for greeter CRTC {:?}", crtc))?;
    Ok(primary.formats.clone())
}

fn modifiers_for_format(formats: &FormatSet, code: Fourcc) -> Vec<Modifier> {
    formats
        .iter()
        .filter(|format| format.code == code)
        .map(|format| format.modifier)
        .collect()
}

fn intersect_modifiers(plane: &[Modifier], egl: &[Modifier]) -> Vec<Modifier> {
    if plane.is_empty() {
        return egl.to_vec();
    }
    if egl.is_empty() {
        return plane.to_vec();
    }
    let plane_set: std::collections::HashSet<_> = plane.iter().copied().collect();
    egl.iter()
        .copied()
        .filter(|m| plane_set.contains(m))
        .collect()
}

fn make_fb(fd: &DrmDeviceFd, bo: &GbmBuffer) -> Result<GbmFramebuffer> {
    // Prefer opaque FB when possible (matches compositor scanout habit).
    framebuffer_from_bo(fd, bo, true).context("framebuffer_from_bo failed for greeter buffer")
}

fn make_cpu_buffers(
    gbm: &GbmDevice<DrmDeviceFd>,
    fd: &DrmDeviceFd,
    width: u16,
    height: u16,
) -> Result<([GbmBuffer; 2], [GbmFramebuffer; 2])> {
    let mut allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::SCANOUT | GbmBufferFlags::WRITE | GbmBufferFlags::LINEAR,
    );
    let flags = GbmBufferFlags::SCANOUT | GbmBufferFlags::WRITE | GbmBufferFlags::LINEAR;
    let modifiers = [Modifier::Linear, Modifier::Invalid];

    let mut make_one = || -> Result<(GbmBuffer, GbmFramebuffer)> {
        let mut bo = allocator
            .create_buffer_with_flags(
                width as u32,
                height as u32,
                Fourcc::Xrgb8888,
                &modifiers,
                flags,
            )
            .context(
                "failed to create LINEAR GBM buffer object (often means no DRM master — is the greeter VT active?)",
            )?;
        let stride = bo.stride();
        bo.map_mut(0, 0, width.into(), height.into(), |mapping| {
            render::fill_background(mapping.buffer_mut(), stride, width as u32, height as u32);
        })
        .context("failed to map GBM buffer object")?;
        let fb = make_fb(fd, &bo)?;
        Ok((bo, fb))
    };

    let (bo0, fb0) = make_one()?;
    let (bo1, fb1) = make_one()?;
    Ok(([bo0, bo1], [fb0, fb1]))
}

fn try_open_gpu_scanout(
    gbm: &GbmDevice<DrmDeviceFd>,
    fd: &DrmDeviceFd,
    plane_formats: &FormatSet,
    width: u16,
    height: u16,
    sizes: render::FontSizes,
    background_style: render::BackgroundStyle,
) -> Result<([GbmBuffer; 2], [GbmFramebuffer; 2], Option<GpuRenderer>)> {
    let mut gpu = GpuRenderer::new(gbm, sizes, background_style)?;
    let egl_formats = gpu.egl_render_formats().clone();
    let mut allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let flags = GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT;

    let mut last_err = anyhow!("no GPU color format worked for greeter scanout");
    for &format in &GPU_COLOR_FORMATS {
        let plane_mods = modifiers_for_format(plane_formats, format);
        let egl_mods = modifiers_for_format(&egl_formats, format);
        let mut modifiers = intersect_modifiers(&plane_mods, &egl_mods);
        if modifiers.is_empty() {
            // Last-ditch: allow the driver to pick (Invalid) or force Linear.
            modifiers = vec![Modifier::Invalid, Modifier::Linear];
        }

        tracing::info!(
            ?format,
            plane_mods = plane_mods.len(),
            egl_mods = egl_mods.len(),
            chosen_mods = modifiers.len(),
            "trying greeter GPU scanout format"
        );

        let make_one =
            |allocator: &mut GbmAllocator<DrmDeviceFd>| -> Result<(GbmBuffer, GbmFramebuffer)> {
                let bo = allocator
                    .create_buffer_with_flags(
                        width as u32,
                        height as u32,
                        format,
                        &modifiers,
                        flags,
                    )
                    .with_context(|| {
                        format!("failed to allocate greeter GBM buffer for {format:?}")
                    })?;
                let fb = make_fb(fd, &bo)?;
                Ok((bo, fb))
            };

        let pair = (|| -> Result<_> {
            let (bo0, fb0) = make_one(&mut allocator)?;
            let (bo1, fb1) = make_one(&mut allocator)?;
            let mut probe = bo0
                .export()
                .context("failed to export GPU scanout buffer for bind probe")?;
            gpu.probe_bind(&mut probe)
                .context("GPU scanout dmabuf cannot be bound as a GLES framebuffer")?;
            Ok(([bo0, bo1], [fb0, fb1]))
        })();

        match pair {
            Ok((bos, fbs)) => {
                tracing::info!(?format, "greeter GPU scanout format selected");
                return Ok((bos, fbs, Some(gpu)));
            }
            Err(e) => {
                tracing::warn!(?format, error = ?e, "greeter GPU scanout format failed");
                last_err = e;
            }
        }
    }

    Err(last_err)
}

fn select_background_style() -> render::BackgroundStyle {
    match std::env::var("FOCALDM_BACKGROUND") {
        Ok(value) if value.eq_ignore_ascii_case("aurora") => {
            return render::BackgroundStyle::Aurora;
        }
        Ok(value) if value.eq_ignore_ascii_case("spiral") => {
            return render::BackgroundStyle::SpiralTunnel;
        }
        Ok(value) if value.eq_ignore_ascii_case("random") => {}
        Ok(value) => tracing::warn!(
            value,
            "unknown FOCALDM_BACKGROUND; expected aurora, spiral, or random"
        ),
        Err(_) => {}
    }

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    if (nanos ^ u128::from(std::process::id())) & 1 == 0 {
        render::BackgroundStyle::Aurora
    } else {
        render::BackgroundStyle::SpiralTunnel
    }
}

fn configured_vt() -> Option<i32> {
    for key in ["FOCALDM_VT", "XDG_VTNR"] {
        if let Ok(raw) = std::env::var(key) {
            if let Ok(vt) = raw.parse::<i32>() {
                if vt > 0 {
                    return Some(vt);
                }
            }
        }
    }
    None
}

/// If libseat did not already receive Enable (session not on the active VT),
/// request a switch to the configured greeter VT. focaldmd should have done
/// this already; this is a backup for respawn races.
fn ensure_session_active(session: &mut LibSeatSession) -> Result<()> {
    if session.is_active() {
        tracing::info!("libseat session already active");
        return Ok(());
    }

    let Some(vt) = configured_vt() else {
        tracing::warn!(
            "libseat session inactive and FOCALDM_VT/XDG_VTNR unset — DRM master may fail"
        );
        return Ok(());
    };

    tracing::warn!(
        vt,
        "libseat session inactive at start; requesting VT switch"
    );
    session
        .change_vt(vt)
        .map_err(|e| anyhow!("VT switch to {vt} failed: {e:?}"))?;

    // Give logind/libseat a moment to complete the switch. Enable is normally
    // observed via the session notifier once it is in the event loop; for
    // TakeDevice during open() we rely on focaldmd's prior VT_ACTIVATE.
    std::thread::sleep(Duration::from_millis(150));
    tracing::info!(
        active = session.is_active(),
        vt,
        "after greeter VT switch request"
    );
    Ok(())
}
