use std::fs;
use std::io;
#[cfg(any(target_os = "windows", target_os = "linux"))]
use std::io::Write;
use std::path::PathBuf;
#[cfg(any(target_os = "windows", target_os = "linux"))]
use std::process::{Command, Stdio};

use crate::ui::Config;

fn path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.map(|base| base.join("TopVNC").join("last-session"))
}

#[cfg(target_os = "windows")]
fn protected_password(password: &str, protect: bool) -> io::Result<String> {
    let code = if protect {
        "Add-Type -AssemblyName System.Security; $bytes=[Text.Encoding]::UTF8.GetBytes([Console]::In.ReadToEnd()); [Console]::Out.Write([Convert]::ToBase64String([Security.Cryptography.ProtectedData]::Protect($bytes,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)))"
    } else {
        "Add-Type -AssemblyName System.Security; $bytes=[Convert]::FromBase64String([Console]::In.ReadToEnd()); [Console]::Out.Write([Text.Encoding]::UTF8.GetString([Security.Cryptography.ProtectedData]::Unprotect($bytes,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)))"
    };
    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", code])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(password.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "Windows could not protect the saved password: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    String::from_utf8(output.stdout).map_err(|_| io::Error::other("invalid password encoding"))
}

#[cfg(target_os = "linux")]
fn saved_password(host: &str, port: &str, password: Option<&str>) -> io::Result<String> {
    let mut command = Command::new("secret-tool");
    if password.is_some() {
        command.args(["store", "--label=TopVNC session"]);
    } else {
        command.arg("lookup");
    }
    command.args(["application", "topvnc", "host", host, "port", port]);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    if let Some(password) = password {
        child.stdin.take().unwrap().write_all(password.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(io::Error::other("system secret store is unavailable"));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end_matches('\n')
        .to_owned())
}

pub fn load(config: &mut Config) {
    let Some(path) = path() else { return };
    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };
    let mut lines = contents.lines();
    if lines.next() != Some("topvnc-session-1") {
        return;
    }
    let (Some(host), Some(port)) = (lines.next(), lines.next()) else {
        return;
    };
    if host.is_empty()
        || host.chars().any(char::is_whitespace)
        || port.parse::<u16>().ok().filter(|port| *port > 0).is_none()
    {
        return;
    }
    config.host = host.to_owned();
    config.port = port.to_owned();
    #[cfg(target_os = "windows")]
    if let Some(protected) = lines.next().filter(|value| !value.is_empty())
        && let Ok(password) = protected_password(protected, false)
    {
        config.password = password;
    }
    #[cfg(target_os = "linux")]
    if let Ok(password) = saved_password(host, port, None) {
        config.password = password;
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn password_is_protected_for_current_user() {
        let secret = "pássword 🔐";
        let encrypted = match protected_password(secret, true) {
            Ok(value) => value,
            Err(error) if error.to_string().contains("user profile loaded") => {
                eprintln!("DPAPI test skipped: sandbox user profile is unavailable");
                return;
            }
            Err(error) => panic!("DPAPI failed: {error}"),
        };
        assert!(!encrypted.contains(secret));
        assert_eq!(protected_password(&encrypted, false).unwrap(), secret);
    }
}

pub fn save(config: &Config) -> io::Result<()> {
    let path = path().ok_or_else(|| io::Error::other("settings directory unavailable"))?;
    fs::create_dir_all(path.parent().unwrap())?;
    #[cfg(target_os = "windows")]
    let password_result = protected_password(&config.password, true);
    #[cfg(target_os = "linux")]
    let password_result = saved_password(config.host.trim(), &config.port, Some(&config.password));
    #[cfg(target_os = "macos")]
    let password_result: io::Result<String> = Ok(String::new());
    let protected = if cfg!(target_os = "windows") {
        password_result.as_ref().map(String::as_str).unwrap_or("")
    } else {
        ""
    };
    fs::write(
        path,
        format!(
            "topvnc-session-1\n{}\n{}\n{}\n",
            config.host.trim(),
            config.port,
            protected
        ),
    )?;
    password_result.map(|_| ())
}
