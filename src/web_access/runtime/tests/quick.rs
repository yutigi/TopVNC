//! The Quick Tunnel route, with simulated cloudflared and gateway programs.

use super::*;

const URL: &str = "https://seasonal-deck-organisms-sf.trycloudflare.com";

/// Start from a stopped server and bring the server row to Ready, which
/// starts cloudflared.
fn tunnel_started(now: Instant) -> (WebRuntime, FakeSpawner) {
    let (mut web, spawner) = runtime();
    web.start(quick_launch(), "pw", &HostServer::Stopped, now)
        .unwrap();
    assert_eq!(
        web.poll(&HostServer::Stopped, now),
        Some(ServerRequest::Start(QUICK_RFB_TARGET))
    );
    web.poll(&ours(), now);
    assert_eq!(*web.row(Row::Tunnel), RowState::Starting);
    (web, spawner)
}

#[test]
fn starts_a_tokenless_tunnel_then_a_gateway_for_the_printed_url() {
    let now = Instant::now();
    let (mut web, spawner) = tunnel_started(now);
    assert_eq!(web.route(), Some(RouteKind::QuickTunnel));
    // No token file and no configuration file.
    assert_eq!(
        spawner.args(Program::Tunnel),
        [
            "tunnel",
            "--no-autoupdate",
            "--url",
            "http://127.0.0.1:6080"
        ]
        .map(OsString::from)
    );
    assert_eq!(web.app_origin(), None);

    // A registered connection alone is not enough: there is no page origin.
    spawner.push(Program::Tunnel, Event::Registered(0));
    web.poll(&ours(), now);
    assert_eq!(*web.row(Row::Tunnel), RowState::Starting);
    assert_eq!(spawner.count(), 1, "the gateway waits for the URL");

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
            URL,
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
        ["pair"]
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
fn a_tunnel_that_prints_no_url_fails_and_starts_no_gateway() {
    let now = Instant::now();
    let (mut web, spawner) = tunnel_started(now);
    spawner.push(Program::Tunnel, Event::Registered(0));
    web.poll(&ours(), now + TUNNEL_START);
    assert_eq!(
        *web.row(Row::Tunnel),
        RowState::Failed("cloudflared printed no Quick Tunnel URL in time".into())
    );
    assert!(spawner.child(Program::Tunnel).lock().unwrap().killed);
    assert_eq!(*web.row(Row::Gateway), RowState::Stopped);
    assert_eq!(spawner.count(), 1);
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
