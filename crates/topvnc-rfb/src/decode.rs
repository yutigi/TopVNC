//! Rectangle decoding: pixels for complete rectangles from [`MessageReader`].
//!
//! [`MessageReader`]: crate::MessageReader

use crate::protocol::{RAW_ENCODING, ZLIB_ENCODING};
use crate::tight::{TightDecoder, TightRect, decode_jpeg_rect};
use crate::{Rect, invalid};
use flate2::{Decompress, FlushDecompress};
use std::io;

/// Where a decoded rectangle's 32-bit B, G, R, X pixels are.
pub enum Decoded<'a> {
    /// In place in the received data (Raw).
    Wire(&'a [u8]),
    /// In the output buffer passed to [`RectDecoder::decode`].
    Output,
    /// Not decoded yet: Tight JPEG data for [`decode_jpeg_rect`], which
    /// needs no decoder state and so can run on another thread.
    Jpeg(&'a [u8]),
}

/// Decoder state that persists across rectangles and updates: the Zlib
/// stream and Tight's four zlib streams. Rectangles must be decoded in wire
/// order.
pub struct RectDecoder {
    zlib: Decompress,
    tight: TightDecoder,
}

impl Default for RectDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl RectDecoder {
    pub fn new() -> Self {
        Self {
            zlib: Decompress::new(true),
            tight: TightDecoder::new(),
        }
    }

    /// Decode `rect`, leaving JPEG data undecoded.
    pub fn decode<'a>(&mut self, rect: &Rect<'a>, output: &mut Vec<u8>) -> io::Result<Decoded<'a>> {
        let (width, height) = (usize::from(rect.width), usize::from(rect.height));
        match rect.encoding {
            RAW_ENCODING => Ok(Decoded::Wire(rect.body)),
            ZLIB_ENCODING => {
                let length = width * height * 4;
                output.resize(length, 0);
                let compressed = &rect.body[4..];
                let zlib = &mut self.zlib;
                let input_before = zlib.total_in();
                let output_before = zlib.total_out();
                zlib.decompress(compressed, output, FlushDecompress::Sync)
                    .map_err(|_| invalid("invalid zlib rectangle"))?;
                if zlib.total_in() - input_before != compressed.len() as u64
                    || zlib.total_out() - output_before != length as u64
                {
                    return Err(invalid("zlib rectangle has incorrect decoded length"));
                }
                Ok(Decoded::Output)
            }
            _ => match self.tight.decode(rect.body, width, height, output)? {
                TightRect::Pixels => Ok(Decoded::Output),
                TightRect::Jpeg(jpeg) => Ok(Decoded::Jpeg(jpeg)),
            },
        }
    }

    /// Decode `rect` completely on this thread, including JPEG data, and
    /// return its pixels.
    pub fn decode_all<'a>(
        &mut self,
        rect: &Rect<'a>,
        output: &'a mut Vec<u8>,
    ) -> io::Result<&'a [u8]> {
        match self.decode(rect, output)? {
            Decoded::Wire(pixels) => Ok(pixels),
            Decoded::Output => Ok(output),
            Decoded::Jpeg(jpeg) => {
                decode_jpeg_rect(
                    jpeg,
                    usize::from(rect.width),
                    usize::from(rect.height),
                    output,
                )?;
                Ok(output)
            }
        }
    }
}
