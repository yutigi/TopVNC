//! Local browser test of the whole client path without Cloudflare or a real
//! desktop. It serves `web/dist` at http://127.0.0.1:PORT and stands in for
//! the Cloudflare edge: `/ws` checks the page origin and forwards the
//! upgrade to the real gateway with a test-signed Access JWT, as the Worker
//! does with the real one. The gateway relays the Noise channel to an
//! in-process TopVNC server with a password and an animated synthetic
//! scene, which prints the input it receives.
//!
//! `cargo run -p topvnc-web-gateway --features test-fixtures --example local_web`
//! (build web/dist first with `npm run build` in web/). Loopback only; the
//! test signing key is public, so never expose this harness.

use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
use topvnc::{ClientEvent, Framebuffer, ServerConfig, VncServer};
use topvnc_access::fixtures;
use topvnc_web_gateway::{Config, Gateway, create_host_key, routes, unix_now};

const PASSWORD: &str = "topvnc-local";
const SCENE: (u16, u16) = (1280, 720);
/// Files served from web/dist; nothing else is reachable.
const FILES: [(&str, &str); 6] = [
    ("/", "index.html"),
    ("/index.html", "index.html"),
    ("/app.js", "app.js"),
    ("/session-worker.js", "session-worker.js"),
    ("/style.css", "style.css"),
    ("/topvnc_web_bg.wasm", "topvnc_web_bg.wasm"),
];

struct Edge {
    dist: PathBuf,
    origin: String,
    gateway_url: String,
    headers: Vec<(HeaderName, HeaderValue)>,
}

/// A moving bar and a frame counter, scaled to the served size.
fn scene(frame: u32, percent: u8) -> std::io::Result<Framebuffer> {
    let scale = |value: u16| (u32::from(value) * u32::from(percent) / 100) as u16;
    let (width, height) = (scale(SCENE.0), scale(SCENE.1));
    let mut framebuffer = Framebuffer::new(width, height)?;
    let bar = (frame as usize * 6) % usize::from(width);
    for (index, pixel) in framebuffer.pixels_mut().iter_mut().enumerate() {
        let (x, y) = (index % usize::from(width), index / usize::from(width));
        *pixel = if x.abs_diff(bar) < 24 {
            0x00d5b56a
        } else if (x / 40 + y / 40) % 2 == 0 {
            0x00233140
        } else {
            0x00182230
        };
        // Frame number in binary along the top, 16-pixel cells.
        if y < 16 && x < 16 * 16 && frame >> (x / 16) & 1 == 1 {
            *pixel = 0x00ffffff;
        }
    }
    Ok(framebuffer)
}

fn serve_scene(server: Arc<VncServer>) {
    let runner = Arc::clone(&server);
    std::thread::spawn(move || runner.run());
    server.set_serve_scale(100);
    let mut applied = 100;
    let started = Instant::now();
    for frame in 0u32.. {
        let requested = server.requested_serve_scale();
        if requested != applied {
            server.set_serve_scale(requested);
            applied = requested;
            eprintln!("scene: served size {applied}%");
        }
        if let Ok(framebuffer) = scene(frame, applied) {
            let _ = server.update_framebuffer(&framebuffer);
        }
        while let Ok(event) = server.try_event() {
            match event {
                ClientEvent::Key { keysym, down, .. } => {
                    eprintln!(
                        "input: key {keysym:#06x} {}",
                        if down { "down" } else { "up" }
                    );
                }
                ClientEvent::Pointer { buttons, x, y, .. } => {
                    eprintln!("input: pointer {x},{y} buttons {buttons:#x}");
                }
                ClientEvent::RelativePointer {
                    buttons, dx, dy, ..
                } => {
                    eprintln!("input: motion {dx},{dy} buttons {buttons:#x}");
                }
                ClientEvent::ClientDisconnected { .. } => eprintln!("scene: viewer disconnected"),
                ClientEvent::ClipboardText { .. } => {}
            }
        }
        // About 30 frames per second.
        let next = Duration::from_millis(33) * (frame + 1);
        std::thread::sleep(next.saturating_sub(started.elapsed()));
    }
}

async fn file(State(edge): State<Arc<Edge>>, uri: Uri) -> Response {
    let Some((_, name)) = FILES.iter().find(|(path, _)| *path == uri.path()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(body) = tokio::fs::read(edge.dist.join(name)).await else {
        return (StatusCode::NOT_FOUND, "Build web/dist first").into_response();
    };
    let kind = match name.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        _ => "application/wasm",
    };
    let mut response = ([(header::CONTENT_TYPE, kind)], body).into_response();
    for (name, value) in &edge.headers {
        response.headers_mut().insert(name.clone(), value.clone());
    }
    response
}

/// The edge's job: check the page's own origin, then forward to the gateway
/// with the JWT Access would add.
async fn upgrade(
    State(edge): State<Arc<Edge>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    if origin != Some(edge.origin.as_str()) {
        return (StatusCode::FORBIDDEN, "WebSocket request rejected").into_response();
    }
    let edge = Arc::clone(&edge);
    ws.protocols(["binary"])
        .on_upgrade(move |browser| relay(browser, edge))
}

async fn relay(browser: WebSocket, edge: Arc<Edge>) {
    let mut request = edge.gateway_url.as_str().into_client_request().unwrap();
    let headers = request.headers_mut();
    headers.insert("origin", fixtures::ORIGIN.parse().unwrap());
    headers.insert("sec-websocket-protocol", "binary".parse().unwrap());
    let token = fixtures::token(unix_now() + 3600);
    headers.insert("cf-access-jwt-assertion", token.parse().unwrap());
    let Ok((gateway, _)) = tokio_tungstenite::connect_async(request).await else {
        eprintln!("edge: gateway refused the upgrade");
        return;
    };
    let (mut to_browser, mut from_browser) = browser.split();
    let (mut to_gateway, mut from_gateway) = gateway.split();
    let upstream = async {
        while let Some(Ok(message)) = from_browser.next().await {
            let message = match message {
                Message::Binary(data) => tungstenite::Message::Binary(data),
                Message::Close(_) => break,
                _ => continue,
            };
            if to_gateway.send(message).await.is_err() {
                break;
            }
        }
        let _ = to_gateway.close().await;
    };
    let downstream = async {
        while let Some(Ok(message)) = from_gateway.next().await {
            let message = match message {
                tungstenite::Message::Binary(data) => Message::Binary(data),
                tungstenite::Message::Close(frame) => {
                    let frame = frame.map(|frame| axum::extract::ws::CloseFrame {
                        code: frame.code.into(),
                        reason: frame.reason.as_str().into(),
                    });
                    let _ = to_browser.send(Message::Close(frame)).await;
                    break;
                }
                _ => continue,
            };
            if to_browser.send(message).await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        () = upstream => {}
        () = downstream => {}
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dist = root.join("web/dist");
    // Kept between runs so a paired test browser stays paired.
    let dir = root.join("target/local-web");
    std::fs::create_dir_all(&dir)?;
    if !dir.join("host.key").exists() {
        create_host_key(&dir.join("host.key"))?;
    }

    let server = Arc::new(VncServer::bind(
        "127.0.0.1:0",
        scene(0, 100)?,
        ServerConfig {
            name: "TopVNC local web test".into(),
            password: Some(PASSWORD.into()),
            ..ServerConfig::default()
        },
    )?);
    let rfb_target = server.local_addr()?;
    std::thread::spawn(move || serve_scene(server));

    let policy = fixtures::policy();
    let gateway = Gateway::new(Config {
        listen: "127.0.0.1:0".parse()?,
        rfb_target,
        app_origin: fixtures::ORIGIN.into(),
        issuer: policy.issuer,
        audience: policy.audience,
        owner_email: policy.owner_email,
        max_session_seconds: 7200,
        host_key_file: dir.join("host.key"),
        browser_keys_file: dir.join("browsers.txt"),
    })?;
    gateway
        .preload_signing_keys(fixtures::jwks().as_bytes())
        .await?;
    // Cached keys go stale after a few minutes, and the test issuer cannot
    // be reached, so keep loading the test keys.
    let refresher = Arc::clone(&gateway);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let _ = refresher
                .preload_signing_keys(fixtures::jwks().as_bytes())
                .await;
        }
    });
    let gateway_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let gateway_url = format!("ws://{}/ws", gateway_listener.local_addr()?);
    let invite = gateway.open_pairing()?;
    tokio::spawn(async move { axum::serve(gateway_listener, routes(gateway)).await });

    let port: u16 = std::env::var("TOPVNC_LOCAL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8765);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let headers = std::fs::read_to_string(dist.join("_headers"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().split_once(": "))
        .filter_map(|(name, value)| Some((name.parse().ok()?, value.parse().ok()?)))
        .collect();
    let edge = Arc::new(Edge {
        dist,
        origin: origin.clone(),
        gateway_url,
        headers,
    });
    let app = Router::new()
        .route("/ws", get(upgrade))
        .fallback(get(file))
        .with_state(edge);
    eprintln!(
        "Local web test on {origin} (loopback only; test Access key; synthetic scene).\n\
         Pair:     {origin}/#pair={}\n\
         Password: {PASSWORD}",
        invite.encode()
    );
    axum::serve(listener, app).await?;
    Ok(())
}
