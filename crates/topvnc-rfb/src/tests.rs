use super::*;
use flate2::{Compress, Compression, FlushCompress};
use protocol::{
    CAPTURE_STATUS_ENCODING, CONTINUOUS_UPDATES_MESSAGE, FENCE_MESSAGE, FENCE_REQUEST,
    MAX_FENCE_PAYLOAD, MAX_SERVER_CUT_TEXT_BYTES,
};

fn rect_header(x: u16, y: u16, width: u16, height: u16, encoding: i32) -> Vec<u8> {
    let mut header = Vec::new();
    for value in [x, y, width, height] {
        header.extend_from_slice(&value.to_be_bytes());
    }
    header.extend_from_slice(&encoding.to_be_bytes());
    header
}

fn update(rects: &[Vec<u8>]) -> Vec<u8> {
    let mut message = vec![0, 0];
    message.extend_from_slice(&(rects.len() as u16).to_be_bytes());
    for rect in rects {
        message.extend_from_slice(rect);
    }
    message
}

fn raw_rect(x: u16, y: u16, width: u16, height: u16, seed: u8) -> Vec<u8> {
    let mut rect = rect_header(x, y, width, height, RAW_ENCODING);
    for index in 0..usize::from(width) * usize::from(height) {
        let value = seed.wrapping_add(index as u8);
        rect.extend_from_slice(&[value, value ^ 0x55, value ^ 0xaa, 0]);
    }
    rect
}

fn zlib_rect(compressor: &mut Compress, x: u16, y: u16, width: u16, height: u16) -> Vec<u8> {
    let pixels: Vec<u8> = (0..usize::from(width) * usize::from(height) * 4)
        .map(|index| (index % 7) as u8)
        .collect();
    let mut compressed = Vec::with_capacity(pixels.len() + 256);
    compressor
        .compress_vec(&pixels, &mut compressed, FlushCompress::Sync)
        .unwrap();
    let mut rect = rect_header(x, y, width, height, ZLIB_ENCODING);
    rect.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
    rect.extend_from_slice(&compressed);
    rect
}

fn fence(flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut message = vec![FENCE_MESSAGE, 0, 0, 0];
    message.extend_from_slice(&flags.to_be_bytes());
    message.push(payload.len() as u8);
    message.extend_from_slice(payload);
    message
}

fn cut_text(text: &[u8]) -> Vec<u8> {
    let mut message = vec![3, 0, 0, 0];
    message.extend_from_slice(&(text.len() as u32).to_be_bytes());
    message.extend_from_slice(text);
    message
}

/// A stream with every message kind: two updates (one resizing the
/// framebuffer), Raw, Zlib on a persistent stream, Tight fill and palette,
/// pseudo-rectangles, and the non-update messages in between.
fn mixed_stream() -> Vec<u8> {
    let mut compressor = Compress::new(Compression::fast(), true);
    let mut stream = update(&[
        rect_header(0, 0, 25, 0, SERVE_SCALE_ENCODING),
        raw_rect(1, 2, 5, 3, 10),
        zlib_rect(&mut compressor, 0, 0, 8, 4),
        rect_header(1, 0, 0, 0, POINTER_MOTION_CHANGE_ENCODING),
    ]);
    stream.push(2); // Bell.
    stream.extend_from_slice(&fence(FENCE_REQUEST, b"probe"));
    stream.extend_from_slice(&cut_text(b"copied"));
    stream.push(CONTINUOUS_UPDATES_MESSAGE);
    let mut tight_fill = rect_header(10, 10, 6, 6, tight::TIGHT_ENCODING);
    tight_fill.extend_from_slice(&[0x80, 0x12, 0x34, 0x56]);
    let mut tight_palette = rect_header(0, 12, 3, 1, tight::TIGHT_ENCODING);
    tight_palette.extend_from_slice(&[
        0x40,
        tight::FILTER_PALETTE,
        1,
        1,
        2,
        3,
        4,
        5,
        6,
        0b1010_0000,
    ]);
    stream.extend_from_slice(&update(&[
        rect_header(0, 0, 16, 16, DESKTOP_SIZE_ENCODING),
        rect_header(0, 0, 16, 16, DESKTOP_SIZE_ENCODING),
        tight_fill,
        tight_palette,
        zlib_rect(&mut compressor, 0, 0, 8, 8),
        rect_header(1, 2, 0, 0, SERVE_DISPLAY_ENCODING),
        rect_header(1, 0, 0, 0, CAPTURE_STATUS_ENCODING),
        rect_header(0, 0, 0, 0, EXTENDED_MOUSE_BUTTONS_ENCODING),
    ]));
    stream.extend_from_slice(&update(&[]));
    stream
}

/// Feed `chunks` and describe every message, applying rectangles to a
/// framebuffer that starts at 12 by 8.
fn replay<'a>(chunks: impl IntoIterator<Item = &'a [u8]>) -> io::Result<(Vec<String>, Vec<u32>)> {
    let mut reader = MessageReader::new(12, 8, Encoding::Tight { quality: 6 });
    let mut decoder = RectDecoder::new();
    let mut framebuffer = Framebuffer::new(12, 8)?;
    let mut output = Vec::new();
    let mut log = Vec::new();
    for chunk in chunks {
        reader.push(chunk)?;
        while let Some(message) = reader.next_message()? {
            log.push(match message {
                ServerMessage::Rect(rect) => {
                    let pixels = decoder.decode_all(&rect, &mut output)?;
                    framebuffer.apply_raw(rect.x, rect.y, rect.width, rect.height, pixels)?;
                    format!(
                        "rect {} {} {}x{} {}",
                        rect.x, rect.y, rect.width, rect.height, rect.encoding
                    )
                }
                ServerMessage::DesktopSize { width, height } => {
                    framebuffer = Framebuffer::new(width, height)?;
                    format!("size {width}x{height}")
                }
                other => format!("{other:?}"),
            });
        }
    }
    assert_eq!(reader.buffered(), 0);
    assert!(!reader.in_update());
    Ok((log, framebuffer.pixels().to_vec()))
}

#[test]
fn messages_are_independent_of_chunk_boundaries() {
    let stream = mixed_stream();
    let (expected, pixels) = replay([stream.as_slice()]).unwrap();
    assert_eq!(
        expected,
        [
            "UpdateStart",
            "ServeScale(25)",
            "rect 1 2 5x3 0",
            "rect 0 0 8x4 6",
            "PointerMode { relative: false }",
            "UpdateEnd",
            "Bell",
            "Fence { flags: 2147483648, payload: [112, 114, 111, 98, 101] }",
            "CutText([99, 111, 112, 105, 101, 100])",
            "EndOfContinuousUpdates",
            "UpdateStart",
            "size 16x16",
            "rect 10 10 6x6 7",
            "rect 0 12 3x1 7",
            "rect 0 0 8x8 6",
            "ServeDisplay(ServedDisplays { served: 1, count: 2 })",
            "CaptureStatus(NoDisplay)",
            "ExtendedMouseButtons",
            "UpdateEnd",
            "UpdateStart",
            "UpdateEnd",
        ]
    );
    assert_eq!(pixels[10 * 16 + 10], 0x123456);
    assert_eq!(pixels[12 * 16..12 * 16 + 3], [0x040506, 0x010203, 0x040506]);
    for size in 1..64 {
        assert_eq!(
            replay(stream.chunks(size)).unwrap(),
            (expected.clone(), pixels.clone()),
            "chunks of {size}"
        );
    }
    for split in 0..stream.len() {
        let (first, second) = stream.split_at(split);
        assert_eq!(
            replay([first, second]).unwrap(),
            (expected.clone(), pixels.clone()),
            "split at {split}"
        );
    }
}

#[test]
fn exact_reads_stop_at_each_message() {
    let mut stream = update(&[raw_rect(0, 0, 4, 2, 1)]);
    let first = stream.len();
    stream.extend_from_slice(&update(&[raw_rect(0, 0, 4, 2, 2)]));
    let mut source = io::Cursor::new(&stream);
    let mut reader = MessageReader::new(4, 2, Encoding::Raw);
    let mut seen = 0;
    loop {
        match reader.next_message().unwrap() {
            Some(ServerMessage::UpdateEnd) => break,
            Some(_) => seen += 1,
            None => assert_ne!(reader.read_from(&mut source, true).unwrap(), 0),
        }
    }
    assert_eq!(seen, 2);
    assert_eq!(source.position() as usize, first);
    // A greedy read takes everything available.
    let mut reader = MessageReader::new(4, 2, Encoding::Raw);
    let mut source = io::Cursor::new(&stream);
    reader.read_from(&mut source, false).unwrap();
    assert_eq!(reader.buffered(), stream.len());
}

/// Push `bytes` and return the first error from reading messages.
fn first_error(reader: &mut MessageReader, bytes: &[u8]) -> io::Error {
    let result = reader.push(bytes).and_then(|()| {
        while reader.next_message()?.is_some() {}
        Ok(())
    });
    result.expect_err("stream should be rejected")
}

#[test]
fn malformed_headers_are_rejected_before_their_bodies_arrive() {
    let reader = || MessageReader::new(16, 16, Encoding::Tight { quality: 6 });
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (vec![9], "unknown server message type"),
        (
            update(&[rect_header(10, 0, 7, 1, RAW_ENCODING)]),
            "server sent an out-of-bounds rectangle",
        ),
        (
            update(&[rect_header(0, 0, 0, 1, RAW_ENCODING)]),
            "server sent an out-of-bounds rectangle",
        ),
        (
            update(&[rect_header(0, 0, 1, 1, 5)]),
            "server sent an unsupported encoding",
        ),
        (
            update(&[rect_header(1, 0, 4, 4, DESKTOP_SIZE_ENCODING)]),
            "invalid DesktopSize origin",
        ),
        (
            update(&[rect_header(0, 0, 0, 4, DESKTOP_SIZE_ENCODING)]),
            "invalid framebuffer dimensions",
        ),
        (
            update(&[rect_header(0, 0, 24, 0, SERVE_SCALE_ENCODING)]),
            "invalid served-size capability",
        ),
        (
            update(&[rect_header(3, 2, 0, 0, SERVE_DISPLAY_ENCODING)]),
            "invalid display capability",
        ),
        (
            update(&[rect_header(1, 0, 0, 1, CAPTURE_STATUS_ENCODING)]),
            "invalid capture status",
        ),
        (
            update(&[rect_header(1, 1, 0, 0, CAPTURE_STATUS_ENCODING)]),
            "invalid capture status",
        ),
        (
            update(&[rect_header(0, 0, 1, 1, tight::TIGHT_ENCODING), vec![0xa0]]),
            "unsupported Tight compression type",
        ),
        (
            fence(0, &[0; MAX_FENCE_PAYLOAD + 1])[..9].to_vec(),
            "fence payload is too long",
        ),
        (
            [3, 0, 0, 0]
                .into_iter()
                .chain((MAX_SERVER_CUT_TEXT_BYTES as u32 + 1).to_be_bytes())
                .collect(),
            "server clipboard text is too large",
        ),
    ];
    for (bytes, message) in cases {
        let error = first_error(&mut reader(), &bytes);
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{message}");
        assert_eq!(error.to_string(), message);
    }
    // A Zlib length beyond what the rectangle can need, before its data.
    let mut zlib = rect_header(0, 0, 1, 1, ZLIB_ENCODING);
    zlib.extend_from_slice(&(70_000u32).to_be_bytes());
    let error = first_error(&mut reader(), &update(&[zlib]));
    assert_eq!(error.to_string(), "compressed rectangle exceeds size limit");
    // Zlib is not accepted after advertising Raw only.
    let mut raw_only = MessageReader::new(16, 16, Encoding::Raw);
    let error = first_error(
        &mut raw_only,
        &update(&[rect_header(0, 0, 1, 1, ZLIB_ENCODING)]),
    );
    assert_eq!(error.to_string(), "server sent an unsupported encoding");
}

#[test]
fn rectangles_after_desktop_size_use_the_new_bounds() {
    let mut reader = MessageReader::new(4, 4, Encoding::Raw);
    let stream = update(&[
        rect_header(0, 0, 2, 2, DESKTOP_SIZE_ENCODING),
        rect_header(0, 0, 3, 3, RAW_ENCODING),
    ]);
    assert_eq!(
        first_error(&mut reader, &stream).to_string(),
        "server sent an out-of-bounds rectangle"
    );
    assert_eq!(reader.frame(), (2, 2));
}

#[test]
fn the_viewer_limit_bounds_desktop_size_and_buffering() {
    let mut reader = MessageReader::new(16, 16, Encoding::Raw);
    reader.set_max_framebuffer_pixels(32 * 32);
    let error = first_error(
        &mut reader,
        &update(&[rect_header(0, 0, 33, 32, DESKTOP_SIZE_ENCODING)]),
    );
    assert_eq!(
        error.to_string(),
        "framebuffer is larger than this viewer accepts"
    );
    // A peer that keeps sending a body never grows the buffer past the
    // limit; the reader fails instead.
    let mut reader = MessageReader::new(16, 16, Encoding::Tight { quality: 6 });
    reader.set_max_framebuffer_pixels(32 * 32);
    let limit = reader.buffer_limit();
    assert!(limit < 8 * 1024 * 1024, "{limit}");
    let mut jpeg = update(&[rect_header(0, 0, 16, 16, tight::TIGHT_ENCODING)]);
    jpeg.extend_from_slice(&[0x90, 0xff, 0xff, 0xff]);
    reader.push(&jpeg).unwrap();
    assert!(reader.next_message().unwrap().is_some()); // UpdateStart.
    assert!(reader.next_message().unwrap().is_none());
    // Nothing is taken from here on, so everything pushed stays buffered.
    let chunk = vec![0; 64 * 1024];
    let mut pushed = jpeg.len();
    let error = loop {
        match reader.push(&chunk) {
            Ok(()) => pushed += chunk.len(),
            Err(error) => break error,
        }
    };
    assert_eq!(
        error.to_string(),
        "server data exceeds the receive buffer limit"
    );
    assert!(pushed <= limit);
}

#[test]
fn input_and_control_messages_encode_as_rfb_expects() {
    assert_eq!(
        messages::update_request(true, 640, 480),
        [3, 1, 0, 0, 0, 0, 2, 128, 1, 224]
    );
    assert_eq!(
        messages::continuous_updates_message(true, 640, 480)[..2],
        [CONTINUOUS_UPDATES_MESSAGE, 1]
    );
    assert_eq!(
        messages::key_event(0xff0d, true),
        [4, 1, 0, 0, 0, 0, 0xff, 0x0d]
    );
    assert_eq!(
        messages::pointer_event(BUTTON_LEFT, 1, 2, false),
        ([5, 1, 0, 1, 0, 2, 0], 6)
    );
    assert_eq!(
        messages::pointer_event(BUTTON_BACK, 1, 2, true),
        ([5, 0x80, 0, 1, 0, 2, 1], 7)
    );
    let long = messages::fence_message(FENCE_REQUEST, &[1; 100]);
    assert_eq!(long.len(), 9 + MAX_FENCE_PAYLOAD);
    assert_eq!(
        messages::set_encodings_message(&[7, -223]),
        [2, 0, 0, 2, 0, 0, 0, 7, 0xff, 0xff, 0xff, 0x21]
    );
    // The VNC DES response for a known challenge.
    let response = messages::vnc_auth_response(&[0; 16], "password");
    assert_ne!(response, [0; 16]);
    assert_eq!(response[..8], response[8..]);
}

#[test]
fn advertised_encodings_match_what_the_reader_accepts() {
    for encoding in [
        Encoding::Raw,
        Encoding::Zlib,
        Encoding::Tight { quality: 6 },
    ] {
        for wire in encoding.advertised() {
            if [RAW_ENCODING, ZLIB_ENCODING, tight::TIGHT_ENCODING].contains(&wire) {
                assert!(encoding.accepts(wire), "{encoding:?} {wire}");
            }
        }
    }
    assert!(!Encoding::Raw.accepts(tight::TIGHT_ENCODING));
    assert_eq!(encoding_name(tight::TIGHT_ENCODING), "Tight");
}
