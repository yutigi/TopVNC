use std::collections::HashMap;
use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use topvnc::{
    BUTTON_BACK, BUTTON_FORWARD, BUTTON_LEFT, BUTTON_MIDDLE, BUTTON_RIGHT, BUTTON_WHEEL_DOWN,
    BUTTON_WHEEL_LEFT, BUTTON_WHEEL_RIGHT, BUTTON_WHEEL_UP, CenterWatch, Encoding, Foveation,
    Framebuffer, InputWriter, Session, StatsSnapshot, UploadArea, encoding_name,
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

mod settings;
mod ui;
mod window;
use desktop_host::{HostPermissions, MouseMode, ServeOptions, ServerNotice};
use ui::{
    Box2, Canvas, Compression, Config, Field, Quality, ServeRequest, ServerPhase, ServerView, Tab,
    UiState, WindowMode,
};
use window::{Draw, Layer, Presenter, Ui, UiEvent, Waker};

fn encoding(config: &Config) -> Encoding {
    match config.compression {
        Compression::Raw => Encoding::Raw,
        Compression::Zlib => Encoding::Zlib,
        Compression::Tight => Encoding::Tight {
            quality: config.jpeg_quality,
        },
    }
}

const USAGE: &str = "usage: topvnc [HOST:PORT] [--allow-insecure] [--fit | --native-size | --window WIDTHxHEIGHT] [--quality 0-9] [--input-debug]";

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
    let mut input_debug = false;
    let mut address = None;
    let mut args = arguments.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
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

    #[cfg(windows)]
    let _timer = TimerResolution::new();
    let mut ui = Ui::new()?;
    let mut connection_error = None;
    // Lives across viewer sessions; dropping it stops the server.
    let mut server = HostedServer::default();
    loop {
        let Some((next_config, session)) =
            show_landing(&mut ui, config, connection_error.take(), &mut server)?
        else {
            return Ok(());
        };
        config = next_config;
        connection_error = match run_session(&mut ui, session, &mut config, input_debug) {
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

/// The desktop server started from the Server tab. It runs on a background
/// thread so the UI and viewer sessions stay responsive.
#[derive(Default)]
struct HostedServer {
    view: ServerView,
    running: Option<(
        Arc<AtomicBool>,
        mpsc::Receiver<HostEvent>,
        thread::JoinHandle<()>,
    )>,
    authenticated: bool,
}

impl HostedServer {
    fn start(&mut self, request: ServeRequest) {
        if self.running.is_some() {
            return;
        }
        let shutdown = Arc::new(AtomicBool::new(false));
        let (events, receiver) = mpsc::channel();
        let options = ServeOptions {
            address: request.address,
            display: request.display,
            allow_insecure: request.allow_insecure,
            scale: request.scale,
            mouse: MouseMode::Auto,
            foveation: request.foveation,
        };
        self.authenticated = request.password.is_some();
        let password = request.password;
        let thread_shutdown = Arc::clone(&shutdown);
        let spawned = thread::Builder::new()
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
            });
        match spawned {
            Ok(thread) => {
                self.view = ServerView {
                    phase: ServerPhase::Starting,
                    message: None,
                    permissions: self.view.permissions,
                };
                self.running = Some((shutdown, receiver, thread));
            }
            Err(error) => {
                self.view.message = Some(format!("Could not start the server: {error}"));
            }
        }
    }

    fn stop(&mut self) {
        if let Some((shutdown, ..)) = &self.running {
            shutdown.store(true, Ordering::Release);
            self.view.phase = ServerPhase::Stopping;
        }
    }

    /// Apply status from the server thread. Returns the error that ended it.
    fn poll(&mut self) -> Option<String> {
        let (_, receiver, _) = self.running.as_ref()?;
        let events = receiver.try_iter().collect::<Vec<_>>();
        let mut failure = None;
        let mut stopped = false;
        for event in events {
            match event {
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
            self.view = ServerView {
                phase: ServerPhase::Stopped,
                message: failure.is_none().then(|| "Server stopped.".into()),
                permissions: self.view.permissions,
            };
        }
        failure
    }

    fn apply(&mut self, notice: ServerNotice) {
        match (notice, &mut self.view.phase) {
            (
                ServerNotice::Serving {
                    address,
                    width,
                    height,
                    ..
                },
                ServerPhase::Starting,
            ) => {
                self.view.phase = ServerPhase::Serving {
                    address: address.to_string(),
                    width,
                    height,
                    connections: 0,
                    authenticated: self.authenticated,
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
    let mut state = UiState {
        error,
        ..UiState::default()
    };
    let (result_tx, result_rx) = mpsc::channel::<Result<Session, String>>();
    let mut connecting = false;
    let mut permissions_checked: Option<Instant> = None;
    let mut dragging_serve_scale = false;
    // The pointer position in form pixels while it is over the window.
    let mut cursor: Option<(usize, usize)> = None;
    loop {
        // Permissions can change in System Settings while the window is open.
        if server.view.phase == ServerPhase::Stopped
            && permissions_checked.is_none_or(|checked| checked.elapsed().as_secs() >= 1)
        {
            server.view.permissions = host_permissions();
            permissions_checked = Some(Instant::now());
        }
        if let Some(error) = server.poll() {
            state.switch_tab(Tab::Server);
            state.error = Some(error);
        }
        ui::landing(
            &mut Canvas::new(&mut pixels, WIDTH, HEIGHT),
            &config,
            &state,
            connecting,
            &server.view,
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
                    if ui::TAB_CONNECT.contains(x, y) {
                        state.switch_tab(Tab::Connect);
                    } else if ui::TAB_SERVER.contains(x, y) {
                        state.switch_tab(Tab::Server);
                    } else if state.tab == Tab::Server {
                        // The form describes the next start, so it is locked while serving.
                        let editable = server.view.phase == ServerPhase::Stopped;
                        state.focus = None;
                        for (area, field) in [
                            (ui::HOST, Field::ServeHost),
                            (ui::PORT, Field::ServePort),
                            (ui::PASSWORD, Field::ServePassword),
                            (ui::SERVE_DISPLAY, Field::ServeDisplay),
                        ] {
                            if editable && area.contains(x, y) {
                                state.focus = Some(field);
                            }
                        }
                        let serve = &mut config.serve;
                        if editable && ui::INSECURE.contains(x, y) {
                            serve.allow_insecure = !serve.allow_insecure;
                        }
                        if editable && ui::LOCAL_ONLY.contains(x, y) {
                            serve.host = ui::LOCAL_ONLY_HOST.into();
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
                        for (area, foveation) in [
                            (ui::SERVE_FOVEATE_AUTO, Foveation::Auto),
                            (ui::SERVE_FOVEATE_ON, Foveation::On),
                            (ui::SERVE_FOVEATE_OFF, Foveation::Off),
                        ] {
                            if editable && area.contains(x, y) {
                                serve.foveation = foveation;
                            }
                        }
                        if ui::CONNECT.contains(x, y) {
                            server_clicked = true;
                        }
                    } else {
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
                        if ui::RAW.contains(x, y) {
                            config.compression = Compression::Raw;
                        }
                        if ui::ZLIB.contains(x, y) {
                            config.compression = Compression::Zlib;
                        }
                        if ui::TIGHT.contains(x, y) {
                            config.compression = Compression::Tight;
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
                        }
                        if ui::FPS120.contains(x, y) {
                            config.fps = 120;
                        }
                        if ui::FPS_NO_LIMIT.contains(x, y) {
                            config.fps = 0;
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
        if server_clicked {
            match server.view.phase {
                ServerPhase::Stopped => match config.serve.request() {
                    Ok(request) => {
                        state.error = None;
                        state.focus = None;
                        server.start(request);
                    }
                    Err(error) => state.error = Some(error),
                },
                ServerPhase::Serving { .. } => server.stop(),
                ServerPhase::Starting | ServerPhase::Stopping => {}
            }
        }
        if connect_clicked && !connecting {
            match config.address() {
                Ok(address) => {
                    state.error = None;
                    connecting = true;
                    let sender = result_tx.clone();
                    let password = config.password.clone();
                    let allow_insecure = config.allow_insecure;
                    let encoding = encoding(&config);
                    thread::spawn(move || {
                        let result = Session::connect_with_encoding(
                            &address,
                            allow_insecure,
                            encoding,
                            || Ok(password.clone()),
                        )
                        .map_err(|error| error.to_string());
                        let _ = sender.send(result);
                    });
                }
                Err(error) => state.error = Some(error),
            }
        }
    }
}

/// How often the title bar's statistics refresh.
const STATS_INTERVAL: Duration = Duration::from_secs(1);

/// When the oldest update the presenter thread has not yet shown finished
/// uploading; set by the network thread, which also wakes that thread.
#[derive(Default)]
struct Arrival {
    pending: Mutex<Option<Instant>>,
    ready: Condvar,
}

impl Arrival {
    fn mark(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.get_or_insert_with(Instant::now);
            self.ready.notify_one();
        }
    }

    fn take(&self) -> Option<Instant> {
        self.pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.take())
    }

    /// Put back an arrival that could not be presented, keeping the older
    /// of it and any newer one.
    fn restore(&self, arrived: Option<Instant>) {
        if let (Ok(mut pending), Some(arrived)) = (self.pending.lock(), arrived) {
            *pending = Some(pending.map_or(arrived, |newer| newer.min(arrived)));
        }
    }

    /// Wait for an update and take when it arrived; `None` once `stop` is set.
    fn wait(&self, stop: &AtomicBool) -> Option<Instant> {
        let mut pending = self.pending.lock().ok()?;
        loop {
            if stop.load(Ordering::Acquire) {
                return None;
            }
            if let Some(arrived) = pending.take() {
                return Some(arrived);
            }
            pending = self.ready.wait(pending).ok()?;
        }
    }

    /// Wake a thread in [`Arrival::wait`] after setting its stop flag.
    fn wake(&self) {
        drop(self.pending.lock());
        self.ready.notify_all();
    }
}

/// Lock `mutex`, going on after a thread panicked while holding it.
fn acquire<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One layer of a frame: where it goes in the window, in physical pixels, and
/// which part of it shows.
#[derive(Clone)]
struct LayerDraw<L = Arc<Layer>> {
    layer: L,
    target: [f64; 4],
    source: [f64; 4],
    smooth: bool,
}

/// The window's physical size and the layers drawn in it, in order.
#[derive(Clone)]
struct FrameLayout {
    size: (u32, u32),
    draws: Vec<LayerDraw>,
}

impl FrameLayout {
    fn draw(&self, presenter: &mut Presenter) -> Result<bool, Box<dyn Error>> {
        let draws: Vec<Draw<'_>> = self
            .draws
            .iter()
            .map(|draw| Draw {
                layer: &draw.layer,
                target: draw.target,
                source: draw.source,
                smooth: draw.smooth,
            })
            .collect();
        presenter.draw_at(self.size, &draws)
    }
}

/// The layers of a frame for a window of `logical` size at `scale`: the
/// remote image, placed by `mode`, and the settings overlay in its `Box2`.
fn frame_layers<L: Clone>(
    image: &L,
    smooth_image: bool,
    overlay: Option<(&L, Box2)>,
    mode: WindowMode,
    remote: (usize, usize),
    logical: (usize, usize),
    scale: f64,
) -> Vec<LayerDraw<L>> {
    let physical = |x: usize, y: usize, w: usize, h: usize| {
        [
            x as f64 * scale,
            y as f64 * scale,
            w as f64 * scale,
            h as f64 * scale,
        ]
    };
    let mut draws = Vec::with_capacity(2);
    if let Some((target, source)) = placement(mode, remote, logical) {
        draws.push(LayerDraw {
            layer: image.clone(),
            target: physical(target.x0, target.y0, target.width(), target.height()),
            source: [
                source.x0 as f64,
                source.y0 as f64,
                source.width() as f64,
                source.height() as f64,
            ],
            smooth: smooth_image,
        });
    }
    if let Some((layer, area)) = overlay {
        draws.push(LayerDraw {
            layer: layer.clone(),
            target: physical(area.x, area.y, area.w, area.h),
            source: [area.x as f64, area.y as f64, area.w as f64, area.h as f64],
            smooth: scale.fract() != 0.0,
        });
    }
    draws
}

/// State the window thread shares with the presenter thread, which shows
/// each update as the network thread uploads it instead of when the window
/// thread's event loop next runs. On macOS that loop returns no more often
/// than the display refreshes, which is less often than an update's center
/// and the rest of it need presenting.
struct PresentShared {
    arrival: Arrival,
    /// Ends the presenter thread.
    stop: AtomicBool,
    /// The window is minimized or fully covered.
    occluded: AtomicBool,
    /// The FPS limit, or 0 for none.
    fps: AtomicUsize,
    /// Whether the network thread may upload the center of an update before
    /// the rest of it has arrived.
    early: AtomicBool,
    /// What to draw, set by the window thread whenever it changes.
    layout: Mutex<Option<FrameLayout>>,
    /// Time from an update's upload to its presentation, summed since the
    /// title last changed, and the number of presentations.
    to_present: Mutex<(Duration, u32)>,
}

impl PresentShared {
    fn new(fps: usize) -> Self {
        Self {
            arrival: Arrival::default(),
            stop: AtomicBool::new(false),
            occluded: AtomicBool::new(false),
            fps: AtomicUsize::new(fps),
            // With a frame limit, presentation waits for the next slot
            // whichever part of an update arrived, so showing the center
            // early would only leave the rest of the update a frame behind.
            early: AtomicBool::new(fps == 0),
            layout: Mutex::new(None),
            to_present: Mutex::new((Duration::ZERO, 0)),
        }
    }
}

/// Show each update as the network thread uploads it, until `shared.stop` is
/// set. `draw` draws a layout and says whether the window could take the
/// frame; its error ends the loop.
fn present_frames(
    shared: &PresentShared,
    mut draw: impl FnMut(&FrameLayout) -> Result<bool, String>,
) -> Result<(), String> {
    let mut last_present: Option<Instant> = None;
    // No drawing before this, after the window could not take a frame.
    let mut retry_at = Instant::now();
    while let Some(arrived) = shared.arrival.wait(&shared.stop) {
        let interval = match shared.fps.load(Ordering::Relaxed) {
            0 => Duration::ZERO,
            fps => Duration::from_secs_f64(1.0 / fps as f64),
        };
        let due = last_present.map_or(retry_at, |at| (at + interval).max(retry_at));
        let now = Instant::now();
        if due > now {
            thread::sleep(due - now);
            if shared.stop.load(Ordering::Acquire) {
                return Ok(());
            }
        }
        // Updates that arrived meanwhile are part of this frame.
        shared.arrival.take();
        let layout = if shared.occluded.load(Ordering::Relaxed) {
            None
        } else {
            acquire(&shared.layout).clone()
        };
        let shown = match layout {
            Some(layout) => draw(&layout)?,
            None => false,
        };
        let now = Instant::now();
        if shown {
            last_present = Some(now);
            let mut total = acquire(&shared.to_present);
            total.0 += now.saturating_duration_since(arrived);
            total.1 += 1;
        } else {
            shared.arrival.restore(Some(arrived));
            retry_at = now + RETRY_INTERVAL;
        }
    }
    Ok(())
}

/// The presenter thread. Dropping it stops and joins the thread, so drop it
/// before the window and presenter it uses.
struct PresentThread {
    shared: Arc<PresentShared>,
    handle: Option<thread::JoinHandle<()>>,
}

impl PresentThread {
    /// An error in presenting goes to `errors`, and `waker` wakes the window
    /// thread to read it.
    fn spawn(
        presenter: Arc<Mutex<Presenter>>,
        shared: Arc<PresentShared>,
        errors: mpsc::Sender<String>,
        waker: Waker,
    ) -> std::io::Result<Self> {
        let thread_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("topvnc-present".into())
            .spawn(move || {
                let result = present_frames(&thread_shared, |layout| {
                    layout
                        .draw(&mut acquire(&presenter))
                        .map_err(|error| error.to_string())
                });
                if let Err(message) = result {
                    let _ = errors.send(message);
                    waker.wake();
                }
            })?;
        Ok(Self {
            shared,
            handle: Some(handle),
        })
    }

    /// Whether the thread has ended, as after a panic.
    fn stopped(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
    }
}

impl Drop for PresentThread {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.arrival.wake();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// How far ahead of the rest of their updates centers reached the GPU,
/// summed since the title last changed, and the number of updates; set by
/// the network thread.
#[derive(Default)]
struct CenterLead(Mutex<(Duration, u32)>);

impl CenterLead {
    fn add(&self, lead: Duration) {
        if let Ok(mut total) = self.0.lock() {
            total.0 += lead;
            total.1 += 1;
        }
    }

    fn take(&self) -> (Duration, u32) {
        self.0
            .lock()
            .map(|mut total| std::mem::take(&mut *total))
            .unwrap_or_default()
    }
}

/// The title's note on how early centers were presented, for the updates
/// that had one, or nothing.
fn center_status((total, updates): (Duration, u32)) -> Option<String> {
    (updates > 0).then(|| {
        format!(
            " · center {:.1} ms early",
            (total / updates).as_secs_f64() * 1e3
        )
    })
}

/// Upload `area` of `framebuffer` to the image.
fn upload(image: &Layer, framebuffer: &Framebuffer, area: UploadArea) {
    image.upload(
        framebuffer.pixels(),
        framebuffer.width(),
        area.x,
        area.y,
        area.width,
        area.height,
    );
}

/// How long to wait before drawing again when the window could not take a
/// frame, as while it is minimized.
const RETRY_INTERVAL: Duration = Duration::from_millis(16);

/// The settings overlay, drawn in software at the window's logical size.
struct Overlay {
    pixels: Vec<u32>,
    size: (usize, usize),
    layer: Arc<Layer>,
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
) -> Result<Option<String>, Box<dyn Error>> {
    let writer = session.writer();
    let result = run_session_inner(ui, session, config, input_debug);
    let _ = writer.shutdown();
    result
}

fn run_session_inner(
    ui: &mut Ui,
    mut session: Session,
    config: &mut Config,
    input_debug: bool,
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
    let remote = (usize::from(info.width), usize::from(info.height));
    let initial = match config.window_mode {
        WindowMode::Fit => fit_dimensions(remote, display_space()),
        WindowMode::Native => remote,
        WindowMode::Custom => config.custom_size()?,
    };
    let title = format!("TopVNC — {}", info.name);
    let window = ui.create_window(
        WindowAttributes::default()
            .with_title(&title)
            .with_inner_size(LogicalSize::new(initial.0 as f64, initial.1 as f64))
            .with_resizable(true),
    )?;
    let presenter = Arc::new(Mutex::new(Presenter::new(ui, Arc::clone(&window))?));
    let image = Arc::new(acquire(&presenter).layer(u32::from(info.width), u32::from(info.height)));
    let shared = Arc::new(PresentShared::new(config.fps));
    let center_lead = Arc::new(CenterLead::default());
    let (error_tx, error_rx) = mpsc::channel();
    let (present_error_tx, present_error_rx) = mpsc::channel();
    {
        let image = Arc::clone(&image);
        let shared = Arc::clone(&shared);
        let center_lead = Arc::clone(&center_lead);
        let writer = writer.clone();
        let waker = ui.waker();
        let (width, height) = (info.width, info.height);
        thread::Builder::new()
            .name("topvnc-network".into())
            .spawn(move || {
                let result: std::io::Result<()> = (|| {
                    // Updates are decoded into this thread's own framebuffer
                    // and uploaded from it, so the window thread never copies
                    // or scales pixels. An update goes up whole, except that
                    // a server that sends the screen center first (spec 008)
                    // has the center uploaded and presented as soon as it is
                    // complete, ahead of the rest of the update.
                    let mut framebuffer = Framebuffer::new(width, height)?;
                    let mut watch = CenterWatch::new(width, height);
                    let mut scratch = Vec::new();
                    writer.request_update(false, width, height)?;
                    loop {
                        watch.begin(shared.early.load(Ordering::Relaxed));
                        let mut early = None;
                        session.read_update_pipelined(&mut scratch, |x, y, w, h, bytes| {
                            framebuffer.apply_raw(x, y, w, h, bytes)?;
                            if let Some(area) = watch.rectangle(x, y, w, h) {
                                upload(&image, &framebuffer, area);
                                early = Some(Instant::now());
                                shared.arrival.mark();
                            }
                            Ok(())
                        })?;
                        if let Some(area) = watch.finish() {
                            upload(&image, &framebuffer, area);
                            if let Some(early) = early {
                                center_lead.add(early.elapsed());
                            }
                            shared.arrival.mark();
                        }
                    }
                })();
                let _ = error_tx.send(result);
                waker.wake();
            })?;
    }
    // Shows each update as it is uploaded. Declared after the window and the
    // presenter so that it stops before they drop.
    let present = PresentThread::spawn(
        Arc::clone(&presenter),
        Arc::clone(&shared),
        present_error_tx,
        ui.waker(),
    )?;

    let mut overlay: Option<Overlay> = None;
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
    // A left press that began on the overlay; its release stays local.
    let mut ui_captured_mouse = false;
    let mut focused = window.has_focus();
    let mut locked = false;
    // A minimized or fully covered window draws nothing until it is shown.
    let mut occluded = false;
    let mut redraw = true;
    // No drawing before this, after the window could not take a frame.
    let mut retry_at = Instant::now();
    let mut last_stats = (Instant::now(), stats.snapshot());
    loop {
        // The presenter thread paces and shows updates; this thread draws
        // when the window, the overlay, or the layout changes.
        shared.fps.store(config.fps, Ordering::Relaxed);
        shared.early.store(config.fps == 0, Ordering::Relaxed);
        let now = Instant::now();
        // Sleep until input arrives, a redraw is due, or the statistics are
        // due. Frames do not wake this thread.
        let timeout = if redraw && !occluded {
            retry_at.saturating_duration_since(now)
        } else {
            (last_stats.0 + STATS_INTERVAL).saturating_duration_since(now)
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
                            shared.occluded.store(now_occluded, Ordering::Relaxed);
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
                                    } else if ui::LIVE_60.contains(x, y) {
                                        config.fps = 60;
                                    } else if ui::LIVE_120.contains(x, y) {
                                        config.fps = 120;
                                    } else if ui::LIVE_NO_LIMIT.contains(x, y) {
                                        config.fps = 0;
                                    } else if ui::LIVE_SMOOTH.contains(x, y) {
                                        config.quality = Quality::Smooth;
                                    } else if ui::LIVE_SHARP.contains(x, y) {
                                        config.quality = Quality::Sharp;
                                    } else if ui::LIVE_RAW.contains(x, y) {
                                        config.compression = Compression::Raw;
                                        writer.set_encoding(encoding(config))?;
                                    } else if ui::LIVE_ZLIB.contains(x, y) {
                                        config.compression = Compression::Zlib;
                                        writer.set_encoding(encoding(config))?;
                                    } else if ui::LIVE_TIGHT.contains(x, y) {
                                        config.compression = Compression::Tight;
                                        writer.set_encoding(encoding(config))?;
                                    } else if let Some(level) = ui::live_quality_at(x, y)
                                        && config.compression == Compression::Tight
                                    {
                                        config.jpeg_quality = level;
                                        writer.set_encoding(encoding(config))?;
                                    } else if ui::LIVE_MOUSE_AUTO.contains(x, y) {
                                        config.relative_mouse = true;
                                        writer.set_relative_pointer_allowed(true)?;
                                    } else if ui::LIVE_MOUSE_OFF.contains(x, y) {
                                        config.relative_mouse = false;
                                        writer.set_relative_pointer_allowed(false)?;
                                    } else if ui::SCALE_SLIDER.contains(x, y) {
                                        dragging_ui_scale = true;
                                        config.ui_scale = ui::scale_from_slider_x(x);
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
        if let Ok(message) = present_error_rx.try_recv() {
            return Err(message.into());
        }
        if present.stopped() {
            return Err("the presenter thread stopped".into());
        }
        // Games that turn the camera with the mouse hide the host's cursor;
        // the host then asks for relative motion, and the pointer locks.
        let lock = config.relative_mouse && focused && !settings_open && writer.relative_pointer();
        if lock != locked {
            locked = lock;
            lock_pointer(&window, locked);
            motion = (0.0, 0.0);
            overlay_changed = true;
            redraw = true;
        }
        // Live throughput in the title bar shows whether the link or the
        // encoding limits the frame rate.
        if last_stats.0.elapsed() >= STATS_INTERVAL {
            let now = (Instant::now(), stats.snapshot());
            let mut status = format!(
                "{title} — {}",
                throughput(last_stats.1, now.1, now.0 - last_stats.0)
            );
            let (spent, presented) = std::mem::take(&mut *acquire(&shared.to_present));
            if presented > 0 {
                status += &format!(
                    " · {:.1} ms to present",
                    (spent / presented).as_secs_f64() * 1e3
                );
            }
            status += &center_status(center_lead.take()).unwrap_or_default();
            if locked {
                status += " · mouse locked, F8 releases";
            }
            window.set_title(&status);
            last_stats = now;
        }
        if !redraw || occluded || Instant::now() < retry_at {
            continue;
        }
        let logical = logical_size(&window);
        let scale = window.scale_factor();
        let physical = window.inner_size();
        if overlay
            .as_ref()
            .is_some_and(|overlay| overlay.size != logical)
        {
            overlay = None;
        }
        let overlay = overlay.get_or_insert_with(|| {
            overlay_changed = true;
            Overlay {
                pixels: vec![0; logical.0 * logical.1],
                size: logical,
                layer: Arc::new(acquire(&presenter).layer(logical.0 as u32, logical.1 as u32)),
            }
        });
        // The settings button hides while the pointer is locked, so it never
        // covers a game; F8 still opens the panel.
        let area = if settings_open {
            Some(ui::SETTINGS_PANEL)
        } else if locked {
            None
        } else {
            Some(ui::open_settings_box(config.ui_scale))
        }
        .and_then(|area| clip(area, logical));
        if overlay_changed && let Some(area) = area {
            ui::overlay(
                &mut Canvas::new(&mut overlay.pixels, logical.0, logical.1),
                config,
                settings_open,
            );
            overlay
                .layer
                .upload(&overlay.pixels, logical.0, area.x, area.y, area.w, area.h);
        }
        overlay_changed = false;
        let layout = FrameLayout {
            size: (physical.width, physical.height),
            draws: frame_layers(
                &image,
                config.quality == Quality::Smooth,
                area.map(|area| (&overlay.layer, area)),
                config.window_mode,
                remote,
                logical,
                scale,
            ),
        };
        let shown = layout.draw(&mut acquire(&presenter))?;
        // The presenter thread draws this layout for every update from now on.
        *acquire(&shared.layout) = Some(layout);
        if !shown {
            retry_at = Instant::now() + RETRY_INTERVAL;
            continue;
        }
        redraw = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn title_reports_how_early_centers_were_presented() {
        // Nothing is said while no update had its center shown early.
        assert_eq!(center_status((Duration::ZERO, 0)), None);
        // Three updates whose centers went up 8.1 ms before the rest, together.
        assert_eq!(
            center_status((Duration::from_micros(8_100), 3)),
            Some(" · center 2.7 ms early".to_string())
        );
        let lead = CenterLead::default();
        lead.add(Duration::from_millis(2));
        lead.add(Duration::from_millis(4));
        assert_eq!(lead.take(), (Duration::from_millis(6), 2));
        assert_eq!(lead.take(), (Duration::ZERO, 0));
    }

    #[test]
    fn frame_layers_scale_the_image_and_the_overlay_to_the_window() {
        // A 1920x1080 remote fitted to a 960x540 point window on a 2x
        // display: the image fills the 1920x1080 pixel surface.
        let overlay = Box2 {
            x: 10,
            y: 20,
            w: 100,
            h: 40,
        };
        let draws = frame_layers(
            &'i',
            true,
            Some((&'o', overlay)),
            WindowMode::Fit,
            (1920, 1080),
            (960, 540),
            2.0,
        );
        assert_eq!(draws.len(), 2);
        assert_eq!(draws[0].layer, 'i');
        assert_eq!(draws[0].target, [0.0, 0.0, 1920.0, 1080.0]);
        assert_eq!(draws[0].source, [0.0, 0.0, 1920.0, 1080.0]);
        assert!(draws[0].smooth);
        assert_eq!(draws[1].layer, 'o');
        assert_eq!(draws[1].target, [20.0, 40.0, 200.0, 80.0]);
        assert_eq!(draws[1].source, [10.0, 20.0, 100.0, 40.0]);
        // Whole-number scales sample the overlay without filtering.
        assert!(!draws[1].smooth);
        // A fractional scale filters it; no overlay leaves the image alone.
        let draws = frame_layers(
            &'i',
            false,
            Some((&'o', overlay)),
            WindowMode::Fit,
            (1920, 1080),
            (960, 540),
            1.5,
        );
        assert!(!draws[0].smooth && draws[1].smooth);
        let draws = frame_layers(&'i', true, None, WindowMode::Fit, (4, 2), (400, 400), 1.0);
        assert_eq!(draws.len(), 1);
        // Letterboxed: the image is centered with its aspect.
        assert_eq!(draws[0].target, [0.0, 100.0, 400.0, 200.0]);
        // Native size crops around the center instead of scaling.
        let draws = frame_layers(
            &'i',
            true,
            None,
            WindowMode::Native,
            (200, 100),
            (100, 100),
            2.0,
        );
        assert_eq!(draws[0].target, [0.0, 0.0, 200.0, 200.0]);
        assert_eq!(draws[0].source, [50.0, 0.0, 100.0, 100.0]);
    }

    #[test]
    fn arrival_keeps_the_oldest_time_and_wakes_a_waiter_to_stop() {
        let arrival = Arrival::default();
        assert_eq!(arrival.take(), None);
        arrival.mark();
        let first = arrival.take().unwrap();
        arrival.mark();
        let second = arrival.take().unwrap();
        assert!(second >= first);
        // A put-back arrival keeps the older of it and a newer one.
        arrival.mark();
        arrival.restore(Some(first));
        assert_eq!(arrival.take(), Some(first));
        // A waiter returns the arrival, and `None` after stop.
        let arrival = Arc::new(Arrival::default());
        let stop = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (arrival, stop) = (Arc::clone(&arrival), Arc::clone(&stop));
            thread::spawn(move || (arrival.wait(&stop), arrival.wait(&stop)))
        };
        thread::sleep(Duration::from_millis(20));
        arrival.mark();
        thread::sleep(Duration::from_millis(20));
        stop.store(true, Ordering::Release);
        arrival.wake();
        let (arrived, stopped) = waiter.join().unwrap();
        assert!(arrived.is_some());
        assert_eq!(stopped, None);
    }

    fn test_layout() -> FrameLayout {
        FrameLayout {
            size: (100, 50),
            draws: Vec::new(),
        }
    }

    /// Run the presenter loop on a thread with `draw`.
    fn run_presenter(
        shared: &Arc<PresentShared>,
        draw: impl FnMut(&FrameLayout) -> Result<bool, String> + Send + 'static,
    ) -> thread::JoinHandle<Result<(), String>> {
        let shared = Arc::clone(shared);
        thread::spawn(move || present_frames(&shared, draw))
    }

    fn stop_presenter(shared: &PresentShared) {
        shared.stop.store(true, Ordering::Release);
        shared.arrival.wake();
    }

    fn wait_for(condition: impl Fn() -> bool) -> bool {
        for _ in 0..500 {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(2));
        }
        false
    }

    /// Instants at which a presenter loop drew, for `run_presenter`.
    fn drawn_at(
        times: &Arc<Mutex<Vec<Instant>>>,
        result: bool,
    ) -> impl FnMut(&FrameLayout) -> Result<bool, String> + Send + 'static {
        let times = Arc::clone(times);
        move |_| {
            times.lock().unwrap().push(Instant::now());
            Ok(result)
        }
    }

    #[test]
    fn presenter_shows_each_upload_at_once_without_a_limit() {
        let shared = Arc::new(PresentShared::new(0));
        *acquire(&shared.layout) = Some(test_layout());
        let times = Arc::new(Mutex::new(Vec::new()));
        let handle = run_presenter(&shared, drawn_at(&times, true));
        // The center of an update, then the rest of it 4 ms later: each one
        // is shown at once, not at a frame boundary.
        shared.arrival.mark();
        assert!(wait_for(|| times.lock().unwrap().len() == 1));
        thread::sleep(Duration::from_millis(4));
        let marked = Instant::now();
        shared.arrival.mark();
        assert!(wait_for(|| times.lock().unwrap().len() == 2));
        let second = times.lock().unwrap()[1];
        assert!(second.duration_since(marked) < Duration::from_millis(10));
        assert_eq!(acquire(&shared.to_present).1, 2);
        stop_presenter(&shared);
        assert_eq!(handle.join().unwrap(), Ok(()));
    }

    #[test]
    fn presenter_keeps_to_the_frame_limit_and_merges_updates() {
        // 50 fps: one frame per 20 ms.
        let shared = Arc::new(PresentShared::new(50));
        *acquire(&shared.layout) = Some(test_layout());
        let times = Arc::new(Mutex::new(Vec::new()));
        let handle = run_presenter(&shared, drawn_at(&times, true));
        shared.arrival.mark();
        assert!(wait_for(|| times.lock().unwrap().len() == 1));
        // Five uploads inside the first interval make one more frame.
        for _ in 0..5 {
            shared.arrival.mark();
            thread::sleep(Duration::from_millis(2));
        }
        thread::sleep(Duration::from_millis(60));
        let times = times.lock().unwrap().clone();
        assert_eq!(times.len(), 2, "{times:?}");
        assert!(times[1].duration_since(times[0]) >= Duration::from_millis(19));
        stop_presenter(&shared);
        assert_eq!(handle.join().unwrap(), Ok(()));
    }

    #[test]
    fn presenter_tries_again_after_a_frame_the_window_could_not_take() {
        let shared = Arc::new(PresentShared::new(0));
        *acquire(&shared.layout) = Some(test_layout());
        let times = Arc::new(Mutex::new(Vec::new()));
        let handle = {
            let times = Arc::clone(&times);
            run_presenter(&shared, move |_| {
                let mut times = times.lock().unwrap();
                times.push(Instant::now());
                // The window cannot take the first frame.
                Ok(times.len() > 1)
            })
        };
        shared.arrival.mark();
        assert!(wait_for(|| times.lock().unwrap().len() == 2));
        let times = times.lock().unwrap().clone();
        assert!(times[1].duration_since(times[0]) >= RETRY_INTERVAL);
        // Only the frame that was shown counts toward the statistic, and it
        // is timed from the upload.
        assert!(wait_for(|| acquire(&shared.to_present).1 == 1));
        assert!(acquire(&shared.to_present).0 >= RETRY_INTERVAL);
        stop_presenter(&shared);
        assert_eq!(handle.join().unwrap(), Ok(()));
    }

    #[test]
    fn presenter_waits_for_a_layout_and_for_the_window_to_be_visible() {
        let shared = Arc::new(PresentShared::new(0));
        let times = Arc::new(Mutex::new(Vec::new()));
        let handle = run_presenter(&shared, drawn_at(&times, true));
        // Nothing to draw yet.
        shared.arrival.mark();
        thread::sleep(Duration::from_millis(50));
        assert!(times.lock().unwrap().is_empty());
        // A covered window gets nothing either.
        shared.occluded.store(true, Ordering::Relaxed);
        *acquire(&shared.layout) = Some(test_layout());
        thread::sleep(Duration::from_millis(50));
        assert!(times.lock().unwrap().is_empty());
        // Once visible, the update that waited is shown.
        shared.occluded.store(false, Ordering::Relaxed);
        assert!(wait_for(|| times.lock().unwrap().len() == 1));
        stop_presenter(&shared);
        assert_eq!(handle.join().unwrap(), Ok(()));
    }

    #[test]
    fn presenter_stops_on_a_drawing_error() {
        let shared = Arc::new(PresentShared::new(0));
        *acquire(&shared.layout) = Some(test_layout());
        let handle = run_presenter(&shared, |_| Err("no surface".to_string()));
        shared.arrival.mark();
        assert_eq!(handle.join().unwrap(), Err("no surface".to_string()));
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

    #[test]
    fn hosted_server_status_follows_notices_in_order() {
        let mut server = HostedServer::default();
        let address = "0.0.0.0:5900".parse().unwrap();
        let serving = |width, height| ServerNotice::Serving {
            address,
            display: "DISPLAY1".into(),
            width,
            height,
        };
        // Notices for a server that is not starting are ignored.
        server.apply(serving(1920, 1080));
        assert_eq!(server.view.phase, ServerPhase::Stopped);
        server.view.phase = ServerPhase::Starting;
        server.authenticated = true;
        server.apply(serving(1920, 1080));
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
                address: "0.0.0.0:5900".into(),
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

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn hosted_server_reports_unsupported_platforms_and_stops() {
        let mut server = HostedServer::default();
        server.start(ServeRequest {
            address: "127.0.0.1:0".into(),
            display: None,
            password: Some("secret".into()),
            allow_insecure: false,
            scale: 1.0,
            foveation: Foveation::Auto,
        });
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
