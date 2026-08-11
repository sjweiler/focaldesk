//! Phase C2c: full ICC output LUT for SDR scanout.
//!
//! Bakes an encoded output-space → display transform with lcms2 and uploads it
//! as a 2D atlas. Matching the LUT source to the output gamut prevents an sRGB
//! intermediate from clipping wide-gamut colors.

use crate::core::color::{ColorDescription, PrimariesChromaticity, TransferFunction};
use crate::core::icc::IccError;
use lcms2::{CIExyY, CIExyYTRIPLE, Intent, PixelFormat, Profile, ToneCurve, Transform};

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

fn srgb_tone_curve() -> ToneCurve {
    const SAMPLES: usize = 4096;
    let values = (0..SAMPLES)
        .map(|index| {
            let encoded = index as f32 / (SAMPLES - 1) as f32;
            crate::core::color::srgb_to_linear(encoded)
        })
        .collect::<Vec<_>>();
    ToneCurve::new_tabulated_float(&values)
}

fn source_profile(description: ColorDescription) -> Result<Profile, IccError> {
    let PrimariesChromaticity { r, g, b, w } = description.primaries.chromaticity();
    let white = CIExyY {
        x: f64::from(w[0]),
        y: f64::from(w[1]),
        Y: 1.0,
    };
    let primaries = CIExyYTRIPLE {
        Red: CIExyY {
            x: f64::from(r[0]),
            y: f64::from(r[1]),
            Y: 1.0,
        },
        Green: CIExyY {
            x: f64::from(g[0]),
            y: f64::from(g[1]),
            Y: 1.0,
        },
        Blue: CIExyY {
            x: f64::from(b[0]),
            y: f64::from(b[1]),
            Y: 1.0,
        },
    };
    let curve = match description.transfer {
        TransferFunction::Srgb | TransferFunction::Bt1886 => srgb_tone_curve(),
        TransferFunction::Gamma22 => ToneCurve::new(2.2),
        TransferFunction::Linear => ToneCurve::new(1.0),
        TransferFunction::St2084Pq => return Err(IccError::Invalid("PQ ICC LUT source")),
    };
    Profile::new_rgb(&white, &primaries, &[&curve, &curve, &curve]).map_err(Into::into)
}

/// Bake an encoded output-color-space → display ICC transform into a 3D LUT.
///
/// Using the advertised output color space as the LUT source is essential for
/// wide gamut. A fixed sRGB source would clamp Display P3 colors before the
/// device transform can see them.
pub fn build_output_to_device_lut(
    icc: &[u8],
    source_description: ColorDescription,
) -> Result<OutputIccLut, IccError> {
    let display = Profile::new_icc(icc)?;
    let source = source_profile(source_description)?;
    let transform = Transform::new(
        &source,
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

/// Compatibility wrapper for callers and tests that explicitly need sRGB as
/// the source color space.
pub fn build_srgb_to_device_lut(icc: &[u8]) -> Result<OutputIccLut, IccError> {
    build_output_to_device_lut(icc, ColorDescription::SRGB)
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
        assert_eq!(
            lut.rgb.len(),
            LUT_GRID_SIZE * LUT_GRID_SIZE * LUT_GRID_SIZE * 3
        );
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

    #[test]
    fn display_p3_source_lut_does_not_remap_p3_primaries_through_srgb() {
        let display =
            source_profile(ColorDescription::DISPLAY_P3_SRGB).expect("create Display P3 profile");
        let bytes = display.icc().expect("serialize Display P3 profile");
        let lut = build_output_to_device_lut(&bytes, ColorDescription::DISPLAY_P3_SRGB)
            .expect("bake Display P3 identity LUT");
        let red = lut.sample_rgb8(LUT_GRID_SIZE - 1, 0, 0);
        assert!(red[0] > 247 && red[1] < 8 && red[2] < 8, "{red:?}");
    }
}
