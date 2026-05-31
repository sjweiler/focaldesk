use std::time::{Duration, Instant};

use smithay::backend::renderer::gles::{GlesFrame, GlesRenderer, GlesTexture};
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Rectangle, Size};

use crate::core::chrome_layout::{build_chrome_layout, ChromeLayout};
use crate::core::desktop::DesktopState;
use crate::core::fonts::{FontId, TextStyle};
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

pub fn prepare_output(
    state: &mut DesktopState,
    renderer: &mut GlesRenderer,
    output_id: OutputId,
    buffer_size: Size<i32, Physical>,
    ui_state: &mut UiState<GlesTexture>,
    now: Instant,
    dt: Duration,
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

    let pointer_on_this_output = state.output_contains_pointer(output_id);

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
    state.update_ui_hover_for_output(output_id);

    state.render.ensure_shader_programs(renderer)?;
    // need to pass state.theme.wallpaper into this function so theme wallpaper can be loaded
    state.render.ensure_wallpaper_loaded(renderer);

    let active_theme = state.theme.active_theme();
    let active_theme_id = active_theme
        .id
        .builtin_id()
        .unwrap_or(BuiltInThemeId::Classic);

    let preload_fonts = match active_theme_id {
        BuiltInThemeId::Classic => [
            FontId::IbmPlexSansRegular,
            FontId::IbmPlexSansMedium,
            FontId::IbmPlexSansSemiBold,
        ],

        BuiltInThemeId::Moonbase => [
            FontId::RajdhaniRegular,
            FontId::RajdhaniMedium,
            FontId::RajdhaniSemiBold,
        ],

        BuiltInThemeId::Eagle => [
            FontId::OrbitronRegular,
            FontId::OrbitronMedium,
            FontId::OrbitronSemiBold,
        ],
    };

    for font in preload_fonts {
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

    // Dialog text uses 16px in draw_dialog; glyph keys include size, so we must
    // rasterize at 16px or draw_text_cached finds no glyphs.
    // let dialog_text_style = TextStyle {
    //     font: FontId::Debug,
    //     size_px: 16,
    // };
    //let dialog_text_style =
    // for d in &state.dialogs {
    //    state.fonts.prepare_text(&d.title, dialog_text_style)?;
    //    state.fonts.prepare_text(&d.message, dialog_text_style)?;
    //    for b in &d.buttons {
    //       state.fonts.prepare_text(&b.label, dialog_text_style)?;
    //   }
    //}

    if state.fonts.atlas_dirty {
        state.render.upload_font_atlas(renderer, &state.fonts)?;
        state.fonts.atlas_dirty = false;
    }

    let frame_ctx = FrameCtx {
        output_size: (buffer_size.w, buffer_size.h),
        output_scale,
        buffer_scale,
        damage: vec![Rectangle::from_loc_and_size(
            (0, 0),
            (buffer_size.w, buffer_size.h),
        )],
        work: Rectangle::from_loc_and_size((0, 0), (logical_w, logical_h)),
        frame_no: state.render.frame_no,
        now,
        dt,
        active_output: state.focused_output,
        rendering_output: output_id,
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

pub fn draw_output(
    state: &mut DesktopState,
    frame: &mut GlesFrame<'_, '_>,
    prepared: &PreparedOutput,
    elements: &[FlowRenderElement],
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
    };
    if prepared.frame_ctx.rendering_output == prepared.frame_ctx.active_output {
        state.sync_egui(&egui_frame_ctx);
    }

    let inputs = RenderInputs {
        ctx: &prepared.frame_ctx,
        layout: &prepared.layout,
        scene,
        output: output_state,
        metrics: &state.chrome.metrics,
        elements: &elements,
        popup_elements: &[],
        sidebar_hover_slot: state.sidebar_hover_for_output(prepared.frame_ctx.active_output),
        draw_software_cursor: prepared.draw_software_cursor,
        ui_tree: &state.ui,
        current_workspace: state
            .outputs
            .get(&prepared.frame_ctx.rendering_output)
            .map(|o| o.active_workspace)
            .unwrap_or(state.active_workspace),
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
