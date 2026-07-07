//! Composes one frame of the login box into a BGRX8888 dumb buffer, driven
//! by `LoginState` from `crate::login`. Ported from focaldesk-greeter's
//! drm_backend.rs paint routine, retargeted from that app's greetd-shaped
//! `LoginPhase` onto focaldmd's `LoginState`.

use crate::font;
use crate::ipc_client::AuthMessageStyle;
use crate::login::LoginState;

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

pub fn fill_background(buf: &mut [u8], pitch: u32, width: u32, height: u32) {
    fill_rect(buf, pitch, (0, 0, width, height), (0x18, 0x18, 0x1c));
}

fn accent_color(state: &LoginState) -> (u8, u8, u8) {
    match state {
        LoginState::EnterUsername { error: None, .. } => (0x4a, 0x90, 0xd9),
        LoginState::EnterUsername { error: Some(_), .. } => (0xe7, 0x4c, 0x3c),
        LoginState::Waiting { .. } => (0xf1, 0xc4, 0x0f),
        LoginState::Prompt { .. } => (0x9b, 0x59, 0xb6),
        LoginState::Done => (0x2e, 0xcc, 0x71),
    }
}

fn prompt_line(state: &LoginState) -> String {
    match state {
        LoginState::EnterUsername {
            error: Some(msg), ..
        } => msg.clone(),
        LoginState::EnterUsername { error: None, .. } => "login:".to_string(),
        LoginState::Waiting { .. } => "authenticating...".to_string(),
        LoginState::Prompt { message, .. } => message.clone(),
        LoginState::Done => "starting session...".to_string(),
    }
}

/// The text currently being typed: the username field before submission, or
/// the PAM prompt's answer field afterwards. Masked with `*` for secret
/// prompts. Empty while waiting on the daemon — there is nothing to edit.
fn field_text(state: &LoginState) -> String {
    match state {
        LoginState::EnterUsername { username, .. } => username.clone(),
        LoginState::Prompt { input, style, .. } => {
            if *style == AuthMessageStyle::Secret {
                "*".repeat(input.chars().count())
            } else {
                input.to_string()
            }
        }
        LoginState::Waiting { .. } | LoginState::Done => String::new(),
    }
}

fn notices(state: &LoginState) -> &[(AuthMessageStyle, String)] {
    match state {
        LoginState::Prompt { notices, .. } => notices,
        _ => &[],
    }
}

/// Truncates `text` so it never renders past `max_width` pixels — PAM
/// messages are arbitrary-length and, unclipped, would overrun the frame
/// and wrap pixels into the scanline below (real corruption on a real
/// framebuffer, not just a cosmetic clip).
fn truncate_to_width(text: &str, scale: u32, max_width: u32) -> String {
    let advance = (font::GLYPH_WIDTH + 1) * scale;
    let max_chars = (max_width / advance).max(1) as usize;
    text.chars().take(max_chars).collect()
}

pub fn paint_login_box(buf: &mut [u8], pitch: u32, width: u32, height: u32, state: &LoginState) {
    fill_background(buf, pitch, width, height);
    let box_w = width / 3;
    let box_h = height / 8;
    let x0 = (width - box_w) / 2;
    let y0 = (height - box_h) / 2;
    let accent = accent_color(state);
    draw_rect_border(buf, pitch, (x0, y0, box_w, box_h), 4, accent);

    // Crude resolution-relative scale: 1x at 480p tall, up from there.
    let scale = (height / 240).max(2);
    let glyph_h = font::GLYPH_HEIGHT * scale;
    let line_h = glyph_h + glyph_h / 4;
    // Prompt/notices are captions above the box, so they're clipped and
    // centered against the full frame width, not the (much narrower) box —
    // otherwise a long PAM message would either overrun the frame (wrapping
    // pixels into the row below) or, if centered on the box's origin
    // instead of the frame's, overrun on the right even after clipping.
    let max_caption_w = width.saturating_sub(40);
    let max_field_w = box_w.saturating_sub(16);

    let prompt = truncate_to_width(&prompt_line(state), scale, max_caption_w);
    let prompt_w = font::text_width(&prompt, scale);
    let prompt_x = width.saturating_sub(prompt_w) / 2;
    let prompt_y = y0.saturating_sub(glyph_h + glyph_h / 2);
    font::draw_text(buf, pitch, prompt_x, prompt_y, scale, accent, &prompt);

    // Notices (e.g. a password-expiry warning) stack upward above the prompt.
    for (i, (style, msg)) in notices(state).iter().enumerate() {
        let color = match style {
            AuthMessageStyle::Error => (0xe7, 0x4c, 0x3c),
            _ => (0x95, 0x95, 0x9c),
        };
        let msg = truncate_to_width(msg, scale, max_caption_w);
        let ny = prompt_y.saturating_sub((i as u32 + 1) * line_h);
        let nw = font::text_width(&msg, scale);
        let nx = width.saturating_sub(nw) / 2;
        font::draw_text(buf, pitch, nx, ny, scale, color, &msg);
    }

    let shown = truncate_to_width(&field_text(state), scale, max_field_w);
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
    use zeroize::Zeroizing;

    /// Same rationale as focaldesk-greeter's font/drm_backend preview tests:
    /// no way to view real DRM scanout from this environment, so the
    /// composed box is checked by rendering each state to a stacked PNG and
    /// looking at it. Run explicitly with:
    ///   cargo test -p focaldm-greeter -- --ignored render_login_box_preview --nocapture
    #[test]
    #[ignore = "writes a preview PNG for manual visual inspection, not an assertion"]
    fn render_login_box_preview() {
        let frame_w = 480u32;
        let frame_h = 270u32;
        let states = [
            LoginState::EnterUsername {
                username: "steve".to_string(),
                error: None,
            },
            LoginState::Waiting {
                username: "steve".to_string(),
            },
            LoginState::Prompt {
                username: "steve".to_string(),
                style: AuthMessageStyle::Secret,
                message: "Password:".to_string(),
                input: Zeroizing::new("hunter2".to_string()),
                notices: vec![(
                    AuthMessageStyle::Info,
                    "your password expires in 3 days".to_string(),
                )],
            },
            LoginState::EnterUsername {
                username: String::new(),
                error: Some("authentication failed".to_string()),
            },
            LoginState::Done,
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

        let path = std::env::temp_dir().join("focaldm_greeter_login_box_preview.png");
        image::save_buffer(&path, &rgba, frame_w, total_h, image::ColorType::Rgba8)
            .expect("failed to save preview PNG");
        println!("wrote login box preview to {}", path.display());
    }
}
