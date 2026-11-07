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

#[test]
fn supervised_protocol_pairs_and_stops_on_stdin_eof() {
    let dir = scratch();
    // Exercise the shared-website link through the actual supervised process.
    let config_path = dir.join("gateway.toml");
    let mut config = std::fs::read_to_string(&config_path).unwrap();
    config.push_str("host_id = \"gaming-pc\"\n");
    std::fs::write(&config_path, config).unwrap();
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
    assert!(
        rest.starts_with(&format!("{ORIGIN}/?host=gaming-pc#pair=")),
        "{pair}"
    );
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
