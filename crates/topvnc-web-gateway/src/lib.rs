//! Opt-in host gateway for TopVNC's browser client. It accepts WebSocket
//! upgrades that Cloudflare Access authenticated, terminates the browser's
//! Noise E2E channel on this host, and only then connects to one fixed
//! loopback target: the TopVNC server, where the browser still has to pass
//! VNC password authentication. Cloudflare's Worker, VPC service, and Tunnel
//! carry only ciphertext. The gateway never interprets RFB and never takes a
//! destination from the browser.

use axum::{
    Router,
    extract::{
        FromRequestParts, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::any,
};
use serde::Deserialize;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, Notify, Semaphore},
    time::{Instant, timeout, timeout_at},
};
use topvnc_access::{
    AccessPolicy, Identity, KEY_REFRESH_SECONDS, KEY_TTL_SECONDS, KeySet, MAX_JWKS_BYTES,
    READ_CHUNK_BYTES, token_key_id, validate_origin, validate_upgrade,
};
use topvnc_e2e::{
    self as e2e, Channel, Key, Keypair, MAX_CIPHERTEXT_BYTES, MAX_PLAINTEXT_BYTES, Mode,
    PairingInvite, Responder,
};

pub const MAX_BROWSER_KEYS: usize = 8;
pub const PAIRING_SECONDS: u64 = 600;
const HANDSHAKE_SECONDS: u64 = 10;
const MAX_BROWSER_KEYS_FILE_BYTES: u64 = 4096;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub rfb_target: SocketAddr,
    pub app_origin: String,
    pub issuer: String,
    pub audience: String,
    pub owner_email: String,
    pub max_session_seconds: u64,
    /// The host's static Noise private key, created by `--init-host-key`.
    pub host_key_file: PathBuf,
    /// Enrolled browser public keys, one per line; written by pairing.
    pub browser_keys_file: PathBuf,
}

#[derive(Default)]
struct KeyCache {
    set: Option<KeySet>,
    loaded_at: Option<Instant>,
    last_attempt: Option<Instant>,
}

struct Enrollment {
    keys: Vec<Key>,
    /// At most one single-use invite secret and its expiry.
    invite: Option<(Key, Instant)>,
}

/// The one browser session the gateway relays at a time.
struct ActiveSession {
    id: u64,
    browser: Key,
    stop: Arc<Notify>,
}

pub struct Gateway {
    config: Config,
    policy: AccessPolicy,
    http: reqwest::Client,
    keys: Mutex<KeyCache>,
    handshakes: Semaphore,
    active: StdMutex<Option<ActiveSession>>,
    next_session: std::sync::atomic::AtomicU64,
    host: Keypair,
    enrollment: StdMutex<Enrollment>,
}

/// Holds the session slot until dropped.
struct SessionClaim<'a> {
    gateway: &'a Gateway,
    id: u64,
    stop: Arc<Notify>,
}

impl Drop for SessionClaim<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.gateway.active.lock()
            && active.as_ref().is_some_and(|session| session.id == self.id)
        {
            *active = None;
        }
    }
}

/// Creates the host identity without replacing an existing one. Rotation is
/// explicit: delete the file and pair every browser again.
pub fn create_host_key(path: &Path) -> io::Result<Keypair> {
    let keypair = Keypair::generate().map_err(io::Error::other)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    writeln!(file, "{}", e2e::encode_key(&keypair.private))?;
    file.sync_all()?;
    Ok(keypair)
}

fn load_host_key(path: &Path) -> Result<Keypair, Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
            return Err("host key file must be readable only by its owner".into());
        }
    }
    let text = fs::read_to_string(path)
        .map_err(|e| format!("cannot read host key file (run --init-host-key first): {e}"))?;
    Ok(Keypair::from_private(e2e::decode_key(text.trim())?)?)
}

fn load_browser_keys(path: &Path) -> Result<Vec<Key>, Box<dyn std::error::Error>> {
    let text = match fs::metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
        Ok(m) if m.len() > MAX_BROWSER_KEYS_FILE_BYTES => {
            return Err("browser keys file is too large".into());
        }
        Ok(_) => fs::read_to_string(path)?,
    };
    let mut keys = Vec::new();
    for line in text.lines() {
        let key = line.split('#').next().unwrap_or_default().trim();
        if key.is_empty() {
            continue;
        }
        let key = e2e::decode_key(key)?;
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    if keys.len() > MAX_BROWSER_KEYS {
        return Err("too many enrolled browser keys".into());
    }
    Ok(keys)
}

fn append_browser_key(path: &Path, key: &Key) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(
        file,
        "{} # paired at unix time {}",
        e2e::encode_key(key),
        unix_now()
    )?;
    file.sync_all()
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Gateway {
    pub fn new(config: Config) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        validate_origin(&config.app_origin)?;
        if !config.listen.ip().is_loopback()
            || !config.rfb_target.ip().is_loopback()
            || config.rfb_target.port() == 0
            || !(1..=7200).contains(&config.max_session_seconds)
        {
            return Err(
                "the gateway requires loopback listen and RFB target addresses, an RFB port, and a 1-7200 second session cap"
                    .into(),
            );
        }
        let policy = AccessPolicy::new(&config.issuer, &config.audience, &config.owner_email)?;
        let host = load_host_key(&config.host_key_file)?;
        let keys = load_browser_keys(&config.browser_keys_file)?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()?;
        Ok(Arc::new(Self {
            config,
            policy,
            http,
            keys: Mutex::new(KeyCache::default()),
            handshakes: Semaphore::new(8),
            active: StdMutex::new(None),
            next_session: std::sync::atomic::AtomicU64::new(1),
            host,
            enrollment: StdMutex::new(Enrollment { keys, invite: None }),
        }))
    }

    /// Use `jwks` as the Access signing keys instead of fetching them from
    /// the issuer. For local test harnesses only; never in the gateway binary.
    #[cfg(feature = "test-fixtures")]
    pub async fn preload_signing_keys(&self, jwks: &[u8]) -> Result<(), &'static str> {
        let mut cache = self.keys.lock().await;
        cache.set = Some(KeySet::parse(jwks)?);
        cache.loaded_at = Some(Instant::now());
        cache.last_attempt = Some(Instant::now());
        Ok(())
    }

    pub fn host_public(&self) -> Key {
        self.host.public
    }

    pub fn enrolled_browsers(&self) -> usize {
        self.enrollment.lock().map_or(0, |e| e.keys.len())
    }

    /// Replaces any earlier invite. Show it only on the host itself.
    pub fn open_pairing(&self) -> Result<PairingInvite, &'static str> {
        let mut enrollment = self
            .enrollment
            .lock()
            .map_err(|_| "enrollment unavailable")?;
        if enrollment.keys.len() >= MAX_BROWSER_KEYS {
            return Err("browser enrollment list is full");
        }
        let invite = PairingInvite::generate(self.host.public)?;
        enrollment.invite = Some((
            invite.secret,
            Instant::now() + Duration::from_secs(PAIRING_SECONDS),
        ));
        Ok(invite)
    }

    fn pairing_secret(&self) -> Option<Key> {
        let enrollment = self.enrollment.lock().ok()?;
        enrollment
            .invite
            .filter(|(_, expires)| Instant::now() < *expires)
            .map(|(secret, _)| secret)
    }

    fn authorize(&self, browser: Key, mode: Mode, secret: Option<Key>) -> Result<(), &'static str> {
        let mut enrollment = self
            .enrollment
            .lock()
            .map_err(|_| "enrollment unavailable")?;
        if mode == Mode::Session {
            return match enrollment.keys.contains(&browser) {
                true => Ok(()),
                false => Err("browser key is not enrolled"),
            };
        }
        // Single use: the handshake proved this exact secret, so consume it.
        if secret.is_none() || enrollment.invite.map(|(s, _)| s) != secret {
            return Err("pairing invite expired or already used");
        }
        enrollment.invite = None;
        if !enrollment.keys.contains(&browser) {
            if enrollment.keys.len() >= MAX_BROWSER_KEYS {
                return Err("browser enrollment list is full");
            }
            append_browser_key(&self.config.browser_keys_file, &browser)
                .map_err(|_| "could not save browser enrollment")?;
            enrollment.keys.push(browser);
        }
        eprintln!("enrolled browser key {}", e2e::fingerprint(&browser));
        Ok(())
    }

    /// Take the session slot for `browser`. A newer connection from the same
    /// enrolled browser replaces its own earlier session, which a suspended
    /// phone can leave open until the idle timeout; another browser is
    /// refused while a session is active.
    fn claim(&self, browser: Key) -> Result<SessionClaim<'_>, &'static str> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| "session state unavailable")?;
        if let Some(current) = active.as_ref() {
            if current.browser != browser {
                return Err("another browser session is active");
            }
            current.stop.notify_one();
        }
        let id = self
            .next_session
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stop = Arc::new(Notify::new());
        *active = Some(ActiveSession {
            id,
            browser,
            stop: stop.clone(),
        });
        Ok(SessionClaim {
            gateway: self,
            id,
            stop,
        })
    }

    async fn authenticate(&self, token: &str) -> Result<Identity, &'static str> {
        let kid = token_key_id(token)?;
        // Serialize refreshes; at most eight handshakes may wait for this lock.
        let mut cache = self.keys.lock().await;
        let stale = cache
            .loaded_at
            .is_none_or(|t| t.elapsed().as_secs() >= KEY_TTL_SECONDS);
        let unknown = cache.set.as_ref().is_none_or(|s| !s.contains(&kid));
        if stale || unknown {
            if cache
                .last_attempt
                .is_some_and(|t| t.elapsed().as_secs() < KEY_REFRESH_SECONDS)
            {
                return Err("signing key refresh rate limited");
            }
            cache.last_attempt = Some(Instant::now());
            let mut response = self
                .http
                .get(self.policy.jwks_url())
                .send()
                .await
                .map_err(|_| "JWKS unavailable")?;
            if !response.status().is_success()
                || response
                    .content_length()
                    .is_some_and(|n| n > MAX_JWKS_BYTES as u64)
            {
                return Err("JWKS response rejected");
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| "JWKS read failed")? {
                if chunk.len() > MAX_JWKS_BYTES.saturating_sub(bytes.len()) {
                    return Err("JWKS too large");
                }
                bytes.extend_from_slice(&chunk);
            }
            cache.set = Some(KeySet::parse(&bytes)?);
            cache.loaded_at = Some(Instant::now());
        }
        cache
            .set
            .as_ref()
            .ok_or("no signing keys")?
            .verify(token, &self.policy, unix_now())
    }
}

pub fn routes(state: Arc<Gateway>) -> Router {
    Router::new().fallback(any(upgrade)).with_state(state)
}

/// A loopback-only, bounded synthetic target. Never connects to a desktop.
pub async fn spawn_synthetic_echo() -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        let mut bytes = vec![0; READ_CHUNK_BYTES];
        while let Ok((mut stream, _)) = listener.accept().await {
            // One connection at a time; no task or queue per accepted message.
            loop {
                let count = match timeout(Duration::from_secs(60), stream.read(&mut bytes)).await {
                    Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                    Ok(Ok(count)) => count,
                };
                if !matches!(
                    timeout(Duration::from_secs(5), stream.write_all(&bytes[..count])).await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
        }
    });
    Ok((address, task))
}

async fn upgrade(State(state): State<Arc<Gateway>>, request: axum::extract::Request) -> Response {
    let headers = request.headers();
    let header = |name: &str| headers.get(name).and_then(|h| h.to_str().ok());
    if validate_upgrade(
        request.method().as_str(),
        request.uri().path(),
        request.uri().query(),
        header("origin"),
        header("sec-websocket-protocol"),
        header("upgrade"),
        &state.config.app_origin,
    )
    .is_err()
    {
        return (StatusCode::FORBIDDEN, "WebSocket request rejected").into_response();
    }
    let Ok(_pending) = state.handshakes.try_acquire() else {
        return (StatusCode::TOO_MANY_REQUESTS, "Too many handshakes").into_response();
    };
    let Some(token) = header("cf-access-jwt-assertion").map(str::to_owned) else {
        return (StatusCode::UNAUTHORIZED, "Access authentication required").into_response();
    };
    let (mut parts, _) = request.into_parts();
    let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(ws) => ws,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid WebSocket handshake").into_response(),
    };
    let identity = match timeout(Duration::from_secs(6), state.authenticate(&token)).await {
        Ok(Ok(identity)) => identity,
        _ => return (StatusCode::UNAUTHORIZED, "Access authentication failed").into_response(),
    };
    let seconds = identity
        .expires_at
        .saturating_sub(unix_now())
        .min(state.config.max_session_seconds);
    if seconds == 0 {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let state = state.clone();
    ws.protocols(["binary"])
        .max_message_size(MAX_CIPHERTEXT_BYTES)
        .max_frame_size(MAX_CIPHERTEXT_BYTES)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_CIPHERTEXT_BYTES * 2)
        .on_upgrade(move |socket| async move {
            session(socket, state, Duration::from_secs(seconds)).await;
        })
}

/// Opens the target only after the browser proves an enrolled key (or a live
/// invite) and the host proves the pinned key. There is no plaintext fallback.
async fn session(mut socket: WebSocket, state: Arc<Gateway>, lifetime: Duration) {
    let expires = Instant::now() + lifetime;
    let deadline = expires.min(Instant::now() + Duration::from_secs(HANDSHAKE_SECONDS));
    let (channel, browser) = match timeout_at(deadline, handshake(&mut socket, &state)).await {
        Ok(Ok(established)) => established,
        Ok(Err(reason)) => return reject(socket, 1008, reason).await,
        Err(_) => return reject(socket, 1008, "E2E handshake timed out").await,
    };
    // 1013: try again later, once the other browser disconnects.
    let claim = match state.claim(browser) {
        Ok(claim) => claim,
        Err(reason) => return reject(socket, 1013, reason).await,
    };
    let tcp = match timeout(
        Duration::from_secs(3),
        TcpStream::connect(state.config.rfb_target),
    )
    .await
    {
        Ok(Ok(tcp)) if tcp.set_nodelay(true).is_ok() => tcp,
        _ => return reject(socket, 1011, "RFB host offline").await,
    };
    relay(socket, tcp, channel, expires, &claim.stop).await;
}

async fn handshake(
    socket: &mut WebSocket,
    state: &Gateway,
) -> Result<(Channel, Key), &'static str> {
    let first = receive_binary(socket).await?;
    let secret = state.pairing_secret();
    let (responder, reply) = Responder::accept(&state.host.private, &first, secret.as_ref())?;
    socket
        .send(Message::Binary(reply.into()))
        .await
        .map_err(|_| "browser disconnected")?;
    let last = receive_binary(socket).await?;
    let (channel, browser, mode) = responder.finish(&last)?;
    state.authorize(browser, mode, secret)?;
    Ok((channel, browser))
}

async fn receive_binary(socket: &mut WebSocket) -> Result<Vec<u8>, &'static str> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Binary(data))) => return Ok(data.to_vec()),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Text(_))) => return Err("binary messages required"),
            _ => return Err("browser closed during E2E handshake"),
        }
    }
}

async fn reject(mut socket: WebSocket, code: u16, reason: &'static str) {
    let _ = timeout(
        Duration::from_secs(1),
        socket.send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        }))),
    )
    .await;
    eprintln!("session rejected: code={code} reason={reason}");
}

async fn relay(
    mut socket: WebSocket,
    mut tcp: TcpStream,
    mut channel: Channel,
    expires: Instant,
    stop: &Notify,
) {
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    heartbeat.tick().await;
    let mut last_activity = Instant::now();
    let mut bytes = vec![0; MAX_PLAINTEXT_BYTES];
    let (mut upstream_bytes, mut downstream_bytes) = (0_u64, 0_u64);
    let (code, reason) = loop {
        let write_deadline = || expires.min(Instant::now() + Duration::from_secs(5));
        tokio::select! {
            _ = tokio::time::sleep_until(expires) => break (1008, "Session expired"),
            _ = stop.notified() => break (1000, "Replaced by a newer connection from this browser"),
            _ = heartbeat.tick() => {
                if last_activity.elapsed() >= Duration::from_secs(60) { break (1001, "Peer idle"); }
                if !matches!(timeout_at(write_deadline(), socket.send(Message::Ping(Vec::new().into()))).await, Ok(Ok(()))) {
                    break (1013, "Slow WebSocket peer");
                }
            }
            message = socket.recv() => {
                last_activity = Instant::now();
                match message {
                    Some(Ok(Message::Binary(data))) => {
                        if data.len() > MAX_CIPHERTEXT_BYTES { break (1009, "Message too large"); }
                        let Ok(plaintext) = channel.open(&data) else {
                            break (1008, "E2E integrity check failed");
                        };
                        if !matches!(timeout_at(write_deadline(), tcp.write_all(&plaintext)).await, Ok(Ok(()))) {
                            break (1013, "Slow TCP peer");
                        }
                        upstream_bytes = upstream_bytes.saturating_add(plaintext.len() as u64);
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if !matches!(timeout_at(write_deadline(), socket.send(Message::Pong(data))).await, Ok(Ok(()))) {
                            break (1013, "Slow WebSocket peer");
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Text(_))) => break (1003, "Binary messages required"),
                    Some(Ok(Message::Close(_))) | None => break (1000, "Browser closed"),
                    // An abrupt disconnect, or a frame the size limits refused.
                    Some(Err(_)) => break (1011, "Browser connection lost or message rejected"),
                }
            }
            read = tcp.read(&mut bytes) => {
                match read {
                    Ok(0) => break (1000, "RFB connection closed"),
                    Ok(n) => {
                        last_activity = Instant::now();
                        let Ok(sealed) = channel.seal(&bytes[..n]) else {
                            break (1011, "E2E encryption failed");
                        };
                        if !matches!(timeout_at(write_deadline(), socket.send(Message::Binary(sealed.into()))).await, Ok(Ok(()))) {
                            break (1013, "Slow WebSocket peer");
                        }
                        downstream_bytes = downstream_bytes.saturating_add(n as u64);
                    }
                    Err(_) => break (1011, "RFB connection failed"),
                }
            }
        }
    };
    let _ = timeout(
        Duration::from_secs(1),
        socket.send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        }))),
    )
    .await;
    let _ = tcp.shutdown().await;
    eprintln!(
        "relay closed: code={code} reason={reason} upstream_bytes={upstream_bytes} downstream_bytes={downstream_bytes}"
    );
}

#[cfg(test)]
mod tests;
