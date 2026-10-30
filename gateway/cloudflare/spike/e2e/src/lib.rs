//! Browser-to-host end-to-end channel shared by the Phase 0 gateway and browser.
//!
//! Noise XK (`snow`, pure-Rust resolver): the browser pins the host's static key
//! from a pairing invite read at the host, and the host accepts only enrolled
//! browser keys. The first connection uses XKpsk3 with the invite's one-time
//! 256-bit secret, which authenticates the browser key being enrolled. Each
//! WebSocket binary message carries exactly one Noise message, so the relay
//! sees only ciphertext. Nonces are implicit and sequential: a dropped,
//! reordered, replayed, or altered message fails authentication and the
//! channel stays closed. Cloudflare still serves the browser code; this channel
//! does not protect against substituted client code.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use blake2::{Blake2s256, Digest};
use snow::{
    Builder, HandshakeState, TransportState,
    params::{DHChoice, NoiseParams},
    resolvers::{CryptoResolver, DefaultResolver},
};

pub const KEY_BYTES: usize = 32;
pub const TAG_BYTES: usize = 16;
/// Largest plaintext carried by one transport message.
pub const MAX_PLAINTEXT_BYTES: usize = 16 * 1024;
pub const MAX_CIPHERTEXT_BYTES: usize = MAX_PLAINTEXT_BYTES + TAG_BYTES;
/// Handshake messages carry empty payloads, so their sizes are fixed:
/// version/mode header + e + tag; e + tag; encrypted s + tag.
pub const FIRST_MESSAGE_BYTES: usize = 2 + KEY_BYTES + TAG_BYTES;
pub const SECOND_MESSAGE_BYTES: usize = KEY_BYTES + TAG_BYTES;
pub const THIRD_MESSAGE_BYTES: usize = KEY_BYTES + 2 * TAG_BYTES;
const MAX_INVITE_CHARS: usize = 128;
const VERSION: u8 = 1;
const PROLOGUE: &[u8] = b"TopVNC browser E2E v1";
const SESSION_PATTERN: &str = "Noise_XK_25519_ChaChaPoly_BLAKE2s";
const PAIRING_PATTERN: &str = "Noise_XKpsk3_25519_ChaChaPoly_BLAKE2s";

pub type Key = [u8; KEY_BYTES];
pub type E2eResult<T> = Result<T, &'static str>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// An enrolled browser reconnects.
    Session = 1,
    /// A browser presents a one-time invite secret to enroll its key.
    Pairing = 2,
}

pub struct Keypair {
    pub private: Key,
    pub public: Key,
}

impl Keypair {
    pub fn generate() -> E2eResult<Self> {
        let mut private = [0; KEY_BYTES];
        getrandom::fill(&mut private).map_err(|_| "random number generator unavailable")?;
        Self::from_private(private)
    }

    pub fn from_private(private: Key) -> E2eResult<Self> {
        let mut dh = DefaultResolver
            .resolve_dh(&DHChoice::Curve25519)
            .ok_or("X25519 unavailable")?;
        dh.set(&private);
        Ok(Self {
            private,
            public: to_key(dh.pubkey())?,
        })
    }
}

pub fn encode_key(key: &Key) -> String {
    URL_SAFE_NO_PAD.encode(key)
}

pub fn decode_key(text: &str) -> E2eResult<Key> {
    if text.len() != 43 {
        return Err("invalid key encoding");
    }
    to_key(
        &URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| "invalid key encoding")?,
    )
}

/// A short, non-secret label for comparing public keys across devices.
pub fn fingerprint(public: &Key) -> String {
    let digest = Blake2s256::new()
        .chain_update(b"TopVNC key fingerprint v1")
        .chain_update(public)
        .finalize();
    digest[..8]
        .chunks(2)
        .map(|pair| format!("{:02x}{:02x}", pair[0], pair[1]))
        .collect::<Vec<_>>()
        .join("-")
}

fn to_key(bytes: &[u8]) -> E2eResult<Key> {
    bytes.try_into().map_err(|_| "invalid key length")
}

/// Shown only at the host, for example as a QR code; never sent through the relay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingInvite {
    pub host: Key,
    pub secret: Key,
}

impl PairingInvite {
    pub fn generate(host: Key) -> E2eResult<Self> {
        let mut secret = [0; KEY_BYTES];
        getrandom::fill(&mut secret).map_err(|_| "random number generator unavailable")?;
        Ok(Self { host, secret })
    }

    pub fn encode(&self) -> String {
        format!("v1.{}.{}", encode_key(&self.host), encode_key(&self.secret))
    }

    pub fn decode(text: &str) -> E2eResult<Self> {
        if text.len() > MAX_INVITE_CHARS {
            return Err("invalid pairing invite");
        }
        let mut parts = text.split('.');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some("v1"), Some(host), Some(secret), None) => Ok(Self {
                host: decode_key(host)?,
                secret: decode_key(secret)?,
            }),
            _ => Err("invalid pairing invite"),
        }
    }
}

fn builder<'a>(
    mode: Mode,
    prologue: &'a [u8],
    local_private: &'a Key,
    secret: Option<&'a Key>,
) -> E2eResult<Builder<'a>> {
    let pattern = match mode {
        Mode::Session => SESSION_PATTERN,
        Mode::Pairing => PAIRING_PATTERN,
    };
    let params: NoiseParams = pattern.parse().map_err(|_| "unsupported Noise pattern")?;
    let builder = Builder::new(params)
        .local_private_key(local_private)
        .and_then(|b| b.prologue(prologue))
        .map_err(|_| "invalid handshake parameters")?;
    match (mode, secret) {
        (Mode::Session, None) => Ok(builder),
        (Mode::Pairing, Some(secret)) => {
            builder.psk(3, secret).map_err(|_| "invalid pairing secret")
        }
        _ => Err("pairing secret does not match handshake mode"),
    }
}

/// Binds the cleartext version/mode header into the handshake transcript.
fn prologue(mode: Mode) -> Vec<u8> {
    [PROLOGUE, &[VERSION, mode as u8]].concat()
}

/// Browser side. Pins the host key before sending anything.
pub struct Initiator {
    state: HandshakeState,
}

impl Initiator {
    /// Returns the first handshake message, including its version/mode header.
    pub fn start(
        browser_private: &Key,
        host_public: &Key,
        pairing_secret: Option<&Key>,
    ) -> E2eResult<(Self, Vec<u8>)> {
        let mode = if pairing_secret.is_some() {
            Mode::Pairing
        } else {
            Mode::Session
        };
        let prologue = prologue(mode);
        let mut state = builder(mode, &prologue, browser_private, pairing_secret)?
            .remote_public_key(host_public)
            .and_then(Builder::build_initiator)
            .map_err(|_| "invalid host key")?;
        let mut message = vec![0; FIRST_MESSAGE_BYTES];
        message[0] = VERSION;
        message[1] = mode as u8;
        let written = state
            .write_message(&[], &mut message[2..])
            .map_err(|_| "handshake failed")?;
        if written + 2 != FIRST_MESSAGE_BYTES {
            return Err("handshake failed");
        }
        Ok((Self { state }, message))
    }

    /// Verifies the host's reply and returns the final handshake message and
    /// the channel. Fails when the host does not hold the pinned key.
    pub fn finish(mut self, reply: &[u8]) -> E2eResult<(Vec<u8>, Channel)> {
        if reply.len() != SECOND_MESSAGE_BYTES {
            return Err("unexpected handshake message");
        }
        let mut payload = [0; SECOND_MESSAGE_BYTES];
        if self.state.read_message(reply, &mut payload) != Ok(0) {
            return Err("host identity verification failed");
        }
        let mut message = vec![0; THIRD_MESSAGE_BYTES];
        if self.state.write_message(&[], &mut message) != Ok(THIRD_MESSAGE_BYTES) {
            return Err("handshake failed");
        }
        Ok((message, Channel::new(self.state)?))
    }
}

/// Host side. The caller must authorize the returned browser key.
pub struct Responder {
    state: HandshakeState,
    mode: Mode,
}

impl Responder {
    /// `pairing_secret` is the host's currently valid invite secret, if any.
    pub fn accept(
        host_private: &Key,
        first: &[u8],
        pairing_secret: Option<&Key>,
    ) -> E2eResult<(Self, Vec<u8>)> {
        if first.len() != FIRST_MESSAGE_BYTES || first[0] != VERSION {
            return Err("unsupported E2E handshake");
        }
        let (mode, secret) = match first[1] {
            1 => (Mode::Session, None),
            2 => (
                Mode::Pairing,
                Some(pairing_secret.ok_or("no pairing invite is active")?),
            ),
            _ => return Err("unsupported E2E handshake"),
        };
        let prologue = prologue(mode);
        let mut state = builder(mode, &prologue, host_private, secret)?
            .build_responder()
            .map_err(|_| "invalid host key")?;
        let mut payload = [0; FIRST_MESSAGE_BYTES];
        if state.read_message(&first[2..], &mut payload) != Ok(0) {
            return Err("E2E handshake failed");
        }
        let mut reply = vec![0; SECOND_MESSAGE_BYTES];
        if state.write_message(&[], &mut reply) != Ok(SECOND_MESSAGE_BYTES) {
            return Err("E2E handshake failed");
        }
        Ok((Self { state, mode }, reply))
    }

    /// Returns the channel, the authenticated browser key, and the mode. In
    /// pairing mode, success also proves knowledge of the invite secret.
    pub fn finish(mut self, last: &[u8]) -> E2eResult<(Channel, Key, Mode)> {
        if last.len() != THIRD_MESSAGE_BYTES {
            return Err("unexpected handshake message");
        }
        let mut payload = [0; THIRD_MESSAGE_BYTES];
        if self.state.read_message(last, &mut payload) != Ok(0) {
            return Err("E2E handshake failed");
        }
        let browser = to_key(
            self.state
                .get_remote_static()
                .ok_or("missing browser key")?,
        )?;
        Ok((Channel::new(self.state)?, browser, self.mode))
    }
}

/// An established channel. Any failure poisons it permanently.
pub struct Channel {
    state: Option<TransportState>,
}

impl Channel {
    fn new(state: HandshakeState) -> E2eResult<Self> {
        Ok(Self {
            state: Some(
                state
                    .into_transport_mode()
                    .map_err(|_| "handshake incomplete")?,
            ),
        })
    }

    pub fn seal(&mut self, plaintext: &[u8]) -> E2eResult<Vec<u8>> {
        let state = self.state.as_mut().ok_or("E2E channel closed")?;
        if plaintext.is_empty() || plaintext.len() > MAX_PLAINTEXT_BYTES {
            self.state = None;
            return Err("invalid E2E plaintext length");
        }
        let mut message = vec![0; plaintext.len() + TAG_BYTES];
        if state.write_message(plaintext, &mut message) != Ok(message.len()) {
            self.state = None;
            return Err("E2E encryption failed");
        }
        Ok(message)
    }

    pub fn open(&mut self, message: &[u8]) -> E2eResult<Vec<u8>> {
        let state = self.state.as_mut().ok_or("E2E channel closed")?;
        if message.len() <= TAG_BYTES || message.len() > MAX_CIPHERTEXT_BYTES {
            self.state = None;
            return Err("invalid E2E message length");
        }
        let mut plaintext = vec![0; message.len() - TAG_BYTES];
        if state.read_message(message, &mut plaintext) != Ok(plaintext.len()) {
            self.state = None;
            return Err("E2E integrity check failed");
        }
        Ok(plaintext)
    }
}

#[cfg(test)]
mod tests;
