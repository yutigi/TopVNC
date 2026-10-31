//! Tight encoding (RFB encoding 7): the client decoder, the body layout the
//! message reader uses to find rectangle boundaries, and the numbers the
//! server-side encoder shares.
//!
//! Only the 24-bit true-color case is implemented, where a "TPIXEL" is three
//! bytes in red, green, blue order.

use crate::invalid;
use flate2::{Decompress, FlushDecompress};
use std::io;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

pub const TIGHT_ENCODING: i32 = 7;
/// Pseudo-encodings -32..=-23 select a JPEG quality level from 0 to 9.
pub const QUALITY_LEVEL_0: i32 = -32;
/// Pseudo-encodings -256..=-247 select a compression level from 0 to 9.
pub const COMPRESS_LEVEL_0: i32 = -256;
/// Tight decoders, including TigerVNC's, reject wider rectangles.
pub const MAX_RECT_WIDTH: usize = 2048;
/// Rectangles are split into bands this tall so they can be encoded in
/// parallel and every compressed length fits the 22-bit compact length.
pub const MAX_RECT_HEIGHT: usize = 256;
/// Basic-compression data shorter than this is sent without zlib.
pub const MIN_TO_COMPRESS: usize = 12;
/// Compact lengths carry at most 22 bits.
pub const MAX_COMPACT_LENGTH: usize = (1 << 22) - 1;

pub const FILL: u8 = 0x08;
pub const JPEG: u8 = 0x09;
pub const EXPLICIT_FILTER: u8 = 0x04;
pub const FILTER_COPY: u8 = 0;
pub const FILTER_PALETTE: u8 = 1;
pub const FILTER_GRADIENT: u8 = 2;

pub fn push_compact_length(output: &mut Vec<u8>, length: usize) {
    debug_assert!(length <= MAX_COMPACT_LENGTH);
    let mut byte = (length & 0x7f) as u8;
    if length > 0x7f {
        output.push(byte | 0x80);
        byte = ((length >> 7) & 0x7f) as u8;
        if length > 0x3fff {
            output.push(byte | 0x80);
            byte = (length >> 14) as u8;
        }
    }
    output.push(byte);
}

/// A compact length at the start of `bytes` and the bytes it uses, or
/// `None` while it is incomplete.
pub fn read_compact_length(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut length = 0;
    for index in 0..3 {
        let byte = *bytes.get(index)?;
        if index == 2 {
            return Some((length | usize::from(byte) << 14, 3));
        }
        length |= usize::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return Some((length, index + 1));
        }
    }
    unreachable!("a compact length has at most three bytes")
}

/// Where the parts of one Tight rectangle body lie.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    control: u8,
    kind: Kind,
    /// The whole body's length.
    length: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Fill,
    /// JPEG data at `start`.
    Jpeg {
        start: usize,
    },
    Basic {
        stream: usize,
        filter: u8,
        /// Palette colors start at byte 3 when there is a palette.
        palette_size: usize,
        /// Filtered pixel data, after inflating when `compressed`.
        data_length: usize,
        data_start: usize,
        compressed: bool,
    },
}

/// Parse a body's structure from the bytes received so far. `Ok(None)`
/// means more bytes are needed; an error means the body is malformed. The
/// lengths it reads are bounded by the 22-bit compact length and the
/// rectangle size, so the caller can bound its buffer before waiting.
fn layout(bytes: &[u8], width: usize, height: usize) -> io::Result<Option<Layout>> {
    let Some(&control) = bytes.first() else {
        return Ok(None);
    };
    let kind = control >> 4;
    if kind == FILL {
        return Ok(Some(Layout {
            control,
            kind: Kind::Fill,
            length: 4,
        }));
    }
    if kind == JPEG {
        let Some((length, used)) = read_compact_length(&bytes[1..]) else {
            return Ok(None);
        };
        return Ok(Some(Layout {
            control,
            kind: Kind::Jpeg { start: 1 + used },
            length: 1 + used + length,
        }));
    }
    if kind > JPEG {
        return Err(invalid("unsupported Tight compression type"));
    }
    let mut position = 1;
    let filter = if kind & EXPLICIT_FILTER != 0 {
        let Some(&filter) = bytes.get(position) else {
            return Ok(None);
        };
        position += 1;
        filter
    } else {
        FILTER_COPY
    };
    let mut palette_size = 0;
    let data_length = match filter {
        FILTER_COPY | FILTER_GRADIENT => width * height * 3,
        FILTER_PALETTE => {
            let Some(&size) = bytes.get(position) else {
                return Ok(None);
            };
            palette_size = usize::from(size) + 1;
            position += 1 + palette_size * 3;
            if palette_size <= 2 {
                width.div_ceil(8) * height
            } else {
                width * height
            }
        }
        _ => return Err(invalid("unsupported Tight filter")),
    };
    let stream = usize::from(kind & 0x03);
    if data_length < MIN_TO_COMPRESS {
        return Ok(Some(Layout {
            control,
            kind: Kind::Basic {
                stream,
                filter,
                palette_size,
                data_length,
                data_start: position,
                compressed: false,
            },
            length: position + data_length,
        }));
    }
    let Some((length, used)) = bytes.get(position..).and_then(read_compact_length) else {
        return Ok(None);
    };
    Ok(Some(Layout {
        control,
        kind: Kind::Basic {
            stream,
            filter,
            palette_size,
            data_length,
            data_start: position + used,
            compressed: true,
        },
        length: position + used + length,
    }))
}

/// The length of the Tight rectangle body at the start of `bytes`, or
/// `None` while too few bytes have arrived to tell.
pub fn body_length(bytes: &[u8], width: usize, height: usize) -> io::Result<Option<usize>> {
    Ok(layout(bytes, width, height)?.map(|layout| layout.length))
}

/// A decoded Tight rectangle.
pub enum TightRect<'a> {
    /// `output` holds the pixels.
    Pixels,
    /// JPEG data, for [`decode_jpeg_rect`]. It needs no decoder state, so it
    /// can run on another thread.
    Jpeg(&'a [u8]),
}

/// Client-side Tight state: four persistent zlib streams.
pub struct TightDecoder {
    streams: [Decompress; 4],
    data: Vec<u8>,
}

impl Default for TightDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl TightDecoder {
    pub fn new() -> Self {
        Self {
            streams: std::array::from_fn(|_| Decompress::new(true)),
            data: Vec::new(),
        }
    }

    /// Decode one complete Tight rectangle body. Pixels are written to
    /// `output` as 32-bit B, G, R, X bytes, the layout Raw rectangles use,
    /// except that JPEG data is returned undecoded.
    pub fn decode<'a>(
        &mut self,
        body: &'a [u8],
        width: usize,
        height: usize,
        output: &mut Vec<u8>,
    ) -> io::Result<TightRect<'a>> {
        let layout = layout(body, width, height)?
            .filter(|layout| layout.length == body.len())
            .ok_or_else(|| invalid("Tight rectangle has an incorrect length"))?;
        for (index, stream) in self.streams.iter_mut().enumerate() {
            if layout.control & (1 << index) != 0 {
                stream.reset(true);
            }
        }
        let (stream, filter, palette_size, data_length, data_start, compressed) = match layout.kind
        {
            Kind::Jpeg { start } => return Ok(TightRect::Jpeg(&body[start..])),
            Kind::Fill => {
                output.resize(width * height * 4, 0);
                let pixel = [body[3], body[2], body[1], 0];
                for target in output.chunks_exact_mut(4) {
                    target.copy_from_slice(&pixel);
                }
                return Ok(TightRect::Pixels);
            }
            Kind::Basic {
                stream,
                filter,
                palette_size,
                data_length,
                data_start,
                compressed,
            } => (
                stream,
                filter,
                palette_size,
                data_length,
                data_start,
                compressed,
            ),
        };
        output.resize(width * height * 4, 0);
        let data = if compressed {
            self.data.resize(data_length, 0);
            inflate_exact(
                &mut self.streams[stream],
                &body[data_start..],
                &mut self.data,
            )?;
            &self.data[..]
        } else {
            &body[data_start..]
        };
        match filter {
            FILTER_COPY => {
                for (target, source) in output.chunks_exact_mut(4).zip(data.chunks_exact(3)) {
                    target.copy_from_slice(&[source[2], source[1], source[0], 0]);
                }
            }
            FILTER_PALETTE => {
                let colors = &body[3..3 + palette_size * 3];
                let mut put = |pixel: usize, index: usize| -> io::Result<()> {
                    let color = colors
                        .get(index * 3..index * 3 + 3)
                        .filter(|_| index < palette_size)
                        .ok_or_else(|| invalid("Tight palette index out of range"))?;
                    output[pixel * 4..pixel * 4 + 4]
                        .copy_from_slice(&[color[2], color[1], color[0], 0]);
                    Ok(())
                };
                if palette_size <= 2 {
                    let row_bytes = width.div_ceil(8);
                    for row in 0..height {
                        for column in 0..width {
                            let bit = data[row * row_bytes + column / 8] >> (7 - column % 8) & 1;
                            put(row * width + column, usize::from(bit))?;
                        }
                    }
                } else {
                    for (pixel, index) in data.iter().enumerate() {
                        put(pixel, usize::from(*index))?;
                    }
                }
            }
            _ => {
                // Gradient: each channel was sent as the difference from
                // left + up - up-left, clamped to 0..=255.
                let row_length = width * 3;
                for row in 0..height {
                    for column in 0..width {
                        for channel in 0..3 {
                            let at = |r: usize, c: usize| {
                                i32::from(output[(r * width + c) * 4 + 2 - channel])
                            };
                            let left = if column > 0 { at(row, column - 1) } else { 0 };
                            let up = if row > 0 { at(row - 1, column) } else { 0 };
                            let up_left = if row > 0 && column > 0 {
                                at(row - 1, column - 1)
                            } else {
                                0
                            };
                            let estimate = (left + up - up_left).clamp(0, 255) as u8;
                            let value = data[row * row_length + column * 3 + channel]
                                .wrapping_add(estimate);
                            output[(row * width + column) * 4 + 2 - channel] = value;
                        }
                    }
                }
            }
        }
        Ok(TightRect::Pixels)
    }

    /// Decode one complete body, including JPEG data, to `output`.
    pub fn decode_all(
        &mut self,
        body: &[u8],
        width: usize,
        height: usize,
        output: &mut Vec<u8>,
    ) -> io::Result<()> {
        match self.decode(body, width, height, output)? {
            TightRect::Pixels => Ok(()),
            TightRect::Jpeg(jpeg) => decode_jpeg_rect(jpeg, width, height, output),
        }
    }
}

/// Inflate `compressed` with a persistent stream and require exactly
/// `output.len()` bytes out.
fn inflate_exact(stream: &mut Decompress, compressed: &[u8], output: &mut [u8]) -> io::Result<()> {
    let input_start = stream.total_in();
    let output_start = stream.total_out();
    // One spare byte detects a stream that produces more than announced,
    // including output the inflater holds back after consuming all input.
    let mut spare = [0u8; 1];
    loop {
        let consumed = (stream.total_in() - input_start) as usize;
        let produced = (stream.total_out() - output_start) as usize;
        if produced > output.len() {
            break;
        }
        let before = (stream.total_in(), stream.total_out());
        let result = if produced < output.len() {
            stream.decompress(
                &compressed[consumed..],
                &mut output[produced..],
                FlushDecompress::Sync,
            )
        } else {
            stream.decompress(&compressed[consumed..], &mut spare, FlushDecompress::Sync)
        };
        result.map_err(|_| invalid("invalid Tight zlib data"))?;
        if (stream.total_in(), stream.total_out()) == before {
            break;
        }
    }
    if stream.total_in() - input_start != compressed.len() as u64
        || stream.total_out() - output_start != output.len() as u64
    {
        return Err(invalid("Tight zlib data has an incorrect length"));
    }
    Ok(())
}

/// Decode a Tight JPEG rectangle's data to 32-bit B, G, R, X bytes in
/// `output`, which is resized to fit. Rejects data whose size is not
/// `width` by `height`.
pub fn decode_jpeg_rect(
    jpeg: &[u8],
    width: usize,
    height: usize,
    output: &mut Vec<u8>,
) -> io::Result<()> {
    output.resize(width * height * 4, 0);
    let options = DecoderOptions::new_fast()
        .set_max_width(width)
        .set_max_height(height)
        .jpeg_set_out_colorspace(ColorSpace::BGRA);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
    decoder
        .decode_headers()
        .map_err(|_| invalid("invalid Tight JPEG rectangle"))?;
    if decoder.dimensions() != Some((width, height)) {
        return Err(invalid("Tight JPEG size does not match its rectangle"));
    }
    decoder
        .decode_into(output)
        .map_err(|_| invalid("invalid Tight JPEG rectangle"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compress, Compression, FlushCompress};

    fn decode(body: &[u8], width: usize, height: usize) -> io::Result<Vec<u32>> {
        let mut output = Vec::new();
        TightDecoder::new().decode_all(body, width, height, &mut output)?;
        Ok(output
            .chunks_exact(4)
            .map(|pixel| u32::from_le_bytes([pixel[0], pixel[1], pixel[2], 0]))
            .collect())
    }

    fn compressed_body(prefix: &[u8], data: &[u8]) -> Vec<u8> {
        let mut compressor = Compress::new(Compression::fast(), true);
        let mut compressed = Vec::with_capacity(data.len() + 128);
        compressor
            .compress_vec(data, &mut compressed, FlushCompress::Sync)
            .unwrap();
        let mut body = prefix.to_vec();
        push_compact_length(&mut body, compressed.len());
        body.extend_from_slice(&compressed);
        body
    }

    #[test]
    fn compact_lengths_use_one_to_three_bytes() {
        for (length, bytes) in [
            (0, vec![0]),
            (127, vec![0x7f]),
            (128, vec![0x80, 0x01]),
            (16_383, vec![0xff, 0x7f]),
            (16_384, vec![0x80, 0x80, 0x01]),
            (MAX_COMPACT_LENGTH, vec![0xff, 0xff, 0xff]),
        ] {
            let mut output = Vec::new();
            push_compact_length(&mut output, length);
            assert_eq!(output, bytes, "{length}");
            assert_eq!(read_compact_length(&output), Some((length, bytes.len())));
            assert_eq!(read_compact_length(&output[..bytes.len() - 1]), None);
        }
    }

    #[test]
    fn body_length_needs_only_the_headers_and_matches_every_kind() {
        let palette = [0x40, FILTER_PALETTE, 2, 0, 0, 0, 1, 1, 1, 2, 2, 2, 0, 1, 2];
        let bodies: Vec<(Vec<u8>, usize, usize)> = vec![
            (vec![0x80, 1, 2, 3], 5, 5),
            (vec![0x40, FILTER_COPY, 1, 2, 3, 4, 5, 6, 7, 8, 9], 3, 1),
            (vec![0x00, 1, 2, 3, 4, 5, 6], 2, 1),
            (palette.to_vec(), 3, 1),
            (compressed_body(&[0x40, FILTER_GRADIENT], &[7; 48]), 4, 4),
            (
                compressed_body(&[0x41, FILTER_PALETTE, 1, 0, 0, 0, 9, 9, 9], &[0xaa; 16]),
                13,
                8,
            ),
            (vec![0x90, 3, 0xff, 0xd8, 0xff], 16, 16),
        ];
        for (body, width, height) in bodies {
            assert_eq!(body_length(&body, width, height).unwrap(), Some(body.len()));
            // Any prefix either is too short to tell or already knows the
            // same length.
            for cut in 0..body.len() {
                let known = body_length(&body[..cut], width, height).unwrap();
                assert!(
                    known.is_none() || known == Some(body.len()),
                    "{body:?} at {cut}"
                );
            }
            // Trailing data belongs to the next rectangle.
            let mut longer = body.clone();
            longer.extend_from_slice(&[0x80, 0, 0, 0]);
            assert_eq!(
                body_length(&longer, width, height).unwrap(),
                Some(body.len())
            );
        }
    }

    #[test]
    fn short_basic_data_is_sent_uncompressed() {
        let body = [0x40, FILTER_COPY, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        assert_eq!(
            decode(&body, 3, 1).unwrap(),
            vec![0x010203, 0x040506, 0x070809]
        );
        assert_eq!(
            decode(&[0x00, 1, 2, 3, 4, 5, 6], 2, 1).unwrap(),
            vec![0x010203, 0x040506]
        );
    }

    #[test]
    fn fill_and_small_palettes_decode() {
        assert_eq!(
            decode(&[0x80, 0x12, 0x34, 0x56], 2, 2).unwrap(),
            vec![0x123456; 4]
        );
        // Two colors, one bit per pixel.
        assert_eq!(
            decode(
                &[0x40, FILTER_PALETTE, 1, 1, 2, 3, 4, 5, 6, 0b1010_0000],
                3,
                1
            )
            .unwrap(),
            vec![0x040506, 0x010203, 0x040506]
        );
    }

    #[test]
    fn gradient_filter_reconstructs_from_neighbor_estimates() {
        let mut residuals = Vec::new();
        for residual in [10u8, 10, 20, 5] {
            residuals.extend_from_slice(&[residual; 3]);
        }
        let body = compressed_body(&[0x40, FILTER_GRADIENT], &residuals);
        assert_eq!(
            decode(&body, 2, 2).unwrap(),
            vec![0x0a0a0a, 0x141414, 0x1e1e1e, 0x2d2d2d]
        );
    }

    #[test]
    fn streams_persist_until_the_server_resets_them() {
        let mut compressor = Compress::new(Compression::fast(), true);
        let mut decoder = TightDecoder::new();
        let mut output = Vec::new();
        for value in [1u8, 2] {
            let mut compressed = Vec::with_capacity(128);
            compressor
                .compress_vec(&[value; 12], &mut compressed, FlushCompress::Sync)
                .unwrap();
            // Stream 2, no reset, implied copy filter, four pixels.
            let mut body = vec![0x02 << 4];
            push_compact_length(&mut body, compressed.len());
            body.extend_from_slice(&compressed);
            decoder.decode_all(&body, 4, 1, &mut output).unwrap();
            assert_eq!(output[..4], [value, value, value, 0]);
        }
    }

    #[test]
    fn malformed_rectangles_are_rejected() {
        let rejected = |body: &[u8], width, height| {
            decode(body, width, height).unwrap_err().kind() == io::ErrorKind::InvalidData
        };
        assert!(rejected(&[0xa0], 1, 1));
        assert!(rejected(&[0x40, 3], 1, 1));
        assert!(rejected(
            &[0x40, FILTER_PALETTE, 2, 0, 0, 0, 1, 1, 1, 2, 2, 2, 3],
            1,
            1
        ));
        // Zlib data that inflates to the wrong length.
        assert!(rejected(
            &compressed_body(&[0x40, FILTER_COPY], &[0; 15]),
            4,
            1
        ));
        // A body shorter or longer than its layout.
        assert!(rejected(&[0x80, 1, 2], 1, 1));
        assert!(rejected(&[0x80, 1, 2, 3, 4], 1, 1));
        assert!(rejected(&[0x90, 4, 1, 2, 3, 4], 16, 16));
    }
}
