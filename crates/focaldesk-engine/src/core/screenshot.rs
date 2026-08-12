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
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

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

/// Write a 16-bit Display P3 PNG with its matching ICC profile in an iCCP chunk.
pub fn write_display_p3_png(path: &Path, width: u32, height: u32, pixels: &[u16]) -> Result<()> {
    let expected = width as usize * height as usize * 3;
    anyhow::ensure!(pixels.len() == expected, "invalid RGB16 screenshot buffer");

    // ImageEncoder accepts native-endian 16-bit samples and performs PNG's
    // required big-endian conversion itself.
    let mut bytes = Vec::with_capacity(pixels.len() * 2);
    for sample in pixels {
        bytes.extend_from_slice(&sample.to_ne_bytes());
    }

    let profile = super::icc_lut::source_profile(ColorDescription::DISPLAY_P3_SRGB)
        .map_err(|e| anyhow::anyhow!("create screenshot Display P3 profile: {e:?}"))?
        .icc()
        .context("serialize screenshot Display P3 profile")?;
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut encoder = PngEncoder::new(BufWriter::new(file));
    encoder
        .set_icc_profile(profile)
        .context("attach screenshot ICC profile")?;
    encoder
        .write_image(&bytes, width, height, ExtendedColorType::Rgb16)
        .context("encode 16-bit screenshot PNG")
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
    use image::ImageDecoder;
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
    fn png_is_rgb16_and_contains_an_icc_profile() {
        let mut encoded = Vec::new();
        {
            let profile = super::super::icc_lut::source_profile(ColorDescription::DISPLAY_P3_SRGB)
                .unwrap()
                .icc()
                .unwrap();
            let mut encoder = PngEncoder::new(&mut encoded);
            encoder.set_icc_profile(profile).unwrap();
            encoder
                .write_image(&[0u8; 6], 1, 1, ExtendedColorType::Rgb16)
                .unwrap();
        }
        let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(encoded)).unwrap();
        assert_eq!(decoder.original_color_type(), ExtendedColorType::Rgb16);
        assert!(decoder.icc_profile().unwrap().is_some());
    }
}
