//! Phase C2c: full ICC output LUT for SDR scanout.
//!
//! Bakes an sRGB → display transform with lcms2 and uploads it as a 2D atlas (future)
//! or applies the parametric fallback until the LUT shader lands.

use crate::core::icc::IccError;
use lcms2::{Intent, PixelFormat, Profile, Transform};

/// Edge length of the baked 3D LUT (33³ = 35,937 samples).
pub const LUT_GRID_SIZE: usize = 33;

/// When false, the compositor uses the parametric C1b encode even if an ICC LUT is available.
pub fn icc_lut_shader_enabled() -> bool {
    !matches!(
        std::env::var("FOCALDESK_ICC_LUT").ok().as_deref(),
        Some("0") | Some("false") | Some("no") | Some("off")
    )
}

/// sRGB-encoded RGB8 samples for each cell of a `LUT_GRID_SIZE`³ cube.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputIccLut {
    pub grid_size: u32,
    pub rgb: Vec<u8>,
}

impl OutputIccLut {
    pub fn grid_size(&self) -> usize {
        self.grid_size as usize
    }

    pub fn sample_index(&self, r: usize, g: usize, b: usize) -> usize {
        let n = self.grid_size();
        (b * n * n + g * n + r) * 3
    }

    pub fn sample_rgb8(&self, r: usize, g: usize, b: usize) -> [u8; 3] {
        let i = self.sample_index(r, g, b);
        [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
    }

    /// 2D atlas size for GLES: width = grid², height = grid (one row per blue slice).
    pub fn atlas_size(&self) -> (u32, u32) {
        let n = self.grid_size;
        (n * n, n)
    }
}

/// Bake sRGB → display ICC transform into a 3D LUT using perceptual intent.
pub fn build_srgb_to_device_lut(icc: &[u8]) -> Result<OutputIccLut, IccError> {
    let display = Profile::new_icc(icc)?;
    let srgb = Profile::new_srgb();
    let transform = Transform::new(
        &srgb,
        PixelFormat::RGB_8,
        &display,
        PixelFormat::RGB_8,
        Intent::Perceptual,
    )?;

    let n = LUT_GRID_SIZE;
    let mut rgb = vec![0u8; n * n * n * 3];
    let mut slice_in = vec![0u8; n * n * 3];
    let mut slice_out = vec![0u8; n * n * 3];

    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let i = (g * n + r) * 3;
                slice_in[i] = grid_to_u8(r, n);
                slice_in[i + 1] = grid_to_u8(g, n);
                slice_in[i + 2] = grid_to_u8(b, n);
            }
        }
        transform.transform_pixels(&slice_in, &mut slice_out);
        for g in 0..n {
            for r in 0..n {
                let src = (g * n + r) * 3;
                let dst = (b * n * n + g * n + r) * 3;
                rgb[dst..dst + 3].copy_from_slice(&slice_out[src..src + 3]);
            }
        }
    }

    Ok(OutputIccLut {
        grid_size: n as u32,
        rgb,
    })
}

fn grid_to_u8(index: usize, grid: usize) -> u8 {
    if grid <= 1 {
        return 0;
    }
    ((index * 255) / (grid - 1)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn lut_grid_has_expected_sample_count() {
        let lut = OutputIccLut {
            grid_size: LUT_GRID_SIZE as u32,
            rgb: vec![0; LUT_GRID_SIZE.pow(3) * 3],
        };
        assert_eq!(lut.rgb.len(), LUT_GRID_SIZE * LUT_GRID_SIZE * LUT_GRID_SIZE * 3);
        assert_eq!(lut.atlas_size(), (33 * 33, 33));
    }

    #[test]
    fn srgb_icc_produces_identityish_corners() {
        let srgb = Profile::new_srgb();
        let bytes = srgb.icc().expect("srgb icc bytes");
        let lut = build_srgb_to_device_lut(&bytes).expect("bake srgb lut");
        let black = lut.sample_rgb8(0, 0, 0);
        let white = lut.sample_rgb8(LUT_GRID_SIZE - 1, LUT_GRID_SIZE - 1, LUT_GRID_SIZE - 1);
        assert!(black[0] < 8 && black[1] < 8 && black[2] < 8);
        assert!(white[0] > 247 && white[1] > 247 && white[2] > 247);
    }

    #[test]
    fn bluish_lut_differs_from_srgb_lut() {
        let srgb_path = "/usr/share/color/icc/colord/sRGB.icc";
        let bluish_path = "/usr/share/color/icc/colord/Bluish.icc";
        if !Path::new(bluish_path).exists() {
            return;
        }
        let srgb_lut =
            build_srgb_to_device_lut(&std::fs::read(srgb_path).expect("srgb")).expect("srgb lut");
        let bluish_lut =
            build_srgb_to_device_lut(&std::fs::read(bluish_path).expect("bluish")).expect("bluish");
        assert_ne!(srgb_lut.rgb, bluish_lut.rgb);
    }
}
