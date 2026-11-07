use std::collections::HashMap;
use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use topvnc::{
    BUTTON_BACK, BUTTON_FORWARD, BUTTON_LEFT, BUTTON_MIDDLE, BUTTON_RIGHT, BUTTON_WHEEL_DOWN,
    BUTTON_WHEEL_LEFT, BUTTON_WHEEL_RIGHT, BUTTON_WHEEL_UP, Encoding, Framebuffer, InputWriter,
    ListenerId, ListenerSecurity, PerformanceMode, ServerListeners, Session, StatsSnapshot,
    encoding_name,
};
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{DeviceEvent, ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowAttributes};

// Each host backend uses part of the shared logic; tests cover all of it.
#[allow(dead_code)]
mod desktop_host;
#[cfg(target_os = "macos")]
mod macos_server;
#[cfg(windows)]
mod windows_server;

mod helpers;
mod settings;
mod ui;
mod web_access;
mod window;
use desktop_host::{DisplayArea, HostPermissions, ServeOptions, ServerNotice};
use ui::{
    Box2, Canvas, Compression, Config, Field, Quality, ServeRequest, ServerPhase, ServerView, Tab,
    UiState, WindowMode,
};
use web_access::runtime::{HostServer, ServerRequest, WebRuntime};
use window::{Draw, Layer, Presenter, Ui, UiEvent};
mod session_render;
use session_render::{RenderOptions, RenderWorker};
use topvnc::presentation::{FrameMailbox, MotionPresentation};

fn encoding(config: &Config) -> Encoding {
    match config.compression {
        Compression::Raw => Encoding::Raw,
        Compression::Zlib => Encoding::Zlib,
        Compression::Tight => Encoding::Tight {
            quality: config.jpeg_quality,
        },
    }
}

fn connect_native(config: &Config) -> std::io::Result<Session> {
    let address = config.address().map_err(std::io::Error::other)?;
    Session::connect_with_transport(
        &address,
        config.tls.client(&config.host)?,
        config.allow_insecure,
        encoding(config),
        || Ok(config.password.clone()),
    )
}

const USAGE: &str = "usage: topvnc [HOST:PORT] [--tls-ca CA.pem] [--tls-server-name NAME] [--tls-cert CERT.pem --tls-key KEY.pem] [--unencrypted] [--allow-insecure] [--fit | --native-size | --window WIDTHxHEIGHT] [--quality 0-9] [--progressive] [--ultra] [--smooth-motion | --immediate] [--input-debug]";

fn parse_quality(value: &str) -> Result<u8, Box<dyn Error>> {
    match value.parse::<u8>() {
        Ok(quality) if quality <= 9 => Ok(quality),
        _ => Err("--quality must be a JPEG quality level from 0 to 9".into()),
    }
}

fn parse_size(value: &str) -> Result<(usize, usize), Box<dyn Error>> {
    let (width, height) = value
        .split_once('x')
        .ok_or("window size must be WIDTHxHEIGHT")?;
    let width: usize = width.parse()?;
    let height: usize = height.parse()?;
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Err("window dimensions must be between 1 and 8192".into());
    }
    Ok((width, height))
}

fn fit_dimensions(remote: (usize, usize), available: (usize, usize)) -> (usize, usize) {
    let scale = (available.0 as f64 / remote.0 as f64)
        .min(available.1 as f64 / remote.1 as f64)
        .min(1.0);
    (
        (remote.0 as f64 * scale).floor().max(1.0) as usize,
        (remote.1 as f64 * scale).floor().max(1.0) as usize,
    )
}

fn display_space() -> (usize, usize) {
    let display = display_info::DisplayInfo::all()
        .ok()
        .and_then(|displays| displays.into_iter().find(|item| item.is_primary));
    let (width, height) = if let Some(display) = display {
        #[cfg(target_os = "windows")]
        let factor = display.scale_factor.max(1.0) as f64;
        #[cfg(not(target_os = "windows"))]
        let factor = 1.0;
        (
            (display.width as f64 / factor) as usize,
            (display.height as f64 / factor) as usize,
        )
    } else {
        (1280, 720)
    };
    (
        width.saturating_mul(95) / 100,
        height.saturating_mul(85) / 100,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

impl Rect {
    fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    fn width(self) -> usize {
        self.x1 - self.x0
    }

    fn height(self) -> usize {
        self.y1 - self.y0
    }
}

fn draw_rect(remote: (usize, usize), window: (usize, usize)) -> Option<Rect> {
    if remote.0 == 0 || remote.1 == 0 || window.0 == 0 || window.1 == 0 {
        return None;
    }
    let remote_aspect = remote.0 as f64 / remote.1 as f64;
    let window_aspect = window.0 as f64 / window.1 as f64;
    // Round so a window with the remote's own size draws it unscaled.
    let (width, height) = if remote_aspect > window_aspect {
        (window.0, (window.0 as f64 / remote_aspect).round() as usize)
    } else {
        ((window.1 as f64 * remote_aspect).round() as usize, window.1)
    };
    let width = width.max(1).min(window.0);
    let height = height.max(1).min(window.1);
    let x0 = (window.0 - width) / 2;
    let y0 = (window.1 - height) / 2;
    Some(Rect {
        x0,
        y0,
        x1: x0 + width,
        y1: y0 + height,
    })
}

/// Where the remote image is drawn in a `window`-sized window, and which
/// part of it, both in logical pixels. Fit and Custom scale the whole image
/// to fit; Native shows one remote pixel per window pixel, centered and
/// cropped.
fn placement(
    mode: WindowMode,
    remote: (usize, usize),
    window: (usize, usize),
) -> Option<(Rect, Rect)> {
    if mode != WindowMode::Native {
        let target = draw_rect(remote, window)?;
        let source = Rect {
            x0: 0,
            y0: 0,
            x1: remote.0,
            y1: remote.1,
        };
        return Some((target, source));
    }
    if remote.0 == 0 || remote.1 == 0 || window.0 == 0 || window.1 == 0 {
        return None;
    }
    let width = remote.0.min(window.0);
    let height = remote.1.min(window.1);
    let target_x = (window.0 - width) / 2;
    let target_y = (window.1 - height) / 2;
    let source_x = (remote.0 - width) / 2;
    let source_y = (remote.1 - height) / 2;
    Some((
        Rect {
            x0: target_x,
            y0: target_y,
            x1: target_x + width,
            y1: target_y + height,
        },
        Rect {
            x0: source_x,
            y0: source_y,
            x1: source_x + width,
            y1: source_y + height,
        },
    ))
}

/// Frame rate, size, bandwidth, and encoding between two stats snapshots.
fn throughput(before: StatsSnapshot, after: StatsSnapshot, elapsed: std::time::Duration) -> String {
    let seconds = elapsed.as_secs_f64().max(1e-3);
    let frames = after.frames.saturating_sub(before.frames);
    let bytes = after.bytes.saturating_sub(before.bytes) as f64;
    format!(
        "{:.0} fps · {:.0} KB/frame · {:.0} Mbit/s · {}{}",
        frames as f64 / seconds,
        bytes / frames.max(1) as f64 / 1000.0,
        bytes * 8.0 / seconds / 1e6,
        after.encoding.map_or("waiting", encoding_name),
        // Continuous updates: the server pushes frames without requests.
        if after.continuous_updates {
            " · push"
        } else {
            ""
        }
    )
}

/// Map a mouse position in an aspect-fitted window to remote framebuffer pixels.
fn remote_pointer(
    x: f64,
    y: f64,
    window: (usize, usize),
    remote: (usize, usize),
) -> Option<(u16, u16)> {
    if !x.is_finite()
        || !y.is_finite()
        || window.0 == 0
        || window.1 == 0
        || remote.0 == 0
        || remote.1 == 0
    {
        return None;
    }
    let draw = draw_rect(remote, window)?;
    let rx = ((x - draw.x0 as f64) * (remote.0 - 1) as f64
        / (draw.x1 - draw.x0).saturating_sub(1).max(1) as f64)
        .floor()
        .clamp(0.0, (remote.0 - 1) as f64) as u16;
    let ry = ((y - draw.y0 as f64) * (remote.1 - 1) as f64
        / (draw.y1 - draw.y0).saturating_sub(1).max(1) as f64)
        .floor()
        .clamp(0.0, (remote.1 - 1) as f64) as u16;
    Some((rx, ry))
}

/// Map a mouse position in logical window pixels to the remote pixel shown
/// there in `mode`.
fn pointer_for_mode(
    x: f64,
    y: f64,
    window: (usize, usize),
    remote: (usize, usize),
    mode: WindowMode,
) -> Option<(u16, u16)> {
    if mode != WindowMode::Native {
        return remote_pointer(x, y, window, remote);
    }
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let (target, source) = placement(mode, remote, window)?;
    let rx = (x.floor() as isize - target.x0 as isize + source.x0 as isize)
        .clamp(0, remote.0 as isize - 1) as u16;
    let ry = (y.floor() as isize - target.y0 as isize + source.y0 as isize)
        .clamp(0, remote.1 as isize - 1) as u16;
    Some((rx, ry))
}

/// The keysym a physical key sends, as on a US keyboard with Shift held or
/// not. Games bind physical keys, so keypad keys keep their own keysyms.
fn keysym(key: KeyCode, shift: bool) -> Option<u32> {
    use KeyCode::*;
    const LETTERS: [KeyCode; 26] = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO,
        KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ];
    const DIGITS: [KeyCode; 10] = [
        Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
    ];
    const KEYPAD: [KeyCode; 10] = [
        Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9,
    ];
    const FUNCTION: [KeyCode; 24] = [
        F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20,
        F21, F22, F23, F24,
    ];
    let index = |keys: &[KeyCode]| {
        keys.iter()
            .position(|code| *code == key)
            .map(|index| index as u32)
    };
    if let Some(index) = index(&LETTERS) {
        return Some(if shift { 0x41 } else { 0x61 } + index);
    }
    if let Some(index) = index(&DIGITS) {
        return Some(if shift {
            u32::from(b")!@#$%^&*("[index as usize])
        } else {
            0x30 + index
        });
    }
    if let Some(index) = index(&KEYPAD) {
        return Some(0xffb0 + index);
    }
    if let Some(index) = index(&FUNCTION) {
        return Some(0xffbe + index);
    }
    let shifted = |plain: u8, shifted: u8| u32::from(if shift { shifted } else { plain });
    Some(match key {
        NumpadDecimal => 0xffae,
        NumpadDivide => 0xffaf,
        NumpadMultiply => 0xffaa,
        NumpadSubtract => 0xffad,
        NumpadAdd => 0xffab,
        NumpadEnter => 0xff8d,
        NumpadEqual => 0xffbd,
        Space => 0x20,
        Quote => shifted(b'\'', b'"'),
        Backquote => shifted(b'`', b'~'),
        Backslash => shifted(b'\\', b'|'),
        Comma => shifted(b',', b'<'),
        Equal => shifted(b'=', b'+'),
        BracketLeft => shifted(b'[', b'{'),
        Minus => shifted(b'-', b'_'),
        Period => shifted(b'.', b'>'),
        BracketRight => shifted(b']', b'}'),
        Semicolon => shifted(b';', b':'),
        Slash => shifted(b'/', b'?'),
        Enter => 0xff0d,
        Tab => 0xff09,
        Backspace => 0xff08,
        Escape => 0xff1b,
        Delete => 0xffff,
        Insert => 0xff63,
        Home => 0xff50,
        End => 0xff57,
        PageUp => 0xff55,
        PageDown => 0xff56,
        ArrowLeft => 0xff51,
        ArrowUp => 0xff52,
        ArrowRight => 0xff53,
        ArrowDown => 0xff54,
        ShiftLeft => 0xffe1,
        ShiftRight => 0xffe2,
        ControlLeft => 0xffe3,
        ControlRight => 0xffe4,
        AltLeft => 0xffe9,
        AltRight => 0xffea,
        SuperLeft => 0xffeb,
        SuperRight => 0xffec,
        CapsLock => 0xffe5,
        NumLock => 0xff7f,
        ScrollLock => 0xff14,
        Pause => 0xff13,
        PrintScreen => 0xff61,
        ContextMenu => 0xff67,
        _ => return None,
    })
}

fn translated_key_event(
    pressed_keys: &mut HashMap<KeyCode, u32>,
    key: KeyCode,
    down: bool,
) -> Option<(u32, bool)> {
    if down {
        if pressed_keys.contains_key(&key) {
            return None;
        }
        let shift = pressed_keys.contains_key(&KeyCode::ShiftLeft)
            || pressed_keys.contains_key(&KeyCode::ShiftRight);
        let symbol = keysym(key, shift)?;
        pressed_keys.insert(key, symbol);
        Some((symbol, true))
    } else {
        pressed_keys.remove(&key).map(|symbol| (symbol, false))
    }
}

/// The `BUTTON_*` bit a mouse button holds, if RFB carries it.
fn button_bit(button: MouseButton) -> Option<u16> {
    Some(match button {
        MouseButton::Left => BUTTON_LEFT,
        MouseButton::Middle => BUTTON_MIDDLE,
        MouseButton::Right => BUTTON_RIGHT,
        MouseButton::Back => BUTTON_BACK,
        MouseButton::Forward => BUTTON_FORWARD,
        MouseButton::Other(_) => return None,
    })
}

/// Scroll distance in logical pixels that makes one wheel notch.
const WHEEL_NOTCH_PIXELS: f64 = 40.0;

/// Turns line and pixel scrolling into RFB wheel notches, keeping the
/// remainder of fine trackpad scrolling for the next event.
#[derive(Default)]
struct Wheel {
    vertical: f64,
    horizontal: f64,
}

impl Wheel {
    /// Whole notches scrolled, positive up and right.
    fn notches(&mut self, delta: MouseScrollDelta, scale_factor: f64) -> (i32, i32) {
        let (x, y) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (f64::from(x), f64::from(y)),
            MouseScrollDelta::PixelDelta(position) => (
                position.x / scale_factor / WHEEL_NOTCH_PIXELS,
                position.y / scale_factor / WHEEL_NOTCH_PIXELS,
            ),
        };
        self.horizontal += x;
        self.vertical += y;
        let take = |total: &mut f64| {
            let whole = total.trunc();
            *total -= whole;
            whole as i32
        };
        (take(&mut self.vertical), take(&mut self.horizontal))
    }
}

/// Send one RFB wheel press and release per notch, holding `buttons`.
fn send_wheel(
    writer: &InputWriter,
    buttons: u16,
    (vertical, horizontal): (i32, i32),
    at: Option<(u16, u16)>,
) -> std::io::Result<()> {
    let presses = [
        (vertical.max(0), BUTTON_WHEEL_UP),
        (-vertical.min(0), BUTTON_WHEEL_DOWN),
        (horizontal.max(0), BUTTON_WHEEL_RIGHT),
        (-horizontal.min(0), BUTTON_WHEEL_LEFT),
    ];
    for (count, bit) in presses {
        for _ in 0..count {
            for state in [buttons | bit, buttons] {
                match at {
                    Some((x, y)) => writer.pointer(state, x, y)?,
                    None => writer.pointer_motion(state, 0, 0)?,
                }
            }
        }
    }
    Ok(())
}

/// Lock and hide the pointer so every motion becomes relative input, or
/// release it. Locking keeps the pointer in place (macOS); where only
/// confining is supported (Windows), it is moved to the window's center
/// first, so it stays over the window.
fn lock_pointer(window: &Window, lock: bool) {
    if lock {
        let size = window.inner_size();
        let _ = window.set_cursor_position(PhysicalPosition::new(
            f64::from(size.width) / 2.0,
            f64::from(size.height) / 2.0,
        ));
        let _ = window
            .set_cursor_grab(CursorGrabMode::Locked)
            .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined));
        window.set_cursor_visible(false);
    } else {
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
    }
}

/// Raises the system timer resolution to 1 ms while the viewer runs, so
/// frame-rate limits and waits are not rounded up to 15.6 ms.
#[cfg(windows)]
struct TimerResolution;

#[cfg(windows)]
impl TimerResolution {
    fn new() -> Option<Self> {
        (unsafe { windows_sys::Win32::Media::timeBeginPeriod(1) }
            == windows_sys::Win32::Media::TIMERR_NOERROR)
            .then_some(Self)
    }
}

#[cfg(windows)]
impl Drop for TimerResolution {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Media::timeEndPeriod(1) };
    }
}

/// Apply launch-only options to the form without persisting security choices.
fn apply_viewer_arguments(
    config: &mut Config,
    arguments: impl IntoIterator<Item = String>,
) -> Result<bool, Box<dyn Error>> {
    let mut input_debug = false;
    let mut address = None;
    let mut args = arguments.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--unencrypted" => config.tls.unencrypted = true,
            "--tls-ca" => {
                config.tls.ca = Some(args.next().ok_or("--tls-ca requires CA.pem")?.into())
            }
            "--tls-server-name" => {
                config.tls.server_name = Some(args.next().ok_or("--tls-server-name requires NAME")?)
            }
            "--tls-cert" => {
                config.tls.cert = Some(args.next().ok_or("--tls-cert requires CERT.pem")?.into())
            }
            "--tls-key" => {
                config.tls.key = Some(args.next().ok_or("--tls-key requires KEY.pem")?.into())
            }
            "--allow-insecure" => config.allow_insecure = true,
            "--fit" => config.window_mode = WindowMode::Fit,
            "--native-size" => config.window_mode = WindowMode::Native,
            "--window" => {
                let value = args.next().ok_or("--window requires WIDTHxHEIGHT")?;
                parse_size(&value)?;
                config.window_mode = WindowMode::Custom;
                config.window_size = value;
            }
            "--quality" => {
                let value = args
                    .next()
                    .ok_or("--quality requires a level from 0 to 9")?;
                config.jpeg_quality = parse_quality(&value)?;
            }
            "--progressive" => config.progressive = true,
            "--ultra" => config.performance = PerformanceMode::Ultra,
            "--smooth-motion" => config.motion = MotionPresentation::Smooth,
            "--immediate" => config.motion = MotionPresentation::Immediate,
            "--input-debug" => input_debug = true,
            _ if arg.starts_with('-') || address.is_some() => return Err(USAGE.into()),
            _ => address = Some(arg),
        }
    }
    if let Some(address) = address {
        let (host, port) = address
            .rsplit_once(':')
            .ok_or("address must be HOST:PORT")?;
        config.host = host.to_string();
        config.port = port.to_string();
    }

    config.serve.tls = config.tls.clone();
    Ok(input_debug)
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments
        .first()
        .is_some_and(|argument| argument == "--serve")
    {
        return run_server(&arguments[1..]);
    }
    let mut config = Config::default();
    settings::load(&mut config);
    let input_debug = apply_viewer_arguments(&mut config, arguments)?;
    config.tls.client(&config.host)?;

    #[cfg(windows)]
    let _timer = TimerResolution::new();
    let mut ui = Ui::new()?;
    let mut connection_error = None;
    // Lives across viewer sessions; dropping it stops the server.
    let mut server = HostedServer::default();
    // Declared after the server so it is dropped first: the gateway and the
    // tunnel stop before the server they relay to.
    let mut web = WebRuntime::new(Box::new(web_access::process::ProcessSpawner));
    loop {
        let Some((next_config, session)) = show_landing(
            &mut ui,
            config,
            connection_error.take(),
            &mut server,
            &mut web,
        )?
        else {
            return Ok(());
        };
        config = next_config;
        connection_error = match run_session(
            &mut ui,
            session,
            &mut config,
            input_debug,
            &mut server,
            &mut web,
        ) {
            Ok(error) => error,
            Err(error) => Some(error.to_string()),
        };
        if let Some(error) = &connection_error {
            eprintln!("Session ended: {error}");
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
const SERVER_UNAVAILABLE: &str =
    "the desktop capture and input server backend is available on Windows and macOS only";

#[cfg(windows)]
fn run_server(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    windows_server::run(arguments)
}

#[cfg(target_os = "macos")]
fn run_server(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    macos_server::run(arguments)
}

#[cfg(windows)]
fn copy_text(text: &str) -> Result<(), String> {
    windows_server::copy_text(text)
}

#[cfg(target_os = "macos")]
fn copy_text(text: &str) -> Result<(), String> {
    macos_server::copy_text(text)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn copy_text(_text: &str) -> Result<(), String> {
    Err("Copying to the clipboard is available on Windows and macOS only.".into())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn run_server(_arguments: &[String]) -> Result<(), Box<dyn Error>> {
    Err(SERVER_UNAVAILABLE.into())
}

#[cfg(windows)]
fn serve_desktop(
    options: ServeOptions,
    password: Option<String>,
    shutdown: &AtomicBool,
    report: &(dyn Fn(ServerNotice) + Sync),
) -> Result<(), String> {
    windows_server::serve(options, password, shutdown, report).map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
fn serve_desktop(
    options: ServeOptions,
    password: Option<String>,
    shutdown: &AtomicBool,
    report: &(dyn Fn(ServerNotice) + Sync),
) -> Result<(), String> {
    macos_server::serve(options, password, shutdown, report).map_err(|error| error.to_string())
}

/// Permission state the Server tab shows while stopped, on hosts that need it.
#[cfg(target_os = "macos")]
fn host_permissions() -> Option<HostPermissions> {
    Some(macos_server::permissions())
}

#[cfg(not(target_os = "macos"))]
fn host_permissions() -> Option<HostPermissions> {
    None
}

/// Displays the Server tab offers, where the host lists them.
#[cfg(windows)]
fn host_displays() -> Option<Vec<DisplayArea>> {
    Some(windows_server::displays())
}

#[cfg(not(windows))]
fn host_displays() -> Option<Vec<DisplayArea>> {
    None
}

/// Whether the host can serve all displays side by side.
const HOST_SPANS_DISPLAYS: bool = cfg!(windows);

#[cfg(not(any(windows, target_os = "macos")))]
fn serve_desktop(
    _options: ServeOptions,
    _password: Option<String>,
    _shutdown: &AtomicBool,
    _report: &(dyn Fn(ServerNotice) + Sync),
) -> Result<(), String> {
    Err(SERVER_UNAVAILABLE.into())
}

enum HostEvent {
    Notice(ServerNotice),
    Stopped(Result<(), String>),
}

/// A tab that serves this desktop: the Server tab for native viewers, and
/// the WEB tab for the browser gateway.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    ServerTab,
    Web,
}

impl Owner {
    fn index(self) -> usize {
        self as usize
    }

    fn other(self) -> Self {
        match self {
            Self::ServerTab => Self::Web,
            Self::Web => Self::ServerTab,
        }
    }
}

/// The listener a tab asked for on the desktop server.
#[derive(Debug)]
struct Claim {
    address: String,
    tls: topvnc::transport::TlsOptions,
    password: Option<String>,
    allow_insecure: bool,
    /// Open on the server; `None` while it waits for the server to start.
    listener: Option<(ListenerId, String)>,
}

impl Claim {
    fn new(request: &ServeRequest) -> Self {
        Self {
            address: request.address.clone(),
            tls: request.tls.clone(),
            password: request.password.clone(),
            allow_insecure: request.allow_insecure,
            listener: None,
        }
    }

    /// Both listen on one address, which only one socket can hold.
    fn same_address(&self, other: &Self) -> bool {
        match (
            self.address.parse::<std::net::SocketAddr>(),
            other.address.parse::<std::net::SocketAddr>(),
        ) {
            (Ok(mine), Ok(theirs)) => mine == theirs,
            _ => self.address == other.address,
        }
    }

    /// Viewers of either could use the other's listener as it is.
    fn same_security(&self, other: &Self) -> bool {
        self.tls == other.tls
            && self.password == other.password
            && self.allow_insecure == other.allow_insecure
    }
}

/// The desktop server the Server tab and the WEB tab share. One capture
/// serves every viewer; each tab adds its own listener, with its own
/// address, transport security, and password, and the server runs while
/// either tab has one. It runs on a background thread so the UI and viewer
/// sessions stay responsive.
#[derive(Default)]
struct HostedServer {
    /// What the Server tab shows: its own listener on the shared server.
    view: ServerView,
    /// The server as a whole; its address is the first listener's.
    phase: ServerPhase,
    running: Option<(
        Arc<AtomicBool>,
        mpsc::Receiver<HostEvent>,
        thread::JoinHandle<()>,
    )>,
    /// Reported by the server thread once it binds its first listener.
    listeners: Option<ServerListeners>,
    claims: [Option<Claim>; 2],
    /// The tab whose request the server was started with; that listener is
    /// the one the server binds first.
    first: Option<Owner>,
    /// Listener failures that did not stop the server, for the next poll.
    failure: Option<String>,
}

impl HostedServer {
    fn claim(&self, owner: Owner) -> Option<&Claim> {
        self.claims[owner.index()].as_ref()
    }

    /// The server as one tab sees it: serving only on its own listener.
    fn phase_for(&self, owner: Owner) -> ServerPhase {
        let claim = self.claim(owner);
        match (&self.phase, claim) {
            (ServerPhase::Stopped, _) => ServerPhase::Stopped,
            (ServerPhase::Stopping, _) => ServerPhase::Stopping,
            (_, None) => ServerPhase::Stopped,
            (ServerPhase::Starting, Some(_)) => ServerPhase::Starting,
            (ServerPhase::Serving { .. }, Some(Claim { listener: None, .. })) => {
                ServerPhase::Starting
            }
            (
                ServerPhase::Serving {
                    width,
                    height,
                    connections,
                    view_only,
                    ..
                },
                Some(Claim {
                    listener: Some((_, address)),
                    password,
                    ..
                }),
            ) => ServerPhase::Serving {
                address: address.clone(),
                width: *width,
                height: *height,
                connections: *connections,
                authenticated: password.is_some(),
                view_only: *view_only,
            },
        }
    }

    /// The WEB tab's listener, as it needs to see it.
    fn web_view(&self) -> HostServer {
        match self.phase_for(Owner::Web) {
            ServerPhase::Stopped => HostServer::Stopped,
            ServerPhase::Starting => HostServer::Starting,
            ServerPhase::Serving { .. } => HostServer::Serving,
            ServerPhase::Stopping => HostServer::Stopping,
        }
    }

    fn refresh(&mut self) {
        self.view.phase = self.phase_for(Owner::ServerTab);
    }

    /// Serve `owner`'s listener: start the server with it, or add it to the
    /// server the other tab runs.
    fn start(&mut self, owner: Owner, request: ServeRequest) -> Result<(), String> {
        if self.claim(owner).is_some() {
            return Ok(());
        }
        let result = match self.phase {
            _ if self.running.is_none() => self.spawn(owner, request),
            ServerPhase::Stopping => Err("Wait for the desktop server to finish stopping.".into()),
            _ => {
                self.claims[owner.index()] = Some(Claim::new(&request));
                // Before the server reports its listeners, the claim waits.
                let joined = if self.listeners.is_some() {
                    self.attach(owner)
                } else {
                    Ok(())
                };
                if joined.is_ok() && owner == Owner::ServerTab {
                    self.view.message = Some(
                        "Joined the desktop server browser access runs; its display and \
                         size stay as they started."
                            .into(),
                    );
                }
                joined
            }
        };
        self.refresh();
        result
    }

    fn spawn(&mut self, owner: Owner, request: ServeRequest) -> Result<(), String> {
        let claim = Claim::new(&request);
        let shutdown = Arc::new(AtomicBool::new(false));
        let (events, receiver) = mpsc::channel();
        let options = ServeOptions {
            tls: request.tls,
            performance: request.performance,
            address: request.address,
            display: request.display,
            allow_insecure: request.allow_insecure,
            scale: request.scale,
            mouse: request.mouse,
            priority: request.priority,
        };
        let password = request.password;
        let thread_shutdown = Arc::clone(&shutdown);
        let thread = thread::Builder::new()
            .name("topvnc-server".into())
            .spawn(move || {
                // `mpsc::Sender` is not `Sync`; the capture and input threads
                // share this one through the lock.
                let notices = Mutex::new(events.clone());
                let report = |notice: ServerNotice| {
                    if let Ok(notices) = notices.lock() {
                        let _ = notices.send(HostEvent::Notice(notice));
                    }
                };
                let result = serve_desktop(options, password, &thread_shutdown, &report);
                let _ = events.send(HostEvent::Stopped(result));
            })
            .map_err(|error| format!("Could not start the server: {error}"))?;
        self.phase = ServerPhase::Starting;
        self.view.message = None;
        self.claims[owner.index()] = Some(claim);
        self.first = Some(owner);
        self.running = Some((shutdown, receiver, thread));
        Ok(())
    }

    /// Open `owner`'s waiting listener on the running server. A listener the
    /// other tab already has on the same address is shared when its viewers
    /// would connect the same way; otherwise the address conflicts.
    fn attach(&mut self, owner: Owner) -> Result<(), String> {
        let (Some(listeners), Some(claim)) = (&self.listeners, self.claim(owner)) else {
            return Ok(());
        };
        if claim.listener.is_some() {
            return Ok(());
        }
        let result = match self.claim(owner.other()) {
            Some(other) if other.listener.is_some() && claim.same_address(other) => {
                if claim.same_security(other) {
                    Ok(other.listener.clone().expect("checked above"))
                } else {
                    Err(match owner {
                        Owner::Web => format!(
                            "The Server tab already listens on {}, the address browser access \
                             needs for its gateway. Give the Server tab another port.",
                            claim.address
                        ),
                        Owner::ServerTab => format!(
                            "Browser access already listens on {} for its gateway. Choose \
                             another port.",
                            claim.address
                        ),
                    })
                }
            }
            _ => claim
                .tls
                .server()
                .map_err(|error| error.to_string())
                .and_then(|tls| {
                    let security = ListenerSecurity {
                        tls,
                        password: claim.password.clone(),
                        allow_insecure: claim.allow_insecure,
                    };
                    let id = listeners
                        .add(claim.address.as_str(), security)
                        .map_err(|error| {
                            format!("Could not listen on {}: {error}", claim.address)
                        })?;
                    let bound = listeners
                        .local_addr(id)
                        .map_or_else(|| claim.address.clone(), |address| address.to_string());
                    Ok((id, bound))
                }),
        };
        match result {
            Ok(listener) => {
                if let Some(claim) = &mut self.claims[owner.index()] {
                    claim.listener = Some(listener);
                }
                Ok(())
            }
            Err(error) => {
                self.claims[owner.index()] = None;
                Err(error)
            }
        }
    }

    /// Close `owner`'s listener, and the server when no tab has one left.
    fn stop(&mut self, owner: Owner) {
        let Some(claim) = self.claims[owner.index()].take() else {
            return;
        };
        if self.first == Some(owner) {
            self.first = None;
        }
        if self.claim(owner.other()).is_none() {
            if let Some((shutdown, ..)) = &self.running {
                shutdown.store(true, Ordering::Release);
                self.phase = ServerPhase::Stopping;
            }
        } else if let (Some(listeners), Some((id, _))) = (&self.listeners, claim.listener) {
            let shared = self
                .claim(owner.other())
                .and_then(|other| other.listener.as_ref())
                .is_some_and(|(other, _)| *other == id);
            if !shared {
                listeners.remove(id);
            }
        }
        self.refresh();
    }

    /// The server reported its listeners: give the first one to the tab it
    /// was started for, and open the listeners other tabs wait for.
    fn adopt(&mut self, listeners: ServerListeners) {
        let primary = listeners.primary();
        self.listeners = Some(listeners);
        if let Some(primary) = primary {
            let address = self
                .listeners
                .as_ref()
                .and_then(|listeners| listeners.local_addr(primary))
                .map(|address| address.to_string());
            match self
                .first
                .and_then(|first| self.claims[first.index()].as_mut())
            {
                Some(claim) if claim.listener.is_none() => {
                    claim.listener = Some((primary, address.unwrap_or_default()));
                }
                // The tab stopped while the server started.
                _ => {
                    if let Some(listeners) = &self.listeners {
                        listeners.remove(primary);
                    }
                }
            }
        }
        for owner in [Owner::ServerTab, Owner::Web] {
            if let Err(error) = self.attach(owner) {
                self.failure = Some(error);
            }
        }
    }

    /// Apply status from the server thread. Returns the error that ended
    /// it, or a tab's listener that could not open.
    fn poll(&mut self) -> Option<String> {
        let Some((_, receiver, _)) = self.running.as_ref() else {
            return self.failure.take();
        };
        let events = receiver.try_iter().collect::<Vec<_>>();
        let mut failure = None;
        let mut stopped = false;
        for event in events {
            match event {
                HostEvent::Notice(ServerNotice::Listeners(listeners)) => self.adopt(listeners),
                HostEvent::Notice(notice) => {
                    eprintln!("Server: {notice}");
                    self.apply(notice);
                }
                HostEvent::Stopped(result) => {
                    stopped = true;
                    if let Err(error) = result {
                        eprintln!("Server stopped: {error}");
                        failure = Some(error);
                    }
                }
            }
        }
        if stopped {
            if let Some((_, _, thread)) = self.running.take() {
                let _ = thread.join();
            }
            self.phase = ServerPhase::Stopped;
            self.view.message = failure.is_none().then(|| "Server stopped.".into());
            self.listeners = None;
            self.claims = [None, None];
            self.first = None;
        }
        self.refresh();
        failure.or_else(|| self.failure.take())
    }

    fn apply(&mut self, notice: ServerNotice) {
        match (notice, &mut self.phase) {
            (
                ServerNotice::Serving {
                    address,
                    width,
                    height,
                    ..
                },
                ServerPhase::Starting,
            ) => {
                self.phase = ServerPhase::Serving {
                    address: address.to_string(),
                    width,
                    height,
                    connections: 0,
                    authenticated: false,
                    view_only: false,
                };
            }
            (
                ServerNotice::Resized { width, height },
                ServerPhase::Serving {
                    width: served_width,
                    height: served_height,
                    ..
                },
            ) => {
                (*served_width, *served_height) = (width, height);
            }
            (ServerNotice::Connections(count), ServerPhase::Serving { connections, .. }) => {
                *connections = count;
            }
            (ServerNotice::ViewOnly(state), ServerPhase::Serving { view_only, .. }) => {
                *view_only = state;
            }
            (ServerNotice::Message(message), _) => self.view.message = Some(message),
            _ => {}
        }
        self.refresh();
    }
}

impl Drop for HostedServer {
    fn drop(&mut self) {
        // Joining lets the server release every remotely held key and button.
        if let Some((shutdown, _, thread)) = self.running.take() {
            shutdown.store(true, Ordering::Release);
            let _ = thread.join();
        }
    }
}

fn show_landing(
    ui: &mut Ui,
    mut config: Config,
    error: Option<String>,
    server: &mut HostedServer,
    web: &mut WebRuntime,
) -> Result<Option<(Config, Session)>, Box<dyn Error>> {
    const WIDTH: usize = 800;
    const HEIGHT: usize = 640;
    let window = ui.create_window(
        WindowAttributes::default()
            .with_title("TopVNC")
            .with_inner_size(LogicalSize::new(WIDTH as f64, HEIGHT as f64))
            .with_resizable(false),
    )?;
    let mut presenter = Presenter::new(ui, Arc::clone(&window))?;
    let layer = presenter.layer(WIDTH as u32, HEIGHT as u32);
    let mut pixels = vec![ui::BG; WIDTH * HEIGHT];
    // The pixels last presented; the form is presented again only when it changes.
    let mut shown = Vec::new();
    let mut redraw = true;
    let mut state = UiState::for_landing(error, web);
    let (result_tx, result_rx) = mpsc::channel::<Result<Session, String>>();
    let mut connecting = false;
    let mut helpers = helpers::HelperRuntime::default();
    let mut permissions_checked: Option<Instant> = None;
    let mut dragging_serve_scale = false;
    // The pointer position in form pixels while it is over the window.
    let mut cursor: Option<(usize, usize)> = None;
    loop {
        helpers.poll();
        // Permissions can change in System Settings while the window is open.
        // So can the attached displays, which the Server tab lists.
        if server.view.phase == ServerPhase::Stopped
            && permissions_checked.is_none_or(|checked| checked.elapsed().as_secs() >= 1)
        {
            server.view.permissions = host_permissions();
            if state.tab == Tab::Server {
                server.view.displays = host_displays();
            }
            permissions_checked = Some(Instant::now());
        }
        if let Some(error) = server.poll() {
            // A server the WEB tab started reports there.
            if state.tab != Tab::Web {
                state.switch_tab(Tab::Server);
            }
            state.error = Some(error);
        }
        if let Err(error) = poll_browser_access(server, web, &config) {
            state.error = Some(error);
        }
        ui::landing(
            &mut Canvas::new(&mut pixels, WIDTH, HEIGHT),
            &config,
            &state,
            connecting,
            &server.view,
            &helpers,
            web,
        );
        if redraw || pixels != shown {
            layer.upload(&pixels, WIDTH, 0, 0, WIDTH, HEIGHT);
            let size = window.inner_size();
            // A window that cannot take a frame yet is drawn again on the
            // next pass.
            redraw = !presenter.draw(&[Draw {
                layer: &layer,
                target: [0.0, 0.0, f64::from(size.width), f64::from(size.height)],
                source: [0.0, 0.0, WIDTH as f64, HEIGHT as f64],
                // Whole-number display scales keep the form's pixels sharp.
                smooth: window.scale_factor().fract() != 0.0,
            }])?;
            shown.clone_from(&pixels);
        }
        if let Ok(result) = result_rx.try_recv() {
            connecting = false;
            match result {
                Ok(session) => {
                    if let Err(error) = settings::save(&config) {
                        eprintln!("Could not remember the last session: {error}");
                    }
                    return Ok(Some((config, session)));
                }
                Err(error) => state.error = Some(error),
            }
        }
        let mut connect_clicked = false;
        let mut server_clicked = false;
        let mut web_clicked = false;
        let mut pair_clicked = false;
        let mut copy_url_clicked = false;
        let mut helper_action = None;
        // Woken by input; the timeout picks up server status and the
        // connection result.
        for event in ui.pump(Duration::from_millis(50)) {
            let UiEvent::Window(id, event) = event else {
                continue;
            };
            if id != window.id() {
                continue;
            }
            match event {
                WindowEvent::CloseRequested => return Ok(None),
                WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::RedrawRequested => redraw = true,
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed =>
                {
                    match event.physical_key {
                        PhysicalKey::Code(KeyCode::Escape) => return Ok(None),
                        _ if connecting => {}
                        PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter) => {
                            match state.tab {
                                Tab::Connect => connect_clicked = true,
                                // Enter only starts the server; stopping takes a click.
                                Tab::Server => {
                                    server_clicked = server.view.phase == ServerPhase::Stopped;
                                }
                                // Enter only starts browser access too.
                                Tab::Web => web_clicked = web.is_idle(),
                                Tab::Work => helper_action = Some(helpers::Action::CheckWork),
                                Tab::Network => helper_action = Some(helpers::Action::Network),
                                Tab::Firewall => helper_action = Some(helpers::Action::Firewall),
                            }
                        }
                        PhysicalKey::Code(code) if state.key(&mut config, code) => {}
                        _ => {
                            for character in event.text.iter().flat_map(|text| text.chars()) {
                                state.character(&mut config, character);
                            }
                        }
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    let scale = window.scale_factor();
                    let (x, y) = (position.x / scale, position.y / scale);
                    cursor = (x >= 0.0 && y >= 0.0).then_some((x as usize, y as usize));
                    // The served-size slider follows the mouse until the button is
                    // released; it is locked while the server runs.
                    if dragging_serve_scale && let Some((x, _)) = cursor {
                        if state.tab == Tab::Server && server.view.phase == ServerPhase::Stopped {
                            config.serve.scale = ui::serve_scale_from_slider_x(x);
                        } else {
                            dragging_serve_scale = false;
                        }
                    }
                }
                WindowEvent::CursorLeft { .. } => cursor = None,
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                } => dragging_serve_scale = false,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } if !connecting => {
                    let Some((x, y)) = cursor else {
                        continue;
                    };
                    if let Some((_, tab, _)) = ui::MENU_TABS
                        .iter()
                        .find(|(area, _, _)| state.menu_open && area.contains(x, y))
                    {
                        state.switch_tab(*tab);
                        helper_action = None;
                    } else if ui::TAB_MORE.contains(x, y) {
                        state.menu_open = !state.menu_open;
                    } else if let Some((_, tab, _)) =
                        ui::TABS.iter().find(|(area, _, _)| area.contains(x, y))
                    {
                        state.switch_tab(*tab);
                        helper_action = None;
                    } else {
                        state.menu_open = false;
                    }
                    if !ui::TAB_MORE.contains(x, y)
                        && !ui::TABS.iter().any(|(area, _, _)| area.contains(x, y))
                        && !ui::MENU_TABS.iter().any(|(area, _, _)| area.contains(x, y))
                        && let Some(helper) = state.tab.helper()
                    {
                        state.focus = None;
                        for (area, field) in ui::helper_fields(state.tab) {
                            if area.contains(x, y) {
                                state.focus = Some(*field);
                            }
                        }
                        if state.tab == Tab::Firewall {
                            for (area, profile) in ui::FW_PROFILES {
                                if area.contains(x, y) {
                                    config.helpers.profile = profile.into();
                                }
                            }
                        }
                        let report = helpers.report_mut(helper);
                        let pages = ui::report_lines(&report.text)
                            .len()
                            .div_ceil(ui::report_rows(state.tab))
                            .max(1);
                        if ui::REPORT_PREV.contains(x, y) {
                            report.page = report.page.saturating_sub(1);
                        }
                        if ui::REPORT_NEXT.contains(x, y) {
                            report.page = report.page.saturating_add(1).min(pages - 1);
                        }
                        if ui::REPORT_COPY.contains(x, y) {
                            helper_action = Some(helpers::Action::Copy(report.text.clone()));
                        }
                        match state.tab {
                            Tab::Work if ui::WORK_CHECK.contains(x, y) => {
                                helper_action = Some(helpers::Action::CheckWork)
                            }
                            Tab::Work if ui::WORK_START.contains(x, y) => {
                                helper_action = Some(helpers::Action::StartWork)
                            }
                            Tab::Network if ui::HELPER_RUN.contains(x, y) => {
                                helper_action = Some(helpers::Action::Network)
                            }
                            Tab::Firewall if ui::FW_PREVIEW.contains(x, y) => {
                                helper_action = Some(helpers::Action::Firewall)
                            }
                            Tab::Firewall if ui::FW_SET.contains(x, y) => {
                                helper_action = Some(helpers::Action::SetFirewall)
                            }
                            Tab::Firewall if ui::FW_RESET.contains(x, y) => {
                                helper_action = Some(helpers::Action::ResetFirewall)
                            }
                            _ => {}
                        }
                    } else if state.tab == Tab::Server
                        && !ui::TAB_MORE.contains(x, y)
                        && !ui::TABS.iter().any(|(area, _, _)| area.contains(x, y))
                    {
                        // The form describes the next start, so it is locked while serving.
                        let editable = server.view.phase == ServerPhase::Stopped;
                        state.focus = None;
                        for (area, field) in [
                            (ui::HOST, Field::ServeHost),
                            (ui::PORT, Field::ServePort),
                            (ui::SERVE_PASSWORD, Field::ServePassword),
                        ] {
                            if editable && area.contains(x, y) {
                                state.focus = Some(field);
                            }
                        }
                        let serve = &mut config.serve;
                        if editable && ui::SERVE_DISPLAY.contains(x, y) {
                            serve.display = ui::next_display(
                                serve.display,
                                server.view.displays.as_deref(),
                                HOST_SPANS_DISPLAYS,
                            );
                        }
                        if editable && ui::INSECURE.contains(x, y) {
                            serve.allow_insecure = !serve.allow_insecure;
                        }
                        if editable && ui::SERVE_UNENCRYPTED_TRANSPORT.contains(x, y) {
                            serve.tls.unencrypted = !serve.tls.unencrypted;
                        }
                        if editable && ui::LOCAL_ONLY.contains(x, y) {
                            serve.host = ui::LOCAL_ONLY_HOST.into();
                        }
                        if editable && ui::ULTRA_MODE.contains(x, y) {
                            serve.performance = toggle_performance(serve.performance);
                        }
                        if editable && ui::ALL_NETWORKS.contains(x, y) {
                            serve.host = ui::ALL_NETWORKS_HOST.into();
                        }
                        if editable && ui::SERVE_SCALE_SLIDER.contains(x, y) {
                            dragging_serve_scale = true;
                            serve.scale = ui::serve_scale_from_slider_x(x);
                        }
                        if editable && ui::SERVE_FULL_SIZE.contains(x, y) {
                            serve.scale = 1.0;
                        }
                        if editable && ui::SERVE_HALF_SIZE.contains(x, y) {
                            serve.scale = 0.5;
                        }
                        for (area, priority, _) in ui::SERVE_PRIORITY {
                            if editable && area.contains(x, y) {
                                serve.priority = priority;
                            }
                        }
                        for (area, mode, _) in ui::SERVE_MOUSE {
                            if editable && area.contains(x, y) {
                                serve.mouse = mode;
                            }
                        }
                        if ui::CONNECT.contains(x, y) {
                            server_clicked = true;
                        }
                    } else if state.tab == Tab::Web
                        && !ui::TAB_MORE.contains(x, y)
                        && !ui::TABS.iter().any(|(area, _, _)| area.contains(x, y))
                        && !ui::MENU_TABS.iter().any(|(area, _, _)| area.contains(x, y))
                    {
                        // The password describes the next start.
                        state.focus = (web.is_idle() && ui::WEB_PASSWORD.contains(x, y))
                            .then_some(Field::WebPassword);
                        for (area, row) in ui::WEB_ROWS {
                            if area.contains(x, y) {
                                state.web_row = Some(row);
                            }
                        }
                        // The route describes the next start, like the password.
                        for (area, route, _) in ui::WEB_ROUTES {
                            if web.is_idle() && area.contains(x, y) && state.web_route != route {
                                state.web_route = route;
                                state.error = None;
                                state.notice = None;
                            }
                        }
                        web_clicked |= ui::WEB_START.contains(x, y);
                        pair_clicked |= ui::WEB_PAIR.contains(x, y);
                        copy_url_clicked |=
                            ui::web_copy_url_shown(&state, web) && ui::WEB_COPY_URL.contains(x, y);
                    } else if state.tab == Tab::Connect
                        && !ui::TAB_MORE.contains(x, y)
                        && !ui::TABS.iter().any(|(area, _, _)| area.contains(x, y))
                        && !ui::MENU_TABS.iter().any(|(area, _, _)| area.contains(x, y))
                    {
                        state.focus = None;
                        for (area, field) in [
                            (ui::HOST, Field::Host),
                            (ui::PORT, Field::Port),
                            (ui::PASSWORD, Field::Password),
                            (ui::SIZE, Field::WindowSize),
                        ] {
                            if area.contains(x, y) {
                                state.focus = Some(field);
                            }
                        }
                        if ui::INSECURE.contains(x, y) {
                            config.allow_insecure = !config.allow_insecure;
                        }
                        if ui::UNENCRYPTED_TRANSPORT.contains(x, y) {
                            config.tls.unencrypted = !config.tls.unencrypted;
                        }
                        if ui::RAW.contains(x, y) {
                            config.compression = Compression::Raw;
                        }
                        if ui::ZLIB.contains(x, y) {
                            config.compression = Compression::Zlib;
                        }
                        for (area, level, _) in ui::JPEG_PRESETS {
                            if area.contains(x, y) {
                                config.compression = Compression::Tight;
                                config.jpeg_quality = level;
                            }
                        }
                        if ui::FIT.contains(x, y) {
                            config.window_mode = WindowMode::Fit;
                        }
                        if ui::NATIVE.contains(x, y) {
                            config.window_mode = WindowMode::Native;
                        }
                        if ui::CUSTOM.contains(x, y) {
                            config.window_mode = WindowMode::Custom;
                        }
                        if ui::FPS60.contains(x, y) {
                            config.fps = 60;
                            config.performance = PerformanceMode::Standard;
                        }
                        if ui::FPS120.contains(x, y) {
                            config.fps = 120;
                            config.performance = PerformanceMode::Standard;
                        }
                        if ui::FPS_NO_LIMIT.contains(x, y) {
                            config.fps = 0;
                        }
                        if ui::ULTRA_MODE.contains(x, y) {
                            config.performance = toggle_performance(config.performance);
                        }
                        if ui::SMOOTH.contains(x, y) {
                            config.quality = Quality::Smooth;
                        }
                        if ui::SHARP.contains(x, y) {
                            config.quality = Quality::Sharp;
                        }
                        if ui::CONNECT.contains(x, y) {
                            connect_clicked = true;
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(action) = helper_action
            && let Some(helper) = state.tab.helper()
            && !helpers.busy()
            && let Err(error) = helpers.start(helper, action, &config.helpers)
        {
            *helpers.report_mut(helper) = helpers::Report {
                text: error,
                failed: true,
                ..helpers::Report::default()
            };
        }
        if server_clicked {
            match server.view.phase {
                ServerPhase::Stopped => match config.serve.request() {
                    Ok(request) => {
                        state.error = None;
                        state.focus = None;
                        if let Err(error) = server.start(Owner::ServerTab, request) {
                            state.error = Some(error);
                        }
                    }
                    Err(error) => state.error = Some(error),
                },
                // Browser access keeps its own listener on the server.
                ServerPhase::Serving { .. } => server.stop(Owner::ServerTab),
                ServerPhase::Starting | ServerPhase::Stopping => {}
            }
        }
        if copy_url_clicked && let Some(origin) = web.app_origin() {
            match copy_text(origin) {
                Ok(()) => {
                    state.error = None;
                    state.notice = Some("APP URL COPIED TO THE CLIPBOARD.".into());
                }
                Err(error) => state.error = Some(format!("Could not copy the app URL: {error}")),
            }
        }
        if web_clicked || pair_clicked {
            state.notice = None;
            let start = web.is_idle() && (web_clicked || (pair_clicked && !web.pairing_active()));
            let started = if start {
                state.web_row = None;
                start_web(web, state.web_route, &config, server)
            } else {
                Ok(())
            };
            match started {
                Err(error) => state.error = Some(error),
                Ok(()) => {
                    if start {
                        state.error = None;
                        state.focus = None;
                    } else if web_clicked && !web.is_stopping() {
                        web.stop();
                    }
                    if pair_clicked {
                        if web.pairing_active() {
                            web.cancel_pair();
                        } else if let Err(error) = web.pair() {
                            state.error = Some(error);
                        }
                    }
                }
            }
        }
        if connect_clicked && !connecting {
            match config.address() {
                Ok(_) => {
                    state.error = None;
                    connecting = true;
                    let sender = result_tx.clone();
                    let connection_config = config.clone();
                    thread::spawn(move || {
                        let result =
                            connect_native(&connection_config).map_err(|error| error.to_string());
                        let _ = sender.send(result);
                    });
                }
                Err(error) => state.error = Some(error),
            }
        }
    }
}

/// Advance browser access from either window, including failure shutdown.
fn poll_browser_access(
    server: &mut HostedServer,
    web: &mut WebRuntime,
    config: &Config,
) -> Result<(), String> {
    match web.poll(&server.web_view(), Instant::now()) {
        Some(ServerRequest::Start(address)) => {
            let started = web_serve_request(&config.serve, address)
                .and_then(|request| server.start(Owner::Web, request));
            if started.is_err() {
                web.stop();
            }
            started
        }
        Some(ServerRequest::Stop) => {
            server.stop(Owner::Web);
            Ok(())
        }
        None => Ok(()),
    }
}

/// Check the chosen route's files and start the WEB tab's run.
fn start_web(
    web: &mut WebRuntime,
    route: web_access::config::RouteKind,
    config: &Config,
    server: &HostedServer,
) -> Result<(), String> {
    let launch = web_access::config::prepare(route).map_err(|problem| problem.to_string())?;
    web.start(
        launch,
        &config.serve.password,
        &server.web_view(),
        Instant::now(),
    )
}

/// The WEB tab's desktop server: the Server tab's display, size, mouse,
/// priority, and performance choices, but always plain RFB on the gateway's
/// loopback target with a password, never All networks or no password.
fn web_serve_request(
    form: &ui::ServeForm,
    address: std::net::SocketAddr,
) -> Result<ServeRequest, String> {
    if !address.ip().is_loopback() {
        return Err("Browser access serves on loopback only.".into());
    }
    let mut form = form.clone();
    form.host = match address.ip() {
        std::net::IpAddr::V6(ip) => format!("[{ip}]"),
        ip => ip.to_string(),
    };
    form.port = address.port().to_string();
    form.allow_insecure = false;
    form.tls = topvnc::transport::TlsOptions {
        unencrypted: true,
        ..Default::default()
    };
    form.request()
}

/// How often the title bar's statistics refresh.
const STATS_INTERVAL: Duration = Duration::from_secs(1);

fn toggle_performance(mode: PerformanceMode) -> PerformanceMode {
    match mode {
        PerformanceMode::Standard => PerformanceMode::Ultra,
        PerformanceMode::Ultra => PerformanceMode::Standard,
    }
}

/// Accumulate dirty pixels until an upload is due or the update completes.
#[derive(Default)]
struct UpdateUploads {
    changed: Option<Rect>,
    last_upload: Option<Instant>,
}

impl UpdateUploads {
    fn push(&mut self, rect: Rect, policy: u8, now: Instant) -> Option<Rect> {
        self.changed = Some(self.changed.map_or(rect, |area| area.union(rect)));
        if policy == 1
            && self
                .last_upload
                .is_none_or(|at| now.saturating_duration_since(at) >= Duration::from_millis(8))
        {
            self.last_upload = Some(now);
            self.finish()
        } else {
            None
        }
    }

    fn finish(&mut self) -> Option<Rect> {
        self.changed.take()
    }
}

fn publish_frame(
    frames: &FrameMailbox,
    framebuffer: &Framebuffer,
    area: Rect,
    signal: &session_render::RenderSignal,
) -> std::io::Result<()> {
    let damage = topvnc::DamageRect {
        x: area.x0 as u16,
        y: area.y0 as u16,
        width: area.width() as u16,
        height: area.height() as u16,
    };
    if frames.publish(framebuffer, damage, Instant::now())? {
        signal.wake();
    }
    Ok(())
}

/// How long to wait before drawing again when the window could not take a
/// frame, as while it is minimized.
const RETRY_INTERVAL: Duration = Duration::from_millis(16);

/// The settings overlay, drawn in software at the window's logical size.
struct Overlay {
    pixels: Vec<u32>,
    size: (usize, usize),
    layer: Layer,
}

/// The window's size in logical pixels, which the settings overlay, Fit and
/// Native geometry, and pointer mapping use.
fn logical_size(window: &Window) -> (usize, usize) {
    let size = window.inner_size();
    let scale = window.scale_factor();
    (
        (f64::from(size.width) / scale).round().max(1.0) as usize,
        (f64::from(size.height) / scale).round().max(1.0) as usize,
    )
}

/// `area` clipped to a `size` canvas.
fn clip(area: Box2, size: (usize, usize)) -> Option<Box2> {
    let x1 = (area.x + area.w).min(size.0);
    let y1 = (area.y + area.h).min(size.1);
    (area.x < x1 && area.y < y1).then(|| Box2 {
        x: area.x,
        y: area.y,
        w: x1 - area.x,
        h: y1 - area.y,
    })
}

/// Send `buttons` without motion: where the pointer is in absolute mode, or
/// as a zero delta while it is locked.
fn send_buttons(
    writer: &InputWriter,
    buttons: u16,
    locked: bool,
    at: Option<(u16, u16)>,
) -> std::io::Result<()> {
    match (locked, at) {
        (true, _) => writer.pointer_motion(buttons, 0, 0),
        (false, Some((x, y))) => writer.pointer(buttons, x, y),
        (false, None) => Ok(()),
    }
}

fn run_session(
    ui: &mut Ui,
    session: Session,
    config: &mut Config,
    input_debug: bool,
    server: &mut HostedServer,
    web: &mut WebRuntime,
) -> Result<Option<String>, Box<dyn Error>> {
    let writer = session.writer();
    let result = run_session_inner(ui, session, config, input_debug, server, web);
    let _ = writer.shutdown();
    result
}

fn run_session_inner(
    ui: &mut Ui,
    mut session: Session,
    config: &mut Config,
    input_debug: bool,
    server: &mut HostedServer,
    web: &mut WebRuntime,
) -> Result<Option<String>, Box<dyn Error>> {
    let info = session.info.clone();
    eprintln!(
        "Connected using {:?} security. RFB traffic is not encrypted.",
        info.security
    );
    let writer = session.writer();
    if !config.relative_mouse {
        writer.set_relative_pointer_allowed(false)?;
    }
    let stats = session.stats();
    let mut remote = (usize::from(info.width), usize::from(info.height));
    let initial = match config.window_mode {
        WindowMode::Fit => fit_dimensions(remote, display_space()),
        WindowMode::Native => remote,
        WindowMode::Custom => config.custom_size()?,
    };
    let mut name = info.name.chars().filter(|c| !c.is_control());
    let mut short_name: String = name.by_ref().take(24).collect();
    if name.next().is_some() {
        short_name.push('…');
    }
    let title = if short_name.trim().is_empty() {
        "TopVNC".to_owned()
    } else {
        format!("TopVNC — {short_name}")
    };
    let window = ui.create_window(
        WindowAttributes::default()
            .with_title(&title)
            .with_inner_size(LogicalSize::new(initial.0 as f64, initial.1 as f64))
            .with_resizable(true),
    )?;
    let presenter = Presenter::new(ui, Arc::clone(&window))?;
    let frames = Arc::new(FrameMailbox::new(info.width, info.height)?);
    let upload_policy = Arc::new(AtomicU8::new(config.upload_policy()));
    let (error_tx, error_rx) = mpsc::channel();
    let renderer = RenderWorker::start(
        presenter,
        Arc::clone(&frames),
        RenderOptions::new(config, &window),
        remote,
        error_tx.clone(),
        ui.waker(),
    )?;
    {
        let frames = Arc::clone(&frames);
        let render_signal = renderer.signal();
        let upload_policy = Arc::clone(&upload_policy);
        let writer = writer.clone();
        let waker = ui.waker();
        let (width, height) = (info.width, info.height);
        thread::Builder::new()
            .name("topvnc-network".into())
            .spawn(move || {
                let result: std::io::Result<()> = (|| {
                    // This thread owns decoding, never GPU resources. Whole and
                    // Ultra publish coherent snapshots after the update completes.
                    let mut framebuffer = Framebuffer::new(width, height)?;
                    let mut scratch = Vec::new();
                    writer.request_update(false, width, height)?;
                    loop {
                        let mut uploads = UpdateUploads::default();
                        let controls_before = (
                            writer.relative_pointer(),
                            writer.serve_scale(),
                            writer.serve_display(),
                        );
                        session.read_update_pipelined(&mut scratch, |x, y, w, h, bytes| {
                            if bytes.is_empty() {
                                framebuffer = Framebuffer::new(w, h)?;
                                uploads = UpdateUploads::default();
                                uploads.changed = Some(Rect {
                                    x0: 0,
                                    y0: 0,
                                    x1: usize::from(w),
                                    y1: usize::from(h),
                                });
                                return Ok(());
                            }
                            framebuffer.apply_raw(x, y, w, h, bytes)?;
                            let rect = Rect {
                                x0: usize::from(x),
                                y0: usize::from(y),
                                x1: usize::from(x) + usize::from(w),
                                y1: usize::from(y) + usize::from(h),
                            };
                            if let Some(area) = uploads.push(
                                rect,
                                upload_policy.load(Ordering::Relaxed),
                                Instant::now(),
                            ) {
                                publish_frame(&frames, &framebuffer, area, &render_signal)?;
                            }
                            Ok(())
                        })?;
                        if let Some(area) = uploads.finish() {
                            publish_frame(&frames, &framebuffer, area, &render_signal)?;
                        }
                        if controls_before
                            != (
                                writer.relative_pointer(),
                                writer.serve_scale(),
                                writer.serve_display(),
                            )
                        {
                            waker.wake();
                        }
                    }
                })();
                let _ = error_tx.send(result);
                waker.wake();
            })?;
    }

    let mut overlay_changed = true;
    let mut pressed_keys = HashMap::new();
    let mut buttons = 0u16;
    // The pointer in logical window pixels, and the remote pixel last sent.
    let mut cursor: Option<(f64, f64)> = None;
    let mut last_pointer: Option<(u16, u16)> = None;
    let mut wheel = Wheel::default();
    // Relative motion not yet sent: whole units to send, and the fraction
    // carried to the next event.
    let mut motion = (0.0f64, 0.0f64);
    let mut settings_open = false;
    let mut dragging_ui_scale = false;
    let mut dragging_serve_scale = None;
    // A left press that began on the overlay; its release stays local.
    let mut ui_captured_mouse = false;
    let mut focused = window.has_focus();
    let mut locked = false;
    // A minimized or fully covered window draws nothing until it is shown.
    let mut occluded = false;
    let mut redraw = true;
    let mut last_stats = (Instant::now(), stats.snapshot());
    let mut last_rendered = 0u64;
    let mut compact_status = title.clone();
    let mut debug_status = title.clone();
    let mut known_serve_scale = None;
    let mut known_serve_display = None;
    loop {
        // Hosting and browser access survive viewer sessions. Keep failure
        // handling active here, including stopping public forwarding when
        // the gateway exits, rather than waiting for the landing window.
        if let Some(error) = server.poll() {
            eprintln!("Hosted server: {error}");
        }
        if let Err(error) = poll_browser_access(server, web, config) {
            eprintln!("Browser access: {error}");
        }
        let mut title_changed = false;
        remote = frames.dimensions();
        let serve_scale = writer.serve_scale();
        let serve_display = writer.serve_display();
        if (known_serve_scale, known_serve_display) != (serve_scale, serve_display) {
            (known_serve_scale, known_serve_display) = (serve_scale, serve_display);
            overlay_changed = true;
            redraw = true;
        }
        // Presentation waits never run on the input event thread.
        let timeout = (last_stats.0 + STATS_INTERVAL).saturating_duration_since(Instant::now());
        let timeout = if web.is_idle() {
            timeout
        } else {
            timeout.min(Duration::from_millis(50))
        };
        for event in ui.pump(timeout) {
            match event {
                UiEvent::Device(DeviceEvent::MouseMotion { delta }) if locked => {
                    motion.0 += delta.0;
                    motion.1 += delta.1;
                    continue;
                }
                UiEvent::Window(id, _) if id != window.id() => continue,
                UiEvent::Window(_, event) => {
                    // Motion goes out before the event that follows it, so a
                    // flick and a click arrive in the order they happened.
                    if motion.0.abs() >= 1.0 || motion.1.abs() >= 1.0 {
                        let (x, y) = (motion.0.trunc(), motion.1.trunc());
                        writer.pointer_motion(buttons, x as i32, y as i32)?;
                        motion = (motion.0 - x, motion.1 - y);
                    }
                    match event {
                        WindowEvent::CloseRequested => return Ok(None),
                        WindowEvent::Resized(_)
                        | WindowEvent::ScaleFactorChanged { .. }
                        | WindowEvent::RedrawRequested => {
                            redraw = true;
                            overlay_changed = true;
                        }
                        WindowEvent::Occluded(now_occluded) => {
                            occluded = now_occluded;
                            redraw = true;
                            overlay_changed = true;
                        }
                        WindowEvent::Focused(now_focused) => {
                            focused = now_focused;
                            if !focused {
                                // Keys and buttons held when focus leaves would
                                // otherwise stay down on the remote desktop.
                                for (_, symbol) in pressed_keys.drain() {
                                    writer.key(symbol, false)?;
                                }
                                buttons = 0;
                                send_buttons(&writer, 0, locked, last_pointer)?;
                                ui_captured_mouse = false;
                                dragging_ui_scale = false;
                                dragging_serve_scale = None;
                            }
                        }
                        WindowEvent::KeyboardInput {
                            event,
                            is_synthetic: false,
                            ..
                        } => {
                            let PhysicalKey::Code(code) = event.physical_key else {
                                continue;
                            };
                            let down = event.state == ElementState::Pressed;
                            if code == KeyCode::F8 {
                                if down && !event.repeat {
                                    settings_open = !settings_open;
                                    overlay_changed = true;
                                    redraw = true;
                                    dragging_ui_scale = false;
                                    dragging_serve_scale = None;
                                    for (_, symbol) in pressed_keys.drain() {
                                        writer.key(symbol, false)?;
                                    }
                                    buttons = 0;
                                    send_buttons(&writer, 0, locked, last_pointer)?;
                                }
                                continue;
                            }
                            if settings_open || event.repeat {
                                continue;
                            }
                            if let Some((symbol, down)) =
                                translated_key_event(&mut pressed_keys, code, down)
                            {
                                if input_debug {
                                    eprintln!(
                                        "key {}: {code:?} -> {symbol:#x}",
                                        if down { "down" } else { "up" }
                                    );
                                }
                                writer.key(symbol, down)?;
                            }
                        }
                        WindowEvent::CursorMoved { position, .. } => {
                            let scale = window.scale_factor();
                            let point = (position.x / scale, position.y / scale);
                            cursor = Some(point);
                            if dragging_serve_scale.is_some() {
                                dragging_serve_scale =
                                    Some(ui::live_serve_percent_from_x(point.0.max(0.0) as usize));
                                overlay_changed = true;
                                redraw = true;
                            }
                            if dragging_ui_scale {
                                config.ui_scale =
                                    ui::scale_from_slider_x(point.0.max(0.0) as usize);
                                overlay_changed = true;
                                redraw = true;
                            }
                            let over_button = ui::open_settings_box(config.ui_scale)
                                .contains(point.0.max(0.0) as usize, point.1.max(0.0) as usize);
                            if locked || settings_open || ui_captured_mouse {
                                continue;
                            }
                            if over_button {
                                // The pointer left the remote image for the
                                // settings button: release what it held there.
                                if buttons != 0 {
                                    buttons = 0;
                                    send_buttons(&writer, 0, false, last_pointer)?;
                                }
                                continue;
                            }
                            if let Some(at) = pointer_for_mode(
                                point.0,
                                point.1,
                                logical_size(&window),
                                remote,
                                config.window_mode,
                            ) && last_pointer != Some(at)
                            {
                                if input_debug {
                                    eprintln!(
                                        "pointer: buttons={buttons} remote=({}, {})",
                                        at.0, at.1
                                    );
                                }
                                writer.pointer(buttons, at.0, at.1)?;
                                last_pointer = Some(at);
                            }
                        }
                        WindowEvent::CursorLeft { .. } => cursor = None,
                        WindowEvent::MouseInput { state, button, .. } => {
                            let pressed = state == ElementState::Pressed;
                            let left = button == MouseButton::Left;
                            if left && !pressed && ui_captured_mouse {
                                if let Some(percent) = dragging_serve_scale.take() {
                                    if Some(percent) != writer.serve_scale() {
                                        writer.request_serve_scale(percent)?;
                                    }
                                    overlay_changed = true;
                                    redraw = true;
                                }
                                ui_captured_mouse = false;
                                dragging_ui_scale = false;
                                continue;
                            }
                            // Clicks on the settings panel or its button stay
                            // local; a locked pointer has no position to test.
                            if left && pressed && !locked {
                                let at =
                                    cursor.map(|(x, y)| (x.max(0.0) as usize, y.max(0.0) as usize));
                                if settings_open {
                                    ui_captured_mouse = true;
                                    overlay_changed = true;
                                    redraw = true;
                                    let Some((x, y)) = at else {
                                        continue;
                                    };
                                    if ui::CLOSE_SETTINGS.contains(x, y) {
                                        settings_open = false;
                                    } else if ui::LIVE_FIT.contains(x, y) {
                                        config.window_mode = WindowMode::Fit;
                                    } else if ui::LIVE_NATIVE.contains(x, y) {
                                        config.window_mode = WindowMode::Native;
                                    } else if ui::LIVE_FULLSCREEN.contains(x, y) {
                                        window.set_fullscreen(match window.fullscreen() {
                                            Some(_) => None,
                                            None => Some(Fullscreen::Borderless(None)),
                                        });
                                    } else if let Some(displays) = serve_display
                                        && let Some(display) =
                                            ui::live_display_choice(displays, x, y)
                                    {
                                        // The host answers by announcing what it serves.
                                        if display != displays.served {
                                            writer.request_serve_display(display)?;
                                        }
                                    } else if ui::LIVE_SERVE_SLIDER.contains(x, y)
                                        && serve_scale.is_some()
                                    {
                                        dragging_serve_scale =
                                            Some(ui::live_serve_percent_from_x(x));
                                    } else if ui::LIVE_60.contains(x, y) {
                                        config.fps = 60;
                                        config.performance = PerformanceMode::Standard;
                                        upload_policy
                                            .store(config.upload_policy(), Ordering::Relaxed);
                                    } else if ui::LIVE_120.contains(x, y) {
                                        config.fps = 120;
                                        config.performance = PerformanceMode::Standard;
                                        upload_policy
                                            .store(config.upload_policy(), Ordering::Relaxed);
                                    } else if ui::LIVE_NO_LIMIT.contains(x, y) {
                                        config.fps = 0;
                                    } else if ui::LIVE_SMOOTH.contains(x, y) {
                                        config.quality = Quality::Smooth;
                                    } else if ui::LIVE_SHARP.contains(x, y) {
                                        config.quality = Quality::Sharp;
                                    } else if ui::LIVE_MOUSE_AUTO.contains(x, y) {
                                        config.relative_mouse = true;
                                        writer.set_relative_pointer_allowed(true)?;
                                    } else if ui::LIVE_MOUSE_OFF.contains(x, y) {
                                        config.relative_mouse = false;
                                        writer.set_relative_pointer_allowed(false)?;
                                    } else if ui::LIVE_WHOLE_UPDATE.contains(x, y) {
                                        config.progressive = false;
                                        config.performance = PerformanceMode::Standard;
                                        upload_policy
                                            .store(config.upload_policy(), Ordering::Relaxed);
                                    } else if ui::LIVE_PROGRESSIVE.contains(x, y) {
                                        config.progressive = true;
                                        config.performance = PerformanceMode::Standard;
                                        upload_policy
                                            .store(config.upload_policy(), Ordering::Relaxed);
                                    } else if ui::LIVE_ULTRA.contains(x, y) {
                                        config.performance = toggle_performance(config.performance);
                                        upload_policy
                                            .store(config.upload_policy(), Ordering::Relaxed);
                                    } else if ui::LIVE_MOTION_SMOOTH.contains(x, y) {
                                        config.motion = MotionPresentation::Smooth;
                                    } else if ui::LIVE_MOTION_IMMEDIATE.contains(x, y) {
                                        config.motion = MotionPresentation::Immediate;
                                    } else if ui::SCALE_SLIDER.contains(x, y) {
                                        dragging_ui_scale = true;
                                        config.ui_scale = ui::scale_from_slider_x(x);
                                    } else if ui::LIVE_DEBUG.contains(x, y) {
                                        config.debug = !config.debug;
                                        title_changed = true;
                                    } else if ui::DISCONNECT.contains(x, y) {
                                        return Ok(None);
                                    }
                                    continue;
                                }
                                if let Some((x, y)) = at
                                    && ui::open_settings_box(config.ui_scale).contains(x, y)
                                {
                                    settings_open = true;
                                    ui_captured_mouse = true;
                                    overlay_changed = true;
                                    redraw = true;
                                    for (_, symbol) in pressed_keys.drain() {
                                        writer.key(symbol, false)?;
                                    }
                                    buttons = 0;
                                    send_buttons(&writer, 0, false, last_pointer)?;
                                    continue;
                                }
                            }
                            if settings_open {
                                continue;
                            }
                            let Some(bit) = button_bit(button) else {
                                continue;
                            };
                            buttons = if pressed {
                                buttons | bit
                            } else {
                                buttons & !bit
                            };
                            // A click lands where the pointer is, even before
                            // it has moved over the window.
                            if !locked && let Some((x, y)) = cursor {
                                last_pointer = pointer_for_mode(
                                    x,
                                    y,
                                    logical_size(&window),
                                    remote,
                                    config.window_mode,
                                )
                                .or(last_pointer);
                            }
                            send_buttons(&writer, buttons, locked, last_pointer)?;
                        }
                        WindowEvent::MouseWheel { delta, .. } => {
                            if settings_open {
                                continue;
                            }
                            let notches = wheel.notches(delta, window.scale_factor());
                            if locked {
                                send_wheel(&writer, buttons, notches, None)?;
                                continue;
                            }
                            // Scrolling lands where the pointer is, even
                            // before it has moved over the window.
                            if let Some((x, y)) = cursor {
                                last_pointer = pointer_for_mode(
                                    x,
                                    y,
                                    logical_size(&window),
                                    remote,
                                    config.window_mode,
                                )
                                .or(last_pointer);
                            }
                            if let Some(at) = last_pointer {
                                send_wheel(&writer, buttons, notches, Some(at))?;
                            }
                        }
                        _ => {}
                    }
                }
                UiEvent::Device(_) | UiEvent::Wake => {}
            }
        }
        if motion.0.abs() >= 1.0 || motion.1.abs() >= 1.0 {
            let (x, y) = (motion.0.trunc(), motion.1.trunc());
            writer.pointer_motion(buttons, x as i32, y as i32)?;
            motion = (motion.0 - x, motion.1 - y);
        }
        if let Ok(result) = error_rx.try_recv() {
            return Ok(result.err().map(|error| error.to_string()));
        }
        // Games that turn the camera with the mouse hide the host's cursor;
        // the host then asks for relative motion, and the pointer locks.
        let lock = config.relative_mouse && focused && !settings_open && writer.relative_pointer();
        if lock != locked {
            locked = lock;
            lock_pointer(&window, locked);
            title_changed = true;
            motion = (0.0, 0.0);
            overlay_changed = true;
            redraw = true;
        }
        // Keep the title compact; cache full diagnostics for the debug toggle.
        if last_stats.0.elapsed() >= STATS_INTERVAL {
            let now = (Instant::now(), stats.snapshot());
            let mut status = format!(
                "{title} — decode {}",
                throughput(last_stats.1, now.1, now.0 - last_stats.0)
            );
            let submitted = renderer.snapshot()?;
            let render_fps = submitted.frames.saturating_sub(last_rendered) as f64
                / (now.0 - last_stats.0).as_secs_f64();
            compact_status = format!("{title} · {render_fps:.0} fps");
            status += &format!(" · render {render_fps:.0} fps");
            last_rendered = submitted.frames;
            if let Some((_, p95)) = submitted.pacing {
                status += &format!(
                    " / p95 {:.1} ms · {:.2} ms ready to submit",
                    p95.as_secs_f64() * 1e3,
                    submitted.age_mean.as_secs_f64() * 1e3
                );
            }
            if let Some(mode) = submitted.mode {
                status += &format!(" · {mode:?}");
            }
            status += &format!(" · {} superseded", frames.stats()?.replaced);
            if config.performance == PerformanceMode::Ultra {
                status += " · Ultra";
            }
            debug_status = status;
            title_changed = true;
            last_stats = now;
        }
        if title_changed {
            let status = if config.debug {
                &debug_status
            } else {
                &compact_status
            };
            if locked {
                window.set_title(&format!("{status} · locked (F8)"));
            } else {
                window.set_title(status);
            }
        }
        let mut options = RenderOptions::new(config, &window);
        options.open = settings_open;
        options.locked = locked;
        options.occluded = occluded;
        options.serve_scale = dragging_serve_scale.or(serve_scale);
        options.serve_display = serve_display;
        renderer.update(options, overlay_changed || redraw)?;
        overlay_changed = false;
        redraw = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_serve_request_is_plain_loopback_with_a_password() {
        let mut form = ui::ServeForm {
            host: ui::ALL_NETWORKS_HOST.into(),
            port: "1".into(),
            allow_insecure: true,
            scale: 0.5,
            ..ui::ServeForm::default()
        };
        form.tls.cert = Some("cert.pem".into());
        form.tls.key = Some("key.pem".into());
        // An empty password is refused even though the Server tab allows none.
        assert!(web_serve_request(&form, "127.0.0.1:5900".parse().unwrap()).is_err());
        form.password = "pw".into();
        let request = web_serve_request(&form, "127.0.0.1:5900".parse().unwrap()).unwrap();
        assert_eq!(request.address, "127.0.0.1:5900");
        assert_eq!(request.password.as_deref(), Some("pw"));
        assert!(!request.allow_insecure);
        assert!(request.tls.unencrypted);
        assert_eq!((request.tls.cert, request.tls.key), (None, None));
        // The Server tab's other choices carry over.
        assert_eq!(request.scale, 0.5);
        let v6 = web_serve_request(&form, "[::1]:5900".parse().unwrap()).unwrap();
        assert_eq!(v6.address, "[::1]:5900");
        assert!(web_serve_request(&form, "192.168.1.2:5900".parse().unwrap()).is_err());
    }

    /// Requires a real interactive Windows desktop and GPU. Opens a small
    /// two-second window, then tests worker shutdown with no UI pumping.
    #[cfg(windows)]
    #[test]
    #[ignore = "requires an interactive GPU desktop"]
    fn render_worker_smoke_on_interactive_desktop() {
        let mut ui = Ui::new().unwrap();
        let window = ui
            .create_window(
                WindowAttributes::default()
                    .with_title("TopVNC smooth-frame test (closes automatically)")
                    .with_inner_size(LogicalSize::new(640.0, 360.0)),
            )
            .unwrap();
        let presenter = Presenter::new(&mut ui, Arc::clone(&window)).unwrap();
        let frames = Arc::new(FrameMailbox::new(1920, 1080).unwrap());
        let config = Config {
            performance: PerformanceMode::Ultra,
            ..Config::default()
        };
        let (errors, receiver) = mpsc::channel();
        let renderer = RenderWorker::start(
            presenter,
            Arc::clone(&frames),
            RenderOptions::new(&config, &window),
            (1920, 1080),
            errors,
            ui.waker(),
        )
        .unwrap();
        let signal = renderer.signal();
        let mut framebuffer = Framebuffer::new(1920, 1080).unwrap();
        let started = Instant::now();
        let mut next = started;
        let mut frame = 0u32;
        while started.elapsed() < Duration::from_secs(2) {
            ui.pump(Duration::from_millis(2));
            if Instant::now() >= next {
                for (index, pixel) in framebuffer.pixels_mut().iter_mut().enumerate() {
                    let level = ((index % 1920 + frame as usize) % 128) as u32 + 16;
                    *pixel = level << 16 | level << 8 | level;
                }
                publish_frame(
                    &frames,
                    &framebuffer,
                    Rect {
                        x0: 0,
                        y0: 0,
                        x1: 1920,
                        y1: 1080,
                    },
                    &signal,
                )
                .unwrap();
                frame += 1;
                next += Duration::from_secs_f64(1.0 / 240.0);
            }
            if let Ok(error) = receiver.try_recv() {
                panic!("render worker failed: {error:?}");
            }
        }
        let snapshot = renderer.snapshot().unwrap();
        println!(
            "Windows 1080p GPU worker smoke: {snapshot:?}; mailbox {:?}; monitor refresh {:?}",
            frames.stats().unwrap(),
            window
                .current_monitor()
                .and_then(|monitor| monitor.refresh_rate_millihertz())
        );
        assert!(snapshot.pacing.is_some(), "no frames were submitted");
        assert!(matches!(
            snapshot.mode,
            Some(wgpu::PresentMode::Mailbox | wgpu::PresentMode::Fifo)
        ));
        drop(renderer);
        drop(window);
    }

    #[test]
    fn whole_updates_wait_and_progressive_uploads_batch_dirty_pixels() {
        let now = Instant::now();
        let first = Rect {
            x0: 64,
            y0: 64,
            x1: 128,
            y1: 128,
        };
        let second = Rect {
            x0: 0,
            y0: 0,
            x1: 64,
            y1: 64,
        };
        let mut whole = UpdateUploads::default();
        assert!(whole.push(first, 0, now).is_none());
        assert!(
            whole
                .push(second, 0, now + Duration::from_secs(1))
                .is_none()
        );
        let area = whole.finish().unwrap();
        assert_eq!((area.x0, area.y0, area.x1, area.y1), (0, 0, 128, 128));
        assert!(whole.finish().is_none());

        let mut progressive = UpdateUploads::default();
        let area = progressive.push(first, 1, now).unwrap();
        assert_eq!((area.x0, area.y0), (64, 64));
        assert!(
            progressive
                .push(second, 1, now + Duration::from_millis(1))
                .is_none()
        );
        let area = progressive
            .push(first, 1, now + Duration::from_millis(8))
            .unwrap();
        assert_eq!((area.x0, area.y0, area.x1, area.y1), (0, 0, 128, 128));
        assert!(
            progressive
                .push(second, 1, now + Duration::from_millis(9))
                .is_none()
        );
        assert!(progressive.finish().is_some());
        assert!(progressive.finish().is_none());
    }

    #[test]
    fn ultra_keeps_regions_private_until_the_update_completes() {
        let now = Instant::now();
        let rect = Rect {
            x0: 4,
            y0: 8,
            x1: 12,
            y1: 16,
        };
        let mut uploads = UpdateUploads::default();
        for micros in [0, 1, 100, 499] {
            assert!(
                uploads
                    .push(rect, 2, now + Duration::from_micros(micros))
                    .is_none()
            );
        }
        let area = uploads.finish().unwrap();
        assert_eq!((area.x0, area.y0, area.x1, area.y1), (4, 8, 12, 16));
        assert!(uploads.finish().is_none());
    }

    #[test]
    fn fit_shrinks_large_remote_without_changing_aspect() {
        assert_eq!(fit_dimensions((3840, 2160), (1600, 900)), (1600, 900));
        assert_eq!(fit_dimensions((800, 600), (1600, 900)), (800, 600));
    }

    #[test]
    fn pointer_maps_center_and_letterbox_edges() {
        let remote = (2000, 1000);
        let window = (1000, 750);
        assert_eq!(
            remote_pointer(500.0, 375.0, window, remote),
            Some((1000, 500))
        );
        assert_eq!(remote_pointer(0.0, 0.0, window, remote), Some((0, 0)));
        assert_eq!(
            remote_pointer(999.0, 749.0, window, remote),
            Some((1999, 999))
        );
        assert_eq!(
            remote_pointer(250.0, 500.0, (1000, 1000), (1000, 2000)),
            Some((0, 1000))
        );
        assert_eq!(
            remote_pointer(749.0, 999.0, (1000, 1000), (1000, 2000)),
            Some((999, 1999))
        );
    }

    #[test]
    fn quality_parser_accepts_rfb_levels_only() {
        assert_eq!(parse_quality("0").unwrap(), 0);
        assert_eq!(parse_quality("9").unwrap(), 9);
        assert!(parse_quality("10").is_err());
        assert!(parse_quality("-1").is_err());
        assert!(parse_quality("high").is_err());
    }

    #[test]
    fn window_size_parser_rejects_invalid_dimensions() {
        assert_eq!(parse_size("1280x720").unwrap(), (1280, 720));
        assert!(parse_size("0x720").is_err());
        assert!(parse_size("1280:720").is_err());
    }

    #[test]
    fn shifted_letters_and_symbols_use_matching_keysyms() {
        assert_eq!(keysym(KeyCode::KeyT, false), Some('t' as u32));
        assert_eq!(keysym(KeyCode::KeyT, true), Some('T' as u32));
        assert_eq!(keysym(KeyCode::Digit1, false), Some('1' as u32));
        assert_eq!(keysym(KeyCode::Digit1, true), Some('!' as u32));
        assert_eq!(keysym(KeyCode::Slash, true), Some('?' as u32));
        assert_eq!(keysym(KeyCode::Enter, true), Some(0xff0d));
        // Keypad keys stay distinct from the main row, as games bind them.
        assert_eq!(keysym(KeyCode::Numpad1, false), Some(0xffb1));
        assert_eq!(keysym(KeyCode::Numpad1, true), Some(0xffb1));
        assert_eq!(keysym(KeyCode::NumpadEnter, false), Some(0xff8d));
        assert_eq!(keysym(KeyCode::F1, false), Some(0xffbe));
        assert_eq!(keysym(KeyCode::F24, false), Some(0xffd5));
        assert_eq!(keysym(KeyCode::CapsLock, false), Some(0xffe5));
        assert_eq!(keysym(KeyCode::Fn, false), None);
    }

    #[test]
    fn key_events_preserve_press_release_and_shift_state() {
        let mut held = HashMap::new();
        assert_eq!(
            translated_key_event(&mut held, KeyCode::ShiftLeft, true),
            Some((0xffe1, true))
        );
        assert_eq!(
            translated_key_event(&mut held, KeyCode::KeyT, true),
            Some(('T' as u32, true))
        );
        assert_eq!(translated_key_event(&mut held, KeyCode::KeyT, true), None);
        assert_eq!(
            translated_key_event(&mut held, KeyCode::ShiftLeft, false),
            Some((0xffe1, false))
        );
        assert_eq!(
            translated_key_event(&mut held, KeyCode::KeyT, false),
            Some(('T' as u32, false))
        );
        assert_eq!(
            translated_key_event(&mut held, KeyCode::Enter, true),
            Some((0xff0d, true))
        );
        assert_eq!(
            translated_key_event(&mut held, KeyCode::Enter, false),
            Some((0xff0d, false))
        );
    }

    #[test]
    fn mouse_buttons_include_back_and_forward() {
        assert_eq!(button_bit(MouseButton::Left), Some(BUTTON_LEFT));
        assert_eq!(button_bit(MouseButton::Right), Some(BUTTON_RIGHT));
        assert_eq!(button_bit(MouseButton::Back), Some(BUTTON_BACK));
        assert_eq!(button_bit(MouseButton::Forward), Some(BUTTON_FORWARD));
        assert_eq!(button_bit(MouseButton::Other(9)), None);
    }

    #[test]
    fn wheel_turns_lines_and_pixels_into_whole_notches() {
        let mut wheel = Wheel::default();
        assert_eq!(
            wheel.notches(MouseScrollDelta::LineDelta(0.0, 2.0), 1.0),
            (2, 0)
        );
        assert_eq!(
            wheel.notches(MouseScrollDelta::LineDelta(-1.0, -1.0), 1.0),
            (-1, -1)
        );
        // Trackpad pixels add up across events, in logical pixels.
        let pixels = |x, y| MouseScrollDelta::PixelDelta(PhysicalPosition::new(x, y));
        assert_eq!(wheel.notches(pixels(0.0, 50.0), 2.0), (0, 0));
        assert_eq!(wheel.notches(pixels(0.0, 40.0), 2.0), (1, 0));
        assert_eq!(wheel.notches(pixels(0.0, -10.0), 2.0), (0, 0));
    }

    #[test]
    fn throughput_reports_rate_size_bandwidth_and_encoding() {
        let before = StatsSnapshot {
            bytes: 1_000_000,
            frames: 10,
            ..StatsSnapshot::default()
        };
        let after = StatsSnapshot {
            bytes: 13_000_000,
            frames: 70,
            encoding: Some(7),
            continuous_updates: false,
        };
        assert_eq!(
            throughput(before, after, std::time::Duration::from_secs(2)),
            "30 fps · 200 KB/frame · 48 Mbit/s · Tight"
        );
        let pushed = StatsSnapshot {
            continuous_updates: true,
            ..after
        };
        assert!(
            throughput(before, pushed, std::time::Duration::from_secs(2)).ends_with("Tight · push")
        );
        assert_eq!(
            throughput(before, before, std::time::Duration::from_secs(1)),
            "0 fps · 0 KB/frame · 0 Mbit/s · waiting"
        );
    }

    #[test]
    fn fit_placement_letterboxes_the_whole_image() {
        let (target, source) = placement(WindowMode::Fit, (2, 1), (4, 4)).unwrap();
        assert_eq!(
            target,
            Rect {
                x0: 0,
                y0: 1,
                x1: 4,
                y1: 3
            }
        );
        assert_eq!(
            source,
            Rect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 1
            }
        );
        // Custom sizes draw like Fit once the window exists.
        assert_eq!(
            placement(WindowMode::Custom, (2, 1), (4, 4)),
            placement(WindowMode::Fit, (2, 1), (4, 4))
        );
        assert_eq!(placement(WindowMode::Fit, (2, 1), (0, 4)), None);
    }

    #[test]
    fn native_mode_centers_and_crops_without_scaling() {
        let (target, source) = placement(WindowMode::Native, (4, 2), (2, 2)).unwrap();
        assert_eq!(
            target,
            Rect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 2
            }
        );
        assert_eq!(
            source,
            Rect {
                x0: 1,
                y0: 0,
                x1: 3,
                y1: 2
            }
        );
        assert_eq!(
            pointer_for_mode(0.0, 0.0, (2, 2), (4, 2), WindowMode::Native),
            Some((1, 0))
        );
        // A window larger than the image centers it at one pixel per pixel.
        let (target, source) = placement(WindowMode::Native, (2, 2), (6, 4)).unwrap();
        assert_eq!((target.x0, target.y0, target.width()), (2, 1, 2));
        assert_eq!((source.x0, source.width()), (0, 2));
        assert_eq!(
            pointer_for_mode(3.0, 2.0, (6, 4), (2, 2), WindowMode::Native),
            Some((1, 1))
        );
    }

    #[test]
    fn overlay_areas_are_clipped_to_the_window() {
        let area = Box2 {
            x: 8,
            y: 8,
            w: 432,
            h: 612,
        };
        let clipped = clip(area, (300, 200)).unwrap();
        assert_eq!(
            (clipped.x, clipped.y, clipped.w, clipped.h),
            (8, 8, 292, 192)
        );
        assert!(clip(area, (8, 600)).is_none());
    }

    /// A plain, password-protected listener request on `address`.
    fn plain_request(address: &str, password: &str) -> ServeRequest {
        ServeRequest {
            tls: topvnc::transport::TlsOptions {
                unencrypted: true,
                ..Default::default()
            },
            performance: PerformanceMode::Standard,
            address: address.into(),
            display: desktop_host::DisplaySelection::Primary,
            password: Some(password.into()),
            allow_insecure: false,
            scale: 1.0,
            mouse: desktop_host::MouseMode::Auto,
            priority: topvnc::UpdatePriority::Whole,
        }
    }

    /// Plays the server thread's part: a real RFB server with real
    /// listeners, whose notices reach the hosted server through `events`.
    struct FakeHost {
        server: topvnc::VncServer,
        events: mpsc::Sender<HostEvent>,
        shutdown: Arc<AtomicBool>,
    }

    impl FakeHost {
        /// Report the listeners and start serving, as the thread does.
        /// `hosted` handles each notice as soon as it is sent, the earliest
        /// the window could, so a listener it removes on `Listeners` is
        /// already closed when the thread reports `Serving`.
        fn serve(&self, hosted: &mut HostedServer) -> Option<String> {
            let hosted = std::cell::RefCell::new(hosted);
            let failure = std::cell::RefCell::new(None);
            desktop_host::report_serving(
                &self.server,
                &|notice| {
                    self.events.send(HostEvent::Notice(notice)).unwrap();
                    if let Some(error) = hosted.borrow_mut().poll() {
                        failure.borrow_mut().get_or_insert(error);
                    }
                },
                "DISPLAY1".into(),
                1920,
                1080,
            )
            .unwrap();
            failure.into_inner()
        }

        fn listeners(&self) -> ServerListeners {
            self.server.listeners()
        }
    }

    /// Start `hosted` for `owner` as [`HostedServer::spawn`] does, with a
    /// [`FakeHost`] in place of desktop capture. The claim names the
    /// address the server actually bound.
    fn fake_start(hosted: &mut HostedServer, owner: Owner, mut request: ServeRequest) -> FakeHost {
        let server = topvnc::VncServer::bind(
            request.address.as_str(),
            Framebuffer::new(4, 4).unwrap(),
            topvnc::ServerConfig {
                password: request.password.clone(),
                allow_insecure: request.allow_insecure,
                ..Default::default()
            },
        )
        .unwrap();
        request.address = server.local_addr().unwrap().to_string();
        let (events, receiver) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        hosted.claims[owner.index()] = Some(Claim::new(&request));
        hosted.first = Some(owner);
        hosted.phase = ServerPhase::Starting;
        hosted.running = Some((Arc::clone(&shutdown), receiver, thread::spawn(|| {})));
        hosted.refresh();
        FakeHost {
            server,
            events,
            shutdown,
        }
    }

    fn listener(hosted: &HostedServer, owner: Owner) -> ListenerId {
        hosted.claim(owner).unwrap().listener.as_ref().unwrap().0
    }

    #[test]
    fn hosted_server_status_follows_notices_in_order() {
        let mut server = HostedServer::default();
        // Notices for a server that is not starting are ignored.
        server.apply(ServerNotice::Serving {
            address: "0.0.0.0:5900".parse().unwrap(),
            display: "DISPLAY1".into(),
            width: 1920,
            height: 1080,
        });
        assert_eq!(server.view.phase, ServerPhase::Stopped);
        let host = fake_start(
            &mut server,
            Owner::ServerTab,
            plain_request("127.0.0.1:0", "pw"),
        );
        assert_eq!(server.view.phase, ServerPhase::Starting);
        assert_eq!(host.serve(&mut server), None);
        server.apply(ServerNotice::Connections(2));
        server.apply(ServerNotice::Resized {
            width: 1280,
            height: 720,
        });
        server.apply(ServerNotice::ViewOnly(true));
        server.apply(ServerNotice::Message("Desktop capture resumed.".into()));
        assert_eq!(
            server.view.phase,
            ServerPhase::Serving {
                address: host.server.local_addr().unwrap().to_string(),
                width: 1280,
                height: 720,
                connections: 2,
                authenticated: true,
                view_only: true,
            }
        );
        assert_eq!(
            server.view.message.as_deref(),
            Some("Desktop capture resumed.")
        );
        server.apply(ServerNotice::ViewOnly(false));
        assert!(matches!(
            server.view.phase,
            ServerPhase::Serving {
                view_only: false,
                ..
            }
        ));
    }

    #[test]
    fn browser_access_joins_the_server_tab_server_and_leaves_it_running() {
        let mut server = HostedServer::default();
        let host = fake_start(
            &mut server,
            Owner::ServerTab,
            plain_request("127.0.0.1:0", "native"),
        );
        host.serve(&mut server);
        assert_eq!(server.web_view(), HostServer::Stopped);
        let native = server.view.phase.clone();

        server
            .start(Owner::Web, plain_request("127.0.0.1:0", "browser"))
            .unwrap();
        assert_eq!(server.web_view(), HostServer::Serving);
        let web = listener(&server, Owner::Web);
        assert_ne!(web, listener(&server, Owner::ServerTab));
        assert!(host.listeners().local_addr(web).is_some());
        assert_eq!(server.view.phase, native, "the Server tab is unchanged");

        server.stop(Owner::Web);
        assert_eq!(server.web_view(), HostServer::Stopped);
        assert_eq!(host.listeners().local_addr(web), None);
        assert!(!host.shutdown.load(Ordering::Acquire));
        assert_eq!(server.view.phase, native);

        server.stop(Owner::ServerTab);
        assert!(host.shutdown.load(Ordering::Acquire));
        assert_eq!(server.view.phase, ServerPhase::Stopping);
        // Browser access waits for the server to stop before it restarts.
        assert!(
            server
                .start(Owner::Web, plain_request("127.0.0.1:0", "browser"))
                .is_err()
        );
        host.events.send(HostEvent::Stopped(Ok(()))).unwrap();
        assert_eq!(server.poll(), None);
        assert_eq!(server.view.phase, ServerPhase::Stopped);
        assert_eq!(server.web_view(), HostServer::Stopped);
    }

    #[test]
    fn the_server_tab_joins_and_leaves_without_stopping_browser_access() {
        let mut server = HostedServer::default();
        let host = fake_start(
            &mut server,
            Owner::Web,
            plain_request("127.0.0.1:0", "browser"),
        );
        host.serve(&mut server);
        assert_eq!(server.web_view(), HostServer::Serving);
        // The Server tab does not show the WEB tab's listener as its own.
        assert_eq!(server.view.phase, ServerPhase::Stopped);

        server
            .start(Owner::ServerTab, plain_request("127.0.0.1:0", "native"))
            .unwrap();
        let native = listener(&server, Owner::ServerTab);
        assert!(matches!(
            &server.view.phase,
            ServerPhase::Serving { address, .. }
                if *address == host.listeners().local_addr(native).unwrap().to_string()
        ));
        assert!(server.view.message.is_some(), "joining is explained");

        server.stop(Owner::ServerTab);
        assert_eq!(server.view.phase, ServerPhase::Stopped);
        assert_eq!(host.listeners().local_addr(native), None);
        assert_eq!(server.web_view(), HostServer::Serving);
        assert!(!host.shutdown.load(Ordering::Acquire));
    }

    #[test]
    fn one_address_is_shared_only_by_viewers_that_connect_the_same_way() {
        let mut server = HostedServer::default();
        let host = fake_start(
            &mut server,
            Owner::ServerTab,
            plain_request("127.0.0.1:0", "pw"),
        );
        host.serve(&mut server);
        let address = host.server.local_addr().unwrap().to_string();

        let error = server
            .start(Owner::Web, plain_request(&address, "other"))
            .unwrap_err();
        assert!(error.contains("another port"), "{error}");
        assert_eq!(server.web_view(), HostServer::Stopped);
        assert!(server.claim(Owner::Web).is_none());

        server
            .start(Owner::Web, plain_request(&address, "pw"))
            .unwrap();
        let shared = listener(&server, Owner::ServerTab);
        assert_eq!(listener(&server, Owner::Web), shared);
        server.stop(Owner::Web);
        assert!(host.listeners().local_addr(shared).is_some());
        assert!(matches!(server.view.phase, ServerPhase::Serving { .. }));
    }

    #[test]
    fn a_tab_that_joins_while_the_server_starts_waits_for_its_listener() {
        let mut server = HostedServer::default();
        let host = fake_start(
            &mut server,
            Owner::Web,
            plain_request("127.0.0.1:0", "browser"),
        );
        server
            .start(Owner::ServerTab, plain_request("127.0.0.1:0", "native"))
            .unwrap();
        assert_eq!(server.view.phase, ServerPhase::Starting);
        assert_eq!(server.web_view(), HostServer::Starting);
        assert_eq!(host.serve(&mut server), None);
        assert!(matches!(server.view.phase, ServerPhase::Serving { .. }));
        assert_eq!(server.web_view(), HostServer::Serving);
        assert_ne!(
            listener(&server, Owner::ServerTab),
            listener(&server, Owner::Web)
        );
    }

    #[test]
    fn the_first_listener_closes_when_its_tab_stops_during_start() {
        let mut server = HostedServer::default();
        let host = fake_start(
            &mut server,
            Owner::ServerTab,
            plain_request("127.0.0.1:0", "native"),
        );
        server
            .start(Owner::Web, plain_request("127.0.0.1:0", "browser"))
            .unwrap();
        server.stop(Owner::ServerTab);
        assert!(!host.shutdown.load(Ordering::Acquire), "the WEB tab waits");
        assert_eq!(host.serve(&mut server), None);
        assert_eq!(host.listeners().primary(), None);
        assert_eq!(server.view.phase, ServerPhase::Stopped);
        assert_eq!(server.web_view(), HostServer::Serving);
    }

    #[test]
    fn a_listener_that_cannot_open_is_reported_and_the_server_keeps_running() {
        let mut server = HostedServer::default();
        let host = fake_start(
            &mut server,
            Owner::ServerTab,
            plain_request("127.0.0.1:0", "native"),
        );
        // Another program holds the port the WEB tab asks for.
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = taken.local_addr().unwrap().to_string();
        server
            .start(Owner::Web, plain_request(&address, "browser"))
            .unwrap();
        let error = host.serve(&mut server).unwrap();
        assert!(error.contains(&address), "{error}");
        assert_eq!(server.web_view(), HostServer::Stopped);
        assert!(matches!(server.view.phase, ServerPhase::Serving { .. }));
        assert!(!host.shutdown.load(Ordering::Acquire));
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn hosted_server_reports_unsupported_platforms_and_stops() {
        let mut server = HostedServer::default();
        server
            .start(
                Owner::ServerTab,
                ServeRequest {
                    tls: Default::default(),
                    performance: PerformanceMode::Standard,
                    address: "127.0.0.1:0".into(),
                    display: desktop_host::DisplaySelection::Primary,
                    password: Some("secret".into()),
                    allow_insecure: false,
                    scale: 1.0,
                    mouse: desktop_host::MouseMode::Auto,
                    priority: topvnc::UpdatePriority::Whole,
                },
            )
            .unwrap();
        assert_eq!(server.view.phase, ServerPhase::Starting);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let error = loop {
            if let Some(error) = server.poll() {
                break error;
            }
            assert!(std::time::Instant::now() < deadline);
            thread::yield_now();
        };
        assert_eq!(error, SERVER_UNAVAILABLE);
        assert_eq!(server.view.phase, ServerPhase::Stopped);
        assert!(server.running.is_none());
    }
}

#[cfg(test)]
mod stage_timing {
    use super::*;
    use desktop_host::{CaptureSurface, Rect as HostRect, Rotation};
    use std::time::Instant;
    use topvnc::{DamageRect, ServerConfig, VncServer};

    /// `cargo test --release --bin topvnc stage_timing -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn stage_timing() {
        for (width, height) in [(3024usize, 1964usize), (1512, 982), (1920, 1080)] {
            let mut seed = 7u32;
            let bytes: Vec<u8> = (0..width * height * 4)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as u8
                })
                .collect();
            let surface = CaptureSurface {
                bytes: &bytes,
                row_pitch: width * 4,
                width,
                height,
                rotation: Rotation::Identity,
            };
            let mut framebuffer = Framebuffer::new(width as u16, height as u16).unwrap();
            let started = Instant::now();
            surface.copy_rect(
                HostRect::new(0, 0, width as i32, height as i32),
                framebuffer.pixels_mut(),
                width,
            );
            let copy = started.elapsed();
            let server = VncServer::bind(
                "127.0.0.1:0",
                Framebuffer::new(width as u16, height as u16).unwrap(),
                ServerConfig {
                    allow_insecure: true,
                    ..ServerConfig::default()
                },
            )
            .unwrap();
            let damage = [DamageRect {
                x: 0,
                y: 0,
                width: width as u16,
                height: height as u16,
            }];
            let started = Instant::now();
            server
                .update_framebuffer_regions(&framebuffer, &damage)
                .unwrap();
            let compare = started.elapsed();
            // The viewer converts decoded rectangles into its framebuffer;
            // scaling happens on the GPU.
            let mut viewer = Framebuffer::new(width as u16, height as u16).unwrap();
            let started = Instant::now();
            viewer
                .apply_raw(0, 0, width as u16, height as u16, &bytes)
                .unwrap();
            let apply = started.elapsed();
            println!(
                "{width}x{height}: host copy {:.1} ms, tile compare {:.1} ms | viewer apply {:.1} ms",
                copy.as_secs_f64() * 1e3,
                compare.as_secs_f64() * 1e3,
                apply.as_secs_f64() * 1e3,
            );
        }
    }
}

#[cfg(test)]
#[path = "../tests/native/app.rs"]
mod native_connection_tests;
