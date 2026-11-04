//! The client side of the RFB handshake as a push state machine, plus the
//! blocking [`negotiate_with_encoding`] the native viewer uses over TCP.

use crate::messages::{PIXEL_FORMAT_MESSAGE, set_encodings_message, vnc_auth_response};
use crate::protocol::{MAX_FRAMEBUFFER_DIMENSION, MAX_FRAMEBUFFER_PIXELS, MAX_NAME_BYTES};
use crate::{Encoding, Security, ServerInfo, invalid};
use std::io::{self, Read, Write};

/// A server never needs to send more than one handshake step ahead.
const MAX_HANDSHAKE_BUFFER: usize = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Version {
    V3_3,
    V3_7,
    V3_8,
}

#[derive(Clone, Copy)]
enum Reason {
    Rejected,
    AuthenticationFailed,
}

enum State {
    Version,
    /// RFB 3.3: the server chooses the security type.
    SecurityType,
    SecurityCount,
    SecurityTypes(usize),
    Reason(Reason),
    Challenge,
    Password([u8; 16]),
    SecurityResult,
    ServerInit,
    Done,
    Failed,
}

/// What the handshake needs next.
#[derive(Debug)]
pub enum HandshakeStatus {
    /// More server bytes; see [`ClientHandshake::needed`].
    NeedMore,
    /// The VNC password, for [`ClientHandshake::password`].
    NeedPassword,
    Done(ServerInfo),
}

/// Negotiates an RFB session from received bytes. Bytes to send accumulate
/// in [`ClientHandshake::take_output`]. `None` security requires explicit
/// consent; there is no silent downgrade.
pub struct ClientHandshake {
    state: State,
    input: Vec<u8>,
    output: Vec<u8>,
    allow_insecure: bool,
    encoding: Encoding,
    protocol: Version,
    security: Security,
}

impl ClientHandshake {
    pub fn new(allow_insecure: bool, encoding: Encoding) -> Self {
        Self {
            state: State::Version,
            input: Vec::new(),
            output: Vec::new(),
            allow_insecure,
            encoding,
            protocol: Version::V3_8,
            security: Security::None,
        }
    }

    /// Bytes to send to the server, in order.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }

    /// Received bytes after ServerInit: the start of the session's message
    /// stream.
    pub fn into_leftover(self) -> Vec<u8> {
        match self.state {
            State::Done => self.input,
            _ => Vec::new(),
        }
    }

    /// How many more bytes the next step needs, at least one. Reading exactly
    /// this many never reads past the handshake.
    pub fn needed(&self) -> usize {
        let have = self.input.len();
        let total = match self.state {
            State::Version => 12,
            State::SecurityType | State::SecurityResult => 4,
            State::SecurityCount => 1,
            State::SecurityTypes(count) => count,
            State::Reason(_) => match self.input.get(..4) {
                Some(length) => 4 + u32::from_be_bytes(length.try_into().unwrap()) as usize,
                None => 4,
            },
            State::Challenge => 16,
            State::ServerInit => match self.input.get(20..24) {
                Some(length) => 24 + u32::from_be_bytes(length.try_into().unwrap()) as usize,
                None => 24,
            },
            State::Password(_) | State::Done | State::Failed => return 0,
        };
        total.saturating_sub(have).max(1)
    }

    /// Add received bytes and advance as far as they allow.
    pub fn push(&mut self, bytes: &[u8]) -> io::Result<HandshakeStatus> {
        if matches!(self.state, State::Done | State::Failed) {
            return Err(invalid("handshake already finished"));
        }
        if self.input.len() + bytes.len() > MAX_HANDSHAKE_BUFFER {
            self.state = State::Failed;
            return Err(invalid("server sent too much handshake data"));
        }
        self.input.extend_from_slice(bytes);
        self.advance().inspect_err(|_| self.state = State::Failed)
    }

    /// Answer the server's VNC authentication challenge.
    pub fn password(&mut self, password: &str) -> io::Result<HandshakeStatus> {
        let State::Password(challenge) = self.state else {
            return Err(invalid("the server did not ask for a password"));
        };
        self.output
            .extend_from_slice(&vnc_auth_response(&challenge, password));
        self.state = State::SecurityResult;
        self.advance().inspect_err(|_| self.state = State::Failed)
    }

    /// Remove and return the first `count` received bytes, if present.
    fn take(&mut self, count: usize) -> Option<Vec<u8>> {
        (self.input.len() >= count).then(|| self.input.drain(..count).collect())
    }

    fn client_init(&mut self) {
        self.output.push(1); // ClientInit: share desktop.
        self.state = State::ServerInit;
    }

    /// After the security type is known.
    fn security_selected(&mut self, security: Security) {
        self.security = security;
        if security == Security::VncPassword {
            self.state = State::Challenge;
        } else if self.protocol == Version::V3_8 {
            self.state = State::SecurityResult;
        } else {
            self.client_init();
        }
    }

    fn advance(&mut self) -> io::Result<HandshakeStatus> {
        loop {
            match self.state {
                State::Version => {
                    let Some(version) = self.take(12) else {
                        return Ok(HandshakeStatus::NeedMore);
                    };
                    let (protocol, reply) = match version.as_slice() {
                        b"RFB 003.003\n" => (Version::V3_3, b"RFB 003.003\n".as_slice()),
                        b"RFB 003.007\n" => (Version::V3_7, b"RFB 003.007\n".as_slice()),
                        b"RFB 003.008\n" | b"RFB 003.889\n" => {
                            (Version::V3_8, b"RFB 003.008\n".as_slice())
                        }
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
                    self.protocol = protocol;
                    self.output.extend_from_slice(reply);
                    self.state = if protocol == Version::V3_3 {
                        State::SecurityType
                    } else {
                        State::SecurityCount
                    };
                }
                State::SecurityType => {
                    let Some(bytes) = self.take(4) else {
                        return Ok(HandshakeStatus::NeedMore);
                    };
                    match u32::from_be_bytes(bytes.try_into().unwrap()) {
                        1 if self.allow_insecure => self.security_selected(Security::None),
                        1 => {
                            return Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "server offers only None security; pass --allow-insecure to accept it",
                            ));
                        }
                        2 => self.security_selected(Security::VncPassword),
                        0 => self.state = State::Reason(Reason::Rejected),
                        other => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("unsupported RFB 3.3 security type: {other}"),
                            ));
                        }
                    }
                }
                State::SecurityCount => {
                    let Some(count) = self.take(1) else {
                        return Ok(HandshakeStatus::NeedMore);
                    };
                    self.state = match count[0] {
                        0 => State::Reason(Reason::Rejected),
                        count => State::SecurityTypes(usize::from(count)),
                    };
                }
                State::SecurityTypes(count) => {
                    let Some(types) = self.take(count) else {
                        return Ok(HandshakeStatus::NeedMore);
                    };
                    let selected = if types.contains(&2) {
                        Security::VncPassword
                    } else if self.allow_insecure && types.contains(&1) {
                        Security::None
                    } else {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            format!(
                                "no supported security type among {types:?}; VNC password (2) is supported, and None (1) requires --allow-insecure"
                            ),
                        ));
                    };
                    self.output.push(match selected {
                        Security::None => 1,
                        Security::VncPassword => 2,
                    });
                    self.security_selected(selected);
                }
                State::Reason(reason) => {
                    let Some(length) = self.input.get(..4) else {
                        return Ok(HandshakeStatus::NeedMore);
                    };
                    let length = u32::from_be_bytes(length.try_into().unwrap()) as usize;
                    if length > MAX_NAME_BYTES {
                        return Err(invalid("server rejection reason is too long"));
                    }
                    let Some(text) = self.take(4 + length) else {
                        return Ok(HandshakeStatus::NeedMore);
                    };
                    let text = String::from_utf8_lossy(&text[4..]).into_owned();
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        match reason {
                            Reason::Rejected => format!("server rejected connection: {text}"),
                            Reason::AuthenticationFailed => text,
                        },
                    ));
                }
                State::Challenge => {
                    let Some(challenge) = self.take(16) else {
                        return Ok(HandshakeStatus::NeedMore);
                    };
                    self.state = State::Password(challenge.try_into().unwrap());
                    return Ok(HandshakeStatus::NeedPassword);
                }
                State::Password(_) => return Ok(HandshakeStatus::NeedPassword),
                State::SecurityResult => {
                    let Some(result) = self.take(4) else {
                        return Ok(HandshakeStatus::NeedMore);
                    };
                    if u32::from_be_bytes(result.try_into().unwrap()) == 0 {
                        self.client_init();
                    } else if self.protocol == Version::V3_8 {
                        self.state = State::Reason(Reason::AuthenticationFailed);
                    } else {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "authentication failed",
                        ));
                    }
                }
                State::ServerInit => {
                    let Some(header) = self.input.get(..24) else {
                        return Ok(HandshakeStatus::NeedMore);
                    };
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
                    let name_length =
                        u32::from_be_bytes(header[20..24].try_into().unwrap()) as usize;
                    if name_length > MAX_NAME_BYTES {
                        return Err(invalid("server name is too long"));
                    }
                    let Some(init) = self.take(24 + name_length) else {
                        return Ok(HandshakeStatus::NeedMore);
                    };
                    self.output.extend_from_slice(&PIXEL_FORMAT_MESSAGE);
                    self.output
                        .extend_from_slice(&set_encodings_message(&self.encoding.advertised()));
                    self.state = State::Done;
                    return Ok(HandshakeStatus::Done(ServerInfo {
                        width,
                        height,
                        name: String::from_utf8_lossy(&init[24..]).into_owned(),
                        security: self.security,
                    }));
                }
                State::Done | State::Failed => {
                    return Err(invalid("handshake already finished"));
                }
            }
        }
    }
}

/// Negotiate an RFB session. `None` security requires explicit consent.
pub fn negotiate(
    stream: &mut (impl Read + Write),
    allow_insecure: bool,
    password_prompt: impl FnMut() -> io::Result<String>,
) -> io::Result<ServerInfo> {
    negotiate_with_encoding(stream, allow_insecure, Encoding::Raw, password_prompt)
}

/// Negotiate over a blocking stream, reading exactly the handshake's bytes
/// so the stream is positioned at the first server message afterwards.
pub fn negotiate_with_encoding(
    stream: &mut (impl Read + Write),
    allow_insecure: bool,
    encoding: Encoding,
    mut password_prompt: impl FnMut() -> io::Result<String>,
) -> io::Result<ServerInfo> {
    let mut handshake = ClientHandshake::new(allow_insecure, encoding);
    let mut bytes = Vec::new();
    let mut status = HandshakeStatus::NeedMore;
    loop {
        let output = handshake.take_output();
        if !output.is_empty() {
            stream.write_all(&output)?;
        }
        status = match status {
            HandshakeStatus::Done(info) => return Ok(info),
            HandshakeStatus::NeedPassword => handshake.password(&password_prompt()?)?,
            HandshakeStatus::NeedMore => {
                bytes.resize(handshake.needed(), 0);
                stream.read_exact(&mut bytes)?;
                handshake.push(&bytes)?
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_init(width: u16, height: u16, name: &[u8]) -> Vec<u8> {
        let mut init = Vec::new();
        init.extend_from_slice(&width.to_be_bytes());
        init.extend_from_slice(&height.to_be_bytes());
        init.extend_from_slice(&[0; 16]);
        init.extend_from_slice(&(name.len() as u32).to_be_bytes());
        init.extend_from_slice(name);
        init
    }

    fn password_server() -> Vec<u8> {
        let mut server = b"RFB 003.008\n".to_vec();
        server.extend_from_slice(&[1, 2]);
        server.extend_from_slice(&[7; 16]);
        server.extend_from_slice(&0u32.to_be_bytes());
        server.extend_from_slice(&server_init(640, 480, b"desk"));
        server
    }

    /// Feed `server` in chunks of `chunk` bytes, answering the password.
    fn run(server: &[u8], chunk: usize, allow_insecure: bool) -> io::Result<(ServerInfo, Vec<u8>)> {
        let mut handshake = ClientHandshake::new(allow_insecure, Encoding::Tight { quality: 6 });
        let mut sent = Vec::new();
        let mut chunks = server.chunks(chunk);
        let mut status = HandshakeStatus::NeedMore;
        loop {
            sent.extend(handshake.take_output());
            status = match status {
                HandshakeStatus::Done(info) => return Ok((info, sent)),
                HandshakeStatus::NeedPassword => handshake.password("secret")?,
                HandshakeStatus::NeedMore => match chunks.next() {
                    Some(bytes) => handshake.push(bytes)?,
                    None => return Err(io::ErrorKind::UnexpectedEof.into()),
                },
            };
        }
    }

    #[test]
    fn chunk_boundaries_do_not_change_the_handshake() {
        let server = password_server();
        let (expected, expected_sent) = run(&server, server.len(), false).unwrap();
        assert_eq!((expected.width, expected.height), (640, 480));
        assert_eq!(expected.name, "desk");
        assert_eq!(expected.security, Security::VncPassword);
        let mut reply = b"RFB 003.008\n".to_vec();
        reply.push(2);
        reply.extend_from_slice(&vnc_auth_response(&[7; 16], "secret"));
        reply.push(1);
        assert!(expected_sent.starts_with(&reply));
        for chunk in 1..server.len() {
            let (info, sent) = run(&server, chunk, false).unwrap();
            assert_eq!(
                (info.width, info.height, info.name.as_str()),
                (640, 480, "desk")
            );
            assert_eq!(sent, expected_sent, "chunk size {chunk}");
        }
    }

    #[test]
    fn bytes_after_server_init_are_kept_for_the_session() {
        let mut server = password_server();
        server.extend_from_slice(&[2, 2]); // Two Bell messages.
        let mut handshake = ClientHandshake::new(false, Encoding::Raw);
        let status = handshake.push(&server).unwrap();
        assert!(matches!(status, HandshakeStatus::NeedPassword));
        assert!(matches!(
            handshake.password("secret").unwrap(),
            HandshakeStatus::Done(_)
        ));
        assert_eq!(handshake.into_leftover(), [2, 2]);
    }

    #[test]
    fn none_security_is_refused_without_consent() {
        let mut server = b"RFB 003.008\n".to_vec();
        server.extend_from_slice(&[1, 1]);
        let error = run(&server, 3, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        // RFB 3.3, where the server picks None.
        let mut server = b"RFB 003.003\n".to_vec();
        server.extend_from_slice(&1u32.to_be_bytes());
        assert_eq!(
            run(&server, 1, false).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        // With consent, 3.8 None still reads the security result.
        let mut server = b"RFB 003.008\n".to_vec();
        server.extend_from_slice(&[1, 1]);
        server.extend_from_slice(&0u32.to_be_bytes());
        server.extend_from_slice(&server_init(8, 8, b""));
        let (info, _) = run(&server, 5, true).unwrap();
        assert_eq!(info.security, Security::None);
    }

    #[test]
    fn rejections_and_malformed_handshakes_fail_closed() {
        let failed = |server: &[u8]| run(server, 7, false).unwrap_err();
        // Authentication failure with a reason.
        let mut server = b"RFB 003.008\n".to_vec();
        server.extend_from_slice(&[1, 2]);
        server.extend_from_slice(&[0; 16]);
        server.extend_from_slice(&1u32.to_be_bytes());
        server.extend_from_slice(&5u32.to_be_bytes());
        server.extend_from_slice(b"wrong");
        let error = failed(&server);
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "wrong");
        // A server that refuses the connection.
        let mut server = b"RFB 003.008\n".to_vec();
        server.push(0);
        server.extend_from_slice(&4u32.to_be_bytes());
        server.extend_from_slice(b"busy");
        assert_eq!(
            failed(&server).to_string(),
            "server rejected connection: busy"
        );
        // An oversized reason is rejected before it is buffered.
        let mut server = b"RFB 003.008\n".to_vec();
        server.push(0);
        server.extend_from_slice(&(MAX_NAME_BYTES as u32 + 1).to_be_bytes());
        assert_eq!(failed(&server).kind(), io::ErrorKind::InvalidData);
        // Bad banner, oversized framebuffer, and oversized name.
        assert_eq!(failed(b"HTTP/1.1 200").kind(), io::ErrorKind::InvalidData);
        for init in [
            server_init(0, 10, b""),
            server_init(MAX_FRAMEBUFFER_DIMENSION + 1, 10, b""),
        ] {
            let mut server = password_server();
            let start = server.len() - 28;
            server.truncate(start);
            server.extend_from_slice(&init);
            assert_eq!(failed(&server).kind(), io::ErrorKind::InvalidData);
        }
        let mut server = password_server();
        let length = server.len() - 8;
        server[length..length + 4].copy_from_slice(&(MAX_NAME_BYTES as u32 + 1).to_be_bytes());
        assert_eq!(failed(&server).kind(), io::ErrorKind::InvalidData);
        // Too much data at once.
        let mut handshake = ClientHandshake::new(false, Encoding::Raw);
        assert!(handshake.push(&vec![0; MAX_HANDSHAKE_BUFFER + 1]).is_err());
        // A failed handshake stays failed.
        assert!(handshake.push(b"RFB 003.008\n").is_err());
    }

    #[test]
    fn needed_never_reads_past_the_handshake() {
        let server = password_server();
        let mut handshake = ClientHandshake::new(false, Encoding::Raw);
        let mut position = 0;
        loop {
            let needed = handshake.needed();
            let status = handshake
                .push(&server[position..position + needed])
                .unwrap();
            position += needed;
            match status {
                HandshakeStatus::NeedMore => {}
                HandshakeStatus::NeedPassword => {
                    assert!(matches!(
                        handshake.password("x").unwrap(),
                        HandshakeStatus::NeedMore
                    ));
                }
                HandshakeStatus::Done(_) => break,
            }
        }
        assert_eq!(position, server.len());
    }
}
