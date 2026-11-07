use super::*;
use crate::web_access::config::{Launch, QUICK_LISTEN, QUICK_RFB_TARGET};
use std::path::PathBuf;

mod quick;
use std::sync::{Arc, Mutex};

const ROWS: [Row; 3] = [Row::Server, Row::Tunnel, Row::Gateway];

#[derive(Default)]
struct FakeState {
    events: Vec<Event>,
    sent: Vec<String>,
    input_closed: bool,
    exit: Option<String>,
    killed: bool,
    output: Vec<String>,
    /// Assert shutdown ordering when a public route uses this gateway.
    tunnel_before_close: Option<Handle>,
}

type Handle = Arc<Mutex<FakeState>>;

struct FakeChild(Handle);

impl Supervised for FakeChild {
    fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.0.lock().unwrap().events)
    }
    fn output(&self) -> Vec<String> {
        self.0.lock().unwrap().output.clone()
    }
    fn send(&mut self, command: &str) -> bool {
        let mut state = self.0.lock().unwrap();
        if state.input_closed {
            return false;
        }
        state.sent.push(command.into());
        true
    }
    fn close_input(&mut self) {
        let mut state = self.0.lock().unwrap();
        if let Some(tunnel) = &state.tunnel_before_close {
            assert!(
                tunnel.lock().unwrap().killed,
                "stop forwarding before gateway shutdown"
            );
        }
        state.input_closed = true;
    }
    fn exit(&mut self) -> Option<String> {
        self.0.lock().unwrap().exit.clone()
    }
    fn kill(&mut self) {
        let mut state = self.0.lock().unwrap();
        state.killed = true;
        state.exit.get_or_insert_with(|| "was stopped".into());
    }
}

type Spawned = (Program, PathBuf, Vec<OsString>, Handle);

#[derive(Clone, Default)]
struct FakeSpawner {
    spawned: Arc<Mutex<Vec<Spawned>>>,
    fail: Arc<Mutex<Option<String>>>,
}

impl Spawner for FakeSpawner {
    fn spawn(
        &mut self,
        program: Program,
        path: &std::path::Path,
        args: &[OsString],
    ) -> Result<Box<dyn Supervised>, String> {
        if let Some(error) = self.fail.lock().unwrap().take() {
            return Err(error);
        }
        let handle = Handle::default();
        self.spawned.lock().unwrap().push((
            program,
            path.to_path_buf(),
            args.to_vec(),
            Arc::clone(&handle),
        ));
        Ok(Box::new(FakeChild(handle)))
    }
}

impl FakeSpawner {
    fn count(&self) -> usize {
        self.spawned.lock().unwrap().len()
    }
    fn child(&self, program: Program) -> Handle {
        let spawned = self.spawned.lock().unwrap();
        let (.., handle) = spawned
            .iter()
            .rev()
            .find(|(p, ..)| *p == program)
            .expect("spawned");
        Arc::clone(handle)
    }
    fn args(&self, program: Program) -> Vec<OsString> {
        let spawned = self.spawned.lock().unwrap();
        spawned
            .iter()
            .find(|(p, ..)| *p == program)
            .map(|(_, _, args, _)| args.clone())
            .unwrap()
    }
    fn push(&self, program: Program, event: Event) {
        self.child(program).lock().unwrap().events.push(event);
    }
}

fn launch() -> Launch {
    Launch {
        listen: "127.0.0.1:6080".parse().unwrap(),
        rfb_target: "127.0.0.1:5900".parse().unwrap(),
        route: Route::Access {
            app_origin: "https://topvnc.owner.workers.dev".into(),
            gateway_config: "gateway.local.toml".into(),
            token_file: "tunnel-token.txt".into(),
        },
        gateway_program: "topvnc-web-gateway".into(),
        cloudflared: "cloudflared".into(),
    }
}

fn quick_launch() -> Launch {
    Launch {
        listen: QUICK_LISTEN,
        rfb_target: QUICK_RFB_TARGET,
        route: Route::QuickTunnel {
            web_root: "web/dist".into(),
        },
        gateway_program: "topvnc-web-gateway".into(),
        cloudflared: "cloudflared".into(),
    }
}

/// The WEB tab's listener is open.
fn ours() -> HostServer {
    HostServer::Serving
}

fn runtime() -> (WebRuntime, FakeSpawner) {
    let spawner = FakeSpawner::default();
    (WebRuntime::new(Box::new(spawner.clone())), spawner)
}

/// Start from a stopped server and bring every row to Ready.
fn running(now: Instant) -> (WebRuntime, FakeSpawner) {
    let (mut web, spawner) = runtime();
    web.start(launch(), "pw", &HostServer::Stopped, now)
        .unwrap();
    assert_eq!(
        web.poll(&HostServer::Stopped, now),
        Some(ServerRequest::Start("127.0.0.1:5900".parse().unwrap()))
    );
    assert_eq!(web.poll(&HostServer::Starting, now), None);
    web.poll(&ours(), now);
    spawner.push(Program::Tunnel, Event::Registered(0));
    web.poll(&ours(), now);
    spawner.push(Program::Gateway, Event::Fingerprint("a1d1-0000".into()));
    spawner.push(
        Program::Gateway,
        Event::GatewayReady("127.0.0.1:6080".into()),
    );
    web.poll(&ours(), now);
    for row in ROWS {
        assert_eq!(*web.row(row), RowState::Ready, "{row:?}");
    }
    (web, spawner)
}

#[test]
fn starts_server_then_tunnel_then_gateway() {
    let now = Instant::now();
    let (mut web, spawner) = runtime();
    web.start(launch(), "pw", &HostServer::Stopped, now)
        .unwrap();
    assert_eq!(*web.row(Row::Server), RowState::Starting);
    assert_eq!(*web.row(Row::Tunnel), RowState::Waiting);
    assert!(web.poll(&HostServer::Stopped, now).is_some());
    // The request is made once.
    assert_eq!(web.poll(&HostServer::Starting, now), None);
    assert_eq!(spawner.count(), 0);
    web.poll(&ours(), now);
    assert_eq!(*web.row(Row::Server), RowState::Ready);
    assert_eq!(spawner.count(), 1);
    // The token stays in its file; only the path is passed.
    assert_eq!(
        spawner.args(Program::Tunnel),
        [
            "tunnel",
            "--no-autoupdate",
            "run",
            "--token-file",
            "tunnel-token.txt"
        ]
        .map(OsString::from)
    );
    web.poll(&ours(), now);
    assert_eq!(spawner.count(), 1, "the gateway waits for the tunnel");
    spawner.push(Program::Tunnel, Event::Registered(0));
    spawner.push(Program::Tunnel, Event::Registered(1));
    web.poll(&ours(), now);
    assert_eq!(*web.row(Row::Tunnel), RowState::Ready);
    assert_eq!(web.detail(Row::Tunnel).as_deref(), Some("2 CONNECTIONS"));
    assert_eq!(
        spawner.args(Program::Gateway),
        ["--supervised", "gateway.local.toml"].map(OsString::from)
    );
    assert_eq!(*web.row(Row::Gateway), RowState::Starting);
    spawner.push(Program::Gateway, Event::Fingerprint("a1d1-0000".into()));
    spawner.push(
        Program::Gateway,
        Event::GatewayReady("127.0.0.1:6080".into()),
    );
    web.poll(&ours(), now);
    assert_eq!(*web.row(Row::Gateway), RowState::Ready);
    assert_eq!(web.fingerprint(), Some("a1d1-0000"));
    assert_eq!(web.detail(Row::Server).as_deref(), Some("127.0.0.1:5900"));
    assert_eq!(web.detail(Row::Gateway).as_deref(), Some("127.0.0.1:6080"));
}

#[test]
fn stops_gateway_then_tunnel_then_its_server() {
    let now = Instant::now();
    let (mut web, spawner) = running(now);
    let gateway = spawner.child(Program::Gateway);
    let tunnel = spawner.child(Program::Tunnel);
    gateway.lock().unwrap().output = vec!["relay closed".into()];
    web.stop();
    assert_eq!(web.poll(&ours(), now), None);
    assert!(gateway.lock().unwrap().input_closed);
    assert!(!gateway.lock().unwrap().killed);
    assert!(
        !tunnel.lock().unwrap().killed,
        "the tunnel waits for the gateway"
    );
    assert_eq!(*web.row(Row::Gateway), RowState::Stopping);
    gateway.lock().unwrap().exit = Some("exited".into());
    assert_eq!(web.poll(&ours(), now), Some(ServerRequest::Stop));
    assert!(tunnel.lock().unwrap().killed);
    assert_eq!(*web.row(Row::Server), RowState::Stopping);
    assert_eq!(web.poll(&HostServer::Stopping, now), None);
    assert!(!web.is_idle());
    assert_eq!(web.poll(&HostServer::Stopped, now), None);
    assert!(web.is_idle());
    for row in ROWS {
        assert_eq!(*web.row(row), RowState::Stopped, "{row:?}");
    }
    assert_eq!(web.output(Row::Gateway), ["relay closed"]);
}

#[test]
fn a_gateway_that_ignores_stdin_is_killed_after_the_grace_period() {
    let now = Instant::now();
    let (mut web, spawner) = running(now);
    let gateway = spawner.child(Program::Gateway);
    web.stop();
    web.poll(&ours(), now);
    web.poll(&ours(), now + GATEWAY_STOP / 2);
    assert!(!gateway.lock().unwrap().killed);
    web.poll(&ours(), now + GATEWAY_STOP);
    assert!(gateway.lock().unwrap().killed);
}

#[test]
fn starts_only_with_a_password_and_its_listener_closed() {
    let now = Instant::now();
    // Both routes need their own listener and a password, even while the
    // Server tab serves through another listener on the same capture.
    for launch in [launch(), quick_launch()] {
        // A listener left from an earlier run must close first; a server the
        // Server tab runs does not show here, so it never blocks the start.
        for (server, why) in [
            (ours(), "serving"),
            (HostServer::Starting, "starting"),
            (HostServer::Stopping, "stopping"),
        ] {
            let (mut web, spawner) = runtime();
            assert!(
                web.start(launch.clone(), "pw", &server, now).is_err(),
                "{why}"
            );
            assert!(web.is_idle(), "{why}");
            assert_eq!(web.poll(&server, now), None, "{why}");
            assert_eq!(spawner.count(), 0, "{why}");
        }
        let (mut web, spawner) = runtime();
        assert!(web.start(launch, "", &HostServer::Stopped, now).is_err());
        assert!(web.is_idle());
        assert_eq!(spawner.count(), 0);
    }
}

#[test]
fn a_silent_tunnel_fails_after_its_limit_and_blocks_the_gateway() {
    let now = Instant::now();
    let (mut web, spawner) = runtime();
    web.start(launch(), "pw", &HostServer::Stopped, now)
        .unwrap();
    web.poll(&HostServer::Stopped, now);
    web.poll(&ours(), now);
    web.poll(&ours(), now + TUNNEL_START / 2);
    assert_eq!(*web.row(Row::Tunnel), RowState::Starting);
    web.poll(&ours(), now + TUNNEL_START);
    assert!(matches!(web.row(Row::Tunnel), RowState::Failed(_)));
    assert!(spawner.child(Program::Tunnel).lock().unwrap().killed);
    assert_eq!(*web.row(Row::Gateway), RowState::Stopped);
    assert_eq!(*web.row(Row::Server), RowState::Ready, "other rows stay");
    web.poll(&ours(), now + TUNNEL_START * 2);
    assert_eq!(spawner.count(), 1, "no automatic restart");
}

#[test]
fn a_server_that_never_serves_fails_and_is_stopped() {
    let now = Instant::now();
    let (mut web, _) = runtime();
    web.start(launch(), "pw", &HostServer::Stopped, now)
        .unwrap();
    web.poll(&HostServer::Stopped, now);
    assert_eq!(
        web.poll(&HostServer::Starting, now + SERVER_START),
        Some(ServerRequest::Stop)
    );
    assert!(matches!(web.row(Row::Server), RowState::Failed(_)));
    assert_eq!(*web.row(Row::Tunnel), RowState::Stopped);
}

#[test]
fn exits_and_spawn_errors_fail_their_row() {
    let now = Instant::now();
    let (mut web, spawner) = running(now);
    spawner.child(Program::Gateway).lock().unwrap().exit = Some("exited with code 1".into());
    web.poll(&ours(), now);
    assert_eq!(
        *web.row(Row::Gateway),
        RowState::Failed("the gateway exited with code 1".into())
    );
    assert_eq!(*web.row(Row::Tunnel), RowState::Ready);

    let (mut web, spawner) = runtime();
    *spawner.fail.lock().unwrap() = Some("could not start cloudflared".into());
    web.start(launch(), "pw", &HostServer::Stopped, now)
        .unwrap();
    web.poll(&HostServer::Stopped, now);
    web.poll(&ours(), now);
    assert_eq!(
        *web.row(Row::Tunnel),
        RowState::Failed("could not start cloudflared".into())
    );
}

#[test]
fn the_server_stopping_elsewhere_fails_its_row() {
    let now = Instant::now();
    let (mut web, _) = running(now);
    web.poll(&HostServer::Stopped, now);
    assert_eq!(
        *web.row(Row::Server),
        RowState::Failed("the server stopped".into())
    );
}

#[test]
fn the_tunnel_reconnects_without_a_limit() {
    let now = Instant::now();
    let (mut web, spawner) = running(now);
    spawner.push(Program::Tunnel, Event::Lost(Some(0)));
    web.poll(&ours(), now);
    assert_eq!(*web.row(Row::Tunnel), RowState::Starting);
    assert_eq!(web.detail(Row::Tunnel).as_deref(), Some("RECONNECTING"));
    web.poll(&ours(), now + TUNNEL_START * 3);
    assert_eq!(*web.row(Row::Tunnel), RowState::Starting);
    spawner.push(Program::Tunnel, Event::Registered(3));
    web.poll(&ours(), now + TUNNEL_START * 3);
    assert_eq!(*web.row(Row::Tunnel), RowState::Ready);
    spawner.push(Program::Tunnel, Event::Lost(None));
    web.poll(&ours(), now);
    assert_eq!(*web.row(Row::Tunnel), RowState::Starting);
}

fn pair_line(spawner: &FakeSpawner) {
    spawner.push(
        Program::Gateway,
        Event::Pair {
            seconds: 600,
            link: "https://topvnc.owner.workers.dev/#pair=SECRET".into(),
        },
    );
}

#[test]
fn pairing_waits_for_the_gateway_and_ends_on_enrollment() {
    let now = Instant::now();
    let (mut web, spawner) = runtime();
    web.start(launch(), "pw", &HostServer::Stopped, now)
        .unwrap();
    web.pair().unwrap();
    assert!(web.pairing_active());
    web.poll(&HostServer::Stopped, now);
    web.poll(&ours(), now);
    spawner.push(Program::Tunnel, Event::Registered(0));
    web.poll(&ours(), now);
    assert!(
        spawner
            .child(Program::Gateway)
            .lock()
            .unwrap()
            .sent
            .is_empty()
    );
    spawner.push(
        Program::Gateway,
        Event::GatewayReady("127.0.0.1:6080".into()),
    );
    web.poll(&ours(), now);
    assert_eq!(
        spawner.child(Program::Gateway).lock().unwrap().sent,
        ["pair"]
    );
    assert!(web.pairing().is_none());
    pair_line(&spawner);
    web.poll(&ours(), now);
    assert_eq!(web.pairing().unwrap().minutes_left(now), 10);
    spawner.push(Program::Gateway, Event::Enrolled("0a0b".into()));
    web.poll(&ours(), now);
    assert!(web.pairing().is_none());
    assert!(!web.pairing_active());
    assert_eq!(web.message(), Some("Browser enrolled: key 0a0b."));
}

#[test]
fn pairing_expires_and_can_be_cancelled() {
    let now = Instant::now();
    let (mut web, spawner) = running(now);
    let gateway = spawner.child(Program::Gateway);
    web.pair().unwrap();
    assert_eq!(gateway.lock().unwrap().sent, ["pair"]);
    pair_line(&spawner);
    web.poll(&ours(), now);
    assert!(web.pairing().is_some());
    web.poll(&ours(), now + Duration::from_secs(600));
    assert!(web.pairing().is_none());
    assert_eq!(web.message(), Some("The pairing code expired."));

    web.pair().unwrap();
    pair_line(&spawner);
    web.poll(&ours(), now);
    web.cancel_pair();
    assert!(web.pairing().is_none());
    assert_eq!(
        gateway.lock().unwrap().sent,
        ["pair", "pair", "cancel-pair"]
    );

    // An invite that arrives after Cancel is withdrawn, not shown.
    web.pair().unwrap();
    web.cancel_pair();
    pair_line(&spawner);
    web.poll(&ours(), now);
    assert!(web.pairing().is_none());
    assert_eq!(
        gateway.lock().unwrap().sent.last().map(String::as_str),
        Some("cancel-pair")
    );

    web.pair().unwrap();
    spawner.push(
        Program::Gateway,
        Event::PairError("browser enrollment list is full".into()),
    );
    web.poll(&ours(), now);
    assert!(!web.pairing_active());
    assert_eq!(
        web.message(),
        Some("Pairing is not possible: browser enrollment list is full.")
    );
}

#[test]
fn stopping_or_a_gateway_failure_removes_the_code() {
    let now = Instant::now();
    let (mut web, spawner) = running(now);
    web.pair().unwrap();
    pair_line(&spawner);
    web.poll(&ours(), now);
    spawner.child(Program::Gateway).lock().unwrap().exit = Some("exited".into());
    web.poll(&ours(), now);
    assert!(web.pairing().is_none());

    let (mut web, spawner) = running(now);
    web.pair().unwrap();
    pair_line(&spawner);
    web.poll(&ours(), now);
    web.stop();
    assert!(web.pairing().is_none());
    assert!(web.pair().is_err());
}

#[test]
fn dropping_the_runtime_ends_its_children() {
    let now = Instant::now();
    let (web, spawner) = running(now);
    let gateway = spawner.child(Program::Gateway);
    let tunnel = spawner.child(Program::Tunnel);
    gateway.lock().unwrap().exit = Some("exited".into());
    drop(web);
    assert!(gateway.lock().unwrap().input_closed);
    assert!(tunnel.lock().unwrap().killed);
}
