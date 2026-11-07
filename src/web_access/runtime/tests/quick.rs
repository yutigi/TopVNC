//! The Quick Tunnel route, with simulated cloudflared and gateway programs.

use super::*;

const URL: &str = "https://seasonal-deck-organisms-sf.trycloudflare.com";

/// Start the gateway first; no public route exists until it owns the port.
fn gateway_started(now: Instant) -> (WebRuntime, FakeSpawner) {
    let (mut web, spawner) = runtime();
    web.start(quick_launch(), "pw", &HostServer::Stopped, now)
        .unwrap();
    assert_eq!(
        web.poll(&HostServer::Stopped, now),
        Some(ServerRequest::Start(QUICK_RFB_TARGET))
    );
    web.poll(&ours(), now);
    assert_eq!(*web.row(Row::Gateway), RowState::Starting);
    assert_eq!(*web.row(Row::Tunnel), RowState::Waiting);
    assert_eq!(spawner.count(), 1);
    (web, spawner)
}

fn tunnel_started(now: Instant) -> (WebRuntime, FakeSpawner) {
    let (mut web, spawner) = gateway_started(now);
    spawner.push(
        Program::Gateway,
        Event::GatewayBound(QUICK_LISTEN.to_string()),
    );
    web.poll(&ours(), now);
    assert_eq!(*web.row(Row::Tunnel), RowState::Starting);
    assert_eq!(spawner.count(), 2);
    (web, spawner)
}

#[test]
fn binds_gateway_then_starts_isolated_tunnel_and_sends_the_origin_once() {
    let now = Instant::now();
    let (mut web, spawner) = tunnel_started(now);
    assert_eq!(web.route(), Some(RouteKind::QuickTunnel));
    // No user config or token file: exactly one application-owned ingress.
    let args = spawner.args(Program::Tunnel);
    assert_eq!(
        &args[..3],
        ["tunnel", "--no-autoupdate", "--config"].map(OsString::from)
    );
    let config_path = PathBuf::from(&args[3]);
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        "ingress:\n  - service: http://127.0.0.1:6080\n"
    );
    assert_eq!(
        &args[4..],
        ["--url", "http://127.0.0.1:6080"].map(OsString::from)
    );
    assert_eq!(web.app_origin(), None);

    // A registered connection alone is not enough: there is no page origin.
    spawner.push(Program::Tunnel, Event::Registered(0));
    web.poll(&ours(), now);
    assert_eq!(*web.row(Row::Tunnel), RowState::Starting);
    assert_eq!(spawner.count(), 2, "the bound gateway waits for the URL");
    assert!(
        spawner
            .child(Program::Gateway)
            .lock()
            .unwrap()
            .sent
            .is_empty()
    );

    spawner.push(Program::Tunnel, Event::QuickTunnelUrl(URL.into()));
    // A later URL line never replaces the first.
    spawner.push(
        Program::Tunnel,
        Event::QuickTunnelUrl("https://other.trycloudflare.com".into()),
    );
    web.poll(&ours(), now);
    assert_eq!(*web.row(Row::Tunnel), RowState::Ready);
    assert_eq!(web.app_origin(), Some(URL));
    assert_eq!(
        spawner.args(Program::Gateway),
        [
            "--supervised-quick-tunnel",
            "127.0.0.1:6080",
            "127.0.0.1:5900",
            "web/dist"
        ]
        .map(OsString::from)
    );
    spawner.push(Program::Gateway, Event::Fingerprint("a1d1-0000".into()));
    spawner.push(
        Program::Gateway,
        Event::GatewayReady("127.0.0.1:6080".into()),
    );
    web.poll(&ours(), now);
    assert_eq!(*web.row(Row::Gateway), RowState::Ready);

    // Pairing works as on the Access route; the link is the temporary URL.
    web.pair().unwrap();
    assert_eq!(
        spawner.child(Program::Gateway).lock().unwrap().sent,
        [format!("origin {URL}"), "pair".into()]
    );
    spawner.push(
        Program::Gateway,
        Event::Pair {
            seconds: 600,
            link: format!("{URL}/#pair=SECRET"),
        },
    );
    web.poll(&ours(), now);
    assert!(web.pairing().is_some());
    web.stop();
    web.poll(&ours(), now);
    assert!(
        !config_path.exists(),
        "config lives exactly as long as its tunnel"
    );
}

#[test]
fn the_url_may_come_after_the_connection_and_ends_with_the_run() {
    let now = Instant::now();
    let (mut web, spawner) = tunnel_started(now);
    spawner.push(Program::Tunnel, Event::QuickTunnelUrl(URL.into()));
    web.poll(&ours(), now);
    assert_eq!(*web.row(Row::Tunnel), RowState::Starting);
    assert_eq!(web.app_origin(), Some(URL), "shown while connecting");
    spawner.push(Program::Tunnel, Event::Registered(0));
    web.poll(&ours(), now);
    assert_eq!(*web.row(Row::Tunnel), RowState::Ready);

    web.stop();
    let gateway = spawner.child(Program::Gateway);
    web.poll(&ours(), now);
    gateway.lock().unwrap().exit = Some("exited".into());
    assert_eq!(web.poll(&ours(), now), Some(ServerRequest::Stop));
    web.poll(&HostServer::Stopped, now);
    assert!(web.is_idle());
    assert!(spawner.child(Program::Tunnel).lock().unwrap().killed);
    // The old URL is dead; it is not offered again.
    assert_eq!(web.app_origin(), None);
    assert_eq!(web.route(), Some(RouteKind::QuickTunnel));
}

#[test]
fn a_tunnel_that_prints_no_url_fails_and_stops_the_bound_gateway() {
    let now = Instant::now();
    let (mut web, spawner) = tunnel_started(now);
    spawner.push(Program::Tunnel, Event::Registered(0));
    web.poll(&ours(), now + TUNNEL_START);
    assert_eq!(
        *web.row(Row::Tunnel),
        RowState::Failed("cloudflared printed no Quick Tunnel URL in time".into())
    );
    assert!(spawner.child(Program::Tunnel).lock().unwrap().killed);
    assert_eq!(*web.row(Row::Gateway), RowState::Stopping);
    assert!(spawner.child(Program::Gateway).lock().unwrap().input_closed);
    assert_eq!(spawner.count(), 2);
    assert_eq!(web.app_origin(), None);
}

#[test]
fn gateway_bind_failure_or_timeout_never_starts_a_tunnel() {
    for timeout in [false, true] {
        let now = Instant::now();
        let (mut web, spawner) = gateway_started(now);
        if !timeout {
            spawner.child(Program::Gateway).lock().unwrap().exit =
                Some("exited with code 1".into());
        }
        web.poll(&ours(), now + GATEWAY_START);
        assert!(matches!(web.row(Row::Gateway), RowState::Failed(_)));
        assert_eq!(spawner.count(), 1);
        assert_eq!(web.app_origin(), None);
        web.poll(&ours(), now + GATEWAY_START + GATEWAY_STOP);
    }
}

#[test]
fn gateway_failure_and_stop_kill_forwarding_before_releasing_the_listener() {
    for failed in [false, true] {
        let now = Instant::now();
        let (mut web, spawner) = tunnel_started(now);
        spawner
            .child(Program::Gateway)
            .lock()
            .unwrap()
            .tunnel_before_close = Some(spawner.child(Program::Tunnel));
        spawner.push(Program::Tunnel, Event::QuickTunnelUrl(URL.into()));
        spawner.push(Program::Tunnel, Event::Registered(0));
        web.poll(&ours(), now);
        spawner.push(
            Program::Gateway,
            Event::GatewayReady(QUICK_LISTEN.to_string()),
        );
        web.poll(&ours(), now);
        assert_eq!(*web.row(Row::Gateway), RowState::Ready);
        if failed {
            spawner.child(Program::Gateway).lock().unwrap().exit =
                Some("exited with code 1".into());
        } else {
            web.stop();
        }
        web.poll(&ours(), now);
        assert!(spawner.child(Program::Tunnel).lock().unwrap().killed);
        assert!(spawner.child(Program::Gateway).lock().unwrap().input_closed);
        assert_eq!(web.app_origin(), None);
        web.poll(&ours(), now + GATEWAY_STOP);
        web.poll(&HostServer::Stopped, now + GATEWAY_STOP);
        assert!(web.is_idle());
    }
}

#[test]
fn bound_gateway_waits_for_tunnel_then_has_a_separate_ready_deadline() {
    let now = Instant::now();
    let (mut web, spawner) = tunnel_started(now);
    web.poll(&ours(), now + GATEWAY_START * 2);
    assert_eq!(*web.row(Row::Gateway), RowState::Starting);
    assert!(!spawner.child(Program::Gateway).lock().unwrap().input_closed);
    spawner.push(Program::Tunnel, Event::QuickTunnelUrl(URL.into()));
    spawner.push(Program::Tunnel, Event::Registered(0));
    web.poll(&ours(), now + GATEWAY_START * 2);
    web.poll(&ours(), now + GATEWAY_START * 3);
    assert!(matches!(web.row(Row::Gateway), RowState::Failed(_)));
    assert!(spawner.child(Program::Tunnel).lock().unwrap().killed);
    // The gateway still holds its socket during graceful shutdown.
    assert!(!spawner.child(Program::Gateway).lock().unwrap().killed);
    web.poll(&ours(), now + GATEWAY_START * 3 + GATEWAY_STOP);
}

#[test]
fn returning_to_landing_restores_active_route_and_copy_button() {
    let now = Instant::now();
    let (mut web, spawner) = tunnel_started(now);
    spawner.push(Program::Tunnel, Event::QuickTunnelUrl(URL.into()));
    spawner.push(Program::Tunnel, Event::Registered(0));
    web.poll(&ours(), now);
    let state = crate::ui::UiState::for_landing(None, &web);
    assert_eq!(state.web_route, RouteKind::QuickTunnel);
    assert!(crate::ui::web_copy_url_shown(&state, &web));
    web.stop();
    web.poll(&ours(), now);
    web.poll(&ours(), now + GATEWAY_STOP);
    web.poll(&HostServer::Stopped, now + GATEWAY_STOP);
    assert_eq!(
        crate::ui::UiState::for_landing(None, &web).web_route,
        RouteKind::Access
    );
}

#[test]
fn unexpected_bound_address_never_publishes_a_route() {
    let now = Instant::now();
    let (mut web, spawner) = gateway_started(now);
    spawner.push(
        Program::Gateway,
        Event::GatewayBound("127.0.0.1:18080".into()),
    );
    web.poll(&ours(), now);
    assert!(matches!(web.row(Row::Gateway), RowState::Failed(_)));
    assert_eq!(spawner.count(), 1);
    web.poll(&ours(), now + GATEWAY_STOP);
}

#[test]
fn an_access_run_ignores_quick_tunnel_lines() {
    let now = Instant::now();
    let (mut web, spawner) = runtime();
    web.start(launch(), "pw", &HostServer::Stopped, now)
        .unwrap();
    web.poll(&HostServer::Stopped, now);
    web.poll(&ours(), now);
    spawner.push(Program::Tunnel, Event::QuickTunnelUrl(URL.into()));
    spawner.push(Program::Tunnel, Event::Registered(0));
    web.poll(&ours(), now);
    assert_eq!(web.app_origin(), Some("https://topvnc.owner.workers.dev"));
    assert_eq!(
        spawner.args(Program::Gateway),
        ["--supervised", "gateway.local.toml"].map(OsString::from)
    );
    assert_eq!(web.route(), Some(RouteKind::Access));
}
