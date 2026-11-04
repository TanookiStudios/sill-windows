//! The PC's pointer as a device draws it: its shape (kind 14), so the iPad's pointer turns into an
//! I-beam over text as it does here, as PNG with its hotspot.

/// The three kinds of pointer image Windows hands out.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Shape {
    /// 1 bit per pixel: an AND mask above an XOR mask, so `height` counts both.
    Monochrome,
    /// BGRA.
    Color,
    /// BGRA whose alpha says how to mix: 0 replaces the screen, 255 inverts where the color is set.
    MaskedColor,
}

/// RGBA for any of them, and its real height. A pixel that would invert the screen has no fixed color,
/// so it's drawn black: the I-beam, the commonest such pointer, stays visible over most pages.
pub fn to_rgba(shape: Shape, width: usize, height: usize, pitch: usize, data: &[u8]) -> Option<(Vec<u8>, usize)> {
    let h = if shape == Shape::Monochrome { height / 2 } else { height };
    if width == 0 || h == 0 || data.len() < pitch.checked_mul(height)? {
        return None;
    }
    let mut out = Vec::with_capacity(width * h * 4);
    for y in 0..h {
        for x in 0..width {
            let px = match shape {
                Shape::Monochrome => {
                    let bit = |row: usize| data[row * pitch + x / 8] & (0x80 >> (x % 8)) != 0;
                    match (bit(y), bit(y + h)) {
                        (false, false) => [0, 0, 0, 255],
                        (false, true) => [255, 255, 255, 255],
                        (true, false) => [0, 0, 0, 0],
                        (true, true) => [0, 0, 0, 255],
                    }
                }
                _ => {
                    let p = &data[y * pitch + x * 4..y * pitch + x * 4 + 4];
                    match (shape, p[3]) {
                        (Shape::Color, a) => [p[2], p[1], p[0], a],
                        (_, 0) => [p[2], p[1], p[0], 255],
                        _ if p[..3] == [0, 0, 0] => [0, 0, 0, 0],
                        _ => [0, 0, 0, 255],
                    }
                }
            };
            out.extend_from_slice(&px);
        }
    }
    Some((out, h))
}

pub fn png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut e = png::Encoder::new(&mut out, width, height);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    let mut w = e.write_header().ok()?;
    w.write_image_data(rgba).ok()?;
    w.finish().ok()?;
    Some(out)
}

/// CursorShapeBlob: hotX, hotY, width, height as big-endian floats in points, then the PNG.
pub fn blob(hot_x: f32, hot_y: f32, width: f32, height: f32, png: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(16 + png.len());
    for v in [hot_x, hot_y, width, height] {
        d.extend_from_slice(&v.to_bits().to_be_bytes());
    }
    d.extend_from_slice(png);
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monochrome() {
        // 2×1: AND row 0b01, XOR row 0b01: first pixel black over (0,0), second inverts (1,1).
        let (px, h) = to_rgba(Shape::Monochrome, 2, 2, 1, &[0b0100_0000, 0b0100_0000]).unwrap();
        assert_eq!(h, 1);
        assert_eq!(px, vec![0, 0, 0, 255, 0, 0, 0, 255]);
        let (px, _) = to_rgba(Shape::Monochrome, 2, 2, 1, &[0b1000_0000, 0b0100_0000]).unwrap();
        assert_eq!(px, vec![0, 0, 0, 0, 255, 255, 255, 255], "transparent, then white");
    }

    #[test]
    fn color_and_masked() {
        let (px, _) = to_rgba(Shape::Color, 1, 1, 4, &[10, 20, 30, 128]).unwrap();
        assert_eq!(px, vec![30, 20, 10, 128]);
        let (px, _) = to_rgba(Shape::MaskedColor, 3, 1, 12, &[1, 2, 3, 0, 0, 0, 0, 255, 9, 9, 9, 255]).unwrap();
        assert_eq!(px, vec![3, 2, 1, 255, 0, 0, 0, 0, 0, 0, 0, 255]);
        assert!(to_rgba(Shape::Color, 2, 2, 8, &[0; 4]).is_none(), "short data is refused");
    }
}
