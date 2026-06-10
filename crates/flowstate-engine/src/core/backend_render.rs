#![allow(unused_imports)]

use std::time::{Duration, Instant};

use smithay::backend::renderer::gles::{GlesFrame, GlesRenderer, GlesTexture};
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Rectangle, Size};

use crate::core::chrome_layout::{build_chrome_layout, ChromeLayout};
use crate::core::desktop::DesktopState;
use crate::core::fonts::{FontId, FontRole, TextStyle, style_for};
use crate::core::render::{FlowRenderElement, FrameCtx, RenderInputs, RenderInputsMut};
use crate::core::ui_builder::build_ui_for_output;
use crate::core::ui_state::UiState;
use crate::core::{OutputState, SceneState};
use flowstate_flow::keybinds::BackendKind;
use flowstate_themes::theme::BuiltInThemeId;
use flowstate_themes::FlowTheme;
use flowstate_types::OutputId;
use flowstate_ui::desktop_frame::DesktopFrameCtx;

pub struct PreparedOutput {
    pub frame_ctx: FrameCtx,
    pub layout: ChromeLayout,
    pub draw_software_cursor: bool,
}

fn rect_area(rect: Rectangle<i32, Physical>) -> i64 {
    i64::from(rect.size.w.max(0)) * i64::from(rect.size.h.max(0))
}

fn damage_area_percent(
    damage: &[Rectangle<i32, Physical>],
    output_size: Size<i32, Physical>,
) -> i64 {
    let output_area = i64::from(output_size.w.max(0)) * i64::from(output_size.h.max(0)).max(1);
    let damage_area: i64 = damage.iter().copied().map(rect_area).sum();
    (damage_area * 100 / output_area.max(1)).clamp(0, 100)
}

fn is_full_damage(damage: &[Rectangle<i32, Physical>], output_size: Size<i32, Physical>) -> bool {
    damage.len() == 1 && damage[0] == Rectangle::from_loc_and_size((0, 0), output_size)
}

fn rect_bounds(rects: &[Rectangle<i32, Physical>]) -> Option<Rectangle<i32, Physical>> {
    let first = rects.first()?;
    let mut min_x = first.loc.x;
    let mut min_y = first.loc.y;
    let mut max_x = first.loc.x + first.size.w;
    let mut max_y = first.loc.y + first.size.h;

    for rect in &rects[1..] {
        min_x = min_x.min(rect.loc.x);
        min_y = min_y.min(rect.loc.y);
        max_x = max_x.max(rect.loc.x + rect.size.w);
        max_y = max_y.max(rect.loc.y + rect.size.h);
    }

    Some(Rectangle::from_loc_and_size(
        (min_x, min_y),
        ((max_x - min_x).max(0), (max_y - min_y).max(0)),
    ))
}

fn expand_rect(rect: Rectangle<i32, Physical>, margin: i32) -> Rectangle<i32, Physical> {
    Rectangle::from_loc_and_size(
        (rect.loc.x - margin, rect.loc.y - margin),
        (rect.size.w + margin * 2, rect.size.h + margin * 2),
    )
}

fn compact_damage(
    damage: &[Rectangle<i32, Physical>],
    output_size: Size<i32, Physical>,
) -> Vec<Rectangle<i32, Physical>> {
    const MERGE_MARGIN: i32 = 4;
    const MAX_RECTS: usize = 8;
    const FULL_DAMAGE_PERCENT: i64 = 45;

    let full = Rectangle::from_loc_and_size((0, 0), output_size);
    let output_area = rect_area(full).max(1);
    let mut rects = Vec::with_capacity(damage.len());

    for rect in damage {
        if rect.size.w <= 0 || rect.size.h <= 0 {
            continue;
        }
        if let Some(clipped) = rect.intersection(full) {
            if !clipped.is_empty() {
                rects.push(clipped);
            }
        }
    }

    if rects.is_empty() {
        return vec![full];
    }

    let mut i = 0;
    while i < rects.len() {
        let mut j = i + 1;
        while j < rects.len() {
            if expand_rect(rects[i], MERGE_MARGIN).overlaps(rects[j]) {
                let merged = rect_bounds(&[rects[i], rects[j]]).expect("two rects");
                rects[i] = merged;
                rects.swap_remove(j);
            } else {
                j += 1;
            }
        }
        i += 1;
    }

    let total_area: i64 = rects.iter().copied().map(rect_area).sum();
    if total_area * 100 >= output_area * FULL_DAMAGE_PERCENT {
        return vec![full];
    }

    if rects.len() > MAX_RECTS {
        if let Some(bounds) = rect_bounds(&rects) {
            return vec![bounds];
        }
    }

    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_damage_merges_nearby_rects() {
        let output_size = Size::<i32, Physical>::from((100, 100));
        let damage = [
            Rectangle::from_loc_and_size((10, 10), (10, 10)),
            Rectangle::from_loc_and_size((23, 10), (10, 10)),
        ];

        let compacted = compact_damage(&damage, output_size);

        assert_eq!(
            compacted,
            vec![Rectangle::from_loc_and_size((10, 10), (23, 10))]
        );
    }

    #[test]
    fn compact_damage_uses_full_output_for_large_area() {
        let output_size = Size::<i32, Physical>::from((100, 100));
        let damage = [Rectangle::from_loc_and_size((0, 0), (80, 60))];

        let compacted = compact_damage(&damage, output_size);

        assert_eq!(
            compacted,
            vec![Rectangle::<i32, Physical>::from_loc_and_size(
                (0, 0),
                output_size
            )]
        );
    }
}

pub fn prepare_output(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    output_id: OutputId,
    buffer_size: Size<i32, Physical>,
    ui_state: &mut UiState<GlesTexture>,
    now: Instant,
    dt: Duration,
    force_full_damage: bool,
) -> Result<PreparedOutput, Box<dyn std::error::Error>> {
    let (logical_w, logical_h, scale_factor, output_scale, buffer_scale) = {
        let desk_output = state
            .outputs
            .get(&output_id)
            .expect("active output missing");
        (
            desk_output.logical_size.w,
            desk_output.logical_size.h,
            desk_output.scale_factor,
            desk_output.scale,
            desk_output.scale_factor.round().max(1.0) as i32,
        )
    };

    state.prepare_cursor_for_frame(renderer, output_id)?;

    let pointer_on_this_output = state.output_owns_cursor(output_id);

    let draw_software_cursor = pointer_on_this_output
        && state.cursor_manager.software_cursor_needed()
        && !state.drm_try_pass_cursor_this_frame;

    let logical_size = Size::<i32, Logical>::from((logical_w, logical_h));

    let layout = build_chrome_layout(
        logical_size,
        state.chrome.metrics.topbar_h,
        state.chrome.metrics.sidebar_w,
    );

    build_ui_for_output(&mut state.ui, &layout);
    state.refresh_ui_hover_for_output(output_id);

    if let Some(rect) = state.active_sidebar_pulse_damage_rect(output_id, now) {
        state.mark_output_logical_damage(
            output_id,
            rect,
            0,
            crate::core::desktop::DamageSource::Unknown,
        );
    }

    if let Some(rect) = state.active_topbar_pulse_damage_rect(output_id, now) {
        state.mark_output_logical_damage(
            output_id,
            rect,
            0,
            crate::core::desktop::DamageSource::Unknown,
        );
    }

    if let Some(rect) = state.active_clock_pulse_damage_rect(output_id, now) {
        state.mark_output_logical_damage(
            output_id,
            rect,
            0,
            crate::core::desktop::DamageSource::Unknown,
        );
    }

    let frame_damage = {
        let pending_damage = state
            .outputs
            .get(&output_id)
            .expect("active output missing")
            .pending_damage
            .clone();
        let full_damage = Rectangle::from_loc_and_size((0, 0), buffer_size);
        if force_full_damage || state.render.redraw_all || pending_damage.is_empty() {
            if state.render.redraw_all {
                state.record_damage_source(crate::core::desktop::DamageSource::FullRedrawFallback);
            }
            let frame_damage = vec![full_damage];
            state.log_damage_frame(
                output_id,
                pending_damage.len(),
                frame_damage.len(),
                damage_area_percent(&pending_damage, buffer_size),
                damage_area_percent(&frame_damage, buffer_size),
                true,
                state.render.redraw_all,
            );
            frame_damage
        } else {
            let pre_rects = pending_damage.len();
            let pre_area_percent = damage_area_percent(&pending_damage, buffer_size);
            let frame_damage = compact_damage(&pending_damage, buffer_size);
            state.log_damage_frame(
                output_id,
                pre_rects,
                frame_damage.len(),
                pre_area_percent,
                damage_area_percent(&frame_damage, buffer_size),
                is_full_damage(&frame_damage, buffer_size),
                false,
            );
            frame_damage
        }
    };

    state.render.ensure_shader_programs(renderer)?;
    // need to pass state.theme.wallpaper into this function so theme wallpaper can be loaded
    state.render.ensure_wallpaper_loaded(renderer);

    if !state.render.fonts_prewarm_done {
        prewarm_font_glyphs(state)?;
        state.render.fonts_prewarm_done = true;
    }

    if force_full_damage {
        prepare_portal_chrome_glyphs(state, scale_factor)?;
    }

    if state.fonts.atlas_dirty || state.render.font_atlas_texture.is_none() {
        state.render.upload_font_atlas(renderer, &state.fonts)?;
        state.fonts.atlas_dirty = false;
    }

    let frame_ctx = FrameCtx {
        output_size: (buffer_size.w, buffer_size.h),
        output_scale,
        buffer_scale,
        damage: frame_damage,
        work: Rectangle::from_loc_and_size((0, 0), (logical_w, logical_h)),
        frame_no: state.render.frame_no,
        now,
        dt,
        active_output: state.focused_output,
        rendering_output: output_id,
        focus_pulse: focus_pulse_value(now.saturating_duration_since(state.focus_changed_at)),
        portal_capture: force_full_damage,
    };

    ui_state
        .chrome
        .ensure_gpu_resources(renderer, scale_factor)?;

    Ok(PreparedOutput {
        frame_ctx,
        layout,
        draw_software_cursor,
    })
}

fn prewarm_font_glyphs(state: &mut DesktopState) -> Result<(), Box<dyn std::error::Error>> {
    let active_theme_id = state
        .theme
        .active_theme()
        .id
        .builtin_id()
        .unwrap_or(BuiltInThemeId::Classic);

    let preload_fonts: &[FontId] = match active_theme_id {
        BuiltInThemeId::Classic => &[
            FontId::IbmPlexSansRegular,
            FontId::IbmPlexSansMedium,
            FontId::IbmPlexSansSemiBold,
        ],
        BuiltInThemeId::Moonbase => &[
            FontId::RajdhaniRegular,
            FontId::RajdhaniMedium,
            FontId::RajdhaniSemiBold,
        ],
        BuiltInThemeId::Eagle => &[
            FontId::IbmPlexSansMedium,
            FontId::OrbitronRegular,
            FontId::OrbitronMedium,
            FontId::OrbitronSemiBold,
        ],
    };

    for &font in preload_fonts {
        for size_px in [10, 12, 14, 16, 18, 20, 24] {
            let style = TextStyle { font, size_px };

            state.fonts.prepare_text("FlowState", style)?;
            state.fonts.prepare_text("FlowState Debug", style)?;
            state.fonts.prepare_text("OK", style)?;
            state.fonts.prepare_text("Cancel", style)?;
            const BASIC_ASCII: &str =
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789\
         .,;:!?()[]{}<>+-=*/_@#%&$'\"\\|`~^ ";

            state.fonts.prepare_text(BASIC_ASCII, style)?;
        }
    }

    Ok(())
}

fn prepare_portal_chrome_glyphs(
    state: &mut DesktopState,
    scale_factor: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    use chrono::Local;

    let active_theme = state.theme.active_theme();
    let builtin_id = active_theme
        .id
        .builtin_id()
        .unwrap_or(BuiltInThemeId::Eagle);

    let clock_style = style_for(FontRole::Clock, 24, builtin_id);
    let time_str = Local::now().format("%-I:%M %p").to_string();
    state.fonts.prepare_text(&time_str, clock_style)?;

    let title_style = style_for(FontRole::Title, 24, builtin_id);
    state.fonts.prepare_text("FLOWSTATE", title_style)?;

    let meta_style = style_for(FontRole::Meta, 14, builtin_id);
    let output_number = state.focused_output.0;
    let meta = format!("OUT {output_number} · WS 1");
    state.fonts.prepare_text(&meta, meta_style)?;

    let _ = scale_factor;
    Ok(())
}

fn focus_pulse_value(elapsed: Duration) -> f32 {
    let seconds = elapsed.as_secs_f32();
    if seconds >= 0.45 {
        0.0
    } else {
        1.0 - (seconds / 0.45)
    }
}

/// Import and build client surfaces for an output. Call after `GlesRenderer::bind` and before
/// `GlesRenderer::render` on the same renderer.
pub fn build_output_client_elements(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    output_id: OutputId,
) -> Vec<FlowRenderElement> {
    let (output_handle, output_origin, output_logical_size) = state
        .outputs
        .get(&output_id)
        .map(|o| (o.handle.clone(), o.logical_origin, o.logical_size))
        .expect("output missing");

    let active_workspace = state
        .outputs
        .get(&output_id)
        .map(|o| o.active_workspace)
        .unwrap_or_else(|| state.focused_workspace());

    let layers_on = state.outputs.get(&output_id).map(|o| &o.handle);

    state.import_mapped_surfaces_for_output(renderer, output_origin, output_logical_size);

    state.render.build_client_elements_for_output(
        &state.space,
        &state.windows,
        active_workspace,
        &output_handle,
        layers_on,
        renderer,
    )
}

pub fn build_output_popup_elements(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    output_id: OutputId,
) -> Vec<FlowRenderElement> {
    let output_handle = state
        .outputs
        .get(&output_id)
        .map(|o| o.handle.clone())
        .expect("output missing");

    let active_workspace = state
        .outputs
        .get(&output_id)
        .map(|o| o.active_workspace)
        .unwrap_or_else(|| state.focused_workspace());

    state.render.build_popup_elements_for_output(
        &state.space,
        &state.windows,
        active_workspace,
        &output_handle,
        renderer,
    )
}

pub fn draw_output(
    state: &mut DesktopState,
    frame: &mut GlesFrame<'_, '_>,
    prepared: &PreparedOutput,
    elements: &[FlowRenderElement],
    popup_elements: &[FlowRenderElement],
    ui_state: &mut UiState<GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
) -> Result<(), Box<dyn std::error::Error>> {
    let egui_frame_ctx = DesktopFrameCtx {
        output_size: prepared.frame_ctx.output_size,
        output_scale: prepared.frame_ctx.output_scale,
        work: prepared.layout.work_area.recess,
        active_output: prepared.frame_ctx.active_output,
        rendering_output: prepared.frame_ctx.rendering_output,
        now: prepared.frame_ctx.now,
        start_time: state.render.start_time,
        flip_egui_y: state.backend_kind == BackendKind::Drm,
        portal_capture: prepared.frame_ctx.portal_capture,
    };
    if (prepared.frame_ctx.rendering_output == prepared.frame_ctx.active_output
        || prepared.frame_ctx.portal_capture)
        && state.render.egui.has_open_panels()
    {
        state.sync_egui(&egui_frame_ctx);
    }

    let active_workspace = state
        .outputs
        .get(&prepared.frame_ctx.rendering_output)
        .map(|o| o.active_workspace)
        .unwrap_or(state.active_workspace);

    let inputs = RenderInputs {
        ctx: &prepared.frame_ctx,
        layout: &prepared.layout,
        scene,
        output: output_state,
        metrics: &state.chrome.metrics,
        elements: &elements,
        popup_elements,
        sidebar_hover_slot: state.sidebar_hover_for_output(prepared.frame_ctx.active_output),
        sidebar_pulse: state
            .sidebar_pulse_for_output(prepared.frame_ctx.rendering_output, prepared.frame_ctx.now),
        topbar_pulse: state
            .topbar_pulse_for_output(prepared.frame_ctx.rendering_output, prepared.frame_ctx.now),
        clock_pulse: state
            .clock_pulse_for_output(prepared.frame_ctx.rendering_output, prepared.frame_ctx.now),
        draw_software_cursor: prepared.draw_software_cursor,
        ui_tree: &state.ui,
        current_workspace: active_workspace,
        // 👇 ADD THESE
        dialogs: &state.dialogs,
        active_dialog: state.active_dialog,
        fonts: &state.fonts,
        theme: &state.theme.active_theme(),
        flip_egui_y: state.backend_kind == BackendKind::Drm,
    };

    let muts = RenderInputsMut { ui: ui_state };

    state.render.render_output(frame, inputs, muts)?;
    Ok(())
}
