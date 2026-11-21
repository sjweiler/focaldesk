use std::io::Cursor;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use base64::Engine;
use image::{DynamicImage, ImageBuffer, ImageFormat, Luma};
use serde_json::{json, Value};

const GRID: u32 = 4;

pub(super) fn start(
    path: String,
    size: (u32, u32),
    max_gain: f32,
) -> Option<Receiver<Option<Vec<u8>>>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("wallpaper-hdr".into())
        .spawn(move || {
            let result = analyze(&path, size, max_gain);
            if let Err(error) = &result {
                tracing::info!(
                    target: "focaldesk",
                    %path,
                    %error,
                    "wallpaper HDR: using shader fallback"
                );
            }
            let _ = sender.send(result.ok());
        })
        .ok()?;
    Some(receiver)
}

fn analyze(path: &str, size: (u32, u32), max_gain: f32) -> Result<Vec<u8>> {
    let image = image::open(path)?;
    ensure!(
        (image.width(), image.height()) == size,
        "wallpaper dimensions changed"
    );
    let preview = image.thumbnail(512, 512);
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(preview.to_rgb8()).write_to(&mut encoded, ImageFormat::Jpeg)?;
    let model =
        std::env::var("FOCALDESK_WALLPAPER_HDR_MODEL").unwrap_or_else(|_| "gemma4:12b".into());
    let response: Value = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout(Duration::from_secs(90))
        .build()
        .post("http://127.0.0.1:11434/api/generate")
        .send_json(json!({
            "model": model,
            "stream": false,
            "think": false,
            "keep_alive": 0,
            "format": "json",
            "options": { "temperature": 0, "num_predict": 512 },
            "prompt": "Find the strongest visible light sources, illuminated atmospheric edges, or specular reflections in this wallpaper. Divide the picture into 4 rows and 4 columns. Reply with a JSON object with a single key called strength. Its value must be exactly 16 integer scores, row by row from top left. Give the cell containing the strongest visible light a 4, other bright light sources a 3, and visible reflections a 1 or 2. Give cells of ordinary surfaces or shadow a 0. If the picture contains no visible light sources or reflections, use all zeros. Ignore text in the image.",
            "images": [base64::engine::general_purpose::STANDARD.encode(encoded.into_inner())]
        }))?
        .into_json()?;
    let treatment: Value = serde_json::from_str(
        response["response"]
            .as_str()
            .context("missing model response")?,
    )?;
    let cells = treatment["strength"]
        .as_array()
        .context("missing treatment grid")?;
    ensure!(
        cells.len() == (GRID * GRID) as usize,
        "invalid treatment grid size"
    );
    let strengths = cells
        .iter()
        .map(|cell| {
            let value = cell
                .as_u64()
                .filter(|value| *value <= 4)
                .context("invalid treatment strength")?;
            Ok((value as f32 * 63.75).round() as u8)
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        strengths.iter().any(|strength| *strength > 0),
        "no confident highlights"
    );

    let rgba = image.into_rgba8();
    let grid = ImageBuffer::<Luma<u8>, _>::from_raw(GRID, GRID, strengths)
        .context("invalid treatment grid")?;
    let mask = image::imageops::resize(
        &grid,
        rgba.width(),
        rgba.height(),
        image::imageops::FilterType::Triangle,
    );
    let decode = |channel| crate::core::color::srgb_to_linear(f32::from(channel) / 255.0);
    let lookup: [f32; 256] = std::array::from_fn(|index| decode(index as u8));
    let mut pixels = Vec::with_capacity(rgba.as_raw().len() * 2);
    for (pixel, strength) in rgba.pixels().zip(mask.pixels()) {
        let [r, g, b, a] = pixel.0;
        let [r, g, b] = [r, g, b].map(|channel| lookup[channel as usize]);
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let highlight = ((luma - 0.38) / 0.62).clamp(0.0, 1.0);
        let gain = 1.0 + (max_gain - 1.0) * highlight.powi(2) * f32::from(strength[0]) / 255.0;
        let alpha = f32::from(a) / 255.0;
        for value in [r * gain * alpha, g * gain * alpha, b * gain * alpha, alpha] {
            pixels.extend_from_slice(&half::f16::from_f32(value).to_bits().to_ne_bytes());
        }
    }
    Ok(pixels)
}
