// Small embedded 5x7 bitmap font (uppercase only; lowercase input is
// upper-cased before lookup). Hand-authored rather than transcribed from an
// existing font table, since there's no way to view real DRM scanout output
// from this environment to catch a transcription error. Verified instead by
// rendering to a PNG in the `render_preview` test below and visually
// inspecting it — see that test for how to regenerate the preview after
// changing a glyph.

pub const GLYPH_WIDTH: u32 = 5;
pub const GLYPH_HEIGHT: u32 = 7;

// Each row is the low 5 bits of the byte, bit 4 = leftmost pixel.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'C' => [0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111],
        'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01111, 0b10000, 0b10000, 0b10011, 0b10001, 0b10001, 0b01111],
        'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'J' => [0b00001, 0b00001, 0b00001, 0b00001, 0b10001, 0b10001, 0b01110],
        'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010],
        'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        '0' => [0b01110, 0b10011, 0b10011, 0b10101, 0b11001, 0b11001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        ':' => [0b00000, 0b00100, 0b00000, 0b00000, 0b00100, 0b00000, 0b00000],
        '.' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100],
        '-' | '_' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
        '*' => [0b00000, 0b10101, 0b01110, 0b11111, 0b01110, 0b10101, 0b00000],
        '!' => [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100],
        _ => [0; 7], // space and anything unsupported render blank
    }
}

fn put_pixel(buf: &mut [u8], pitch: u32, x: u32, y: u32, color: (u8, u8, u8)) {
    let offset = (y * pitch + x * 4) as usize;
    if offset + 4 <= buf.len() {
        buf[offset] = color.2;
        buf[offset + 1] = color.1;
        buf[offset + 2] = color.0;
        buf[offset + 3] = 0;
    }
}

pub fn draw_char(buf: &mut [u8], pitch: u32, x: u32, y: u32, scale: u32, color: (u8, u8, u8), c: char) {
    let bits = glyph(c);
    for (row, bits_row) in bits.iter().enumerate() {
        for col in 0..GLYPH_WIDTH {
            if (bits_row >> (GLYPH_WIDTH - 1 - col)) & 1 == 0 {
                continue;
            }
            let px = x + col * scale;
            let py = y + row as u32 * scale;
            for dy in 0..scale {
                for dx in 0..scale {
                    put_pixel(buf, pitch, px + dx, py + dy, color);
                }
            }
        }
    }
}

pub fn draw_text(buf: &mut [u8], pitch: u32, x: u32, y: u32, scale: u32, color: (u8, u8, u8), text: &str) {
    let advance = (GLYPH_WIDTH + 1) * scale;
    for (i, c) in text.chars().enumerate() {
        draw_char(buf, pitch, x + i as u32 * advance, y, scale, color, c);
    }
}

pub fn text_width(text: &str, scale: u32) -> u32 {
    let advance = (GLYPH_WIDTH + 1) * scale;
    text.chars().count() as u32 * advance
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not a pass/fail assertion — there's no way to view real DRM scanout
    /// output from this environment, so glyph correctness is checked by
    /// rendering to a PNG and looking at it. Run explicitly with:
    ///   cargo test -p focaldesk-greeter --test-threads=1 -- --ignored render_preview
    /// then view the PNG written to the path printed on stdout.
    #[test]
    #[ignore = "writes a preview PNG for manual visual inspection, not an assertion"]
    fn render_preview() {
        let scale = 4;
        let width = text_width("ABCDEFGHIJKLM 0123456789 !*:.-_", scale) + 20;
        let height = (GLYPH_HEIGHT * scale) * 3 + 40;
        let pitch = width * 4;
        let mut buf = vec![0x20u8; (pitch * height) as usize];

        draw_text(&mut buf, pitch, 10, 10, scale, (255, 255, 255), "ABCDEFGHIJKLMNOPQRSTUVWXYZ");
        draw_text(
            &mut buf,
            pitch,
            10,
            10 + (GLYPH_HEIGHT * scale) + 10,
            scale,
            (255, 255, 255),
            "0123456789 !*:.-_",
        );
        draw_text(
            &mut buf,
            pitch,
            10,
            10 + (GLYPH_HEIGHT * scale + 10) * 2,
            scale,
            (100, 200, 255),
            "Password:",
        );

        // buf is BGRX per pixel; image::Rgba wants RGBA, so swap R/B on the way out.
        let mut rgba = vec![0u8; buf.len()];
        for px in 0..(width * height) as usize {
            rgba[px * 4] = buf[px * 4 + 2];
            rgba[px * 4 + 1] = buf[px * 4 + 1];
            rgba[px * 4 + 2] = buf[px * 4];
            rgba[px * 4 + 3] = 255;
        }

        let path = std::env::temp_dir().join("focaldesk_greeter_font_preview.png");
        image::save_buffer(&path, &rgba, width, height, image::ColorType::Rgba8)
            .expect("failed to save preview PNG");
        println!("wrote font preview to {}", path.display());
    }
}
