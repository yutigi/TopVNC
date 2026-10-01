//! Shared RFB protocol and framebuffer code for TopVNC.

#![forbid(unsafe_code)]

use des::Des;
use des::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use flate2::{Decompress, FlushDecompress};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MAX_FRAMEBUFFER_DIMENSION: u16 = 8192;
pub const MAX_FRAMEBUFFER_PIXELS: usize = 33_554_432;
const MAX_NAME_BYTES: usize = 4096;
const MAX_CLIENT_CLIPBOARD_BYTES: usize = 1_048_576;
const SERVER_TILE_SIZE: usize = 64;
const SERVER_EVENT_QUEUE_CAPACITY: usize = 64;
const SERVER_SESSION_QUEUE_CAPACITY: usize = 64;
/// How long an incremental request without changes waits before it is
/// answered with an empty update. A change answers it immediately.
const SERVER_EMPTY_UPDATE_INTERVAL: Duration = Duration::from_millis(50);
const SERVER_IDLE_WAKE_INTERVAL: Duration = Duration::from_secs(1);
const SERVER_WRITE_CHUNK_BYTES: usize = 64 * 1024;
const DESKTOP_SIZE_ENCODING: i32 = -223;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const UPDATE_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

fn timed_out(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, message)
}

// Keep read_exact's partial progress intact when an idle server needs a refresh.
// Restarting the parser on a timeout would interpret pixel bytes as a new message.
struct RefreshReader<R, F> {
    reader: R,
    refresh: F,
    requested: bool,
}

impl<R: Read, F: FnMut() -> io::Result<()>> Read for RefreshReader<R, F> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.reader.read(bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    if self.requested {
                        return Err(timed_out(
                            "Full screen refresh timed out. Reconnect to the server.",
                        ));
                    }
                    (self.refresh)()?;
                    self.requested = true;
                }
                result => return result,
            }
        }
    }
}

struct HandshakeStream<'a> {
    stream: &'a mut TcpStream,
    deadline: Instant,
}

impl HandshakeStream<'_> {
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| timed_out("Connection handshake timed out"))
    }
}

impl Read for HandshakeStream<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(bytes)
    }
}

impl Write for HandshakeStream<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

fn connect_tcp(address: &str) -> io::Result<TcpStream> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    // DNS may block independently of TCP. Bound the caller's wait as well.
    let address = address.to_owned();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = address
            .to_socket_addrs()
            .map(|addresses| addresses.collect::<Vec<_>>());
        let _ = sender.send(result);
    });
    let addresses = receiver
        .recv_timeout(CONNECT_TIMEOUT)
        .map_err(|_| timed_out("Server address lookup timed out"))??;
    let mut last_error = io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "Server address has no IP addresses",
    );
    for address in addresses {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| timed_out("TCP connection timed out"))?;
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Raw,
    Zlib,
}

/// Events received from a remote RFB client connected to a [`VncServer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    Key {
        client_id: u64,
        keysym: u32,
        down: bool,
    },
    Pointer {
        client_id: u64,
        buttons: u8,
        x: u16,
        y: u16,
    },
    /// Text copied by a remote client, encoded as RFB Latin-1 bytes.
    ClipboardText { client_id: u64, text: Vec<u8> },
    /// One client disconnected; release only the input state it owned.
    ClientDisconnected { client_id: u64 },
}

/// A framebuffer region the host changed, in framebuffer pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// Configuration for the embedded RFB server. Password authentication is the
/// default; `None` security must be selected explicitly.
#[derive(Clone)]
pub struct ServerConfig {
    pub name: String,
    pub password: Option<String>,
    pub allow_insecure: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "TopVNC".into(),
            password: None,
            allow_insecure: false,
        }
    }
}

/// A small RFB 3.8 server for applications that provide a framebuffer and
/// consume remote input events. It serves Raw rectangles to concurrent clients
/// and announces size changes with the DesktopSize pseudo-encoding.
/// The caller owns display capture and OS input injection.
pub struct VncServer {
    listener: std::net::TcpListener,
    framebuffer: Arc<Mutex<ServerFramebuffer>>,
    events: std::sync::mpsc::SyncSender<ClientEvent>,
    event_receiver: Mutex<std::sync::mpsc::Receiver<ClientEvent>>,
    config: ServerConfig,
    active_clients: Arc<std::sync::atomic::AtomicUsize>,
    stopped: std::sync::atomic::AtomicBool,
    next_client_id: std::sync::atomic::AtomicU64,
    sessions: Arc<Mutex<ServerSessions>>,
    clipboard: Arc<Mutex<ServerClipboard>>,
}

#[derive(Default)]
struct ServerClipboard {
    revision: u64,
    text: Option<Vec<u8>>,
}

/// Messages delivered to one client's update writer.
enum SessionInput {
    PixelFormat(ServerPixelFormat),
    Encodings {
        desktop_size: bool,
    },
    UpdateRequest(UpdateRequest),
    /// The framebuffer or clipboard changed.
    Wake,
    /// The client's reader stopped; the session ends with this error.
    Closed(io::Error),
}

/// Wakes an idle update writer. At most one wake is queued at a time so a
/// fast-changing framebuffer cannot fill the session queue.
struct SessionWaker {
    sender: SyncSender<SessionInput>,
    queued: Arc<AtomicBool>,
}

impl SessionWaker {
    fn wake(&self) {
        if !self.queued.swap(true, Ordering::AcqRel)
            && self.sender.try_send(SessionInput::Wake).is_err()
        {
            self.queued.store(false, Ordering::Release);
        }
    }
}

#[derive(Default)]
struct ServerSessions {
    streams: std::collections::HashMap<u64, TcpStream>,
    wakers: std::collections::HashMap<u64, SessionWaker>,
    exclusive_client: Option<u64>,
}

impl ServerSessions {
    fn watch(&mut self, client_id: u64, waker: SessionWaker) {
        self.wakers.insert(client_id, waker);
    }

    fn wake_all(&self) {
        for waker in self.wakers.values() {
            waker.wake();
        }
    }

    fn register(&mut self, client_id: u64, stream: TcpStream) -> io::Result<()> {
        if self.exclusive_client.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "an exclusive RFB session is active",
            ));
        }
        self.streams.insert(client_id, stream);
        Ok(())
    }

    fn admit(&mut self, client_id: u64, shared: bool) -> io::Result<()> {
        if !self.streams.contains_key(&client_id) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "RFB session is no longer registered",
            ));
        }
        match (shared, self.exclusive_client) {
            (true, Some(exclusive)) if exclusive != client_id => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "an exclusive RFB session is active",
                ));
            }
            (false, Some(exclusive)) if exclusive != client_id => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "another exclusive RFB session is active",
                ));
            }
            (false, _) => {
                for (other_id, stream) in &self.streams {
                    if *other_id != client_id {
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                    }
                }
                self.exclusive_client = Some(client_id);
            }
            _ => {}
        }
        Ok(())
    }

    fn remove(&mut self, client_id: u64) {
        self.streams.remove(&client_id);
        self.wakers.remove(&client_id);
        if self.exclusive_client == Some(client_id) {
            self.exclusive_client = None;
        }
    }

    fn shutdown_all(&self) {
        for stream in self.streams.values() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

struct ServerFramebuffer {
    framebuffer: Framebuffer,
    tile_revisions: Vec<u64>,
    revision: u64,
    tile_columns: usize,
    /// Incremented whenever the framebuffer dimensions change.
    generation: u64,
}

impl ServerFramebuffer {
    fn new(framebuffer: Framebuffer) -> Self {
        let tile_columns = framebuffer.width().div_ceil(SERVER_TILE_SIZE);
        let tile_rows = framebuffer.height().div_ceil(SERVER_TILE_SIZE);
        Self {
            framebuffer,
            tile_revisions: vec![0; tile_columns * tile_rows],
            revision: 0,
            tile_columns,
            generation: 0,
        }
    }

    fn resize(&mut self, framebuffer: Framebuffer) {
        let generation = self.generation.wrapping_add(1);
        *self = Self::new(framebuffer);
        self.generation = generation;
    }

    fn next_revision(&mut self) -> u64 {
        if self.revision == u64::MAX {
            self.revision = 0;
            self.tile_revisions.fill(0);
        }
        self.revision += 1;
        self.revision
    }

    /// Copy one tile from `source` and bump its revision if any pixel changed.
    fn sync_tile(&mut self, index: usize, source: &Framebuffer) {
        let (x, y, width, height) = self.tile_rect(index);
        let stride = self.framebuffer.width();
        let mut changed = false;
        for row in y..y + height {
            let start = row * stride + x;
            let end = start + width;
            if self.framebuffer.pixels[start..end] != source.pixels[start..end] {
                self.framebuffer.pixels[start..end].copy_from_slice(&source.pixels[start..end]);
                changed = true;
            }
        }
        if changed {
            let revision = self.next_revision();
            self.tile_revisions[index] = revision;
        }
    }

    fn tile_rect(&self, index: usize) -> (usize, usize, usize, usize) {
        let tile_x = index % self.tile_columns;
        let tile_y = index / self.tile_columns;
        let x = tile_x * SERVER_TILE_SIZE;
        let y = tile_y * SERVER_TILE_SIZE;
        (
            x,
            y,
            SERVER_TILE_SIZE.min(self.framebuffer.width() - x),
            SERVER_TILE_SIZE.min(self.framebuffer.height() - y),
        )
    }
}

impl VncServer {
    pub fn bind(
        address: impl ToSocketAddrs,
        framebuffer: Framebuffer,
        config: ServerConfig,
    ) -> io::Result<Self> {
        if config
            .password
            .as_ref()
            .is_some_and(|password| password.is_empty())
        {
            return Err(invalid("server password must not be empty"));
        }
        if config.password.is_none() && !config.allow_insecure {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "server requires a password unless allow_insecure is explicitly enabled",
            ));
        }
        let listener = std::net::TcpListener::bind(address)?;
        if config.name.len() > MAX_NAME_BYTES {
            return Err(invalid("server name is too long"));
        }
        let (events, event_receiver) = std::sync::mpsc::sync_channel(SERVER_EVENT_QUEUE_CAPACITY);
        Ok(Self {
            listener,
            framebuffer: Arc::new(Mutex::new(ServerFramebuffer::new(framebuffer))),
            events,
            event_receiver: Mutex::new(event_receiver),
            config,
            active_clients: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            stopped: std::sync::atomic::AtomicBool::new(false),
            next_client_id: std::sync::atomic::AtomicU64::new(1),
            sessions: Arc::new(Mutex::new(ServerSessions::default())),
            clipboard: Arc::new(Mutex::new(ServerClipboard::default())),
        })
    }

    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Open client connections, including ones still authenticating.
    pub fn active_connections(&self) -> usize {
        self.active_clients
            .load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn try_event(&self) -> Result<ClientEvent, std::sync::mpsc::TryRecvError> {
        match self.event_receiver.lock() {
            Ok(receiver) => receiver.try_recv(),
            Err(_) => Err(std::sync::mpsc::TryRecvError::Disconnected),
        }
    }

    pub fn recv_event_timeout(
        &self,
        timeout: Duration,
    ) -> Result<ClientEvent, std::sync::mpsc::RecvTimeoutError> {
        match self.event_receiver.lock() {
            Ok(receiver) => receiver.recv_timeout(timeout),
            Err(_) => Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
        }
    }

    /// Replace the current desktop image. When the dimensions change, clients
    /// that advertised the DesktopSize pseudo-encoding receive the new size;
    /// other clients are disconnected because they cannot follow the change.
    pub fn update_framebuffer(&self, framebuffer: &Framebuffer) -> io::Result<()> {
        update_server_framebuffer(&self.framebuffer, framebuffer)?;
        self.wake_sessions();
        Ok(())
    }

    /// Like [`VncServer::update_framebuffer`], but only compares the damaged
    /// regions; pixels outside them must be unchanged since the last update.
    /// A dimension change replaces the whole framebuffer regardless of damage.
    pub fn update_framebuffer_regions(
        &self,
        framebuffer: &Framebuffer,
        damage: &[DamageRect],
    ) -> io::Result<()> {
        update_server_framebuffer_regions(&self.framebuffer, framebuffer, damage)?;
        self.wake_sessions();
        Ok(())
    }

    fn wake_sessions(&self) {
        if let Ok(sessions) = self.sessions.lock() {
            sessions.wake_all();
        }
    }

    /// Send Latin-1 clipboard text to connected clients promptly, including idle clients.
    /// An empty slice clears the remote clipboard.
    pub fn set_clipboard_text(&self, text: &[u8]) -> io::Result<()> {
        if text.len() > MAX_CLIENT_CLIPBOARD_BYTES {
            return Err(invalid("server clipboard text too large"));
        }
        let mut clipboard = self
            .clipboard
            .lock()
            .map_err(|_| invalid("clipboard lock is poisoned"))?;
        clipboard.revision = clipboard.revision.wrapping_add(1);
        if clipboard.revision == 0 {
            clipboard.revision = 1;
        }
        clipboard.text = Some(text.to_vec());
        drop(clipboard);
        self.wake_sessions();
        Ok(())
    }

    /// Stop accepting clients and close all active client sessions.
    pub fn stop(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        if let Ok(sessions) = self.sessions.lock() {
            sessions.shutdown_all();
        }
    }

    /// Accept clients until [`VncServer::stop`] is called or an I/O error occurs.
    pub fn run(&self) -> io::Result<()> {
        self.listener.set_nonblocking(true)?;
        while !self.stopped.load(std::sync::atomic::Ordering::Acquire) {
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => return Err(error),
            };
            stream.set_nonblocking(false)?;
            if self
                .active_clients
                .fetch_update(
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                    |count| (count < 8).then_some(count + 1),
                )
                .is_err()
            {
                drop(stream);
                continue;
            }
            let client_id = self
                .next_client_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let sessions = Arc::clone(&self.sessions);
            let registration = stream.try_clone().and_then(|session_stream| {
                sessions
                    .lock()
                    .map_err(|_| invalid("session registry lock is poisoned"))?
                    .register(client_id, session_stream)
            });
            if registration.is_err() {
                self.active_clients
                    .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                drop(stream);
                continue;
            }
            let framebuffer = Arc::clone(&self.framebuffer);
            let events = self.events.clone();
            let config = self.config.clone();
            let active_clients = Arc::clone(&self.active_clients);
            let client_sessions = Arc::clone(&self.sessions);
            let clipboard = Arc::clone(&self.clipboard);
            let result = std::thread::Builder::new()
                .name("topvnc-rfb-client".into())
                .spawn(move || {
                    struct ClientSlot {
                        active: Arc<std::sync::atomic::AtomicUsize>,
                        sessions: Arc<Mutex<ServerSessions>>,
                        client_id: u64,
                    }
                    impl Drop for ClientSlot {
                        fn drop(&mut self) {
                            self.active
                                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                            if let Ok(mut sessions) = self.sessions.lock() {
                                sessions.remove(self.client_id);
                            }
                        }
                    }
                    let _slot = ClientSlot {
                        active: active_clients,
                        sessions: client_sessions.clone(),
                        client_id,
                    };
                    let mut stream = stream;
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
                    let _ = serve_client(
                        &mut stream,
                        &framebuffer,
                        &events,
                        &config,
                        &client_sessions,
                        &clipboard,
                        client_id,
                    );
                    let _ = events.send(ClientEvent::ClientDisconnected { client_id });
                });
            if let Err(error) = result {
                self.active_clients
                    .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                return Err(error);
            }
        }
        Ok(())
    }
}

fn update_server_framebuffer(
    shared: &Arc<Mutex<ServerFramebuffer>>,
    framebuffer: &Framebuffer,
) -> io::Result<()> {
    update_server_framebuffer_regions(
        shared,
        framebuffer,
        &[DamageRect {
            x: 0,
            y: 0,
            width: framebuffer.width,
            height: framebuffer.height,
        }],
    )
}

fn update_server_framebuffer_regions(
    shared: &Arc<Mutex<ServerFramebuffer>>,
    framebuffer: &Framebuffer,
    damage: &[DamageRect],
) -> io::Result<()> {
    let mut current = shared
        .lock()
        .map_err(|_| invalid("framebuffer lock is poisoned"))?;
    if current.framebuffer.width != framebuffer.width
        || current.framebuffer.height != framebuffer.height
    {
        current.resize(framebuffer.clone());
        return Ok(());
    }
    if damage.iter().any(|rect| {
        usize::from(rect.x) + usize::from(rect.width) > framebuffer.width()
            || usize::from(rect.y) + usize::from(rect.height) > framebuffer.height()
    }) {
        return Err(invalid("damage rectangle is outside framebuffer"));
    }
    let mut visited = vec![false; current.tile_revisions.len()];
    for rect in damage {
        if rect.width == 0 || rect.height == 0 {
            continue;
        }
        let column_start = usize::from(rect.x) / SERVER_TILE_SIZE;
        let column_end = (usize::from(rect.x) + usize::from(rect.width) - 1) / SERVER_TILE_SIZE;
        let row_start = usize::from(rect.y) / SERVER_TILE_SIZE;
        let row_end = (usize::from(rect.y) + usize::from(rect.height) - 1) / SERVER_TILE_SIZE;
        for tile_row in row_start..=row_end {
            for tile_column in column_start..=column_end {
                let index = tile_row * current.tile_columns + tile_column;
                if !std::mem::replace(&mut visited[index], true) {
                    current.sync_tile(index, framebuffer);
                }
            }
        }
    }
    Ok(())
}

fn send_pending_clipboard(
    stream: &mut TcpStream,
    clipboard: &Arc<Mutex<ServerClipboard>>,
    seen_revision: &mut u64,
) -> io::Result<()> {
    let (revision, text) = {
        let clipboard = clipboard
            .lock()
            .map_err(|_| invalid("clipboard lock is poisoned"))?;
        (clipboard.revision, clipboard.text.clone())
    };
    if revision == *seen_revision {
        return Ok(());
    }
    *seen_revision = revision;
    if let Some(text) = text {
        stream.write_all(&[3, 0, 0, 0])?;
        stream.write_all(&(text.len() as u32).to_be_bytes())?;
        stream.write_all(&text)?;
    }
    Ok(())
}

fn serve_client(
    stream: &mut TcpStream,
    framebuffer: &Arc<Mutex<ServerFramebuffer>>,
    events: &SyncSender<ClientEvent>,
    config: &ServerConfig,
    sessions: &Arc<Mutex<ServerSessions>>,
    clipboard: &Arc<Mutex<ServerClipboard>>,
    client_id: u64,
) -> io::Result<()> {
    stream.write_all(b"RFB 003.008\n")?;
    let mut version = [0; 12];
    stream.read_exact(&mut version)?;
    if &version != b"RFB 003.008\n" {
        return Err(invalid("unsupported RFB client version"));
    }
    let security = if config.password.is_some() { 2 } else { 1 };
    stream.write_all(&[1, security])?;
    let mut selected = [0];
    stream.read_exact(&mut selected)?;
    if selected[0] != security {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "client selected unsupported security",
        ));
    }
    if let Some(password) = &config.password {
        let mut challenge = [0u8; 16];
        // Per-connection unpredictable challenge.
        getrandom::fill(&mut challenge)
            .map_err(|_| io::Error::other("secure random challenge generation failed"))?;
        stream.write_all(&challenge)?;
        let mut answer = [0; 16];
        stream.read_exact(&mut answer)?;
        if !constant_time_equal(&answer, &vnc_response(&challenge, password)) {
            let reason = b"VNC authentication failed";
            stream.write_all(&1u32.to_be_bytes())?;
            stream.write_all(&(reason.len() as u32).to_be_bytes())?;
            stream.write_all(reason)?;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "VNC authentication failed",
            ));
        }
    }
    stream.write_all(&0u32.to_be_bytes())?;
    let mut shared = [0];
    stream.read_exact(&mut shared)?;
    if shared[0] > 1 {
        return Err(invalid("invalid ClientInit shared flag"));
    }
    sessions
        .lock()
        .map_err(|_| invalid("session registry lock is poisoned"))?
        .admit(client_id, shared[0] != 0)?;
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    let name = config.name.as_bytes();
    if name.len() > MAX_NAME_BYTES {
        return Err(invalid("server name is too long"));
    }
    let (generation, tile_count) = {
        let fb = framebuffer
            .lock()
            .map_err(|_| invalid("framebuffer lock is poisoned"))?;
        stream.write_all(&fb.framebuffer.width.to_be_bytes())?;
        stream.write_all(&fb.framebuffer.height.to_be_bytes())?;
        (fb.generation, fb.tile_revisions.len())
    };
    stream.write_all(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0])?;
    stream.write_all(&(name.len() as u32).to_be_bytes())?;
    stream.write_all(name)?;

    // Client messages are read on their own thread so that key and pointer
    // events reach the host immediately, even while this thread is writing a
    // large framebuffer update.
    let (session_sender, session_receiver) =
        std::sync::mpsc::sync_channel(SERVER_SESSION_QUEUE_CAPACITY);
    let wake_queued = Arc::new(AtomicBool::new(false));
    sessions
        .lock()
        .map_err(|_| invalid("session registry lock is poisoned"))?
        .watch(
            client_id,
            SessionWaker {
                sender: session_sender.clone(),
                queued: Arc::clone(&wake_queued),
            },
        );
    let resized = Arc::new(AtomicBool::new(false));
    let reader = {
        let mut stream = stream.try_clone()?;
        let framebuffer = Arc::clone(framebuffer);
        let events = events.clone();
        let resized = Arc::clone(&resized);
        std::thread::Builder::new()
            .name("topvnc-rfb-reader".into())
            .spawn(move || {
                let error = loop {
                    if let Err(error) = read_client_message(
                        &mut stream,
                        &session_sender,
                        &events,
                        &framebuffer,
                        &resized,
                        client_id,
                    ) {
                        break error;
                    }
                };
                let _ = session_sender.send(SessionInput::Closed(error));
            })?
    };
    let result = write_client_updates(
        stream,
        framebuffer,
        clipboard,
        &session_receiver,
        &wake_queued,
        &resized,
        generation,
        tile_count,
    );
    // Unblock the reader, then wait for it so every input event from this
    // client is queued before the caller reports the disconnect.
    let _ = stream.shutdown(std::net::Shutdown::Both);
    drop(session_receiver);
    let _ = reader.join();
    result
}

fn read_client_message(
    stream: &mut TcpStream,
    session: &SyncSender<SessionInput>,
    events: &SyncSender<ClientEvent>,
    framebuffer: &Arc<Mutex<ServerFramebuffer>>,
    resized: &AtomicBool,
    client_id: u64,
) -> io::Result<()> {
    let session_closed = || io::Error::new(io::ErrorKind::BrokenPipe, "session writer closed");
    let input_closed = || io::Error::new(io::ErrorKind::BrokenPipe, "input receiver closed");
    let mut kind = [0];
    stream.read_exact(&mut kind)?;
    match kind[0] {
        0 => {
            let mut format = [0; 19];
            stream.read_exact(&mut format)?;
            session
                .send(SessionInput::PixelFormat(ServerPixelFormat::parse(
                    &format,
                )?))
                .map_err(|_| session_closed())?;
        }
        2 => {
            let mut header = [0; 3];
            stream.read_exact(&mut header)?;
            validate_zero_padding(&header[..1], "invalid SetEncodings padding")?;
            let count = usize::from(u16::from_be_bytes([header[1], header[2]]));
            if count > 65_536 / 4 {
                return Err(invalid("encoding list too large"));
            }
            let mut encodings = vec![0; count * 4];
            stream.read_exact(&mut encodings)?;
            let desktop_size = encodings.chunks_exact(4).any(|encoding| {
                i32::from_be_bytes(encoding.try_into().unwrap()) == DESKTOP_SIZE_ENCODING
            });
            session
                .send(SessionInput::Encodings { desktop_size })
                .map_err(|_| session_closed())?;
        }
        3 => {
            let mut request = [0; 9];
            stream.read_exact(&mut request)?;
            session
                .send(SessionInput::UpdateRequest(parse_update_request(request)?))
                .map_err(|_| session_closed())?;
        }
        4 => {
            let mut data = [0; 7];
            stream.read_exact(&mut data)?;
            if data[0] > 1 {
                return Err(invalid("invalid key event state"));
            }
            validate_zero_padding(&data[1..3], "invalid KeyEvent padding")?;
            events
                .send(ClientEvent::Key {
                    client_id,
                    keysym: u32::from_be_bytes(data[3..7].try_into().unwrap()),
                    down: data[0] != 0,
                })
                .map_err(|_| input_closed())?;
        }
        5 => {
            let mut data = [0; 5];
            stream.read_exact(&mut data)?;
            let x = u16::from_be_bytes([data[1], data[2]]);
            let y = u16::from_be_bytes([data[3], data[4]]);
            let in_bounds = {
                let fb = framebuffer
                    .lock()
                    .map_err(|_| invalid("framebuffer lock is poisoned"))?;
                x < fb.framebuffer.width && y < fb.framebuffer.height
            };
            if !in_bounds {
                // After a resize, pointer events sent for the old size may
                // still be in flight; drop them instead of ending the session.
                if resized.load(Ordering::Acquire) {
                    return Ok(());
                }
                return Err(invalid("pointer outside framebuffer"));
            }
            events
                .send(ClientEvent::Pointer {
                    client_id,
                    buttons: data[0],
                    x,
                    y,
                })
                .map_err(|_| input_closed())?;
        }
        6 => {
            let mut header = [0; 7];
            stream.read_exact(&mut header)?;
            validate_zero_padding(&header[..3], "invalid ClientCutText padding")?;
            let len = u32::from_be_bytes(header[3..7].try_into().unwrap()) as usize;
            if len > MAX_CLIENT_CLIPBOARD_BYTES {
                return Err(invalid("client clipboard text too large"));
            }
            let mut text = vec![0; len];
            stream.read_exact(&mut text)?;
            events
                .send(ClientEvent::ClipboardText { client_id, text })
                .map_err(|_| input_closed())?;
        }
        _ => return Err(invalid("unknown client message")),
    }
    Ok(())
}

struct PendingRequest {
    request: UpdateRequest,
    /// When an unchanged incremental request is answered with an empty update.
    deadline: Instant,
}

#[allow(clippy::too_many_arguments)]
fn write_client_updates(
    stream: &mut TcpStream,
    shared: &Arc<Mutex<ServerFramebuffer>>,
    clipboard: &Arc<Mutex<ServerClipboard>>,
    receiver: &std::sync::mpsc::Receiver<SessionInput>,
    wake_queued: &AtomicBool,
    resized: &AtomicBool,
    mut generation: u64,
    tile_count: usize,
) -> io::Result<()> {
    let mut seen_revisions = vec![u64::MAX; tile_count];
    let mut pixel_format = ServerPixelFormat::DEFAULT;
    let mut desktop_size = false;
    let mut output = Vec::new();
    let mut clipboard_revision = 0;
    let mut pending: Option<PendingRequest> = None;
    loop {
        send_pending_clipboard(stream, clipboard, &mut clipboard_revision)?;
        let (current_generation, width, height) = {
            let fb = shared
                .lock()
                .map_err(|_| invalid("framebuffer lock is poisoned"))?;
            (fb.generation, fb.framebuffer.width, fb.framebuffer.height)
        };
        if current_generation != generation {
            if !desktop_size {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "desktop size changed and the client does not support DesktopSize",
                ));
            }
            resized.store(true, Ordering::Release);
            // Answer the next request with the new size; the client then
            // requests pixels for the new framebuffer.
            if pending.take().is_some() {
                write_desktop_size(stream, width, height)?;
                generation = current_generation;
                let tiles = usize::from(width).div_ceil(SERVER_TILE_SIZE)
                    * usize::from(height).div_ceil(SERVER_TILE_SIZE);
                seen_revisions = vec![u64::MAX; tiles];
                continue;
            }
        } else if let Some(waiting) = &pending {
            let update = prepare_update(
                shared,
                &seen_revisions,
                waiting.request,
                resized.load(Ordering::Acquire),
                generation,
            )?;
            if let Some(update) = update
                && (!update.rectangles.is_empty() || Instant::now() >= waiting.deadline)
            {
                write_update(
                    stream,
                    shared,
                    &mut seen_revisions,
                    &mut output,
                    pixel_format,
                    update,
                    generation,
                )?;
                pending = None;
            }
        }
        let timeout = match &pending {
            Some(waiting) if current_generation == generation => waiting
                .deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1)),
            _ => SERVER_IDLE_WAKE_INTERVAL,
        };
        match receiver.recv_timeout(timeout) {
            Ok(SessionInput::PixelFormat(format)) => pixel_format = format,
            Ok(SessionInput::Encodings {
                desktop_size: supported,
            }) => desktop_size = supported,
            Ok(SessionInput::UpdateRequest(request)) => {
                pending = Some(PendingRequest {
                    request,
                    deadline: if request.incremental {
                        Instant::now() + SERVER_EMPTY_UPDATE_INTERVAL
                    } else {
                        Instant::now()
                    },
                });
            }
            Ok(SessionInput::Wake) => wake_queued.store(false, Ordering::Release),
            Ok(SessionInput::Closed(error)) => return Err(error),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "client reader stopped",
                ));
            }
        }
    }
}

fn write_desktop_size(stream: &mut TcpStream, width: u16, height: u16) -> io::Result<()> {
    let mut message = [0; 16];
    message[3] = 1;
    message[8..10].copy_from_slice(&width.to_be_bytes());
    message[10..12].copy_from_slice(&height.to_be_bytes());
    message[12..16].copy_from_slice(&DESKTOP_SIZE_ENCODING.to_be_bytes());
    stream.write_all(&message)
}

#[derive(Clone, Copy)]
struct ServerRect {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    tile_index: Option<usize>,
    revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ServerPixelFormat {
    bits_per_pixel: u8,
    big_endian: bool,
    red_max: u16,
    green_max: u16,
    blue_max: u16,
    red_shift: u8,
    green_shift: u8,
    blue_shift: u8,
}

impl ServerPixelFormat {
    const DEFAULT: Self = Self {
        bits_per_pixel: 32,
        big_endian: false,
        red_max: 255,
        green_max: 255,
        blue_max: 255,
        red_shift: 16,
        green_shift: 8,
        blue_shift: 0,
    };

    fn parse(bytes: &[u8; 19]) -> io::Result<Self> {
        let bits_per_pixel = bytes[3];
        let depth = bytes[4];
        let big_endian = match bytes[5] {
            0 => false,
            1 => true,
            _ => return Err(invalid("invalid pixel format byte order")),
        };
        if !matches!(bits_per_pixel, 8 | 16 | 32) || depth == 0 || depth > bits_per_pixel {
            return Err(invalid("unsupported pixel format bit depth"));
        }
        if bytes[6] != 1 || bytes[..3] != [0; 3] || bytes[16..] != [0; 3] {
            return Err(invalid("unsupported non-true-color pixel format"));
        }
        let format = Self {
            bits_per_pixel,
            big_endian,
            red_max: u16::from_be_bytes([bytes[7], bytes[8]]),
            green_max: u16::from_be_bytes([bytes[9], bytes[10]]),
            blue_max: u16::from_be_bytes([bytes[11], bytes[12]]),
            red_shift: bytes[13],
            green_shift: bytes[14],
            blue_shift: bytes[15],
        };
        let channels = [
            (format.red_max, format.red_shift),
            (format.green_max, format.green_shift),
            (format.blue_max, format.blue_shift),
        ];
        let mut used_bits = 0u64;
        let mut total_depth = 0u32;
        for (maximum, shift) in channels {
            let width = u32::from(maximum)
                .checked_add(1)
                .filter(|size| size.is_power_of_two())
                .map(u32::trailing_zeros)
                .filter(|width| *width != 0)
                .ok_or_else(|| invalid("invalid true-color channel maximum"))?;
            let end = u32::from(shift) + width;
            if end > u32::from(bits_per_pixel) {
                return Err(invalid("true-color channel exceeds pixel width"));
            }
            let mask = u64::from(maximum) << shift;
            if used_bits & mask != 0 {
                return Err(invalid("true-color channels overlap"));
            }
            used_bits |= mask;
            total_depth += width;
        }
        if total_depth != u32::from(depth) {
            return Err(invalid("pixel format depth does not match channel widths"));
        }
        Ok(format)
    }

    fn bytes_per_pixel(self) -> usize {
        usize::from(self.bits_per_pixel / 8)
    }

    fn encode_row(self, pixels: &[u32], output: &mut Vec<u8>) {
        if self == Self::DEFAULT {
            // The framebuffer already stores 0x00RRGGBB, the default wire layout.
            output.reserve(pixels.len() * 4);
            for pixel in pixels {
                output.extend_from_slice(&pixel.to_le_bytes());
            }
        } else {
            for pixel in pixels {
                self.encode(*pixel, output);
            }
        }
    }

    fn encode(self, rgb: u32, output: &mut Vec<u8>) {
        let red = (rgb >> 16) & 0xff;
        let green = (rgb >> 8) & 0xff;
        let blue = rgb & 0xff;
        let scale = |channel: u32, maximum: u16| (channel * u32::from(maximum) + 127) / 255;
        let packed = (scale(red, self.red_max) << self.red_shift)
            | (scale(green, self.green_max) << self.green_shift)
            | (scale(blue, self.blue_max) << self.blue_shift);
        let bytes = self.bytes_per_pixel();
        for index in 0..bytes {
            let shift_index = if self.big_endian {
                bytes - index - 1
            } else {
                index
            };
            output.push((packed >> (shift_index * 8)) as u8);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct UpdateRequest {
    incremental: bool,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

fn parse_update_request(bytes: [u8; 9]) -> io::Result<UpdateRequest> {
    if bytes[0] > 1 {
        return Err(invalid("invalid update request"));
    }
    Ok(UpdateRequest {
        incremental: bytes[0] != 0,
        x: u16::from_be_bytes([bytes[1], bytes[2]]),
        y: u16::from_be_bytes([bytes[3], bytes[4]]),
        width: u16::from_be_bytes([bytes[5], bytes[6]]),
        height: u16::from_be_bytes([bytes[7], bytes[8]]),
    })
}

fn validate_zero_padding(bytes: &[u8], message: &'static str) -> io::Result<()> {
    if bytes.iter().any(|byte| *byte != 0) {
        return Err(invalid(message));
    }
    Ok(())
}

struct PreparedUpdate {
    rectangles: Vec<ServerRect>,
    acknowledged: Vec<(usize, u64)>,
}

/// Select the rectangles that answer `request`. Returns `None` when the
/// framebuffer no longer has the dimensions the client was told about.
fn prepare_update(
    shared: &Arc<Mutex<ServerFramebuffer>>,
    seen_revisions: &[u64],
    request: UpdateRequest,
    clip_to_framebuffer: bool,
    generation: u64,
) -> io::Result<Option<PreparedUpdate>> {
    let UpdateRequest {
        incremental,
        x,
        y,
        width,
        height,
    } = request;
    let fb = shared
        .lock()
        .map_err(|_| invalid("framebuffer lock is poisoned"))?;
    if fb.generation != generation || seen_revisions.len() != fb.tile_revisions.len() {
        return Ok(None);
    }
    let request_x0 = usize::from(x);
    let request_y0 = usize::from(y);
    let mut request_x1 = request_x0 + usize::from(width);
    let mut request_y1 = request_y0 + usize::from(height);
    if request_x1 > fb.framebuffer.width() || request_y1 > fb.framebuffer.height() {
        // Requests sized for a previous framebuffer may still be in flight
        // after a resize; clip those instead of ending the session.
        if !clip_to_framebuffer {
            return Err(invalid("client requested pixels outside framebuffer"));
        }
        request_x1 = request_x1.min(fb.framebuffer.width());
        request_y1 = request_y1.min(fb.framebuffer.height());
    }

    let mut rectangles = Vec::new();
    let mut acknowledged = Vec::new();
    if !incremental && request_x0 < request_x1 && request_y0 < request_y1 {
        rectangles.push(ServerRect {
            x,
            y,
            width: (request_x1 - request_x0) as u16,
            height: (request_y1 - request_y0) as u16,
            tile_index: None,
            revision: 0,
        });
    }
    for (index, seen_revision) in seen_revisions.iter().enumerate() {
        let revision = fb.tile_revisions[index];
        let (tile_x, tile_y, tile_width, tile_height) = fb.tile_rect(index);
        let tile_x1 = tile_x + tile_width;
        let tile_y1 = tile_y + tile_height;
        let overlap_x0 = tile_x.max(request_x0);
        let overlap_y0 = tile_y.max(request_y0);
        let overlap_x1 = tile_x1.min(request_x1);
        let overlap_y1 = tile_y1.min(request_y1);
        if overlap_x0 >= overlap_x1 || overlap_y0 >= overlap_y1 {
            continue;
        }
        let covers_tile = overlap_x0 == tile_x
            && overlap_y0 == tile_y
            && overlap_x1 == tile_x1
            && overlap_y1 == tile_y1;
        if covers_tile {
            acknowledged.push((index, revision));
        }
        // Keep a partially requested tile pending so a later request for
        // the rest of that tile also receives the outstanding changes.
        if incremental && *seen_revision != revision {
            rectangles.push(ServerRect {
                x: overlap_x0 as u16,
                y: overlap_y0 as u16,
                width: (overlap_x1 - overlap_x0) as u16,
                height: (overlap_y1 - overlap_y0) as u16,
                tile_index: covers_tile.then_some(index),
                revision,
            });
        }
    }
    if rectangles.len() > usize::from(u16::MAX) {
        return Err(invalid("too many changed framebuffer rectangles"));
    }
    Ok(Some(PreparedUpdate {
        rectangles,
        acknowledged,
    }))
}

/// Write a prepared update. The framebuffer lock is taken per row so capture
/// is never blocked on the network. If the framebuffer is resized while the
/// update is being written, the remaining rows are sent black; the client
/// receives the new size in its next update.
fn write_update(
    stream: &mut TcpStream,
    shared: &Arc<Mutex<ServerFramebuffer>>,
    seen_revisions: &mut [u64],
    output: &mut Vec<u8>,
    pixel_format: ServerPixelFormat,
    update: PreparedUpdate,
    generation: u64,
) -> io::Result<()> {
    output.clear();
    output.extend_from_slice(&[0, 0]);
    output.extend_from_slice(&(update.rectangles.len() as u16).to_be_bytes());
    for rect in &update.rectangles {
        output.extend_from_slice(&rect.x.to_be_bytes());
        output.extend_from_slice(&rect.y.to_be_bytes());
        output.extend_from_slice(&rect.width.to_be_bytes());
        output.extend_from_slice(&rect.height.to_be_bytes());
        output.extend_from_slice(&0i32.to_be_bytes());
        let row_len = usize::from(rect.width) * pixel_format.bytes_per_pixel();
        for row in usize::from(rect.y)..usize::from(rect.y) + usize::from(rect.height) {
            {
                let fb = shared
                    .lock()
                    .map_err(|_| invalid("framebuffer lock is poisoned"))?;
                if fb.generation == generation {
                    let start = row * fb.framebuffer.width() + usize::from(rect.x);
                    let end = start + usize::from(rect.width);
                    pixel_format.encode_row(&fb.framebuffer.pixels[start..end], output);
                } else {
                    output.resize(output.len() + row_len, 0);
                }
            }
            if output.len() >= SERVER_WRITE_CHUNK_BYTES {
                stream.write_all(output)?;
                output.clear();
            }
        }
        if let Some(index) = rect.tile_index {
            seen_revisions[index] = rect.revision;
        }
    }
    stream.write_all(output)?;
    output.clear();
    for (index, revision) in update.acknowledged {
        seen_revisions[index] = revision;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Version {
    V3_3,
    V3_7,
    V3_8,
}

fn server_reason(stream: &mut impl Read) -> io::Result<String> {
    let length = read_u32(stream)? as usize;
    if length > MAX_NAME_BYTES {
        return Err(invalid("server rejection reason is too long"));
    }
    let mut reason = vec![0; length];
    stream.read_exact(&mut reason)?;
    Ok(String::from_utf8_lossy(&reason).into_owned())
}

fn vnc_response(challenge: &[u8; 16], password: &str) -> [u8; 16] {
    // VNC's DES key uses each password byte with its bits reversed.
    let mut key = [0u8; 8];
    for (dest, byte) in key.iter_mut().zip(password.as_bytes().iter().copied()) {
        *dest = byte.reverse_bits();
    }
    let cipher = Des::new_from_slice(&key).expect("DES key is always eight bytes");
    let mut response = *challenge;
    for chunk in response.chunks_exact_mut(8) {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    response
}

fn constant_time_equal(left: &[u8; 16], right: &[u8; 16]) -> bool {
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

/// Negotiate an RFB session. `None` security requires explicit consent.
pub fn negotiate(
    stream: &mut (impl Read + Write),
    allow_insecure: bool,
    password_prompt: impl FnMut() -> io::Result<String>,
) -> io::Result<ServerInfo> {
    negotiate_with_encoding(stream, allow_insecure, Encoding::Raw, password_prompt)
}

pub fn negotiate_with_encoding(
    stream: &mut (impl Read + Write),
    allow_insecure: bool,
    encoding: Encoding,
    mut password_prompt: impl FnMut() -> io::Result<String>,
) -> io::Result<ServerInfo> {
    let mut version = [0; 12];
    stream.read_exact(&mut version)?;
    let (protocol, reply) = match &version {
        b"RFB 003.003\n" => (Version::V3_3, b"RFB 003.003\n".as_slice()),
        b"RFB 003.007\n" => (Version::V3_7, b"RFB 003.007\n".as_slice()),
        b"RFB 003.008\n" | b"RFB 003.889\n" => (Version::V3_8, b"RFB 003.008\n".as_slice()),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported server RFB banner: {:?}",
                    String::from_utf8_lossy(&version)
                ),
            ));
        }
    };
    stream.write_all(reply)?;

    let security = if protocol == Version::V3_3 {
        match read_u32(stream)? {
            1 if allow_insecure => Security::None,
            1 => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "server offers only None security; pass --allow-insecure to accept it",
                ));
            }
            2 => Security::VncPassword,
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("server rejected connection: {}", server_reason(stream)?),
                ));
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported RFB 3.3 security type: {other}"),
                ));
            }
        }
    } else {
        let mut count = [0];
        stream.read_exact(&mut count)?;
        if count[0] == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("server rejected connection: {}", server_reason(stream)?),
            ));
        }
        let mut types = vec![0; usize::from(count[0])];
        stream.read_exact(&mut types)?;
        let selected = if types.contains(&2) {
            Security::VncPassword
        } else if allow_insecure && types.contains(&1) {
            Security::None
        } else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "no supported security type among {types:?}; VNC password (2) is supported, and None (1) requires --allow-insecure"
                ),
            ));
        };
        stream.write_all(&[match selected {
            Security::None => 1,
            Security::VncPassword => 2,
        }])?;
        selected
    };

    if security == Security::VncPassword {
        let mut challenge = [0; 16];
        stream.read_exact(&mut challenge)?;
        let password = password_prompt()?;
        stream.write_all(&vnc_response(&challenge, &password))?;
    }
    if (security == Security::VncPassword || protocol == Version::V3_8) && read_u32(stream)? != 0 {
        let reason = if protocol == Version::V3_8 {
            server_reason(stream)?
        } else {
            "authentication failed".to_owned()
        };
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, reason));
    }

    stream.write_all(&[1])?; // ClientInit: share desktop.
    let mut header = [0; 24];
    stream.read_exact(&mut header)?;
    let width = u16::from_be_bytes([header[0], header[1]]);
    let height = u16::from_be_bytes([header[2], header[3]]);
    let pixels = usize::from(width) * usize::from(height);
    if width == 0
        || height == 0
        || width > MAX_FRAMEBUFFER_DIMENSION
        || height > MAX_FRAMEBUFFER_DIMENSION
        || pixels > MAX_FRAMEBUFFER_PIXELS
    {
        return Err(invalid("server framebuffer dimensions exceed limits"));
    }
    let name_length = u32::from_be_bytes(header[20..24].try_into().unwrap()) as usize;
    if name_length > MAX_NAME_BYTES {
        return Err(invalid("server name is too long"));
    }
    let mut name = vec![0; name_length];
    stream.read_exact(&mut name)?;

    // Request 32-bit little-endian true color: on wire B, G, R, unused.
    stream.write_all(&[
        0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0,
    ])?;
    let encoding_id: i32 = match encoding {
        Encoding::Raw => 0,
        Encoding::Zlib => 6,
    };
    stream.write_all(&[2, 0, 0, 1])?;
    stream.write_all(&encoding_id.to_be_bytes())?;
    Ok(ServerInfo {
        width,
        height,
        name: String::from_utf8_lossy(&name).into_owned(),
        security,
    })
}

#[derive(Debug, Clone)]
pub struct Framebuffer {
    width: u16,
    height: u16,
    pixels: Vec<u32>,
}

impl Framebuffer {
    pub fn new(width: u16, height: u16) -> io::Result<Self> {
        let pixels = usize::from(width) * usize::from(height);
        if width == 0
            || height == 0
            || width > MAX_FRAMEBUFFER_DIMENSION
            || height > MAX_FRAMEBUFFER_DIMENSION
            || pixels > MAX_FRAMEBUFFER_PIXELS
        {
            return Err(invalid("framebuffer dimensions exceed limits"));
        }
        Ok(Self {
            width,
            height,
            pixels: vec![0; pixels],
        })
    }

    /// Pixels as 0x00RRGGBB, row-major.
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Mutable 0x00RRGGBB pixels for hosts that render or capture directly.
    /// The buffer length is fixed by the framebuffer dimensions.
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
    }
    pub fn width(&self) -> usize {
        usize::from(self.width)
    }
    pub fn height(&self) -> usize {
        usize::from(self.height)
    }

    pub fn apply_raw(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        bytes: &[u8],
    ) -> io::Result<()> {
        if width == 0
            || height == 0
            || usize::from(x) + usize::from(width) > self.width()
            || usize::from(y) + usize::from(height) > self.height()
        {
            return Err(invalid("rectangle is outside framebuffer"));
        }
        let row_width = usize::from(width);
        if bytes.len() != row_width * usize::from(height) * 4 {
            return Err(invalid("raw rectangle has incorrect byte length"));
        }
        for row in 0..usize::from(height) {
            let dest_start = (usize::from(y) + row) * self.width() + usize::from(x);
            let source_start = row * row_width * 4;
            for col in 0..row_width {
                let pixel = &bytes[source_start + col * 4..source_start + col * 4 + 4];
                self.pixels[dest_start + col] =
                    (u32::from(pixel[2]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[0]);
            }
        }
        Ok(())
    }
}

/// Read one framebuffer update, calling `apply` after each raw rectangle arrives.
pub fn read_update_with(
    reader: &mut impl Read,
    frame_width: u16,
    frame_height: u16,
    scratch: &mut Vec<u8>,
    apply: impl FnMut(u16, u16, u16, u16, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    read_update_with_encoding(
        reader,
        frame_width,
        frame_height,
        scratch,
        Encoding::Raw,
        &mut Decompress::new(true),
        apply,
    )
}

pub fn read_update_with_encoding(
    reader: &mut impl Read,
    frame_width: u16,
    frame_height: u16,
    scratch: &mut Vec<u8>,
    selected_encoding: Encoding,
    decoder: &mut Decompress,
    mut apply: impl FnMut(u16, u16, u16, u16, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    loop {
        let mut kind = [0];
        reader.read_exact(&mut kind)?;
        match kind[0] {
            0 => {
                let mut header = [0; 3];
                reader.read_exact(&mut header)?;
                let count = u16::from_be_bytes([header[1], header[2]]);
                for _ in 0..count {
                    let mut rect = [0; 12];
                    reader.read_exact(&mut rect)?;
                    let x = u16::from_be_bytes([rect[0], rect[1]]);
                    let y = u16::from_be_bytes([rect[2], rect[3]]);
                    let width = u16::from_be_bytes([rect[4], rect[5]]);
                    let height = u16::from_be_bytes([rect[6], rect[7]]);
                    let wire_encoding = i32::from_be_bytes(rect[8..12].try_into().unwrap());
                    if wire_encoding != 0
                        && !(wire_encoding == 6 && selected_encoding == Encoding::Zlib)
                    {
                        return Err(invalid("server sent an unsupported encoding"));
                    }
                    if width == 0
                        || height == 0
                        || usize::from(x) + usize::from(width) > usize::from(frame_width)
                        || usize::from(y) + usize::from(height) > usize::from(frame_height)
                    {
                        return Err(invalid("server sent an out-of-bounds rectangle"));
                    }
                    let length = usize::from(width) * usize::from(height) * 4;
                    scratch.resize(length, 0);
                    if wire_encoding == 0 {
                        reader.read_exact(scratch)?;
                    } else {
                        let compressed_length = read_u32(reader)? as usize;
                        // A zlib block may expand slightly; cap it before allocating.
                        let limit = length + length / 1000 + 65_536;
                        if compressed_length > limit {
                            return Err(invalid("compressed rectangle exceeds size limit"));
                        }
                        let mut compressed = vec![0; compressed_length];
                        reader.read_exact(&mut compressed)?;
                        let input_before = decoder.total_in();
                        let output_before = decoder.total_out();
                        decoder
                            .decompress(&compressed, scratch, FlushDecompress::Sync)
                            .map_err(|_| invalid("invalid zlib rectangle"))?;
                        if decoder.total_in() - input_before != compressed_length as u64
                            || decoder.total_out() - output_before != length as u64
                        {
                            return Err(invalid("zlib rectangle has incorrect decoded length"));
                        }
                    }
                    apply(x, y, width, height, scratch)?;
                }
                return Ok(());
            }
            2 => {} // Bell.
            3 => {
                let mut header = [0; 7];
                reader.read_exact(&mut header)?;
                let length = u32::from_be_bytes(header[3..7].try_into().unwrap()) as usize;
                if length > 1_048_576 {
                    return Err(invalid("server clipboard text is too large"));
                }
                let copied = io::copy(&mut reader.take(length as u64), &mut io::sink())?;
                if copied != length as u64 {
                    return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                }
            }
            _ => return Err(invalid("unknown server message type")),
        }
    }
}

pub fn read_update(
    reader: &mut impl Read,
    framebuffer: &mut Framebuffer,
    scratch: &mut Vec<u8>,
) -> io::Result<()> {
    read_update_with(
        reader,
        framebuffer.width,
        framebuffer.height,
        scratch,
        |x, y, width, height, bytes| framebuffer.apply_raw(x, y, width, height, bytes),
    )
}

#[derive(Clone)]
pub struct InputWriter(Arc<Mutex<TcpStream>>);

impl InputWriter {
    pub fn shutdown(&self) -> io::Result<()> {
        self.0
            .lock()
            .map_err(|_| invalid("connection lock is poisoned"))?
            .shutdown(std::net::Shutdown::Both)
    }

    fn send(&self, bytes: &[u8]) -> io::Result<()> {
        self.0
            .lock()
            .map_err(|_| invalid("connection lock is poisoned"))?
            .write_all(bytes)
    }

    pub fn request_update(&self, incremental: bool, width: u16, height: u16) -> io::Result<()> {
        let mut message = [3, u8::from(incremental), 0, 0, 0, 0, 0, 0, 0, 0];
        message[6..8].copy_from_slice(&width.to_be_bytes());
        message[8..10].copy_from_slice(&height.to_be_bytes());
        self.send(&message)
    }

    pub fn key(&self, keysym: u32, down: bool) -> io::Result<()> {
        self.send(&key_packet(keysym, down))
    }

    pub fn pointer(&self, buttons: u8, x: u16, y: u16) -> io::Result<()> {
        self.send(&pointer_packet(buttons, x, y))
    }
}

fn key_packet(keysym: u32, down: bool) -> [u8; 8] {
    let mut message = [4, u8::from(down), 0, 0, 0, 0, 0, 0];
    message[4..8].copy_from_slice(&keysym.to_be_bytes());
    message
}

fn pointer_packet(buttons: u8, x: u16, y: u16) -> [u8; 6] {
    let mut message = [5, buttons, 0, 0, 0, 0];
    message[2..4].copy_from_slice(&x.to_be_bytes());
    message[4..6].copy_from_slice(&y.to_be_bytes());
    message
}

pub struct Session {
    pub info: ServerInfo,
    reader: TcpStream,
    writer: InputWriter,
    encoding: Encoding,
    decoder: Decompress,
}

impl Session {
    pub fn connect(
        address: &str,
        allow_insecure: bool,
        password_prompt: impl FnMut() -> io::Result<String>,
    ) -> io::Result<Self> {
        Self::connect_with_encoding(address, allow_insecure, Encoding::Raw, password_prompt)
    }

    pub fn connect_with_encoding(
        address: &str,
        allow_insecure: bool,
        encoding: Encoding,
        password_prompt: impl FnMut() -> io::Result<String>,
    ) -> io::Result<Self> {
        Self::from_stream_with_encoding(
            connect_tcp(address)?,
            allow_insecure,
            encoding,
            password_prompt,
            CONNECT_TIMEOUT,
        )
    }

    #[cfg(test)]
    fn from_stream(
        stream: TcpStream,
        allow_insecure: bool,
        password_prompt: impl FnMut() -> io::Result<String>,
        timeout: Duration,
    ) -> io::Result<Self> {
        Self::from_stream_with_encoding(
            stream,
            allow_insecure,
            Encoding::Raw,
            password_prompt,
            timeout,
        )
    }

    fn from_stream_with_encoding(
        mut stream: TcpStream,
        allow_insecure: bool,
        encoding: Encoding,
        password_prompt: impl FnMut() -> io::Result<String>,
        timeout: Duration,
    ) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        let info = negotiate_with_encoding(
            &mut HandshakeStream {
                stream: &mut stream,
                deadline: Instant::now() + timeout,
            },
            allow_insecure,
            encoding,
            password_prompt,
        )
        .map_err(|error| {
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) {
                timed_out("Connection handshake timed out. Check Screen Sharing.")
            } else {
                error
            }
        })?;
        stream.set_read_timeout(Some(UPDATE_IDLE_TIMEOUT))?;
        stream.set_write_timeout(Some(UPDATE_IDLE_TIMEOUT))?;
        let reader = stream.try_clone()?;
        Ok(Self {
            info,
            reader,
            writer: InputWriter(Arc::new(Mutex::new(stream))),
            encoding,
            decoder: Decompress::new(true),
        })
    }

    pub fn writer(&self) -> InputWriter {
        self.writer.clone()
    }

    pub fn read_update(
        &mut self,
        framebuffer: &mut Framebuffer,
        scratch: &mut Vec<u8>,
    ) -> io::Result<()> {
        self.read_update_with(scratch, |x, y, width, height, bytes| {
            framebuffer.apply_raw(x, y, width, height, bytes)
        })
    }

    pub fn read_update_with(
        &mut self,
        scratch: &mut Vec<u8>,
        apply: impl FnMut(u16, u16, u16, u16, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let writer = &self.writer;
        let (width, height) = (self.info.width, self.info.height);
        let mut reader = RefreshReader {
            reader: &mut self.reader,
            refresh: || writer.request_update(false, width, height),
            requested: false,
        };
        read_update_with_encoding(
            &mut reader,
            self.info.width,
            self.info.height,
            scratch,
            self.encoding,
            &mut self.decoder,
            apply,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compress, Compression as ZlibLevel, FlushCompress};
    use std::io::Cursor;

    fn send_framebuffer_update(
        stream: &mut TcpStream,
        shared: &Arc<Mutex<ServerFramebuffer>>,
        seen_revisions: &mut [u64],
        output: &mut Vec<u8>,
        pixel_format: ServerPixelFormat,
        request: UpdateRequest,
    ) -> io::Result<()> {
        let generation = shared.lock().unwrap().generation;
        let update = prepare_update(shared, seen_revisions, request, false, generation)?.unwrap();
        write_update(
            stream,
            shared,
            seen_revisions,
            output,
            pixel_format,
            update,
            generation,
        )
    }

    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (server, client)
    }

    #[test]
    fn exclusive_rfb_session_closes_shared_clients_and_blocks_new_ones() {
        let (server_one, mut peer_one) = tcp_pair();
        let (server_two, _peer_two) = tcp_pair();
        let (server_three, _peer_three) = tcp_pair();
        let mut sessions = ServerSessions::default();
        sessions.register(1, server_one).unwrap();
        sessions.register(2, server_two).unwrap();
        sessions.admit(1, true).unwrap();
        sessions.admit(2, false).unwrap();

        let mut byte = [0];
        assert_eq!(peer_one.read(&mut byte).unwrap(), 0);
        assert!(sessions.register(3, server_three).is_err());
        sessions.remove(2);

        let (server_four, _) = tcp_pair();
        sessions.register(4, server_four).unwrap();
        sessions.admit(4, true).unwrap();
    }

    #[test]
    fn server_clipboard_uses_server_cut_text_wire_message() {
        let clipboard = Arc::new(Mutex::new(ServerClipboard {
            revision: 1,
            text: Some(vec![b'h', 0xe9]),
        }));
        let (mut server_stream, mut client_stream) = tcp_pair();
        let mut seen_revision = 0;
        send_pending_clipboard(&mut server_stream, &clipboard, &mut seen_revision).unwrap();
        let mut message = [0; 10];
        client_stream.read_exact(&mut message).unwrap();
        assert_eq!(message, [3, 0, 0, 0, 0, 0, 0, 2, b'h', 0xe9]);
        assert_eq!(seen_revision, 1);
    }

    #[test]
    fn server_clipboard_rejects_oversized_text_and_supports_clearing() {
        let server = VncServer::bind(
            "127.0.0.1:0",
            Framebuffer::new(1, 1).unwrap(),
            ServerConfig {
                allow_insecure: true,
                ..ServerConfig::default()
            },
        )
        .unwrap();
        let oversized = vec![b'x'; MAX_CLIENT_CLIPBOARD_BYTES + 1];
        assert_eq!(
            server.set_clipboard_text(&oversized).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        {
            let clipboard = server.clipboard.lock().unwrap();
            assert_eq!(clipboard.revision, 0);
            assert!(clipboard.text.is_none());
        }

        server.set_clipboard_text(&[]).unwrap();
        let clipboard = server.clipboard.lock().unwrap();
        assert_eq!(clipboard.revision, 1);
        assert_eq!(clipboard.text.as_deref(), Some([].as_slice()));
    }

    #[test]
    fn embedded_server_sends_updated_clipboard_to_connected_client() {
        let server = Arc::new(
            VncServer::bind(
                "127.0.0.1:0",
                Framebuffer::new(1, 1).unwrap(),
                ServerConfig {
                    allow_insecure: true,
                    ..ServerConfig::default()
                },
            )
            .unwrap(),
        );
        let address = server.local_addr().unwrap();
        let runner = Arc::clone(&server);
        let server_thread = std::thread::spawn(move || runner.run());

        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut banner = [0; 12];
        client.read_exact(&mut banner).unwrap();
        assert_eq!(&banner, b"RFB 003.008\n");
        client.write_all(&banner).unwrap();
        let mut security = [0; 2];
        client.read_exact(&mut security).unwrap();
        assert_eq!(security, [1, 1]);
        client.write_all(&[1]).unwrap();
        let mut security_result = [0; 4];
        client.read_exact(&mut security_result).unwrap();
        assert_eq!(security_result, [0; 4]);
        client.write_all(&[1]).unwrap();
        let mut server_init = [0; 24];
        client.read_exact(&mut server_init).unwrap();
        let name_len = u32::from_be_bytes(server_init[20..24].try_into().unwrap()) as usize;
        let mut name = vec![0; name_len];
        client.read_exact(&mut name).unwrap();
        client
            .write_all(&[
                0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0, 2, 0, 0, 1, 0,
                0, 0, 0,
            ])
            .unwrap();

        server.set_clipboard_text(&[b'h', 0xe9]).unwrap();
        let mut clipboard_header = [0; 8];
        client.read_exact(&mut clipboard_header).unwrap();
        assert_eq!(clipboard_header, [3, 0, 0, 0, 0, 0, 0, 2]);
        let mut clipboard_text = [0; 2];
        client.read_exact(&mut clipboard_text).unwrap();
        assert_eq!(clipboard_text, [b'h', 0xe9]);
        client.write_all(&[4, 1, 0, 0, 0, 0, 0, b'x']).unwrap();
        assert!(matches!(
            server.recv_event_timeout(Duration::from_secs(1)).unwrap(),
            ClientEvent::Key { keysym: 120, .. }
        ));

        server.stop();
        let mut byte = [0];
        assert_eq!(client.read(&mut byte).unwrap(), 0);
        drop(client);
        server_thread.join().unwrap().unwrap();
    }

    #[test]
    fn partial_incremental_tile_requests_keep_revision_pending_until_full_coverage() {
        let initial = Framebuffer::new(64, 1).unwrap();
        let shared = Arc::new(Mutex::new(ServerFramebuffer::new(initial.clone())));
        let mut changed = initial;
        let mut raw = vec![0; 64 * 4];
        raw[40 * 4 + 2] = 255;
        changed.apply_raw(0, 0, 64, 1, &raw).unwrap();
        update_server_framebuffer(&shared, &changed).unwrap();

        let (mut server_stream, mut client_stream) = tcp_pair();
        let mut seen_revisions = [0];
        let mut row_bytes = Vec::new();
        for (x, width) in [(0, 32), (32, 32), (0, 64)] {
            send_framebuffer_update(
                &mut server_stream,
                &shared,
                &mut seen_revisions,
                &mut row_bytes,
                ServerPixelFormat::DEFAULT,
                UpdateRequest {
                    incremental: true,
                    x,
                    y: 0,
                    width,
                    height: 1,
                },
            )
            .unwrap();
            let mut header = [0; 4];
            client_stream.read_exact(&mut header).unwrap();
            assert_eq!(header, [0, 0, 0, 1]);
            let mut rectangle = [0; 12];
            client_stream.read_exact(&mut rectangle).unwrap();
            assert_eq!(u16::from_be_bytes([rectangle[0], rectangle[1]]), x);
            assert_eq!(u16::from_be_bytes([rectangle[4], rectangle[5]]), width);
            let mut pixels = vec![0; usize::from(width) * 4];
            client_stream.read_exact(&mut pixels).unwrap();
            if x == 32 {
                assert_eq!(&pixels[8 * 4..8 * 4 + 4], &[0, 0, 255, 0]);
            }
        }

        send_framebuffer_update(
            &mut server_stream,
            &shared,
            &mut seen_revisions,
            &mut row_bytes,
            ServerPixelFormat::DEFAULT,
            UpdateRequest {
                incremental: true,
                x: 0,
                y: 0,
                width: 32,
                height: 1,
            },
        )
        .unwrap();
        let mut empty = [0; 4];
        client_stream.read_exact(&mut empty).unwrap();
        assert_eq!(empty, [0, 0, 0, 0]);
    }

    fn start_insecure_server(framebuffer: Framebuffer) -> (Arc<VncServer>, std::net::SocketAddr) {
        let server = Arc::new(
            VncServer::bind(
                "127.0.0.1:0",
                framebuffer,
                ServerConfig {
                    allow_insecure: true,
                    ..ServerConfig::default()
                },
            )
            .unwrap(),
        );
        let address = server.local_addr().unwrap();
        let runner = Arc::clone(&server);
        std::thread::spawn(move || runner.run());
        (server, address)
    }

    /// Complete an RFB 3.8 None-security handshake and advertise `encodings`.
    fn raw_client(address: std::net::SocketAddr, encodings: &[i32]) -> (TcpStream, u16, u16) {
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut banner = [0; 12];
        client.read_exact(&mut banner).unwrap();
        client.write_all(&banner).unwrap();
        let mut security = [0; 2];
        client.read_exact(&mut security).unwrap();
        client.write_all(&[1]).unwrap();
        let mut security_result = [0; 4];
        client.read_exact(&mut security_result).unwrap();
        client.write_all(&[1]).unwrap();
        let mut server_init = [0; 24];
        client.read_exact(&mut server_init).unwrap();
        let name_len = u32::from_be_bytes(server_init[20..24].try_into().unwrap()) as usize;
        client.read_exact(&mut vec![0; name_len]).unwrap();
        let mut message = vec![2, 0];
        message.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
        for encoding in encodings {
            message.extend_from_slice(&encoding.to_be_bytes());
        }
        client.write_all(&message).unwrap();
        (
            client,
            u16::from_be_bytes([server_init[0], server_init[1]]),
            u16::from_be_bytes([server_init[2], server_init[3]]),
        )
    }

    fn request(client: &mut TcpStream, incremental: bool, width: u16, height: u16) {
        let mut message = [3, u8::from(incremental), 0, 0, 0, 0, 0, 0, 0, 0];
        message[6..8].copy_from_slice(&width.to_be_bytes());
        message[8..10].copy_from_slice(&height.to_be_bytes());
        client.write_all(&message).unwrap();
    }

    /// Read one FramebufferUpdate of 32-bit Raw or DesktopSize rectangles.
    fn read_rectangles(client: &mut TcpStream) -> Vec<(u16, u16, u16, u16, i32, Vec<u8>)> {
        let mut header = [0; 4];
        client.read_exact(&mut header).unwrap();
        assert_eq!(header[0], 0);
        let count = u16::from_be_bytes([header[2], header[3]]);
        (0..count)
            .map(|_| {
                let mut rect = [0; 12];
                client.read_exact(&mut rect).unwrap();
                let field = |index: usize| u16::from_be_bytes([rect[index], rect[index + 1]]);
                let encoding = i32::from_be_bytes(rect[8..12].try_into().unwrap());
                let mut pixels = Vec::new();
                if encoding == 0 {
                    pixels = vec![0; usize::from(field(4)) * usize::from(field(6)) * 4];
                    client.read_exact(&mut pixels).unwrap();
                }
                (field(0), field(2), field(4), field(6), encoding, pixels)
            })
            .collect()
    }

    #[test]
    fn desktop_size_clients_follow_a_framebuffer_resize() {
        let (server, address) = start_insecure_server(Framebuffer::new(2, 1).unwrap());
        let (mut client, width, height) = raw_client(address, &[0, DESKTOP_SIZE_ENCODING]);
        assert_eq!((width, height), (2, 1));
        request(&mut client, false, 2, 1);
        assert_eq!(read_rectangles(&mut client).len(), 1);

        let mut resized = Framebuffer::new(3, 2).unwrap();
        resized.pixels_mut()[5] = 0x00ff_0000;
        server.update_framebuffer(&resized).unwrap();
        // A request sized for the old framebuffer is answered with the new size.
        request(&mut client, true, 2, 1);
        assert_eq!(
            read_rectangles(&mut client),
            [(0, 0, 3, 2, DESKTOP_SIZE_ENCODING, Vec::new())]
        );
        // Pointer events for the old size that are still in flight are
        // dropped rather than ending the session.
        client.write_all(&[5, 0, 0, 9, 0, 9]).unwrap();
        request(&mut client, false, 3, 2);
        let update = read_rectangles(&mut client);
        assert_eq!(update.len(), 1);
        let (x, y, width, height, encoding, pixels) = &update[0];
        assert_eq!((*x, *y, *width, *height, *encoding), (0, 0, 3, 2, 0));
        assert_eq!(&pixels[5 * 4..5 * 4 + 4], &[0, 0, 0xff, 0]);

        client.write_all(&[5, 0, 0, 2, 0, 1]).unwrap();
        assert_eq!(
            server.recv_event_timeout(Duration::from_secs(1)).unwrap(),
            ClientEvent::Pointer {
                client_id: 1,
                buttons: 0,
                x: 2,
                y: 1,
            }
        );
        server.stop();
    }

    #[test]
    fn clients_without_desktop_size_are_disconnected_on_resize() {
        let (server, address) = start_insecure_server(Framebuffer::new(2, 1).unwrap());
        let (mut client, _, _) = raw_client(address, &[0]);
        request(&mut client, false, 2, 1);
        read_rectangles(&mut client);
        server
            .update_framebuffer(&Framebuffer::new(4, 4).unwrap())
            .unwrap();
        let mut byte = [0];
        assert!(!matches!(client.read(&mut byte), Ok(1)));
        assert_eq!(
            server.recv_event_timeout(Duration::from_secs(1)).unwrap(),
            ClientEvent::ClientDisconnected { client_id: 1 }
        );
        server.stop();
    }

    #[test]
    fn pending_incremental_request_is_answered_by_the_next_change() {
        let (server, address) = start_insecure_server(Framebuffer::new(2, 1).unwrap());
        let (mut client, _, _) = raw_client(address, &[0]);
        request(&mut client, false, 2, 1);
        read_rectangles(&mut client);

        request(&mut client, true, 2, 1);
        // Input sent while the request waits is delivered immediately.
        client.write_all(&[4, 1, 0, 0, 0, 0, 0, b'q']).unwrap();
        assert!(matches!(
            server.recv_event_timeout(Duration::from_secs(1)).unwrap(),
            ClientEvent::Key { keysym: 0x71, .. }
        ));
        let mut changed = Framebuffer::new(2, 1).unwrap();
        changed.pixels_mut()[1] = 0x0000_00ff;
        server.update_framebuffer(&changed).unwrap();
        let update = read_rectangles(&mut client);
        assert_eq!(update.len(), 1);
        assert_eq!(update[0].5, [0, 0, 0, 0, 0xff, 0, 0, 0]);

        // Without changes, an incremental request gets a throttled empty update.
        let started = Instant::now();
        request(&mut client, true, 2, 1);
        assert!(read_rectangles(&mut client).is_empty());
        assert!(started.elapsed() >= SERVER_EMPTY_UPDATE_INTERVAL / 2);
        server.stop();
    }

    #[test]
    fn damage_regions_limit_which_tiles_are_compared() {
        let shared = Arc::new(Mutex::new(ServerFramebuffer::new(
            Framebuffer::new(130, 2).unwrap(),
        )));
        let mut changed = Framebuffer::new(130, 2).unwrap();
        changed.pixels_mut()[0] = 1;
        changed.pixels_mut()[129] = 2;
        // Damage is compared per 64x64 tile: row 1 of the last tile also
        // syncs its row 0, while the undamaged first tile is skipped.
        let damage = DamageRect {
            x: 128,
            y: 1,
            width: 2,
            height: 1,
        };
        update_server_framebuffer_regions(&shared, &changed, &[damage]).unwrap();
        {
            let fb = shared.lock().unwrap();
            assert_eq!(fb.tile_revisions, [0, 0, 1]);
            assert_eq!(fb.framebuffer.pixels()[0], 0);
            assert_eq!(fb.framebuffer.pixels()[129], 2);
        }
        let damage = DamageRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        update_server_framebuffer_regions(&shared, &changed, &[damage]).unwrap();
        {
            let fb = shared.lock().unwrap();
            assert_eq!(fb.tile_revisions, [2, 0, 1]);
            assert_eq!(fb.framebuffer.pixels()[0], 1);
        }
        let outside = DamageRect {
            x: 129,
            y: 0,
            width: 2,
            height: 1,
        };
        assert!(update_server_framebuffer_regions(&shared, &changed, &[outside]).is_err());
    }

    #[test]
    fn server_pixel_format_encodes_rgb565_in_both_byte_orders() {
        let little = ServerPixelFormat {
            bits_per_pixel: 16,
            big_endian: false,
            red_max: 31,
            green_max: 63,
            blue_max: 31,
            red_shift: 11,
            green_shift: 5,
            blue_shift: 0,
        };
        let mut pixel = Vec::new();
        little.encode(0x00ff_0000, &mut pixel);
        assert_eq!(pixel, [0x00, 0xf8]);
        pixel.clear();
        little.encode(0x0000_ff00, &mut pixel);
        assert_eq!(pixel, [0xe0, 0x07]);
        pixel.clear();
        little.encode(0x0000_00ff, &mut pixel);
        assert_eq!(pixel, [0x1f, 0x00]);
        pixel.clear();
        little.encode(0x00ff_ffff, &mut pixel);
        assert_eq!(pixel, [0xff, 0xff]);

        let big = ServerPixelFormat {
            big_endian: true,
            ..little
        };
        pixel.clear();
        big.encode(0x00ff_0000, &mut pixel);
        assert_eq!(pixel, [0xf8, 0x00]);

        let mut invalid = [0, 0, 0, 16, 16, 0, 1, 0, 31, 0, 63, 0, 31, 5, 5, 0, 0, 0, 0];
        assert!(ServerPixelFormat::parse(&invalid).is_err());
        invalid[13] = 11;
        invalid[8] = 30;
        assert!(ServerPixelFormat::parse(&invalid).is_err());
    }

    #[test]
    fn server_pixel_format_encodes_8_and_32_bit_channel_layouts() {
        let rgb332_bytes = [0, 0, 0, 8, 8, 0, 1, 0, 7, 0, 7, 0, 3, 5, 2, 0, 0, 0, 0];
        let rgb332 = ServerPixelFormat::parse(&rgb332_bytes).unwrap();
        let mut pixel = Vec::new();
        rgb332.encode(0x00ff_0000, &mut pixel);
        assert_eq!(pixel, [0xe0]);
        pixel.clear();
        rgb332.encode(0x0000_ff00, &mut pixel);
        assert_eq!(pixel, [0x1c]);
        pixel.clear();
        rgb332.encode(0x0000_00ff, &mut pixel);
        assert_eq!(pixel, [0x03]);

        let mut bytes = [
            0, 0, 0, 32, 24, 1, 1, 0, 255, 0, 255, 0, 255, 0, 8, 16, 0, 0, 0,
        ];
        let big_endian = ServerPixelFormat::parse(&bytes).unwrap();
        pixel.clear();
        big_endian.encode(0x00ff_0000, &mut pixel);
        assert_eq!(pixel, [0x00, 0x00, 0x00, 0xff]);
        bytes[5] = 2;
        assert!(ServerPixelFormat::parse(&bytes).is_err());
    }

    #[test]
    fn server_requires_explicit_insecure_mode_and_serves_raw_framebuffer() {
        let framebuffer = Framebuffer::new(2, 1).unwrap();
        let denied = VncServer::bind("127.0.0.1:0", framebuffer, ServerConfig::default());
        assert!(
            matches!(denied, Err(ref error) if error.kind() == io::ErrorKind::PermissionDenied)
        );

        let mut framebuffer = Framebuffer::new(2, 1).unwrap();
        framebuffer
            .apply_raw(0, 0, 2, 1, &[0, 0, 255, 0, 0, 255, 0, 0])
            .unwrap();
        let config = ServerConfig {
            password: Some("test-pass".into()),
            ..ServerConfig::default()
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, events_rx) = std::sync::mpsc::sync_channel(1024);
        let shared = Arc::new(Mutex::new(ServerFramebuffer::new(framebuffer)));
        let server_fb = Arc::clone(&shared);
        let sessions = Arc::new(Mutex::new(ServerSessions::default()));
        let server_sessions = Arc::clone(&sessions);
        let clipboard = Arc::new(Mutex::new(ServerClipboard::default()));
        let server_clipboard = Arc::clone(&clipboard);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            server_sessions
                .lock()
                .unwrap()
                .register(44, stream.try_clone().unwrap())
                .unwrap();
            let result = serve_client(
                &mut stream,
                &server_fb,
                &tx,
                &config,
                &server_sessions,
                &server_clipboard,
                44,
            );
            let _ = tx.send(ClientEvent::ClientDisconnected { client_id: 44 });
            result
        });

        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut banner = [0; 12];
        client.read_exact(&mut banner).unwrap();
        assert_eq!(&banner, b"RFB 003.008\n");
        client.write_all(b"RFB 003.008\n").unwrap();
        let mut security = [0; 2];
        client.read_exact(&mut security).unwrap();
        assert_eq!(security, [1, 2]);
        client.write_all(&[2]).unwrap();
        let mut challenge = [0; 16];
        client.read_exact(&mut challenge).unwrap();
        client
            .write_all(&vnc_response(&challenge, "test-pass"))
            .unwrap();
        let mut result = [0; 4];
        client.read_exact(&mut result).unwrap();
        assert_eq!(result, [0; 4]);
        client.write_all(&[1]).unwrap();
        let mut init = [0; 24];
        client.read_exact(&mut init).unwrap();
        assert_eq!(&init[..4], &[0, 2, 0, 1]);
        let name_len = u32::from_be_bytes(init[20..24].try_into().unwrap()) as usize;
        let mut name = vec![0; name_len];
        client.read_exact(&mut name).unwrap();
        // Select little-endian RGB565, then Raw encoding.
        client
            .write_all(&[
                0, 0, 0, 0, 16, 16, 0, 1, 0, 31, 0, 63, 0, 31, 11, 5, 0, 0, 0, 0,
            ])
            .unwrap();
        client.write_all(&[2, 0, 0, 1, 0, 0, 0, 0]).unwrap();
        client.write_all(&[3, 0, 0, 0, 0, 0, 0, 2, 0, 1]).unwrap();
        let mut update = [0; 16];
        client.read_exact(&mut update).unwrap();
        assert_eq!(&update[..4], &[0, 0, 0, 1]);
        assert_eq!(&update[4..16], &[0, 0, 0, 0, 0, 2, 0, 1, 0, 0, 0, 0]);
        let mut pixels = [0; 4];
        client.read_exact(&mut pixels).unwrap();
        assert_eq!(pixels, [0x00, 0xf8, 0xe0, 0x07]);
        client.write_all(&[3, 1, 0, 0, 0, 0, 0, 2, 0, 1]).unwrap();
        let mut unchanged_update = [0; 4];
        client.read_exact(&mut unchanged_update).unwrap();
        assert_eq!(unchanged_update, [0, 0, 0, 0]);
        let mut changed_framebuffer = Framebuffer::new(2, 1).unwrap();
        changed_framebuffer
            .apply_raw(0, 0, 2, 1, &[0, 0, 255, 0, 255, 0, 0, 0])
            .unwrap();
        update_server_framebuffer(&shared, &changed_framebuffer).unwrap();
        client.write_all(&[3, 1, 0, 0, 0, 0, 0, 2, 0, 1]).unwrap();
        let mut changed_update = [0; 4];
        client.read_exact(&mut changed_update).unwrap();
        assert_eq!(changed_update, [0, 0, 0, 1]);
        let mut changed_rect = [0; 12];
        client.read_exact(&mut changed_rect).unwrap();
        assert_eq!(&changed_rect[..8], &[0, 0, 0, 0, 0, 2, 0, 1]);
        assert_eq!(&changed_rect[8..], &[0, 0, 0, 0]);
        let mut changed_pixels = [0; 4];
        client.read_exact(&mut changed_pixels).unwrap();
        assert_eq!(changed_pixels, [0x00, 0xf8, 0x1f, 0x00]);
        client.write_all(&[4, 1, 0, 0, 0, 0, 0, b'x']).unwrap();
        assert_eq!(
            events_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            ClientEvent::Key {
                client_id: 44,
                keysym: 120,
                down: true
            }
        );
        client.write_all(&[5, 1, 0, 1, 0, 0]).unwrap();
        assert_eq!(
            events_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            ClientEvent::Pointer {
                client_id: 44,
                buttons: 1,
                x: 1,
                y: 0
            }
        );
        client
            .write_all(&[6, 0, 0, 0, 0, 0, 0, 3, b'h', 0xe9, b'!'])
            .unwrap();
        assert_eq!(
            events_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            ClientEvent::ClipboardText {
                client_id: 44,
                text: vec![b'h', 0xe9, b'!']
            }
        );
        drop(client);
        assert_eq!(
            events_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            ClientEvent::ClientDisconnected { client_id: 44 }
        );
        assert!(server.join().unwrap().is_err());
    }

    #[test]
    fn server_can_stop_accepting_cleanly() {
        let config = ServerConfig {
            allow_insecure: true,
            ..ServerConfig::default()
        };
        let server = Arc::new(
            VncServer::bind("127.0.0.1:0", Framebuffer::new(1, 1).unwrap(), config).unwrap(),
        );
        let runner = Arc::clone(&server);
        let thread = std::thread::spawn(move || runner.run());
        std::thread::sleep(Duration::from_millis(20));
        server.stop();
        thread.join().unwrap().unwrap();
    }

    #[test]
    fn topvnc_client_interoperates_with_embedded_server() {
        let mut initial = Framebuffer::new(2, 1).unwrap();
        initial
            .apply_raw(0, 0, 2, 1, &[0, 0, 255, 0, 0, 255, 0, 0])
            .unwrap();
        let config = ServerConfig {
            allow_insecure: true,
            ..ServerConfig::default()
        };
        let server = Arc::new(VncServer::bind("127.0.0.1:0", initial, config).unwrap());
        let address = server.local_addr().unwrap().to_string();
        let runner = Arc::clone(&server);
        let server_thread = std::thread::spawn(move || runner.run());

        let mut session = Session::connect(&address, true, || unreachable!()).unwrap();
        let writer = session.writer();
        let mut received = Framebuffer::new(2, 1).unwrap();
        let mut scratch = Vec::new();
        writer.request_update(false, 2, 1).unwrap();
        session.read_update(&mut received, &mut scratch).unwrap();
        assert_eq!(received.pixels(), &[0xff0000, 0x00ff00]);

        let mut changed = Framebuffer::new(2, 1).unwrap();
        changed
            .apply_raw(0, 0, 2, 1, &[0, 0, 255, 0, 255, 0, 0, 0])
            .unwrap();
        server.update_framebuffer(&changed).unwrap();
        writer.request_update(true, 2, 1).unwrap();
        session.read_update(&mut received, &mut scratch).unwrap();
        assert_eq!(received.pixels(), &[0xff0000, 0x0000ff]);

        writer.key(0x61, true).unwrap();
        assert_eq!(
            server.recv_event_timeout(Duration::from_secs(1)).unwrap(),
            ClientEvent::Key {
                client_id: 1,
                keysym: 0x61,
                down: true,
            }
        );
        writer.pointer(1, 1, 0).unwrap();
        assert_eq!(
            server.recv_event_timeout(Duration::from_secs(1)).unwrap(),
            ClientEvent::Pointer {
                client_id: 1,
                buttons: 1,
                x: 1,
                y: 0,
            }
        );

        let mut session_two = Session::connect(&address, true, || unreachable!()).unwrap();
        let writer_two = session_two.writer();
        let mut received_two = Framebuffer::new(2, 1).unwrap();
        writer_two.request_update(false, 2, 1).unwrap();
        session_two
            .read_update(&mut received_two, &mut Vec::new())
            .unwrap();
        writer_two.key(0x62, true).unwrap();
        assert_eq!(
            server.recv_event_timeout(Duration::from_secs(1)).unwrap(),
            ClientEvent::Key {
                client_id: 2,
                keysym: 0x62,
                down: true,
            }
        );

        drop(writer);
        drop(session);
        assert_eq!(
            server.recv_event_timeout(Duration::from_secs(1)).unwrap(),
            ClientEvent::ClientDisconnected { client_id: 1 }
        );
        writer_two.key(0x62, false).unwrap();
        assert_eq!(
            server.recv_event_timeout(Duration::from_secs(1)).unwrap(),
            ClientEvent::Key {
                client_id: 2,
                keysym: 0x62,
                down: false,
            }
        );
        drop(writer_two);
        drop(session_two);
        assert_eq!(
            server.recv_event_timeout(Duration::from_secs(1)).unwrap(),
            ClientEvent::ClientDisconnected { client_id: 2 }
        );
        server.stop();
        server_thread.join().unwrap().unwrap();
    }

    #[test]
    fn server_reports_rfb_38_authentication_failure_reason() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let framebuffer = Arc::new(Mutex::new(ServerFramebuffer::new(
            Framebuffer::new(1, 1).unwrap(),
        )));
        let (events, _receiver) = std::sync::mpsc::sync_channel(8);
        let sessions = Arc::new(Mutex::new(ServerSessions::default()));
        let clipboard = Arc::new(Mutex::new(ServerClipboard::default()));
        let config = ServerConfig {
            password: Some("secret".into()),
            ..ServerConfig::default()
        };
        let server_framebuffer = Arc::clone(&framebuffer);
        let server_thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            serve_client(
                &mut stream,
                &server_framebuffer,
                &events,
                &config,
                &sessions,
                &clipboard,
                7,
            )
        });

        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut banner = [0; 12];
        client.read_exact(&mut banner).unwrap();
        client.write_all(b"RFB 003.008\n").unwrap();
        let mut security = [0; 2];
        client.read_exact(&mut security).unwrap();
        assert_eq!(security, [1, 2]);
        client.write_all(&[2]).unwrap();
        let mut challenge = [0; 16];
        client.read_exact(&mut challenge).unwrap();
        client
            .write_all(&vnc_response(&challenge, "incorrect"))
            .unwrap();
        let mut result = [0; 8];
        client.read_exact(&mut result).unwrap();
        assert_eq!(&result[..4], &1u32.to_be_bytes());
        let reason_len = u32::from_be_bytes(result[4..8].try_into().unwrap()) as usize;
        let mut reason = vec![0; reason_len];
        client.read_exact(&mut reason).unwrap();
        assert_eq!(reason, b"VNC authentication failed");
        assert_eq!(
            server_thread.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    struct PausedReader {
        input: Cursor<Vec<u8>>,
        pause_at: u64,
        paused: bool,
    }

    impl Read for PausedReader {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if !self.paused && self.input.position() == self.pause_at {
                self.paused = true;
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let count = if self.paused {
                bytes.len()
            } else {
                bytes
                    .len()
                    .min((self.pause_at - self.input.position()) as usize)
            };
            self.input.read(&mut bytes[..count])
        }
    }

    fn raw_pixel(pixel: [u8; 4]) -> Vec<u8> {
        let mut message = vec![0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0];
        message.extend_from_slice(&pixel);
        message
    }

    #[test]
    fn refresh_preserves_partial_headers_and_pixels() {
        let message = raw_pixel([0x33, 0x22, 0x11, 0]);
        for pause_at in 0..message.len() as u64 {
            let mut refreshes = 0;
            let mut reader = RefreshReader {
                reader: PausedReader {
                    input: Cursor::new(message.clone()),
                    pause_at,
                    paused: false,
                },
                refresh: || {
                    refreshes += 1;
                    Ok(())
                },
                requested: false,
            };
            let mut framebuffer = Framebuffer::new(1, 1).unwrap();
            read_update(&mut reader, &mut framebuffer, &mut Vec::new()).unwrap();
            assert_eq!(framebuffer.pixels(), &[0x112233], "pause at {pause_at}");
            assert_eq!(refreshes, 1);
        }
    }

    #[test]
    fn unresponsive_server_gets_one_refresh_then_an_error() {
        struct Silent;
        impl Read for Silent {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::TimedOut.into())
            }
        }
        let mut refreshes = 0;
        let mut reader = RefreshReader {
            reader: Silent,
            refresh: || {
                refreshes += 1;
                Ok(())
            },
            requested: false,
        };
        let error = reader.read_exact(&mut [0]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("screen refresh timed out"));
        assert_eq!(refreshes, 1);
    }

    fn socket_pair() -> (TcpStream, TcpStream) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        server
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        (client, server)
    }

    #[test]
    fn silent_handshake_times_out() {
        let (client, _server) = socket_pair();
        let result =
            Session::from_stream(client, true, || unreachable!(), Duration::from_millis(50));
        let error = result.err().expect("silent server must time out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("handshake timed out"));
    }

    #[test]
    fn stalled_incremental_update_recovers_over_tcp() {
        let (client, mut server) = socket_pair();
        let worker = std::thread::spawn(move || {
            server.write_all(mock_server().input.get_ref()).unwrap();
            let mut handshake = [0; 42];
            server.read_exact(&mut handshake).unwrap();
            let mut request = [0; 10];
            server.read_exact(&mut request).unwrap();
            assert_eq!(request, [3, 0, 0, 0, 0, 0, 0, 2, 0, 1]);
            server.write_all(&raw_pixel([1, 2, 3, 0])).unwrap();
            server.read_exact(&mut request).unwrap();
            assert_eq!(request[1], 1);
            // Simulate a screen transition that stops incremental responses.
            server.read_exact(&mut request).unwrap();
            assert_eq!(request, [3, 0, 0, 0, 0, 0, 0, 2, 0, 1]);
            server.write_all(&raw_pixel([4, 5, 6, 0])).unwrap();
            server.read_exact(&mut request).unwrap();
            assert_eq!(request[1], 1);
            server.write_all(&raw_pixel([7, 8, 9, 0])).unwrap();
        });
        let mut session =
            Session::from_stream(client, true, || unreachable!(), Duration::from_secs(2)).unwrap();
        session
            .reader
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let mut frame = Framebuffer::new(2, 1).unwrap();
        let mut scratch = Vec::new();
        for (incremental, pixel) in [(false, 0x030201), (true, 0x060504), (true, 0x090807)] {
            session.writer().request_update(incremental, 2, 1).unwrap();
            session.read_update(&mut frame, &mut scratch).unwrap();
            assert_eq!(frame.pixels(), &[pixel, 0]);
        }
        worker.join().unwrap();
    }

    struct MockStream {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for MockStream {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.input.read(bytes)
        }
    }

    impl Write for MockStream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn mock_server() -> MockStream {
        let mut input = b"RFB 003.008\n".to_vec();
        input.extend_from_slice(&[1, 1]); // One security type: None.
        input.extend_from_slice(&[0; 4]); // SecurityResult: OK.
        let mut init = [0; 24];
        init[0..2].copy_from_slice(&2u16.to_be_bytes());
        init[2..4].copy_from_slice(&1u16.to_be_bytes());
        init[20..24].copy_from_slice(&4u32.to_be_bytes());
        input.extend_from_slice(&init);
        input.extend_from_slice(b"Test");
        MockStream {
            input: Cursor::new(input),
            output: Vec::new(),
        }
    }

    #[test]
    fn handshake_requires_explicit_none_security() {
        let mut server = mock_server();
        assert_eq!(
            negotiate(&mut server, false, || unreachable!())
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(server.output, b"RFB 003.008\n");
    }

    #[test]
    fn handshake_requests_raw_32_bit_framebuffer() {
        let mut server = mock_server();
        let info = negotiate(&mut server, true, || unreachable!()).unwrap();
        assert_eq!(
            (info.width, info.height, info.name.as_str()),
            (2, 1, "Test")
        );
        assert_eq!(&server.output[12..14], &[1, 1]); // Security type, ClientInit.
        assert_eq!(&server.output[14..18], &[0, 0, 0, 0]); // SetPixelFormat header.
        assert_eq!(&server.output[18..22], &[32, 24, 0, 1]);
        assert_eq!(&server.output[34..], &[2, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(info.security, Security::None);
    }

    #[test]
    fn handshake_advertises_zlib_only_when_selected() {
        let mut server = mock_server();
        negotiate_with_encoding(&mut server, true, Encoding::Zlib, || unreachable!()).unwrap();
        assert_eq!(&server.output[34..], &[2, 0, 0, 1, 0, 0, 0, 6]);
    }

    #[test]
    fn zlib_rectangles_share_a_stream_across_updates() {
        let mut compressor = Compress::new(ZlibLevel::default(), true);
        let mut decoder = Decompress::new(true);
        let mut scratch = Vec::new();
        let mut frame = Framebuffer::new(2, 1).unwrap();
        for pixel in [[1, 2, 3, 0], [4, 5, 6, 0]] {
            let mut compressed = [0; 128];
            let input_before = compressor.total_in();
            let output_before = compressor.total_out();
            compressor
                .compress(&pixel, &mut compressed, FlushCompress::Sync)
                .unwrap();
            assert_eq!(compressor.total_in() - input_before, 4);
            let length = (compressor.total_out() - output_before) as usize;
            let mut update = vec![0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 1];
            update.extend_from_slice(&6i32.to_be_bytes());
            update.extend_from_slice(&(length as u32).to_be_bytes());
            update.extend_from_slice(&compressed[..length]);
            read_update_with_encoding(
                &mut Cursor::new(update),
                2,
                1,
                &mut scratch,
                Encoding::Zlib,
                &mut decoder,
                |x, y, width, height, bytes| frame.apply_raw(x, y, width, height, bytes),
            )
            .unwrap();
            assert_eq!(
                frame.pixels()[0],
                (u32::from(pixel[2]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[0])
            );
        }
    }

    #[test]
    fn raw_mode_rejects_unadvertised_zlib() {
        let update = [0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 6];
        let mut scratch = Vec::new();
        assert_eq!(
            read_update_with(
                &mut Cursor::new(update),
                2,
                1,
                &mut scratch,
                |_, _, _, _, _| Ok(())
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn zlib_rejects_oversized_and_corrupt_rectangles() {
        let mut header = vec![0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 1];
        header.extend_from_slice(&6i32.to_be_bytes());
        let mut oversized = header.clone();
        oversized.extend_from_slice(&65_541u32.to_be_bytes());
        let mut scratch = Vec::new();
        let mut decoder = Decompress::new(true);
        assert_eq!(
            read_update_with_encoding(
                &mut Cursor::new(oversized),
                2,
                1,
                &mut scratch,
                Encoding::Zlib,
                &mut decoder,
                |_, _, _, _, _| Ok(())
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidData
        );
        header.extend_from_slice(&4u32.to_be_bytes());
        header.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(
            read_update_with_encoding(
                &mut Cursor::new(header),
                2,
                1,
                &mut scratch,
                Encoding::Zlib,
                &mut decoder,
                |_, _, _, _, _| Ok(())
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidData
        );
    }

    fn server_init(input: &mut Vec<u8>) {
        let mut init = [0; 24];
        init[0..2].copy_from_slice(&2u16.to_be_bytes());
        init[2..4].copy_from_slice(&1u16.to_be_bytes());
        input.extend_from_slice(&init);
    }

    #[test]
    fn rfb_33_none_omits_security_result() {
        let mut input = b"RFB 003.003\n".to_vec();
        input.extend_from_slice(&1u32.to_be_bytes());
        server_init(&mut input);
        let mut server = MockStream {
            input: Cursor::new(input),
            output: Vec::new(),
        };
        let info = negotiate(&mut server, true, || unreachable!()).unwrap();
        assert_eq!(info.security, Security::None);
        assert_eq!(&server.output[..12], b"RFB 003.003\n");
    }

    #[test]
    fn rfb_37_none_omits_security_result() {
        let mut input = b"RFB 003.007\n".to_vec();
        input.extend_from_slice(&[1, 1]);
        server_init(&mut input);
        let mut server = MockStream {
            input: Cursor::new(input),
            output: Vec::new(),
        };
        let info = negotiate(&mut server, true, || unreachable!()).unwrap();
        assert_eq!(info.security, Security::None);
        assert_eq!(&server.output[..12], b"RFB 003.007\n");
    }

    #[test]
    fn apple_banner_uses_standard_38_and_vnc_password() {
        let mut input = b"RFB 003.889\n".to_vec();
        input.extend_from_slice(&[5, 30, 33, 36, 2, 35]);
        input.extend(0u8..16); // Challenge.
        input.extend_from_slice(&0u32.to_be_bytes()); // SecurityResult.
        server_init(&mut input);
        let mut server = MockStream {
            input: Cursor::new(input),
            output: Vec::new(),
        };
        let info = negotiate(&mut server, false, || Ok("password".to_owned())).unwrap();
        assert_eq!(info.security, Security::VncPassword);
        assert_eq!(&server.output[..12], b"RFB 003.008\n");
        assert_eq!(server.output[12], 2);
        assert_eq!(server.output.len(), 12 + 1 + 16 + 1 + 20 + 8);
    }

    #[test]
    fn vnc_password_response_matches_independent_des_vector() {
        let challenge: [u8; 16] = core::array::from_fn(|index| index as u8);
        assert_eq!(
            vnc_response(&challenge, "password"),
            [
                0xb8, 0x66, 0x92, 0x41, 0x25, 0xc8, 0xee, 0xbb, 0x9d, 0xeb, 0xc1, 0xdb, 0x61, 0xc5,
                0x38, 0xe2,
            ]
        );
        let response = vnc_response(&challenge, "password");
        assert!(constant_time_equal(&response, &response));
        let mut incorrect = response;
        incorrect[15] ^= 1;
        assert!(!constant_time_equal(&response, &incorrect));
    }

    #[test]
    fn raw_rectangle_converts_and_preserves_other_pixels() {
        let mut fb = Framebuffer::new(2, 2).unwrap();
        fb.apply_raw(1, 0, 1, 1, &[3, 2, 1, 0]).unwrap();
        assert_eq!(fb.pixels(), &[0, 0x010203, 0, 0]);
    }

    #[test]
    fn rejects_out_of_bounds_rectangle() {
        let mut fb = Framebuffer::new(2, 2).unwrap();
        assert!(fb.apply_raw(2, 0, 1, 1, &[0; 4]).is_err());
    }

    #[test]
    fn update_request_parses_full_width_coordinates_and_validates_incremental_flag() {
        let request = parse_update_request([0, 4, 0, 0, 0, 1, 0, 1, 0]).unwrap();
        assert!(!request.incremental);
        assert_eq!(request.x, 1024);
        assert_eq!(request.width, 256);
        assert_eq!(request.height, 256);

        assert_eq!(
            parse_update_request([2, 0, 0, 0, 0, 1, 0, 1, 0])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn client_message_reserved_padding_must_be_zero() {
        validate_zero_padding(&[0, 0, 0], "padding").unwrap();
        assert_eq!(
            validate_zero_padding(&[0, 1], "padding")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn rejects_unexpected_encoding_before_payload() {
        let mut fb = Framebuffer::new(1, 1).unwrap();
        let message = [0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 5];
        assert!(read_update(&mut &message[..], &mut fb, &mut Vec::new()).is_err());
    }

    #[test]
    fn reads_raw_framebuffer_update() {
        let mut fb = Framebuffer::new(2, 1).unwrap();
        let message = [
            0, 0, 0, 1, // FramebufferUpdate with one rectangle.
            0, 1, 0, 0, 0, 1, 0, 1, // x=1, y=0, width=1, height=1.
            0, 0, 0, 0, // Raw encoding.
            0x33, 0x22, 0x11, 0, // B, G, R, unused.
        ];
        read_update(&mut &message[..], &mut fb, &mut Vec::new()).unwrap();
        assert_eq!(fb.pixels(), &[0, 0x112233]);
    }

    #[test]
    fn input_packets_use_network_byte_order() {
        assert_eq!(key_packet(0xff51, true), [4, 1, 0, 0, 0, 0, 0xff, 0x51]);
        assert_eq!(
            pointer_packet(5, 0x1234, 0xabcd),
            [5, 5, 0x12, 0x34, 0xab, 0xcd]
        );
    }
}
