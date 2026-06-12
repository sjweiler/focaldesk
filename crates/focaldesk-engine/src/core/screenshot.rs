use smithay::backend::renderer::gles::GlesRenderer;
use smithay::utils::{Size, Physical};
use smithay::backend::renderer::Bind;
use focaldesk_logging::flog_info;

use image::{ImageBuffer, Rgba};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn capture_screenshot(
    renderer: &mut GlesRenderer,
    texture: &mut smithay::backend::renderer::gles::GlesTexture,
    size: Size<i32, Physical>,
) -> anyhow::Result<()> {
    let width = size.w as usize;
    let height = size.h as usize;

    let mut pixels = vec![0u8; width * height * 4];

    unsafe {
        use smithay::backend::renderer::gles::ffi;

        ffi::gl::BindTexture(ffi::gl::TEXTURE_2D, texture.tex_id());
        ffi::gl::GetTexImage(
            ffi::gl::TEXTURE_2D,
            0,
            ffi::gl::RGBA,
            ffi::gl::UNSIGNED_BYTE,
            pixels.as_mut_ptr() as *mut _,
        );
    }

    flip_vertical(&mut pixels, width, height);

    let path = next_screenshot_path();

    let img: ImageBuffer<Rgba<u8>, _> =
        ImageBuffer::from_raw(width as u32, height as u32, pixels)
            .expect("invalid buffer");

    img.save(&path)?;

    flog_info!("Saved screenshot: {:?}", path);

    Ok(())
}

fn flip_vertical(pixels: &mut [u8], width: usize, height: usize) {
    let stride = width * 4;

    for y in 0..height / 2 {
        let top = y * stride;
        let bottom = (height - 1 - y) * stride;

        for i in 0..stride {
            pixels.swap(top + i, bottom + i);
        }
    }
}

fn next_screenshot_path() -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let dir = dirs::picture_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap())
        .join("FocalDesk");

    std::fs::create_dir_all(&dir).ok();

    dir.join(format!("screenshot-{}.png", ts))
}
