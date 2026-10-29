//! Isolated Phase 0 transport. Never interprets RFB or accepts a remote target.

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
    net::SocketAddr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, Semaphore},
    time::{Instant, timeout, timeout_at},
};
use topvnc_spike_policy::{
    AccessPolicy, Identity, KEY_REFRESH_SECONDS, KEY_TTL_SECONDS, KeySet, MAX_JWKS_BYTES,
    MAX_MESSAGE_BYTES, READ_CHUNK_BYTES, token_key_id, validate_origin, validate_upgrade,
};

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
}

#[derive(Default)]
struct KeyCache {
    set: Option<KeySet>,
    loaded_at: Option<Instant>,
    last_attempt: Option<Instant>,
}

pub struct Gateway {
    config: Config,
    policy: AccessPolicy,
    http: reqwest::Client,
    keys: Mutex<KeyCache>,
    handshakes: Semaphore,
    active: Arc<Semaphore>,
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
                "spike requires loopback listen/RFB addresses and a 1–7200 second session cap"
                    .into(),
            );
        }
        let policy = AccessPolicy::new(&config.issuer, &config.audience, &config.owner_email)?;
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
            active: Arc::new(Semaphore::new(1)),
        }))
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
    let Ok(permit) = state.active.clone().try_acquire_owned() else {
        return (
            StatusCode::CONFLICT,
            "One browser session is already active",
        )
            .into_response();
    };
    let tcp = match timeout(
        Duration::from_secs(3),
        TcpStream::connect(state.config.rfb_target),
    )
    .await
    {
        Ok(Ok(tcp)) => tcp,
        _ => return (StatusCode::BAD_GATEWAY, "RFB host offline").into_response(),
    };
    if tcp.set_nodelay(true).is_err() {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    let seconds = identity
        .expires_at
        .saturating_sub(unix_now())
        .min(state.config.max_session_seconds);
    if seconds == 0 {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.protocols(["binary"])
        .max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_MESSAGE_BYTES * 2)
        .on_upgrade(move |socket| async move {
            relay(socket, tcp, Duration::from_secs(seconds)).await;
            drop(permit);
        })
}

async fn relay(mut socket: WebSocket, mut tcp: TcpStream, lifetime: Duration) {
    let expires = Instant::now() + lifetime;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    heartbeat.tick().await;
    let mut last_activity = Instant::now();
    let mut bytes = vec![0; READ_CHUNK_BYTES];
    let (mut upstream_bytes, mut downstream_bytes) = (0_u64, 0_u64);
    let (code, reason) = loop {
        let write_deadline = || expires.min(Instant::now() + Duration::from_secs(5));
        tokio::select! {
            _ = tokio::time::sleep_until(expires) => break (1008, "Session expired"),
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
                        if data.len() > MAX_MESSAGE_BYTES { break (1009, "Message too large"); }
                        if !matches!(timeout_at(write_deadline(), tcp.write_all(&data)).await, Ok(Ok(()))) {
                            break (1013, "Slow TCP peer");
                        }
                        upstream_bytes = upstream_bytes.saturating_add(data.len() as u64);
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if !matches!(timeout_at(write_deadline(), socket.send(Message::Pong(data))).await, Ok(Ok(()))) {
                            break (1013, "Slow WebSocket peer");
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Text(_))) => break (1003, "Binary messages required"),
                    Some(Ok(Message::Close(_))) | None => break (1000, "Browser closed"),
                    Some(Err(_)) => break (1009, "Invalid or oversized message"),
                }
            }
            read = tcp.read(&mut bytes) => {
                match read {
                    Ok(0) => break (1000, "RFB connection closed"),
                    Ok(n) => {
                        last_activity = Instant::now();
                        if !matches!(timeout_at(write_deadline(), socket.send(Message::Binary(bytes[..n].to_vec().into()))).await, Ok(Ok(()))) {
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
