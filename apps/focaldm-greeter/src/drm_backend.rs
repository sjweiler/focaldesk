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
use smithay::backend::session::{
    libseat::LibSeatSession, libseat::LibSeatSessionNotifier, Session,
};
use smithay::backend::udev::primary_gpu;
use smithay::reexports::drm::buffer::DrmFourcc;
use smithay::reexports::drm::control::{
    connector, crtc, framebuffer, Device as ControlDevice, Event, Mode, ModeTypeFlags,
    PageFlipFlags,
};
use smithay::reexports::gbm::{BufferObject, BufferObjectFlags, Device as GbmDevice};
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::DeviceFd;

use crate::render;

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
    bos: [BufferObject<()>; 2],
    fbs: [framebuffer::Handle; 2],
    front: usize,
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
            |gbm: &GbmDevice<DrmDeviceFd>| -> Result<(BufferObject<()>, framebuffer::Handle)> {
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
                let fb = fd
                    .add_framebuffer(&bo, 24, 32)
                    .context("failed to create framebuffer")?;
                Ok((bo, fb))
            };

        let (bo0, fb0) = make_buffer(&gbm)?;
        let (bo1, fb1) = make_buffer(&gbm)?;

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

    pub fn render(&mut self, state: &render::FrameState<'_>) -> Result<render::FrameHitTargets> {
        let back = 1 - self.front;
        let (width, height) = self.mode.size();
        let stride = self.bos[back].stride();
        let layout = self.bos[back]
            .map_mut(0, 0, width.into(), height.into(), |mapping| {
                render::paint_frame(
                    mapping.buffer_mut(),
                    stride,
                    width as u32,
                    height as u32,
                    state,
                )
            })
            .context("failed to map GBM buffer object for render")?;

        self.fd
            .page_flip(self.crtc, self.fbs[back], PageFlipFlags::EVENT, None)
            .context("failed to queue page flip")?;

        // Block for flip completion: only once this lands is `front` (the
        // buffer render just left) guaranteed off-screen and safe to map
        // again on the next call. Renders happen at keystroke rate, not
        // continuously, so one vblank (~16ms) of latency here doesn't matter
        // — and it's strictly better than the alternative of writing into
        // a buffer the display controller might still be scanning.
        loop {
            let events = self
                .fd
                .receive_events()
                .context("failed to read DRM events")?;
            if events.into_iter().any(|e| matches!(e, Event::PageFlip(_))) {
                break;
            }
        }

        self.front = back;
        Ok(layout)
    }
}
