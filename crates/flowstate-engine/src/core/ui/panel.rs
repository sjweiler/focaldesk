#![allow(unused_imports)]

// crates/flowstate-engine/src/core/ui/panel.rs

use smithay::utils::{Logical, Physical, Point, Rectangle, Size};

#[derive(Clone, Copy, Debug)]
pub enum AccentEdge {
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug)]
pub struct PanelStyle {
    // logical-space metrics
    pub outer_border: f64,
    pub inner_border: f64,
    pub bevel_inset: f64,
    pub highlight_band: f64,
    pub lower_band: f64,
    pub accent_thickness: f64,
    pub shadow_extent: f64,
    pub bracket_len: f64,
    pub bracket_thickness: f64,
    pub content_inset: f64,
}

impl Default for PanelStyle {
    fn default() -> Self {
        Self {
            outer_border: 1.0,
            inner_border: 1.0,
            bevel_inset: 3.0,
            highlight_band: 4.0,
            lower_band: 5.0,
            accent_thickness: 2.0,
            shadow_extent: 8.0,
            bracket_len: 12.0,
            bracket_thickness: 1.0,
            content_inset: 10.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PanelPalette {
    pub body: [f32; 4],
    pub body_top: [f32; 4],
    pub body_bottom: [f32; 4],
    pub outer_border: [f32; 4],
    pub inner_line: [f32; 4],
    pub accent: [f32; 4],
    pub shadow: [f32; 4],
    pub bracket: [f32; 4],
    pub specular: [f32; 4],
}

impl Default for PanelPalette {
    fn default() -> Self {
        Self {
            body: [0.07, 0.08, 0.10, 0.94],
            body_top: [0.17, 0.19, 0.23, 0.16],
            body_bottom: [0.00, 0.00, 0.00, 0.20],
            outer_border: [0.02, 0.03, 0.04, 1.00],
            inner_line: [0.45, 0.50, 0.58, 0.14],
            accent: [0.24, 0.62, 0.98, 0.70],
            shadow: [0.00, 0.00, 0.00, 0.22],
            bracket: [0.32, 0.36, 0.42, 0.28],
            specular: [0.88, 0.92, 1.00, 0.05],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ResolvedPanelStyle {
    pub outer_border_px: i32,
    pub inner_border_px: i32,
    pub bevel_inset_px: i32,
    pub highlight_band_px: i32,
    pub lower_band_px: i32,
    pub accent_thickness_px: i32,
    pub shadow_extent_px: i32,
    pub bracket_len_px: i32,
    pub bracket_thickness_px: i32,
    pub content_inset_px: i32,
    pub palette: PanelPalette,
}

impl ResolvedPanelStyle {
    pub fn from_logical(style: PanelStyle, palette: PanelPalette, scale: f64) -> Self {
        Self {
            outer_border_px: px(style.outer_border, scale),
            inner_border_px: px(style.inner_border, scale),
            bevel_inset_px: px(style.bevel_inset, scale),
            highlight_band_px: px(style.highlight_band, scale),
            lower_band_px: px(style.lower_band, scale),
            accent_thickness_px: px(style.accent_thickness, scale),
            shadow_extent_px: px(style.shadow_extent, scale),
            bracket_len_px: px(style.bracket_len, scale),
            bracket_thickness_px: px(style.bracket_thickness, scale),
            content_inset_px: px(style.content_inset, scale),
            palette,
        }
    }
}

pub fn px(v: f64, scale: f64) -> i32 {
    ((v * scale).round() as i32).max(1)
}

pub fn inset_rect(rect: Rectangle<i32, Physical>, amt: i32) -> Rectangle<i32, Physical> {
    Rectangle::new(
        (rect.loc.x + amt, rect.loc.y + amt).into(),
        (
            (rect.size.w - amt * 2).max(0),
            (rect.size.h - amt * 2).max(0),
        )
            .into(),
    )
}

pub fn expand_rect(rect: Rectangle<i32, Physical>, amt: i32) -> Rectangle<i32, Physical> {
    Rectangle::new(
        (rect.loc.x - amt, rect.loc.y - amt).into(),
        (rect.size.w + amt * 2, rect.size.h + amt * 2).into(),
    )
}

// Replace this trait with your actual renderer/frame abstraction.
pub trait QuadRenderer {
    fn fill_rect(&mut self, rect: Rectangle<i32, Physical>, color: [f32; 4]);
}

pub fn draw_stroke_rect(
    frame: &mut impl QuadRenderer,
    rect: Rectangle<i32, Physical>,
    stroke: i32,
    color: [f32; 4],
) {
    if rect.size.w <= 0 || rect.size.h <= 0 || stroke <= 0 {
        return;
    }

    frame.fill_rect(
        Rectangle::new(rect.loc, (rect.size.w, stroke).into()),
        color,
    );
    frame.fill_rect(
        Rectangle::new(
            (rect.loc.x, rect.loc.y + rect.size.h - stroke).into(),
            (rect.size.w, stroke).into(),
        ),
        color,
    );
    frame.fill_rect(
        Rectangle::new(rect.loc, (stroke, rect.size.h).into()),
        color,
    );
    frame.fill_rect(
        Rectangle::new(
            (rect.loc.x + rect.size.w - stroke, rect.loc.y).into(),
            (stroke, rect.size.h).into(),
        ),
        color,
    );
}

pub fn draw_outer_shadow(
    frame: &mut impl QuadRenderer,
    rect: Rectangle<i32, Physical>,
    shadow_extent: i32,
    color: [f32; 4],
) {
    let outer = expand_rect(rect, shadow_extent);

    frame.fill_rect(
        Rectangle::new(outer.loc, (outer.size.w, shadow_extent).into()),
        color,
    );
    frame.fill_rect(
        Rectangle::new(
            (outer.loc.x, rect.loc.y + rect.size.h).into(),
            (outer.size.w, shadow_extent).into(),
        ),
        color,
    );
    frame.fill_rect(
        Rectangle::new(
            (outer.loc.x, rect.loc.y).into(),
            (shadow_extent, rect.size.h).into(),
        ),
        color,
    );
    frame.fill_rect(
        Rectangle::new(
            (rect.loc.x + rect.size.w, rect.loc.y).into(),
            (shadow_extent, rect.size.h).into(),
        ),
        color,
    );
}

pub fn draw_corner_brackets(
    frame: &mut impl QuadRenderer,
    rect: Rectangle<i32, Physical>,
    len: i32,
    thickness: i32,
    color: [f32; 4],
) {
    // TL
    frame.fill_rect(Rectangle::new(rect.loc, (len, thickness).into()), color);
    frame.fill_rect(Rectangle::new(rect.loc, (thickness, len).into()), color);

    // TR
    frame.fill_rect(
        Rectangle::new(
            (rect.loc.x + rect.size.w - len, rect.loc.y).into(),
            (len, thickness).into(),
        ),
        color,
    );
    frame.fill_rect(
        Rectangle::new(
            (rect.loc.x + rect.size.w - thickness, rect.loc.y).into(),
            (thickness, len).into(),
        ),
        color,
    );

    // BL
    frame.fill_rect(
        Rectangle::new(
            (rect.loc.x, rect.loc.y + rect.size.h - thickness).into(),
            (len, thickness).into(),
        ),
        color,
    );
    frame.fill_rect(
        Rectangle::new(
            (rect.loc.x, rect.loc.y + rect.size.h - len).into(),
            (thickness, len).into(),
        ),
        color,
    );

    // BR
    frame.fill_rect(
        Rectangle::new(
            (
                rect.loc.x + rect.size.w - len,
                rect.loc.y + rect.size.h - thickness,
            )
                .into(),
            (len, thickness).into(),
        ),
        color,
    );
    frame.fill_rect(
        Rectangle::new(
            (
                rect.loc.x + rect.size.w - thickness,
                rect.loc.y + rect.size.h - len,
            )
                .into(),
            (thickness, len).into(),
        ),
        color,
    );
}

pub fn draw_panel_frame(
    frame: &mut impl QuadRenderer,
    rect: Rectangle<i32, Physical>,
    style: &ResolvedPanelStyle,
    accent_edge: AccentEdge,
) {
    let p = style.palette;

    draw_outer_shadow(frame, rect, style.shadow_extent_px, p.shadow);

    frame.fill_rect(rect, p.body);

    let top_band = Rectangle::new(rect.loc, (rect.size.w, style.highlight_band_px).into());
    frame.fill_rect(top_band, p.body_top);

    let bottom_band = Rectangle::new(
        (rect.loc.x, rect.loc.y + rect.size.h - style.lower_band_px).into(),
        (rect.size.w, style.lower_band_px).into(),
    );
    frame.fill_rect(bottom_band, p.body_bottom);

    draw_stroke_rect(frame, rect, style.outer_border_px, p.outer_border);

    let inner = inset_rect(rect, style.outer_border_px + style.bevel_inset_px);
    draw_stroke_rect(frame, inner, style.inner_border_px, p.inner_line);

    let spec = Rectangle::new(
        (
            rect.loc.x + style.content_inset_px,
            rect.loc.y + style.outer_border_px + style.bevel_inset_px,
        )
            .into(),
        (
            (rect.size.w - style.content_inset_px * 2).max(1),
            style.inner_border_px.max(1),
        )
            .into(),
    );
    frame.fill_rect(spec, p.specular);

    let accent = match accent_edge {
        AccentEdge::Top => Rectangle::new(
            (
                rect.loc.x + style.outer_border_px,
                rect.loc.y + style.outer_border_px,
            )
                .into(),
            (
                (rect.size.w - style.outer_border_px * 2).max(1),
                style.accent_thickness_px,
            )
                .into(),
        ),
        AccentEdge::Bottom => Rectangle::new(
            (
                rect.loc.x + style.outer_border_px,
                rect.loc.y + rect.size.h - style.outer_border_px - style.accent_thickness_px,
            )
                .into(),
            (
                (rect.size.w - style.outer_border_px * 2).max(1),
                style.accent_thickness_px,
            )
                .into(),
        ),
        AccentEdge::Left => Rectangle::new(
            (
                rect.loc.x + style.outer_border_px,
                rect.loc.y + style.outer_border_px,
            )
                .into(),
            (
                style.accent_thickness_px,
                (rect.size.h - style.outer_border_px * 2).max(1),
            )
                .into(),
        ),
        AccentEdge::Right => Rectangle::new(
            (
                rect.loc.x + rect.size.w - style.outer_border_px - style.accent_thickness_px,
                rect.loc.y + style.outer_border_px,
            )
                .into(),
            (
                style.accent_thickness_px,
                (rect.size.h - style.outer_border_px * 2).max(1),
            )
                .into(),
        ),
    };
    frame.fill_rect(accent, p.accent);

    draw_corner_brackets(
        frame,
        rect,
        style.bracket_len_px,
        style.bracket_thickness_px,
        p.bracket,
    );
}
