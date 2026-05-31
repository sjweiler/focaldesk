use smithay::utils::{Logical, Physical, Point, Rectangle, Size};

use crate::core::ui::panel::{
    draw_panel_frame, inset_rect, AccentEdge, PanelPalette, PanelStyle, QuadRenderer,
    ResolvedPanelStyle,
};

#[derive(Clone, Copy, Debug)]
pub struct SideBarConfig {
    pub width: f64,
    pub top_offset: f64,
    pub bottom_margin: f64,
    pub module_gap: f64,
    pub header_height: f64,
    pub status_height: f64,
}

impl Default for SideBarConfig {
    fn default() -> Self {
        Self {
            width: 72.0,
            top_offset: 48.0,
            bottom_margin: 12.0,
            module_gap: 10.0,
            header_height: 72.0,
            status_height: 96.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SideBarLayout {
    pub outer: Rectangle<f64, Logical>,
    pub header: Rectangle<f64, Logical>,
    pub slots: Rectangle<f64, Logical>,
    pub status: Rectangle<f64, Logical>,
}

pub fn compute_sidebar_layout(
    output_size: Size<f64, Logical>,
    cfg: &SideBarConfig,
) -> SideBarLayout {
    let outer_h = (output_size.h - cfg.top_offset - cfg.bottom_margin).max(0.0);

    let outer = Rectangle::new(
        Point::from((0.0, cfg.top_offset)),
        Size::from((cfg.width, outer_h)),
    );

    let header = Rectangle::new(
        Point::from((8.0, cfg.top_offset + 8.0)),
        Size::from((cfg.width - 16.0, cfg.header_height)),
    );

    let status = Rectangle::new(
        Point::from((8.0, cfg.top_offset + outer_h - cfg.status_height - 8.0)),
        Size::from((cfg.width - 16.0, cfg.status_height)),
    );

    let slots_y = header.loc.y + header.size.h + cfg.module_gap;
    let slots_h = (status.loc.y - cfg.module_gap - slots_y).max(0.0);

    let slots = Rectangle::new(
        Point::from((8.0, slots_y)),
        Size::from((cfg.width - 16.0, slots_h)),
    );

    SideBarLayout {
        outer,
        header,
        slots,
        status,
    }
}

fn logical_rect_to_physical(rect: Rectangle<f64, Logical>, scale: f64) -> Rectangle<i32, Physical> {
    Rectangle::new(
        rect.loc.to_physical(scale).to_i32_round(),
        rect.size.to_physical(scale).to_i32_round(),
    )
}

pub fn draw_sidebar(frame: &mut impl QuadRenderer, layout: &SideBarLayout, scale: f64) {
    let outer_style = ResolvedPanelStyle::from_logical(
        PanelStyle {
            shadow_extent: 8.0,
            bracket_len: 14.0,
            highlight_band: 2.0,
            lower_band: 6.0,
            accent_thickness: 3.0,
            content_inset: 8.0,
            ..Default::default()
        },
        PanelPalette {
            body: [0.06, 0.07, 0.09, 0.96],
            body_top: [0.14, 0.16, 0.20, 0.10],
            body_bottom: [0.00, 0.00, 0.00, 0.24],
            outer_border: [0.01, 0.02, 0.03, 1.00],
            inner_line: [0.42, 0.48, 0.56, 0.11],
            accent: [0.20, 0.58, 0.96, 0.60],
            shadow: [0.00, 0.00, 0.00, 0.24],
            bracket: [0.28, 0.32, 0.38, 0.24],
            specular: [0.88, 0.92, 1.00, 0.03],
        },
        scale,
    );

    let module_style = ResolvedPanelStyle::from_logical(
        PanelStyle {
            shadow_extent: 3.0,
            bracket_len: 8.0,
            highlight_band: 2.0,
            lower_band: 3.0,
            content_inset: 6.0,
            ..Default::default()
        },
        PanelPalette::default(),
        scale,
    );

    let outer = logical_rect_to_physical(layout.outer, scale);
    let header = logical_rect_to_physical(layout.header, scale);
    let slots = logical_rect_to_physical(layout.slots, scale);
    let status = logical_rect_to_physical(layout.status, scale);

    draw_panel_frame(frame, outer, &outer_style, AccentEdge::Right);
    draw_panel_frame(frame, header, &module_style, AccentEdge::Right);
    draw_panel_frame(frame, slots, &module_style, AccentEdge::Right);
    draw_panel_frame(frame, status, &module_style, AccentEdge::Right);

    draw_slot_segments(frame, slots, scale, &module_style);
    draw_status_lines(frame, status, scale);
}

fn draw_slot_segments(
    frame: &mut impl QuadRenderer,
    rect: Rectangle<i32, Physical>,
    scale: f64,
    style: &ResolvedPanelStyle,
) {
    let inner = inset_rect(rect, style.content_inset_px);
    if inner.size.w <= 0 || inner.size.h <= 0 {
        return;
    }

    let seg_count = 6;
    let line_h = ((1.0 * scale).round() as i32).max(1);
    let seg_h = inner.size.h / seg_count;

    for i in 1..seg_count {
        let y = inner.loc.y + seg_h * i;
        frame.fill_rect(
            Rectangle::new(
                (inner.loc.x + 4, y).into(),
                (inner.size.w - 8, line_h).into(),
            ),
            [0.40, 0.46, 0.54, 0.10],
        );
    }
}

fn draw_status_lines(frame: &mut impl QuadRenderer, rect: Rectangle<i32, Physical>, scale: f64) {
    let line_h = ((1.0 * scale).round() as i32).max(1);
    let pad_x = ((8.0 * scale).round() as i32).max(1);

    let y1 = rect.loc.y + rect.size.h / 3;
    let y2 = rect.loc.y + (rect.size.h * 2) / 3;

    frame.fill_rect(
        Rectangle::new(
            (rect.loc.x + pad_x, y1).into(),
            (rect.size.w - pad_x * 2, line_h).into(),
        ),
        [0.44, 0.50, 0.58, 0.10],
    );

    frame.fill_rect(
        Rectangle::new(
            (rect.loc.x + pad_x, y2).into(),
            (rect.size.w - pad_x * 2, line_h).into(),
        ),
        [0.44, 0.50, 0.58, 0.10],
    );
}
