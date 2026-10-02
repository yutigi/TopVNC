//! Tight encoding (RFB encoding 7): the server-side encoder and the client
//! decoder.
//!
//! Only the 24-bit true-color case is implemented, where a "TPIXEL" is three
//! bytes in red, green, blue order. The server offers Tight only to clients
//! whose pixel format has three 8-bit channels in a 32-bit pixel.

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use jpeg_encoder::{ColorType, Encoder as JpegEncoder, SamplingFactor};
use std::io::{self, Read};
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

pub(crate) const TIGHT_ENCODING: i32 = 7;
/// Pseudo-encodings -32..=-23 select a JPEG quality level from 0 to 9.
pub(crate) const QUALITY_LEVEL_0: i32 = -32;
/// Pseudo-encodings -256..=-247 select a compression level from 0 to 9.
pub(crate) const COMPRESS_LEVEL_0: i32 = -256;
/// Tight decoders, including TigerVNC's, reject wider rectangles.
pub(crate) const MAX_RECT_WIDTH: usize = 2048;
/// Rectangles are split into bands this tall so they can be encoded in
/// parallel and every compressed length fits the 22-bit compact length.
pub(crate) const MAX_RECT_HEIGHT: usize = 256;
/// Basic-compression data shorter than this is sent without zlib.
const MIN_TO_COMPRESS: usize = 12;
/// The largest palette the encoder uses; more colors are sent as JPEG or as
/// plain pixels.
const MAX_ENCODER_PALETTE: usize = 16;
/// Compact lengths carry at most 22 bits.
const MAX_COMPACT_LENGTH: usize = (1 << 22) - 1;

const FILL: u8 = 0x08;
const JPEG: u8 = 0x09;
const EXPLICIT_FILTER: u8 = 0x04;
const FILTER_COPY: u8 = 0;
const FILTER_PALETTE: u8 = 1;
const FILTER_GRADIENT: u8 = 2;

/// JPEG quality and chroma subsampling for each RFB quality level, the same
/// mapping TigerVNC uses.
const JPEG_LEVELS: [(u8, SamplingFactor); 10] = [
    (15, SamplingFactor::R_4_2_0),
    (29, SamplingFactor::R_4_2_0),
    (41, SamplingFactor::R_4_2_0),
    (42, SamplingFactor::R_4_2_2),
    (62, SamplingFactor::R_4_2_2),
    (77, SamplingFactor::R_4_2_2),
    (79, SamplingFactor::R_4_4_4),
    (86, SamplingFactor::R_4_4_4),
    (92, SamplingFactor::R_4_4_4),
    (100, SamplingFactor::R_4_4_4),
];

/// How the server encodes Tight rectangles for one client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TightSettings {
    /// RFB quality level 0-9. JPEG is used only when the client sent one.
    pub quality: Option<u8>,
    /// RFB compression level 0-9, used as the zlib level.
    pub compression: u8,
}

impl Default for TightSettings {
    fn default() -> Self {
        Self {
            quality: None,
            // Fast compression keeps encoding latency low; clients that want
            // smaller updates ask for a higher level.
            compression: 1,
        }
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn push_compact_length(output: &mut Vec<u8>, length: usize) {
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

fn read_compact_length(reader: &mut impl Read) -> io::Result<usize> {
    let mut length = 0;
    for index in 0..3 {
        let mut byte = [0];
        reader.read_exact(&mut byte)?;
        if index == 2 {
            return Ok(length | usize::from(byte[0]) << 14);
        }
        length |= usize::from(byte[0] & 0x7f) << (7 * index);
        if byte[0] & 0x80 == 0 {
            break;
        }
    }
    Ok(length)
}

fn rgb(pixel: u32) -> [u8; 3] {
    [(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8]
}

/// Encode `pixels` (0x00RRGGBB, `width` per row) as one Tight rectangle body:
/// everything after the rectangle header. The rectangle must be at most
/// [`MAX_RECT_WIDTH`] by [`MAX_RECT_HEIGHT`] pixels.
///
/// Every zlib-compressed rectangle resets and uses stream 0, so rectangles
/// can be encoded independently and in parallel.
pub(crate) fn encode_rect(
    pixels: &[u32],
    width: usize,
    height: usize,
    settings: TightSettings,
    output: &mut Vec<u8>,
) -> io::Result<()> {
    debug_assert_eq!(pixels.len(), width * height);
    debug_assert!(width <= MAX_RECT_WIDTH && height <= MAX_RECT_HEIGHT);
    let first = pixels[0];
    if pixels.iter().all(|pixel| *pixel == first) {
        output.push(FILL << 4);
        output.extend_from_slice(&rgb(first));
        return Ok(());
    }
    let mut palette = Vec::with_capacity(MAX_ENCODER_PALETTE);
    let mut few_colors = true;
    for pixel in pixels {
        if !palette.contains(pixel) {
            if palette.len() == MAX_ENCODER_PALETTE {
                few_colors = false;
                break;
            }
            palette.push(*pixel);
        }
    }
    // Text and flat interface elements stay lossless and sharp; photographic
    // and game content goes to JPEG when the client allows it.
    if few_colors {
        return encode_palette(pixels, width, height, &palette, settings, output);
    }
    if let Some(level) = settings.quality {
        return encode_jpeg(pixels, width, height, level, output);
    }
    output.push(EXPLICIT_FILTER << 4 | 1);
    output.push(FILTER_COPY);
    let mut data = Vec::with_capacity(pixels.len() * 3);
    for pixel in pixels {
        data.extend_from_slice(&rgb(*pixel));
    }
    write_basic_data(&data, settings.compression, output)
}

fn encode_palette(
    pixels: &[u32],
    width: usize,
    height: usize,
    palette: &[u32],
    settings: TightSettings,
    output: &mut Vec<u8>,
) -> io::Result<()> {
    // Stream 0 with a reset, explicit filter.
    output.push(EXPLICIT_FILTER << 4 | 1);
    output.push(FILTER_PALETTE);
    output.push((palette.len() - 1) as u8);
    for color in palette {
        output.extend_from_slice(&rgb(*color));
    }
    let index = |pixel: &u32| palette.iter().position(|color| color == pixel).unwrap() as u8;
    let data = if palette.len() == 2 {
        // One bit per pixel, rows padded to whole bytes, first pixel in the
        // most significant bit.
        let row_bytes = width.div_ceil(8);
        let mut data = vec![0; row_bytes * height];
        for (row, source) in pixels.chunks_exact(width).enumerate() {
            for (column, pixel) in source.iter().enumerate() {
                data[row * row_bytes + column / 8] |= index(pixel) << (7 - column % 8);
            }
        }
        data
    } else {
        pixels.iter().map(index).collect()
    };
    write_basic_data(&data, settings.compression, output)
}

fn write_basic_data(data: &[u8], level: u8, output: &mut Vec<u8>) -> io::Result<()> {
    if data.len() < MIN_TO_COMPRESS {
        output.extend_from_slice(data);
        return Ok(());
    }
    let mut compressor = Compress::new(Compression::new(u32::from(level.clamp(1, 9))), true);
    let mut compressed = Vec::with_capacity(data.len() / 2 + 64);
    loop {
        let consumed = compressor.total_in() as usize;
        if compressed.capacity() - compressed.len() < 64 {
            compressed.reserve(compressed.capacity().max(4096));
        }
        let status = compressor
            .compress_vec(&data[consumed..], &mut compressed, FlushCompress::Sync)
            .map_err(|_| io::Error::other("zlib compression failed"))?;
        // A sync flush is complete once all input is consumed and the output
        // buffer still has room left.
        if compressor.total_in() as usize == data.len()
            && compressed.len() < compressed.capacity()
            && status != Status::BufError
        {
            break;
        }
    }
    if compressed.len() > MAX_COMPACT_LENGTH {
        return Err(invalid("compressed Tight rectangle is too large"));
    }
    push_compact_length(output, compressed.len());
    output.extend_from_slice(&compressed);
    Ok(())
}

fn encode_jpeg(
    pixels: &[u32],
    width: usize,
    height: usize,
    level: u8,
    output: &mut Vec<u8>,
) -> io::Result<()> {
    let (quality, sampling) = JPEG_LEVELS[usize::from(level.min(9))];
    let mut bgra = Vec::with_capacity(pixels.len() * 4);
    for pixel in pixels {
        bgra.extend_from_slice(&pixel.to_le_bytes());
    }
    let mut jpeg = Vec::with_capacity(pixels.len() / 4);
    let mut encoder = JpegEncoder::new(&mut jpeg, quality);
    encoder.set_sampling_factor(sampling);
    encoder
        .encode(&bgra, width as u16, height as u16, ColorType::Bgra)
        .map_err(|error| io::Error::other(format!("JPEG encoding failed: {error}")))?;
    if jpeg.len() > MAX_COMPACT_LENGTH {
        return Err(invalid("JPEG rectangle is too large"));
    }
    output.push(JPEG << 4);
    push_compact_length(output, jpeg.len());
    output.extend_from_slice(&jpeg);
    Ok(())
}

/// Client-side Tight state: four persistent zlib streams.
pub(crate) struct TightDecoder {
    streams: [Decompress; 4],
    compressed: Vec<u8>,
    data: Vec<u8>,
}

impl TightDecoder {
    pub(crate) fn new() -> Self {
        Self {
            streams: std::array::from_fn(|_| Decompress::new(true)),
            compressed: Vec::new(),
            data: Vec::new(),
        }
    }

    /// Read one Tight rectangle body and write its pixels to `output` as
    /// 32-bit B, G, R, X bytes, the layout Raw rectangles use.
    pub(crate) fn read_rect(
        &mut self,
        reader: &mut impl Read,
        width: usize,
        height: usize,
        output: &mut Vec<u8>,
    ) -> io::Result<()> {
        let pixels = width * height;
        output.resize(pixels * 4, 0);
        let mut control = [0];
        reader.read_exact(&mut control)?;
        let control = control[0];
        for (index, stream) in self.streams.iter_mut().enumerate() {
            if control & (1 << index) != 0 {
                stream.reset(true);
            }
        }
        let kind = control >> 4;
        match kind {
            FILL => {
                let mut color = [0; 3];
                reader.read_exact(&mut color)?;
                let pixel = [color[2], color[1], color[0], 0];
                for target in output.chunks_exact_mut(4) {
                    target.copy_from_slice(&pixel);
                }
                Ok(())
            }
            JPEG => {
                let length = read_compact_length(reader)?;
                self.compressed.resize(length, 0);
                reader.read_exact(&mut self.compressed)?;
                decode_jpeg(&self.compressed, width, height, output)
            }
            kind if kind > JPEG => Err(invalid("unsupported Tight compression type")),
            kind => self.read_basic(reader, kind, width, height, output),
        }
    }

    fn read_basic(
        &mut self,
        reader: &mut impl Read,
        kind: u8,
        width: usize,
        height: usize,
        output: &mut [u8],
    ) -> io::Result<()> {
        let stream = usize::from(kind & 0x03);
        let filter = if kind & EXPLICIT_FILTER != 0 {
            let mut filter = [0];
            reader.read_exact(&mut filter)?;
            filter[0]
        } else {
            FILTER_COPY
        };
        let mut palette = [[0u8; 3]; 256];
        let mut palette_size = 0;
        let data_length = match filter {
            FILTER_COPY | FILTER_GRADIENT => width * height * 3,
            FILTER_PALETTE => {
                let mut size = [0];
                reader.read_exact(&mut size)?;
                palette_size = usize::from(size[0]) + 1;
                for color in &mut palette[..palette_size] {
                    reader.read_exact(color)?;
                }
                if palette_size <= 2 {
                    width.div_ceil(8) * height
                } else {
                    width * height
                }
            }
            _ => return Err(invalid("unsupported Tight filter")),
        };
        self.data.resize(data_length, 0);
        if data_length < MIN_TO_COMPRESS {
            reader.read_exact(&mut self.data)?;
        } else {
            let length = read_compact_length(reader)?;
            self.compressed.resize(length, 0);
            reader.read_exact(&mut self.compressed)?;
            inflate_exact(&mut self.streams[stream], &self.compressed, &mut self.data)?;
        }
        let data = &self.data;
        match filter {
            FILTER_COPY => {
                for (target, source) in output.chunks_exact_mut(4).zip(data.chunks_exact(3)) {
                    target.copy_from_slice(&[source[2], source[1], source[0], 0]);
                }
            }
            FILTER_PALETTE => {
                let colors = &palette[..palette_size];
                let mut put = |pixel: usize, index: usize| -> io::Result<()> {
                    let color = colors
                        .get(index)
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
        Ok(())
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

fn decode_jpeg(jpeg: &[u8], width: usize, height: usize, output: &mut [u8]) -> io::Result<()> {
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
    use std::io::Cursor;

    fn decode(body: &[u8], width: usize, height: usize) -> io::Result<Vec<u32>> {
        let mut output = Vec::new();
        TightDecoder::new().read_rect(&mut Cursor::new(body), width, height, &mut output)?;
        Ok(output
            .chunks_exact(4)
            .map(|pixel| u32::from_le_bytes([pixel[0], pixel[1], pixel[2], 0]))
            .collect())
    }

    fn roundtrip(pixels: &[u32], width: usize, settings: TightSettings) -> (Vec<u8>, Vec<u32>) {
        let height = pixels.len() / width;
        let mut body = Vec::new();
        encode_rect(pixels, width, height, settings, &mut body).unwrap();
        let decoded = decode(&body, width, height).unwrap();
        (body, decoded)
    }

    fn noise(count: usize) -> Vec<u32> {
        let mut state = 0x1234_5678u32;
        (0..count)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state & 0x00ff_ffff
            })
            .collect()
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
            assert_eq!(
                read_compact_length(&mut Cursor::new(&output)).unwrap(),
                length
            );
        }
    }

    #[test]
    fn solid_rectangles_are_sent_as_one_fill_color() {
        let (body, decoded) = roundtrip(&[0x123456; 64], 8, TightSettings::default());
        assert_eq!(body, [0x80, 0x12, 0x34, 0x56]);
        assert_eq!(decoded, vec![0x123456; 64]);
    }

    #[test]
    fn few_color_rectangles_stay_lossless_even_with_jpeg() {
        let jpeg = TightSettings {
            quality: Some(0),
            compression: 1,
        };
        // Two colors: one bit per pixel with a row width that is not a
        // multiple of eight.
        let two: Vec<u32> = (0..13 * 5)
            .map(|index| if index % 3 == 0 { 0xffffff } else { 0x000080 })
            .collect();
        let (body, decoded) = roundtrip(&two, 13, jpeg);
        assert_eq!(body[..3], [0x41, FILTER_PALETTE, 1]);
        assert_eq!(decoded, two);
        let several: Vec<u32> = (0..40 * 9).map(|index| (index % 7) * 0x010203).collect();
        let (body, decoded) = roundtrip(&several, 40, jpeg);
        assert_eq!(body[..3], [0x41, FILTER_PALETTE, 6]);
        assert_eq!(decoded, several);
    }

    #[test]
    fn many_color_rectangles_use_zlib_without_a_quality_level() {
        let pixels = noise(64 * 32);
        let (body, decoded) = roundtrip(&pixels, 64, TightSettings::default());
        assert_eq!(body[..2], [0x41, FILTER_COPY]);
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn jpeg_rectangles_decode_close_to_the_source() {
        // A smooth gradient survives JPEG with small per-channel error.
        let (width, height) = (96, 40);
        let pixels: Vec<u32> = (0..width * height)
            .map(|index| {
                let (x, y) = ((index % width) as u32, (index / width) as u32);
                (x * 2) << 16 | (y * 5) << 8 | (x + y)
            })
            .collect();
        let settings = TightSettings {
            quality: Some(8),
            compression: 1,
        };
        let (body, decoded) = roundtrip(&pixels, width, settings);
        assert_eq!(body[0], 0x90);
        for (source, decoded) in pixels.iter().zip(&decoded) {
            for shift in [0, 8, 16] {
                let difference =
                    ((source >> shift & 0xff) as i32 - (decoded >> shift & 0xff) as i32).abs();
                assert!(difference <= 12, "{source:06x} decoded as {decoded:06x}");
            }
        }
    }

    #[test]
    fn short_basic_data_is_sent_uncompressed() {
        // Explicit copy filter, three pixels: nine bytes, below the zlib
        // threshold.
        let body = [0x40, FILTER_COPY, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        assert_eq!(
            decode(&body, 3, 1).unwrap(),
            vec![0x010203, 0x040506, 0x070809]
        );
        // Without the explicit filter flag the copy filter is implied.
        assert_eq!(
            decode(&[0x00, 1, 2, 3, 4, 5, 6], 2, 1).unwrap(),
            vec![0x010203, 0x040506]
        );
    }

    #[test]
    fn gradient_filter_reconstructs_from_neighbor_estimates() {
        // 2x2 image: [10, 20 / 30, 45] in every channel.
        // Residuals: 10-0, 20-10, 30-10, 45-(30+20-10)=5.
        let mut body = vec![0x40, FILTER_GRADIENT];
        for residual in [10u8, 10, 20, 5] {
            body.extend_from_slice(&[residual; 3]);
        }
        // Twelve bytes reach the zlib threshold, so compress them.
        let mut compressor = Compress::new(Compression::fast(), true);
        let mut compressed = Vec::with_capacity(128);
        compressor
            .compress_vec(&body[2..], &mut compressed, FlushCompress::Sync)
            .unwrap();
        body.truncate(2);
        push_compact_length(&mut body, compressed.len());
        body.extend_from_slice(&compressed);
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
            decoder
                .read_rect(&mut Cursor::new(body), 4, 1, &mut output)
                .unwrap();
            assert_eq!(output[..4], [value, value, value, 0]);
        }
    }

    #[test]
    fn malformed_rectangles_are_rejected() {
        let rejected = |body: &[u8], width, height| {
            decode(body, width, height).unwrap_err().kind() == io::ErrorKind::InvalidData
        };
        // Unknown compression type and filter.
        assert!(rejected(&[0xa0], 1, 1));
        assert!(rejected(&[0x40, 3], 1, 1));
        // Palette index past the palette.
        assert!(rejected(
            &[0x40, FILTER_PALETTE, 2, 0, 0, 0, 1, 1, 1, 2, 2, 2, 3],
            1,
            1
        ));
        // Zlib data that inflates to the wrong length.
        let mut compressor = Compress::new(Compression::fast(), true);
        let mut compressed = Vec::with_capacity(128);
        compressor
            .compress_vec(&[0; 15], &mut compressed, FlushCompress::Sync)
            .unwrap();
        let mut body = vec![0x40, FILTER_COPY];
        push_compact_length(&mut body, compressed.len());
        body.extend_from_slice(&compressed);
        assert!(rejected(&body, 4, 1));
        // A JPEG whose size does not match its rectangle.
        let mut jpeg = Vec::new();
        encode_jpeg(&noise(16 * 16), 16, 16, 5, &mut jpeg).unwrap();
        assert!(rejected(&jpeg, 16, 8));
        assert!(rejected(&[0x90, 4, 1, 2, 3, 4], 16, 16));
    }
}
