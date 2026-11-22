use super::*;
#[allow(dead_code)]
#[path = "../support/identity.rs"]
mod support;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use support::Identity;

struct PemDirectory(PathBuf);

impl PemDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "topvnc-viewer-certs-{}-{}",
            std::process::id(),
            getrandom::u64().unwrap()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, name: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn resolve(&self, host: &str) -> io::Result<Option<PathBuf>> {
        Ok(TlsOptions::default()
            .client_with_local_ca(host, Some(&self.0))?
            .1)
    }
}

impl Drop for PemDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn viewer_trust_matches_certificate_dns_and_ip_and_is_resolved_per_target() {
    let directory = PemDirectory::new();
    let office = directory.write("office-pc.pem", Identity::new().cert.pem());
    let other = rcgen::generate_simple_self_signed(vec!["other-pc".into(), "::1".into()]).unwrap();
    let other = directory.write("unrelated-filename.PEM", other.cert.pem());
    assert_eq!(
        directory.resolve("LOCALHOST").unwrap(),
        Some(office.clone())
    );
    assert_eq!(directory.resolve("127.0.0.1").unwrap(), Some(office));
    assert_eq!(directory.resolve("other-pc").unwrap(), Some(other.clone()));
    assert_eq!(directory.resolve("[::1]").unwrap(), Some(other));
    assert_eq!(directory.resolve("unknown.invalid").unwrap(), None);
    // The effective TLS identity can differ from the dialed address.
    let options = TlsOptions {
        server_name: Some("other-pc".into()),
        ..Default::default()
    };
    assert_eq!(
        options
            .client_with_local_ca("192.0.2.10", Some(&directory.0))
            .unwrap()
            .1,
        directory.resolve("other-pc").unwrap()
    );
    assert!(
        options.ca.is_none(),
        "do not retain trust for another target"
    );
}

#[test]
fn duplicate_certificates_are_ok_but_distinct_matches_need_an_explicit_choice() {
    let directory = PemDirectory::new();
    let identity = Identity::new();
    let first = directory.write("a.pem", identity.cert.pem());
    directory.write("b.pem", identity.cert.pem().replace('\n', "\r\n"));
    assert_eq!(directory.resolve("localhost").unwrap(), Some(first));
    directory.write("c.pem", Identity::new().cert.pem());
    let error = directory.resolve("localhost").unwrap_err().to_string();
    assert!(error.contains("Multiple certificates"));
    assert!(error.contains("--tls-ca"));
}

#[test]
fn viewer_discovery_ignores_keys_malformed_oversized_and_unrelated_files() {
    let directory = PemDirectory::new();
    let identity = Identity::new();
    directory.write("key.pem", identity.key.serialize_pem());
    directory.write(
        "combined.pem",
        format!("{}{}", identity.cert.pem(), identity.key.serialize_pem()),
    );
    directory.write(
        "malformed.pem",
        "-----BEGIN CERTIFICATE-----\ninvalid\n-----END CERTIFICATE-----",
    );
    directory.write(
        "invalid-der.pem",
        "-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----",
    );
    directory.write("empty.pem", "");
    directory.write("binary.pem", [0xff, 0xfe]);
    directory.write("wrong-extension.txt", identity.cert.pem());
    directory.write(
        "oversized.pem",
        format!(
            "{}{}",
            " ".repeat(MAX_DETECTED_PEM_BYTES as usize),
            identity.cert.pem()
        ),
    );
    std::fs::create_dir(directory.0.join("folder.pem")).unwrap();
    // A matching filename must never substitute for a matching SAN.
    let wrong = rcgen::generate_simple_self_signed(vec!["wrong.invalid".into()]).unwrap();
    directory.write("localhost.pem", wrong.cert.pem());
    assert_eq!(directory.resolve("localhost").unwrap(), None);
}

#[test]
fn explicit_ca_and_plaintext_bypass_discovery_without_masking_conflicts() {
    let directory = PemDirectory::new();
    directory.write("a.pem", Identity::new().cert.pem());
    directory.write("b.pem", Identity::new().cert.pem());
    let ca = directory.write("chosen.txt", Identity::new().cert.pem());
    let explicit = TlsOptions {
        ca: Some(ca),
        ..Default::default()
    };
    let (transport, discovered) = explicit
        .client_with_local_ca("localhost", Some(&directory.0))
        .unwrap();
    assert!(matches!(transport, ClientTransport::Tls { .. }));
    assert!(discovered.is_none());
    let plain = TlsOptions {
        unencrypted: true,
        ..Default::default()
    };
    assert!(matches!(
        plain
            .client_with_local_ca("localhost", Some(&directory.0))
            .unwrap(),
        (ClientTransport::Unencrypted, None)
    ));
    let conflict = TlsOptions {
        unencrypted: true,
        ..explicit
    };
    assert!(
        conflict
            .client_with_local_ca("localhost", Some(&directory.0))
            .is_err()
    );
}

#[test]
fn absent_directory_uses_public_roots_and_directory_errors_include_the_path() {
    let directory = PemDirectory::new();
    let missing = directory.0.join("missing");
    let (transport, discovered) = TlsOptions::default()
        .client_with_local_ca("localhost", Some(&missing))
        .unwrap();
    assert!(matches!(transport, ClientTransport::Tls { .. }));
    assert!(discovered.is_none());
    let file = directory.write("not-a-directory", "");
    let error = detect_client_ca(&file, "localhost").err().unwrap();
    assert!(
        error.to_string().contains(&*file.to_string_lossy()),
        "{error}"
    );
}

#[test]
fn viewer_discovery_limits_only_regular_pem_files() {
    let directory = PemDirectory::new();
    let matching = directory.write("host.pem", Identity::new().cert.pem());
    for index in 1..MAX_VIEWER_PEM_FILES {
        directory.write(&format!("{index}.pem"), "");
    }
    directory.write("notes.txt", "");
    std::fs::create_dir(directory.0.join("backup.pem")).unwrap();
    assert_eq!(directory.resolve("localhost").unwrap(), Some(matching));
    directory.write("extra.PEM", "");
    let error = directory.resolve("localhost").unwrap_err().to_string();
    assert!(error.contains("Too many .pem files"), "{error}");
    assert!(
        error.contains(&format!("limit {MAX_VIEWER_PEM_FILES}")),
        "{error}"
    );
    assert!(error.contains("--tls-ca"), "{error}");
}

#[test]
fn viewer_discovery_skips_a_disappearing_candidate() {
    let directory = PemDirectory::new();
    let path = directory.write("vanished.pem", Identity::new().cert.pem());
    std::fs::remove_file(&path).unwrap();
    assert!(read_viewer_pem(&path).unwrap().is_none());
}

#[cfg(windows)]
#[test]
fn viewer_discovery_reports_a_locked_candidate_by_path() {
    use std::os::windows::fs::OpenOptionsExt;

    let directory = PemDirectory::new();
    let path = directory.write("locked.pem", Identity::new().cert.pem());
    let _lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let error = directory.resolve("localhost").unwrap_err().to_string();
    assert!(error.contains(&*path.to_string_lossy()), "{error}");
    assert!(error.contains("--tls-ca FILE"), "{error}");
}

#[test]
fn viewer_tls_uses_the_inspected_certificates_after_file_replacement() {
    let directory = PemDirectory::new();
    let identity = Identity::new();
    let path = directory.write("host.pem", identity.cert.pem());
    let discovered = detect_client_ca(&directory.0, "localhost")
        .unwrap()
        .unwrap();
    directory.write("host.pem", Identity::new().cert.pem());
    let (transport, reported_path) = discovered.into_client("localhost".into()).unwrap();
    assert_eq!(reported_path, path);

    let config = identity.server_config();
    let (client, server) = socket_pair();
    let deadline = Instant::now() + Duration::from_secs(3);
    let host = thread::spawn(move || NativeStream::server(server, Some(config), deadline));
    let client = NativeStream::client(client, transport, deadline);
    assert!(client.is_ok(), "{:?}", client.err());
    assert!(host.join().unwrap().is_ok());
}

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

/// After TLS completes, sends an application record one byte at a time and
/// never its last byte: ciphertext keeps arriving, plaintext never does.
fn trickle_partial_record(peer: NativeStream, stop: Arc<AtomicBool>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut record = Vec::new();
        {
            let mut state = peer.tls.as_ref().unwrap().lock().unwrap();
            state.writer().write_all(&[0; 64]).unwrap();
            while state.wants_write() {
                state.write_tls(&mut record).unwrap();
            }
        }
        // About 85 bytes at 20 ms each outlasts the test's budget several
        // times over, and still ends if the bound regresses.
        for byte in &record[..record.len() - 1] {
            if stop.load(Ordering::Relaxed)
                || (&*peer.socket)
                    .write_all(std::slice::from_ref(byte))
                    .is_err()
            {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        // Stay connected so only the deadline, not EOF, ends the read.
        while !stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(5));
        }
    })
}

const HANDSHAKE_BUDGET: Duration = Duration::from_millis(150);

#[test]
fn handshake_deadline_bounds_trickled_tls_ciphertext_on_client_and_server() {
    for client_reads in [true, false] {
        let (client, server) = tls_pair();
        let (mut reader, peer) = if client_reads {
            (client, server)
        } else {
            (server, client)
        };
        let stop = Arc::new(AtomicBool::new(false));
        let trickler = trickle_partial_record(peer, stop.clone());
        let start = Instant::now();
        let mut handshake = crate::HandshakeStream::new(&mut reader, start + HANDSHAKE_BUDGET);
        let error = handshake.read_exact(&mut [0; 12]).unwrap_err();
        let elapsed = start.elapsed();
        drop(handshake);
        stop.store(true, Ordering::Relaxed);
        trickler.join().unwrap();
        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ),
            "{error}"
        );
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
        // The bound ends with negotiation; session timeouts take over.
        assert!(reader.deadline.is_none());
    }
}

#[test]
fn session_negotiation_times_out_on_trickled_tls_ciphertext() {
    let (client, server) = tls_pair();
    let stop = Arc::new(AtomicBool::new(false));
    let trickler = trickle_partial_record(server, stop.clone());
    let start = Instant::now();
    let result = crate::Session::from_stream_with_encoding(
        client,
        false,
        crate::Encoding::Raw,
        || panic!("no RFB greeting was decrypted"),
        HANDSHAKE_BUDGET,
    );
    let elapsed = start.elapsed();
    stop.store(true, Ordering::Relaxed);
    trickler.join().unwrap();
    let error = result.err().expect("trickled ciphertext must time out");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("handshake timed out"), "{error}");
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
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

/// A fresh, empty folder for one detection test.
fn pem_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("topvnc-pem-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const CERT_A: &str = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
const CERT_B: &str = "-----BEGIN CERTIFICATE-----\nBBBB\n-----END CERTIFICATE-----\n";
const KEY: &str = "-----BEGIN PRIVATE KEY-----\nKKKK\n-----END PRIVATE KEY-----\n";

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).unwrap();
}

#[test]
fn detects_certificate_and_key_by_pem_label() {
    let dir = pem_dir("label");
    write(&dir, "a.pem", KEY);
    write(&dir, "b.pem", CERT_A);
    write(&dir, "notes.txt", CERT_B);
    assert_eq!(
        detect_host_credentials(&dir),
        Some((dir.join("b.pem"), dir.join("a.pem")))
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn identical_certificate_copies_are_not_ambiguous() {
    let dir = pem_dir("copies");
    write(&dir, "office-pc.pem", CERT_A);
    write(&dir, "server.pem", CERT_A);
    write(&dir, "server-key.pem", KEY);
    let (cert, key) = detect_host_credentials(&dir).unwrap();
    assert_eq!(key, dir.join("server-key.pem"));
    assert_eq!(cert, dir.join("server.pem"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn certificate_named_after_the_key_wins() {
    let dir = pem_dir("named");
    write(&dir, "ca.pem", CERT_B);
    write(&dir, "host.pem", CERT_A);
    write(&dir, "host_key.pem", KEY);
    assert_eq!(
        detect_host_credentials(&dir),
        Some((dir.join("host.pem"), dir.join("host_key.pem")))
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ambiguous_or_incomplete_folders_detect_nothing() {
    let dir = pem_dir("ambiguous");
    assert_eq!(detect_host_credentials(&dir.join("missing")), None);
    assert_eq!(detect_host_credentials(&dir), None);
    write(&dir, "key.pem", KEY);
    assert_eq!(detect_host_credentials(&dir), None, "no certificate");
    write(&dir, "one.pem", CERT_A);
    write(&dir, "two.pem", CERT_B);
    assert_eq!(
        detect_host_credentials(&dir),
        None,
        "two different certificates"
    );
    std::fs::remove_file(dir.join("two.pem")).unwrap();
    write(&dir, "key2.pem", KEY);
    assert_eq!(detect_host_credentials(&dir), None, "two private keys");
    std::fs::remove_dir_all(dir).unwrap();
}
