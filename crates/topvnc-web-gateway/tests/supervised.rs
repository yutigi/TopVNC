//! Runs the real gateway binary in `--supervised` mode, as the TopVNC window
//! does, and checks its line protocol and shutdown on stdin end-of-file.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const ORIGIN: &str = "https://topvnc.example.workers.dev";

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "topvnc-supervised-{}-{}",
        std::process::id(),
        topvnc_web_gateway::unix_now()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("gateway.toml"),
        format!(
            "listen = \"127.0.0.1:0\"\nrfb_target = \"127.0.0.1:5900\"\napp_origin = \"{ORIGIN}\"\n\
             issuer = \"https://team.cloudflareaccess.com\"\naudience = \"aud\"\n\
             owner_email = \"owner@example.com\"\nmax_session_seconds = 60\n\
             host_key_file = \"host.key\"\nbrowser_keys_file = \"browsers.txt\"\n"
        ),
    )
    .unwrap();
    topvnc_web_gateway::create_host_key(&dir.join("host.key")).unwrap();
    dir
}

fn next_line(lines: &mpsc::Receiver<String>) -> String {
    lines
        .recv_timeout(Duration::from_secs(20))
        .expect("a stdout line from the supervised gateway")
}

const QUICK: &str = "https://seasonal-deck-organisms-sf.trycloudflare.com";

/// A fake web/dist beside a fresh scratch directory.
fn quick_scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "topvnc-supervised-{name}-{}-{}",
        std::process::id(),
        topvnc_web_gateway::unix_now()
    ));
    let dist = dir.join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    for (_, name, _) in topvnc_web_gateway::assets::PAGE_FILES {
        std::fs::write(dist.join(name), format!("contents of {name}")).unwrap();
    }
    dir
}

/// One plain HTTP/1.0 GET; returns the whole response.
fn http_get(address: &str, path: &str) -> String {
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(stream, "GET {path} HTTP/1.0\r\nHost: {address}\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn wait_for_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
    let began = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if began.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            panic!("the supervised gateway did not stop");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn quick_tunnel_mode_serves_the_page_pairs_and_stops_on_stdin_eof() {
    let dir = quick_scratch("quick");
    let mut child = Command::new(env!("CARGO_BIN_EXE_topvnc-web-gateway"))
        .args(["--supervised-quick-tunnel", "127.0.0.1:0", "127.0.0.1:5900"])
        .arg(dir.join("dist"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut stderr = child.stderr.take().unwrap();
    let log = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });

    let bound = next_line(&lines);
    let bound_address = bound
        .strip_prefix("bound ")
        .expect("bound before publication");
    assert!(
        std::net::TcpListener::bind(bound_address).is_err(),
        "the child retains its listener"
    );
    // The socket is held but no HTTP is admitted before setting the origin.
    let mut premature = std::net::TcpStream::connect(bound_address).unwrap();
    premature
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    write!(premature, "GET / HTTP/1.0\r\nHost: localhost\r\n\r\n").unwrap();
    let mut byte = [0];
    assert!(premature.read(&mut byte).is_err());
    drop(premature);
    assert!(lines.try_recv().is_err(), "not Ready without the origin");
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "origin {QUICK}").unwrap();
    stdin.flush().unwrap();
    let fingerprint = next_line(&lines);
    assert!(fingerprint.starts_with("fingerprint "), "{fingerprint}");
    let ready = next_line(&lines);
    let address = ready.strip_prefix("ready ").expect("ready line").to_owned();
    assert_eq!(
        address, bound_address,
        "the same listener is served without rebinding"
    );
    let page = http_get(&address, "/");
    assert!(page.starts_with("HTTP/1.0 200"), "{page}");
    assert!(
        page.contains("content-security-policy: default-src 'self'"),
        "{page}"
    );
    assert!(page.ends_with("contents of index.html"), "{page}");
    assert!(http_get(&address, "/host.key").starts_with("HTTP/1.0 404"));

    writeln!(stdin, "pair").unwrap();
    stdin.flush().unwrap();
    let pair = next_line(&lines);
    let rest = pair.strip_prefix("pair 600 ").expect("pair line");
    assert!(rest.starts_with(&format!("{QUICK}/#pair=")), "{pair}");

    // Closing stdin is the supervisor's stop signal.
    drop(stdin);
    let status = wait_for_exit(&mut child);
    assert!(status.success(), "{status}");
    let log = log.join().unwrap();
    assert!(log.contains("Quick Tunnel: no Access sign-in"), "{log}");
    assert!(log.contains("a new key for this run only"), "{log}");
    assert!(!log.contains("#pair="), "the link must not reach stderr");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn quick_tunnel_mode_refuses_other_origins_and_a_missing_build() {
    let dir = quick_scratch("quick-refused");
    for (origin, dist) in [
        ("https://topvnc.example.workers.dev", dir.join("dist")),
        ("https://a.trycloudflare.com/", dir.join("dist")),
        (QUICK, dir.join("missing")),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_topvnc-web-gateway"))
            .args([
                "--supervised-quick-tunnel",
                "127.0.0.1:0",
                "127.0.0.1:5900",
                origin,
            ])
            .arg(&dist)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success(), "{origin} {}", dist.display());
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("ready"),
            "{origin}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn pending_quick_gateway_refuses_occupied_ports_and_bad_origins_and_obeys_eof() {
    let dir = quick_scratch("pending-refused");
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    for (address, command, success, bound) in [
        (occupied.local_addr().unwrap().to_string(), "", false, false),
        (
            "127.0.0.1:0".into(),
            "origin https://a.workers.dev\n",
            false,
            true,
        ),
        (
            "127.0.0.1:0".into(),
            "origin https://a.trycloudflare.com/path\n",
            false,
            true,
        ),
        ("127.0.0.1:0".into(), "", true, true),
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_topvnc-web-gateway"))
            .args(["--supervised-quick-tunnel", &address, "127.0.0.1:5900"])
            .arg(dir.join("dist"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(command.as_bytes()).unwrap();
        }
        assert_eq!(wait_for_exit(&mut child).success(), success);
        let mut stdout = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        assert_eq!(stdout.contains("bound "), bound, "{stdout}");
        assert!(!stdout.contains("ready "), "{stdout}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn supervised_protocol_pairs_and_stops_on_stdin_eof() {
    let dir = scratch();
    let mut child = Command::new(env!("CARGO_BIN_EXE_topvnc-web-gateway"))
        .arg("--supervised")
        .arg(dir.join("gateway.toml"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut stderr = child.stderr.take().unwrap();
    let log = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });

    let fingerprint = next_line(&lines);
    assert!(fingerprint.starts_with("fingerprint "), "{fingerprint}");
    let ready = next_line(&lines);
    let address = ready.strip_prefix("ready ").expect("ready line");
    assert!(address.starts_with("127.0.0.1:"), "{ready}");

    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "pair").unwrap();
    let pair = next_line(&lines);
    let rest = pair.strip_prefix("pair 600 ").expect("pair line");
    assert!(rest.starts_with(&format!("{ORIGIN}/#pair=")), "{pair}");
    writeln!(stdin, "cancel-pair").unwrap();
    writeln!(stdin, "unknown").unwrap();
    stdin.flush().unwrap();

    // Closing stdin is the supervisor's stop signal.
    drop(stdin);
    let began = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if began.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            panic!("the supervised gateway did not stop after stdin closed");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "{status}");
    let log = log.join().unwrap();
    assert!(log.contains("Gateway listening on"), "{log}");
    assert!(log.contains("Pairing invite cancelled."), "{log}");
    assert!(!log.contains("#pair="), "the link must not reach stderr");
    let _ = std::fs::remove_dir_all(dir);
}
