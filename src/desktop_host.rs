//! Platform-neutral parts of the desktop server host: remote input ownership,
//! keysym translation, capture geometry, frame hand-off, and cursor
//! compositing. OS backends supply capture and injection; everything here is
//! testable on any platform.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::net::SocketAddr;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};
use topvnc::{MAX_FRAMEBUFFER_DIMENSION, MAX_FRAMEBUFFER_PIXELS};

pub const MAX_HELD_KEYS_PER_CLIENT: usize = 256;
/// Largest clipboard text accepted from the host system, in characters.
pub const MAX_CLIPBOARD_CHARS: usize = 1_048_576;
/// One wheel notch, as Windows reports it.
pub const WHEEL_DELTA: i32 = 120;
/// Capture is recreated after a failure with exponential backoff between these.
pub const CAPTURE_RETRY_MIN: Duration = Duration::from_millis(250);
pub const CAPTURE_RETRY_MAX: Duration = Duration::from_secs(2);

pub const SERVE_USAGE: &str =
    "usage: topvnc --serve [HOST:PORT] [--display NUMBER] [--allow-insecure]";

/// Options for `topvnc --serve`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeOptions {
    pub address: String,
    /// 1-based display number; `None` serves the primary display.
    pub display: Option<usize>,
    pub allow_insecure: bool,
}

/// Progress a desktop server host reports while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerNotice {
    /// Capture started and the listener accepts viewers.
    Serving {
        address: SocketAddr,
        display: String,
        width: u16,
        height: u16,
    },
    /// The served display changed size.
    Resized { width: u16, height: u16 },
    /// The number of open viewer connections changed.
    Connections(usize),
    /// Whether remote keyboard and pointer input is ignored because the host
    /// lacks permission to inject it.
    ViewOnly(bool),
    /// A recoverable condition, such as a capture pause or clipboard failure.
    Message(String),
}

/// Host permissions the Server tab shows while the server is stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostPermissions {
    pub screen_recording: bool,
    pub accessibility: bool,
}

impl std::fmt::Display for ServerNotice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serving {
                address,
                display,
                width,
                height,
            } => write!(
                formatter,
                "Serving {display} ({width}x{height}) on {address}. \
                 TCP is unencrypted; use a trusted LAN or secure tunnel."
            ),
            Self::Resized { width, height } => {
                write!(formatter, "Display size changed to {width}x{height}.")
            }
            Self::Connections(count) => write!(formatter, "{count} viewer connection(s) open."),
            Self::ViewOnly(true) => formatter.write_str(
                "View-only: remote keyboard and mouse input is ignored until Accessibility \
                 access is granted.",
            ),
            Self::ViewOnly(false) => {
                formatter.write_str("Accessibility access granted; remote input is enabled.")
            }
            Self::Message(message) => formatter.write_str(message),
        }
    }
}

pub fn parse_serve_arguments(arguments: &[String]) -> Result<ServeOptions, &'static str> {
    let mut options = ServeOptions {
        address: "127.0.0.1:5900".to_owned(),
        display: None,
        allow_insecure: false,
    };
    let mut address_set = false;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--allow-insecure" => options.allow_insecure = true,
            "--display" if options.display.is_none() => {
                let number = arguments
                    .next()
                    .and_then(|value| value.parse::<usize>().ok())
                    .filter(|number| *number > 0)
                    .ok_or(SERVE_USAGE)?;
                options.display = Some(number);
            }
            _ if argument.starts_with('-') || address_set => return Err(SERVE_USAGE),
            _ => {
                options.address = argument.clone();
                address_set = true;
            }
        }
    }
    Ok(options)
}

/// A Windows virtual-key code and whether it is sent as an extended key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VirtualKey {
    pub code: u16,
    pub extended: bool,
}

/// The physical key a keysym presses, as a platform key code `C`. Keysyms
/// without a key code are typed as Unicode characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyIdentity<C = VirtualKey> {
    Virtual(C),
    Unicode(u32),
}

pub fn windows_key_identity(keysym: u32) -> KeyIdentity {
    keysym_to_virtual_key(keysym)
        .map(KeyIdentity::Virtual)
        .unwrap_or(KeyIdentity::Unicode(keysym))
}

/// Keys whose scan codes carry the 0xE0 prefix on a standard keyboard.
fn is_extended_virtual_key(code: u16) -> bool {
    matches!(
        code,
        0x03 // Break
            | 0x21..=0x28 // Page Up/Down, End, Home, arrows
            | 0x2c // Print Screen
            | 0x2d // Insert
            | 0x2e // Delete
            | 0x5b..=0x5d // Windows keys, Applications
            | 0x6f // Numpad divide
            | 0x90 // Num Lock
            | 0xa3 // Right Control
            | 0xa5 // Right Alt
            | 0xad..=0xb3 // Volume and media keys
    )
}

pub fn keysym_to_virtual_key(keysym: u32) -> Option<VirtualKey> {
    let standard = |code: u16| VirtualKey {
        code,
        extended: is_extended_virtual_key(code),
    };
    // Keypad navigation keys share virtual keys with the dedicated navigation
    // cluster but are the non-extended keypad keys.
    let keypad = |code: u16| VirtualKey {
        code,
        extended: false,
    };
    if let Some(code) = ascii_keysym_to_virtual_key(keysym) {
        return Some(standard(code));
    }
    Some(match keysym {
        0xff08 => standard(0x08), // BackSpace
        0xff09 => standard(0x09), // Tab
        0xff0b => standard(0x0c), // Clear
        0xff0d => standard(0x0d), // Return
        0xff13 => standard(0x13), // Pause
        0xff14 => standard(0x91), // Scroll Lock
        0xff15 => standard(0x2c), // Sys Req
        0xff1b => standard(0x1b), // Escape
        0xffff => standard(0x2e), // Delete
        0xff50 => standard(0x24), // Home
        0xff51 => standard(0x25), // Left
        0xff52 => standard(0x26), // Up
        0xff53 => standard(0x27), // Right
        0xff54 => standard(0x28), // Down
        0xff55 => standard(0x21), // Page Up
        0xff56 => standard(0x22), // Page Down
        0xff57 => standard(0x23), // End
        0xff61 => standard(0x2c), // Print
        0xff63 => standard(0x2d), // Insert
        0xff67 => standard(0x5d), // Menu
        0xff6a => standard(0x2f), // Help
        0xff6b => standard(0x03), // Break
        0xff7f => standard(0x90), // Num Lock
        0xff80 => keypad(0x20),   // KP_Space
        0xff89 => keypad(0x09),   // KP_Tab
        0xff8d => VirtualKey {
            code: 0x0d,
            extended: true,
        }, // KP_Enter
        0xff95 => keypad(0x24),   // KP_Home
        0xff96 => keypad(0x25),   // KP_Left
        0xff97 => keypad(0x26),   // KP_Up
        0xff98 => keypad(0x27),   // KP_Right
        0xff99 => keypad(0x28),   // KP_Down
        0xff9a => keypad(0x21),   // KP_Page_Up
        0xff9b => keypad(0x22),   // KP_Page_Down
        0xff9c => keypad(0x23),   // KP_End
        0xff9d => keypad(0x0c),   // KP_Begin
        0xff9e => keypad(0x2d),   // KP_Insert
        0xff9f => keypad(0x2e),   // KP_Delete
        0xffaa => standard(0x6a), // KP_Multiply
        0xffab => standard(0x6b), // KP_Add
        0xffac => standard(0x6c), // KP_Separator
        0xffad => standard(0x6d), // KP_Subtract
        0xffae => standard(0x6e), // KP_Decimal
        0xffaf => standard(0x6f), // KP_Divide
        0xffb0..=0xffb9 => standard(0x60 + (keysym - 0xffb0) as u16), // KP_0..KP_9
        0xffbd => standard(0x92), // KP_Equal
        0xffbe..=0xffd5 => standard(0x70 + (keysym - 0xffbe) as u16), // F1..F24
        0xffe1 => standard(0xa0), // Shift_L
        0xffe2 => standard(0xa1), // Shift_R
        0xffe3 => standard(0xa2), // Control_L
        0xffe4 => standard(0xa3), // Control_R
        0xffe5 => standard(0x14), // Caps_Lock
        0xffe9 => standard(0xa4), // Alt_L
        0xffea | 0xfe03 => standard(0xa5), // Alt_R, ISO_Level3_Shift (AltGr)
        0xffeb => standard(0x5b), // Super_L
        0xffec => standard(0x5c), // Super_R
        0x1008_ff11 => standard(0xae), // XF86AudioLowerVolume
        0x1008_ff12 => standard(0xad), // XF86AudioMute
        0x1008_ff13 => standard(0xaf), // XF86AudioRaiseVolume
        0x1008_ff14 => standard(0xb3), // XF86AudioPlay
        0x1008_ff15 => standard(0xb2), // XF86AudioStop
        0x1008_ff16 => standard(0xb1), // XF86AudioPrev
        0x1008_ff17 => standard(0xb0), // XF86AudioNext
        _ => return None,
    })
}

/// Map printable ASCII keysyms to US-layout virtual keys. Shifted symbols use
/// the unshifted key; the viewer sends Shift separately.
fn ascii_keysym_to_virtual_key(keysym: u32) -> Option<u16> {
    Some(match keysym {
        0x20 => 0x20,
        0x30..=0x39 => keysym as u16,
        0x41..=0x5a => keysym as u16,
        0x61..=0x7a => (keysym as u16) - 0x20,
        0x21 => 0x31,
        0x22 | 0x27 => 0xde,
        0x23 => 0x33,
        0x24 => 0x34,
        0x25 => 0x35,
        0x26 => 0x37,
        0x28 => 0x39,
        0x29 => 0x30,
        0x2a => 0x38,
        0x2b | 0x3d => 0xbb,
        0x2c | 0x3c => 0xbc,
        0x2d | 0x5f => 0xbd,
        0x2e | 0x3e => 0xbe,
        0x2f | 0x3f => 0xbf,
        0x3a | 0x3b => 0xba,
        0x40 => 0x32,
        0x5b | 0x7b => 0xdb,
        0x5c | 0x7c => 0xdc,
        0x5d | 0x7d => 0xdd,
        0x5e => 0x36,
        0x60 | 0x7e => 0xc0,
        _ => return None,
    })
}

/// UTF-16 units typed for a keysym that has no virtual key: Latin-1 keysyms
/// and the 0x01000000 Unicode keysym range.
pub fn unicode_key_units(keysym: u32) -> Option<(u16, Option<u16>)> {
    let codepoint = match keysym {
        0x20..=0xff => keysym,
        0x0100_0000..=0x0110_ffff => keysym & 0x00ff_ffff,
        _ => return None,
    };
    let character = char::from_u32(codepoint)?;
    let mut encoded = [0; 2];
    let units = character.encode_utf16(&mut encoded);
    Some((units[0], units.get(1).copied()))
}

/// A macOS virtual key code (`kVK_*`, ANSI layout).
pub type MacKeyCode = u16;

pub const MAC_KEY_COMMAND: MacKeyCode = 0x37;
pub const MAC_KEY_SHIFT: MacKeyCode = 0x38;
pub const MAC_KEY_CAPS_LOCK: MacKeyCode = 0x39;
pub const MAC_KEY_OPTION: MacKeyCode = 0x3a;
pub const MAC_KEY_CONTROL: MacKeyCode = 0x3b;
pub const MAC_KEY_RIGHT_COMMAND: MacKeyCode = 0x36;
pub const MAC_KEY_RIGHT_SHIFT: MacKeyCode = 0x3c;
pub const MAC_KEY_RIGHT_OPTION: MacKeyCode = 0x3d;
pub const MAC_KEY_RIGHT_CONTROL: MacKeyCode = 0x3e;

/// Quartz event flags (`kCGEventFlagMask*`).
pub const MAC_FLAG_ALPHA_SHIFT: u64 = 0x0001_0000;
pub const MAC_FLAG_SHIFT: u64 = 0x0002_0000;
pub const MAC_FLAG_CONTROL: u64 = 0x0004_0000;
pub const MAC_FLAG_OPTION: u64 = 0x0008_0000;
pub const MAC_FLAG_COMMAND: u64 = 0x0010_0000;
pub const MAC_FLAG_NUMERIC_PAD: u64 = 0x0020_0000;
pub const MAC_FLAG_SECONDARY_FN: u64 = 0x0080_0000;

pub fn macos_key_identity(keysym: u32) -> KeyIdentity<MacKeyCode> {
    keysym_to_macos_key(keysym)
        .map(KeyIdentity::Virtual)
        .unwrap_or(KeyIdentity::Unicode(keysym))
}

pub fn keysym_to_macos_key(keysym: u32) -> Option<MacKeyCode> {
    const LETTERS: [MacKeyCode; 26] = [
        0x00, 0x0b, 0x08, 0x02, 0x0e, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2e, 0x2d, 0x1f,
        0x23, 0x0c, 0x0f, 0x01, 0x11, 0x20, 0x09, 0x0d, 0x07, 0x10, 0x06,
    ];
    const DIGITS: [MacKeyCode; 10] = [0x1d, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1a, 0x1c, 0x19];
    const KEYPAD_DIGITS: [MacKeyCode; 10] =
        [0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5b, 0x5c];
    const FUNCTION_KEYS: [MacKeyCode; 20] = [
        0x7a, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6d, 0x67, 0x6f, 0x69, 0x6b, 0x71,
        0x6a, 0x40, 0x4f, 0x50, 0x5a,
    ];
    // Shifted symbols use the unshifted US-layout key; the viewer sends
    // Shift separately.
    Some(match keysym {
        0x61..=0x7a => LETTERS[(keysym - 0x61) as usize],
        0x41..=0x5a => LETTERS[(keysym - 0x41) as usize],
        0x30..=0x39 => DIGITS[(keysym - 0x30) as usize],
        0x20 => 0x31,
        0x21 => DIGITS[1],
        0x40 => DIGITS[2],
        0x23 => DIGITS[3],
        0x24 => DIGITS[4],
        0x25 => DIGITS[5],
        0x5e => DIGITS[6],
        0x26 => DIGITS[7],
        0x2a => DIGITS[8],
        0x28 => DIGITS[9],
        0x29 => DIGITS[0],
        0x2d | 0x5f => 0x1b,
        0x3d | 0x2b => 0x18,
        0x5b | 0x7b => 0x21,
        0x5d | 0x7d => 0x1e,
        0x5c | 0x7c => 0x2a,
        0x3b | 0x3a => 0x29,
        0x27 | 0x22 => 0x27,
        0x2c | 0x3c => 0x2b,
        0x2e | 0x3e => 0x2f,
        0x2f | 0x3f => 0x2c,
        0x60 | 0x7e => 0x32,
        0xff08 => 0x33,                   // BackSpace: Delete
        0xff09 | 0xff89 => 0x30,          // Tab, KP_Tab
        0xff0d => 0x24,                   // Return
        0xff1b => 0x35,                   // Escape
        0xffff | 0xff9f => 0x75,          // Delete, KP_Delete: Forward Delete
        0xff63 | 0xff6a | 0xff9e => 0x72, // Insert, Help, KP_Insert: Help
        0xff50 | 0xff95 => 0x73,          // Home
        0xff57 | 0xff9c => 0x77,          // End
        0xff55 | 0xff9a => 0x74,          // Page Up
        0xff56 | 0xff9b => 0x79,          // Page Down
        0xff51 | 0xff96 => 0x7b,          // Left
        0xff53 | 0xff98 => 0x7c,          // Right
        0xff54 | 0xff99 => 0x7d,          // Down
        0xff52 | 0xff97 => 0x7e,          // Up
        // Apple keyboards put F13-F15 where Print, Scroll Lock, and Pause are.
        0xff61 | 0xff15 => FUNCTION_KEYS[12],
        0xff14 => FUNCTION_KEYS[13],
        0xff13 => FUNCTION_KEYS[14],
        0xff0b | 0xff7f => 0x47, // Clear, Num Lock: keypad Clear
        0xff80 => 0x31,          // KP_Space
        0xff8d => 0x4c,          // KP_Enter
        0xffaa => 0x43,          // KP_Multiply
        0xffab => 0x45,          // KP_Add
        0xffad => 0x4e,          // KP_Subtract
        0xffac | 0xffae => 0x41, // KP_Separator, KP_Decimal
        0xffaf => 0x4b,          // KP_Divide
        0xffbd => 0x51,          // KP_Equal
        0xffb0..=0xffb9 => KEYPAD_DIGITS[(keysym - 0xffb0) as usize],
        0xffbe..=0xffd1 => FUNCTION_KEYS[(keysym - 0xffbe) as usize], // F1..F20
        0xffe1 => MAC_KEY_SHIFT,
        0xffe2 => MAC_KEY_RIGHT_SHIFT,
        0xffe3 => MAC_KEY_CONTROL,
        0xffe4 => MAC_KEY_RIGHT_CONTROL,
        0xffe5 => MAC_KEY_CAPS_LOCK,
        0xffe9 => MAC_KEY_OPTION,                 // Alt_L
        0xffea | 0xfe03 => MAC_KEY_RIGHT_OPTION,  // Alt_R, ISO_Level3_Shift
        0xffe7 | 0xffeb => MAC_KEY_COMMAND,       // Meta_L, Super_L
        0xffe8 | 0xffec => MAC_KEY_RIGHT_COMMAND, // Meta_R, Super_R
        0x1008_ff13 => 0x48,                      // XF86AudioRaiseVolume
        0x1008_ff11 => 0x49,                      // XF86AudioLowerVolume
        0x1008_ff12 => 0x4a,                      // XF86AudioMute
        _ => return None,
    })
}

/// The event flags a held modifier key contributes, including the
/// device-dependent bit that tells left and right apart. `None` for keys that
/// are not modifiers. Caps Lock is a modifier key but its flag is the system's
/// lock state, not whether the key is held.
pub fn macos_modifier_flags(code: MacKeyCode) -> Option<u64> {
    Some(match code {
        MAC_KEY_CONTROL => MAC_FLAG_CONTROL | 0x0001,
        MAC_KEY_SHIFT => MAC_FLAG_SHIFT | 0x0002,
        MAC_KEY_RIGHT_SHIFT => MAC_FLAG_SHIFT | 0x0004,
        MAC_KEY_COMMAND => MAC_FLAG_COMMAND | 0x0008,
        MAC_KEY_RIGHT_COMMAND => MAC_FLAG_COMMAND | 0x0010,
        MAC_KEY_OPTION => MAC_FLAG_OPTION | 0x0020,
        MAC_KEY_RIGHT_OPTION => MAC_FLAG_OPTION | 0x0040,
        MAC_KEY_RIGHT_CONTROL => MAC_FLAG_CONTROL | 0x2000,
        MAC_KEY_CAPS_LOCK => 0,
        _ => return None,
    })
}

/// Flags an Apple keyboard sets on a key's own events: arrows and keypad keys
/// are numeric-pad keys, and function and navigation keys are Fn keys.
pub fn macos_key_flags(code: MacKeyCode) -> u64 {
    match code {
        0x7b..=0x7e => MAC_FLAG_NUMERIC_PAD | MAC_FLAG_SECONDARY_FN,
        0x41 | 0x43 | 0x45 | 0x47 | 0x4b | 0x4c | 0x4e | 0x51..=0x59 | 0x5b | 0x5c => {
            MAC_FLAG_NUMERIC_PAD
        }
        0x40
        | 0x4f
        | 0x50
        | 0x5a
        | 0x60..=0x65
        | 0x67
        | 0x69..=0x6b
        | 0x6d
        | 0x6f
        | 0x71..=0x7a => MAC_FLAG_SECONDARY_FN,
        _ => 0,
    }
}

/// Modifier flags for the held keys of every client.
pub fn macos_event_flags(held: impl IntoIterator<Item = KeyIdentity<MacKeyCode>>) -> u64 {
    held.into_iter()
        .filter_map(|key| match key {
            KeyIdentity::Virtual(code) => macos_modifier_flags(code),
            KeyIdentity::Unicode(_) => None,
        })
        .fold(0, |flags, flag| flags | flag)
}

/// A display's frame in macOS global display coordinates: points, with the
/// origin at the main display's top-left corner and y growing downward.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The global point at the center of framebuffer pixel (`x`, `y`) when a
/// `pixels`-sized framebuffer covers the display at `bounds`. Pixels past
/// the edge, as after the display shrinks, clamp to the last pixel.
pub fn display_point(
    x: u16,
    y: u16,
    pixels: (u16, u16),
    bounds: DisplayBounds,
) -> Option<(f64, f64)> {
    let (pixel_width, pixel_height) = pixels;
    let valid = |value: f64| value.is_finite();
    if pixel_width == 0
        || pixel_height == 0
        || ![bounds.x, bounds.y, bounds.width, bounds.height]
            .into_iter()
            .all(valid)
        || bounds.width <= 0.0
        || bounds.height <= 0.0
    {
        return None;
    }
    let axis = |pixel: u16, pixels: u16, origin: f64, extent: f64| {
        let pixel = f64::from(pixel.min(pixels - 1));
        origin + (pixel + 0.5) * extent / f64::from(pixels)
    };
    Some((
        axis(x, pixel_width, bounds.x, bounds.width),
        axis(y, pixel_height, bounds.y, bounds.height),
    ))
}

/// Presses this close together, in framebuffer pixels, can form a multi-click.
pub const CLICK_SLOP_PIXELS: i32 = 4;

#[derive(Debug, Clone, Copy)]
struct Click {
    button: u8,
    at: Instant,
    x: i32,
    y: i32,
    count: i64,
}

/// Supplies the click count (`kCGMouseEventClickState`) for injected presses.
#[derive(Debug, Default)]
pub struct ClickTracker {
    last: Option<Click>,
}

impl ClickTracker {
    /// Click count for pressing `button` at pixel (`x`, `y`): one more than
    /// the previous press when it used the same button within `interval` and
    /// `CLICK_SLOP_PIXELS`, otherwise 1.
    pub fn press(&mut self, button: u8, x: i32, y: i32, at: Instant, interval: Duration) -> i64 {
        let count = match self.last {
            Some(last)
                if last.button == button
                    && at.saturating_duration_since(last.at) <= interval
                    && (x - last.x).abs() <= CLICK_SLOP_PIXELS
                    && (y - last.y).abs() <= CLICK_SLOP_PIXELS =>
            {
                last.count.saturating_add(1)
            }
            _ => 1,
        };
        self.last = Some(Click {
            button,
            at,
            x,
            y,
            count,
        });
        count
    }

    /// Click count for releasing `button`: that of its latest press.
    pub fn release(&self, button: u8) -> i64 {
        match self.last {
            Some(last) if last.button == button => last.count,
            _ => 1,
        }
    }
}

/// Button state to inject for one pointer event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerTransition {
    /// Held buttons (bits 0-2) across all clients before the event.
    pub previous: u8,
    /// Held buttons across all clients after the event.
    pub buttons: u8,
    /// Wheel notches: positive scrolls up.
    pub vertical_notches: i32,
    /// Wheel notches: positive scrolls right.
    pub horizontal_notches: i32,
}

/// Tracks which remote client holds each key and button so one client's
/// disconnect releases only its own input. `K` is the backend's key type.
pub struct RemoteInputState<K> {
    key_owners: HashMap<K, HashSet<u64>>,
    client_buttons: HashMap<u64, u8>,
}

impl<K> Default for RemoteInputState<K> {
    fn default() -> Self {
        Self {
            key_owners: HashMap::new(),
            client_buttons: HashMap::new(),
        }
    }
}

const HELD_BUTTONS: u8 = 0b0000_0111;

impl<K: Copy + Eq + Hash + Ord> RemoteInputState<K> {
    /// Returns whether the key transition must be injected.
    pub fn key_event(&mut self, client_id: u64, key: K, down: bool) -> bool {
        if down {
            let already_held = self.is_held_by(client_id, &key);
            if !already_held
                && self
                    .key_owners
                    .values()
                    .filter(|owners| owners.contains(&client_id))
                    .count()
                    >= MAX_HELD_KEYS_PER_CLIENT
            {
                return false;
            }
            let owners = self.key_owners.entry(key).or_default();
            let inserted = owners.insert(client_id);
            // Repeat a held key's press so remote auto-repeat still works.
            !inserted || owners.len() == 1
        } else if let Some(owners) = self.key_owners.get_mut(&key) {
            let should_release = owners.remove(&client_id) && owners.is_empty();
            if owners.is_empty() {
                self.key_owners.remove(&key);
            }
            should_release
        } else {
            false
        }
    }

    pub fn is_held_by(&self, client_id: u64, key: &K) -> bool {
        self.key_owners
            .get(key)
            .is_some_and(|owners| owners.contains(&client_id))
    }

    /// Keys held by any client.
    pub fn held_keys(&self) -> impl Iterator<Item = K> + '_ {
        self.key_owners.keys().copied()
    }

    pub fn pointer_event(&mut self, client_id: u64, buttons: u8) -> PointerTransition {
        let old_client_buttons = self.client_buttons.insert(client_id, buttons).unwrap_or(0);
        let pressed = |mask: u8| buttons & mask != 0 && old_client_buttons & mask == 0;
        let notches = |positive: u8, negative: u8| {
            i32::from(pressed(positive)) - i32::from(pressed(negative))
        };
        PointerTransition {
            previous: self.combined_buttons_except(client_id, old_client_buttons),
            buttons: self.combined_buttons(),
            // RFB buttons 4/5 scroll up/down and 6/7 scroll left/right.
            vertical_notches: notches(8, 16),
            horizontal_notches: notches(64, 32),
        }
    }

    fn combined_buttons(&self) -> u8 {
        self.client_buttons
            .values()
            .fold(0, |all, state| all | (state & HELD_BUTTONS))
    }

    fn combined_buttons_except(&self, client_id: u64, replacement: u8) -> u8 {
        self.client_buttons
            .iter()
            .filter(|(id, _)| **id != client_id)
            .fold(replacement & HELD_BUTTONS, |all, (_, state)| {
                all | (state & HELD_BUTTONS)
            })
    }

    /// Forget a client; returns keys to release and the button transition.
    pub fn disconnect(&mut self, client_id: u64) -> (Vec<K>, u8, u8) {
        let previous = self.combined_buttons();
        let mut released = Vec::new();
        self.key_owners.retain(|key, owners| {
            if owners.remove(&client_id) && owners.is_empty() {
                released.push(*key);
            }
            !owners.is_empty()
        });
        released.sort_unstable();
        self.client_buttons.remove(&client_id);
        (released, previous, self.combined_buttons())
    }

    /// Forget all clients; returns every held key and button.
    pub fn release_all(&mut self) -> (Vec<K>, u8) {
        let mut keys = self.key_owners.keys().copied().collect::<Vec<_>>();
        keys.sort_unstable();
        let buttons = self.combined_buttons();
        self.key_owners.clear();
        self.client_buttons.clear();
        (keys, buttons)
    }
}

/// Normalize a desktop pixel to the 0..=65535 absolute mouse range spanning
/// `extent` pixels from `origin`, targeting the pixel's center.
pub fn absolute_mouse_coordinate(pixel: i32, origin: i32, extent: i32) -> i32 {
    if extent <= 0 {
        return 0;
    }
    let offset = i64::from(pixel - origin).clamp(0, i64::from(extent) - 1);
    ((offset * 2 + 1) * 65_536 / (i64::from(extent) * 2)).clamp(0, 65_535) as i32
}

pub fn latin1_to_string(text: &[u8]) -> String {
    text.iter().map(|byte| char::from(*byte)).collect()
}

pub fn latin1_to_utf16(text: &[u8]) -> Vec<u16> {
    text.iter()
        .map(|byte| char::from(*byte) as u16)
        .chain(std::iter::once(0))
        .collect()
}

/// RFB clipboard text is Latin-1; other characters become `?`.
pub fn latin1_from_unicode(text: &str) -> Vec<u8> {
    text.chars()
        .take(MAX_CLIPBOARD_CHARS)
        .map(|character| u8::try_from(u32::from(character)).unwrap_or(b'?'))
        .collect()
}

pub fn validate_capture_dimensions(width: u16, height: u16) -> Result<(), &'static str> {
    let pixels = usize::from(width)
        .checked_mul(usize::from(height))
        .ok_or("display pixel count overflow")?;
    if width == 0
        || height == 0
        || width > MAX_FRAMEBUFFER_DIMENSION
        || height > MAX_FRAMEBUFFER_DIMENSION
        || pixels > MAX_FRAMEBUFFER_PIXELS
    {
        return Err("display dimensions exceed VNC framebuffer limits");
    }
    Ok(())
}

/// A half-open pixel rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn is_empty(self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }

    pub fn intersect(self, other: Self) -> Option<Self> {
        let rect = Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        (!rect.is_empty()).then_some(rect)
    }

    fn width(self) -> usize {
        (self.right - self.left) as usize
    }
}

/// Changed regions of a captured frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameDamage {
    Full,
    Regions(Vec<Rect>),
}

/// Beyond this many rectangles, damage collapses to their bounding box.
pub const MAX_FRAME_DAMAGE_RECTS: usize = 64;

impl FrameDamage {
    fn bounded(rects: Vec<Rect>) -> Self {
        if rects.len() <= MAX_FRAME_DAMAGE_RECTS {
            return Self::Regions(rects);
        }
        let union = rects.iter().skip(1).fold(rects[0], |all, rect| {
            Rect::new(
                all.left.min(rect.left),
                all.top.min(rect.top),
                all.right.max(rect.right),
                all.bottom.max(rect.bottom),
            )
        });
        Self::Regions(vec![union])
    }

    /// Damage of this frame plus a newer one that replaced it unread.
    pub fn merge(self, newer: Self) -> Self {
        match (self, newer) {
            (Self::Regions(mut older), Self::Regions(newer)) => {
                older.extend(newer);
                Self::bounded(older)
            }
            _ => Self::Full,
        }
    }

    /// The damaged rectangles inside `bounds`.
    pub fn rects(&self, bounds: Rect) -> Vec<Rect> {
        match self {
            Self::Full => vec![bounds],
            Self::Regions(rects) => rects
                .iter()
                .filter_map(|rect| rect.intersect(bounds))
                .collect(),
        }
    }
}

/// Damage for a complete frame from its dirty rectangles (`[x, y, width,
/// height]` in frame pixels), validated and clipped to `width`×`height`. A
/// frame without usable dirty rectangles is fully damaged.
pub fn frame_damage(dirty: Option<&[[f64; 4]]>, width: usize, height: usize) -> FrameDamage {
    let Some(dirty) = dirty else {
        return FrameDamage::Full;
    };
    let bounds = Rect::new(0, 0, width as i32, height as i32);
    let rects = dirty
        .iter()
        .filter_map(|&[x, y, w, h]| {
            if ![x, y, w, h].into_iter().all(f64::is_finite) || w <= 0.0 || h <= 0.0 {
                return None;
            }
            let clamp = |value: f64, max: i32| value.clamp(0.0, f64::from(max)) as i32;
            Rect::new(
                clamp(x.floor(), bounds.right),
                clamp(y.floor(), bounds.bottom),
                clamp((x + w).ceil(), bounds.right),
                clamp((y + h).ceil(), bounds.bottom),
            )
            .intersect(bounds)
        })
        .collect::<Vec<_>>();
    if rects.is_empty() {
        FrameDamage::Full
    } else {
        FrameDamage::bounded(rects)
    }
}

/// A single-slot hand-off from a capture callback to the serving loop. A
/// newer frame replaces an unread one and inherits its damage, so publishing
/// never waits for the consumer and at most one frame is queued.
pub struct FrameSlot<T> {
    pending: Mutex<Option<(T, FrameDamage)>>,
    ready: Condvar,
}

impl<T> Default for FrameSlot<T> {
    fn default() -> Self {
        Self {
            pending: Mutex::new(None),
            ready: Condvar::new(),
        }
    }
}

impl<T> FrameSlot<T> {
    /// Store `frame`, returning the unread frame it replaced so the caller
    /// can release it outside the lock.
    pub fn publish(&self, frame: T, damage: FrameDamage) -> Option<T> {
        let Ok(mut pending) = self.pending.lock() else {
            return Some(frame);
        };
        let (replaced, damage) = match pending.take() {
            Some((older, older_damage)) => (Some(older), older_damage.merge(damage)),
            None => (None, damage),
        };
        *pending = Some((frame, damage));
        drop(pending);
        self.ready.notify_one();
        replaced
    }

    /// Wait up to `timeout` for a frame and take it.
    pub fn take_timeout(&self, timeout: Duration) -> Option<(T, FrameDamage)> {
        let pending = self.pending.lock().ok()?;
        let (mut pending, _) = self
            .ready
            .wait_timeout_while(pending, timeout, |pending| pending.is_none())
            .ok()?;
        pending.take()
    }
}

/// How the captured image is rotated relative to the upright desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    Identity,
    Rotate90,
    Rotate180,
    Rotate270,
}

impl Rotation {
    /// Captured image size for an upright desktop of `width`×`height`.
    pub fn source_size(self, width: u16, height: u16) -> (u16, u16) {
        match self {
            Self::Identity | Self::Rotate180 => (width, height),
            Self::Rotate90 | Self::Rotate270 => (height, width),
        }
    }

    /// The captured pixel shown at upright desktop pixel (`x`, `y`).
    pub fn source_coordinate(
        self,
        x: usize,
        y: usize,
        source_width: usize,
        source_height: usize,
    ) -> (usize, usize) {
        match self {
            Self::Identity => (x, y),
            Self::Rotate90 => (y, source_height - 1 - x),
            Self::Rotate180 => (source_width - 1 - x, source_height - 1 - y),
            Self::Rotate270 => (source_width - 1 - y, x),
        }
    }

    /// Map a captured-image rectangle to upright desktop coordinates.
    pub fn desktop_rect(self, source: Rect, source_width: i32, source_height: i32) -> Rect {
        match self {
            Self::Identity => source,
            Self::Rotate90 => Rect::new(
                source_height - source.bottom,
                source.left,
                source_height - source.top,
                source.right,
            ),
            Self::Rotate180 => Rect::new(
                source_width - source.right,
                source_height - source.bottom,
                source_width - source.left,
                source_height - source.top,
            ),
            Self::Rotate270 => Rect::new(
                source.top,
                source_width - source.right,
                source.bottom,
                source_width - source.left,
            ),
        }
    }
}

/// A mapped BGRA capture surface.
pub struct CaptureSurface<'a> {
    pub bytes: &'a [u8],
    pub row_pitch: usize,
    pub width: usize,
    pub height: usize,
    pub rotation: Rotation,
}

impl CaptureSurface<'_> {
    /// Copy the upright desktop `rect` into `desktop` (0x00RRGGBB pixels,
    /// `desktop_width` per row). `rect` must lie inside the desktop.
    pub fn copy_rect(&self, rect: Rect, desktop: &mut [u32], desktop_width: usize) {
        let pixel = |source_x: usize, source_y: usize| {
            let offset = source_y * self.row_pitch + source_x * 4;
            let bytes: [u8; 4] = self.bytes[offset..offset + 4].try_into().unwrap();
            u32::from_le_bytes(bytes) & 0x00ff_ffff
        };
        for y in rect.top as usize..rect.bottom as usize {
            let row_start = y * desktop_width + rect.left as usize;
            let row = &mut desktop[row_start..row_start + rect.width()];
            if self.rotation == Rotation::Identity {
                let start = y * self.row_pitch + rect.left as usize * 4;
                let source = &self.bytes[start..start + rect.width() * 4];
                for (target, bytes) in row.iter_mut().zip(source.chunks_exact(4)) {
                    *target = u32::from_le_bytes(bytes.try_into().unwrap()) & 0x00ff_ffff;
                }
            } else {
                for (column, target) in row.iter_mut().enumerate() {
                    let (source_x, source_y) = self.rotation.source_coordinate(
                        rect.left as usize + column,
                        y,
                        self.width,
                        self.height,
                    );
                    *target = pixel(source_x, source_y);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorKind {
    /// 1bpp AND mask rows followed by 1bpp XOR mask rows.
    Monochrome,
    /// BGRA with straight alpha.
    Color,
    /// BGR replaced where alpha is 0 and XORed where alpha is 0xFF.
    MaskedColor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorShape {
    kind: CursorKind,
    width: u32,
    /// Visible rows (half the stored rows for monochrome cursors).
    height: u32,
    pitch: usize,
    data: Vec<u8>,
}

impl CursorShape {
    /// Validate a cursor bitmap. `stored_height` counts both mask halves for
    /// monochrome cursors.
    pub fn new(
        kind: CursorKind,
        width: u32,
        stored_height: u32,
        pitch: u32,
        data: Vec<u8>,
    ) -> Result<Self, &'static str> {
        let height = match kind {
            CursorKind::Monochrome if !stored_height.is_multiple_of(2) => {
                return Err("invalid monochrome cursor height");
            }
            CursorKind::Monochrome => stored_height / 2,
            CursorKind::Color | CursorKind::MaskedColor => stored_height,
        };
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err("invalid cursor dimensions");
        }
        let minimum_pitch = match kind {
            CursorKind::Monochrome => width.div_ceil(8),
            CursorKind::Color | CursorKind::MaskedColor => width * 4,
        };
        let required = (pitch as usize)
            .checked_mul(stored_height as usize)
            .ok_or("cursor size overflow")?;
        if pitch < minimum_pitch || data.len() < required {
            return Err("invalid cursor row layout");
        }
        if kind == CursorKind::MaskedColor
            && (0..height as usize).any(|row| {
                (0..width as usize)
                    .any(|column| !matches!(data[row * pitch as usize + column * 4 + 3], 0 | 0xff))
            })
        {
            return Err("invalid masked-color cursor alpha");
        }
        Ok(Self {
            kind,
            width,
            height,
            pitch: pitch as usize,
            data,
        })
    }

    pub fn bounds(&self, x: i32, y: i32) -> Rect {
        Rect::new(x, y, x + self.width as i32, y + self.height as i32)
    }

    /// Draw the cursor with its top-left corner at (`x`, `y`), touching only
    /// pixels inside `clip`, which must lie inside the desktop.
    fn composite(&self, pixels: &mut [u32], stride: usize, x: i32, y: i32, clip: Rect) {
        let Some(area) = self.bounds(x, y).intersect(clip) else {
            return;
        };
        for screen_y in area.top..area.bottom {
            let cursor_y = (screen_y - y) as usize;
            let row = cursor_y * self.pitch;
            for screen_x in area.left..area.right {
                let cursor_x = (screen_x - x) as usize;
                let target = &mut pixels[screen_y as usize * stride + screen_x as usize];
                *target = match self.kind {
                    CursorKind::Monochrome => {
                        let bit = 0x80 >> (cursor_x % 8);
                        let and_mask = self.data[row + cursor_x / 8] & bit != 0;
                        let xor_row = (cursor_y + self.height as usize) * self.pitch;
                        let xor_mask = self.data[xor_row + cursor_x / 8] & bit != 0;
                        (if and_mask { *target } else { 0 })
                            ^ if xor_mask { 0x00ff_ffff } else { 0 }
                    }
                    CursorKind::Color => {
                        let source = &self.data[row + cursor_x * 4..row + cursor_x * 4 + 4];
                        let alpha = u32::from(source[3]);
                        (0..3).fold(0, |pixel, channel| {
                            let foreground = u32::from(source[channel]);
                            let background = (*target >> (channel * 8)) & 0xff;
                            let blended =
                                (foreground * alpha + background * (255 - alpha) + 127) / 255;
                            pixel | (blended << (channel * 8))
                        })
                    }
                    CursorKind::MaskedColor => {
                        let source = &self.data[row + cursor_x * 4..row + cursor_x * 4 + 4];
                        let color = u32::from_le_bytes(source.try_into().unwrap()) & 0x00ff_ffff;
                        if source[3] == 0 {
                            color
                        } else {
                            *target ^ color
                        }
                    }
                };
            }
        }
    }
}

/// The captured desktop without a cursor, plus the software cursor drawn on
/// top when presenting to the served framebuffer. Keeping the clean image lets
/// cursor-only changes redraw just the old and new cursor areas.
pub struct DesktopImage {
    width: usize,
    height: usize,
    clean: Vec<u32>,
    cursor: Option<CursorShape>,
    cursor_position: (i32, i32),
    cursor_visible: bool,
    drawn_cursor: Option<Rect>,
    cursor_changed: bool,
}

impl DesktopImage {
    pub fn new(width: u16, height: u16) -> Self {
        let (width, height) = (usize::from(width), usize::from(height));
        Self {
            width,
            height,
            clean: vec![0; width * height],
            cursor: None,
            cursor_position: (0, 0),
            cursor_visible: false,
            drawn_cursor: None,
            cursor_changed: false,
        }
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width as i32, self.height as i32)
    }

    pub fn copy_from(&mut self, surface: &CaptureSurface<'_>, rect: Rect) {
        if let Some(rect) = rect.intersect(self.bounds()) {
            surface.copy_rect(rect, &mut self.clean, self.width);
        }
    }

    pub fn set_cursor_position(&mut self, x: i32, y: i32, visible: bool) {
        if (x, y) != self.cursor_position || visible != self.cursor_visible {
            self.cursor_position = (x, y);
            self.cursor_visible = visible;
            self.cursor_changed = true;
        }
    }

    pub fn set_cursor_shape(&mut self, shape: Option<CursorShape>) {
        if shape != self.cursor {
            self.cursor = shape;
            self.cursor_changed = true;
        }
    }

    /// Compose `captured` regions and any cursor change into `output`, which
    /// holds the served framebuffer pixels. Appends the regions that changed.
    pub fn present(&mut self, captured: &[Rect], output: &mut [u32], damage: &mut Vec<Rect>) {
        debug_assert_eq!(output.len(), self.clean.len());
        let start = damage.len();
        damage.extend(
            captured
                .iter()
                .filter_map(|rect| rect.intersect(self.bounds())),
        );
        let cursor_rect = match (&self.cursor, self.cursor_visible) {
            (Some(cursor), true) => cursor
                .bounds(self.cursor_position.0, self.cursor_position.1)
                .intersect(self.bounds()),
            _ => None,
        };
        if self.cursor_changed || cursor_rect != self.drawn_cursor {
            damage.extend(self.drawn_cursor);
            damage.extend(cursor_rect);
        }
        self.cursor_changed = false;
        self.drawn_cursor = cursor_rect;
        for rect in &damage[start..] {
            for y in rect.top as usize..rect.bottom as usize {
                let row = y * self.width + rect.left as usize..y * self.width + rect.right as usize;
                output[row.clone()].copy_from_slice(&self.clean[row]);
            }
            if let (Some(cursor), Some(_)) = (&self.cursor, cursor_rect) {
                cursor.composite(
                    output,
                    self.width,
                    self.cursor_position.0,
                    self.cursor_position.1,
                    *rect,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vk(code: u16) -> Option<VirtualKey> {
        Some(VirtualKey {
            code,
            extended: is_extended_virtual_key(code),
        })
    }

    fn key(keysym: u32) -> KeyIdentity {
        windows_key_identity(keysym)
    }

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn serve_arguments_select_address_display_and_security() {
        assert_eq!(
            parse_serve_arguments(&[]),
            Ok(ServeOptions {
                address: "127.0.0.1:5900".into(),
                display: None,
                allow_insecure: false,
            })
        );
        assert_eq!(
            parse_serve_arguments(&arguments(&[
                "0.0.0.0:5901",
                "--display",
                "2",
                "--allow-insecure"
            ])),
            Ok(ServeOptions {
                address: "0.0.0.0:5901".into(),
                display: Some(2),
                allow_insecure: true,
            })
        );
        for invalid in [
            &["--display"][..],
            &["--display", "0"],
            &["--display", "x"],
            &["--display", "1", "--display", "2"],
            &["a:1", "b:2"],
            &["--unknown"],
        ] {
            assert_eq!(parse_serve_arguments(&arguments(invalid)), Err(SERVE_USAGE));
        }
    }

    fn check_disconnect_releases_only_that_clients_input<K>(key: fn(u32) -> K)
    where
        K: Copy + Eq + Hash + Ord + std::fmt::Debug,
    {
        let mut state = RemoteInputState::default();
        assert!(state.key_event(10, key(0x61), true));
        assert!(!state.key_event(20, key(0x61), true));
        assert!(!state.key_event(10, key(0x61), false));
        assert!(state.key_event(10, key(0xffe1), true));

        let transition = state.pointer_event(10, 1);
        assert_eq!((transition.previous, transition.buttons), (0, 1));
        let transition = state.pointer_event(20, 1);
        assert_eq!((transition.previous, transition.buttons), (1, 1));

        let (released, previous, combined) = state.disconnect(10);
        assert_eq!(released, vec![key(0xffe1)]);
        assert_eq!((previous, combined), (1, 1));

        let (released, previous, combined) = state.disconnect(20);
        assert_eq!(released, vec![key(0x61)]);
        assert_eq!((previous, combined), (1, 0));
    }

    #[test]
    fn disconnect_releases_only_that_clients_held_input() {
        check_disconnect_releases_only_that_clients_input(windows_key_identity);
        check_disconnect_releases_only_that_clients_input(macos_key_identity);
    }

    #[test]
    fn held_keys_are_bounded_per_client() {
        let mut state = RemoteInputState::default();
        for index in 0..MAX_HELD_KEYS_PER_CLIENT as u32 {
            assert!(state.key_event(1, key(0x0100_0400 + index), true));
        }
        assert!(!state.key_event(1, key(u32::MAX), true));
        assert!(state.key_event(2, key(u32::MAX - 1), true));
        assert!(state.key_event(1, key(0x0100_0400), false));
        assert!(state.key_event(1, key(u32::MAX), true));
    }

    #[test]
    fn virtual_key_aliases_share_cross_client_ownership() {
        let mut state = RemoteInputState::default();
        assert!(state.key_event(1, key('a' as u32), true));
        assert!(!state.key_event(2, key('A' as u32), true));
        assert!(!state.key_event(1, key('a' as u32), false));
        assert!(state.key_event(2, key('A' as u32), false));

        assert!(state.key_event(1, key('1' as u32), true));
        assert!(!state.key_event(2, key('!' as u32), true));
        assert!(!state.key_event(1, key('1' as u32), false));
        assert!(state.key_event(2, key('!' as u32), false));
    }

    #[test]
    fn left_and_right_modifiers_are_distinct_keys() {
        let mut state = RemoteInputState::default();
        assert!(state.key_event(1, key(0xffe1), true));
        assert!(state.key_event(2, key(0xffe2), true));
        assert!(state.key_event(1, key(0xffe1), false));
        assert!(state.key_event(2, key(0xffe2), false));
    }

    #[test]
    fn wheel_buttons_are_pulses_and_do_not_stick_as_buttons() {
        let mut state = RemoteInputState::<KeyIdentity>::default();
        let transition =
            |previous, buttons, vertical_notches, horizontal_notches| PointerTransition {
                previous,
                buttons,
                vertical_notches,
                horizontal_notches,
            };
        assert_eq!(state.pointer_event(1, 8), transition(0, 0, 1, 0));
        assert_eq!(state.pointer_event(1, 8), transition(0, 0, 0, 0));
        assert_eq!(state.pointer_event(1, 0), transition(0, 0, 0, 0));
        assert_eq!(state.pointer_event(1, 16), transition(0, 0, -1, 0));
        assert_eq!(state.pointer_event(1, 32 | 1), transition(0, 1, 0, -1));
        assert_eq!(state.pointer_event(1, 64 | 1), transition(1, 1, 0, 1));
    }

    #[test]
    fn unicode_keysyms_cover_latin1_and_supplementary_characters() {
        assert_eq!(unicode_key_units(0x61), Some((0x61, None)));
        assert_eq!(unicode_key_units(0xe9), Some((0xe9, None)));
        assert_eq!(unicode_key_units(0x0101_f600), Some((0xd83d, Some(0xde00))));
        assert_eq!(unicode_key_units(0x0111_0000), None);
        assert_eq!(unicode_key_units(0xff08), None);
    }

    #[test]
    fn keysyms_map_to_windows_virtual_keys_and_extended_flags() {
        assert_eq!(keysym_to_virtual_key('a' as u32), vk(0x41));
        assert_eq!(keysym_to_virtual_key('A' as u32), vk(0x41));
        assert_eq!(keysym_to_virtual_key('!' as u32), vk(0x31));
        assert_eq!(keysym_to_virtual_key(';' as u32), vk(0xba));
        assert_eq!(keysym_to_virtual_key(0xffc9), vk(0x7b));
        assert_eq!(keysym_to_virtual_key(0xffca), vk(0x7c));
        assert_eq!(keysym_to_virtual_key(0xffd5), vk(0x87));
        assert_eq!(keysym_to_virtual_key(0xffb3), vk(0x63));
        assert_eq!(keysym_to_virtual_key(0xe9), None);

        let extended = |keysym| keysym_to_virtual_key(keysym).unwrap().extended;
        for keysym in [
            0xff51, 0xff50, 0xffff, 0xff63, 0xffaf, 0xffe4, 0xffea, 0xff8d,
        ] {
            assert!(extended(keysym), "{keysym:#x} should be extended");
        }
        for keysym in [0xff96, 0xff95, 0xff9f, 0xff0d, 0xffe3, 0xffe9, 0x61] {
            assert!(!extended(keysym), "{keysym:#x} should not be extended");
        }
        assert_eq!(keysym_to_virtual_key(0xff96).unwrap().code, 0x25);
        assert_ne!(windows_key_identity(0xff8d), windows_key_identity(0xff0d));
        assert_ne!(windows_key_identity(0xff96), windows_key_identity(0xff51));
        assert_eq!(keysym_to_virtual_key(0xfe03), keysym_to_virtual_key(0xffea));
    }

    #[test]
    fn clipboard_latin1_is_converted_to_terminated_utf16() {
        assert_eq!(
            latin1_to_utf16(&[b'h', 0xe9, b'!']),
            [b'h' as u16, 0xe9, b'!' as u16, 0]
        );
        assert_eq!(latin1_to_utf16(&[]), [0]);
        assert_eq!(latin1_from_unicode("hé😀"), [b'h', 0xe9, b'?']);
    }

    #[test]
    fn shutdown_drain_releases_every_remote_key_and_button() {
        let mut state = RemoteInputState::default();
        state.key_event(1, key(0x61), true);
        state.key_event(2, key(0x62), true);
        state.pointer_event(1, 1);
        state.pointer_event(2, 4);
        assert_eq!(
            state.release_all(),
            (
                vec![windows_key_identity(0x61), windows_key_identity(0x62)],
                5
            )
        );
        assert_eq!(state.combined_buttons(), 0);
        assert_eq!(state.release_all(), (Vec::new(), 0));
    }

    #[test]
    fn absolute_mouse_coordinates_round_trip_to_the_same_pixel() {
        for extent in [1, 2, 1080, 1920, 3840, 7680] {
            for pixel in [0, 1, extent / 2, extent - 1]
                .into_iter()
                .filter(|pixel| *pixel < extent)
            {
                let normalized = absolute_mouse_coordinate(pixel, 0, extent);
                assert!((0..=65_535).contains(&normalized));
                // Windows maps normalized coordinates by scaling to the extent.
                assert_eq!(
                    i64::from(normalized) * i64::from(extent) / 65_536,
                    pixel as i64
                );
                assert_eq!(
                    i64::from(normalized) * i64::from(extent) / 65_535,
                    pixel as i64
                );
            }
        }
        assert_eq!(absolute_mouse_coordinate(-1920, -1920, 3840), 8);
        assert_eq!(
            absolute_mouse_coordinate(10_000, 0, 100),
            absolute_mouse_coordinate(99, 0, 100)
        );
    }

    #[test]
    fn capture_dimensions_are_bounded_before_allocation() {
        assert!(validate_capture_dimensions(8192, 4096).is_ok());
        assert!(validate_capture_dimensions(8192, 4097).is_err());
        assert!(validate_capture_dimensions(8193, 1).is_err());
        assert!(validate_capture_dimensions(0, 1).is_err());
    }

    #[test]
    fn display_rotation_maps_capture_pixels_to_upright_desktop() {
        let mapped = |rotation: Rotation| {
            (0..2)
                .flat_map(|y| (0..3).map(move |x| rotation.source_coordinate(x, y, 2, 3)))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            mapped(Rotation::Rotate90),
            [(0, 2), (0, 1), (0, 0), (1, 2), (1, 1), (1, 0)]
        );
        assert_eq!(
            mapped(Rotation::Rotate270),
            [(1, 0), (1, 1), (1, 2), (0, 0), (0, 1), (0, 2)]
        );
        assert_eq!(Rotation::Rotate180.source_coordinate(0, 0, 3, 2), (2, 1));
        assert_eq!(Rotation::Rotate90.source_size(1080, 1920), (1920, 1080));
        assert_eq!(Rotation::Rotate180.source_size(1920, 1080), (1920, 1080));
    }

    #[test]
    fn rotated_damage_rectangles_cover_exactly_the_mapped_pixels() {
        let (source_width, source_height) = (5usize, 3usize);
        let source = Rect::new(1, 0, 3, 2);
        for rotation in [
            Rotation::Identity,
            Rotation::Rotate90,
            Rotation::Rotate180,
            Rotation::Rotate270,
        ] {
            let (width, height) = match rotation {
                Rotation::Identity | Rotation::Rotate180 => (source_width, source_height),
                _ => (source_height, source_width),
            };
            let rect = rotation.desktop_rect(source, source_width as i32, source_height as i32);
            for y in 0..height {
                for x in 0..width {
                    let (sx, sy) = rotation.source_coordinate(x, y, source_width, source_height);
                    let in_source = (1..3).contains(&sx) && (0..2).contains(&sy);
                    let in_rect =
                        rect.intersect(Rect::new(x as i32, y as i32, x as i32 + 1, y as i32 + 1));
                    assert_eq!(in_source, in_rect.is_some(), "{rotation:?} at ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn capture_surface_copies_rotated_bgra_rows_with_padding() {
        // Source is 2x3 with a padded row pitch; pixel value encodes (x, y).
        let (width, height, pitch) = (2usize, 3usize, 12usize);
        let mut bytes = vec![0xee; pitch * height];
        for y in 0..height {
            for x in 0..width {
                bytes[y * pitch + x * 4..y * pitch + x * 4 + 4]
                    .copy_from_slice(&[x as u8, y as u8, 0x10, 0xff]);
            }
        }
        let surface = CaptureSurface {
            bytes: &bytes,
            row_pitch: pitch,
            width,
            height,
            rotation: Rotation::Identity,
        };
        let mut desktop = vec![0; 6];
        surface.copy_rect(Rect::new(0, 1, 2, 3), &mut desktop, 2);
        assert_eq!(desktop, [0, 0, 0x10_0100, 0x10_0101, 0x10_0200, 0x10_0201]);

        let surface = CaptureSurface {
            rotation: Rotation::Rotate90,
            ..surface
        };
        // Upright desktop is 3x2.
        let mut desktop = vec![0; 6];
        surface.copy_rect(Rect::new(0, 0, 3, 2), &mut desktop, 3);
        let expected = (0..2)
            .flat_map(|y| {
                (0..3).map(move |x| {
                    let (sx, sy) = Rotation::Rotate90.source_coordinate(x, y, 2, 3);
                    0x10_0000 | (sy as u32) << 8 | sx as u32
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(desktop, expected);
    }

    #[test]
    fn cursors_composite_color_masked_and_monochrome_shapes() {
        let color = CursorShape::new(CursorKind::Color, 1, 1, 4, vec![100, 50, 200, 128]).unwrap();
        let mut pixels = vec![0];
        color.composite(&mut pixels, 1, 0, 0, Rect::new(0, 0, 1, 1));
        assert_eq!(pixels, [100 << 16 | 25 << 8 | 50]);

        let masked = CursorShape::new(
            CursorKind::MaskedColor,
            2,
            1,
            8,
            vec![1, 2, 3, 0, 0xff, 0x0f, 0x55, 0xff],
        )
        .unwrap();
        let mut pixels = vec![0x1e_140a, 0x1e_140a];
        masked.composite(&mut pixels, 2, 0, 0, Rect::new(0, 0, 2, 1));
        assert_eq!(pixels, [0x03_0201, 0x4b_1bf5]);

        let monochrome =
            CursorShape::new(CursorKind::Monochrome, 1, 2, 1, vec![0x80, 0x80]).unwrap();
        let mut pixels = vec![0x03_0201];
        monochrome.composite(&mut pixels, 1, 0, 0, Rect::new(0, 0, 1, 1));
        assert_eq!(pixels, [0xfc_fdfe]);
    }

    #[test]
    fn invalid_cursor_shapes_are_rejected() {
        assert!(CursorShape::new(CursorKind::Monochrome, 1, 3, 1, vec![0; 3]).is_err());
        assert!(CursorShape::new(CursorKind::Color, 2, 1, 4, vec![0; 8]).is_err());
        assert!(CursorShape::new(CursorKind::Color, 1, 2, 4, vec![0; 4]).is_err());
        assert!(CursorShape::new(CursorKind::MaskedColor, 1, 1, 4, vec![0, 0, 0, 7]).is_err());
        assert!(CursorShape::new(CursorKind::Color, 0, 1, 4, Vec::new()).is_err());
    }

    #[test]
    fn cursor_moves_redraw_only_the_old_and_new_cursor_areas() {
        let mut image = DesktopImage::new(8, 4);
        image.clean.fill(0x11);
        let mut output = vec![0; 32];
        let mut damage = Vec::new();
        image.present(&[image.bounds()], &mut output, &mut damage);
        assert_eq!(damage, [Rect::new(0, 0, 8, 4)]);
        assert!(output.iter().all(|pixel| *pixel == 0x11));

        let cursor = CursorShape::new(CursorKind::Color, 2, 2, 8, [0xff; 16].to_vec()).unwrap();
        image.set_cursor_shape(Some(cursor));
        image.set_cursor_position(1, 1, true);
        damage.clear();
        image.present(&[], &mut output, &mut damage);
        assert_eq!(damage, [Rect::new(1, 1, 3, 3)]);
        assert_eq!(output[9], 0x00ff_ffff);
        assert_eq!(output[0], 0x11);

        image.set_cursor_position(7, 3, true);
        damage.clear();
        image.present(&[], &mut output, &mut damage);
        assert_eq!(damage, [Rect::new(1, 1, 3, 3), Rect::new(7, 3, 8, 4)]);
        assert_eq!(output[9], 0x11);
        assert_eq!(output[31], 0x00ff_ffff);

        // Captured damage under the cursor keeps the cursor drawn on top.
        image.clean.fill(0x22);
        damage.clear();
        image.present(&[Rect::new(6, 2, 9, 5)], &mut output, &mut damage);
        assert_eq!(damage, [Rect::new(6, 2, 8, 4)]);
        assert_eq!(output[30], 0x22);
        assert_eq!(output[31], 0x00ff_ffff);

        image.set_cursor_position(7, 3, false);
        damage.clear();
        image.present(&[], &mut output, &mut damage);
        assert_eq!(damage, [Rect::new(7, 3, 8, 4)]);
        assert_eq!(output[31], 0x22);

        damage.clear();
        image.present(&[], &mut output, &mut damage);
        assert!(damage.is_empty());
    }

    #[test]
    fn keysyms_map_to_macos_ansi_key_codes() {
        let mac = keysym_to_macos_key;
        // Letters, digits, and shifted symbols share their US-layout key.
        for (keysyms, code) in [
            (&['a', 'A'][..], 0x00),
            (&['s', 'S'], 0x01),
            (&['z', 'Z'], 0x06),
            (&['q', 'Q'], 0x0c),
            (&['m', 'M'], 0x2e),
            (&['1', '!'], 0x12),
            (&['2', '@'], 0x13),
            (&['6', '^'], 0x16),
            (&['0', ')'], 0x1d),
            (&['-', '_'], 0x1b),
            (&['=', '+'], 0x18),
            (&['[', '{'], 0x21),
            (&[']', '}'], 0x1e),
            (&['\\', '|'], 0x2a),
            (&[';', ':'], 0x29),
            (&['\'', '"'], 0x27),
            (&[',', '<'], 0x2b),
            (&['.', '>'], 0x2f),
            (&['/', '?'], 0x2c),
            (&['`', '~'], 0x32),
            (&[' '], 0x31),
        ] {
            for keysym in keysyms {
                assert_eq!(mac(*keysym as u32), Some(code), "{keysym:?}");
            }
        }
        // Every printable ASCII character has a key.
        assert!((0x20..=0x7e).all(|keysym| mac(keysym).is_some()));

        for (keysym, code) in [
            (0xff08, 0x33),      // BackSpace
            (0xff09, 0x30),      // Tab
            (0xff0d, 0x24),      // Return
            (0xff1b, 0x35),      // Escape
            (0xffff, 0x75),      // Delete: Forward Delete
            (0xff63, 0x72),      // Insert: Help
            (0xff50, 0x73),      // Home
            (0xff57, 0x77),      // End
            (0xff55, 0x74),      // Page Up
            (0xff56, 0x79),      // Page Down
            (0xff51, 0x7b),      // Left
            (0xff53, 0x7c),      // Right
            (0xff54, 0x7d),      // Down
            (0xff52, 0x7e),      // Up
            (0xffbe, 0x7a),      // F1
            (0xffc9, 0x6f),      // F12
            (0xffca, 0x69),      // F13
            (0xffd1, 0x5a),      // F20
            (0xffb0, 0x52),      // KP_0
            (0xffb7, 0x59),      // KP_7
            (0xffb8, 0x5b),      // KP_8
            (0xff8d, 0x4c),      // KP_Enter
            (0xffae, 0x41),      // KP_Decimal
            (0xffbd, 0x51),      // KP_Equal
            (0x1008_ff13, 0x48), // Volume up
            (0x1008_ff11, 0x49), // Volume down
            (0x1008_ff12, 0x4a), // Mute
        ] {
            assert_eq!(mac(keysym), Some(code), "{keysym:#x}");
        }
        let function_keys = (0xffbe..=0xffd1)
            .map(mac)
            .collect::<Option<Vec<_>>>()
            .unwrap();
        assert_eq!(function_keys.len(), 20);
        assert_eq!(function_keys.iter().collect::<HashSet<_>>().len(), 20);
        assert_eq!(mac(0xffd2), None); // F21
        // Media keys and keysyms without a key fall back to Unicode typing.
        assert_eq!(mac(0x1008_ff14), None);
        assert_eq!(macos_key_identity(0xe9), KeyIdentity::Unicode(0xe9));
        assert_eq!(
            macos_key_identity(0x0100_20ac),
            KeyIdentity::Unicode(0x0100_20ac)
        );
    }

    #[test]
    fn macos_modifiers_stay_distinct_and_map_alt_and_super() {
        let mac = keysym_to_macos_key;
        assert_eq!(mac(0xffe1), Some(MAC_KEY_SHIFT));
        assert_eq!(mac(0xffe2), Some(MAC_KEY_RIGHT_SHIFT));
        assert_eq!(mac(0xffe3), Some(MAC_KEY_CONTROL));
        assert_eq!(mac(0xffe4), Some(MAC_KEY_RIGHT_CONTROL));
        assert_eq!(mac(0xffe9), Some(MAC_KEY_OPTION));
        assert_eq!(mac(0xffea), Some(MAC_KEY_RIGHT_OPTION));
        assert_eq!(mac(0xfe03), Some(MAC_KEY_RIGHT_OPTION));
        for (left, right) in [(0xffeb, 0xffec), (0xffe7, 0xffe8)] {
            assert_eq!(mac(left), Some(MAC_KEY_COMMAND));
            assert_eq!(mac(right), Some(MAC_KEY_RIGHT_COMMAND));
        }
        assert_eq!(mac(0xffe5), Some(MAC_KEY_CAPS_LOCK));

        let mut state = RemoteInputState::default();
        assert!(state.key_event(1, macos_key_identity(0xffe1), true));
        assert!(state.key_event(2, macos_key_identity(0xffe2), true));
        assert!(state.key_event(1, macos_key_identity(0xffe1), false));
        assert!(state.key_event(2, macos_key_identity(0xffe2), false));

        let left = macos_modifier_flags(MAC_KEY_SHIFT).unwrap();
        let right = macos_modifier_flags(MAC_KEY_RIGHT_SHIFT).unwrap();
        assert_eq!(left & right, MAC_FLAG_SHIFT);
        assert_ne!(left, right);
        assert_eq!(macos_modifier_flags(0x00), None);
    }

    #[test]
    fn macos_event_flags_combine_held_modifiers_of_all_clients() {
        let mut state = RemoteInputState::default();
        let flags = |state: &RemoteInputState<KeyIdentity<MacKeyCode>>| {
            macos_event_flags(state.held_keys())
        };
        assert_eq!(flags(&state), 0);
        state.key_event(1, macos_key_identity(0xffeb), true); // Command
        state.key_event(1, macos_key_identity('c' as u32), true);
        state.key_event(2, macos_key_identity(0xffe2), true); // Right Shift
        let held = flags(&state);
        assert_eq!(
            held & (MAC_FLAG_COMMAND | MAC_FLAG_SHIFT | MAC_FLAG_OPTION | MAC_FLAG_CONTROL),
            MAC_FLAG_COMMAND | MAC_FLAG_SHIFT
        );
        assert_eq!(held & 0xffff, 0x0008 | 0x0004);
        state.disconnect(1);
        assert_eq!(flags(&state), MAC_FLAG_SHIFT | 0x0004);
        // Caps Lock contributes no held flag; the backend uses its lock state.
        state.key_event(3, macos_key_identity(0xffe5), true);
        assert_eq!(flags(&state), MAC_FLAG_SHIFT | 0x0004);

        assert_eq!(
            macos_key_flags(0x7b),
            MAC_FLAG_NUMERIC_PAD | MAC_FLAG_SECONDARY_FN
        );
        assert_eq!(macos_key_flags(0x52), MAC_FLAG_NUMERIC_PAD);
        assert_eq!(macos_key_flags(0x4c), MAC_FLAG_NUMERIC_PAD);
        assert_eq!(macos_key_flags(0x7a), MAC_FLAG_SECONDARY_FN);
        assert_eq!(macos_key_flags(0x75), MAC_FLAG_SECONDARY_FN);
        assert_eq!(macos_key_flags(0x00), 0);
        assert_eq!(macos_key_flags(0x24), 0);
        assert_eq!(macos_key_flags(MAC_KEY_COMMAND), 0);
    }

    #[test]
    fn display_points_land_inside_the_target_pixel() {
        let bounds = |x, y, width, height| DisplayBounds {
            x,
            y,
            width,
            height,
        };
        let layouts = [
            ((1920, 1080), bounds(0.0, 0.0, 1920.0, 1080.0)), // 1x
            ((3024, 1964), bounds(0.0, 0.0, 1512.0, 982.0)),  // Retina 2x
            ((2560, 1440), bounds(-1440.0, -900.0, 1440.0, 810.0)), // Left of and above main
            ((3840, 2160), bounds(1512.0, 0.0, 2560.0, 1440.0)), // 1.5x
            ((3600, 2338), bounds(-1800.0, 120.0, 1800.0, 1169.0)), // Scaled Retina mode
            ((2880, 1800), bounds(0.0, -1117.0, 1728.0, 1117.0)), // Fractional 1.6667x
        ];
        for ((width, height), display) in layouts {
            let scale_x = display.width / f64::from(width);
            let scale_y = display.height / f64::from(height);
            for (px, py) in [
                (0, 0),
                (1, 1),
                (width / 3, height / 2),
                (width - 1, height - 1),
            ] {
                let (x, y) = display_point(px, py, (width, height), display).unwrap();
                let left = display.x + f64::from(px) * scale_x;
                let top = display.y + f64::from(py) * scale_y;
                assert!(x > left && x < left + scale_x, "x for {px} at {display:?}");
                assert!(y > top && y < top + scale_y, "y for {py} at {display:?}");
                // macOS maps a point back to the pixel by scaling its offset.
                assert_eq!(((x - display.x) / scale_x).floor() as u16, px);
                assert_eq!(((y - display.y) / scale_y).floor() as u16, py);
            }
        }
        let display = bounds(-100.0, 0.0, 100.0, 50.0);
        assert_eq!(
            display_point(500, 500, (200, 100), display),
            display_point(199, 99, (200, 100), display)
        );
        assert_eq!(display_point(0, 0, (0, 100), display), None);
        assert_eq!(
            display_point(0, 0, (10, 10), bounds(0.0, 0.0, 0.0, 10.0)),
            None
        );
        assert_eq!(
            display_point(0, 0, (10, 10), bounds(f64::NAN, 0.0, 10.0, 10.0)),
            None
        );
    }

    #[test]
    fn click_counts_follow_button_interval_and_distance() {
        let interval = Duration::from_millis(500);
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut clicks = ClickTracker::default();
        assert_eq!(clicks.release(1), 1);
        assert_eq!(clicks.press(1, 100, 100, at(0), interval), 1);
        assert_eq!(clicks.release(1), 1);
        assert_eq!(clicks.press(1, 102, 99, at(200), interval), 2);
        assert_eq!(clicks.release(1), 2);
        assert_eq!(clicks.press(1, 101, 103, at(400), interval), 3);
        assert_eq!(clicks.release(1), 3);
        // Too late, too far, or another button starts a new sequence.
        assert_eq!(clicks.press(1, 101, 103, at(901), interval), 1);
        assert_eq!(
            clicks.press(1, 101 + CLICK_SLOP_PIXELS + 1, 103, at(1000), interval),
            1
        );
        assert_eq!(clicks.press(4, 106, 103, at(1100), interval), 1);
        assert_eq!(clicks.release(1), 1);
        assert_eq!(clicks.press(4, 106, 103, at(1600), interval), 2);
    }

    #[test]
    fn dirty_rectangles_are_validated_and_clipped() {
        let regions = |rects: &[Rect]| FrameDamage::Regions(rects.to_vec());
        assert_eq!(frame_damage(None, 100, 50), FrameDamage::Full);
        assert_eq!(frame_damage(Some(&[]), 100, 50), FrameDamage::Full);
        assert_eq!(
            frame_damage(
                Some(&[
                    [10.0, 5.0, 20.0, 10.0],
                    [-5.5, 40.2, 10.0, 30.0],
                    [99.5, 0.0, 1.0, 1.0]
                ]),
                100,
                50
            ),
            regions(&[
                Rect::new(10, 5, 30, 15),
                Rect::new(0, 40, 5, 50),
                Rect::new(99, 0, 100, 1),
            ])
        );
        // Invalid or off-display rectangles are dropped; none left is full.
        let invalid = [
            [f64::NAN, 0.0, 1.0, 1.0],
            [0.0, 0.0, f64::INFINITY, 1.0],
            [0.0, 0.0, -1.0, 1.0],
            [0.0, 0.0, 1.0, 0.0],
            [200.0, 0.0, 10.0, 10.0],
            [0.0, -20.0, 10.0, 10.0],
        ];
        assert_eq!(frame_damage(Some(&invalid), 100, 50), FrameDamage::Full);
        let mut mixed = invalid.to_vec();
        mixed.push([1.0, 1.0, 1.0, 1.0]);
        assert_eq!(
            frame_damage(Some(&mixed), 100, 50),
            regions(&[Rect::new(1, 1, 2, 2)])
        );
        assert_eq!(
            frame_damage(Some(&[[0.0, 0.0, 1e300, 1e300]]), 100, 50),
            regions(&[Rect::new(0, 0, 100, 50)])
        );
        // Too many rectangles collapse to their bounding box.
        let many = (0..=MAX_FRAME_DAMAGE_RECTS)
            .map(|index| [index as f64, 2.0, 1.0, 1.0])
            .collect::<Vec<_>>();
        assert_eq!(
            frame_damage(Some(&many), 100, 50),
            regions(&[Rect::new(0, 2, MAX_FRAME_DAMAGE_RECTS as i32 + 1, 3)])
        );
        assert_eq!(
            regions(&[Rect::new(-5, 0, 3, 3), Rect::new(200, 0, 300, 3)])
                .rects(Rect::new(0, 0, 100, 50)),
            [Rect::new(0, 0, 3, 3)]
        );
        assert_eq!(
            FrameDamage::Full.rects(Rect::new(0, 0, 4, 4)),
            [Rect::new(0, 0, 4, 4)]
        );
    }

    #[test]
    fn frame_slot_keeps_only_the_newest_frame_and_merges_damage() {
        let slot = FrameSlot::default();
        let wait = Duration::from_millis(1);
        assert_eq!(slot.take_timeout(wait), None);
        let a = Rect::new(0, 0, 1, 1);
        let b = Rect::new(5, 5, 6, 6);
        assert_eq!(slot.publish(1, FrameDamage::Regions(vec![a])), None);
        assert_eq!(slot.publish(2, FrameDamage::Regions(vec![b])), Some(1));
        assert_eq!(
            slot.take_timeout(wait),
            Some((2, FrameDamage::Regions(vec![a, b])))
        );
        assert_eq!(slot.take_timeout(wait), None);

        slot.publish(3, FrameDamage::Full);
        slot.publish(4, FrameDamage::Regions(vec![a]));
        assert_eq!(slot.take_timeout(wait), Some((4, FrameDamage::Full)));

        // Damage merged across many replaced frames stays bounded.
        for frame in 0..1000 {
            slot.publish(
                frame,
                FrameDamage::Regions(vec![Rect::new(frame, 0, frame + 1, 1)]),
            );
        }
        let (frame, damage) = slot.take_timeout(wait).unwrap();
        assert_eq!(frame, 999);
        let FrameDamage::Regions(rects) = damage else {
            panic!("expected regions");
        };
        assert!(rects.len() <= MAX_FRAME_DAMAGE_RECTS);
        assert!(rects.iter().any(|rect| rect.left == 0));
        assert!(rects.iter().any(|rect| rect.right == 1000));

        // A waiting consumer wakes when a frame arrives.
        std::thread::scope(|scope| {
            let consumer = scope.spawn(|| slot.take_timeout(Duration::from_secs(5)));
            std::thread::sleep(Duration::from_millis(20));
            slot.publish(7, FrameDamage::Full);
            assert_eq!(consumer.join().unwrap(), Some((7, FrameDamage::Full)));
        });
    }

    #[test]
    fn latin1_clipboard_text_becomes_a_string() {
        assert_eq!(latin1_to_string(&[b'h', 0xe9, b'\n', 0xff]), "hé\nÿ");
        assert_eq!(
            latin1_from_unicode(&latin1_to_string(&[0xa9, b'x'])),
            [0xa9, b'x']
        );
    }
}
