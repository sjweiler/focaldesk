//! GPU chrome shell drawing (top bar, sidebar, work-area bezel).

use focaldesk_themes::{ChromeTheme, FlowTheme};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesPixelProgram, GlesTexProgram, Uniform,
};
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};

use crate::atlas::{IconAtlas, IconId};
use crate::chrome::ChromeMetrics;
use crate::chrome_layout::ChromeLayoutLogical;
use crate::chrome_shaders::ChromeShaders;
use crate::chrome_theme::{
    BevelStyle, ButtonStyle, GlassStyle, LightChannelStyle, TopBarStyle,
    chrome_theme_from_flow_theme,
};
use crate::desktop_frame::DesktopFrameCtx;
use crate::types::UiElementKind;
use crate::uitree::UiTree;
use crate::{UiVisualState, UiVisualStyle};

pub fn draw_chrome_below_work_wallpaper(
    frame: &mut GlesFrame<'_, '_>,
    shaders: &ChromeShaders,
    frame_ctx: &DesktopFrameCtx,
    layout: &ChromeLayoutLogical,
    sidebar_hover_slot: Option<usize>,
    theme: &ChromeTheme,
) {
    let legacy_theme = chrome_theme_from_flow_theme(theme);

    let beveled = shaders
        .beveled_panel
        .clone()
        .expect("beveled_panel shader not compiled");
    let light = shaders
        .light_channel
        .as_ref()
        .expect("light_channel shader not compiled");
    let button = shaders
        .recessed_button
        .as_ref()
        .expect("button shader not compiled");
    let top_bar = shaders
        .top_bar
        .as_ref()
        .expect("top bar shader not compiled");

    let fullscreen_rect: Rectangle<i32, Physical> = Rectangle::from_loc_and_size(
        Point::<i32, Physical>::from((0, 0)),
        Size::<i32, Physical>::from(frame_ctx.output_size),
    );
    let damage = &[fullscreen_rect];

    let _ = draw_top_bar(
        frame,
        top_bar,
        layout.topbar.outer,
        frame_ctx.output_scale,
        damage,
        &legacy_theme.top_bar,
    );

    for rect in [
        layout.topbar.outer,
        layout.topbar.inner,
        layout.sidebar.outer,
        layout.sidebar.inner,
        layout.work_area.outer,
        layout.work_area.inner_frame,
        layout.work_area.recess,
    ] {
        let style = if rect == layout.work_area.recess || rect == layout.sidebar.inner {
            &legacy_theme.panel_inner
        } else if rect == layout.sidebar.outer {
            &legacy_theme.sidebar
        } else if rect == layout.topbar.inner {
            &legacy_theme.frame_inner
        } else {
            &legacy_theme.frame_outer
        };
        let _ = draw_beveled_panel(frame, &beveled, rect, frame_ctx.output_scale, damage, style);
    }

    let _ = draw_beveled_panel(
        frame,
        &beveled,
        layout.topbar.title,
        frame_ctx.output_scale,
        damage,
        &legacy_theme.panel_inner,
    );
    let _ = draw_beveled_panel(
        frame,
        &beveled,
        layout.topbar.trim,
        frame_ctx.output_scale,
        damage,
        &legacy_theme.trim,
    );

    if let Some(rect) = layout.topbar.light {
        let _ = draw_light_channel(
            frame,
            light,
            rect,
            frame_ctx.output_scale,
            damage,
            &legacy_theme.light,
        );
    }

    for rect in &layout.topbar.status_wells {
        let _ = draw_recessed_button(
            frame,
            button,
            *rect,
            frame_ctx.output_scale,
            damage,
            &legacy_theme.button,
        );
        let _ = draw_light_channel(
            frame,
            light,
            inset_rect(*rect, 3),
            frame_ctx.output_scale,
            damage,
            &legacy_theme.light,
        );
    }

    let _ = draw_recessed_button(
        frame,
        button,
        layout.topbar.clock_well,
        frame_ctx.output_scale,
        damage,
        &legacy_theme.button,
    );
    let _ = draw_light_channel(
        frame,
        light,
        inset_rect(layout.topbar.clock_well, 3),
        frame_ctx.output_scale,
        damage,
        &legacy_theme.light,
    );

    for (i, slot) in layout.sidebar.slots.iter().enumerate() {
        let hovered = sidebar_hover_slot == Some(i);
        let _ = draw_beveled_panel(
            frame,
            &beveled,
            slot.outer,
            frame_ctx.output_scale,
            damage,
            &legacy_theme.module,
        );
        let _ = draw_beveled_panel(
            frame,
            &beveled,
            slot.inner,
            frame_ctx.output_scale,
            damage,
            &legacy_theme.module_inner,
        );
        let _ = draw_recessed_button(
            frame,
            button,
            slot.icon_well,
            frame_ctx.output_scale,
            damage,
            &legacy_theme.button,
        );

        let hover = if hovered { 1.0 } else { 0.0 };
        let mut light_style = legacy_theme.light;
        light_style.glow_color[3] = 0.08 + hover * 0.55;
        light_style.core_color[3] = 0.18 + hover * 0.55;
        light_style.glow_radius = 8.0 + hover * 6.0;
        light_style.core_inset = 3.0 - hover * 0.75;
        let _ = draw_light_channel(
            frame,
            light,
            inset_rect(slot.icon_well, 3),
            frame_ctx.output_scale,
            damage,
            &light_style,
        );
    }

    if let Some(rect) = layout.sidebar.light {
        let _ = draw_light_channel(
            frame,
            light,
            rect,
            frame_ctx.output_scale,
            damage,
            &legacy_theme.light,
        );
    }

    for rect in layout
        .sidebar
        .caps
        .iter()
        .chain(layout.decoration.corner_caps.iter())
        .chain(layout.decoration.corner_joint_caps.iter())
    {
        let _ = draw_beveled_panel(
            frame,
            &beveled,
            *rect,
            frame_ctx.output_scale,
            damage,
            &legacy_theme.corner_cap,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw_chrome_trim_glass_icons(
    frame: &mut GlesFrame<'_, '_>,
    shaders: &ChromeShaders,
    frame_ctx: &DesktopFrameCtx,
    layout: &ChromeLayoutLogical,
    metrics: &ChromeMetrics,
    ui_tree: &UiTree,
    theme: &FlowTheme,
    sidebar_hover_slot: Option<usize>,
    atlas: Option<&IconAtlas>,
) -> Result<(), GlesError> {
    let legacy_theme = chrome_theme_from_flow_theme(&theme.chrome);
    let beveled = shaders
        .beveled_panel
        .clone()
        .expect("beveled_panel shader not compiled");
    let glass = shaders.glass.as_ref().expect("glass shader not compiled");

    let fullscreen_rect: Rectangle<i32, Physical> = Rectangle::from_loc_and_size(
        Point::<i32, Physical>::from((0, 0)),
        Size::<i32, Physical>::from(frame_ctx.output_size),
    );
    let damage = &[fullscreen_rect];

    if let Some(rect) = layout.work_area.trim {
        let _ = draw_beveled_panel(
            frame,
            &beveled,
            rect,
            frame_ctx.output_scale,
            damage,
            &legacy_theme.trim,
        );
    }

    let _ = draw_workarea_glass(
        frame,
        frame_ctx,
        glass,
        layout.work_area.glass,
        frame_ctx.output_scale,
        damage,
        &legacy_theme.glass,
    );

    draw_active_lightbar(frame, shaders, frame_ctx, layout);

    let Some(atlas) = atlas else {
        return Ok(());
    };
    let Some(tinted_icon) = shaders.tinted_icon.clone() else {
        return Ok(());
    };

    let icon_px = metrics.icon_base_px as i32;
    let is_active_output = frame_ctx.rendering_output == frame_ctx.active_output;
    let output_factor = if is_active_output { 1.0 } else { 0.75 };

    for el in &ui_tree.elements {
        if !el.visible {
            continue;
        }

        let scale = match el.kind {
            UiElementKind::Clock => 1.0,
            _ => {
                if el.active {
                    el.press_scale
                } else if el.hovered {
                    el.hover_scale
                } else {
                    1.0
                }
            }
        };

        let base_rect_logical = Rectangle::<i32, Logical>::from_loc_and_size(
            (el.bounds.x, el.bounds.y),
            (el.bounds.w, el.bounds.h),
        );

        let mut style = themed_icon_style(theme, el.visual_state());
        style.tint[0] *= output_factor;
        style.tint[1] *= output_factor;
        style.tint[2] *= output_factor;
        style.tint[3] *= output_factor;
        style.glow *= output_factor;

        match el.kind {
            UiElementKind::SidebarButton | UiElementKind::WorkspaceSlot => {
                if el.selected || el.active {
                    let selected_rect = inset_rect(base_rect_logical, 3);
                    let selected_style = selected_sidebar_style(theme, el.hovered || el.active);
                    let _ = draw_beveled_panel(
                        frame,
                        &beveled,
                        selected_rect,
                        frame_ctx.output_scale,
                        damage,
                        &selected_style,
                    );
                }
                if let Some(icon_id) = el.icon {
                    let mut icon_rect = icon_rect_in_module(base_rect_logical, icon_px);
                    if scale != 1.0 {
                        let cx = icon_rect.loc.x + icon_rect.size.w / 2;
                        let cy = icon_rect.loc.y + icon_rect.size.h / 2;
                        let new_w = ((icon_rect.size.w as f32) * scale).round() as i32;
                        let new_h = ((icon_rect.size.h as f32) * scale).round() as i32;
                        icon_rect = Rectangle::from_loc_and_size(
                            (cx - new_w / 2, cy - new_h / 2),
                            (new_w, new_h),
                        );
                    }
                    render_icon_with_tint(
                        frame,
                        atlas,
                        icon_id,
                        icon_rect,
                        frame_ctx.output_scale,
                        style,
                        &tinted_icon,
                    );
                }
            }
            UiElementKind::TopbarIndicator | UiElementKind::TopbarButton => {
                if let Some(icon_id) = el.icon {
                    let icon_rect = well_icon_rect(base_rect_logical);
                    render_icon_with_tint(
                        frame,
                        atlas,
                        icon_id,
                        icon_rect,
                        frame_ctx.output_scale,
                        style,
                        &tinted_icon,
                    );
                }
            }
            UiElementKind::Clock => {
                use chrono::Local;
                let now = Local::now();
                let time_str = now.format("%-I:%M %p").to_string();
                let _ = (time_str, base_rect_logical);
            }
            _ => {}
        }
    }

    let _ = sidebar_hover_slot;
    Ok(())
}

fn draw_active_lightbar(
    frame: &mut GlesFrame<'_, '_>,
    shaders: &ChromeShaders,
    frame_ctx: &DesktopFrameCtx,
    layout: &ChromeLayoutLogical,
) {
    if frame_ctx.active_output != frame_ctx.rendering_output {
        return;
    }
    let Some(program) = shaders.amber_lightbar.as_ref() else {
        return;
    };
    let bar_rect =
        Rectangle::from_loc_and_size(layout.topbar.outer.loc, (layout.topbar.outer.size.w, 10));
    let fullscreen_rect: Rectangle<i32, Physical> = Rectangle::from_loc_and_size(
        Point::<i32, Physical>::from((0, 0)),
        Size::<i32, Physical>::from(frame_ctx.output_size),
    );
    let damage = &[fullscreen_rect];
    let _ = draw_amber_lightbar(frame, program, bar_rect, frame_ctx.output_scale, damage);
}

fn themed_icon_style(theme: &FlowTheme, state: UiVisualState) -> UiVisualStyle {
    let (tint, glow, alpha, scale) = match state {
        UiVisualState::Inactive => (theme.icons.inactive, 0.0, theme.icons.inactive[3], 1.0),
        UiVisualState::Hover => (
            theme.icons.hover,
            0.14,
            theme.icons.hover[3],
            theme.hover_scale,
        ),
        UiVisualState::Active => (
            theme.icons.active,
            0.32,
            theme.icons.active[3],
            theme.press_scale,
        ),
        UiVisualState::Selected => (theme.icons.active, 0.28, theme.icons.active[3], 1.02),
        UiVisualState::Disabled => (theme.icons.disabled, 0.0, theme.icons.disabled[3], 1.0),
    };
    UiVisualStyle {
        tint,
        glow,
        alpha,
        scale,
    }
}

fn selected_sidebar_style(theme: &FlowTheme, hovered: bool) -> BevelStyle {
    let mut face_color = theme.chrome.panel_color;
    face_color[3] = 0.34;
    let mut glow_color = theme.icons.glow;
    glow_color[3] = if hovered {
        (glow_color[3] + 0.22).min(0.85)
    } else {
        (glow_color[3] + 0.12).min(0.70)
    };
    BevelStyle {
        bevel: 2.0,
        softness: 1.25,
        glow_width: 5.0,
        glow_alpha: if hovered { 0.72 } else { 0.52 },
        inner_shadow: 0.10,
        face_color,
        light_color: theme.chrome.accent_color,
        shadow_color: [0.0, 0.0, 0.0, 0.45],
        glow_color,
    }
}

#[inline]
pub fn to_physical_rect(
    rect_logical: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) -> Rectangle<i32, Physical> {
    rect_logical.to_physical_precise_round(scale)
}

#[inline]
pub fn inset_rect(r: Rectangle<i32, Logical>, px: i32) -> Rectangle<i32, Logical> {
    Rectangle::from_loc_and_size(
        (r.loc.x + px, r.loc.y + px),
        ((r.size.w - px * 2).max(1), (r.size.h - px * 2).max(1)),
    )
}

pub fn well_icon_rect(well_logical: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    inset_rect(well_logical, (well_logical.size.h / 5).max(4))
}

fn icon_rect_in_module(module: Rectangle<i32, Logical>, icon_px: i32) -> Rectangle<i32, Logical> {
    let x = module.loc.x + ((module.size.w - icon_px).max(0) / 2);
    let y = module.loc.y + ((module.size.h - icon_px).max(0) / 2);
    Rectangle::from_loc_and_size((x, y), (icon_px, icon_px))
}

pub fn draw_top_bar(
    frame: &mut GlesFrame<'_, '_>,
    program: &GlesPixelProgram,
    rect_logical: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    damage: &[Rectangle<i32, Physical>],
    style: &TopBarStyle,
) -> Result<(), GlesError> {
    let rect_physical = to_physical_rect(rect_logical, scale);
    let src_rect = Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (rect_physical.size.w as f64, rect_physical.size.h as f64),
    );
    let buffer_size = Size::<i32, Buffer>::from((rect_physical.size.w, rect_physical.size.h));
    frame.render_pixel_shader_to(
        program,
        src_rect,
        rect_physical,
        buffer_size,
        Some(damage),
        1.0,
        &[
            Uniform::new(
                "u_size",
                [rect_physical.size.w as f32, rect_physical.size.h as f32],
            ),
            Uniform::new("u_radius", style.radius),
            Uniform::new("u_softness", style.softness),
            Uniform::new("u_bevel", style.bevel),
            Uniform::new("u_highlight_strength", style.highlight_strength),
            Uniform::new("u_shadow_strength", style.shadow_strength),
            Uniform::new("u_trim_height", style.trim_height),
            Uniform::new("u_trim_brightness", style.trim_brightness),
            Uniform::new("u_face_color", style.face_color),
            Uniform::new("u_edge_color", style.edge_color),
            Uniform::new("u_trim_color", style.trim_color),
        ],
    )
}

pub fn draw_recessed_button(
    frame: &mut GlesFrame<'_, '_>,
    button: &GlesPixelProgram,
    rect_logical: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    damage: &[Rectangle<i32, Physical>],
    style: &ButtonStyle,
) -> Result<(), GlesError> {
    let rect_physical = to_physical_rect(rect_logical, scale);
    let src_rect = Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (rect_physical.size.w as f64, rect_physical.size.h as f64),
    );
    let buffer_size = Size::<i32, Buffer>::from((rect_physical.size.w, rect_physical.size.h));
    frame.render_pixel_shader_to(
        button,
        src_rect,
        rect_physical,
        buffer_size,
        Some(damage),
        1.0,
        &[
            Uniform::new(
                "u_size",
                [rect_physical.size.w as f32, rect_physical.size.h as f32],
            ),
            Uniform::new("u_bevel", style.bevel),
            Uniform::new("u_softness", style.softness),
            Uniform::new("u_inner_shadow", style.inner_shadow),
            Uniform::new("u_glow_strength", style.glow_strength),
            Uniform::new("u_glow_radius", style.glow_radius),
            Uniform::new("u_face_color", style.face_color),
            Uniform::new("u_shadow_color", style.shadow_color),
            Uniform::new("u_glow_color", style.glow_color),
        ],
    )
}

pub fn draw_workarea_glass(
    frame: &mut GlesFrame<'_, '_>,
    frame_ctx: &DesktopFrameCtx,
    program: &GlesPixelProgram,
    rect_logical: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    damage: &[Rectangle<i32, Physical>],
    style: &GlassStyle,
) -> Result<(), GlesError> {
    let rect_physical = to_physical_rect(rect_logical, scale);
    let src_rect = Rectangle::from_loc_and_size(
        (0.0, 0.0),
        (rect_physical.size.w as f64, rect_physical.size.h as f64),
    );
    let size = Size::from((rect_physical.size.w, rect_physical.size.h));
    let t = frame_ctx
        .now
        .duration_since(frame_ctx.start_time)
        .as_secs_f32();
    frame.render_pixel_shader_to(
        program,
        src_rect,
        rect_physical,
        size,
        Some(damage),
        1.0,
        &[
            Uniform::new(
                "u_size",
                [rect_physical.size.w as f32, rect_physical.size.h as f32],
            ),
            Uniform::new("u_opacity", style.opacity),
            Uniform::new("u_edge_width", style.edge_width),
            Uniform::new("u_edge_brightness", style.edge_brightness),
            Uniform::new("u_highlight_strength", style.highlight_strength),
            Uniform::new("u_tint", style.tint),
            Uniform::new("u_edge_color", style.edge_color),
            Uniform::new("u_time", t),
        ],
    )
}

pub fn draw_beveled_panel(
    frame: &mut GlesFrame<'_, '_>,
    program: &GlesPixelProgram,
    rect_logical: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    damage: &[Rectangle<i32, Physical>],
    style: &BevelStyle,
) -> Result<(), GlesError> {
    let rect_physical = to_physical_rect(rect_logical, scale);
    let src_rect = Rectangle::from_loc_and_size(
        (0.0, 0.0),
        (rect_physical.size.w as f64, rect_physical.size.h as f64),
    );
    let size = Size::from((rect_physical.size.w, rect_physical.size.h));
    frame.render_pixel_shader_to(
        program,
        src_rect,
        rect_physical,
        size,
        Some(damage),
        1.0,
        &[
            Uniform::new("u_bevel", style.bevel),
            Uniform::new("u_softness", style.softness),
            Uniform::new("u_glow_width", style.glow_width),
            Uniform::new("u_glow_alpha", style.glow_alpha),
            Uniform::new("u_inner_shadow", style.inner_shadow),
            Uniform::new("u_face_color", style.face_color),
            Uniform::new("u_light_color", style.light_color),
            Uniform::new("u_shadow_color", style.shadow_color),
            Uniform::new("u_glow_color", style.glow_color),
        ],
    )
}

pub fn draw_light_channel(
    frame: &mut GlesFrame<'_, '_>,
    program: &GlesPixelProgram,
    rect_logical: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    damage: &[Rectangle<i32, Physical>],
    style: &LightChannelStyle,
) -> Result<(), GlesError> {
    let rect_physical = to_physical_rect(rect_logical, scale);
    let src_rect = Rectangle::from_loc_and_size(
        (0.0, 0.0),
        (rect_physical.size.w as f64, rect_physical.size.h as f64),
    );
    let size = Size::from((rect_physical.size.w, rect_physical.size.h));
    frame.render_pixel_shader_to(
        program,
        src_rect,
        rect_physical,
        size,
        Some(damage),
        1.0,
        &[
            Uniform::new("u_slot_inset", style.slot_inset),
            Uniform::new("u_core_inset", style.core_inset),
            Uniform::new("u_glow_radius", style.glow_radius),
            Uniform::new("u_softness", style.softness),
            Uniform::new("u_housing_color", style.housing_color),
            Uniform::new("u_glow_color", style.glow_color),
            Uniform::new("u_core_color", style.core_color),
        ],
    )
}

fn draw_amber_lightbar(
    frame: &mut GlesFrame<'_, '_>,
    program: &GlesPixelProgram,
    rect_logical: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    damage: &[Rectangle<i32, Physical>],
) -> Result<(), GlesError> {
    let rect_physical = to_physical_rect(rect_logical, scale);
    let src_rect = Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (rect_physical.size.w as f64, rect_physical.size.h as f64),
    );
    let size = Size::<i32, Buffer>::from((rect_physical.size.w, rect_physical.size.h));
    frame.render_pixel_shader_to(
        program,
        src_rect,
        rect_physical,
        size,
        Some(damage),
        1.0,
        &[
            Uniform::new("u_color", [1.0, 0.75, 0.05, 1.0]),
            Uniform::new("alpha", 1.0f32),
        ],
    )
}

pub fn render_icon_with_tint(
    frame: &mut GlesFrame<'_, '_>,
    atlas: &IconAtlas,
    icon: IconId,
    rect_logical: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    style: UiVisualStyle,
    program: &GlesTexProgram,
) {
    if let Some(entry) = atlas.get(icon) {
        let rect_physical = to_physical_rect(rect_logical, scale);
        let src = Rectangle::<f64, Buffer>::from_loc_and_size(
            (entry.x as f64, entry.y as f64),
            (entry.w as f64, entry.h as f64),
        );
        let damage_local =
            Rectangle::from_loc_and_size((0, 0), (rect_physical.size.w, rect_physical.size.h));
        let _ = frame.render_texture_from_to(
            &atlas.texture,
            src,
            rect_physical,
            &[damage_local],
            &[],
            Transform::Normal,
            style.alpha,
            Some(program),
            &[Uniform::new("u_tint", style.tint)],
        );
    }
}
