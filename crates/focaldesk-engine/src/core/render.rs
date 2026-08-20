#![allow(unused_imports)]

use crate::core::app::App;
use crate::core::layout::LayoutSnapshot;
use crate::core::output::OutputState; // if still needed (ideally not)
use crate::core::ui_state::UiState;
use focaldesk_ui::chrome::ChromeMetrics;
use focaldesk_ui::types::{ElementId, UiElementKind};
use image::GenericImageView;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::gles::ffi;
use smithay::backend::renderer::gles::GlesFrame;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::Frame;
use smithay::backend::renderer::{ImportMem, Texture};
use smithay::desktop::Window;
use smithay::desktop::{PopupManager, Space};
use smithay::output::Output;
use smithay::utils::Buffer;
use smithay::utils::Transform;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale, Size};
use std::collections::HashMap;
use std::time::{Duration, Instant};
//use focaldesk_ui::atlas::render_atlas_icon_with_alpha;

use smithay::backend::renderer::gles::GlesTexProgram;

use crate::core::scene::SceneState;
//use crate::core::output::OutputId;
use focaldesk_cursor::{CursorIcon as FlowCursorIcon, CursorManager};
use focaldesk_logging::{flog, flog_error, flog_info, session_id};
use focaldesk_notifications::NotificationSnapshot;
use focaldesk_types::OutputId;
use focaldesk_ui::atlas::{render_atlas_icon_with_alpha, IconId, IconState};
use smithay::backend::renderer::element::render_elements;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind};
use smithay::backend::renderer::utils::draw_render_elements;
use wayland_server::protocol::wl_surface::WlSurface;
//use focaldesk_ui::atlas::render_atlas_icon_with_alpha;
use crate::core::color::{srgb_to_linear, SurfaceColorRenderState};
use crate::core::desktop::DesktopState;
use crate::core::desktop::{
    ClockPulseFrame, SidebarPulseFrame, TopbarPulseFrame, TopbarPulseTarget,
};
use crate::core::fonts::style_for;
use crate::core::fonts::FontRole;
use crate::core::fonts::FontRole::Title;
use crate::core::fonts::{FontId, FontSystem, TextStyle};
use crate::core::lock::{LockPulseKind, LockScreenSnapshot, LOCK_PULSE_DURATION};
use crate::core::shell::ManagedWindow;
use focaldesk_resources::RenderResources;
use focaldesk_themes::theme::BuiltInThemeId;
use focaldesk_themes::BackgroundTheme;
use focaldesk_themes::FlowTheme;
use focaldesk_themes::IconTheme;
use focaldesk_themes::TextTheme;
use focaldesk_themes::WallpaperTheme;
use focaldesk_types::WorkspaceId;
use focaldesk_ui::chrome_draw::draw_flow_field;
use focaldesk_ui::chrome_layout::{ChromeLayout, ChromeLayoutLogical, SIDEBAR_CORNER_RADIUS};
use focaldesk_ui::chrome_shaders::ChromeShaders;
use focaldesk_ui::desktop_frame::DesktopFrameCtx;
use focaldesk_ui::dialog::{Dialog, DialogId};
use focaldesk_ui::dialog_layout::layout_dialog;
use focaldesk_ui::dialog_layout::DialogLayout;
use focaldesk_ui::{UiVisualState, UiVisualStyle};
use smithay::backend::renderer::gles::GlesError;
use smithay::backend::renderer::gles::GlesPixelProgram;
use smithay::backend::renderer::gles::Uniform;
use smithay::wayland::seat::WaylandFocus;
#[cfg(feature = "xwayland")]
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::{error, info};

//use crate::core::chrome_svg::ChromeSvgCache;

//use crate::core::output::OutputState;
//use crate::core::ui::UiState;

render_elements! {
    pub FlowRenderElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
}

#[cfg(feature = "xwayland")]
static XWAYLAND_RENDER_LOGS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug)]
pub struct FrameCtx {
    pub output_size: (i32, i32),  // physical pixels
    pub output_scale: Scale<f64>, // fractional
    pub buffer_scale: i32,        // integer >= 1
    pub damage: Vec<Rectangle<i32, Physical>>,
    pub work: Rectangle<i32, Logical>,
    pub frame_no: u64,
    pub now: std::time::Instant,
    pub dt: std::time::Duration,
    pub active_output: OutputId,
    pub rendering_output: OutputId,
    pub focus_pulse: f32,
    /// Full-frame portal/OBS capture — keep egui and chrome on the captured output.
    pub portal_capture: bool,
    //pub time: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputRenderStage {
    All,
    Base,
    /// Work-area glass in FP16 after the opaque base blit, before clients.
    LinearGlassUnderClients,
    Clients,
    /// Compositor-owned dock, top-bar controls, indicators, and clock.
    ChromeOverlay,
    /// Client popups and transient compositor overlays above the client layer.
    Overlay,
    /// egui + software cursor only (sRGB), decoded into the linear scene before output encode.
    EguiOverlay,
}

/// Where work-area glass is composited in the staged linear SDR path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ChromeGlassPass {
    /// Legacy single-pass SDR: glass in the base stage.
    #[default]
    InBaseSdr,
    /// Staged linear: opaque base only; glass deferred.
    Skip,
    /// Staged linear: draw glass into the FP16 target with linearized colors.
    LinearUnderClients,
}

#[derive(Clone)]
pub enum ClientCompositingMode {
    Sdr,
    /// sRGB-encoded chrome/wallpaper assets → scene-linear Rec.709.
    LinearUi {
        srgb_to_linear: GlesTexProgram,
    },
    Linear {
        client_to_scene: GlesTexProgram,
        srgb_to_linear: GlesTexProgram,
    },
}

impl ClientCompositingMode {
    pub fn ui_textures_linear(&self) -> bool {
        matches!(self, Self::LinearUi { .. } | Self::Linear { .. })
    }
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
        focus_pulse: f32,
        portal_capture: bool,
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
            focus_pulse,
            portal_capture,
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
    pub sw_cursor_surface: Option<WlSurface>,
    pub sw_cursor_surface_elements: Vec<FlowRenderElement>,
    sw_cursor_cache_key: Option<(FlowCursorIcon, u32, u32)>,
    pub sw_cursor_hotspot: (i32, i32),
    pub sw_cursor_tex_size: (i32, i32),
    pub sw_cursor_dst_rect: Option<(i32, i32, i32, i32)>,
    pub scratch_damage: [Rectangle<i32, Physical>; 8],
    pub scratch_damage_len: usize,
    pub resources: RenderResources,
    pub redraw_all: bool,
    pub chrome_shaders: ChromeShaders,
    /// Reused framebuffer snapshot for the small sidebar/topbar glass controls.
    pub glass_control_background: Option<GlesTexture>,
    glass_control_background_size: (i32, i32),
    glass_control_background_linear: bool,
    glass_control_background_disabled: bool,
    pub start_time: Instant,
    //pub chrome_svg: ChromeSvgCache,
    pub font_atlas_texture: Option<GlesTexture>,
    pub fonts_prewarm_done: bool,
    pub portal_capture_blit_id: Id,
    output_icc_lut_gpu: HashMap<OutputId, OutputIccLutGpu>,
    pub icc_lut_fallback_logged: std::collections::HashSet<OutputId>,
}

struct OutputIccLutGpu {
    lut: crate::core::icc_lut::OutputIccLut,
    texture: GlesTexture,
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
    pub sidebar_pulse: Option<SidebarPulseFrame>,
    pub topbar_pulse: Option<TopbarPulseFrame>,
    pub clock_pulse: Option<ClockPulseFrame>,
    /// When true, composite the cursor from [`RenderState::sw_cursor_texture`] after chrome.
    pub draw_software_cursor: bool,
    /// Focus is retained by the accessibility model, while chrome rendering
    /// reads element state exclusively from the per-output components.
    pub ui_focus: Option<ElementId>,
    pub current_workspace: WorkspaceId,
    /// A fullscreen client on this output owns the entire display, including the shell chrome.
    pub fullscreen_client: bool,
    /// False when trusted standalone panel/dock layer surfaces own the shell presentation.
    pub draw_internal_chrome: bool,
    // 👇 ADD THESE
    pub dialogs: &'a [Dialog],
    pub active_dialog: Option<DialogId>,
    pub fonts: &'a FontSystem,
    pub theme: &'a FlowTheme,
    pub notifications: &'a [NotificationSnapshot],
    pub notification_unread_count: usize,
    pub update_available_count: usize,
    pub lock_screen: &'a LockScreenSnapshot,
    pub flip_egui_y: bool,
    pub client_compositing: ClientCompositingMode,
    pub chrome_glass_pass: ChromeGlassPass,
    /// When true, egui/cursor are drawn in a follow-up SDR pass (egui_glow outputs sRGB).
    pub defer_egui_to_sdr: bool,
    pub surface_colors: &'a std::collections::HashMap<Id, SurfaceColorRenderState>,
}

pub struct RenderInputsMut<'a> {
    pub ui: &'a mut UiState<GlesTexture>,
    pub desktop_output: &'a mut focaldesk_ui::desktop_output::DesktopOutput,
}

fn linearize_rgba(c: [f32; 4]) -> [f32; 4] {
    [
        srgb_to_linear(c[0]),
        srgb_to_linear(c[1]),
        srgb_to_linear(c[2]),
        c[3],
    ]
}

/// Linear Rec.709/sRGB scene color to linear Display P3 shader-authoring
/// coordinates. The wide shader epilogue applies the inverse transform before
/// blending into the FP16 scene, so existing theme colors retain their look.
fn scene_linear_to_display_p3(c: [f32; 4]) -> [f32; 4] {
    [
        0.822593 * c[0] + 0.177534 * c[1],
        0.033200 * c[0] + 0.966784 * c[1],
        0.017085 * c[0] + 0.072396 * c[1] + 0.910301 * c[2],
        c[3],
    ]
}

fn bevel_style_to_display_p3(mut style: BevelStyle) -> BevelStyle {
    style.face_color = scene_linear_to_display_p3(style.face_color);
    style.light_color = scene_linear_to_display_p3(style.light_color);
    style.shadow_color = scene_linear_to_display_p3(style.shadow_color);
    style.glow_color = scene_linear_to_display_p3(style.glow_color);
    style
}

fn linearize_flow_theme(theme: &FlowTheme) -> FlowTheme {
    let mut t = theme.clone();
    t.background.color = linearize_rgba(t.background.color);
    t.wallpaper.tint_color = linearize_rgba(t.wallpaper.tint_color);
    t.chrome.bg_color = linearize_rgba(t.chrome.bg_color);
    t.chrome.panel_color = linearize_rgba(t.chrome.panel_color);
    t.chrome.accent_color = linearize_rgba(t.chrome.accent_color);
    t.chrome.trim_color = linearize_rgba(t.chrome.trim_color);
    t.chrome.glass_tint = linearize_rgba(t.chrome.glass_tint);
    t.dialog.panel_color = linearize_rgba(t.dialog.panel_color);
    t.dialog.title_color = linearize_rgba(t.dialog.title_color);
    t.dialog.text_color = linearize_rgba(t.dialog.text_color);
    t.dialog.button_color = linearize_rgba(t.dialog.button_color);
    t.dialog.overlay_dim = linearize_rgba(t.dialog.overlay_dim);
    t.text.title = linearize_rgba(t.text.title);
    t.text.normal = linearize_rgba(t.text.normal);
    t.text.dim = linearize_rgba(t.text.dim);
    t.text.accent = linearize_rgba(t.text.accent);
    t.text.meta_label = linearize_rgba(t.text.meta_label);
    t.text.meta_value = linearize_rgba(t.text.meta_value);
    t.text.clock = linearize_rgba(t.text.clock);
    t.icons.inactive = linearize_rgba(t.icons.inactive);
    t.icons.hover = linearize_rgba(t.icons.hover);
    t.icons.active = linearize_rgba(t.icons.active);
    t.icons.disabled = linearize_rgba(t.icons.disabled);
    t.icons.glow = linearize_rgba(t.icons.glow);
    t
}

/// Split a front-to-back render-element list without moving elements across a
/// color-state boundary.
///
/// Smithay consumes the complete list front-to-back for occlusion and draws it
/// back-to-front.  Color-managed rendering needs a different texture program
/// for some elements, so each run is submitted separately in reverse run order.
/// Grouping non-adjacent elements with the same key would change stacking.
fn contiguous_runs_by_key<'a, T, K: PartialEq>(
    items: &'a [T],
    mut key_for: impl FnMut(&T) -> K,
) -> Vec<(K, &'a [T])> {
    let Some(first) = items.first() else {
        return Vec::new();
    };

    let mut runs = Vec::new();
    let mut start = 0;
    let mut current_key = key_for(first);
    for (index, item) in items.iter().enumerate().skip(1) {
        let next_key = key_for(item);
        if next_key != current_key {
            runs.push((current_key, &items[start..index]));
            start = index;
            current_key = next_key;
        }
    }
    runs.push((current_key, &items[start..]));
    runs
}

#[cfg(test)]
mod color_run_tests {
    use super::{clipped_dest_local_damage, contiguous_runs_by_key};
    use smithay::utils::{Physical, Rectangle};

    #[test]
    fn alternating_color_runs_preserve_back_to_front_draw_order() {
        // Input is Smithay's required front-to-back order. The first and last
        // elements intentionally share a color state but must not be batched.
        let elements = [('s', 0), ('s', 1), ('p', 2), ('s', 3), ('s', 4)];
        let runs = contiguous_runs_by_key(&elements, |element| element.0);

        assert_eq!(
            runs.iter()
                .map(|(key, run)| (*key, run.len()))
                .collect::<Vec<_>>(),
            vec![('s', 2), ('p', 1), ('s', 2)]
        );

        // Each Smithay submission reverses its run internally. Submitting the
        // runs in reverse therefore matches one full-list submission exactly.
        let draw_order = runs
            .iter()
            .rev()
            .flat_map(|(_, run)| run.iter().rev())
            .map(|element| element.1)
            .collect::<Vec<_>>();
        assert_eq!(draw_order, vec![4, 3, 2, 1, 0]);
    }

    #[test]
    fn retained_shader_damage_is_clipped_and_destination_local() {
        let destination = Rectangle::<i32, Physical>::from_loc_and_size((100, 40), (80, 30));
        let damage = [
            Rectangle::from_loc_and_size((90, 35), (20, 20)),
            Rectangle::from_loc_and_size((170, 60), (30, 20)),
            Rectangle::from_loc_and_size((0, 0), (20, 20)),
        ];

        assert_eq!(
            clipped_dest_local_damage(destination, &damage),
            vec![
                Rectangle::from_loc_and_size((0, 0), (10, 15)),
                Rectangle::from_loc_and_size((70, 20), (10, 10)),
            ]
        );
    }
}

fn chrome_theme_from_flow_theme(chrome: &focaldesk_themes::ChromeTheme) -> ChromeTheme {
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

/// Smithay pixel/texture draws expect damage in dest-local coordinates.
fn dest_local_damage(size: Size<i32, Physical>) -> [Rectangle<i32, Physical>; 1] {
    [Rectangle::from_loc_and_size((0, 0), (size.w, size.h))]
}

/// Convert output-local damage to the destination-local coordinates expected
/// by Smithay's pixel shader helpers.
fn clipped_dest_local_damage(
    dest: Rectangle<i32, Physical>,
    damage: &[Rectangle<i32, Physical>],
) -> Vec<Rectangle<i32, Physical>> {
    damage
        .iter()
        .filter_map(|rect| rect.intersection(dest))
        .map(|mut rect| {
            rect.loc -= dest.loc;
            rect
        })
        .collect()
}

fn ellipsize_for_width(text: &str, max_width: i32, size_px: u32) -> String {
    let max_chars = ((max_width.max(0) as usize) / ((size_px as usize).max(1) / 2).max(1)).max(3);
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }

    let keep = max_chars.saturating_sub(3);
    let mut out: String = text.chars().take(keep).collect();
    out.push_str("...");
    out
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

fn tooltip_rect_for_element(
    el_bounds: focaldesk_ui::element::UiRect,
    el_kind: UiElementKind,
    text_w: i32,
    output_size: Size<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let pad_x = 10;
    let width = (text_w + pad_x * 2).clamp(64, 260);
    let height = 30;
    let gap = 10;

    let max_x = (output_size.w - width - 6).max(0);
    let max_y = (output_size.h - height - 6).max(0);
    let (x, y) = match el_kind {
        UiElementKind::TopbarIndicator | UiElementKind::TopbarButton => (
            (el_bounds.x + (el_bounds.w - width) / 2).clamp(6, max_x),
            (el_bounds.y + el_bounds.h + gap).clamp(6, max_y),
        ),
        UiElementKind::TopbarFlowField => (
            (el_bounds.x + 8).clamp(6, max_x),
            (el_bounds.y + el_bounds.h + gap).clamp(6, max_y),
        ),
        _ => (
            (el_bounds.x + el_bounds.w + gap).clamp(6, max_x),
            (el_bounds.y + (el_bounds.h - height) / 2).clamp(6, max_y),
        ),
    };

    Rectangle::from_loc_and_size((x, y), (width, height))
}

fn title_glyph_style(token: &str, in_meta: bool, is_active_output: bool) -> UiVisualStyle {
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

impl Default for RenderState {
    fn default() -> Self {
        Self::new()
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
            sw_cursor_surface: None,
            sw_cursor_surface_elements: Vec::new(),
            sw_cursor_cache_key: None,
            sw_cursor_hotspot: (0, 0),
            sw_cursor_tex_size: (0, 0),
            sw_cursor_dst_rect: None,
            resources: RenderResources::new(),
            redraw_all: true,
            chrome_shaders: ChromeShaders::new(),
            glass_control_background: None,
            glass_control_background_size: (1, 1),
            glass_control_background_linear: false,
            glass_control_background_disabled: false,
            start_time: Instant::now(),

            font_atlas_texture: None,
            fonts_prewarm_done: false,
            portal_capture_blit_id: Id::new(),
            output_icc_lut_gpu: HashMap::new(),
            icc_lut_fallback_logged: std::collections::HashSet::new(),
        }
    }

    /// Upload or reuse the ICC LUT 2D atlas for an output.
    pub fn ensure_output_icc_lut_texture(
        &mut self,
        renderer: &mut GlesRenderer,
        output_id: OutputId,
        lut: &crate::core::icc_lut::OutputIccLut,
    ) -> Result<&GlesTexture, smithay::backend::renderer::gles::GlesError> {
        let needs_upload = self
            .output_icc_lut_gpu
            .get(&output_id)
            .map(|cached| cached.lut != *lut)
            .unwrap_or(true);
        if needs_upload {
            let (w, h) = lut.atlas_size();
            let mut rgba = Vec::with_capacity((w * h * 4) as usize);
            for chunk in lut.rgb.chunks_exact(3) {
                rgba.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            let texture = renderer.import_memory(
                &rgba,
                Fourcc::Abgr8888,
                Size::from((w as i32, h as i32)),
                false,
            )?;
            self.output_icc_lut_gpu.insert(
                output_id,
                OutputIccLutGpu {
                    lut: lut.clone(),
                    texture,
                },
            );
        }
        Ok(&self.output_icc_lut_gpu.get(&output_id).unwrap().texture)
    }

    fn draw_topbar_identity(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        fonts: &FontSystem,
        layout: &ChromeLayoutLogical,
        title: &str,
        output_number: u64,
        workspace_number: usize,
        theme: &focaldesk_themes::FlowTheme,
        scale: Scale<f64>,
    ) -> Result<(), GlesError> {
        let builtin_id = theme.id.builtin_id().unwrap_or(BuiltInThemeId::Eagle);
        let title_style = style_for(FontRole::Title, 24, builtin_id);
        let style = style_for(FontRole::Meta, 18, builtin_id);
        let gap = theme.spacing.max(4);

        let output_s = output_number.to_string();
        let workspace_s = workspace_number.to_string();
        let title_bounds = fonts
            .vertical_bounds(title, title_style)
            .unwrap_or((-(title_style.size_px as i32), 0));
        let meta_bounds = [
            fonts.vertical_bounds("OUT", style),
            fonts.vertical_bounds(&output_s, style),
            fonts.vertical_bounds("WS", style),
            fonts.vertical_bounds(&workspace_s, style),
        ]
        .into_iter()
        .flatten()
        .reduce(|(top, bottom), (next_top, next_bottom)| {
            (top.min(next_top), bottom.max(next_bottom))
        })
        .unwrap_or((-(style.size_px as i32), 0));
        let title_center_y = layout.topbar.title.loc.y + layout.topbar.title.size.h / 2;
        let title_y = title_center_y - (title_bounds.0 + title_bounds.1) / 2;
        let meta_y = title_center_y - (meta_bounds.0 + meta_bounds.1) / 2;
        let mut x_logical = layout.topbar.title.loc.x + 14;

        self.draw_text_cached(
            frame,
            fonts,
            title,
            x_logical,
            title_y,
            title_style,
            theme.text.title,
            scale,
        )?;

        x_logical += fonts.advance_width(title, title_style) + gap;

        self.draw_text_cached(
            frame,
            fonts,
            "OUT",
            x_logical,
            meta_y,
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
            meta_y,
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
            meta_y,
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
            meta_y,
            style,
            theme.text.meta_value,
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
                tex,
                src,
                dst,
                &[damage_local],
                &[],
                Transform::Normal,
                1.0,
                Some(program),
                &[Uniform::new("u_tint", color)],
            ) {
                error!(
                    target: "focaldesk",
                    session_id = session_id(),
                    error = ?e,
                    "tinted icon render failed"
                );
            }

            // ✅ MUST be inside loop
            cursor_x += glyph.advance.round() as i32;
        }

        Ok(())
    }

    fn draw_hover_tooltip(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        fonts: &FontSystem,
        desktop_output: &focaldesk_ui::desktop_output::DesktopOutput,
        output_size: Size<i32, Logical>,
        theme: &FlowTheme,
        scale: Scale<f64>,
    ) -> Result<(), GlesError> {
        if self.font_atlas_texture.is_none() || self.chrome_shaders.font_text.is_none() {
            return Ok(());
        }

        let Some(el) = desktop_output.chrome_elements().find(|el| {
            el.hovered
                && matches!(
                    el.kind,
                    UiElementKind::SidebarButton
                        | UiElementKind::WorkspaceSlot
                        | UiElementKind::TopbarIndicator
                        | UiElementKind::TopbarButton
                        | UiElementKind::TopbarFlowField
                )
        }) else {
            return Ok(());
        };

        let Some(text) = el.tooltip.as_deref().filter(|text| !text.is_empty()) else {
            return Ok(());
        };

        let style = style_for(
            FontRole::Label,
            14,
            theme.id.builtin_id().unwrap_or(BuiltInThemeId::Eagle),
        );
        let text_w = fonts.advance_width(text, style);
        let rect = tooltip_rect_for_element(el.bounds, el.kind, text_w, output_size);

        self.draw_rounded_rect(frame, rect, scale, 6.0, [0.035, 0.050, 0.075, 0.92])?;

        let baseline_y = rect.loc.y + 20;
        self.draw_text_cached(
            frame,
            fonts,
            text,
            rect.loc.x + 10,
            baseline_y,
            style,
            theme.text.normal,
            scale,
        )
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
        self.sw_cursor_surface = None;
        self.sw_cursor_surface_elements.clear();
        self.sw_cursor_cache_key = None;
        self.sw_cursor_dst_rect = None;
    }

    pub fn clear_sw_cursor_cache_key(&mut self) {
        self.sw_cursor_cache_key = None;
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
        let full = Rectangle::from_loc_and_size((0, 0), ctx.output_size);
        if !self.sw_cursor_surface_elements.is_empty() {
            draw_render_elements(
                frame,
                ctx.output_scale.x,
                &self.sw_cursor_surface_elements,
                std::slice::from_ref(&full),
            )?;
            return Ok(());
        }

        let Some(tex) = self.sw_cursor_texture.as_ref() else {
            return Ok(());
        };
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
            std::slice::from_ref(&full),
        )?;
        Ok(())
    }

    fn render_icon_with_tint(
        frame: &mut GlesFrame<'_, '_>,
        atlas: &focaldesk_ui::atlas::IconAtlas,
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
                error!(
                    target: "focaldesk",
                    session_id = session_id(),
                    icon = ?icon,
                    error = ?e,
                    "tinted icon render failed"
                );
            }
        }
    }

    fn draw_title_text(
        frame: &mut GlesFrame<'_, '_>,
        atlas: &focaldesk_ui::atlas::IconAtlas,
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
                let dest_logical =
                    Rectangle::from_loc_and_size((x_logical, y_logical), (glyph_w, glyph_h));

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
        atlas: &focaldesk_ui::atlas::IconAtlas,
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
                // FOCALDESK title stays bright.
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
            atlas: &focaldesk_ui::atlas::IconAtlas,
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
          //          flog_error!("render_atlas_icon failed for {:?}: {:?}", icon, e);
           //     }
            //}
        }
    */

    fn draw_icon_in_rect(
        frame: &mut GlesFrame<'_, '_>,
        atlas: &focaldesk_ui::atlas::IconAtlas,
        icon: IconId,
        _state: IconState,
        rect_logical: Rectangle<i32, Logical>,
        scale: Scale<f64>,
        style: UiVisualStyle,
        program: &GlesTexProgram,
    ) {
        RenderState::render_icon_with_tint(frame, atlas, icon, rect_logical, scale, style, program);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_glass_control(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        atlas: &focaldesk_ui::atlas::IconAtlas,
        icon: IconId,
        control_rect_logical: Rectangle<i32, Logical>,
        icon_rect_logical: Rectangle<i32, Logical>,
        scale: Scale<f64>,
        theme: &FlowTheme,
        hovered: bool,
        enabled: bool,
        active: bool,
        output_factor: f32,
        linear_target: bool,
    ) -> Result<bool, GlesError> {
        let program = if linear_target && self.chrome_shaders.wide_gamut_ready() {
            self.chrome_shaders
                .glass_control_wide
                .clone()
                .or_else(|| self.chrome_shaders.glass_control.clone())
        } else {
            self.chrome_shaders.glass_control.clone()
        };
        let Some(program) = program else {
            return Ok(false);
        };
        let Some(background) = self.glass_control_background.clone() else {
            return Ok(false);
        };
        let Some(entry) = atlas.get(icon).copied() else {
            return Ok(false);
        };

        let control = to_physical_rect(control_rect_logical, scale);
        let icon_rect = to_physical_rect(icon_rect_logical, scale);
        if control.size.w <= 0 || control.size.h <= 0 {
            return Ok(false);
        }

        // glCopyTexSubImage2D cannot convert between floating-point and
        // normalized fixed-point attachments. Match the snapshot texture to
        // the active scene target: RGBA16F for the staged linear compositor,
        // RGBA8 for the legacy SDR path.
        let required_w = self.glass_control_background_size.0.max(control.size.w);
        let required_h = self.glass_control_background_size.1.max(control.size.h);
        let reallocate = (required_w, required_h) != self.glass_control_background_size
            || linear_target != self.glass_control_background_linear;
        let background_id = background.tex_id();
        frame.with_context(|gl| unsafe {
            gl.ActiveTexture(ffi::TEXTURE1);
            gl.BindTexture(ffi::TEXTURE_2D, background_id);
            if reallocate {
                gl.TexImage2D(
                    ffi::TEXTURE_2D,
                    0,
                    if linear_target {
                        ffi::RGBA16F as i32
                    } else {
                        ffi::RGBA as i32
                    },
                    required_w,
                    required_h,
                    0,
                    ffi::RGBA,
                    if linear_target {
                        ffi::HALF_FLOAT
                    } else {
                        ffi::UNSIGNED_BYTE
                    },
                    std::ptr::null(),
                );
            }
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_S,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_T,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.CopyTexSubImage2D(
                ffi::TEXTURE_2D,
                0,
                0,
                0,
                control.loc.x,
                control.loc.y,
                control.size.w,
                control.size.h,
            );
            gl.ActiveTexture(ffi::TEXTURE0);
        })?;
        if reallocate {
            self.glass_control_background_size = (required_w, required_h);
            self.glass_control_background_linear = linear_target;
        }

        let atlas_size = atlas.texture.size();
        let icon_uv_origin = [
            entry.x as f32 / atlas_size.w as f32,
            entry.y as f32 / atlas_size.h as f32,
        ];
        let icon_uv_size = [
            entry.w as f32 / atlas_size.w as f32,
            entry.h as f32 / atlas_size.h as f32,
        ];
        let icon_local_rect = [
            (icon_rect.loc.x - control.loc.x) as f32 / control.size.w as f32,
            (icon_rect.loc.y - control.loc.y) as f32 / control.size.h as f32,
            icon_rect.size.w as f32 / control.size.w as f32,
            icon_rect.size.h as f32 / control.size.h as f32,
        ];
        let source = Rectangle::<f64, Buffer>::from_loc_and_size(
            (0.0, 0.0),
            (atlas_size.w as f64, atlas_size.h as f64),
        );
        let damage = [Rectangle::from_loc_and_size((0, 0), control.size)];
        let opacity = if enabled { 0.96 } else { 0.72 };
        let glass_tint = if linear_target && self.chrome_shaders.wide_gamut_ready() {
            scene_linear_to_display_p3(theme.chrome.glass_tint)
        } else {
            theme.chrome.glass_tint
        };
        let accent_color = if linear_target && self.chrome_shaders.wide_gamut_ready() {
            scene_linear_to_display_p3(theme.chrome.accent_color)
        } else {
            theme.chrome.accent_color
        };
        let result = frame.render_texture_from_to(
            &atlas.texture,
            source,
            control,
            &damage,
            &[],
            Transform::Normal,
            1.0,
            Some(&program),
            &[
                Uniform::new("u_background", 1i32),
                Uniform::new(
                    "u_background_uv_size",
                    [
                        control.size.w as f32 / required_w as f32,
                        control.size.h as f32 / required_h as f32,
                    ],
                ),
                Uniform::new("u_size", [control.size.w as f32, control.size.h as f32]),
                Uniform::new("u_icon_uv_origin", icon_uv_origin),
                Uniform::new("u_icon_uv_size", icon_uv_size),
                Uniform::new("u_icon_rect", icon_local_rect),
                Uniform::new(
                    "u_icon_texel_size",
                    [1.0 / atlas_size.w as f32, 1.0 / atlas_size.h as f32],
                ),
                Uniform::new("u_glass_tint", glass_tint),
                Uniform::new(
                    "u_accent_color",
                    [accent_color[0], accent_color[1], accent_color[2]],
                ),
                Uniform::new(
                    "u_corner_radius",
                    theme.chrome.corner_radius * scale.x.max(scale.y) as f32,
                ),
                Uniform::new(
                    "u_border_width",
                    (theme.chrome.border_width * scale.x.max(scale.y) as f32).max(1.0),
                ),
                Uniform::new("u_hover", hovered as u8 as f32),
                // UiElement currently uses `active` for the depressed/latched visual state.
                Uniform::new("u_pressed", active as u8 as f32),
                Uniform::new("u_enabled", enabled as u8 as f32),
                Uniform::new("u_active", active as u8 as f32),
                Uniform::new("u_warning", 0.0f32),
                Uniform::new("u_light_dir", [-0.45f32, -0.65, 0.80]),
                Uniform::new("u_opacity", opacity),
                Uniform::new("u_output_factor", output_factor),
                Uniform::new("u_icon_strength", 0.88f32),
                Uniform::new("u_etch_depth", 5.0f32),
            ],
        );
        let _ = frame.with_context(|gl| unsafe {
            gl.ActiveTexture(ffi::TEXTURE1);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
            gl.ActiveTexture(ffi::TEXTURE0);
        });
        result.map(|_| true)
    }

    pub fn ensure_shader_programs(&mut self, renderer: &mut GlesRenderer) -> Result<(), GlesError> {
        self.chrome_shaders.ensure_compiled(renderer)?;
        if self.chrome_shaders.glass_control.is_some()
            && self.glass_control_background.is_none()
            && !self.glass_control_background_disabled
        {
            match renderer.import_memory(&[0, 0, 0, 0], Fourcc::Abgr8888, (1, 1).into(), false) {
                Ok(texture) => {
                    self.glass_control_background = Some(texture);
                    self.glass_control_background_size = (1, 1);
                }
                Err(err) => {
                    self.glass_control_background_disabled = true;
                    flog(format!(
                        "glass control background allocation failed; keeping legacy controls: {err}"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Drop every cached shader program and GPU texture handle.
    ///
    /// These are all "compile/upload once, reuse forever" caches, which is normally
    /// correct — but the DRM backend recreates the `EGLContext` (and thus the
    /// `GlesRenderer`) when resuming from suspend, which invalidates every GL object
    /// handle compiled/uploaded against the old context. Without this, resume leaves
    /// the renderer replaying draw calls against dead handles forever (blank screen,
    /// GL_INVALID_* error flood) instead of recompiling/re-uploading them.
    pub fn invalidate_gpu_state(&mut self) {
        self.chrome_shaders = ChromeShaders::new();
        self.glass_control_background = None;
        self.glass_control_background_size = (1, 1);
        self.glass_control_background_linear = false;
        self.glass_control_background_disabled = false;
        self.wallpaper_texture = None;
        self.sw_cursor_texture = None;
        self.sw_cursor_cache_key = None;
        self.font_atlas_texture = None;
        self.fonts_prewarm_done = false;
        self.output_icc_lut_gpu.clear();
        self.icc_lut_fallback_logged.clear();
        self.redraw_all = true;
    }

    fn draw_clock_text(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        atlas: &focaldesk_ui::atlas::IconAtlas,
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
        let total_w: i32 =
            widths.iter().sum::<i32>() + spacing * (widths.len().saturating_sub(1) as i32);

        let start_x_logical = rect_logical.loc.x + ((rect_logical.size.w - total_w).max(0) / 2);
        let y_logical = rect_logical.loc.y + ((rect_logical.size.h - digit_h) / 2);

        let mut x_logical = start_x_logical;

        for (idx, ch) in glyphs.iter().enumerate() {
            if let Some(icon) = char_to_clock_icon(*ch) {
                let w = widths[idx];

                let glyph_rect_logical =
                    Rectangle::from_loc_and_size((x_logical, y_logical), (w, digit_h));

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
        let shadow = [0.02, 0.01, 0.0, color[3].min(0.9)];

        self.draw_text_cached(
            frame,
            fonts,
            text,
            x_logical + 1,
            y_logical + 1,
            style,
            shadow,
            scale,
        )?;

        self.draw_text_cached(
            frame, fonts, text, x_logical, y_logical, style, color, scale,
        )?;

        Ok(())
    }

    pub fn ensure_wallpaper_loaded(&mut self, renderer: &mut GlesRenderer) {
        if self.wallpaper_texture.is_some() {
            return;
        }
        info!(
            target: "focaldesk",
            session_id = session_id(),
            "ensure_wallpaper_loaded: attempting load"
        );

        let tex = Self::load_wallpaper(
            renderer,
            "/home/steve/focaldesk/assets/wallpaper/focaldesk_wallpaper.png",
        );
        info!(
            target: "focaldesk",
            session_id = session_id(),
            loaded = tex.is_some(),
            "ensure_wallpaper_loaded: load result"
        );

        self.wallpaper_texture = tex;
    }

    pub fn load_wallpaper(renderer: &mut GlesRenderer, path: &str) -> Option<GlesTexture> {
        info!(
            target: "focaldesk",
            session_id = session_id(),
            path = %path,
            "load_wallpaper: opening"
        );

        let img = match image::open(path) {
            Ok(i) => i,
            Err(e) => {
                error!(
                    target: "focaldesk",
                    session_id = session_id(),
                    path = %path,
                    error = ?e,
                    "load_wallpaper: image::open failed"
                );
                return None;
            }
        };

        let (w, h) = img.dimensions();
        info!(
            target: "focaldesk",
            session_id = session_id(),
            width = w,
            height = h,
            "load_wallpaper: decoded"
        );

        //let rgba = img.to_rgba8();
        let mut rgba = img.to_rgba8();

        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2); // swap R and B
        }

        //let fourcc = Fourcc::Rgba8888;
        info!(
            target: "focaldesk",
            session_id = session_id(),
            bytes = rgba.len(),
            "load_wallpaper: rgba bytes"
        );

        // IMPORTANT: your buffer is RGBA; ABGR is often wrong here.
        let fourcc = Fourcc::Argb8888; // try this first
        info!(
            target: "focaldesk",
            session_id = session_id(),
            fourcc = ?fourcc,
            "load_wallpaper: importing to GPU"
        );

        match renderer.import_memory(&rgba, fourcc, (w as i32, h as i32).into(), false) {
            Ok(tex) => {
                info!(
                    target: "focaldesk",
                    session_id = session_id(),
                    "load_wallpaper: import_memory OK"
                );
                Some(tex)
            }
            Err(e) => {
                error!(
                    target: "focaldesk",
                    session_id = session_id(),
                    error = ?e,
                    "load_wallpaper: import_memory FAILED"
                );
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
        let damage_local = clipped_dest_local_damage(rect_physical, damage);

        frame.render_pixel_shader_to(
            program,
            src_rect,
            rect_physical,
            buffer_size,
            Some(&damage_local),
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
    ) {
        let rect_physical = to_physical_rect(rect_logical, scale);

        let src_rect = Rectangle::<f64, Buffer>::from_loc_and_size(
            (0.0, 0.0),
            (rect_physical.size.w as f64, rect_physical.size.h as f64),
        );

        let buffer_size = Size::<i32, Buffer>::from((rect_physical.size.w, rect_physical.size.h));
        let damage_local = clipped_dest_local_damage(rect_physical, damage);

        let _ = frame.render_pixel_shader_to(
            button,
            src_rect,
            rect_physical,
            buffer_size,
            Some(&damage_local),
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
        );
    }

    fn rect_apply_flipped180(
        r: Rectangle<i32, Physical>,
        output_size: (i32, i32),
    ) -> Rectangle<i32, Physical> {
        let (w, h) = output_size;
        Rectangle::new(
            ((w - (r.loc.x + r.size.w)), (h - (r.loc.y + r.size.h))).into(),
            r.size,
        )
    }

    fn draw_popup_elements(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        elements: &[FlowRenderElement],
        client_compositing: &ClientCompositingMode,
        surface_colors: &std::collections::HashMap<Id, SurfaceColorRenderState>,
    ) -> Result<(), GlesError> {
        let damage = &ctx.damage;

        match client_compositing {
            ClientCompositingMode::Linear {
                client_to_scene, ..
            } => {
                let runs = contiguous_runs_by_key(elements, |elem| {
                    surface_colors
                        .get(elem.id())
                        .copied()
                        .unwrap_or_else(SurfaceColorRenderState::srgb_default)
                });
                for (color, run) in runs.into_iter().rev() {
                    let m = color.client_to_scene;
                    let uniforms = vec![
                        Uniform::new(
                            "u_decode_tf",
                            color.description.transfer.decode_mode() as u32 as f32,
                        ),
                        Uniform::new(
                            "u_reference_white_nits",
                            color.description.reference_white_nits.max(1.0),
                        ),
                        Uniform::new(
                            "u_linear_to_scene_scale",
                            color.description.linear_to_scene_scale(),
                        ),
                        Uniform::new("u_m0", [m[0][0], m[0][1], m[0][2]]),
                        Uniform::new("u_m1", [m[1][0], m[1][1], m[1][2]]),
                        Uniform::new("u_m2", [m[2][0], m[2][1], m[2][2]]),
                        Uniform::new("u_src_bits", color.pq_src_bits),
                    ];
                    frame.override_default_tex_program(client_to_scene.clone(), uniforms);
                    let result = draw_render_elements(frame, ctx.output_scale.x, run, damage);
                    frame.clear_tex_program_override();
                    result?;
                }
            }
            ClientCompositingMode::LinearUi { srgb_to_linear } => {
                frame.override_default_tex_program(srgb_to_linear.clone(), vec![]);
                draw_render_elements(frame, ctx.output_scale.x, elements, damage)?;
                frame.clear_tex_program_override();
            }
            ClientCompositingMode::Sdr => {
                draw_render_elements(frame, ctx.output_scale.x, elements, damage)?;
            }
        }

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

        let dst_phys: Rectangle<i32, Physical> = rect.to_physical_precise_round::<f64, i32>(scale);

        let src_buffer: Rectangle<f64, Buffer> =
            Rectangle::from_size((dst_phys.size.w as f64, dst_phys.size.h as f64).into());

        let buffer_size: Size<i32, Buffer> = (dst_phys.size.w, dst_phys.size.h).into();

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
        self.render_stage(frame, inputs, muts, OutputRenderStage::All)
    }

    pub fn render_stage(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        inputs: RenderInputs<'_>,
        muts: RenderInputsMut<'_>,
        stage: OutputRenderStage,
    ) -> Result<(), GlesError> {
        let linear_theme_storage;
        let theme = if inputs.client_compositing.ui_textures_linear()
            || matches!(
                inputs.chrome_glass_pass,
                ChromeGlassPass::LinearUnderClients
            ) {
            linear_theme_storage = linearize_flow_theme(inputs.theme);
            &linear_theme_storage
        } else {
            inputs.theme
        };

        if matches!(stage, OutputRenderStage::All | OutputRenderStage::Base) {
            self.draw_background(frame, inputs.ctx, inputs.output, theme.background);

            // Chrome draws opaque bevels over the work region; clients must be composited
            // after that shell (and work-area wallpaper), or they are fully covered.
            if !inputs.fullscreen_client {
                if inputs.draw_internal_chrome {
                    self.draw_chrome_below_work_wallpaper(
                        frame,
                        inputs.ctx,
                        inputs.layout,
                        inputs.output,
                        inputs.metrics,
                        muts.ui,
                        inputs.sidebar_hover_slot,
                        inputs.sidebar_pulse,
                        inputs.topbar_pulse,
                        inputs.clock_pulse,
                        theme.chrome,
                    );
                }

                self.draw_wallpaper_in_rect(
                    frame,
                    inputs.ctx,
                    inputs.layout.work_area.recess,
                    inputs.ctx.output_scale,
                    theme.wallpaper.clone(),
                    &inputs.client_compositing,
                );

                // Work-area glass must sit under client surfaces (trim/icons stay above).
                if inputs.draw_internal_chrome
                    && matches!(inputs.chrome_glass_pass, ChromeGlassPass::InBaseSdr)
                {
                    self.draw_work_area_glass_layer(frame, &inputs, theme)?;
                }
            }
        }

        if matches!(stage, OutputRenderStage::LinearGlassUnderClients) {
            if !inputs.fullscreen_client && inputs.draw_internal_chrome {
                self.draw_work_area_glass_layer(frame, &inputs, theme)?;
            }
            return Ok(());
        }

        if matches!(stage, OutputRenderStage::All | OutputRenderStage::Clients) {
            self.draw_clients(
                frame,
                inputs.ctx,
                inputs.elements,
                &inputs.client_compositing,
                inputs.surface_colors,
            );
        }

        if matches!(
            stage,
            OutputRenderStage::Base
                | OutputRenderStage::Clients
                | OutputRenderStage::LinearGlassUnderClients
        ) {
            return Ok(());
        }

        if matches!(stage, OutputRenderStage::EguiOverlay) {
            let egui_frame_ctx = DesktopFrameCtx {
                output_size: inputs.ctx.output_size,
                output_scale: inputs.ctx.output_scale,
                work: inputs.layout.work_area.recess,
                active_output: inputs.ctx.active_output,
                rendering_output: inputs.ctx.rendering_output,
                now: inputs.ctx.now,
                start_time: self.start_time,
                flip_egui_y: inputs.flip_egui_y,
                portal_capture: inputs.ctx.portal_capture,
            };
            muts.desktop_output.egui.render(
                frame,
                &egui_frame_ctx,
                &inputs.ctx.damage,
                &self.chrome_shaders,
                inputs.theme,
            )?;
            if inputs.draw_software_cursor {
                self.draw_software_cursor_overlay(frame, inputs.ctx)?;
            }
            return Ok(());
        }

        if matches!(
            stage,
            OutputRenderStage::All | OutputRenderStage::ChromeOverlay
        ) && !inputs.fullscreen_client
            && inputs.draw_internal_chrome
        {
            self.draw_chrome_trim_glass_icons(
                frame,
                inputs.ctx,
                inputs.layout,
                inputs.output,
                inputs.metrics,
                muts.ui,
                inputs.ui_focus,
                muts.desktop_output,
                inputs.current_workspace,
                inputs.fonts,
                inputs.notification_unread_count,
                inputs.update_available_count,
                theme,
                inputs.client_compositing.ui_textures_linear(),
            );
        }

        if matches!(stage, OutputRenderStage::ChromeOverlay) {
            return Ok(());
        }

        self.draw_popup_elements(
            frame,
            inputs.ctx,
            inputs.popup_elements,
            &inputs.client_compositing,
            inputs.surface_colors,
        )?;

        if inputs.ctx.active_output == inputs.ctx.rendering_output {
            self.draw_notifications(frame, inputs.ctx, inputs.notifications, inputs.fonts, theme)?;
        }

        let program = self.chrome_shaders.tinted_icon.clone();

        if let (Some(atlas), Some(program)) = (muts.ui.chrome.atlas.as_ref(), program.as_ref()) {
            let output_px: Size<i32, Physical> = inputs.ctx.output_size.into();
            // Modal scrim: every output. Panel / text: only on the output that opened the dialog.
            let draw_dialog_chrome = inputs
                .active_dialog
                .and_then(|id| inputs.dialogs.iter().find(|d| d.id == id))
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

        let wide_gamut = inputs.client_compositing.ui_textures_linear()
            && self.chrome_shaders.wide_gamut_ready();
        if let Err(err) = self.draw_active_output_glow(frame, inputs.ctx, theme, wide_gamut) {
            error!(
                target: "focaldesk",
                session_id = session_id(),
                error = ?err,
                "active output accent render failed"
            );
        }

        self.draw_lock_screen(
            frame,
            inputs.ctx,
            inputs.lock_screen,
            inputs.fonts,
            theme,
            wide_gamut,
        )?;

        if inputs.defer_egui_to_sdr {
            return Ok(());
        }

        let egui_frame_ctx = DesktopFrameCtx {
            output_size: inputs.ctx.output_size,
            output_scale: inputs.ctx.output_scale,
            work: inputs.layout.work_area.recess,
            active_output: inputs.ctx.active_output,
            rendering_output: inputs.ctx.rendering_output,
            now: inputs.ctx.now,
            start_time: self.start_time,
            flip_egui_y: inputs.flip_egui_y,
            portal_capture: inputs.ctx.portal_capture,
        };
        muts.desktop_output.egui.render(
            frame,
            &egui_frame_ctx,
            &inputs.ctx.damage,
            &self.chrome_shaders,
            theme,
        )?;

        if inputs.draw_software_cursor {
            self.draw_software_cursor_overlay(frame, inputs.ctx)?;
        }

        Ok(())
    }

    fn draw_notifications(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        notifications: &[NotificationSnapshot],
        fonts: &FontSystem,
        theme: &FlowTheme,
    ) -> Result<(), GlesError> {
        if notifications.is_empty() {
            return Ok(());
        }
        if self.font_atlas_texture.is_none() || self.chrome_shaders.font_text.is_none() {
            return Ok(());
        }

        let toast_w = ctx.work.size.w.min(360).max(240);
        let toast_h = 86;
        let margin = 18;
        let gap = 10;
        let x = ctx.work.loc.x + ctx.work.size.w - toast_w - margin;
        let mut y = ctx.work.loc.y + margin;
        let theme_id = theme.id.builtin_id().unwrap_or(BuiltInThemeId::Eagle);

        for notification in notifications {
            let mut alpha = 0.94;
            if let Some(timeout) = notification.timeout {
                let remaining = timeout.saturating_sub(notification.age);
                if remaining < Duration::from_millis(300) {
                    alpha *= remaining.as_secs_f32() / 0.3;
                }
            }

            let rect = Rectangle::<i32, Logical>::from_loc_and_size((x, y), (toast_w, toast_h));
            let mut bg = theme.chrome.panel_color;
            bg[3] = alpha;
            self.draw_rounded_rect(frame, rect, ctx.output_scale, 8.0, bg)?;

            let accent_rect = Rectangle::<i32, Logical>::from_loc_and_size((x, y), (4, toast_h));
            let mut accent = theme.chrome.accent_color;
            accent[3] = alpha.min(0.85);
            self.draw_rounded_rect(frame, accent_rect, ctx.output_scale, 2.0, accent)?;

            let text_x = x + 18;
            let title = ellipsize_for_width(&notification.title, toast_w - 36, 16);
            let body = ellipsize_for_width(&notification.body, toast_w - 36, 15);

            self.draw_text_cached(
                frame,
                fonts,
                &title,
                text_x,
                y + 28,
                style_for(FontRole::Title, 16, theme_id),
                [1.0, 0.97, 0.90, alpha],
                ctx.output_scale,
            )?;
            self.draw_text_cached(
                frame,
                fonts,
                &body,
                text_x,
                y + 58,
                style_for(FontRole::Body, 15, theme_id),
                [0.78, 0.86, 1.0, alpha * 0.92],
                ctx.output_scale,
            )?;

            y += toast_h + gap;
        }

        Ok(())
    }

    fn draw_lock_screen(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        lock: &LockScreenSnapshot,
        fonts: &FontSystem,
        theme: &FlowTheme,
        wide_gamut: bool,
    ) -> Result<(), GlesError> {
        if !lock.active {
            return Ok(());
        }

        let output_physical = Size::<i32, Physical>::from(ctx.output_size);
        let full_physical = Rectangle::<i32, Physical>::from_loc_and_size((0, 0), output_physical);
        RenderState::draw_solid_rect(
            frame,
            full_physical,
            &[full_physical],
            [0.005, 0.008, 0.014, 0.72],
        )?;
        self.draw_screensaver_background(frame, ctx, full_physical, wide_gamut)?;

        let logical_w = (f64::from(ctx.output_size.0) / ctx.output_scale.x).round() as i32;
        let logical_h = (f64::from(ctx.output_size.1) / ctx.output_scale.y).round() as i32;
        let screen = Rectangle::<i32, Logical>::from_loc_and_size((0, 0), (logical_w, logical_h));
        let haze = Rectangle::<i32, Logical>::from_loc_and_size(
            (screen.loc.x + 24, screen.loc.y + 24),
            ((screen.size.w - 48).max(1), (screen.size.h - 48).max(1)),
        );
        self.draw_rounded_rect(
            frame,
            haze,
            ctx.output_scale,
            18.0,
            [0.12, 0.16, 0.20, 0.12],
        )?;

        let panel_w = screen.size.w.min(460).max(320);
        let panel_h = 190;
        let panel_x = screen.loc.x + (screen.size.w - panel_w) / 2;
        let panel_y = screen.loc.y + (screen.size.h - panel_h) / 2;
        let panel =
            Rectangle::<i32, Logical>::from_loc_and_size((panel_x, panel_y), (panel_w, panel_h));

        let mut panel_color = theme.chrome.panel_color;
        panel_color[3] = 0.74;
        self.draw_rounded_rect(frame, panel, ctx.output_scale, 12.0, panel_color)?;

        let mut trim = theme.chrome.accent_color;
        trim[3] = 0.36;
        let trim_rect = Rectangle::<i32, Logical>::from_loc_and_size(
            (panel_x + 14, panel_y + 12),
            (panel_w - 28, 3),
        );
        self.draw_rounded_rect(frame, trim_rect, ctx.output_scale, 2.0, trim)?;

        let pulse_program = if wide_gamut {
            self.chrome_shaders.pulse_wide.as_ref()
        } else {
            self.chrome_shaders.pulse.as_ref()
        };
        if let (Some(program), Some(pulse)) = (pulse_program, lock.pulse) {
            let color = match pulse.kind {
                LockPulseKind::Rejected => [0.78, 0.02, 0.02, 0.92],
                LockPulseKind::Accepted => [0.35, 1.0, 0.28, 0.86],
            };
            let panel_physical = to_physical_rect(panel, ctx.output_scale);
            let pulse_damage = dest_local_damage(panel_physical.size);
            let center = Point::<f64, Logical>::from((
                f64::from(panel.loc.x + panel.size.w / 2),
                f64::from(panel.loc.y + panel.size.h / 2),
            ));
            Self::draw_pulse(
                frame,
                program,
                panel,
                center,
                pulse.elapsed,
                ctx.output_scale,
                &pulse_damage,
                color,
            )?;

            let fade = 1.0
                - (pulse.elapsed.as_secs_f32() / LOCK_PULSE_DURATION.as_secs_f32()).clamp(0.0, 1.0);
            let wash_color = match pulse.kind {
                LockPulseKind::Rejected => [0.55, 0.0, 0.0, 0.24 * fade],
                LockPulseKind::Accepted => [1.0, 0.62, 0.12, 0.18 * fade],
            };
            self.draw_rounded_rect(frame, panel, ctx.output_scale, 12.0, wash_color)?;
        }

        if self.font_atlas_texture.is_none() || self.chrome_shaders.font_text.is_none() {
            return Ok(());
        }

        let theme_id = theme.id.builtin_id().unwrap_or(BuiltInThemeId::Eagle);
        self.draw_text_cached(
            frame,
            fonts,
            "FOCALDESK LOCKED",
            panel_x + 28,
            panel_y + 48,
            style_for(FontRole::Title, 18, theme_id),
            [1.0, 0.95, 0.82, 0.96],
            ctx.output_scale,
        )?;

        let field = Rectangle::<i32, Logical>::from_loc_and_size(
            (panel_x + 28, panel_y + 72),
            (panel_w - 56, 48),
        );
        self.draw_rounded_rect(
            frame,
            field,
            ctx.output_scale,
            8.0,
            [0.02, 0.03, 0.05, 0.78],
        )?;

        let reveal_button = Rectangle::<i32, Logical>::from_loc_and_size(
            (field.loc.x + field.size.w - 82, field.loc.y + 8),
            (68, 32),
        );
        self.draw_rounded_rect(
            frame,
            reveal_button,
            ctx.output_scale,
            6.0,
            [0.08, 0.11, 0.16, 0.88],
        )?;

        let password_display = if lock.password_visible {
            lock.password_text.clone()
        } else if lock.password_len == 0 {
            String::new()
        } else {
            "*".repeat(lock.password_len.min(48))
        };
        self.draw_text_cached(
            frame,
            fonts,
            &password_display,
            field.loc.x + 18,
            field.loc.y + 31,
            style_for(FontRole::Body, 22, theme_id),
            [0.82, 0.90, 1.0, 0.94],
            ctx.output_scale,
        )?;
        self.draw_text_cached(
            frame,
            fonts,
            if lock.password_visible {
                "Hide"
            } else {
                "Show"
            },
            reveal_button.loc.x + 15,
            reveal_button.loc.y + 22,
            style_for(FontRole::Label, 14, theme_id),
            [0.86, 0.92, 1.0, 0.92],
            ctx.output_scale,
        )?;

        let message = if lock.authenticating {
            "Authenticating"
        } else {
            lock.message.as_str()
        };
        let message_color = match lock.pulse.map(|pulse| pulse.kind) {
            Some(LockPulseKind::Rejected) => [1.0, 0.22, 0.18, 0.95],
            Some(LockPulseKind::Accepted) => [0.68, 1.0, 0.48, 0.95],
            None => [0.70, 0.78, 0.92, 0.86],
        };
        self.draw_text_cached(
            frame,
            fonts,
            message,
            panel_x + 28,
            panel_y + 150,
            style_for(FontRole::Label, 15, theme_id),
            message_color,
            ctx.output_scale,
        )?;

        Ok(())
    }

    fn draw_screensaver_background(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        dst: Rectangle<i32, Physical>,
        wide_gamut: bool,
    ) -> Result<(), GlesError> {
        let program = if wide_gamut {
            self.chrome_shaders.screensaver_wide.as_ref()
        } else {
            self.chrome_shaders.screensaver.as_ref()
        };
        let Some(program) = program else {
            return Ok(());
        };

        let src = Rectangle::<f64, Buffer>::from_size(
            (f64::from(dst.size.w), f64::from(dst.size.h)).into(),
        );
        let buffer_size = Size::<i32, Buffer>::from((dst.size.w, dst.size.h));
        let damage = std::slice::from_ref(&dst);
        let elapsed = ctx.now.duration_since(self.start_time).as_secs_f32();

        frame.render_pixel_shader_to(
            program,
            src,
            dst,
            buffer_size,
            Some(damage),
            1.0,
            &[
                Uniform::new(
                    "u_resolution",
                    [dst.size.w.max(1) as f32, dst.size.h.max(1) as f32],
                ),
                Uniform::new("u_time", elapsed),
            ],
        )
    }

    fn draw_active_output_glow(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        theme: &FlowTheme,
        wide_gamut: bool,
    ) -> Result<(), GlesError> {
        if ctx.active_output != ctx.rendering_output {
            return Ok(());
        }

        let program = if wide_gamut {
            self.chrome_shaders.accent_wide.as_ref()
        } else {
            self.chrome_shaders.accent.as_ref()
        };
        let Some(program) = program else {
            return Ok(());
        };

        let output_size = Size::<i32, Physical>::from(ctx.output_size);
        let dst = Rectangle::<i32, Physical>::from_loc_and_size((0, 0), output_size);
        let src = Rectangle::<f64, Buffer>::from_size(
            (f64::from(output_size.w), f64::from(output_size.h)).into(),
        );
        // This pass runs on a retained scene target. Never modify pixels outside
        // the frame's advertised damage: the output encode/present stages update
        // only that same region, so wider draws become visible later as seemingly
        // unrelated client or pointer damage reaches those pixels.
        let damage = &ctx.damage;
        if damage.is_empty() {
            return Ok(());
        }

        let mut accent = if wide_gamut {
            scene_linear_to_display_p3(theme.chrome.accent_color)
        } else {
            theme.chrome.accent_color
        };
        accent[3] = 1.0;
        let elapsed = ctx.now.duration_since(self.start_time).as_secs_f32();

        let buffer_size = Size::<i32, Buffer>::from((output_size.w, output_size.h));

        frame.render_pixel_shader_to(
            program,
            src,
            dst,
            buffer_size,
            Some(damage),
            1.0,
            &[
                Uniform::new("u_resolution", [output_size.w as f32, output_size.h as f32]),
                Uniform::new("u_rect", [0.0f32, 0.0f32, output_size.w as f32, 44.0f32]),
                Uniform::new("u_accent", accent),
                Uniform::new("u_time", elapsed),
                Uniform::new("u_pulse", ctx.focus_pulse),
                Uniform::new("u_active", 1.0f32),
            ],
        )
    }

    pub fn render_output(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        inputs: RenderInputs<'_>,
        muts: RenderInputsMut<'_>,
    ) -> Result<(), GlesError> {
        self.render_into_frame(frame, inputs, muts)
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
        atlas: &focaldesk_ui::atlas::IconAtlas,
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
        _atlas: &focaldesk_ui::atlas::IconAtlas,
        _program: &GlesTexProgram,
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
        self.draw_rounded_rect(frame, layout.bounds, scale, 8.0, [0.05, 0.07, 0.10, 0.9])?;

        // `draw_text_cached` takes logical coords and lifts with `scale` (matches panel rects).
        let title_baseline = layout.title_rect.loc.y + layout.title_rect.size.h - 8;
        let mut y = layout.message_rect.loc.y + 20;

        flog(format!("DRAW REAL FONT TITLE: {}", dialog.title));
        self.draw_text_cached(
            frame,
            fonts,
            &dialog.title,
            layout.title_rect.loc.x,
            title_baseline,
            style_for(
                FontRole::Title,
                16,
                theme.id.builtin_id().unwrap_or(BuiltInThemeId::Eagle),
            ),
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
                style_for(
                    FontRole::Body,
                    16,
                    theme.id.builtin_id().unwrap_or(BuiltInThemeId::Eagle),
                ),
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
            self.draw_rounded_rect(frame, *rect, scale, 4.0, [0.12, 0.16, 0.20, 0.95])?;

            self.draw_text_cached(
                frame,
                fonts,
                &button.label,
                rect.loc.x + 18,
                rect.loc.y + 26,
                style_for(
                    FontRole::Label,
                    16,
                    theme.id.builtin_id().unwrap_or(BuiltInThemeId::Eagle),
                ),
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
        atlas: &focaldesk_ui::atlas::IconAtlas,
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
                let rect_logical = Rectangle::from_loc_and_size((cursor_x, y), (glyph_w, glyph_h));

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

    fn get_char(&self, ch: char) -> Option<IconId> {
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
    fn draw_work_area_glass_layer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        inputs: &RenderInputs<'_>,
        theme: &FlowTheme,
    ) -> Result<(), GlesError> {
        let wide_gamut = matches!(
            inputs.chrome_glass_pass,
            ChromeGlassPass::LinearUnderClients
        ) && self.chrome_shaders.wide_gamut_ready();
        let glass = if wide_gamut {
            self.chrome_shaders.glass_wide.as_ref()
        } else {
            self.chrome_shaders.glass.as_ref()
        };
        let Some(glass) = glass else {
            return Ok(());
        };
        let legacy_theme = chrome_theme_from_flow_theme(&theme.chrome);
        let mut style = legacy_theme.glass;
        if wide_gamut {
            style.tint = scene_linear_to_display_p3(style.tint);
            style.edge_color = scene_linear_to_display_p3(style.edge_color);
        }
        self.draw_workarea_glass(
            frame,
            inputs.ctx,
            glass,
            inputs.layout.work_area.glass,
            inputs.ctx.output_scale,
            &inputs.ctx.damage,
            &style,
        )
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
        let damage_local = clipped_dest_local_damage(rect_physical, damage);
        if damage_local.is_empty() {
            return Ok(());
        }

        let t = ctx.now.duration_since(self.start_time).as_secs_f32();

        let uniforms = [
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
        ];

        frame.render_pixel_shader_to(
            program,
            src_rect,          // Rectangle<f64, Buffer>
            dst_rect_physical, // Rectangle<i32, Physical>
            size,              // Size<i32, Buffer>
            Some(&damage_local),
            1.0, // alpha
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
        Self::draw_beveled_panel_with_radius(
            frame,
            program,
            rect_logical,
            scale,
            damage,
            style,
            0.0,
        )
    }

    fn draw_beveled_panel_with_radius(
        frame: &mut GlesFrame<'_, '_>,
        program: &GlesPixelProgram,
        rect_logical: Rectangle<i32, Logical>,
        scale: Scale<f64>,
        damage: &[Rectangle<i32, Physical>],
        style: &BevelStyle,
        corner_radius: f32,
    ) -> Result<(), GlesError> {
        let rect_physical = to_physical_rect(rect_logical, scale);
        let src_rect = Rectangle::from_loc_and_size(
            (0.0, 0.0),
            (rect_physical.size.w as f64, rect_physical.size.h as f64),
        );
        let dst_rect_physical = rect_physical;
        let size = Size::from((rect_physical.size.w, rect_physical.size.h));
        let damage_local = clipped_dest_local_damage(rect_physical, damage);

        let uniforms = [
            Uniform::new("u_bevel", style.bevel),
            Uniform::new("u_softness", style.softness),
            Uniform::new("u_glow_width", style.glow_width),
            Uniform::new("u_glow_alpha", style.glow_alpha),
            Uniform::new("u_inner_shadow", style.inner_shadow),
            Uniform::new(
                "u_corner_radius",
                corner_radius * scale.x.max(scale.y) as f32,
            ),
            Uniform::new("u_face_color", style.face_color),
            Uniform::new("u_light_color", style.light_color),
            Uniform::new("u_shadow_color", style.shadow_color),
            Uniform::new("u_glow_color", style.glow_color),
        ];

        frame.render_pixel_shader_to(
            program,
            src_rect,          // Rectangle<f64, Buffer>
            dst_rect_physical, // Rectangle<i32, Physical>
            size,              // Size<i32, Buffer>
            Some(&damage_local),
            1.0, // alpha
            &uniforms,
        )
    }

    fn draw_light_channel(
        frame: &mut GlesFrame<'_, '_>,
        program: &GlesPixelProgram,
        rect_logical: Rectangle<i32, Logical>,
        scale: Scale<f64>,
        damage: &[Rectangle<i32, Physical>],
        style: &LightChannelStyle,
    ) -> Result<(), GlesError> {
        let rect_physical = to_physical_rect(rect_logical, scale);
        let dst_rect_physical = rect_physical;

        let src_rect = Rectangle::from_loc_and_size(
            (0.0, 0.0),
            (rect_physical.size.w as f64, rect_physical.size.h as f64),
        );
        let size = Size::from((rect_physical.size.w, rect_physical.size.h));
        let damage_local = clipped_dest_local_damage(rect_physical, damage);
        if damage_local.is_empty() {
            return Ok(());
        }

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
            src_rect,          // Rectangle<f64, Buffer>
            dst_rect_physical, // Rectangle<i32, Physical>
            size,              // Size<i32, Buffer>
            Some(&damage_local),
            1.0, // alpha
            &uniforms,
        )
    }

    fn draw_sidebar_pulse(
        frame: &mut GlesFrame<'_, '_>,
        program: &GlesPixelProgram,
        rect_logical: Rectangle<i32, Logical>,
        click_local: Point<f64, Logical>,
        elapsed: Duration,
        scale: Scale<f64>,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        Self::draw_pulse(
            frame,
            program,
            rect_logical,
            click_local,
            elapsed,
            scale,
            damage,
            [0.0, 0.5, 1.0, 1.0],
        )
    }

    fn draw_pulse(
        frame: &mut GlesFrame<'_, '_>,
        program: &GlesPixelProgram,
        rect_logical: Rectangle<i32, Logical>,
        click_local: Point<f64, Logical>,
        elapsed: Duration,
        scale: Scale<f64>,
        damage: &[Rectangle<i32, Physical>],
        color: [f32; 4],
    ) -> Result<(), GlesError> {
        let rect_physical = to_physical_rect(rect_logical, scale);
        let src_rect = Rectangle::from_loc_and_size(
            (0.0, 0.0),
            (rect_physical.size.w as f64, rect_physical.size.h as f64),
        );
        let size = Size::from((rect_physical.size.w, rect_physical.size.h));

        let click_x = ((click_local.x - f64::from(rect_logical.loc.x)) * scale.x)
            .clamp(0.0, f64::from(rect_physical.size.w)) as f32;
        let click_y = ((click_local.y - f64::from(rect_logical.loc.y)) * scale.y)
            .clamp(0.0, f64::from(rect_physical.size.h)) as f32;

        let uniforms = [
            Uniform::new("u_click_pos", [click_x, click_y]),
            Uniform::new("u_time", elapsed.as_secs_f32()),
            Uniform::new(
                "u_size",
                [rect_physical.size.w as f32, rect_physical.size.h as f32],
            ),
            Uniform::new("u_color", color),
        ];

        frame.render_pixel_shader_to(
            program,
            src_rect,
            rect_physical,
            size,
            Some(damage),
            1.0,
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

    fn expand_rect(rect: Rectangle<i32, Physical>, px: i32) -> Rectangle<i32, Physical> {
        Rectangle::from_loc_and_size(
            (rect.loc.x - px, rect.loc.y - px),
            (rect.size.w + px * 2, rect.size.h + px * 2),
        )
    }

    fn inset_rect_xy(rect: Rectangle<i32, Physical>, px: i32, py: i32) -> Rectangle<i32, Physical> {
        Rectangle::from_loc_and_size(
            (rect.loc.x + px, rect.loc.y + py),
            ((rect.size.w - px * 2).max(1), (rect.size.h - py * 2).max(1)),
        )
    }

    pub fn draw_debug_rect(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        _ctx: &FrameCtx,
        _output: &OutputState,
    ) {
        // Debug marker where window should start
        let marker: Rectangle<i32, Physical> = Rectangle::new((64, 36).into(), (20, 20).into());

        frame
            .clear(Color32F::new(1.0, 0.0, 0.0, 1.0), &[marker])
            .unwrap();
    }

    pub fn draw_background(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        _output: &OutputState,
        bg: BackgroundTheme,
    ) {
        let c = bg.color;
        let color = Color32F::new(c[0], c[1], c[2], c[3]);
        for rect in &ctx.damage {
            frame.clear(color, std::slice::from_ref(rect)).unwrap();
        }
    }

    pub fn draw_wallpaper_in_rect(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        target_logical: Rectangle<i32, Logical>,
        scale: Scale<f64>,
        theme: WallpaperTheme,
        client_compositing: &ClientCompositingMode,
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

        let dst_world: Rectangle<i32, Physical> = Rectangle::new(
            (blit.dst.x, blit.dst.y).into(),
            (blit.dst.w, blit.dst.h).into(),
        );

        //let dst =
        //    RenderState::rect_apply_flipped180(dst_world, (ctx.output_size.0, ctx.output_size.1));
        let dst = dst_world;

        let damage_local = clipped_dest_local_damage(dst, &ctx.damage);
        if damage_local.is_empty() {
            return;
        }

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

        let wide_gamut =
            client_compositing.ui_textures_linear() && self.chrome_shaders.wide_gamut_ready();
        let tint = if wide_gamut {
            scene_linear_to_display_p3(theme.tint_color)
        } else {
            theme.tint_color
        };
        let uniforms = [
            Uniform::new("u_tint", tint),
            Uniform::new(
                "u_decode_srgb",
                if client_compositing.ui_textures_linear() {
                    1.0f32
                } else {
                    0.0f32
                },
            ),
        ];

        let wallpaper_program = if wide_gamut {
            self.chrome_shaders.wallpaper_tint_wide.as_ref()
        } else {
            self.chrome_shaders.wallpaper_tint.as_ref()
        };

        frame
            .render_texture_from_to(
                tex,
                src_rect,
                dst,
                &damage_local,
                &[],
                Transform::Normal,
                1.0,
                wallpaper_program,
                &uniforms,
            )
            .unwrap();
    }

    /// Overlay the bundled wallpaper's wide-gamut accents and conservative
    /// HDR10 highlight lifts in the FP16 scene. The ordinary SDR wallpaper
    /// remains the visual base, so this pass is a no-op on an sRGB SDR output
    /// and never raises its blacks.
    pub fn draw_wallpaper_creative_grade(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        target_logical: Rectangle<i32, Logical>,
        scale: Scale<f64>,
        theme: WallpaperTheme,
        grade_mode: f32,
        reference_white_nits: f32,
        peak_nits: f32,
    ) {
        use crate::core::wallpaper::{compute_wallpaper_blit, RectI, SizeI, WallpaperMode};
        use smithay::backend::renderer::gles::Uniform;

        let Some(texture) = self.wallpaper_texture.as_ref() else {
            return;
        };
        let Some(program) = self.chrome_shaders.wallpaper_creative_grade.as_ref() else {
            return;
        };

        if grade_mode < 0.5 {
            return;
        }

        let target_physical = to_physical_rect(target_logical, scale);
        let source_size = texture.size();
        let Some(blit) = compute_wallpaper_blit(
            SizeI {
                w: source_size.w,
                h: source_size.h,
            },
            RectI {
                x: target_physical.loc.x,
                y: target_physical.loc.y,
                w: target_physical.size.w,
                h: target_physical.size.h,
            },
            WallpaperMode::Fill,
        ) else {
            return;
        };

        let destination: Rectangle<i32, Physical> = Rectangle::new(
            (blit.dst.x, blit.dst.y).into(),
            (blit.dst.w, blit.dst.h).into(),
        );
        let damage = clipped_dest_local_damage(destination, &ctx.damage);
        if damage.is_empty() {
            return;
        }

        let width = source_size.w as f64;
        let height = source_size.h as f64;
        let source: Rectangle<f64, Buffer> = Rectangle::new(
            (blit.uv.u0 as f64 * width, blit.uv.v0 as f64 * height).into(),
            (
                (blit.uv.u1 - blit.uv.u0) as f64 * width,
                (blit.uv.v1 - blit.uv.v0) as f64 * height,
            )
                .into(),
        );
        let uniforms = [
            Uniform::new("u_tint", theme.tint_color),
            Uniform::new(
                "u_texel_size",
                [
                    1.0f32 / source_size.w.max(1) as f32,
                    1.0f32 / source_size.h.max(1) as f32,
                ],
            ),
            Uniform::new("u_grade_mode", grade_mode),
            Uniform::new("u_reference_white_nits", reference_white_nits),
            Uniform::new("u_peak_nits", peak_nits),
        ];

        if let Err(err) = frame.render_texture_from_to(
            texture,
            source,
            destination,
            &damage,
            &[],
            Transform::Normal,
            1.0,
            Some(program),
            &uniforms,
        ) {
            flog_error!("wallpaper creative grade draw failed: {err}");
        }
    }

    /// Build client render elements for one output using smithay's Space region logic.
    ///
    /// Must run while the GLES renderer is current (during an active `GlesFrame`), so dmabuf
    /// textures are imported at draw time rather than during Wayland dispatch.
    pub fn build_client_elements_for_output(
        &mut self,
        space: &Space<Window>,
        windows: &[ManagedWindow],
        active_workspace: WorkspaceId,
        output: &Output,
        layers_on: Option<&Output>,
        renderer: &mut GlesRenderer,
    ) -> Vec<FlowRenderElement> {
        use smithay::backend::renderer::element::AsRenderElements;
        use smithay::utils::{Logical, Point, Rectangle, Scale};

        let Some(region) = space.output_geometry(output) else {
            flog("build_client_elements_for_output: output not mapped in Space");
            return Vec::new();
        };

        let scale = output.current_scale().fractional_scale();
        let mut out = Vec::new();

        let on_workspace: std::collections::HashSet<_> = windows
            .iter()
            .filter(|mw| mw.mapped && mw.workspace == active_workspace)
            .map(|mw| &mw.window)
            .collect();

        // `Space::elements()` is back-to-front. `draw_render_elements` expects
        // front-to-back input so opaque-region culling cannot hide top windows
        // behind older mapped windows.
        for window in space.elements().rev() {
            if !on_workspace.contains(window) {
                continue;
            }

            let popup_bbox = space.element_location(window).map(|element_loc| {
                let mut bbox = window.bbox_with_popups();
                bbox.loc += element_loc - window.geometry().loc;
                bbox
            });
            let bbox = match (space.element_bbox(window), popup_bbox) {
                (Some(space_bbox), Some(popup_bbox)) => space_bbox.merge(popup_bbox),
                (Some(space_bbox), None) => space_bbox,
                (None, Some(popup_bbox)) => popup_bbox,
                (None, None) => continue,
            };
            if !region.overlaps(bbox) {
                continue;
            }

            window.on_commit();

            let render_loc = space
                .element_location(window)
                .map(|element_loc| {
                    let geo = window.geometry();
                    Point::<i32, Logical>::from((
                        element_loc.x - geo.loc.x,
                        element_loc.y - geo.loc.y,
                    ))
                })
                .unwrap_or(Point::from((0, 0)));

            let location = render_loc - region.loc;
            let render_pos = location.to_physical_precise_round(scale);

            // Render the window surface tree here, but keep xdg popups out of this pass.
            // Popups are composited separately below so they can sit above the compositor chrome
            // without being drawn twice.
            let Some(surface) = window.wl_surface() else {
                continue;
            };
            let elems = render_elements_from_surface_tree::<_, FlowRenderElement>(
                renderer,
                &surface,
                render_pos,
                Scale::from(scale),
                1.0,
                Kind::Unspecified,
            );

            #[cfg(feature = "xwayland")]
            if window.x11_surface().is_some() {
                if let Some(managed) = windows.iter().find(|mw| &mw.window == window) {
                    let seq = XWAYLAND_RENDER_LOGS.fetch_add(1, Ordering::Relaxed);
                    if seq < 300 {
                        flog(&format!(
                            "XWayland render id={:?} title={:?} region={:?} render_pos={:?} elems={}",
                            managed.id,
                            managed.title(),
                            region,
                            render_pos,
                            elems.len()
                        ));
                    }
                }
            }

            out.extend(elems);
        }

        if let Some(out_handle) = layers_on {
            crate::core::portal::push_layer_elements_for_output(
                renderer,
                out_handle,
                region.size,
                Scale::from(scale),
                &mut out,
            );
        }

        out
    }

    /// Build xdg popup elements for a top overlay pass.
    ///
    /// `Window::render_elements` includes popups in the client layer, but FocalDesk draws shell
    /// chrome after that layer. Menus need a second top pass so browser/app popups are not covered
    /// by compositor trim and icons.
    pub fn build_popup_elements_for_output(
        &mut self,
        space: &Space<Window>,
        windows: &[ManagedWindow],
        active_workspace: WorkspaceId,
        output: &Output,
        renderer: &mut GlesRenderer,
    ) -> Vec<FlowRenderElement> {
        let Some(region) = space.output_geometry(output) else {
            return Vec::new();
        };

        let scale = output.current_scale().fractional_scale();
        let mut out = Vec::new();

        let on_workspace: std::collections::HashSet<_> = windows
            .iter()
            .filter(|mw| mw.mapped && mw.workspace == active_workspace)
            .map(|mw| &mw.window)
            .collect();

        for window in space.elements().rev() {
            if !on_workspace.contains(window) {
                continue;
            }

            let Some(window_loc) = space.element_location(window) else {
                continue;
            };
            let Some(surface) = window.wl_surface() else {
                continue;
            };

            for (popup, popup_offset) in PopupManager::popups_for_surface(&surface) {
                let geo = window.geometry();

                let popup_loc = window_loc - geo.loc + popup_offset - popup.geometry().loc;

                let mut popup_bbox = popup.geometry();
                popup_bbox.loc += popup_loc;

                if !region.overlaps(popup_bbox) {
                    continue;
                }

                let output_loc = popup_loc - region.loc;
                let render_pos = output_loc.to_physical_precise_round(scale);

                out.extend(render_elements_from_surface_tree::<_, FlowRenderElement>(
                    renderer,
                    popup.wl_surface(),
                    render_pos,
                    Scale::from(scale),
                    1.0,
                    Kind::Unspecified,
                ));
            }
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
        wide_gamut: bool,
    ) {
        if ctx.active_output != ctx.rendering_output {
            return;
        }

        let program = if wide_gamut && self.chrome_shaders.wide_gamut_ready() {
            self.chrome_shaders
                .amber_lightbar_wide
                .as_ref()
                .or(self.chrome_shaders.amber_lightbar.as_ref())
        } else {
            self.chrome_shaders.amber_lightbar.as_ref()
        };
        let Some(program) = program else {
            return;
        };

        let bar_rect_logical =
            Rectangle::from_loc_and_size(layout.topbar.outer.loc, (layout.topbar.outer.size.w, 10));

        let _ = Self::draw_amber_lightbar(
            frame,
            program,
            bar_rect_logical,
            ctx.output_scale,
            &ctx.damage,
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
        let damage_local = clipped_dest_local_damage(rect_physical, damage);
        if damage_local.is_empty() {
            return Ok(());
        }

        frame.render_pixel_shader_to(
            program,
            src_rect,
            rect_physical,
            size,
            Some(&damage_local),
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
        elements: &[FlowRenderElement],
        client_compositing: &ClientCompositingMode,
        surface_colors: &std::collections::HashMap<Id, SurfaceColorRenderState>,
    ) {
        use smithay::backend::renderer::element::Element;
        use smithay::backend::renderer::gles::Uniform;

        let damage = &ctx.damage;

        let ClientCompositingMode::Linear {
            client_to_scene, ..
        } = client_compositing
        else {
            if let ClientCompositingMode::LinearUi { srgb_to_linear } = client_compositing {
                frame.override_default_tex_program(srgb_to_linear.clone(), vec![]);
                draw_render_elements(frame, ctx.output_scale.x, elements, damage).unwrap();
                frame.clear_tex_program_override();
            } else {
                draw_render_elements(frame, ctx.output_scale.x, elements, damage).unwrap();
            }
            return;
        };

        let runs = contiguous_runs_by_key(elements, |elem| {
            surface_colors
                .get(elem.id())
                .copied()
                .unwrap_or_else(SurfaceColorRenderState::srgb_default)
        });
        for (color, run) in runs.into_iter().rev() {
            let m = color.client_to_scene;
            let uniforms = vec![
                Uniform::new(
                    "u_decode_tf",
                    color.description.transfer.decode_mode() as u32 as f32,
                ),
                Uniform::new(
                    "u_reference_white_nits",
                    color.description.reference_white_nits.max(1.0),
                ),
                Uniform::new(
                    "u_linear_to_scene_scale",
                    color.description.linear_to_scene_scale(),
                ),
                Uniform::new("u_m0", [m[0][0], m[0][1], m[0][2]]),
                Uniform::new("u_m1", [m[1][0], m[1][1], m[1][2]]),
                Uniform::new("u_m2", [m[2][0], m[2][1], m[2][2]]),
                Uniform::new("u_src_bits", color.pq_src_bits),
            ];
            frame.override_default_tex_program(client_to_scene.clone(), uniforms);
            let result = draw_render_elements(frame, ctx.output_scale.x, run, damage);
            frame.clear_tex_program_override();
            result.unwrap();
        }
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
        sidebar_pulse: Option<SidebarPulseFrame>,
        topbar_pulse: Option<TopbarPulseFrame>,
        clock_pulse: Option<ClockPulseFrame>,
        theme: focaldesk_themes::ChromeTheme,
    ) {
        let legacy_theme = chrome_theme_from_flow_theme(&theme);

        let beveled = self
            .chrome_shaders
            .beveled_panel
            .clone()
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

        let pulse = self.chrome_shaders.pulse.as_ref();

        let damage = &ctx.damage;

        //
        // 1. STRUCTURAL SHELL
        //

        let _ = Self::draw_top_bar(
            frame,
            top_bar,
            layout.topbar.outer,
            ctx.output_scale,
            damage,
            &legacy_theme.top_bar,
        );

        let _ = Self::draw_beveled_panel(
            frame,
            &beveled,
            layout.topbar.outer,
            ctx.output_scale,
            damage,
            &legacy_theme.frame_outer,
        );

        let _ = Self::draw_beveled_panel(
            frame,
            &beveled,
            layout.topbar.inner,
            ctx.output_scale,
            damage,
            &legacy_theme.frame_inner,
        );

        let _ = Self::draw_beveled_panel_with_radius(
            frame,
            &beveled,
            layout.sidebar.outer,
            ctx.output_scale,
            damage,
            &legacy_theme.sidebar,
            SIDEBAR_CORNER_RADIUS,
        );

        let _ = Self::draw_beveled_panel_with_radius(
            frame,
            &beveled,
            layout.sidebar.inner,
            ctx.output_scale,
            damage,
            &legacy_theme.panel_inner,
            (SIDEBAR_CORNER_RADIUS - 4.0).max(0.0),
        );

        let _ = Self::draw_beveled_panel(
            frame,
            &beveled,
            layout.work_area.outer,
            ctx.output_scale,
            damage,
            &legacy_theme.frame_outer,
        );

        let _ = Self::draw_beveled_panel(
            frame,
            &beveled,
            layout.work_area.inner_frame,
            ctx.output_scale,
            damage,
            &legacy_theme.frame_inner,
        );

        let _ = Self::draw_beveled_panel(
            frame,
            &beveled,
            layout.work_area.recess,
            ctx.output_scale,
            damage,
            &legacy_theme.panel_inner,
        );

        //
        // 2. TOP BAR DETAILS
        //

        let _ = Self::draw_beveled_panel(
            frame,
            &beveled,
            layout.topbar.title,
            ctx.output_scale,
            damage,
            &legacy_theme.panel_inner,
        );

        let ai_button = layout.topbar.ai_button;
        Self::draw_recessed_button(
            frame,
            button,
            ai_button,
            ctx.output_scale,
            damage,
            &legacy_theme.button,
        );

        if let (Some(pulse_shader), Some(pulse_frame)) = (pulse, topbar_pulse) {
            if pulse_frame.target == TopbarPulseTarget::AiButton {
                let _ = Self::draw_sidebar_pulse(
                    frame,
                    pulse_shader,
                    ai_button,
                    pulse_frame.click_local,
                    pulse_frame.elapsed,
                    ctx.output_scale,
                    damage,
                );
            }
        }

        let _ = Self::draw_beveled_panel(
            frame,
            &beveled,
            layout.topbar.trim,
            ctx.output_scale,
            damage,
            &legacy_theme.trim,
        );

        if let Some(rect) = layout.topbar.light {
            let _ = Self::draw_light_channel(
                frame,
                light,
                rect,
                ctx.output_scale,
                damage,
                &legacy_theme.light,
            );
        }

        for (i, rect) in layout.topbar.status_wells.iter().enumerate() {
            Self::draw_recessed_button(
                frame,
                button,
                *rect,
                ctx.output_scale,
                damage,
                &legacy_theme.button,
            );

            let _ = Self::draw_light_channel(
                frame,
                light,
                inset_rect(*rect, 3),
                ctx.output_scale,
                damage,
                &legacy_theme.light,
            );

            if let (Some(pulse_shader), Some(pulse_frame)) = (pulse, topbar_pulse) {
                if pulse_frame.target == TopbarPulseTarget::Indicator(i) {
                    let _ = Self::draw_sidebar_pulse(
                        frame,
                        pulse_shader,
                        *rect,
                        pulse_frame.click_local,
                        pulse_frame.elapsed,
                        ctx.output_scale,
                        damage,
                    );
                }
            }
        }

        Self::draw_recessed_button(
            frame,
            button,
            layout.topbar.clock_well,
            ctx.output_scale,
            damage,
            &legacy_theme.button,
        );

        let _ = Self::draw_light_channel(
            frame,
            light,
            inset_rect(layout.topbar.clock_well, 3),
            ctx.output_scale,
            damage,
            &legacy_theme.light,
        );

        if let (Some(pulse_shader), Some(pulse_frame)) = (pulse, clock_pulse) {
            let _ = Self::draw_sidebar_pulse(
                frame,
                pulse_shader,
                layout.topbar.clock_well,
                pulse_frame.click_local,
                pulse_frame.elapsed,
                ctx.output_scale,
                damage,
            );
        }

        //
        // 3. SIDEBAR MODULESFtopbar
        //

        for (i, slot) in layout.sidebar.slots.iter().enumerate() {
            let outer = slot.outer;
            let inner = slot.inner;
            let well = slot.icon_well;

            let hovered = sidebar_hover_slot == Some(i);

            let _ = Self::draw_beveled_panel(
                frame,
                &beveled,
                outer,
                ctx.output_scale,
                damage,
                &legacy_theme.module,
            );
            let _ = Self::draw_beveled_panel(
                frame,
                &beveled,
                inner,
                ctx.output_scale,
                damage,
                &legacy_theme.module_inner,
            );
            Self::draw_recessed_button(
                frame,
                button,
                well,
                ctx.output_scale,
                damage,
                &legacy_theme.button,
            );

            //if hovered {
            let hover = if hovered { 1.0 } else { 0.0 };

            let glow_rect = inset_rect(well, 3);

            let mut light_style = legacy_theme.light;

            // baseline glow
            light_style.glow_color[3] = 0.08 + hover * 0.55;
            light_style.core_color[3] = 0.18 + hover * 0.55;

            // hover boost
            light_style.glow_radius = 8.0 + hover * 6.0;
            light_style.core_inset = 3.0 - hover * 0.75;

            let _ = Self::draw_light_channel(
                frame,
                light,
                glow_rect,
                ctx.output_scale,
                damage,
                &light_style,
            );

            if let (Some(pulse_shader), Some(pulse_frame)) = (pulse, sidebar_pulse) {
                if pulse_frame.slot == i {
                    let _ = Self::draw_sidebar_pulse(
                        frame,
                        pulse_shader,
                        outer,
                        pulse_frame.click_local,
                        pulse_frame.elapsed,
                        ctx.output_scale,
                        damage,
                    );
                }
            }

            //let glow_rect = inset_rect(*well, 3);
            //let _ = Self::draw_light_channel(frame, light, glow_rect, damage, &legacy_theme.light);
            // }
        }

        if let Some(rect) = layout.sidebar.light {
            let _ = Self::draw_light_channel(
                frame,
                light,
                rect,
                ctx.output_scale,
                damage,
                &legacy_theme.light,
            );
        }

        for rect in &layout.sidebar.caps {
            let _ = Self::draw_beveled_panel(
                frame,
                &beveled,
                *rect,
                ctx.output_scale,
                damage,
                &legacy_theme.corner_cap,
            );
        }

        //
        // 4. DECORATIVE CAPS / JOINTS
        //

        for rect in &layout.decoration.corner_caps {
            let _ = Self::draw_beveled_panel(
                frame,
                &beveled,
                *rect,
                ctx.output_scale,
                damage,
                &legacy_theme.corner_cap,
            );
        }

        for rect in &layout.decoration.corner_joint_caps {
            let _ = Self::draw_beveled_panel(
                frame,
                &beveled,
                *rect,
                ctx.output_scale,
                damage,
                &legacy_theme.corner_cap,
            );
        }
    }

    /// Work trim and chrome icons — drawn above client surfaces (glass is under clients).
    fn draw_chrome_trim_glass_icons(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        ctx: &FrameCtx,
        layout: &ChromeLayoutLogical,
        _output: &OutputState,
        metrics: &ChromeMetrics,
        //ui: &mut UiState<GlesTexture>,
        ui_state: &mut UiState<GlesTexture>,
        ui_focus: Option<ElementId>,
        desktop_output: &focaldesk_ui::desktop_output::DesktopOutput,
        current_workspace: WorkspaceId,
        fonts: &FontSystem,
        notification_unread_count: usize,
        update_available_count: usize,
        theme: &FlowTheme,
        linear_target: bool,
    ) {
        let wide_gamut = linear_target && self.chrome_shaders.wide_gamut_ready();
        let legacy_theme = chrome_theme_from_flow_theme(&theme.chrome);

        let beveled = if wide_gamut {
            self.chrome_shaders
                .beveled_panel_wide
                .clone()
                .or_else(|| self.chrome_shaders.beveled_panel.clone())
        } else {
            self.chrome_shaders.beveled_panel.clone()
        }
        .expect("beveled_panel shader not compiled");

        let fullscreen_rect: Rectangle<i32, Physical> = Rectangle::from_loc_and_size(
            Point::<i32, Physical>::from((0, 0)),
            Size::<i32, Physical>::from(ctx.output_size),
        );
        let damage = &ctx.damage;
        let damage_intersects = |rect: Rectangle<i32, Logical>| {
            let rect = rect.to_physical_precise_round(ctx.output_scale);
            damage.iter().any(|damaged| damaged.overlaps(rect))
        };

        if let Some(rect) = layout
            .work_area
            .trim
            .filter(|rect| damage_intersects(*rect))
        {
            let trim_style = if wide_gamut {
                bevel_style_to_display_p3(legacy_theme.trim)
            } else {
                legacy_theme.trim
            };
            let _ = Self::draw_beveled_panel(
                frame,
                &beveled,
                rect,
                ctx.output_scale,
                damage,
                &trim_style,
            );
        }

        let lightbar_rect =
            Rectangle::from_loc_and_size(layout.topbar.outer.loc, (layout.topbar.outer.size.w, 10));
        if damage_intersects(lightbar_rect) {
            self.draw_active_lightbar(frame, ctx, layout, wide_gamut);
        }

        if let Some(atlas) = ui_state.chrome.atlas.as_ref() {
            let icon_px = metrics.icon_base_px as i32;

            let _text = format!(
                "FOCALDESK · OUT {} · WS {}",
                ctx.rendering_output.0, current_workspace.0
            );

            let _label_rect_logical = title_label_rect(layout.topbar.title);

            let _is_active = ctx.rendering_output == ctx.active_output; // or output.output_id

            //let _ = RenderState::draw_title_text(frame, atlas, _label_rect_logical, &text, is_active, ctx.output_scale, tinted_icon);

            let output_number = ctx.rendering_output.0;
            let workspace_number = current_workspace.0 as usize;
            let active_theme = theme;

            if damage_intersects(layout.topbar.title) {
                let _ = self.draw_topbar_identity(
                    frame,
                    fonts,
                    layout,
                    "FOCALDESK",
                    output_number,
                    workspace_number,
                    active_theme,
                    ctx.output_scale,
                );
            }

            let tinted_icon = if wide_gamut {
                self.chrome_shaders
                    .tinted_icon_wide
                    .clone()
                    .or_else(|| self.chrome_shaders.tinted_icon.clone())
            } else {
                self.chrome_shaders.tinted_icon.clone()
            }
            .expect("glass shader not compiled");

            for el in desktop_output.chrome_elements() {
                if !el.visible {
                    continue;
                }

                let element_rect = Rectangle::<i32, Logical>::from_loc_and_size(
                    (el.bounds.x, el.bounds.y),
                    (el.bounds.w, el.bounds.h),
                );
                if !damage_intersects(element_rect) {
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

                let base_rect_logical = element_rect;

                // center-based scaling

                let icon_state = if el.active {
                    IconState::Active
                } else if el.selected {
                    IconState::Active
                } else if el.hovered {
                    IconState::Hover
                } else if !el.enabled {
                    IconState::Inactive
                } else {
                    IconState::Inactive
                };

                let state = el.visual_state();
                let mut style = themed_icon_style(active_theme, state);
                if wide_gamut {
                    style.tint = scene_linear_to_display_p3(style.tint);
                }

                let is_active_output =
                    ctx.rendering_output == ctx.active_output || ctx.portal_capture;
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
                            let mut icon_rect_logical =
                                icon_rect_in_module(base_rect_logical, icon_px);
                            if scale != 1.0 {
                                let cx_logical =
                                    icon_rect_logical.loc.x + icon_rect_logical.size.w / 2;
                                let cy_logical =
                                    icon_rect_logical.loc.y + icon_rect_logical.size.h / 2;

                                let new_w_logical =
                                    ((icon_rect_logical.size.w as f32) * scale).round() as i32;
                                let new_h_logical =
                                    ((icon_rect_logical.size.h as f32) * scale).round() as i32;

                                icon_rect_logical = Rectangle::from_loc_and_size(
                                    (
                                        cx_logical - new_w_logical / 2,
                                        cy_logical - new_h_logical / 2,
                                    ),
                                    (new_w_logical, new_h_logical),
                                );
                            }
                            let control_rect_logical = inset_rect(base_rect_logical, 3);
                            if el.selected || el.active {
                                let mut selected_style =
                                    selected_sidebar_style(active_theme, el.hovered || el.active);
                                if wide_gamut {
                                    selected_style = bevel_style_to_display_p3(selected_style);
                                }
                                let _ = Self::draw_beveled_panel(
                                    frame,
                                    &beveled,
                                    control_rect_logical,
                                    ctx.output_scale,
                                    damage,
                                    &selected_style,
                                );
                            }
                            Self::draw_icon_in_rect(
                                frame,
                                atlas,
                                icon_id,
                                icon_state,
                                icon_rect_logical,
                                ctx.output_scale,
                                style,
                                &tinted_icon,
                            );
                            if el.id == focaldesk_ui::ui_builder::TOPBAR_NOTIFICATIONS_ID
                                && notification_unread_count > 0
                            {
                                let count = notification_unread_count.min(99).to_string();
                                let badge = Rectangle::from_loc_and_size(
                                    (
                                        base_rect_logical.loc.x + base_rect_logical.size.w - 16,
                                        base_rect_logical.loc.y + 4,
                                    ),
                                    (18, 14),
                                );
                                let _ = self.draw_rounded_rect(
                                    frame,
                                    badge,
                                    ctx.output_scale,
                                    6.0,
                                    active_theme.chrome.accent_color,
                                );
                                let badge_style = style_for(
                                    FontRole::Meta,
                                    11,
                                    active_theme
                                        .id
                                        .builtin_id()
                                        .unwrap_or(BuiltInThemeId::Eagle),
                                );
                                let _ = self.draw_text_cached(
                                    frame,
                                    fonts,
                                    &count,
                                    badge.loc.x + 5,
                                    badge.loc.y + 11,
                                    badge_style,
                                    active_theme.text.title,
                                    ctx.output_scale,
                                );
                            }
                        }
                    }

                    UiElementKind::TopbarIndicator | UiElementKind::TopbarButton => {
                        if let Some(icon_id) = el.icon {
                            let icon_rect_logical = well_icon_rect(base_rect_logical);
                            Self::draw_icon_in_rect(
                                frame,
                                atlas,
                                icon_id,
                                icon_state,
                                icon_rect_logical,
                                ctx.output_scale,
                                style,
                                &tinted_icon,
                            );
                            let badge_count =
                                if el.id == focaldesk_ui::ui_builder::TOPBAR_NOTIFICATIONS_ID {
                                    notification_unread_count
                                } else if el.id == focaldesk_ui::ui_builder::TOPBAR_UPDATES_ID {
                                    update_available_count
                                } else {
                                    0
                                };
                            if badge_count > 0 {
                                let count = badge_count.min(99).to_string();
                                let badge = Rectangle::from_loc_and_size(
                                    (
                                        base_rect_logical.loc.x + base_rect_logical.size.w - 16,
                                        base_rect_logical.loc.y + 4,
                                    ),
                                    (18, 14),
                                );
                                let _ = self.draw_rounded_rect(
                                    frame,
                                    badge,
                                    ctx.output_scale,
                                    6.0,
                                    active_theme.chrome.accent_color,
                                );
                                let badge_style = style_for(
                                    FontRole::Meta,
                                    11,
                                    active_theme
                                        .id
                                        .builtin_id()
                                        .unwrap_or(BuiltInThemeId::Eagle),
                                );
                                let _ = self.draw_text_cached(
                                    frame,
                                    fonts,
                                    &count,
                                    badge.loc.x + 5,
                                    badge.loc.y + 11,
                                    badge_style,
                                    active_theme.text.title,
                                    ctx.output_scale,
                                );
                            }
                        }
                    }

                    UiElementKind::TopbarFlowField => {
                        let mode = if !el.enabled {
                            4
                        } else if el.active && el.selected {
                            3
                        } else if el.active {
                            2
                        } else if el.selected {
                            1
                        } else {
                            0
                        };

                        let energy = if !el.enabled {
                            0.96
                        } else if el.active && el.selected {
                            0.96
                        } else if el.active {
                            0.88
                        } else if el.selected {
                            0.94
                        } else {
                            0.40
                        };

                        let accent = if wide_gamut {
                            scene_linear_to_display_p3(active_theme.chrome.accent_color)
                        } else {
                            active_theme.chrome.accent_color
                        };
                        let color = match mode {
                            1 => [accent[0], accent[1], accent[2], 0.98],
                            2 => [0.94, 0.97, 1.00, 0.92],
                            3 => [1.00, 0.72, 0.18, 1.00],
                            4 => [0.98, 0.30, 0.30, 1.00],
                            _ => [accent[0] * 0.72, accent[1] * 0.90, accent[2], 0.70],
                        };
                        let flow_program = if wide_gamut {
                            self.chrome_shaders
                                .flow_field_wide
                                .as_ref()
                                .or(self.chrome_shaders.flow_field.as_ref())
                        } else {
                            self.chrome_shaders.flow_field.as_ref()
                        };
                        if let Some(program) = flow_program {
                            let _ = draw_flow_field(
                                frame,
                                program,
                                base_rect_logical,
                                ctx.output_scale,
                                damage,
                                mode,
                                energy,
                                color,
                            );
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
                            active_theme
                                .id
                                .builtin_id()
                                .unwrap_or(BuiltInThemeId::Eagle),
                        );

                        let _ = self.draw_clock_font_text(
                            frame,
                            fonts,
                            &time_str,
                            clock_rect_logical,
                            ctx.output_scale,
                            clock_style,
                            active_theme.text.clock,
                        );
                    }

                    _ => {}
                }

                if ui_focus == Some(el.id) && ctx.rendering_output == ctx.active_output {
                    let focus_rect: Rectangle<i32, Physical> =
                        base_rect_logical.to_physical_precise_round(ctx.output_scale);
                    let thickness =
                        ((2.0 * ctx.output_scale.x.max(ctx.output_scale.y)).round() as i32).max(2);
                    let horizontal_width = focus_rect.size.w.max(1);
                    let vertical_height = (focus_rect.size.h - thickness * 2).max(1);
                    let regions = [
                        Rectangle::from_loc_and_size(focus_rect.loc, (horizontal_width, thickness)),
                        Rectangle::from_loc_and_size(
                            (
                                focus_rect.loc.x,
                                focus_rect.loc.y + focus_rect.size.h - thickness,
                            ),
                            (horizontal_width, thickness),
                        ),
                        Rectangle::from_loc_and_size(
                            (focus_rect.loc.x, focus_rect.loc.y + thickness),
                            (thickness, vertical_height),
                        ),
                        Rectangle::from_loc_and_size(
                            (
                                focus_rect.loc.x + focus_rect.size.w - thickness,
                                focus_rect.loc.y + thickness,
                            ),
                            (thickness, vertical_height),
                        ),
                    ];
                    let _ = Self::draw_solid_rect(
                        frame,
                        fullscreen_rect,
                        &regions,
                        [1.0, 0.82, 0.12, 1.0],
                    );
                }
            }

            let output_logical_size = Size::<i32, Logical>::from((
                (ctx.output_size.0 as f64 / ctx.output_scale.x).round() as i32,
                (ctx.output_size.1 as f64 / ctx.output_scale.y).round() as i32,
            ));

            let _ = self.draw_hover_tooltip(
                frame,
                fonts,
                desktop_output,
                output_logical_size,
                active_theme,
                ctx.output_scale,
            );

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
            self.draw_title_text(frame, atlas, label_rect, "FOCALDESK");

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
        (
            title_rect_logical.loc.x + pad_x,
            title_rect_logical.loc.y + pad_y,
        ),
        (
            title_rect_logical.size.w - pad_x * 2, // 👈 FULL WIDTH
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
        (
            (well_logical.size.w - 16).max(1),
            (well_logical.size.h - 10).max(1),
        ),
    )
}

#[inline]
pub fn inset_rect(r: Rectangle<i32, Logical>, px: i32) -> Rectangle<i32, Logical> {
    Rectangle::from_loc_and_size(
        (r.loc.x + px, r.loc.y + px),
        ((r.size.w - px * 2).max(1), (r.size.h - px * 2).max(1)),
    )
}
#[inline]
fn center_rect_in(outer: Rectangle<i32, Logical>, w: i32, h: i32) -> Rectangle<i32, Logical> {
    let x = outer.loc.x + ((outer.size.w - w).max(0) / 2);
    let y = outer.loc.y + ((outer.size.h - h).max(0) / 2);
    Rectangle::from_loc_and_size((x, y), (w, h))
}

#[inline]
fn icon_rect_in_module(module: Rectangle<i32, Logical>, icon_px: i32) -> Rectangle<i32, Logical> {
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
    pub frame_outer: BevelStyle, // frame outer
    pub frame_inner: BevelStyle, // frame inner

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
            light_color: [0.165, 0.215, 0.305, 1.0],
            shadow_color: [0.006, 0.010, 0.018, 1.0],
            glow_color: [0.0, 0.0, 0.0, 1.0],
        },

        frame_inner: BevelStyle {
            bevel: 3.0,
            softness: 1.2,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 2.2,
            face_color: [0.050, 0.075, 0.120, 1.0],
            light_color: [0.185, 0.235, 0.325, 1.0],
            shadow_color: [0.010, 0.016, 0.026, 1.0],
            glow_color: [0.0, 0.0, 0.0, 1.0],
        },

        panel_base: BevelStyle {
            bevel: 2.5,
            softness: 1.25,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 1.8,
            face_color: [0.060, 0.085, 0.135, 1.0], // was too bright
            light_color: [0.205, 0.255, 0.345, 1.0],
            shadow_color: [0.014, 0.020, 0.032, 1.0],
            glow_color: [0.0, 0.0, 0.0, 1.0],
        },

        panel_inner: BevelStyle {
            bevel: 2.5,
            softness: 1.35,
            glow_width: 0.0,
            glow_alpha: 0.0,
            face_color: [0.025, 0.045, 0.080, 1.0],
            inner_shadow: 4.8, // increase
            light_color: [0.105, 0.145, 0.220, 1.0],
            shadow_color: [0.004, 0.008, 0.015, 1.0],
            glow_color: [0.0, 0.0, 0.0, 1.0],
        },

        trim: BevelStyle {
            bevel: 1.4,
            softness: 0.95,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 1.2,
            face_color: [0.075, 0.105, 0.160, 1.0],
            light_color: [0.235, 0.290, 0.380, 1.0],
            shadow_color: [0.020, 0.028, 0.040, 1.0],
            glow_color: [0.0, 0.0, 0.0, 1.0],
        },

        sidebar: BevelStyle {
            bevel: 2.8,
            softness: 1.2,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 2.4,
            face_color: [0.050, 0.073, 0.118, 1.0],
            light_color: [0.155, 0.205, 0.290, 1.0],
            shadow_color: [0.008, 0.013, 0.022, 1.0],
            glow_color: [0.0, 0.0, 0.0, 1.0],
        },

        module: BevelStyle {
            bevel: 2.4,
            softness: 1.1,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 1.8,
            face_color: [0.070, 0.098, 0.150, 1.0],
            light_color: [0.200, 0.250, 0.335, 1.0],
            shadow_color: [0.012, 0.018, 0.028, 1.0],
            glow_color: [0.0, 0.0, 0.0, 1.0],
        },

        module_inner: BevelStyle {
            bevel: 2.0,
            softness: 1.15,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 3.0,
            face_color: [0.040, 0.060, 0.102, 1.0],
            light_color: [0.105, 0.145, 0.215, 1.0],
            shadow_color: [0.004, 0.008, 0.014, 1.0],
            glow_color: [0.0, 0.0, 0.0, 1.0],
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
            face_color: [0.055, 0.078, 0.120, 1.0],
            light_color: [0.170, 0.220, 0.305, 1.0],
            shadow_color: [0.008, 0.013, 0.022, 1.0],
            glow_color: [0.0, 0.0, 0.0, 1.0],
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
            opacity: 0.08,                    // down from 0.90+
            edge_width: 12.0,                 // tighter
            edge_brightness: 0.75,            // WAS TOO HIGH
            highlight_strength: 0.10,         // cut this a lot
            tint: [0.035, 0.085, 0.200, 1.0], // darker tint
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
