// Standalone DRM/KMS scanout for the greeter, deliberately independent of
// focaldesk-engine's backend::drm (which is 3000+ lines of GBM/EGL/GLES/
// wayland-server machinery for the real compositor). The greeter only ever
// needs to show a login box on one output, so it uses legacy dumb-buffer
// modesetting instead: no GPU-accelerated rendering, no atomic KMS, no
// wayland surfaces. Ported from focaldesk-greeter's drm_backend.rs, which
// this mirrors closely — see drm-rs's own `examples/legacy_modeset.rs`.
//
// Known corners cut, deliberately, to keep this a first working pass:
// - Text is drawn with the small hand-authored bitmap font in `crate::font`,
//   not a real rasterizer (no hinting/antialiasing/proportional spacing).
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
use smithay::reexports::drm::buffer::{Buffer as DrmBuffer, DrmFourcc};
use smithay::reexports::drm::control::{
    connector, crtc, dumbbuffer::DumbBuffer, framebuffer, Device as ControlDevice, Mode,
    ModeTypeFlags,
};
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::DeviceFd;

use crate::login::LoginState;
use crate::render;

pub struct GreeterOutput {
    session: LibSeatSession,
    fd: DrmDeviceFd,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    dumb: DumbBuffer,
    fb: framebuffer::Handle,
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

        let (width, height) = mode.size();
        let mut dumb = fd
            .create_dumb_buffer((width.into(), height.into()), DrmFourcc::Xrgb8888, 32)
            .context("failed to create dumb buffer")?;

        {
            let pitch = dumb.pitch();
            let mut mapping = fd
                .map_dumb_buffer(&mut dumb)
                .context("failed to map dumb buffer")?;
            render::fill_background(mapping.as_mut(), pitch, width as u32, height as u32);
        }

        let fb = fd
            .add_framebuffer(&dumb, 24, 32)
            .context("failed to create framebuffer")?;

        fd.set_crtc(crtc_handle, Some(fb), (0, 0), &[con.handle()], Some(mode))
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
            crtc: crtc_handle,
            connector: con.handle(),
            mode,
            dumb,
            fb,
        };

        Ok((output, notifier, libinput))
    }

    pub fn change_vt(&mut self, vt: i32) -> Result<()> {
        self.session
            .change_vt(vt)
            .map_err(|e| anyhow!("VT switch to {vt} failed: {e:?}"))
    }

    /// Re-applies the CRTC after a `SessionEvent::ActivateSession`. Another
    /// process may have taken DRM master and scanned out something else
    /// while we were paused; this is a best-effort re-assertion, not a full
    /// atomic-KMS resume path.
    pub fn reassert_scanout(&mut self) -> Result<()> {
        self.fd
            .set_crtc(
                self.crtc,
                Some(self.fb),
                (0, 0),
                &[self.connector],
                Some(self.mode),
            )
            .context("failed to re-set CRTC on session resume")
    }

    pub fn render(&mut self, state: &LoginState) -> Result<()> {
        let (width, height) = self.mode.size();
        let pitch = self.dumb.pitch();
        let mut mapping = self
            .fd
            .map_dumb_buffer(&mut self.dumb)
            .context("failed to map dumb buffer for render")?;
        render::paint_login_box(mapping.as_mut(), pitch, width as u32, height as u32, state);
        Ok(())
    }
}
