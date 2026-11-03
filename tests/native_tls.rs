mod support;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use support::{Host, Identity};
use topvnc::transport::ClientTransport;
use topvnc::{ClientEvent, Encoding, Framebuffer, Security, Session};

/// Records the actual bytes crossing the native network boundary.
struct Recorder {
    address: SocketAddr,
    client_bytes: Arc<Mutex<Vec<u8>>>,
    server_bytes: Arc<Mutex<Vec<u8>>>,
    runner: thread::JoinHandle<()>,
}

impl Recorder {
    fn start(target: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client_bytes = Arc::new(Mutex::new(Vec::new()));
        let server_bytes = Arc::new(Mutex::new(Vec::new()));
        let client_log = client_bytes.clone();
        let server_log = server_bytes.clone();
        let runner = thread::spawn(move || {
            let (client, _) = listener.accept().unwrap();
            let server = TcpStream::connect(target).unwrap();
            for stream in [&client, &server] {
                stream
                    .set_read_timeout(Some(Duration::from_secs(15)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(15)))
                    .unwrap();
                stream.set_nodelay(true).unwrap();
            }
            fn copy(mut source: TcpStream, mut destination: TcpStream, log: Arc<Mutex<Vec<u8>>>) {
                let mut bytes = [0; 4096];
                while let Ok(count) = source.read(&mut bytes) {
                    if count == 0 {
                        break;
                    }
                    let mut log = log.lock().unwrap();
                    assert!(
                        log.len() + count < 1024 * 1024,
                        "fixture exceeded recording bound"
                    );
                    log.extend_from_slice(&bytes[..count]);
                    drop(log);
                    if destination.write_all(&bytes[..count]).is_err() {
                        break;
                    }
                }
                let _ = source.shutdown(Shutdown::Both);
                let _ = destination.shutdown(Shutdown::Both);
            }
            let other_client = client.try_clone().unwrap();
            let other_server = server.try_clone().unwrap();
            let forward = thread::spawn(move || copy(client, server, client_log));
            copy(other_server, other_client, server_log);
            forward.join().unwrap();
        });
        Self {
            address,
            client_bytes,
            server_bytes,
            runner,
        }
    }

    fn finish(self) -> (Vec<u8>, Vec<u8>) {
        self.runner.join().unwrap();
        (
            self.client_bytes.lock().unwrap().clone(),
            self.server_bytes.lock().unwrap().clone(),
        )
    }
}

fn pixels(session: &mut Session, frame: &mut Framebuffer) {
    let mut scratch = Vec::new();
    for _ in 0..12 {
        let mut changed = false;
        session
            .read_update_with(&mut scratch, |x, y, w, h, bytes| {
                changed = true;
                frame.apply_raw(x, y, w, h, bytes)
            })
            .unwrap();
        if changed {
            return;
        }
    }
    panic!("no pixel update received");
}

fn assert_tls_records(mut bytes: &[u8]) {
    assert_eq!(bytes[0], 22, "first record must be a TLS handshake");
    let mut application_records = 0;
    while !bytes.is_empty() {
        assert!(bytes.len() >= 5);
        assert!(matches!(bytes[0], 20..=23));
        assert_eq!(bytes[1], 3);
        assert!(matches!(bytes[2], 1 | 3));
        application_records += usize::from(bytes[0] == 23);
        let length = usize::from(u16::from_be_bytes([bytes[3], bytes[4]]));
        assert!(length <= 18 * 1024 && bytes.len() >= length + 5);
        bytes = &bytes[length + 5..];
    }
    assert!(application_records > 0);
}

#[test]
fn encrypted_native_session_carries_incremental_pixels_clipboard_and_input() {
    // Multiple encodings also exercise records split across large frame writes.
    for encoding in [
        Encoding::Raw,
        Encoding::Zlib,
        Encoding::Tight { quality: 6 },
    ] {
        let identity = Identity::new();
        let mut source = Framebuffer::new(192, 128).unwrap();
        for (index, pixel) in source.pixels_mut().iter_mut().enumerate() {
            *pixel = if index % 192 < 96 { 0x123456 } else { 0xabcdef };
        }
        let host = Host::start(Some(identity.server_config()), source.clone());
        let recorder = Recorder::start(host.address());
        let mut session = Session::connect_with_transport(
            &recorder.address.to_string(),
            identity.client("localhost"),
            false,
            encoding,
            || Ok("native-test-password".into()),
        )
        .unwrap();
        assert_eq!(session.info.security, Security::VncPassword);
        session.set_continuous_updates(false);
        let writer = session.writer();
        let mut received = Framebuffer::new(192, 128).unwrap();
        writer.request_update(false, 192, 128).unwrap();
        pixels(&mut session, &mut received);
        assert_eq!(received.pixels(), source.pixels());
        source.pixels_mut()[4] = 0xff0000;
        host.server.update_framebuffer(&source).unwrap();
        let clipboard = b"native TLS clipboard secret marker 0123456789";
        host.server.set_clipboard_text(clipboard).unwrap();
        writer.request_update(true, 192, 128).unwrap();
        pixels(&mut session, &mut received);
        assert_eq!(received.pixels(), source.pixels());
        writer.key(0x1234abcd, true).unwrap();
        writer.pointer(1, 19, 37).unwrap();
        assert!(matches!(
            host.server
                .recv_event_timeout(Duration::from_secs(2))
                .unwrap(),
            ClientEvent::Key {
                keysym: 0x1234abcd,
                down: true,
                ..
            }
        ));
        assert!(matches!(
            host.server
                .recv_event_timeout(Duration::from_secs(2))
                .unwrap(),
            ClientEvent::Pointer {
                buttons: 1,
                x: 19,
                y: 37,
                ..
            }
        ));
        writer.shutdown().unwrap();
        assert!(matches!(
            host.server
                .recv_event_timeout(Duration::from_secs(2))
                .unwrap(),
            ClientEvent::ClientDisconnected { .. }
        ));
        let (outgoing, incoming) = recorder.finish();
        for bytes in [&outgoing, &incoming] {
            assert_tls_records(bytes);
            for secret in [
                b"RFB 003.008\n".as_slice(),
                b"TopVNC encrypted native automation fixture",
                clipboard.as_slice(),
                &[0x56, 0x34, 0x12, 0, 0x56, 0x34, 0x12, 0],
                &[4, 1, 0, 0, 0x12, 0x34, 0xab, 0xcd],
                &[5, 1, 0, 19, 0, 37],
            ] {
                assert!(
                    !bytes.windows(secret.len()).any(|part| part == secret),
                    "plaintext leaked into network recording"
                );
            }
        }
    }
}

#[test]
fn native_tls_rejects_untrusted_identity_wrong_name_and_wrong_password() {
    let identity = Identity::new();
    let stranger = Identity::new();
    let host = Host::start(
        Some(identity.server_config()),
        Framebuffer::new(2, 2).unwrap(),
    );
    for transport in [
        stranger.client("localhost"),
        identity.client("wrong.invalid"),
    ] {
        let mut prompted = false;
        let result = Session::connect_with_transport(
            &host.address().to_string(),
            transport,
            false,
            Encoding::Raw,
            || {
                prompted = true;
                Ok("native-test-password".into())
            },
        );
        assert!(result.is_err());
        assert!(
            !prompted,
            "RFB password requested before TLS identity validation"
        );
    }
    let result = Session::connect_with_transport(
        &host.address().to_string(),
        identity.client("localhost"),
        false,
        Encoding::Raw,
        || Ok("wrong".into()),
    );
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("authentication failed")
    );
    // The same listener still accepts a valid encrypted client after failures.
    Session::connect_with_transport(
        &host.address().to_string(),
        identity.client("127.0.0.1"),
        false,
        Encoding::Raw,
        || Ok("native-test-password".into()),
    )
    .unwrap()
    .writer()
    .shutdown()
    .unwrap();
}

#[test]
fn native_tls_never_falls_back_to_a_plaintext_peer() {
    let identity = Identity::new();
    let host = Host::start(None, Framebuffer::new(2, 2).unwrap());
    let result = Session::connect_with_transport(
        &host.address().to_string(),
        identity.client("localhost"),
        false,
        Encoding::Raw,
        || panic!("password leaked to plaintext host"),
    );
    // The user sees the cause, not rustls' "corrupt message" text.
    let message = result.err().unwrap().to_string();
    assert!(message.contains("unencrypted VNC, not TLS"), "{message}");
    assert!(message.contains("--unencrypted"), "{message}");
    // Legacy interoperability is an independent, explicit choice.
    Session::connect_with_transport(
        &host.address().to_string(),
        ClientTransport::Unencrypted,
        false,
        Encoding::Raw,
        || Ok("native-test-password".into()),
    )
    .unwrap()
    .writer()
    .shutdown()
    .unwrap();
}

#[test]
fn tls_host_rejects_plain_rfb_before_any_desktop_data() {
    let identity = Identity::new();
    let host = Host::start(
        Some(identity.server_config()),
        Framebuffer::new(2, 2).unwrap(),
    );
    let mut socket = TcpStream::connect(host.address()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket.write_all(b"RFB 003.008\n").unwrap();
    let mut bytes = [0; 1024];
    match socket.read(&mut bytes) {
        Ok(count) => assert!(!bytes[..count].windows(4).any(|part| part == b"RFB ")),
        Err(error) => assert!(!matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        )),
    }
}

#[test]
fn native_tls_rejects_expired_certificates_and_does_not_enable_none_authentication() {
    let expired = Identity::expired();
    let host = Host::start(
        Some(expired.server_config()),
        Framebuffer::new(2, 2).unwrap(),
    );
    assert!(
        Session::connect_with_transport(
            &host.address().to_string(),
            expired.client("localhost"),
            false,
            Encoding::Raw,
            || panic!("password requested for expired TLS identity")
        )
        .is_err()
    );

    let identity = Identity::new();
    let host = Host::configured(
        Framebuffer::new(2, 2).unwrap(),
        topvnc::ServerConfig {
            tls: Some(identity.server_config()),
            allow_insecure: true,
            ..Default::default()
        },
    );
    assert!(
        Session::connect_with_transport(
            &host.address().to_string(),
            identity.client("localhost"),
            false,
            Encoding::Raw,
            || panic!("None security must not ask for a password")
        )
        .is_err()
    );
    Session::connect_with_transport(
        &host.address().to_string(),
        identity.client("localhost"),
        true,
        Encoding::Raw,
        || panic!("None security must not ask for a password"),
    )
    .unwrap()
    .writer()
    .shutdown()
    .unwrap();
}
