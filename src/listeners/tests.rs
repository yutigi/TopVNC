use crate::{Framebuffer, ListenerSecurity, ServerConfig, Session, VncServer};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn password(password: &str) -> ListenerSecurity {
    ListenerSecurity {
        password: Some(password.into()),
        ..ListenerSecurity::default()
    }
}

fn server(password: &str) -> Arc<VncServer> {
    Arc::new(
        VncServer::bind(
            "127.0.0.1:0",
            Framebuffer::new(16, 16).unwrap(),
            ServerConfig {
                password: Some(password.into()),
                ..ServerConfig::default()
            },
        )
        .unwrap(),
    )
}

fn connect(address: std::net::SocketAddr, password: &str) -> std::io::Result<Session> {
    let password = password.to_owned();
    // Plain TCP is explicit for these loopback listeners.
    Session::connect(&address.to_string(), true, move || Ok(password.clone()))
}

fn wait_for_connections(server: &VncServer, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.active_connections() != count {
        assert!(
            Instant::now() < deadline,
            "{} connections, expected {count}",
            server.active_connections()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn each_listener_authenticates_with_its_own_password() {
    let server = server("native");
    let runner = Arc::clone(&server);
    let serving = std::thread::spawn(move || runner.run());
    let listeners = server.listeners();
    let web = listeners.add("127.0.0.1:0", password("browser")).unwrap();
    let web_address = listeners.local_addr(web).unwrap();
    let native_address = server.local_addr().unwrap();
    assert_ne!(web_address, native_address);

    let native = connect(native_address, "native").unwrap();
    let browser = connect(web_address, "browser").unwrap();
    assert!(connect(web_address, "native").is_err());
    assert!(connect(native_address, "browser").is_err());
    // Both viewers see the one framebuffer.
    assert_eq!(
        (native.info.width, native.info.height),
        (browser.info.width, browser.info.height)
    );
    wait_for_connections(&server, 2);
    drop((native, browser));
    server.stop();
    serving.join().unwrap().unwrap();
}

#[test]
fn removing_a_listener_ends_only_its_viewers() {
    let server = server("native");
    let runner = Arc::clone(&server);
    let serving = std::thread::spawn(move || runner.run());
    let listeners = server.listeners();
    let web = listeners.add("127.0.0.1:0", password("browser")).unwrap();
    let web_address = listeners.local_addr(web).unwrap();
    let _native = connect(server.local_addr().unwrap(), "native").unwrap();
    let _browser = connect(web_address, "browser").unwrap();
    wait_for_connections(&server, 2);

    assert!(listeners.remove(web));
    assert!(!listeners.remove(web), "already removed");
    assert_eq!(listeners.local_addr(web), None);
    wait_for_connections(&server, 1);
    assert!(connect(web_address, "browser").is_err());
    // The first listener still takes viewers.
    let _again = connect(server.local_addr().unwrap(), "native").unwrap();
    wait_for_connections(&server, 2);
    server.stop();
    serving.join().unwrap().unwrap();
}

#[test]
fn the_first_listener_can_close_while_others_serve() {
    let server = server("native");
    let runner = Arc::clone(&server);
    let serving = std::thread::spawn(move || runner.run());
    let listeners = server.listeners();
    let first = listeners.primary().unwrap();
    let web = listeners.add("127.0.0.1:0", password("browser")).unwrap();
    assert!(listeners.remove(first));
    assert_eq!(listeners.primary(), None);
    assert!(server.local_addr().is_err());
    let _browser = connect(listeners.local_addr(web).unwrap(), "browser").unwrap();
    wait_for_connections(&server, 1);
    server.stop();
    serving.join().unwrap().unwrap();
}

#[test]
fn listeners_need_a_password_or_explicit_insecurity() {
    let server = server("native");
    let listeners = server.listeners();
    for security in [ListenerSecurity::default(), password("")] {
        assert!(listeners.add("127.0.0.1:0", security).is_err());
    }
    let open = ListenerSecurity {
        allow_insecure: true,
        ..ListenerSecurity::default()
    };
    assert!(listeners.add("127.0.0.1:0", open).is_ok());
}

#[test]
fn a_stopped_server_frees_its_ports_and_takes_no_listeners() {
    let server = server("native");
    let listeners = server.listeners();
    let web = listeners.add("127.0.0.1:0", password("browser")).unwrap();
    let web_address = listeners.local_addr(web).unwrap();
    server.stop();
    assert!(listeners.add("127.0.0.1:0", password("browser")).is_err());
    assert_eq!(listeners.local_addr(web), None);
    // The handle outlives the server without holding its ports.
    drop(server);
    std::net::TcpListener::bind(web_address).unwrap();
}

#[test]
fn dropping_the_server_closes_listeners_that_handles_still_share() {
    let server = server("native");
    let listeners = server.listeners();
    let address = server.local_addr().unwrap();
    drop(server);
    assert_eq!(listeners.primary(), None);
    std::net::TcpListener::bind(address).unwrap();
}

#[test]
fn handles_compare_by_the_set_they_share() {
    let one = server("native");
    let two = server("native");
    assert_eq!(one.listeners(), one.listeners());
    assert_ne!(one.listeners(), two.listeners());
}
