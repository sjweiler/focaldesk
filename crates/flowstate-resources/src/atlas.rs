use anyhow::{Result, anyhow};
use std::collections::HashMap;

use image::RgbaImage;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{GlesError, GlesTexture};
use smithay::backend::renderer::{ImportMem, Renderer, RendererSuper};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};

use crate::svg::rasterize_svg;

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
    FocusShellLabel,
    PowerMenu,
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
}

#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
pub struct AtlasIcon {
    pub id: IconId,
    pub state: IconState,
}

pub struct IconAtlas {
    pub texture: GlesTexture,
    pub rects: HashMap<AtlasIcon, AtlasRect>,
    pub width: u32,
    pub height: u32,
}

const STATES: [IconState; 3] = [IconState::Inactive, IconState::Hover, IconState::Active];

fn icon_color(icon: IconId, state: IconState) -> &'static str {
    match (icon, state) {
        (_, IconState::Inactive) => "#7F91A3",
        (_, IconState::Hover) => "#C8D4DE",
        (IconId::FocusPip, IconState::Active) => "#4DA3FF",
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
    pub fn get(&self, id: IconId, state: IconState) -> Option<&AtlasRect> {
        self.rects.get(&AtlasIcon { id, state })
    }
}

pub fn render_atlas_icon(
    frame: &mut impl smithay::backend::renderer::Frame<TextureId = GlesTexture, Error = GlesError>,
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
    let full = Rectangle::<i32, Physical>::from_loc_and_size((0, 0), (2560, 1440));
    frame.render_texture_from_to(atlas, src, dst, &[full], &[], Transform::Normal, 1.0)
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
pub fn build_icon_atlas<R>(renderer: &mut R) -> Result<IconAtlas>
where
    R: Renderer<TextureId = GlesTexture> + ImportMem,
    <R as RendererSuper>::Error: std::fmt::Debug,
{
    const ATLAS_W: u32 = 1024;
    const ATLAS_H: u32 = 1024;
    const CELL: u32 = 64;
    const ICON: u32 = 48;
    const PAD: u32 = 8;

    let mut atlas_rgba = vec![0u8; (ATLAS_W * ATLAS_H * 4) as usize];

    let all_icons: &[(IconId, &[u8])] = &[
        (
            IconId::Launcher,
            include_bytes!("../../../assets/svg/launcher.svg"),
        ),
        (
            IconId::Overflow,
            include_bytes!("../../../assets/svg/overflow.svg"),
        ),
        (
            IconId::Settings,
            include_bytes!("../../../assets/svg/settings.svg"),
        ),
        (
            IconId::Battery,
            include_bytes!("../../../assets/svg/battery.svg"),
        ),
        (
            IconId::LinePower,
            include_bytes!("../../../assets/svg/plug.svg"),
        ),
        (
            IconId::FocusPip,
            include_bytes!("../../../assets/svg/focus_pip.svg"),
        ),
        (
            IconId::Ethernet,
            include_bytes!("../../../assets/svg/ethernet.svg"),
        ),
        (
            IconId::EthernetOff,
            include_bytes!("../../../assets/svg/ethernet-disabled.svg"),
        ),
        (IconId::Wifi, include_bytes!("../../../assets/svg/wifi.svg")),
        (
            IconId::WifiOff,
            include_bytes!("../../../assets/svg/wifi-off.svg"),
        ),
        (
            IconId::Bluetooth,
            include_bytes!("../../../assets/svg/bluetooth.svg"),
        ),
        (
            IconId::BluetoothOff,
            include_bytes!("../../../assets/svg/bluetooth-off.svg"),
        ),
        (
            IconId::AssignToSlot,
            include_bytes!("../../../assets/svg/assign-to-slot.svg"),
        ),
        (
            IconId::Slot(1),
            include_bytes!("../../../assets/svg/slot-1.svg"),
        ),
        (
            IconId::Slot(2),
            include_bytes!("../../../assets/svg/slot-2.svg"),
        ),
        (
            IconId::Slot(3),
            include_bytes!("../../../assets/svg/slot-3.svg"),
        ),
        (
            IconId::Slot(4),
            include_bytes!("../../../assets/svg/slot-4.svg"),
        ),
        (
            IconId::Slot(5),
            include_bytes!("../../../assets/svg/slot-5.svg"),
        ),
        (
            IconId::Slot(6),
            include_bytes!("../../../assets/svg/slot-6.svg"),
        ),
        (
            IconId::Slot(7),
            include_bytes!("../../../assets/svg/slot-7.svg"),
        ),
        (
            IconId::Slot(8),
            include_bytes!("../../../assets/svg/slot-8.svg"),
        ),
        (
            IconId::Slot(9),
            include_bytes!("../../../assets/svg/slot-9.svg"),
        ),
        (
            IconId::Microphone,
            include_bytes!("../../../assets/svg/microphone.svg"),
        ),
        (
            IconId::MicrophoneOff,
            include_bytes!("../../../assets/svg/microphone-off.svg"),
        ),
        (
            IconId::FocusShellLabel,
            include_bytes!("../../../assets/svg/flowstate-logo.svg"),
        ),
        (
            IconId::HDR,
            include_bytes!("../../../assets/svg/hdr-enabled.svg"),
        ),
        (
            IconId::DiagonalResize,
            include_bytes!("../../../assets/cursor/bd_double_arrow.png"),
        ),
        (
            IconId::CrossHair,
            include_bytes!("../../../assets/cursor/crosshair.png"),
        ),
        (
            IconId::OppositeDiagonalResize,
            include_bytes!("../../../assets/cursor/fd_double_arrow.png"),
        ),
        (
            IconId::Grabbing,
            include_bytes!("../../../assets/cursor/grabbing.png"),
        ),
        (
            IconId::OpenHand,
            include_bytes!("../../../assets/cursor/hand1.png"),
        ),
        (
            IconId::NormalPointer,
            include_bytes!("../../../assets/cursor/left_ptr.png"),
        ),
        (
            IconId::ActivePointer,
            include_bytes!("../../../assets/cursor/left_ptr_active.png"),
        ),
        (
            IconId::NotAllowed,
            include_bytes!("../../../assets/cursor/not-allowed.png"),
        ),
        (
            IconId::HorizontalResize,
            include_bytes!("../../../assets/cursor/sb_h_double_arrow.png"),
        ),
        (
            IconId::VerticslResize,
            include_bytes!("../../../assets/cursor/sb_v_double_arrow.png"),
        ),
        (
            IconId::Busy,
            include_bytes!("../../../assets/cursor/watch.png"),
        ),
        (
            IconId::Xterm,
            include_bytes!("../../../assets/cursor/xterm.png"),
        ),
    ];

    let mut rects = HashMap::new();
    let mut cell_index: u32 = 0;
    let cells_per_row = ATLAS_W / CELL;

    for &(icon_id, svg_bytes) in all_icons {
        for state in STATES {
            let styled_svg = style_svg_for_state(svg_bytes, icon_id, state)?;
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
                AtlasIcon { id: icon_id, state },
                AtlasRect {
                    x: x + PAD,
                    y: y + PAD,
                    w: ICON,
                    h: ICON,
                },
            );

            cell_index += 1;
        }
    }

    let debug_img = RgbaImage::from_raw(ATLAS_W, ATLAS_H, atlas_rgba.clone())
        .expect("failed to build atlas debug image");

    debug_img.save("/tmp/atlas-debug.png").unwrap();

    for px in atlas_rgba.chunks_exact_mut(4) {
        px.swap(0, 2); // swap R and B
    }

    let atlas_upload = atlas_rgba.clone();

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
