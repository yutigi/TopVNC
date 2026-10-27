use crate::desktop_host::{HostPermissions, MIN_SERVE_SCALE, MouseMode, normalize_serve_scale};
use crate::helpers::{Helper, HelperForm, HelperRuntime};
use topvnc::presentation::MotionPresentation;
use topvnc::{PerformanceMode, UpdatePriority};
use winit::keyboard::KeyCode;

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Compression {
    Raw,
    Zlib,
    /// Tight with JPEG for photographic and game content; falls back to Zlib
    /// or Raw on servers without Tight.
    Tight,
}

/// RFB JPEG quality level (0-9) used with Tight unless `--quality` sets one.
pub const DEFAULT_JPEG_QUALITY: u8 = 6;

#[derive(Clone, Debug)]
pub struct Config {
    pub performance: PerformanceMode,
    pub motion: MotionPresentation,
    pub host: String,
    pub port: String,
    pub password: String,
    pub allow_insecure: bool,
    pub window_mode: WindowMode,
    pub window_size: String,
    /// Presentation limit in frames per second; 0 presents every frame as
    /// it arrives.
    pub fps: usize,
    pub quality: Quality,
    pub compression: Compression,
    /// RFB JPEG quality level, 0 (smallest) to 9 (best), used with Tight.
    pub jpeg_quality: u8,
    pub ui_scale: f32,
    /// Show detailed session statistics in the title bar.
    pub debug: bool,
    /// Lock the pointer and send relative motion when the host asks, as it
    /// does while a game hides the cursor.
    pub relative_mouse: bool,
    /// Present partial updates as rectangles arrive.
    pub progressive: bool,
    pub serve: ServeForm,
    pub helpers: HelperForm,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            performance: PerformanceMode::Standard,
            motion: MotionPresentation::Smooth,
            host: String::new(),
            port: "5900".into(),
            password: String::new(),
            allow_insecure: false,
            window_mode: WindowMode::Fit,
            window_size: "1280x720".into(),
            fps: 0,
            quality: Quality::Smooth,
            compression: Compression::Tight,
            jpeg_quality: DEFAULT_JPEG_QUALITY,
            ui_scale: 1.0,
            debug: false,
            relative_mouse: true,
            progressive: false,
            serve: ServeForm::default(),
            helpers: HelperForm::default(),
        }
    }
}

/// Validate a host and port typed into a form and join them as `HOST:PORT`.
fn socket_address(host: &str, port: &str, host_error: &str) -> Result<String, String> {
    let host = host.trim();
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return Err(host_error.into());
    }
    let port: u16 = port.parse().map_err(|_| "Port must be 1-65535.")?;
    if port == 0 {
        return Err("Port must be 1-65535.".into());
    }
    if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
        return Err("Use brackets around an IPv6 address.".into());
    }
    Ok(format!("{host}:{port}"))
}

pub const LOCAL_ONLY_HOST: &str = "127.0.0.1";
pub const ALL_NETWORKS_HOST: &str = "0.0.0.0";

/// The Server tab form. Like the client's security choices, it is never saved.
#[derive(Clone, Debug)]
pub struct ServeForm {
    pub performance: PerformanceMode,
    pub host: String,
    pub port: String,
    pub password: String,
    pub display: String,
    pub allow_insecure: bool,
    /// Served size as a fraction of the display's pixel size.
    pub scale: f32,
    /// When viewers are asked for relative pointer motion.
    pub mouse: MouseMode,
    pub priority: UpdatePriority,
}

impl Default for ServeForm {
    fn default() -> Self {
        Self {
            performance: PerformanceMode::Standard,
            host: LOCAL_ONLY_HOST.into(),
            port: "5900".into(),
            password: String::new(),
            display: String::new(),
            allow_insecure: false,
            scale: 1.0,
            mouse: MouseMode::Auto,
            priority: UpdatePriority::Whole,
        }
    }
}

/// A validated request to start serving this desktop.
#[derive(Debug, PartialEq)]
pub struct ServeRequest {
    pub performance: PerformanceMode,
    pub address: String,
    /// 1-based display number; `None` serves the primary display.
    pub display: Option<usize>,
    /// `None` only when unauthenticated access was explicitly allowed.
    pub password: Option<String>,
    pub allow_insecure: bool,
    /// Served size as a fraction of the display's pixel size.
    pub scale: f32,
    pub mouse: MouseMode,
    pub priority: UpdatePriority,
}

impl ServeForm {
    pub fn request(&self) -> Result<ServeRequest, String> {
        let address = socket_address(&self.host, &self.port, "Enter a valid listen address.")?;
        let display = match self.display.trim() {
            "" => None,
            number => Some(
                number
                    .parse::<usize>()
                    .ok()
                    .filter(|number| *number > 0)
                    .ok_or("Display must be a number from 1, or empty.")?,
            ),
        };
        // A typed password always enables authentication; the checkbox only
        // permits leaving it empty.
        let password = if self.password.is_empty() {
            if !self.allow_insecure {
                return Err("Enter a password or allow none authentication.".into());
            }
            None
        } else {
            Some(self.password.clone())
        };
        Ok(ServeRequest {
            performance: self.performance,
            address,
            display,
            password,
            allow_insecure: self.allow_insecure,
            scale: normalize_serve_scale(self.scale),
            mouse: self.mouse,
            priority: self.priority,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ServerPhase {
    #[default]
    Stopped,
    Starting,
    Serving {
        address: String,
        width: u16,
        height: u16,
        connections: usize,
        authenticated: bool,
        /// Remote keyboard and pointer input is ignored for lack of permission.
        view_only: bool,
    },
    Stopping,
}

/// What the Server tab shows about the desktop server.
#[derive(Clone, Debug, Default)]
pub struct ServerView {
    pub phase: ServerPhase,
    /// The latest status message from the server host.
    pub message: Option<String>,
    /// Host permission state, on platforms that require it.
    pub permissions: Option<HostPermissions>,
}

impl Config {
    pub fn presentation_interval(&self) -> std::time::Duration {
        if self.performance == PerformanceMode::Ultra || self.fps == 0 {
            std::time::Duration::ZERO
        } else {
            std::time::Duration::from_secs_f64(1.0 / self.fps as f64)
        }
    }

    /// 0 = whole, 1 = progressive batching, 2 = coherent Ultra updates.
    pub fn upload_policy(&self) -> u8 {
        if self.performance == PerformanceMode::Ultra {
            2
        } else {
            u8::from(self.progressive)
        }
    }

    pub fn address(&self) -> Result<String, String> {
        let address = socket_address(&self.host, &self.port, "Enter a valid server address.")?;
        if self.window_mode == WindowMode::Custom {
            self.custom_size()?;
        }
        Ok(address)
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Host,
    Port,
    Password,
    WindowSize,
    ServeHost,
    ServePort,
    ServePassword,
    ServeDisplay,
    WorkHost,
    WorkPort,
    NetworkPort,
    FirewallPeer,
    FirewallLocal,
    FirewallProgram,
    FirewallInterface,
    FirewallPort,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tab {
    #[default]
    Connect,
    Server,
    Work,
    Network,
    Firewall,
}

impl Tab {
    /// Text fields in Tab-key order.
    fn fields(self) -> &'static [Field] {
        match self {
            Self::Connect => &[Field::Host, Field::Port, Field::Password, Field::WindowSize],
            Self::Server => &[
                Field::ServeHost,
                Field::ServePort,
                Field::ServePassword,
                Field::ServeDisplay,
            ],
            Self::Work => &[Field::WorkHost, Field::WorkPort],
            Self::Network => &[Field::NetworkPort],
            Self::Firewall => &[
                Field::FirewallPeer,
                Field::FirewallLocal,
                Field::FirewallProgram,
                Field::FirewallInterface,
                Field::FirewallPort,
            ],
        }
    }

    pub fn helper(self) -> Option<Helper> {
        match self {
            Self::Work => Some(Helper::Work),
            Self::Network => Some(Helper::Network),
            Self::Firewall => Some(Helper::Firewall),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct UiState {
    pub tab: Tab,
    pub menu_open: bool,
    pub focus: Option<Field>,
    pub error: Option<String>,
}

impl UiState {
    /// Handle an editing key; returns whether it was one.
    pub fn key(&mut self, config: &mut Config, key: KeyCode) -> bool {
        match key {
            KeyCode::Tab => {
                let fields = self.tab.fields();
                let next = self
                    .focus
                    .and_then(|focus| fields.iter().position(|field| *field == focus))
                    .map_or(0, |index| (index + 1) % fields.len());
                self.focus = Some(fields[next]);
                true
            }
            KeyCode::Backspace => {
                if let Some(value) = self.value_mut(config) {
                    value.pop();
                }
                true
            }
            _ => false,
        }
    }

    pub fn switch_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.focus = None;
            self.error = None;
        }
        self.menu_open = false;
    }

    pub fn character(&mut self, config: &mut Config, character: char) {
        if character.is_control() {
            return;
        }
        if let Some(value) = self.value_mut(config)
            && value.chars().count()
                < if self.focus == Some(Field::FirewallProgram) {
                    1024
                } else {
                    128
                }
        {
            value.push(character);
        }
    }

    fn value_mut<'a>(&self, config: &'a mut Config) -> Option<&'a mut String> {
        match self.focus? {
            Field::Host => Some(&mut config.host),
            Field::Port => Some(&mut config.port),
            Field::Password => Some(&mut config.password),
            Field::WindowSize => Some(&mut config.window_size),
            Field::ServeHost => Some(&mut config.serve.host),
            Field::ServePort => Some(&mut config.serve.port),
            Field::ServePassword => Some(&mut config.serve.password),
            Field::ServeDisplay => Some(&mut config.serve.display),
            Field::WorkHost => Some(&mut config.helpers.work_host),
            Field::WorkPort => Some(&mut config.helpers.work_port),
            Field::NetworkPort => Some(&mut config.helpers.network_port),
            Field::FirewallPeer => Some(&mut config.helpers.peer),
            Field::FirewallLocal => Some(&mut config.helpers.local),
            Field::FirewallProgram => Some(&mut config.helpers.program),
            Field::FirewallInterface => Some(&mut config.helpers.interface),
            Field::FirewallPort => Some(&mut config.helpers.firewall_port),
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

pub const ULTRA_MODE: Box2 = Box2 {
    x: 520,
    y: 66,
    w: 238,
    h: 32,
};

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
pub const PASSWORD: Box2 = Box2 {
    x: 40,
    y: 215,
    w: 718,
    h: 40,
};
pub const RAW: Box2 = Box2 {
    x: 400,
    y: 280,
    w: 62,
    h: 36,
};
pub const ZLIB: Box2 = Box2 {
    x: 470,
    y: 280,
    w: 72,
    h: 36,
};
/// Tight JPEG quality presets: the RFB quality level each button selects.
/// Lower levels send smaller frames, so they keep up on slower links.
pub const JPEG_PRESETS: [(Box2, u8, &str); 3] = [
    (
        Box2 {
            x: 550,
            y: 280,
            w: 64,
            h: 36,
        },
        3,
        "LOW",
    ),
    (
        Box2 {
            x: 620,
            y: 280,
            w: 64,
            h: 36,
        },
        DEFAULT_JPEG_QUALITY,
        "MED",
    ),
    (
        Box2 {
            x: 690,
            y: 280,
            w: 68,
            h: 36,
        },
        9,
        "HIGH",
    ),
];
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
pub const FPS60: Box2 = Box2 {
    x: 40,
    y: 483,
    w: 90,
    h: 36,
};
pub const FPS120: Box2 = Box2 {
    x: 142,
    y: 483,
    w: 100,
    h: 36,
};
pub const FPS_NO_LIMIT: Box2 = Box2 {
    x: 254,
    y: 483,
    w: 122,
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

pub const TAB_CONNECT: Box2 = Box2 {
    x: 178,
    y: 34,
    w: 110,
    h: 36,
};
pub const TAB_SERVER: Box2 = Box2 {
    x: 296,
    y: 34,
    w: 98,
    h: 36,
};
pub const TAB_MORE: Box2 = Box2 {
    x: 402,
    y: 34,
    w: 72,
    h: 36,
};
pub const MENU_WORK: Box2 = Box2 {
    x: 402,
    y: 78,
    w: 150,
    h: 36,
};
pub const MENU_NETWORK: Box2 = Box2 {
    x: 402,
    y: 118,
    w: 150,
    h: 36,
};
pub const MENU_FIREWALL: Box2 = Box2 {
    x: 402,
    y: 158,
    w: 150,
    h: 36,
};
pub const MENU_TABS: [(Box2, Tab, &str); 3] = [
    (MENU_WORK, Tab::Work, "WORK"),
    (MENU_NETWORK, Tab::Network, "NETWORK"),
    (MENU_FIREWALL, Tab::Firewall, "FIREWALL"),
];
pub const TABS: [(Box2, Tab, &str); 2] = [
    (TAB_CONNECT, Tab::Connect, "CONNECT"),
    (TAB_SERVER, Tab::Server, "SERVER"),
];
pub const HELPER_RUN: Box2 = Box2 {
    x: 40,
    y: 574,
    w: 718,
    h: 40,
};
pub const FW_PREVIEW: Box2 = Box2 {
    x: 40,
    y: 574,
    w: 230,
    h: 40,
};
pub const FW_SET: Box2 = Box2 {
    x: 284,
    y: 574,
    w: 230,
    h: 40,
};
pub const FW_RESET: Box2 = Box2 {
    x: 528,
    y: 574,
    w: 230,
    h: 40,
};
pub const WORK_CHECK: Box2 = Box2 {
    x: 40,
    y: 574,
    w: 350,
    h: 40,
};
pub const WORK_START: Box2 = Box2 {
    x: 408,
    y: 574,
    w: 350,
    h: 40,
};
pub const REPORT_PREV: Box2 = Box2 {
    x: 40,
    y: 536,
    w: 82,
    h: 28,
};
pub const REPORT_NEXT: Box2 = Box2 {
    x: 130,
    y: 536,
    w: 82,
    h: 28,
};
pub const REPORT_COPY: Box2 = Box2 {
    x: 548,
    y: 536,
    w: 210,
    h: 28,
};
pub const NETWORK_PORT: Box2 = Box2 {
    x: 40,
    y: 130,
    w: 210,
    h: 40,
};
pub const FW_PEER: Box2 = Box2 {
    x: 40,
    y: 130,
    w: 350,
    h: 40,
};
pub const FW_LOCAL: Box2 = Box2 {
    x: 408,
    y: 130,
    w: 350,
    h: 40,
};
pub const FW_PROGRAM: Box2 = Box2 {
    x: 40,
    y: 200,
    w: 718,
    h: 40,
};
pub const FW_INTERFACE: Box2 = Box2 {
    x: 40,
    y: 270,
    w: 350,
    h: 40,
};
pub const FW_PORT: Box2 = Box2 {
    x: 40,
    y: 340,
    w: 210,
    h: 40,
};
pub const FW_PROFILES: [(Box2, &str); 3] = [
    (
        Box2 {
            x: 408,
            y: 270,
            w: 110,
            h: 40,
        },
        "Domain",
    ),
    (
        Box2 {
            x: 526,
            y: 270,
            w: 110,
            h: 40,
        },
        "Private",
    ),
    (
        Box2 {
            x: 644,
            y: 270,
            w: 114,
            h: 40,
        },
        "Public",
    ),
];

pub fn helper_fields(tab: Tab) -> &'static [(Box2, Field)] {
    match tab {
        Tab::Work => &[(HOST, Field::WorkHost), (PORT, Field::WorkPort)],
        Tab::Network => &[(NETWORK_PORT, Field::NetworkPort)],
        Tab::Firewall => &[
            (FW_PEER, Field::FirewallPeer),
            (FW_LOCAL, Field::FirewallLocal),
            (FW_PROGRAM, Field::FirewallProgram),
            (FW_INTERFACE, Field::FirewallInterface),
            (FW_PORT, Field::FirewallPort),
        ],
        _ => &[],
    }
}
pub const SERVE_PASSWORD: Box2 = Box2 {
    x: 40,
    y: 215,
    w: 490,
    h: 40,
};
pub const SERVE_DISPLAY: Box2 = Box2 {
    x: 548,
    y: 215,
    w: 210,
    h: 40,
};
pub const SERVE_PRIORITY: [(Box2, UpdatePriority, &str); 4] = [
    (
        Box2 {
            x: 190,
            y: 428,
            w: 150,
            h: 26,
        },
        UpdatePriority::Whole,
        "WHOLE",
    ),
    (
        Box2 {
            x: 348,
            y: 428,
            w: 125,
            h: 26,
        },
        UpdatePriority::Center,
        "CENTER",
    ),
    (
        Box2 {
            x: 481,
            y: 428,
            w: 125,
            h: 26,
        },
        UpdatePriority::Mouse,
        "MOUSE",
    ),
    (
        Box2 {
            x: 614,
            y: 428,
            w: 144,
            h: 26,
        },
        UpdatePriority::Changes,
        "CHANGES",
    ),
];

/// The `--mouse` choices: when viewers are asked for relative motion.
pub const SERVE_MOUSE: [(Box2, MouseMode, &str); 3] = [
    (
        Box2 {
            x: 400,
            y: 280,
            w: 80,
            h: 36,
        },
        MouseMode::Auto,
        "AUTO",
    ),
    (
        Box2 {
            x: 488,
            y: 280,
            w: 131,
            h: 36,
        },
        MouseMode::Relative,
        "RELATIVE",
    ),
    (
        Box2 {
            x: 627,
            y: 280,
            w: 131,
            h: 36,
        },
        MouseMode::Absolute,
        "ABSOLUTE",
    ),
];
pub const LOCAL_ONLY: Box2 = Box2 {
    x: 40,
    y: 386,
    w: 196,
    h: 36,
};
pub const ALL_NETWORKS: Box2 = Box2 {
    x: 248,
    y: 386,
    w: 196,
    h: 36,
};
/// Clickable and draggable area of the served-size slider.
pub const SERVE_SCALE_SLIDER: Box2 = Box2 {
    x: 460,
    y: 386,
    w: 160,
    h: 36,
};
pub const SERVE_FULL_SIZE: Box2 = Box2 {
    x: 628,
    y: 386,
    w: 62,
    h: 36,
};
pub const SERVE_HALF_SIZE: Box2 = Box2 {
    x: 696,
    y: 386,
    w: 62,
    h: 36,
};
const SERVE_SLIDER_LEFT: usize = 468;
const SERVE_SLIDER_RIGHT: usize = 612;

/// The served-size scale at slider position `x`, in hundredths.
pub fn serve_scale_from_slider_x(x: usize) -> f32 {
    let fraction = (x.saturating_sub(SERVE_SLIDER_LEFT) as f32
        / (SERVE_SLIDER_RIGHT - SERVE_SLIDER_LEFT) as f32)
        .clamp(0.0, 1.0);
    normalize_serve_scale(MIN_SERVE_SCALE + fraction * (1.0 - MIN_SERVE_SCALE))
}

fn serve_slider_x(scale: f32) -> usize {
    let fraction = (normalize_serve_scale(scale) - MIN_SERVE_SCALE) / (1.0 - MIN_SERVE_SCALE);
    SERVE_SLIDER_LEFT
        + (fraction * (SERVE_SLIDER_RIGHT - SERVE_SLIDER_LEFT) as f32).round() as usize
}
const SERVER_STATUS: Box2 = Box2 {
    x: 40,
    y: 478,
    w: 718,
    h: 44,
};

pub fn landing(
    canvas: &mut Canvas<'_>,
    config: &Config,
    state: &UiState,
    connecting: bool,
    server: &ServerView,
    helpers: &HelperRuntime,
) {
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
    canvas.text(40, 40, "TOPVNC", TEXT, 3);
    for (area, tab, label) in TABS {
        canvas.button(area, label, state.tab == tab);
    }
    canvas.button(
        TAB_MORE,
        "...",
        state.menu_open || state.tab.helper().is_some(),
    );
    match state.tab {
        Tab::Connect => connect_tab(canvas, config, state, connecting),
        Tab::Server => server_tab(canvas, &config.serve, state, server),
        Tab::Work | Tab::Network | Tab::Firewall => {
            helper_tab(canvas, &config.helpers, state, helpers)
        }
    }
    if state.menu_open {
        for (area, tab, label) in MENU_TABS {
            canvas.button(area, label, state.tab == tab);
        }
    }
}

/// Preserve line breaks and wrap long commands without hiding their contents.
pub fn report_lines(text: &str) -> Vec<String> {
    text.lines()
        .flat_map(|line| {
            let clean: String = line
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            if clean.trim().is_empty() {
                vec![String::new()]
            } else {
                wrap(&clean, 57)
            }
        })
        .collect()
}

pub fn report_rows(tab: Tab) -> usize {
    match tab {
        Tab::Firewall => 6,
        Tab::Work => 12,
        _ => 15,
    }
}

fn helper_tab(
    canvas: &mut Canvas<'_>,
    form: &HelperForm,
    state: &UiState,
    helpers: &HelperRuntime,
) {
    let helper = state.tab.helper().expect("helper tab");
    let enabled = cfg!(windows) && !helpers.busy();
    let field = |canvas: &mut Canvas<'_>, area, value: &str, placeholder, focus| {
        canvas.field(area, value, placeholder, state.focus == Some(focus), false);
    };
    let output_y = match state.tab {
        Tab::Work => {
            canvas.text(42, 76, "START A WORK SERVER IN ITS OWN TERMINAL", MUTED, 2);
            canvas.label(40, 108, "LOCAL VPN IPV4");
            canvas.label(548, 108, "PORT");
            field(
                canvas,
                HOST,
                &form.work_host,
                "YOUR LOCAL VPN IP",
                Field::WorkHost,
            );
            field(canvas, PORT, &form.work_port, "5900", Field::WorkPort);
            canvas.text(
                40,
                192,
                "Start-TopVncWorkServer.cmd / target/debug/topvnc.exe",
                MUTED,
                1,
            );
            canvas.text(
                40,
                218,
                "ENTER THE PASSWORD IN THE SERVER TERMINAL.",
                TEXT,
                2,
            );
            canvas.text(
                40,
                244,
                "UNENCRYPTED VNC. KEEP YOUR TRUSTED PRIVATE VPN READY.",
                MUTED,
                2,
            );
            canvas.button(WORK_CHECK, "CHECK LAUNCHER", enabled);
            canvas.button(WORK_START, "START IN TERMINAL", false);
            280
        }
        Tab::Network => {
            canvas.text(
                42,
                76,
                "INSPECT LOCAL LISTENERS AND FIREWALL POLICY",
                MUTED,
                2,
            );
            canvas.label(40, 108, "LISTENER PORT");
            field(
                canvas,
                NETWORK_PORT,
                &form.network_port,
                "5900",
                Field::NetworkPort,
            );
            canvas.text(276, 137, "Test-TopVncNetwork.ps1", MUTED, 1);
            canvas.text(
                40,
                188,
                "READ ONLY / NO PEER PROBE / NO NETWORK CHANGES",
                MUTED,
                2,
            );
            canvas.button(
                HELPER_RUN,
                if helpers.busy() {
                    "RUNNING..."
                } else {
                    "RUN DIAGNOSTICS"
                },
                enabled,
            );
            220
        }
        Tab::Firewall => {
            canvas.text(
                42,
                76,
                "MANAGE A RULE FOR ONE PRIVATE NETWORK PEER",
                MUTED,
                2,
            );
            canvas.label(40, 108, "PEER IPV4");
            canvas.label(408, 108, "LOCAL IPV4");
            field(
                canvas,
                FW_PEER,
                &form.peer,
                "OTHER COMPUTER",
                Field::FirewallPeer,
            );
            field(
                canvas,
                FW_LOCAL,
                &form.local,
                "THIS COMPUTER",
                Field::FirewallLocal,
            );
            canvas.label(40, 178, "SERVER EXECUTABLE (ABSOLUTE .EXE PATH)");
            field(
                canvas,
                FW_PROGRAM,
                &form.program,
                "C:\\TopVNC\\topvnc.exe",
                Field::FirewallProgram,
            );
            canvas.label(40, 248, "INTERFACE ALIAS");
            canvas.label(408, 248, "FIREWALL PROFILE");
            field(
                canvas,
                FW_INTERFACE,
                &form.interface,
                "YOUR VPN INTERFACE",
                Field::FirewallInterface,
            );
            for (area, profile) in FW_PROFILES {
                canvas.button(area, profile, form.profile == profile);
            }
            canvas.label(40, 318, "TCP PORT");
            field(
                canvas,
                FW_PORT,
                &form.firewall_port,
                "5900",
                Field::FirewallPort,
            );
            canvas.text(
                276,
                343,
                "SET / RESET REQUEST ADMINISTRATOR APPROVAL.",
                MUTED,
                1,
            );
            canvas.text(
                276,
                361,
                "RESET REMOVES ONLY THIS EXACT TOPVNC RULE.",
                MUTED,
                1,
            );
            canvas.button(
                FW_PREVIEW,
                if helpers.busy() {
                    "RUNNING..."
                } else {
                    "PREVIEW RULE"
                },
                enabled,
            );
            canvas.button(FW_SET, "SET RULE", enabled);
            canvas.button(FW_RESET, "RESET RULE", enabled);
            398
        }
        _ => unreachable!(),
    };
    let report = helpers.report(helper);
    let area = Box2 {
        x: 40,
        y: output_y,
        w: 718,
        h: 524 - output_y,
    };
    canvas.fill(area, FIELD);
    canvas.frame(area, BORDER);
    let lines = report_lines(if cfg!(windows) {
        &report.text
    } else {
        crate::helpers::WINDOWS_ONLY
    });
    let rows = report_rows(state.tab);
    let pages = lines.len().div_ceil(rows).max(1);
    let page = report.page.min(pages - 1);
    for (index, line) in lines.iter().skip(page * rows).take(rows).enumerate() {
        canvas.text(
            52,
            output_y + 10 + index * 18,
            line,
            if report.failed { ERROR } else { TEXT },
            2,
        );
    }
    canvas.button(REPORT_PREV, "PREV", false);
    canvas.button(REPORT_NEXT, "NEXT", false);
    canvas.text(
        230,
        544,
        &format!(
            "PAGE {} / {}{}",
            page + 1,
            pages,
            if helpers.busy() {
                " / BUSY"
            } else if report.copied {
                " / COPIED"
            } else {
                ""
            }
        ),
        MUTED,
        1,
    );
    canvas.button(REPORT_COPY, "COPY REPORT", false);
}

fn connect_tab(canvas: &mut Canvas<'_>, config: &Config, state: &UiState, connecting: bool) {
    canvas.text(42, 76, "CONNECT TO A REMOTE DESKTOP", MUTED, 2);
    ultra_button(canvas, ULTRA_MODE, config.performance);
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
    canvas.label(40, 192, "VNC PASSWORD");
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
    canvas.label(400, 258, "COMPRESSION");
    canvas.button(RAW, "RAW", config.compression == Compression::Raw);
    canvas.button(ZLIB, "ZLIB", config.compression == Compression::Zlib);
    // The label names the level, which `--quality` may set off the presets.
    canvas.label(550, 258, &format!("TIGHT JPEG Q{}", config.jpeg_quality));
    for (area, level, name) in JPEG_PRESETS {
        canvas.button(
            area,
            name,
            config.compression == Compression::Tight && config.jpeg_quality == level,
        );
    }
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
    canvas.label(40, 456, "FRAME LIMIT");
    canvas.label(420, 456, "SCALING");
    canvas.button(
        FPS60,
        "60 FPS",
        config.fps == 60 && config.performance == PerformanceMode::Standard,
    );
    canvas.button(
        FPS120,
        "120 FPS",
        config.fps == 120 && config.performance == PerformanceMode::Standard,
    );
    canvas.button(
        FPS_NO_LIMIT,
        "NO LIMIT",
        config.presentation_interval().is_zero(),
    );
    canvas.button(SMOOTH, "SMOOTH", config.quality == Quality::Smooth);
    canvas.button(SHARP, "SHARP", config.quality == Quality::Sharp);
    if let Some(error) = &state.error {
        let short: String = error.chars().take(62).collect();
        canvas.text(40, 537, &short, ERROR, 2);
    } else {
        canvas.text(40, 537, "TCP TRANSPORT  /  UNENCRYPTED", MUTED, 2);
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

fn server_tab(canvas: &mut Canvas<'_>, form: &ServeForm, state: &UiState, server: &ServerView) {
    canvas.text(42, 76, "SHARE THIS DESKTOP OVER VNC", MUTED, 2);
    ultra_button(canvas, ULTRA_MODE, form.performance);
    canvas.label(40, 108, "LISTEN ADDRESS");
    canvas.label(548, 108, "PORT");
    canvas.field(
        HOST,
        &form.host,
        LOCAL_ONLY_HOST,
        state.focus == Some(Field::ServeHost),
        false,
    );
    canvas.field(
        PORT,
        &form.port,
        "5900",
        state.focus == Some(Field::ServePort),
        false,
    );
    canvas.label(40, 192, "SERVER PASSWORD");
    canvas.field(
        SERVE_PASSWORD,
        &form.password,
        if form.allow_insecure {
            "NONE"
        } else {
            "REQUIRED"
        },
        state.focus == Some(Field::ServePassword),
        true,
    );
    canvas.fill(INSECURE, if form.allow_insecure { ACCENT } else { FIELD });
    canvas.frame(INSECURE, BORDER);
    if form.allow_insecure {
        canvas.text(45, 296, "X", BG, 2);
    }
    canvas.text(76, 296, "ALLOW NONE AUTHENTICATION", TEXT, 2);
    canvas.label(548, 192, "DISPLAY");
    canvas.field(
        SERVE_DISPLAY,
        &form.display,
        "PRIMARY",
        state.focus == Some(Field::ServeDisplay),
        false,
    );
    // Auto asks for relative motion while a game hides the cursor; macOS
    // hosts cannot tell, so they need Relative for mouse look.
    canvas.label(400, 258, "GAME MOUSE");
    for (area, mode, name) in SERVE_MOUSE {
        canvas.button(area, name, form.mouse == mode);
    }
    canvas.text(
        40,
        331,
        "ONLY 8 PASSWORD CHARACTERS ARE USED. TCP IS UNENCRYPTED.",
        MUTED,
        2,
    );
    canvas.label(40, 362, "LISTEN ON");
    canvas.button(
        LOCAL_ONLY,
        "THIS PC ONLY",
        form.host.trim() == LOCAL_ONLY_HOST,
    );
    canvas.button(
        ALL_NETWORKS,
        "ALL NETWORKS",
        form.host.trim() == ALL_NETWORKS_HOST,
    );
    canvas.label(460, 362, "SERVED SIZE");
    let scale = normalize_serve_scale(form.scale);
    canvas.text(698, 362, &format!("{scale:.2}X"), TEXT, 2);
    let track_y = SERVE_SCALE_SLIDER.y + 15;
    canvas.fill(
        Box2 {
            x: SERVE_SLIDER_LEFT,
            y: track_y,
            w: SERVE_SLIDER_RIGHT - SERVE_SLIDER_LEFT,
            h: 6,
        },
        BORDER,
    );
    let thumb = serve_slider_x(scale);
    canvas.fill(
        Box2 {
            x: SERVE_SLIDER_LEFT,
            y: track_y,
            w: thumb - SERVE_SLIDER_LEFT,
            h: 6,
        },
        ACCENT,
    );
    canvas.fill(
        Box2 {
            x: thumb.saturating_sub(6),
            y: SERVE_SCALE_SLIDER.y + 5,
            w: 12,
            h: 26,
        },
        ACCENT,
    );
    canvas.button(SERVE_FULL_SIZE, "FULL", scale == 1.0);
    canvas.button(SERVE_HALF_SIZE, "HALF", scale == 0.5);

    canvas.label(40, 434, "PRIORITY");
    for (area, priority, name) in SERVE_PRIORITY {
        canvas.button(area, name, form.priority == priority);
    }
    canvas.label(40, 456, "STATUS");
    canvas.fill(SERVER_STATUS, PANEL);
    canvas.frame(SERVER_STATUS, BORDER);
    let serving = matches!(server.phase, ServerPhase::Serving { .. });
    let view_only = matches!(
        server.phase,
        ServerPhase::Serving {
            view_only: true,
            ..
        }
    );
    canvas.fill(
        Box2 {
            x: SERVER_STATUS.x + 14,
            y: SERVER_STATUS.y + 17,
            w: 10,
            h: 10,
        },
        match (serving, view_only) {
            (true, true) => ERROR,
            (true, false) => ACCENT,
            _ => MUTED,
        },
    );
    let text_y = SERVER_STATUS.y + 15;
    let status = match &server.phase {
        ServerPhase::Stopped => stopped_status(server.permissions),
        ServerPhase::Starting => "STARTING CAPTURE...".to_string(),
        ServerPhase::Stopping => "STOPPING...".to_string(),
        ServerPhase::Serving {
            address,
            width,
            height,
            connections,
            view_only,
            ..
        } => {
            let viewers = match connections {
                1 => "1 VIEWER".to_string(),
                count => format!("{count} VIEWERS"),
            };
            let right = SERVER_STATUS.x + SERVER_STATUS.w - 12;
            canvas.text(right - viewers.len() * 12, text_y, &viewers, TEXT, 2);
            let room = (SERVER_STATUS.w - 48) / 12 - viewers.len() - 2;
            let mode = if *view_only { "VIEW ONLY" } else { "SERVING" };
            let status = format!("{mode} {width}X{height} ON {address}");
            status.chars().take(room).collect()
        }
    };
    canvas.text(
        SERVER_STATUS.x + 34,
        text_y,
        &status,
        if serving { TEXT } else { MUTED },
        2,
    );

    if let Some(error) = &state.error {
        status_message(canvas, error, ERROR);
    } else if let ServerPhase::Serving {
        authenticated: false,
        ..
    } = server.phase
    {
        canvas.text(
            40,
            537,
            "NO AUTHENTICATION: ANY VIEWER CAN CONTROL THIS PC",
            ERROR,
            2,
        );
    } else if view_only {
        canvas.text(
            40,
            537,
            "VIEW ONLY: ALLOW ACCESSIBILITY ACCESS FOR REMOTE INPUT",
            ERROR,
            2,
        );
    } else if let Some(message) = &server.message {
        status_message(canvas, message, MUTED);
    } else {
        canvas.text(40, 537, "TCP TRANSPORT  /  UNENCRYPTED", MUTED, 2);
    }
    match server.phase {
        ServerPhase::Stopped => canvas.button(CONNECT, "START SERVER", true),
        ServerPhase::Starting => canvas.button(CONNECT, "STARTING...", true),
        ServerPhase::Serving { .. } => canvas.button(CONNECT, "STOP SERVER", false),
        ServerPhase::Stopping => canvas.button(CONNECT, "STOPPING...", false),
    }
}

fn stopped_status(permissions: Option<HostPermissions>) -> String {
    let answer = |allowed| if allowed { "YES" } else { "NO" };
    match permissions {
        Some(HostPermissions {
            screen_recording,
            accessibility,
        }) => format!(
            "STOPPED / SCREEN RECORDING: {} / ACCESSIBILITY: {}",
            answer(screen_recording),
            answer(accessibility)
        ),
        None if cfg!(any(windows, target_os = "macos")) => "STOPPED".into(),
        None => "STOPPED  /  WINDOWS AND MACOS HOSTS ONLY".into(),
    }
}

/// Characters per line of the message below the server status.
const MESSAGE_LINE: usize = 60;

/// Break `message` into lines of at most `width` characters, at spaces where
/// possible.
fn wrap(message: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in message.split_whitespace() {
        let mut word = word.to_string();
        while word.chars().count() > width {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            let rest = word.split_off(
                word.char_indices()
                    .nth(width)
                    .map_or(word.len(), |(at, _)| at),
            );
            lines.push(std::mem::replace(&mut word, rest));
        }
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// The message below the server status: one large line, two when it does
/// not fit, or small text for long instructions such as permission errors.
fn status_message(canvas: &mut Canvas<'_>, message: &str, color: u32) {
    let large = wrap(message, MESSAGE_LINE);
    match large.as_slice() {
        [line] => canvas.text(40, 537, line, color, 2),
        [first, second] => {
            canvas.text(40, 527, first, color, 2);
            canvas.text(40, 546, second, color, 2);
        }
        _ => {
            for (index, line) in wrap(message, MESSAGE_LINE * 2).iter().take(4).enumerate() {
                canvas.text(40, 525 + index * 10, line, color, 1);
            }
        }
    }
}

pub fn open_settings_box(scale: f32) -> Box2 {
    let scale = scale.clamp(0.5, 2.0);
    Box2 {
        x: 12,
        y: 12,
        w: (150.0 * scale).round() as usize,
        h: (36.0 * scale).round() as usize,
    }
}
pub const CLOSE_SETTINGS: Box2 = Box2 {
    x: 362,
    y: 20,
    w: 72,
    h: 32,
};
pub const LIVE_SERVE_SLIDER: Box2 = Box2 {
    x: 24,
    y: 196,
    w: 392,
    h: 40,
};

pub fn live_serve_percent_from_x(x: usize) -> u8 {
    let offset = x
        .saturating_sub(SLIDER_LEFT)
        .min(SLIDER_RIGHT - SLIDER_LEFT);
    25 + ((offset * 75 + (SLIDER_RIGHT - SLIDER_LEFT) / 2) / (SLIDER_RIGHT - SLIDER_LEFT)) as u8
}

pub const LIVE_FIT: Box2 = Box2 {
    x: 24,
    y: 86,
    w: 190,
    h: 38,
};
pub const LIVE_NATIVE: Box2 = Box2 {
    x: 226,
    y: 86,
    w: 190,
    h: 38,
};
pub const LIVE_FULLSCREEN: Box2 = Box2 {
    x: 24,
    y: 130,
    w: 392,
    h: 38,
};
pub const LIVE_60: Box2 = Box2 {
    x: 24,
    y: 270,
    w: 110,
    h: 38,
};
pub const LIVE_120: Box2 = Box2 {
    x: 146,
    y: 270,
    w: 124,
    h: 38,
};
pub const LIVE_NO_LIMIT: Box2 = Box2 {
    x: 282,
    y: 270,
    w: 134,
    h: 38,
};
pub const LIVE_SMOOTH: Box2 = Box2 {
    x: 24,
    y: 340,
    w: 190,
    h: 38,
};
pub const LIVE_SHARP: Box2 = Box2 {
    x: 226,
    y: 340,
    w: 190,
    h: 38,
};
pub const LIVE_MOUSE_AUTO: Box2 = Box2 {
    x: 24,
    y: 410,
    w: 190,
    h: 38,
};
pub const LIVE_MOUSE_OFF: Box2 = Box2 {
    x: 226,
    y: 410,
    w: 190,
    h: 38,
};
pub const LIVE_WHOLE_UPDATE: Box2 = Box2 {
    x: 24,
    y: 480,
    w: 110,
    h: 38,
};
pub const LIVE_PROGRESSIVE: Box2 = Box2 {
    x: 146,
    y: 480,
    w: 158,
    h: 38,
};
pub const LIVE_ULTRA: Box2 = Box2 {
    x: 316,
    y: 480,
    w: 100,
    h: 38,
};
pub const SCALE_SLIDER: Box2 = Box2 {
    x: 24,
    y: 578,
    w: 392,
    h: 48,
};
pub const LIVE_MOTION_SMOOTH: Box2 = Box2 {
    x: 24,
    y: 524,
    w: 190,
    h: 24,
};
pub const LIVE_MOTION_IMMEDIATE: Box2 = Box2 {
    x: 226,
    y: 524,
    w: 190,
    h: 24,
};
pub const LIVE_DEBUG: Box2 = Box2 {
    x: 24,
    y: 656,
    w: 190,
    h: 42,
};
pub const DISCONNECT: Box2 = Box2 {
    x: 226,
    y: 656,
    w: 190,
    h: 42,
};
const SLIDER_LEFT: usize = 36;
const SLIDER_RIGHT: usize = 404;

pub fn scale_from_slider_x(x: usize) -> f32 {
    let fraction = (x.saturating_sub(SLIDER_LEFT) as f32 / (SLIDER_RIGHT - SLIDER_LEFT) as f32)
        .clamp(0.0, 1.0);
    0.5 + fraction * 1.5
}

fn slider_x(scale: f32) -> usize {
    SLIDER_LEFT
        + (((scale.clamp(0.5, 2.0) - 0.5) / 1.5) * (SLIDER_RIGHT - SLIDER_LEFT) as f32).round()
            as usize
}
pub const SETTINGS_PANEL: Box2 = Box2 {
    x: 8,
    y: 8,
    w: 432,
    h: 694,
};

fn ultra_button(canvas: &mut Canvas<'_>, area: Box2, performance: PerformanceMode) {
    let ultra = performance == PerformanceMode::Ultra;
    canvas.button(area, if ultra { "ULTRA: ON" } else { "ULTRA: OFF" }, ultra);
}

pub fn overlay(canvas: &mut Canvas<'_>, config: &Config, open: bool, serve_scale: Option<u8>) {
    if !open {
        let area = open_settings_box(config.ui_scale);
        let text_scale = (2.0 * config.ui_scale).round().clamp(1.0, 4.0) as usize;
        let text_width = 11 * 6 * text_scale;
        canvas.fill(area, PANEL);
        canvas.frame(area, BORDER);
        canvas.text(
            area.x + area.w.saturating_sub(text_width) / 2,
            area.y + area.h.saturating_sub(7 * text_scale) / 2,
            "F8 SETTINGS",
            TEXT,
            text_scale,
        );
        return;
    }
    canvas.fill(SETTINGS_PANEL, BG);
    canvas.frame(SETTINGS_PANEL, ACCENT);
    canvas.text(24, 28, "SESSION SETTINGS", TEXT, 3);
    canvas.button(CLOSE_SETTINGS, "CLOSE", false);
    canvas.text(24, 62, "WINDOW", ACCENT, 2);
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
    canvas.button(LIVE_FULLSCREEN, "FULL SCREEN ON / OFF", false);
    canvas.text(24, 178, "SERVED SIZE", ACCENT, 2);
    let color = if serve_scale.is_some() {
        ACCENT
    } else {
        BORDER
    };
    canvas.fill(
        Box2 {
            x: SLIDER_LEFT,
            y: 212,
            w: SLIDER_RIGHT - SLIDER_LEFT,
            h: 6,
        },
        BORDER,
    );
    if let Some(percent) = serve_scale {
        canvas.text(252, 178, &format!("{percent}% SHARED"), MUTED, 1);
        let thumb = SLIDER_LEFT
            + (usize::from(percent.clamp(25, 100) - 25) * (SLIDER_RIGHT - SLIDER_LEFT) + 37) / 75;
        canvas.fill(
            Box2 {
                x: SLIDER_LEFT,
                y: 212,
                w: thumb - SLIDER_LEFT,
                h: 6,
            },
            color,
        );
        canvas.fill(
            Box2 {
                x: thumb - 6,
                y: 204,
                w: 12,
                h: 22,
            },
            color,
        );
        canvas.text(24, 230, "25%", MUTED, 1);
        canvas.text(390, 230, "100%", MUTED, 1);
    } else {
        canvas.text(252, 178, "UNAVAILABLE", MUTED, 1);
        canvas.text(24, 230, "REQUIRES AN UPDATED TOPVNC HOST", MUTED, 1);
    }
    canvas.text(24, 246, "FRAME LIMIT", ACCENT, 2);
    canvas.button(
        LIVE_60,
        "60 FPS",
        config.fps == 60 && config.performance == PerformanceMode::Standard,
    );
    canvas.button(
        LIVE_120,
        "120 FPS",
        config.fps == 120 && config.performance == PerformanceMode::Standard,
    );
    canvas.button(
        LIVE_NO_LIMIT,
        "NO LIMIT",
        config.presentation_interval().is_zero(),
    );
    canvas.text(24, 316, "SCALING", ACCENT, 2);
    canvas.button(LIVE_SMOOTH, "SMOOTH", config.quality == Quality::Smooth);
    canvas.button(LIVE_SHARP, "SHARP", config.quality == Quality::Sharp);
    canvas.text(24, 386, "GAME MOUSE", ACCENT, 2);
    canvas.button(LIVE_MOUSE_AUTO, "AUTO LOCK", config.relative_mouse);
    canvas.button(LIVE_MOUSE_OFF, "OFF", !config.relative_mouse);
    canvas.text(24, 456, "PRESENT UPDATES", ACCENT, 2);
    canvas.button(LIVE_WHOLE_UPDATE, "WHOLE", config.upload_policy() == 0);
    canvas.button(LIVE_PROGRESSIVE, "PROGRESSIVE", config.upload_policy() == 1);
    canvas.button(LIVE_ULTRA, "ULTRA", config.upload_policy() == 2);
    canvas.button(
        LIVE_MOTION_SMOOTH,
        "SMOOTH MOTION",
        config.motion == MotionPresentation::Smooth,
    );
    canvas.button(
        LIVE_MOTION_IMMEDIATE,
        "IMMEDIATE",
        config.motion == MotionPresentation::Immediate,
    );
    canvas.text(24, 552, "F8 BUTTON SIZE", ACCENT, 2);
    canvas.text(330, 552, &format!("{:.2}X", config.ui_scale), TEXT, 2);
    let track_y = SCALE_SLIDER.y + 19;
    canvas.fill(
        Box2 {
            x: SLIDER_LEFT,
            y: track_y,
            w: SLIDER_RIGHT - SLIDER_LEFT,
            h: 6,
        },
        BORDER,
    );
    let thumb = slider_x(config.ui_scale);
    canvas.fill(
        Box2 {
            x: SLIDER_LEFT,
            y: track_y,
            w: thumb - SLIDER_LEFT,
            h: 6,
        },
        ACCENT,
    );
    canvas.fill(
        Box2 {
            x: thumb.saturating_sub(6),
            y: track_y - 10,
            w: 12,
            h: 26,
        },
        ACCENT,
    );
    canvas.text(24, track_y + 26, "0.5X", MUTED, 1);
    canvas.text(384, track_y + 26, "2X", MUTED, 1);
    canvas.text(24, 634, "RFB TRAFFIC IS UNENCRYPTED", ERROR, 2);
    canvas.button(
        LIVE_DEBUG,
        if config.debug {
            "DEBUG: ON"
        } else {
            "DEBUG: OFF"
        },
        config.debug,
    );
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
        '%' => [24, 25, 2, 4, 8, 19, 3],
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
        '&' => [12, 18, 20, 8, 21, 18, 13],
        '\'' => [4, 4, 8, 0, 0, 0, 0],
        '"' => [10, 10, 0, 0, 0, 0, 0],
        '<' => [2, 4, 8, 16, 8, 4, 2],
        '>' | '→' => [8, 4, 2, 1, 2, 4, 8],
        ' ' => [0; 7],
        _ => [31, 17, 21, 21, 21, 17, 31],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ultra_overrides_pacing_and_preserves_user_settings_and_security() {
        assert_eq!(Config::default().motion, MotionPresentation::Smooth);
        let mut config = Config {
            fps: 60,
            progressive: true,
            ..Config::default()
        };
        let interval = config.presentation_interval();
        assert_eq!(config.upload_policy(), 1);
        config.performance = PerformanceMode::Ultra;
        assert_eq!(config.presentation_interval(), std::time::Duration::ZERO);
        assert_eq!(config.upload_policy(), 2);
        assert_eq!(config.fps, 60);
        assert!(!config.allow_insecure);
        config.performance = PerformanceMode::Standard;
        assert_eq!(config.presentation_interval(), interval);
        assert_eq!(config.upload_policy(), 1);

        let form = ServeForm {
            performance: PerformanceMode::Ultra,
            ..ServeForm::default()
        };
        assert!(form.request().is_err());
        let form = ServeForm {
            password: "secret".into(),
            ..form
        };
        assert_eq!(form.request().unwrap().performance, PerformanceMode::Ultra);
        assert!(!form.request().unwrap().allow_insecure);
        const {
            assert!(LIVE_WHOLE_UPDATE.x + LIVE_WHOLE_UPDATE.w < LIVE_PROGRESSIVE.x);
            assert!(LIVE_PROGRESSIVE.x + LIVE_PROGRESSIVE.w < LIVE_ULTRA.x);
            assert!(LIVE_ULTRA.x + LIVE_ULTRA.w <= SETTINGS_PANEL.x + SETTINGS_PANEL.w);
            assert!(LIVE_ULTRA.y + LIVE_ULTRA.h < LIVE_MOTION_SMOOTH.y);
            assert!(LIVE_MOTION_SMOOTH.y + LIVE_MOTION_SMOOTH.h < 552);
            assert!(LIVE_MOTION_SMOOTH.x + LIVE_MOTION_SMOOTH.w < LIVE_MOTION_IMMEDIATE.x);
        }
    }

    #[test]
    fn server_priority_buttons_fit_and_reach_the_host_request() {
        let mut form = ServeForm {
            password: "secret".into(),
            ..ServeForm::default()
        };
        for (area, priority, name) in SERVE_PRIORITY {
            assert!(12 + name.len() * 12 <= area.w);
            assert!(area.y + area.h <= 456);
            form.priority = priority;
            assert_eq!(form.request().unwrap().priority, priority);
        }
        for pair in SERVE_PRIORITY.windows(2) {
            assert!(pair[0].0.x + pair[0].0.w < pair[1].0.x);
        }
        const { assert!(LIVE_PROGRESSIVE.y + LIVE_PROGRESSIVE.h < SCALE_SLIDER.y) };
        const { assert!(DISCONNECT.y + DISCONNECT.h <= SETTINGS_PANEL.y + SETTINGS_PANEL.h) };
        assert!(!Config::default().progressive);
    }

    #[test]
    fn helper_tabs_keep_field_focus_and_values_separate() {
        let mut state = UiState::default();
        let mut config = Config::default();
        for tab in [Tab::Work, Tab::Network, Tab::Firewall] {
            state.switch_tab(tab);
            assert_eq!(state.focus, None);
            for &expected in tab.fields() {
                state.key(&mut config, KeyCode::Tab);
                assert_eq!(state.focus, Some(expected));
            }
            state.key(&mut config, KeyCode::Tab);
            assert_eq!(state.focus, Some(tab.fields()[0]));
        }
        state.switch_tab(Tab::Network);
        state.key(&mut config, KeyCode::Tab);
        state.character(&mut config, '1');
        assert_eq!(config.helpers.network_port, "59001");
        assert_eq!(config.helpers.work_port, "5900");
        assert_eq!(config.helpers.firewall_port, "5900");
        assert_eq!(config.port, "5900");
    }

    #[test]
    fn reports_preserve_blank_lines_and_page_long_unicode_commands() {
        let text = format!("Status\n\n{}\nLast line", "é".repeat(400));
        let lines = report_lines(&text);
        assert_eq!(&lines[..2], &["Status", ""]);
        assert_eq!(lines.last().unwrap(), "Last line");
        assert!(lines.iter().all(|line| line.chars().count() <= 57));
        assert_eq!(lines[2..lines.len() - 1].concat(), "é".repeat(400));
        assert!(lines.len() > report_rows(Tab::Firewall));
    }

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

    #[test]
    fn tab_skips_removed_username_and_f8_hitbox_scales() {
        let mut state = UiState::default();
        let mut config = Config::default();
        for expected in [Field::Host, Field::Port, Field::Password, Field::WindowSize] {
            state.key(&mut config, KeyCode::Tab);
            assert!(state.focus == Some(expected));
        }
        let small = open_settings_box(0.5);
        let large = open_settings_box(2.0);
        assert!(large.w > small.w && large.h > small.h);
        assert!(!small.contains(large.x + large.w - 1, large.y + large.h - 1));
        assert!(large.contains(large.x + large.w - 1, large.y + large.h - 1));
        assert_eq!(scale_from_slider_x(0), 0.5);
        assert_eq!(scale_from_slider_x(usize::MAX), 2.0);
        assert!((scale_from_slider_x((SLIDER_LEFT + SLIDER_RIGHT) / 2) - 1.25).abs() < 0.01);
    }

    #[test]
    fn server_tab_cycles_its_own_fields_and_switching_clears_focus() {
        let mut state = UiState::default();
        let mut config = Config::default();
        state.key(&mut config, KeyCode::Tab);
        state.switch_tab(Tab::Server);
        assert_eq!(state.focus, None);
        for expected in [
            Field::ServeHost,
            Field::ServePort,
            Field::ServePassword,
            Field::ServeDisplay,
            Field::ServeHost,
        ] {
            state.key(&mut config, KeyCode::Tab);
            assert_eq!(state.focus, Some(expected));
        }
        state.key(&mut config, KeyCode::Tab);
        state.character(&mut config, '7');
        assert_eq!(config.serve.port, "59007");
        assert_eq!(config.port, "5900");
    }

    #[test]
    fn server_messages_wrap_at_spaces_and_split_long_words() {
        assert_eq!(wrap("ONE TWO THREE", 7), ["ONE TWO", "THREE"]);
        assert_eq!(wrap("  ", 7), Vec::<String>::new());
        assert_eq!(wrap("ABCDEFGHIJ KL", 4), ["ABCD", "EFGH", "IJ", "KL"]);
        assert_eq!(wrap("é😀é😀é", 2), ["é😀", "é😀", "é"]);
        let message = "Screen Recording access is required. Allow TopVNC in System Settings → \
                       Privacy & Security → Screen & System Audio Recording, then relaunch it";
        assert!(wrap(message, MESSAGE_LINE * 2).len() <= 4);
    }

    #[test]
    fn stopped_status_reports_host_permissions() {
        let permissions = |screen_recording, accessibility| {
            Some(HostPermissions {
                screen_recording,
                accessibility,
            })
        };
        let status = stopped_status(permissions(true, false));
        assert_eq!(
            status,
            "STOPPED / SCREEN RECORDING: YES / ACCESSIBILITY: NO"
        );
        // It fits the status panel next to the indicator.
        assert!(status.len() <= (SERVER_STATUS.w - 46) / 12);
        assert!(stopped_status(None).starts_with("STOPPED"));
    }

    #[test]
    fn serve_form_requires_a_password_unless_none_is_explicit() {
        let mut form = ServeForm::default();
        assert!(form.request().is_err());
        form.allow_insecure = true;
        let request = form.request().unwrap();
        assert_eq!(request.address, "127.0.0.1:5900");
        assert_eq!((request.password, request.display), (None, None));
        form.password = "secret".into();
        assert_eq!(form.request().unwrap().password.as_deref(), Some("secret"));
        form.display = "2".into();
        assert_eq!(form.request().unwrap().display, Some(2));
        for display in ["0", "-1", "two"] {
            form.display = display.into();
            assert!(form.request().is_err());
        }
        form.display.clear();
        form.host = "::".into();
        assert!(form.request().is_err());
        form.host = "[::]".into();
        assert_eq!(form.request().unwrap().address, "[::]:5900");
        form.port = "0".into();
        assert!(form.request().is_err());
    }

    #[test]
    fn serve_scale_follows_the_slider_in_hundredths() {
        assert_eq!(serve_scale_from_slider_x(0), MIN_SERVE_SCALE);
        assert_eq!(
            serve_scale_from_slider_x(SERVE_SLIDER_LEFT),
            MIN_SERVE_SCALE
        );
        assert_eq!(serve_scale_from_slider_x(SERVE_SLIDER_RIGHT), 1.0);
        assert_eq!(serve_scale_from_slider_x(800), 1.0);
        for scale in [0.25, 0.5, 0.73, 1.0] {
            let at = serve_scale_from_slider_x(serve_slider_x(scale));
            assert!((at - scale).abs() <= 0.01, "{scale} came back as {at}");
        }
        // Each slider pixel lands on a hundredth.
        for x in SERVE_SLIDER_LEFT..=SERVE_SLIDER_RIGHT {
            let scale = serve_scale_from_slider_x(x);
            assert_eq!((scale * 100.0).round() / 100.0, scale);
        }
        // Requests carry the normalized scale.
        let mut form = ServeForm {
            allow_insecure: true,
            scale: 0.4999,
            ..ServeForm::default()
        };
        assert_eq!(form.request().unwrap().scale, 0.5);
        form.scale = 7.0;
        assert_eq!(form.request().unwrap().scale, 1.0);
    }

    #[test]
    fn serve_requests_carry_the_mouse_mode() {
        let mut form = ServeForm {
            allow_insecure: true,
            ..ServeForm::default()
        };
        assert_eq!(form.request().unwrap().mouse, MouseMode::Auto);
        for (_, mode, _) in SERVE_MOUSE {
            form.mouse = mode;
            assert_eq!(form.request().unwrap().mouse, mode);
        }
    }

    #[test]
    fn jpeg_presets_cover_the_default_and_stay_in_rfb_range() {
        let levels = JPEG_PRESETS.map(|(_, level, _)| level);
        assert!(levels.contains(&DEFAULT_JPEG_QUALITY));
        assert!(levels.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(levels.iter().all(|level| *level <= 9));
    }

    #[test]
    fn new_button_rows_fit_their_labels_without_overlapping() {
        // Button text is 12 pixels per character after a 12-pixel inset.
        let rows: [&[(Box2, &str)]; 2] = [
            &[
                (RAW, "RAW"),
                (ZLIB, "ZLIB"),
                (JPEG_PRESETS[0].0, JPEG_PRESETS[0].2),
                (JPEG_PRESETS[1].0, JPEG_PRESETS[1].2),
                (JPEG_PRESETS[2].0, JPEG_PRESETS[2].2),
            ],
            &[
                (SERVE_MOUSE[0].0, SERVE_MOUSE[0].2),
                (SERVE_MOUSE[1].0, SERVE_MOUSE[1].2),
                (SERVE_MOUSE[2].0, SERVE_MOUSE[2].2),
            ],
        ];
        for row in rows {
            for (area, name) in row {
                assert!(12 + name.len() * 12 <= area.w, "{name} overflows");
                assert!(
                    area.x + area.w <= HOST.x + PASSWORD.w,
                    "{name} past the form"
                );
            }
            for pair in row.windows(2) {
                assert!(pair[0].0.x + pair[0].0.w < pair[1].0.x);
            }
        }
        // The Server tab's password and display share one row.
        assert_eq!(SERVE_PASSWORD.y, SERVE_DISPLAY.y);
        const { assert!(SERVE_PASSWORD.x + SERVE_PASSWORD.w < SERVE_DISPLAY.x) };
        // The mouse row stays clear of the authentication checkbox text.
        let checkbox_text_end = 76 + "ALLOW NONE AUTHENTICATION".len() * 12;
        assert!(checkbox_text_end < SERVE_MOUSE[0].0.x);
    }
}
