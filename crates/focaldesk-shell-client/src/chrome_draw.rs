//! GLES drawing primitives owned by the standalone shell renderer.

use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesPixelProgram, Uniform};
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Size};

use crate::chrome_theme::{BevelStyle, ButtonStyle, TopBarStyle};

#[inline]
fn to_physical_rect(rect: Rectangle<i32, Logical>, scale: Scale<f64>) -> Rectangle<i32, Physical> {
    rect.to_physical_precise_round(scale)
}

#[inline]
pub fn inset_rect(r: Rectangle<i32, Logical>, px: i32) -> Rectangle<i32, Logical> {
    Rectangle::from_loc_and_size(
        (r.loc.x + px, r.loc.y + px),
        ((r.size.w - px * 2).max(1), (r.size.h - px * 2).max(1)),
    )
}

pub fn well_icon_rect(well: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    inset_rect(well, (well.size.h / 5).max(4))
}

pub fn draw_top_bar(
    frame: &mut GlesFrame<'_, '_>,
    program: &GlesPixelProgram,
    rect: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    damage: &[Rectangle<i32, Physical>],
    style: &TopBarStyle,
) -> Result<(), GlesError> {
    let dst = to_physical_rect(rect, scale);
    let src = Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (dst.size.w as f64, dst.size.h as f64),
    );
    frame.render_pixel_shader_to(
        program,
        src,
        dst,
        Size::<i32, Buffer>::from((dst.size.w, dst.size.h)),
        Some(damage),
        1.0,
        &[
            Uniform::new("u_size", [dst.size.w as f32, dst.size.h as f32]),
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
    program: &GlesPixelProgram,
    rect: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    damage: &[Rectangle<i32, Physical>],
    style: &ButtonStyle,
) -> Result<(), GlesError> {
    let dst = to_physical_rect(rect, scale);
    let src = Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (dst.size.w as f64, dst.size.h as f64),
    );
    frame.render_pixel_shader_to(
        program,
        src,
        dst,
        Size::<i32, Buffer>::from((dst.size.w, dst.size.h)),
        Some(damage),
        1.0,
        &[
            Uniform::new("u_size", [dst.size.w as f32, dst.size.h as f32]),
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

pub fn draw_beveled_panel(
    frame: &mut GlesFrame<'_, '_>,
    program: &GlesPixelProgram,
    rect: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    damage: &[Rectangle<i32, Physical>],
    style: &BevelStyle,
) -> Result<(), GlesError> {
    let dst = to_physical_rect(rect, scale);
    let src = Rectangle::<f64, Buffer>::from_loc_and_size(
        (0.0, 0.0),
        (dst.size.w as f64, dst.size.h as f64),
    );
    frame.render_pixel_shader_to(
        program,
        src,
        dst,
        Size::<i32, Buffer>::from((dst.size.w, dst.size.h)),
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
