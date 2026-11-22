use super::*;
#[allow(dead_code)]
#[path = "../support/mod.rs"]
mod support;
use support::{Host, Identity};

#[test]
fn encrypted_native_app_connection_uses_form_trust_and_keeps_password_auth() {
    let identity = Identity::new();
    let directory = std::env::temp_dir().join(format!(
        "topvnc-native-tls-{}-{}",
        std::process::id(),
        getrandom::u64().unwrap()
    ));
    std::fs::create_dir(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let ca = directory.join("ca.pem");
    std::fs::write(&ca, identity.cert.pem()).unwrap();
    let mut source = Framebuffer::new(4, 3).unwrap();
    source.pixels_mut().fill(0x123456);
    let cert = directory.join("server.pem");
    let key = directory.join("server-key.pem");
    std::fs::write(&cert, identity.cert.pem()).unwrap();
    std::fs::write(&key, identity.key.serialize_pem()).unwrap();
    let host_tls = topvnc::transport::TlsOptions {
        cert: Some(cert),
        key: Some(key),
        ..Default::default()
    };
    let host = Host::start(host_tls.server().unwrap(), source.clone());
    let mut config = Config {
        host: "127.0.0.1".into(),
        port: host.address().port().to_string(),
        password: "native-test-password".into(),
        compression: Compression::Raw,
        tls: topvnc::transport::TlsOptions {
            ca: Some(ca.clone()),
            server_name: Some("localhost".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let NativeConnection {
        mut session,
        security,
    } = connect_native_in(&config, Some(&directory)).unwrap();
    assert!(matches!(security, ui::ViewerSecurity::Certificate(path) if path == ca));
    assert_eq!(session.info.security, topvnc::Security::VncPassword);
    let writer = session.writer();
    writer.request_update(false, 4, 3).unwrap();
    let mut received = Framebuffer::new(4, 3).unwrap();
    let mut scratch = Vec::new();
    for _ in 0..8 {
        session.read_update(&mut received, &mut scratch).unwrap();
        if received.pixels() == source.pixels() {
            break;
        }
    }
    assert_eq!(received.pixels(), source.pixels());
    writer.key(0x61, true).unwrap();
    assert!(matches!(
        host.server
            .recv_event_timeout(Duration::from_secs(2))
            .unwrap(),
        topvnc::ClientEvent::Key {
            keysym: 0x61,
            down: true,
            ..
        }
    ));
    writer.shutdown().unwrap();
    config.tls.server_name = Some("wrong.invalid".into());
    assert!(connect_native_in(&config, Some(&directory)).is_err());
    config.tls.server_name = Some("localhost".into());
    // No launch flag is needed when the target certificate is present.
    config.tls.ca = None;
    let NativeConnection { session, security } =
        connect_native_in(&config, Some(&directory)).unwrap();
    assert!(matches!(security, ui::ViewerSecurity::Certificate(path) if path == ca));
    assert_eq!(session.info.security, topvnc::Security::VncPassword);
    session.writer().shutdown().unwrap();
    // Editing the identity must select trust again, never reuse the last CA.
    config.tls.server_name = Some("wrong.invalid".into());
    assert!(connect_native_in(&config, Some(&directory)).is_err());
    config.tls.server_name = None;
    let session = connect_native_in(&config, Some(&directory))
        .unwrap()
        .session;
    session.writer().shutdown().unwrap();
    // A different local certificate for the same name cannot authenticate this
    // server, and a failing attempt cannot fall back to public roots or TCP.
    let stranger = Identity::new();
    std::fs::remove_file(directory.join("ca.pem")).unwrap();
    std::fs::write(directory.join("server.pem"), stranger.cert.pem()).unwrap();
    assert!(connect_native_in(&config, Some(&directory)).is_err());
    std::fs::remove_file(directory.join("server.pem")).unwrap();
    assert!(connect_native_in(&config, Some(&directory)).is_err());
}
