//! The browser's authoritative framebuffer, in the RGBA layout a canvas
//! `ImageData` uses, and the damage not yet presented. Every rectangle is
//! applied here as it is decoded; presentation may skip frames, but the
//! accumulated damage always covers every changed pixel since the last
//! presentation, so a skipped frame never leaves stale pixels.

use std::io;
use topvnc_rfb::{DamageRect, validate_framebuffer_size};

/// Damage rectangles kept before they are merged into their bounding box.
const MAX_DAMAGE_RECTS: usize = 32;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub struct RgbaFrame {
    width: u16,
    height: u16,
    pixels: Vec<u8>,
    damage: Vec<DamageRect>,
}

impl RgbaFrame {
    pub fn new(width: u16, height: u16, max_pixels: usize) -> io::Result<Self> {
        let mut frame = Self {
            width: 0,
            height: 0,
            pixels: Vec::new(),
            damage: Vec::new(),
        };
        frame.resize(width, height, max_pixels)?;
        Ok(frame)
    }

    /// A new, black framebuffer of this size, all of it damaged.
    pub fn resize(&mut self, width: u16, height: u16, max_pixels: usize) -> io::Result<()> {
        validate_framebuffer_size(width, height)?;
        let pixels = usize::from(width) * usize::from(height);
        if pixels > max_pixels {
            return Err(invalid("framebuffer is larger than this viewer accepts"));
        }
        self.pixels.clear();
        // Opaque black.
        self.pixels.resize(pixels * 4, 0);
        for alpha in self.pixels.iter_mut().skip(3).step_by(4) {
            *alpha = 255;
        }
        (self.width, self.height) = (width, height);
        self.damage.clear();
        self.damage.push(DamageRect {
            x: 0,
            y: 0,
            width,
            height,
        });
        Ok(())
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Apply 32-bit B, G, R, X pixels, as rectangles decode to.
    pub fn apply_bgrx(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        bytes: &[u8],
    ) -> io::Result<()> {
        let row = usize::from(width) * 4;
        if width == 0
            || height == 0
            || u32::from(x) + u32::from(width) > u32::from(self.width)
            || u32::from(y) + u32::from(height) > u32::from(self.height)
        {
            return Err(invalid("rectangle is outside framebuffer"));
        }
        if bytes.len() != row * usize::from(height) {
            return Err(invalid("rectangle has incorrect byte length"));
        }
        let stride = usize::from(self.width) * 4;
        for (line, source) in bytes.chunks_exact(row).enumerate() {
            let start = (usize::from(y) + line) * stride + usize::from(x) * 4;
            let target = &mut self.pixels[start..start + row];
            for (rgba, bgrx) in target.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
                rgba.copy_from_slice(&[bgrx[2], bgrx[1], bgrx[0], 255]);
            }
        }
        self.add_damage(DamageRect {
            x,
            y,
            width,
            height,
        });
        Ok(())
    }

    fn add_damage(&mut self, rect: DamageRect) {
        let contains = |outer: &DamageRect, inner: &DamageRect| {
            outer.x <= inner.x
                && outer.y <= inner.y
                && u32::from(outer.x) + u32::from(outer.width)
                    >= u32::from(inner.x) + u32::from(inner.width)
                && u32::from(outer.y) + u32::from(outer.height)
                    >= u32::from(inner.y) + u32::from(inner.height)
        };
        if self.damage.iter().any(|existing| contains(existing, &rect)) {
            return;
        }
        self.damage.retain(|existing| !contains(&rect, existing));
        self.damage.push(rect);
        if self.damage.len() > MAX_DAMAGE_RECTS {
            let union = bounding_box(&self.damage);
            self.damage.clear();
            self.damage.push(union);
        }
    }

    /// The regions changed since the last call, at most
    /// `MAX_DAMAGE_RECTS`, covering every changed pixel.
    pub fn take_damage(&mut self) -> Vec<DamageRect> {
        std::mem::take(&mut self.damage)
    }

    pub fn has_damage(&self) -> bool {
        !self.damage.is_empty()
    }
}

fn bounding_box(rects: &[DamageRect]) -> DamageRect {
    let left = rects.iter().map(|r| r.x).min().unwrap_or(0);
    let top = rects.iter().map(|r| r.y).min().unwrap_or(0);
    let right = rects
        .iter()
        .map(|r| u32::from(r.x) + u32::from(r.width))
        .max()
        .unwrap_or(0);
    let bottom = rects
        .iter()
        .map(|r| u32::from(r.y) + u32::from(r.height))
        .max()
        .unwrap_or(0);
    DamageRect {
        x: left,
        y: top,
        width: (right - u32::from(left)) as u16,
        height: (bottom - u32::from(top)) as u16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, width: u16, height: u16) -> DamageRect {
        DamageRect {
            x,
            y,
            width,
            height,
        }
    }

    fn covered(damage: &[DamageRect], x: u16, y: u16) -> bool {
        damage
            .iter()
            .any(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
    }

    #[test]
    fn converts_bgrx_to_opaque_rgba_and_checks_bounds() {
        let mut frame = RgbaFrame::new(3, 2, 64).unwrap();
        assert_eq!(frame.take_damage(), [rect(0, 0, 3, 2)]);
        frame
            .apply_bgrx(1, 1, 2, 1, &[1, 2, 3, 0, 4, 5, 6, 99])
            .unwrap();
        assert_eq!(&frame.pixels()[16..24], &[3, 2, 1, 255, 6, 5, 4, 255]);
        assert_eq!(&frame.pixels()[..4], &[0, 0, 0, 255]);
        assert_eq!(frame.take_damage(), [rect(1, 1, 2, 1)]);
        assert!(frame.apply_bgrx(2, 1, 2, 1, &[0; 8]).is_err());
        assert!(frame.apply_bgrx(0, 0, 1, 1, &[0; 3]).is_err());
        assert!(frame.apply_bgrx(0, 0, 0, 1, &[]).is_err());
        assert!(!frame.has_damage());
        assert!(RgbaFrame::new(9, 9, 64).is_err());
        assert!(frame.resize(0, 5, 64).is_err());
    }

    #[test]
    fn damage_is_bounded_but_covers_every_changed_pixel() {
        let mut frame = RgbaFrame::new(200, 200, 40_000).unwrap();
        frame.take_damage();
        let mut changed = Vec::new();
        for index in 0..100u16 {
            let (x, y) = ((index * 37) % 190, (index * 53) % 190);
            frame.apply_bgrx(x, y, 2, 3, &[7; 24]).unwrap();
            changed.push((x, y));
            // Presentation is skipped: damage keeps accumulating.
        }
        let damage = frame.take_damage();
        assert!(damage.len() <= MAX_DAMAGE_RECTS);
        for (x, y) in changed {
            assert!(covered(&damage, x + 1, y + 2));
        }
        // Contained rectangles are not added twice.
        frame.apply_bgrx(10, 10, 5, 5, &[0; 100]).unwrap();
        frame.apply_bgrx(11, 11, 2, 2, &[0; 16]).unwrap();
        assert_eq!(frame.take_damage(), [rect(10, 10, 5, 5)]);
        frame.apply_bgrx(11, 11, 2, 2, &[0; 16]).unwrap();
        frame.apply_bgrx(10, 10, 5, 5, &[0; 100]).unwrap();
        assert_eq!(frame.take_damage(), [rect(10, 10, 5, 5)]);
    }
}
