use minifb::{InputCallback, Key, MouseButton, MouseMode, ScaleMode, Window, WindowOptions};
use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use topvnc::{Encoding, Framebuffer, Session};

mod settings;
mod ui;
use ui::{Box2, Canvas, Compression, Config, Field, Quality, UiState, WindowMode};

impl From<Compression> for Encoding {
    fn from(value: Compression) -> Self {
        match value {
            Compression::Raw => Encoding::Raw,
            Compression::Zlib => Encoding::Zlib,
        }
    }
}

const USAGE: &str = "usage: topvnc [HOST:PORT] [--allow-insecure] [--fit | --native-size | --window WIDTHxHEIGHT] [--input-debug]";

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
    let remote_aspect = remote.0 as f32 / remote.1 as f32;
    let window_aspect = window.0 as f32 / window.1 as f32;
    let (width, height) = if remote_aspect > window_aspect {
        (window.0, (window.0 as f32 / remote_aspect) as usize)
    } else {
        ((window.1 as f32 * remote_aspect) as usize, window.1)
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

fn bilinear(a: u32, b: u32, c: u32, d: u32, x_weight: u32, y_weight: u32) -> u32 {
    let mut result = 0;
    for shift in [0, 8, 16] {
        let channel = |pixel: u32| (pixel >> shift) & 0xff;
        let top = channel(a) * (256 - x_weight) + channel(b) * x_weight;
        let bottom = channel(c) * (256 - x_weight) + channel(d) * x_weight;
        let value = (top * (256 - y_weight) + bottom * y_weight + 32768) >> 16;
        result |= value << shift;
    }
    result
}

struct ScaledFrame {
    window: (usize, usize),
    draw: Rect,
    pixels: Vec<u32>,
    x_taps: Vec<SampleTap>,
    y_taps: Vec<SampleTap>,
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
        Ok(Self {
            window,
            draw,
            pixels: vec![0; count],
            x_taps,
            y_taps,
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
    dirty: Option<Rect>,
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
    let mut config = Config::default();
    settings::load(&mut config);
    let mut input_debug = false;
    let mut address = None;
    let mut args = std::env::args().skip(1);
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
    loop {
        let Some((next_config, session)) = show_landing(config, connection_error.take())? else {
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

fn show_landing(
    mut config: Config,
    error: Option<String>,
) -> Result<Option<(Config, Session)>, Box<dyn Error>> {
    let mut window = Window::new(
        "TopVNC — Connect",
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
    while window.is_open() {
        ui::landing(
            &mut Canvas::new(&mut pixels, 800, 640),
            &config,
            &state,
            connecting,
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
        for event in input_rx.try_iter() {
            match event {
                InputEvent::Character(character) if !connecting => {
                    state.character(&mut config, character)
                }
                InputEvent::Key(Key::Enter, true) if !connecting => connect_clicked = true,
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
        if state.click(window.get_mouse_down(MouseButton::Left)) && !connecting {
            let (x, y) = (mx as usize, my as usize);
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
        if connect_clicked && !connecting {
            match config.address() {
                Ok(address) => {
                    state.error = None;
                    connecting = true;
                    let sender = result_tx.clone();
                    let password = config.password.clone();
                    let allow_insecure = config.allow_insecure;
                    let compression = config.compression;
                    thread::spawn(move || {
                        let result = Session::connect_with_encoding(
                            &address,
                            allow_insecure,
                            compression.into(),
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
            let mut incremental = false;
            loop {
                worker_writer.request_update(incremental, info.width, info.height)?;
                session.read_update_with(&mut scratch, |x, y, width, height, bytes| {
                    let mut frame = worker_framebuffer.lock().unwrap();
                    frame.framebuffer.apply_raw(x, y, width, height, bytes)?;
                    let rect = Rect {
                        x0: usize::from(x),
                        y0: usize::from(y),
                        x1: usize::from(x) + usize::from(width),
                        y1: usize::from(y) + usize::from(height),
                    };
                    frame.dirty = Some(frame.dirty.map_or(rect, |dirty| dirty.union(rect)));
                    Ok(())
                })?;
                incremental = true;
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
    let mut window = Window::new(
        &format!("TopVNC — {}", info.name),
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
    let (input_tx, input_rx) = mpsc::channel();
    window.set_input_callback(Box::new(KeyEvents(input_tx)));
    let mut pressed_keys = HashMap::new();
    let mut last_pointer = None;
    let mut ui_state = UiState::default();
    let mut settings_open = false;
    let mut dragging_ui_scale = false;
    let mut ui_captured_mouse = false;
    let mut redraw = false;
    while window.is_open() {
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
        {
            let mut frame = framebuffer.lock().unwrap();
            let dirty = if full_redraw {
                frame.dirty = None;
                Some(Rect {
                    x0: 0,
                    y0: 0,
                    x1: remote.0,
                    y1: remote.1,
                })
            } else {
                frame.dirty.take()
            };
            if let Some(dirty) = dirty {
                scaled.update(&frame.framebuffer, dirty);
            }
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
