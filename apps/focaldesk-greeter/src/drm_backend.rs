// Standalone DRM/KMS scanout for the greeter, deliberately independent of
// focaldesk-engine's backend::drm (which is 3000+ lines of GBM/EGL/GLES/
// wayland-server machinery for the real compositor). The greeter only ever
// needs to show a login box on one output, so it uses legacy dumb-buffer
// modesetting instead: no GPU-accelerated rendering, no atomic KMS, no
// wayland surfaces. See drm-rs's own `examples/legacy_modeset.rs`, which this
// mirrors closely.
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

use crate::font;
use crate::state::{LoginPhase, LoginScreenState};

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
            fill_background(mapping.as_mut(), pitch, width as u32, height as u32);
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

    pub fn render(&mut self, state: &LoginScreenState) -> Result<()> {
        let (width, height) = self.mode.size();
        let pitch = self.dumb.pitch();
        let mut mapping = self
            .fd
            .map_dumb_buffer(&mut self.dumb)
            .context("failed to map dumb buffer for render")?;
        paint_login_box(mapping.as_mut(), pitch, width as u32, height as u32, state);
        Ok(())
    }
}

fn put_pixel(buf: &mut [u8], pitch: u32, x: u32, y: u32, color: (u8, u8, u8)) {
    let offset = (y * pitch + x * 4) as usize;
    if offset + 4 <= buf.len() {
        buf[offset] = color.2; // B
        buf[offset + 1] = color.1; // G
        buf[offset + 2] = color.0; // R
        buf[offset + 3] = 0; // X (unused in XRGB8888)
    }
}

fn fill_rect(buf: &mut [u8], pitch: u32, rect: (u32, u32, u32, u32), color: (u8, u8, u8)) {
    let (x0, y0, w, h) = rect;
    for y in y0..y0.saturating_add(h) {
        for x in x0..x0.saturating_add(w) {
            put_pixel(buf, pitch, x, y, color);
        }
    }
}

fn draw_rect_border(
    buf: &mut [u8],
    pitch: u32,
    rect: (u32, u32, u32, u32),
    thickness: u32,
    color: (u8, u8, u8),
) {
    let (x0, y0, w, h) = rect;
    fill_rect(buf, pitch, (x0, y0, w, thickness), color); // top
    fill_rect(buf, pitch, (x0, y0 + h - thickness, w, thickness), color); // bottom
    fill_rect(buf, pitch, (x0, y0, thickness, h), color); // left
    fill_rect(buf, pitch, (x0 + w - thickness, y0, thickness, h), color); // right
}

fn phase_color(phase: &LoginPhase) -> (u8, u8, u8) {
    match phase {
        LoginPhase::EnteringUsername => (0x4a, 0x90, 0xd9),
        LoginPhase::EnteringResponse { .. } => (0x9b, 0x59, 0xb6),
        LoginPhase::Authenticating => (0xf1, 0xc4, 0x0f),
        LoginPhase::Cancelling => (0x95, 0x95, 0x9c),
        LoginPhase::Failed { .. } => (0xe7, 0x4c, 0x3c),
        LoginPhase::Starting => (0x2e, 0xcc, 0x71),
    }
}

fn fill_background(buf: &mut [u8], pitch: u32, width: u32, height: u32) {
    fill_rect(buf, pitch, (0, 0, width, height), (0x18, 0x18, 0x1c));
}

fn paint_login_box(buf: &mut [u8], pitch: u32, width: u32, height: u32, state: &LoginScreenState) {
    fill_background(buf, pitch, width, height);
    let box_w = width / 3;
    let box_h = height / 8;
    let x0 = (width - box_w) / 2;
    let y0 = (height - box_h) / 2;
    let accent = phase_color(&state.phase);
    draw_rect_border(buf, pitch, (x0, y0, box_w, box_h), 4, accent);

    // Crude resolution-relative scale: 1x at 480p tall, up from there.
    let scale = (height / 240).max(2);
    let glyph_h = font::GLYPH_HEIGHT * scale;

    let prompt = state.prompt_text();
    let prompt_w = font::text_width(prompt, scale);
    let prompt_x = x0 + box_w.saturating_sub(prompt_w) / 2;
    let prompt_y = y0.saturating_sub(glyph_h + glyph_h / 2);
    font::draw_text(buf, pitch, prompt_x, prompt_y, scale, accent, prompt);

    let shown: String = if state.is_secret_input() {
        "*".repeat(state.input.chars().count())
    } else {
        state.input.clone()
    };
    if !shown.is_empty() {
        let text_w = font::text_width(&shown, scale);
        let text_x = x0 + box_w.saturating_sub(text_w) / 2;
        let text_y = y0 + box_h.saturating_sub(glyph_h) / 2;
        font::draw_text(
            buf,
            pitch,
            text_x,
            text_y,
            scale,
            (0xf0, 0xf0, 0xf0),
            &shown,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same rationale as `font::tests::render_preview`: no way to view real
    /// DRM scanout from here, so the composed box (border + prompt + masked
    /// input, not just isolated glyphs) is checked by rendering each phase
    /// to a stacked PNG and looking at it. Run explicitly with:
    ///   cargo test -p focaldesk-greeter --bin focaldesk-greeter -- --ignored render_login_box_preview --nocapture
    #[test]
    #[ignore = "writes a preview PNG for manual visual inspection, not an assertion"]
    fn render_login_box_preview() {
        let frame_w = 480u32;
        let frame_h = 270u32;
        let states = [
            LoginScreenState {
                username: String::new(),
                input: "steve".to_string(),
                phase: LoginPhase::EnteringUsername,
            },
            LoginScreenState {
                username: "steve".to_string(),
                input: "hunter2".to_string(),
                phase: LoginPhase::EnteringResponse {
                    secret: true,
                    prompt: "Password:".to_string(),
                },
            },
            LoginScreenState {
                username: String::new(),
                input: String::new(),
                phase: LoginPhase::Failed {
                    message: "authentication failed".to_string(),
                },
            },
            LoginScreenState {
                username: "steve".to_string(),
                input: String::new(),
                phase: LoginPhase::Starting,
            },
            LoginScreenState {
                username: "steve".to_string(),
                input: String::new(),
                phase: LoginPhase::Cancelling,
            },
        ];

        let pitch = frame_w * 4;
        let total_h = frame_h * states.len() as u32;
        let mut buf = vec![0u8; (pitch * total_h) as usize];

        for (i, state) in states.iter().enumerate() {
            let y_off = frame_h * i as u32;
            let frame_start = (y_off * pitch) as usize;
            let frame_end = ((y_off + frame_h) * pitch) as usize;
            paint_login_box(
                &mut buf[frame_start..frame_end],
                pitch,
                frame_w,
                frame_h,
                state,
            );
        }

        let mut rgba = vec![0u8; buf.len()];
        for px in 0..(frame_w * total_h) as usize {
            rgba[px * 4] = buf[px * 4 + 2];
            rgba[px * 4 + 1] = buf[px * 4 + 1];
            rgba[px * 4 + 2] = buf[px * 4];
            rgba[px * 4 + 3] = 255;
        }

        let path = std::env::temp_dir().join("focaldesk_greeter_login_box_preview.png");
        image::save_buffer(&path, &rgba, frame_w, total_h, image::ColorType::Rgba8)
            .expect("failed to save preview PNG");
        println!("wrote login box preview to {}", path.display());
    }
}
