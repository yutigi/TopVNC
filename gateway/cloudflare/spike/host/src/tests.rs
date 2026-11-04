use super::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::net::TcpListener;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{self, client::IntoClientRequest},
};
use topvnc_spike_e2e::Initiator;
use topvnc_spike_policy::fixtures;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

#[tokio::test]
async fn synthetic_target_echoes_bounded_chunks_and_accepts_a_new_session() {
    let (address, task) = spawn_synthetic_echo().await.unwrap();
    assert!(address.ip().is_loopback());
    let payload: Vec<u8> = (0..READ_CHUNK_BYTES * 3).map(|n| (n % 251) as u8).collect();
    for _ in 0..2 {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(&payload).await.unwrap();
        let mut received = vec![0; payload.len()];
        timeout(Duration::from_secs(5), stream.read_exact(&mut received))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received, payload);
        stream.shutdown().await.unwrap();
    }
    task.abort();
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "topvnc-spike-host-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Server(tokio::task::JoinHandle<()>);

impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Harness {
    url: String,
    rfb: TcpListener,
    gateway: Arc<Gateway>,
    browser: Keypair,
    dir: TempDir,
    _server: Server,
}

fn config(dir: &Path, rfb_target: SocketAddr, max_session_seconds: u64) -> Config {
    let policy = fixtures::policy();
    Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        rfb_target,
        app_origin: fixtures::ORIGIN.into(),
        issuer: policy.issuer,
        audience: policy.audience,
        owner_email: policy.owner_email,
        max_session_seconds,
        host_key_file: dir.join("host.key"),
        browser_keys_file: dir.join("browsers.txt"),
    }
}

/// A host identity plus one enrolled browser key.
fn provision(dir: &Path) -> Keypair {
    create_host_key(&dir.join("host.key")).unwrap();
    let browser = Keypair::generate().unwrap();
    let line = format!("# enrolled\n{} # test\n", e2e::encode_key(&browser.public));
    fs::write(dir.join("browsers.txt"), line).unwrap();
    browser
}

async fn harness() -> Harness {
    harness_with_lifetime(7200).await
}

async fn harness_with_lifetime(max_session_seconds: u64) -> Harness {
    let dir = TempDir::new();
    let browser = provision(&dir.0);
    serve(dir, browser, max_session_seconds).await
}

/// Starts a gateway from the identity and enrollment files already in `dir`.
async fn serve(dir: TempDir, browser: Keypair, max_session_seconds: u64) -> Harness {
    let rfb = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway = Gateway::new(config(
        &dir.0,
        rfb.local_addr().unwrap(),
        max_session_seconds,
    ))
    .unwrap();
    {
        let mut cache = gateway.keys.lock().await;
        cache.set = Some(KeySet::parse(fixtures::jwks().as_bytes()).unwrap());
        cache.loaded_at = Some(Instant::now());
        cache.last_attempt = Some(Instant::now());
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let app = routes(gateway.clone());
    let server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    Harness {
        url,
        rfb,
        gateway,
        browser,
        dir,
        _server: server,
    }
}

fn request(url: &str, token: Option<&str>) -> tungstenite::http::Request<()> {
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", fixtures::ORIGIN.parse().unwrap());
    request
        .headers_mut()
        .insert("sec-websocket-protocol", "binary".parse().unwrap());
    if let Some(token) = token {
        request
            .headers_mut()
            .insert("cf-access-jwt-assertion", token.parse().unwrap());
    }
    request
}

async fn open(url: &str) -> Ws {
    let token = fixtures::token(unix_now() + 3600);
    let (ws, response) = connect_async(request(url, Some(&token))).await.unwrap();
    assert_eq!(response.headers()["sec-websocket-protocol"], "binary");
    ws
}

/// The next data or close message, skipping heartbeats.
async fn next(ws: &mut Ws) -> Option<tungstenite::Message> {
    loop {
        match timeout(Duration::from_secs(12), ws.next()).await.unwrap() {
            Some(Ok(tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_))) => {}
            Some(Ok(message)) => return Some(message),
            _ => return None,
        }
    }
}

/// The browser side of the E2E handshake. `None` if the host refused it.
async fn handshake(
    ws: &mut Ws,
    browser: &Key,
    host: &Key,
    secret: Option<&Key>,
) -> Option<Channel> {
    let (initiator, first) = Initiator::start(browser, host, secret).unwrap();
    ws.send(tungstenite::Message::Binary(first.into()))
        .await
        .unwrap();
    let Some(tungstenite::Message::Binary(reply)) = next(ws).await else {
        return None;
    };
    let (last, channel) = initiator.finish(&reply).ok()?;
    ws.send(tungstenite::Message::Binary(last.into()))
        .await
        .unwrap();
    Some(channel)
}

async fn connect(h: &Harness) -> (Ws, Channel) {
    connect_as(h, &h.browser.private, None).await
}

async fn connect_as(h: &Harness, browser: &Key, secret: Option<&Key>) -> (Ws, Channel) {
    let mut ws = open(&h.url).await;
    let channel = handshake(&mut ws, browser, &h.gateway.host_public(), secret)
        .await
        .unwrap();
    (ws, channel)
}

async fn send_sealed(ws: &mut Ws, channel: &mut Channel, plaintext: &[u8]) {
    let sealed = channel.seal(plaintext).unwrap();
    ws.send(tungstenite::Message::Binary(sealed.into()))
        .await
        .unwrap();
}

/// Asserts the gateway closed the socket, with `reason` when one is given.
async fn assert_closed(ws: &mut Ws, reason: Option<&str>) {
    match next(ws).await {
        Some(tungstenite::Message::Close(frame)) => {
            if let Some(reason) = reason {
                assert_eq!(frame.unwrap().reason.as_str(), reason);
            }
        }
        None => assert!(reason.is_none(), "closed without a reason frame"),
        Some(other) => panic!("expected close, got {other:?}"),
    }
}

async fn assert_no_tcp(rfb: &TcpListener) {
    assert!(
        timeout(Duration::from_millis(200), rfb.accept())
            .await
            .is_err()
    );
}

async fn assert_tcp_closed(tcp: &mut TcpStream) {
    let mut byte = [0];
    assert_eq!(
        timeout(Duration::from_secs(2), tcp.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn denies_unauthenticated_expired_wrong_origin_and_injected_destination_without_tcp() {
    let h = harness().await;
    let good = fixtures::token(unix_now() + 3600);
    let expired = fixtures::token(unix_now() - 1);
    let mut wrong_origin = request(&h.url, Some(&good));
    wrong_origin
        .headers_mut()
        .insert("origin", "https://attacker.example".parse().unwrap());
    for req in [
        request(&h.url, None),
        request(&h.url, Some("invalid")),
        request(&h.url, Some(&expired)),
        wrong_origin,
        request(&format!("{}?target=127.0.0.1:22", h.url), Some(&good)),
    ] {
        assert!(connect_async(req).await.is_err());
    }
    assert_no_tcp(&h.rfb).await;
}

#[tokio::test]
async fn relays_only_ciphertext_and_propagates_tcp_close() {
    let h = harness().await;
    let (mut ws, mut channel) = connect(&h).await;
    let (mut tcp, _) = h.rfb.accept().await.unwrap();
    tcp.write_all(b"RFB 003.008\n").await.unwrap();
    let Some(tungstenite::Message::Binary(sealed)) = next(&mut ws).await else {
        panic!("expected an encrypted message");
    };
    assert_eq!(sealed.len(), 12 + e2e::TAG_BYTES);
    assert!(!sealed.windows(3).any(|w| w == b"RFB"));
    assert_eq!(channel.open(&sealed).unwrap(), b"RFB 003.008\n");
    send_sealed(&mut ws, &mut channel, b"first").await;
    send_sealed(&mut ws, &mut channel, b"second").await;
    let mut bytes = [0; 11];
    tcp.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"firstsecond");
    drop(tcp);
    assert_closed(&mut ws, Some("RFB connection closed")).await;
}

#[tokio::test]
async fn rejects_unenrolled_browsers_wrong_host_keys_and_malformed_handshakes_without_tcp() {
    let h = harness().await;
    let stranger = Keypair::generate().unwrap();
    let mut ws = open(&h.url).await;
    assert!(
        handshake(&mut ws, &stranger.private, &h.gateway.host_public(), None)
            .await
            .is_some()
    );
    assert_closed(&mut ws, Some("browser key is not enrolled")).await;

    let impostor = Keypair::generate().unwrap();
    let mut ws = open(&h.url).await;
    assert!(
        handshake(&mut ws, &h.browser.private, &impostor.public, None)
            .await
            .is_none()
    );

    for first in [
        tungstenite::Message::Text("hello".into()),
        tungstenite::Message::Binary(vec![1; e2e::FIRST_MESSAGE_BYTES].into()),
        tungstenite::Message::Binary(b"RFB 003.008\n".to_vec().into()),
    ] {
        let mut ws = open(&h.url).await;
        ws.send(first).await.unwrap();
        assert_closed(&mut ws, None).await;
    }
    assert_no_tcp(&h.rfb).await;
}

#[tokio::test]
async fn pairing_enrolls_a_browser_once_persists_it_and_allows_reconnection() {
    let h = harness().await;
    let phone = Keypair::generate().unwrap();
    let invite = h.gateway.open_pairing().unwrap();
    assert_eq!(invite.host, h.gateway.host_public());
    let (mut ws, mut channel) = connect_as(&h, &phone.private, Some(&invite.secret)).await;
    let (mut tcp, _) = h.rfb.accept().await.unwrap();
    send_sealed(&mut ws, &mut channel, b"paired").await;
    let mut bytes = [0; 6];
    tcp.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"paired");
    ws.close(None).await.unwrap();
    assert_tcp_closed(&mut tcp).await;

    assert_eq!(h.gateway.enrolled_browsers(), 2);
    let saved = load_browser_keys(&h.dir.0.join("browsers.txt")).unwrap();
    assert_eq!(saved, vec![h.browser.public, phone.public]);

    // The invite is single use.
    let other = Keypair::generate().unwrap();
    let mut ws = open(&h.url).await;
    assert!(
        handshake(&mut ws, &other.private, &invite.host, Some(&invite.secret))
            .await
            .is_none()
    );
    assert_no_tcp(&h.rfb).await;

    let (mut ws, mut channel) = connect_as(&h, &phone.private, None).await;
    let (mut tcp, _) = h.rfb.accept().await.unwrap();
    send_sealed(&mut ws, &mut channel, b"again").await;
    let mut bytes = [0; 5];
    tcp.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"again");
}

#[tokio::test]
async fn revokes_a_browser_whose_line_is_removed_before_restart() {
    let h = harness().await;
    let revoked = Keypair::generate().unwrap();
    let invite = h.gateway.open_pairing().unwrap();
    let (mut ws, _) = connect_as(&h, &revoked.private, Some(&invite.secret)).await;
    let (mut tcp, _) = h.rfb.accept().await.unwrap();
    ws.close(None).await.unwrap();
    assert_tcp_closed(&mut tcp).await;
    assert_eq!(h.gateway.enrolled_browsers(), 2);

    // Revoke as the README says: stop the gateway, delete the line, restart.
    let Harness {
        dir,
        browser,
        _server: server,
        ..
    } = h;
    drop(server);
    let path = dir.0.join("browsers.txt");
    let line_key = e2e::encode_key(&revoked.public);
    let kept: String = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|line| !line.contains(&line_key))
        .map(|line| format!("{line}\n"))
        .collect();
    fs::write(&path, kept).unwrap();
    let h = serve(dir, browser, 7200).await;
    assert_eq!(h.gateway.enrolled_browsers(), 1);

    let mut ws = open(&h.url).await;
    assert!(
        handshake(&mut ws, &revoked.private, &h.gateway.host_public(), None)
            .await
            .is_some()
    );
    assert_closed(&mut ws, Some("browser key is not enrolled")).await;
    assert_no_tcp(&h.rfb).await;

    // The browser that stayed enrolled still connects.
    let (mut ws, mut channel) = connect(&h).await;
    let (mut tcp, _) = h.rfb.accept().await.unwrap();
    send_sealed(&mut ws, &mut channel, b"kept").await;
    let mut bytes = [0; 4];
    tcp.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"kept");
}

#[tokio::test]
async fn rejects_wrong_or_expired_pairing_secrets_without_consuming_or_opening_tcp() {
    let h = harness().await;
    let phone = Keypair::generate().unwrap();
    let invite = h.gateway.open_pairing().unwrap();
    let guess = PairingInvite::generate(invite.host).unwrap();
    let mut ws = open(&h.url).await;
    assert!(
        handshake(&mut ws, &phone.private, &invite.host, Some(&guess.secret))
            .await
            .is_some()
    );
    assert_closed(&mut ws, Some("E2E handshake failed")).await;
    assert_no_tcp(&h.rfb).await;
    assert_eq!(h.gateway.enrolled_browsers(), 1);

    // A failed guess does not consume the invite, but expiry does disable it.
    assert_eq!(h.gateway.pairing_secret(), Some(invite.secret));
    h.gateway.enrollment.lock().unwrap().invite = Some((invite.secret, Instant::now()));
    let mut ws = open(&h.url).await;
    assert!(
        handshake(&mut ws, &phone.private, &invite.host, Some(&invite.secret))
            .await
            .is_none()
    );
    assert_no_tcp(&h.rfb).await;
    assert_eq!(h.gateway.enrolled_browsers(), 1);
}

#[tokio::test]
async fn closes_on_plaintext_tampered_or_replayed_messages() {
    let h = harness().await;
    for attack in 0..3 {
        let (mut ws, mut channel) = connect(&h).await;
        let (mut tcp, _) = h.rfb.accept().await.unwrap();
        let sealed = channel.seal(b"input").unwrap();
        let injected = match attack {
            0 => b"plaintext RFB bytes".to_vec(),
            1 => {
                let mut altered = sealed.clone();
                altered[2] ^= 1;
                altered
            }
            _ => {
                ws.send(tungstenite::Message::Binary(sealed.clone().into()))
                    .await
                    .unwrap();
                let mut bytes = [0; 5];
                tcp.read_exact(&mut bytes).await.unwrap();
                sealed
            }
        };
        ws.send(tungstenite::Message::Binary(injected.into()))
            .await
            .unwrap();
        assert_closed(&mut ws, Some("E2E integrity check failed")).await;
        assert_tcp_closed(&mut tcp).await;
    }
}

#[tokio::test]
async fn times_out_an_incomplete_handshake_without_tcp() {
    let h = harness().await;
    let mut ws = open(&h.url).await;
    assert_closed(&mut ws, Some("E2E handshake timed out")).await;
    assert_no_tcp(&h.rfb).await;
}

#[tokio::test]
async fn reports_an_offline_target_after_the_handshake() {
    let Harness {
        url,
        rfb,
        gateway,
        browser,
        dir: _dir,
        _server,
    } = harness().await;
    drop(rfb);
    let mut ws = open(&url).await;
    assert!(
        handshake(&mut ws, &browser.private, &gateway.host_public(), None)
            .await
            .is_some()
    );
    assert_closed(&mut ws, Some("RFB host offline")).await;
}

#[tokio::test]
async fn rejects_second_session_and_releases_slot_after_close() {
    let h = harness().await;
    let (mut first, _) = connect(&h).await;
    let (mut tcp, _) = h.rfb.accept().await.unwrap();
    let token = fixtures::token(unix_now() + 3600);
    match connect_async(request(&h.url, Some(&token))).await {
        Err(tungstenite::Error::Http(r)) => assert_eq!(r.status(), 409),
        _ => panic!("second session was not rejected"),
    }
    first.close(None).await.unwrap();
    assert_tcp_closed(&mut tcp).await;
    // A fresh session is accepted after the previous TCP side has closed.
    let (mut second, _) = connect(&h).await;
    second.close(None).await.unwrap();
}

#[tokio::test]
async fn closes_on_text_and_oversized_messages() {
    let h = harness().await;
    for message in [
        tungstenite::Message::Text("not RFB binary".into()),
        tungstenite::Message::Binary(vec![0; MAX_CIPHERTEXT_BYTES + 1].into()),
    ] {
        let (mut ws, _) = connect(&h).await;
        let (mut tcp, _) = h.rfb.accept().await.unwrap();
        let _ = ws.send(message).await;
        let close = timeout(Duration::from_secs(2), ws.next()).await.unwrap();
        assert!(matches!(
            close,
            Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_)) | None
        ));
        assert_tcp_closed(&mut tcp).await;
    }
}

#[tokio::test]
async fn refuses_public_or_lan_rfb_targets_and_listen_addresses() {
    let dir = TempDir::new();
    provision(&dir.0);
    let mut config = config(&dir.0, "127.0.0.1:5900".parse().unwrap(), 7200);
    assert!(Gateway::new(config.clone()).is_ok());
    config.listen = "0.0.0.0:6080".parse().unwrap();
    assert!(Gateway::new(config.clone()).is_err());
    config.listen = "127.0.0.1:6080".parse().unwrap();
    config.rfb_target = "192.168.1.10:5900".parse().unwrap();
    assert!(Gateway::new(config).is_err());
}

#[test]
fn validates_identity_and_enrollment_files() {
    let dir = TempDir::new();
    let host_key = dir.0.join("host.key");
    let created = create_host_key(&host_key).unwrap();
    assert!(
        create_host_key(&host_key).is_err(),
        "must not replace a host key"
    );
    assert_eq!(load_host_key(&host_key).unwrap().public, created.public);

    let browsers = dir.0.join("browsers.txt");
    assert!(load_browser_keys(&browsers).unwrap().is_empty());
    let key = e2e::encode_key(&created.public);
    for (text, valid) in [
        (format!("# comment\n\n{key} # label\n{key}\n"), true),
        (format!("{key}\nnot-a-key\n"), false),
        (format!("{}\n", &key[1..]), false),
        (
            (0..=MAX_BROWSER_KEYS)
                .map(|_| e2e::encode_key(&Keypair::generate().unwrap().public) + "\n")
                .collect(),
            false,
        ),
        (" ".repeat(MAX_BROWSER_KEYS_FILE_BYTES as usize + 1), false),
    ] {
        fs::write(&browsers, text).unwrap();
        assert_eq!(load_browser_keys(&browsers).is_ok(), valid);
    }
    fs::write(&host_key, "not a key").unwrap();
    assert!(load_host_key(&host_key).is_err());
}

#[tokio::test]
async fn resumes_after_idle_without_using_an_expired_write_timeout() {
    let h = harness().await;
    let (mut ws, mut channel) = connect(&h).await;
    let (mut tcp, _) = h.rfb.accept().await.unwrap();
    tokio::time::sleep(Duration::from_secs(6)).await;
    tcp.write_all(b"after idle").await.unwrap();
    let Some(tungstenite::Message::Binary(sealed)) = next(&mut ws).await else {
        panic!("expected an encrypted message");
    };
    assert_eq!(channel.open(&sealed).unwrap(), b"after idle");
    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn enforces_session_lifetime_without_waiting_for_peer_activity() {
    let h = harness_with_lifetime(1).await;
    let (mut ws, _) = connect(&h).await;
    let (mut tcp, _) = h.rfb.accept().await.unwrap();
    assert_closed(&mut ws, Some("Session expired")).await;
    assert_tcp_closed(&mut tcp).await;
}

#[tokio::test]
async fn enforces_aggregate_fragment_size_before_forwarding() {
    use tungstenite::protocol::frame::{
        Frame,
        coding::{Data, OpCode},
    };
    let h = harness().await;
    let (mut ws, _) = connect(&h).await;
    let (mut tcp, _) = h.rfb.accept().await.unwrap();
    for (opcode, last) in [(Data::Binary, false), (Data::Continue, true)] {
        let _ = ws
            .send(tungstenite::Message::Frame(Frame::message(
                vec![0; MAX_CIPHERTEXT_BYTES / 2 + 1],
                OpCode::Data(opcode),
                last,
            )))
            .await;
    }
    let _ = timeout(Duration::from_secs(2), ws.next()).await.unwrap();
    assert_tcp_closed(&mut tcp).await;
}
