//! The TopVNC host service (spec 020): lets an authorized viewer reach the
//! Windows lock and sign-in screens and sign in there.
//!
//! Windows shows those screens on the Winlogon desktop, which only SYSTEM
//! can capture and send input to. The `TopVNCHost` service runs as
//! LocalSystem in session 0, which has no desktop, and starts a host agent
//! (`topvnc --host-agent`) as SYSTEM in the session at the physical console.
//! The agent is the ordinary `--serve` server with capture and input
//! following the desktop that receives input. Ctrl+Alt+Delete cannot be
//! injected as keys, so the agent asks the service, which calls `SendSAS`.
//!
//! Nothing here bypasses Windows sign-in: viewers authenticate to the VNC
//! server, then see and use the normal lock screen, which asks for the
//! Windows password as usual.

use crate::desktop_host::{ServerNotice, parse_serve_arguments};
use crate::service_pipe::{self, UI_PIPE_FLAG};
use crate::ui::{ServiceState, ServiceStatus};
use crate::web_access::job;
use crate::windows_server;
use std::error::Error;
use std::ffi::c_void;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::ToSocketAddrs;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_CALL_NOT_IMPLEMENTED,
    ERROR_FAILED_SERVICE_CONTROLLER_CONNECT, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED,
    ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_NOT_ACTIVE, ERROR_SUCCESS, GENERIC_READ,
    GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree, NO_ERROR, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authentication::Identity::{
    LSA_HANDLE, LSA_OBJECT_ATTRIBUTES, LSA_UNICODE_STRING, LsaClose, LsaFreeMemory,
    LsaNtStatusToWinError, LsaOpenPolicy, LsaRetrievePrivateData, LsaStorePrivateData,
    POLICY_CREATE_SECRET, POLICY_GET_PRIVATE_INFORMATION,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    DuplicateTokenEx, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, SecurityIdentification,
    SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_SESSIONID, TOKEN_ASSIGN_PRIMARY,
    TOKEN_DUPLICATE, TOKEN_QUERY, TokenPrimary, TokenSessionId,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_READ_ATTRIBUTES,
    OPEN_EXISTING, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, ReadFile, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, CloseServiceHandle, ControlService, CreateServiceW, DeleteService,
    OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW, QueryServiceConfigW, QueryServiceStatus,
    RegisterServiceCtrlHandlerExW, SC_ACTION, SC_ACTION_RESTART, SC_HANDLE, SC_MANAGER_CONNECT,
    SC_MANAGER_CREATE_SERVICE, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_ALL_ACCESS,
    SERVICE_AUTO_START, SERVICE_CONFIG_DESCRIPTION, SERVICE_CONFIG_FAILURE_ACTIONS,
    SERVICE_CONTINUE_PENDING, SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN,
    SERVICE_CONTROL_STOP, SERVICE_DESCRIPTIONW, SERVICE_ERROR_NORMAL, SERVICE_FAILURE_ACTIONSW,
    SERVICE_PAUSE_PENDING, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
    SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE,
    SERVICE_STOP, SERVICE_STOP_PENDING, SERVICE_STOPPED, SERVICE_TABLE_ENTRYW,
    SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW, StartServiceW,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateEventW, CreateProcessAsUserW, GetCurrentProcess,
    GetCurrentProcessId, GetProcessId, INFINITE, OpenProcessToken, PROCESS_INFORMATION,
    ResumeThread, STARTUPINFOW, SetEvent, TerminateProcess, WaitForMultipleObjects,
    WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

/// The service's name in the service manager; also `sc query TopVNCHost`.
pub const SERVICE_NAME: &str = "TopVNCHost";
const SERVICE_DISPLAY_NAME: &str = "TopVNC Host";
const SERVICE_DESCRIPTION: &str = "Serves this computer's screen to authorized TopVNC viewers, \
     including the Windows lock and sign-in screens.";
/// The LSA private data key holding the VNC password. `L$` keeps it local
/// to this computer; only SYSTEM can read it back.
const PASSWORD_SECRET: &str = "L$TopVNCHostPassword";
/// Under Program Files, which only administrators can change: the service
/// runs this copy as SYSTEM, so it must not be writable by users.
const INSTALL_FOLDER: &str = "TopVNC";
const SAS_POLICY_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
const SAS_POLICY_VALUE: &str = "SoftwareSASGeneration";
const AGENT_DESKTOP: &str = "winsta0\\default";
const AGENT_RESTART_MIN: Duration = Duration::from_secs(1);
const AGENT_RESTART_MAX: Duration = Duration::from_secs(30);
/// An agent that ran this long resets the restart backoff.
const AGENT_STABLE: Duration = Duration::from_secs(60);
const AGENT_STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// From starting an agent to it opening both of its pipes.
const AGENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Put first after `--host-agent` by the service, followed by the pipes'
/// name.
const AGENT_PIPE_FLAG: &str = "--agent-pipe";
const AGENT_PIPE_PREFIX: &str = "topvnc-host-agent-";
/// Only SYSTEM, which the service and its agents run as, may open the pipes.
const AGENT_PIPE_SECURITY: &str = "D:P(A;;GA;;;SY)";
const CONSOLE_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const SERVICE_STOP_TIMEOUT: Duration = Duration::from_secs(20);
/// The service log is rotated past this size.
const MAX_LOG_BYTES: u64 = 1024 * 1024;
const NO_CONSOLE_SESSION: u32 = u32::MAX;
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
/// The standard right to delete an object, here the service.
const DELETE_ACCESS: u32 = 0x0001_0000;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

/// Validate `topvnc --serve` options for the service and return them with
/// TLS file paths made absolute, as the service starts in System32. The
/// service always asks for a password, and plain TCP is accepted only on a
/// loopback address, such as for the web gateway on this computer.
pub fn service_arguments(arguments: &[String]) -> Result<Vec<String>, String> {
    let options = parse_serve_arguments(arguments).map_err(str::to_owned)?;
    if options.allow_insecure {
        return Err(
            "the host service always requires a VNC password; remove --allow-insecure".into(),
        );
    }
    if options.tls.unencrypted {
        let addresses = options
            .address
            .to_socket_addrs()
            .map_err(|error| format!("could not resolve {}: {error}", options.address))?
            .collect::<Vec<_>>();
        if addresses.is_empty() || addresses.iter().any(|address| !address.ip().is_loopback()) {
            return Err(
                "the host service sends plain TCP only to this computer: use a loopback \
                 address such as 127.0.0.1:5900 with --unencrypted, or --tls-cert and --tls-key"
                    .into(),
            );
        }
    }
    let mut normalized = Vec::with_capacity(arguments.len());
    let mut values = arguments.iter();
    while let Some(argument) = values.next() {
        normalized.push(argument.clone());
        if matches!(argument.as_str(), "--tls-cert" | "--tls-key")
            && let Some(path) = values.next()
        {
            let absolute = std::path::absolute(path)
                .map_err(|error| format!("could not resolve {path}: {error}"))?;
            normalized.push(absolute.to_string_lossy().into_owned());
        }
    }
    Ok(normalized)
}

/// Quote `argument` for a Windows command line, as `CommandLineToArgvW` and
/// the Rust runtime split it.
fn quote_argument(argument: &str, line: &mut String) {
    if !argument.is_empty() && !argument.contains([' ', '\t', '\n', '\u{b}', '"']) {
        line.push_str(argument);
        return;
    }
    line.push('"');
    let mut backslashes = 0;
    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                // Escape the backslashes before a quote, then the quote.
                line.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                line.push('"');
                backslashes = 0;
            }
            _ => {
                line.extend(std::iter::repeat_n('\\', backslashes));
                line.push(character);
                backslashes = 0;
            }
        }
    }
    // Backslashes before the closing quote are doubled.
    line.extend(std::iter::repeat_n('\\', backslashes * 2));
    line.push('"');
}

/// A command line running `program` with `arguments`.
pub fn command_line(program: &Path, arguments: &[String]) -> String {
    let mut line = String::new();
    quote_argument(&program.to_string_lossy(), &mut line);
    if !arguments.is_empty() {
        line.push(' ');
        line.push_str(&argument_line(arguments));
    }
    line
}

/// `arguments` quoted for a Windows command line, without the program.
pub fn argument_line(arguments: &[String]) -> String {
    let mut line = String::new();
    for (index, argument) in arguments.iter().enumerate() {
        if index > 0 {
            line.push(' ');
        }
        quote_argument(argument, &mut line);
    }
    line
}

/// Split a command line as Windows does for a program's arguments.
fn split_command_line(line: &str) -> Vec<String> {
    let line = wide(line);
    let mut count = 0;
    let list = unsafe { CommandLineToArgvW(line.as_ptr(), &mut count) };
    if list.is_null() {
        return Vec::new();
    }
    // SAFETY: Windows returned `count` null-terminated strings, freed below.
    let arguments = unsafe {
        std::slice::from_raw_parts(list, usize::try_from(count).unwrap_or(0))
            .iter()
            .map(|&argument| from_wide(argument))
            .collect()
    };
    unsafe { LocalFree(list.cast()) };
    arguments
}

/// The text of a null-terminated UTF-16 string.
///
/// # Safety
/// `text` must be null or point to a null-terminated UTF-16 string.
unsafe fn from_wide(text: *const u16) -> String {
    if text.is_null() {
        return String::new();
    }
    unsafe {
        let length = (0..).take_while(|&at| *text.add(at) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
    }
}

/// What the agent tells the service on its standard output, one per line.
#[derive(Debug, PartialEq, Eq)]
pub enum AgentMessage {
    /// A viewer pressed Ctrl+Alt+Delete.
    SecureAttention,
    /// A line for the service log: server notices, or anything unexpected,
    /// such as a panic message.
    Log(String),
}

impl AgentMessage {
    pub fn parse(line: &str) -> Self {
        let line = line.trim_end_matches(['\r', '\n']);
        match line {
            "SAS" => Self::SecureAttention,
            _ => Self::Log(line.strip_prefix("LOG ").unwrap_or(line).to_owned()),
        }
    }
}

// ---- Service manager handles -------------------------------------------

struct ScHandle(SC_HANDLE);

impl ScHandle {
    fn manager(access: u32) -> std::io::Result<Self> {
        let handle = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), access) };
        if handle.is_null() {
            Err(last_error())
        } else {
            Ok(Self(handle))
        }
    }

    fn service(&self, access: u32) -> std::io::Result<Self> {
        let name = wide(SERVICE_NAME);
        let handle = unsafe { OpenServiceW(self.0, name.as_ptr(), access) };
        if handle.is_null() {
            Err(last_error())
        } else {
            Ok(Self(handle))
        }
    }

    fn state(&self) -> std::io::Result<SERVICE_STATUS_CURRENT_STATE> {
        let mut status = SERVICE_STATUS::default();
        if unsafe { QueryServiceStatus(self.0, &mut status) } == 0 {
            return Err(last_error());
        }
        Ok(status.dwCurrentState)
    }

    /// The service's command line; needs `SERVICE_QUERY_CONFIG`.
    fn binary_path(&self) -> std::io::Result<String> {
        // The service manager caps the configuration at 8 KiB.
        const MAX_CONFIG_BYTES: u32 = 8 * 1024;
        let mut needed = 0;
        unsafe { QueryServiceConfigW(self.0, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 || needed > MAX_CONFIG_BYTES {
            return Err(last_error());
        }
        // `u64` keeps the structure's pointers aligned.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
        if unsafe { QueryServiceConfigW(self.0, buffer.as_mut_ptr().cast(), needed, &mut needed) }
            == 0
        {
            return Err(last_error());
        }
        // SAFETY: the service manager filled the buffer with the structure
        // and the strings it points to.
        Ok(unsafe {
            let config = &*buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>();
            from_wide(config.lpBinaryPathName)
        })
    }
}

impl Drop for ScHandle {
    fn drop(&mut self) {
        unsafe { CloseServiceHandle(self.0) };
    }
}

fn is_error(error: &std::io::Error, code: u32) -> bool {
    error.raw_os_error() == Some(code as i32)
}

fn administrator_needed(error: std::io::Error) -> Box<dyn Error> {
    if is_error(&error, ERROR_ACCESS_DENIED) {
        "this needs an administrator terminal: right-click Terminal and choose Run as administrator"
            .into()
    } else {
        format!("could not open the Windows service manager: {error}").into()
    }
}

// ---- The VNC password, as LSA private data -----------------------------

struct LsaPolicy(LSA_HANDLE);

impl LsaPolicy {
    fn open(access: u32) -> Result<Self, String> {
        let attributes = LSA_OBJECT_ATTRIBUTES {
            Length: size_of::<LSA_OBJECT_ATTRIBUTES>() as u32,
            ..Default::default()
        };
        let mut handle = 0;
        let status = unsafe { LsaOpenPolicy(std::ptr::null(), &attributes, access, &mut handle) };
        lsa_result(status, "open the local security policy")?;
        Ok(Self(handle))
    }
}

impl Drop for LsaPolicy {
    fn drop(&mut self) {
        unsafe { LsaClose(self.0) };
    }
}

fn lsa_result(status: i32, action: &str) -> Result<(), String> {
    if status == 0 {
        return Ok(());
    }
    let error = unsafe { LsaNtStatusToWinError(status) };
    Err(format!(
        "could not {action}: {}",
        std::io::Error::from_raw_os_error(error as i32)
    ))
}

/// A counted UTF-16 string over `text`, which must outlive it.
fn lsa_string(text: &mut [u16]) -> Result<LSA_UNICODE_STRING, String> {
    let bytes = u16::try_from(size_of_val(text)).map_err(|_| "the text is too long")?;
    Ok(LSA_UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: text.as_mut_ptr(),
    })
}

/// Store the password, or delete it with `None`.
fn store_password(password: Option<&str>) -> Result<(), String> {
    let policy = LsaPolicy::open(POLICY_CREATE_SECRET as u32)?;
    let mut name = PASSWORD_SECRET.encode_utf16().collect::<Vec<_>>();
    let name = lsa_string(&mut name)?;
    let mut value = password
        .map(|password| password.encode_utf16().collect::<Vec<_>>())
        .unwrap_or_default();
    let data = lsa_string(&mut value)?;
    let data = if password.is_some() {
        &data as *const LSA_UNICODE_STRING
    } else {
        std::ptr::null()
    };
    let status = unsafe { LsaStorePrivateData(policy.0, &name, data) };
    value.fill(0);
    lsa_result(status, "store the VNC password")
}

/// Read the password back; only SYSTEM can.
fn read_password() -> Result<String, String> {
    let policy = LsaPolicy::open(POLICY_GET_PRIVATE_INFORMATION as u32)?;
    let mut name = PASSWORD_SECRET.encode_utf16().collect::<Vec<_>>();
    let name = lsa_string(&mut name)?;
    let mut data: *mut LSA_UNICODE_STRING = std::ptr::null_mut();
    let status = unsafe { LsaRetrievePrivateData(policy.0, &name, &mut data) };
    lsa_result(
        status,
        "read the VNC password; reinstall with topvnc --install-service",
    )?;
    if data.is_null() {
        return Err("the stored VNC password is missing".into());
    }
    // SAFETY: LSA returned a valid counted string, freed below.
    let password = unsafe {
        let data = &*data;
        let units = std::slice::from_raw_parts_mut(data.Buffer, usize::from(data.Length) / 2);
        let password = String::from_utf16(units);
        units.fill(0);
        password
    };
    unsafe { LsaFreeMemory(data.cast()) };
    password.map_err(|_| "the stored VNC password is not valid text".into())
}

// ---- Install and uninstall ---------------------------------------------

fn install_folder() -> Result<PathBuf, String> {
    let program_files =
        std::env::var_os("ProgramFiles").ok_or("the ProgramFiles folder is not set")?;
    Ok(PathBuf::from(program_files).join(INSTALL_FOLDER))
}

/// `SendSAS` from a service works only when this policy allows services
/// (1) or both services and ease-of-access tools (3).
pub fn software_sas_allowed() -> bool {
    let key = wide(SAS_POLICY_KEY);
    let value = wide(SAS_POLICY_VALUE);
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    let read = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut data).cast(),
            &mut size,
        )
    };
    read == ERROR_SUCCESS && data & 1 != 0
}

// ---- Status, for the TopVNC window -------------------------------------

/// The service's state and address. Needs no administrator rights.
pub fn query_status() -> Result<ServiceStatus, String> {
    let manager = ScHandle::manager(SC_MANAGER_CONNECT)
        .map_err(|error| format!("could not open the Windows service manager: {error}"))?;
    let service = match manager.service(SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG) {
        Ok(service) => service,
        Err(error) if is_error(&error, ERROR_SERVICE_DOES_NOT_EXIST) => {
            return Ok(ServiceStatus::default());
        }
        Err(error) => {
            return Err(format!(
                "could not read the {SERVICE_NAME} service: {error}"
            ));
        }
    };
    let state = service
        .state()
        .map_err(|error| format!("could not read the {SERVICE_NAME} service: {error}"))?;
    let command = service
        .binary_path()
        .map_err(|error| format!("could not read the {SERVICE_NAME} configuration: {error}"))?;
    let options = service_options(&command)
        .ok_or_else(|| format!("could not parse the {SERVICE_NAME} configuration"))?;
    Ok(ServiceStatus {
        state: service_state(state),
        address: Some(options.address),
        unencrypted: options.tls.unencrypted,
    })
}

fn service_state(state: SERVICE_STATUS_CURRENT_STATE) -> ServiceState {
    match state {
        SERVICE_RUNNING => ServiceState::Running,
        SERVICE_START_PENDING | SERVICE_CONTINUE_PENDING => ServiceState::Starting,
        SERVICE_STOP_PENDING | SERVICE_PAUSE_PENDING => ServiceState::Stopping,
        // Stopped, and paused, which this service does not accept.
        _ => ServiceState::Stopped,
    }
}

/// The installed service's transport and address, without its password.
fn service_options(command: &str) -> Option<crate::desktop_host::ServeOptions> {
    if command.trim().is_empty() {
        return None;
    }
    match split_command_line(command).as_slice() {
        [_, mode, options @ ..] if mode == "--host-service" => parse_serve_arguments(options).ok(),
        _ => None,
    }
}

/// Where the installer gets the VNC password.
enum PasswordSource {
    /// Asked twice on the console.
    Prompt,
    /// From the TopVNC window, which asked for it.
    Given(String),
}

/// `topvnc --install-service [--serve options]`, from an administrator
/// terminal. The TopVNC window starts it elevated with `--ui-pipe NAME`
/// first, and passes the password and gets the result through that pipe.
pub fn install(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    if let [flag, name, options @ ..] = arguments
        && flag == UI_PIPE_FLAG
    {
        return service_pipe::report_to_window(name, |pipe| {
            let password = pipe.read_password()?;
            install_with(options, PasswordSource::Given(password), pipe)
        });
    }
    install_with(arguments, PasswordSource::Prompt, &mut std::io::stdout())
}

/// `topvnc --uninstall-service`, from an administrator terminal, or with
/// `--ui-pipe NAME` from the TopVNC window.
pub fn uninstall(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments {
        [] => uninstall_with(&mut std::io::stdout()),
        [flag, name] if flag == UI_PIPE_FLAG => {
            service_pipe::report_to_window(name, |pipe| uninstall_with(pipe))
        }
        _ => Err("usage: topvnc --uninstall-service".into()),
    }
}

fn install_with(
    arguments: &[String],
    password: PasswordSource,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let arguments = service_arguments(arguments)?;
    // Load the TLS files now, so a mistake is reported here and not by a
    // service that keeps restarting.
    let options = parse_serve_arguments(&arguments)?;
    options.tls.server()?;
    let manager = ScHandle::manager(SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE)
        .map_err(administrator_needed)?;
    match manager.service(SERVICE_QUERY_STATUS) {
        Ok(_) => {
            return Err(format!(
                "the {SERVICE_NAME} service is already installed; run topvnc --uninstall-service first"
            )
            .into());
        }
        Err(error) if is_error(&error, ERROR_SERVICE_DOES_NOT_EXIST) => {}
        Err(error) => return Err(error.into()),
    }
    let password = match password {
        PasswordSource::Given(password) => password,
        PasswordSource::Prompt => {
            let password = rpassword::prompt_password("VNC password for the host service: ")?;
            if !password.is_empty()
                && rpassword::prompt_password("Repeat the VNC password: ")? != password
            {
                return Err("the passwords do not match".into());
            }
            password
        }
    };
    if password.is_empty() {
        return Err("the VNC password must not be empty".into());
    }

    let folder = install_folder()?;
    std::fs::create_dir_all(&folder)?;
    let executable = folder.join("topvnc.exe");
    std::fs::copy(std::env::current_exe()?, &executable)
        .map_err(|error| format!("could not copy TopVNC to {}: {error}", executable.display()))?;
    store_password(Some(&password))?;
    drop(password);

    let mut service_arguments = vec!["--host-service".to_owned()];
    service_arguments.extend(arguments);
    let command = wide(&command_line(&executable, &service_arguments));
    let name = wide(SERVICE_NAME);
    let display_name = wide(SERVICE_DISPLAY_NAME);
    let service = unsafe {
        CreateServiceW(
            manager.0,
            name.as_ptr(),
            display_name.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            command.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            // LocalSystem.
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if service.is_null() {
        let error = last_error();
        let _ = store_password(None);
        return Err(format!("could not create the {SERVICE_NAME} service: {error}").into());
    }
    let service = ScHandle(service);
    let mut description = wide(SERVICE_DESCRIPTION);
    let description = SERVICE_DESCRIPTIONW {
        lpDescription: description.as_mut_ptr(),
    };
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_DESCRIPTION,
            (&raw const description).cast(),
        )
    };
    // Restart the service if it crashes: 5 seconds, then 30, then 60.
    let mut actions = [5_000, 30_000, 60_000].map(|delay| SC_ACTION {
        Type: SC_ACTION_RESTART,
        Delay: delay,
    });
    let failure = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: 24 * 60 * 60,
        cActions: actions.len() as u32,
        lpsaActions: actions.as_mut_ptr(),
        ..Default::default()
    };
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_FAILURE_ACTIONS,
            (&raw const failure).cast(),
        )
    };
    if unsafe { StartServiceW(service.0, 0, std::ptr::null()) } == 0 {
        return Err(format!(
            "the {SERVICE_NAME} service was installed but did not start: {}",
            last_error()
        )
        .into());
    }
    // The service is installed now, so a report that cannot be written is
    // not an installation failure.
    let mut summary = format!(
        "Installed and started the {SERVICE_NAME} service, serving on {}.\n\
         It runs {} as SYSTEM.\n\
         Viewers that sign in with the VNC password can now see and use the Windows lock and \
         sign-in screens. Stop the Server tab's own server first: both cannot use one port.\n",
        options.address,
        executable.display()
    );
    if !software_sas_allowed() {
        summary.push_str(
            "Ctrl+Alt+Delete from viewers is off: Windows allows it only when the policy \
             \"Disable or enable software Secure Attention Sequence\" is set to Services. \
             The lock screen normally does not need it.\n",
        );
    }
    summary.push_str(&format!("Log: {}\n", log_path()?.display()));
    let _ = out.write_all(summary.as_bytes());
    Ok(())
}

fn uninstall_with(out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let manager = ScHandle::manager(SC_MANAGER_CONNECT).map_err(administrator_needed)?;
    let service = match manager.service(SERVICE_STOP | SERVICE_QUERY_STATUS | DELETE_ACCESS) {
        Ok(service) => service,
        Err(error) if is_error(&error, ERROR_SERVICE_DOES_NOT_EXIST) => {
            let _ = store_password(None);
            return Err(format!("the {SERVICE_NAME} service is not installed").into());
        }
        Err(error) => return Err(administrator_needed(error)),
    };
    let mut status = SERVICE_STATUS::default();
    if unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) } == 0 {
        let error = last_error();
        if !is_error(&error, ERROR_SERVICE_NOT_ACTIVE) {
            return Err(format!("could not stop the {SERVICE_NAME} service: {error}").into());
        }
    }
    let started = Instant::now();
    while service.state()? != SERVICE_STOPPED {
        if started.elapsed() > SERVICE_STOP_TIMEOUT {
            return Err(format!("the {SERVICE_NAME} service did not stop").into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if unsafe { DeleteService(service.0) } == 0 {
        return Err(format!(
            "could not remove the {SERVICE_NAME} service: {}",
            last_error()
        )
        .into());
    }
    drop(service);
    store_password(None)?;
    let executable = install_folder()?.join("topvnc.exe");
    remove_installed_executable(&executable).map_err(|error| {
        format!(
            "Removed the {SERVICE_NAME} service and its stored password, but removal is \
             incomplete: could not delete {}: {error}. Exit TopVNC, then delete this file \
             from an administrator terminal.",
            executable.display()
        )
    })?;
    let _ = std::fs::remove_dir(install_folder()?);
    let _ = writeln!(
        out,
        "Removed the {SERVICE_NAME} service and its stored password."
    );
    Ok(())
}

fn remove_installed_executable(executable: &Path) -> std::io::Result<()> {
    // An uninstaller running from this copy cannot delete its own image.
    if let Ok(installed) = executable.canonicalize()
        && std::env::current_exe()?.canonicalize()? == installed
    {
        return Err(std::io::Error::other(
            "the uninstaller is running from this executable",
        ));
    }
    // A stopped service may take a moment to release its executable.
    for attempt in 0..25 {
        match std::fs::remove_file(executable) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if attempt == 24 => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
    unreachable!("the last attempt returns")
}

// ---- The service process -----------------------------------------------

/// The service's status handle and stop event, for the control handler.
static STATUS_HANDLE: AtomicUsize = AtomicUsize::new(0);
static STOP_EVENT: AtomicUsize = AtomicUsize::new(0);

fn set_service_status(state: SERVICE_STATUS_CURRENT_STATE, exit_code: u32) {
    let handle = STATUS_HANDLE.load(Ordering::Acquire) as SERVICE_STATUS_HANDLE;
    if handle.is_null() {
        return;
    }
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: exit_code,
        dwWaitHint: if state == SERVICE_RUNNING || state == SERVICE_STOPPED {
            0
        } else {
            (AGENT_STOP_TIMEOUT * 2).as_millis() as u32
        },
        ..Default::default()
    };
    unsafe { SetServiceStatus(handle, &status) };
}

unsafe extern "system" fn control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            set_service_status(SERVICE_STOP_PENDING, NO_ERROR);
            let event = STOP_EVENT.load(Ordering::Acquire) as HANDLE;
            if !event.is_null() {
                unsafe { SetEvent(event) };
            }
            NO_ERROR
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut windows_sys::core::PWSTR) {
    let name = wide(SERVICE_NAME);
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control_handler), std::ptr::null())
    };
    if handle.is_null() {
        return;
    }
    STATUS_HANDLE.store(handle as usize, Ordering::Release);
    set_service_status(SERVICE_START_PENDING, NO_ERROR);
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        set_service_status(
            SERVICE_STOPPED,
            last_error().raw_os_error().unwrap_or(1) as u32,
        );
        return;
    }
    STOP_EVENT.store(event as usize, Ordering::Release);
    set_service_status(SERVICE_RUNNING, NO_ERROR);
    let log = Arc::new(ServiceLog::open());
    log.write(&format!("{SERVICE_DISPLAY_NAME} service started."));
    // The --serve options follow --host-service on the service's command
    // line; the service manager passes no arguments of its own here.
    let arguments = std::env::args().skip(2).collect::<Vec<_>>();
    let exit_code = match supervise(&arguments, event, &log) {
        Ok(()) => NO_ERROR,
        Err(error) => {
            log.write(&format!("The service stopped: {error}"));
            1
        }
    };
    log.write(&format!("{SERVICE_DISPLAY_NAME} service stopped."));
    set_service_status(SERVICE_STOPPED, exit_code);
}

/// `topvnc --host-service`: run by the service manager only.
pub fn run_service() -> Result<(), Box<dyn Error>> {
    let mut name = wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        let error = last_error();
        if is_error(&error, ERROR_FAILED_SERVICE_CONTROLLER_CONNECT) {
            return Err(
                "--host-service is started by Windows; install it with topvnc --install-service"
                    .into(),
            );
        }
        return Err(error.into());
    }
    Ok(())
}

pub fn log_path() -> Result<PathBuf, String> {
    let data = std::env::var_os("ProgramData").ok_or("the ProgramData folder is not set")?;
    Ok(PathBuf::from(data).join("TopVNC").join("host-service.log"))
}

/// The service log, rotated once past [`MAX_LOG_BYTES`]. Logging never
/// stops the service.
struct ServiceLog {
    file: Mutex<Option<(PathBuf, File)>>,
}

impl ServiceLog {
    fn open() -> Self {
        let file = log_path().ok().and_then(|path| {
            std::fs::create_dir_all(path.parent()?).ok()?;
            let file = Self::append(&path)?;
            Some((path, file))
        });
        Self {
            file: Mutex::new(file),
        }
    }

    fn append(path: &Path) -> Option<File> {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    }

    fn write(&self, message: &str) {
        let Ok(mut guard) = self.file.lock() else {
            return;
        };
        let Some((path, file)) = guard.as_mut() else {
            return;
        };
        if file
            .metadata()
            .is_ok_and(|metadata| metadata.len() > MAX_LOG_BYTES)
        {
            let _ = std::fs::rename(&*path, path.with_extension("log.old"));
            match Self::append(path) {
                Some(new) => *file = new,
                None => return,
            }
        }
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let _ = writeln!(file, "[{seconds}] {}", message.replace(['\r', '\n'], " "));
    }
}

/// Wait up to `timeout` for the stop event. Returns whether it was set.
fn wait_for_stop(stop: HANDLE, timeout: Duration) -> bool {
    unsafe { WaitForSingleObject(stop, timeout.as_millis() as u32) == WAIT_OBJECT_0 }
}

/// Keep one agent running in the console session until the service stops.
fn supervise(
    arguments: &[String],
    stop: HANDLE,
    log: &Arc<ServiceLog>,
) -> Result<(), Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    if !software_sas_allowed() {
        log.write(
            "Ctrl+Alt+Delete from viewers is off: the software Secure Attention Sequence \
             policy does not allow services.",
        );
    }
    let mut backoff = AGENT_RESTART_MIN;
    let mut no_session_logged = false;
    loop {
        if wait_for_stop(stop, Duration::ZERO) {
            return Ok(());
        }
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        if session == NO_CONSOLE_SESSION {
            if !no_session_logged {
                log.write("No session is at the console; waiting for one.");
                no_session_logged = true;
            }
            if wait_for_stop(stop, CONSOLE_CHECK_INTERVAL) {
                return Ok(());
            }
            continue;
        }
        no_session_logged = false;
        let started = Instant::now();
        match Agent::start(&executable, arguments, session, log) {
            Err(error) => log.write(&format!("Could not start the host agent: {error}")),
            Ok(agent) => {
                log.write(&format!("Host agent started in console session {session}."));
                let ended = agent.run(stop, session, log);
                if ended == AgentEnd::Stop {
                    return Ok(());
                }
                if ended == AgentEnd::SessionChanged {
                    // Serve the new console session right away.
                    backoff = AGENT_RESTART_MIN;
                    continue;
                }
            }
        }
        if started.elapsed() >= AGENT_STABLE {
            backoff = AGENT_RESTART_MIN;
        }
        if wait_for_stop(stop, backoff) {
            return Ok(());
        }
        backoff = (backoff * 2).min(AGENT_RESTART_MAX);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum AgentEnd {
    /// The service is stopping.
    Stop,
    /// Another session is at the console now.
    SessionChanged,
    /// The agent exited by itself.
    Exited,
}

/// A host agent process in a console session.
struct Agent {
    process: OwnedHandle,
    /// Closing it tells the agent to stop.
    input: Option<ServicePipe>,
    output: Option<std::thread::JoinHandle<()>>,
    /// Ends the agent if the service ends without stopping it. Each agent
    /// has its own: a job stays in the session of its first process, so an
    /// agent in the next console session cannot join this one's.
    _job: job::Job,
}

// ---- The pipes between the service and an agent ------------------------
//
// The agent runs in the console session and the service in session 0, and
// handles cannot be inherited across sessions. So the service creates two
// named pipes, one each way, that only SYSTEM can open, starts the agent
// with their name, and accepts only that agent process on them. The agent
// accepts only pipes made by the service whose process ID is in the name.

/// A new name for an agent's pipes: the service's process ID and 128
/// random bits, so no other process can take the name first.
fn new_agent_pipe_name() -> std::io::Result<String> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|error| std::io::Error::other(error.to_string()))?;
    let service = unsafe { GetCurrentProcessId() };
    Ok(random.iter().fold(
        format!("{AGENT_PIPE_PREFIX}{service}-"),
        |mut name, byte| {
            name.push_str(&format!("{byte:02x}"));
            name
        },
    ))
}

/// The service process ID in an agent pipe name, if `name` is one the
/// service makes. The agent opens nothing else, so never a file, a device,
/// or a remote share.
fn agent_pipe_service(name: &str) -> Option<u32> {
    let (service, id) = name.strip_prefix(AGENT_PIPE_PREFIX)?.split_once('-')?;
    let digits = !service.is_empty() && service.bytes().all(|byte| byte.is_ascii_digit());
    let random = id.len() == 32
        && id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    if digits && random {
        service.parse().ok()
    } else {
        None
    }
}

fn agent_pipe_path(name: &str, to_agent: bool) -> Vec<u16> {
    let direction = if to_agent { "to-agent" } else { "from-agent" };
    wide(&format!(r"\\.\pipe\{name}.{direction}"))
}

/// The service's end of one pipe to an agent. It is overlapped so that
/// waiting for the agent can time out or notice the agent exiting.
struct ServicePipe {
    pipe: OwnedHandle,
    /// Signalled when an overlapped operation on `pipe` completes.
    event: OwnedHandle,
}

impl ServicePipe {
    fn create(name: &str, to_agent: bool, security: &str) -> std::io::Result<Self> {
        let sddl = wide(security);
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
            return Err(last_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let direction = if to_agent {
            PIPE_ACCESS_OUTBOUND
        } else {
            PIPE_ACCESS_INBOUND
        };
        let path = agent_pipe_path(name, to_agent);
        // The first instance: if the name already exists, this fails rather
        // than joining someone else's pipe.
        let pipe = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                direction | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                0,
                &attributes,
            )
        };
        let created = last_error();
        unsafe { LocalFree(descriptor) };
        if pipe == INVALID_HANDLE_VALUE {
            return Err(created);
        }
        // SAFETY: a new handle owned from here on.
        let pipe = unsafe { OwnedHandle::from_raw_handle(pipe) };
        // Manual reset, as overlapped operations require.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(last_error());
        }
        // SAFETY: a new handle owned from here on.
        let event = unsafe { OwnedHandle::from_raw_handle(event) };
        Ok(Self { pipe, event })
    }

    fn overlapped(&self) -> OVERLAPPED {
        OVERLAPPED {
            hEvent: raw(&self.event),
            ..Default::default()
        }
    }

    /// Wait for the process `agent` to open the pipe. Fails if it exits
    /// first, if any other process opens it, or after `timeout`.
    fn accept(&self, agent: HANDLE, timeout: Duration) -> std::io::Result<()> {
        let pipe = raw(&self.pipe);
        let mut overlapped = self.overlapped();
        if unsafe { ConnectNamedPipe(pipe, &mut overlapped) } == 0 {
            match unsafe { GetLastError() } {
                ERROR_PIPE_CONNECTED => {}
                ERROR_IO_PENDING => {
                    let handles = [raw(&self.event), agent];
                    let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
                    let waited =
                        unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, milliseconds) };
                    let wait_error = last_error();
                    let mut transferred = 0;
                    if waited != WAIT_OBJECT_0 {
                        // The overlapped structure must outlive the operation.
                        unsafe {
                            CancelIoEx(pipe, &overlapped);
                            GetOverlappedResult(pipe, &overlapped, &mut transferred, 1);
                        }
                        return Err(match waited {
                            _ if waited == WAIT_OBJECT_0 + 1 => {
                                std::io::Error::other("the host agent exited before it connected")
                            }
                            WAIT_TIMEOUT => std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "the host agent did not connect in time",
                            ),
                            _ => wait_error,
                        });
                    }
                    if unsafe { GetOverlappedResult(pipe, &overlapped, &mut transferred, 0) } == 0 {
                        return Err(last_error());
                    }
                }
                error => return Err(std::io::Error::from_raw_os_error(error as i32)),
            }
        }
        let mut client = 0;
        if unsafe { GetNamedPipeClientProcessId(pipe, &mut client) } == 0 {
            return Err(last_error());
        }
        if client != unsafe { GetProcessId(agent) } {
            return Err(std::io::Error::other(
                "another process opened the host agent's pipe",
            ));
        }
        Ok(())
    }
}

/// Reads what the agent writes; the agent closing its end reads as the end.
impl Read for ServicePipe {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let pipe = raw(&self.pipe);
        let mut overlapped = self.overlapped();
        let length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        let started = unsafe {
            ReadFile(
                pipe,
                buffer.as_mut_ptr(),
                length,
                std::ptr::null_mut(),
                &mut overlapped,
            )
        };
        if started == 0 {
            match unsafe { GetLastError() } {
                ERROR_IO_PENDING => {}
                ERROR_BROKEN_PIPE => return Ok(0),
                error => return Err(std::io::Error::from_raw_os_error(error as i32)),
            }
        }
        let mut transferred = 0;
        if unsafe { GetOverlappedResult(pipe, &overlapped, &mut transferred, 1) } == 0 {
            let error = last_error();
            return if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                Ok(0)
            } else {
                Err(error)
            };
        }
        Ok(transferred as usize)
    }
}

/// The agent's end of one pipe from the service named in `name`. Fails if
/// another process made the pipe.
fn open_service_pipe(name: &str, to_agent: bool) -> std::io::Result<File> {
    let service = agent_pipe_service(name).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name:?} is not a host agent pipe"),
        )
    })?;
    let path = agent_pipe_path(name, to_agent);
    // Reading the attributes is needed to check the pipe's server.
    let access = FILE_READ_ATTRIBUTES
        | if to_agent {
            GENERIC_READ
        } else {
            GENERIC_WRITE
        };
    let pipe = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            // The service may identify the agent but not act as it.
            SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            std::ptr::null_mut(),
        )
    };
    if pipe == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    // SAFETY: a new handle owned from here on.
    let pipe = unsafe { OwnedHandle::from_raw_handle(pipe) };
    let mut server = 0;
    if unsafe { GetNamedPipeServerProcessId(raw(&pipe), &mut server) } == 0 {
        return Err(last_error());
    }
    if server != service {
        return Err(std::io::Error::other(
            "the host agent's pipe was not made by the service",
        ));
    }
    Ok(File::from(pipe))
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    use std::os::windows::io::AsRawHandle;
    handle.as_raw_handle()
}

/// A primary SYSTEM token, this service's own, moved to `session`.
fn session_token(session: u32) -> std::io::Result<OwnedHandle> {
    let mut own = std::ptr::null_mut();
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE
                | TOKEN_QUERY
                | TOKEN_ASSIGN_PRIMARY
                | TOKEN_ADJUST_DEFAULT
                | TOKEN_ADJUST_SESSIONID,
            &mut own,
        )
    } == 0
    {
        return Err(last_error());
    }
    // SAFETY: a new handle owned from here on.
    let own = unsafe { OwnedHandle::from_raw_handle(own) };
    let mut token = std::ptr::null_mut();
    if unsafe {
        DuplicateTokenEx(
            raw(&own),
            MAXIMUM_ALLOWED,
            std::ptr::null(),
            SecurityIdentification,
            TokenPrimary,
            &mut token,
        )
    } == 0
    {
        return Err(last_error());
    }
    // SAFETY: a new handle owned from here on.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    if unsafe {
        SetTokenInformation(
            raw(&token),
            TokenSessionId,
            (&raw const session).cast(),
            size_of::<u32>() as u32,
        )
    } == 0
    {
        return Err(last_error());
    }
    Ok(token)
}

impl Agent {
    fn start(
        executable: &Path,
        arguments: &[String],
        session: u32,
        log: &Arc<ServiceLog>,
    ) -> std::io::Result<Self> {
        let token = session_token(session)?;
        let name = new_agent_pipe_name()?;
        let input = ServicePipe::create(&name, true, AGENT_PIPE_SECURITY)?;
        let output = ServicePipe::create(&name, false, AGENT_PIPE_SECURITY)?;
        let mut agent_arguments = vec!["--host-agent".to_owned(), AGENT_PIPE_FLAG.to_owned(), name];
        agent_arguments.extend_from_slice(arguments);
        let mut command = wide(&command_line(executable, &agent_arguments));
        let mut desktop = wide(AGENT_DESKTOP);
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            lpDesktop: desktop.as_mut_ptr(),
            ..Default::default()
        };
        let mut process = PROCESS_INFORMATION::default();
        if unsafe {
            CreateProcessAsUserW(
                raw(&token),
                std::ptr::null(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                // Nothing is inherited: the agent runs in another session.
                0,
                CREATE_NO_WINDOW | CREATE_SUSPENDED,
                std::ptr::null(),
                std::ptr::null(),
                &startup,
                &mut process,
            )
        } == 0
        {
            return Err(last_error());
        }
        // SAFETY: new handles owned from here on.
        let (handle, thread) = unsafe {
            (
                OwnedHandle::from_raw_handle(process.hProcess),
                OwnedHandle::from_raw_handle(process.hThread),
            )
        };
        // The agent ends with the service, even if the service crashes.
        let job = match job::Job::new().and_then(|job| job.assign(raw(&handle)).map(|()| job)) {
            Ok(job) => job,
            Err(error) => {
                unsafe { TerminateProcess(raw(&handle), 1) };
                return Err(std::io::Error::other(error));
            }
        };
        unsafe { ResumeThread(raw(&thread)) };
        drop(thread);
        let connected = input
            .accept(raw(&handle), AGENT_CONNECT_TIMEOUT)
            .and_then(|()| output.accept(raw(&handle), AGENT_CONNECT_TIMEOUT));
        if let Err(error) = connected {
            unsafe {
                TerminateProcess(raw(&handle), 1);
                WaitForSingleObject(raw(&handle), INFINITE);
            }
            return Err(error);
        }
        let log = Arc::clone(log);
        let output = std::thread::Builder::new()
            .name("topvnc-agent-output".into())
            .spawn(move || {
                for line in BufReader::new(output).lines() {
                    let Ok(line) = line else { break };
                    match AgentMessage::parse(&line) {
                        AgentMessage::SecureAttention => {
                            if let Err(error) = send_secure_attention() {
                                log.write(&format!("Could not start Ctrl+Alt+Delete: {error}"));
                            }
                        }
                        AgentMessage::Log(message) => log.write(&message),
                    }
                }
            })?;
        Ok(Self {
            process: handle,
            input: Some(input),
            output: Some(output),
            _job: job,
        })
    }

    /// Wait until the service stops, the console session changes, or the
    /// agent exits.
    fn run(mut self, stop: HANDLE, session: u32, log: &ServiceLog) -> AgentEnd {
        let handles = [stop, raw(&self.process)];
        let end = loop {
            let waited = unsafe {
                WaitForMultipleObjects(
                    handles.len() as u32,
                    handles.as_ptr(),
                    0,
                    CONSOLE_CHECK_INTERVAL.as_millis() as u32,
                )
            };
            if waited == WAIT_OBJECT_0 {
                break AgentEnd::Stop;
            }
            if waited == WAIT_OBJECT_0 + 1 {
                break AgentEnd::Exited;
            }
            if waited != WAIT_TIMEOUT {
                log.write(&format!(
                    "Waiting for the host agent failed: {}",
                    last_error()
                ));
                break AgentEnd::Exited;
            }
            if unsafe { WTSGetActiveConsoleSessionId() } != session {
                log.write("Another session is at the console; moving the host agent there.");
                break AgentEnd::SessionChanged;
            }
        };
        if end == AgentEnd::Exited {
            log.write("The host agent exited.");
        }
        self.stop();
        end
    }

    /// Ask the agent to stop, releasing held keys, and end it if it does not.
    fn stop(&mut self) {
        drop(self.input.take());
        let process = raw(&self.process);
        if unsafe { WaitForSingleObject(process, AGENT_STOP_TIMEOUT.as_millis() as u32) }
            != WAIT_OBJECT_0
        {
            unsafe {
                TerminateProcess(process, 1);
                WaitForSingleObject(process, INFINITE);
            }
        }
        if let Some(output) = self.output.take() {
            let _ = output.join();
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        if self.input.is_some() {
            self.stop();
        }
    }
}

/// Start Ctrl+Alt+Delete on the console session, as a service may.
fn send_secure_attention() -> Result<(), String> {
    let library = wide("sas.dll");
    let module = unsafe {
        LoadLibraryExW(
            library.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if module.is_null() {
        return Err(format!("could not load sas.dll: {}", last_error()));
    }
    let Some(procedure) = (unsafe { GetProcAddress(module, c"SendSAS".as_ptr().cast()) }) else {
        return Err("sas.dll has no SendSAS".into());
    };
    // SAFETY: SendSAS is `VOID SendSAS(BOOL AsUser)`.
    let send: unsafe extern "system" fn(i32) = unsafe { std::mem::transmute(procedure) };
    // As the service, not as the signed-in user.
    unsafe { send(0) };
    Ok(())
}

// ---- The agent process -------------------------------------------------

static AGENT_SHUTDOWN: AtomicBool = AtomicBool::new(false);
/// The agent's pipe to the service, once open.
static TO_SERVICE: OnceLock<Mutex<File>> = OnceLock::new();

fn tell_service(line: &str) {
    let Some(pipe) = TO_SERVICE.get() else {
        return;
    };
    let mut pipe = pipe.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = pipe.write_all(format!("{line}\n").as_bytes());
}

fn request_secure_attention() {
    tell_service("SAS");
}

/// `topvnc --host-agent --agent-pipe NAME [--serve options]`: started by
/// the service as SYSTEM in the console session.
pub fn run_agent(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    let not_from_service =
        || format!("--host-agent is started by the {SERVICE_NAME} service as SYSTEM");
    let [flag, name, arguments @ ..] = arguments else {
        return Err(not_from_service().into());
    };
    if flag != AGENT_PIPE_FLAG {
        return Err(not_from_service().into());
    }
    let unreachable =
        |error: std::io::Error| format!("could not reach the {SERVICE_NAME} service: {error}");
    let from_service = open_service_pipe(name, true).map_err(unreachable)?;
    let to_service = open_service_pipe(name, false).map_err(unreachable)?;
    let _ = TO_SERVICE.set(Mutex::new(to_service));
    // The agent has no console, so failures are reported to the service log.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tell_service(&format!(
            "LOG The host agent failed: {}",
            info.to_string().replace(['\r', '\n'], " ")
        ));
        default_hook(info);
    }));
    let result = serve_agent(arguments, from_service);
    if let Err(error) = &result {
        tell_service(&format!("LOG The host agent stopped: {error}"));
    }
    result
}

fn serve_agent(arguments: &[String], mut from_service: File) -> Result<(), Box<dyn Error>> {
    let arguments = service_arguments(arguments)?;
    let options = parse_serve_arguments(&arguments)?;
    let password = read_password().map_err(|error| {
        format!("{error}. --host-agent is started by the {SERVICE_NAME} service as SYSTEM")
    })?;
    // The service closes its end of this pipe to stop the agent.
    std::thread::Builder::new()
        .name("topvnc-agent-stop".into())
        .spawn(move || {
            let mut sink = Vec::new();
            let _ = from_service.read_to_end(&mut sink);
            AGENT_SHUTDOWN.store(true, Ordering::Release);
        })?;
    windows_server::follow_input_desktops(request_secure_attention);
    windows_server::serve(options, Some(password), &AGENT_SHUTDOWN, &|notice| {
        if !matches!(notice, ServerNotice::Connections(_)) {
            tell_service(&format!(
                "LOG {}",
                notice.to_string().replace(['\r', '\n'], " ")
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn service_options_require_a_password_and_keep_plain_tcp_on_this_computer() {
        assert!(service_arguments(&arguments(&["127.0.0.1:5900", "--unencrypted"])).is_ok());
        assert!(service_arguments(&arguments(&["[::1]:5900", "--unencrypted"])).is_ok());
        assert!(service_arguments(&arguments(&["0.0.0.0:5900", "--unencrypted"])).is_err());
        assert!(service_arguments(&arguments(&["192.168.1.5:5900", "--unencrypted"])).is_err());
        assert!(
            service_arguments(&arguments(&[
                "127.0.0.1:5900",
                "--unencrypted",
                "--allow-insecure"
            ]))
            .is_err()
        );
        assert!(service_arguments(&arguments(&["--bogus"])).is_err());
        // TLS paths become absolute, as the service starts in System32.
        let normalized = service_arguments(&arguments(&[
            "0.0.0.0:5900",
            "--tls-cert",
            "cert.pem",
            "--tls-key",
            "key.pem",
            "--display",
            "all",
        ]))
        .unwrap();
        assert_eq!(normalized.len(), 7);
        assert!(Path::new(&normalized[2]).is_absolute());
        assert!(normalized[2].ends_with("cert.pem"));
        assert!(Path::new(&normalized[4]).is_absolute());
        assert_eq!(&normalized[5..], ["--display", "all"]);
    }

    #[test]
    fn command_lines_quote_arguments_as_windows_splits_them() {
        let line = command_line(
            Path::new(r"C:\Program Files\TopVNC\topvnc.exe"),
            &arguments(&[
                "--host-agent",
                r"C:\My Certs\cert.pem",
                "",
                r#"say "hi""#,
                r"ends\",
                r"a\\b",
            ]),
        );
        assert_eq!(
            line,
            r#""C:\Program Files\TopVNC\topvnc.exe" --host-agent "C:\My Certs\cert.pem" "" "say \"hi\"" ends\ a\\b"#
        );
        let mut quoted = String::new();
        quote_argument(r"with space\", &mut quoted);
        assert_eq!(quoted, r#""with space\\""#);
    }

    #[test]
    fn command_lines_split_back_into_the_same_arguments() {
        let values = arguments(&[
            "--host-service",
            "127.0.0.1:5900",
            "--tls-cert",
            r"C:\My Certs\cert.pem",
            "",
            r#"say "hi""#,
            r"ends\",
            r"a\\b",
        ]);
        let program = r"C:\Program Files\TopVNC\topvnc.exe";
        let split = split_command_line(&command_line(Path::new(program), &values));
        assert_eq!(split[0], program);
        assert_eq!(&split[1..], values);
        assert_eq!(
            split_command_line(&argument_line(&values))[1..],
            values[1..]
        );
        assert_eq!(argument_line(&[]), "");
    }

    #[test]
    fn installed_command_lines_give_the_service_address() {
        let program = Path::new(r"C:\Program Files\TopVNC\topvnc.exe");
        let line = command_line(
            program,
            &arguments(&["--host-service", "127.0.0.1:5901", "--unencrypted"]),
        );
        let options = service_options(&line).unwrap();
        assert_eq!(options.address, "127.0.0.1:5901");
        assert!(options.tls.unencrypted);
        let tls = command_line(
            program,
            &arguments(&[
                "--host-service",
                "--tls-cert",
                "cert.pem",
                "--tls-key",
                "key.pem",
            ]),
        );
        assert!(!service_options(&tls).unwrap().tls.unencrypted);
        // The default address, when none was given.
        let line = command_line(program, &arguments(&["--host-service", "--unencrypted"]));
        assert_eq!(service_options(&line).unwrap().address, "127.0.0.1:5900");
        // Anything that is not this service's command line has no address.
        for line in [
            String::new(),
            command_line(program, &arguments(&["--host-agent", "127.0.0.1:5901"])),
            command_line(program, &arguments(&["--host-service", "--bogus"])),
        ] {
            assert!(service_options(&line).is_none());
        }
    }

    #[test]
    fn uninstall_deletes_the_copy_and_reports_a_loaded_executable() {
        use std::os::windows::fs::OpenOptionsExt;
        let folder = PathBuf::from(format!("target/service-removal-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let executable = folder.join("topvnc.exe");
        std::fs::write(&executable, b"copy").unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&executable)
            .unwrap();
        assert!(remove_installed_executable(&executable).is_err());
        assert!(executable.exists());
        drop(held);
        remove_installed_executable(&executable).unwrap();
        assert!(!executable.exists());
        remove_installed_executable(&executable).unwrap();
        std::fs::remove_dir(folder).unwrap();
        // Never attempt to delete the running image, or wait for our own exit.
        let current = std::env::current_exe().unwrap();
        assert!(
            remove_installed_executable(&current)
                .unwrap_err()
                .to_string()
                .contains("uninstaller")
        );
        assert!(current.exists());
    }

    #[test]
    fn service_manager_states_map_to_what_the_window_shows() {
        assert_eq!(service_state(SERVICE_RUNNING), ServiceState::Running);
        for state in [SERVICE_START_PENDING, SERVICE_CONTINUE_PENDING] {
            assert_eq!(service_state(state), ServiceState::Starting);
        }
        for state in [SERVICE_STOP_PENDING, SERVICE_PAUSE_PENDING] {
            assert_eq!(service_state(state), ServiceState::Stopping);
        }
        assert_eq!(service_state(SERVICE_STOPPED), ServiceState::Stopped);
        // Reading the status needs no administrator rights, whether or not
        // the service is installed.
        assert!(query_status().is_ok());
    }

    #[test]
    fn agent_lines_are_secure_attention_requests_or_log_text() {
        assert_eq!(
            AgentMessage::parse("SAS\r\n"),
            AgentMessage::SecureAttention
        );
        assert_eq!(
            AgentMessage::parse("LOG Serving display 1 of 2."),
            AgentMessage::Log("Serving display 1 of 2.".into())
        );
        // A panic message or anything else is logged as is.
        assert_eq!(
            AgentMessage::parse("thread 'main' panicked"),
            AgentMessage::Log("thread 'main' panicked".into())
        );
    }

    /// Tests are not SYSTEM, so they open the pipes under a looser rule;
    /// everything else is the service's code.
    const TEST_PIPE_SECURITY: &str = "D:P(A;;GA;;;WD)";

    fn this_process() -> HANDLE {
        unsafe { GetCurrentProcess() }
    }

    #[test]
    fn agent_pipe_names_carry_the_service_and_nothing_else_is_opened() {
        let name = new_agent_pipe_name().unwrap();
        assert_eq!(
            agent_pipe_service(&name),
            Some(unsafe { GetCurrentProcessId() })
        );
        assert_ne!(name, new_agent_pipe_name().unwrap());
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            agent_pipe_service(&format!("{AGENT_PIPE_PREFIX}42-{id}")),
            Some(42)
        );
        for bad in [
            format!("{AGENT_PIPE_PREFIX}-{id}"),
            format!("{AGENT_PIPE_PREFIX}4x-{id}"),
            format!("{AGENT_PIPE_PREFIX}42-{}", &id[1..]),
            format!("{AGENT_PIPE_PREFIX}42-{}", id.to_uppercase()),
            format!("{AGENT_PIPE_PREFIX}42-{id}\\..\\other"),
            format!("{AGENT_PIPE_PREFIX}99999999999-{id}"),
            format!("other-42-{id}"),
            r"\\server\share\file".to_owned(),
        ] {
            assert_eq!(agent_pipe_service(&bad), None, "{bad}");
        }
    }

    #[test]
    fn agent_pipes_carry_lines_out_and_closing_stops_the_agent() {
        let name = new_agent_pipe_name().unwrap();
        let input = ServicePipe::create(&name, true, TEST_PIPE_SECURITY).unwrap();
        let output = ServicePipe::create(&name, false, TEST_PIPE_SECURITY).unwrap();
        // A second service pipe of the same name is refused.
        assert!(ServicePipe::create(&name, true, TEST_PIPE_SECURITY).is_err());
        let mut from_service = open_service_pipe(&name, true).unwrap();
        let mut to_service = open_service_pipe(&name, false).unwrap();
        input
            .accept(this_process(), Duration::from_secs(5))
            .unwrap();
        output
            .accept(this_process(), Duration::from_secs(5))
            .unwrap();

        to_service.write_all(b"SAS\nLOG ready\n").unwrap();
        drop(to_service);
        let lines: Vec<String> = BufReader::new(output)
            .lines()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(lines, ["SAS", "LOG ready"]);

        drop(input);
        let mut sink = Vec::new();
        assert_eq!(from_service.read_to_end(&mut sink).unwrap(), 0);
    }

    #[test]
    fn agent_pipes_accept_only_the_agent_in_time() {
        let name = new_agent_pipe_name().unwrap();
        let pipe = ServicePipe::create(&name, false, TEST_PIPE_SECURITY).unwrap();
        // A running process that never connects: waiting times out.
        let mut slow = std::process::Command::new("cmd")
            .args(["/c", "ping -n 5 127.0.0.1 >nul"])
            .spawn()
            .unwrap();
        let waited = {
            use std::os::windows::io::AsRawHandle;
            pipe.accept(slow.as_raw_handle(), Duration::from_millis(50))
        };
        let _ = slow.kill();
        let _ = slow.wait();
        assert_eq!(waited.unwrap_err().kind(), std::io::ErrorKind::TimedOut);

        // This process opens the pipe, but the service expects another.
        let _client = open_service_pipe(&name, false).unwrap();
        let mut other = std::process::Command::new("cmd")
            .args(["/c", "exit"])
            .spawn()
            .unwrap();
        use std::os::windows::io::AsRawHandle;
        let refused = pipe.accept(other.as_raw_handle(), Duration::from_secs(5));
        let _ = other.wait();
        assert!(refused.unwrap_err().to_string().contains("another process"));
    }

    #[test]
    fn agents_open_only_pipes_made_by_the_named_service() {
        let id = "0123456789abcdef0123456789abcdef";
        let mut random = [0u8; 4];
        getrandom::fill(&mut random).unwrap();
        let elsewhere = unsafe { GetCurrentProcessId() }.wrapping_add(1);
        // Unique per run, but naming a service that is not this process.
        let name = format!(
            "{AGENT_PIPE_PREFIX}{elsewhere}-{}{}",
            &id[8..],
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let _pipe = ServicePipe::create(&name, true, TEST_PIPE_SECURITY).unwrap();
        let opened = open_service_pipe(&name, true);
        assert!(opened.unwrap_err().to_string().contains("not made by"));
    }

    #[test]
    fn host_agents_need_the_service_pipe() {
        let error = run_agent(&arguments(&["127.0.0.1:5900"])).unwrap_err();
        assert!(error.to_string().contains("started by the"), "{error}");
        let error = run_agent(&arguments(&[AGENT_PIPE_FLAG, r"..\secret"])).unwrap_err();
        assert!(
            error.to_string().contains("not a host agent pipe"),
            "{error}"
        );
        let missing = format!("{AGENT_PIPE_PREFIX}1-0123456789abcdef0123456789abcdef");
        let error = run_agent(&arguments(&[AGENT_PIPE_FLAG, &missing])).unwrap_err();
        assert!(error.to_string().contains("could not reach"), "{error}");
    }
}
