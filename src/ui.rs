use minifb::Key;

pub const BG: u32 = 0x0b1020;
const PANEL: u32 = 0x172238;
const FIELD: u32 = 0x101a2c;
const BORDER: u32 = 0x344761;
const TEXT: u32 = 0xeaf2ff;
const MUTED: u32 = 0x9eafc5;
const ACCENT: u32 = 0x54d6ba;
const ERROR: u32 = 0xff8b8b;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowMode {
    Fit,
    Native,
    Custom,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    Smooth,
    Sharp,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: String,
    pub username: String,
    pub password: String,
    pub allow_insecure: bool,
    pub window_mode: WindowMode,
    pub window_size: String,
    pub fps: usize,
    pub quality: Quality,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: "5900".into(),
            username: String::new(),
            password: String::new(),
            allow_insecure: false,
            window_mode: WindowMode::Fit,
            window_size: "1280x720".into(),
            fps: 60,
            quality: Quality::Smooth,
        }
    }
}

impl Config {
    pub fn address(&self) -> Result<String, String> {
        let host = self.host.trim();
        if host.is_empty() || host.chars().any(char::is_whitespace) {
            return Err("Enter a valid server address.".into());
        }
        let port: u16 = self.port.parse().map_err(|_| "Port must be 1-65535.")?;
        if port == 0 {
            return Err("Port must be 1-65535.".into());
        }
        if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
            return Err("Use brackets around an IPv6 address.".into());
        }
        if self.window_mode == WindowMode::Custom {
            self.custom_size()?;
        }
        Ok(format!("{host}:{port}"))
    }

    pub fn custom_size(&self) -> Result<(usize, usize), String> {
        let (width, height) = self
            .window_size
            .split_once('x')
            .ok_or("Window size must be WIDTHxHEIGHT.")?;
        let width: usize = width.parse().map_err(|_| "Invalid window width.")?;
        let height: usize = height.parse().map_err(|_| "Invalid window height.")?;
        if !(1..=8192).contains(&width) || !(1..=8192).contains(&height) {
            return Err("Window dimensions must be 1-8192.".into());
        }
        if width * height > 33_554_432 {
            return Err("Window size is too large.".into());
        }
        Ok((width, height))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Host,
    Port,
    Username,
    Password,
    WindowSize,
}

#[derive(Default)]
pub struct UiState {
    pub focus: Option<Field>,
    pub error: Option<String>,
    mouse_was_down: bool,
}

impl UiState {
    pub fn click(&mut self, down: bool) -> bool {
        let pressed = down && !self.mouse_was_down;
        self.mouse_was_down = down;
        pressed
    }

    pub fn key(&mut self, config: &mut Config, key: Key) -> bool {
        match key {
            Key::Tab => {
                self.focus = Some(match self.focus {
                    None => Field::Host,
                    Some(Field::Host) => Field::Port,
                    Some(Field::Port) => Field::Username,
                    Some(Field::Username) => Field::Password,
                    Some(Field::Password) => Field::WindowSize,
                    Some(Field::WindowSize) => Field::Host,
                });
                true
            }
            Key::Backspace => {
                if let Some(value) = self.value_mut(config) {
                    value.pop();
                }
                true
            }
            _ => false,
        }
    }

    pub fn character(&mut self, config: &mut Config, character: char) {
        if character.is_control() {
            return;
        }
        if let Some(value) = self.value_mut(config)
            && value.chars().count() < 128
        {
            value.push(character);
        }
    }

    fn value_mut<'a>(&self, config: &'a mut Config) -> Option<&'a mut String> {
        match self.focus? {
            Field::Host => Some(&mut config.host),
            Field::Port => Some(&mut config.port),
            Field::Username => Some(&mut config.username),
            Field::Password => Some(&mut config.password),
            Field::WindowSize => Some(&mut config.window_size),
        }
    }
}

#[derive(Clone, Copy)]
pub struct Box2 {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Box2 {
    pub fn contains(self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

pub struct Canvas<'a> {
    pixels: &'a mut [u32],
    width: usize,
    height: usize,
}

impl<'a> Canvas<'a> {
    pub fn new(pixels: &'a mut [u32], width: usize, height: usize) -> Self {
        Self {
            pixels,
            width,
            height,
        }
    }

    pub fn fill(&mut self, area: Box2, color: u32) {
        for y in area.y.min(self.height)..area.y.saturating_add(area.h).min(self.height) {
            let row = y * self.width;
            for x in area.x.min(self.width)..area.x.saturating_add(area.w).min(self.width) {
                self.pixels[row + x] = color;
            }
        }
    }

    pub fn frame(&mut self, area: Box2, color: u32) {
        if area.w == 0 || area.h == 0 {
            return;
        }
        self.fill(Box2 { h: 1, ..area }, color);
        self.fill(
            Box2 {
                y: area.y + area.h - 1,
                h: 1,
                ..area
            },
            color,
        );
        self.fill(Box2 { w: 1, ..area }, color);
        self.fill(
            Box2 {
                x: area.x + area.w - 1,
                w: 1,
                ..area
            },
            color,
        );
    }

    pub fn text(&mut self, x: usize, y: usize, message: &str, color: u32, scale: usize) {
        let mut cursor = x;
        for character in message.chars() {
            if cursor + 5 * scale > self.width {
                break;
            }
            let rows = glyph(character.to_ascii_uppercase());
            for (row, bits) in rows.into_iter().enumerate() {
                for col in 0..5 {
                    if bits & (1 << (4 - col)) != 0 {
                        self.fill(
                            Box2 {
                                x: cursor + col * scale,
                                y: y + row * scale,
                                w: scale,
                                h: scale,
                            },
                            color,
                        );
                    }
                }
            }
            cursor += 6 * scale;
        }
    }

    pub fn label(&mut self, x: usize, y: usize, message: &str) {
        self.text(x, y, message, MUTED, 2);
    }

    pub fn field(
        &mut self,
        area: Box2,
        value: &str,
        placeholder: &str,
        focused: bool,
        masked: bool,
    ) {
        self.fill(area, FIELD);
        self.frame(area, if focused { ACCENT } else { BORDER });
        let display = if value.is_empty() {
            placeholder.to_string()
        } else if masked {
            "*".repeat(value.chars().count())
        } else {
            value.to_string()
        };
        let max_chars = area.w.saturating_sub(28) / 12;
        let visible: String = display
            .chars()
            .rev()
            .take(max_chars)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        self.text(
            area.x + 12,
            area.y + 12,
            &visible,
            if value.is_empty() { MUTED } else { TEXT },
            2,
        );
        if focused && visible.chars().count() < max_chars {
            self.fill(
                Box2 {
                    x: area.x + 12 + visible.chars().count() * 12,
                    y: area.y + 9,
                    w: 2,
                    h: 18,
                },
                ACCENT,
            );
        }
    }

    pub fn button(&mut self, area: Box2, label: &str, selected: bool) {
        self.fill(area, if selected { ACCENT } else { PANEL });
        self.frame(area, if selected { ACCENT } else { BORDER });
        self.text(
            area.x + 12,
            area.y + (area.h.saturating_sub(14)) / 2,
            label,
            if selected { BG } else { TEXT },
            2,
        );
    }
}

pub const HOST: Box2 = Box2 {
    x: 40,
    y: 130,
    w: 490,
    h: 40,
};
pub const PORT: Box2 = Box2 {
    x: 548,
    y: 130,
    w: 210,
    h: 40,
};
pub const USERNAME: Box2 = Box2 {
    x: 40,
    y: 215,
    w: 345,
    h: 40,
};
pub const PASSWORD: Box2 = Box2 {
    x: 403,
    y: 215,
    w: 355,
    h: 40,
};
pub const INSECURE: Box2 = Box2 {
    x: 40,
    y: 292,
    w: 24,
    h: 24,
};
pub const FIT: Box2 = Box2 {
    x: 40,
    y: 386,
    w: 120,
    h: 36,
};
pub const NATIVE: Box2 = Box2 {
    x: 172,
    y: 386,
    w: 120,
    h: 36,
};
pub const CUSTOM: Box2 = Box2 {
    x: 304,
    y: 386,
    w: 120,
    h: 36,
};
pub const SIZE: Box2 = Box2 {
    x: 436,
    y: 386,
    w: 180,
    h: 36,
};
pub const FPS30: Box2 = Box2 {
    x: 40,
    y: 483,
    w: 90,
    h: 36,
};
pub const FPS60: Box2 = Box2 {
    x: 142,
    y: 483,
    w: 90,
    h: 36,
};
pub const FPS120: Box2 = Box2 {
    x: 244,
    y: 483,
    w: 100,
    h: 36,
};
pub const SMOOTH: Box2 = Box2 {
    x: 420,
    y: 483,
    w: 140,
    h: 36,
};
pub const SHARP: Box2 = Box2 {
    x: 572,
    y: 483,
    w: 130,
    h: 36,
};
pub const CONNECT: Box2 = Box2 {
    x: 40,
    y: 565,
    w: 718,
    h: 48,
};

pub fn landing(canvas: &mut Canvas<'_>, config: &Config, state: &UiState, connecting: bool) {
    canvas.fill(
        Box2 {
            x: 0,
            y: 0,
            w: 800,
            h: 640,
        },
        BG,
    );
    canvas.fill(
        Box2 {
            x: 0,
            y: 0,
            w: 800,
            h: 8,
        },
        ACCENT,
    );
    canvas.text(40, 36, "TOPVNC", TEXT, 4);
    canvas.text(42, 76, "CONNECT TO A REMOTE DESKTOP", MUTED, 2);
    canvas.label(40, 108, "SERVER ADDRESS");
    canvas.label(548, 108, "PORT");
    canvas.field(
        HOST,
        &config.host,
        "192.168.1.100",
        state.focus == Some(Field::Host),
        false,
    );
    canvas.field(
        PORT,
        &config.port,
        "5900",
        state.focus == Some(Field::Port),
        false,
    );
    canvas.label(40, 192, "USERNAME (UNSUPPORTED)");
    canvas.label(403, 192, "VNC PASSWORD");
    canvas.field(
        USERNAME,
        &config.username,
        "NOT SENT TO SERVER",
        state.focus == Some(Field::Username),
        false,
    );
    canvas.field(
        PASSWORD,
        &config.password,
        "OPTIONAL",
        state.focus == Some(Field::Password),
        true,
    );
    canvas.fill(INSECURE, if config.allow_insecure { ACCENT } else { FIELD });
    canvas.frame(INSECURE, BORDER);
    if config.allow_insecure {
        canvas.text(45, 296, "X", BG, 2);
    }
    canvas.text(76, 296, "ALLOW NONE AUTHENTICATION", TEXT, 2);
    canvas.text(
        40,
        331,
        "VNC PASSWORD TRAFFIC IS UNENCRYPTED. USE A SECURE TUNNEL.",
        MUTED,
        2,
    );
    canvas.label(40, 362, "WINDOW SIZE");
    canvas.button(FIT, "FIT", config.window_mode == WindowMode::Fit);
    canvas.button(NATIVE, "NATIVE", config.window_mode == WindowMode::Native);
    canvas.button(CUSTOM, "CUSTOM", config.window_mode == WindowMode::Custom);
    canvas.field(
        SIZE,
        &config.window_size,
        "1280x720",
        state.focus == Some(Field::WindowSize),
        false,
    );
    canvas.label(40, 456, "UPDATE RATE");
    canvas.label(420, 456, "SCALING");
    canvas.button(FPS30, "30 FPS", config.fps == 30);
    canvas.button(FPS60, "60 FPS", config.fps == 60);
    canvas.button(FPS120, "120 FPS", config.fps == 120);
    canvas.button(SMOOTH, "SMOOTH", config.quality == Quality::Smooth);
    canvas.button(SHARP, "SHARP", config.quality == Quality::Sharp);
    if let Some(error) = &state.error {
        let short: String = error.chars().take(62).collect();
        canvas.text(40, 537, &short, ERROR, 2);
    } else {
        canvas.text(40, 537, "RAW ENCODING  /  TCP TRANSPORT", MUTED, 2);
    }
    canvas.button(
        CONNECT,
        if connecting {
            "CONNECTING..."
        } else {
            "CONNECT"
        },
        true,
    );
}

pub const OPEN_SETTINGS: Box2 = Box2 {
    x: 12,
    y: 12,
    w: 138,
    h: 32,
};
pub const CLOSE_SETTINGS: Box2 = Box2 {
    x: 362,
    y: 20,
    w: 72,
    h: 32,
};
pub const LIVE_FIT: Box2 = Box2 {
    x: 24,
    y: 142,
    w: 190,
    h: 38,
};
pub const LIVE_NATIVE: Box2 = Box2 {
    x: 226,
    y: 142,
    w: 190,
    h: 38,
};
pub const LIVE_30: Box2 = Box2 {
    x: 24,
    y: 228,
    w: 110,
    h: 38,
};
pub const LIVE_60: Box2 = Box2 {
    x: 146,
    y: 228,
    w: 110,
    h: 38,
};
pub const LIVE_120: Box2 = Box2 {
    x: 268,
    y: 228,
    w: 148,
    h: 38,
};
pub const LIVE_SMOOTH: Box2 = Box2 {
    x: 24,
    y: 314,
    w: 190,
    h: 38,
};
pub const LIVE_SHARP: Box2 = Box2 {
    x: 226,
    y: 314,
    w: 190,
    h: 38,
};
pub const DISCONNECT: Box2 = Box2 {
    x: 24,
    y: 413,
    w: 392,
    h: 42,
};

pub fn overlay(canvas: &mut Canvas<'_>, config: &Config, open: bool) {
    if !open {
        canvas.button(OPEN_SETTINGS, "F8 SETTINGS", false);
        return;
    }
    canvas.fill(
        Box2 {
            x: 8,
            y: 8,
            w: 432,
            h: 462,
        },
        BG,
    );
    canvas.frame(
        Box2 {
            x: 8,
            y: 8,
            w: 432,
            h: 462,
        },
        ACCENT,
    );
    canvas.text(24, 28, "SESSION SETTINGS", TEXT, 3);
    canvas.button(CLOSE_SETTINGS, "CLOSE", false);
    canvas.text(24, 83, "WINDOW", ACCENT, 2);
    canvas.text(24, 107, "DRAG WINDOW EDGES TO RESIZE", MUTED, 2);
    canvas.button(
        LIVE_FIT,
        "FIT IMAGE",
        config.window_mode != WindowMode::Native,
    );
    canvas.button(
        LIVE_NATIVE,
        "1:1 PIXELS",
        config.window_mode == WindowMode::Native,
    );
    canvas.text(24, 199, "UPDATE RATE", ACCENT, 2);
    canvas.button(LIVE_30, "30 FPS", config.fps == 30);
    canvas.button(LIVE_60, "60 FPS", config.fps == 60);
    canvas.button(LIVE_120, "120 FPS", config.fps == 120);
    canvas.text(24, 285, "SCALING", ACCENT, 2);
    canvas.button(LIVE_SMOOTH, "SMOOTH", config.quality == Quality::Smooth);
    canvas.button(LIVE_SHARP, "SHARP", config.quality == Quality::Sharp);
    canvas.text(24, 376, "RFB TRAFFIC IS UNENCRYPTED", ERROR, 2);
    canvas.button(DISCONNECT, "DISCONNECT", false);
}

fn glyph(c: char) -> [u8; 7] {
    match c {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 14],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [31, 4, 4, 4, 4, 4, 31],
        'J' => [7, 2, 2, 2, 18, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 31],
        '/' => [1, 1, 2, 4, 8, 16, 16],
        '\\' => [16, 16, 8, 4, 2, 1, 1],
        '[' => [14, 8, 8, 8, 8, 8, 14],
        ']' => [14, 2, 2, 2, 2, 2, 14],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        '*' => [0, 21, 14, 31, 14, 21, 0],
        '!' => [4, 4, 4, 4, 4, 0, 4],
        '?' => [14, 17, 1, 2, 4, 0, 4],
        '=' => [0, 31, 0, 31, 0, 0, 0],
        ',' => [0, 0, 0, 0, 12, 12, 8],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        ' ' => [0; 7],
        _ => [31, 17, 21, 21, 21, 17, 31],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_connection_values() {
        let mut config = Config::default();
        assert!(config.address().is_err());
        config.host = "localhost".into();
        assert_eq!(config.address().unwrap(), "localhost:5900");
        config.port = "0".into();
        assert!(config.address().is_err());
        config.port = "5900".into();
        config.window_mode = WindowMode::Custom;
        config.window_size = "99999x2".into();
        assert!(config.address().is_err());
        config.window_size = "8192x8192".into();
        assert!(config.address().is_err());
    }
}
