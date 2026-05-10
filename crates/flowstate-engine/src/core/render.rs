use smithay::backend::renderer::gles::GlesRenderer;
use crate::core::app::App;
use smithay::utils::{Physical, Logical, Size, Rectangle, Scale, Point};
use smithay::backend::renderer::gles::GlesFrame;
use crate::core::layout::LayoutSnapshot;
use crate::core::output::OutputState; // if still needed (ideally not)
use crate::core::ui_state::UiState;
use std::time::{Duration, Instant};
use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::Frame;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::Texture;
use flowstate_ui::chrome::ChromeMetrics;
use image::GenericImageView;
use smithay::backend::renderer::ImportMem;
use smithay::backend::allocator::Fourcc;
use smithay::desktop::Space;
use smithay::desktop::Window;
use smithay::backend::renderer::element::AsRenderElements;
use smithay::utils::Transform;
use smithay::utils::Buffer;
use flowstate_ui::uitree::UiTree;
use flowstate_ui::types::UiElementKind;
//use flowstate_ui::atlas::render_atlas_icon_with_alpha;

use smithay::backend::renderer::gles::GlesTexProgram;

use crate::core::scene::SceneState;
//use crate::core::output::OutputId;
use flowstate_types::OutputId;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::element::render_elements;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Id, Kind};
use flowstate_cursor::{CursorIcon as FlowCursorIcon, CursorManager};
use flowstate_logging::flog;
use flowstate_ui::atlas::IconId;
use flowstate_ui::atlas::IconState;
//use flowstate_ui::atlas::render_atlas_icon_with_alpha;
use flowstate_resources::RenderResources;
use crate::core::desktop::DesktopState;
use smithay::backend::renderer::gles::Uniform;
use smithay::backend::renderer::gles::GlesError;
use smithay::backend::renderer::gles::GlesPixelProgram;
use crate::core::chrome_shaders::ChromeShaders;
use crate::core::chrome_layout::{ChromeLayout, ChromeLayoutLogical};
use flowstate_types::WorkspaceId;
use crate::core::shell::ManagedWindow;
use flowstate_ui::UiVisualStyle;
use flowstate_ui::visual_style;
use flowstate_ui::dialog_layout::layout_dialog;
use flowstate_ui::dialog_layout::DialogLayout;
use flowstate_ui::dialog::{Dialog, DialogId};
use crate::core::fonts::{FontId, FontSystem, TextStyle};
use flowstate_themes::FlowTheme;
use flowstate_themes::BackgroundTheme;
use flowstate_themes::WallpaperTheme;
use flowstate_themes::IconTheme;
use flowstate_themes::TextTheme;
use crate::core::fonts::style_for;
use crate::core::fonts::FontRole::Title;
use crate::core::fonts::FontRole;
use flowstate_themes::theme::BuiltInThemeId;


//use crate::core::chrome_svg::ChromeSvgCache;

//use crate::core::output::OutputState;
//use crate::core::ui::UiState;

render_elements! {
    pub FlowRenderElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
}

#[derive(Clone, Debug)]
pub struct FrameCtx {
    pub output_size: (i32, i32), // physical pixels
    pub output_scale: Scale<f64>,       // fractional
    pub buffer_scale: i32,       // integer >= 1
    pub damage: Vec<Rectangle<i32, Physical>>,
    pub work: Rectangle<i32, Logical>,
    pub frame_no: u64,
    pub now: std::time::Instant,
    pub dt: std::time::Duration,
    pub active_output: OutputId,
    pub rendering_output: OutputId,
    //pub time: f32,
}

impl FrameCtx {
    pub fn new(
        output_size: (i32, i32),
        output_scale: Scale<f64>,
        buffer_scale: i32,
        damage: Vec<Rectangle<i32, Physical>>,
        work: Rectangle<i32, Logical>,
        frame_no: u64,
        now: std::time::Instant,
        dt: std::time::Duration,
        active_output: OutputId,
        rendering_output: OutputId,
     ) -> Self {
        Self {
            output_size,
            output_scale,
            buffer_scale,
            damage,
            work,
            frame_no,
            now,
            dt,
            active_output,
            rendering_output,
        }
    }
}

pub struct RenderState {
    pub frame_no: u64,
    pub last_frame: Option<Instant>,
     // wallpaper texture, damage tracking, etc
    pub wallpaper_texture: Option<GlesTexture>,
    /// Software cursor: GPU texture + layout (physical pixels), when not on the DRM cursor plane.
    pub sw_cursor_texture: Option<GlesTexture>,
    sw_cursor_cache_key: Option<(FlowCursorIcon, u32, u32)>,
    pub sw_cursor_hotspot: (i32, i32),
    pub sw_cursor_tex_size: (i32, i32),
    pub sw_cursor_dst_rect: Option<(i32, i32, i32, i32)>,
    pub scratch_damage: [Rectangle<i32, Physical>; 8],
    pub scratch_damage_len: usize,
    pub resources: RenderResources,
    pub redraw_all: bool,
    pub chrome_shaders: ChromeShaders,
    pub start_time: Instant,
    //pub chrome_svg: ChromeSvgCache,
    pub font_atlas_texture: Option<GlesTexture>,

}

pub struct RenderInputs<'a> {
    pub ctx: &'a FrameCtx,
    pub layout: &'a ChromeLayout,
    pub scene: &'a SceneState,
    pub output: &'a OutputState,
    pub metrics: &'a ChromeMetrics,
    pub elements: &'a [FlowRenderElement],
    pub popup_elements: &'a [FlowRenderElement],
    pub sidebar_hover_slot: Option<usize>, // 👈 ADD THIS
    /// When true, composite the cursor from [`RenderState::sw_cursor_texture`] after chrome.
    pub draw_software_cursor: bool,
    pub ui_tree: &'a UiTree,
    pub current_workspace: WorkspaceId,   
    // 👇 ADD THESE
    pub dialogs: &'a [Dialog],
    pub active_dialog: Option<DialogId>,   
    pub fonts: &'a FontSystem,
    pub theme: &'a FlowTheme,
}

pub struct RenderInputsMut<'a> {
    pub ui: &'a mut UiState<GlesTexture>,
}
 
fn chrome_theme_from_flow_theme(
    chrome: &flowstate_themes::ChromeTheme,
) -> ChromeTheme {
    let mut legacy = default_chrome_theme();
    legacy.frame_outer.face_color = chrome.bg_color;
    legacy.panel_inner.face_color = chrome.panel_color;
    legacy.trim.face_color = chrome.trim_color;
    legacy.light.glow_color = chrome.accent_color;
    legacy.light.core_color = chrome.accent_color;
    legacy.button.glow_color = chrome.accent_color;
    legacy.glass.tint = chrome.glass_tint;
    legacy.top_bar.radius = chrome.corner_radius;
    legacy.top_bar.trim_color = chrome.trim_color;

    legacy
}

#[inline]
fn to_physical_rect(
    rect_logical: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) -> Rectangle<i32, Physical> {
    rect_logical.to_physical_precise_round(scale)
}


fn title_glyph_style(
    token: &str,
    in_meta: bool,
    is_active_output: bool,
) -> UiVisualStyle {
    let upper = token.to_ascii_uppercase();

    let is_separator = upper == ".";
    let is_meta_label = upper == "OUT" || upper == "WS";
    let is_meta_number = in_meta && upper.chars().all(|c| c.is_ascii_digit());

    if is_meta_number {
        if is_active_output {
            UiVisualStyle {
                tint: [1.0, 0.72, 0.22, 1.0],
                glow: 0.25,
                alpha: 1.0,
                scale: 1.0,
            }
        } else {
            UiVisualStyle {
                tint: [1.0, 0.72, 0.22, 0.45],
                glow: 0.0,
                alpha: 0.45,
                scale: 1.0,
            }
        }
    } else if is_meta_label || is_separator || in_meta {
        UiVisualStyle {
            tint: [0.45, 0.65, 0.95, if is_active_output { 0.42 } else { 0.28 }],
            glow: 0.0,
            alpha: if is_active_output { 0.42 } else { 0.28 },
            scale: 1.0,
        }
    } else {
        UiVisualStyle {
            tint: if is_active_output {
                [1.0, 1.0, 1.0, 0.75]
            } else {
                [0.70, 0.80, 1.0, 0.38]
            },
            glow: 0.0,
            alpha: if is_active_output { 0.75 } else { 0.38 },
            scale: 1.0,
        }
}
} 
 
fn clock_glyph_style(ch: char) -> UiVisualStyle {
    match ch {
        'A' | 'P' | 'M' => UiVisualStyle {
            tint: [0.45, 0.65, 0.95, 0.65], // dim blue
            glow: 0.0,
            alpha: 0.65,
            scale: 1.0,
        },
        ':' => UiVisualStyle {
            tint: [1.0, 0.72, 0.22, 0.85], // amber but softer
            glow: 0.0,
            alpha: 0.85,
            scale: 1.0,
        },
        _ => UiVisualStyle {
            tint: [1.0, 0.85, 0.45, 1.0], // bright clock digits
            glow: 0.0,
            alpha: 1.0,
            scale: 1.0,
        },
    }
}

 
fn char_to_clock_icon(c: char) -> Option<IconId> {
    match c {
        '0' => Some(IconId::Clock0),
        '1' => Some(IconId::Clock1),
        '2' => Some(IconId::Clock2),
        '3' => Some(IconId::Clock3),
        '4' => Some(IconId::Clock4),
        '5' => Some(IconId::Clock5),
        '6' => Some(IconId::Clock6),
        '7' => Some(IconId::Clock7),
        '8' => Some(IconId::Clock8),
        '9' => Some(IconId::Clock9),
        ':' => Some(IconId::ClockColon),
        'A' => Some(IconId::ClockA),
        'P' => Some(IconId::ClockP),
        'M' => Some(IconId::ClockM),
        _ => None,
    }
} 

fn glyph_for_char(c: char) -> Option<IconId> {
    match c.to_ascii_uppercase() {
        'A' => Some(IconId::A),
        'B' => Some(IconId::B),
        'C' => Some(IconId::C),
        'D' => Some(IconId::D),
        'E' => Some(IconId::E),
        'F' => Some(IconId::F),
        'G' => Some(IconId::G),
        'H' => Some(IconId::H),
        'I' => Some(IconId::I),
        'J' => Some(IconId::J),
        'K' => Some(IconId::K),
        'L' => Some(IconId::L),
        'M' => Some(IconId::M),
        'N' => Some(IconId::N),
        'O' => Some(IconId::O),
        'P' => Some(IconId::P),
        'Q' => Some(IconId::Q),
        'R' => Some(IconId::R),
        'S' => Some(IconId::S),
        'T' => Some(IconId::T),
        'U' => Some(IconId::U),
        'V' => Some(IconId::V),
        'W' => Some(IconId::W),
        'X' => Some(IconId::X),
        'Y' => Some(IconId::Y),
        'Z' => Some(IconId::Z),

        '0' => Some(IconId::Num0),
        '1' => Some(IconId::Num1),
        '2' => Some(IconId::Num2),
        '3' => Some(IconId::Num3),
        '4' => Some(IconId::Num4),
        '5' => Some(IconId::Num5),
        '6' => Some(IconId::Num6),
        '7' => Some(IconId::Num7),
        '8' => Some(IconId::Num8),
        '9' => Some(IconId::Num9),

        ':' => Some(IconId::Colon),
        '-' => Some(IconId::Dash),
        '.' => Some(IconId::Dot),
        '/' => Some(IconId::Slash),
        '%' => Some(IconId::Percent),

        _ => None,
    }
}

fn glyphs_for_text(text: &str) -> Vec<IconId> {
    text.chars().filter_map(glyph_for_char).collect()
}

fn icon_tint(state: IconState, alpha: f32) -> [f32; 4] {
    match state {
        IconState::Active => [0.55, 0.72, 0.92, alpha],
        IconState::Hover => [0.95, 0.68, 0.20, alpha],
        IconState::Inactive => [0.28, 0.42, 0.58, alpha],
    }
}

impl RenderState {
    pub fn new() -> Self {
        let zero: Rectangle<i32, Physical> = Rectangle::from_size(Size::from((0, 0)));
        Self {
        
            last_frame: None,
            frame_no: 0,
            scratch_damage: [zero; 8],
            scratch_damage_len: 0,
            wallpaper_texture: None,
            sw_cursor_texture: None,
            sw_cursor_cache_key: None,
            sw_cursor_hotspot: (0, 0),
            sw_cursor_tex_size: (0, 0),
            sw_cursor_dst_rect: None,
            resources: RenderResources::new(), 
            redraw_all: true,
            chrome_shaders: ChromeShaders::new(),
            start_time: Instant::now(),
            
            font_atlas_texture: None,
        }
    }


fn draw_topbar_meta(
    &mut self,
    frame: &mut GlesFrame<'_, '_>,
    fonts: &FontSystem,
    layout: &ChromeLayoutLogical,
    title: &str,
    output_number: u64,
    workspace_number: usize,
    theme: &flowstate_themes::FlowTheme,
    scale: Scale<f64>,
) -> Result<(), GlesError> {

    let builtin_id = theme.id.builtin_id().unwrap_or(BuiltInThemeId::Eagle);
    let title_style = style_for(FontRole::Title, 24, builtin_id);
    let style = style_for(FontRole::Meta, 18, builtin_id);
    let gap = theme.spacing.max(4);

    let y_logical = layout.title_rect.loc.y + 24;
    // Same left inset as `draw_topbar_title`, then skip past measured title so meta never overlaps.
    let title_left_logical = layout.title_rect.loc.x + 14;
    let mut x_logical = title_left_logical + fonts.advance_width(title, title_style) + gap;

    let output_s = output_number.to_string();
    let workspace_s = workspace_number.to_string();

    self.draw_text_cached(
        frame,
        fonts,
        "OUT",
        x_logical,
        y_logical,
        style,
        theme.text.meta_label,
        scale,
    )?;

    x_logical += fonts.advance_width("OUT", style) + gap;

    self.draw_text_cached(
        frame,
        fonts,
        &output_s,
        x_logical,
        y_logical,
        style,
        theme.text.meta_value,
        scale,
    )?;

    x_logical += fonts.advance_width(&output_s, style) + gap;

    self.draw_text_cached(
        frame,
        fonts,
        "WS",
        x_logical,
        y_logical,
        style,
        theme.text.meta_label,
        scale,
    )?;

    x_logical += fonts.advance_width("WS", style) + gap;

    self.draw_text_cached(
        frame,
        fonts,
        &workspace_s,
        x_logical,
        y_logical,
        style,
        theme.text.meta_value,
        scale,
    )?;

    Ok(())
}

fn draw_topbar_title(
    &mut self,
    frame: &mut GlesFrame<'_, '_>,
    fonts: &FontSystem,
    layout: &ChromeLayoutLogical,
    title: &str,
    theme: &flowstate_themes::FlowTheme,
    scale: Scale<f64>,
) -> Result<(), GlesError> {

     let builtin_id = theme
        .id
        .builtin_id()
        .unwrap_or(BuiltInThemeId::Eagle);

 let style = style_for(FontRole::Title, 24, builtin_id);
// let style = TextStyle {
//    font: FontId::Debug,
//    size_px: 24,
//};
    let x_logical = layout.title_rect.loc.x + 14; // 120;
    let y_logical = layout.title_rect.loc.y + 24;

    self.draw_text_cached(
        frame,
        fonts,
        title,
        x_logical,
        y_logical,
        style,
        theme.text.title,
        scale,
    )?;

    Ok(())
}



pub fn draw_text_cached(
    &self,
    frame: &mut GlesFrame<'_, '_>,
    fonts: &FontSystem,
    text: &str,
    x: i32,
    baseline_y: i32,
    style: TextStyle,
    color: [f32; 4],
    scale: Scale<f64>,
) -> Result<(), GlesError> {
    let tex = match self.font_atlas_texture.as_ref() {
        Some(t) => t,
        None => return Ok(()),
    };

    let program = self
        .chrome_shaders
        .font_text
        .as_ref()
        .expect("font shader missing");

    let mut cursor_x = x;

    for ch in text.chars() {
        if ch == ' ' {
            cursor_x += style.size_px as i32 / 2;
            continue;
        }

        let Some(glyph) = fonts.glyph((style.font, style.size_px, ch)) else {
            continue;
        };

        let dst_logical = Rectangle::<i32, Logical>::from_loc_and_size(
            (
                cursor_x + glyph.xmin,
                baseline_y - glyph.ymin - glyph.h as i32,
            ),
            (glyph.w as i32, glyph.h as i32),
        );

        let dst = dst_logical.to_physical_precise_round(scale);

        // `render_texture_from_to` / build_texture_mat expect src in texture pixels;
        // it normalizes to UVs internally. Do not divide by atlas size here.
        let src = Rectangle::<f64, Buffer>::from_loc_and_size(
            (glyph.atlas_x as f64, glyph.atlas_y as f64),
            (glyph.w as f64, glyph.h as f64),
        );

let damage_local =
    Rectangle::<i32, Physical>::from_loc_and_size((0, 0), (dst.size.w, dst.size.h));

        if let Err(e) = frame.render_texture_from_to(
            &tex,
            src,
            dst,
            &[damage_local],
            &[],
            Transform::Normal,
            1.0,
            Some(program),
            &[Uniform::new("u_tint", color)],
        ) {
            eprintln!("tinted icon render failed: {:?}", e);
        }

        // ✅ MUST be inside loop
        cursor_x += glyph.advance.round() as i32;
    }

    Ok(())
}


pub fn upload_font_atlas(
    &mut self,
    renderer: &mut GlesRenderer,
    fonts: &FontSystem,
) -> Result<(), GlesError> {
    let (w, h) = fonts.atlas_size();

    let mut rgba = Vec::with_capacity((w * h * 4) as usize);

    // Grayscale coverage in R/G/B equally so it survives BGRA vs RGBA swizzle bugs;
    // A repeats coverage so shaders can sample either `.r` or `.a`.
    //
    // `Abgr8888` → GLES RGBA/RGBA8 path (preferred on GLES; see drm copy notes in this crate).
    for &cov in fonts.atlas_pixels() {
        rgba.extend_from_slice(&[cov, cov, cov, cov]);
    }

    let texture = renderer.import_memory(
        &rgba,
        Fourcc::Abgr8888,
        Size::from((w as i32, h as i32)),
        false,
    )?;

    self.font_atlas_texture = Some(texture);

    Ok(())
}
   
    
    pub fn clear_sw_cursor_texture(&mut self) {
        self.sw_cursor_texture = None;
        self.sw_cursor_cache_key = None;
        self.sw_cursor_dst_rect = None;
    }

    /// Upload cursor RGBA for DRM scan-out and/or software overlay; cheap when the pixmap is unchanged.
    pub fn upload_cursor_texture_for_desktop(
        &mut self,
        renderer: &mut GlesRenderer,
        mgr: &mut CursorManager,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let key_icon = mgr.current_flow_icon();
        let img = match mgr.current_image() {
            Ok(i) => i,
            Err(_) => return Ok(()),
        };
        let key = (key_icon, img.width, img.height);
        if self.sw_cursor_cache_key == Some(key) && self.sw_cursor_texture.is_some() {
            self.sw_cursor_hotspot = (img.hotspot_x as i32, img.hotspot_y as i32);
            self.sw_cursor_tex_size = (img.width as i32, img.height as i32);
            return Ok(());
        }

        let mut rgba = img.rgba.clone();
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let w = img.width as i32;
        let h = img.height as i32;
        let tex = renderer.import_memory(&rgba, Fourcc::Argb8888, (w, h).into(), false)?;
        self.sw_cursor_texture = Some(tex);
        self.sw_cursor_cache_key = Some(key);
        self.sw_cursor_hotspot = (img.hotspot_x as i32, img.hotspot_y as i32);
        self.sw_cursor_tex_size = (w, h);
        Ok(())
    }

    fn draw_software_cursor_overlay(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
    ) -> Result<(), GlesError> {
        let Some((dx, dy, _w, _h)) = self.sw_cursor_dst_rect else {
            return Ok(());
        };
        let Some(tex) = self.sw_cursor_texture.as_ref() else {
            return Ok(());
        };
        let full = Rectangle::from_loc_and_size((0, 0), ctx.output_size);
        let damage = std::slice::from_ref(&full);
        let elem = TextureRenderElement::from_static_texture(
            Id::new(),
            frame.context_id(),
            (dx as f64, dy as f64),
            tex.clone(),
            1,
            Transform::Normal,
            Some(1.0),
            None,
            None,
            None,
            Kind::Unspecified,
        );
        draw_render_elements::<GlesRenderer, Scale<f64>, TextureRenderElement<GlesTexture>>(
            frame,
            ctx.output_scale,
            std::slice::from_ref(&elem),
            damage,
        )?;
        Ok(())
    }

fn render_icon_with_tint(
    frame: &mut GlesFrame<'_, '_>,
    atlas: &flowstate_ui::atlas::IconAtlas,
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

        //let tint = icon_tint(state, alpha);

        // Smithay expects `damage` in dest-local space, not output coordinates.
        let damage_local_physical =
            Rectangle::from_loc_and_size((0, 0), (rect_physical.size.w, rect_physical.size.h));

        if let Err(e) = frame.render_texture_from_to(
            &atlas.texture,
            src,
            rect_physical,
            &[damage_local_physical],
            &[],
            Transform::Normal,
            style.alpha, //1.0,
            Some(program),
            &[Uniform::new("u_tint", style.tint)], //&[Uniform::new("u_tint", tint)],
        ) {
            eprintln!("tinted icon render failed for {:?}: {:?}", icon, e);
        }
    }
}


fn draw_title_text(
    frame: &mut GlesFrame<'_, '_>,
    atlas: &flowstate_ui::atlas::IconAtlas,
    title_rect_logical: Rectangle<i32, Logical>,
    text: &str,
    is_active: bool,
    scale: Scale<f64>,
    program: &GlesTexProgram,
) -> Result<(), GlesError> {
    if text.is_empty() {
        return Ok(());
    }

    let pad_x = 8;
    let pad_y = 4;
    let gap = 1;

    let glyph_h = (title_rect_logical.size.h - pad_y * 2).max(8);
    let glyph_w = ((glyph_h as f32) * 0.82) as i32;

    let mut x_logical = title_rect_logical.loc.x + pad_x;
    let y_logical = title_rect_logical.loc.y + ((title_rect_logical.size.h - glyph_h) / 2);
    let max_right_logical = title_rect_logical.loc.x + title_rect_logical.size.w - pad_x;

    let mut in_meta = false;

    for token in text.split_whitespace() {
        let upper = token.to_ascii_uppercase();

        let is_meta_label = upper == "OUT" || upper == "WS";

        let style = title_glyph_style(token, in_meta, is_active);

        for glyph in glyphs_for_text(token) {
            let dest_logical = Rectangle::from_loc_and_size(
                (x_logical, y_logical),
                (glyph_w, glyph_h),
            );

            if dest_logical.loc.x + dest_logical.size.w > max_right_logical {
                return Ok(());
            }

            RenderState::render_icon_with_tint(
                frame,
                atlas,
                glyph,
                dest_logical,
                scale,
                style,
                program,
            );

            x_logical += glyph_w + gap;
        }

        x_logical += glyph_w * 2;

        if x_logical >= max_right_logical {
            break;
        }

        if is_meta_label {
            in_meta = true;
        }
    }

    Ok(())
}

/*
fn draw_title_text(
    frame: &mut GlesFrame<'_, '_>,
    atlas: &flowstate_ui::atlas::IconAtlas,
    title_rect: Rectangle<i32, Physical>,
    text: &str,
    is_active: bool,
    program: &GlesTexProgram,
) -> Result<(), GlesError> {
    if text.is_empty() {
        return Ok(());
    }

    let pad_x = 8;
    let pad_y = 4;
    let gap = 1;

    let glyph_h = (title_rect.size.h - pad_y * 2).max(8);
    let glyph_w = ((glyph_h as f32) * 0.82) as i32;

    let mut x = title_rect.loc.x + pad_x;
    let y = title_rect.loc.y + ((title_rect.size.h - glyph_h) / 2);
    let max_right = title_rect.loc.x + title_rect.size.w - pad_x;

    let title_intensity = if is_active { 0.75 } else { 0.38 };
    let meta_intensity  = if is_active { 0.42 } else { 0.22 };
    let mut in_meta = false;

    for token in text.split_whitespace() {
        let upper = token.to_ascii_uppercase();

        let is_separator = upper == ".";
        let is_meta_label = upper == "OUT" || upper == "WS";
        //let is_meta_number = in_meta && upper.chars().all(|c| c.is_ascii_digit());
        let is_output_number = in_meta && upper.chars().all(|c| c.is_ascii_digit());

        let is_active_output_number = is_output_number && is_active;

        // Only the actual OUT/WS values are amber.
        let (state, alpha) = if is_active_output_number {    
            (IconState::Hover, 1.0)
        } else if is_meta_label || is_separator || in_meta {
            // OUT, WS, separators, and other meta text are dim.
            //(IconState::Inactive, meta_intensity)
            (IconState::Inactive, meta_intensity)
        } else {
            // FLOWSTATE title stays bright.
            (IconState::Active, title_intensity)
        };

        let tint = icon_tint(state, alpha);
        
        for glyph in glyphs_for_text(token) {
            let dest = Rectangle::from_loc_and_size((x, y), (glyph_w, glyph_h));

            if dest.loc.x + dest.size.w > max_right {
                return Ok(());
            }

            RenderState::render_icon_with_tint(
                frame,
                atlas,
                glyph,
                dest,
                style,
                program,
            );

            x += glyph_w + gap;
        }

        //x += glyph_w + gap;
        x += glyph_w * 2; // instead of + gap after tokens

        if x >= max_right {
            break;
        }

        if is_meta_label {
            in_meta = true;
        }
    }

    Ok(())
}*/
/*
    fn draw_icon_in_rect_with_alpha(
        frame: &mut GlesFrame<'_, '_>,
        atlas: &flowstate_ui::atlas::IconAtlas,
        icon: IconId,
        state: IconState,
        rect: Rectangle<i32, Physical>,
        alpha: f32,
        program: &GlesTexProgram,
    ) {
        self.render_icon_with_tint(frame, atlas, icon, state, rect, 1.0, program);
        //if let Some(entry) = atlas.get(icon) {
        //    if let Err(e) = render_atlas_icon_with_alpha(
       //         frame,
       //         &atlas.texture,
       //         *entry,
       //         rect.loc.x,
       //         rect.loc.y,
      //          rect.size.w,
      //          rect.size.h,
      //          alpha,
       //     ) {
      //          eprintln!("render_atlas_icon failed for {:?}: {:?}", icon, e);
       //     }
        //}
    }
*/

    fn draw_icon_in_rect(
        frame: &mut GlesFrame<'_, '_>,
        atlas: &flowstate_ui::atlas::IconAtlas,
        icon: IconId,
        state: IconState,
        rect_logical: Rectangle<i32, Logical>,
        scale: Scale<f64>,
        style: UiVisualStyle,
        program: &GlesTexProgram,
    ) {
        RenderState::render_icon_with_tint(frame, atlas, icon, rect_logical, scale, style, program);
    }

    pub fn ensure_shader_programs(
        &mut self,
        renderer: &mut GlesRenderer,
    ) -> Result<(), GlesError> {
        self.chrome_shaders.ensure_compiled(renderer)    
    }
  
  
  fn draw_clock_text(
    &self,
    frame: &mut GlesFrame<'_, '_>,
    atlas: &flowstate_ui::atlas::IconAtlas,
    text: &str,
    rect_logical: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    program: &GlesTexProgram,
) {
    let glyphs: Vec<char> = text.chars().filter(|c| *c != ' ').collect();
    if glyphs.is_empty() {
        return;
    }

    let digit_h = (rect_logical.size.h - 6).max(8);
    let digit_w = ((digit_h as f32) * 0.72) as i32;
    let colon_w = (digit_w / 2).max(4);
    let ampm_w = ((digit_w as f32) * 0.78) as i32;

    let mut widths = Vec::with_capacity(glyphs.len());
    for ch in &glyphs {
        let w = match *ch {
            ':' => colon_w,
            'A' | 'P' | 'M' => ampm_w,
            _ => digit_w,
        };
        widths.push(w);
    }

    let spacing = 2;
    let total_w: i32 = widths.iter().sum::<i32>() + spacing * (widths.len().saturating_sub(1) as i32);

    let start_x_logical = rect_logical.loc.x + ((rect_logical.size.w - total_w).max(0) / 2);
    let y_logical = rect_logical.loc.y + ((rect_logical.size.h - digit_h) / 2);

    let mut x_logical = start_x_logical;

    for (idx, ch) in glyphs.iter().enumerate() {
        if let Some(icon) = char_to_clock_icon(*ch) {
            let w = widths[idx];

            let glyph_rect_logical = Rectangle::from_loc_and_size(
                (x_logical, y_logical),
                (w, digit_h),
            );

            //let state = match *ch {
            //    'A' | 'P' | 'M' => IconState::Inactive,
            //    _ => IconState::Active,
            //};

            let style = clock_glyph_style(*ch);

            Self::render_icon_with_tint(
                frame,
                atlas,
                icon,
                glyph_rect_logical,
                scale,
                style,
                program,
            );

            //Self::draw_icon_in_rect(frame, atlas, icon, state, glyph_rect, 1.0, program);
            x_logical += w + spacing;
        }
    }
}
   
fn draw_clock_font_text(
    &mut self,
    frame: &mut GlesFrame<'_, '_>,
    fonts: &FontSystem,
    text: &str,
    rect_logical: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    style: TextStyle,
    color: [f32; 4],
) -> Result<(), Box<dyn std::error::Error>> {
    let text_w = fonts.advance_width(text, style);
    let x_logical = rect_logical.loc.x + ((rect_logical.size.w - text_w).max(0) / 2);
    let y_logical = rect_logical.loc.y + 24;

    self.draw_text_cached(
        frame,
        fonts,
        text,
        x_logical,
        y_logical,
        style,
        color,
        scale,
    )?;

    Ok(())
}

    pub fn ensure_wallpaper_loaded(&mut self, renderer: &mut GlesRenderer)
    {
        if self.wallpaper_texture.is_some() {
            return;
        }
        eprintln!("ensure_wallpaper_loaded: attempting load…");

        let tex = Self::load_wallpaper(
            renderer,
            //"/home/steve/flowstate/assets/wallpaper/doctor.png",
            //"/home/steve/flowstate/assets/wallpaper/WALLPAPER.png",
            "/home/steve/flowstate/assets/wallpaper/ChatGPT Image Mar 29, 2026, 07_11_29 PM.png",
        );
//"/home/steve/flowstate/assets/icons/sidebar/launcher_56.png",
            //"/tmp/atlas-debug.png",
            //"/home/steve/flowstate/assets/wallpaper/flowstate_wallpaper_tagline_5k.png",
            //"/home/steve/flowstate/assets/wallpaper/ChatGPT Image Mar 26, 2026, 08_07_23 AM.png",
            //"/home/steve/flowstate/assets/wallpaper/ChatGPT Image Mar 29, 2026, 07_11_29 PM.png",
            //"/home/steve/flowstate/assets/wallpaper/WALLPAPER.png",
        eprintln!(
            "ensure_wallpaper_loaded: load result is_some={}",
            tex.is_some()
        );

        self.wallpaper_texture = tex;
    }
    
    pub fn load_wallpaper(renderer: &mut GlesRenderer, path: &str) -> Option<GlesTexture> {
        eprintln!("load_wallpaper: opening {path}");

        let img = match image::open(path) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("load_wallpaper: image::open failed: {e:?}");
                return None;
            }
        };

        let (w, h) = img.dimensions();
        eprintln!("load_wallpaper: decoded {w}x{h}");

        //let rgba = img.to_rgba8();
        let mut rgba = img.to_rgba8();

        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2); // swap R and B
        }

        //let fourcc = Fourcc::Rgba8888;
        eprintln!("load_wallpaper: rgba bytes={}", rgba.len());

        // IMPORTANT: your buffer is RGBA; ABGR is often wrong here.
        let fourcc = Fourcc::Argb8888; // try this first
        eprintln!("load_wallpaper: importing to GPU as {fourcc:?}");

        match renderer.import_memory(&rgba, fourcc, (w as i32, h as i32).into(), false) {
            Ok(tex) => {
                eprintln!("load_wallpaper: import_memory OK");
                Some(tex)
            }
            Err(e) => {
                eprintln!("load_wallpaper: import_memory FAILED: {e:?}");
                None
            }
        }
    }
    
    fn draw_top_bar(
    frame: &mut GlesFrame<'_, '_>,
    program: &GlesPixelProgram,
    rect_logical: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    damage: &[Rectangle<i32, Physical>],
    style: &TopBarStyle,
    ) -> Result<(), GlesError> {
        use smithay::backend::renderer::gles::Uniform;
        use smithay::utils::{Buffer, Rectangle, Size};
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
                Uniform::new("u_size", [rect_physical.size.w as f32, rect_physical.size.h as f32]),
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
    ) {
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
                Uniform::new("u_size", [rect_physical.size.w as f32, rect_physical.size.h as f32]),
                Uniform::new("u_bevel", style.bevel),
                Uniform::new("u_softness", style.softness),
                Uniform::new("u_inner_shadow", style.inner_shadow),
                Uniform::new("u_glow_strength", style.glow_strength),
                Uniform::new("u_glow_radius", style.glow_radius),
                Uniform::new("u_face_color", style.face_color),
                Uniform::new("u_shadow_color", style.shadow_color),
                Uniform::new("u_glow_color", style.glow_color),
            ],
        );
    }

    fn rect_apply_flipped180(
        r: Rectangle<i32, Physical>,
        output_size: (i32, i32),
    ) -> Rectangle<i32, Physical> {
        let (W, H) = output_size;
        Rectangle::new(
            ((W - (r.loc.x + r.size.w)), (H - (r.loc.y + r.size.h))).into(),
            r.size,
        )
    }

    fn draw_popup_elements(
    &mut self,
    frame: &mut GlesFrame<'_, '_>,
    ctx: &FrameCtx,
    elements: &[FlowRenderElement],
    ) -> Result<(), GlesError> {
        let full = Rectangle::from_loc_and_size((0, 0), ctx.output_size);
        let damage = std::slice::from_ref(&full);

        draw_render_elements(
            frame,
            ctx.output_scale.x,
            elements,
            damage,
        )?;

        Ok(())
    }

pub fn draw_rounded_rect(
    &mut self,
    frame: &mut GlesFrame<'_, '_>,
    rect: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    radius: f32,
    color: [f32; 4],
) -> Result<(), GlesError> {
    let program = match self.chrome_shaders.rounded_rect.as_ref() {
        Some(p) => p,
        None => return Ok(()),
    };
    
    let dst_phys: Rectangle<i32, Physical> =
        rect.to_physical_precise_round::<f64, i32>(scale);
    
    let src_buffer: Rectangle<f64, Buffer> = Rectangle::from_size((
        dst_phys.size.w as f64,
        dst_phys.size.h as f64,
    ).into());

    let buffer_size: Size<i32, Buffer> = (
        dst_phys.size.w,
        dst_phys.size.h,
    ).into();
    
   let uniforms = [
        Uniform::new("u_size", [dst_phys.size.w as f32, dst_phys.size.h as f32]),
        Uniform::new("u_radius", radius * scale.x as f32),
        Uniform::new("u_color", color),
    ];

    frame.render_pixel_shader_to(
        program,
        src_buffer,
        dst_phys,
        buffer_size,
        None,
        1.0,
        &uniforms,
    )
}
        
   // 1) CHANGE render_into_frame()

    pub fn render_into_frame(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        inputs: RenderInputs<'_>,
        muts: RenderInputsMut<'_>,
    ) -> Result<(), GlesError> {
                
        let theme = inputs.theme;
        
        self.draw_background(frame, inputs.ctx, inputs.output, theme.background);

        // Chrome draws opaque bevels over the work region; clients must be composited
        // after that shell (and work-area wallpaper), or they are fully covered.
        self.draw_chrome_below_work_wallpaper(
            frame,
            inputs.ctx,
            inputs.layout,
            inputs.output,
            inputs.metrics,
            muts.ui,
            inputs.sidebar_hover_slot,
            theme.chrome,
        );
        
        self.draw_wallpaper_in_rect(
            frame,
            inputs.ctx,
            inputs.layout.work_recess,
            inputs.ctx.output_scale,
            theme.wallpaper.clone(),
        );
        
        self.draw_clients(
            frame,
            inputs.ctx,
            inputs.scene,
            inputs.output,
            inputs.elements,
        );
        
        
        self.draw_chrome_trim_glass_icons(
            frame,
            inputs.ctx,
            inputs.layout,
            inputs.output,
            inputs.metrics,
            muts.ui,
            inputs.ui_tree,
            inputs.current_workspace,
            inputs.fonts,
            theme,
        );

        // xdg popups are included in [`Window::render_elements`] when [`PopupManager::commit`] runs.
        self.draw_popup_elements(frame, inputs.ctx, inputs.popup_elements)?;
        
        // notifications
        // this will render notifications here in future - this is a placeholder
        println!(
    "dialogs={}, active_dialog={:?}",
    inputs.dialogs.len(),
    inputs.active_dialog
);
        let program = self.chrome_shaders.tinted_icon.clone();

        if let (Some(atlas), Some(program)) = (
            muts.ui.chrome.atlas.as_ref(),
            program.as_ref(),
        ) {
            let output_px: Size<i32, Physical> = inputs.ctx.output_size.into();
            // Modal scrim: every output. Panel / text: only on the output that opened the dialog.
            let draw_dialog_chrome = inputs
                .active_dialog
                .and_then(|id| {
                    inputs
                        .dialogs
                        .iter()
                        .find(|d| d.id == id)
                })
                .is_some_and(|d| d.owner_output == inputs.ctx.rendering_output);
            self.render_active_dialog_for_output(
                frame,
                inputs.ctx.work,
                output_px,
                inputs.dialogs,
                inputs.active_dialog,
                draw_dialog_chrome,
                atlas,
                program,
                inputs.fonts,
                inputs.ctx.output_scale,
                theme,
            )?;
        }
            
          
        
           //self.draw_text_test(frame, inputs.fonts)?; 
        
        
        if inputs.draw_software_cursor {
            self.draw_software_cursor_overlay(frame, inputs.ctx)?;
        }

        Ok(())
    }

     fn render_active_dialog_for_output(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        // Logical chrome / work area (`FrameCtx::work`).
        screen_logical: Rectangle<i32, Logical>,
        output_pixels: Size<i32, Physical>,
        dialogs: &[Dialog],
        active_dialog: Option<DialogId>,
        draw_dialog_chrome: bool,
        atlas: &flowstate_ui::atlas::IconAtlas,
        program: &GlesTexProgram,
        fonts: &FontSystem,
        scale: Scale<f64>,
        theme: &FlowTheme,
    ) -> Result<(), GlesError> {
        let Some(dialog_id) = active_dialog else {
            return Ok(());
        };

        let Some(dialog) = dialogs.iter().find(|d| d.id == dialog_id) else {
            return Ok(());
        };

        let layout = layout_dialog(dialog, screen_logical);

        self.draw_dialog(
            frame,
            dialog,
            &layout,
            atlas,
            program,
            fonts,
            output_pixels,
            scale,
            draw_dialog_chrome,
            theme,
        )?;

        Ok(())
    }
     
fn draw_dialog(
    &mut self,
    frame: &mut GlesFrame<'_, '_>,
    dialog: &Dialog,
    layout: &DialogLayout,
    atlas: &flowstate_ui::atlas::IconAtlas,
    program: &GlesTexProgram,
    fonts: &FontSystem,
    output_pixels: Size<i32, Physical>,
    scale: Scale<f64>,
    // When false: fullscreen dim scrim only (other DRM heads while modal is up).
    draw_dialog_chrome: bool,
    theme: &FlowTheme,
) -> Result<(), GlesError> {
    // Full framebuffer in physical px; dialogs are laid out in logical space (`layout`) then lifted.
    // Must use `Frame::draw_solid`, not `clear`: smithay `clear` disables blending, so translucent
    // RGBA wipes the scene (often reads as black) instead of dimming composited content.
    let fb_physical = Rectangle::<i32, Physical>::from_loc_and_size((0, 0), output_pixels);

    RenderState::draw_solid_rect(frame, fb_physical, &[fb_physical], [0.0, 0.0, 0.0, 0.45])?;

    if !draw_dialog_chrome {
        return Ok(());
    }

    if self.font_atlas_texture.is_none() {
    flog("FONT TEXT: missing font_atlas_texture");
    return Ok(());
}
if self.chrome_shaders.font_text.is_none() {
    flog("FONT DRAW: missing font_text shader");
    return Ok(());
}

   // let panel_phys = layout.bounds.to_physical_precise_round(scale);

    //RenderState::draw_solid_rect(frame, fb, &[panel_phys], [0.08, 0.12, 0.16, 0.95])?;
    self.draw_rounded_rect(
        frame,
        layout.bounds,
        scale,
        8.0,
        [0.05, 0.07, 0.10, 0.9],
    )?;

    // `draw_text_cached` takes logical coords and lifts with `scale` (matches panel rects).
    let title_baseline =
        layout.title_rect.loc.y + layout.title_rect.size.h - 8;
    let mut y = layout.message_rect.loc.y + 20;

flog(&format!("DRAW REAL FONT TITLE: {}", dialog.title));
    self.draw_text_cached(
        frame,
        fonts,
        &dialog.title,
        layout.title_rect.loc.x,
        title_baseline,
        style_for(FontRole::Title, 16, theme.id.builtin_id().unwrap_or(BuiltInThemeId::Eagle)), 
        //TextStyle {
        //    font: FontId::Title,
        //    size_px: 16,
        //},
        [1.0, 0.97, 0.90, 1.0],
        scale,
    )?;
    

    for line in dialog.message.lines() {
         self.draw_text_cached(
            frame,
            fonts,
            line,
            layout.message_rect.loc.x,
            y,
            style_for(FontRole::Body, 16, theme.id.builtin_id().unwrap_or(BuiltInThemeId::Eagle)),
            //TextStyle {
            //    font: FontId::Debug,
            //    size_px: 16,
            //},
            [0.78, 0.86, 1.0, 0.9],
            scale,
        )?;
        //RenderState::draw_text(
       //     frame,
       //     atlas,
       //     line,
       //     layout.bounds.loc.x + 32,
      //      y,
      //      [0.85, 0.95, 1.0, 1.0],
      //      program,
      //  )?;

        y += 24;
    }

    for (idx, rect) in &layout.button_rects {
        let button = &dialog.buttons[*idx];

        //let btn = rect.to_physical_precise_round(scale);
        //RenderState::draw_solid_rect(frame, fb, &[btn], [0.12, 0.18, 0.24, 1.0])?;
        self.draw_rounded_rect(
        frame,
        *rect,
        scale,
        4.0,
        [0.12, 0.16, 0.20, 0.95],
    )?;

        self.draw_text_cached(
                frame,
                fonts,
                &button.label,
                rect.loc.x + 18,
                rect.loc.y + 26,
                style_for(FontRole::Label, 16, theme.id.builtin_id().unwrap_or(BuiltInThemeId::Eagle)),
               // TextStyle {
               //     font: FontId::Debug,
                //    size_px: 16,
               // },
                [1.0, 1.0, 1.0, 1.0],
                scale,
            )?;
       // RenderState::draw_text(
      //      frame,
     //       atlas,
    //        &button.label,
    //        rect.loc.x + 18,
    //        rect.loc.y + 26,
     //       [1.0, 1.0, 1.0, 1.0],
     //       program,
     //   )?;
    }

    Ok(())
}
    
fn draw_text(
    frame: &mut GlesFrame<'_, '_>,
    atlas: &flowstate_ui::atlas::IconAtlas,
    text: &str,
    x: i32,
    y: i32,
    color: [f32; 4],
    scale: Scale<f64>,
    program: &GlesTexProgram,
) -> Result<(), GlesError> {
    if text.is_empty() {
        return Ok(());
    }

    let glyph_h = 16; // simple fixed size for now
    let glyph_w = ((glyph_h as f32) * 0.82) as i32;
    let gap = 1;

    let mut cursor_x = x;

    let style = UiVisualStyle {
        tint: color,
        glow: 0.0,
        alpha: color[3],
        scale: 1.0,
    };

    for ch in text.chars() {
        if ch == ' ' {
            cursor_x += glyph_w * 2;
            continue;
        }

        if let Some(icon) = glyph_for_char(ch) {
            let rect_logical = Rectangle::from_loc_and_size(
                (cursor_x, y),
                (glyph_w, glyph_h),
            );

            RenderState::render_icon_with_tint(
                frame,
                atlas,
                icon,
                rect_logical,
                scale,
                style,
                program,
            );

            cursor_x += glyph_w + gap;
        }
    }

    Ok(())
}

fn get_char(&self, ch: char) -> Option<IconId> 
{
   match ch {
        'A' => Some(IconId::A),
        'B' => Some(IconId::B),
        'C' => Some(IconId::B),
        'D' => Some(IconId::B),
        'E' => Some(IconId::B),
        'F' => Some(IconId::B),
        'G' => Some(IconId::B),
        'H' => Some(IconId::B),
        'I' => Some(IconId::B),
        'J' => Some(IconId::B),
        'K' => Some(IconId::B),
        'L' => Some(IconId::B),
        'M' => Some(IconId::B),
        'N' => Some(IconId::B),
        'O' => Some(IconId::B),
        'P' => Some(IconId::B),
        'Q' => Some(IconId::B),
        'R' => Some(IconId::B),
        'S' => Some(IconId::B),
        'T' => Some(IconId::B),
        'V' => Some(IconId::B),
        'W' => Some(IconId::B),
        'X' => Some(IconId::B),
        'Y' => Some(IconId::B),
        'Z' => Some(IconId::B),
        '0' => Some(IconId::Num0),
        '1' => Some(IconId::Num1),
        '2' => Some(IconId::Num2),
        '3' => Some(IconId::Num3),
        '4' => Some(IconId::Num4),
        '5' => Some(IconId::Num5),
        '6' => Some(IconId::Num6),
        '7' => Some(IconId::Num7),
        '8' => Some(IconId::Num8),
        '9' => Some(IconId::Num9),
        ' ' => None, // just spacing
        _ => None,
    }
}
    fn draw_workarea_glass(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
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
        let dst_rect_physical = rect_physical;
        let size = Size::from((rect_physical.size.w, rect_physical.size.h));

        let t = ctx.now.duration_since(self.start_time).as_secs_f32();

        let uniforms = [
            Uniform::new("u_size", [rect_physical.size.w as f32, rect_physical.size.h as f32]),
            Uniform::new("u_opacity", style.opacity),
            Uniform::new("u_edge_width", style.edge_width),
            Uniform::new("u_edge_brightness", style.edge_brightness),
            Uniform::new("u_highlight_strength", style.highlight_strength),
            Uniform::new("u_tint", style.tint),
            Uniform::new("u_edge_color", style.edge_color),
            Uniform::new("u_time", t),
        ];


        frame.render_pixel_shader_to(
            program,
            src_rect,        // Rectangle<f64, Buffer>
            dst_rect_physical,        // Rectangle<i32, Physical>
            size,            // Size<i32, Buffer>
            Some(damage),    // Option<&[Rectangle<i32, Physical>]>
            1.0,             // alpha
            &uniforms,
        )
    }

     fn draw_beveled_panel(
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
        let dst_rect_physical = rect_physical;
        let size = Size::from((rect_physical.size.w, rect_physical.size.h));

        let uniforms = [
            Uniform::new("u_bevel", style.bevel),
            Uniform::new("u_softness", style.softness),
            Uniform::new("u_glow_width", style.glow_width),
            Uniform::new("u_glow_alpha", style.glow_alpha),
            Uniform::new("u_inner_shadow", style.inner_shadow),
            Uniform::new("u_face_color", style.face_color),
            Uniform::new("u_light_color", style.light_color),
            Uniform::new("u_shadow_color", style.shadow_color),
            Uniform::new("u_glow_color", style.glow_color),
        ];


        frame.render_pixel_shader_to(
            program,
            src_rect,        // Rectangle<f64, Buffer>
            dst_rect_physical,        // Rectangle<i32, Physical>
            size,            // Size<i32, Buffer>
            Some(damage),    // Option<&[Rectangle<i32, Physical>]>
            1.0,             // alpha
            &uniforms,
        )
    }
 
 
     fn draw_light_channel(

        frame: &mut GlesFrame<'_, '_>,
        program: &GlesPixelProgram,
        rect_logical: Rectangle<i32, Logical>,
        scale: Scale<f64>,
        damage: &[Rectangle<i32, Physical>],
        style: &LightChannelStyle) -> Result<(), GlesError> {
    let rect_physical = to_physical_rect(rect_logical, scale);
    let dst_rect_physical = rect_physical;

    let src_rect = Rectangle::from_loc_and_size(
        (0.0, 0.0),
        (rect_physical.size.w as f64, rect_physical.size.h as f64),
    );
    let size = Size::from((rect_physical.size.w, rect_physical.size.h));

           let uniforms = [
                Uniform::new("u_slot_inset", style.slot_inset),
                Uniform::new("u_core_inset", style.core_inset),
                Uniform::new("u_glow_radius", style.glow_radius),
                Uniform::new("u_softness", style.softness),
                Uniform::new("u_housing_color", style.housing_color),
                Uniform::new("u_glow_color", style.glow_color),
                Uniform::new("u_core_color", style.core_color),
            ];

        frame.render_pixel_shader_to(
            program,
            src_rect,        // Rectangle<f64, Buffer>
            dst_rect_physical,        // Rectangle<i32, Physical>
            size,            // Size<i32, Buffer>
            Some(damage),    // Option<&[Rectangle<i32, Physical>]>
            1.0,             // alpha
            &uniforms,
        )
    }

/// Solid fill over `regions` in the same coordinate space as [`Frame::clear`]: `dest` is usually
/// `Rectangle::from_loc_and_size((0, 0), output_size)` and `regions` are absolute physical rects.
fn draw_solid_rect(
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

fn expand_rect(
    rect: Rectangle<i32, Physical>,
    px: i32,
) -> Rectangle<i32, Physical> {
    Rectangle::from_loc_and_size(
        (rect.loc.x - px, rect.loc.y - px),
        (rect.size.w + px * 2, rect.size.h + px * 2),
    )
}

fn inset_rect_xy(
    rect: Rectangle<i32, Physical>,
    px: i32,
    py: i32,
) -> Rectangle<i32, Physical> {
    Rectangle::from_loc_and_size(
        (rect.loc.x + px, rect.loc.y + py),
        (
            (rect.size.w - px * 2).max(1),
            (rect.size.h - py * 2).max(1),
        ),
    )
}




    pub fn draw_debug_rect(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        output: &OutputState,
    ) {
        // Debug marker where window should start
        let marker: Rectangle<i32, Physical> =
            Rectangle::new((64, 36).into(), (20, 20).into());

        frame.clear(Color32F::new(1.0, 0.0, 0.0, 1.0), &[marker]).unwrap();
    }

    pub fn draw_background(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        output: &OutputState,
        bg: BackgroundTheme,
    ) {
 
        // 1) Clear whole output
        let full: Rectangle<i32, Physical> = Rectangle::new((0, 0).into(), ctx.output_size.into());
        let full_damage = [full];

        let c = bg.color;
        
        frame
            .clear(Color32F::new(c[0],c[1],c[2],c[3]), &full_damage)
            //.clear(Color32F::new(0.07, 0.08, 0.10, 1.0), &full_damage)
            .unwrap();
    }

    pub fn draw_wallpaper_in_rect(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        target_logical: Rectangle<i32, Logical>,
        scale: Scale<f64>,
        theme: WallpaperTheme,
    ) {
        use smithay::backend::renderer::gles::{GlesTexProgram, Uniform};
        use smithay::utils::{Buffer, Physical, Rectangle, Transform};

        use crate::core::wallpaper::{compute_wallpaper_blit, RectI, SizeI, WallpaperMode};

        let Some(tex) = self.wallpaper_texture.as_ref() else {
            return;
        };
        let target_physical = to_physical_rect(target_logical, scale);

        let out = RectI {
            x: target_physical.loc.x,
            y: target_physical.loc.y,
            w: target_physical.size.w,
            h: target_physical.size.h,
        };

        let sz = tex.size();
        let src = SizeI { w: sz.w, h: sz.h };

        let mode = WallpaperMode::Fill;

        let Some(blit) = compute_wallpaper_blit(src, out, mode) else {
            return;
        };

        let dst_world: Rectangle<i32, Physical> =
            Rectangle::new((blit.dst.x, blit.dst.y).into(), (blit.dst.w, blit.dst.h).into());

        //let dst =
        //    RenderState::rect_apply_flipped180(dst_world, (ctx.output_size.0, ctx.output_size.1));
        let dst = dst_world;

        let dsts = [dst];
        let damage = [target_physical];

        let tw = src.w as f64;
        let th = src.h as f64;

        let src_rect: Rectangle<f64, Buffer> = Rectangle::new(
            (blit.uv.u0 as f64 * tw, blit.uv.v0 as f64 * th).into(),
            (
                (blit.uv.u1 as f64 - blit.uv.u0 as f64) * tw,
                (blit.uv.v1 as f64 - blit.uv.v0 as f64) * th,
            )
                .into(),
        );
        
let uniforms = [
    Uniform::new("u_tint", theme.tint_color),
];

let wallpaper_tint = self.chrome_shaders.wallpaper_tint.as_ref();

        frame
            .render_texture_from_to(
                tex,
                src_rect,
                dst,
                &dsts,
                &damage,
                Transform::Normal,
                1.0,
                wallpaper_tint,
                &uniforms,
            )
            .unwrap();   
    }



pub fn build_client_elements(
    &mut self,
    space: &Space<Window>,
    windows: &[ManagedWindow],
    active_workspace: WorkspaceId,
    origin: Point<i32, smithay::utils::Logical>,
    logical_size: Size<i32, Logical>,
    renderer: &mut GlesRenderer,
    ctx: &FrameCtx,
    layers_on: Option<&smithay::output::Output>,
) -> Vec<FlowRenderElement> {
    use smithay::backend::renderer::element::AsRenderElements;
    use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};

    let mut out = Vec::new();

let output_rect = Rectangle::<i32, Logical>::from_loc_and_size(
    origin,
    logical_size,
);


    for window in space.elements() {
        let Some(managed) = windows
            .iter()
            .find(|mw| mw.mapped && &mw.window == window)
        else {
            continue;
        };

        if managed.workspace != active_workspace {
            continue;
        }

    
        let Some(global_loc) = space.element_location(window) else {
            continue;
        };

        // Window bbox is local-to-window-space; move it into global desktop space.
        let bbox = window.bbox();
        let global_bbox = Rectangle::from_loc_and_size(
            (bbox.loc.x + global_loc.x, bbox.loc.y + global_loc.y),
            bbox.size,
        );

        if !global_bbox.overlaps(output_rect) {
            continue;
        }

        // Convert desktop-global position into this output's local coordinate space.
        let local_loc = global_loc - origin;

        let render_pos = local_loc.to_physical_precise_round(ctx.output_scale);

        out.extend(
            window.render_elements::<FlowRenderElement>(
                renderer,
                render_pos,
                ctx.output_scale,
                1.0,
            ),
        );
    }

    if let Some(out_handle) = layers_on {
        crate::core::portal::push_layer_elements_for_output(
            renderer,
            out_handle,
            origin,
            logical_size,
            ctx.output_scale,
            &mut out,
        );
    }

    out
}


/*
    pub fn build_client_elements(
        &mut self,
        space: &Space<Window>,
        renderer: &mut GlesRenderer,
        ctx: &FrameCtx,
    ) -> Vec<FlowRenderElement> {
        let mut out = Vec::new();
        let scale = ctx.output_scale;

        // Variant A: elements() yields (&Window) only

        //let bs = ctx.buffer_scale;

let output = desktop.outputs.get(&ctx.active_output).unwrap();
let origin = output.logical_origin;
let output_rect = Rectangle::from_loc_and_size(origin, output.logical_size);

for window in space.elements() {
    if let Some(global_loc) = space.element_location(window) {
        let bbox = window.bbox().loc + global_loc; // whatever form your bbox API needs
        if !bbox.intersects(output_rect) {
            continue;
        }

        let local_loc = global_loc - origin.to_f64_or_i32();
        let location_p = local_loc.to_physical_precise_round(scale);
        let elems = window.render_elements(renderer, location_p, scale, 1.0);
        out.extend(elems);
    }
}
/*        for window in space.elements() {
            if let Some(location) = space.element_location(window) {
                let location_p = location.to_physical_precise_round(scale);
                let elems = window.render_elements(renderer, location_p, scale, 1.0);

                flog(&format!("window log loc={:?}, elements={}", location, elems.len()));
                flog(&format!("window phys loc={:?}, elements={}", location_p, elems.len()));

                out.extend(elems);
            }
        }*/

        out
    }
*/

  
  pub fn draw_active_lightbar(
    &self,
    frame: &mut GlesFrame<'_, '_>,
    ctx: &FrameCtx,
    layout: &ChromeLayoutLogical,
) {
    if ctx.active_output != ctx.rendering_output {
        return;
    }

    let Some(program) = self.chrome_shaders.amber_lightbar.as_ref() else {
        return;
    };

    let bar_rect_logical = Rectangle::from_loc_and_size(
        layout.topbar_outer.loc,
        (layout.topbar_outer.size.w, 10),
    );

    let full = Rectangle::from_loc_and_size((0, 0), ctx.output_size);
    let damage = std::slice::from_ref(&full);

    let _ = Self::draw_amber_lightbar(
        frame,
        program,
        bar_rect_logical,
        ctx.output_scale,
        damage,
    );
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
            //Uniform::new("u_color", [1.0, 0.58, 0.12, 1.0]),
            Uniform::new("u_color", [1.0, 0.75, 0.05, 1.0]),
            Uniform::new("alpha", 1.0f32),
        ],
    )
}
    
    pub fn draw_clients(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        scene: &SceneState,
        output: &OutputState,
        elements: &[FlowRenderElement],
    ) {
        // 1) Build elements from Space<Window>
        //let elements = build_client_elements(&scene.space, renderer, ctx);

        // 2) Choose damage
        let full = smithay::utils::Rectangle::from_loc_and_size((0, 0), ctx.output_size);
        let damage = std::slice::from_ref(&full);

        // 3) Draw
        draw_render_elements(
            frame,
            ctx.output_scale.x,
            &elements,
            damage,
        )
            .unwrap();
    }

    /// Top bar, sidebar, work-area bezel, and joints — everything that must sit *under*
    /// the work wallpaper and client surfaces.
    fn draw_chrome_below_work_wallpaper(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        layout: &ChromeLayoutLogical,
        _output: &OutputState,
        _metrics: &ChromeMetrics,
        _ui: &mut UiState<GlesTexture>,
        sidebar_hover_slot: Option<usize>,
        theme: flowstate_themes::ChromeTheme,
    ) {
        let legacy_theme = chrome_theme_from_flow_theme(&theme);

        let beveled = self
            .chrome_shaders
            .beveled_panel
            .as_ref()
            .expect("beveled_panel shader not compiled");

        let light = self
            .chrome_shaders
            .light_channel
            .as_ref()
            .expect("light_channel shader not compiled");

        let button = self
            .chrome_shaders
            .recessed_button
            .as_ref()
            .expect("button shader not compiled");

        let top_bar = self
            .chrome_shaders
            .top_bar
            .as_ref()
            .expect("top bar shader not compiled");

        let fullscreen_rect: Rectangle<i32, Physical> = Rectangle::from_loc_and_size(
            Point::<i32, Physical>::from((0, 0)),
            Size::<i32, Physical>::from(ctx.output_size),
        );
        let damage = &[fullscreen_rect];

        //
        // 1. STRUCTURAL SHELL
        //

        let _ = Self::draw_top_bar(
            frame,
            top_bar,
            layout.topbar_outer,
            ctx.output_scale,
            damage,
            &legacy_theme.top_bar,
        );

        Self::draw_beveled_panel(
            frame,
            beveled,
            layout.topbar_outer,
            ctx.output_scale,
            damage,
            &legacy_theme.frame_outer,
        );

        Self::draw_beveled_panel(
            frame,
            beveled,
            layout.topbar_inner,
            ctx.output_scale,
            damage,
            &legacy_theme.frame_inner,
        );

        Self::draw_beveled_panel(
            frame,
            beveled,
            layout.sidebar_outer,
            ctx.output_scale,
            damage,
            &legacy_theme.sidebar,
        );

        Self::draw_beveled_panel(
            frame,
            beveled,
            layout.sidebar_inner,
            ctx.output_scale,
            damage,
            &legacy_theme.panel_inner,
        );

        Self::draw_beveled_panel(
            frame,
            beveled,
            layout.work_outer,
            ctx.output_scale,
            damage,
            &legacy_theme.frame_outer,
        );

        Self::draw_beveled_panel(
            frame,
            beveled,
            layout.work_inner_frame,
            ctx.output_scale,
            damage,
            &legacy_theme.frame_inner,
        );

        Self::draw_beveled_panel(
            frame,
            beveled,
            layout.work_recess,
            ctx.output_scale,
            damage,
            &legacy_theme.panel_inner,
        );

        //
        // 2. TOP BAR DETAILS
        //

        Self::draw_beveled_panel(
            frame,
            beveled,
            layout.title_rect,
            ctx.output_scale,
            damage,
            &legacy_theme.panel_inner,
        );

        Self::draw_beveled_panel(
            frame,
            beveled,
            layout.topbar_trim,
            ctx.output_scale,
            damage,
            &legacy_theme.trim,
        );

        if let Some(rect) = layout.topbar_light {
            Self::draw_light_channel(
                frame,
                light,
                rect,
                ctx.output_scale,
                damage,
                &legacy_theme.light,
            );
        }

        for rect in &layout.status_wells {
            Self::draw_recessed_button(
                frame,
                button,
                *rect,
                ctx.output_scale,
                damage,
                &legacy_theme.button,
            );

            Self::draw_light_channel(
                frame,
                light,
                inset_rect(*rect, 3),
                ctx.output_scale,
                damage,
                &legacy_theme.light,
            );
        }

        Self::draw_recessed_button(
            frame,
            button,
            layout.clock_well,
            ctx.output_scale,
            damage,
            &legacy_theme.button,
        );

        Self::draw_light_channel(
            frame,
            light,
            inset_rect(layout.clock_well, 3),
            ctx.output_scale,
            damage,
            &legacy_theme.light,
        );

        //
        // 3. SIDEBAR MODULESFtopbar
        //

        for (i, ((outer, inner), well)) in layout
            .slot_outer_rects
            .iter()
            .zip(layout.slot_inner_rects.iter())
            .zip(layout.slot_icon_wells.iter())
            .enumerate()
        {
            let hovered = sidebar_hover_slot == Some(i);

            let _ = Self::draw_beveled_panel(frame, beveled, *outer, ctx.output_scale, damage, &legacy_theme.module);
            let _ = Self::draw_beveled_panel(frame, beveled, *inner, ctx.output_scale, damage, &legacy_theme.module_inner);
            Self::draw_recessed_button(frame, button, *well, ctx.output_scale, damage, &legacy_theme.button);

            //if hovered {
            let hover = if hovered { 1.0 } else { 0.0 };

            let glow_rect = inset_rect(*well, 3);

            let mut light_style = legacy_theme.light;
            
            // baseline glow
            light_style.glow_color[3] = 0.08 + hover * 0.55;
            light_style.core_color[3] = 0.18 + hover * 0.55;
            
            // hover boost
            light_style.glow_radius = 8.0 + hover * 6.0;
            light_style.core_inset = 3.0 - hover * 0.75;

            let _ = Self::draw_light_channel(frame, light, glow_rect, ctx.output_scale, damage, &light_style);
            
                //let glow_rect = inset_rect(*well, 3);
                //let _ = Self::draw_light_channel(frame, light, glow_rect, damage, &legacy_theme.light);
           // }
        }

        if let Some(rect) = layout.sidebar_light_rect {
            Self::draw_light_channel(
                frame,
                light,
                rect,
                ctx.output_scale,
                damage,
                &legacy_theme.light,
            );
        }

        for rect in &layout.sidebar_caps {
            Self::draw_beveled_panel(frame, beveled, *rect, ctx.output_scale, damage, &legacy_theme.corner_cap);
        }

        //
        // 4. DECORATIVE CAPS / JOINTS
        //

        for rect in &layout.corner_caps {
            Self::draw_beveled_panel(frame, beveled, *rect, ctx.output_scale, damage, &legacy_theme.corner_cap);
        }

        for rect in &layout.corner_joint_caps {
            Self::draw_beveled_panel(frame, beveled, *rect, ctx.output_scale, damage, &legacy_theme.corner_cap);
        }
    }

    /// Work trim, glass tint, and chrome icons — drawn above client surfaces.
    fn draw_chrome_trim_glass_icons(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        layout: &ChromeLayoutLogical,
        output: &OutputState,
        metrics: &ChromeMetrics,
        //ui: &mut UiState<GlesTexture>,
        ui_state: &mut UiState<GlesTexture>,
        ui_tree: &UiTree,
        current_workspace: WorkspaceId, 
        fonts: &FontSystem,
        theme: &FlowTheme,
    ) {
       let legacy_theme = chrome_theme_from_flow_theme(&theme.chrome);

        let beveled = self
            .chrome_shaders
            .beveled_panel
            .as_ref()
            .expect("beveled_panel shader not compiled");

        let glass = self
            .chrome_shaders
            .glass
            .as_ref()
            .expect("glass shader not compiled");

       
            
            
            
        let fullscreen_rect: Rectangle<i32, Physical> = Rectangle::from_loc_and_size(
            Point::<i32, Physical>::from((0, 0)),
            Size::<i32, Physical>::from(ctx.output_size),
        );
        let damage = &[fullscreen_rect];

        if let Some(rect) = layout.work_trim {
            Self::draw_beveled_panel(
                frame,
                beveled,
                rect,
                ctx.output_scale,
                damage,
                &legacy_theme.trim,
            );
        }

        self.draw_workarea_glass(
            frame,
            ctx,
            glass,
            layout.glass_rect,
            ctx.output_scale,
            damage,
            &legacy_theme.glass,
        );
        
  
        
        self.draw_active_lightbar(frame, ctx, layout);

        if let Some(atlas) = ui_state.chrome.atlas.as_ref() {
            let icon_px = metrics.icon_base_px as i32;
     
        let text = format!(
            "FLOWSTATE · OUT {} · WS {}",
            ctx.rendering_output.0,
            current_workspace.0
        );

        let _label_rect_logical = title_label_rect(layout.title_rect);
        
        
        let is_active = ctx.rendering_output == ctx.active_output; // or output.output_id
        
        //let _ = RenderState::draw_title_text(frame, atlas, _label_rect_logical, &text, is_active, ctx.output_scale, tinted_icon);
        
    let output_number = ctx.rendering_output.0;
    let workspace_number = 1;
    let active_theme = theme;
        

let _ = self.draw_topbar_title(
    frame,
    fonts,
    layout,
    "FLOWSTATE",
    &active_theme,
    ctx.output_scale,
);

let _ = self.draw_topbar_meta(
    frame,
    fonts,
    layout,
    "FLOWSTATE",
    output_number,
    workspace_number,
    &active_theme,
    ctx.output_scale,
);

 
                 let tinted_icon = self
            .chrome_shaders
            .tinted_icon
            .clone()
            .expect("glass shader not compiled");
        
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

// center-based scaling

    let icon_state = if el.active {
        IconState::Active
    } else if el.hovered {
        IconState::Hover
    } else if !el.enabled {
        IconState::Inactive
    } else {
        IconState::Inactive
    };

let state = el.visual_state();
let mut style = visual_style(state);

let is_active_output = ctx.rendering_output == ctx.active_output;
let output_factor = if is_active_output { 1.0 } else { 0.75 };

// Dim only the icon color/alpha on inactive outputs.
style.tint[0] *= output_factor;
style.tint[1] *= output_factor;
style.tint[2] *= output_factor;
style.tint[3] *= output_factor;
//style.alpha *= output_factor;
style.glow *= output_factor;

    match el.kind {
        UiElementKind::SidebarButton | UiElementKind::WorkspaceSlot => {
            if let Some(icon_id) = el.icon {
                let mut icon_rect_logical = icon_rect_in_module(base_rect_logical, icon_px);
                if scale != 1.0 {
    let cx_logical = icon_rect_logical.loc.x + icon_rect_logical.size.w / 2;
    let cy_logical = icon_rect_logical.loc.y + icon_rect_logical.size.h / 2;

    let new_w_logical = ((icon_rect_logical.size.w as f32) * scale).round() as i32;
    let new_h_logical = ((icon_rect_logical.size.h as f32) * scale).round() as i32;

    icon_rect_logical = Rectangle::from_loc_and_size(
        (cx_logical - new_w_logical / 2, cy_logical - new_h_logical / 2),
        (new_w_logical, new_h_logical),
    );
    }
                Self::draw_icon_in_rect(frame, atlas, icon_id, icon_state, icon_rect_logical, ctx.output_scale, style, &tinted_icon);
            }
        }

        UiElementKind::TopbarIndicator | UiElementKind::TopbarButton => {
            if let Some(icon_id) = el.icon {
                let icon_rect_logical = well_icon_rect(base_rect_logical);
                Self::draw_icon_in_rect(frame, atlas, icon_id, icon_state, icon_rect_logical, ctx.output_scale, style, &tinted_icon);
            }
        }

        UiElementKind::Clock => {
            use chrono::Local;

            let now = Local::now();
            let time_str = now.format("%-I:%M %p").to_string();

            let clock_rect_logical = inset_rect(base_rect_logical, 4);
            //self.draw_clock_text(frame, atlas, &time_str, clock_rect,tinted_icon);
            
            let clock_style = style_for(
                FontRole::Clock,
                24,
                active_theme.id.builtin_id().unwrap_or(BuiltInThemeId::Eagle),
            );
            
            self.draw_clock_font_text(frame,fonts,&time_str,clock_rect_logical,ctx.output_scale,clock_style, active_theme.text.clock,);
        }

        _ => {}
    }
}         

/*
            let sidebar_icons = [
                IconId::Launcher,
                IconId::Settings,
                IconId::Overflow,
                IconId::Slot(1),
                IconId::Slot(2),
                IconId::Slot(3),
                IconId::Slot(4),
                IconId::Slot(5),
                IconId::Slot(6),
                IconId::Slot(7),
                IconId::Slot(8),
                IconId::Slot(9),
                IconId::AssignToSlot,
                IconId::Browser,
                IconId::Terminal,
                IconId::Files,
            ];

            for (well, icon_id) in layout.slot_icon_wells.iter().zip(sidebar_icons.iter()) {
                let icon_rect = icon_rect_in_module(*well, icon_px);
                Self::draw_icon_in_rect(frame, atlas, *icon_id, IconState::Inactive, icon_rect);
            }

            let label_rect = title_label_rect(layout.title_rect);
            self.draw_title_text(frame, atlas, label_rect, "FLOWSTATE");

            let status_icons = [IconId::Wifi, IconId::Bluetooth, IconId::Speaker, IconId::Power];
            for (well, icon_id) in layout.status_wells.iter().zip(status_icons.iter()) {
                let icon_rect = well_icon_rect(*well);
                Self::draw_icon_in_rect(frame, atlas, *icon_id, IconState::Active, icon_rect);
            }

            use chrono::Local;

            let now = Local::now();
            let time_str = now.format("%-I:%M %p").to_string();

            let clock_rect = inset_rect(layout.clock_well, 4);
            self.draw_clock_text(frame, atlas, &time_str, clock_rect); */
            
  
        }
    }



}
    


fn title_label_rect(title_rect_logical: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    let pad_x = 10;
    let pad_y = 4;

    //let avail_h = (title_rect.size.h - pad_y * 2).max(1);

    // Example aspect ratio: 4:1. Adjust to your actual logo asset.
    //let w =  (avail_h * 10).max(1);
    
    

   // Rectangle::from_loc_and_size(
   //     (title_rect.loc.x + pad_x, title_rect.loc.y + pad_y),
   //     (w.min(title_rect.size.w - pad_x * 2), avail_h),
   // )
    
     Rectangle::from_loc_and_size(
        (title_rect_logical.loc.x + pad_x, title_rect_logical.loc.y + pad_y),
        (
            title_rect_logical.size.w - pad_x * 2,   // 👈 FULL WIDTH
            (title_rect_logical.size.h - pad_y * 2).max(1),
        ),
    )
}


pub fn well_icon_rect(well_logical: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    let well = well_logical;
    inset_rect(well, (well.size.h / 5).max(4))
}

pub fn clock_text_rect(well_logical: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    Rectangle::from_loc_and_size(
        (well_logical.loc.x + 8, well_logical.loc.y + 5),
        ((well_logical.size.w - 16).max(1), (well_logical.size.h - 10).max(1)),
    )
}



#[inline]
pub fn inset_rect(
    r: Rectangle<i32, Logical>,
    px: i32,
) -> Rectangle<i32, Logical> {
    Rectangle::from_loc_and_size(
        (r.loc.x + px, r.loc.y + px),
        (
            (r.size.w - px * 2).max(1),
            (r.size.h - px * 2).max(1),
        ),
    )
}
#[inline]
fn center_rect_in(
    outer: Rectangle<i32, Logical>,
    w: i32,
    h: i32,
) -> Rectangle<i32, Logical> {
    let x = outer.loc.x + ((outer.size.w - w).max(0) / 2);
    let y = outer.loc.y + ((outer.size.h - h).max(0) / 2);
    Rectangle::from_loc_and_size((x, y), (w, h))
}

#[inline]
fn icon_rect_in_module(
    module: Rectangle<i32, Logical>,
    icon_px: i32,
) -> Rectangle<i32, Logical> {
    center_rect_in(module, icon_px, icon_px)
}




#[derive(Debug, Clone, Copy)]
pub struct BevelStyle {
    pub bevel: f32,
    pub softness: f32,
    pub glow_width: f32,
    pub glow_alpha: f32,
    pub inner_shadow: f32,
    pub face_color: [f32; 4],
    pub light_color: [f32; 4],
    pub shadow_color: [f32; 4],
    pub glow_color: [f32; 4],
}

#[derive(Debug, Clone, Copy)]
pub struct LightChannelStyle {
    pub slot_inset: f32,
    pub core_inset: f32,
    pub glow_radius: f32,
    pub softness: f32,
    pub housing_color: [f32; 4],
    pub glow_color: [f32; 4],
    pub core_color: [f32; 4],
}

#[derive(Debug, Clone, Copy)]
pub struct GlassStyle {
    pub opacity: f32,
    pub edge_width: f32,
    pub edge_brightness: f32,
    pub highlight_strength: f32,

    pub tint: [f32; 4],
    pub edge_color: [f32; 4],
}

#[derive(Debug, Clone, Copy)]
pub struct LineStyle {
    pub color: [f32; 4],
    pub thickness: i32,
    pub alpha: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct GlowStyle {
    pub color: [f32; 4],
    pub alpha: f32,
    pub inset: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct ButtonStyle {
    pub bevel: f32,
    pub softness: f32,
    pub inner_shadow: f32,

    pub glow_strength: f32,
    pub glow_radius: f32,

    pub face_color: [f32; 4],
    pub shadow_color: [f32; 4],
    pub glow_color: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct TopBarStyle {
    pub radius: f32,
    pub softness: f32,
    pub bevel: f32,
    pub highlight_strength: f32,
    pub shadow_strength: f32,
    pub trim_height: f32,
    pub trim_brightness: f32,
    pub face_color: [f32; 4],
    pub edge_color: [f32; 4],
    pub trim_color: [f32; 4],
}

#[derive(Debug, Clone, Copy)]
pub struct ChromeTheme {  
    // frame
    pub frame_outer: BevelStyle,  // frame outer    
    pub frame_inner: BevelStyle,  // frame inner
    
    // Surface layers
    pub panel_base: BevelStyle,  // panael base
    pub panel_inner: BevelStyle, // panel recess
    
    
    
    // functional areas
    pub sidebar: BevelStyle,
    pub module: BevelStyle,
    pub module_inner: BevelStyle,
    pub icon_well: BevelStyle,    
    pub icon_well_active: BevelStyle,
    
    // decorative / trim
    pub trim: BevelStyle,
    pub corner_cap: BevelStyle,
    
    // Effects
    pub light: LightChannelStyle,    
    pub glass: GlassStyle,
    
    pub line_highlight: LineStyle,
    pub line_groove: LineStyle,

    pub glow_active: GlowStyle,
    
    pub button: ButtonStyle,
    
    pub top_bar: TopBarStyle,
}

pub fn default_chrome_theme() -> ChromeTheme {
    ChromeTheme {
        frame_outer: BevelStyle {
            bevel: 4.0,
            softness: 1.15,
            glow_width: 0.0,
            glow_alpha: 0.0,
            face_color: [0.030, 0.050, 0.090, 1.0],
            inner_shadow: 3.5,
            light_color:  [0.165, 0.215, 0.305, 1.0],
            shadow_color: [0.006, 0.010, 0.018, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        frame_inner: BevelStyle {
            bevel: 3.0,
            softness: 1.2,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 2.2,
            face_color: [0.050, 0.075, 0.120, 1.0],
            light_color:  [0.185, 0.235, 0.325, 1.0],
            shadow_color: [0.010, 0.016, 0.026, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        panel_base: BevelStyle {
            bevel: 2.5,
            softness: 1.25,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 1.8,
            face_color: [0.060, 0.085, 0.135, 1.0],   // was too bright
            light_color:  [0.205, 0.255, 0.345, 1.0],
            shadow_color: [0.014, 0.020, 0.032, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        panel_inner: BevelStyle {
            bevel: 2.5,
            softness: 1.35,
            glow_width: 0.0,
            glow_alpha: 0.0,
            face_color:   [0.025, 0.045, 0.080, 1.0],
            inner_shadow: 4.8,   // increase
            light_color:  [0.105, 0.145, 0.220, 1.0],
            shadow_color: [0.004, 0.008, 0.015, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        trim: BevelStyle {
            bevel: 1.4,
            softness: 0.95,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 1.2,
            face_color:  [0.075, 0.105, 0.160, 1.0],
            light_color:  [0.235, 0.290, 0.380, 1.0],
            shadow_color: [0.020, 0.028, 0.040, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        sidebar: BevelStyle {
            bevel: 2.8,
            softness: 1.2,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 2.4,
            face_color:   [0.050, 0.073, 0.118, 1.0],
            light_color:  [0.155, 0.205, 0.290, 1.0],
            shadow_color: [0.008, 0.013, 0.022, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },
        
        module: BevelStyle {
            bevel: 2.4,
            softness: 1.1,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 1.8,
            face_color:   [0.070, 0.098, 0.150, 1.0],
            light_color:  [0.200, 0.250, 0.335, 1.0],
            shadow_color: [0.012, 0.018, 0.028, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        module_inner: BevelStyle {
            bevel: 2.0,
            softness: 1.15,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 3.0,
            face_color:   [0.040, 0.060, 0.102, 1.0],
            light_color:  [0.105, 0.145, 0.215, 1.0],
            shadow_color: [0.004, 0.008, 0.014, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        icon_well: BevelStyle {
            bevel: 1.4,
            softness: 0.8,
            glow_width: 3.0,
            glow_alpha: 0.015,
            inner_shadow: 5.5,
            face_color: [0.015, 0.022, 0.040, 1.0],
            light_color: [0.08, 0.11, 0.17, 1.0],
            shadow_color: [0.001, 0.002, 0.005, 1.0],
            glow_color: [0.03, 0.06, 0.12, 1.0],
        },

        icon_well_active: BevelStyle {
            bevel: 1.5,
            softness: 1.0,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 1.5,
            face_color: [0.03, 0.06, 0.11, 1.0],
            light_color: [0.16, 0.24, 0.38, 1.0],
            shadow_color: [0.00, 0.02, 0.05, 1.0],
            glow_color: [0.0, 0.0, 0.0, 1.0],
        },

        corner_cap: BevelStyle {
            bevel: 2.0,
            softness: 1.05,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 1.8,
            face_color:   [0.055, 0.078, 0.120, 1.0],
            light_color:  [0.170, 0.220, 0.305, 1.0],
            shadow_color: [0.008, 0.013, 0.022, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        light: LightChannelStyle {
            slot_inset: 1.0,
            core_inset: 3.0,
            glow_radius: 8.0,
            softness: 2.0,
            housing_color: [0.03, 0.05, 0.08, 1.0],
            glow_color: [0.18, 0.30, 0.55, 1.0],
            core_color: [0.10, 0.18, 0.34, 1.0],
        },
        
        glass: GlassStyle {
            opacity: 0.08,              // down from 0.90+
            edge_width: 12.0,           // tighter
            edge_brightness: 0.75,      // WAS TOO HIGH
            highlight_strength: 0.10,   // cut this a lot
            tint: [0.035, 0.085, 0.200, 1.0],   // darker tint
            edge_color: [0.30, 0.55, 0.95, 0.14],
        },
        
        line_highlight: LineStyle {
            color: [0.55, 0.75, 1.00, 1.0],
            thickness: 1,
            alpha: 0.10,
        },

        line_groove: LineStyle {
            color: [0.0, 0.0, 0.0, 1.0],
            thickness: 1,
            alpha: 0.28,
        },

        glow_active: GlowStyle {
            color: [0.35, 0.65, 1.00, 1.0],
            alpha: 0.08,
            inset: 0,
        },
        
        button: ButtonStyle {
            bevel: 3.0,
            softness: 1.5,
            inner_shadow: 0.7,

            glow_strength: 0.12,
            glow_radius: 0.55,

            face_color: [0.08, 0.08, 0.09, 1.0],
            shadow_color: [0.0, 0.0, 0.0, 1.0],

            // teal
            glow_color: [0.2, 0.9, 0.8, 1.0],
        },
        
        top_bar: TopBarStyle {
            radius: 10.0,
            softness: 1.8,
            bevel: 8.0,
            highlight_strength: 0.05,
            shadow_strength: 0.10,
            trim_height: 0.035,
            trim_brightness: 0.15,
            face_color: [0.025, 0.045, 0.085, 0.96],
            edge_color: [0.01, 0.015, 0.03, 1.0],
            trim_color: [0.72, 0.82, 0.95, 1.0],
        },
                
    }
}


/*
pub fn default_chrome_theme() -> ChromeTheme {
    ChromeTheme {
        frame_outer: BevelStyle {
            chamfer: 1.0,
            bevel: 4.0,
            softness: 0.8,
            light_dir: [0.7, -0.7],
            face_color: [0.15, 0.18, 0.24, 1.0],
            light_color: [0.45, 0.52, 0.62, 1.0],
            shadow_color: [0.02, 0.03, 0.05, 1.0],
        },
        frame_inner: BevelStyle {
            chamfer: 1.0,
            bevel: 4.0,
            softness: 2.0,
            light_dir: [0.7, -0.7],
            face_color: [0.07, 0.10, 0.15, 1.0],
            light_color: [0.8, 0.9, 1.0, 0.25],
shadow_color: [0.0, 0.0, 0.0, 0.5],
        },
        panel_base: BevelStyle {
            chamfer: 5.0,
            bevel: 3.0,
            softness: 2.0,
            light_dir: [0.7, -0.7],
            face_color: [0.05, 0.08, 0.12, 1.0],
            light_color: [0.14, 0.17, 0.24, 1.0],
            shadow_color: [0.00, 0.00, 0.01, 1.0],
        },
        panel_inner: BevelStyle {
            chamfer: 4.0,
            bevel: -2.0,
            softness: 2.0,
            light_dir: [0.7, -0.7],
            face_color: [0.03, 0.05, 0.09, 1.0],
            light_color: [0.10, 0.13, 0.19, 1.0],
            shadow_color: [0.00, 0.00, 0.00, 1.0],
        },
        trim: BevelStyle {
            chamfer: 3.0,
            bevel: 2.0,
            softness: 1.0,
            light_dir: [0.7, -0.7],
            face_color: [0.09, 0.11, 0.16, 1.0],
            light_color: [0.25, 0.30, 0.40, 1.0],
            shadow_color: [0.01, 0.02, 0.03, 1.0],
        },
        sidebar: BevelStyle {
            chamfer: 8.0,
            bevel: 2.5,
            softness: 0.6,
            light_dir: [0.7, -0.7],
            face_color:  [0.035, 0.050, 0.080, 1.0],
            light_color:  [0.22, 0.30, 0.40, 1.0],
            shadow_color: [0.010, 0.015, 0.025, 1.0],
        },
        module: BevelStyle {
            chamfer: 6.0,
            bevel: 2.0,
            softness: 0.6,
            light_dir: [0.7, -0.7],
            face_color:   [0.070, 0.095, 0.14, 1.0],
            light_color:  [0.22, 0.30, 0.40, 1.0],
            shadow_color: [0.010, 0.015, 0.025, 1.0],
        },
        module_inner: BevelStyle {
            chamfer: 4.0,
            bevel: 1.5,
            softness: 0.8,
            light_dir: [0.7, -0.7],
            face_color:   [0.050, 0.070, 0.11, 1.0],
            light_color:  [0.16, 0.22, 0.30, 1.0],
            shadow_color: [0.006, 0.010, 0.018, 1.0],        
        },
        icon_well: BevelStyle {
            chamfer: 4.0,
            bevel: 2.5,
            softness: 0.6,
            light_dir: [-0.4, 0.8], // different direction = visual separation
            face_color:   [0.035, 0.050, 0.085, 1.0],
            light_color:  [0.10, 0.14, 0.20, 1.0],
            shadow_color: [0.003, 0.005, 0.010, 1.0],
        },   
        corner_cap: BevelStyle {
            chamfer: 2.0,
            bevel: 2.0,
            softness: 1.0,
            light_dir: [0.7, -0.7],
            face_color: [0.12, 0.14, 0.19, 1.0],
            light_color: [0.35, 0.40, 0.50, 1.0],
            shadow_color: [0.02, 0.02, 0.03, 1.0],
        },
        light: LightChannelStyle {
            slot_inset: 1.0,
            core_inset: 2.0,
            glow_radius: 6.0,
            softness: 2.0,
            housing_color: [0.02, 0.05, 0.10, 1.0],
            glow_color: [0.20, 0.55, 1.00, 1.0],
            core_color: [0.75, 0.90, 1.00, 1.0],
        },
        
    }
}
*/
