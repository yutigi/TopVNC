use topvnc_spike_host::{Config, Gateway, routes, spawn_synthetic_echo};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 || args[1] != "--synthetic-echo" {
        eprintln!(
            "Usage: topvnc-spike-host --synthetic-echo <host.local.toml>\nPhase 0 only: synthetic echo traffic; Access JWT required. Desktop forwarding is disabled pending E2E."
        );
        if args.len() == 2 && args[1] == "--help" {
            return Ok(());
        }
        std::process::exit(2);
    }
    let mut config: Config = toml::from_str(&std::fs::read_to_string(&args[2])?)?;
    // The executable always substitutes its own echo target. The saved target
    // cannot enable RFB forwarding; that needs a separately validated E2E path.
    let (echo_address, echo_task) = spawn_synthetic_echo().await?;
    config.rfb_target = echo_address;
    let listen = config.listen;
    let gateway = Gateway::new(config)?;
    let listener = tokio::net::TcpListener::bind(listen).await?;
    eprintln!(
        "Phase 0 synthetic gateway listening on {} (Access JWT required; no desktop target)",
        listener.local_addr()?
    );
    let result = axum::serve(listener, routes(gateway))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    echo_task.abort();
    result?;
    Ok(())
}
