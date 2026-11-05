//! The WEB tab's run state: the desktop server, the tunnel connector, and the
//! gateway, started in that order and stopped in reverse. The desktop server
//! is the landing window's in-process host; this module only asks for its
//! loopback listener and watches it. The landing window adds that listener to
//! a server the Server tab already runs, so both can serve at once. The
//! connector and the gateway are supervised children.
//!
//! On the Quick Tunnel route the connector also decides the page origin: the
//! tunnel is Ready only once cloudflared has printed its
//! `https://<label>.trycloudflare.com` URL and registered a connection, and
//! the gateway then starts with that exact origin.

use super::config::{Launch, Route, RouteKind};
use super::lines::{Event, Program};
use super::pairing::PairingCode;
use super::process::{Spawner, Supervised};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

pub const SERVER_START: Duration = Duration::from_secs(20);
pub const TUNNEL_START: Duration = Duration::from_secs(45);
pub const GATEWAY_START: Duration = Duration::from_secs(15);
/// How long the gateway gets to close its sessions after stdin closes.
pub const GATEWAY_STOP: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Server,
    Tunnel,
    Gateway,
}

impl Row {
    fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Server => "SERVER",
            Self::Tunnel => "TUNNEL",
            Self::Gateway => "GATEWAY",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowState {
    Stopped,
    /// Queued behind an earlier row.
    Waiting,
    Starting,
    Ready,
    Stopping,
    Failed(String),
}

/// The WEB tab's listener on the landing window's desktop server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostServer {
    Stopped,
    Starting,
    Serving,
    Stopping,
}

/// What the landing window should do with the WEB tab's listener.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerRequest {
    /// Serve plain RFB on this loopback address with the typed password,
    /// starting the desktop server if it is not running.
    Start(SocketAddr),
    /// Close the listener, and the server if no other tab uses it.
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Running,
    Stopping,
}

pub struct WebRuntime {
    spawner: Box<dyn Spawner>,
    launch: Option<Launch>,
    phase: Phase,
    rows: [RowState; 3],
    /// When each row entered Starting.
    since: [Option<Instant>; 3],
    server_requested: bool,
    stop_requested: bool,
    tunnel: Option<Box<dyn Supervised>>,
    gateway: Option<Box<dyn Supervised>>,
    /// Output of children that have been dropped, kept for the tab.
    kept: [Vec<String>; 2],
    connections: BTreeSet<u32>,
    tunnel_was_ready: bool,
    /// The Quick Tunnel's origin, from cloudflared's first valid URL line.
    quick_origin: Option<String>,
    gateway_stop_deadline: Option<Instant>,
    /// Pair as soon as the gateway is ready.
    pair_wanted: bool,
    /// `pair` was sent and its answer has not arrived.
    pair_pending: bool,
    pairing: Option<PairingCode>,
    fingerprint: Option<String>,
    message: Option<String>,
}

impl WebRuntime {
    pub fn new(spawner: Box<dyn Spawner>) -> Self {
        Self {
            spawner,
            launch: None,
            phase: Phase::Idle,
            rows: [RowState::Stopped, RowState::Stopped, RowState::Stopped],
            since: [None; 3],
            server_requested: false,
            stop_requested: false,
            tunnel: None,
            gateway: None,
            kept: [Vec::new(), Vec::new()],
            connections: BTreeSet::new(),
            tunnel_was_ready: false,
            quick_origin: None,
            gateway_stop_deadline: None,
            pair_wanted: false,
            pair_pending: false,
            pairing: None,
            fingerprint: None,
            message: None,
        }
    }

    pub fn is_idle(&self) -> bool {
        self.phase == Phase::Idle
    }

    pub fn is_stopping(&self) -> bool {
        self.phase == Phase::Stopping
    }

    pub fn row(&self, row: Row) -> &RowState {
        &self.rows[row.index()]
    }

    /// The row's address or connection count, when it has one.
    pub fn detail(&self, row: Row) -> Option<String> {
        let launch = self.launch.as_ref()?;
        match (row, self.row(row)) {
            (Row::Server, RowState::Ready) => Some(launch.rfb_target.to_string()),
            (Row::Tunnel, RowState::Ready) => Some(match self.connections.len() {
                1 => "1 CONNECTION".into(),
                count => format!("{count} CONNECTIONS"),
            }),
            (Row::Tunnel, RowState::Starting) if self.tunnel_was_ready => {
                Some("RECONNECTING".into())
            }
            (Row::Gateway, RowState::Ready) => Some(launch.listen.to_string()),
            _ => None,
        }
    }

    /// Recent sanitized output of the tunnel or gateway.
    pub fn output(&self, row: Row) -> Vec<String> {
        let (child, kept) = match row {
            Row::Server => return Vec::new(),
            Row::Tunnel => (&self.tunnel, &self.kept[0]),
            Row::Gateway => (&self.gateway, &self.kept[1]),
        };
        child
            .as_ref()
            .map_or_else(|| kept.clone(), |child| child.output())
    }

    /// The page address to open on the phone. A Quick Tunnel's is known
    /// only once cloudflared prints it, and ends with the run.
    pub fn app_origin(&self) -> Option<&str> {
        match &self.launch.as_ref()?.route {
            Route::Access { app_origin, .. } => Some(app_origin),
            Route::QuickTunnel { .. } if self.phase == Phase::Idle => None,
            Route::QuickTunnel { .. } => self.quick_origin.as_deref(),
        }
    }

    /// The route of the current, or last, run.
    pub fn route(&self) -> Option<RouteKind> {
        Some(self.launch.as_ref()?.route.kind())
    }

    pub fn fingerprint(&self) -> Option<&str> {
        self.fingerprint.as_deref()
    }

    pub fn pairing(&self) -> Option<&PairingCode> {
        self.pairing.as_ref()
    }

    /// Waiting for a pairing code, or showing one.
    pub fn pairing_active(&self) -> bool {
        self.pairing.is_some() || self.pair_wanted || self.pair_pending
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Start browser access. `password` is the VNC password the WEB tab will
    /// serve with; `server` is the tab's listener, which must be closed.
    pub fn start(
        &mut self,
        launch: Launch,
        password: &str,
        server: &HostServer,
        now: Instant,
    ) -> Result<(), String> {
        if self.phase != Phase::Idle {
            return Err("Browser access is already running.".into());
        }
        if *server != HostServer::Stopped {
            return Err("Wait for the desktop server to finish stopping.".into());
        }
        if password.is_empty() {
            return Err("Enter a VNC password.".into());
        }
        // A fresh state; only the spawner carries over.
        let spawner = std::mem::replace(&mut self.spawner, Box::new(NoSpawner));
        *self = Self::new(spawner);
        self.launch = Some(launch);
        self.phase = Phase::Running;
        self.rows = [RowState::Starting, RowState::Waiting, RowState::Waiting];
        self.since[Row::Server.index()] = Some(now);
        Ok(())
    }

    /// Stop everything this tab started, gateway first.
    pub fn stop(&mut self) {
        if self.phase == Phase::Running {
            self.phase = Phase::Stopping;
            self.end_pairing();
        }
    }

    /// Show a pairing code as soon as the gateway is ready.
    pub fn pair(&mut self) -> Result<(), String> {
        if self.phase == Phase::Stopping {
            return Err("Browser access is stopping.".into());
        }
        if self.pairing_active() {
            return Ok(());
        }
        self.message = None;
        match (&mut self.gateway, self.rows[Row::Gateway.index()].clone()) {
            (Some(gateway), RowState::Ready) => {
                if !gateway.send("pair") {
                    return Err("The gateway is not accepting commands.".into());
                }
                self.pair_pending = true;
            }
            _ => self.pair_wanted = true,
        }
        Ok(())
    }

    /// Remove the pairing code and withdraw the invite.
    pub fn cancel_pair(&mut self) {
        let open = self.pairing.is_some() || self.pair_pending;
        self.end_pairing();
        if open && let Some(gateway) = &mut self.gateway {
            gateway.send("cancel-pair");
        }
    }

    fn end_pairing(&mut self) {
        self.pairing = None;
        self.pair_wanted = false;
        self.pair_pending = false;
    }

    fn set(&mut self, row: Row, state: RowState, now: Instant) {
        if state == RowState::Starting && self.rows[row.index()] != RowState::Starting {
            self.since[row.index()] = Some(now);
        }
        let failed = matches!(state, RowState::Failed(_));
        self.rows[row.index()] = state;
        if failed {
            // Rows queued behind a failed one will not start.
            for later in &mut self.rows[row.index() + 1..] {
                if *later == RowState::Waiting {
                    *later = RowState::Stopped;
                }
            }
            if row == Row::Gateway {
                self.end_pairing();
            }
        }
    }

    fn timed_out(&self, row: Row, limit: Duration, now: Instant) -> bool {
        self.rows[row.index()] == RowState::Starting
            && self.since[row.index()].is_some_and(|since| now.duration_since(since) >= limit)
    }

    /// Advance the state. Returns a request for the desktop server, which
    /// the caller applies before the next poll.
    pub fn poll(&mut self, server: &HostServer, now: Instant) -> Option<ServerRequest> {
        if let Some(pairing) = &self.pairing
            && pairing.expires <= now
        {
            self.pairing = None;
            self.message = Some("The pairing code expired.".into());
        }
        match self.phase {
            Phase::Idle => None,
            Phase::Running => self.poll_running(server, now),
            Phase::Stopping => self.poll_stopping(server, now),
        }
    }

    fn poll_running(&mut self, server: &HostServer, now: Instant) -> Option<ServerRequest> {
        let launch = self.launch.clone()?;
        let mut request = None;
        // Server.
        match (self.row(Row::Server).clone(), server) {
            (RowState::Starting, HostServer::Stopped) if !self.server_requested => {
                self.server_requested = true;
                request = Some(ServerRequest::Start(launch.rfb_target));
            }
            (RowState::Starting, HostServer::Serving) => {
                self.set(Row::Server, RowState::Ready, now);
            }
            (RowState::Starting | RowState::Ready, HostServer::Stopped) => {
                self.set(
                    Row::Server,
                    RowState::Failed("the server stopped".into()),
                    now,
                );
            }
            (RowState::Starting, _) if self.timed_out(Row::Server, SERVER_START, now) => {
                self.set(
                    Row::Server,
                    RowState::Failed("the server did not start in time".into()),
                    now,
                );
                request = Some(ServerRequest::Stop);
            }
            _ => {}
        }
        // Tunnel connector.
        if *self.row(Row::Server) == RowState::Ready && *self.row(Row::Tunnel) == RowState::Waiting
        {
            let args = tunnel_args(&launch);
            match self
                .spawner
                .spawn(Program::Tunnel, &launch.cloudflared, &args)
            {
                Ok(child) => {
                    self.tunnel = Some(child);
                    self.set(Row::Tunnel, RowState::Starting, now);
                }
                Err(error) => self.set(Row::Tunnel, RowState::Failed(error), now),
            }
        }
        self.poll_tunnel(now);
        // Gateway.
        if *self.row(Row::Tunnel) == RowState::Ready && *self.row(Row::Gateway) == RowState::Waiting
        {
            let spawned = gateway_args(&launch, self.quick_origin.as_deref()).and_then(|args| {
                self.spawner
                    .spawn(Program::Gateway, &launch.gateway_program, &args)
            });
            match spawned {
                Ok(child) => {
                    self.gateway = Some(child);
                    self.set(Row::Gateway, RowState::Starting, now);
                }
                Err(error) => self.set(Row::Gateway, RowState::Failed(error), now),
            }
        }
        self.poll_gateway(now);
        request
    }

    fn poll_tunnel(&mut self, now: Instant) {
        let late = self.timed_out(Row::Tunnel, TUNNEL_START, now);
        let quick = self
            .launch
            .as_ref()
            .is_some_and(|launch| launch.route.kind() == RouteKind::QuickTunnel);
        let Some(tunnel) = &mut self.tunnel else {
            return;
        };
        if matches!(self.rows[Row::Tunnel.index()], RowState::Failed(_)) {
            return;
        }
        for event in tunnel.take_events() {
            match event {
                Event::Registered(index) => {
                    self.connections.insert(index);
                }
                Event::Lost(Some(index)) => {
                    self.connections.remove(&index);
                }
                Event::Lost(None) => self.connections.clear(),
                // Set once: a later line never moves the page elsewhere.
                Event::QuickTunnelUrl(url) if quick && self.quick_origin.is_none() => {
                    self.quick_origin = Some(url);
                }
                _ => {}
            }
        }
        // A Quick Tunnel is reachable only at the URL cloudflared printed.
        let routable = !quick || self.quick_origin.is_some();
        if let Some(exit) = tunnel.exit() {
            self.connections.clear();
            self.set(
                Row::Tunnel,
                RowState::Failed(format!("cloudflared {exit}")),
                now,
            );
        } else if !self.connections.is_empty() && routable {
            self.tunnel_was_ready = true;
            self.set(Row::Tunnel, RowState::Ready, now);
        } else if self.tunnel_was_ready {
            // cloudflared reconnects by itself; no start limit applies.
            self.rows[Row::Tunnel.index()] = RowState::Starting;
        } else if late {
            tunnel.kill();
            let reason = if routable {
                "no tunnel connection registered in time"
            } else {
                "cloudflared printed no Quick Tunnel URL in time"
            };
            self.set(Row::Tunnel, RowState::Failed(reason.into()), now);
        }
    }

    fn poll_gateway(&mut self, now: Instant) {
        let late = self.timed_out(Row::Gateway, GATEWAY_START, now);
        let Some(gateway) = &mut self.gateway else {
            return;
        };
        if matches!(self.rows[Row::Gateway.index()], RowState::Failed(_)) {
            return;
        }
        let mut ready = false;
        for event in gateway.take_events() {
            match event {
                Event::Fingerprint(fingerprint) => self.fingerprint = Some(fingerprint),
                Event::GatewayReady(_) => ready = true,
                Event::Pair { seconds, link } if self.pair_pending => {
                    self.pair_pending = false;
                    match PairingCode::new(link, now + Duration::from_secs(seconds)) {
                        Ok(code) => self.pairing = Some(code),
                        Err(error) => {
                            gateway.send("cancel-pair");
                            self.message = Some(error);
                        }
                    }
                }
                // Cancelled before it arrived: withdraw it too.
                Event::Pair { .. } => {
                    gateway.send("cancel-pair");
                }
                Event::PairError(reason) => {
                    self.pair_pending = false;
                    self.message = Some(format!("Pairing is not possible: {reason}."));
                }
                Event::Enrolled(fingerprint) => {
                    self.pairing = None;
                    self.pair_pending = false;
                    self.message = Some(format!("Browser enrolled: key {fingerprint}."));
                }
                Event::Registered(_) | Event::Lost(_) | Event::QuickTunnelUrl(_) => {}
            }
        }
        if ready && self.rows[Row::Gateway.index()] == RowState::Starting {
            self.rows[Row::Gateway.index()] = RowState::Ready;
            if self.pair_wanted && gateway.send("pair") {
                self.pair_wanted = false;
                self.pair_pending = true;
            }
        }
        if let Some(exit) = gateway.exit() {
            self.set(
                Row::Gateway,
                RowState::Failed(format!("the gateway {exit}")),
                now,
            );
        } else if late && self.rows[Row::Gateway.index()] == RowState::Starting {
            gateway.kill();
            self.set(
                Row::Gateway,
                RowState::Failed("the gateway did not report ready in time".into()),
                now,
            );
        }
    }

    fn poll_stopping(&mut self, server: &HostServer, now: Instant) -> Option<ServerRequest> {
        if let Some(gateway) = &mut self.gateway {
            let deadline = *self.gateway_stop_deadline.get_or_insert_with(|| {
                gateway.close_input();
                now + GATEWAY_STOP
            });
            if gateway.exit().is_none() && now < deadline {
                if !matches!(self.rows[Row::Gateway.index()], RowState::Failed(_)) {
                    self.rows[Row::Gateway.index()] = RowState::Stopping;
                }
                return None;
            }
            gateway.kill();
            self.kept[1] = gateway.output();
            self.gateway = None;
            self.finish(Row::Gateway);
        }
        if let Some(mut tunnel) = self.tunnel.take() {
            tunnel.kill();
            self.kept[0] = tunnel.output();
            self.connections.clear();
            self.finish(Row::Tunnel);
        }
        self.finish(Row::Gateway);
        self.finish(Row::Tunnel);
        match server {
            HostServer::Starting | HostServer::Serving if !self.stop_requested => {
                self.stop_requested = true;
                if !matches!(self.rows[Row::Server.index()], RowState::Failed(_)) {
                    self.rows[Row::Server.index()] = RowState::Stopping;
                }
                return Some(ServerRequest::Stop);
            }
            HostServer::Stopped => {}
            _ => return None,
        }
        self.finish(Row::Server);
        self.phase = Phase::Idle;
        None
    }

    /// A stopped row shows Stopped, unless it failed first.
    fn finish(&mut self, row: Row) {
        if !matches!(self.rows[row.index()], RowState::Failed(_)) {
            self.rows[row.index()] = RowState::Stopped;
        }
    }
}

impl Drop for WebRuntime {
    fn drop(&mut self) {
        // Let the gateway close its sessions; the children's own drop, and
        // the Windows job, end anything left.
        if let Some(gateway) = &mut self.gateway {
            gateway.close_input();
            let deadline = Instant::now() + GATEWAY_STOP;
            while gateway.exit().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            gateway.kill();
        }
        if let Some(tunnel) = &mut self.tunnel {
            tunnel.kill();
        }
    }
}

/// cloudflared's arguments. The Access route's token stays in its file;
/// only the path is passed. A Quick Tunnel needs no token at all.
fn tunnel_args(launch: &Launch) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["tunnel".into(), "--no-autoupdate".into()];
    match &launch.route {
        Route::Access { token_file, .. } => {
            args.extend(["run".into(), "--token-file".into()]);
            args.push(token_file.clone().into_os_string());
        }
        Route::QuickTunnel { .. } => {
            args.push("--url".into());
            args.push(format!("http://{}", launch.listen).into());
        }
    }
    args
}

/// The gateway's arguments; a Quick Tunnel gateway needs the learned origin.
fn gateway_args(launch: &Launch, quick_origin: Option<&str>) -> Result<Vec<OsString>, String> {
    Ok(match &launch.route {
        Route::Access { gateway_config, .. } => vec![
            "--supervised".into(),
            gateway_config.clone().into_os_string(),
        ],
        Route::QuickTunnel { web_root } => vec![
            "--supervised-quick-tunnel".into(),
            launch.listen.to_string().into(),
            launch.rfb_target.to_string().into(),
            quick_origin
                .ok_or("the Quick Tunnel URL is not known")?
                .into(),
            web_root.clone().into_os_string(),
        ],
    })
}

/// A placeholder while the runtime is rebuilt in `start`.
struct NoSpawner;

impl Spawner for NoSpawner {
    fn spawn(
        &mut self,
        _: Program,
        _: &std::path::Path,
        _: &[OsString],
    ) -> Result<Box<dyn Supervised>, String> {
        Err("no spawner".into())
    }
}

#[cfg(test)]
mod tests;
