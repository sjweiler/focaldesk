//! Color-managed screenshot conversion and PNG encoding.
//!
//! Screenshot files use Display P3 with the sRGB transfer function regardless
//! of the monitor being captured.  The compositor's FP16 scene is converted
//! directly to that space, preserving wide-gamut colors and avoiding a
//! round-trip through the monitor-specific scanout encoding.

use crate::core::color::{
    linear_to_srgb, scene_to_output_matrix, ColorDescription, RenderingIntent,
};
use anyhow::{Context, Result};
use png::{BitDepth, ColorType, Info, ScaledFloat, SourceChromaticities};
use std::borrow::Cow;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

const DISPLAY_P3_PNG_GAMMA: u32 = 45_455;

fn display_p3_png_chromaticities() -> SourceChromaticities {
    SourceChromaticities::new(
        (0.3127, 0.3290),
        (0.6800, 0.3200),
        (0.2650, 0.6900),
        (0.1500, 0.0600),
    )
}

/// Convert native-endian RGBA16F scene pixels to Display P3 RGB16 while
/// preserving the readback row and column order.
pub fn linear_scene_f16_to_display_p3_rgb16(
    pixels: &[u8],
    width: usize,
    height: usize,
) -> Result<Vec<u16>> {
    let expected = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(8))
        .context("screenshot dimensions overflow")?;
    anyhow::ensure!(
        pixels.len() == expected,
        "invalid RGBA16F screenshot buffer: got {}, expected {expected}",
        pixels.len()
    );

    let matrix =
        scene_to_output_matrix(ColorDescription::DISPLAY_P3_SRGB, RenderingIntent::Relative);
    let mut out = Vec::with_capacity(width * height * 3);

    for y in 0..height {
        for x in 0..width {
            let offset = (y * width + x) * 8;
            let channel = |i: usize| {
                let bits = u16::from_ne_bytes([pixels[offset + i * 2], pixels[offset + i * 2 + 1]]);
                half_to_f32(bits)
            };
            append_display_p3_rgb16(&mut out, [channel(0), channel(1), channel(2)], matrix);
        }
    }
    Ok(out)
}

/// Convert RGBA8 sRGB compositor pixels for the legacy render path while
/// preserving the readback row and column order.
pub fn srgb_rgba8_to_display_p3_rgb16(
    pixels: &[u8],
    width: usize,
    height: usize,
) -> Result<Vec<u16>> {
    let expected = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(4))
        .context("screenshot dimensions overflow")?;
    anyhow::ensure!(pixels.len() == expected, "invalid RGBA8 screenshot buffer");

    let matrix =
        scene_to_output_matrix(ColorDescription::DISPLAY_P3_SRGB, RenderingIntent::Relative);
    let mut out = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            let offset = (y * width + x) * 4;
            let linear = [
                crate::core::color::srgb_to_linear(f32::from(pixels[offset]) / 255.0),
                crate::core::color::srgb_to_linear(f32::from(pixels[offset + 1]) / 255.0),
                crate::core::color::srgb_to_linear(f32::from(pixels[offset + 2]) / 255.0),
            ];
            append_display_p3_rgb16(&mut out, linear, matrix);
        }
    }
    Ok(out)
}

fn append_display_p3_rgb16(out: &mut Vec<u16>, rgb: [f32; 3], matrix: [[f32; 3]; 3]) {
    for row in matrix {
        let linear = row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2];
        let encoded = linear_to_srgb(linear.clamp(0.0, 1.0));
        out.push((encoded * 65535.0).round() as u16);
    }
}

/// Write a 16-bit Display P3 PNG with matching ICC, cHRM, and gAMA metadata.
pub fn write_display_p3_png(path: &Path, width: u32, height: u32, pixels: &[u16]) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    encode_display_p3_png(BufWriter::new(file), width, height, pixels)
        .with_context(|| format!("encode {}", path.display()))
}

fn encode_display_p3_png(
    writer: impl Write,
    width: u32,
    height: u32,
    pixels: &[u16],
) -> Result<()> {
    let expected = width as usize * height as usize * 3;
    anyhow::ensure!(pixels.len() == expected, "invalid RGB16 screenshot buffer");

    // The png crate accepts the on-disk byte order for 16-bit samples.
    let mut bytes = Vec::with_capacity(pixels.len() * 2);
    for sample in pixels {
        bytes.extend_from_slice(&sample.to_be_bytes());
    }

    let profile = super::icc_lut::source_profile(ColorDescription::DISPLAY_P3_SRGB)
        .map_err(|e| anyhow::anyhow!("create screenshot Display P3 profile: {e:?}"))?
        .icc()
        .context("serialize screenshot Display P3 profile")?;

    // iCCP is authoritative. Matching cHRM/gAMA chunks give non-ICC-aware
    // decoders a safe Display-P3 fallback instead of silently treating the
    // samples as sRGB/Rec.709.
    let mut info = Info::with_size(width, height);
    info.color_type = ColorType::Rgb;
    info.bit_depth = BitDepth::Sixteen;
    info.icc_profile = Some(Cow::Owned(profile));
    info.source_gamma = Some(ScaledFloat::from_scaled(DISPLAY_P3_PNG_GAMMA));
    info.source_chromaticities = Some(display_p3_png_chromaticities());

    let encoder = png::Encoder::with_info(writer, info).context("configure Display P3 PNG")?;
    let mut writer = encoder
        .write_header()
        .context("write Display P3 PNG header")?;
    writer
        .write_image_data(&bytes)
        .context("write 16-bit Display P3 PNG pixels")
}

fn half_to_f32(bits: u16) -> f32 {
    let sign = ((bits & 0x8000) as u32) << 16;
    let exponent = (bits >> 10) & 0x1f;
    let fraction = bits & 0x03ff;
    let value = match exponent {
        0 if fraction == 0 => sign,
        0 => {
            let mut fraction = fraction as u32;
            let mut exponent = 113u32;
            while fraction & 0x0400 == 0 {
                fraction <<= 1;
                exponent -= 1;
            }
            sign | (exponent << 23) | ((fraction & 0x03ff) << 13)
        }
        0x1f => sign | 0x7f80_0000 | ((fraction as u32) << 13),
        _ => sign | (((exponent as u32) + 112) << 23) | ((fraction as u32) << 13),
    };
    f32::from_bits(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn rgba16f_pixel(rgb: [u16; 3]) -> Vec<u8> {
        [rgb[0], rgb[1], rgb[2], 0x3c00]
            .into_iter()
            .flat_map(u16::to_ne_bytes)
            .collect()
    }

    #[test]
    fn fp16_conversion_preserves_rows_and_keeps_16_bit_precision() {
        let mut top_down = rgba16f_pixel([0x3c00, 0x0000, 0x0000]);
        top_down.extend(rgba16f_pixel([0x0000, 0x0000, 0x3c00]));
        let converted = linear_scene_f16_to_display_p3_rgb16(&top_down, 1, 2).unwrap();
        assert!(
            converted[0] > converted[2],
            "top row should be red: {converted:?}"
        );
        assert!(
            converted[5] > converted[3],
            "bottom row should be blue: {converted:?}"
        );

        let mid = linear_scene_f16_to_display_p3_rgb16(&rgba16f_pixel([0x3800; 3]), 1, 1).unwrap();
        assert!(mid.iter().any(|sample| sample % 257 != 0));
    }

    #[test]
    fn screenshot_conversion_preserves_readback_orientation() {
        let red = rgba16f_pixel([0x3c00, 0x0000, 0x0000]);
        let green = rgba16f_pixel([0x0000, 0x3c00, 0x0000]);
        let blue = rgba16f_pixel([0x0000, 0x0000, 0x3c00]);
        let white = rgba16f_pixel([0x3c00, 0x3c00, 0x3c00]);

        // The renderer's screenshot readback is already in the desired PNG
        // order: top-left, top-right, bottom-left, bottom-right.
        let raw_f16 = [red.clone(), green.clone(), blue.clone(), white.clone()].concat();
        let converted_f16 = linear_scene_f16_to_display_p3_rgb16(&raw_f16, 2, 2).unwrap();
        let expected_f16 = [red, green, blue, white]
            .into_iter()
            .flat_map(|pixel| linear_scene_f16_to_display_p3_rgb16(&pixel, 1, 1).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(converted_f16, expected_f16);

        let raw_rgba8 = [
            255, 0, 0, 255, // top-left: red
            0, 255, 0, 255, // top-right: green
            0, 0, 255, 255, // bottom-left: blue
            255, 255, 255, 255, // bottom-right: white
        ];
        let converted_rgba8 = srgb_rgba8_to_display_p3_rgb16(&raw_rgba8, 2, 2).unwrap();
        let expected_rgba8 = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 255, 255],
        ]
        .into_iter()
        .flat_map(|pixel| srgb_rgba8_to_display_p3_rgb16(&pixel, 1, 1).unwrap())
        .collect::<Vec<_>>();
        assert_eq!(converted_rgba8, expected_rgba8);
    }

    #[test]
    fn png_is_rgb16_with_consistent_display_p3_metadata() {
        let mut encoded = Vec::new();
        encode_display_p3_png(&mut encoded, 1, 1, &[0, 32_768, u16::MAX]).unwrap();

        let decoder = png::Decoder::new(Cursor::new(encoded));
        let mut reader = decoder.read_info().unwrap();
        let info = reader.info();
        assert_eq!(info.color_type, ColorType::Rgb);
        assert_eq!(info.bit_depth, BitDepth::Sixteen);
        assert_eq!(
            info.gamma().map(ScaledFloat::into_scaled),
            Some(DISPLAY_P3_PNG_GAMMA)
        );
        assert_eq!(info.chromaticities(), Some(display_p3_png_chromaticities()));

        let expected_profile =
            super::super::icc_lut::source_profile(ColorDescription::DISPLAY_P3_SRGB)
                .unwrap()
                .icc()
                .unwrap();
        assert_eq!(
            info.icc_profile.as_deref(),
            Some(expected_profile.as_slice())
        );

        let embedded_profile =
            lcms2::Profile::new_icc(info.icc_profile.as_deref().unwrap()).unwrap();
        match embedded_profile.read_tag(lcms2::TagSignature::ChromaticityTag) {
            lcms2::Tag::CIExyYTRIPLE(chromaticities) => {
                assert!((chromaticities.Red.x - 0.6800).abs() < 1e-4);
                assert!((chromaticities.Red.y - 0.3200).abs() < 1e-4);
                assert!((chromaticities.Green.x - 0.2650).abs() < 1e-4);
                assert!((chromaticities.Green.y - 0.6900).abs() < 1e-4);
                assert!((chromaticities.Blue.x - 0.1500).abs() < 1e-4);
                assert!((chromaticities.Blue.y - 0.0600).abs() < 1e-4);
            }
            tag => panic!("expected ICC chromaticity tag, got {tag:?}"),
        }
        match embedded_profile.read_tag(lcms2::TagSignature::RedTRCTag) {
            lcms2::Tag::ToneCurve(curve) => {
                let actual: f32 = curve.eval(0.5);
                assert!((actual - crate::core::color::srgb_to_linear(0.5)).abs() < 1e-4);
            }
            tag => panic!("expected ICC red TRC tag, got {tag:?}"),
        }

        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let output = reader.next_frame(&mut pixels).unwrap();
        assert_eq!(&pixels[..output.buffer_size()], &[0, 0, 128, 0, 255, 255]);
    }
}
