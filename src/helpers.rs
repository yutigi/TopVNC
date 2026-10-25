//! Windows script integration. Only the explicit launcher action starts a
//! server; diagnostics and firewall operations are always read-only.
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

pub const WINDOWS_ONLY: &str =
    "These helpers require Windows PowerShell and the Windows networking tools.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Helper {
    Work,
    Network,
    Firewall,
}

#[derive(Clone, Debug)]
pub struct HelperForm {
    pub work_host: String,
    pub work_port: String,
    pub network_port: String,
    pub peer: String,
    pub local: String,
    pub program: String,
    pub interface: String,
    pub profile: String,
    pub firewall_port: String,
}

impl Default for HelperForm {
    fn default() -> Self {
        Self {
            work_host: String::new(),
            work_port: "5900".into(),
            network_port: "5900".into(),
            peer: String::new(),
            local: String::new(),
            program: script_path("Start-TopVncWorkServer.cmd")
                .ok()
                .and_then(|p| {
                    p.parent()?
                        .parent()
                        .map(|root| root.join("target/debug/topvnc.exe"))
                })
                .map(|p| p.to_string_lossy().replace('/', "\\"))
                .unwrap_or_default(),
            interface: String::new(),
            profile: "Public".into(),
            firewall_port: "5900".into(),
        }
    }
}

pub fn port(value: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| "Port must be 1-65535.".into())
}

impl HelperForm {
    pub fn work_bind(&self) -> Result<String, String> {
        let ip = self
            .work_host
            .parse::<std::net::Ipv4Addr>()
            .map_err(|_| "Enter the local IPv4 address assigned to your VPN interface.")?;
        if ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
            return Err("Use one concrete local IPv4 address.".into());
        }
        Ok(format!("{ip}:{}", port(&self.work_port)?))
    }
}

/// Single-quoted PowerShell literals are data, including apostrophes, dollar
/// signs, backticks and newlines. Never interpolate form text as executable code.
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn script_path(name: &str) -> Result<PathBuf, String> {
    let mut roots = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        roots.push(parent.to_path_buf());
    }
    roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    roots.into_iter().map(|root| root.join("scripts").join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| format!("Missing scripts/{name}. Keep the scripts folder beside TopVNC or run from the source checkout."))
}

#[derive(Clone)]
pub enum Action {
    CheckWork,
    StartWork,
    Network,
    Firewall,
    Copy(String),
}

fn code(action: &Action, form: &HelperForm) -> Result<String, String> {
    let body = match action {
        Action::CheckWork | Action::StartWork => {
            let bind = form.work_bind()?;
            let path = script_path("Start-TopVncWorkServer.cmd")?;
            format!(
                "$env:TOPVNC_BIND={}; & {}{}; if ($LASTEXITCODE -ne 0) {{ throw 'Launcher failed. Review the terminal output.' }}",
                literal(&bind),
                literal(&path.to_string_lossy()),
                if matches!(action, Action::CheckWork) {
                    " /check"
                } else {
                    ""
                }
            )
        }
        Action::Network => {
            let port = port(&form.network_port)?;
            let path = script_path("Test-TopVncNetwork.ps1")?;
            format!(
                "& {} -Port {port} | ConvertTo-Json -Depth 6",
                literal(&path.to_string_lossy())
            )
        }
        Action::Firewall => {
            let port = port(&form.firewall_port)?;
            if [&form.peer, &form.local, &form.program, &form.interface]
                .iter()
                .any(|s| s.trim().is_empty())
            {
                return Err("Enter peer IP, local IP, program path, and interface alias.".into());
            }
            if !["Domain", "Private", "Public"].contains(&form.profile.as_str()) {
                return Err("Choose one firewall profile.".into());
            }
            let path = literal(&script_path("Set-TopVncPeerFirewall.ps1")?.to_string_lossy());
            let args = format!(
                "-PeerAddress {} -LocalAddress {} -ProgramPath {} -InterfaceAlias {} -Profile {} -Port {port}",
                literal(&form.peer),
                literal(&form.local),
                literal(&form.program),
                literal(&form.interface),
                literal(&form.profile)
            );
            // Dot-source to suppress the CLI's duplicate host output, then call
            // the same function without either mutation switch.
            format!(
                ". {path} {args}; $r=Invoke-TopVncPeerFirewall {args}; $r | Select-Object Status,Operation,RuleName,Existing,Note | Format-List | Out-String -Width 120; $r.Parameters | Format-Table -AutoSize | Out-String -Width 160; 'APPLY COMMAND (review in administrator PowerShell):'; $r.ApplyCommand; ''; 'ROLLBACK COMMAND:'; $r.RollbackCommand; ''; $r.AdministratorInstruction"
            )
        }
        Action::Copy(_) => "Set-Clipboard -Value ([Console]::In.ReadToEnd())".into(),
    };
    Ok(format!(
        "$ErrorActionPreference='Stop'; [Console]::InputEncoding=[Text.UTF8Encoding]::new($false); [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); {body}"
    ))
}

#[derive(Default)]
pub struct Report {
    pub text: String,
    pub page: usize,
    pub failed: bool,
    pub copied: bool,
}

pub struct HelperRuntime {
    pub work: Report,
    pub network: Report,
    pub firewall: Report,
    pending: Option<Pending>,
}

struct Pending {
    helper: Helper,
    receiver: mpsc::Receiver<Result<String, String>>,
    cancel: Arc<AtomicBool>,
}

impl Default for HelperRuntime {
    fn default() -> Self {
        Self {
            work: Report { text: "Check the launcher before starting. Start opens a separate terminal for password entry. Keep it open while hosting; close it or press Ctrl+C to stop. Closing the viewer does not stop this separate server.".into(), ..Report::default() },
            network: Report { text: "Run a read-only report of local listeners and firewall policy. No peer connection is attempted. Unknown means a query failed, not that a rule is absent. Review addresses before sharing output.".into(), ..Report::default() },
            firewall: Report { text: "Preview only. Enter the exact scope, then review the generated apply and rollback commands. Copy the report to use a command in an existing administrator PowerShell. No rule is changed by this tab.".into(), ..Report::default() },
            pending: None,
        }
    }
}

impl HelperRuntime {
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn report(&self, helper: Helper) -> &Report {
        match helper {
            Helper::Work => &self.work,
            Helper::Network => &self.network,
            Helper::Firewall => &self.firewall,
        }
    }

    pub fn report_mut(&mut self, helper: Helper) -> &mut Report {
        match helper {
            Helper::Work => &mut self.work,
            Helper::Network => &mut self.network,
            Helper::Firewall => &mut self.firewall,
        }
    }

    pub fn start(
        &mut self,
        helper: Helper,
        action: Action,
        form: &HelperForm,
    ) -> Result<(), String> {
        if !cfg!(windows) {
            return Err(WINDOWS_ONLY.into());
        }
        if self.busy() {
            return Err("Wait for the current helper to finish.".into());
        }
        let code = code(&action, form)?;
        let copying = matches!(action, Action::Copy(_));
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name("topvnc-helper".into())
            .spawn(move || {
                let _ = tx.send(run(&action, &code, &worker_cancel));
            })
            .map_err(|e| format!("Could not start helper: {e}"))?;
        // Keep the report when copying so it remains available after success.
        if !copying {
            *self.report_mut(helper) = Report {
                text: "Running...".into(),
                ..Report::default()
            };
        }
        self.pending = Some(Pending {
            helper,
            receiver: rx,
            cancel,
        });
        Ok(())
    }

    pub fn poll(&mut self) {
        let Some(pending) = &self.pending else {
            return;
        };
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("The helper worker stopped unexpectedly.".into())
            }
        };
        let helper = pending.helper;
        self.pending = None;
        let report = self.report_mut(helper);
        match result {
            Ok(text) if text == COPY_OK => report.copied = true,
            Ok(text) => {
                *report = Report {
                    text,
                    ..Report::default()
                }
            }
            Err(text) => {
                *report = Report {
                    text,
                    failed: true,
                    ..Report::default()
                }
            }
        }
    }
}

impl Drop for HelperRuntime {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending {
            pending.cancel.store(true, Ordering::Release);
        }
    }
}

const COPY_OK: &str = "Report copied.";

#[cfg(not(windows))]
fn run(_: &Action, _: &str, _: &AtomicBool) -> Result<String, String> {
    Err(WINDOWS_ONLY.into())
}

#[cfg(windows)]
fn run(action: &Action, code: &str, cancel: &AtomicBool) -> Result<String, String> {
    use std::io::{Read, Write};
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile"]);
    if matches!(action, Action::StartWork) {
        // This console is intentional: the launcher requires interactive local
        // password entry and has an independent lifetime from the viewer.
        command
            .args(["-NoExit", "-Command", code])
            .creation_flags(0x00000010); // CREATE_NEW_CONSOLE
        command
            .spawn()
            .map_err(|e| format!("Could not open server terminal: {e}"))?;
        return Ok("Server terminal opened. Enter the VNC password there. This does not confirm that hosting started; check the terminal for errors. Close it or press Ctrl+C to stop. The viewer can close independently.".into());
    }
    command
        .args(["-NonInteractive", "-Command", code])
        .creation_flags(0x08000000); // CREATE_NO_WINDOW
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not run Windows PowerShell: {e}"))?;
    if let Some(mut stdin) = child.stdin.take()
        && let Action::Copy(text) = action
        && let Err(error) = stdin.write_all(text.as_bytes())
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("Could not copy report: {error}"));
    }
    // Drain both pipes concurrently to avoid deadlocks, retaining at most 64 KiB
    // from each stream even if the helper produces unexpectedly large output.
    fn capture(mut pipe: impl Read) -> std::io::Result<String> {
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        let mut truncated = false;
        loop {
            let count = pipe.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            let keep = count.min(65_536 - bytes.len());
            bytes.extend_from_slice(&chunk[..keep]);
            truncated |= keep != count;
        }
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if truncated {
            text.push_str("\n[Output truncated at 64 KiB.]");
        }
        Ok(text)
    }
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out = std::thread::spawn(move || capture(stdout));
    let err = std::thread::spawn(move || capture(stderr));
    let began = Instant::now();
    let status = loop {
        if cancel.load(Ordering::Acquire) || began.elapsed() >= Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(
                "Helper cancelled or timed out after 30 seconds. No firewall change was requested."
                    .into(),
            );
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Could not wait for helper: {error}"));
            }
        }
    };
    let stdout = out
        .join()
        .map_err(|_| "Output reader stopped.")?
        .map_err(|e| e.to_string())?;
    let stderr = err
        .join()
        .map_err(|_| "Error reader stopped.")?
        .map_err(|e| e.to_string())?;
    let text = format!("{stdout}\n{stderr}").trim().to_string();
    if !status.success() {
        return Err(format!("Helper failed ({status}):\n{text}"));
    }
    if matches!(action, Action::Copy(_)) {
        Ok(COPY_OK.into())
    } else if text.is_empty() {
        Ok("Helper completed without output.".into())
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_ports_and_broad_work_binds() {
        for value in ["0", "65536", "-1", "5900; exit", ""] {
            assert!(port(value).is_err());
        }
        let mut form = HelperForm::default();
        for host in [
            "",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "localhost",
            "1.2.3.4;exit",
        ] {
            form.work_host = host.into();
            assert!(form.work_bind().is_err());
        }
        form.work_host = "127.0.0.1".into();
        assert_eq!(form.work_bind().unwrap(), "127.0.0.1:5900");
    }

    #[test]
    fn firewall_preview_keeps_hostile_values_inside_literals() {
        let form = HelperForm {
            peer: "192.168.50.20".into(),
            local: "192.168.50.10".into(),
            program: "C:\\Team O'Brien\\$()`; -Apply\\topvnc.exe".into(),
            interface: "VPN'; throw 'injected".into(),
            ..HelperForm::default()
        };
        let command = code(&Action::Firewall, &form).unwrap();
        assert!(command.contains("-ProgramPath 'C:\\Team O''Brien\\$()`; -Apply\\topvnc.exe'"));
        assert!(command.contains("-InterfaceAlias 'VPN''; throw ''injected'"));
        assert!(!command.contains(" -Remove"));
        assert!(!command.contains(" -Apply "));
        assert!(!command.contains("ExecutionPolicy"));
        assert!(!command.contains("RunAs"));
    }

    #[cfg(windows)]
    #[test]
    fn powershell_literals_round_trip_without_running_embedded_code() {
        let value = "O'Brien; Write-Output 'UNEXPECTED'; $(`test`) é";
        let code = format!(
            "[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); [Console]::Out.Write({})",
            literal(value)
        );
        let output = run(&Action::Network, &code, &AtomicBool::new(false)).unwrap();
        assert_eq!(output, value);
    }

    #[cfg(windows)]
    #[test]
    fn runs_read_only_launcher_check() {
        let form = HelperForm {
            work_host: "127.0.0.1".into(),
            ..HelperForm::default()
        };
        // `cargo test` on a fresh checkout does not build target/debug/topvnc.exe.
        // A stub file is sufficient for /check and must never be executed.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/helper-tests")
            .join(format!("launcher's check {}", std::process::id()));
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        let launcher = root.join("scripts/Start-TopVncWorkServer.cmd");
        std::fs::write(
            &launcher,
            include_bytes!("../scripts/Start-TopVncWorkServer.cmd"),
        )
        .unwrap();
        std::fs::write(
            root.join("target/debug/topvnc.exe"),
            b"/check only; never execute",
        )
        .unwrap();
        let command = code(&Action::CheckWork, &form).unwrap().replace(
            &literal(
                &script_path("Start-TopVncWorkServer.cmd")
                    .unwrap()
                    .to_string_lossy(),
            ),
            &literal(&launcher.to_string_lossy()),
        );
        let output = run(&Action::CheckWork, &command, &AtomicBool::new(false)).unwrap();
        assert!(output.contains("CHECK ONLY"));
        assert!(output.contains("127.0.0.1:5900"));
    }
}
