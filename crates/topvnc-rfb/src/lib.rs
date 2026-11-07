//! Sans-IO RFB client protocol for TopVNC: the handshake, incremental
//! message framing, Raw/Zlib/Tight rectangle decoding, the framebuffer, and
//! input encoding. Nothing here opens sockets, starts threads, or touches a
//! platform UI, so the native viewer (over TCP) and the browser client (over
//! an encrypted WebSocket) share the same validation and decoding.
//!
//! All server-supplied lengths, dimensions, encodings, and coordinates are
//! checked before anything is allocated or indexed.

#![forbid(unsafe_code)]

mod decode;
mod framebuffer;
mod handshake;
mod input;
pub mod messages;
pub mod protocol;
mod reader;
pub mod tight;

pub use decode::{Decoded, RectDecoder};
pub use framebuffer::{DamageRect, Framebuffer, validate_framebuffer_size};
pub use handshake::{ClientHandshake, HandshakeStatus, negotiate, negotiate_with_encoding};
pub use input::InputState;
pub use protocol::{
    BUTTON_BACK, BUTTON_FORWARD, BUTTON_LEFT, BUTTON_MIDDLE, BUTTON_RIGHT, BUTTON_WHEEL_DOWN,
    BUTTON_WHEEL_LEFT, BUTTON_WHEEL_RIGHT, BUTTON_WHEEL_UP, MAX_FRAMEBUFFER_DIMENSION,
    MAX_FRAMEBUFFER_PIXELS, MAX_SERVED_DISPLAYS,
};
pub use reader::{MessageReader, Rect, ServerMessage};

use protocol::{
    CAPTURE_STATUS_ENCODING, CLIENT_TIGHT_COMPRESS_LEVEL, CONTINUOUS_UPDATES_ENCODING,
    DESKTOP_SIZE_ENCODING, EXTENDED_MOUSE_BUTTONS_ENCODING, FENCE_ENCODING,
    POINTER_MOTION_CHANGE_ENCODING, RAW_ENCODING, SERVE_DISPLAY_ENCODING, SERVE_SCALE_ENCODING,
    ZLIB_ENCODING,
};
use std::io;

pub(crate) fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub width: u16,
    pub height: u16,
    pub name: String,
    pub security: Security,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    None,
    VncPassword,
}

/// Which host display a TopVNC host serves, as it announces to viewers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServedDisplays {
    /// The display served, numbered from 1, or 0 when all displays are
    /// served side by side.
    pub served: u8,
    /// Attached displays, from 1 to [`MAX_SERVED_DISPLAYS`].
    pub count: u8,
}

impl ServedDisplays {
    pub fn is_valid(self) -> bool {
        (1..=MAX_SERVED_DISPLAYS).contains(&self.count) && self.served <= self.count
    }
}

/// Whether a TopVNC host can capture its desktop, as it announces to
/// viewers. While it cannot, viewers keep the last picture and should say
/// why; input is still sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureStatus {
    /// The picture is live.
    #[default]
    Live,
    /// No display is attached or connected, such as after the only monitor
    /// was unplugged.
    NoDisplay,
    /// The desktop is on a screen the host cannot capture, such as the lock
    /// screen or a UAC prompt.
    SecureDesktop,
    /// Capture stopped for another reason, including codes this viewer does
    /// not know.
    Unavailable,
}

impl CaptureStatus {
    /// The code sent in the rectangle's `x` field.
    pub fn code(self) -> u16 {
        match self {
            Self::Live => 0,
            Self::NoDisplay => 1,
            Self::SecureDesktop => 2,
            Self::Unavailable => 3,
        }
    }

    /// Unknown codes from newer hosts still mean the picture is not live.
    pub fn from_code(code: u16) -> Self {
        match code {
            0 => Self::Live,
            1 => Self::NoDisplay,
            2 => Self::SecureDesktop,
            _ => Self::Unavailable,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Raw,
    Zlib,
    /// Tight with JPEG at an RFB quality level from 0 (smallest) to 9 (best).
    /// Zlib and Raw are accepted from servers without Tight.
    Tight {
        quality: u8,
    },
}

impl Encoding {
    /// The SetEncodings list the client sends, most preferred first.
    pub fn advertised(self) -> Vec<i32> {
        let mut encodings = match self {
            Self::Raw => vec![RAW_ENCODING],
            Self::Zlib => vec![ZLIB_ENCODING],
            Self::Tight { quality } => vec![
                tight::TIGHT_ENCODING,
                ZLIB_ENCODING,
                RAW_ENCODING,
                tight::QUALITY_LEVEL_0 + i32::from(quality.min(9)),
                tight::COMPRESS_LEVEL_0 + i32::from(CLIENT_TIGHT_COMPRESS_LEVEL),
            ],
        };
        // Servers that support them push updates without a request per
        // frame, with fences for flow control.
        encodings.extend([
            FENCE_ENCODING,
            CONTINUOUS_UPDATES_ENCODING,
            DESKTOP_SIZE_ENCODING,
            SERVE_SCALE_ENCODING,
            SERVE_DISPLAY_ENCODING,
            CAPTURE_STATUS_ENCODING,
        ]);
        // Relative motion for games that capture the mouse, and the back
        // and forward buttons.
        encodings.extend([
            POINTER_MOTION_CHANGE_ENCODING,
            EXTENDED_MOUSE_BUTTONS_ENCODING,
        ]);
        encodings
    }

    /// Whether a rectangle in `wire_encoding` is acceptable after
    /// advertising this encoding.
    pub fn accepts(self, wire_encoding: i32) -> bool {
        matches!(
            (self, wire_encoding),
            (_, RAW_ENCODING)
                | (Self::Zlib | Self::Tight { .. }, ZLIB_ENCODING)
                | (Self::Tight { .. }, tight::TIGHT_ENCODING)
        )
    }
}

/// A short name for an RFB encoding number.
pub fn encoding_name(encoding: i32) -> &'static str {
    match encoding {
        RAW_ENCODING => "Raw",
        ZLIB_ENCODING => "Zlib",
        tight::TIGHT_ENCODING => "Tight",
        _ => "other",
    }
}

#[cfg(test)]
mod tests;
