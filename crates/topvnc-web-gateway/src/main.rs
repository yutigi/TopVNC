use qrcode::{QrCode, render::unicode::Dense1x2};
use std::path::Path;
use std::sync::Arc;
use topvnc_e2e as e2e;
use topvnc_web_gateway::{
    Config, Gateway, PAIRING_SECONDS, QUICK_TUNNEL_SESSION_SECONDS, QuickTunnelConfig,
    create_host_key, routes, spawn_synthetic_echo,
};

const USAGE: &str = "Usage:
  topvnc-web-gateway --init-host-key <gateway.toml>
  topvnc-web-gateway [--pair] <gateway.toml>
  topvnc-web-gateway --synthetic-echo [--pair] <gateway.toml>
  topvnc-web-gateway --supervised <gateway.toml>
  topvnc-web-gateway --supervised-quick-tunnel <listen> <rfb-target> <app-origin> <web-dist>
Relays Cloudflare Access-authenticated, end-to-end encrypted browser sessions
to the configured loopback rfb_target, normally `topvnc --serve` with a VNC
password. --pair shows a one-time enrollment QR code on this console.
--synthetic-echo replaces the target with a loopback echo, for route tests.
--supervised is for the TopVNC window: it reads `pair` and `cancel-pair`
commands on stdin, stops when stdin closes, and reports `fingerprint`,
`ready`, `pair`, and `pair-error` lines on stdout instead of a QR code.
--supervised-quick-tunnel is the same for a temporary Quick Tunnel: no Access
sign-in, only pairing and the VNC password. It serves the built web app from
<web-dist>, accepts upgrades only from <app-origin> (an exact
https://<label>.trycloudflare.com origin), and keeps a new host key and its
enrollments in memory for this run only.";

/// Key paths are resolved relative to the configuration file.
fn load_config(path: &str) -> Result<Config, Box<dyn std::error::Error>> {
    let mut config: Config = toml::from_str(&std::fs::read_to_string(path)?)?;
    let base = Path::new(path).parent().unwrap_or(Path::new("."));
    config.host_key_file = base.join(&config.host_key_file);
    config.browser_keys_file = base.join(&config.browser_keys_file);
    Ok(config)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    let (synthetic, pair, config_path) = match args.as_slice() {
        ["--init-host-key", path] => {
            let config = load_config(path)?;
            let keypair = create_host_key(&config.host_key_file).map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => format!(
                    "{} already exists. Delete it only to rotate the host key; every browser must then pair again.",
                    config.host_key_file.display()
                ),
                _ => format!("cannot create {}: {e}", config.host_key_file.display()),
            })?;
            eprintln!(
                "Created {}\nHost key fingerprint: {}\nRestrict this file to your account; never copy it to the browser or Cloudflare.",
                config.host_key_file.display(),
                e2e::fingerprint(&keypair.public)
            );
            return Ok(());
        }
        [path] if !path.starts_with('-') => (false, false, *path),
        ["--pair", path] => (false, true, *path),
        ["--synthetic-echo", path] => (true, false, *path),
        ["--synthetic-echo", "--pair", path] => (true, true, *path),
        ["--supervised", path] => {
            let config = load_config(path)?;
            let listen = config.listen;
            let target = config.rfb_target;
            return supervised(
                Gateway::new(config)?,
                listen,
                &format!(
                    "for the RFB server at {target} (Access JWT, an enrolled browser, and the VNC password required)"
                ),
            )
            .await;
        }
        [
            "--supervised-quick-tunnel",
            listen,
            target,
            origin,
            web_root,
        ] => {
            let config = QuickTunnelConfig {
                listen: listen.parse()?,
                rfb_target: target.parse()?,
                app_origin: (*origin).to_owned(),
                max_session_seconds: QUICK_TUNNEL_SESSION_SECONDS,
                web_root: web_root.into(),
            };
            let (listen, target) = (config.listen, config.rfb_target);
            return supervised(
                Gateway::quick_tunnel(config)?,
                listen,
                &format!(
                    "for the RFB server at {target}, serving the web app for {origin} (Quick Tunnel: no Access sign-in; an enrolled browser and the VNC password required)"
                ),
            )
            .await;
        }
        ["--help"] => {
            eprintln!("{USAGE}");
            return Ok(());
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    let mut config = load_config(config_path)?;
    let echo_task = if synthetic {
        let (echo_address, echo_task) = spawn_synthetic_echo().await?;
        config.rfb_target = echo_address;
        Some(echo_task)
    } else {
        None
    };
    let listen = config.listen;
    let target = config.rfb_target;
    let origin = config.app_origin.clone();
    let gateway = Gateway::new(config)?;
    eprintln!(
        "Host key fingerprint: {} ({} enrolled browser key(s))",
        e2e::fingerprint(&gateway.host_public()),
        gateway.enrolled_browsers()
    );
    if pair {
        let invite = gateway.open_pairing()?;
        let link = format!("{origin}/#pair={}", invite.encode());
        // Light modules print as blocks so the code reads on a dark console.
        let qr = QrCode::new(link.as_bytes())?
            .render::<Dense1x2>()
            .dark_color(Dense1x2::Light)
            .light_color(Dense1x2::Dark)
            .build();
        eprintln!(
            "{qr}\nOne-time pairing invite, valid for {} minutes. Sign in to the web app first, then scan this code with the phone camera.\nDo not send this link through chat, email, or another device.\n{link}",
            PAIRING_SECONDS / 60
        );
    }
    let listener = tokio::net::TcpListener::bind(listen).await?;
    if synthetic {
        eprintln!(
            "Synthetic gateway listening on {} (Access JWT and E2E required; loopback echo target, no desktop)",
            listener.local_addr()?
        );
    } else {
        eprintln!(
            "Gateway listening on {} for the RFB server at {target} (Access JWT, an enrolled browser, and the VNC password required)",
            listener.local_addr()?
        );
    }
    let result = axum::serve(listener, routes(gateway))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    if let Some(task) = echo_task {
        task.abort();
    }
    result?;
    Ok(())
}

/// A command from the supervising TopVNC window.
#[derive(Debug, PartialEq)]
enum Command {
    Pair,
    CancelPair,
    Unknown,
}

fn parse_command(line: &str) -> Command {
    match line.trim() {
        "pair" => Command::Pair,
        "cancel-pair" => Command::CancelPair,
        _ => Command::Unknown,
    }
}

/// Runs under the TopVNC window. stdout carries the line protocol in
/// specs/018-cloudflare-setup-ui/plan.md and is the only place the pairing
/// link is written; stderr stays the human log. Closing stdin, which also
/// happens when the supervisor dies, shuts the gateway down.
async fn supervised(
    gateway: Arc<Gateway>,
    listen: std::net::SocketAddr,
    serving: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let origin = gateway.app_origin().to_owned();
    let fingerprint = e2e::fingerprint(&gateway.host_public());
    eprintln!(
        "Host key fingerprint: {fingerprint} ({} enrolled browser key(s){})",
        gateway.enrolled_browsers(),
        if gateway.is_pairing_only() {
            "; a new key for this run only"
        } else {
            ""
        }
    );
    println!("fingerprint {fingerprint}");
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let address = listener.local_addr()?;
    eprintln!("Gateway listening on {address} {serving}");
    println!("ready {address}");

    // A plain thread: a blocked tokio stdin read would hold up runtime
    // shutdown, and this thread simply ends with the process.
    let (commands, mut received) = tokio::sync::mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("supervisor-stdin".into())
        .spawn(move || {
            use std::io::BufRead;
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if commands.send(Some(parse_command(&line))).is_err() {
                    return;
                }
            }
            let _ = commands.send(None);
        })?;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let control = Arc::clone(&gateway);
    let control_task = tokio::spawn(async move {
        while let Some(Some(command)) = received.recv().await {
            match command {
                Command::Pair => match control.open_pairing() {
                    Ok(invite) => {
                        println!("pair {PAIRING_SECONDS} {origin}/#pair={}", invite.encode());
                        eprintln!("Pairing invite open for {} minutes.", PAIRING_SECONDS / 60);
                    }
                    Err(reason) => println!("pair-error {reason}"),
                },
                Command::CancelPair => {
                    if control.cancel_pairing() {
                        eprintln!("Pairing invite cancelled.");
                    }
                }
                Command::Unknown => eprintln!("ignored an unknown supervisor command"),
            }
        }
        let _ = stop.send(());
    });
    let result = axum::serve(listener, routes(gateway))
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = stopped => {}
            }
            eprintln!("Gateway stopping.");
        })
        .await;
    control_task.abort();
    result?;
    Ok(())
}

#[cfg(test)]
mod supervisor_tests {
    use super::{Command, parse_command};

    #[test]
    fn commands_are_exact_words() {
        assert_eq!(parse_command("pair"), Command::Pair);
        assert_eq!(parse_command(" pair "), Command::Pair);
        assert_eq!(parse_command("cancel-pair"), Command::CancelPair);
        assert_eq!(parse_command("pair now"), Command::Unknown);
        assert_eq!(parse_command(""), Command::Unknown);
    }
}
