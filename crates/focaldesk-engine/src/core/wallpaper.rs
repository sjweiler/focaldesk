#[derive(Debug, Clone, Copy)]
pub enum WallpaperMode {
    Fill, // cover
    Fit,  // contain
    Stretch,
    Center,
    Tile,
}

#[derive(Debug, Clone, Copy)]
pub struct RectI {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct SizeI {
    pub w: i32,
    pub h: i32,
}

/// Normalized UV rectangle.
#[derive(Debug, Clone, Copy)]
pub struct Uv {
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Blit {
    pub dst: RectI,
    pub uv: Uv,
}

fn clamp01(x: f32) -> f32 {
    x.max(0.0).min(1.0)
}

fn uv_full() -> Uv {
    Uv {
        u0: 0.0,
        v0: 0.0,
        u1: 1.0,
        v1: 1.0,
    }
}

pub fn compute_wallpaper_blit(src: SizeI, out: RectI, mode: WallpaperMode) -> Option<Blit> {
    if src.w <= 0 || src.h <= 0 || out.w <= 0 || out.h <= 0 {
        return None;
    }

    let sw = src.w as f32;
    let sh = src.h as f32;
    let ow = out.w as f32;
    let oh = out.h as f32;

    match mode {
        WallpaperMode::Stretch => Some(Blit {
            dst: out,
            uv: uv_full(),
        }),

        WallpaperMode::Fit => {
            // contain
            let s = (ow / sw).min(oh / sh);
            let dw = (sw * s).round() as i32;
            let dh = (sh * s).round() as i32;
            let dx = out.x + (out.w - dw) / 2;
            let dy = out.y + (out.h - dh) / 2;
            Some(Blit {
                dst: RectI {
                    x: dx,
                    y: dy,
                    w: dw,
                    h: dh,
                },
                uv: uv_full(),
            })
        }

        WallpaperMode::Fill => {
            // cover
            let s = (ow / sw).max(oh / sh);
            // crop in source pixels
            let cw = (ow / s).round();
            let ch = (oh / s).round();
            let cx = ((sw - cw) * 0.5).max(0.0);
            let cy = ((sh - ch) * 0.5).max(0.0);

            let u0 = clamp01(cx / sw);
            let v0 = clamp01(cy / sh);
            let u1 = clamp01((cx + cw) / sw);
            let v1 = clamp01((cy + ch) / sh);

            Some(Blit {
                dst: out,
                uv: Uv { u0, v0, u1, v1 },
            })
        }

        WallpaperMode::Center => {
            let dw = src.w.min(out.w);
            let dh = src.h.min(out.h);
            let dx = out.x + (out.w - dw) / 2;
            let dy = out.y + (out.h - dh) / 2;

            // If src bigger, crop centered; else full.
            let cx = ((src.w - dw) / 2).max(0);
            let cy = ((src.h - dh) / 2).max(0);

            let u0 = cx as f32 / sw;
            let v0 = cy as f32 / sh;
            let u1 = (cx + dw) as f32 / sw;
            let v1 = (cy + dh) as f32 / sh;

            Some(Blit {
                dst: RectI {
                    x: dx,
                    y: dy,
                    w: dw,
                    h: dh,
                },
                uv: Uv { u0, v0, u1, v1 },
            })
        }
        WallpaperMode::Tile => compute_wallpaper_blits(src, out, mode).into_iter().next(),
    }
}

pub fn compute_wallpaper_blits(src: SizeI, out: RectI, mode: WallpaperMode) -> Vec<Blit> {
    if !matches!(mode, WallpaperMode::Tile) {
        return compute_wallpaper_blit(src, out, mode).into_iter().collect();
    }
    if src.w <= 0 || src.h <= 0 || out.w <= 0 || out.h <= 0 {
        return Vec::new();
    }
    let columns = (out.w + src.w - 1) / src.w;
    let rows = (out.h + src.h - 1) / src.h;
    if columns.saturating_mul(rows) > 4096 {
        return compute_wallpaper_blit(src, out, WallpaperMode::Fill)
            .into_iter()
            .collect();
    }
    let mut blits = Vec::with_capacity((columns * rows) as usize);
    for row in 0..rows {
        for column in 0..columns {
            let width = src.w.min(out.w - column * src.w);
            let height = src.h.min(out.h - row * src.h);
            blits.push(Blit {
                dst: RectI {
                    x: out.x + column * src.w,
                    y: out.y + row * src.h,
                    w: width,
                    h: height,
                },
                uv: Uv {
                    u0: 0.0,
                    v0: 0.0,
                    u1: width as f32 / src.w as f32,
                    v1: height as f32 / src.h as f32,
                },
            });
        }
    }
    blits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_mode_clips_edge_tiles_and_uvs() {
        let blits = compute_wallpaper_blits(
            SizeI { w: 64, h: 64 },
            RectI {
                x: 10,
                y: 20,
                w: 100,
                h: 90,
            },
            WallpaperMode::Tile,
        );
        assert_eq!(blits.len(), 4);
        assert_eq!(blits[3].dst.w, 36);
        assert_eq!(blits[3].dst.h, 26);
        assert!((blits[3].uv.u1 - 0.5625).abs() < f32::EPSILON);
    }
}
