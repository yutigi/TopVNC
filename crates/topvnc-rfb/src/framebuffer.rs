use crate::invalid;
use crate::protocol::{MAX_FRAMEBUFFER_DIMENSION, MAX_FRAMEBUFFER_PIXELS};
use std::io;

/// Reject zero, oversized, or too many pixels before anything is allocated.
pub fn validate_framebuffer_size(width: u16, height: u16) -> io::Result<()> {
    if width == 0
        || height == 0
        || width > MAX_FRAMEBUFFER_DIMENSION
        || height > MAX_FRAMEBUFFER_DIMENSION
        || usize::from(width) * usize::from(height) > MAX_FRAMEBUFFER_PIXELS
    {
        return Err(invalid("invalid framebuffer dimensions"));
    }
    Ok(())
}

/// A framebuffer region the host changed, in framebuffer pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone)]
pub struct Framebuffer {
    width: u16,
    height: u16,
    pixels: Vec<u32>,
}

impl Framebuffer {
    pub fn new(width: u16, height: u16) -> io::Result<Self> {
        validate_framebuffer_size(width, height)
            .map_err(|_| invalid("framebuffer dimensions exceed limits"))?;
        Ok(Self {
            width,
            height,
            pixels: vec![0; usize::from(width) * usize::from(height)],
        })
    }

    /// Pixels as 0x00RRGGBB, row-major.
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Mutable 0x00RRGGBB pixels for hosts that render or capture directly.
    /// The buffer length is fixed by the framebuffer dimensions.
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
    }
    pub fn width(&self) -> usize {
        usize::from(self.width)
    }
    pub fn height(&self) -> usize {
        usize::from(self.height)
    }
    /// Width and height as RFB carries them.
    pub fn dimensions(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    pub fn apply_raw(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        bytes: &[u8],
    ) -> io::Result<()> {
        if width == 0
            || height == 0
            || usize::from(x) + usize::from(width) > self.width()
            || usize::from(y) + usize::from(height) > self.height()
        {
            return Err(invalid("rectangle is outside framebuffer"));
        }
        let row_width = usize::from(width);
        if bytes.len() != row_width * usize::from(height) * 4 {
            return Err(invalid("raw rectangle has incorrect byte length"));
        }
        let stride = self.width();
        for (row, source) in bytes.chunks_exact(row_width * 4).enumerate() {
            let dest_start = (usize::from(y) + row) * stride + usize::from(x);
            let target = &mut self.pixels[dest_start..dest_start + row_width];
            // Validated row slices let the compiler vectorize the conversion
            // without a separate bounds check for each pixel. Wire byte 3 is
            // padding, even when a server sends a nonzero value there.
            for (pixel, bgra) in target.iter_mut().zip(source.chunks_exact(4)) {
                *pixel = u32::from_le_bytes([bgra[0], bgra[1], bgra[2], 0]);
            }
        }
        Ok(())
    }
}
