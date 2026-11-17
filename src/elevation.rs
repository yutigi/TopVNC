//! Serving from TopVNC reopened as administrator (spec 024, "Serving as
//! administrator").
//!
//! Windows rejects input that a process sends while an app at a higher
//! integrity level is in front (User Interface Privilege Isolation). Task
//! Manager always runs as administrator, so opening it froze remote input
//! for a server that does not, including clicks on other windows. A server
//! that runs as administrator is not blocked.
//!
//! So on Windows, starting a server without administrator rights first asks
//! Windows for approval at this computer:
//!
//! - From a TopVNC window, TopVNC reopens as administrator
//!   (`--elevated-window`), starts the same server there, and this window
//!   closes.
//! - From `--serve` in a terminal, the server runs in TopVNC started as
//!   administrator in the background (`--serve --ui-pipe NAME`). The
//!   terminal keeps the password prompt and shows the server's messages;
//!   ending it, such as with Ctrl+C, stops the server.
//!
//! If approval is cancelled or fails, the server runs without administrator
//! rights, as before, and says what that means.
//!
//! The password never appears on a command line or in a file. It goes over
//! the private pipe in `service_pipe`, and only to the process started here.

use crate::StartAction;
use crate::desktop_host::{DisplaySelection, MIN_SERVE_SCALE, MouseMode};
use crate::service_pipe::{ElevatedProcess, LaunchError, UI_PIPE_FLAG, WindowPipe, password_frame};
use crate::ui::ServeForm;
use std::error::Error;
use std::io::Write;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::time::Duration;
use topvnc::{PerformanceMode, UpdatePriority};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Put first by a TopVNC window that hands its server start to TopVNC
/// reopened as administrator, followed by `--ui-pipe NAME`.
pub const WINDOW_FLAG: &str = "--elevated-window";
/// `--serve` without asking for administrator approval, such as for
/// scripts that nobody watches.
pub const NO_ELEVATE_FLAG: &str = "--no-elevate";
/// The most a window hands over: the Server tab's form and the viewer's
/// command line.
const MAX_HANDOFF_BYTES: usize = 64 * 1024;
/// What TopVNC reopened as administrator answers once it took over.
const READY: &str = "ready";
/// How long a terminal waits for the server it relays to exit.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether this process runs as administrator: its token is elevated, as
/// it is after approval or for SYSTEM.
pub fn is_elevated() -> bool {
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut elevation = TOKEN_ELEVATION::default();
    let mut needed = 0;
    (unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut needed,
        )
    } != 0)
        && elevation.TokenIsElevated != 0
}

/// What a TopVNC window hands to TopVNC reopened as administrator.
#[derive(Clone, Debug)]
pub struct Handoff {
    /// The start to perform there.
    pub action: StartAction,
    /// The Server tab's form, with its password.
    pub form: ServeForm,
    /// The window's own command line, such as viewer TLS options.
    pub viewer: Vec<String>,
}

impl StartAction {
    fn name(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Web => "web",
            Self::WebPair => "web-pair",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        [Self::Server, Self::Web, Self::WebPair]
            .into_iter()
            .find(|action| action.name() == name)
    }
}

fn flag(value: bool) -> String {
    if value { "1" } else { "0" }.into()
}

/// A path the elevated process finds wherever it starts.
fn absolute(path: &PathBuf) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.clone())
        .to_string_lossy()
        .into_owned()
}

/// `handoff` as a frame: a little-endian total length, then key and value
/// strings, each a little-endian length and UTF-8.
fn encode(handoff: &Handoff) -> Result<Vec<u8>, String> {
    let form = &handoff.form;
    let tls = &form.tls;
    let mut fields = vec![
        ("action", handoff.action.name().to_owned()),
        ("host", form.host.clone()),
        ("port", form.port.clone()),
        ("password", form.password.clone()),
        (
            "performance",
            match form.performance {
                PerformanceMode::Standard => "standard",
                PerformanceMode::Ultra => "ultra",
            }
            .into(),
        ),
        (
            "display",
            match form.display {
                DisplaySelection::Primary => "primary".into(),
                DisplaySelection::All => "all".into(),
                DisplaySelection::Number(number) => number.to_string(),
            },
        ),
        ("allow-insecure", flag(form.allow_insecure)),
        ("scale", form.scale.to_string()),
        (
            "mouse",
            match form.mouse {
                MouseMode::Auto => "auto",
                MouseMode::Relative => "relative",
                MouseMode::Absolute => "absolute",
            }
            .into(),
        ),
        (
            "priority",
            match form.priority {
                UpdatePriority::Whole => "whole",
                UpdatePriority::Center => "center",
                UpdatePriority::Mouse => "mouse",
                UpdatePriority::Changes => "changes",
            }
            .into(),
        ),
        ("unencrypted", flag(tls.unencrypted)),
    ];
    for (key, path) in [
        ("tls-cert", &tls.cert),
        ("tls-key", &tls.key),
        ("tls-ca", &tls.ca),
    ] {
        if let Some(path) = path {
            fields.push((key, absolute(path)));
        }
    }
    if let Some(name) = &tls.server_name {
        fields.push(("tls-server-name", name.clone()));
    }
    fields.extend(
        handoff
            .viewer
            .iter()
            .map(|argument| ("viewer", argument.clone())),
    );
    let mut frame = vec![0; 4];
    for (key, value) in &mut fields {
        for text in [key.as_bytes(), value.as_bytes()] {
            frame.extend_from_slice(&(text.len() as u32).to_le_bytes());
            frame.extend_from_slice(text);
        }
        if *key == "password" {
            // SAFETY: zeros are valid UTF-8.
            unsafe { value.as_bytes_mut() }.fill(0);
        }
    }
    let length = frame.len() - 4;
    if length > MAX_HANDOFF_BYTES {
        frame.fill(0);
        return Err("The server settings are too long to hand over.".into());
    }
    frame[..4].copy_from_slice(&(length as u32).to_le_bytes());
    Ok(frame)
}

/// The handoff in a frame's payload. Anything unknown, missing, or out of
/// range is refused.
fn decode(payload: &[u8]) -> Result<Handoff, String> {
    let invalid = |what: &str| format!("the TopVNC window sent an invalid {what}");
    let mut strings = Vec::new();
    let mut rest = payload;
    while !rest.is_empty() {
        let (length, tail) = rest
            .split_first_chunk::<4>()
            .ok_or_else(|| invalid("handoff"))?;
        let length = u32::from_le_bytes(*length) as usize;
        if length > tail.len() {
            return Err(invalid("handoff"));
        }
        let (text, tail) = tail.split_at(length);
        strings.push(std::str::from_utf8(text).map_err(|_| invalid("handoff"))?);
        rest = tail;
    }
    if strings.len() % 2 != 0 {
        return Err(invalid("handoff"));
    }
    let mut action = None;
    let mut form = ServeForm::default();
    let mut viewer = Vec::new();
    for pair in strings.chunks_exact(2) {
        let (key, value) = (pair[0], pair[1]);
        let on = || match value {
            "1" => Ok(true),
            "0" => Ok(false),
            _ => Err(invalid(key)),
        };
        match key {
            "action" => action = Some(StartAction::parse(value).ok_or_else(|| invalid(key))?),
            "host" => form.host = value.into(),
            "port" => form.port = value.into(),
            "password" => form.password = value.into(),
            "performance" => {
                form.performance = match value {
                    "standard" => PerformanceMode::Standard,
                    "ultra" => PerformanceMode::Ultra,
                    _ => return Err(invalid(key)),
                }
            }
            "display" => {
                form.display = DisplaySelection::parse(value).ok_or_else(|| invalid(key))?
            }
            "allow-insecure" => form.allow_insecure = on()?,
            "scale" => {
                form.scale = value
                    .parse::<f32>()
                    .ok()
                    .filter(|scale| (MIN_SERVE_SCALE..=1.0).contains(scale))
                    .ok_or_else(|| invalid(key))?
            }
            "mouse" => {
                form.mouse = match value {
                    "auto" => MouseMode::Auto,
                    "relative" => MouseMode::Relative,
                    "absolute" => MouseMode::Absolute,
                    _ => return Err(invalid(key)),
                }
            }
            "priority" => {
                form.priority = match value {
                    "whole" => UpdatePriority::Whole,
                    "center" => UpdatePriority::Center,
                    "mouse" => UpdatePriority::Mouse,
                    "changes" => UpdatePriority::Changes,
                    _ => return Err(invalid(key)),
                }
            }
            "unencrypted" => form.tls.unencrypted = on()?,
            "tls-cert" => form.tls.cert = Some(value.into()),
            "tls-key" => form.tls.key = Some(value.into()),
            "tls-ca" => form.tls.ca = Some(value.into()),
            "tls-server-name" => form.tls.server_name = Some(value.into()),
            "viewer" => viewer.push(value.to_owned()),
            _ => return Err(invalid("setting")),
        }
    }
    Ok(Handoff {
        action: action.ok_or_else(|| invalid("handoff"))?,
        form,
        viewer,
    })
}

/// Reopen TopVNC as administrator and hand it `handoff`. `Ok` once it took
/// over, so this window should close. Blocks through the approval prompt,
/// so call it off the UI thread. Tests never call it.
pub fn hand_off(handoff: &Handoff) -> Result<(), LaunchError> {
    let mut frame = encode(handoff).map_err(LaunchError::Failed)?;
    let started = ElevatedProcess::start(WINDOW_FLAG, &[], true, "administrator window");
    let sent = started.and_then(|window| {
        window.send(&frame).map_err(LaunchError::Failed)?;
        Ok(window)
    });
    frame.fill(0);
    let report = sent?.read_report();
    if report == READY {
        Ok(())
    } else if report.is_empty() {
        Err(LaunchError::Failed(
            "TopVNC started as administrator ended before it took over.".into(),
        ))
    } else {
        Err(LaunchError::Failed(report))
    }
}

/// In TopVNC reopened by [`hand_off`], with `--ui-pipe NAME` after
/// `--elevated-window`: take the start over from the window that asked.
pub fn receive(arguments: &[String]) -> Result<Handoff, Box<dyn Error>> {
    let [flag, name] = arguments else {
        return Err(format!("usage: topvnc {WINDOW_FLAG} {UI_PIPE_FLAG} NAME").into());
    };
    if flag != UI_PIPE_FLAG {
        return Err(format!("usage: topvnc {WINDOW_FLAG} {UI_PIPE_FLAG} NAME").into());
    }
    let mut pipe = WindowPipe::connect(name)?;
    let received = pipe.read_frame(MAX_HANDOFF_BYTES).and_then(|mut frame| {
        let handoff = decode(&frame);
        frame.fill(0);
        handoff
    });
    match received {
        Ok(handoff) => {
            pipe.write_all(READY.as_bytes())?;
            Ok(handoff)
        }
        Err(error) => {
            let _ = writeln!(pipe, "{error}");
            Err(error.into())
        }
    }
}

/// `--serve` options with TLS paths made absolute, as the elevated server
/// may start in another folder.
fn absolute_options(arguments: &[String]) -> Vec<String> {
    let mut options = Vec::with_capacity(arguments.len());
    let mut path_next = false;
    for argument in arguments {
        options.push(if path_next {
            absolute(&PathBuf::from(argument))
        } else {
            argument.clone()
        });
        path_next = matches!(argument.as_str(), "--tls-cert" | "--tls-key");
    }
    options
}

/// `--serve` from a terminal: run the server in TopVNC started as
/// administrator, show its messages here, and return its exit code. Ending
/// this process stops it.
pub fn serve_elevated(arguments: &[String], password: Option<&str>) -> Result<u32, LaunchError> {
    let mut frame = password
        .map(password_frame)
        .transpose()
        .map_err(LaunchError::Failed)?;
    let started = ElevatedProcess::start(
        "--serve",
        &absolute_options(arguments),
        false,
        "administrator server",
    );
    let sent = started.and_then(|server| {
        if let Some(frame) = &frame {
            server.send(frame).map_err(LaunchError::Failed)?;
        }
        Ok(server)
    });
    if let Some(frame) = &mut frame {
        frame.fill(0);
    }
    let server = sent?;
    server.relay(|output| {
        let _ = std::io::stderr().write_all(output);
    });
    Ok(server.exit_code(EXIT_TIMEOUT).unwrap_or(1))
}

/// What a server without administrator rights cannot do, for its host.
pub const NOT_ELEVATED: &str = "Running without administrator rights: while an app that runs as \
     administrator, such as Task Manager, is in front, Windows blocks remote input. Start the \
     server again and approve the administrator prompt to control such apps.";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn form() -> ServeForm {
        ServeForm {
            tls: topvnc::transport::TlsOptions {
                unencrypted: false,
                cert: Some("cert.pem".into()),
                key: Some(r"C:\keys\key.pem".into()),
                ca: None,
                server_name: Some("host.example".into()),
            },
            performance: PerformanceMode::Ultra,
            host: "0.0.0.0".into(),
            port: "5901".into(),
            password: "pässword".into(),
            display: DisplaySelection::Number(2),
            allow_insecure: false,
            scale: 0.5,
            mouse: MouseMode::Relative,
            priority: UpdatePriority::Changes,
        }
    }

    fn payload(frame: &[u8]) -> &[u8] {
        let length = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!(length, frame.len() - 4);
        &frame[4..]
    }

    #[test]
    fn a_handoff_carries_the_server_form_and_the_viewer_command_line() {
        for action in [StartAction::Server, StartAction::Web, StartAction::WebPair] {
            let handoff = Handoff {
                action,
                form: form(),
                viewer: vec!["--tls-ca".into(), "ca.pem".into(), String::new()],
            };
            let frame = encode(&handoff).unwrap();
            let received = decode(payload(&frame)).unwrap();
            assert_eq!(received.action, action);
            assert_eq!(received.viewer, handoff.viewer);
            let (sent, form) = (&handoff.form, &received.form);
            assert_eq!(form.host, sent.host);
            assert_eq!(form.port, sent.port);
            assert_eq!(form.password, sent.password);
            assert_eq!(form.performance, sent.performance);
            assert_eq!(form.display, sent.display);
            assert_eq!(form.allow_insecure, sent.allow_insecure);
            assert_eq!(form.scale, sent.scale);
            assert_eq!(form.mouse, sent.mouse);
            assert_eq!(form.priority, sent.priority);
            assert_eq!(form.tls.unencrypted, sent.tls.unencrypted);
            assert_eq!(form.tls.server_name, sent.tls.server_name);
            // Relative TLS paths are resolved here, where they were typed.
            let cert = form.tls.cert.as_deref().unwrap();
            assert!(cert.is_absolute() && cert.ends_with("cert.pem"), "{cert:?}");
            assert_eq!(form.tls.key.as_deref(), Some(Path::new(r"C:\keys\key.pem")));
            assert_eq!(form.tls.ca, None);
        }
    }

    #[test]
    fn an_empty_password_is_handed_over_for_allow_none() {
        let mut sent = form();
        sent.password.clear();
        sent.allow_insecure = true;
        sent.tls = Default::default();
        sent.tls.unencrypted = true;
        let handoff = Handoff {
            action: StartAction::Server,
            form: sent,
            viewer: Vec::new(),
        };
        let received = decode(payload(&encode(&handoff).unwrap())).unwrap();
        assert!(received.form.password.is_empty());
        assert!(received.form.allow_insecure);
        assert!(received.form.tls.unencrypted);
        assert_eq!(received.form.tls.cert, None);
    }

    fn strings(fields: &[&str]) -> Vec<u8> {
        let mut payload = Vec::new();
        for field in fields {
            payload.extend_from_slice(&(field.len() as u32).to_le_bytes());
            payload.extend_from_slice(field.as_bytes());
        }
        payload
    }

    #[test]
    fn malformed_handoffs_are_refused() {
        assert!(decode(&strings(&["action", "server"])).is_ok());
        for fields in [
            &[][..],
            &["host", "0.0.0.0"],
            &["action"],
            &["action", "install"],
            &["action", "server", "unknown", "1"],
            &["action", "server", "allow-insecure", "yes"],
            &["action", "server", "scale", "0.1"],
            &["action", "server", "scale", "NaN"],
            &["action", "server", "display", "0"],
            &["action", "server", "mouse", "fast"],
            &["action", "server", "priority", "first"],
            &["action", "server", "performance", "max"],
        ] {
            assert!(decode(&strings(fields)).is_err(), "{fields:?}");
        }
        let valid = strings(&["action", "server"]);
        // Cut short, a length past the end, and text that is not UTF-8.
        assert!(decode(&valid[..valid.len() - 1]).is_err());
        assert!(decode(&[9, 0, 0, 0, b'a']).is_err());
        assert!(decode(&[1, 0, 0]).is_err());
        let mut bad = strings(&["action", "server", "host"]);
        bad.extend_from_slice(&[1, 0, 0, 0, 0xff]);
        assert!(decode(&bad).is_err());
    }

    #[test]
    fn handoffs_too_long_to_send_are_refused() {
        let handoff = Handoff {
            action: StartAction::Server,
            form: form(),
            viewer: vec!["x".repeat(MAX_HANDOFF_BYTES)],
        };
        assert!(encode(&handoff).is_err());
    }

    #[test]
    fn terminal_servers_get_absolute_tls_paths() {
        let options = absolute_options(&[
            "0.0.0.0:5900".into(),
            "--tls-cert".into(),
            "cert.pem".into(),
            "--tls-key".into(),
            r"C:\keys\key.pem".into(),
            "--display".into(),
            "all".into(),
        ]);
        assert_eq!(options[0], "0.0.0.0:5900");
        assert!(Path::new(&options[2]).is_absolute(), "{}", options[2]);
        assert!(options[2].ends_with("cert.pem"));
        assert_eq!(options[4], r"C:\keys\key.pem");
        assert_eq!(&options[5..], ["--display", "all"]);
    }

    #[test]
    fn this_process_reports_whether_it_runs_as_administrator() {
        // Either answer is valid; it must not fail or crash.
        let _ = is_elevated();
    }

    #[test]
    fn windows_pipes_refuse_other_arguments() {
        assert!(receive(&[]).is_err());
        assert!(receive(&["--other".into(), "x".into()]).is_err());
        assert!(receive(&[UI_PIPE_FLAG.into(), r"\\server\share".into()]).is_err());
    }
}
