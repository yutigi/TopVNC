//! Shared RFB protocol and framebuffer code for TopVNC.

#![forbid(unsafe_code)]

use des::Des;
use des::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use flate2::{Decompress, FlushDecompress};
use std::io::{self, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod fovea;
mod tight;
use tight::{TightDecoder, TightSettings};

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
/// Total time a client has to finish the handshake, including typing its
/// password, before its connection slot is released.
const SERVER_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
const DESKTOP_SIZE_ENCODING: i32 = -223;
/// Pseudo-encoding: the client understands Fence messages.
const FENCE_ENCODING: i32 = -312;
/// Pseudo-encoding: the client understands ContinuousUpdates.
const CONTINUOUS_UPDATES_ENCODING: i32 = -313;
/// Message type of EnableContinuousUpdates (client) and
/// EndOfContinuousUpdates (server).
const CONTINUOUS_UPDATES_MESSAGE: u8 = 150;
const FENCE_MESSAGE: u8 = 248;
const FENCE_BLOCK_BEFORE: u32 = 1 << 0;
const FENCE_BLOCK_AFTER: u32 = 1 << 1;
const FENCE_SYNC_NEXT: u32 = 1 << 2;
const FENCE_REQUEST: u32 = 1 << 31;
const FENCE_SUPPORTED_FLAGS: u32 = FENCE_BLOCK_BEFORE | FENCE_BLOCK_AFTER | FENCE_SYNC_NEXT;
const MAX_FENCE_PAYLOAD: usize = 64;
/// Pseudo-encoding (QEMU Pointer Motion Change): the client can send
/// relative pointer motion when the server asks for it.
const POINTER_MOTION_CHANGE_ENCODING: i32 = -257;
/// Pseudo-encoding: the client can send extended PointerEvents with the
/// back and forward buttons.
const EXTENDED_MOUSE_BUTTONS_ENCODING: i32 = -316;
/// Relative PointerEvent coordinates are deltas offset by this value.
const RELATIVE_POINTER_ORIGIN: i32 = 0x7fff;
/// Largest delta one relative PointerEvent carries; larger motion is split.
/// Relative coordinates therefore lie in 0x4000..=0xbffe, and absolute ones
/// below `MAX_FRAMEBUFFER_DIMENSION`, so the server can tell which mode a
/// client used for every event, including ones in flight at a mode change.
const MAX_RELATIVE_DELTA: i32 = 0x3fff;
const _: () =
    assert!((MAX_FRAMEBUFFER_DIMENSION as i32) < RELATIVE_POINTER_ORIGIN - MAX_RELATIVE_DELTA);
/// Marks an extended PointerEvent in *button-mask* once ExtendedMouseButtons
/// is negotiated; otherwise the bit is the back button.
const EXTENDED_POINTER_MARKER: u8 = 0x80;
/// Continuous updates keep at most this many unacknowledged updates on the
/// wire, so a slow link cannot build up a queue of stale frames.
const MAX_UPDATES_IN_FLIGHT: usize = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const UPDATE_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
/// Client socket read buffer; large enough for many small rectangles per read.
const CLIENT_READ_BUFFER_BYTES: usize = 256 * 1024;
/// When the client waited longer than this for one update's data, the link
/// is the bottleneck: requesting the next update early would only queue a
/// second frame behind the first, so it is requested after the update.
/// JPEG decoding runs on other threads, so the wait is about the update's
/// transmission time; below a 60 Hz frame interval, the link carries every
/// frame of a 60 fps source and requesting early only removes idle gaps.
const PIPELINE_MAX_NETWORK_WAIT: Duration = Duration::from_millis(14);
/// At most this many threads decode JPEG rectangles.
const MAX_JPEG_THREADS: usize = 8;
/// Bytes of buffers kept for reuse between rectangles: enough for every band
/// of a large update in flight, without holding on to a burst of big ones.
const MAX_SPARE_BYTES: usize = 32 * 1024 * 1024;
/// Updates with fewer pixels than this are encoded on the session thread.
const SERVER_PARALLEL_ENCODE_PIXELS: usize = 128 * 1024;
/// Encoded Tight bands are written once this much is waiting, so the link
/// carries the first bands while later ones are still being encoded.
const SERVER_STREAM_FLUSH_BYTES: usize = 4 * 1024;
/// The compression level the client requests with Tight: fast zlib, since
/// most gaming content is sent as JPEG anyway.
const CLIENT_TIGHT_COMPRESS_LEVEL: u8 = 1;

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

/// Running totals for a client session, readable from any thread.
#[derive(Clone, Default)]
pub struct SessionStats(Arc<StatsCounters>);

#[derive(Default)]
struct StatsCounters {
    network_wait_nanos: std::sync::atomic::AtomicU64,
    bytes: std::sync::atomic::AtomicU64,
    frames: std::sync::atomic::AtomicU64,
    /// The last rectangle's wire encoding, or `i64::MIN` before the first.
    last_encoding: std::sync::atomic::AtomicI64,
    continuous_updates: AtomicBool,
}

/// A point-in-time copy of [`SessionStats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    /// Bytes received from the server since the handshake.
    pub bytes: u64,
    /// Framebuffer updates that changed at least one rectangle.
    pub frames: u64,
    /// The encoding of the most recent rectangle.
    pub encoding: Option<i32>,
    /// The server pushes updates without a request per frame.
    pub continuous_updates: bool,
}

impl SessionStats {
    fn new() -> Self {
        let stats = Self::default();
        stats.0.last_encoding.store(i64::MIN, Ordering::Relaxed);
        stats
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        let encoding = self.0.last_encoding.load(Ordering::Relaxed);
        StatsSnapshot {
            bytes: self.0.bytes.load(Ordering::Relaxed),
            frames: self.0.frames.load(Ordering::Relaxed),
            encoding: (encoding != i64::MIN).then_some(encoding as i32),
            continuous_updates: self.0.continuous_updates.load(Ordering::Relaxed),
        }
    }
}

/// A short name for an RFB encoding number.
pub fn encoding_name(encoding: i32) -> &'static str {
    match encoding {
        0 => "Raw",
        6 => "Zlib",
        tight::TIGHT_ENCODING => "Tight",
        _ => "other",
    }
}

/// Counts received bytes and the time spent blocked reading the socket.
struct WaitTimer<R> {
    inner: R,
    stats: SessionStats,
}

impl<R: Read> Read for WaitTimer<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let started = Instant::now();
        let result = self.inner.read(bytes);
        let counters = &self.stats.0;
        counters.network_wait_nanos.fetch_add(
            started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
        if let Ok(count) = result {
            counters.bytes.fetch_add(count as u64, Ordering::Relaxed);
        }
        result
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
    /// Tight with JPEG at an RFB quality level from 0 (smallest) to 9 (best).
    /// Zlib and Raw are accepted from servers without Tight.
    Tight {
        quality: u8,
    },
}

impl Encoding {
    /// The SetEncodings list the client sends, most preferred first.
    fn advertised(self) -> Vec<i32> {
        let mut encodings = match self {
            Self::Raw => vec![0],
            Self::Zlib => vec![6],
            Self::Tight { quality } => vec![
                tight::TIGHT_ENCODING,
                6,
                0,
                tight::QUALITY_LEVEL_0 + i32::from(quality.min(9)),
                tight::COMPRESS_LEVEL_0 + i32::from(CLIENT_TIGHT_COMPRESS_LEVEL),
            ],
        };
        // Servers that support them push updates without a request per
        // frame, with fences for flow control.
        encodings.extend([FENCE_ENCODING, CONTINUOUS_UPDATES_ENCODING]);
        // Relative motion for games that capture the mouse, and the back
        // and forward buttons.
        encodings.extend([
            POINTER_MOTION_CHANGE_ENCODING,
            EXTENDED_MOUSE_BUTTONS_ENCODING,
        ]);
        encodings
    }

    fn accepts(self, wire_encoding: i32) -> bool {
        matches!(
            (self, wire_encoding),
            (_, 0)
                | (Self::Zlib | Self::Tight { .. }, 6)
                | (Self::Tight { .. }, tight::TIGHT_ENCODING)
        )
    }

    /// [`Encoding::accepts`] as flags: bit `n` stands for
    /// `DECODED_ENCODINGS[n]`.
    fn accepted_flags(self) -> u8 {
        DECODED_ENCODINGS
            .iter()
            .enumerate()
            .filter(|(_, wire_encoding)| self.accepts(**wire_encoding))
            .fold(0, |flags, (bit, _)| flags | 1 << bit)
    }
}

/// Every pixel encoding the client decodes.
const DECODED_ENCODINGS: [i32; 3] = [0, 6, tight::TIGHT_ENCODING];

/// Decoder state that persists across framebuffer updates: the Zlib stream,
/// Tight's four zlib streams, and the threads that decode JPEG rectangles.
pub struct UpdateDecoder {
    zlib: Decompress,
    tight: TightDecoder,
    /// Started with the first JPEG rectangle.
    jpeg: Option<JpegPool>,
    /// Buffers returned by finished rectangles, for reuse, and their total
    /// capacity.
    spare: Vec<Vec<u8>>,
    spare_bytes: usize,
}

impl UpdateDecoder {
    pub fn new() -> Self {
        Self {
            zlib: Decompress::new(true),
            tight: TightDecoder::new(),
            jpeg: None,
            spare: Vec::new(),
            spare_bytes: 0,
        }
    }

    fn buffer(&mut self) -> Vec<u8> {
        let buffer = self.spare.pop().unwrap_or_default();
        self.spare_bytes -= buffer.capacity();
        buffer
    }

    fn recycle(&mut self, buffer: Vec<u8>) {
        if self.spare_bytes + buffer.capacity() <= MAX_SPARE_BYTES {
            self.spare_bytes += buffer.capacity();
            self.spare.push(buffer);
        }
    }
}

/// A Tight JPEG rectangle to decode: its index in the update, data, size,
/// and a buffer for the pixels.
struct JpegJob {
    index: usize,
    jpeg: Vec<u8>,
    width: usize,
    height: usize,
    output: Vec<u8>,
    done: std::sync::mpsc::Sender<JpegDone>,
}

struct JpegDone {
    index: usize,
    /// The data buffer, for reuse.
    jpeg: Vec<u8>,
    pixels: io::Result<Vec<u8>>,
}

/// Threads that decode JPEG rectangles while the network thread reads the
/// rest of the update.
struct JpegPool {
    jobs: Option<std::sync::mpsc::Sender<JpegJob>>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl JpegPool {
    fn new(threads: usize) -> io::Result<Self> {
        let (jobs, receiver) = std::sync::mpsc::channel::<JpegJob>();
        let receiver = Arc::new(Mutex::new(receiver));
        let mut pool = Self {
            jobs: Some(jobs),
            workers: Vec::with_capacity(threads),
        };
        for _ in 0..threads {
            let receiver = Arc::clone(&receiver);
            pool.workers.push(
                std::thread::Builder::new()
                    .name("topvnc-jpeg".into())
                    .spawn(move || {
                        loop {
                            let job = match receiver.lock() {
                                Ok(receiver) => receiver.recv(),
                                Err(_) => return,
                            };
                            let Ok(mut job) = job else {
                                return;
                            };
                            let pixels = tight::decode_jpeg_rect(
                                &job.jpeg,
                                job.width,
                                job.height,
                                &mut job.output,
                            )
                            .map(|_| job.output);
                            // The update was abandoned after an error.
                            let _ = job.done.send(JpegDone {
                                index: job.index,
                                jpeg: job.jpeg,
                                pixels,
                            });
                        }
                    })?,
            );
        }
        Ok(pool)
    }
}

impl Drop for JpegPool {
    fn drop(&mut self) {
        // Closing the channel ends the threads.
        self.jobs = None;
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// A rectangle's x, y, width, and height.
type RectArea = (u16, u16, u16, u16);

/// Rectangles of one update in wire order that are not yet applied, while
/// JPEG rectangles decode on the pool. They are applied strictly in order,
/// so overlapping rectangles behave as if decoded one after another.
struct PendingRects {
    /// Position and size, and the pixels once decoded.
    queue: std::collections::VecDeque<(RectArea, Option<Vec<u8>>)>,
    /// The update index of `queue[0]`.
    first: usize,
    /// Rectangles decoding on the pool.
    decoding: usize,
    done: (
        std::sync::mpsc::Sender<JpegDone>,
        std::sync::mpsc::Receiver<JpegDone>,
    ),
}

type Apply<'a> = dyn FnMut(u16, u16, u16, u16, &[u8]) -> io::Result<()> + 'a;

impl PendingRects {
    fn new() -> Self {
        Self {
            queue: std::collections::VecDeque::new(),
            first: 0,
            decoding: 0,
            done: std::sync::mpsc::channel(),
        }
    }

    /// A rectangle decoded on the network thread: applied at once unless an
    /// earlier one is still decoding.
    fn decoded(
        &mut self,
        rect: RectArea,
        pixels: &[u8],
        decoder: &mut UpdateDecoder,
        apply: &mut Apply<'_>,
    ) -> io::Result<()> {
        if self.queue.is_empty() {
            self.first += 1;
            return apply(rect.0, rect.1, rect.2, rect.3, pixels);
        }
        let mut copy = decoder.buffer();
        copy.clear();
        copy.extend_from_slice(pixels);
        self.queue.push_back((rect, Some(copy)));
        Ok(())
    }

    /// Decode a JPEG rectangle on the pool.
    fn decode_jpeg(
        &mut self,
        rect: RectArea,
        jpeg: Vec<u8>,
        decoder: &mut UpdateDecoder,
    ) -> io::Result<()> {
        if decoder.jpeg.is_none() {
            let threads = std::thread::available_parallelism()
                .map_or(1, usize::from)
                .min(MAX_JPEG_THREADS);
            decoder.jpeg = Some(JpegPool::new(threads)?);
        }
        let output = decoder.buffer();
        let index = self.first + self.queue.len();
        let job = JpegJob {
            index,
            jpeg,
            width: usize::from(rect.2),
            height: usize::from(rect.3),
            output,
            done: self.done.0.clone(),
        };
        decoder
            .jpeg
            .as_ref()
            .and_then(|pool| pool.jobs.as_ref())
            .ok_or_else(|| io::Error::other("JPEG decoder threads stopped"))?
            .send(job)
            .map_err(|_| io::Error::other("JPEG decoder threads stopped"))?;
        self.queue.push_back((rect, None));
        self.decoding += 1;
        Ok(())
    }

    /// Apply every rectangle whose pixels are ready, in order. With `wait`,
    /// wait for all of them.
    fn apply_ready(
        &mut self,
        wait: bool,
        decoder: &mut UpdateDecoder,
        apply: &mut Apply<'_>,
    ) -> io::Result<()> {
        loop {
            while let Some((_, Some(_))) = self.queue.front() {
                let (rect, pixels) = self.queue.pop_front().unwrap();
                let pixels = pixels.unwrap();
                self.first += 1;
                apply(rect.0, rect.1, rect.2, rect.3, &pixels)?;
                decoder.recycle(pixels);
            }
            if self.decoding == 0 {
                return Ok(());
            }
            let finished = if wait {
                self.done
                    .1
                    .recv()
                    .map_err(|_| io::Error::other("JPEG decoder threads stopped"))?
            } else {
                match self.done.1.try_recv() {
                    Ok(finished) => finished,
                    Err(_) => return Ok(()),
                }
            };
            self.decoding -= 1;
            decoder.recycle(finished.jpeg);
            self.queue[finished.index - self.first].1 = Some(finished.pixels?);
        }
    }
}

impl Default for UpdateDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Pointer buttons as [`ClientEvent`] and [`InputWriter`] carry them: bits
/// 0-6 as in RFB's *button-mask*, then back and forward.
pub const BUTTON_LEFT: u16 = 1 << 0;
pub const BUTTON_MIDDLE: u16 = 1 << 1;
pub const BUTTON_RIGHT: u16 = 1 << 2;
pub const BUTTON_WHEEL_UP: u16 = 1 << 3;
pub const BUTTON_WHEEL_DOWN: u16 = 1 << 4;
pub const BUTTON_WHEEL_LEFT: u16 = 1 << 5;
pub const BUTTON_WHEEL_RIGHT: u16 = 1 << 6;
pub const BUTTON_BACK: u16 = 1 << 7;
pub const BUTTON_FORWARD: u16 = 1 << 8;

/// Events received from a remote RFB client connected to a [`VncServer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    Key {
        client_id: u64,
        keysym: u32,
        down: bool,
    },
    /// The pointer is at (`x`, `y`) with `buttons` (`BUTTON_*` bits) held.
    Pointer {
        client_id: u64,
        buttons: u16,
        x: u16,
        y: u16,
    },
    /// The pointer moved by (`dx`, `dy`) device units with `buttons` held.
    /// Sent only while the server asks for relative motion; see
    /// [`VncServer::set_relative_pointer`].
    RelativePointer {
        client_id: u64,
        buttons: u16,
        dx: i32,
        dy: i32,
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
    /// Runs first on every thread the server starts for a client: its
    /// session, input reader, and encoder threads. Hosts use it to set a
    /// scheduling class, which new threads do not inherit on every platform.
    pub thread_setup: Option<fn()>,
    /// When to encode Tight updates foveated for first-person games.
    pub foveation: Foveation,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "TopVNC".into(),
            password: None,
            allow_insecure: false,
            thread_setup: None,
            foveation: Foveation::Off,
        }
    }
}

/// When the server encodes Tight updates foveated (spec 008). JPEG quality
/// falls by zone around the framebuffer center, where a first-person game's
/// crosshair is, and the center is sent first. The center starts at the
/// client's quality level; with continuous updates, the zones' quality then
/// follows the link's measured throughput. Applies only to clients that
/// receive Tight JPEG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Foveation {
    /// Every rectangle uses the client's quality level.
    #[default]
    Off,
    /// While the host asks for relative pointer motion
    /// ([`VncServer::set_relative_pointer`]), as it does while a game has
    /// captured the mouse.
    Auto,
    /// Always.
    On,
}

/// A small RFB 3.8 server for applications that provide a framebuffer and
/// consume remote input events. It serves Raw rectangles to concurrent clients
/// and announces size changes with the DesktopSize pseudo-encoding.
/// The caller owns display capture and OS input injection.
pub struct VncServer {
    listener: std::net::TcpListener,
    framebuffer: Arc<Mutex<ServerFramebuffer>>,
    /// The framebuffer's generation and size, readable without its lock.
    geometry: Arc<Geometry>,
    /// The host wants relative pointer motion from clients that support it.
    relative_pointer: Arc<AtomicBool>,
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
    Encodings(ClientEncodings),
    UpdateRequest(UpdateRequest),
    /// EnableContinuousUpdates: start pushing changes in `region`, or stop.
    ContinuousUpdates {
        enable: bool,
        region: UpdateRequest,
    },
    /// A Fence message from the client: a request to echo, or a response
    /// to one of the server's, with when it arrived.
    Fence {
        flags: u32,
        payload: Vec<u8>,
        received: Instant,
    },
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

/// The server framebuffer's generation, width, and height in one atomic, so
/// input readers can check pointer bounds without the framebuffer lock,
/// which capture and encoding hold for milliseconds at a time.
#[derive(Default)]
struct Geometry(std::sync::atomic::AtomicU64);

impl Geometry {
    fn store(&self, generation: u64, width: u16, height: u16) {
        self.0.store(
            (generation << 32) | (u64::from(width) << 16) | u64::from(height),
            Ordering::Release,
        );
    }

    /// The low 32 bits of the generation, the width, and the height.
    fn load(&self) -> (u32, u16, u16) {
        let value = self.0.load(Ordering::Acquire);
        ((value >> 32) as u32, (value >> 16) as u16, value as u16)
    }
}

struct ServerFramebuffer {
    framebuffer: Framebuffer,
    tile_revisions: Vec<u64>,
    revision: u64,
    tile_columns: usize,
    /// Incremented whenever the framebuffer dimensions change.
    generation: u64,
    /// Host frames that changed at least one tile, across resizes; sessions
    /// time the host's frame rate with it.
    frames: u64,
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
            frames: 0,
        }
    }

    fn resize(&mut self, framebuffer: Framebuffer) {
        let generation = self.generation.wrapping_add(1);
        let frames = self.frames;
        *self = Self::new(framebuffer);
        self.generation = generation;
        self.frames = frames;
    }

    fn next_revision(&mut self) -> u64 {
        if self.revision == u64::MAX {
            self.revision = 0;
            self.tile_revisions.fill(0);
        }
        self.revision += 1;
        self.revision
    }

    /// Copy one tile from `source` and bump its revision if any pixel
    /// changed. Returns whether one did.
    fn sync_tile(&mut self, index: usize, source: &Framebuffer) -> bool {
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
        changed
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
        if config.name.len() > MAX_NAME_BYTES {
            return Err(invalid("server name is too long"));
        }
        let listener = std::net::TcpListener::bind(address)?;
        let (events, event_receiver) = std::sync::mpsc::sync_channel(SERVER_EVENT_QUEUE_CAPACITY);
        let geometry = Geometry::default();
        geometry.store(0, framebuffer.width, framebuffer.height);
        Ok(Self {
            listener,
            framebuffer: Arc::new(Mutex::new(ServerFramebuffer::new(framebuffer))),
            geometry: Arc::new(geometry),
            relative_pointer: Arc::new(AtomicBool::new(false)),
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
        update_server_framebuffer(&self.framebuffer, &self.geometry, framebuffer)?;
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
        update_server_framebuffer_regions(&self.framebuffer, &self.geometry, framebuffer, damage)?;
        self.wake_sessions();
        Ok(())
    }

    /// Ask clients that support the QEMU Pointer Motion Change extension to
    /// send relative pointer motion (`true`), as games that capture the mouse
    /// expect, or absolute positions (`false`, the default). Their motion
    /// then arrives as [`ClientEvent::RelativePointer`]. Other clients keep
    /// sending absolute positions.
    pub fn set_relative_pointer(&self, relative: bool) {
        if self.relative_pointer.swap(relative, Ordering::AcqRel) != relative {
            self.wake_sessions();
        }
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
                // A client that resets before it is accepted affects only
                // that connection; keep listening for others.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            if stream.set_nonblocking(false).is_err() {
                continue;
            }
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
            let shared = SessionShared {
                framebuffer: Arc::clone(&self.framebuffer),
                geometry: Arc::clone(&self.geometry),
                relative_pointer: Arc::clone(&self.relative_pointer),
                clipboard: Arc::clone(&self.clipboard),
                thread_setup: self.config.thread_setup,
                foveation: self.config.foveation,
            };
            let events = self.events.clone();
            let config = self.config.clone();
            let active_clients = Arc::clone(&self.active_clients);
            let client_sessions = Arc::clone(&self.sessions);
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
                    if let Some(setup) = shared.thread_setup {
                        setup();
                    }
                    let mut stream = stream;
                    let _ = stream.set_nodelay(true);
                    let _ = serve_client(
                        &mut stream,
                        &shared,
                        &events,
                        &config,
                        &client_sessions,
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
    geometry: &Geometry,
    framebuffer: &Framebuffer,
) -> io::Result<()> {
    update_server_framebuffer_regions(
        shared,
        geometry,
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
    geometry: &Geometry,
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
        // Under the framebuffer lock, so readers see sizes in order.
        geometry.store(current.generation, framebuffer.width, framebuffer.height);
        return Ok(());
    }
    if damage.iter().any(|rect| {
        usize::from(rect.x) + usize::from(rect.width) > framebuffer.width()
            || usize::from(rect.y) + usize::from(rect.height) > framebuffer.height()
    }) {
        return Err(invalid("damage rectangle is outside framebuffer"));
    }
    let mut visited = vec![false; current.tile_revisions.len()];
    let mut changed = false;
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
                    changed |= current.sync_tile(index, framebuffer);
                }
            }
        }
    }
    current.frames += u64::from(changed);
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

/// Server state every client session uses.
struct SessionShared {
    framebuffer: Arc<Mutex<ServerFramebuffer>>,
    geometry: Arc<Geometry>,
    relative_pointer: Arc<AtomicBool>,
    clipboard: Arc<Mutex<ServerClipboard>>,
    /// See [`ServerConfig::thread_setup`].
    thread_setup: Option<fn()>,
    foveation: Foveation,
}

fn serve_client(
    stream: &mut TcpStream,
    shared: &SessionShared,
    events: &SyncSender<ClientEvent>,
    config: &ServerConfig,
    sessions: &Arc<Mutex<ServerSessions>>,
    client_id: u64,
) -> io::Result<()> {
    // Bound the whole handshake, not each read, so a client that trickles
    // bytes cannot hold one of the few connection slots indefinitely.
    let mut handshake = HandshakeStream {
        stream: &mut *stream,
        deadline: Instant::now() + SERVER_HANDSHAKE_TIMEOUT,
    };
    handshake.write_all(b"RFB 003.008\n")?;
    let mut version = [0; 12];
    handshake.read_exact(&mut version)?;
    if &version != b"RFB 003.008\n" {
        return Err(invalid("unsupported RFB client version"));
    }
    let security = if config.password.is_some() { 2 } else { 1 };
    handshake.write_all(&[1, security])?;
    let mut selected = [0];
    handshake.read_exact(&mut selected)?;
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
        handshake.write_all(&challenge)?;
        let mut answer = [0; 16];
        handshake.read_exact(&mut answer)?;
        if !constant_time_equal(&answer, &vnc_response(&challenge, password)) {
            let reason = b"VNC authentication failed";
            handshake.write_all(&1u32.to_be_bytes())?;
            handshake.write_all(&(reason.len() as u32).to_be_bytes())?;
            handshake.write_all(reason)?;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "VNC authentication failed",
            ));
        }
    }
    handshake.write_all(&0u32.to_be_bytes())?;
    let mut share_flag = [0];
    handshake.read_exact(&mut share_flag)?;
    if share_flag[0] > 1 {
        return Err(invalid("invalid ClientInit shared flag"));
    }
    sessions
        .lock()
        .map_err(|_| invalid("session registry lock is poisoned"))?
        .admit(client_id, share_flag[0] != 0)?;
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    let name = config.name.as_bytes();
    if name.len() > MAX_NAME_BYTES {
        return Err(invalid("server name is too long"));
    }
    let (generation, tile_count) = {
        let fb = shared
            .framebuffer
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
    // The pointer mode last announced to the client, which its reader uses.
    let announced_relative = Arc::new(AtomicBool::new(false));
    let reader = {
        let mut stream = stream.try_clone()?;
        let mut state = ClientReader {
            client_id,
            initial_generation: generation as u32,
            geometry: Arc::clone(&shared.geometry),
            announced_relative: Arc::clone(&announced_relative),
            encodings: ClientEncodings::default(),
            position: None,
        };
        let events = events.clone();
        let thread_setup = shared.thread_setup;
        std::thread::Builder::new()
            .name("topvnc-rfb-reader".into())
            .spawn(move || {
                if let Some(setup) = thread_setup {
                    setup();
                }
                let error = loop {
                    if let Err(error) =
                        read_client_message(&mut stream, &session_sender, &events, &mut state)
                    {
                        break error;
                    }
                };
                let _ = session_sender.send(SessionInput::Closed(error));
            })?
    };
    let result = write_client_updates(
        stream,
        shared,
        &announced_relative,
        &session_receiver,
        &wake_queued,
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

/// Input state of one client on its reader thread, which sees the client's
/// messages in the order they were sent.
struct ClientReader {
    client_id: u64,
    /// The low 32 bits of the framebuffer generation at the handshake.
    initial_generation: u32,
    geometry: Arc<Geometry>,
    /// The pointer mode the writer last announced to this client.
    announced_relative: Arc<AtomicBool>,
    encodings: ClientEncodings,
    /// The last absolute position, where a stale relative event's buttons
    /// are applied.
    position: Option<(u16, u16)>,
}

impl ClientReader {
    /// Interpret a PointerEvent: `mask` is its *button-mask*, `extended`
    /// the extended byte of an extended PointerEvent (else 0). Returns the
    /// event to deliver, if any.
    ///
    /// Events sent before the client learned of a mode change are still in
    /// flight when the mode changes. Their coordinates show which mode they
    /// use, and their motion is dropped while their buttons apply.
    fn pointer(
        &mut self,
        mask: u8,
        extended: u8,
        x: u16,
        y: u16,
    ) -> io::Result<Option<ClientEvent>> {
        let client_id = self.client_id;
        let buttons = if self.encodings.extended_mouse_buttons {
            // The high bit only marks an extended event.
            u16::from(mask & !EXTENDED_POINTER_MARKER)
                | (u16::from(extended & 1) * BUTTON_BACK)
                | (u16::from(extended >> 1 & 1) * BUTTON_FORWARD)
        } else {
            u16::from(mask)
        };
        if self.encodings.pointer_motion_change && self.announced_relative.load(Ordering::Acquire) {
            let (mut dx, mut dy) = (
                i32::from(x) - RELATIVE_POINTER_ORIGIN,
                i32::from(y) - RELATIVE_POINTER_ORIGIN,
            );
            if dx.abs() > MAX_RELATIVE_DELTA || dy.abs() > MAX_RELATIVE_DELTA {
                // An absolute position sent before the client switched.
                (dx, dy) = (0, 0);
            }
            return Ok(Some(ClientEvent::RelativePointer {
                client_id,
                buttons,
                dx,
                dy,
            }));
        }
        let (generation, width, height) = self.geometry.load();
        if x < width && y < height {
            self.position = Some((x, y));
            return Ok(Some(ClientEvent::Pointer {
                client_id,
                buttons,
                x,
                y,
            }));
        }
        if generation != self.initial_generation {
            // After a resize, pointer events sent for the old size may still
            // be in flight; drop them instead of ending the session.
            return Ok(None);
        }
        if self.encodings.pointer_motion_change {
            // A relative event sent before the client switched to absolute
            // positions: keep its buttons, without the motion.
            return Ok(self.position.map(|(x, y)| ClientEvent::Pointer {
                client_id,
                buttons,
                x,
                y,
            }));
        }
        Err(invalid("pointer outside framebuffer"))
    }
}

fn read_client_message(
    stream: &mut TcpStream,
    session: &SyncSender<SessionInput>,
    events: &SyncSender<ClientEvent>,
    state: &mut ClientReader,
) -> io::Result<()> {
    let session_closed = || io::Error::new(io::ErrorKind::BrokenPipe, "session writer closed");
    let input_closed = || io::Error::new(io::ErrorKind::BrokenPipe, "input receiver closed");
    let client_id = state.client_id;
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
            let encodings = ClientEncodings::parse(
                encodings
                    .chunks_exact(4)
                    .map(|encoding| i32::from_be_bytes(encoding.try_into().unwrap())),
            );
            state.encodings = encodings;
            session
                .send(SessionInput::Encodings(encodings))
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
            let mut extended = [0];
            if state.encodings.extended_mouse_buttons && data[0] & EXTENDED_POINTER_MARKER != 0 {
                stream.read_exact(&mut extended)?;
            }
            let x = u16::from_be_bytes([data[1], data[2]]);
            let y = u16::from_be_bytes([data[3], data[4]]);
            // Checked against the framebuffer geometry itself, not the
            // writer's, which may not have noticed a resize yet.
            if let Some(event) = state.pointer(data[0], extended[0], x, y)? {
                events.send(event).map_err(|_| input_closed())?;
            }
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
        CONTINUOUS_UPDATES_MESSAGE => {
            let mut data = [0; 9];
            stream.read_exact(&mut data)?;
            if data[0] > 1 {
                return Err(invalid("invalid EnableContinuousUpdates flag"));
            }
            let region = parse_update_request([
                1, data[1], data[2], data[3], data[4], data[5], data[6], data[7], data[8],
            ])?;
            session
                .send(SessionInput::ContinuousUpdates {
                    enable: data[0] != 0,
                    region,
                })
                .map_err(|_| session_closed())?;
        }
        FENCE_MESSAGE => {
            let (flags, payload) = read_fence(stream)?;
            session
                .send(SessionInput::Fence {
                    flags,
                    payload,
                    received: Instant::now(),
                })
                .map_err(|_| session_closed())?;
        }
        _ => return Err(invalid("unknown client message")),
    }
    Ok(())
}

/// What a client advertised with SetEncodings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ClientEncodings {
    desktop_size: bool,
    fence: bool,
    continuous_updates: bool,
    /// QEMU Pointer Motion Change: the client sends relative motion on request.
    pointer_motion_change: bool,
    /// The client sends extended PointerEvents once acknowledged.
    extended_mouse_buttons: bool,
    /// Set when the client prefers Tight over Raw.
    tight: Option<TightSettings>,
}

impl ClientEncodings {
    fn parse(encodings: impl Iterator<Item = i32>) -> Self {
        let mut result = Self::default();
        let mut preferred = None;
        let mut quality = None;
        let mut compression = None;
        for encoding in encodings {
            match encoding {
                0 | tight::TIGHT_ENCODING => {
                    preferred.get_or_insert(encoding);
                }
                DESKTOP_SIZE_ENCODING => result.desktop_size = true,
                FENCE_ENCODING => result.fence = true,
                CONTINUOUS_UPDATES_ENCODING => result.continuous_updates = true,
                POINTER_MOTION_CHANGE_ENCODING => result.pointer_motion_change = true,
                EXTENDED_MOUSE_BUTTONS_ENCODING => result.extended_mouse_buttons = true,
                level @ tight::QUALITY_LEVEL_0..=-23 => {
                    quality.get_or_insert((level - tight::QUALITY_LEVEL_0) as u8);
                }
                level @ tight::COMPRESS_LEVEL_0..=-247 => {
                    compression.get_or_insert((level - tight::COMPRESS_LEVEL_0) as u8);
                }
                _ => {}
            }
        }
        if preferred == Some(tight::TIGHT_ENCODING) {
            let defaults = TightSettings::default();
            result.tight = Some(TightSettings {
                quality,
                compression: compression.unwrap_or(defaults.compression),
            });
        }
        result
    }
}

struct PendingRequest {
    request: UpdateRequest,
    /// When an unchanged incremental request is answered with an empty update.
    deadline: Instant,
}

/// Read a Fence message body after its type byte.
fn read_fence(stream: &mut impl Read) -> io::Result<(u32, Vec<u8>)> {
    let mut header = [0; 8];
    stream.read_exact(&mut header)?;
    let flags = u32::from_be_bytes(header[3..7].try_into().unwrap());
    let length = usize::from(header[7]);
    if length > MAX_FENCE_PAYLOAD {
        return Err(invalid("fence payload is too long"));
    }
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload)?;
    Ok((flags, payload))
}

fn fence_message(flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut message = vec![FENCE_MESSAGE, 0, 0, 0];
    message.extend_from_slice(&flags.to_be_bytes());
    message.push(payload.len().min(MAX_FENCE_PAYLOAD) as u8);
    message.extend_from_slice(&payload[..payload.len().min(MAX_FENCE_PAYLOAD)]);
    message
}

/// When continuous updates may send the next update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendWindow {
    Open,
    /// The link has probably finished sending the update in flight.
    OpensAt(Instant),
    /// Wait for a fence acknowledgement.
    Closed,
}

/// Flow control for continuous updates. Every pushed update is followed by
/// a fence; its acknowledgement shows the update was received and decoded.
/// While one update is in flight, the next is sent once the link has
/// probably finished transmitting everything before it, judged from the
/// measured throughput. Waiting for the acknowledgement as well would leave
/// the link idle for a round trip every frame; sending earlier would queue
/// stale frames.
#[derive(Debug, Default)]
struct FlowControl {
    in_flight: std::collections::VecDeque<InFlight>,
    next_fence: u32,
    /// Recent throughput samples in bytes per second.
    samples: std::collections::VecDeque<f64>,
    /// Recent updates' bytes and delivery times; see
    /// [`FlowControl::delivery_rate`].
    deliveries: std::collections::VecDeque<(usize, Duration)>,
    /// From the times acknowledgements arrived, which the session may
    /// handle later: the shortest probe round trip, and when the previous
    /// update's acknowledgement arrived.
    arrival_base_delay: Option<Duration>,
    last_arrival: Option<Instant>,
    /// When the previous acknowledgement arrived.
    last_acknowledgement: Option<Instant>,
    /// When the link is expected to finish transmitting what was sent.
    link_free: Option<Instant>,
    /// Shortest round trip of a bare fence: the link's fixed delay.
    base_delay: Option<Duration>,
    /// When the unanswered probe fence was sent.
    probe_sent: Option<Instant>,
    last_probe: Option<Instant>,
}

/// An update waiting for its fence acknowledgement.
#[derive(Debug, Clone, Copy)]
struct InFlight {
    sequence: u32,
    bytes: usize,
    /// When its first and last bytes were written.
    first_write: Instant,
    sent: Instant,
}

/// Throughput samples kept; the estimate is their maximum.
const FLOW_SAMPLES: usize = 8;
/// Updates are paced as if the link were this fraction of its estimated
/// throughput, so an estimate that is a little high, as after a burst,
/// drains instead of building a queue.
const FLOW_PACING: f64 = 0.95;
/// Payload of probe fences; update fences carry four-byte sequence numbers.
const FLOW_PROBE: [u8; 1] = [0xff];
/// How often the round trip is re-measured while nothing is in flight.
const FLOW_PROBE_INTERVAL: Duration = Duration::from_secs(1);

impl FlowControl {
    /// Whether to send a probe fence now: only on an idle link, so its
    /// round trip carries no transmission or queueing time.
    fn probe_due(&self, now: Instant) -> bool {
        self.in_flight.is_empty()
            && self.probe_sent.is_none()
            && self
                .last_probe
                .is_none_or(|last| now.saturating_duration_since(last) >= FLOW_PROBE_INTERVAL)
    }

    fn probing(&mut self, now: Instant) -> &'static [u8] {
        self.probe_sent = Some(now);
        self.last_probe = Some(now);
        &FLOW_PROBE
    }

    /// Estimated link throughput in bytes per second.
    fn throughput(&self) -> Option<f64> {
        self.samples.iter().copied().reduce(f64::max)
    }

    /// What the link sustains, in bytes per second, for deciding how much
    /// to send: recent updates' bytes over their total delivery time.
    ///
    /// An update's delivery time is the smaller of two upper bounds on its
    /// transmission time, both from the times acknowledgements arrived:
    /// - from when its first byte was written to its acknowledgement, minus
    ///   the link's fixed round trip, which includes waiting for the rest
    ///   of the update to be encoded, decoding, and any queueing behind
    ///   earlier updates;
    /// - the gap since the previous acknowledgement, which includes any
    ///   idle time.
    ///
    /// Summing keeps small updates, which a bursty link can deliver faster
    /// than it sustains, from counting as much as large ones. The pacing
    /// estimate, [`FlowControl::throughput`], overstates the rate when the
    /// session handles acknowledgements late and back to back, and does not
    /// count bytes streamed while the rest of an update was being encoded.
    /// Needs half the samples.
    fn delivery_rate(&self) -> Option<f64> {
        if self.deliveries.len() < FLOW_SAMPLES / 2 {
            return None;
        }
        let (bytes, time) = self
            .deliveries
            .iter()
            .fold((0, Duration::ZERO), |(bytes, time), sample| {
                (bytes + sample.0, time + sample.1)
            });
        Some(bytes as f64 / time.as_secs_f64())
    }

    fn window(&self) -> SendWindow {
        match self.in_flight.len() {
            0 => SendWindow::Open,
            count if count >= MAX_UPDATES_IN_FLIGHT => SendWindow::Closed,
            _ => self
                .link_free
                .map_or(SendWindow::Closed, SendWindow::OpensAt),
        }
    }

    /// Record an update of `bytes` whose first and last bytes were written
    /// at `first_write` and `sent`; returns the fence payload to send after
    /// it.
    fn sent(&mut self, bytes: usize, first_write: Instant, sent: Instant) -> [u8; 4] {
        let sequence = self.next_fence;
        self.next_fence = self.next_fence.wrapping_add(1);
        self.in_flight.push_back(InFlight {
            sequence,
            bytes,
            first_write,
            sent,
        });
        // A client that never answers fences cannot grow this without bound.
        if self.in_flight.len() > 16 {
            self.in_flight.pop_front();
        }
        // Updates leave the link in order: this one starts once the link
        // has finished the ones before it.
        self.link_free = self.throughput().map(|rate| {
            self.link_free.map_or(sent, |free| free.max(sent))
                + Duration::from_secs_f64(bytes as f64 / (rate * FLOW_PACING))
        });
        sequence.to_be_bytes()
    }

    /// Record a fence acknowledgement carrying `payload`, which arrived at
    /// `arrived` and is handled at `now`.
    fn acknowledged(&mut self, payload: &[u8], now: Instant, arrived: Instant) {
        if payload == FLOW_PROBE {
            if let Some(sent) = self.probe_sent.take() {
                let round_trip = now.saturating_duration_since(sent);
                self.base_delay = Some(
                    self.base_delay
                        .map_or(round_trip, |base| base.min(round_trip)),
                );
                let round_trip = arrived.saturating_duration_since(sent);
                self.arrival_base_delay = Some(
                    self.arrival_base_delay
                        .map_or(round_trip, |base| base.min(round_trip)),
                );
            }
            return;
        }
        let Ok(sequence) = <[u8; 4]>::try_from(payload).map(u32::from_be_bytes) else {
            return;
        };
        let Some(position) = self
            .in_flight
            .iter()
            .position(|entry| entry.sequence == sequence)
        else {
            return;
        };
        let InFlight {
            bytes,
            first_write,
            sent,
            ..
        } = self.in_flight[position];
        self.in_flight.drain(..=position);
        let previous = self.last_acknowledgement.replace(now);
        let previous_arrival = self.last_arrival.replace(arrived);
        if position > 0 {
            return;
        }
        // Two upper bounds on this update's transmission time, so bytes over
        // either never overestimates the throughput: the delay beyond the
        // link's fixed round trip, and, because updates are delivered in
        // order, the gap since the previous acknowledgement. The smaller one
        // excludes queueing (the first can include it) and idle time (the
        // second can). The maximum of recent samples is the estimate.
        let beyond_delay = self
            .base_delay
            .map(|base| now.saturating_duration_since(sent).saturating_sub(base));
        let since_previous = previous.map(|previous| now.saturating_duration_since(previous));
        let transfer = match (beyond_delay, since_previous) {
            (Some(beyond), Some(gap)) => beyond.min(gap),
            (Some(bound), None) | (None, Some(bound)) => bound,
            (None, None) => return,
        }
        .max(Duration::from_micros(500));
        if self.samples.len() == FLOW_SAMPLES {
            self.samples.pop_front();
        }
        self.samples
            .push_back(bytes as f64 / transfer.as_secs_f64());
        if let Some(base) = self.arrival_base_delay {
            let since_written = arrived
                .saturating_duration_since(first_write)
                .saturating_sub(base);
            let delivery = previous_arrival
                .map_or(since_written, |previous| {
                    since_written.min(arrived.saturating_duration_since(previous))
                })
                .max(Duration::from_micros(500));
            if self.deliveries.len() == FLOW_SAMPLES {
                self.deliveries.pop_front();
            }
            self.deliveries.push_back((bytes, delivery));
        }
    }
}

fn write_client_updates(
    stream: &mut TcpStream,
    session: &SessionShared,
    announced_relative: &AtomicBool,
    receiver: &std::sync::mpsc::Receiver<SessionInput>,
    wake_queued: &AtomicBool,
    mut generation: u64,
    tile_count: usize,
) -> io::Result<()> {
    let shared = &session.framebuffer;
    let mut seen_revisions = vec![u64::MAX; tile_count];
    let mut pixel_format = ServerPixelFormat::DEFAULT;
    let mut encodings = ClientEncodings::default();
    let mut output = Vec::new();
    let mut clipboard_revision = 0;
    let mut pending: Option<PendingRequest> = None;
    // Set once the framebuffer has been resized; requests sized for the old
    // framebuffer are then clipped instead of ending the session.
    let mut resized = false;
    // The region continuous updates cover, while they are enabled.
    let mut continuous: Option<UpdateRequest> = None;
    let mut announced_continuous = false;
    let mut flow = FlowControl::default();
    // The pointer mode announced since the client last set its encodings:
    // `Some(true)` for relative motion.
    let mut pointer_mode: Option<bool> = None;
    let mut encoder = SessionEncoder {
        pool: None,
        thread_setup: session.thread_setup,
        fovea: fovea::State::new(),
    };
    // Pseudo-encoding rectangles waiting to be sent.
    let mut pseudo: Vec<PseudoRect> = Vec::new();
    loop {
        send_pending_clipboard(stream, &session.clipboard, &mut clipboard_revision)?;
        let (current_generation, width, height, frames) = {
            let fb = shared
                .lock()
                .map_err(|_| invalid("framebuffer lock is poisoned"))?;
            (
                fb.generation,
                fb.framebuffer.width,
                fb.framebuffer.height,
                fb.frames,
            )
        };
        encoder.fovea.observe_frames(frames, Instant::now());
        if encodings.pointer_motion_change {
            let relative = session.relative_pointer.load(Ordering::Acquire);
            if pointer_mode != Some(relative) {
                pseudo.retain(|rect| rect.encoding != POINTER_MOTION_CHANGE_ENCODING);
                // QEMU's layout: x is 1 for absolute and 0 for relative, and
                // the rectangle spans the framebuffer.
                pseudo.push(PseudoRect {
                    encoding: POINTER_MOTION_CHANGE_ENCODING,
                    x: u16::from(!relative),
                    y: 0,
                    width,
                    height,
                });
                pointer_mode = Some(relative);
            }
        }
        // Clients receiving continuous updates take them at once; others
        // with their next requested update, so no update arrives that they
        // did not ask for.
        if continuous.is_some() && !pseudo.is_empty() {
            write_pseudo_update(stream, &pseudo, announced_relative)?;
            pseudo.clear();
        }
        let tight = encodings.tight.filter(|_| pixel_format.has_tight_pixels());
        let foveate = match session.foveation {
            Foveation::Off => false,
            Foveation::Auto => session.relative_pointer.load(Ordering::Acquire),
            Foveation::On => true,
        };
        // Quality levels by zone, when foveating a client that takes JPEG.
        let zones = tight
            .and_then(|settings| settings.quality)
            .filter(|_| foveate)
            .map(|level| encoder.fovea.levels(level));
        let mut window = flow.window();
        if current_generation != generation {
            if !encodings.desktop_size {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "desktop size changed and the client does not support DesktopSize",
                ));
            }
            resized = true;
            // Answer the next request with the new size, or send it right
            // away to a client receiving continuous updates; the client then
            // asks for pixels of the new framebuffer.
            if pending.take().is_some() || continuous.is_some() {
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
                resized,
                generation,
            )?;
            if let Some(update) = update
                && (!update.rectangles.is_empty()
                    || !pseudo.is_empty()
                    || Instant::now() >= waiting.deadline)
            {
                let (bytes, first_write) = write_prepared_update(
                    stream,
                    shared,
                    &mut seen_revisions,
                    &mut output,
                    pixel_format,
                    tight,
                    zones,
                    update,
                    generation,
                    &mut encoder,
                    &pseudo,
                )?;
                note_pseudo_sent(&pseudo, announced_relative);
                pseudo.clear();
                // Track requested updates too, so flow control knows what is
                // still on the wire when continuous updates start.
                if encodings.fence {
                    let payload = flow.sent(bytes, first_write, Instant::now());
                    stream
                        .write_all(&fence_message(FENCE_REQUEST | FENCE_BLOCK_BEFORE, &payload))?;
                }
                pending = None;
            }
        } else if let Some(region) = continuous {
            let now = Instant::now();
            if flow.probe_due(now) {
                let probe = flow.probing(now);
                stream.write_all(&fence_message(FENCE_REQUEST | FENCE_BLOCK_BEFORE, probe))?;
            }
            let open = match window {
                SendWindow::Open => true,
                SendWindow::OpensAt(at) => Instant::now() >= at,
                SendWindow::Closed => false,
            };
            let mut prepared = if open {
                prepare_update(shared, &seen_revisions, region, true, generation)?
            } else {
                None
            };
            // Before the emptiness check: leaving out the periphery can
            // leave nothing to send yet.
            if let Some(update) = &mut prepared {
                if zones.is_some() {
                    skip_periphery(shared, update, generation, &mut encoder.fovea)?;
                } else {
                    encoder.fovea.periphery_released();
                }
            }
            if let Some(update) = prepared
                && !update.rectangles.is_empty()
            {
                let (bytes, first_write) = write_prepared_update(
                    stream,
                    shared,
                    &mut seen_revisions,
                    &mut output,
                    pixel_format,
                    tight,
                    zones,
                    update,
                    generation,
                    &mut encoder,
                    &[],
                )?;
                let sent = Instant::now();
                let payload = flow.sent(bytes, first_write, sent);
                stream.write_all(&fence_message(FENCE_REQUEST | FENCE_BLOCK_BEFORE, &payload))?;
                window = flow.window();
                if zones.is_some() {
                    encoder.fovea.sent(bytes, flow.delivery_rate(), sent);
                }
            }
        }
        let now = Instant::now();
        let mut timeout = SERVER_IDLE_WAKE_INTERVAL;
        if let Some(waiting) = &pending
            && current_generation == generation
        {
            timeout = waiting.deadline.saturating_duration_since(now);
        }
        if continuous.is_some() {
            match window {
                SendWindow::OpensAt(at) => timeout = timeout.min(at.saturating_duration_since(now)),
                // Held-back periphery tiles go out on time even when nothing
                // else changes.
                SendWindow::Open => {
                    if let Some(due) = encoder.fovea.periphery_due() {
                        timeout = timeout.min(due.saturating_duration_since(now));
                    }
                }
                SendWindow::Closed => {}
            }
        }
        match receiver.recv_timeout(timeout.max(Duration::from_millis(1))) {
            Ok(SessionInput::PixelFormat(format)) => pixel_format = format,
            Ok(SessionInput::Encodings(advertised)) => {
                if advertised.extended_mouse_buttons && !encodings.extended_mouse_buttons {
                    // An empty rectangle acknowledges extended PointerEvents.
                    pseudo.push(PseudoRect {
                        encoding: EXTENDED_MOUSE_BUTTONS_ENCODING,
                        x: 0,
                        y: 0,
                        width: 0,
                        height: 0,
                    });
                }
                // Announce the pointer mode again: a client that set its
                // encodings may have reset its own.
                pointer_mode = None;
                if !advertised.pointer_motion_change {
                    announced_relative.store(false, Ordering::Release);
                }
                encodings = advertised;
                // Continuous updates rely on fences for flow control.
                if encodings.continuous_updates && encodings.fence && !announced_continuous {
                    stream.write_all(&[CONTINUOUS_UPDATES_MESSAGE])?;
                    announced_continuous = true;
                }
            }
            Ok(SessionInput::UpdateRequest(request)) => {
                // Continuous updates already cover incremental requests.
                if continuous.is_none() || !request.incremental {
                    pending = Some(PendingRequest {
                        request,
                        deadline: if request.incremental {
                            Instant::now() + SERVER_EMPTY_UPDATE_INTERVAL
                        } else {
                            Instant::now()
                        },
                    });
                }
            }
            Ok(SessionInput::ContinuousUpdates { enable, region }) => {
                if !announced_continuous {
                    return Err(invalid("continuous updates were not offered"));
                }
                if enable {
                    continuous = Some(region);
                } else if continuous.take().is_some() {
                    flow = FlowControl::default();
                    stream.write_all(&[CONTINUOUS_UPDATES_MESSAGE])?;
                }
            }
            Ok(SessionInput::Fence {
                flags,
                payload,
                received,
            }) => {
                if flags & FENCE_REQUEST != 0 {
                    // Messages are handled in order, which satisfies every
                    // supported flag.
                    stream.write_all(&fence_message(flags & FENCE_SUPPORTED_FLAGS, &payload))?;
                } else {
                    flow.acknowledged(&payload, Instant::now(), received);
                }
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

/// Write a prepared update with Tight when the client accepts it, otherwise
/// Raw. Tight updates are foveated when `zones` gives quality levels for
/// the fovea, mid zone, and periphery. Returns the bytes written and when
/// the first of them were.
#[allow(clippy::too_many_arguments)]
fn write_prepared_update(
    stream: &mut TcpStream,
    shared: &Arc<Mutex<ServerFramebuffer>>,
    seen_revisions: &mut [u64],
    output: &mut Vec<u8>,
    pixel_format: ServerPixelFormat,
    tight: Option<TightSettings>,
    zones: Option<[u8; 3]>,
    update: PreparedUpdate,
    generation: u64,
    encoder: &mut SessionEncoder,
    pseudo: &[PseudoRect],
) -> io::Result<(usize, Instant)> {
    match tight {
        Some(settings) => write_tight_update(
            stream,
            shared,
            seen_revisions,
            output,
            settings,
            zones,
            update,
            generation,
            encoder,
            pseudo,
        ),
        None => {
            let started = Instant::now();
            write_update(
                stream,
                shared,
                seen_revisions,
                output,
                pixel_format,
                update,
                generation,
                pseudo,
            )
            .map(|bytes| (bytes, started))
        }
    }
}

fn write_desktop_size(stream: &mut TcpStream, width: u16, height: u16) -> io::Result<()> {
    write_pseudo_update(
        stream,
        &[PseudoRect {
            encoding: DESKTOP_SIZE_ENCODING,
            x: 0,
            y: 0,
            width,
            height,
        }],
        &AtomicBool::new(false),
    )
}

/// A rectangle that carries a pseudo-encoding instead of pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PseudoRect {
    encoding: i32,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

impl PseudoRect {
    fn header(&self, output: &mut Vec<u8>) {
        for value in [self.x, self.y, self.width, self.height] {
            output.extend_from_slice(&value.to_be_bytes());
        }
        output.extend_from_slice(&self.encoding.to_be_bytes());
    }
}

/// The FramebufferUpdate header for `count` rectangles after `pseudo`.
fn update_header(output: &mut Vec<u8>, pseudo: &[PseudoRect], count: usize) -> io::Result<()> {
    let total = u16::try_from(pseudo.len() + count)
        .map_err(|_| invalid("too many changed framebuffer rectangles"))?;
    output.extend_from_slice(&[0, 0]);
    output.extend_from_slice(&total.to_be_bytes());
    for rect in pseudo {
        rect.header(output);
    }
    Ok(())
}

/// After `pseudo` went out, record the pointer mode it announced: the
/// client's reader interprets pointer events in that mode from now on.
fn note_pseudo_sent(pseudo: &[PseudoRect], announced_relative: &AtomicBool) {
    if let Some(rect) = pseudo
        .iter()
        .rev()
        .find(|rect| rect.encoding == POINTER_MOTION_CHANGE_ENCODING)
    {
        // Events already in flight still use the old mode; the reader
        // recognizes them by their coordinates.
        announced_relative.store(rect.x == 0, Ordering::Release);
    }
}

/// Write a FramebufferUpdate holding only pseudo-encoding rectangles.
fn write_pseudo_update(
    stream: &mut impl Write,
    pseudo: &[PseudoRect],
    announced_relative: &AtomicBool,
) -> io::Result<()> {
    let mut message = Vec::with_capacity(4 + 12 * pseudo.len());
    update_header(&mut message, pseudo, 0)?;
    stream.write_all(&message)?;
    note_pseudo_sent(pseudo, announced_relative);
    Ok(())
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

    /// Tight's compact 3-byte pixels need three 8-bit channels in a 32-bit
    /// pixel; other formats are served as Raw.
    fn has_tight_pixels(self) -> bool {
        self.bits_per_pixel == 32
            && self.red_max == 255
            && self.green_max == 255
            && self.blue_max == 255
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
#[allow(clippy::too_many_arguments)]
fn write_update(
    stream: &mut TcpStream,
    shared: &Arc<Mutex<ServerFramebuffer>>,
    seen_revisions: &mut [u64],
    output: &mut Vec<u8>,
    pixel_format: ServerPixelFormat,
    update: PreparedUpdate,
    generation: u64,
    pseudo: &[PseudoRect],
) -> io::Result<usize> {
    let mut written = 0;
    output.clear();
    update_header(output, pseudo, update.rectangles.len())?;
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
                written += output.len();
                output.clear();
            }
        }
        if let Some(index) = rect.tile_index {
            seen_revisions[index] = rect.revision;
        }
    }
    stream.write_all(output)?;
    written += output.len();
    output.clear();
    for (index, revision) in update.acknowledged {
        seen_revisions[index] = revision;
    }
    Ok(written)
}

/// Rows per Tight band when `workers` threads encode an update spanning
/// `rows` rows: about one band per thread, in whole tiles, between one tile
/// and Tight's height limit. Fewer, larger bands compress a little better;
/// more bands encode, travel, and decode in parallel.
fn band_rows(rows: usize, workers: usize) -> usize {
    (rows / workers.max(1) / SERVER_TILE_SIZE * SERVER_TILE_SIZE)
        .clamp(SERVER_TILE_SIZE, tight::MAX_RECT_HEIGHT)
}

/// Merge changed tiles into larger rectangles and split them into bands at
/// most `band_rows` tall and Tight's width limit wide. Merging only joins
/// rectangles that share a full edge, so the result covers exactly the same
/// pixels.
fn tight_rects(rects: &[ServerRect], band_rows: usize) -> Vec<(usize, usize, usize, usize)> {
    let band_rows = band_rows.clamp(1, tight::MAX_RECT_HEIGHT);
    // Rectangles arrive in row-major tile order, so horizontal neighbors are
    // consecutive.
    let mut runs: Vec<(usize, usize, usize, usize)> = Vec::new();
    for rect in rects {
        let (x, y, width, height) = (
            usize::from(rect.x),
            usize::from(rect.y),
            usize::from(rect.width),
            usize::from(rect.height),
        );
        if let Some(last) = runs.last_mut()
            && last.1 == y
            && last.3 == height
            && last.0 + last.2 == x
            && last.2 + width <= tight::MAX_RECT_WIDTH
        {
            last.2 += width;
        } else {
            runs.push((x, y, width, height));
        }
    }
    // Join runs with the run directly above that has the same columns.
    let mut merged: Vec<(usize, usize, usize, usize)> = Vec::with_capacity(runs.len());
    let mut open = std::collections::HashMap::new();
    for (x, y, width, height) in runs {
        if let Some(index) = open.remove(&(x, width, y)) {
            let above: &mut (usize, usize, usize, usize) = &mut merged[index];
            if above.3 + height <= band_rows {
                above.3 += height;
                open.insert((x, width, y + height), index);
                continue;
            }
        }
        open.insert((x, width, y + height), merged.len());
        merged.push((x, y, width, height));
    }
    let mut result = Vec::with_capacity(merged.len());
    for (x, y, width, height) in merged {
        for band_y in (y..y + height).step_by(band_rows) {
            for band_x in (x..x + width).step_by(tight::MAX_RECT_WIDTH) {
                result.push((
                    band_x,
                    band_y,
                    tight::MAX_RECT_WIDTH.min(x + width - band_x),
                    band_rows.min(y + height - band_y),
                ));
            }
        }
    }
    result
}

/// Leave the periphery out of a continuous update when the session's rung
/// sends it only with every Nth update (spec 008). Left-out tiles are not
/// acknowledged, so they stay pending and go out with a later update.
fn skip_periphery(
    shared: &Arc<Mutex<ServerFramebuffer>>,
    update: &mut PreparedUpdate,
    generation: u64,
    fovea: &mut fovea::State,
) -> io::Result<()> {
    let fb = shared
        .lock()
        .map_err(|_| invalid("framebuffer lock is poisoned"))?;
    // Tile indexes refer to the framebuffer the update was prepared for.
    if fb.generation != generation {
        return Ok(());
    }
    let frame = (fb.framebuffer.width(), fb.framebuffer.height());
    let periphery =
        |index: usize| fovea::zone(fb.tile_rect(index), frame) == fovea::Zone::Periphery;
    let in_periphery = |rect: &ServerRect| rect.tile_index.is_some_and(periphery);
    let has_periphery = update.rectangles.iter().any(in_periphery);
    let has_center = update.rectangles.iter().any(|rect| !in_periphery(rect));
    if !fovea.send_periphery(has_center, has_periphery, Instant::now()) {
        update.rectangles.retain(|rect| !in_periphery(rect));
        update.acknowledged.retain(|&(index, _)| !periphery(index));
    }
    Ok(())
}

/// A rectangle's pixels, copied out of the framebuffer, with its size.
type Snapshot = (Vec<u32>, usize, usize);

/// One update's rectangles for the encoder threads, which claim them in
/// order through `next`.
struct EncodeBatch {
    snapshots: Arc<Vec<Snapshot>>,
    settings: Arc<Vec<TightSettings>>,
    next: Arc<std::sync::atomic::AtomicUsize>,
    results: std::sync::mpsc::Sender<(usize, io::Result<Vec<u8>>)>,
}

/// Encoder threads for one client session. They start with its first large
/// update and stay, so later updates start no threads.
struct EncodePool {
    batches: Vec<std::sync::mpsc::Sender<EncodeBatch>>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl EncodePool {
    fn new(threads: usize, thread_setup: Option<fn()>) -> io::Result<Self> {
        let mut pool = Self {
            batches: Vec::with_capacity(threads),
            workers: Vec::with_capacity(threads),
        };
        for _ in 0..threads {
            let (sender, receiver) = std::sync::mpsc::channel::<EncodeBatch>();
            pool.workers.push(
                std::thread::Builder::new()
                    .name("topvnc-encoder".into())
                    .spawn(move || {
                        if let Some(setup) = thread_setup {
                            setup();
                        }
                        for batch in receiver {
                            loop {
                                let index = batch.next.fetch_add(1, Ordering::Relaxed);
                                let Some((pixels, width, height)) = batch.snapshots.get(index)
                                else {
                                    break;
                                };
                                let mut body = Vec::new();
                                let result = tight::encode_rect(
                                    pixels,
                                    *width,
                                    *height,
                                    batch.settings[index],
                                    &mut body,
                                )
                                .map(|_| body);
                                // The session stopped waiting after an error.
                                if batch.results.send((index, result)).is_err() {
                                    break;
                                }
                            }
                        }
                    })?,
            );
            pool.batches.push(sender);
        }
        Ok(pool)
    }

    /// Encode `snapshots` and pass each body to `ready` in order, as soon as
    /// it and every body before it are done, so the first bands can be sent
    /// while later ones are still being encoded.
    fn encode(
        &self,
        snapshots: Vec<Snapshot>,
        settings: Vec<TightSettings>,
        mut ready: impl FnMut(usize, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let stopped = || io::Error::other("a Tight encoder thread stopped");
        let count = snapshots.len();
        let snapshots = Arc::new(snapshots);
        let settings = Arc::new(settings);
        let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (results, receiver) = std::sync::mpsc::channel();
        for batch in &self.batches {
            batch
                .send(EncodeBatch {
                    snapshots: Arc::clone(&snapshots),
                    settings: Arc::clone(&settings),
                    next: Arc::clone(&next),
                    results: results.clone(),
                })
                .map_err(|_| stopped())?;
        }
        // Once every thread finishes the batch, a missing body ends the wait.
        drop(results);
        let mut done: Vec<Option<Vec<u8>>> = (0..count).map(|_| None).collect();
        let mut next_ready = 0;
        while next_ready < count {
            let (index, body) = receiver.recv().map_err(|_| stopped())?;
            done[index] = Some(body?);
            while let Some(body) = done.get_mut(next_ready).and_then(Option::take) {
                ready(next_ready, &body)?;
                next_ready += 1;
            }
        }
        Ok(())
    }
}

impl Drop for EncodePool {
    fn drop(&mut self) {
        // Closing the channels ends the threads.
        self.batches.clear();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// How many threads encode a large update.
fn encoder_threads() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// A session's encoder threads, started on first use, and its foveation
/// state.
struct SessionEncoder {
    pool: Option<EncodePool>,
    /// Runs first on every encoder thread; see [`ServerConfig::thread_setup`].
    thread_setup: Option<fn()>,
    fovea: fovea::State,
}

/// Write a prepared update with Tight encoding. The rectangles are copied
/// under one framebuffer lock, so every update shows a single captured frame,
/// then encoded without holding the lock. Large updates are encoded on
/// `pool`, which starts on first use, and each band is written as soon as it
/// is ready. With `zones`, the update is foveated: rectangles are laid out
/// by zone, center first, each at its zone's quality level. Returns the
/// bytes written and when the first of them were.
#[allow(clippy::too_many_arguments)]
fn write_tight_update(
    stream: &mut TcpStream,
    shared: &Arc<Mutex<ServerFramebuffer>>,
    seen_revisions: &mut [u64],
    output: &mut Vec<u8>,
    settings: TightSettings,
    zones: Option<[u8; 3]>,
    update: PreparedUpdate,
    generation: u64,
    encoder: &mut SessionEncoder,
    pseudo: &[PseudoRect],
) -> io::Result<(usize, Instant)> {
    let workers = encoder_threads();
    let (rects, rect_settings, snapshots) = {
        let fb = shared
            .lock()
            .map_err(|_| invalid("framebuffer lock is poisoned"))?;
        let frame = (fb.framebuffer.width(), fb.framebuffer.height());
        // After a resize the update no longer matches the framebuffer's
        // zones; it is sent black in bands.
        let (rects, rect_settings): (Vec<_>, Vec<_>) = match zones
            .filter(|_| fb.generation == generation)
        {
            Some(levels) => fovea::foveated_rects(&update.rectangles, frame)
                .into_iter()
                .map(|(rect, zone)| {
                    let quality = Some(levels[zone as usize]);
                    (
                        rect,
                        TightSettings {
                            quality,
                            ..settings
                        },
                    )
                })
                .unzip(),
            None => {
                let rows = update
                    .rectangles
                    .iter()
                    .map(|rect| usize::from(rect.y)..usize::from(rect.y) + usize::from(rect.height))
                    .reduce(|all, rows| all.start.min(rows.start)..all.end.max(rows.end))
                    .map_or(0, |rows| rows.len());
                let rects = tight_rects(&update.rectangles, band_rows(rows, workers));
                let count = rects.len();
                (rects, vec![settings; count])
            }
        };
        if rects.len() > usize::from(u16::MAX) {
            return Err(invalid("too many changed framebuffer rectangles"));
        }
        let stride = fb.framebuffer.width();
        let snapshots = rects
            .iter()
            .map(|&(x, y, width, height)| {
                // After a resize the remaining area is sent black; the client
                // receives the new size in its next update.
                let pixels = if fb.generation == generation {
                    let mut pixels = Vec::with_capacity(width * height);
                    for row in y..y + height {
                        let start = row * stride + x;
                        pixels.extend_from_slice(&fb.framebuffer.pixels[start..start + width]);
                    }
                    pixels
                } else {
                    vec![0; width * height]
                };
                (pixels, width, height)
            })
            .collect::<Vec<_>>();
        (rects, rect_settings, snapshots)
    };
    let mut written = 0;
    let mut first_write = None;
    output.clear();
    update_header(output, pseudo, rects.len())?;
    {
        let mut ready = |index: usize, body: &[u8]| -> io::Result<()> {
            let (x, y, width, height) = rects[index];
            for value in [x, y, width, height] {
                output.extend_from_slice(&(value as u16).to_be_bytes());
            }
            output.extend_from_slice(&tight::TIGHT_ENCODING.to_be_bytes());
            output.extend_from_slice(body);
            // Send bands as they become ready, so transmission overlaps the
            // encoding of the rest.
            if output.len() >= SERVER_STREAM_FLUSH_BYTES {
                first_write.get_or_insert_with(Instant::now);
                stream.write_all(output)?;
                written += output.len();
                output.clear();
            }
            Ok(())
        };
        let pixels: usize = snapshots.iter().map(|(pixels, ..)| pixels.len()).sum();
        if workers > 1 && snapshots.len() > 1 && pixels >= SERVER_PARALLEL_ENCODE_PIXELS {
            let pool = match &mut encoder.pool {
                Some(pool) => pool,
                None => encoder
                    .pool
                    .insert(EncodePool::new(workers, encoder.thread_setup)?),
            };
            pool.encode(snapshots, rect_settings, &mut ready)?;
        } else {
            for (index, (pixels, width, height)) in snapshots.iter().enumerate() {
                let mut body = Vec::new();
                tight::encode_rect(pixels, *width, *height, rect_settings[index], &mut body)?;
                ready(index, &body)?;
            }
        }
    }
    let first_write = *first_write.get_or_insert_with(Instant::now);
    stream.write_all(output)?;
    written += output.len();
    output.clear();
    for rect in &update.rectangles {
        if let Some(index) = rect.tile_index {
            seen_revisions[index] = rect.revision;
        }
    }
    for (index, revision) in update.acknowledged {
        seen_revisions[index] = revision;
    }
    Ok((written, first_write))
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
    stream.write_all(&set_encodings_message(&encoding.advertised()))?;
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
        &mut UpdateDecoder::new(),
        apply,
    )
}

/// Read one framebuffer update. Every rectangle is decoded to 32-bit
/// B, G, R, X bytes before `apply` receives it.
pub fn read_update_with_encoding(
    reader: &mut impl Read,
    frame_width: u16,
    frame_height: u16,
    scratch: &mut Vec<u8>,
    selected_encoding: Encoding,
    decoder: &mut UpdateDecoder,
    apply: impl FnMut(u16, u16, u16, u16, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    read_update_inner(
        reader,
        (frame_width, frame_height),
        scratch,
        |wire_encoding| selected_encoding.accepts(wire_encoding),
        decoder,
        || Ok(()),
        |_| {},
        |_| Ok(()),
        apply,
    )
}

/// Server messages about the update stream itself.
enum ServerControl<'a> {
    /// The server supports continuous updates, or stopped sending them.
    EndOfContinuousUpdates,
    Fence {
        flags: u32,
        payload: &'a [u8],
    },
    /// The server asks for relative pointer motion, or absolute positions.
    PointerMode {
        relative: bool,
    },
    /// The server accepts extended PointerEvents.
    ExtendedMouseButtons,
}

/// `accepts` says whether a rectangle's encoding is one the client asked
/// for. `started` runs once the FramebufferUpdate header arrives, before any
/// rectangle data is read, `rectangle` once per rectangle with its
/// encoding, and `control` for continuous-update and fence messages that
/// arrive before the update.
#[allow(clippy::too_many_arguments)]
fn read_update_inner(
    reader: &mut impl Read,
    (frame_width, frame_height): (u16, u16),
    scratch: &mut Vec<u8>,
    accepts: impl Fn(i32) -> bool,
    decoder: &mut UpdateDecoder,
    mut started: impl FnMut() -> io::Result<()>,
    mut rectangle: impl FnMut(i32),
    mut control: impl FnMut(ServerControl<'_>) -> io::Result<()>,
    mut apply: impl FnMut(u16, u16, u16, u16, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    loop {
        let mut kind = [0];
        reader.read_exact(&mut kind)?;
        match kind[0] {
            0 => {
                let mut header = [0; 3];
                reader.read_exact(&mut header)?;
                started()?;
                let count = u16::from_be_bytes([header[1], header[2]]);
                let mut pending = PendingRects::new();
                for _ in 0..count {
                    let mut rect = [0; 12];
                    reader.read_exact(&mut rect)?;
                    let x = u16::from_be_bytes([rect[0], rect[1]]);
                    let y = u16::from_be_bytes([rect[2], rect[3]]);
                    let width = u16::from_be_bytes([rect[4], rect[5]]);
                    let height = u16::from_be_bytes([rect[6], rect[7]]);
                    let wire_encoding = i32::from_be_bytes(rect[8..12].try_into().unwrap());
                    // Pseudo-rectangles carry no pixels and need not lie
                    // inside the framebuffer.
                    match wire_encoding {
                        POINTER_MOTION_CHANGE_ENCODING => {
                            control(ServerControl::PointerMode { relative: x == 0 })?;
                            continue;
                        }
                        EXTENDED_MOUSE_BUTTONS_ENCODING => {
                            control(ServerControl::ExtendedMouseButtons)?;
                            continue;
                        }
                        _ => {}
                    }
                    if !accepts(wire_encoding) {
                        return Err(invalid("server sent an unsupported encoding"));
                    }
                    rectangle(wire_encoding);
                    if width == 0
                        || height == 0
                        || usize::from(x) + usize::from(width) > usize::from(frame_width)
                        || usize::from(y) + usize::from(height) > usize::from(frame_height)
                    {
                        return Err(invalid("server sent an out-of-bounds rectangle"));
                    }
                    let rect = (x, y, width, height);
                    let length = usize::from(width) * usize::from(height) * 4;
                    match wire_encoding {
                        0 => {
                            scratch.resize(length, 0);
                            reader.read_exact(scratch)?;
                        }
                        6 => {
                            scratch.resize(length, 0);
                            let compressed_length = read_u32(reader)? as usize;
                            // A zlib block may expand slightly; cap it before allocating.
                            let limit = length + length / 1000 + 65_536;
                            if compressed_length > limit {
                                return Err(invalid("compressed rectangle exceeds size limit"));
                            }
                            let mut compressed = vec![0; compressed_length];
                            reader.read_exact(&mut compressed)?;
                            let zlib = &mut decoder.zlib;
                            let input_before = zlib.total_in();
                            let output_before = zlib.total_out();
                            zlib.decompress(&compressed, scratch, FlushDecompress::Sync)
                                .map_err(|_| invalid("invalid zlib rectangle"))?;
                            if zlib.total_in() - input_before != compressed_length as u64
                                || zlib.total_out() - output_before != length as u64
                            {
                                return Err(invalid("zlib rectangle has incorrect decoded length"));
                            }
                        }
                        _ => {
                            let mut jpeg = decoder.buffer();
                            if decoder.tight.read_rect_deferred(
                                reader,
                                usize::from(width),
                                usize::from(height),
                                scratch,
                                &mut jpeg,
                            )? {
                                pending.decode_jpeg(rect, jpeg, decoder)?;
                                pending.apply_ready(false, decoder, &mut apply)?;
                                continue;
                            }
                            decoder.recycle(jpeg);
                        }
                    }
                    pending.decoded(rect, scratch, decoder, &mut apply)?;
                    pending.apply_ready(false, decoder, &mut apply)?;
                }
                return pending.apply_ready(true, decoder, &mut apply);
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
            CONTINUOUS_UPDATES_MESSAGE => control(ServerControl::EndOfContinuousUpdates)?,
            FENCE_MESSAGE => {
                let (flags, payload) = read_fence(reader)?;
                control(ServerControl::Fence {
                    flags,
                    payload: &payload,
                })?;
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

/// Sends input and control messages to the server. Clones share one
/// connection, one pointer mode, and one encoding, so a mode change and the
/// input events around it are ordered.
#[derive(Clone)]
pub struct InputWriter {
    state: Arc<Mutex<WriterState>>,
    /// [`Encoding::accepted_flags`] of every encoding asked for since the
    /// handshake, which the session's reader checks without the lock:
    /// updates the server began before a switch still use the old encoding.
    accepted: Arc<AtomicU8>,
}

struct WriterState {
    stream: TcpStream,
    /// The encoding the client asks for.
    encoding: Encoding,
    /// The client advertises relative pointer motion.
    relative_allowed: bool,
    /// The server asked for relative pointer motion.
    relative: bool,
    /// The server acknowledged extended PointerEvents.
    extended_buttons: bool,
    /// The last absolute position sent.
    position: (u16, u16),
    /// The buttons last sent.
    buttons: u16,
}

impl WriterState {
    fn send_pointer(&mut self, buttons: u16, x: u16, y: u16) -> io::Result<()> {
        let (message, length) = pointer_packet(buttons, x, y, self.extended_buttons);
        self.buttons = buttons;
        self.stream.write_all(&message[..length])
    }

    /// Send `buttons` without motion, if they changed.
    fn send_buttons(&mut self, buttons: u16) -> io::Result<()> {
        if buttons == self.buttons {
            return Ok(());
        }
        let (x, y) = if self.relative {
            (
                RELATIVE_POINTER_ORIGIN as u16,
                RELATIVE_POINTER_ORIGIN as u16,
            )
        } else {
            self.position
        };
        self.send_pointer(buttons, x, y)
    }

    /// Send SetEncodings for the chosen encoding and pointer motion.
    fn send_encodings(&mut self) -> io::Result<()> {
        let relative_allowed = self.relative_allowed;
        let encodings = self
            .encoding
            .advertised()
            .into_iter()
            .filter(|encoding| relative_allowed || *encoding != POINTER_MOTION_CHANGE_ENCODING)
            .collect::<Vec<_>>();
        self.stream.write_all(&set_encodings_message(&encodings))
    }
}

impl InputWriter {
    /// A writer for a connection whose handshake advertised `encoding`.
    fn new(stream: TcpStream, encoding: Encoding) -> Self {
        Self {
            state: Arc::new(Mutex::new(WriterState {
                stream,
                encoding,
                relative_allowed: true,
                relative: false,
                extended_buttons: false,
                position: (0, 0),
                buttons: 0,
            })),
            accepted: Arc::new(AtomicU8::new(encoding.accepted_flags())),
        }
    }

    fn state(&self) -> io::Result<std::sync::MutexGuard<'_, WriterState>> {
        self.state
            .lock()
            .map_err(|_| invalid("connection lock is poisoned"))
    }

    pub fn shutdown(&self) -> io::Result<()> {
        self.state()?.stream.shutdown(std::net::Shutdown::Both)
    }

    fn send(&self, bytes: &[u8]) -> io::Result<()> {
        self.state()?.stream.write_all(bytes)
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

    fn enable_continuous_updates(&self, enable: bool, width: u16, height: u16) -> io::Result<()> {
        let mut message = [
            CONTINUOUS_UPDATES_MESSAGE,
            u8::from(enable),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        message[6..8].copy_from_slice(&width.to_be_bytes());
        message[8..10].copy_from_slice(&height.to_be_bytes());
        self.send(&message)
    }

    fn fence(&self, flags: u32, payload: &[u8]) -> io::Result<()> {
        self.send(&fence_message(flags, payload))
    }

    /// The pointer is at (`x`, `y`) with `buttons` (`BUTTON_*` bits) held.
    /// While the server asks for relative motion, only button changes are
    /// sent.
    pub fn pointer(&self, buttons: u16, x: u16, y: u16) -> io::Result<()> {
        let mut state = self.state()?;
        if state.relative {
            return state.send_buttons(buttons);
        }
        state.position = (x, y);
        state.send_pointer(buttons, x, y)
    }

    /// The pointer moved by (`dx`, `dy`) device units with `buttons` held.
    /// While the server asks for absolute positions, only button changes
    /// are sent, at the last position.
    pub fn pointer_motion(&self, buttons: u16, dx: i32, dy: i32) -> io::Result<()> {
        let mut state = self.state()?;
        if !state.relative {
            return state.send_buttons(buttons);
        }
        let (mut dx, mut dy) = (dx, dy);
        loop {
            let step_x = dx.clamp(-MAX_RELATIVE_DELTA, MAX_RELATIVE_DELTA);
            let step_y = dy.clamp(-MAX_RELATIVE_DELTA, MAX_RELATIVE_DELTA);
            state.send_pointer(
                buttons,
                (RELATIVE_POINTER_ORIGIN + step_x) as u16,
                (RELATIVE_POINTER_ORIGIN + step_y) as u16,
            )?;
            (dx, dy) = (dx - step_x, dy - step_y);
            if (dx, dy) == (0, 0) {
                return Ok(());
            }
        }
    }

    /// Whether the server asks for relative motion: the viewer should lock
    /// and hide its pointer and send [`InputWriter::pointer_motion`].
    pub fn relative_pointer(&self) -> bool {
        self.state().is_ok_and(|state| state.relative)
    }

    /// Advertise relative pointer motion (the default) or stop. Declining
    /// switches to absolute positions at once.
    pub fn set_relative_pointer_allowed(&self, allowed: bool) -> io::Result<()> {
        let mut state = self.state()?;
        if state.relative_allowed == allowed {
            return Ok(());
        }
        state.relative_allowed = allowed;
        state.relative = false;
        state.send_encodings()
    }

    /// Ask for `encoding` from the server's next update on, on the same
    /// connection. Rectangles in the encodings asked for before stay
    /// accepted, since the server may have begun them before the switch.
    pub fn set_encoding(&self, encoding: Encoding) -> io::Result<()> {
        let mut state = self.state()?;
        if state.encoding == encoding {
            return Ok(());
        }
        // Accept the new encoding before the server can send it.
        self.accepted
            .fetch_or(encoding.accepted_flags(), Ordering::Release);
        state.encoding = encoding;
        state.send_encodings()
    }

    /// Whether the client asked for rectangles in `wire_encoding`.
    fn accepts(&self, wire_encoding: i32) -> bool {
        let accepted = self.accepted.load(Ordering::Acquire);
        DECODED_ENCODINGS
            .iter()
            .position(|decoded| *decoded == wire_encoding)
            .is_some_and(|bit| accepted & 1 << bit != 0)
    }

    /// The server switched pointer modes. Input sent after this uses the
    /// new mode, so it happens before the following fence is answered.
    fn server_pointer_mode(&self, relative: bool) {
        if let Ok(mut state) = self.state() {
            // A switch announced before the client declined is ignored.
            state.relative = relative && state.relative_allowed;
        }
    }

    fn server_extended_buttons(&self) {
        if let Ok(mut state) = self.state() {
            state.extended_buttons = true;
        }
    }
}

fn key_packet(keysym: u32, down: bool) -> [u8; 8] {
    let mut message = [4, u8::from(down), 0, 0, 0, 0, 0, 0];
    message[4..8].copy_from_slice(&keysym.to_be_bytes());
    message
}

/// A PointerEvent and its length: extended (seven bytes) when the server
/// accepts extended events and back or forward is held. Without extended
/// events, back and forward are not sent, since the client advertised them.
fn pointer_packet(buttons: u16, x: u16, y: u16, extended: bool) -> ([u8; 7], usize) {
    let mut message = [5, (buttons & 0x7f) as u8, 0, 0, 0, 0, 0];
    message[2..4].copy_from_slice(&x.to_be_bytes());
    message[4..6].copy_from_slice(&y.to_be_bytes());
    if extended && buttons & (BUTTON_BACK | BUTTON_FORWARD) != 0 {
        message[1] |= EXTENDED_POINTER_MARKER;
        message[6] =
            u8::from(buttons & BUTTON_BACK != 0) | (u8::from(buttons & BUTTON_FORWARD != 0) << 1);
        return (message, 7);
    }
    (message, 6)
}

fn set_encodings_message(encodings: &[i32]) -> Vec<u8> {
    let mut message = vec![2, 0];
    message.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
    for encoding in encodings {
        message.extend_from_slice(&encoding.to_be_bytes());
    }
    message
}

pub struct Session {
    pub info: ServerInfo,
    reader: BufReader<WaitTimer<TcpStream>>,
    stats: SessionStats,
    /// Whether the next update should be requested as soon as the current
    /// one starts arriving.
    pipeline: bool,
    /// Accept continuous updates when the server offers them.
    want_continuous: bool,
    /// The server pushes updates; no requests are needed.
    continuous: bool,
    /// Also says which encodings the client has asked for.
    writer: InputWriter,
    decoder: UpdateDecoder,
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
        let stats = SessionStats::new();
        let reader = BufReader::with_capacity(
            CLIENT_READ_BUFFER_BYTES,
            WaitTimer {
                inner: stream.try_clone()?,
                stats: stats.clone(),
            },
        );
        Ok(Self {
            info,
            reader,
            stats,
            pipeline: true,
            want_continuous: true,
            continuous: false,
            writer: InputWriter::new(stream, encoding),
            decoder: UpdateDecoder::new(),
        })
    }

    pub fn writer(&self) -> InputWriter {
        self.writer.clone()
    }

    /// Whether to accept continuous updates when the server offers them; on
    /// by default. Call before the first read.
    pub fn set_continuous_updates(&mut self, enabled: bool) {
        self.want_continuous = enabled;
    }

    /// Counters that stay readable after the session moves to another thread.
    pub fn stats(&self) -> SessionStats {
        self.stats.clone()
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
        self.read_update_requesting(false, scratch, apply)
    }

    /// Like [`Session::read_update_with`], but also request the next
    /// incremental update. While the link has spare capacity, the request is
    /// sent as soon as this update starts arriving, so the server can prepare
    /// the next frame without waiting a round trip. When the client waited
    /// long for the previous update's data, the link is saturated and an
    /// early request would only queue frames, so the request is sent after
    /// the update instead. Call [`InputWriter::request_update`] once before
    /// the first read.
    pub fn read_update_pipelined(
        &mut self,
        scratch: &mut Vec<u8>,
        apply: impl FnMut(u16, u16, u16, u16, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        self.read_update_requesting(true, scratch, apply)
    }

    fn read_update_requesting(
        &mut self,
        request_next: bool,
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
        let continuous = std::cell::Cell::new(self.continuous);
        let want_continuous = self.want_continuous;
        let pipeline = self.pipeline;
        let counters = &self.stats.0;
        let network_wait = &counters.network_wait_nanos;
        let mut wait_before_body = None;
        let mut requested_early = false;
        let mut rectangles = 0u32;
        let mut apply = apply;
        let result = read_update_inner(
            &mut reader,
            (width, height),
            scratch,
            |wire_encoding| writer.accepts(wire_encoding),
            &mut self.decoder,
            || {
                wait_before_body = Some(network_wait.load(Ordering::Relaxed));
                if request_next && pipeline && !continuous.get() {
                    writer.request_update(true, width, height)?;
                    requested_early = true;
                }
                Ok(())
            },
            |encoding| {
                rectangles += 1;
                counters
                    .last_encoding
                    .store(i64::from(encoding), Ordering::Relaxed);
            },
            |message| match message {
                ServerControl::EndOfContinuousUpdates => {
                    if continuous.get() {
                        // The server stopped pushing; go back to requests.
                        continuous.set(false);
                        writer.request_update(true, width, height)
                    } else if want_continuous && request_next {
                        continuous.set(true);
                        writer.enable_continuous_updates(true, width, height)
                    } else {
                        Ok(())
                    }
                }
                // Messages are handled in order, which satisfies every
                // supported flag.
                ServerControl::Fence { flags, payload } if flags & FENCE_REQUEST != 0 => {
                    writer.fence(flags & FENCE_SUPPORTED_FLAGS, payload)
                }
                ServerControl::Fence { .. } => Ok(()),
                ServerControl::PointerMode { relative } => {
                    writer.server_pointer_mode(relative);
                    Ok(())
                }
                ServerControl::ExtendedMouseButtons => {
                    writer.server_extended_buttons();
                    Ok(())
                }
            },
            |x, y, w, h, bytes| apply(x, y, w, h, bytes),
        );
        self.continuous = continuous.get();
        counters
            .continuous_updates
            .store(self.continuous, Ordering::Relaxed);
        result?;
        if rectangles > 0 {
            counters.frames.fetch_add(1, Ordering::Relaxed);
        }
        if request_next && !self.continuous {
            if let Some(before) = wait_before_body {
                let waited = network_wait.load(Ordering::Relaxed).saturating_sub(before);
                self.pipeline = Duration::from_nanos(waited) <= PIPELINE_MAX_NETWORK_WAIT;
            }
            if !requested_early {
                writer.request_update(true, width, height)?;
            }
        }
        Ok(())
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
            &[],
        )
        .map(|_| ())
    }

    /// Read updates until one carries pixels. TopVNC's server first sends
    /// updates without any: the extended-button acknowledgement and the
    /// pointer mode.
    fn read_until_pixels(
        session: &mut Session,
        scratch: &mut Vec<u8>,
        framebuffer: &mut Framebuffer,
        pipelined: bool,
    ) {
        for _ in 0..8 {
            let mut changed = false;
            let apply = |x, y, width, height, bytes: &[u8]| {
                changed = true;
                framebuffer.apply_raw(x, y, width, height, bytes)
            };
            if pipelined {
                session.read_update_pipelined(scratch, apply)
            } else {
                session.read_update_with(scratch, apply)
            }
            .unwrap();
            if changed {
                return;
            }
        }
        panic!("no update with pixels arrived");
    }

    /// Session state around `framebuffer`, with its geometry current.
    fn session_shared(framebuffer: &Arc<Mutex<ServerFramebuffer>>) -> SessionShared {
        let geometry = Geometry::default();
        {
            let fb = framebuffer.lock().unwrap();
            geometry.store(fb.generation, fb.framebuffer.width, fb.framebuffer.height);
        }
        SessionShared {
            framebuffer: Arc::clone(framebuffer),
            geometry: Arc::new(geometry),
            relative_pointer: Arc::new(AtomicBool::new(false)),
            clipboard: Arc::new(Mutex::new(ServerClipboard::default())),
            thread_setup: None,
            foveation: Foveation::Off,
        }
    }

    fn client_reader(session: &SessionShared, client_id: u64) -> ClientReader {
        ClientReader {
            client_id,
            initial_generation: session.geometry.load().0,
            geometry: Arc::clone(&session.geometry),
            announced_relative: Arc::new(AtomicBool::new(false)),
            encodings: ClientEncodings::default(),
            position: None,
        }
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
        update_server_framebuffer(&shared, &Geometry::default(), &changed).unwrap();

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
        start_server(
            framebuffer,
            ServerConfig {
                allow_insecure: true,
                ..ServerConfig::default()
            },
        )
    }

    fn start_server(
        framebuffer: Framebuffer,
        config: ServerConfig,
    ) -> (Arc<VncServer>, std::net::SocketAddr) {
        let server = Arc::new(VncServer::bind("127.0.0.1:0", framebuffer, config).unwrap());
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
    fn reader_drops_stale_pointer_events_before_the_writer_sees_a_resize() {
        let shared = Arc::new(Mutex::new(ServerFramebuffer::new(
            Framebuffer::new(4, 4).unwrap(),
        )));
        let session_state = session_shared(&shared);
        let mut reader = client_reader(&session_state, 1);
        let (session, _session_receiver) = std::sync::mpsc::sync_channel(1);
        let (events, event_receiver) = std::sync::mpsc::sync_channel(1);
        let (mut server_stream, mut client_stream) = tcp_pair();
        let mut read_pointer = |x: u8, y: u8| {
            client_stream.write_all(&[5, 0, 0, x, 0, y]).unwrap();
            read_client_message(&mut server_stream, &session, &events, &mut reader)
        };

        // Out of bounds without a resize is a protocol error.
        assert!(read_pointer(9, 0).is_err());
        // The writer has not run, so only the framebuffer knows it shrank.
        update_server_framebuffer(
            &shared,
            &session_state.geometry,
            &Framebuffer::new(2, 2).unwrap(),
        )
        .unwrap();
        read_pointer(3, 3).unwrap();
        assert!(event_receiver.try_recv().is_err());
        read_pointer(1, 1).unwrap();
        assert_eq!(
            event_receiver.try_recv().unwrap(),
            ClientEvent::Pointer {
                client_id: 1,
                buttons: 0,
                x: 1,
                y: 1,
            }
        );
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
        update_server_framebuffer_regions(&shared, &Geometry::default(), &changed, &[damage])
            .unwrap();
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
        update_server_framebuffer_regions(&shared, &Geometry::default(), &changed, &[damage])
            .unwrap();
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
        assert!(
            update_server_framebuffer_regions(&shared, &Geometry::default(), &changed, &[outside])
                .is_err()
        );
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
        let session = session_shared(&shared);
        let sessions = Arc::new(Mutex::new(ServerSessions::default()));
        let server_sessions = Arc::clone(&sessions);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            server_sessions
                .lock()
                .unwrap()
                .register(44, stream.try_clone().unwrap())
                .unwrap();
            let result = serve_client(&mut stream, &session, &tx, &config, &server_sessions, 44);
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
        update_server_framebuffer(&shared, &Geometry::default(), &changed_framebuffer).unwrap();
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
        read_until_pixels(&mut session, &mut scratch, &mut received, false);
        assert_eq!(received.pixels(), &[0xff0000, 0x00ff00]);

        let mut changed = Framebuffer::new(2, 1).unwrap();
        changed
            .apply_raw(0, 0, 2, 1, &[0, 0, 255, 0, 255, 0, 0, 0])
            .unwrap();
        server.update_framebuffer(&changed).unwrap();
        writer.request_update(true, 2, 1).unwrap();
        read_until_pixels(&mut session, &mut scratch, &mut received, false);
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

    /// Smooth many-color content on the left, two-color stripes on the
    /// right, with edge tiles narrower and shorter than 64 pixels.
    fn mixed_content(width: u16, height: u16) -> Framebuffer {
        let mut framebuffer = Framebuffer::new(width, height).unwrap();
        let columns = usize::from(width);
        for (index, pixel) in framebuffer.pixels_mut().iter_mut().enumerate() {
            let (x, y) = ((index % columns) as u32, (index / columns) as u32);
            *pixel = if x < 150 {
                (x * 255 / 150) << 16 | (y * 255 / u32::from(height)) << 8 | ((x + y) / 2)
            } else if (y / 3) % 2 == 0 {
                0xffffff
            } else {
                0x202020
            };
        }
        framebuffer
    }

    #[test]
    fn parallel_jpeg_rectangles_apply_in_wire_order() {
        let (width, height) = (128u16, 64u16);
        let mut source = mixed_content(width, height);
        // Noise, so the encoder picks JPEG rather than a palette.
        let mut seed = 5u32;
        for pixel in source.pixels_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            *pixel = seed & 0x00ff_ffff;
        }
        let settings = TightSettings {
            quality: Some(9),
            compression: 1,
        };
        let mut jpeg = Vec::new();
        tight::encode_rect(
            source.pixels(),
            usize::from(width),
            usize::from(height),
            settings,
            &mut jpeg,
        )
        .unwrap();
        assert_eq!(jpeg[0] >> 4, 0x09, "the rectangle is JPEG");
        let rect = |message: &mut Vec<u8>, x: u16, y: u16, w: u16, h: u16| {
            for value in [x, y, w, h] {
                message.extend_from_slice(&value.to_be_bytes());
            }
            message.extend_from_slice(&tight::TIGHT_ENCODING.to_be_bytes());
        };
        // A JPEG rectangle, a fill over part of it, then a second JPEG.
        let mut message = vec![0, 0, 0, 3];
        rect(&mut message, 0, 0, width, height);
        message.extend_from_slice(&jpeg);
        rect(&mut message, 8, 8, 4, 4);
        message.extend_from_slice(&[0x80, 0x12, 0x34, 0x56]);
        rect(&mut message, 0, height, width, height);
        message.extend_from_slice(&jpeg);
        let mut decoder = UpdateDecoder::new();
        let mut framebuffer = Framebuffer::new(width, height * 2).unwrap();
        let mut order = Vec::new();
        read_update_with_encoding(
            &mut Cursor::new(&message),
            width,
            height * 2,
            &mut Vec::new(),
            Encoding::Tight { quality: 9 },
            &mut decoder,
            |x, y, w, h, bytes| {
                order.push(y);
                framebuffer.apply_raw(x, y, w, h, bytes)
            },
        )
        .unwrap();
        assert_eq!(order, [0, 8, height]);
        let pixels = framebuffer.pixels();
        let stride = usize::from(width);
        assert_eq!(pixels[9 * stride + 9], 0x123456);
        // The JPEG content arrived around the fill and in the second band.
        let close = |a: u32, b: u32| {
            [0, 8, 16]
                .iter()
                .all(|shift| ((a >> shift & 0xff) as i32 - (b >> shift & 0xff) as i32).abs() <= 24)
        };
        assert!(close(pixels[0], source.pixels()[0]));
        assert!(close(
            pixels[usize::from(height) * stride + 5],
            source.pixels()[5]
        ));

        // Data that is not JPEG, and JPEG of the wrong size, fail the update
        // after earlier rectangles were applied in order.
        let mut broken = jpeg.clone();
        let start = broken
            .windows(2)
            .position(|marker| marker == [0xff, 0xd8])
            .unwrap();
        broken[start + 1] = 0;
        let mut corrupt = vec![0, 0, 0, 2];
        rect(&mut corrupt, 8, 8, 4, 4);
        corrupt.extend_from_slice(&[0x80, 1, 2, 3]);
        rect(&mut corrupt, 0, 0, width, height);
        corrupt.extend_from_slice(&broken);
        let mut applied = 0;
        assert!(
            read_update_with_encoding(
                &mut Cursor::new(&corrupt),
                width,
                height * 2,
                &mut Vec::new(),
                Encoding::Tight { quality: 9 },
                &mut decoder,
                |_, _, _, _, _| {
                    applied += 1;
                    Ok(())
                },
            )
            .is_err()
        );
        assert_eq!(applied, 1);
        let mut wrong_size = vec![0, 0, 0, 1];
        rect(&mut wrong_size, 0, 0, width / 2, height);
        wrong_size.extend_from_slice(&jpeg);
        assert!(
            read_update_with_encoding(
                &mut Cursor::new(&wrong_size),
                width,
                height * 2,
                &mut Vec::new(),
                Encoding::Tight { quality: 9 },
                &mut decoder,
                |_, _, _, _, _| Ok(()),
            )
            .is_err()
        );
    }

    #[test]
    fn tight_without_a_quality_level_is_lossless() {
        let source = mixed_content(300, 140);
        let (server, address) = start_insecure_server(source.clone());
        let (mut client, width, height) = raw_client(address, &[tight::TIGHT_ENCODING]);
        request(&mut client, false, width, height);
        let mut received = Framebuffer::new(width, height).unwrap();
        read_update_with_encoding(
            &mut client,
            width,
            height,
            &mut Vec::new(),
            Encoding::Tight { quality: 0 },
            &mut UpdateDecoder::new(),
            |x, y, width, height, bytes| received.apply_raw(x, y, width, height, bytes),
        )
        .unwrap();
        assert_eq!(received.pixels(), source.pixels());
        server.stop();
    }

    #[test]
    fn tight_session_receives_jpeg_and_pipelined_incremental_updates() {
        let source = mixed_content(300, 140);
        let (server, address) = start_insecure_server(source.clone());
        let mut session = Session::connect_with_encoding(
            &address.to_string(),
            true,
            Encoding::Tight { quality: 9 },
            || unreachable!(),
        )
        .unwrap();
        let writer = session.writer();
        let mut received = Framebuffer::new(300, 140).unwrap();
        let mut scratch = Vec::new();
        writer.request_update(false, 300, 140).unwrap();
        read_until_pixels(&mut session, &mut scratch, &mut received, true);
        for (expected, actual) in source.pixels().iter().zip(received.pixels()) {
            for shift in [0, 8, 16] {
                let difference =
                    ((expected >> shift & 0xff) as i32 - (actual >> shift & 0xff) as i32).abs();
                assert!(difference <= 16, "{expected:06x} received as {actual:06x}");
            }
        }

        // The next request was sent when the first update started arriving,
        // so the change arrives without another explicit request.
        let mut changed = source.clone();
        changed.pixels_mut()[299] = 0x00ff00;
        server.update_framebuffer(&changed).unwrap();
        // TopVNC's server offers continuous updates and the session takes them.
        assert!(session.stats().snapshot().continuous_updates);
        let deadline = Instant::now() + Duration::from_secs(2);
        while received.pixels()[299] != 0x00ff00 {
            assert!(Instant::now() < deadline, "the change never arrived");
            session
                .read_update_pipelined(&mut scratch, |x, y, width, height, bytes| {
                    received.apply_raw(x, y, width, height, bytes)
                })
                .unwrap();
        }
        server.stop();
    }

    #[test]
    fn session_switches_encodings_without_reconnecting() {
        let source = mixed_content(300, 140);
        let (server, address) = start_insecure_server(source.clone());
        let mut session = Session::connect_with_encoding(
            &address.to_string(),
            true,
            Encoding::Raw,
            || unreachable!(),
        )
        .unwrap();
        let writer = session.writer();
        let stats = session.stats();
        let mut received = Framebuffer::new(300, 140).unwrap();
        let mut scratch = Vec::new();
        writer.request_update(false, 300, 140).unwrap();
        read_until_pixels(&mut session, &mut scratch, &mut received, true);
        assert_eq!(stats.snapshot().encoding, Some(0));

        // Each switch reaches the server's next updates; ones already on the
        // wire in the old encoding are still decoded.
        let mut changed = source;
        let mut serial = 0;
        for (encoding, wire_encoding) in [
            (Encoding::Tight { quality: 6 }, tight::TIGHT_ENCODING),
            (Encoding::Raw, 0),
        ] {
            writer.set_encoding(encoding).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while stats.snapshot().encoding != Some(wire_encoding) {
                assert!(Instant::now() < deadline, "{encoding:?} never arrived");
                serial += 1;
                changed.pixels_mut()[0] = serial;
                server.update_framebuffer(&changed).unwrap();
                session
                    .read_update_pipelined(&mut scratch, |x, y, width, height, bytes| {
                        received.apply_raw(x, y, width, height, bytes)
                    })
                    .unwrap();
            }
        }
        server.stop();
    }

    #[test]
    fn tight_clients_with_other_pixel_formats_receive_raw() {
        let (server, address) = start_insecure_server(mixed_content(70, 10));
        let (mut client, width, height) =
            raw_client(address, &[tight::TIGHT_ENCODING, tight::QUALITY_LEVEL_0]);
        // 16-bit RGB565, little-endian.
        client
            .write_all(&[
                0, 0, 0, 0, 16, 16, 0, 1, 0, 31, 0, 63, 0, 31, 11, 5, 0, 0, 0, 0,
            ])
            .unwrap();
        request(&mut client, false, width, height);
        let mut header = [0; 16];
        client.read_exact(&mut header).unwrap();
        assert_eq!(i32::from_be_bytes(header[12..16].try_into().unwrap()), 0);
        server.stop();
    }

    #[test]
    fn client_encodings_follow_the_preference_order() {
        let parse = |encodings: &[i32]| ClientEncodings::parse(encodings.iter().copied());
        assert_eq!(parse(&[0, 7, -30]).tight, None);
        assert_eq!(
            parse(&[7, 0, -30, -250, -29]).tight,
            Some(TightSettings {
                quality: Some(2),
                compression: 6,
            })
        );
        assert_eq!(parse(&[7]).tight, Some(TightSettings::default()));
        let with_size = parse(&[DESKTOP_SIZE_ENCODING, 7]);
        assert!(with_size.desktop_size);
        assert!(with_size.tight.is_some());
        assert_eq!(parse(&[6, 1, -33, -246]), ClientEncodings::default());
    }

    #[test]
    fn tight_rectangles_merge_tiles_and_respect_size_limits() {
        let tile = |x: u16, y: u16, width: u16, height: u16| ServerRect {
            x,
            y,
            width,
            height,
            tile_index: None,
            revision: 0,
        };
        // A 3x2 block of tiles plus a separate tile below it.
        let mut rects = Vec::new();
        for row in 0..2 {
            for column in 0..3 {
                rects.push(tile(column * 64, row * 64, 64, 64));
            }
        }
        rects.push(tile(64, 128, 64, 20));
        assert_eq!(
            tight_rects(&rects, tight::MAX_RECT_HEIGHT),
            vec![(0, 0, 192, 128), (64, 128, 64, 20)]
        );
        // One-tile bands keep the block apart row by row.
        assert_eq!(
            tight_rects(&rects, 64),
            vec![(0, 0, 192, 64), (0, 64, 192, 64), (64, 128, 64, 20)]
        );

        // A full 2560x320 area: 2048-pixel-wide and 256-pixel-tall pieces.
        let rects = (0..5)
            .flat_map(|row| (0..40).map(move |column| tile(column * 64, row * 64, 64, 64)))
            .collect::<Vec<_>>();
        assert_eq!(
            tight_rects(&rects, tight::MAX_RECT_HEIGHT),
            vec![
                (0, 0, 2048, 256),
                (2048, 0, 512, 256),
                (0, 256, 2048, 64),
                (2048, 256, 512, 64),
            ]
        );
        // A non-incremental request is a single rectangle; it is split too.
        assert_eq!(
            tight_rects(&[tile(0, 0, 100, 300)], 128),
            vec![(0, 0, 100, 128), (0, 128, 100, 128), (0, 256, 100, 44)]
        );
        assert_eq!(
            tight_rects(&[tile(0, 0, 2100, 300)], tight::MAX_RECT_HEIGHT),
            vec![
                (0, 0, 2048, 256),
                (2048, 0, 52, 256),
                (0, 256, 2048, 44),
                (2048, 256, 52, 44),
            ]
        );
    }

    /// Smooth, grainy content like a game frame, which Tight sends as JPEG.
    fn grainy_frame(width: u16, height: u16) -> Framebuffer {
        let mut framebuffer = Framebuffer::new(width, height).unwrap();
        let columns = usize::from(width);
        let mut seed = 3u32;
        for (index, pixel) in framebuffer.pixels_mut().iter_mut().enumerate() {
            let (x, y) = ((index % columns) as f32, (index / columns) as f32);
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let grain = (seed % 9) as f32 - 4.0;
            let wave = 50.0 * ((x * 0.05).sin() * (y * 0.04).cos());
            let channel = |base: f32| (base + wave + grain).clamp(0.0, 255.0) as u32;
            *pixel =
                channel(120.0) << 16 | channel(100.0 + y * 0.1) << 8 | channel(70.0 + x * 0.05);
        }
        framebuffer
    }

    fn full_region(width: u16, height: u16) -> UpdateRequest {
        UpdateRequest {
            incremental: true,
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    fn session_encoder(fovea: fovea::State) -> SessionEncoder {
        SessionEncoder {
            pool: None,
            thread_setup: None,
            fovea,
        }
    }

    /// Write `update` with Tight and return the FramebufferUpdate's bytes.
    #[allow(clippy::too_many_arguments)]
    fn tight_update_bytes(
        shared: &Arc<Mutex<ServerFramebuffer>>,
        seen_revisions: &mut [u64],
        settings: TightSettings,
        zones: Option<[u8; 3]>,
        update: PreparedUpdate,
        encoder: &mut SessionEncoder,
    ) -> Vec<u8> {
        let (mut server, mut client) = tcp_pair();
        let generation = shared.lock().unwrap().generation;
        let (bytes, _) = write_tight_update(
            &mut server,
            shared,
            seen_revisions,
            &mut Vec::new(),
            settings,
            zones,
            update,
            generation,
            encoder,
            &[],
        )
        .unwrap();
        let mut message = vec![0; bytes];
        client.read_exact(&mut message).unwrap();
        message
    }

    fn decode_tight(message: &[u8], width: u16, height: u16) -> Framebuffer {
        let mut framebuffer = Framebuffer::new(width, height).unwrap();
        read_update_with_encoding(
            &mut Cursor::new(message),
            width,
            height,
            &mut Vec::new(),
            Encoding::Tight { quality: 6 },
            &mut UpdateDecoder::new(),
            |x, y, w, h, bytes| framebuffer.apply_raw(x, y, w, h, bytes),
        )
        .unwrap();
        framebuffer
    }

    #[test]
    fn foveated_updates_send_each_zone_at_its_level_center_first() {
        let (width, height) = (640u16, 384u16);
        let frame = (usize::from(width), usize::from(height));
        let source = grainy_frame(width, height);
        let shared = Arc::new(Mutex::new(ServerFramebuffer::new(source.clone())));
        let tile_count = shared.lock().unwrap().tile_revisions.len();
        let mut seen_revisions = vec![u64::MAX; tile_count];
        let update = prepare_update(
            &shared,
            &seen_revisions,
            full_region(width, height),
            false,
            0,
        )
        .unwrap()
        .unwrap();
        let changed = update.rectangles.clone();
        let mut encoder = session_encoder(fovea::State::new());
        let settings = TightSettings {
            quality: Some(6),
            compression: 1,
        };
        let zones = encoder.fovea.levels(6);
        assert_eq!(zones, [6, 4, 1]);
        let message = tight_update_bytes(
            &shared,
            &mut seen_revisions,
            settings,
            Some(zones),
            update,
            &mut encoder,
        );
        assert!(seen_revisions.iter().all(|revision| *revision != u64::MAX));

        // Center first, and every rectangle exactly as Tight encodes it at
        // its zone's level.
        let layout = fovea::foveated_rects(&changed, frame);
        assert_eq!(layout[0], ((192, 128, 256, 64), fovea::Zone::Fovea));
        assert_eq!(layout.last().unwrap().1, fovea::Zone::Periphery);
        let mut expected = vec![0, 0];
        expected.extend_from_slice(&(layout.len() as u16).to_be_bytes());
        for &((x, y, w, h), zone) in &layout {
            for value in [x, y, w, h] {
                expected.extend_from_slice(&(value as u16).to_be_bytes());
            }
            expected.extend_from_slice(&tight::TIGHT_ENCODING.to_be_bytes());
            let mut pixels = Vec::with_capacity(w * h);
            for row in y..y + h {
                pixels.extend_from_slice(&source.pixels()[row * frame.0 + x..][..w]);
            }
            let zone_settings = TightSettings {
                quality: Some(zones[zone as usize]),
                ..settings
            };
            tight::encode_rect(&pixels, w, h, zone_settings, &mut expected).unwrap();
        }
        assert_eq!(message, expected);

        // The mixed-quality update decodes; the fovea is pixel-identical to
        // today's bands at the client's level, and sharper than the
        // periphery.
        let foveated = decode_tight(&message, width, height);
        let mut seen_revisions = vec![u64::MAX; tile_count];
        let update = prepare_update(
            &shared,
            &seen_revisions,
            full_region(width, height),
            false,
            0,
        )
        .unwrap()
        .unwrap();
        let banded = decode_tight(
            &tight_update_bytes(
                &shared,
                &mut seen_revisions,
                settings,
                None,
                update,
                &mut encoder,
            ),
            width,
            height,
        );
        let mut error = [(0u64, 0u64); 3];
        for &((x, y, w, h), zone) in &layout {
            for row in y..y + h {
                for index in row * frame.0 + x..row * frame.0 + x + w {
                    let (actual, wanted) = (foveated.pixels()[index], source.pixels()[index]);
                    if zone == fovea::Zone::Fovea {
                        assert_eq!(actual, banded.pixels()[index]);
                    }
                    for shift in [0, 8, 16] {
                        let difference = (i64::from(actual >> shift & 0xff)
                            - i64::from(wanted >> shift & 0xff))
                        .unsigned_abs();
                        error[zone as usize].0 += difference;
                    }
                    error[zone as usize].1 += 3;
                }
            }
        }
        let mean = error.map(|(sum, count)| sum as f64 / count as f64);
        assert!(mean[0] < mean[1] && mean[1] < mean[2], "{mean:?}");
        assert!(mean[2] < 12.0, "{mean:?}");
    }

    #[test]
    fn auto_foveation_follows_the_hosts_relative_pointer_wish() {
        let (width, height) = (640u16, 384u16);
        let (server, address) = start_server(
            grainy_frame(width, height),
            ServerConfig {
                allow_insecure: true,
                foveation: Foveation::Auto,
                ..ServerConfig::default()
            },
        );
        // The client does not support relative motion; foveation follows
        // the host's wish anyway.
        let (mut client, width, height) = raw_client(
            address,
            &[tight::TIGHT_ENCODING, tight::QUALITY_LEVEL_0 + 6],
        );
        let mut decoder = UpdateDecoder::new();
        let mut layout = |client: &mut TcpStream| {
            request(client, false, width, height);
            let mut rects = Vec::new();
            read_update_with_encoding(
                client,
                width,
                height,
                &mut Vec::new(),
                Encoding::Tight { quality: 6 },
                &mut decoder,
                |x, y, w, h, _| {
                    rects.push((
                        usize::from(x),
                        usize::from(y),
                        usize::from(w),
                        usize::from(h),
                    ));
                    Ok(())
                },
            )
            .unwrap();
            rects
        };
        let whole = [ServerRect {
            x: 0,
            y: 0,
            width,
            height,
            tile_index: None,
            revision: 0,
        }];
        let bands = tight_rects(&whole, band_rows(usize::from(height), encoder_threads()));
        let foveated: Vec<_> = fovea::foveated_rects(&whole, (640, 384))
            .into_iter()
            .map(|(rect, _)| rect)
            .collect();
        assert_eq!(layout(&mut client), bands);
        server.set_relative_pointer(true);
        assert_eq!(layout(&mut client), foveated);
        server.set_relative_pointer(false);
        assert_eq!(layout(&mut client), bands);
        server.stop();
    }

    #[test]
    fn held_back_periphery_tiles_stay_pending_and_go_out_later() {
        let (width, height) = (640u16, 384u16);
        let frame = (usize::from(width), usize::from(height));
        let mut source = grainy_frame(width, height);
        let shared = Arc::new(Mutex::new(ServerFramebuffer::new(source.clone())));
        let periphery: Vec<bool> = {
            let fb = shared.lock().unwrap();
            (0..fb.tile_revisions.len())
                .map(|index| fovea::zone(fb.tile_rect(index), frame) == fovea::Zone::Periphery)
                .collect()
        };
        let in_periphery =
            |rect: &ServerRect| rect.tile_index.is_some_and(|index| periphery[index]);
        let mut seen_revisions = vec![u64::MAX; periphery.len()];
        // Rung 3 sends the periphery with every second update.
        let mut encoder = session_encoder(fovea::State::at_rung(3));
        let settings = TightSettings {
            quality: Some(6),
            compression: 1,
        };
        let zones = Some(encoder.fovea.levels(6));
        let region = full_region(width, height);

        let mut first = prepare_update(&shared, &seen_revisions, region, true, 0)
            .unwrap()
            .unwrap();
        skip_periphery(&shared, &mut first, 0, &mut encoder.fovea).unwrap();
        assert!(!first.rectangles.is_empty());
        assert!(!first.rectangles.iter().any(in_periphery));
        assert!(
            !first
                .acknowledged
                .iter()
                .any(|&(index, _)| periphery[index])
        );
        assert!(encoder.fovea.periphery_due().is_some());
        tight_update_bytes(
            &shared,
            &mut seen_revisions,
            settings,
            zones,
            first,
            &mut encoder,
        );
        for (index, revision) in seen_revisions.iter().enumerate() {
            assert_eq!(*revision == u64::MAX, periphery[index], "tile {index}");
        }

        // The next update with a center change carries the held-back tiles.
        source.pixels_mut()[192 * frame.0 + 320] ^= 0xffffff;
        update_server_framebuffer(&shared, &Geometry::default(), &source).unwrap();
        let mut second = prepare_update(&shared, &seen_revisions, region, true, 0)
            .unwrap()
            .unwrap();
        skip_periphery(&shared, &mut second, 0, &mut encoder.fovea).unwrap();
        assert_eq!(encoder.fovea.periphery_due(), None);
        assert_eq!(
            second
                .rectangles
                .iter()
                .filter(|rect| in_periphery(rect))
                .count(),
            periphery.iter().filter(|tile| **tile).count()
        );
        assert_eq!(
            second
                .rectangles
                .iter()
                .filter(|rect| !in_periphery(rect))
                .count(),
            1
        );
        tight_update_bytes(
            &shared,
            &mut seen_revisions,
            settings,
            zones,
            second,
            &mut encoder,
        );
        let third = prepare_update(&shared, &seen_revisions, region, true, 0)
            .unwrap()
            .unwrap();
        assert!(third.rectangles.is_empty());
    }

    #[test]
    fn topvnc_session_decodes_foveated_updates() {
        let (width, height) = (640u16, 384u16);
        let source = grainy_frame(width, height);
        let (server, address) = start_server(
            source.clone(),
            ServerConfig {
                allow_insecure: true,
                foveation: Foveation::On,
                ..ServerConfig::default()
            },
        );
        let mut session = Session::connect_with_encoding(
            &address.to_string(),
            true,
            Encoding::Tight { quality: 6 },
            || unreachable!(),
        )
        .unwrap();
        let writer = session.writer();
        let mut received = Framebuffer::new(width, height).unwrap();
        let mut scratch = Vec::new();
        writer.request_update(false, width, height).unwrap();
        read_until_pixels(&mut session, &mut scratch, &mut received, true);
        let mut worst = [0; 3];
        for (index, (wanted, actual)) in source.pixels().iter().zip(received.pixels()).enumerate() {
            let (x, y) = (index % usize::from(width), index / usize::from(width));
            let zone = fovea::zone((x / 64 * 64, y / 64 * 64, 64, 64), (640, 384)) as usize;
            for shift in [0, 8, 16] {
                let difference = (wanted >> shift & 0xff).abs_diff(actual >> shift & 0xff);
                worst[zone] = worst[zone].max(difference);
            }
        }
        // The center is as close as quality 6 gets; the periphery, at
        // level 1, is coarser.
        assert!(worst[0] <= 24 && worst[0] < worst[2], "{worst:?}");
        assert!(worst[2] <= 64, "{worst:?}");
        server.stop();
    }

    #[test]
    fn delivery_rate_follows_arrival_times() {
        let start = Instant::now();
        let at = |us: u64| start + Duration::from_micros(us);
        let mut flow = FlowControl::default();
        flow.probing(at(0));
        // The probe arrived after 10 ms and was handled 2 ms later.
        flow.acknowledged(&FLOW_PROBE, at(12_000), at(10_000));
        assert_eq!(flow.base_delay, Some(Duration::from_millis(12)));
        assert_eq!(flow.arrival_base_delay, Some(Duration::from_millis(10)));
        // 1 MB written from 100 ms and acknowledged on arrival at 130 ms:
        // 20 ms beyond the round trip, a link of 50 MB/s.
        let first = flow.sent(1_000_000, at(100_000), at(105_000));
        flow.acknowledged(&first, at(130_000), at(130_000));
        // Two updates queued back to back: their acknowledgements arrive
        // 20 ms apart, the second one's transmission time, but the session
        // handles them together. The pacing estimate takes the 0.1 ms gap
        // as a transfer at 2 GB/s; the delivery estimate takes the gap
        // between arrivals instead of the time since the third was written,
        // which includes 20 ms of queueing.
        let second = flow.sent(1_000_000, at(200_000), at(205_000));
        let third = flow.sent(1_000_000, at(205_000), at(210_000));
        flow.acknowledged(&second, at(256_000), at(235_000));
        flow.acknowledged(&third, at(256_100), at(255_000));
        assert_eq!(flow.throughput(), Some(2e9));
        assert_eq!(flow.delivery_rate(), None, "needs four samples");
        let fourth = flow.sent(1_000_000, at(300_000), at(305_000));
        flow.acknowledged(&fourth, at(330_000), at(330_000));
        // A small update delivered in a burst barely moves the estimate:
        // 4.01 MB over 20 + 25 + 20 + 20 + 0.5 ms.
        let small = flow.sent(10_000, at(400_000), at(400_000));
        flow.acknowledged(&small, at(410_100), at(410_100));
        let rate = flow.delivery_rate().unwrap();
        assert!((rate - 4.01e6 / 0.0855).abs() < 1.0, "{rate}");
        // Without a measured round trip there are no delivery samples.
        let mut unprobed = FlowControl::default();
        for update in 0..8 {
            let sent = at(update * 10_000);
            let payload = unprobed.sent(1000, sent, sent);
            let arrived = sent + Duration::from_millis(5);
            unprobed.acknowledged(&payload, arrived, arrived);
        }
        assert!(unprobed.throughput().is_some());
        assert_eq!(unprobed.delivery_rate(), None);
    }

    #[test]
    fn host_frames_count_changes_across_resizes() {
        let geometry = Geometry::default();
        let mut frame = Framebuffer::new(128, 64).unwrap();
        let shared = Arc::new(Mutex::new(ServerFramebuffer::new(frame.clone())));
        let frames = || shared.lock().unwrap().frames;
        // A publish that changes nothing is not a frame.
        update_server_framebuffer(&shared, &geometry, &frame).unwrap();
        assert_eq!(frames(), 0);
        frame.pixels_mut()[0] = 0xffffff;
        update_server_framebuffer(&shared, &geometry, &frame).unwrap();
        assert_eq!(frames(), 1);
        let mut resized = Framebuffer::new(64, 64).unwrap();
        update_server_framebuffer(&shared, &geometry, &resized).unwrap();
        assert_eq!(frames(), 1);
        resized.pixels_mut()[0] = 0x00ff00;
        update_server_framebuffer(&shared, &geometry, &resized).unwrap();
        assert_eq!(frames(), 2);
    }

    /// Codec timing for a 1920x1080 frame of smooth, grainy content:
    /// `cargo test --release --lib tight_codec_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn tight_codec_timing() {
        let sizes = std::env::var("TOPVNC_TIMING_SIZES").unwrap_or_else(|_| "1920x1080".into());
        for size in sizes.split(',') {
            let (width, height) = size.split_once('x').unwrap();
            tight_codec_timing_at(width.parse().unwrap(), height.parse().unwrap());
        }
    }

    fn tight_codec_timing_at(width: usize, height: usize) {
        println!("{width}x{height}:");
        let mut seed = 1u32;
        let pixels: Vec<u32> = (0..width * height)
            .map(|index| {
                let (x, y) = ((index % width) as f32, (index / width) as f32);
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let grain = (seed % 13) as f32 - 6.0;
                let wave = 40.0 * ((x * 0.03).sin() * (y * 0.02).cos());
                let channel = |base: f32| (base + wave + grain).clamp(0.0, 255.0) as u32;
                channel(120.0) << 16 | channel(90.0 + y * 0.1) << 8 | channel(60.0 + x * 0.05)
            })
            .collect();
        let tiles = (0..height.div_ceil(64))
            .flat_map(|row| {
                (0..width.div_ceil(64)).map(move |column| ServerRect {
                    x: (column * 64) as u16,
                    y: (row * 64) as u16,
                    width: 64.min(width - column * 64) as u16,
                    height: 64.min(height - row * 64) as u16,
                    tile_index: None,
                    revision: 0,
                })
            })
            .collect::<Vec<_>>();
        let workers = encoder_threads();
        let pool = EncodePool::new(workers, None).unwrap();
        let settings = |quality| TightSettings {
            quality: Some(quality),
            compression: 1,
        };
        // 0.2's bands, then bands sized for this machine's threads, then the
        // first foveated rungs (spec 008) as a quality-6 client gets them.
        let mut cases = Vec::new();
        for rows in [tight::MAX_RECT_HEIGHT, band_rows(height, workers)] {
            let rects = tight_rects(&tiles, rows);
            for quality in [3, 6, 9] {
                let all = vec![settings(quality); rects.len()];
                cases.push((
                    format!("{rows}-row bands, quality {quality}"),
                    rects.clone(),
                    all,
                ));
            }
        }
        let layout = fovea::foveated_rects(&tiles, (width, height));
        for (rung, name) in [(0, "Sharp"), (1, "Balanced"), (2, "Wi-Fi")] {
            let levels = fovea::State::at_rung(rung).levels(6);
            cases.push((
                format!("foveated {name} {levels:?}"),
                layout.iter().map(|&(rect, _)| rect).collect(),
                layout
                    .iter()
                    .map(|&(_, zone)| settings(levels[zone as usize]))
                    .collect(),
            ));
        }
        for (name, rects, rect_settings) in cases {
            let snapshots = rects
                .iter()
                .map(|&(x, y, w, h)| {
                    let mut copy = Vec::with_capacity(w * h);
                    for row in y..y + h {
                        copy.extend_from_slice(&pixels[row * width + x..row * width + x + w]);
                    }
                    (copy, w, h)
                })
                .collect::<Vec<_>>();
            // The rectangle holding the framebuffer's center pixel.
            let center = rects
                .iter()
                .position(|&(x, y, w, h)| {
                    (x..x + w).contains(&(width / 2)) && (y..y + h).contains(&(height / 2))
                })
                .unwrap();
            // Warm the threads and allocator, then time the second run.
            let mut bodies = Vec::new();
            let (mut first, mut centered) = (Duration::ZERO, Duration::ZERO);
            for _ in 0..2 {
                bodies.clear();
                let started = Instant::now();
                pool.encode(snapshots.clone(), rect_settings.clone(), |index, body| {
                    if index == 0 {
                        first = started.elapsed();
                    }
                    if index == center {
                        centered = started.elapsed();
                    }
                    bodies.push(body.to_vec());
                    Ok(())
                })
                .unwrap();
            }
            let started = Instant::now();
            pool.encode(snapshots.clone(), rect_settings.clone(), |_, _| Ok(()))
                .unwrap();
            let encoded = started.elapsed();
            let sequential = Instant::now();
            for ((pixels, w, h), settings) in snapshots.iter().zip(&rect_settings) {
                tight::encode_rect(pixels, *w, *h, *settings, &mut Vec::new()).unwrap();
            }
            let sequential = sequential.elapsed();
            let mut decoder = tight::TightDecoder::new();
            let mut output = Vec::new();
            let started = Instant::now();
            for (body, &(_, _, w, h)) in bodies.iter().zip(&rects) {
                decoder
                    .read_rect(&mut Cursor::new(body), w, h, &mut output)
                    .unwrap();
            }
            let decoded = started.elapsed();
            // The same rectangles as one update, decoded the way a session
            // does, with JPEG on the decoder threads.
            let mut update = vec![0, 0];
            update.extend_from_slice(&(rects.len() as u16).to_be_bytes());
            for (body, &(x, y, w, h)) in bodies.iter().zip(&rects) {
                for value in [x, y, w, h] {
                    update.extend_from_slice(&(value as u16).to_be_bytes());
                }
                update.extend_from_slice(&tight::TIGHT_ENCODING.to_be_bytes());
                update.extend_from_slice(body);
            }
            let mut update_decoder = UpdateDecoder::new();
            let mut framebuffer = Framebuffer::new(width as u16, height as u16).unwrap();
            let mut pooled = Duration::ZERO;
            // The first run starts the threads.
            for _ in 0..2 {
                let started = Instant::now();
                read_update_with_encoding(
                    &mut Cursor::new(&update),
                    width as u16,
                    height as u16,
                    &mut Vec::new(),
                    Encoding::Tight { quality: 6 },
                    &mut update_decoder,
                    |x, y, w, h, bytes| framebuffer.apply_raw(x, y, w, h, bytes),
                )
                .unwrap();
                pooled = started.elapsed();
            }
            let bytes: usize = bodies.iter().map(Vec::len).sum();
            println!(
                "{name}: {} rects, {:.0} KB, encode {:.1} ms on {workers} threads (first rectangle {:.1} ms, center {:.1} ms) / {:.1} ms one thread, decode {:.1} ms one thread / {:.1} ms with decoder threads, including framebuffer writes",
                rects.len(),
                bytes as f64 / 1000.0,
                encoded.as_secs_f64() * 1000.0,
                first.as_secs_f64() * 1000.0,
                centered.as_secs_f64() * 1000.0,
                sequential.as_secs_f64() * 1000.0,
                decoded.as_secs_f64() * 1000.0,
                pooled.as_secs_f64() * 1000.0
            );
        }
    }

    #[test]
    fn bands_follow_the_encoder_thread_count() {
        assert_eq!(band_rows(1080, 16), 64);
        assert_eq!(band_rows(1080, 8), 128);
        assert_eq!(band_rows(1080, 4), 256);
        assert_eq!(band_rows(1080, 1), tight::MAX_RECT_HEIGHT);
        assert_eq!(band_rows(40, 16), 64);
        assert_eq!(band_rows(0, 0), 64);
    }

    #[test]
    fn encoder_pool_returns_bands_in_order() {
        let pool = EncodePool::new(4, None).unwrap();
        let settings = TightSettings::default();
        // Bands of different sizes finish out of order.
        let snapshots = (0..12)
            .map(|index| {
                let side = if index % 3 == 0 { 96 } else { 8 };
                (
                    (0..side * side).map(|pixel| pixel * 7 + index).collect(),
                    side as usize,
                    side as usize,
                )
            })
            .collect::<Vec<Snapshot>>();
        let mut order = Vec::new();
        pool.encode(
            snapshots.clone(),
            vec![settings; snapshots.len()],
            |index, body| {
                let (pixels, width, height) = &snapshots[index];
                let mut expected = Vec::new();
                tight::encode_rect(pixels, *width, *height, settings, &mut expected).unwrap();
                assert_eq!(body, expected);
                order.push(index);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(order, (0..12).collect::<Vec<_>>());
        // An error from the writer ends the update; the pool stays usable.
        let failed = pool.encode(
            snapshots.clone(),
            vec![settings; snapshots.len()],
            |_, _| Err(io::Error::other("link closed")),
        );
        assert!(failed.is_err());
        let mut count = 0;
        pool.encode(
            snapshots.clone(),
            vec![settings; snapshots.len()],
            |_, _| {
                count += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(count, 12);
    }

    /// Read one server message: (type, body). Updates return their raw
    /// rectangles' headers; fences return flags and payload.
    enum ServerMessage {
        Update(Vec<(u16, u16, u16, u16)>),
        Fence(u32, Vec<u8>),
        EndOfContinuousUpdates,
    }

    fn read_server_message(client: &mut TcpStream) -> ServerMessage {
        let mut kind = [0];
        client.read_exact(&mut kind).unwrap();
        match kind[0] {
            0 => {
                let mut header = [0; 3];
                client.read_exact(&mut header).unwrap();
                let count = u16::from_be_bytes([header[1], header[2]]);
                let rects = (0..count)
                    .map(|_| {
                        let mut rect = [0; 12];
                        client.read_exact(&mut rect).unwrap();
                        let field =
                            |index: usize| u16::from_be_bytes([rect[index], rect[index + 1]]);
                        assert_eq!(i32::from_be_bytes(rect[8..12].try_into().unwrap()), 0);
                        let mut pixels = vec![0; usize::from(field(4)) * usize::from(field(6)) * 4];
                        client.read_exact(&mut pixels).unwrap();
                        (field(0), field(2), field(4), field(6))
                    })
                    .collect();
                ServerMessage::Update(rects)
            }
            FENCE_MESSAGE => {
                let (flags, payload) = read_fence(client).unwrap();
                ServerMessage::Fence(flags, payload)
            }
            CONTINUOUS_UPDATES_MESSAGE => ServerMessage::EndOfContinuousUpdates,
            other => panic!("unexpected server message {other}"),
        }
    }

    /// Skip probe fences, answering them, and return the next other message.
    fn next_message(client: &mut TcpStream) -> ServerMessage {
        loop {
            match read_server_message(client) {
                ServerMessage::Fence(flags, payload) if payload == FLOW_PROBE => {
                    assert_ne!(flags & FENCE_REQUEST, 0);
                    client
                        .write_all(&fence_message(flags & FENCE_SUPPORTED_FLAGS, &payload))
                        .unwrap();
                }
                message => return message,
            }
        }
    }

    #[test]
    fn continuous_updates_push_changes_and_wait_for_fences() {
        let (server, address) = start_insecure_server(Framebuffer::new(64, 64).unwrap());
        let (mut client, width, height) =
            raw_client(address, &[0, FENCE_ENCODING, CONTINUOUS_UPDATES_ENCODING]);
        // Advertising both pseudo-encodings is answered with
        // EndOfContinuousUpdates, which offers them.
        assert!(matches!(
            read_server_message(&mut client),
            ServerMessage::EndOfContinuousUpdates
        ));
        let mut enable = [CONTINUOUS_UPDATES_MESSAGE, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        enable[6..8].copy_from_slice(&width.to_be_bytes());
        enable[8..10].copy_from_slice(&height.to_be_bytes());
        client.write_all(&enable).unwrap();

        // A change is pushed without a request and followed by a fence.
        let mut frame = Framebuffer::new(64, 64).unwrap();
        frame.pixels_mut()[0] = 0xff0000;
        server.update_framebuffer(&frame).unwrap();
        let ServerMessage::Update(rects) = next_message(&mut client) else {
            panic!("expected a pushed update");
        };
        assert_eq!(rects, vec![(0, 0, 64, 64)]);
        let ServerMessage::Fence(flags, sequence) = next_message(&mut client) else {
            panic!("expected a fence after the update");
        };
        assert_eq!(flags, FENCE_REQUEST | FENCE_BLOCK_BEFORE);
        assert_eq!(sequence.len(), 4);

        // Without a throughput estimate, the next change waits for the
        // fence to be answered.
        frame.pixels_mut()[0] = 0x00ff00;
        server.update_framebuffer(&frame).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut byte = [0];
        assert!(client.peek(&mut byte).is_err());
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .write_all(&fence_message(FENCE_BLOCK_BEFORE, &sequence))
            .unwrap();
        assert!(matches!(
            next_message(&mut client),
            ServerMessage::Update(_)
        ));

        // Disabling continuous updates is confirmed.
        enable[1] = 0;
        client.write_all(&enable).unwrap();
        loop {
            match next_message(&mut client) {
                ServerMessage::EndOfContinuousUpdates => break,
                ServerMessage::Fence(..) => {}
                ServerMessage::Update(_) => panic!("no update without a request"),
            }
        }
        server.stop();
    }

    #[test]
    fn server_answers_client_fences_and_rejects_unoffered_continuous_updates() {
        let (server, address) = start_insecure_server(Framebuffer::new(8, 8).unwrap());
        let (mut client, _, _) = raw_client(address, &[0, FENCE_ENCODING]);
        client
            .write_all(&fence_message(
                FENCE_REQUEST | FENCE_BLOCK_AFTER | (1 << 3),
                b"hi",
            ))
            .unwrap();
        let ServerMessage::Fence(flags, payload) = read_server_message(&mut client) else {
            panic!("expected a fence response");
        };
        // Unknown flags and the request flag are cleared.
        assert_eq!(flags, FENCE_BLOCK_AFTER);
        assert_eq!(payload, b"hi");
        // Continuous updates were not offered (no -313), so enabling them
        // ends the session.
        client
            .write_all(&[CONTINUOUS_UPDATES_MESSAGE, 1, 0, 0, 0, 0, 0, 8, 0, 8])
            .unwrap();
        assert_eq!(client.read(&mut [0; 16]).unwrap_or(0), 0);
        server.stop();
    }

    #[test]
    fn flow_control_paces_updates_from_measured_throughput() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut flow = FlowControl::default();
        assert!(flow.probe_due(at(0)));
        assert_eq!(flow.probing(at(0)), FLOW_PROBE);
        assert!(!flow.probe_due(at(1)));
        // A 10 ms round trip on an idle link.
        flow.acknowledged(&FLOW_PROBE, at(10), at(10));
        assert_eq!(flow.base_delay, Some(Duration::from_millis(10)));
        assert_eq!(flow.window(), SendWindow::Open);

        // 1 MB acknowledged 30 ms after it was sent: 20 ms beyond the round
        // trip, so 50 MB/s.
        let first = flow.sent(1_000_000, at(100), at(100));
        assert_eq!(flow.window(), SendWindow::Closed);
        flow.acknowledged(&first, at(130), at(130));
        assert_eq!(flow.throughput(), Some(50_000_000.0));

        // With one update in flight, the next may go once the link has
        // probably sent it: 1 MB at 95% of 50 MB/s is about 21 ms.
        flow.sent(1_000_000, at(200), at(200));
        let SendWindow::OpensAt(opens) = flow.window() else {
            panic!("expected a timed window");
        };
        let wait = opens - at(200);
        assert!(wait > Duration::from_millis(20) && wait < Duration::from_millis(22));
        // A second update queues behind the first on the link.
        flow.sent(1_000_000, opens, opens);
        flow.sent(1_000_000, opens, opens);
        assert_eq!(flow.window(), SendWindow::Closed);

        // Unknown and malformed acknowledgements are ignored.
        flow.acknowledged(&[1, 2, 3], at(400), at(400));
        flow.acknowledged(&99u32.to_be_bytes(), at(400), at(400));
        assert_eq!(flow.in_flight.len(), 3);
        // Acknowledging the last one clears everything before it.
        let last = flow.in_flight.back().unwrap().sequence.to_be_bytes();
        flow.acknowledged(&last, at(400), at(400));
        assert!(flow.in_flight.is_empty());
        assert_eq!(flow.window(), SendWindow::Open);
        // Probes repeat only on an idle link, once a second.
        assert!(!flow.probe_due(at(900)));
        assert!(flow.probe_due(at(1000)));
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
        let config = ServerConfig {
            password: Some("secret".into()),
            ..ServerConfig::default()
        };
        let session = session_shared(&framebuffer);
        let server_thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            serve_client(&mut stream, &session, &events, &config, &sessions, 7)
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
            let mut handshake = vec![0; 12 + 1 + 1 + 20 + set_encodings(&[0]).len()];
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
            .get_ref()
            .inner
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
        assert_eq!(&server.output[34..], &set_encodings(&[0]));
        assert_eq!(info.security, Security::None);
    }

    #[test]
    fn handshake_advertises_zlib_only_when_selected() {
        let mut server = mock_server();
        negotiate_with_encoding(&mut server, true, Encoding::Zlib, || unreachable!()).unwrap();
        assert_eq!(&server.output[34..], &set_encodings(&[6]));
    }

    /// SetEncodings for `encodings`, followed by the fence, continuous-update,
    /// and pointer pseudo-encodings every mode advertises.
    fn set_encodings(encodings: &[i32]) -> Vec<u8> {
        let all = [
            encodings,
            &[
                FENCE_ENCODING,
                CONTINUOUS_UPDATES_ENCODING,
                POINTER_MOTION_CHANGE_ENCODING,
                EXTENDED_MOUSE_BUTTONS_ENCODING,
            ],
        ]
        .concat();
        let mut message = vec![2, 0];
        message.extend_from_slice(&(all.len() as u16).to_be_bytes());
        for encoding in all {
            message.extend_from_slice(&encoding.to_be_bytes());
        }
        message
    }

    #[test]
    fn handshake_advertises_tight_with_zlib_and_raw_fallbacks() {
        let mut server = mock_server();
        negotiate_with_encoding(
            &mut server,
            true,
            Encoding::Tight { quality: 6 },
            || unreachable!(),
        )
        .unwrap();
        assert_eq!(&server.output[34..], &set_encodings(&[7, 6, 0, -26, -255]));
    }

    #[test]
    fn zlib_rectangles_share_a_stream_across_updates() {
        let mut compressor = Compress::new(ZlibLevel::default(), true);
        let mut decoder = UpdateDecoder::new();
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
        let mut decoder = UpdateDecoder::new();
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
        assert_eq!(
            server.output.len(),
            12 + 1 + 16 + 1 + 20 + set_encodings(&[0]).len()
        );
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
        let (packet, length) = pointer_packet(5, 0x1234, 0xabcd, false);
        assert_eq!(&packet[..length], [5, 5, 0x12, 0x34, 0xab, 0xcd]);
    }

    /// A relative PointerEvent for (`dx`, `dy`).
    fn relative_event(mask: u8, dx: i32, dy: i32) -> [u8; 6] {
        let (x, y) = (
            (RELATIVE_POINTER_ORIGIN + dx) as u16,
            (RELATIVE_POINTER_ORIGIN + dy) as u16,
        );
        let mut message = [5, mask, 0, 0, 0, 0];
        message[2..4].copy_from_slice(&x.to_be_bytes());
        message[4..6].copy_from_slice(&y.to_be_bytes());
        message
    }

    fn pointer_event(server: &VncServer) -> ClientEvent {
        server.recv_event_timeout(Duration::from_secs(2)).unwrap()
    }

    #[test]
    fn server_switches_pointer_modes_and_drops_stale_motion() {
        let (server, address) = start_insecure_server(Framebuffer::new(64, 32).unwrap());
        let (mut client, width, height) = raw_client(address, &[0, POINTER_MOTION_CHANGE_ENCODING]);
        // The mode is announced with the first update the client requests:
        // absolute, QEMU's x = 1, spanning the framebuffer.
        request(&mut client, false, width, height);
        let update = read_rectangles(&mut client);
        assert_eq!(
            update[0],
            (
                1,
                0,
                width,
                height,
                POINTER_MOTION_CHANGE_ENCODING,
                Vec::new()
            )
        );
        assert_eq!(update[1].4, 0);
        client.write_all(&[5, 0, 0, 7, 0, 8]).unwrap();
        let absolute = |buttons, x, y| ClientEvent::Pointer {
            client_id: 1,
            buttons,
            x,
            y,
        };
        let relative = |buttons, dx, dy| ClientEvent::RelativePointer {
            client_id: 1,
            buttons,
            dx,
            dy,
        };
        assert_eq!(pointer_event(&server), absolute(0, 7, 8));

        // A mode change answers a waiting request at once, without pixels.
        request(&mut client, true, width, height);
        server.set_relative_pointer(true);
        assert_eq!(
            read_rectangles(&mut client),
            [(
                0,
                0,
                width,
                height,
                POINTER_MOTION_CHANGE_ENCODING,
                Vec::new()
            )]
        );
        // An absolute event sent before the client switched keeps its
        // button but not its motion.
        client.write_all(&[5, 1, 0, 3, 0, 4]).unwrap();
        assert_eq!(pointer_event(&server), relative(BUTTON_LEFT, 0, 0));
        client.write_all(&relative_event(1, 10, -3)).unwrap();
        assert_eq!(pointer_event(&server), relative(BUTTON_LEFT, 10, -3));
        client
            .write_all(&relative_event(0, MAX_RELATIVE_DELTA, -MAX_RELATIVE_DELTA))
            .unwrap();
        assert_eq!(
            pointer_event(&server),
            relative(0, MAX_RELATIVE_DELTA, -MAX_RELATIVE_DELTA)
        );

        server.set_relative_pointer(false);
        request(&mut client, true, width, height);
        assert_eq!(
            read_rectangles(&mut client),
            [(
                1,
                0,
                width,
                height,
                POINTER_MOTION_CHANGE_ENCODING,
                Vec::new()
            )]
        );
        // A relative event still in flight presses its button where the
        // pointer was, instead of ending the session.
        client.write_all(&relative_event(4, 5, 5)).unwrap();
        assert_eq!(pointer_event(&server), absolute(BUTTON_RIGHT, 7, 8));
        client.write_all(&[5, 0, 0, 9, 0, 9]).unwrap();
        assert_eq!(pointer_event(&server), absolute(0, 9, 9));

        // A client that stops advertising the extension is absolute again,
        // whatever the host wants.
        server.set_relative_pointer(true);
        request(&mut client, true, width, height);
        read_rectangles(&mut client);
        client.write_all(&set_encodings_message(&[0])).unwrap();
        client.write_all(&[5, 0, 0, 1, 0, 2]).unwrap();
        assert_eq!(pointer_event(&server), absolute(0, 1, 2));
        server.stop();
    }

    #[test]
    fn clients_without_the_extension_never_switch_to_relative_motion() {
        let (server, address) = start_insecure_server(Framebuffer::new(8, 8).unwrap());
        let (mut client, _, _) = raw_client(address, &[0]);
        server.set_relative_pointer(true);
        client.write_all(&[5, 0, 0, 2, 0, 3]).unwrap();
        assert_eq!(
            pointer_event(&server),
            ClientEvent::Pointer {
                client_id: 1,
                buttons: 0,
                x: 2,
                y: 3,
            }
        );
        // Bit 7 is the back button for clients without extended events.
        client.write_all(&[5, 0x80, 0, 2, 0, 3]).unwrap();
        assert_eq!(
            pointer_event(&server),
            ClientEvent::Pointer {
                client_id: 1,
                buttons: BUTTON_BACK,
                x: 2,
                y: 3,
            }
        );
        server.stop();
    }

    #[test]
    fn extended_pointer_events_carry_back_and_forward() {
        let (server, address) = start_insecure_server(Framebuffer::new(8, 8).unwrap());
        let (mut client, width, height) =
            raw_client(address, &[0, EXTENDED_MOUSE_BUTTONS_ENCODING]);
        // An empty rectangle at the start of the next requested update
        // acknowledges the extension.
        request(&mut client, false, width, height);
        let update = read_rectangles(&mut client);
        assert_eq!(
            update[0],
            (0, 0, 0, 0, EXTENDED_MOUSE_BUTTONS_ENCODING, Vec::new())
        );
        assert_eq!(update.len(), 2);
        let pointer = |buttons| ClientEvent::Pointer {
            client_id: 1,
            buttons,
            x: 1,
            y: 2,
        };
        // The high bit marks an extended event with one more byte.
        client.write_all(&[5, 0x80 | 4, 0, 1, 0, 2, 0b01]).unwrap();
        assert_eq!(pointer_event(&server), pointer(BUTTON_RIGHT | BUTTON_BACK));
        client.write_all(&[5, 0x80, 0, 1, 0, 2, 0b10]).unwrap();
        assert_eq!(pointer_event(&server), pointer(BUTTON_FORWARD));
        // A normal event releases both.
        client.write_all(&[5, 1, 0, 1, 0, 2]).unwrap();
        assert_eq!(pointer_event(&server), pointer(BUTTON_LEFT));
        server.stop();
    }

    #[test]
    fn input_writer_follows_the_server_pointer_mode() {
        let (local, mut remote) = tcp_pair();
        remote
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let writer = InputWriter::new(local, Encoding::Raw);
        let mut expect = |bytes: &[u8]| {
            let mut received = vec![0; bytes.len()];
            remote.read_exact(&mut received).unwrap();
            assert_eq!(received, bytes);
        };
        writer.pointer(BUTTON_LEFT, 10, 20).unwrap();
        expect(&[5, 1, 0, 10, 0, 20]);
        // Relative motion in absolute mode sends only button changes.
        writer.pointer_motion(BUTTON_LEFT, 5, 5).unwrap();
        writer.pointer_motion(0, 5, 5).unwrap();
        expect(&[5, 0, 0, 10, 0, 20]);

        writer.server_pointer_mode(true);
        assert!(writer.relative_pointer());
        // Absolute positions in relative mode send only button changes.
        writer.pointer(0, 30, 40).unwrap();
        writer.pointer(BUTTON_MIDDLE, 30, 40).unwrap();
        expect(&relative_event(2, 0, 0));
        // Large motion is split into deltas the server accepts.
        writer
            .pointer_motion(BUTTON_MIDDLE, MAX_RELATIVE_DELTA + 7, -2)
            .unwrap();
        expect(&relative_event(2, MAX_RELATIVE_DELTA, -2));
        expect(&relative_event(2, 7, 0));
        // Back and forward need the server's acknowledgement.
        writer.pointer_motion(BUTTON_BACK, 1, 0).unwrap();
        expect(&relative_event(0, 1, 0));
        writer.server_extended_buttons();
        writer
            .pointer_motion(BUTTON_BACK | BUTTON_FORWARD, 0, 1)
            .unwrap();
        let mut extended = relative_event(0x80, 0, 1).to_vec();
        extended.push(0b11);
        expect(&extended);

        // Declining resends SetEncodings without the extension and ignores
        // a switch the server announced before it saw that.
        writer.set_relative_pointer_allowed(false).unwrap();
        assert!(!writer.relative_pointer());
        let declined = Encoding::Raw
            .advertised()
            .into_iter()
            .filter(|encoding| *encoding != POINTER_MOTION_CHANGE_ENCODING)
            .collect::<Vec<_>>();
        expect(&set_encodings_message(&declined));
        writer.server_pointer_mode(true);
        assert!(!writer.relative_pointer());
        writer.pointer(0, 1, 2).unwrap();
        expect(&[5, 0, 0, 1, 0, 2]);
        writer.set_relative_pointer_allowed(true).unwrap();
        expect(&set_encodings_message(&Encoding::Raw.advertised()));
    }

    #[test]
    fn input_writer_switches_encodings_on_the_same_connection() {
        let (local, mut remote) = tcp_pair();
        remote
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let writer = InputWriter::new(local, Encoding::Raw);
        let mut expect = |encodings: &[i32]| {
            let bytes = set_encodings_message(encodings);
            let mut received = vec![0; bytes.len()];
            remote.read_exact(&mut received).unwrap();
            assert_eq!(received, bytes);
        };
        assert!(writer.accepts(0));
        assert!(!writer.accepts(6) && !writer.accepts(tight::TIGHT_ENCODING));
        let tight = Encoding::Tight { quality: 3 };
        writer.set_encoding(tight).unwrap();
        expect(&tight.advertised());
        assert!(writer.accepts(6) && writer.accepts(tight::TIGHT_ENCODING));
        // Another quality level is another SetEncodings.
        let sharper = Encoding::Tight { quality: 8 };
        writer.set_encoding(sharper).unwrap();
        expect(&sharper.advertised());
        // Updates the server began before the switch back may still be Tight.
        writer.set_encoding(Encoding::Raw).unwrap();
        expect(&Encoding::Raw.advertised());
        assert!(writer.accepts(tight::TIGHT_ENCODING));
        assert!(!writer.accepts(16));

        // Choosing the current encoding sends nothing, and declined relative
        // motion stays declined across a switch.
        writer.set_encoding(Encoding::Raw).unwrap();
        writer.set_relative_pointer_allowed(false).unwrap();
        let without_motion = |encoding: Encoding| {
            encoding
                .advertised()
                .into_iter()
                .filter(|encoding| *encoding != POINTER_MOTION_CHANGE_ENCODING)
                .collect::<Vec<_>>()
        };
        expect(&without_motion(Encoding::Raw));
        writer.set_encoding(Encoding::Zlib).unwrap();
        expect(&without_motion(Encoding::Zlib));
    }

    #[test]
    fn topvnc_session_sends_relative_motion_when_the_server_asks() {
        let (server, address) = start_insecure_server(Framebuffer::new(16, 16).unwrap());
        let mut session = Session::connect(&address.to_string(), true, || unreachable!()).unwrap();
        let writer = session.writer();
        writer.request_update(false, 16, 16).unwrap();
        let reader = std::thread::spawn(move || {
            let mut scratch = Vec::new();
            // Runs until the server stops.
            while session
                .read_update_pipelined(&mut scratch, |_, _, _, _, _| Ok(()))
                .is_ok()
            {}
        });
        let wait_for = |relative: bool| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while writer.relative_pointer() != relative {
                assert!(Instant::now() < deadline, "the pointer mode never changed");
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        server.set_relative_pointer(true);
        wait_for(true);
        writer.pointer_motion(0, 7, -4).unwrap();
        assert_eq!(
            pointer_event(&server),
            ClientEvent::RelativePointer {
                client_id: 1,
                buttons: 0,
                dx: 7,
                dy: -4,
            }
        );
        // TopVNC's server acknowledges extended buttons at once.
        writer.pointer_motion(BUTTON_FORWARD, 0, 0).unwrap();
        assert_eq!(
            pointer_event(&server),
            ClientEvent::RelativePointer {
                client_id: 1,
                buttons: BUTTON_FORWARD,
                dx: 0,
                dy: 0,
            }
        );
        server.set_relative_pointer(false);
        wait_for(false);
        writer.pointer(0, 3, 4).unwrap();
        assert_eq!(
            pointer_event(&server),
            ClientEvent::Pointer {
                client_id: 1,
                buttons: 0,
                x: 3,
                y: 4,
            }
        );
        server.stop();
        reader.join().unwrap();
    }
}
