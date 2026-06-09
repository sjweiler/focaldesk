use anyhow::{anyhow, Result};
use std::collections::HashMap;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{GlesError, GlesTexture};
use smithay::backend::renderer::{ImportMem, Renderer, RendererSuper};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};
use image::RgbaImage;

use crate::svg::rasterize_svg;
use smithay::backend::renderer::Texture;
use smithay::backend::renderer::gles::GlesFrame;
    

#[derive(Clone, Copy, Debug)]
pub struct AtlasRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl AtlasRect {
    pub fn uv(&self, atlas_w: u32, atlas_h: u32) -> (f64, f64, f64, f64) {
        let u0 = self.x as f64 / atlas_w as f64;
        let v0 = self.y as f64 / atlas_h as f64;
        let u1 = (self.x + self.w) as f64 / atlas_w as f64;
        let v1 = (self.y + self.h) as f64 / atlas_h as f64;
        (u0, v0, u1, v1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconState {
    Inactive,
    Hover,
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconId {
    Launcher,
    Overflow,
    Settings,
    Battery,
    BatteryOff,
    LinePower,
    FocusPip,
    Ethernet,
    EthernetOff,
    Wifi,
    WifiOff,
    Bluetooth,
    BluetoothOff,
    AssignToSlot,
    Slot(u8),
    Microphone,
    MicrophoneOff,
    Speaker,
    SpeakerOff,
    FlowStateLabel,
    Power,
    HDR,
    DiagonalResize,
    CrossHair, 
    OppositeDiagonalResize, 
    Grabbing, 
    OpenHand, 
    NormalPointer, 
    ActivePointer, 
    NotAllowed, 
    HorizontalResize, 
    VerticslResize, 
    Busy, 
    Xterm, 
    Clock0,
    Clock1,
    Clock2,
    Clock3,
    Clock4,
    Clock5,
    Clock6,
    Clock7,
    Clock8,
    Clock9,
    ClockColon,
    ClockA,
    ClockP,
    ClockM,
    A, B, C, D, E, F, G, H, I, J, K, L, M,
    N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    Num0, Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9,
    Colon,
    Dash,
    Dot,
    Slash,
    Percent,
    Browser,
    Terminal,
    Files,
    Plus,
    Minus
}

//#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
//pub struct AtlasIcon {
//    pub id: IconId,
//    pub state: IconState,
//}

pub struct IconAtlas {
    pub texture: GlesTexture,
    pub rects: HashMap<IconId, AtlasRect>,
    pub width: u32,
    pub height: u32,
}

const STATES: [IconState; 3] = [
    IconState::Inactive,
    IconState::Hover,
    IconState::Active,
];

fn icon_color(icon: IconId, state: IconState) -> &'static str {
    let is_text_glyph = matches!(
        icon,
        IconId::A
            | IconId::B
            | IconId::C
            | IconId::D
            | IconId::E
            | IconId::F
            | IconId::G
            | IconId::H
            | IconId::I
            | IconId::J
            | IconId::K
            | IconId::L
            | IconId::M
            | IconId::N
            | IconId::O
            | IconId::P
            | IconId::Q
            | IconId::R
            | IconId::S
            | IconId::T
            | IconId::U
            | IconId::V
            | IconId::W
            | IconId::X
            | IconId::Y
            | IconId::Z
            | IconId::Num0
            | IconId::Num1
            | IconId::Num2
            | IconId::Num3
            | IconId::Num4
            | IconId::Num5
            | IconId::Num6
            | IconId::Num7
            | IconId::Num8
            | IconId::Num9
            | IconId::Colon
            | IconId::Dash
            | IconId::Dot
            | IconId::Slash
            | IconId::Percent
    );

    match (icon, state) {
        (_, IconState::Inactive) => "#8CA4C4",
        (_, IconState::Hover) if is_text_glyph => "#DDBB87",
        (_, IconState::Hover) => "#C8D4DE",
        (IconId::FocusPip, IconState::Active) => "#4DA3FF",
        (_, IconState::Active) if is_text_glyph => "#ECF3FF",
        (_, IconState::Active) => "#FFFFFF",
    }
}

fn style_svg_for_state(svg_bytes: &[u8], icon: IconId, state: IconState) -> Result<Vec<u8>> {
    let svg = std::str::from_utf8(svg_bytes)?;
    let color = icon_color(icon, state);
    let styled = svg.replace("currentColor", color);
    Ok(styled.into_bytes())
}

fn rasterize_svg_bytes(svg_bytes: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    let img = rasterize_svg(svg_bytes, w, h)?;
    Ok(img.into_raw())
}

fn blit_rgba(
    atlas: &mut [u8],
    atlas_w: u32,
    atlas_h: u32,
    dst_x: u32,
    dst_y: u32,
    src_w: u32,
    src_h: u32,
    src: &[u8],
) -> Result<()> {
    if dst_x + src_w > atlas_w || dst_y + src_h > atlas_h {
        return Err(anyhow!("icon does not fit in atlas"));
    }

    let src_stride = (src_w * 4) as usize;
    let dst_stride = (atlas_w * 4) as usize;

    for row in 0..src_h as usize {
        let src_off = row * src_stride;
        let dst_off = ((dst_y as usize + row) * dst_stride) + (dst_x as usize * 4);

        let src_slice = &src[src_off..src_off + src_stride];
        let dst_slice = &mut atlas[dst_off..dst_off + src_stride];
        dst_slice.copy_from_slice(src_slice);
    }


    Ok(())
}

impl IconAtlas {
    pub fn get(&self, id: IconId) -> Option<&AtlasRect> {
        self.rects.get(&id)
    }
}


pub fn render_atlas_icon(
    frame: &mut impl smithay::backend::renderer::Frame<
        TextureId = GlesTexture,
        Error = GlesError,
    >,
    atlas: &GlesTexture,
    entry: AtlasRect,
    x: i32,
    y: i32,
    w: i32,
    h: i32,    
) -> Result<(), GlesError> {
    render_atlas_icon_with_alpha(frame, atlas, entry, x, y, w, h, 1.0)
}

pub fn render_atlas_icon_with_alpha(
    frame: &mut impl smithay::backend::renderer::Frame<
        TextureId = GlesTexture,
        Error = GlesError,
    >,
    atlas: &GlesTexture,
    entry: AtlasRect,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    alpha: f32,
) -> Result<(), GlesError> {
let size = atlas.size();
let atlas_w = size.w as f64;
let atlas_h = size.h as f64;
    let src = Rectangle::<f64, Buffer>::from_loc_and_size(
        (
            entry.x as f64,
            entry.y as f64,
        ),
        (
            entry.w as f64,
            entry.h as f64,
        ),
    );


        
    let dst = Rectangle::<i32, Physical>::from_loc_and_size((x, y), (w, h));
    let full =  Rectangle::<i32, Physical>::from_loc_and_size((0,0),(2560,1440));
    
    
    frame.render_texture_from_to(
        atlas,
        src,
        dst,
        &[full],
        &[],
        Transform::Normal,
        alpha.clamp(0.0, 1.0),
    )
}

/*
pub fn render_atlas_icon(
    frame: &mut impl smithay::backend::renderer::Frame<
        TextureId = GlesTexture,
        Error = GlesError,
    >,
    atlas: &GlesTexture,
    entry: AtlasRect,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> Result<(), GlesError> {


let src = Rectangle::<f64, Buffer>::from_loc_and_size(
    (entry.x as f64, entry.y as f64),
    (entry.w as f64, entry.h as f64),
);


    let dst = Rectangle::<i32, Physical>::from_loc_and_size((x, y), (w, h));
   

flog_info!(
        "render_atlas_icon: src=({}, {}) {}x{}, dst=({}, {}) {}x{}",
        entry.x, entry.y, entry.w, entry.h, x, y, w, h
    );
   
    frame.render_texture_from_to(
        atlas,
        src,
        dst,
        &[dst],
        &[],
        Transform::Normal,
        1.0,
    )
}
*/

fn style_svg_white(svg_bytes: &[u8]) -> Result<Vec<u8>> {
    let svg = std::str::from_utf8(svg_bytes)?;

    let styled = svg
        .replace("currentColor", "#FFFFFF")
        .replace("stroke=\"black\"", "stroke=\"#FFFFFF\"")
        .replace("stroke=\"#000\"", "stroke=\"#FFFFFF\"")
        .replace("stroke=\"#000000\"", "stroke=\"#FFFFFF\"")
        .replace("fill=\"black\"", "fill=\"#FFFFFF\"")
        .replace("fill=\"#000\"", "fill=\"#FFFFFF\"")
        .replace("fill=\"#000000\"", "fill=\"#FFFFFF\"");

    Ok(styled.into_bytes())
}

pub fn build_icon_atlas<R>(renderer: &mut R) -> Result<IconAtlas>
where
    R: Renderer<TextureId = GlesTexture> + ImportMem,
    <R as RendererSuper>::Error: std::fmt::Debug,
{
    const ATLAS_W: u32 = 2048;
    const ATLAS_H: u32 = 2048;
    const CELL: u32 = 64;
    const ICON:u32 = 48;
    const PAD: u32 = 8;

    let mut atlas_rgba = vec![0u8; (ATLAS_W * ATLAS_H * 4) as usize];

    let all_icons: &[(IconId, &[u8])] = &[
        (IconId::Launcher, include_bytes!("../../../assets/svg/launcher.svg")),
        (IconId::Overflow, include_bytes!("../../../assets/svg/overflow.svg")),
        (IconId::Settings, include_bytes!("../../../assets/svg/settings.svg")),
        (IconId::Battery, include_bytes!("../../../assets/svg/battery.svg")),
        (IconId::LinePower, include_bytes!("../../../assets/svg/plug.svg")),
        (IconId::FocusPip, include_bytes!("../../../assets/svg/focus_pip.svg")),
        (IconId::Ethernet, include_bytes!("../../../assets/svg/ethernet.svg")),
        (IconId::EthernetOff, include_bytes!("../../../assets/svg/ethernet-disabled.svg")),
        (IconId::Wifi, include_bytes!("../../../assets/svg/wifi.svg")),
        (IconId::WifiOff, include_bytes!("../../../assets/svg/wifi-off.svg")),
        (IconId::Bluetooth, include_bytes!("../../../assets/svg/bluetooth.svg")),
        (IconId::BluetoothOff, include_bytes!("../../../assets/svg/bluetooth-off.svg")),
        (IconId::AssignToSlot, include_bytes!("../../../assets/svg/assign-to-slot.svg")),
        (IconId::Slot(1), include_bytes!("../../../assets/svg/slot-1.svg")),
        (IconId::Slot(2), include_bytes!("../../../assets/svg/slot-2.svg")),
        (IconId::Slot(3), include_bytes!("../../../assets/svg/slot-3.svg")),
        (IconId::Slot(4), include_bytes!("../../../assets/svg/slot-4.svg")),
        (IconId::Slot(5), include_bytes!("../../../assets/svg/slot-5.svg")),
        (IconId::Slot(6), include_bytes!("../../../assets/svg/slot-6.svg")),
        (IconId::Slot(7), include_bytes!("../../../assets/svg/slot-7.svg")),
        (IconId::Slot(8), include_bytes!("../../../assets/svg/slot-8.svg")),
        (IconId::Slot(9), include_bytes!("../../../assets/svg/slot-9.svg")),
        (IconId::Microphone, include_bytes!("../../../assets/svg/microphone.svg")),
        (IconId::MicrophoneOff, include_bytes!("../../../assets/svg/microphone-off.svg")),
        //(IconId::FlowStateLabel, include_bytes!("../../../assets/svg/flowstate-logo.svg")),
        (IconId::HDR, include_bytes!("../../../assets/svg/hdr-enabled.svg")),
        (IconId::Clock0, include_bytes!("../../../assets/svg/clock_0.svg")),
        (IconId::Clock1, include_bytes!("../../../assets/svg/clock_1.svg")),
        (IconId::Clock2, include_bytes!("../../../assets/svg/clock_2.svg")),
        (IconId::Clock3, include_bytes!("../../../assets/svg/clock_3.svg")),
        (IconId::Clock4, include_bytes!("../../../assets/svg/clock_4.svg")),
        (IconId::Clock5, include_bytes!("../../../assets/svg/clock_5.svg")),
        (IconId::Clock6, include_bytes!("../../../assets/svg/clock_6.svg")),
        (IconId::Clock7, include_bytes!("../../../assets/svg/clock_7.svg")),
        (IconId::Clock8, include_bytes!("../../../assets/svg/clock_8.svg")),
        (IconId::Clock9, include_bytes!("../../../assets/svg/clock_9.svg")),
        (IconId::ClockColon, include_bytes!("../../../assets/svg/clock_colon.svg")),
        (IconId::ClockA, include_bytes!("../../../assets/svg/clock_a.svg")),
        (IconId::ClockP, include_bytes!("../../../assets/svg/clock_p.svg")),
        (IconId::ClockM, include_bytes!("../../../assets/svg/clock_m.svg")),
        (IconId::A,  include_bytes!("../../../assets/svg/glyph_A.svg")),
        (IconId::B,  include_bytes!("../../../assets/svg/glyph_B.svg")), 
        (IconId::C,  include_bytes!("../../../assets/svg/glyph_C.svg")),  
        (IconId::D,  include_bytes!("../../../assets/svg/glyph_D.svg")),  
        (IconId::E,  include_bytes!("../../../assets/svg/glyph_E.svg")),  
        (IconId::F,  include_bytes!("../../../assets/svg/glyph_F.svg")),  
        (IconId::G,  include_bytes!("../../../assets/svg/glyph_G.svg")),  
        (IconId::H,  include_bytes!("../../../assets/svg/glyph_H.svg")),  
        (IconId::I,  include_bytes!("../../../assets/svg/glyph_I.svg")),  
        (IconId::J,  include_bytes!("../../../assets/svg/glyph_J.svg")),  
        (IconId::K,  include_bytes!("../../../assets/svg/glyph_K.svg")),  
        (IconId::L,  include_bytes!("../../../assets/svg/glyph_L.svg")),  
        (IconId::M,  include_bytes!("../../../assets/svg/glyph_M.svg")), 
        (IconId::N,  include_bytes!("../../../assets/svg/glyph_N.svg")),  
        (IconId::O,  include_bytes!("../../../assets/svg/glyph_O.svg")),  
        (IconId::P,  include_bytes!("../../../assets/svg/glyph_P.svg")), 
        (IconId::Q,  include_bytes!("../../../assets/svg/glyph_Q.svg")),  
        (IconId::R,  include_bytes!("../../../assets/svg/glyph_R.svg")),  
        (IconId::S,  include_bytes!("../../../assets/svg/glyph_S.svg")),  
        (IconId::T,  include_bytes!("../../../assets/svg/glyph_T.svg")),  
        (IconId::U,  include_bytes!("../../../assets/svg/glyph_U.svg")),  
        (IconId::V,  include_bytes!("../../../assets/svg/glyph_V.svg")),  
        (IconId::W,  include_bytes!("../../../assets/svg/glyph_W.svg")),  
        (IconId::X,  include_bytes!("../../../assets/svg/glyph_X.svg")),  
        (IconId::Y,  include_bytes!("../../../assets/svg/glyph_Y.svg")),  
        (IconId::Z,  include_bytes!("../../../assets/svg/glyph_Z.svg")), 
        (IconId::Num0,  include_bytes!("../../../assets/svg/glyph_0.svg")), 
        (IconId::Num1,  include_bytes!("../../../assets/svg/glyph_1.svg")),  
        (IconId::Num2,  include_bytes!("../../../assets/svg/glyph_2.svg")),  
        (IconId::Num3,  include_bytes!("../../../assets/svg/glyph_3.svg")),  
        (IconId::Num4,  include_bytes!("../../../assets/svg/glyph_4.svg")),  
        (IconId::Num5,  include_bytes!("../../../assets/svg/glyph_5.svg")),  
        (IconId::Num6,  include_bytes!("../../../assets/svg/glyph_6.svg")),  
        (IconId::Num7,  include_bytes!("../../../assets/svg/glyph_7.svg")),  
        (IconId::Num8,  include_bytes!("../../../assets/svg/glyph_8.svg")),  
        (IconId::Num9,  include_bytes!("../../../assets/svg/glyph_9.svg")), 
        (IconId::Colon,  include_bytes!("../../../assets/svg/glyph_colon.svg")), 
        (IconId::Dash,  include_bytes!("../../../assets/svg/glyph_dash.svg")), 
        (IconId::Dot,  include_bytes!("../../../assets/svg/glyph_dot.svg")), 
        (IconId::Slash,  include_bytes!("../../../assets/svg/glyph_slash.svg")), 
        (IconId::Percent,  include_bytes!("../../../assets/svg/glyph_percent.svg")), 
        (IconId::Power, include_bytes!("../../../assets/svg/power-menu.svg")),
        (IconId::Speaker, include_bytes!("../../../assets/svg/volume.svg")),
        (IconId::SpeakerOff, include_bytes!("../../../assets/svg/volume-off.svg")),
        (IconId::Browser, include_bytes!("../../../assets/svg/browser.svg")),
        (IconId::Terminal, include_bytes!("../../../assets/svg/terminal.svg")),
        (IconId::Files, include_bytes!("../../../assets/svg/files.svg")),
        (IconId::Plus, include_bytes!("../../../assets/svg/plus.svg")),
        (IconId::Minus, include_bytes!("../../../assets/svg/minus.svg")),
    
    ];

    let mut rects = HashMap::new();
    let mut cell_index: u32 = 0;
    let cells_per_row = ATLAS_W / CELL;

    for &(icon_id, svg_bytes) in all_icons {
        //for state in STATES {
            let styled_svg = style_svg_white(svg_bytes)?;
            let rgba = rasterize_svg_bytes(&styled_svg, ICON, ICON)?;

            let col = cell_index % cells_per_row;
            let row = cell_index / cells_per_row;

            let x = col * CELL;
            let y = row * CELL;

            if y + CELL > ATLAS_H {
                return Err(anyhow!("icon atlas overflow: atlas too small"));
            }
            
            blit_rgba(
                &mut atlas_rgba,
                ATLAS_W,
                ATLAS_H,
                x + PAD,
                y + PAD,
                ICON,
                ICON,
                &rgba,
            )?;
            rects.insert(
                icon_id,
                AtlasRect { x: x+PAD, y: y+PAD, w: ICON, h: ICON },
            );

            cell_index += 1;
        //}
    }

    let debug_img = RgbaImage::from_raw(ATLAS_W, ATLAS_H, atlas_rgba.clone())
        .expect("failed to build atlas debug image");

    debug_img.save("/tmp/atlas-debug.png").unwrap();

    for px in atlas_rgba.chunks_exact_mut(4) {
        px.swap(0, 2); // swap R and B
    }

    let mut atlas_upload = atlas_rgba.clone();



    let tex = renderer
        .import_memory(
            &atlas_upload,
            Fourcc::Argb8888,
            Size::from((ATLAS_W as i32, ATLAS_H as i32)),
            false,
        )
        .map_err(|e| anyhow!("atlas import_memory failed: {:?}", e))?;

    Ok(IconAtlas {
        texture: tex,
        rects,
        width: ATLAS_W,
        height: ATLAS_H,
    })
}
