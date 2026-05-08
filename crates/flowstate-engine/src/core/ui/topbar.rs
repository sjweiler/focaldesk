// crates/flowstate-engine/src/core/ui/topbar.rs

use smithay::utils::{Logical, Physical, Point, Rectangle, Size};

use crate::core::ui::panel::{
    draw_panel_frame, inset_rect, AccentEdge, PanelPalette, PanelStyle, QuadRenderer,
    ResolvedPanelStyle,
};

#[derive(Clone, Copy, Debug)]
pub struct TopBarConfig {
    pub height: f64,
    pub launcher_width: f64,
    pub slots_width: f64,
    pub system_width: f64,
}

impl Default for TopBarConfig {
    fn default() -> Self {
        Self {
            height: 42.0,
            launcher_width: 72.0,
            slots_width: 560.0,
            system_width: 240.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TopBarLayout {
    pub outer: Rectangle<f64, Logical>,
    pub launcher: Rectangle<f64, Logical>,
    pub slots: Rectangle<f64, Logical>,
    pub system: Rectangle<f64, Logical>,
}

pub fn compute_topbar_layout(
    output_size: Size<f64, Logical>,
    cfg: &TopBarConfig,
) -> TopBarLayout {
    let outer = Rectangle::new(
        Point::from((0.0, 0.0)),
        Size::from((output_size.w, cfg.height)),
    );

    let launcher = Rectangle::new(
        Point::from((10.0, 6.0)),
        Size::from((cfg.launcher_width, cfg.height - 12.0)),
    );

    let system = Rectangle::new(
        Point::from((output_size.w - cfg.system_width - 10.0, 6.0)),
        Size::from((cfg.system_width, cfg.height - 12.0)),
    );

    let slots_x = launcher.loc.x + launcher.size.w + 12.0;
    let slots_w = (system.loc.x - 12.0 - slots_x).max(0.0);

    let slots = Rectangle::new(
        Point::from((slots_x, 6.0)),
        Size::from((slots_w, cfg.height - 12.0)),
    );

    TopBarLayout {
        outer,
        launcher,
        slots,
        system,
    }
}

fn logical_rect_to_physical(
    rect: Rectangle<f64, Logical>,
    scale: f64,
) -> Rectangle<i32, Physical> {
    Rectangle::new(
        rect.loc.to_physical(scale).to_i32_round(),
        rect.size.to_physical(scale).to_i32_round(),
    )
}

pub fn draw_topbar(
    frame: &mut impl QuadRenderer,
    layout: &TopBarLayout,
    scale: f64,
) {
    let outer_style = ResolvedPanelStyle::from_logical(
        PanelStyle {
            shadow_extent: 6.0,
            bracket_len: 10.0,
            highlight_band: 3.0,
            lower_band: 4.0,
            ..Default::default()
        },
        PanelPalette::default(),
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
        PanelPalette {
            body:         [0.08, 0.09, 0.11, 0.96],
            body_top:     [0.18, 0.20, 0.24, 0.10],
            body_bottom:  [0.00, 0.00, 0.00, 0.16],
            outer_border: [0.02, 0.03, 0.04, 0.95],
            inner_line:   [0.45, 0.50, 0.58, 0.10],
            accent:       [0.24, 0.62, 0.98, 0.55],
            shadow:       [0.00, 0.00, 0.00, 0.14],
            bracket:      [0.28, 0.32, 0.38, 0.18],
            specular:     [0.90, 0.94, 1.00, 0.03],
        },
        scale,
    );

    let outer = logical_rect_to_physical(layout.outer, scale);
    draw_panel_frame(frame, outer, &outer_style, AccentEdge::Bottom);

    let launcher = logical_rect_to_physical(layout.launcher, scale);
    let slots = logical_rect_to_physical(layout.slots, scale);
    let system = logical_rect_to_physical(layout.system, scale);

    draw_panel_frame(frame, launcher, &module_style, AccentEdge::Bottom);
    draw_panel_frame(frame, slots, &module_style, AccentEdge::Bottom);
    draw_panel_frame(frame, system, &module_style, AccentEdge::Bottom);

    draw_slot_dividers(frame, slots, scale, &module_style);
}

fn draw_slot_dividers(
    frame: &mut impl QuadRenderer,
    slots: Rectangle<i32, Physical>,
    scale: f64,
    style: &ResolvedPanelStyle,
) {
    let count = 9;
    let pad = style.content_inset_px;
    let inner = inset_rect(slots, pad);

    if inner.size.w <= 0 || inner.size.h <= 0 {
        return;
    }

    let divider_w = ((1.0 * scale).round() as i32).max(1);
    let slot_w = inner.size.w / count;

    for i in 1..count {
        let x = inner.loc.x + slot_w * i;
        frame.fill_rect(
            Rectangle::new((x, inner.loc.y + 4).into(), (divider_w, inner.size.h - 8).into()),
            [0.40, 0.46, 0.54, 0.12],
        );
    }
}
