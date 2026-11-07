//! The SERVICE tab (spec 020): the TopVNC Host service's status, and
//! installing or removing it through an elevated installer, without an
//! administrator terminal.

use crate::desktop_host::{DisplaySelection, MouseMode};
use crate::ui::ServeForm;
pub use crate::ui::{ServiceAction, ServiceState, ServiceView};
use crate::web_access::runtime::HostServer;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use topvnc::{PerformanceMode, UpdatePriority};

/// How often the shown tab reads the service's status.
const STATUS_INTERVAL: Duration = Duration::from_secs(1);
#[cfg(not(windows))]
const SERVICE_WINDOWS_ONLY: &str = "The TopVNC Host service is available on Windows only.";

/// The `--serve` options the service gets from the Server tab's form, and
/// the password. The service always requires a password.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn install_options(form: &ServeForm) -> Result<(Vec<String>, String), String> {
    if form.allow_insecure {
        return Err(
            "The service always requires a password: turn off ALLOW NONE AUTHENTICATION in the \
             Server tab."
                .into(),
        );
    }
    if form.password.is_empty() {
        return Err("Enter the VNC password viewers will use for the service.".into());
    }
    let request = form.request()?;
    let mut options = vec![request.address];
    if request.performance == PerformanceMode::Ultra {
        options.push("--ultra".into());
    }
    if request.tls.unencrypted {
        options.push("--unencrypted".into());
    }
    for (flag, path) in [
        ("--tls-cert", &request.tls.cert),
        ("--tls-key", &request.tls.key),
    ] {
        if let Some(path) = path {
            options.push(flag.into());
            options.push(path.to_string_lossy().into_owned());
        }
    }
    options.push("--display".into());
    options.push(match request.display {
        DisplaySelection::Primary => "primary".into(),
        DisplaySelection::All => "all".into(),
        DisplaySelection::Number(number) => number.to_string(),
    });
    if request.scale != 1.0 {
        options.push("--scale".into());
        options.push(request.scale.to_string());
    }
    options.push("--mouse".into());
    options.push(
        match request.mouse {
            MouseMode::Auto => "auto",
            MouseMode::Relative => "relative",
            MouseMode::Absolute => "absolute",
        }
        .into(),
    );
    options.push("--priority".into());
    options.push(
        match request.priority {
            UpdatePriority::Whole => "whole",
            UpdatePriority::Center => "center",
            UpdatePriority::Mouse => "mouse",
            UpdatePriority::Changes => "changes",
        }
        .into(),
    );
    Ok((options, form.password.clone()))
}

/// The port in `HOST:PORT`.
fn port(address: &str) -> Option<u16> {
    address.rsplit_once(':')?.1.parse().ok()
}

/// Whether two `HOST:PORT` addresses use the same port. On Windows a second
/// listener on any of the host's addresses would compete for it.
pub fn same_port(first: &str, second: &str) -> bool {
    port(first).is_some() && port(first) == port(second)
}

/// The SERVICE tab's state: the last status read, and the installer run
/// in progress or its report.
#[derive(Default)]
pub struct ServiceRuntime {
    /// What the tab shows.
    pub view: ServiceView,
    pending: Option<mpsc::Receiver<Result<String, String>>>,
    checked: Option<Instant>,
}

impl ServiceRuntime {
    /// Take a finished installer's report, and read the status about once a
    /// second while the tab is `shown`.
    pub fn poll(&mut self, shown: bool) {
        if let Some(receiver) = &self.pending {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("The installer worker stopped unexpectedly.".into()))
                }
            };
            if let Some(result) = result {
                self.pending = None;
                self.view.busy = None;
                (self.view.report, self.view.failed) = match result {
                    Ok(report) => (report, false),
                    Err(report) => (report, true),
                };
                // Show the outcome now.
                self.checked = None;
            }
        }
        if shown
            && self
                .checked
                .is_none_or(|checked| checked.elapsed() >= STATUS_INTERVAL)
        {
            self.refresh();
        }
    }

    /// Read the status now.
    pub fn refresh(&mut self) {
        self.checked = Some(Instant::now());
        let view = &mut self.view;
        #[cfg(windows)]
        {
            match crate::windows_service::query_status() {
                Ok(status) => {
                    view.status = status;
                    view.status_error = None;
                }
                Err(error) => view.status_error = Some(error),
            }
            view.sas_allowed = crate::windows_service::software_sas_allowed();
            view.log_path = crate::windows_service::log_path()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
        }
        #[cfg(not(windows))]
        {
            view.status_error = Some(SERVICE_WINDOWS_ONLY.into());
        }
    }

    /// Why the Server tab cannot listen on `address`: the running service
    /// holds that port.
    pub fn port_conflict(&self, address: &str) -> Option<String> {
        let status = &self.view.status;
        let listening = matches!(status.state, ServiceState::Starting | ServiceState::Running);
        let service = status.address.as_deref()?;
        (listening && same_port(service, address)).then(|| {
            format!(
                "The TopVNC Host service serves on {service}. Choose another port, or remove \
                 the service in ... > SERVICE."
            )
        })
    }

    /// Reuse only the installed service configured for the gateway's exact
    /// plain-RFB loopback endpoint. A busy port alone is never proof that
    /// a listener is our service or that its transport is compatible.
    pub fn web_server(&self, target: std::net::SocketAddr) -> Result<Option<HostServer>, String> {
        let status = &self.view.status;
        let Some(address) = &status.address else {
            return Ok(None);
        };
        if status.state == ServiceState::NotInstalled || !same_port(address, &target.to_string()) {
            return Ok(None);
        }
        if !target.ip().is_loopback()
            || address.parse::<std::net::SocketAddr>().ok() != Some(target)
            || !status.unencrypted
        {
            return Err(format!(
                "The TopVNC Host service uses {address}. Browser access needs the service \
                 installed with {target} --unencrypted, or a gateway target on another port."
            ));
        }
        match status.state {
            ServiceState::Starting => Ok(Some(HostServer::Starting)),
            ServiceState::Running => Ok(Some(HostServer::Serving)),
            _ => Err(
                "Start the TopVNC Host service in Windows before starting browser access.".into(),
            ),
        }
    }

    /// Ask Windows for administrator approval and run the installer for
    /// `action` with the Server tab's `form`. Problems found here are
    /// returned before any approval prompt.
    pub fn start(&mut self, action: ServiceAction, form: &ServeForm) -> Result<(), String> {
        if self.pending.is_some() {
            return Ok(());
        }
        #[cfg(not(windows))]
        {
            let _ = (action, form);
            Err(SERVICE_WINDOWS_ONLY.into())
        }
        #[cfg(windows)]
        {
            use crate::service_pipe::{Request, run_elevated};
            self.refresh();
            let installed = self.view.status.state != ServiceState::NotInstalled;
            let request = match action {
                ServiceAction::Install if installed => {
                    return Err("The service is already installed. Remove it first.".into());
                }
                ServiceAction::Remove if !installed && self.view.status_error.is_none() => {
                    return Err("The service is not installed.".into());
                }
                ServiceAction::Install => {
                    let (options, password) = install_options(form)?;
                    let options = check_service_options(&options)?;
                    Request::Install { options, password }
                }
                ServiceAction::Remove => Request::Remove,
            };
            let (sender, receiver) = mpsc::channel();
            std::thread::Builder::new()
                .name("topvnc-service-installer".into())
                .spawn(move || {
                    let _ = sender.send(run_elevated(request));
                })
                .map_err(|error| format!("Could not start the installer: {error}"))?;
            self.pending = Some(receiver);
            self.view.busy = Some(action);
            self.view.report = "Waiting for Windows administrator approval...".into();
            self.view.failed = false;
            Ok(())
        }
    }
}

/// The installer's own checks, made before asking for approval: plain TCP
/// only on loopback, and TLS files that load. Returns the options with TLS
/// paths made absolute here, as the elevated installer may start in another
/// folder.
#[cfg(windows)]
fn check_service_options(options: &[String]) -> Result<Vec<String>, String> {
    let normalized = crate::windows_service::service_arguments(options)?;
    let parsed = crate::desktop_host::parse_serve_arguments(&normalized).map_err(str::to_owned)?;
    parsed.tls.server().map_err(|error| error.to_string())?;
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop_host::parse_serve_arguments;
    use crate::ui::ServiceStatus;

    #[test]
    fn service_options_come_from_the_server_tab_and_parse_back() {
        let mut form = ServeForm {
            host: "0.0.0.0".into(),
            port: "5901".into(),
            password: "secret".into(),
            performance: PerformanceMode::Ultra,
            display: DisplaySelection::Number(2),
            scale: 0.5,
            mouse: MouseMode::Relative,
            priority: UpdatePriority::Changes,
            ..ServeForm::default()
        };
        form.tls.cert = Some("C:\\Certs\\my cert.pem".into());
        form.tls.key = Some("C:\\Certs\\key.pem".into());
        let (options, password) = install_options(&form).unwrap();
        assert_eq!(password, "secret");
        // The password is never an argument.
        assert!(!options.iter().any(|option| option.contains("secret")));
        assert!(!options.iter().any(|option| option == "--allow-insecure"));
        let parsed = parse_serve_arguments(&options).unwrap();
        let request = form.request().unwrap();
        assert_eq!(parsed.address, "0.0.0.0:5901");
        assert_eq!(parsed.performance, request.performance);
        assert_eq!(parsed.tls.cert, request.tls.cert);
        assert_eq!(parsed.tls.key, request.tls.key);
        assert!(!parsed.tls.unencrypted);
        assert_eq!(parsed.display, request.display);
        assert_eq!(parsed.scale, request.scale);
        assert_eq!(parsed.mouse, request.mouse);
        assert_eq!(parsed.priority, request.priority);

        // Defaults, plain TCP, and every display.
        let form = ServeForm {
            password: "p".into(),
            display: DisplaySelection::All,
            tls: topvnc::transport::TlsOptions {
                unencrypted: true,
                ..Default::default()
            },
            ..ServeForm::default()
        };
        let (options, _) = install_options(&form).unwrap();
        let parsed = parse_serve_arguments(&options).unwrap();
        assert_eq!(parsed.address, "127.0.0.1:5900");
        assert!(parsed.tls.unencrypted);
        assert_eq!(parsed.display, DisplaySelection::All);
        assert_eq!(parsed.scale, 1.0);
        assert!(!options.contains(&"--scale".to_owned()));
    }

    #[test]
    fn the_service_always_needs_a_password() {
        let mut form = ServeForm::default();
        assert!(install_options(&form).is_err());
        form.allow_insecure = true;
        assert!(install_options(&form).is_err());
        // Even with a password, "allow none" is refused rather than ignored.
        form.password = "secret".into();
        assert!(install_options(&form).is_err());
        form.allow_insecure = false;
        assert!(install_options(&form).is_ok());
        form.port = "0".into();
        assert!(install_options(&form).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn plain_tcp_beyond_this_computer_is_refused_before_approval() {
        let form = ServeForm {
            host: "0.0.0.0".into(),
            password: "secret".into(),
            tls: topvnc::transport::TlsOptions {
                unencrypted: true,
                ..Default::default()
            },
            ..ServeForm::default()
        };
        let (options, _) = install_options(&form).unwrap();
        assert!(check_service_options(&options).is_err());
        // Missing TLS files are reported here too, not by a restarting service.
        let form = ServeForm {
            password: "secret".into(),
            ..ServeForm::default()
        };
        let (options, _) = install_options(&form).unwrap();
        assert!(check_service_options(&options).is_err());
        let form = ServeForm {
            tls: topvnc::transport::TlsOptions {
                unencrypted: true,
                ..Default::default()
            },
            ..form
        };
        let (options, _) = install_options(&form).unwrap();
        assert!(check_service_options(&options).is_ok());

        // TLS files given relative to this window reach the elevated
        // installer as absolute paths.
        let folder = format!("target/service-tests-{}", std::process::id());
        std::fs::create_dir_all(&folder).unwrap();
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let (cert_path, key_path) = (format!("{folder}/cert.pem"), format!("{folder}/key.pem"));
        std::fs::write(&cert_path, cert.pem()).unwrap();
        std::fs::write(&key_path, signing_key.serialize_pem()).unwrap();
        let mut form = ServeForm {
            password: "secret".into(),
            ..ServeForm::default()
        };
        form.tls.cert = Some(cert_path.clone().into());
        form.tls.key = Some(key_path.into());
        let (options, _) = install_options(&form).unwrap();
        let checked = check_service_options(&options);
        let _ = std::fs::remove_dir_all(&folder);
        let checked = checked.unwrap();
        let cert = &checked[checked.iter().position(|o| o == "--tls-cert").unwrap() + 1];
        assert!(std::path::Path::new(cert).is_absolute());
        assert!(cert.ends_with("cert.pem"));
    }

    #[test]
    fn the_server_tab_avoids_the_running_service_port() {
        let mut runtime = ServiceRuntime::default();
        runtime.view.status = ServiceStatus {
            state: ServiceState::Running,
            address: Some("127.0.0.1:5900".into()),
            unencrypted: true,
        };
        assert!(runtime.port_conflict("127.0.0.1:5900").is_some());
        // Any host on the same port collides.
        assert!(runtime.port_conflict("0.0.0.0:5900").is_some());
        assert!(runtime.port_conflict("[::1]:5900").is_some());
        assert!(runtime.port_conflict("127.0.0.1:5901").is_none());
        assert!(same_port("[::1]:5900", "0.0.0.0:5900"));
        assert!(!same_port("bad", "bad"));
        runtime.view.status.state = ServiceState::Starting;
        assert!(runtime.port_conflict("127.0.0.1:5900").is_some());
        for state in [
            ServiceState::Stopped,
            ServiceState::Stopping,
            ServiceState::NotInstalled,
        ] {
            runtime.view.status.state = state;
            assert!(runtime.port_conflict("127.0.0.1:5900").is_none());
        }
        runtime.view.status = ServiceStatus {
            state: ServiceState::Running,
            address: None,
            unencrypted: false,
        };
        assert!(runtime.port_conflict("127.0.0.1:5900").is_none());
    }

    #[test]
    fn web_reuses_only_a_compatible_active_service() {
        let target = "127.0.0.1:5900".parse().unwrap();
        let mut runtime = ServiceRuntime::default();
        assert_eq!(runtime.web_server(target), Ok(None));
        runtime.view.status = ServiceStatus {
            state: ServiceState::Running,
            address: Some(target.to_string()),
            unencrypted: true,
        };
        assert_eq!(runtime.web_server(target), Ok(Some(HostServer::Serving)));
        runtime.view.status.state = ServiceState::Starting;
        assert_eq!(runtime.web_server(target), Ok(Some(HostServer::Starting)));
        for state in [ServiceState::Stopped, ServiceState::Stopping] {
            runtime.view.status.state = state;
            assert!(runtime.web_server(target).is_err());
        }
        runtime.view.status.state = ServiceState::Running;
        runtime.view.status.unencrypted = false;
        assert!(
            runtime.web_server(target).is_err(),
            "TLS cannot be used by a plain-RFB gateway"
        );
        runtime.view.status.unencrypted = true;
        for address in ["[::1]:5900", "0.0.0.0:5900", "127.0.0.2:5900"] {
            runtime.view.status.address = Some(address.into());
            assert!(runtime.web_server(target).is_err(), "{address}");
        }
        runtime.view.status.address = Some("127.0.0.1:5901".into());
        assert_eq!(
            runtime.web_server(target),
            Ok(None),
            "another port can host locally"
        );
    }

    #[test]
    fn finished_installers_report_and_refresh_the_status() {
        let mut runtime = ServiceRuntime::default();
        let (sender, receiver) = mpsc::channel();
        runtime.pending = Some(receiver);
        runtime.view.busy = Some(ServiceAction::Install);
        runtime.checked = Some(Instant::now());
        runtime.poll(false);
        assert_eq!(runtime.view.busy, Some(ServiceAction::Install));
        sender.send(Err("approval was cancelled".into())).unwrap();
        runtime.poll(false);
        assert_eq!(runtime.view.busy, None);
        assert!(runtime.view.failed);
        assert_eq!(runtime.view.report, "approval was cancelled");
        assert!(runtime.checked.is_none());

        let (sender, receiver) = mpsc::channel::<Result<String, String>>();
        runtime.pending = Some(receiver);
        runtime.view.busy = Some(ServiceAction::Remove);
        drop(sender);
        runtime.poll(false);
        assert!(runtime.view.failed);
        assert_eq!(runtime.view.busy, None);
    }
}
