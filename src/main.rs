use minifb::{InputCallback, Key, MouseButton, MouseMode, ScaleMode, Window, WindowOptions};
use std::collections::HashMap;
use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use topvnc::{Encoding, Framebuffer, Session, StatsSnapshot, encoding_name};

// Each host backend uses part of the shared logic; tests cover all of it.
#[allow(dead_code)]
mod desktop_host;
#[cfg(target_os = "macos")]
mod macos_server;
#[cfg(windows)]
mod windows_server;

mod settings;
mod ui;
use desktop_host::{HostPermissions, ServeOptions, ServerNotice};
use ui::{
    Box2, Canvas, Compression, Config, Field, Quality, ServeRequest, ServerPhase, ServerView, Tab,
    UiState, WindowMode,
};

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

#[derive(Clone, Copy)]
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

#[derive(Clone, Copy)]
struct SampleTap {
    lo: usize,
    hi: usize,
    weight: u32,
}

fn sample_taps(source: usize, target: usize) -> Vec<SampleTap> {
    (0..target)
        .map(|position| {
            let source_position = ((position as f64 + 0.5) * source as f64 / target as f64 - 0.5)
                .clamp(0.0, (source - 1) as f64);
            let lo = source_position.floor() as usize;
            SampleTap {
                lo,
                hi: (lo + 1).min(source - 1),
                weight: ((source_position - lo as f64) * 256.0).round() as u32,
            }
        })
        .collect()
}

/// Blend two 0x00RRGGBB pixels; `weight` (0..=256) is `b`'s share. Red and
/// blue are blended together in one multiply, green in another.
#[inline]
fn lerp(a: u32, b: u32, weight: u32) -> u32 {
    let inverse = 256 - weight;
    let red_blue = ((a & 0xff00ff) * inverse + (b & 0xff00ff) * weight + 0x800080) >> 8;
    let green = ((a & 0x00ff00) * inverse + (b & 0x00ff00) * weight + 0x008000) >> 8;
    (red_blue & 0xff00ff) | (green & 0x00ff00)
}

#[inline]
fn bilinear(a: u32, b: u32, c: u32, d: u32, x_weight: u32, y_weight: u32) -> u32 {
    lerp(lerp(a, b, x_weight), lerp(c, d, x_weight), y_weight)
}

struct ScaledFrame {
    window: (usize, usize),
    draw: Rect,
    pixels: Vec<u32>,
    x_taps: Vec<SampleTap>,
    y_taps: Vec<SampleTap>,
    /// Columns map one-to-one onto consecutive source columns, so rows can
    /// be copied instead of sampled.
    copy_rows: bool,
    quality: Quality,
    mode: WindowMode,
}

impl ScaledFrame {
    #[cfg(test)]
    fn new(remote: (usize, usize), window: (usize, usize)) -> Result<Self, Box<dyn Error>> {
        Self::with_settings(remote, window, WindowMode::Fit, Quality::Smooth)
    }

    fn with_settings(
        remote: (usize, usize),
        window: (usize, usize),
        mode: WindowMode,
        quality: Quality,
    ) -> Result<Self, Box<dyn Error>> {
        let count = window
            .0
            .checked_mul(window.1)
            .ok_or("window is too large")?;
        if window.0 > 8192 || window.1 > 8192 || count > 33_554_432 {
            return Err("window is too large".into());
        }
        let draw = if mode == WindowMode::Native {
            let width = remote.0.min(window.0);
            let height = remote.1.min(window.1);
            Rect {
                x0: (window.0 - width) / 2,
                y0: (window.1 - height) / 2,
                x1: (window.0 + width) / 2,
                y1: (window.1 + height) / 2,
            }
        } else {
            draw_rect(remote, window).ok_or("window has no drawable area")?
        };
        let (x_taps, y_taps) = if mode == WindowMode::Native {
            let x_offset = (remote.0 - (draw.x1 - draw.x0)) / 2;
            let y_offset = (remote.1 - (draw.y1 - draw.y0)) / 2;
            (
                (0..draw.x1 - draw.x0)
                    .map(|x| SampleTap {
                        lo: x + x_offset,
                        hi: x + x_offset,
                        weight: 0,
                    })
                    .collect(),
                (0..draw.y1 - draw.y0)
                    .map(|y| SampleTap {
                        lo: y + y_offset,
                        hi: y + y_offset,
                        weight: 0,
                    })
                    .collect(),
            )
        } else {
            (
                sample_taps(remote.0, draw.x1 - draw.x0),
                sample_taps(remote.1, draw.y1 - draw.y0),
            )
        };
        let unscaled = |taps: &[SampleTap]| {
            // With a zero weight only `lo` contributes to a sample.
            taps.iter().all(|tap| tap.weight == 0)
                && taps.windows(2).all(|pair| pair[1].lo == pair[0].lo + 1)
        };
        let copy_rows = unscaled(&x_taps) && unscaled(&y_taps);
        Ok(Self {
            window,
            draw,
            pixels: vec![0; count],
            x_taps,
            y_taps,
            copy_rows,
            quality,
            mode,
        })
    }

    fn update(&mut self, source: &Framebuffer, dirty: Rect) {
        let x_range = self
            .x_taps
            .iter()
            .position(|tap| tap.hi >= dirty.x0 && tap.lo < dirty.x1)
            .zip(
                self.x_taps
                    .iter()
                    .rposition(|tap| tap.hi >= dirty.x0 && tap.lo < dirty.x1),
            );
        let y_range = self
            .y_taps
            .iter()
            .position(|tap| tap.hi >= dirty.y0 && tap.lo < dirty.y1)
            .zip(
                self.y_taps
                    .iter()
                    .rposition(|tap| tap.hi >= dirty.y0 && tap.lo < dirty.y1),
            );
        let (Some((x_start, x_end)), Some((y_start, y_end))) = (x_range, y_range) else {
            return;
        };
        let source_width = source.width();
        let source_pixels = source.pixels();
        if self.copy_rows {
            let length = x_end - x_start + 1;
            let source_x = self.x_taps[x_start].lo;
            for y in y_start..=y_end {
                let target = (self.draw.y0 + y) * self.window.0 + self.draw.x0 + x_start;
                let source = self.y_taps[y].lo * source_width + source_x;
                self.pixels[target..target + length]
                    .copy_from_slice(&source_pixels[source..source + length]);
            }
            return;
        }
        for y in y_start..=y_end {
            let y_tap = self.y_taps[y];
            let target_row = (self.draw.y0 + y) * self.window.0 + self.draw.x0;
            let top_row = y_tap.lo * source_width;
            let bottom_row = y_tap.hi * source_width;
            for x in x_start..=x_end {
                let x_tap = self.x_taps[x];
                self.pixels[target_row + x] = if self.quality == Quality::Sharp {
                    let sx = if x_tap.weight >= 128 {
                        x_tap.hi
                    } else {
                        x_tap.lo
                    };
                    let sy = if y_tap.weight >= 128 {
                        y_tap.hi
                    } else {
                        y_tap.lo
                    };
                    source_pixels[sy * source_width + sx]
                } else {
                    bilinear(
                        source_pixels[top_row + x_tap.lo],
                        source_pixels[top_row + x_tap.hi],
                        source_pixels[bottom_row + x_tap.lo],
                        source_pixels[bottom_row + x_tap.hi],
                        x_tap.weight,
                        y_tap.weight,
                    )
                };
            }
        }
    }
}

struct FrameState {
    framebuffer: Framebuffer,
    /// Area changed by fully received updates and not yet presented.
    dirty: Option<Rect>,
}

/// Frame rate, size, bandwidth, and encoding between two stats snapshots.
fn throughput(before: StatsSnapshot, after: StatsSnapshot, elapsed: std::time::Duration) -> String {
    let seconds = elapsed.as_secs_f64().max(1e-3);
    let frames = after.frames.saturating_sub(before.frames);
    let bytes = after.bytes.saturating_sub(before.bytes) as f64;
    format!(
        "{:.0} fps · {:.0} KB/frame · {:.0} Mbit/s · {}",
        frames as f64 / seconds,
        bytes / frames.max(1) as f64 / 1000.0,
        bytes * 8.0 / seconds / 1e6,
        after.encoding.map_or("waiting", encoding_name)
    )
}

/// Copy `area` of `source` into the same place in `target`.
fn copy_area(source: &Framebuffer, target: &mut Framebuffer, area: Rect) {
    let width = source.width();
    let source = source.pixels();
    let target = target.pixels_mut();
    for row in area.y0..area.y1 {
        let range = row * width + area.x0..row * width + area.x1;
        target[range.clone()].copy_from_slice(&source[range]);
    }
}

/// Map a mouse position in an aspect-fitted window to remote framebuffer pixels.
fn remote_pointer(
    x: f32,
    y: f32,
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
    let rx = ((x - draw.x0 as f32) * (remote.0 - 1) as f32
        / (draw.x1 - draw.x0).saturating_sub(1).max(1) as f32)
        .floor()
        .clamp(0.0, (remote.0 - 1) as f32) as u16;
    let ry = ((y - draw.y0 as f32) * (remote.1 - 1) as f32
        / (draw.y1 - draw.y0).saturating_sub(1).max(1) as f32)
        .floor()
        .clamp(0.0, (remote.1 - 1) as f32) as u16;
    Some((rx, ry))
}

fn keysym(key: Key, shift: bool) -> Option<u32> {
    use Key::*;
    Some(match key {
        key if (A as u32..=Z as u32).contains(&(key as u32)) => {
            (if shift { 0x41 } else { 0x61 }) + (key as u32 - A as u32)
        }
        key if (Key0 as u32..=Key9 as u32).contains(&(key as u32)) => {
            if shift {
                b")!@#$%^&*("[(key as u32 - Key0 as u32) as usize] as u32
            } else {
                0x30 + (key as u32 - Key0 as u32)
            }
        }
        key if (NumPad0 as u32..=NumPad9 as u32).contains(&(key as u32)) => {
            0x30 + (key as u32 - NumPad0 as u32)
        }
        NumPadDot => 0x2e,
        NumPadSlash => 0x2f,
        NumPadAsterisk => 0x2a,
        NumPadMinus => 0x2d,
        NumPadPlus => 0x2b,
        Space => 0x20,
        Apostrophe => {
            if shift {
                0x22
            } else {
                0x27
            }
        }
        Backquote => {
            if shift {
                0x7e
            } else {
                0x60
            }
        }
        Backslash => {
            if shift {
                0x7c
            } else {
                0x5c
            }
        }
        Comma => {
            if shift {
                0x3c
            } else {
                0x2c
            }
        }
        Equal => {
            if shift {
                0x2b
            } else {
                0x3d
            }
        }
        LeftBracket => {
            if shift {
                0x7b
            } else {
                0x5b
            }
        }
        Minus => {
            if shift {
                0x5f
            } else {
                0x2d
            }
        }
        Period => {
            if shift {
                0x3e
            } else {
                0x2e
            }
        }
        RightBracket => {
            if shift {
                0x7d
            } else {
                0x5d
            }
        }
        Semicolon => {
            if shift {
                0x3a
            } else {
                0x3b
            }
        }
        Slash => {
            if shift {
                0x3f
            } else {
                0x2f
            }
        }
        Enter | NumPadEnter => 0xff0d,
        Tab => 0xff09,
        Backspace => 0xff08,
        Escape => 0xff1b,
        Delete => 0xffff,
        Insert => 0xff63,
        Home => 0xff50,
        End => 0xff57,
        PageUp => 0xff55,
        PageDown => 0xff56,
        Left => 0xff51,
        Up => 0xff52,
        Right => 0xff53,
        Down => 0xff54,
        LeftShift => 0xffe1,
        RightShift => 0xffe2,
        LeftCtrl => 0xffe3,
        RightCtrl => 0xffe4,
        LeftAlt => 0xffe9,
        RightAlt => 0xffea,
        LeftSuper => 0xffeb,
        RightSuper => 0xffec,
        key if (F1 as u32..=F12 as u32).contains(&(key as u32)) => {
            0xffbe + (key as u32 - F1 as u32)
        }
        _ => return None,
    })
}

enum InputEvent {
    Character(char),
    Key(Key, bool),
}

struct KeyEvents(mpsc::Sender<InputEvent>);

impl InputCallback for KeyEvents {
    fn add_char(&mut self, uni_char: u32) {
        if let Some(character) = char::from_u32(uni_char) {
            let _ = self.0.send(InputEvent::Character(character));
        }
    }

    fn set_key_state(&mut self, key: Key, down: bool) {
        let _ = self.0.send(InputEvent::Key(key, down));
    }
}

fn translated_key_event(
    pressed_keys: &mut HashMap<Key, u32>,
    key: Key,
    down: bool,
) -> Option<(u32, bool)> {
    if down {
        if pressed_keys.contains_key(&key) {
            return None;
        }
        let shift = pressed_keys.contains_key(&Key::LeftShift)
            || pressed_keys.contains_key(&Key::RightShift);
        let symbol = keysym(key, shift)?;
        pressed_keys.insert(key, symbol);
        Some((symbol, true))
    } else {
        pressed_keys.remove(&key).map(|symbol| (symbol, false))
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

    let mut connection_error = None;
    // Lives across viewer sessions; dropping it stops the server.
    let mut server = HostedServer::default();
    loop {
        let Some((next_config, session)) =
            show_landing(config, connection_error.take(), &mut server)?
        else {
            return Ok(());
        };
        config = next_config;
        connection_error = match run_session(session, &mut config, input_debug) {
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
    mut config: Config,
    error: Option<String>,
    server: &mut HostedServer,
) -> Result<Option<(Config, Session)>, Box<dyn Error>> {
    let mut window = Window::new(
        "TopVNC",
        800,
        640,
        WindowOptions {
            resize: false,
            ..WindowOptions::default()
        },
    )?;
    window.set_target_fps(60);
    let mut pixels = vec![ui::BG; 800 * 640];
    let mut state = UiState::default();
    state.error = error;
    let (input_tx, input_rx) = mpsc::channel();
    window.set_input_callback(Box::new(KeyEvents(input_tx)));
    let (result_tx, result_rx) = mpsc::channel::<Result<Session, String>>();
    let mut connecting = false;
    let mut permissions_checked: Option<std::time::Instant> = None;
    let mut dragging_serve_scale = false;
    while window.is_open() {
        // Permissions can change in System Settings while the window is open.
        if server.view.phase == ServerPhase::Stopped
            && permissions_checked.is_none_or(|checked| checked.elapsed().as_secs() >= 1)
        {
            server.view.permissions = host_permissions();
            permissions_checked = Some(std::time::Instant::now());
        }
        if let Some(error) = server.poll() {
            state.switch_tab(Tab::Server);
            state.error = Some(error);
        }
        ui::landing(
            &mut Canvas::new(&mut pixels, 800, 640),
            &config,
            &state,
            connecting,
            &server.view,
        );
        window.update_with_buffer(&pixels, 800, 640)?;
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
        for event in input_rx.try_iter() {
            match event {
                InputEvent::Character(character) if !connecting => {
                    state.character(&mut config, character)
                }
                InputEvent::Key(Key::Enter, true) if !connecting => match state.tab {
                    Tab::Connect => connect_clicked = true,
                    // Enter only starts the server; stopping takes a click.
                    Tab::Server => {
                        server_clicked = server.view.phase == ServerPhase::Stopped;
                    }
                },
                InputEvent::Key(Key::Escape, true) => return Ok(None),
                InputEvent::Key(key, true) if !connecting => {
                    state.key(&mut config, key);
                }
                _ => {}
            }
        }
        let (mx, my) = window
            .get_unscaled_mouse_pos(MouseMode::Clamp)
            .unwrap_or((-1.0, -1.0));
        let click = state.click(window.get_mouse_down(MouseButton::Left)) && !connecting;
        let (x, y) = (mx as usize, my as usize);
        if click && ui::TAB_CONNECT.contains(x, y) {
            state.switch_tab(Tab::Connect);
        } else if click && ui::TAB_SERVER.contains(x, y) {
            state.switch_tab(Tab::Server);
        } else if click && state.tab == Tab::Server {
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
            }
            if editable && ui::SERVE_FULL_SIZE.contains(x, y) {
                serve.scale = 1.0;
            }
            if editable && ui::SERVE_HALF_SIZE.contains(x, y) {
                serve.scale = 0.5;
            }
            if ui::CONNECT.contains(x, y) {
                server_clicked = true;
            }
        } else if click {
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
            if ui::FPS30.contains(x, y) {
                config.fps = 30;
            }
            if ui::FPS60.contains(x, y) {
                config.fps = 60;
            }
            if ui::FPS120.contains(x, y) {
                config.fps = 120;
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
        // The served-size slider follows the mouse until the button is
        // released; it is locked while the server runs.
        if dragging_serve_scale {
            if window.get_mouse_down(MouseButton::Left)
                && state.tab == Tab::Server
                && server.view.phase == ServerPhase::Stopped
            {
                config.serve.scale = ui::serve_scale_from_slider_x(x);
            } else {
                dragging_serve_scale = false;
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
    Ok(None)
}

fn pointer_for_mode(
    x: f32,
    y: f32,
    window: (usize, usize),
    remote: (usize, usize),
    scaled: &ScaledFrame,
) -> Option<(u16, u16)> {
    if scaled.mode != WindowMode::Native {
        return remote_pointer(x, y, window, remote);
    }
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let offset_x = (remote.0 - (scaled.draw.x1 - scaled.draw.x0)) / 2;
    let offset_y = (remote.1 - (scaled.draw.y1 - scaled.draw.y0)) / 2;
    let rx = (x.floor() as isize - scaled.draw.x0 as isize + offset_x as isize)
        .clamp(0, remote.0 as isize - 1) as u16;
    let ry = (y.floor() as isize - scaled.draw.y0 as isize + offset_y as isize)
        .clamp(0, remote.1 as isize - 1) as u16;
    Some((rx, ry))
}

fn backup_region(pixels: &[u32], width: usize, height: usize, area: Box2) -> Vec<u32> {
    let mut backup = Vec::with_capacity(area.w.min(width) * area.h.min(height));
    for y in area.y..(area.y + area.h).min(height) {
        for x in area.x..(area.x + area.w).min(width) {
            backup.push(pixels[y * width + x]);
        }
    }
    backup
}

fn restore_region(pixels: &mut [u32], width: usize, height: usize, area: Box2, backup: &[u32]) {
    let mut index = 0;
    for y in area.y..(area.y + area.h).min(height) {
        for x in area.x..(area.x + area.w).min(width) {
            pixels[y * width + x] = backup[index];
            index += 1;
        }
    }
}

fn run_session(
    session: Session,
    config: &mut Config,
    input_debug: bool,
) -> Result<Option<String>, Box<dyn Error>> {
    let writer = session.writer();
    let result = run_session_inner(session, config, input_debug);
    let _ = writer.shutdown();
    result
}

fn run_session_inner(
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
    let stats = session.stats();
    let framebuffer = Arc::new(Mutex::new(FrameState {
        framebuffer: Framebuffer::new(info.width, info.height)?,
        dirty: None,
    }));
    let worker_framebuffer = Arc::clone(&framebuffer);
    let worker_writer = writer.clone();
    let (error_tx, error_rx) = mpsc::channel();
    thread::spawn(move || {
        let result: std::io::Result<()> = (|| {
            let mut scratch = Vec::new();
            // Each update requests the next one as soon as it starts arriving,
            // so the server never waits a round trip between frames.
            worker_writer.request_update(false, info.width, info.height)?;
            loop {
                let mut received: Option<Rect> = None;
                // Rectangles are decoded before the lock is taken; the lock
                // only covers copying pixels into the shared framebuffer.
                session.read_update_pipelined(&mut scratch, |x, y, width, height, bytes| {
                    worker_framebuffer
                        .lock()
                        .unwrap()
                        .framebuffer
                        .apply_raw(x, y, width, height, bytes)?;
                    let rect = Rect {
                        x0: usize::from(x),
                        y0: usize::from(y),
                        x1: usize::from(x) + usize::from(width),
                        y1: usize::from(y) + usize::from(height),
                    };
                    received = Some(received.map_or(rect, |area| area.union(rect)));
                    Ok(())
                })?;
                // Present an update only once all of it has arrived.
                if let Some(rect) = received {
                    let mut frame = worker_framebuffer.lock().unwrap();
                    frame.dirty = Some(frame.dirty.map_or(rect, |dirty| dirty.union(rect)));
                }
            }
        })();
        let _ = error_tx.send(result);
    });

    let remote = (usize::from(info.width), usize::from(info.height));
    let initial = match config.window_mode {
        WindowMode::Fit => fit_dimensions(remote, display_space()),
        WindowMode::Native => remote,
        WindowMode::Custom => config.custom_size()?,
    };
    let title = format!("TopVNC — {}", info.name);
    let mut window = Window::new(
        &title,
        initial.0,
        initial.1,
        WindowOptions {
            resize: true,
            scale_mode: ScaleMode::Stretch,
            ..WindowOptions::default()
        },
    )?;
    window.set_target_fps(config.fps);
    let mut scaled = ScaledFrame::with_settings(
        remote,
        window.get_size(),
        config.window_mode,
        config.quality,
    )?;
    // The window thread scales from its own copy, so the network thread never
    // waits for scaling.
    let mut presented = Framebuffer::new(info.width, info.height)?;
    let (input_tx, input_rx) = mpsc::channel();
    window.set_input_callback(Box::new(KeyEvents(input_tx)));
    let mut pressed_keys = HashMap::new();
    let mut last_pointer = None;
    let mut ui_state = UiState::default();
    let mut settings_open = false;
    let mut dragging_ui_scale = false;
    let mut ui_captured_mouse = false;
    let mut redraw = false;
    let mut last_stats = (std::time::Instant::now(), stats.snapshot());
    while window.is_open() {
        // Live throughput in the title bar shows whether the link or the
        // encoding limits the frame rate.
        if last_stats.0.elapsed() >= std::time::Duration::from_secs(1) {
            let now = (std::time::Instant::now(), stats.snapshot());
            window.set_title(&format!(
                "{title} — {}",
                throughput(last_stats.1, now.1, now.0 - last_stats.0)
            ));
            last_stats = now;
        }
        if let Ok(Err(error)) = error_rx.try_recv() {
            writer.shutdown()?;
            return Ok(Some(error.to_string()));
        }
        let size = window.get_size();
        if size.0 == 0 || size.1 == 0 {
            window.update();
            continue;
        }
        let full_redraw = redraw
            || size != scaled.window
            || config.quality != scaled.quality
            || config.window_mode != scaled.mode;
        if full_redraw {
            scaled = ScaledFrame::with_settings(remote, size, config.window_mode, config.quality)?;
            redraw = false;
        }
        let dirty = {
            let mut frame = framebuffer.lock().unwrap();
            if let Some(dirty) = frame.dirty.take() {
                copy_area(&frame.framebuffer, &mut presented, dirty);
                Some(dirty)
            } else {
                None
            }
        };
        let dirty = if full_redraw {
            Some(Rect {
                x0: 0,
                y0: 0,
                x1: remote.0,
                y1: remote.1,
            })
        } else {
            dirty
        };
        if let Some(dirty) = dirty {
            scaled.update(&presented, dirty);
        }
        let area = if settings_open {
            ui::SETTINGS_PANEL
        } else {
            ui::open_settings_box(config.ui_scale)
        };
        let backup = backup_region(&scaled.pixels, size.0, size.1, area);
        ui::overlay(
            &mut Canvas::new(&mut scaled.pixels, size.0, size.1),
            config,
            settings_open,
        );
        let update_result = window.update_with_buffer(&scaled.pixels, size.0, size.1);
        restore_region(&mut scaled.pixels, size.0, size.1, area, &backup);
        update_result?;

        for event in input_rx.try_iter() {
            if let InputEvent::Key(Key::F8, true) = event {
                settings_open = !settings_open;
                dragging_ui_scale = false;
                ui_captured_mouse = true;
                for (_, symbol) in pressed_keys.drain() {
                    writer.key(symbol, false)?;
                }
                if let Some((_, x, y)) = last_pointer.take() {
                    writer.pointer(0, x, y)?;
                }
                continue;
            }
            if settings_open {
                continue;
            }
            if let InputEvent::Key(key, down) = event {
                if key == Key::F8 {
                    continue;
                }
                if let Some((symbol, down)) = translated_key_event(&mut pressed_keys, key, down) {
                    if input_debug {
                        eprintln!(
                            "key {}: {key:?} -> {symbol:#x}",
                            if down { "down" } else { "up" }
                        );
                    }
                    writer.key(symbol, down)?;
                }
            }
        }
        let mouse = window
            .get_unscaled_mouse_pos(MouseMode::Clamp)
            .unwrap_or((-1.0, -1.0));
        let click = ui_state.click(window.get_mouse_down(MouseButton::Left));
        let (mx, my) = (mouse.0 as usize, mouse.1 as usize);
        let was_settings_open = settings_open;
        if click {
            if !settings_open && ui::open_settings_box(config.ui_scale).contains(mx, my) {
                settings_open = true;
                ui_captured_mouse = true;
                for (_, symbol) in pressed_keys.drain() {
                    writer.key(symbol, false)?;
                }
                if let Some((_, x, y)) = last_pointer.take() {
                    writer.pointer(0, x, y)?;
                }
            } else if settings_open {
                ui_captured_mouse = true;
                if ui::CLOSE_SETTINGS.contains(mx, my) {
                    settings_open = false;
                    dragging_ui_scale = false;
                } else if ui::LIVE_FIT.contains(mx, my) {
                    config.window_mode = WindowMode::Fit;
                } else if ui::LIVE_NATIVE.contains(mx, my) {
                    config.window_mode = WindowMode::Native;
                } else if ui::LIVE_30.contains(mx, my) {
                    config.fps = 30;
                } else if ui::LIVE_60.contains(mx, my) {
                    config.fps = 60;
                } else if ui::LIVE_120.contains(mx, my) {
                    config.fps = 120;
                } else if ui::LIVE_SMOOTH.contains(mx, my) {
                    config.quality = Quality::Smooth;
                } else if ui::LIVE_SHARP.contains(mx, my) {
                    config.quality = Quality::Sharp;
                } else if ui::SCALE_SLIDER.contains(mx, my) {
                    dragging_ui_scale = true;
                    config.ui_scale = ui::scale_from_slider_x(mx);
                } else if ui::DISCONNECT.contains(mx, my) {
                    writer.shutdown()?;
                    return Ok(None);
                }
                window.set_target_fps(config.fps);
            }
        }
        if dragging_ui_scale {
            if window.get_mouse_down(MouseButton::Left) && settings_open {
                config.ui_scale = ui::scale_from_slider_x(mx);
            } else {
                dragging_ui_scale = false;
            }
        }
        let suppress_pointer = was_settings_open
            || settings_open
            || ui_captured_mouse
            || ui::open_settings_box(config.ui_scale).contains(mx, my);
        if !window.get_mouse_down(MouseButton::Left) {
            ui_captured_mouse = false;
        }
        if suppress_pointer {
            if let Some((_, x, y)) = last_pointer.take() {
                writer.pointer(0, x, y)?;
            }
            continue;
        }
        if let Some((x, y)) = window.get_unscaled_mouse_pos(MouseMode::Clamp)
            && let Some((remote_x, remote_y)) = pointer_for_mode(x, y, size, remote, &scaled)
        {
            let buttons = u8::from(window.get_mouse_down(MouseButton::Left))
                | (u8::from(window.get_mouse_down(MouseButton::Middle)) << 1)
                | (u8::from(window.get_mouse_down(MouseButton::Right)) << 2);
            let pointer = (buttons, remote_x, remote_y);
            if last_pointer != Some(pointer) {
                if input_debug {
                    eprintln!("pointer: buttons={buttons} remote=({remote_x}, {remote_y})");
                }
                writer.pointer(buttons, remote_x, remote_y)?;
                last_pointer = Some(pointer);
            }
            if let Some((_, wheel_y)) = window.get_scroll_wheel() {
                let wheel_button = if wheel_y > 0.0 {
                    8
                } else if wheel_y < 0.0 {
                    16
                } else {
                    0
                };
                if wheel_button != 0 {
                    writer.pointer(buttons | wheel_button, remote_x, remote_y)?;
                    writer.pointer(buttons, remote_x, remote_y)?;
                }
            }
        }
    }
    writer.shutdown()?;
    Ok(None)
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
        assert_eq!(keysym(Key::T, false), Some('t' as u32));
        assert_eq!(keysym(Key::T, true), Some('T' as u32));
        assert_eq!(keysym(Key::Key1, false), Some('1' as u32));
        assert_eq!(keysym(Key::Key1, true), Some('!' as u32));
        assert_eq!(keysym(Key::NumPad1, false), Some('1' as u32));
        assert_eq!(keysym(Key::NumPad1, true), Some('1' as u32));
        assert_eq!(keysym(Key::Enter, true), Some(0xff0d));
    }

    #[test]
    fn key_events_preserve_press_release_and_shift_state() {
        let mut held = HashMap::new();
        assert_eq!(
            translated_key_event(&mut held, Key::LeftShift, true),
            Some((0xffe1, true))
        );
        assert_eq!(
            translated_key_event(&mut held, Key::T, true),
            Some(('T' as u32, true))
        );
        assert_eq!(translated_key_event(&mut held, Key::T, true), None);
        assert_eq!(
            translated_key_event(&mut held, Key::LeftShift, false),
            Some((0xffe1, false))
        );
        assert_eq!(
            translated_key_event(&mut held, Key::T, false),
            Some(('T' as u32, false))
        );
        assert_eq!(
            translated_key_event(&mut held, Key::Enter, true),
            Some((0xff0d, true))
        );
        assert_eq!(
            translated_key_event(&mut held, Key::Enter, false),
            Some((0xff0d, false))
        );
    }

    #[test]
    fn downscale_blends_source_pixels_instead_of_dropping_them() {
        let mut source = Framebuffer::new(2, 2).unwrap();
        source
            .apply_raw(
                0,
                0,
                2,
                2,
                &[
                    0, 0, 255, 0, // Red.
                    0, 255, 0, 0, // Green.
                    255, 0, 0, 0, // Blue.
                    255, 255, 255, 0, // White.
                ],
            )
            .unwrap();
        let mut scaled = ScaledFrame::new((2, 2), (1, 1)).unwrap();
        scaled.update(
            &source,
            Rect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 2,
            },
        );
        assert_eq!(scaled.pixels, vec![0x808080]);
    }

    #[test]
    fn partial_rescale_matches_full_rescale() {
        let mut source = Framebuffer::new(4, 4).unwrap();
        let full = Rect {
            x0: 0,
            y0: 0,
            x1: 4,
            y1: 4,
        };
        let mut partial = ScaledFrame::new((4, 4), (6, 6)).unwrap();
        partial.update(&source, full);
        source.apply_raw(1, 1, 1, 1, &[0, 0, 255, 0]).unwrap();
        partial.update(
            &source,
            Rect {
                x0: 1,
                y0: 1,
                x1: 2,
                y1: 2,
            },
        );
        let mut complete = ScaledFrame::new((4, 4), (6, 6)).unwrap();
        complete.update(&source, full);
        assert_eq!(partial.pixels, complete.pixels);
    }

    #[test]
    fn unscaled_frames_copy_rows_and_match_sampling() {
        let mut source = Framebuffer::new(5, 3).unwrap();
        for (index, pixel) in source.pixels_mut().iter_mut().enumerate() {
            *pixel = index as u32 * 0x010203;
        }
        let full = Rect {
            x0: 0,
            y0: 0,
            x1: 5,
            y1: 3,
        };
        let mut copied = ScaledFrame::new((5, 3), (5, 3)).unwrap();
        assert!(copied.copy_rows);
        copied.update(&source, full);
        assert_eq!(copied.pixels, source.pixels());
        // The same frame sampled tap by tap gives the same pixels.
        let mut sampled = ScaledFrame::new((5, 3), (5, 3)).unwrap();
        sampled.copy_rows = false;
        sampled.update(&source, full);
        assert_eq!(sampled.pixels, copied.pixels);
        // Scaled frames keep sampling.
        assert!(!ScaledFrame::new((5, 3), (10, 6)).unwrap().copy_rows);
    }

    #[test]
    fn throughput_reports_rate_size_bandwidth_and_encoding() {
        let before = StatsSnapshot {
            bytes: 1_000_000,
            frames: 10,
            encoding: None,
        };
        let after = StatsSnapshot {
            bytes: 13_000_000,
            frames: 70,
            encoding: Some(7),
        };
        assert_eq!(
            throughput(before, after, std::time::Duration::from_secs(2)),
            "30 fps · 200 KB/frame · 48 Mbit/s · Tight"
        );
        assert_eq!(
            throughput(before, before, std::time::Duration::from_secs(1)),
            "0 fps · 0 KB/frame · 0 Mbit/s · waiting"
        );
    }

    #[test]
    fn copy_area_copies_only_the_area() {
        let mut source = Framebuffer::new(3, 2).unwrap();
        source.pixels_mut().fill(7);
        let mut target = Framebuffer::new(3, 2).unwrap();
        copy_area(
            &source,
            &mut target,
            Rect {
                x0: 1,
                y0: 1,
                x1: 3,
                y1: 2,
            },
        );
        assert_eq!(target.pixels(), &[0, 0, 0, 0, 7, 7]);
    }

    #[test]
    fn aspect_fit_keeps_letterbox_pixels_black() {
        let mut source = Framebuffer::new(2, 1).unwrap();
        source.apply_raw(0, 0, 2, 1, &[255; 8]).unwrap();
        let mut scaled = ScaledFrame::new((2, 1), (4, 4)).unwrap();
        scaled.update(
            &source,
            Rect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 1,
            },
        );
        assert_eq!(&scaled.pixels[..4], &[0; 4]);
        assert_eq!(&scaled.pixels[12..], &[0; 4]);
        assert_eq!(&scaled.pixels[4..8], &[0xffffff; 4]);
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

    #[test]
    fn native_mode_centers_and_crops_without_scaling() {
        let mut source = Framebuffer::new(4, 2).unwrap();
        source.apply_raw(1, 0, 1, 1, &[0, 0, 255, 0]).unwrap();
        let mut scaled =
            ScaledFrame::with_settings((4, 2), (2, 2), WindowMode::Native, Quality::Smooth)
                .unwrap();
        scaled.update(
            &source,
            Rect {
                x0: 0,
                y0: 0,
                x1: 4,
                y1: 2,
            },
        );
        assert_eq!(scaled.pixels[0], 0xff0000);
        assert_eq!(
            pointer_for_mode(0.0, 0.0, (2, 2), (4, 2), &scaled),
            Some((1, 0))
        );
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
            let full = Rect {
                x0: 0,
                y0: 0,
                x1: width,
                y1: height,
            };
            let mut presented = Framebuffer::new(width as u16, height as u16).unwrap();
            let started = Instant::now();
            copy_area(&framebuffer, &mut presented, full);
            let mirror = started.elapsed();
            let window = fit_dimensions((width, height), (1820, 918));
            let mut scaled = ScaledFrame::with_settings(
                (width, height),
                window,
                WindowMode::Fit,
                Quality::Smooth,
            )
            .unwrap();
            let started = Instant::now();
            scaled.update(&presented, full);
            let scale = started.elapsed();
            println!(
                "{width}x{height}: host copy {:.1} ms, tile compare {:.1} ms | viewer mirror {:.1} ms, scale to {}x{} {:.1} ms",
                copy.as_secs_f64() * 1e3,
                compare.as_secs_f64() * 1e3,
                mirror.as_secs_f64() * 1e3,
                window.0,
                window.1,
                scale.as_secs_f64() * 1e3,
            );
        }
    }
}
