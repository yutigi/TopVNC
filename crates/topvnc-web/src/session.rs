//! One browser connection: the Noise handshake with the host, then RFB inside
//! the channel. Transport-free: the page passes each WebSocket message to
//! [`ClientSession::receive`] and sends what [`ClientSession::take_outgoing`]
//! returns, in order.

use crate::frame::RgbaFrame;
use crate::keys::{Keyboard, text_key_events};
use std::collections::VecDeque;
use std::io;
use topvnc_e2e::{Channel, Initiator, Key, MAX_PLAINTEXT_BYTES};
use topvnc_rfb::messages::{continuous_updates_message, fence_message, update_request};
use topvnc_rfb::protocol::{FENCE_REQUEST, FENCE_SUPPORTED_FLAGS};
use topvnc_rfb::{
    CaptureStatus, ClientHandshake, Encoding, HandshakeStatus, InputState, MessageReader,
    RectDecoder, ServerInfo, ServerMessage,
};

/// The host proved the pinned key: the encrypted channel is up.
pub const EVENT_SECURE: u32 = 1 << 0;
/// The server asks for the VNC password.
pub const EVENT_PASSWORD: u32 = 1 << 1;
/// The RFB handshake finished.
pub const EVENT_CONNECTED: u32 = 1 << 2;
/// Pixels changed; see [`ClientSession::take_damage`].
pub const EVENT_FRAME: u32 = 1 << 3;
/// The framebuffer size changed.
pub const EVENT_RESIZED: u32 = 1 << 4;
pub const EVENT_BELL: u32 = 1 << 5;
/// The server switched between relative and absolute pointer motion.
pub const EVENT_POINTER_MODE: u32 = 1 << 6;
/// The host announced served-size or display controls.
pub const EVENT_CAPABILITIES: u32 = 1 << 7;
/// A round-trip time was measured.
pub const EVENT_RTT: u32 = 1 << 8;
/// The host serves more than [`MAX_WEB_FRAMEBUFFER_PIXELS`]; nothing is
/// drawn and a size that fits was asked for. See
/// [`ClientSession::oversize`].
pub const EVENT_OVERSIZE: u32 = 1 << 9;
/// The host can or can no longer capture its desktop; see
/// [`ClientSession::capture_status`].
pub const EVENT_CAPTURE_STATUS: u32 = 1 << 10;

/// The largest framebuffer the browser shows: two 4K displays side by side,
/// 64 MiB as RGBA, and the canvas area iOS Safari allows. A host serving more
/// is asked for a smaller served size, or for one display instead of all.
pub const MAX_WEB_FRAMEBUFFER_PIXELS: usize = 16_777_216;
/// Fence payload prefix for round-trip measurements.
const PING_FENCE: &[u8] = b"TopVNC.Ping/1:";

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

enum Phase {
    Handshake(Box<Initiator>),
    Rfb(ClientHandshake),
    Running(Box<Running>),
    Failed,
}

struct Running {
    info: ServerInfo,
    reader: MessageReader,
    decoder: RectDecoder,
    input: InputState,
    keyboard: Keyboard,
    frame: RgbaFrame,
    /// The server pushes updates; no requests are needed.
    continuous: bool,
    /// The size at the start of the current update.
    update_size: (u16, u16),
    scratch: Vec<u8>,
    /// The host serves more than this browser shows (`info` has its size):
    /// `frame` is a 1×1 placeholder, rectangles are decoded, to keep the
    /// zlib streams in step, but not applied, and updates ask for one pixel.
    oversize: bool,
    /// A smaller size was asked for since the size last changed.
    fit_requested: bool,
}

/// Whether the browser shows a framebuffer of this size.
fn fits(width: u16, height: u16) -> bool {
    usize::from(width) * usize::from(height) <= MAX_WEB_FRAMEBUFFER_PIXELS
}

/// The framebuffer for a host size: a placeholder while it does not fit.
fn frame_for(width: u16, height: u16) -> io::Result<RgbaFrame> {
    if fits(width, height) {
        RgbaFrame::new(width, height, MAX_WEB_FRAMEBUFFER_PIXELS)
    } else {
        topvnc_rfb::validate_framebuffer_size(width, height)?;
        RgbaFrame::new(1, 1, MAX_WEB_FRAMEBUFFER_PIXELS)
    }
}

/// The served percentage below `current` at which `width` × `height`, served
/// at `current`, fits; `None` below the host's 25% minimum.
pub(crate) fn fitting_scale(current: u8, width: u16, height: u16) -> Option<u8> {
    let pixels = f64::from(width) * f64::from(height);
    let ratio = (MAX_WEB_FRAMEBUFFER_PIXELS as f64 / pixels).sqrt();
    let percent = (f64::from(current) * ratio)
        .floor()
        .min(f64::from(current) - 1.0);
    (percent >= 25.0).then_some(percent as u8)
}

impl Running {
    /// The region updates ask for: one pixel while nothing is drawn.
    fn request_size(&self) -> (u16, u16) {
        if self.oversize {
            (1, 1)
        } else {
            (self.info.width, self.info.height)
        }
    }

    /// Ask the host for a size that fits, at most once per size: a smaller
    /// served size keeps the chosen displays, else one display instead of
    /// all. Both are host-wide choices.
    fn ask_to_fit(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        if !self.oversize || self.fit_requested {
            return Ok(());
        }
        let (width, height) = (self.info.width, self.info.height);
        if let Some(percent) = self
            .input
            .serve_scale()
            .and_then(|current| fitting_scale(current, width, height))
        {
            self.input.request_serve_scale(percent, out)?;
        } else if self
            .input
            .serve_display()
            .is_some_and(|displays| displays.served == 0 && displays.count > 0)
        {
            self.input.request_serve_display(1, out)?;
        } else {
            return Err(invalid(
                "the host's screen is larger than this browser can show, and the host cannot share it smaller",
            ));
        }
        self.fit_requested = true;
        Ok(())
    }
}

pub struct ClientSession {
    phase: Phase,
    channel: Option<Channel>,
    /// Plaintext waiting to be sealed.
    plaintext: Vec<u8>,
    /// Sealed WebSocket messages, in order.
    outgoing: VecDeque<Vec<u8>>,
    encoding: Encoding,
    /// RFB bytes received, for bandwidth statistics.
    received: u64,
    frames: u64,
    ping: Option<(u32, f64)>,
    next_ping: u32,
    rtt: Option<f64>,
}

impl ClientSession {
    /// Pins `host`. With `pairing_secret`, this connection enrolls `browser`.
    pub fn new(
        browser: &Key,
        host: &Key,
        pairing_secret: Option<&Key>,
        quality: u8,
    ) -> io::Result<Self> {
        let (initiator, first) =
            Initiator::start(browser, host, pairing_secret).map_err(invalid)?;
        Ok(Self {
            phase: Phase::Handshake(Box::new(initiator)),
            channel: None,
            plaintext: Vec::new(),
            outgoing: VecDeque::from([first]),
            encoding: Encoding::Tight {
                quality: quality.min(9),
            },
            received: 0,
            frames: 0,
            ping: None,
            next_ping: 0,
            rtt: None,
        })
    }

    pub fn take_outgoing(&mut self) -> Option<Vec<u8>> {
        self.outgoing.pop_front()
    }

    /// Handle one WebSocket message from the gateway. Returns `EVENT_*`
    /// bits. After an error the session is failed and the page should close.
    pub fn receive(&mut self, message: &[u8], now: f64) -> io::Result<u32> {
        let result = self.receive_inner(message, now);
        if result.is_err() {
            self.phase = Phase::Failed;
        }
        result
    }

    fn receive_inner(&mut self, message: &[u8], now: f64) -> io::Result<u32> {
        if let Phase::Handshake(_) = self.phase {
            let Phase::Handshake(initiator) = std::mem::replace(&mut self.phase, Phase::Failed)
            else {
                unreachable!()
            };
            let (last, channel) = initiator.finish(message).map_err(invalid)?;
            self.outgoing.push_back(last);
            self.channel = Some(channel);
            // The browser never accepts None security.
            self.phase = Phase::Rfb(ClientHandshake::new(false, self.encoding));
            return Ok(EVENT_SECURE);
        }
        let channel = self
            .channel
            .as_mut()
            .ok_or_else(|| invalid("the connection has failed"))?;
        let plaintext = channel.open(message).map_err(invalid)?;
        self.received += plaintext.len() as u64;
        let mut events = 0;
        match &mut self.phase {
            Phase::Rfb(handshake) => {
                let status = handshake.push(&plaintext)?;
                events |= self.after_handshake(status)?;
            }
            Phase::Running(running) => running.reader.push(&plaintext)?,
            Phase::Handshake(_) | Phase::Failed => {
                return Err(invalid("the connection has failed"));
            }
        }
        if let Phase::Running(_) = self.phase {
            events |= self.process(now)?;
        }
        self.flush()?;
        Ok(events)
    }

    /// Answer the server's VNC password challenge. The password is used once
    /// and not kept.
    pub fn password(&mut self, password: &str, now: f64) -> io::Result<u32> {
        let Phase::Rfb(handshake) = &mut self.phase else {
            return Err(invalid("the server did not ask for a password"));
        };
        let status = handshake.password(password)?;
        let mut events = self.after_handshake(status)?;
        if let Phase::Running(_) = self.phase {
            events |= self.process(now)?;
        }
        self.flush()?;
        Ok(events)
    }

    fn after_handshake(&mut self, status: HandshakeStatus) -> io::Result<u32> {
        let Phase::Rfb(handshake) = &mut self.phase else {
            unreachable!("called during the RFB handshake")
        };
        self.plaintext.extend(handshake.take_output());
        let info = match status {
            HandshakeStatus::NeedMore => return Ok(0),
            HandshakeStatus::NeedPassword => return Ok(EVENT_PASSWORD),
            HandshakeStatus::Done(info) => info,
        };
        let Phase::Rfb(handshake) = std::mem::replace(&mut self.phase, Phase::Failed) else {
            unreachable!()
        };
        // A host still serving a size this browser cannot show, such as all
        // displays chosen from another viewer, is asked for a smaller one
        // once its controls arrive with the first update.
        let frame = frame_for(info.width, info.height)?;
        let oversize = !fits(info.width, info.height);
        // The reader keeps the protocol's own limit, so an oversized host
        // can be read until it serves a size that fits.
        let mut reader = MessageReader::new(info.width, info.height, self.encoding);
        reader.push(&handshake.into_leftover())?;
        let (width, height) = if oversize {
            (1, 1)
        } else {
            (info.width, info.height)
        };
        self.plaintext
            .extend_from_slice(&update_request(false, width, height));
        let events = if oversize { EVENT_OVERSIZE } else { 0 };
        self.phase = Phase::Running(Box::new(Running {
            oversize,
            fit_requested: false,
            update_size: (info.width, info.height),
            input: InputState::new(self.encoding.advertised()),
            reader,
            decoder: RectDecoder::new(),
            keyboard: Keyboard::default(),
            frame,
            continuous: false,
            scratch: Vec::new(),
            info,
        }));
        Ok(EVENT_CONNECTED | EVENT_RESIZED | EVENT_FRAME | events)
    }

    /// Apply every complete server message.
    fn process(&mut self, now: f64) -> io::Result<u32> {
        let Phase::Running(running) = &mut self.phase else {
            return Ok(0);
        };
        let running = running.as_mut();
        let out = &mut self.plaintext;
        let mut events = 0;
        while let Some(message) = running.reader.next_message()? {
            match message {
                ServerMessage::UpdateStart => {
                    running.update_size = (running.info.width, running.info.height)
                }
                ServerMessage::DesktopSize { width, height } => {
                    if fits(width, height) {
                        if running.oversize {
                            running.frame =
                                RgbaFrame::new(width, height, MAX_WEB_FRAMEBUFFER_PIXELS)?;
                        } else {
                            running
                                .frame
                                .resize(width, height, MAX_WEB_FRAMEBUFFER_PIXELS)?;
                        }
                        running.oversize = false;
                    } else {
                        // Free the old framebuffer rather than keep its size.
                        running.frame = frame_for(width, height)?;
                        running.oversize = true;
                        events |= EVENT_OVERSIZE;
                    }
                    running.fit_requested = false;
                    (running.info.width, running.info.height) = (width, height);
                    events |= EVENT_RESIZED | EVENT_FRAME;
                }
                ServerMessage::ServeScale(percent) => {
                    running.input.set_serve_scale(percent);
                    events |= EVENT_CAPABILITIES;
                }
                ServerMessage::ServeDisplay(displays) => {
                    running.input.set_serve_display(displays);
                    events |= EVENT_CAPABILITIES;
                }
                ServerMessage::CaptureStatus(status) => {
                    if running.input.capture_status() != status {
                        running.input.set_capture_status(status);
                        events |= EVENT_CAPTURE_STATUS;
                    }
                }
                ServerMessage::PointerMode { relative } => {
                    running.input.server_pointer_mode(relative);
                    events |= EVENT_POINTER_MODE;
                }
                ServerMessage::ExtendedMouseButtons => running.input.server_extended_buttons(),
                ServerMessage::Rect(rect) => {
                    let pixels = running.decoder.decode_all(&rect, &mut running.scratch)?;
                    if !running.oversize {
                        running.frame.apply_bgrx(
                            rect.x,
                            rect.y,
                            rect.width,
                            rect.height,
                            pixels,
                        )?;
                        events |= EVENT_FRAME;
                    }
                }
                ServerMessage::UpdateEnd => {
                    self.frames += 1;
                    // The update's capabilities are known by now.
                    running.ask_to_fit(out)?;
                    let (width, height) = running.request_size();
                    if (running.info.width, running.info.height) != running.update_size {
                        if running.continuous {
                            out.extend_from_slice(&continuous_updates_message(true, width, height));
                        }
                        out.extend_from_slice(&update_request(false, width, height));
                    } else if !running.continuous {
                        out.extend_from_slice(&update_request(true, width, height));
                    }
                }
                ServerMessage::Bell => events |= EVENT_BELL,
                // Clipboard sharing is not implemented in the browser yet.
                ServerMessage::CutText(_) => {}
                ServerMessage::EndOfContinuousUpdates => {
                    let (width, height) = running.request_size();
                    if running.continuous {
                        // The server stopped pushing; go back to requests.
                        running.continuous = false;
                        out.extend_from_slice(&update_request(true, width, height));
                    } else {
                        running.continuous = true;
                        out.extend_from_slice(&continuous_updates_message(true, width, height));
                    }
                }
                // Messages are handled in order, which satisfies every
                // supported flag, and the reply leaves after the updates
                // before it were applied: the server's flow control sees
                // how fast this browser really is.
                ServerMessage::Fence { flags, payload } if flags & FENCE_REQUEST != 0 => {
                    out.extend_from_slice(&fence_message(flags & FENCE_SUPPORTED_FLAGS, payload));
                }
                ServerMessage::Fence { payload, .. } => {
                    if let Some((id, sent)) = self.ping
                        && payload.strip_prefix(PING_FENCE) == Some(&id.to_be_bytes()[..])
                    {
                        self.rtt = Some((now - sent).max(0.0));
                        self.ping = None;
                        events |= EVENT_RTT;
                    }
                }
            }
        }
        Ok(events)
    }

    /// Seal pending plaintext into WebSocket messages.
    fn flush(&mut self) -> io::Result<()> {
        if self.plaintext.is_empty() {
            return Ok(());
        }
        let channel = self
            .channel
            .as_mut()
            .ok_or_else(|| invalid("the connection has failed"))?;
        for chunk in self.plaintext.chunks(MAX_PLAINTEXT_BYTES) {
            self.outgoing
                .push_back(channel.seal(chunk).map_err(invalid)?);
        }
        self.plaintext.clear();
        Ok(())
    }

    fn running(&mut self) -> Option<&mut Running> {
        match &mut self.phase {
            Phase::Running(running) => Some(running),
            _ => None,
        }
    }

    /// Run `send` on the input state of a running session and seal what it
    /// wrote. Input before the session runs is dropped, never queued.
    fn input(
        &mut self,
        send: impl FnOnce(&mut Running, &mut Vec<u8>) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut out = std::mem::take(&mut self.plaintext);
        let result = match self.running() {
            Some(running) => send(running, &mut out),
            None => Ok(()),
        };
        self.plaintext = out;
        result?;
        self.flush()
    }

    /// Pointer input is dropped while nothing is drawn: the page has no
    /// picture to aim at.
    pub fn pointer(&mut self, buttons: u16, x: u16, y: u16) -> io::Result<()> {
        self.input(|running, out| {
            if running.oversize {
                return Ok(());
            }
            let x = x.min(running.info.width.saturating_sub(1));
            let y = y.min(running.info.height.saturating_sub(1));
            running.input.pointer(buttons, x, y, out);
            Ok(())
        })
    }

    pub fn pointer_motion(&mut self, buttons: u16, dx: i32, dy: i32) -> io::Result<()> {
        self.input(|running, out| {
            if running.oversize {
                return Ok(());
            }
            running.input.pointer_motion(buttons, dx, dy, out);
            Ok(())
        })
    }

    /// A physical key by `KeyboardEvent.code`. Returns whether it was sent.
    pub fn key_code(&mut self, code: &str, down: bool) -> io::Result<bool> {
        let mut sent = false;
        self.input(|running, out| {
            if let Some((keysym, down)) = running.keyboard.event(code, down) {
                running.input.key(keysym, down, out);
                sent = true;
            }
            Ok(())
        })?;
        Ok(sent)
    }

    /// A keysym press or release, for toolbar keys.
    pub fn key_sym(&mut self, keysym: u32, down: bool) -> io::Result<()> {
        self.input(|running, out| {
            running.input.key(keysym, down, out);
            Ok(())
        })
    }

    /// Type text from an on-screen keyboard or input method.
    pub fn type_text(&mut self, text: &str) -> io::Result<()> {
        self.input(|running, out| {
            let shift_held = running.keyboard.shift_held()
                || running
                    .input
                    .held_keys()
                    .iter()
                    .any(|key| matches!(key, 0xffe1 | 0xffe2));
            for (keysym, down) in text_key_events(text, shift_held) {
                running.input.key(keysym, down, out);
            }
            Ok(())
        })
    }

    /// Release every held key and button on the host, for focus loss, page
    /// hiding, a cancelled gesture, or disconnecting.
    pub fn release_all(&mut self) -> io::Result<()> {
        self.input(|running, out| {
            running.input.release_all(out);
            running.keyboard.clear();
            Ok(())
        })
    }

    /// Ask for another JPEG quality, 0 (smallest) to 9 (best).
    pub fn set_quality(&mut self, quality: u8) -> io::Result<()> {
        let encoding = Encoding::Tight {
            quality: quality.min(9),
        };
        self.encoding = encoding;
        self.input(|running, out| {
            running.input.advertise(encoding.advertised(), out);
            Ok(())
        })
    }

    pub fn request_serve_scale(&mut self, percent: u8) -> io::Result<()> {
        self.input(|running, out| running.input.request_serve_scale(percent, out))
    }

    pub fn request_serve_display(&mut self, display: u8) -> io::Result<()> {
        self.input(|running, out| running.input.request_serve_display(display, out))
    }

    /// Send a round-trip probe unless one is outstanding.
    pub fn ping(&mut self, now: f64) -> io::Result<()> {
        if self.ping.is_some() || self.running().is_none() {
            return Ok(());
        }
        let id = self.next_ping;
        self.next_ping = self.next_ping.wrapping_add(1);
        self.ping = Some((id, now));
        let mut payload = PING_FENCE.to_vec();
        payload.extend_from_slice(&id.to_be_bytes());
        self.plaintext
            .extend_from_slice(&fence_message(FENCE_REQUEST, &payload));
        self.flush()
    }

    pub fn frame(&self) -> Option<&RgbaFrame> {
        match &self.phase {
            Phase::Running(running) => Some(&running.frame),
            _ => None,
        }
    }

    pub fn take_damage(&mut self) -> Vec<topvnc_rfb::DamageRect> {
        self.running()
            .map(|running| running.frame.take_damage())
            .unwrap_or_default()
    }

    pub fn server_name(&self) -> Option<&str> {
        match &self.phase {
            Phase::Running(running) => Some(&running.info.name),
            _ => None,
        }
    }

    pub fn relative_pointer(&self) -> bool {
        matches!(&self.phase, Phase::Running(running) if running.input.relative_pointer())
    }

    pub fn serve_scale(&self) -> Option<u8> {
        match &self.phase {
            Phase::Running(running) => running.input.serve_scale(),
            _ => None,
        }
    }

    pub fn served_displays(&self) -> Option<topvnc_rfb::ServedDisplays> {
        match &self.phase {
            Phase::Running(running) => running.input.serve_display(),
            _ => None,
        }
    }

    /// Whether the host can capture its desktop; live unless it said not.
    pub fn capture_status(&self) -> CaptureStatus {
        match &self.phase {
            Phase::Running(running) => running.input.capture_status(),
            _ => CaptureStatus::Live,
        }
    }

    pub fn continuous_updates(&self) -> bool {
        matches!(&self.phase, Phase::Running(running) if running.continuous)
    }

    /// The host's size while it serves more than this browser shows.
    pub fn oversize(&self) -> Option<(u16, u16)> {
        match &self.phase {
            Phase::Running(running) if running.oversize => {
                Some((running.info.width, running.info.height))
            }
            _ => None,
        }
    }

    pub fn received_bytes(&self) -> u64 {
        self.received
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn rtt(&self) -> Option<f64> {
        self.rtt
    }
}
