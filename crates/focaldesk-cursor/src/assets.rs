// assets.rs
use crate::cursor::CursorIcon;
use anyhow::{anyhow, Result};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct CursorImage {
    pub width: u32,
    pub height: u32,
    size: u32,
    scale: f32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    pub rgba: Vec<u8>,
}

impl CursorImage {
    pub fn logical_base_size(&self) -> u32 {
        self.size
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CacheKey {
    icon: CursorIcon,
    size: u32,
    scale_milli: u32,
}

pub struct CursorAssets {
    cache: HashMap<CacheKey, CursorImage>,
}

impl CursorAssets {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    pub fn image_for(&mut self, icon: CursorIcon, size: u32, scale: f32) -> Result<&CursorImage> {
        let key = CacheKey {
            icon,
            size,
            scale_milli: (scale * 1000.0) as u32,
        };

        if !self.cache.contains_key(&key) {
            let px = ((size as f32) * scale).round().max(1.0) as u32;
            let svg = svg_bytes_for(icon);
            let hotspot = hotspot_for(icon, px);
            let image = rasterize_svg(svg, px, px, hotspot, size, scale)?;
            self.cache.insert(key, image);
        }

        self.cache
            .get(&key)
            .ok_or_else(|| anyhow!("missing cached cursor image"))
    }
}

const BD_DOUBLE_ARROW: &[u8] = include_bytes!("../../../assets/svg/cursor/bd_double_arrow.svg");
const CROSSHAIR: &[u8] = include_bytes!("../../../assets/svg/cursor/crosshair.svg");
const FD_DOUBLE_ARROW: &[u8] = include_bytes!("../../../assets/svg/cursor/fd_double_arrow.svg");
const GRABBING: &[u8] = include_bytes!("../../../assets/svg/cursor/grabbing.svg");
const HAND1: &[u8] = include_bytes!("../../../assets/svg/cursor/hand1.svg");
const LEFT_PTR: &[u8] = include_bytes!("../../../assets/svg/cursor/left_ptr.svg");
#[allow(dead_code)]
const LEFT_PTR_ACTIVE: &[u8] = include_bytes!("../../../assets/svg/cursor/left_ptr_active.svg");
const NOT_ALLOWED: &[u8] = include_bytes!("../../../assets/svg/cursor/not-allowed.svg");
const SB_H_DOUBLE_ARROW: &[u8] = include_bytes!("../../../assets/svg/cursor/sb_h_double_arrow.svg");
const SB_V_DOUBLE_ARROW: &[u8] = include_bytes!("../../../assets/svg/cursor/sb_v_double_arrow.svg");
const WATCH: &[u8] = include_bytes!("../../../assets/svg/cursor/watch.svg");
const XTERM: &[u8] = include_bytes!("../../../assets/svg/cursor/xterm.svg");

fn svg_bytes_for(icon: CursorIcon) -> &'static [u8] {
    match icon {
        CursorIcon::Default => LEFT_PTR,
        CursorIcon::Pointer => HAND1,
        CursorIcon::Text => XTERM,
        CursorIcon::Crosshair => CROSSHAIR,
        CursorIcon::Grab => HAND1,
        CursorIcon::Grabbing => GRABBING,
        CursorIcon::Move => GRABBING,
        CursorIcon::Wait => WATCH,
        CursorIcon::Help => LEFT_PTR,
        CursorIcon::NotAllowed => NOT_ALLOWED,
        CursorIcon::EwResize => SB_H_DOUBLE_ARROW,
        CursorIcon::NsResize => SB_V_DOUBLE_ARROW,
        CursorIcon::NwseResize => BD_DOUBLE_ARROW,
        CursorIcon::NeswResize => FD_DOUBLE_ARROW,
    }
}

fn hotspot_for(icon: CursorIcon, size: u32) -> (u32, u32) {
    match icon {
        CursorIcon::Default | CursorIcon::Pointer => (2, 1),
        CursorIcon::Text => (size / 2, size / 2),
        CursorIcon::Wait => (size / 2, size / 2),
        _ => (2, 1),
    }
}

fn rasterize_svg(
    svg_bytes: &[u8],
    width: u32,
    height: u32,
    hotspot: (u32, u32),
    size: u32,
    scale: f32,
) -> Result<CursorImage> {
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg_bytes, &opt)?;

    let mut pixmap =
        tiny_skia::Pixmap::new(width, height).ok_or_else(|| anyhow!("failed to create pixmap"))?;

    let svg_size = tree.size();
    let transform = tiny_skia::Transform::from_scale(
        width as f32 / svg_size.width(),
        height as f32 / svg_size.height(),
    );

    resvg::render(&tree, transform, &mut pixmap.as_mut());

    Ok(CursorImage {
        width,
        height,
        size,
        scale,
        hotspot_x: hotspot.0,
        hotspot_y: hotspot.1,
        rgba: pixmap.data().to_vec(),
    })
}
