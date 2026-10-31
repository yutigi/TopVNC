//! Incremental server-to-client message framing. Bytes arrive in chunks of
//! any size, from a TCP read or a WebSocket message; [`MessageReader`]
//! returns each message, or each rectangle of an update, once all its bytes
//! are buffered. Every length and rectangle is validated against the
//! current framebuffer before the reader waits for (and buffers) the body,
//! so a server cannot make it allocate more than the largest rectangle the
//! framebuffer allows.

use crate::protocol::{
    CONTINUOUS_UPDATES_MESSAGE, DESKTOP_SIZE_ENCODING, EXTENDED_MOUSE_BUTTONS_ENCODING,
    FENCE_MESSAGE, MAX_FENCE_PAYLOAD, MAX_FRAMEBUFFER_PIXELS, MAX_SERVER_CUT_TEXT_BYTES,
    POINTER_MOTION_CHANGE_ENCODING, RAW_ENCODING, SERVE_DISPLAY_ENCODING, SERVE_SCALE_ENCODING,
    ZLIB_ENCODING,
};
use crate::{Encoding, ServedDisplays, invalid, tight, validate_framebuffer_size};
use std::io::{self, Read};
use std::ops::Range;

/// Initial buffer size, large enough for many small rectangles per read.
const INITIAL_BUFFER_BYTES: usize = 256 * 1024;
/// How far a greedy read may go past the next message, as a socket read
/// buffer would.
const READ_AHEAD_BYTES: usize = 256 * 1024;
/// Rectangle header: position, size, encoding.
const RECT_HEADER_BYTES: usize = 12;

/// A rectangle whose bytes have all arrived, validated against the
/// framebuffer and the negotiated encoding.
#[derive(Debug, Clone, Copy)]
pub struct Rect<'a> {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub encoding: i32,
    /// Everything after the rectangle header.
    pub body: &'a [u8],
}

/// One server message, or one part of a FramebufferUpdate, in wire order.
#[derive(Debug)]
pub enum ServerMessage<'a> {
    /// A FramebufferUpdate header arrived; its rectangles follow.
    UpdateStart,
    /// DesktopSize changed the framebuffer size. Later rectangles are
    /// validated against the new size.
    DesktopSize {
        width: u16,
        height: u16,
    },
    /// The host offers served-size control at this percentage.
    ServeScale(u8),
    /// The host offers display switching.
    ServeDisplay(ServedDisplays),
    /// The server asks for relative pointer motion, or absolute positions.
    PointerMode {
        relative: bool,
    },
    /// The server accepts extended PointerEvents.
    ExtendedMouseButtons,
    Rect(Rect<'a>),
    /// The current update's last rectangle has been returned.
    UpdateEnd,
    Bell,
    /// Server clipboard text, Latin-1, at most 1 MiB.
    CutText(&'a [u8]),
    /// The server supports continuous updates, or stopped sending them.
    EndOfContinuousUpdates,
    Fence {
        flags: u32,
        payload: &'a [u8],
    },
}

enum Parsed {
    UpdateStart(u16),
    UpdateEnd,
    /// A DesktopSize rectangle that repeats the current size.
    SameSize,
    DesktopSize(u16, u16),
    ServeScale(u8),
    ServeDisplay(ServedDisplays),
    PointerMode(bool),
    ExtendedMouseButtons,
    Rect {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        encoding: i32,
        body: Range<usize>,
    },
    Bell,
    CutText(Range<usize>),
    EndOfContinuousUpdates,
    Fence {
        flags: u32,
        payload: Range<usize>,
    },
}

enum Step {
    /// This many bytes from the start of the buffer are needed.
    Need(usize),
    Done(usize, Parsed),
}

fn be16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// The largest zlib data accepted for `length` decoded bytes: a block may
/// expand slightly.
fn zlib_limit(length: usize) -> usize {
    length + length / 1000 + 65_536
}

/// The largest buffered unit for a framebuffer of `pixels`: a Raw or Zlib
/// rectangle covering all of it, a Tight rectangle, or clipboard text.
fn unit_limit(pixels: usize) -> usize {
    let rect = RECT_HEADER_BYTES + 4 + zlib_limit(pixels * 4);
    let tight = RECT_HEADER_BYTES + 3 + 256 * 3 + 3 + tight::MAX_COMPACT_LENGTH;
    rect.max(tight).max(8 + MAX_SERVER_CUT_TEXT_BYTES)
}

pub struct MessageReader {
    /// Received bytes are `buffer[start..end]`. The buffer stays zeroed
    /// storage so reads need no initialization.
    buffer: Vec<u8>,
    start: usize,
    end: usize,
    /// Bytes from `start` needed to make progress, after `next_message` returned
    /// `None`.
    needed: usize,
    max_pixels: usize,
    frame: (u16, u16),
    encoding: Encoding,
    /// Rectangles left in the current update.
    remaining: Option<u16>,
}

impl MessageReader {
    /// A reader for a `width` by `height` framebuffer that accepts the
    /// rectangle encodings `encoding` allows.
    pub fn new(width: u16, height: u16, encoding: Encoding) -> Self {
        Self {
            buffer: Vec::new(),
            start: 0,
            end: 0,
            needed: 1,
            max_pixels: MAX_FRAMEBUFFER_PIXELS,
            frame: (width, height),
            encoding,
            remaining: None,
        }
    }

    /// Reject framebuffers, including DesktopSize changes, with more than
    /// `pixels` pixels. This also bounds the receive buffer.
    pub fn set_max_framebuffer_pixels(&mut self, pixels: usize) {
        self.max_pixels = pixels.min(MAX_FRAMEBUFFER_PIXELS);
    }

    /// The most bytes this reader buffers at once.
    pub fn buffer_limit(&self) -> usize {
        unit_limit(self.max_pixels) + READ_AHEAD_BYTES
    }

    /// Set the framebuffer size and accepted encodings for the next update.
    /// Only meaningful between messages.
    pub fn set_frame(&mut self, width: u16, height: u16, encoding: Encoding) {
        self.frame = (width, height);
        self.encoding = encoding;
    }

    /// Forget buffered bytes and any partly read update, for a new stream
    /// that starts at a message boundary. The buffer is kept for reuse.
    pub fn reset(&mut self) {
        (self.start, self.end) = (0, 0);
        self.needed = 1;
        self.remaining = None;
    }

    /// The framebuffer size, including DesktopSize changes read so far.
    pub fn frame(&self) -> (u16, u16) {
        self.frame
    }

    /// Bytes received and not yet returned as messages.
    pub fn buffered(&self) -> usize {
        self.end - self.start
    }

    /// Whether the reader is inside a FramebufferUpdate.
    pub fn in_update(&self) -> bool {
        self.remaining.is_some()
    }

    /// Make room for `additional` more bytes after the buffered ones. Room
    /// for the rest of the next message is reserved at once, so a large
    /// rectangle is received in place and moved at most once, while it is
    /// still short.
    fn reserve(&mut self, additional: usize) -> io::Result<()> {
        let buffered = self.buffered();
        let additional = additional.max(self.needed.saturating_sub(buffered));
        if buffered + additional > self.buffer_limit() {
            return Err(invalid("server data exceeds the receive buffer limit"));
        }
        if self.buffer.len() - self.end >= additional {
            return Ok(());
        }
        self.buffer.copy_within(self.start..self.end, 0);
        (self.start, self.end) = (0, buffered);
        if self.buffer.len() - self.end < additional {
            let wanted = (buffered + additional).max(INITIAL_BUFFER_BYTES);
            self.buffer.resize(wanted.min(self.buffer_limit()), 0);
        }
        Ok(())
    }

    /// Append received bytes. Fails when they would exceed the buffer limit,
    /// which only a malformed or malicious stream reaches when the caller
    /// takes every message after each push.
    pub fn push(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.reserve(bytes.len())?;
        self.buffer[self.end..self.end + bytes.len()].copy_from_slice(bytes);
        self.end += bytes.len();
        Ok(())
    }

    /// Read once from `reader` into the buffer and return the byte count,
    /// 0 at end of stream. With `exact`, never read past the bytes the next
    /// message needs, so the reader can be dropped between messages without
    /// losing data; otherwise read up to `READ_AHEAD_BYTES` further, which
    /// needs fewer reads. Bounding the read-ahead bounds what a later
    /// compaction moves.
    pub fn read_from(&mut self, reader: &mut impl Read, exact: bool) -> io::Result<usize> {
        let missing = self.needed.saturating_sub(self.buffered()).max(1);
        let room = if exact {
            missing
        } else {
            (missing + READ_AHEAD_BYTES)
                .min(self.buffer_limit() - self.buffered())
                .max(missing)
        };
        self.reserve(room)?;
        let stop = self.end + room;
        let count = reader.read(&mut self.buffer[self.end..stop])?;
        self.end += count;
        Ok(count)
    }

    /// The next complete message, or `None` until more bytes arrive. After
    /// an error the stream is unusable.
    pub fn next_message(&mut self) -> io::Result<Option<ServerMessage<'_>>> {
        loop {
            let (consumed, parsed) = match self.step()? {
                Step::Need(bytes) => {
                    self.needed = bytes;
                    return Ok(None);
                }
                Step::Done(consumed, parsed) => (consumed, parsed),
            };
            let base = self.start;
            self.start += consumed;
            if self.start == self.end {
                // The message bytes stay in place until the next read.
                (self.start, self.end) = (0, 0);
            }
            self.needed = 1;
            if matches!(
                parsed,
                Parsed::SameSize
                    | Parsed::DesktopSize(..)
                    | Parsed::ServeScale(_)
                    | Parsed::ServeDisplay(_)
                    | Parsed::PointerMode(_)
                    | Parsed::ExtendedMouseButtons
                    | Parsed::Rect { .. }
            ) {
                self.remaining = self.remaining.map(|count| count - 1);
            }
            let slice = |range: Range<usize>| base + range.start..base + range.end;
            return Ok(Some(match parsed {
                Parsed::SameSize => continue,
                Parsed::UpdateStart(count) => {
                    self.remaining = Some(count);
                    ServerMessage::UpdateStart
                }
                Parsed::UpdateEnd => {
                    self.remaining = None;
                    ServerMessage::UpdateEnd
                }
                Parsed::DesktopSize(width, height) => {
                    self.frame = (width, height);
                    ServerMessage::DesktopSize { width, height }
                }
                Parsed::ServeScale(percent) => ServerMessage::ServeScale(percent),
                Parsed::ServeDisplay(displays) => ServerMessage::ServeDisplay(displays),
                Parsed::PointerMode(relative) => ServerMessage::PointerMode { relative },
                Parsed::ExtendedMouseButtons => ServerMessage::ExtendedMouseButtons,
                Parsed::Rect {
                    x,
                    y,
                    width,
                    height,
                    encoding,
                    body,
                } => ServerMessage::Rect(Rect {
                    x,
                    y,
                    width,
                    height,
                    encoding,
                    body: &self.buffer[slice(body)],
                }),
                Parsed::Bell => ServerMessage::Bell,
                Parsed::CutText(text) => ServerMessage::CutText(&self.buffer[slice(text)]),
                Parsed::EndOfContinuousUpdates => ServerMessage::EndOfContinuousUpdates,
                Parsed::Fence { flags, payload } => ServerMessage::Fence {
                    flags,
                    payload: &self.buffer[slice(payload)],
                },
            }));
        }
    }

    fn step(&self) -> io::Result<Step> {
        let data = &self.buffer[self.start..self.end];
        match self.remaining {
            Some(0) => Ok(Step::Done(0, Parsed::UpdateEnd)),
            Some(_) => self.rect(data),
            None => {
                let Some(&kind) = data.first() else {
                    return Ok(Step::Need(1));
                };
                let need = |length: usize, parsed: &dyn Fn() -> Parsed| {
                    Ok(if data.len() < length {
                        Step::Need(length)
                    } else {
                        Step::Done(length, parsed())
                    })
                };
                match kind {
                    0 => {
                        if data.len() < 4 {
                            return Ok(Step::Need(4));
                        }
                        Ok(Step::Done(4, Parsed::UpdateStart(be16(&data[2..4]))))
                    }
                    2 => Ok(Step::Done(1, Parsed::Bell)),
                    3 => {
                        if data.len() < 8 {
                            return Ok(Step::Need(8));
                        }
                        let length = be32(&data[4..8]) as usize;
                        if length > MAX_SERVER_CUT_TEXT_BYTES {
                            return Err(invalid("server clipboard text is too large"));
                        }
                        need(8 + length, &|| Parsed::CutText(8..8 + length))
                    }
                    CONTINUOUS_UPDATES_MESSAGE => Ok(Step::Done(1, Parsed::EndOfContinuousUpdates)),
                    FENCE_MESSAGE => {
                        if data.len() < 9 {
                            return Ok(Step::Need(9));
                        }
                        let length = usize::from(data[8]);
                        if length > MAX_FENCE_PAYLOAD {
                            return Err(invalid("fence payload is too long"));
                        }
                        let flags = be32(&data[4..8]);
                        need(9 + length, &|| Parsed::Fence {
                            flags,
                            payload: 9..9 + length,
                        })
                    }
                    _ => Err(invalid("unknown server message type")),
                }
            }
        }
    }

    fn rect(&self, data: &[u8]) -> io::Result<Step> {
        if data.len() < RECT_HEADER_BYTES {
            return Ok(Step::Need(RECT_HEADER_BYTES));
        }
        let x = be16(&data[0..2]);
        let y = be16(&data[2..4]);
        let width = be16(&data[4..6]);
        let height = be16(&data[6..8]);
        let encoding = be32(&data[8..12]) as i32;
        let pseudo = |parsed| Ok(Step::Done(RECT_HEADER_BYTES, parsed));
        // Pseudo-rectangles carry no pixels and need not lie inside the
        // framebuffer.
        match encoding {
            DESKTOP_SIZE_ENCODING => {
                if x != 0 || y != 0 {
                    return Err(invalid("invalid DesktopSize origin"));
                }
                validate_framebuffer_size(width, height)?;
                if usize::from(width) * usize::from(height) > self.max_pixels {
                    return Err(invalid("framebuffer is larger than this viewer accepts"));
                }
                if self.frame == (width, height) {
                    return pseudo(Parsed::SameSize);
                }
                return pseudo(Parsed::DesktopSize(width, height));
            }
            SERVE_SCALE_ENCODING => {
                if x != 0 || y != 0 || height != 0 || !(25..=100).contains(&width) {
                    return Err(invalid("invalid served-size capability"));
                }
                return pseudo(Parsed::ServeScale(width as u8));
            }
            SERVE_DISPLAY_ENCODING => {
                let displays = u8::try_from(x)
                    .ok()
                    .zip(u8::try_from(y).ok())
                    .map(|(served, count)| ServedDisplays { served, count })
                    .filter(|displays| width == 0 && height == 0 && displays.is_valid())
                    .ok_or_else(|| invalid("invalid display capability"))?;
                return pseudo(Parsed::ServeDisplay(displays));
            }
            POINTER_MOTION_CHANGE_ENCODING => return pseudo(Parsed::PointerMode(x == 0)),
            EXTENDED_MOUSE_BUTTONS_ENCODING => return pseudo(Parsed::ExtendedMouseButtons),
            _ => {}
        }
        if !self.encoding.accepts(encoding) {
            return Err(invalid("server sent an unsupported encoding"));
        }
        let (frame_width, frame_height) = self.frame;
        if width == 0
            || height == 0
            || usize::from(x) + usize::from(width) > usize::from(frame_width)
            || usize::from(y) + usize::from(height) > usize::from(frame_height)
        {
            return Err(invalid("server sent an out-of-bounds rectangle"));
        }
        let length = usize::from(width) * usize::from(height) * 4;
        let body = &data[RECT_HEADER_BYTES..];
        let body_length = match encoding {
            RAW_ENCODING => Some(length),
            ZLIB_ENCODING => {
                if body.len() < 4 {
                    return Ok(Step::Need(RECT_HEADER_BYTES + 4));
                }
                let compressed = be32(&body[..4]) as usize;
                // A zlib block may expand slightly; cap it before allocating.
                if compressed > zlib_limit(length) {
                    return Err(invalid("compressed rectangle exceeds size limit"));
                }
                Some(4 + compressed)
            }
            _ => tight::body_length(body, usize::from(width), usize::from(height))?,
        };
        Ok(match body_length {
            // The Tight header is incomplete; it is short, so read on.
            None => Step::Need(data.len() + 1),
            Some(body_length) if body.len() < body_length => {
                Step::Need(RECT_HEADER_BYTES + body_length)
            }
            Some(body_length) => Step::Done(
                RECT_HEADER_BYTES + body_length,
                Parsed::Rect {
                    x,
                    y,
                    width,
                    height,
                    encoding,
                    body: RECT_HEADER_BYTES..RECT_HEADER_BYTES + body_length,
                },
            ),
        })
    }
}
