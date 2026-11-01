//! Tight encoding (RFB encoding 7): the server-side encoder. The client
//! decoder and the numbers both sides share are in `topvnc_rfb::tight`.
//!
//! Only the 24-bit true-color case is implemented, where a "TPIXEL" is three
//! bytes in red, green, blue order. The server offers Tight only to clients
//! whose pixel format has three 8-bit channels in a 32-bit pixel.

use flate2::{Compress, Compression, FlushCompress, Status};
use jpeg_encoder::{ColorType, Encoder as JpegEncoder, SamplingFactor};
use std::io;
pub(crate) use topvnc_rfb::tight::{
    COMPRESS_LEVEL_0, MAX_RECT_HEIGHT, MAX_RECT_WIDTH, QUALITY_LEVEL_0, TIGHT_ENCODING,
    decode_jpeg_rect,
};
use topvnc_rfb::tight::{
    EXPLICIT_FILTER, FILL, FILTER_COPY, FILTER_PALETTE, JPEG, MAX_COMPACT_LENGTH, MIN_TO_COMPRESS,
    push_compact_length,
};

/// The largest palette the encoder uses; more colors are sent as JPEG or as
/// plain pixels.
const MAX_ENCODER_PALETTE: usize = 16;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use topvnc_rfb::tight::TightDecoder;

    fn decode(body: &[u8], width: usize, height: usize) -> io::Result<Vec<u32>> {
        let mut output = Vec::new();
        TightDecoder::new().decode_all(body, width, height, &mut output)?;
        Ok(output
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| u32::from_le_bytes([pixel[0], pixel[1], pixel[2], 0]))
            .collect())
    }

    fn roundtrip(pixels: &[u32], width: usize, settings: TightSettings) -> (Vec<u8>, Vec<u32>) {
        let height = pixels.len() / width;
        let mut body = Vec::new();
        encode_rect(pixels, width, height, settings, &mut body).unwrap();
        assert_eq!(
            topvnc_rfb::tight::body_length(&body, width, height).unwrap(),
            Some(body.len())
        );
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
    fn short_palette_data_is_sent_uncompressed() {
        // Two colors in a 3x1 rectangle: one data byte, below the zlib
        // threshold.
        let (body, decoded) = roundtrip(&[1, 2, 1], 3, TightSettings::default());
        assert_eq!(body.len(), 3 + 6 + 1);
        assert_eq!(decoded, [1, 2, 1]);
    }

    #[test]
    fn a_jpeg_of_the_wrong_size_is_rejected() {
        let mut jpeg = Vec::new();
        encode_jpeg(&noise(16 * 16), 16, 16, 5, &mut jpeg).unwrap();
        assert_eq!(
            decode(&jpeg, 16, 8).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
