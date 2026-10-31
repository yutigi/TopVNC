use qrcode::{QrCode, render::unicode::Dense1x2};
use std::path::Path;
use topvnc_e2e as e2e;
use topvnc_web_gateway::{
    Config, Gateway, PAIRING_SECONDS, create_host_key, routes, spawn_synthetic_echo,
};

const USAGE: &str = "Usage:
  topvnc-web-gateway --init-host-key <gateway.toml>
  topvnc-web-gateway [--pair] <gateway.toml>
  topvnc-web-gateway --synthetic-echo [--pair] <gateway.toml>
Relays Cloudflare Access-authenticated, end-to-end encrypted browser sessions
to the configured loopback rfb_target, normally `topvnc --serve` with a VNC
password. --pair shows a one-time enrollment QR code on this console.
--synthetic-echo replaces the target with a loopback echo, for route tests.";

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
