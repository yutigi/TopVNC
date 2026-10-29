use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{self, client::IntoClientRequest},
};
use topvnc_spike_policy::fixtures;

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

async fn harness() -> (String, TcpListener, tokio::task::JoinHandle<()>) {
    harness_with_lifetime(7200).await
}

async fn harness_with_lifetime(
    max_session_seconds: u64,
) -> (String, TcpListener, tokio::task::JoinHandle<()>) {
    let rfb = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let policy = fixtures::policy();
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        rfb_target: rfb.local_addr().unwrap(),
        app_origin: fixtures::ORIGIN.into(),
        issuer: policy.issuer,
        audience: policy.audience,
        owner_email: policy.owner_email,
        max_session_seconds,
    };
    let state = Gateway::new(config).unwrap();
    {
        let mut cache = state.keys.lock().await;
        cache.set = Some(KeySet::parse(fixtures::jwks().as_bytes()).unwrap());
        cache.loaded_at = Some(Instant::now());
        cache.last_attempt = Some(Instant::now());
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, routes(state)).await.unwrap();
    });
    (url, rfb, task)
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

async fn connect(
    url: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>> {
    let token = fixtures::token(unix_now() + 3600);
    let (ws, response) = connect_async(request(url, Some(&token))).await.unwrap();
    assert_eq!(response.headers()["sec-websocket-protocol"], "binary");
    ws
}

#[tokio::test]
async fn denies_unauthenticated_expired_wrong_origin_and_injected_destination_without_tcp() {
    let (url, rfb, task) = harness().await;
    let good = fixtures::token(unix_now() + 3600);
    let expired = fixtures::token(unix_now() - 1);
    let mut wrong_origin = request(&url, Some(&good));
    wrong_origin
        .headers_mut()
        .insert("origin", "https://attacker.example".parse().unwrap());
    for req in [
        request(&url, None),
        request(&url, Some("invalid")),
        request(&url, Some(&expired)),
        wrong_origin,
        request(&format!("{url}?target=127.0.0.1:22"), Some(&good)),
    ] {
        assert!(connect_async(req).await.is_err());
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rfb.accept())
            .await
            .is_err()
    );
    task.abort();
}

#[tokio::test]
async fn relays_ordered_binary_stream_and_propagates_tcp_close() {
    let (url, rfb, task) = harness().await;
    let mut ws = connect(&url).await;
    let (mut tcp, _) = rfb.accept().await.unwrap();
    tcp.write_all(b"RFB 003.008\n").await.unwrap();
    assert_eq!(
        ws.next().await.unwrap().unwrap().into_data().as_ref(),
        b"RFB 003.008\n"
    );
    ws.send(tungstenite::Message::Binary(b"first".to_vec().into()))
        .await
        .unwrap();
    ws.send(tungstenite::Message::Binary(b"second".to_vec().into()))
        .await
        .unwrap();
    let mut bytes = [0; 11];
    tcp.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"firstsecond");
    drop(tcp);
    let close = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(close.is_close());
    task.abort();
}

#[tokio::test]
async fn rejects_second_session_and_releases_slot_after_close() {
    let (url, rfb, task) = harness().await;
    let mut first = connect(&url).await;
    let (mut tcp, _) = rfb.accept().await.unwrap();
    let token = fixtures::token(unix_now() + 3600);
    let result = connect_async(request(&url, Some(&token))).await;
    match result {
        Err(tungstenite::Error::Http(r)) => assert_eq!(r.status(), 409),
        _ => panic!("second session was not rejected"),
    }
    first.close(None).await.unwrap();
    let mut b = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), tcp.read(&mut b))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    // A fresh session is accepted after the previous TCP side has closed.
    let mut second = connect(&url).await;
    second.close(None).await.unwrap();
    task.abort();
}

#[tokio::test]
async fn closes_on_text_and_oversized_messages() {
    let (url, rfb, task) = harness().await;
    for message in [
        tungstenite::Message::Text("not RFB binary".into()),
        tungstenite::Message::Binary(vec![0; MAX_MESSAGE_BYTES + 1].into()),
    ] {
        let mut ws = connect(&url).await;
        let (mut tcp, _) = rfb.accept().await.unwrap();
        let _ = ws.send(message).await;
        let close = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap();
        assert!(matches!(
            close,
            Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_)) | None
        ));
        let mut b = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), tcp.read(&mut b))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }
    task.abort();
}

#[test]
fn refuses_public_or_lan_rfb_targets_and_listen_addresses() {
    let p = fixtures::policy();
    let mut config = Config {
        listen: "0.0.0.0:6080".parse().unwrap(),
        rfb_target: "127.0.0.1:5900".parse().unwrap(),
        app_origin: fixtures::ORIGIN.into(),
        issuer: p.issuer,
        audience: p.audience,
        owner_email: p.owner_email,
        max_session_seconds: 7200,
    };
    assert!(Gateway::new(config.clone()).is_err());
    config.listen = "127.0.0.1:6080".parse().unwrap();
    config.rfb_target = "192.168.1.10:5900".parse().unwrap();
    assert!(Gateway::new(config).is_err());
}

#[tokio::test]
async fn resumes_after_idle_without_using_an_expired_write_timeout() {
    let (url, rfb, task) = harness().await;
    let mut ws = connect(&url).await;
    let (mut tcp, _) = rfb.accept().await.unwrap();
    tokio::time::sleep(Duration::from_secs(6)).await;
    tcp.write_all(b"after idle").await.unwrap();
    let message = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(message.into_data().as_ref(), b"after idle");
    ws.close(None).await.unwrap();
    task.abort();
}

#[tokio::test]
async fn enforces_session_lifetime_without_waiting_for_peer_activity() {
    let (url, rfb, task) = harness_with_lifetime(1).await;
    let mut ws = connect(&url).await;
    let (mut tcp, _) = rfb.accept().await.unwrap();
    let message = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(message.is_close());
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), tcp.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    task.abort();
}

#[tokio::test]
async fn enforces_aggregate_fragment_size_before_forwarding() {
    use tungstenite::protocol::frame::{
        Frame,
        coding::{Data, OpCode},
    };
    let (url, rfb, task) = harness().await;
    let mut ws = connect(&url).await;
    let (mut tcp, _) = rfb.accept().await.unwrap();
    for (opcode, last) in [(Data::Binary, false), (Data::Continue, true)] {
        let _ = ws
            .send(tungstenite::Message::Frame(Frame::message(
                vec![0; MAX_MESSAGE_BYTES / 2 + 1],
                OpCode::Data(opcode),
                last,
            )))
            .await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap();
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), tcp.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    task.abort();
}
