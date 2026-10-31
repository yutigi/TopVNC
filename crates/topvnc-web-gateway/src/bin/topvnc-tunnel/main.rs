use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "Usage:
  topvnc-tunnel <tunnel-token.txt>
Runs the private Tunnel connector: cloudflared tunnel --no-autoupdate run
--token-file <file>. Set CLOUDFLARED to its full path if cloudflared is not on
PATH. Ctrl+C stops it.";

/// The token stays in its protected file; it never appears on a command line.
fn cloudflared_args(token_file: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["tunnel", "--no-autoupdate", "run", "--token-file"]
        .map(OsString::from)
        .into();
    args.push(token_file.into());
    args
}

/// Checks only that the file exists; the token is never read here.
fn check_token_file(path: &Path) -> Result<(), String> {
    match path.is_file() {
        true => Ok(()),
        false => Err(format!(
            "{} not found. Save the connector token there (README step 5) and restrict it to your account.",
            path.display()
        )),
    }
}

#[cfg(unix)]
fn interrupts() -> std::io::Result<tokio::signal::unix::Signal> {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
}

#[cfg(windows)]
fn interrupts() -> std::io::Result<tokio::signal::windows::CtrlC> {
    tokio::signal::windows::ctrl_c()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    let token_file = match args.as_slice() {
        ["--help"] => {
            eprintln!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        [path] => Path::new(*path),
        _ => {
            eprintln!("{USAGE}");
            return Ok(ExitCode::from(2));
        }
    };
    check_token_file(token_file)?;
    // Ctrl+C reaches cloudflared through the shared console. Listen before
    // spawning so this process outlives cloudflared's graceful shutdown.
    let mut interrupt = interrupts()?;
    let program = std::env::var_os("CLOUDFLARED").unwrap_or_else(|| "cloudflared".into());
    let mut child = tokio::process::Command::new(&program)
        .args(cloudflared_args(token_file))
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!(
                "{} not found. Install cloudflared or set CLOUDFLARED to its full path.",
                Path::new(&program).display()
            ),
            _ => format!("cannot start {}: {e}", Path::new(&program).display()),
        })?;
    loop {
        tokio::select! {
            status = child.wait() => {
                return Ok(match status?.success() {
                    true => ExitCode::SUCCESS,
                    false => ExitCode::FAILURE,
                });
            }
            Some(()) = interrupt.recv() => {}
        }
    }
}

#[cfg(test)]
mod tests;
