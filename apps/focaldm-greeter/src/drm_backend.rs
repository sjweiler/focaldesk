// Standalone DRM/KMS scanout for the greeter, deliberately independent of
// focaldesk-engine's backend::drm (which is 3000+ lines of GBM/EGL/GLES/
// wayland-server machinery for the real compositor). The greeter only ever
// needs to show a login box on one output, so it uses legacy (non-atomic)
// modesetting with a single CPU-mapped GBM buffer for scanout: no GPU-
// accelerated rendering, no wayland surfaces. Ported from focaldesk-greeter's
// drm_backend.rs, which this mirrors closely — see drm-rs's own
// `examples/legacy_modeset.rs`. Buffers come from GBM rather than the
// simpler dumb-buffer ioctls because the proprietary NVIDIA driver doesn't
// implement dumb buffers (ENOSYS) but does support GBM allocation.
//
// Known corners cut, deliberately, to keep this a first working pass:
// - Text is drawn with the real IBM Plex Sans rasterizer in `crate::font`,
//   but still via CPU software rendering rather than GPU acceleration.
// - CRTC selection takes resource_handles().crtcs()[0] rather than checking
//   the connector's encoder `possible_crtcs` bitmask. Fine for one GPU/one
//   output; would misbehave on more exotic multi-GPU setups.
// - No xkbcommon layout composition; keycodes come from `crate::keymap`'s
//   fixed US-QWERTY table.

use anyhow::{anyhow, Context, Result};
use smithay::backend::drm::DrmDeviceFd;
use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf};
use smithay::backend::allocator::gbm::GbmBuffer;
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::renderer::gles::{
    GlesPixelProgram, GlesRenderer, Uniform, UniformName, UniformType,
};
use smithay::backend::renderer::{Bind, Frame, Renderer};
use smithay::backend::session::{
    libseat::LibSeatSession, libseat::LibSeatSessionNotifier, Session,
};
use smithay::backend::udev::primary_gpu;
use smithay::reexports::drm::buffer::DrmFourcc;
use smithay::reexports::drm::control::{
    connector, crtc, framebuffer, Device as ControlDevice, Event, Mode, ModeTypeFlags,
    PageFlipFlags,
};
use smithay::reexports::gbm::{BufferObjectFlags, Device as GbmDevice};
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{Buffer, DeviceFd, Physical, Rectangle, Size, Transform};

use crate::render;

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
    fbs: [framebuffer::Handle; 2],
    front: usize,
    flip_pending: bool,
    gpu: Option<GpuBackground>,
}

struct GpuBackground {
    renderer: GlesRenderer,
    program: GlesPixelProgram,
}

impl GpuBackground {
    fn new(gbm: &GbmDevice<DrmDeviceFd>) -> Result<Self> {
        let display = unsafe { EGLDisplay::new(gbm.clone()) }
            .context("failed to create EGL display for greeter")?;
        let context = EGLContext::new(&display)
            .context("failed to create EGL context for greeter")?;
        let mut renderer = unsafe { GlesRenderer::new(context) }
            .context("failed to create GLES renderer for greeter")?;

        let program = renderer
            .compile_custom_pixel_shader(
                GREETER_BACKGROUND_FRAG,
                &[
                    UniformName::new("u_resolution", UniformType::_2f),
                    UniformName::new("u_time", UniformType::_1f),
                ],
            )
            .context("failed to compile greeter background shader")?;

        Ok(Self {
            renderer,
            program,
        })
    }

    fn render_into(
        &mut self,
        dmabuf: &mut Dmabuf,
        width: u32,
        height: u32,
        phase: f32,
    ) -> Result<()> {
        let rect_f = Rectangle::<f64, Buffer>::new((0.0, 0.0).into(), (width as f64, height as f64).into());
        let rect_i = Rectangle::<i32, Physical>::new((0, 0).into(), (width as i32, height as i32).into());
        let buffer_size = Size::<i32, Buffer>::from((width as i32, height as i32));
        let physical_size = Size::<i32, Physical>::from((width as i32, height as i32));

        let mut target = self.renderer.bind(dmabuf).context("failed to bind greeter scanout dmabuf")?;
        let mut frame = self
            .renderer
            .render(&mut target, physical_size, Transform::Normal)
            .context("failed to begin greeter shader frame")?;

        let uniforms = [
            Uniform::new("u_resolution", [width as f32, height as f32]),
            Uniform::new("u_time", phase),
        ];

        frame
            .render_pixel_shader_to(
                &self.program,
                rect_f,
                rect_i,
                buffer_size,
                Some(std::slice::from_ref(&rect_i)),
                1.0,
                &uniforms,
            )
            .context("failed to draw greeter shader background")?;
        let sync = frame.finish().context("failed to finish greeter shader frame")?;
        self.renderer.wait(&sync).context("failed to wait for greeter shader frame")?;
        Ok(())
    }
}

impl GreeterOutput {
    /// Opens the primary GPU via libseat, mode-sets the first connected
    /// output at its preferred mode, and hands back the pieces the caller
    /// needs to register with calloop: the session notifier (Pause/Activate
    /// events) and a libinput context (keyboard/pointer events).
    pub fn open() -> Result<(Self, LibSeatSessionNotifier, Libinput)> {
        let (mut session, notifier) = LibSeatSession::new()
            .map_err(|e| anyhow!("could not initialize libseat session: {e}"))?;

        let gpu_path = primary_gpu(session.seat())
            .context("failed to enumerate GPUs for seat")?
            .ok_or_else(|| anyhow!("no primary GPU found for seat {}", session.seat()))?;

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
        let crtc_info: Vec<crtc::Info> = res
            .crtcs()
            .iter()
            .flat_map(|handle| fd.get_crtc(*handle))
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

        let crtc_handle = crtc_info
            .first()
            .ok_or_else(|| anyhow!("no CRTCs available on {}", gpu_path.display()))?
            .handle();

        // Legacy dumb buffers aren't implemented by the proprietary NVIDIA
        // DRM driver (ENOSYS on create_dumb_buffer); GBM buffer objects are
        // the allocation path that works across i915/amdgpu/nouveau *and*
        // nvidia-drm, so the greeter uses those instead even though it's
        // still doing plain CPU-mapped software rendering, not GPU-accelerated.
        let gbm = GbmDevice::new(fd.clone()).context("failed to create GBM device")?;

        let (width, height) = mode.size();
        let make_buffer =
            |gbm: &GbmDevice<DrmDeviceFd>| -> Result<(GbmBuffer, framebuffer::Handle)> {
                let mut bo = gbm
                    .create_buffer_object::<()>(
                        width.into(),
                        height.into(),
                        DrmFourcc::Xrgb8888,
                        BufferObjectFlags::SCANOUT
                            | BufferObjectFlags::WRITE
                            | BufferObjectFlags::LINEAR,
                    )
                    .context("failed to create GBM buffer object")?;
                let stride = bo.stride();
                bo.map_mut(0, 0, width.into(), height.into(), |mapping| {
                    render::fill_background(
                        mapping.buffer_mut(),
                        stride,
                        width as u32,
                        height as u32,
                    );
                })
                .context("failed to map GBM buffer object")?;
                let gbm_bo = GbmBuffer::from_bo(bo, true);
                let fb = fd
                    .add_framebuffer(&gbm_bo, 24, 32)
                    .context("failed to create framebuffer")?;
                Ok((gbm_bo, fb))
            };

        let (bo0, fb0) = make_buffer(&gbm)?;
        let (bo1, fb1) = make_buffer(&gbm)?;

        let gpu = GpuBackground::new(&gbm).ok();

        fd.set_crtc(crtc_handle, Some(fb0), (0, 0), &[con.handle()], Some(mode))
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
            bos: [bo0, bo1],
            fbs: [fb0, fb1],
            front: 0,
            flip_pending: false,
            gpu,
        };

        Ok((output, notifier, libinput))
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

    pub fn gpu_background_enabled(&self) -> bool {
        self.gpu.is_some()
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
                Some(self.fbs[self.front]),
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
        let stride = self.bos[back].stride();
        let mut dmabuf = if self.gpu.is_some() {
            Some(
                self.bos[back]
                    .export()
                    .context("failed to export greeter scanout buffer")?,
            )
        } else {
            None
        };

        let background_ok = if let (Some(gpu), Some(dmabuf)) = (self.gpu.as_mut(), dmabuf.as_mut())
        {
            gpu.render_into(dmabuf, width as u32, height as u32, state.pulse_phase)
                .is_ok()
        } else {
            false
        };
        drop(dmabuf);

        let layout = self.bos[back]
            .map_mut(0, 0, width.into(), height.into(), |mapping| {
                let buf = mapping.buffer_mut();
                let frame_state = render::FrameState {
                    login: state.login,
                    pointer: state.pointer,
                    power_menu_open: state.power_menu_open,
                    pulse_phase: state.pulse_phase,
                    paint_background: !background_ok,
                };

                render::paint_frame(buf, stride, width as u32, height as u32, &frame_state)
            })
            .context("failed to map GBM buffer object for render")?;

        self.fd
            .page_flip(self.crtc, self.fbs[back], PageFlipFlags::EVENT, None)
            .context("failed to queue page flip")?;

        self.front = back;
        self.flip_pending = true;
        Ok(layout)
    }
}
