//! The Quick Tunnel route: pairing only, page served by the gateway.

use super::*;
use crate::assets::{PAGE_FILES, PAGE_HEADERS};

const QUICK: &str = "https://seasonal-deck-organisms-sf.trycloudflare.com";

/// A fake build of web/dist: each page file holds its own name.
fn fake_dist(dir: &Path) -> PathBuf {
    let dist = dir.join("dist");
    fs::create_dir_all(&dist).unwrap();
    for (_, name, _) in PAGE_FILES {
        fs::write(dist.join(name), format!("contents of {name}")).unwrap();
    }
    dist
}

fn quick_config(dir: &Path, rfb_target: SocketAddr, max_session_seconds: u64) -> QuickTunnelConfig {
    QuickTunnelConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        rfb_target,
        app_origin: QUICK.into(),
        max_session_seconds,
        web_root: fake_dist(dir),
    }
}

struct Quick {
    base: String,
    url: String,
    rfb: TcpListener,
    gateway: Arc<Gateway>,
    dir: TempDir,
    _server: Server,
}

async fn quick(max_session_seconds: u64) -> Quick {
    let dir = TempDir::new();
    let rfb = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway = Gateway::quick_tunnel(quick_config(
        &dir.0,
        rfb.local_addr().unwrap(),
        max_session_seconds,
    ))
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = routes(gateway.clone());
    let server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    Quick {
        base: format!("http://{address}"),
        url: format!("ws://{address}/ws"),
        rfb,
        gateway,
        dir,
        _server: server,
    }
}

/// An upgrade as a browser sends it through the Quick Tunnel: no JWT.
fn quick_request(url: &str, origin: Option<&str>) -> tungstenite::http::Request<()> {
    let mut request = url.into_client_request().unwrap();
    let headers = request.headers_mut();
    if let Some(origin) = origin {
        headers.insert("origin", origin.parse().unwrap());
    }
    headers.insert("sec-websocket-protocol", "binary".parse().unwrap());
    request
}

async fn quick_open(q: &Quick) -> Ws {
    let (ws, response) = connect_async(quick_request(&q.url, Some(QUICK)))
        .await
        .unwrap();
    assert_eq!(response.headers()["sec-websocket-protocol"], "binary");
    ws
}

#[tokio::test]
async fn admits_only_its_origin_and_only_enrolled_browsers() {
    let q = quick(7200).await;
    assert!(q.gateway.is_pairing_only());
    assert_eq!(q.gateway.app_origin(), QUICK);
    let root = q.url.replace("/ws", "/");
    let query = format!("{}?target=127.0.0.1:22", q.url);
    for (url, origin) in [
        (q.url.as_str(), None),
        (q.url.as_str(), Some(fixtures::ORIGIN)),
        (q.url.as_str(), Some("https://other.trycloudflare.com")),
        (q.url.as_str(), Some("https://attacker.example")),
        (query.as_str(), Some(QUICK)),
        (root.as_str(), Some(QUICK)),
    ] {
        assert!(
            connect_async(quick_request(url, origin)).await.is_err(),
            "{url} from {origin:?}"
        );
    }
    // No JWT is needed to reach the handshake, but a stranger's key is
    // refused there, before any connection to the desktop.
    let stranger = Keypair::generate().unwrap();
    let mut ws = quick_open(&q).await;
    assert!(
        handshake(&mut ws, &stranger.private, &q.gateway.host_public(), None)
            .await
            .is_some()
    );
    assert_closed(&mut ws, Some("browser key is not enrolled")).await;
    // A guessed pairing secret fails at the host's last handshake check and
    // enrolls nothing.
    let invite = q.gateway.open_pairing().unwrap();
    let guess = PairingInvite::generate(invite.host).unwrap();
    let mut ws = quick_open(&q).await;
    assert!(
        handshake(
            &mut ws,
            &stranger.private,
            &invite.host,
            Some(&guess.secret)
        )
        .await
        .is_some()
    );
    assert_closed(&mut ws, Some("E2E handshake failed")).await;
    assert_eq!(q.gateway.enrolled_browsers(), 0);
    assert_no_tcp(&q.rfb).await;
}

#[tokio::test]
async fn pairs_in_memory_only_and_relays() {
    let q = quick(7200).await;
    let phone = Keypair::generate().unwrap();
    let invite = q.gateway.open_pairing().unwrap();
    let mut ws = quick_open(&q).await;
    let mut channel = handshake(&mut ws, &phone.private, &invite.host, Some(&invite.secret))
        .await
        .unwrap();
    let (mut tcp, _) = q.rfb.accept().await.unwrap();
    send_sealed(&mut ws, &mut channel, b"paired").await;
    let mut bytes = [0; 6];
    tcp.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"paired");
    ws.close(None).await.unwrap();
    assert_tcp_closed(&mut tcp).await;
    assert_eq!(q.gateway.enrolled_browsers(), 1);
    // Nothing but the page build is on disk.
    let files: Vec<_> = fs::read_dir(&q.dir.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(files, ["dist"]);

    // The enrolled key reconnects without an invite.
    let mut ws = quick_open(&q).await;
    let mut channel = handshake(&mut ws, &phone.private, &invite.host, None)
        .await
        .unwrap();
    let (mut tcp, _) = q.rfb.accept().await.unwrap();
    send_sealed(&mut ws, &mut channel, b"again").await;
    let mut bytes = [0; 5];
    tcp.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"again");
}

#[tokio::test]
async fn caps_the_session_without_a_jwt() {
    let q = quick(1).await;
    let phone = Keypair::generate().unwrap();
    let invite = q.gateway.open_pairing().unwrap();
    let mut ws = quick_open(&q).await;
    handshake(&mut ws, &phone.private, &invite.host, Some(&invite.secret))
        .await
        .unwrap();
    let (mut tcp, _) = q.rfb.accept().await.unwrap();
    assert_closed(&mut ws, Some("Session expired")).await;
    assert_tcp_closed(&mut tcp).await;
}

#[tokio::test]
async fn serves_only_page_files_with_security_headers() {
    let q = quick(7200).await;
    let http = reqwest::Client::new();
    for (route, name, kind) in PAGE_FILES {
        let response = http.get(format!("{}{route}", q.base)).send().await.unwrap();
        assert_eq!(response.status(), 200, "{route}");
        let headers = response.headers().clone();
        assert_eq!(headers["content-type"], kind, "{route}");
        assert_eq!(headers["cache-control"], "no-cache", "{route}");
        for (header, value) in PAGE_HEADERS {
            assert_eq!(headers[header], value, "{route} {header}");
        }
        assert_eq!(
            response.text().await.unwrap(),
            format!("contents of {name}")
        );
    }
    for path in [
        "/host.key",
        "/dist/index.html",
        "/app.js.map",
        "/_headers",
        "/%2e%2e/Cargo.toml",
    ] {
        let response = http.get(format!("{}{path}", q.base)).send().await.unwrap();
        assert_eq!(response.status(), 404, "{path}");
    }
    let post = http.post(format!("{}/", q.base)).send().await.unwrap();
    assert_eq!(post.status(), 405);
    // /ws is only ever an upgrade.
    let plain = http.get(format!("{}/ws", q.base)).send().await.unwrap();
    assert_eq!(plain.status(), 403);
    assert_no_tcp(&q.rfb).await;
}

#[tokio::test]
async fn an_access_gateway_does_not_serve_the_page() {
    let h = harness().await;
    let page = h.url.replace("ws://", "http://").replace("/ws", "/");
    let response = reqwest::get(page).await.unwrap();
    assert_eq!(response.status(), 403);
}

#[test]
fn config_is_checked_and_each_run_has_a_new_host_key() {
    let dir = TempDir::new();
    let target: SocketAddr = "127.0.0.1:5900".parse().unwrap();
    let first = Gateway::quick_tunnel(quick_config(&dir.0, target, 7200)).unwrap();
    let second = Gateway::quick_tunnel(quick_config(&dir.0, target, 7200)).unwrap();
    assert_ne!(first.host_public(), second.host_public());
    assert_eq!(first.enrolled_browsers(), 0);

    let mut workers = quick_config(&dir.0, target, 7200);
    workers.app_origin = fixtures::ORIGIN.into();
    assert!(
        Gateway::quick_tunnel(workers).is_err(),
        "workers.dev origin"
    );
    let mut public = quick_config(&dir.0, target, 7200);
    public.listen = "0.0.0.0:6080".parse().unwrap();
    assert!(Gateway::quick_tunnel(public).is_err(), "all networks");
    let mut lan = quick_config(&dir.0, target, 7200);
    lan.rfb_target = "192.168.1.10:5900".parse().unwrap();
    assert!(Gateway::quick_tunnel(lan).is_err(), "LAN target");
    assert!(
        Gateway::quick_tunnel(quick_config(&dir.0, target, 7201)).is_err(),
        "session cap"
    );
    let unbuilt = quick_config(&dir.0, target, 7200);
    fs::remove_file(unbuilt.web_root.join("topvnc_web_bg.wasm")).unwrap();
    let error = Gateway::quick_tunnel(unbuilt).err().unwrap().to_string();
    assert!(error.contains("npm run build"), "{error}");

    // And the Access route refuses a Quick Tunnel origin.
    provision(&dir.0);
    let mut access = config(&dir.0, target, 7200);
    access.app_origin = QUICK.into();
    assert!(Gateway::new(access).is_err());
}
