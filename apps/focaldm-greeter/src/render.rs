//! High-level greeter frame composition.
//!
//! The greeter is deliberately software-rendered, so this module owns the
//! whole visual language: background pulse, centered lock panel, readable
//! text, and a clickable power menu.

use std::f32::consts::TAU;

use smithay::backend::renderer::gles::{GlesError, GlesFrame, Uniform};
use smithay::backend::renderer::Color32F;
use smithay::utils::{Physical, Rectangle, Transform};

use crate::font;
use crate::glyph_atlas::{FontId, GlyphAtlas, IconId};
use crate::ipc_client::AuthMessageStyle;
use crate::login::LoginState;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    Suspend,
    Hibernate,
    Restart,
    PowerOff,
}

impl PowerAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Suspend => "Suspend",
            Self::Hibernate => "Hibernate",
            Self::Restart => "Restart",
            Self::PowerOff => "Shut down",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct FrameHitTargets {
    pub power_button: Rect,
    pub power_menu_items: Vec<(PowerAction, Rect)>,
    pub field: Rect,
}

pub struct FrameState<'a> {
    pub login: &'a LoginState,
    pub pointer: Option<(i32, i32)>,
    pub power_menu_open: bool,
    pub pulse_phase: f32,
    pub paint_background: bool,
}

fn blend_channel(dst: u8, src: u8, alpha: u8) -> u8 {
    let alpha = alpha as u16;
    let inv = 255u16.saturating_sub(alpha);
    (((dst as u16 * inv) + (src as u16 * alpha)) / 255) as u8
}

fn blend_pixel(buf: &mut [u8], pitch: u32, x: i32, y: i32, color: (u8, u8, u8), alpha: u8) {
    if alpha == 0 || x < 0 || y < 0 {
        return;
    }

    let x = x as u32;
    let y = y as u32;
    let offset = (y * pitch + x * 4) as usize;
    if offset + 4 > buf.len() {
        return;
    }

    buf[offset] = blend_channel(buf[offset], color.2, alpha);
    buf[offset + 1] = blend_channel(buf[offset + 1], color.1, alpha);
    buf[offset + 2] = blend_channel(buf[offset + 2], color.0, alpha);
    buf[offset + 3] = 0;
}

fn baseline_from_top(font: &font::FontFace, size: f32, top: i32) -> i32 {
    let ascent = font
        .horizontal_line_metrics(size)
        .map(|metrics| metrics.ascent)
        .unwrap_or(size);
    top + ascent.round() as i32
}

fn line_step(font: &font::FontFace, size: f32) -> i32 {
    font::line_height(font, size).ceil() as i32
}

fn fill_rect(buf: &mut [u8], pitch: u32, rect: Rect, color: (u8, u8, u8), alpha: u8) {
    if rect.w <= 0 || rect.h <= 0 {
        return;
    }
    for y in rect.y..rect.y + rect.h {
        for x in rect.x..rect.x + rect.w {
            blend_pixel(buf, pitch, x, y, color, alpha);
        }
    }
}

fn draw_border(
    buf: &mut [u8],
    pitch: u32,
    rect: Rect,
    thickness: i32,
    color: (u8, u8, u8),
    alpha: u8,
) {
    let t = thickness.max(1).min(rect.w.max(1)).min(rect.h.max(1));
    fill_rect(
        buf,
        pitch,
        Rect {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: t,
        },
        color,
        alpha,
    );
    fill_rect(
        buf,
        pitch,
        Rect {
            x: rect.x,
            y: rect.y + rect.h - t,
            w: rect.w,
            h: t,
        },
        color,
        alpha,
    );
    fill_rect(
        buf,
        pitch,
        Rect {
            x: rect.x,
            y: rect.y,
            w: t,
            h: rect.h,
        },
        color,
        alpha,
    );
    fill_rect(
        buf,
        pitch,
        Rect {
            x: rect.x + rect.w - t,
            y: rect.y,
            w: t,
            h: rect.h,
        },
        color,
        alpha,
    );
}

// Classic arrow-pointer silhouette (tip + notch + tail flourish), traced as a
// single non-self-intersecting polygon. The hotspot is the tip at (0, 0),
// matching `hover()`'s use of the raw pointer coordinate as the click point.
pub(crate) const CURSOR_POLY: &[(f32, f32)] = &[
    (0.0, 0.0),
    (0.0, 17.0),
    (4.0, 13.0),
    (7.0, 20.0),
    (9.0, 19.0),
    (6.0, 12.0),
    (11.0, 12.0),
];

// Even-odd scanline fill so the arrow polygon can be reused both for the
// white body and, offset by a pixel in every direction, as a dark outline
// that keeps it legible over any background color.
fn fill_polygon(
    buf: &mut [u8],
    pitch: u32,
    points: &[(f32, f32)],
    ox: i32,
    oy: i32,
    color: (u8, u8, u8),
    alpha: u8,
) {
    let (Some(min_y), Some(max_y)) = (
        points.iter().map(|p| p.1).reduce(f32::min),
        points.iter().map(|p| p.1).reduce(f32::max),
    ) else {
        return;
    };

    for y in min_y.floor() as i32..=max_y.ceil() as i32 {
        let yf = y as f32 + 0.5;
        let mut xs: Vec<f32> = Vec::new();
        for i in 0..points.len() {
            let (x1, y1) = points[i];
            let (x2, y2) = points[(i + 1) % points.len()];
            if (y1 <= yf && y2 > yf) || (y2 <= yf && y1 > yf) {
                xs.push(x1 + (yf - y1) / (y2 - y1) * (x2 - x1));
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());

        for pair in xs.chunks_exact(2) {
            for x in pair[0].round() as i32..pair[1].round() as i32 {
                blend_pixel(buf, pitch, ox + x, oy + y, color, alpha);
            }
        }
    }
}

fn draw_cursor(buf: &mut [u8], pitch: u32, x: i32, y: i32) {
    for (dx, dy) in [
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ] {
        fill_polygon(buf, pitch, CURSOR_POLY, x + dx, y + dy, (12, 14, 18), 235);
    }
    fill_polygon(buf, pitch, CURSOR_POLY, x, y, (255, 255, 255), 255);
}

fn draw_shadow(buf: &mut [u8], pitch: u32, rect: Rect) {
    for layer in 0..4 {
        let inset = layer * 2;
        let alpha = 40u8.saturating_sub(layer as u8 * 8);
        fill_rect(
            buf,
            pitch,
            Rect {
                x: rect.x - 8 + inset,
                y: rect.y - 8 + inset,
                w: rect.w + 16 - inset * 2,
                h: rect.h + 16 - inset * 2,
            },
            (0, 0, 0),
            alpha,
        );
    }
}

fn draw_circle(
    buf: &mut [u8],
    pitch: u32,
    cx: i32,
    cy: i32,
    radius: i32,
    color: (u8, u8, u8),
    alpha: u8,
) {
    let r2 = radius * radius;
    for y in cy - radius..=cy + radius {
        for x in cx - radius..=cx + radius {
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy <= r2 {
                blend_pixel(buf, pitch, x, y, color, alpha);
            }
        }
    }
}

fn draw_circle_ring(
    buf: &mut [u8],
    pitch: u32,
    cx: i32,
    cy: i32,
    outer_r: i32,
    inner_r: i32,
    color: (u8, u8, u8),
    alpha: u8,
) {
    let outer_sq = outer_r * outer_r;
    let inner_sq = inner_r.max(0) * inner_r.max(0);
    for y in cy - outer_r..=cy + outer_r {
        for x in cx - outer_r..=cx + outer_r {
            let dx = x - cx;
            let dy = y - cy;
            let dist_sq = dx * dx + dy * dy;
            if dist_sq <= outer_sq && dist_sq >= inner_sq {
                blend_pixel(buf, pitch, x, y, color, alpha);
            }
        }
    }
}

fn draw_line(
    buf: &mut [u8],
    pitch: u32,
    mut x0: i32,
    mut y0: i32,
    x1: i32,
    y1: i32,
    thickness: i32,
    color: (u8, u8, u8),
    alpha: u8,
) {
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;

    loop {
        fill_rect(
            buf,
            pitch,
            Rect {
                x: x0 - thickness / 2,
                y: y0 - thickness / 2,
                w: thickness,
                h: thickness,
            },
            color,
            alpha,
        );
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

fn draw_power_icon(buf: &mut [u8], pitch: u32, rect: Rect, color: (u8, u8, u8), alpha: u8) {
    let cx = rect.x + rect.w / 2;
    let cy = rect.y + rect.h / 2 - 1;
    let outer = rect.w.min(rect.h) / 2 - 4;
    let inner = outer - 4;
    draw_circle_ring(buf, pitch, cx, cy, outer, inner, color, alpha);
    draw_line(
        buf,
        pitch,
        cx,
        cy - outer + 2,
        cx,
        cy - inner + 1,
        3,
        color,
        alpha,
    );
}

fn background_color(x: i32, y: i32, w: i32, h: i32, phase: f32) -> (u8, u8, u8) {
    let fx = x as f32 / w.max(1) as f32;
    let fy = y as f32 / h.max(1) as f32;

    let c1x = 0.32 + phase.sin() * 0.05;
    let c1y = 0.28 + (phase * 1.2).cos() * 0.04;
    let c2x = 0.72 + (phase * 0.8).cos() * 0.03;
    let c2y = 0.74 + (phase * 1.1).sin() * 0.05;

    let d1 = ((fx - c1x).powi(2) + (fy - c1y).powi(2)) / (0.42 * 0.42);
    let d2 = ((fx - c2x).powi(2) + (fy - c2y).powi(2)) / (0.36 * 0.36);
    let glow1 = (1.0 - d1).clamp(0.0, 1.0);
    let glow1 = glow1 * glow1 * (0.68 + glow1 * 0.32);
    let glow2 = (1.0 - d2).clamp(0.0, 1.0);
    let glow2 = glow2 * glow2 * (0.74 + glow2 * 0.26);
    let vignette = (1.0 - (((fx - 0.5).powi(2) + (fy - 0.48).powi(2)) * 1.55)).clamp(0.0, 1.0);
    let scan = (fy * 12.0 + phase * 6.0).sin() * 0.014;

    let base = [12.0, 17.0, 28.0];
    let blue = [24.0, 61.0, 97.0];
    let teal = [34.0, 123.0, 137.0];
    let amber = [210.0, 145.0, 56.0];

    let mut r = base[0]
        + blue[0] * glow1
        + teal[0] * glow2 * 0.7
        + amber[0] * (glow2 * 0.15)
        + 20.0 * vignette
        + 255.0 * scan;
    let mut g = base[1]
        + blue[1] * glow1
        + teal[1] * glow2 * 0.7
        + amber[1] * (glow2 * 0.15)
        + 26.0 * vignette
        + 255.0 * scan;
    let mut b = base[2]
        + blue[2] * glow1
        + teal[2] * glow2 * 0.7
        + amber[2] * (glow2 * 0.15)
        + 32.0 * vignette
        + 255.0 * scan;

    let shadow = ((1.0 - vignette) * 18.0).max(0.0);
    r += shadow;
    g += shadow;
    b += shadow;

    (
        r.clamp(0.0, 255.0) as u8,
        g.clamp(0.0, 255.0) as u8,
        b.clamp(0.0, 255.0) as u8,
    )
}

fn paint_background(buf: &mut [u8], pitch: u32, width: i32, height: i32, phase: f32) {
    let _phase_sin = phase.sin();
    let _phase_cos = phase.cos();
    for y in 0..height {
        for x in 0..width {
            let (r, g, b) = background_color(x, y, width, height, phase);
            blend_pixel(buf, pitch, x, y, (r, g, b), 255);
        }
    }
}

pub fn fill_background(buf: &mut [u8], pitch: u32, width: u32, height: u32) {
    paint_background(buf, pitch, width as i32, height as i32, 0.0);
}

fn prompt_line(state: &LoginState) -> String {
    match state {
        LoginState::EnterUsername {
            error: Some(msg), ..
        } => msg.clone(),
        LoginState::EnterUsername { error: None, .. } => "unlock your session".to_string(),
        LoginState::Waiting { .. } => "authenticating".to_string(),
        LoginState::Prompt { message, .. } => message.clone(),
        LoginState::Done => "starting session".to_string(),
    }
}

fn headline(state: &LoginState) -> &'static str {
    match state {
        LoginState::EnterUsername { error: None, .. } => "Welcome back",
        LoginState::EnterUsername { error: Some(_), .. } => "Authentication failed",
        LoginState::Waiting { .. } => "Checking credentials",
        LoginState::Prompt { .. } => "Enter passphrase",
        LoginState::Done => "Launching session",
    }
}

fn subtitle(state: &LoginState) -> String {
    match state {
        LoginState::EnterUsername { username, .. } if username.is_empty() => {
            "Type your username and press Enter".to_string()
        }
        LoginState::EnterUsername { username, .. } => format!("User: {username}"),
        LoginState::Waiting { username } => format!("Authenticating {username}"),
        LoginState::Prompt { username, .. } => format!("Session for {username}"),
        LoginState::Done => "Please wait".to_string(),
    }
}

fn field_text(state: &LoginState) -> String {
    match state {
        LoginState::EnterUsername { username, .. } => username.clone(),
        LoginState::Prompt { input, style, .. } => {
            if *style == AuthMessageStyle::Secret {
                "•".repeat(input.chars().count())
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

fn draw_spinner(buf: &mut [u8], pitch: u32, cx: i32, cy: i32, phase: f32, color: (u8, u8, u8)) {
    for i in 0..8 {
        let angle = phase * 1.6 + i as f32 * TAU / 8.0;
        let px = cx as f32 + angle.cos() * 12.0;
        let py = cy as f32 + angle.sin() * 12.0;
        let alpha = 70 + ((i as f32 + phase * 4.0).sin().max(0.0) * 185.0) as u8;
        draw_circle(
            buf,
            pitch,
            px.round() as i32,
            py.round() as i32,
            3,
            color,
            alpha,
        );
    }
}

fn draw_text_block(
    buf: &mut [u8],
    pitch: u32,
    x: i32,
    baseline_y: i32,
    size: f32,
    color: (u8, u8, u8),
    font: &font::FontFace,
    text: &str,
) {
    font::draw_text(buf, pitch, x, baseline_y, size, color, font, text);
}

fn center_x(w: i32, text_w: f32) -> i32 {
    ((w as f32 - text_w) * 0.5).round() as i32
}

fn hover(pointer: Option<(i32, i32)>, rect: Rect) -> bool {
    pointer.is_some_and(|(x, y)| rect.contains(x, y))
}

/// The set of (font, size) combinations `paint_frame` ever draws text at.
/// Derived purely from the fixed output height, so — since the DRM mode
/// never changes for the life of the process — this is the exact, complete
/// set of glyph sizes the greeter will ever need. `glyph_atlas` bakes
/// exactly these sizes; keeping the computation here as the single source
/// of truth means the atlas can never drift out of sync with what
/// `paint_frame` actually asks for.
#[derive(Clone, Copy, Debug)]
pub struct FontSizes {
    pub title: f32,
    pub avatar: f32,
    pub body: f32,
    pub field: f32,
    pub small: f32,
}

pub fn font_sizes(height: u32) -> FontSizes {
    let height = height as f32;
    let title = (height * 0.043).clamp(24.0, 38.0);
    FontSizes {
        title,
        avatar: title * 0.95,
        body: (height * 0.023).clamp(15.0, 20.0),
        field: (height * 0.031).clamp(18.0, 28.0),
        small: (height * 0.018).clamp(12.0, 16.0),
    }
}

pub fn paint_frame(
    buf: &mut [u8],
    pitch: u32,
    width: u32,
    height: u32,
    state: &FrameState,
) -> FrameHitTargets {
    let width = width as i32;
    let height = height as i32;
    if state.paint_background {
        paint_background(buf, pitch, width, height, state.pulse_phase);
    }

    let panel_w = (width as f32 * 0.42).clamp(420.0, 700.0) as i32;
    let panel_h = (height as f32 * 0.34).clamp(270.0, 400.0) as i32;
    let panel = Rect {
        x: ((width - panel_w) / 2).max(32),
        y: ((height - panel_h) / 2 + 22).max(48),
        w: panel_w.min(width - 48),
        h: panel_h,
    };

    let power_button = Rect {
        x: width - 92,
        y: 28,
        w: 56,
        h: 56,
    };

    draw_shadow(buf, pitch, panel);
    fill_rect(buf, pitch, panel, (11, 16, 25), 232);
    draw_border(buf, pitch, panel, 2, (147, 172, 193), 84);
    fill_rect(
        buf,
        pitch,
        Rect {
            x: panel.x,
            y: panel.y,
            w: 5,
            h: panel.h,
        },
        (77, 152, 218),
        140,
    );
    fill_rect(
        buf,
        pitch,
        Rect {
            x: panel.x,
            y: panel.y,
            w: panel.w,
            h: 1,
        },
        (255, 255, 255),
        24,
    );

    let regular = font::regular();
    let medium = font::medium();
    let sizes = font_sizes(height as u32);
    let title_size = sizes.title;
    let body_size = sizes.body;
    let field_size = sizes.field;
    let small_size = sizes.small;

    let avatar_x = panel.x + 46;
    let avatar_y = panel.y + 52;
    let avatar_color = match state.login {
        LoginState::EnterUsername { error: None, .. } => (72, 152, 220),
        LoginState::EnterUsername { error: Some(_), .. } => (220, 92, 80),
        LoginState::Waiting { .. } => (220, 180, 64),
        LoginState::Prompt { .. } => (74, 170, 150),
        LoginState::Done => (80, 196, 116),
    };
    draw_circle(buf, pitch, avatar_x, avatar_y, 27, avatar_color, 200);
    draw_circle_ring(buf, pitch, avatar_x, avatar_y, 30, 24, (255, 255, 255), 80);
    let avatar_char = subtitle(state.login)
        .chars()
        .find(|c| c.is_ascii_alphanumeric())
        .unwrap_or('f')
        .to_ascii_uppercase()
        .to_string();
    draw_text_block(
        buf,
        pitch,
        avatar_x - 9,
        avatar_y + 12,
        title_size * 0.95,
        (255, 255, 255),
        medium,
        &avatar_char,
    );

    let title_x = panel.x + 96;
    let title_y = baseline_from_top(medium, title_size, panel.y + 34);
    draw_text_block(
        buf,
        pitch,
        title_x,
        title_y,
        title_size,
        (238, 243, 248),
        medium,
        headline(state.login),
    );

    let subtitle = subtitle(state.login);
    let subtitle_top = panel.y + 34 + line_step(medium, title_size);
    let subtitle_y = baseline_from_top(regular, body_size, subtitle_top);
    draw_text_block(
        buf,
        pitch,
        title_x,
        subtitle_y,
        body_size,
        (170, 182, 194),
        regular,
        &font::ellipsize(regular, body_size, &subtitle, panel.w - 130),
    );

    if matches!(state.login, LoginState::Waiting { .. }) {
        draw_spinner(
            buf,
            pitch,
            panel.x + panel.w - 58,
            panel.y + 62,
            state.pulse_phase,
            (220, 180, 64),
        );
    }

    let prompt = prompt_line(state.login);
    let prompt = font::ellipsize(regular, body_size, &prompt, panel.w - 64);
    let prompt_baseline = baseline_from_top(regular, body_size, panel.y + panel.h - 124);
    draw_text_block(
        buf,
        pitch,
        panel.x + 30,
        prompt_baseline,
        body_size,
        (197, 207, 218),
        regular,
        &prompt,
    );

    let mut notice_y = prompt_baseline + 24;
    for (style, msg) in notices(state.login) {
        let color = match style {
            AuthMessageStyle::Error => (230, 110, 100),
            _ => (155, 170, 182),
        };
        let msg = font::ellipsize(regular, small_size, msg, panel.w - 60);
        let notice_baseline = baseline_from_top(regular, small_size, notice_y);
        draw_text_block(
            buf,
            pitch,
            panel.x + 30,
            notice_baseline,
            small_size,
            color,
            regular,
            &msg,
        );
        notice_y += line_step(regular, small_size);
    }

    let field = Rect {
        x: panel.x + 28,
        y: panel.y + panel.h - 82,
        w: panel.w - 56,
        h: 48,
    };
    let field_active = matches!(
        state.login,
        LoginState::EnterUsername { .. } | LoginState::Prompt { .. }
    );
    let field_hover = hover(state.pointer, field);
    let field_fill = if field_active || field_hover {
        204
    } else {
        180
    };
    fill_rect(buf, pitch, field, (19, 26, 39), field_fill);
    draw_border(
        buf,
        pitch,
        field,
        2,
        if field_active {
            (94, 156, 221)
        } else {
            (118, 132, 149)
        },
        if field_hover { 150 } else { 96 },
    );

    let shown = field_text(state.login);
    let field_inner_x = field.x + 18;
    let field_baseline = baseline_from_top(medium, field_size, field.y + 10);
    if !shown.is_empty() {
        draw_text_block(
            buf,
            pitch,
            field_inner_x,
            field_baseline,
            field_size,
            (245, 247, 250),
            medium,
            &shown,
        );
    } else {
        let placeholder = match state.login {
            LoginState::EnterUsername { .. } => "Username".to_string(),
            LoginState::Prompt { style, .. } => match style {
                AuthMessageStyle::Secret => "Password".to_string(),
                _ => "Response".to_string(),
            },
            _ => String::new(),
        };
        draw_text_block(
            buf,
            pitch,
            field_inner_x,
            field_baseline,
            field_size,
            (113, 130, 146),
            regular,
            &placeholder,
        );
    }

    if field_active && state.pulse_phase.fract() < 0.55 {
        let caret_x =
            field_inner_x + font::measure_width(medium, field_size, &shown).round() as i32 + 2;
        fill_rect(
            buf,
            pitch,
            Rect {
                x: caret_x,
                y: field.y + 11,
                w: 2,
                h: 26,
            },
            (245, 247, 250),
            220,
        );
    }

    let note_y = panel.y + panel.h + 24;
    if let LoginState::EnterUsername {
        error: Some(msg), ..
    } = state.login
    {
        let msg = font::ellipsize(regular, body_size, msg, width - 64);
        let msg_x = center_x(width, font::measure_width(regular, body_size, &msg));
        let msg_baseline = baseline_from_top(regular, body_size, note_y);
        draw_text_block(
            buf,
            pitch,
            msg_x,
            msg_baseline,
            body_size,
            (228, 92, 86),
            regular,
            &msg,
        );
    } else if matches!(state.login, LoginState::Done) {
        let msg = "Handing off to the session...";
        let msg_x = center_x(width, font::measure_width(regular, body_size, msg));
        let msg_baseline = baseline_from_top(regular, body_size, note_y);
        draw_text_block(
            buf,
            pitch,
            msg_x,
            msg_baseline,
            body_size,
            (160, 174, 187),
            regular,
            msg,
        );
    } else {
        let msg = "Esc cancels. Click the power icon for power options.";
        let msg_x = center_x(width, font::measure_width(regular, small_size, msg));
        let msg_baseline = baseline_from_top(regular, small_size, note_y);
        draw_text_block(
            buf,
            pitch,
            msg_x,
            msg_baseline,
            small_size,
            (152, 166, 178),
            regular,
            msg,
        );
    }

    let power_hover = hover(state.pointer, power_button);
    draw_shadow(buf, pitch, power_button);
    fill_rect(
        buf,
        pitch,
        power_button,
        if state.power_menu_open {
            (26, 35, 48)
        } else {
            (14, 20, 31)
        },
        238,
    );
    draw_border(
        buf,
        pitch,
        power_button,
        2,
        if power_hover || state.power_menu_open {
            (233, 240, 247)
        } else {
            (149, 166, 183)
        },
        if power_hover || state.power_menu_open {
            160
        } else {
            96
        },
    );
    draw_power_icon(
        buf,
        pitch,
        power_button,
        if power_hover || state.power_menu_open {
            (237, 243, 249)
        } else {
            (168, 184, 199)
        },
        215,
    );

    let mut power_menu_items = Vec::new();
    if state.power_menu_open {
        let actions = [
            PowerAction::Suspend,
            PowerAction::Hibernate,
            PowerAction::Restart,
            PowerAction::PowerOff,
        ];
        let item_h = 40;
        let item_gap = 4;
        let menu_w = 220;
        let menu_h = 16 + actions.len() as i32 * item_h + (actions.len() as i32 - 1) * item_gap;
        let menu_x = (power_button.x + power_button.w - menu_w).max(24);
        let mut menu_y = power_button.y + power_button.h + 12;
        if menu_y + menu_h > height - 24 {
            menu_y = power_button.y - menu_h - 12;
        }
        let menu = Rect {
            x: menu_x,
            y: menu_y,
            w: menu_w,
            h: menu_h,
        };
        draw_shadow(buf, pitch, menu);
        fill_rect(buf, pitch, menu, (10, 15, 24), 244);
        draw_border(buf, pitch, menu, 2, (116, 134, 153), 108);

        let mut y = menu.y + 8;
        for action in actions {
            let item = Rect {
                x: menu.x + 8,
                y,
                w: menu.w - 16,
                h: item_h,
            };
            let item_hover = hover(state.pointer, item);
            if item_hover {
                fill_rect(buf, pitch, item, (33, 44, 60), 230);
                draw_border(buf, pitch, item, 1, (97, 154, 221), 120);
            }
            draw_text_block(
                buf,
                pitch,
                item.x + 16,
                item.y + 27,
                body_size,
                if item_hover {
                    (245, 247, 250)
                } else {
                    (211, 219, 227)
                },
                regular,
                action.label(),
            );
            power_menu_items.push((action, item));
            y += item_h + item_gap;
        }
    }

    if let Some((px, py)) = state.pointer {
        draw_cursor(buf, pitch, px, py);
    }

    FrameHitTargets {
        power_button,
        power_menu_items,
        field,
    }
}

fn to_unit(color: (u8, u8, u8)) -> [f32; 3] {
    [
        color.0 as f32 / 255.0,
        color.1 as f32 / 255.0,
        color.2 as f32 / 255.0,
    ]
}

fn premultiplied(color: (u8, u8, u8), alpha: u8) -> Color32F {
    let a = alpha as f32 / 255.0;
    let [r, g, b] = to_unit(color);
    Color32F::new(r * a, g * a, b * a, a)
}

fn physical(rect: Rect) -> Rectangle<i32, Physical> {
    Rectangle::new(
        (rect.x, rect.y).into(),
        (rect.w.max(0), rect.h.max(0)).into(),
    )
}

fn gpu_solid(
    frame: &mut GlesFrame,
    rect: Rect,
    color: (u8, u8, u8),
    alpha: u8,
) -> Result<(), GlesError> {
    if rect.w <= 0 || rect.h <= 0 || alpha == 0 {
        return Ok(());
    }
    let dst = physical(rect);
    // Smithay treats damage as dest-local (origin at `dst.loc`), not absolute
    // screen coords. Passing `dst` itself clips any non-origin rect to empty.
    let damage = Rectangle::from_size(dst.size);
    frame.draw_solid(dst, &[damage], premultiplied(color, alpha))
}

fn gpu_border(
    frame: &mut GlesFrame,
    rect: Rect,
    thickness: i32,
    color: (u8, u8, u8),
    alpha: u8,
) -> Result<(), GlesError> {
    let t = thickness.max(1).min(rect.w.max(1)).min(rect.h.max(1));
    gpu_solid(
        frame,
        Rect {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: t,
        },
        color,
        alpha,
    )?;
    gpu_solid(
        frame,
        Rect {
            x: rect.x,
            y: rect.y + rect.h - t,
            w: rect.w,
            h: t,
        },
        color,
        alpha,
    )?;
    gpu_solid(
        frame,
        Rect {
            x: rect.x,
            y: rect.y,
            w: t,
            h: rect.h,
        },
        color,
        alpha,
    )?;
    gpu_solid(
        frame,
        Rect {
            x: rect.x + rect.w - t,
            y: rect.y,
            w: t,
            h: rect.h,
        },
        color,
        alpha,
    )?;
    Ok(())
}

fn gpu_shadow(frame: &mut GlesFrame, rect: Rect) -> Result<(), GlesError> {
    for layer in 0..4i32 {
        let inset = layer * 2;
        let alpha = 40u8.saturating_sub(layer as u8 * 8);
        gpu_solid(
            frame,
            Rect {
                x: rect.x - 8 + inset,
                y: rect.y - 8 + inset,
                w: rect.w + 16 - inset * 2,
                h: rect.h + 16 - inset * 2,
            },
            (0, 0, 0),
            alpha,
        )?;
    }
    Ok(())
}

fn gpu_icon(
    frame: &mut GlesFrame,
    atlas: &GlyphAtlas,
    id: IconId,
    dst: Rect,
    color: (u8, u8, u8),
    alpha: u8,
) -> Result<(), GlesError> {
    let atlas_rect = atlas.icon_rect(id);
    if atlas_rect.w == 0 || atlas_rect.h == 0 || alpha == 0 {
        return Ok(());
    }
    let src = atlas.icon_src(id);
    let dst_rect = physical(dst);
    let damage = Rectangle::from_size(dst_rect.size);
    let uniforms = [Uniform::new("u_color", to_unit(color))];
    frame.render_texture_from_to(
        atlas.texture(),
        src,
        dst_rect,
        &[damage],
        &[],
        Transform::Normal,
        alpha as f32 / 255.0,
        Some(atlas.program()),
        &uniforms,
    )
}

fn gpu_text(
    frame: &mut GlesFrame,
    atlas: &GlyphAtlas,
    font_id: FontId,
    size: f32,
    x: i32,
    baseline_y: i32,
    color: (u8, u8, u8),
    text: &str,
) -> Result<(), GlesError> {
    let uniforms = [Uniform::new("u_color", to_unit(color))];
    let mut caret = x as f32;
    for ch in text.chars() {
        let Some(info) = atlas.glyph(font_id, size, ch) else {
            // Only reachable for text the atlas didn't bake for (PAM
            // messages aren't guaranteed ASCII/Latin-1 the way user-typed
            // input is, see `glyph_atlas::baked_chars`). Drop the glyph
            // rather than the whole frame; log so a systematically missing
            // character is diagnosable instead of silently invisible.
            tracing::debug!(char = ?ch, "greeter glyph atlas miss, skipping glyph");
            continue;
        };
        if let (Some(rect), Some(src)) = (info.rect, atlas.glyph_src(info)) {
            let gx = caret as i32 + info.xmin;
            let gy = baseline_y - info.ymin - rect.h as i32;
            let dst = physical(Rect {
                x: gx,
                y: gy,
                w: rect.w as i32,
                h: rect.h as i32,
            });
            let damage = Rectangle::from_size(dst.size);
            frame.render_texture_from_to(
                atlas.texture(),
                src,
                dst,
                &[damage],
                &[],
                Transform::Normal,
                1.0,
                Some(atlas.program()),
                &uniforms,
            )?;
        }
        caret += info.advance;
    }
    Ok(())
}

/// GPU-drawing counterpart to [`paint_frame`]: same visual layout (panel,
/// avatar, text, field, power menu, cursor), but every element is drawn via
/// `Frame::draw_solid`/`Frame::render_texture_from_to` against a pre-baked
/// [`GlyphAtlas`] instead of CPU pixel blending. The background gradient is
/// expected to already have been drawn into `frame` (via the pixel shader)
/// before this is called — it composites the UI on top within the same
/// GLES frame, so the caller never needs to CPU-map the scanout buffer.
///
/// Layout math (panel/field/power-button geometry, notice stepping, etc.)
/// is intentionally duplicated from `paint_frame` rather than shared: it's
/// small, stable arithmetic, and keeping the two paint functions fully
/// independent means a bug in the GPU path can never affect the CPU
/// fallback that unsupported hardware relies on.
pub fn paint_frame_gpu(
    frame: &mut GlesFrame,
    atlas: &GlyphAtlas,
    width: u32,
    height: u32,
    state: &FrameState,
) -> Result<FrameHitTargets, GlesError> {
    let width = width as i32;
    let height = height as i32;

    let panel_w = (width as f32 * 0.42).clamp(420.0, 700.0) as i32;
    let panel_h = (height as f32 * 0.34).clamp(270.0, 400.0) as i32;
    let panel = Rect {
        x: ((width - panel_w) / 2).max(32),
        y: ((height - panel_h) / 2 + 22).max(48),
        w: panel_w.min(width - 48),
        h: panel_h,
    };

    let power_button = Rect {
        x: width - 92,
        y: 28,
        w: 56,
        h: 56,
    };

    gpu_shadow(frame, panel)?;
    gpu_solid(frame, panel, (11, 16, 25), 232)?;
    gpu_border(frame, panel, 2, (147, 172, 193), 84)?;
    gpu_solid(
        frame,
        Rect {
            x: panel.x,
            y: panel.y,
            w: 5,
            h: panel.h,
        },
        (77, 152, 218),
        140,
    )?;
    gpu_solid(
        frame,
        Rect {
            x: panel.x,
            y: panel.y,
            w: panel.w,
            h: 1,
        },
        (255, 255, 255),
        24,
    )?;

    let regular = font::regular();
    let medium = font::medium();
    let sizes = font_sizes(height as u32);
    let title_size = sizes.title;
    let body_size = sizes.body;
    let field_size = sizes.field;
    let small_size = sizes.small;

    let avatar_x = panel.x + 46;
    let avatar_y = panel.y + 52;
    let avatar_color = match state.login {
        LoginState::EnterUsername { error: None, .. } => (72, 152, 220),
        LoginState::EnterUsername { error: Some(_), .. } => (220, 92, 80),
        LoginState::Waiting { .. } => (220, 180, 64),
        LoginState::Prompt { .. } => (74, 170, 150),
        LoginState::Done => (80, 196, 116),
    };
    gpu_icon(
        frame,
        atlas,
        IconId::AvatarDisc,
        Rect {
            x: avatar_x - 27,
            y: avatar_y - 27,
            w: 55,
            h: 55,
        },
        avatar_color,
        200,
    )?;
    gpu_icon(
        frame,
        atlas,
        IconId::AvatarRing,
        Rect {
            x: avatar_x - 30,
            y: avatar_y - 30,
            w: 61,
            h: 61,
        },
        (255, 255, 255),
        80,
    )?;
    let avatar_char = subtitle(state.login)
        .chars()
        .find(|c| c.is_ascii_alphanumeric())
        .unwrap_or('f')
        .to_ascii_uppercase()
        .to_string();
    gpu_text(
        frame,
        atlas,
        FontId::Medium,
        sizes.avatar,
        avatar_x - 9,
        avatar_y + 12,
        (255, 255, 255),
        &avatar_char,
    )?;

    let title_x = panel.x + 96;
    let title_y = baseline_from_top(medium, title_size, panel.y + 34);
    gpu_text(
        frame,
        atlas,
        FontId::Medium,
        title_size,
        title_x,
        title_y,
        (238, 243, 248),
        headline(state.login),
    )?;

    let subtitle_text = subtitle(state.login);
    let subtitle_top = panel.y + 34 + line_step(medium, title_size);
    let subtitle_y = baseline_from_top(regular, body_size, subtitle_top);
    gpu_text(
        frame,
        atlas,
        FontId::Regular,
        body_size,
        title_x,
        subtitle_y,
        (170, 182, 194),
        &font::ellipsize(regular, body_size, &subtitle_text, panel.w - 130),
    )?;

    if matches!(state.login, LoginState::Waiting { .. }) {
        let cx = panel.x + panel.w - 58;
        let cy = panel.y + 62;
        for i in 0..8 {
            let angle = state.pulse_phase * 1.6 + i as f32 * TAU / 8.0;
            let px = cx as f32 + angle.cos() * 12.0;
            let py = cy as f32 + angle.sin() * 12.0;
            let alpha = 70 + ((i as f32 + state.pulse_phase * 4.0).sin().max(0.0) * 185.0) as u8;
            gpu_icon(
                frame,
                atlas,
                IconId::SpinnerDot,
                Rect {
                    x: px.round() as i32 - 3,
                    y: py.round() as i32 - 3,
                    w: 7,
                    h: 7,
                },
                (220, 180, 64),
                alpha,
            )?;
        }
    }

    let prompt = prompt_line(state.login);
    let prompt = font::ellipsize(regular, body_size, &prompt, panel.w - 64);
    let prompt_baseline = baseline_from_top(regular, body_size, panel.y + panel.h - 124);
    gpu_text(
        frame,
        atlas,
        FontId::Regular,
        body_size,
        panel.x + 30,
        prompt_baseline,
        (197, 207, 218),
        &prompt,
    )?;

    let mut notice_y = prompt_baseline + 24;
    for (style, msg) in notices(state.login) {
        let color = match style {
            AuthMessageStyle::Error => (230, 110, 100),
            _ => (155, 170, 182),
        };
        let msg = font::ellipsize(regular, small_size, msg, panel.w - 60);
        let notice_baseline = baseline_from_top(regular, small_size, notice_y);
        gpu_text(
            frame,
            atlas,
            FontId::Regular,
            small_size,
            panel.x + 30,
            notice_baseline,
            color,
            &msg,
        )?;
        notice_y += line_step(regular, small_size);
    }

    let field = Rect {
        x: panel.x + 28,
        y: panel.y + panel.h - 82,
        w: panel.w - 56,
        h: 48,
    };
    let field_active = matches!(
        state.login,
        LoginState::EnterUsername { .. } | LoginState::Prompt { .. }
    );
    let field_hover = hover(state.pointer, field);
    let field_fill = if field_active || field_hover {
        204
    } else {
        180
    };
    gpu_solid(frame, field, (19, 26, 39), field_fill)?;
    gpu_border(
        frame,
        field,
        2,
        if field_active {
            (94, 156, 221)
        } else {
            (118, 132, 149)
        },
        if field_hover { 150 } else { 96 },
    )?;

    let shown = field_text(state.login);
    let field_inner_x = field.x + 18;
    let field_baseline = baseline_from_top(medium, field_size, field.y + 10);
    if !shown.is_empty() {
        gpu_text(
            frame,
            atlas,
            FontId::Medium,
            field_size,
            field_inner_x,
            field_baseline,
            (245, 247, 250),
            &shown,
        )?;
    } else {
        let placeholder = match state.login {
            LoginState::EnterUsername { .. } => "Username".to_string(),
            LoginState::Prompt { style, .. } => match style {
                AuthMessageStyle::Secret => "Password".to_string(),
                _ => "Response".to_string(),
            },
            _ => String::new(),
        };
        gpu_text(
            frame,
            atlas,
            FontId::Regular,
            field_size,
            field_inner_x,
            field_baseline,
            (113, 130, 146),
            &placeholder,
        )?;
    }

    if field_active && state.pulse_phase.fract() < 0.55 {
        let caret_x =
            field_inner_x + font::measure_width(medium, field_size, &shown).round() as i32 + 2;
        gpu_solid(
            frame,
            Rect {
                x: caret_x,
                y: field.y + 11,
                w: 2,
                h: 26,
            },
            (245, 247, 250),
            220,
        )?;
    }

    let note_y = panel.y + panel.h + 24;
    if let LoginState::EnterUsername {
        error: Some(msg), ..
    } = state.login
    {
        let msg = font::ellipsize(regular, body_size, msg, width - 64);
        let msg_x = center_x(width, font::measure_width(regular, body_size, &msg));
        let msg_baseline = baseline_from_top(regular, body_size, note_y);
        gpu_text(
            frame,
            atlas,
            FontId::Regular,
            body_size,
            msg_x,
            msg_baseline,
            (228, 92, 86),
            &msg,
        )?;
    } else if matches!(state.login, LoginState::Done) {
        let msg = "Handing off to the session...";
        let msg_x = center_x(width, font::measure_width(regular, body_size, msg));
        let msg_baseline = baseline_from_top(regular, body_size, note_y);
        gpu_text(
            frame,
            atlas,
            FontId::Regular,
            body_size,
            msg_x,
            msg_baseline,
            (160, 174, 187),
            msg,
        )?;
    } else {
        let msg = "Esc cancels. Click the power icon for power options.";
        let msg_x = center_x(width, font::measure_width(regular, small_size, msg));
        let msg_baseline = baseline_from_top(regular, small_size, note_y);
        gpu_text(
            frame,
            atlas,
            FontId::Regular,
            small_size,
            msg_x,
            msg_baseline,
            (152, 166, 178),
            msg,
        )?;
    }

    let power_hover = hover(state.pointer, power_button);
    gpu_shadow(frame, power_button)?;
    gpu_solid(
        frame,
        power_button,
        if state.power_menu_open {
            (26, 35, 48)
        } else {
            (14, 20, 31)
        },
        238,
    )?;
    gpu_border(
        frame,
        power_button,
        2,
        if power_hover || state.power_menu_open {
            (233, 240, 247)
        } else {
            (149, 166, 183)
        },
        if power_hover || state.power_menu_open {
            160
        } else {
            96
        },
    )?;
    gpu_icon(
        frame,
        atlas,
        IconId::PowerIcon,
        power_button,
        if power_hover || state.power_menu_open {
            (237, 243, 249)
        } else {
            (168, 184, 199)
        },
        215,
    )?;

    let mut power_menu_items = Vec::new();
    if state.power_menu_open {
        let actions = [
            PowerAction::Suspend,
            PowerAction::Hibernate,
            PowerAction::Restart,
            PowerAction::PowerOff,
        ];
        let item_h = 40;
        let item_gap = 4;
        let menu_w = 220;
        let menu_h = 16 + actions.len() as i32 * item_h + (actions.len() as i32 - 1) * item_gap;
        let menu_x = (power_button.x + power_button.w - menu_w).max(24);
        let mut menu_y = power_button.y + power_button.h + 12;
        if menu_y + menu_h > height - 24 {
            menu_y = power_button.y - menu_h - 12;
        }
        let menu = Rect {
            x: menu_x,
            y: menu_y,
            w: menu_w,
            h: menu_h,
        };
        gpu_shadow(frame, menu)?;
        gpu_solid(frame, menu, (10, 15, 24), 244)?;
        gpu_border(frame, menu, 2, (116, 134, 153), 108)?;

        let mut y = menu.y + 8;
        for action in actions {
            let item = Rect {
                x: menu.x + 8,
                y,
                w: menu.w - 16,
                h: item_h,
            };
            let item_hover = hover(state.pointer, item);
            if item_hover {
                gpu_solid(frame, item, (33, 44, 60), 230)?;
                gpu_border(frame, item, 1, (97, 154, 221), 120)?;
            }
            gpu_text(
                frame,
                atlas,
                FontId::Regular,
                body_size,
                item.x + 16,
                item.y + 27,
                if item_hover {
                    (245, 247, 250)
                } else {
                    (211, 219, 227)
                },
                action.label(),
            )?;
            power_menu_items.push((action, item));
            y += item_h + item_gap;
        }
    }

    if let Some((px, py)) = state.pointer {
        let dst = Rect {
            x: px - 1,
            y: py - 1,
            w: 13,
            h: 22,
        };
        gpu_icon(frame, atlas, IconId::CursorOutline, dst, (12, 14, 18), 235)?;
        gpu_icon(frame, atlas, IconId::CursorBody, dst, (255, 255, 255), 255)?;
    }

    Ok(FrameHitTargets {
        power_button,
        power_menu_items,
        field,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroize::Zeroizing;

    #[test]
    #[ignore = "writes a preview PNG for manual visual inspection, not an assertion"]
    fn render_lock_screen_preview() {
        let frame_w = 1280u32;
        let frame_h = 720u32;
        let state0 = LoginState::EnterUsername {
            username: String::new(),
            error: None,
        };
        let state1 = LoginState::EnterUsername {
            username: "steve".to_string(),
            error: Some("authentication failed".to_string()),
        };
        let state2 = LoginState::Prompt {
            username: "steve".to_string(),
            style: AuthMessageStyle::Secret,
            message: "Password:".to_string(),
            input: Zeroizing::new("hunter2".to_string()),
            notices: vec![(AuthMessageStyle::Info, "password expires soon".to_string())],
        };
        let states = [
            FrameState {
                login: &state0,
                pointer: Some((0, 0)),
                power_menu_open: false,
                pulse_phase: 0.0,
                paint_background: true,
            },
            FrameState {
                login: &state1,
                pointer: Some((0, 0)),
                power_menu_open: false,
                pulse_phase: 0.8,
                paint_background: true,
            },
            FrameState {
                login: &state2,
                pointer: Some((0, 0)),
                power_menu_open: true,
                pulse_phase: 1.2,
                paint_background: true,
            },
        ];

        let pitch = frame_w * 4;
        let total_h = frame_h * states.len() as u32;
        let mut buf = vec![0u8; (pitch * total_h) as usize];

        for (i, state) in states.iter().enumerate() {
            let y_off = frame_h * i as u32;
            let frame_start = (y_off * pitch) as usize;
            let frame_end = ((y_off + frame_h) * pitch) as usize;
            paint_frame(
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

        let path = std::env::temp_dir().join("focaldm_greeter_lock_screen_preview.png");
        image::save_buffer(&path, &rgba, frame_w, total_h, image::ColorType::Rgba8)
            .expect("failed to save preview PNG");
        println!("wrote lock screen preview to {}", path.display());
    }
}
