//! Shared RFB protocol and framebuffer code for TopVNC.

#![forbid(unsafe_code)]

use des::Des;
use des::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_DIMENSION: u16 = 8192;
const MAX_PIXELS: usize = 33_554_432;
const MAX_NAME_BYTES: usize = 4096;
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

/// Negotiate an RFB session. `None` security requires explicit consent.
pub fn negotiate(
    stream: &mut (impl Read + Write),
    allow_insecure: bool,
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
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || pixels > MAX_PIXELS
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
    // Raw is the only advertised encoding.
    stream.write_all(&[2, 0, 0, 1, 0, 0, 0, 0])?;
    Ok(ServerInfo {
        width,
        height,
        name: String::from_utf8_lossy(&name).into_owned(),
        security,
    })
}

#[derive(Debug)]
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
            || width > MAX_DIMENSION
            || height > MAX_DIMENSION
            || pixels > MAX_PIXELS
        {
            return Err(invalid("framebuffer dimensions exceed limits"));
        }
        Ok(Self {
            width,
            height,
            pixels: vec![0; pixels],
        })
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
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
                    let encoding = i32::from_be_bytes(rect[8..12].try_into().unwrap());
                    if encoding != 0 {
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
                    reader.read_exact(scratch)?;
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
}

impl Session {
    pub fn connect(
        address: &str,
        allow_insecure: bool,
        password_prompt: impl FnMut() -> io::Result<String>,
    ) -> io::Result<Self> {
        Self::from_stream(
            connect_tcp(address)?,
            allow_insecure,
            password_prompt,
            CONNECT_TIMEOUT,
        )
    }

    fn from_stream(
        mut stream: TcpStream,
        allow_insecure: bool,
        password_prompt: impl FnMut() -> io::Result<String>,
        timeout: Duration,
    ) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        let info = negotiate(
            &mut HandshakeStream {
                stream: &mut stream,
                deadline: Instant::now() + timeout,
            },
            allow_insecure,
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
        read_update_with(
            &mut reader,
            self.info.width,
            self.info.height,
            scratch,
            apply,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
