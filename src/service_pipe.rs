//! The private pipe between the TopVNC window and the elevated service
//! installer (spec 020, "Installing from the TopVNC window").
//!
//! Administrator approval (`runas`) cannot hand a process standard input,
//! and the VNC password must never be on a command line or in a file. So the
//! window creates a named pipe that only SYSTEM, Administrators, and the
//! signed-in user may open, starts `topvnc --install-service --ui-pipe NAME`
//! elevated, checks that the client is that process, and only then sends the
//! password. The installer writes its summary or error back on the same pipe.

use std::error::Error;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_CANCELLED, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GENERIC_READ,
    GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetExitCodeProcess, GetProcessId, OpenProcessToken,
    WaitForMultipleObjects, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

/// Put first after `--install-service` or `--uninstall-service` by the
/// window, followed by the pipe's name.
pub const UI_PIPE_FLAG: &str = "--ui-pipe";
const PIPE_PREFIX: &str = "topvnc-service-";
const MAX_PASSWORD_BYTES: usize = 1024;
/// The installer's report is a few lines; more is cut off.
const MAX_REPORT_BYTES: usize = 64 * 1024;
/// From approval to the installer reaching the pipe.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// Uninstalling waits up to 20 seconds for the service to stop.
const EXIT_TIMEOUT: Duration = Duration::from_secs(60);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Whether `name` is a pipe name the window makes: a fixed prefix and 32
/// lowercase hex digits. The installer accepts nothing else, so it never
/// opens a file, a device, or a remote share.
pub fn valid_pipe_name(name: &str) -> bool {
    name.strip_prefix(PIPE_PREFIX).is_some_and(|id| {
        id.len() == 32
            && id
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn pipe_path(name: &str) -> Vec<u16> {
    wide(&format!(r"\\.\pipe\{name}"))
}

/// The password as the window sends it: a little-endian length, then UTF-8.
fn password_frame(password: &str) -> Result<Vec<u8>, String> {
    let bytes = password.as_bytes();
    if bytes.is_empty() {
        return Err("The service needs a VNC password.".into());
    }
    if bytes.len() > MAX_PASSWORD_BYTES {
        return Err(format!(
            "The VNC password must be at most {MAX_PASSWORD_BYTES} bytes."
        ));
    }
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(bytes);
    Ok(frame)
}

fn read_password_frame(reader: &mut impl Read) -> Result<String, String> {
    let failed = |error: std::io::Error| {
        format!("could not read the VNC password from the TopVNC window: {error}")
    };
    let mut length = [0; 4];
    reader.read_exact(&mut length).map_err(failed)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_PASSWORD_BYTES {
        return Err("the TopVNC window sent a VNC password of an invalid length".into());
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).map_err(failed)?;
    String::from_utf8(bytes).map_err(|error| {
        error.into_bytes().fill(0);
        "the VNC password from the TopVNC window is not valid text".into()
    })
}

// ---- The elevated installer's end --------------------------------------

/// The installer's connection to the window that started it.
pub struct WindowPipe(File);

impl WindowPipe {
    fn connect(name: &str) -> Result<Self, String> {
        if !valid_pipe_name(name) {
            return Err(format!("{name} is not a TopVNC window pipe"));
        }
        let path = pipe_path(name);
        // Identification level: whoever owns the pipe cannot act as this
        // administrator.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(format!(
                "could not reach the TopVNC window: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self(unsafe { File::from_raw_handle(handle) }))
    }

    pub fn read_password(&mut self) -> Result<String, String> {
        read_password_frame(&mut self.0)
    }
}

impl Write for WindowPipe {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// Run an installer `step` for the window named by `name`. Its error is
/// written to the window too, as nobody sees this process's console.
pub fn report_to_window(
    name: &str,
    step: impl FnOnce(&mut WindowPipe) -> Result<(), Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let mut pipe = WindowPipe::connect(name)?;
    let result = step(&mut pipe);
    if let Err(error) = &result {
        let _ = writeln!(pipe, "{error}");
    }
    let _ = pipe.flush();
    result
}

// ---- The window's end --------------------------------------------------

/// What the window asks the elevated installer to do.
pub enum Request {
    /// Install with these `--serve` options and password.
    Install {
        options: Vec<String>,
        password: String,
    },
    Remove,
}

/// The window's end of one installer run: the pipe and an event for its
/// overlapped operations.
struct ControlPipe {
    name: String,
    pipe: OwnedHandle,
    event: OwnedHandle,
}

/// The signed-in user's SID, as text for a security descriptor.
fn current_user_sid() -> Result<String, String> {
    let failed = |action: &str| format!("Could not {action}: {}", std::io::Error::last_os_error());
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(failed("read this account"));
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut needed = 0;
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if needed == 0 || needed > 1024 {
        return Err(failed("read this account"));
    }
    // `u64` keeps the structure's pointer aligned.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(failed("read this account"));
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return Err(failed("read this account"));
    }
    // SAFETY: Windows returned a null-terminated string, freed below.
    let sid = unsafe {
        let length = (0..).take_while(|&at| *text.add(at) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
    };
    unsafe { LocalFree(text.cast()) };
    Ok(sid)
}

impl ControlPipe {
    fn create() -> Result<Self, String> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random)
            .map_err(|error| format!("Could not name the pipe: {error}"))?;
        let name = random
            .iter()
            .fold(PIPE_PREFIX.to_owned(), |mut name, byte| {
                name.push_str(&format!("{byte:02x}"));
                name
            });
        // Only SYSTEM, Administrators (the approved installer), and this
        // account; the default would let every user read it.
        let sddl = wide(&format!(
            "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{})",
            current_user_sid()?
        ));
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(format!(
                "Could not secure the pipe: {}",
                std::io::Error::last_os_error()
            ));
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let path = pipe_path(&name);
        // The first instance: if the name already exists, this fails rather
        // than joining someone else's pipe.
        let pipe = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                0,
                &attributes,
            )
        };
        let created = std::io::Error::last_os_error();
        unsafe { LocalFree(descriptor) };
        if pipe == INVALID_HANDLE_VALUE {
            return Err(format!("Could not create the pipe: {created}"));
        }
        let pipe = unsafe { OwnedHandle::from_raw_handle(pipe) };
        // Manual reset, as overlapped operations require.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(format!(
                "Could not create the pipe: {}",
                std::io::Error::last_os_error()
            ));
        }
        let event = unsafe { OwnedHandle::from_raw_handle(event) };
        Ok(Self { name, pipe, event })
    }

    fn overlapped(&self) -> OVERLAPPED {
        OVERLAPPED {
            hEvent: self.event.as_raw_handle(),
            ..Default::default()
        }
    }

    /// Wait for the process `client` to connect. Fails if it ends first, if
    /// any other process connects, or after `timeout`.
    fn accept(&self, client: HANDLE, timeout: Duration) -> Result<(), String> {
        let pipe = self.pipe.as_raw_handle();
        let mut overlapped = self.overlapped();
        if unsafe { ConnectNamedPipe(pipe, &mut overlapped) } == 0 {
            match unsafe { GetLastError() } {
                ERROR_PIPE_CONNECTED => {}
                ERROR_IO_PENDING => {
                    let handles = [self.event.as_raw_handle(), client];
                    let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
                    let waited =
                        unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, milliseconds) };
                    let wait_error = std::io::Error::last_os_error();
                    let mut transferred = 0;
                    if waited != WAIT_OBJECT_0 {
                        // The overlapped structure must outlive the operation.
                        unsafe {
                            CancelIoEx(pipe, &overlapped);
                            GetOverlappedResult(pipe, &overlapped, &mut transferred, 1);
                        }
                        return Err(match waited {
                            _ if waited == WAIT_OBJECT_0 + 1 => {
                                "The installer ended before it reached the TopVNC window.".into()
                            }
                            WAIT_TIMEOUT => {
                                "The installer did not reach the TopVNC window in time.".into()
                            }
                            _ => format!("Could not wait for the installer: {wait_error}"),
                        });
                    }
                    if unsafe { GetOverlappedResult(pipe, &overlapped, &mut transferred, 0) } == 0 {
                        return Err(format!(
                            "The installer could not connect: {}",
                            std::io::Error::last_os_error()
                        ));
                    }
                }
                error => {
                    return Err(format!(
                        "Could not wait for the installer: {}",
                        std::io::Error::from_raw_os_error(error as i32)
                    ));
                }
            }
        }
        // The password goes only to the process this window started.
        let mut connected = 0;
        if unsafe { GetNamedPipeClientProcessId(pipe, &mut connected) } == 0
            || connected != unsafe { GetProcessId(client) }
        {
            return Err(
                "Another process connected to the installer's pipe, so nothing was sent.".into(),
            );
        }
        Ok(())
    }

    /// One overlapped read or write, waited for. A closed client reads as
    /// the end.
    fn transfer(
        &self,
        start: impl FnOnce(HANDLE, *mut OVERLAPPED, &mut u32) -> i32,
    ) -> std::io::Result<usize> {
        let pipe = self.pipe.as_raw_handle();
        let mut overlapped = self.overlapped();
        let mut transferred = 0;
        if start(pipe, &mut overlapped, &mut transferred) == 0 {
            match unsafe { GetLastError() } {
                ERROR_IO_PENDING => {
                    if unsafe { GetOverlappedResult(pipe, &overlapped, &mut transferred, 1) } == 0 {
                        let error = std::io::Error::last_os_error();
                        return if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                            Ok(0)
                        } else {
                            Err(error)
                        };
                    }
                }
                ERROR_BROKEN_PIPE => return Ok(0),
                error => return Err(std::io::Error::from_raw_os_error(error as i32)),
            }
        }
        Ok(transferred as usize)
    }

    fn write_all(&self, mut bytes: &[u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            let chunk = &bytes[..bytes.len().min(u32::MAX as usize)];
            let written = self.transfer(|pipe, overlapped, transferred| unsafe {
                WriteFile(
                    pipe,
                    chunk.as_ptr(),
                    chunk.len() as u32,
                    transferred,
                    overlapped,
                )
            })?;
            if written == 0 {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            bytes = &bytes[written..];
        }
        Ok(())
    }

    /// Everything the installer writes until it closes the pipe, up to
    /// [`MAX_REPORT_BYTES`].
    fn read_report(&self) -> String {
        let mut report = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = self.transfer(|pipe, overlapped, transferred| unsafe {
                ReadFile(
                    pipe,
                    chunk.as_mut_ptr(),
                    chunk.len() as u32,
                    transferred,
                    overlapped,
                )
            });
            match read {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let keep = count.min(MAX_REPORT_BYTES - report.len());
                    report.extend_from_slice(&chunk[..keep]);
                }
            }
        }
        String::from_utf8_lossy(&report).trim().to_owned()
    }
}

/// Start `program` with `arguments` after Windows administrator approval,
/// with no window.
fn launch_elevated(program: &Path, arguments: &[String]) -> Result<OwnedHandle, String> {
    let file = wide(&program.to_string_lossy());
    let parameters = wide(&crate::windows_service::argument_line(arguments));
    let verb = wide("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: parameters.as_ptr(),
        nShow: SW_HIDE,
        ..Default::default()
    };
    // The shell may hand the request to COM components.
    let com = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    let launched = unsafe { ShellExecuteExW(&mut info) };
    let error = std::io::Error::last_os_error();
    if com >= 0 {
        unsafe { CoUninitialize() };
    }
    if launched == 0 {
        return Err(if error.raw_os_error() == Some(ERROR_CANCELLED as i32) {
            "Windows administrator approval was cancelled. Nothing was changed.".into()
        } else {
            format!("Could not ask Windows for administrator approval: {error}")
        });
    }
    if info.hProcess.is_null() {
        return Err("Windows did not report the installer process.".into());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(info.hProcess) })
}

/// Run `request` in an elevated installer and return its report. Blocks
/// through the approval prompt, so call it off the UI thread. Tests never
/// call it: it asks for administrator approval.
pub fn run_elevated(request: Request) -> Result<String, String> {
    let mut frame = match &request {
        Request::Install { password, .. } => Some(password_frame(password)?),
        Request::Remove => None,
    };
    let pipe = ControlPipe::create()?;
    let mut arguments = vec![
        match request {
            Request::Install { .. } => "--install-service",
            Request::Remove => "--uninstall-service",
        }
        .to_owned(),
        UI_PIPE_FLAG.to_owned(),
        pipe.name.clone(),
    ];
    if let Request::Install { options, .. } = &request {
        arguments.extend(options.iter().cloned());
    }
    let program = std::env::current_exe()
        .map_err(|error| format!("Could not find the TopVNC program: {error}"))?;
    let installer = launch_elevated(&program, &arguments)?;
    let accepted = pipe.accept(installer.as_raw_handle(), CONNECT_TIMEOUT);
    if accepted.is_ok()
        && let Some(frame) = &frame
    {
        // An installer that already failed may have closed the pipe; its
        // report says why.
        let _ = pipe.write_all(frame);
    }
    if let Some(frame) = &mut frame {
        frame.fill(0);
    }
    accepted?;
    let report = pipe.read_report();
    let milliseconds = u32::try_from(EXIT_TIMEOUT.as_millis()).unwrap_or(u32::MAX);
    unsafe { WaitForSingleObject(installer.as_raw_handle(), milliseconds) };
    let mut code = 0;
    if unsafe { GetExitCodeProcess(installer.as_raw_handle(), &mut code) } == 0 {
        return Err(format!(
            "Could not read the installer's result: {}",
            std::io::Error::last_os_error()
        ));
    }
    match code {
        0 => Ok(report),
        _ if !report.is_empty() => Err(report),
        code => Err(format!("The installer failed with exit code {code}.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    #[test]
    fn installers_accept_only_window_pipe_names() {
        let name = format!("{PIPE_PREFIX}{}", "0123456789abcdef".repeat(2));
        assert!(valid_pipe_name(&name));
        for name in [
            String::new(),
            PIPE_PREFIX.to_owned(),
            format!("{PIPE_PREFIX}{}", "0".repeat(31)),
            format!("{PIPE_PREFIX}{}", "0".repeat(33)),
            format!("{PIPE_PREFIX}{}", "A".repeat(32)),
            format!(r"{PIPE_PREFIX}{}\..\x", "0".repeat(28)),
            format!(r"\\server\pipe\{name}"),
            format!("other-{}", "0".repeat(32)),
        ] {
            assert!(!valid_pipe_name(&name), "{name}");
        }
        assert!(WindowPipe::connect(r"\\server\share\file").is_err());
    }

    #[test]
    fn password_frames_round_trip_and_reject_bad_lengths() {
        let frame = password_frame("pässword").unwrap();
        assert_eq!(&frame[..4], &("pässword".len() as u32).to_le_bytes());
        assert_eq!(
            read_password_frame(&mut frame.as_slice()).unwrap(),
            "pässword"
        );
        assert!(password_frame("").is_err());
        assert!(password_frame(&"x".repeat(MAX_PASSWORD_BYTES)).is_ok());
        assert!(password_frame(&"x".repeat(MAX_PASSWORD_BYTES + 1)).is_err());
        for frame in [
            0u32.to_le_bytes().to_vec(),
            (MAX_PASSWORD_BYTES as u32 + 1).to_le_bytes().to_vec(),
            // Shorter than its length.
            [&5u32.to_le_bytes()[..], b"abc"].concat(),
            [&2u32.to_le_bytes()[..], &[0xff, 0xfe]].concat(),
            vec![1, 0],
        ] {
            assert!(read_password_frame(&mut frame.as_slice()).is_err());
        }
    }

    /// The window's end with an installer in a thread of this process,
    /// which stands in for the elevated one. Nothing is elevated.
    #[test]
    fn the_window_sends_the_password_to_its_installer_and_reads_the_report() {
        let pipe = ControlPipe::create().unwrap();
        // The name exists only once.
        assert!(
            unsafe {
                CreateNamedPipeW(
                    pipe_path(&pipe.name).as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE,
                    1,
                    0,
                    0,
                    0,
                    std::ptr::null(),
                )
            } == INVALID_HANDLE_VALUE
        );
        let name = pipe.name.clone();
        let installer = std::thread::spawn(move || {
            report_to_window(&name, |window| {
                let password = window.read_password()?;
                writeln!(window, "Installed with a {}-byte password.", password.len())?;
                Err("then failed".into())
            })
            .map_err(|error| error.to_string())
        });
        // This process is the client, so a real handle to it stands in for
        // the installer's; it does not end while the test waits.
        let this = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                GetCurrentProcessId(),
            )
        };
        assert!(!this.is_null());
        let this = unsafe { OwnedHandle::from_raw_handle(this) };
        pipe.accept(this.as_raw_handle(), Duration::from_secs(10))
            .unwrap();
        pipe.write_all(&password_frame("secret").unwrap()).unwrap();
        assert_eq!(
            pipe.read_report(),
            "Installed with a 6-byte password.\nthen failed"
        );
        assert!(installer.join().unwrap().is_err());
    }

    #[test]
    fn the_window_refuses_other_clients_and_installers_that_end_early() {
        // Stands in for the started installer; it never connects.
        let mut started = std::process::Command::new("cmd.exe")
            .args(["/c", "exit", "0"])
            .spawn()
            .unwrap();
        started.wait().unwrap();

        // A client that is not the started installer gets nothing: this
        // process connects first.
        let pipe = ControlPipe::create().unwrap();
        let _other = WindowPipe::connect(&pipe.name).unwrap();
        let error = pipe
            .accept(started.as_raw_handle(), Duration::from_secs(10))
            .unwrap_err();
        assert!(error.contains("Another process"), "{error}");

        // An installer that ends without connecting is reported.
        let pipe = ControlPipe::create().unwrap();
        let error = pipe
            .accept(started.as_raw_handle(), Duration::from_secs(10))
            .unwrap_err();
        assert!(error.contains("ended before"), "{error}");
    }
}
