//! One supervised child program. Reader threads drain both pipes as soon as
//! output arrives, so a child never blocks on a full pipe while TopVNC is
//! busy in a viewer session. They keep only a bounded, sanitized tail of the
//! output and a small queue of recognized events, in memory only.

use super::lines::{self, Event, Program, Stream};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

/// Recent output lines kept per child.
pub const OUTPUT_LINES: usize = 64;
/// Characters kept per output line.
pub const LINE_CHARS: usize = 512;
/// Longer lines are cut while reading; no expected line comes close.
const MAX_LINE_BYTES: usize = 4096;
/// Recognized events waiting for the runtime. Events are rare (ready,
/// pairing, connection changes); the oldest is dropped if it ever fills.
const MAX_EVENTS: usize = 32;

/// A running child, as the runtime sees it. Tests provide fakes.
pub trait Supervised: Send {
    /// Events recognized since the last call, oldest first.
    fn take_events(&mut self) -> Vec<Event>;
    /// The kept output, oldest first, already sanitized.
    fn output(&self) -> Vec<String>;
    /// Send one command line on stdin. Returns false if stdin is closed.
    fn send(&mut self, command: &str) -> bool;
    /// Close stdin; the supervised gateway treats this as "stop".
    fn close_input(&mut self);
    /// How the program ended, once it has.
    fn exit(&mut self) -> Option<String>;
    /// End the program now and wait for it.
    fn kill(&mut self);
}

/// Starts children. Tests provide fakes.
pub trait Spawner {
    fn spawn(
        &mut self,
        program: Program,
        path: &Path,
        args: &[OsString],
    ) -> Result<Box<dyn Supervised>, String>;
}

#[derive(Default)]
struct Shared {
    output: VecDeque<String>,
    events: VecDeque<Event>,
}

impl Shared {
    fn line(&mut self, program: Program, stream: Stream, line: &str) {
        if let Some(event) = lines::classify(program, stream, line) {
            if self.events.len() == MAX_EVENTS {
                self.events.pop_front();
            }
            self.events.push_back(event);
        }
        let kept = lines::sanitize(line.trim_end(), LINE_CHARS);
        if kept.trim().is_empty() {
            return;
        }
        if self.output.len() == OUTPUT_LINES {
            self.output.pop_front();
        }
        self.output.push_back(kept);
    }
}

/// Split `pipe` into lines of at most `MAX_LINE_BYTES` and pass each on.
fn read_lines(mut pipe: impl Read, mut each: impl FnMut(&str)) {
    let mut line = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let count = match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        for &byte in &chunk[..count] {
            if byte == b'\n' {
                each(&String::from_utf8_lossy(&line));
                line.clear();
            } else if line.len() < MAX_LINE_BYTES {
                line.push(byte);
            }
        }
    }
    if !line.is_empty() {
        each(&String::from_utf8_lossy(&line));
    }
}

struct ProcessChild {
    child: Child,
    stdin: Option<ChildStdin>,
    shared: Arc<Mutex<Shared>>,
    exit: Option<String>,
}

impl Supervised for ProcessChild {
    fn take_events(&mut self) -> Vec<Event> {
        self.shared
            .lock()
            .map(|mut shared| shared.events.drain(..).collect())
            .unwrap_or_default()
    }

    fn output(&self) -> Vec<String> {
        self.shared
            .lock()
            .map(|shared| shared.output.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn send(&mut self, command: &str) -> bool {
        let Some(stdin) = &mut self.stdin else {
            return false;
        };
        let sent = writeln!(stdin, "{command}").and_then(|()| stdin.flush());
        if sent.is_err() {
            self.stdin = None;
        }
        sent.is_ok()
    }

    fn close_input(&mut self) {
        self.stdin = None;
    }

    fn exit(&mut self) -> Option<String> {
        if self.exit.is_none()
            && let Ok(Some(status)) = self.child.try_wait()
        {
            self.exit = Some(match status.code() {
                Some(0) => "exited".into(),
                Some(code) => format!("exited with code {code}"),
                None => "was ended".into(),
            });
        }
        self.exit.clone()
    }

    fn kill(&mut self) {
        self.stdin = None;
        if self.exit().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.exit = Some("was stopped".into());
        }
    }
}

impl Drop for ProcessChild {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Starts real child processes without console windows, inside TopVNC's
/// job on Windows.
pub struct ProcessSpawner;

/// Remove cloudflared routing options only for the application-owned Quick
/// Tunnel. Keep unrelated environment (for example HTTPS_PROXY) intact.
fn isolate_quick_tunnel(
    command: &mut Command,
    program: Program,
    args: &[OsString],
    inherited_names: impl Iterator<Item = OsString>,
) {
    if program == Program::Tunnel && args.iter().any(|arg| arg == "--url") {
        for name in inherited_names {
            if name
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("TUNNEL_")
            {
                command.env_remove(name);
            }
        }
    }
}

impl Spawner for ProcessSpawner {
    fn spawn(
        &mut self,
        program: Program,
        path: &Path,
        args: &[OsString],
    ) -> Result<Box<dyn Supervised>, String> {
        let mut command = Command::new(path);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        isolate_quick_tunnel(
            &mut command,
            program,
            args,
            std::env::vars_os().map(|(name, _)| name),
        );
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", path.display()))?;
        #[cfg(windows)]
        if let Err(error) = super::job::adopt(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let shared = Arc::new(Mutex::new(Shared::default()));
        for (stream, pipe) in [
            (
                Stream::Stdout,
                child
                    .stdout
                    .take()
                    .map(|p| Box::new(p) as Box<dyn Read + Send>),
            ),
            (
                Stream::Stderr,
                child
                    .stderr
                    .take()
                    .map(|p| Box::new(p) as Box<dyn Read + Send>),
            ),
        ] {
            let Some(pipe) = pipe else { continue };
            let shared = Arc::clone(&shared);
            let started = std::thread::Builder::new()
                .name("topvnc-web-output".into())
                .spawn(move || {
                    read_lines(pipe, |line| {
                        if let Ok(mut shared) = shared.lock() {
                            shared.line(program, stream, line);
                        }
                    });
                });
            if let Err(error) = started {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "could not read the output of {}: {error}",
                    path.display()
                ));
            }
        }
        Ok(Box::new(ProcessChild {
            stdin: child.stdin.take(),
            child,
            shared,
            exit: None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_tunnel_cannot_inherit_named_routes_but_keeps_network_environment() {
        for quick in [false, true] {
            let mut command = Command::new("cloudflared");
            command.env("HTTPS_PROXY", "http://127.0.0.1:3128");
            let args = if quick {
                vec!["--url".into(), "http://127.0.0.1:6080".into()]
            } else {
                vec!["run".into()]
            };
            isolate_quick_tunnel(
                &mut command,
                Program::Tunnel,
                &args,
                ["TUNNEL_NAME", "TUNNEL_URL", "HTTPS_PROXY"]
                    .into_iter()
                    .map(OsString::from),
            );
            let env: Vec<_> = command.get_envs().collect();
            assert!(env.contains(&(
                std::ffi::OsStr::new("HTTPS_PROXY"),
                Some(std::ffi::OsStr::new("http://127.0.0.1:3128"))
            )));
            assert_eq!(
                env.contains(&(std::ffi::OsStr::new("TUNNEL_NAME"), None)),
                quick
            );
            assert_eq!(
                env.contains(&(std::ffi::OsStr::new("TUNNEL_URL"), None)),
                quick
            );
        }
    }

    #[test]
    fn output_is_bounded_and_sanitized() {
        let mut shared = Shared::default();
        for index in 0..(OUTPUT_LINES + 10) {
            shared.line(Program::Gateway, Stream::Stderr, &format!("line {index}"));
        }
        assert_eq!(shared.output.len(), OUTPUT_LINES);
        assert_eq!(shared.output.front().unwrap(), "line 10");
        shared.line(
            Program::Gateway,
            Stream::Stdout,
            "pair 600 https://a.workers.dev/#pair=SECRET",
        );
        assert!(!shared.output.back().unwrap().contains("SECRET"));
        assert!(matches!(shared.events.back(), Some(Event::Pair { .. })));
        shared.line(Program::Gateway, Stream::Stderr, &"z".repeat(5000));
        assert_eq!(shared.output.back().unwrap().chars().count(), LINE_CHARS);
    }

    #[test]
    fn events_are_bounded() {
        let mut shared = Shared::default();
        for index in 0..(MAX_EVENTS as u32 + 5) {
            shared.line(
                Program::Tunnel,
                Stream::Stderr,
                &format!("INF Registered tunnel connection connIndex={index}"),
            );
        }
        assert_eq!(shared.events.len(), MAX_EVENTS);
        assert_eq!(shared.events.front(), Some(&Event::Registered(5)));
    }

    #[test]
    fn lines_are_split_and_cut() {
        let input = format!("one\r\ntwo\n{}\nlast", "x".repeat(MAX_LINE_BYTES + 100));
        let mut seen = Vec::new();
        read_lines(input.as_bytes(), |line| seen.push(line.to_string()));
        assert_eq!(seen.len(), 4);
        assert_eq!(seen[0], "one\r");
        assert_eq!(seen[1], "two");
        assert_eq!(seen[2].len(), MAX_LINE_BYTES);
        assert_eq!(seen[3], "last");
    }

    /// A real child: output arrives through the reader threads, stdin
    /// commands reach it, and closing stdin ends a program that reads it.
    #[cfg(windows)]
    #[test]
    fn a_real_child_is_read_and_stopped() {
        let script = "$line = [Console]::In.ReadLine(); [Console]::Out.WriteLine('ready ' + $line); [Console]::Error.WriteLine('enrolled browser key 0a0b'); $null = [Console]::In.ReadToEnd(); exit 3";
        let args = ["-NoProfile", "-NonInteractive", "-Command", script].map(OsString::from);
        let mut child = ProcessSpawner
            .spawn(Program::Gateway, Path::new("powershell.exe"), &args)
            .unwrap();
        assert!(child.send("127.0.0.1:6080"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut events = Vec::new();
        while events.len() < 2 && std::time::Instant::now() < deadline {
            events.extend(child.take_events());
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            events.contains(&Event::GatewayReady("127.0.0.1:6080".into())),
            "{events:?}"
        );
        assert!(
            events.contains(&Event::Enrolled("0a0b".into())),
            "{events:?}"
        );
        child.close_input();
        while child.exit().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(child.exit().as_deref(), Some("exited with code 3"));
        assert!(
            child
                .output()
                .iter()
                .any(|line| line == "ready 127.0.0.1:6080")
        );
    }

    #[cfg(windows)]
    const JOB_HELPER: &str = "TOPVNC_TEST_JOB_HELPER";

    /// Run only as the helper process of
    /// `children_end_when_topvnc_is_killed`: start a long-lived child the way
    /// TopVNC does, report its process ID, and wait to be killed.
    #[cfg(windows)]
    #[test]
    fn job_helper() {
        if std::env::var_os(JOB_HELPER).is_none() {
            return;
        }
        let script = "[Console]::Out.WriteLine('pid ' + $PID); Start-Sleep -Seconds 120";
        let args = ["-NoProfile", "-NonInteractive", "-Command", script].map(OsString::from);
        let child = ProcessSpawner
            .spawn(Program::Tunnel, Path::new("powershell.exe"), &args)
            .unwrap();
        loop {
            if let Some(pid) = child
                .output()
                .iter()
                .find_map(|line| line.strip_prefix("pid "))
            {
                println!("grandchild {pid}");
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_secs(120));
    }

    /// SC-002: killing TopVNC (here, a helper using the same spawner) ends
    /// its children through the Windows job, with no chance to clean up.
    #[cfg(windows)]
    #[test]
    fn children_end_when_topvnc_is_killed() {
        use std::io::BufRead;
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };
        let mut helper = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "web_access::process::tests::job_helper",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(JOB_HELPER, "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = std::io::BufReader::new(helper.stdout.take().unwrap());
        let pid: u32 = stdout
            .lines()
            .map_while(Result::ok)
            // libtest may print the test name on the same line.
            .find_map(|line| line.split("grandchild ").nth(1)?.trim().parse().ok())
            .expect("the helper reports its child");
        // SAFETY: a plain query for a process ID; the handle is checked and
        // closed below.
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!process.is_null(), "the child should be running");
        helper.kill().unwrap();
        let _ = helper.wait();
        // SAFETY: `process` is a valid handle opened above.
        let ended = unsafe { WaitForSingleObject(process, 5000) } == WAIT_OBJECT_0;
        // SAFETY: closed once.
        unsafe { CloseHandle(process) };
        assert!(ended, "the child outlived the killed parent");
    }
}
