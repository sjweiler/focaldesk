use flowstate_themes::FlowTheme;
use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::Frame;
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesPixelProgram, Uniform};
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Size};

use crate::dialog::Dialog;
use crate::dialog_layout::DialogLayout;
use crate::desktop_frame::DesktopFrameCtx;

pub fn draw_solid_rect(
    frame: &mut GlesFrame<'_, '_>,
    dest: Rectangle<i32, Physical>,
    regions: &[Rectangle<i32, Physical>],
    color: [f32; 4],
) -> Result<(), GlesError> {
    if regions.is_empty() {
        return Ok(());
    }
    frame.draw_solid(
        dest,
        regions,
        Color32F::new(color[0], color[1], color[2], color[3]),
    )
}

#[inline]
fn to_physical_rect(
    rect_logical: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) -> Rectangle<i32, Physical> {
    rect_logical.to_physical_precise_round(scale)
}

pub fn draw_rounded_rect(
    frame: &mut GlesFrame<'_, '_>,
    program: &GlesPixelProgram,
    rect: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    radius: f32,
    color: [f32; 4],
    damage: &[Rectangle<i32, Physical>],
) -> Result<(), GlesError> {
    let rect_physical = to_physical_rect(rect, scale);
    let src_buffer = Rectangle::from_loc_and_size(
        (0.0, 0.0),
        (rect_physical.size.w as f64, rect_physical.size.h as f64),
    );
    let buffer_size = Size::<i32, Buffer>::from((rect_physical.size.w, rect_physical.size.h));

    frame.render_pixel_shader_to(
        program,
        src_buffer,
        rect_physical,
        buffer_size,
        Some(damage),
        1.0,
        &[
            Uniform::new("u_size", [rect_physical.size.w as f32, rect_physical.size.h as f32]),
            Uniform::new("u_radius", radius * scale.x as f32),
            Uniform::new("u_color", color),
        ],
    )
}

pub fn draw_dialog(
    frame: &mut GlesFrame<'_, '_>,
    frame_ctx: &DesktopFrameCtx,
    dialog: &Dialog,
    layout: &DialogLayout,
    rounded_program: &GlesPixelProgram,
    draw_panel: bool,
    theme: &FlowTheme,
) -> Result<(), GlesError> {
    let output_pixels = Size::<i32, Physical>::from(frame_ctx.output_size);
    let fb_physical =
        Rectangle::<i32, Physical>::from_loc_and_size((0, 0), output_pixels);
    let damage = frame_ctx.fullscreen_damage();

    let dim = theme.dialog.overlay_dim;
    draw_solid_rect(frame, fb_physical, &damage, dim)?;

    if !draw_panel {
        return Ok(());
    }

    draw_rounded_rect(
        frame,
        rounded_program,
        layout.bounds,
        frame_ctx.output_scale,
        8.0,
        [0.05, 0.07, 0.10, 0.9],
        &damage,
    )?;

    let _ = dialog;
    Ok(())
}
