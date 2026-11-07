//! RFB numbers shared by TopVNC's client and its embedded server.

pub const RAW_ENCODING: i32 = 0;
pub const ZLIB_ENCODING: i32 = 6;
pub const SERVE_SCALE_ENCODING: i32 = 0x5456_4e53;
pub const SERVE_SCALE_FENCE: &[u8] = b"TopVNC.Scale/1:";
pub const SERVE_DISPLAY_ENCODING: i32 = 0x5456_4e44;
pub const SERVE_DISPLAY_FENCE: &[u8] = b"TopVNC.Display/1:";
/// TopVNC private pseudo-encoding (TVNC): whether the host can capture its
/// desktop. A payload-free rectangle with `x` = [`crate::CaptureStatus`]
/// code and `y` = width = height = 0.
pub const CAPTURE_STATUS_ENCODING: i32 = 0x5456_4e43;
/// The most host displays the display capability describes.
pub const MAX_SERVED_DISPLAYS: u8 = 16;
pub const MAX_FRAMEBUFFER_DIMENSION: u16 = 8192;
pub const MAX_FRAMEBUFFER_PIXELS: usize = 33_554_432;
pub const MAX_NAME_BYTES: usize = 4096;
/// The largest server clipboard text a client accepts.
pub const MAX_SERVER_CUT_TEXT_BYTES: usize = 1_048_576;
pub const DESKTOP_SIZE_ENCODING: i32 = -223;
/// Pseudo-encoding: the client understands Fence messages.
pub const FENCE_ENCODING: i32 = -312;
/// Pseudo-encoding: the client understands ContinuousUpdates.
pub const CONTINUOUS_UPDATES_ENCODING: i32 = -313;
/// Message type of EnableContinuousUpdates (client) and
/// EndOfContinuousUpdates (server).
pub const CONTINUOUS_UPDATES_MESSAGE: u8 = 150;
pub const FENCE_MESSAGE: u8 = 248;
pub const FENCE_BLOCK_BEFORE: u32 = 1 << 0;
pub const FENCE_BLOCK_AFTER: u32 = 1 << 1;
pub const FENCE_SYNC_NEXT: u32 = 1 << 2;
pub const FENCE_REQUEST: u32 = 1 << 31;
pub const FENCE_SUPPORTED_FLAGS: u32 = FENCE_BLOCK_BEFORE | FENCE_BLOCK_AFTER | FENCE_SYNC_NEXT;
pub const MAX_FENCE_PAYLOAD: usize = 64;
/// Pseudo-encoding (QEMU Pointer Motion Change): the client can send
/// relative pointer motion when the server asks for it.
pub const POINTER_MOTION_CHANGE_ENCODING: i32 = -257;
/// Pseudo-encoding: the client can send extended PointerEvents with the
/// back and forward buttons.
pub const EXTENDED_MOUSE_BUTTONS_ENCODING: i32 = -316;
/// Relative PointerEvent coordinates are deltas offset by this value.
pub const RELATIVE_POINTER_ORIGIN: i32 = 0x7fff;
/// Largest delta one relative PointerEvent carries; larger motion is split.
/// Relative coordinates therefore lie in 0x4000..=0xbffe, and absolute ones
/// below `MAX_FRAMEBUFFER_DIMENSION`, so the server can tell which mode a
/// client used for every event, including ones in flight at a mode change.
pub const MAX_RELATIVE_DELTA: i32 = 0x3fff;
const _: () =
    assert!((MAX_FRAMEBUFFER_DIMENSION as i32) < RELATIVE_POINTER_ORIGIN - MAX_RELATIVE_DELTA);
/// Marks an extended PointerEvent in *button-mask* once ExtendedMouseButtons
/// is negotiated; otherwise the bit is the back button.
pub const EXTENDED_POINTER_MARKER: u8 = 0x80;
/// The compression level the client requests with Tight: fast zlib, since
/// most gaming content is sent as JPEG anyway.
pub const CLIENT_TIGHT_COMPRESS_LEVEL: u8 = 1;

/// Pointer buttons as input events carry them: bits 0-6 as in RFB's
/// *button-mask*, then back and forward.
pub const BUTTON_LEFT: u16 = 1 << 0;
pub const BUTTON_MIDDLE: u16 = 1 << 1;
pub const BUTTON_RIGHT: u16 = 1 << 2;
pub const BUTTON_WHEEL_UP: u16 = 1 << 3;
pub const BUTTON_WHEEL_DOWN: u16 = 1 << 4;
pub const BUTTON_WHEEL_LEFT: u16 = 1 << 5;
pub const BUTTON_WHEEL_RIGHT: u16 = 1 << 6;
pub const BUTTON_BACK: u16 = 1 << 7;
pub const BUTTON_FORWARD: u16 = 1 << 8;
