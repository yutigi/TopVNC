use super::*;
#[allow(dead_code)]
#[path = "../support/identity.rs"]
mod support;
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use support::Identity;

fn socket_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    (client, listener.accept().unwrap().0)
}

fn tls_pair() -> (NativeStream, NativeStream) {
    let identity = Identity::new();
    let config = identity.server_config();
    let (client, server) = socket_pair();
    let deadline = Instant::now() + Duration::from_secs(3);
    let host = thread::spawn(move || NativeStream::server(server, Some(config), deadline).unwrap());
    let client = NativeStream::client(client, identity.client("localhost"), deadline).unwrap();
    (client, host.join().unwrap())
}

#[test]
fn stalled_tls_handshake_has_one_deadline() {
    let (client, _silent_peer) = socket_pair();
    let start = Instant::now();
    let error = NativeStream::client(
        client,
        Identity::new().client("localhost"),
        start + Duration::from_millis(80),
    )
    .err()
    .unwrap();
    assert!(matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ));
    assert!(start.elapsed() < Duration::from_secs(2));
}

fn handshake_against_peer_sending(greeting: &'static [u8]) -> io::Error {
    let (client, mut peer) = socket_pair();
    let writer = thread::spawn(move || {
        peer.write_all(greeting).unwrap();
        // Hold the socket open so only the greeting decides the outcome.
        let mut sink = [0; 1024];
        while matches!(peer.read(&mut sink), Ok(n) if n > 0) {}
    });
    let error = NativeStream::client(
        client,
        Identity::new().client("localhost"),
        Instant::now() + Duration::from_secs(3),
    )
    .err()
    .unwrap();
    writer.join().unwrap();
    error
}

#[test]
fn plaintext_rfb_server_is_named_instead_of_a_corrupt_record() {
    let error = handshake_against_peer_sending(b"RFB 003.008\n");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(error.to_string(), PLAINTEXT_SERVER);
}

#[test]
fn other_non_tls_replies_keep_the_tls_error() {
    let error = handshake_against_peer_sending(b"HTTP/1.1 400 Bad Request\r\n\r\n");
    assert_ne!(error.to_string(), PLAINTEXT_SERVER);
}

#[test]
fn idle_tls_reader_does_not_block_input_writer_and_shutdown_wakes_reader() {
    let (mut client, mut server) = tls_pair();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut reader = client.try_clone().unwrap();
    let (started, ready) = mpsc::channel();
    let (read_frame, frame_received) = mpsc::channel();
    let waiting = thread::spawn(move || {
        started.send(()).unwrap();
        let mut reply = [0; 5];
        reader.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"frame");
        read_frame.send(()).unwrap();
        reader.read(&mut reply)
    });
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    thread::sleep(Duration::from_millis(20));
    client.write_all(b"input").unwrap();
    let mut input = [0; 5];
    server.read_exact(&mut input).unwrap();
    assert_eq!(&input, b"input");
    server.write_all(b"frame").unwrap();
    frame_received.recv_timeout(Duration::from_secs(2)).unwrap();
    client.shutdown(Shutdown::Both).unwrap();
    // Any buffered frame is readable first; the following read must terminate.
    assert!(waiting.join().unwrap().is_err());
}

#[test]
fn partial_tls_record_survives_read_timeout() {
    let (mut client, server) = tls_pair();
    client
        .set_read_timeout(Some(Duration::from_millis(80)))
        .unwrap();
    let mut record = Vec::new();
    {
        let mut state = server.tls.as_ref().unwrap().lock().unwrap();
        state.writer().write_all(b"fragmented frame").unwrap();
        while state.wants_write() {
            state.write_tls(&mut record).unwrap();
        }
    }
    (&*server.socket).write_all(&record[..3]).unwrap();
    let mut received = [0; 16];
    let error = client.read(&mut received).unwrap_err();
    assert!(matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ));
    (&*server.socket).write_all(&record[3..]).unwrap();
    client.read_exact(&mut received).unwrap();
    assert_eq!(&received, b"fragmented frame");
}

#[test]
fn tampered_ciphertext_is_terminal() {
    let (mut client, server) = tls_pair();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut record = Vec::new();
    {
        let mut state = server.tls.as_ref().unwrap().lock().unwrap();
        state.writer().write_all(b"secret frame").unwrap();
        while state.wants_write() {
            state.write_tls(&mut record).unwrap();
        }
    }
    *record.last_mut().unwrap() ^= 1;
    (&*server.socket).write_all(&record).unwrap();
    let error = client.read(&mut [0; 64]).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(client.write_all(b"input").is_err());
}

#[test]
fn native_tls_defaults_and_overrides_are_explicit() {
    let default = TlsOptions::default();
    assert!(
        matches!(default.client("[::1]").unwrap(), ClientTransport::Tls { server_name, .. } if server_name == "::1")
    );
    assert!(
        default
            .server()
            .unwrap_err()
            .to_string()
            .contains("--tls-cert")
    );
    let mut plain = TlsOptions {
        unencrypted: true,
        ..Default::default()
    };
    assert!(matches!(
        plain.client("localhost").unwrap(),
        ClientTransport::Unencrypted
    ));
    assert!(plain.server().unwrap().is_none());
    plain.ca = Some("ca.pem".into());
    assert!(plain.client("localhost").is_err());
    plain.cert = Some("cert.pem".into());
    assert!(plain.server().is_err());
}
