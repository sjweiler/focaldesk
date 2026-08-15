#![allow(unused_imports)]

use std::time::{Duration, Instant};

use smithay::backend::renderer::gles::{GlesFrame, GlesRenderer, GlesTexture};
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Rectangle, Scale, Size};

use crate::core::chrome_layout::{build_chrome_layout, ChromeLayout};
use crate::core::desktop::DesktopState;
use crate::core::fonts::{style_for, FontId, FontRole, TextStyle};
use crate::core::render::{
    ChromeGlassPass, ClientCompositingMode, FlowRenderElement, FrameCtx, RenderInputs,
    RenderInputsMut,
};
use crate::core::ui_builder::{build_ui_for_output_with_options, AiFlowMode, UiBuildOptions};
use crate::core::ui_state::UiState;
use crate::core::{OutputState, SceneState};
use focaldesk_flow::keybinds::BackendKind;
use focaldesk_logging::FLogLevel;
use focaldesk_themes::theme::BuiltInThemeId;
use focaldesk_themes::FlowTheme;
use focaldesk_types::OutputId;
use focaldesk_ui::desktop_frame::DesktopFrameCtx;

pub struct PreparedOutput {
    pub frame_ctx: FrameCtx,
    /// Portion of frame damage that intersects compositor-owned chrome.
    /// Client-only updates in the work recess leave this empty, allowing the
    /// expensive icon/text overlay to remain retained.
    pub shell_damage: Vec<Rectangle<i32, Physical>>,
    pub layout: ChromeLayout,
    pub draw_software_cursor: bool,
    pub sdr_base_generation: u64,
    pub force_sdr_base_redraw: bool,
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

#[cfg(test)]
fn damage_area_permyriad(
    damage: &[Rectangle<i32, Physical>],
    output_size: Size<i32, Physical>,
) -> i64 {
    let output_area = i64::from(output_size.w.max(0)) * i64::from(output_size.h.max(0)).max(1);
    let damage_area: i64 = damage.iter().copied().map(rect_area).sum();
    (damage_area * 10_000 / output_area.max(1)).clamp(0, 10_000)
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

fn merge_rects(
    a: Rectangle<i32, Physical>,
    b: Rectangle<i32, Physical>,
) -> Rectangle<i32, Physical> {
    let min_x = a.loc.x.min(b.loc.x);
    let min_y = a.loc.y.min(b.loc.y);
    let max_x = (a.loc.x + a.size.w).max(b.loc.x + b.size.w);
    let max_y = (a.loc.y + a.size.h).max(b.loc.y + b.size.h);
    Rectangle::from_loc_and_size((min_x, min_y), (max_x - min_x, max_y - min_y))
}

fn compact_damage(
    damage: &[Rectangle<i32, Physical>],
    output_size: Size<i32, Physical>,
) -> Vec<Rectangle<i32, Physical>> {
    const MERGE_MARGIN: i32 = 4;
    const MAX_RECTS: usize = 8;

    let full = Rectangle::from_loc_and_size((0, 0), output_size);
    let mut rects: Vec<Rectangle<i32, Physical>> = Vec::with_capacity(damage.len().min(MAX_RECTS));

    for rect in damage {
        if rect.size.w <= 0 || rect.size.h <= 0 {
            continue;
        }
        if let Some(clipped) = rect.intersection(full) {
            if !clipped.is_empty() {
                let mut candidate = clipped;
                let mut i = 0;
                while i < rects.len() {
                    if expand_rect(candidate, MERGE_MARGIN).overlaps(rects[i]) {
                        candidate = merge_rects(candidate, rects.swap_remove(i));
                        // A merge can bridge a rectangle inspected earlier. Restart to guarantee
                        // transitive coalescing and non-overlapping output rectangles.
                        i = 0;
                    } else {
                        i += 1;
                    }
                }
                rects.push(candidate);
            }
        }
    }

    if rects.is_empty() {
        return Vec::new();
    }

    if rects.len() > MAX_RECTS {
        if let Some(bounds) = rect_bounds(&rects) {
            return vec![bounds];
        }
    }

    rects
}

fn shell_overlay_damage(
    damage: &[Rectangle<i32, Physical>],
    layout: &ChromeLayout,
    output_scale: Scale<f64>,
    output_size: Size<i32, Physical>,
) -> Vec<Rectangle<i32, Physical>> {
    let mut shell_regions = vec![layout.topbar.outer, layout.sidebar.outer];
    if let Some(trim) = layout.work_area.trim {
        shell_regions.push(trim);
    }

    let intersections: Vec<_> = shell_regions
        .into_iter()
        .map(|rect| rect.to_physical_precise_round(output_scale))
        .flat_map(|shell| {
            damage
                .iter()
                .filter_map(move |rect| rect.intersection(shell))
        })
        .collect();

    if intersections.is_empty() {
        Vec::new()
    } else {
        compact_damage(&intersections, output_size)
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

    let draw_software_cursor =
        pointer_on_this_output && state.cursor_manager.software_cursor_needed();

    let layout = state
        .chrome_layout_for_output(output_id)
        .expect("active output layout missing");
    let ui_options = state
        .ui_build_options_for_output(output_id)
        .expect("active output UI options missing");

    build_ui_for_output_with_options(&mut state.ui, &layout, ui_options);
    state.refresh_ui_hover_for_output(output_id);
    if output_id == state.focused_output {
        state.publish_accessibility_tree();
    }

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
        if force_full_damage || state.render.redraw_all {
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

    let lut_shader_before = state.render.chrome_shaders.output_encode_lut.is_some();
    state.render.ensure_shader_programs(renderer)?;
    if !lut_shader_before && state.render.chrome_shaders.output_encode_lut.is_some() {
        focaldesk_logging::flog_info!(
            "ICC LUT shader ready; refreshing wp_color preferred identities"
        );
        crate::core::wayland::color_management_protocol::notify_preferred_color_changed(state);
    }
    // need to pass state.theme.wallpaper into this function so theme wallpaper can be loaded
    state.render.ensure_wallpaper_loaded(renderer);

    if !state.render.fonts_prewarm_done {
        prewarm_font_glyphs(state)?;
        state.render.fonts_prewarm_done = true;
    }

    prepare_portal_chrome_glyphs(state, scale_factor)?;

    prepare_lock_screen_glyphs(state)?;

    if state.fonts.atlas_dirty || state.render.font_atlas_texture.is_none() {
        state.render.upload_font_atlas(renderer, &state.fonts)?;
        state.fonts.atlas_dirty = false;
    }

    let shell_damage = shell_overlay_damage(&frame_damage, &layout, output_scale, buffer_size);

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

    if state.debug.show_fps && state.render.frame_no.is_multiple_of(120) {
        let frame_ms = dt.as_secs_f64() * 1000.0;
        let fps = if dt.is_zero() {
            0.0
        } else {
            1.0 / dt.as_secs_f64()
        };
        focaldesk_logging::logging::flog(
            FLogLevel::Debug,
            format!(
                "frame timing output={output_id:?} frame={} dt={frame_ms:.2}ms fps={fps:.1}",
                state.render.frame_no
            ),
        );
    }

    ui_state
        .chrome
        .ensure_gpu_resources(renderer, scale_factor)?;

    Ok(PreparedOutput {
        frame_ctx,
        shell_damage,
        layout,
        draw_software_cursor,
        sdr_base_generation: state
            .outputs
            .get(&output_id)
            .map(|output| output.sdr_base_generation)
            .unwrap_or(0),
        force_sdr_base_redraw: state.render.redraw_all,
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
            FontId::IbmPlexSansRegular,
            FontId::IbmPlexSansMedium,
            FontId::OrbitronRegular,
            FontId::OrbitronMedium,
            FontId::OrbitronSemiBold,
        ],
    };

    for &font in preload_fonts {
        for size_px in [10, 12, 14, 16, 18, 20, 24] {
            let style = TextStyle { font, size_px };

            state.fonts.prepare_text("FocalDesk", style)?;
            state.fonts.prepare_text("FocalDesk Debug", style)?;
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

fn prepare_lock_screen_glyphs(state: &mut DesktopState) -> Result<(), Box<dyn std::error::Error>> {
    if !state.lock_screen.active {
        return Ok(());
    }

    let theme_id = state
        .theme
        .active_theme()
        .id
        .builtin_id()
        .unwrap_or(BuiltInThemeId::Classic);

    state
        .fonts
        .prepare_text("FOCALDESK LOCKED", style_for(FontRole::Title, 18, theme_id))?;
    state
        .fonts
        .prepare_text("ShowHide", style_for(FontRole::Label, 14, theme_id))?;
    state.fonts.prepare_text(
        "Enter passwordAuthenticatingUnlockedWrong password",
        style_for(FontRole::Label, 15, theme_id),
    )?;
    state.fonts.prepare_text(
        &state.lock_screen.message,
        style_for(FontRole::Label, 15, theme_id),
    )?;

    let password_display = if state.lock_screen.password_visible {
        state.lock_screen.password.as_str().to_string()
    } else {
        "*".repeat(state.lock_screen.password.chars().count().min(48))
    };
    state
        .fonts
        .prepare_text(&password_display, style_for(FontRole::Body, 22, theme_id))?;

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
    state.fonts.prepare_text("FOCALDESK", title_style)?;

    let meta_style = style_for(FontRole::Meta, 18, builtin_id);
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

/// Import committed client buffers for mapped windows on this output.
/// Call after `GlesRenderer::bind` and before `GlesRenderer::render`.
pub fn import_output_client_surfaces(
    state: &DesktopState,
    renderer: &mut GlesRenderer,
    output_id: OutputId,
) {
    let Some(output) = state.outputs.get(&output_id) else {
        return;
    };
    let mapped = state.space.elements().count();
    if mapped > 0 {
        focaldesk_logging::flog_info!(
            "import client surfaces output={} mapped_windows={}",
            output_id.0,
            mapped
        );
    }
    state.import_mapped_surfaces_for_output(renderer, output.logical_origin, output.logical_size);
}

/// Build client render elements for an output. Call after `GlesRenderer::bind` and before
/// `GlesRenderer::render` on the same offscreen target.
pub fn build_output_client_elements(
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

    let layers_on = state.outputs.get(&output_id).map(|o| &o.handle);

    let output = state.outputs.get(&output_id).expect("output missing");
    state.import_mapped_surfaces_for_output(renderer, output.logical_origin, output.logical_size);

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

    let mut elements = state.render.build_popup_elements_for_output(
        &state.space,
        &state.windows,
        active_workspace,
        &output_handle,
        renderer,
    );

    if let Some(output) = state.outputs.get(&output_id) {
        crate::core::portal::push_trusted_shell_elements_for_output(
            renderer,
            &output.handle,
            output.logical_size,
            smithay::utils::Scale::from(output.scale_factor),
            &mut elements,
        );
    }

    elements
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
    draw_output_stage(
        state,
        frame,
        prepared,
        elements,
        popup_elements,
        ui_state,
        scene,
        output_state,
        crate::core::render::OutputRenderStage::All,
        ClientCompositingMode::Sdr,
        ChromeGlassPass::InBaseSdr,
        false,
    )
}

pub fn draw_output_stage(
    state: &mut DesktopState,
    frame: &mut GlesFrame<'_, '_>,
    prepared: &PreparedOutput,
    elements: &[FlowRenderElement],
    popup_elements: &[FlowRenderElement],
    ui_state: &mut UiState<GlesTexture>,
    scene: &SceneState,
    output_state: &OutputState,
    stage: crate::core::render::OutputRenderStage,
    client_compositing: ClientCompositingMode,
    chrome_glass_pass: ChromeGlassPass,
    defer_egui_to_sdr: bool,
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
    if matches!(
        stage,
        crate::core::render::OutputRenderStage::All
            | crate::core::render::OutputRenderStage::Base
            | crate::core::render::OutputRenderStage::Overlay
    ) && state
        .desktop_outputs
        .get(&prepared.frame_ctx.rendering_output)
        .is_some_and(|output| output.egui.has_open_panels())
    {
        state.sync_egui(&egui_frame_ctx);
    }

    let active_workspace = state
        .outputs
        .get(&prepared.frame_ctx.rendering_output)
        .map(|o| o.active_workspace)
        .unwrap_or(state.active_workspace);
    let fullscreen_client = state.windows.iter().any(|window| {
        window.mapped
            && !window.minimized
            && window.fullscreen
            && window.workspace == active_workspace
            && window.output == Some(prepared.frame_ctx.rendering_output)
    });
    let notifications = if state.lock_screen.active && state.privacy.hide_lock_screen_notifications
    {
        Vec::new()
    } else {
        state.notification_snapshots.clone()
    };
    let lock_screen = state.lock_screen.snapshot(prepared.frame_ctx.now);
    let draw_internal_chrome = state
        .outputs
        .get(&prepared.frame_ctx.rendering_output)
        .map(|output| {
            !crate::core::wayland::trusted_shell::reservation_for_output(&output.handle).is_active()
        })
        .unwrap_or(true);

    let inputs = RenderInputs {
        ctx: &prepared.frame_ctx,
        layout: &prepared.layout,
        scene,
        output: output_state,
        metrics: &state.chrome.metrics,
        elements,
        popup_elements,
        sidebar_hover_slot: state
            .desktop_outputs
            .get(&prepared.frame_ctx.rendering_output)
            .and_then(focaldesk_ui::desktop_output::DesktopOutput::sidebar_hover_slot),
        sidebar_pulse: state
            .sidebar_pulse_for_output(prepared.frame_ctx.rendering_output, prepared.frame_ctx.now),
        topbar_pulse: state
            .topbar_pulse_for_output(prepared.frame_ctx.rendering_output, prepared.frame_ctx.now),
        clock_pulse: state
            .clock_pulse_for_output(prepared.frame_ctx.rendering_output, prepared.frame_ctx.now),
        draw_software_cursor: prepared.draw_software_cursor,
        ui_focus: state.ui.focused,
        current_workspace: active_workspace,
        fullscreen_client,
        draw_internal_chrome,
        // 👇 ADD THESE
        dialogs: &state.dialogs,
        active_dialog: state.active_dialog,
        fonts: &state.fonts,
        theme: state.theme.active_theme(),
        notifications: &notifications,
        notification_unread_count: state.notification_unread_count,
        lock_screen: &lock_screen,
        flip_egui_y: state.backend_kind == BackendKind::Drm,
        client_compositing,
        chrome_glass_pass,
        defer_egui_to_sdr,
        surface_colors: &state.surface_colors,
    };

    let desktop_output = state
        .desktop_outputs
        .get_mut(&prepared.frame_ctx.rendering_output)
        .expect("desktop output UI missing for output");
    let muts = RenderInputsMut {
        ui: ui_state,
        desktop_output,
    };

    state.render.render_stage(frame, inputs, muts, stage)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_damage_keeps_an_empty_frame_empty() {
        let output_size = Size::<i32, Physical>::from((100, 100));

        assert!(compact_damage(&[], output_size).is_empty());
    }

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
    fn compact_damage_does_not_widen_a_large_client_region_to_the_output() {
        let output_size = Size::<i32, Physical>::from((100, 100));
        let damage = [Rectangle::from_loc_and_size((0, 0), (80, 60))];

        let compacted = compact_damage(&damage, output_size);

        assert_eq!(compacted, damage);
        assert!(!is_full_damage(&compacted, output_size));
    }

    #[test]
    fn terminal_and_flow_damage_do_not_invalidate_unrelated_chrome() {
        let output_size = Size::<i32, Physical>::from((1920, 1080));
        let terminal = Rectangle::from_loc_and_size((96, 96), (1720, 920));
        let flow_field = Rectangle::from_loc_and_size((720, 20), (480, 48));

        let compacted = compact_damage(&[terminal, flow_field], output_size);

        assert_eq!(compacted.len(), 2);
        assert!(!is_full_damage(&compacted, output_size));
        assert!(compacted.contains(&terminal));
        assert!(compacted.contains(&flow_field));
        assert!(compacted.iter().all(|rect| !rect.contains((40, 540))));
    }

    #[test]
    fn client_recess_damage_does_not_invalidate_shell_chrome() {
        let logical_size = Size::<i32, Logical>::from((1920, 1080));
        let output_size = Size::<i32, Physical>::from((1920, 1080));
        let layout = build_chrome_layout(logical_size, 64, 76);
        let terminal = layout
            .work_area
            .recess
            .to_physical_precise_round(Scale::from((1.0, 1.0)));

        let shell =
            shell_overlay_damage(&[terminal], &layout, Scale::from((1.0, 1.0)), output_size);

        assert!(shell.is_empty());
    }

    #[test]
    fn mixed_client_and_ai_button_damage_redraws_only_that_chrome() {
        let logical_size = Size::<i32, Logical>::from((1920, 1080));
        let output_size = Size::<i32, Physical>::from((1920, 1080));
        let layout = build_chrome_layout(logical_size, 64, 76);
        let scale = Scale::from((1.0, 1.0));
        let terminal = layout.work_area.recess.to_physical_precise_round(scale);
        let ai_button = layout.topbar.ai_button.to_physical_precise_round(scale);

        let shell = shell_overlay_damage(&[terminal, ai_button], &layout, scale, output_size);

        assert_eq!(shell.len(), 1);
        assert!(shell.contains(&ai_button));
    }

    #[test]
    fn compact_damage_coalesces_transitive_neighbors() {
        let output_size = Size::<i32, Physical>::from((200, 100));
        let damage = [
            Rectangle::from_loc_and_size((10, 10), (10, 10)),
            Rectangle::from_loc_and_size((36, 10), (10, 10)),
            Rectangle::from_loc_and_size((23, 10), (10, 10)),
        ];

        let compacted = compact_damage(&damage, output_size);

        assert_eq!(
            compacted,
            vec![Rectangle::from_loc_and_size((10, 10), (36, 10))]
        );
    }

    #[test]
    fn overlapping_damage_is_coalesced_without_widening() {
        let output_size = Size::<i32, Physical>::from((100, 100));
        let repeated = Rectangle::from_loc_and_size((10, 10), (60, 50));

        let compacted = compact_damage(&[repeated, repeated], output_size);

        assert_eq!(compacted, vec![repeated]);
        assert!(!is_full_damage(&compacted, output_size));
    }

    #[test]
    fn small_surface_updates_avoid_nearly_all_4k_pixels() {
        let output_size = Size::<i32, Physical>::from((3840, 2160));
        // A 64x64 client update plus the one-pixel logical safety border at scale 1.
        let damage = [Rectangle::from_loc_and_size((100, 100), (66, 66))];

        let compacted = compact_damage(&damage, output_size);
        let damaged_permyriad = damage_area_permyriad(&compacted, output_size);

        assert_eq!(compacted, damage);
        assert!(
            damaged_permyriad <= 6,
            "expected <=0.06% of a 4K output, got {}.{:02}%",
            damaged_permyriad / 100,
            damaged_permyriad % 100
        );
    }

    #[test]
    fn commit_storm_compaction_keeps_a_bounded_render_list() {
        let output_size = Size::<i32, Physical>::from((1920, 1080));
        let damage: Vec<_> = (0..512)
            .map(|i| {
                let x = (i * 37) % 1900;
                let y = (i * 53) % 1060;
                Rectangle::from_loc_and_size((x, y), (8, 8))
            })
            .collect();

        let compacted = compact_damage(&damage, output_size);

        assert!(
            compacted.len() <= 8,
            "renderer received {} rectangles",
            compacted.len()
        );
        assert!(compacted
            .iter()
            .all(|rect| rect.overlaps(Rectangle::from_size(output_size))));
    }
}
